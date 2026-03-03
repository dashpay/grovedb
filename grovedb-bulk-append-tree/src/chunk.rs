//! Chunk blob serialization and deserialization.
//!
//! Chunk blobs are immutable once written, suitable for CDN caching and client
//! sync. Two wire formats are supported:
//!
//! - **Fixed-size** (flag `0x01`): all entries share the same length. The
//!   header stores entry count and entry size once, then raw entries follow
//!   without per-entry length prefixes.
//! - **Variable-size** (flag `0x00`): each entry is preceded by a 4-byte
//!   big-endian length prefix.
//!
//! `serialize_chunk_blob` auto-detects which format to use. The deserializer
//! handles both transparently.

use crate::BulkAppendError;

/// Format flag: variable-size entries (per-entry length prefix).
const FORMAT_VARIABLE: u8 = 0x00;
/// Format flag: fixed-size entries (count + size in header).
const FORMAT_FIXED: u8 = 0x01;

/// Maximum number of entries in a single chunk blob.
///
/// This prevents memory exhaustion from crafted blobs with huge counts.
/// Since chunks are produced by epochs (epoch_size = 2^height, height ≤ 16),
/// no legitimate chunk exceeds 65536 entries. We use 1M as a generous cap.
const MAX_CHUNK_ENTRIES: usize = 1 << 20;

/// Serialize entries into a chunk blob.
///
/// Auto-selects the most compact format:
/// - If all entries have the same length -> fixed-size format
/// - Otherwise -> variable-size format
///
/// Returns an empty `Vec` for an empty slice (no header byte).
pub fn serialize_chunk_blob(entries: &[Vec<u8>]) -> Vec<u8> {
    if entries.is_empty() {
        return Vec::new();
    }

    let all_same_len = entries.iter().all(|e| e.len() == entries[0].len());

    if all_same_len {
        serialize_fixed(entries)
    } else {
        serialize_variable(entries)
    }
}

/// Deserialize a chunk blob into individual entries.
///
/// Handles both fixed-size and variable-size formats based on the leading
/// format byte.
pub fn deserialize_chunk_blob(blob: &[u8]) -> Result<Vec<Vec<u8>>, BulkAppendError> {
    if blob.is_empty() {
        return Ok(Vec::new());
    }

    match blob[0] {
        FORMAT_FIXED => deserialize_fixed(&blob[1..]),
        FORMAT_VARIABLE => deserialize_variable(&blob[1..]),
        other => Err(BulkAppendError::CorruptedData(format!(
            "unknown chunk blob format flag: 0x{:02x}",
            other
        ))),
    }
}

// -- Fixed-size format -------------------------------------------------------
// Layout: [0x01] [count: u32 BE] [entry_size: u32 BE] [entry_0] [entry_1] ...

fn serialize_fixed(entries: &[Vec<u8>]) -> Vec<u8> {
    let count = entries.len();
    let entry_size = entries[0].len();
    // 1 (flag) + 4 (count) + 4 (entry_size) + N * entry_size
    let total = 1 + 4 + 4 + count * entry_size;
    let mut blob = Vec::with_capacity(total);
    blob.push(FORMAT_FIXED);
    blob.extend_from_slice(&(count as u32).to_be_bytes());
    blob.extend_from_slice(&(entry_size as u32).to_be_bytes());
    for entry in entries {
        blob.extend_from_slice(entry);
    }
    blob
}

fn deserialize_fixed(data: &[u8]) -> Result<Vec<Vec<u8>>, BulkAppendError> {
    if data.len() < 8 {
        return Err(BulkAppendError::CorruptedData(
            "fixed chunk blob truncated at header".to_string(),
        ));
    }
    let count = u32::from_be_bytes(
        data[0..4]
            .try_into()
            .map_err(|_| BulkAppendError::CorruptedData("bad count bytes".into()))?,
    ) as usize;
    let entry_size = u32::from_be_bytes(
        data[4..8]
            .try_into()
            .map_err(|_| BulkAppendError::CorruptedData("bad entry_size bytes".into()))?,
    ) as usize;
    let payload = &data[8..];

    // DoS prevention: cap entry count to prevent huge allocations from
    // crafted headers (e.g. count=4B, entry_size=0 → 96 GB Vec alloc).
    if count > MAX_CHUNK_ENTRIES {
        return Err(BulkAppendError::CorruptedData(format!(
            "fixed chunk blob count {} exceeds maximum {}",
            count, MAX_CHUNK_ENTRIES
        )));
    }

    // Overflow-safe size check: use checked_mul to prevent wrapping
    let expected = count.checked_mul(entry_size).ok_or_else(|| {
        BulkAppendError::CorruptedData(format!(
            "fixed chunk blob count * entry_size overflows (count={}, entry_size={})",
            count, entry_size
        ))
    })?;
    if payload.len() != expected {
        return Err(BulkAppendError::CorruptedData(format!(
            "fixed chunk blob payload is {} bytes, expected {} (count={}, entry_size={})",
            payload.len(),
            expected,
            count,
            entry_size
        )));
    }

    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let start = i * entry_size;
        entries.push(payload[start..start + entry_size].to_vec());
    }
    Ok(entries)
}

// -- Variable-size format ----------------------------------------------------
// Layout: [0x00] [len_0: u32 BE] [entry_0] [len_1: u32 BE] [entry_1] ...

fn serialize_variable(entries: &[Vec<u8>]) -> Vec<u8> {
    let total: usize = 1 + entries.iter().map(|e| 4 + e.len()).sum::<usize>();
    let mut blob = Vec::with_capacity(total);
    blob.push(FORMAT_VARIABLE);
    for entry in entries {
        blob.extend_from_slice(&(entry.len() as u32).to_be_bytes());
        blob.extend_from_slice(entry);
    }
    blob
}

fn deserialize_variable(data: &[u8]) -> Result<Vec<Vec<u8>>, BulkAppendError> {
    let mut entries = Vec::new();
    let mut offset = 0;
    while offset < data.len() {
        if entries.len() >= MAX_CHUNK_ENTRIES {
            return Err(BulkAppendError::CorruptedData(format!(
                "variable chunk blob exceeds maximum {} entries",
                MAX_CHUNK_ENTRIES
            )));
        }
        if offset + 4 > data.len() {
            return Err(BulkAppendError::CorruptedData(
                "chunk blob truncated at length prefix".to_string(),
            ));
        }
        let len = u32::from_be_bytes(
            data[offset..offset + 4]
                .try_into()
                .map_err(|_| BulkAppendError::CorruptedData("bad length prefix bytes".into()))?,
        ) as usize;
        offset += 4;
        if offset + len > data.len() {
            return Err(BulkAppendError::CorruptedData(
                "chunk blob truncated at entry data".to_string(),
            ));
        }
        entries.push(data[offset..offset + len].to_vec());
        offset += len;
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_size_roundtrip() {
        let entries = vec![b"hello".to_vec(), b"world".to_vec(), b"12345".to_vec()];
        let blob = serialize_chunk_blob(&entries);
        assert_eq!(blob[0], FORMAT_FIXED);
        // 1 (flag) + 4 (count) + 4 (entry_size) + 3*5 = 24
        assert_eq!(blob.len(), 24);
        let decoded = deserialize_chunk_blob(&blob).expect("decode fixed blob");
        assert_eq!(entries, decoded);
    }

    #[test]
    fn variable_size_roundtrip() {
        let entries = vec![b"hi".to_vec(), b"world".to_vec(), b"!".to_vec()];
        let blob = serialize_chunk_blob(&entries);
        assert_eq!(blob[0], FORMAT_VARIABLE);
        let decoded = deserialize_chunk_blob(&blob).expect("decode variable blob");
        assert_eq!(entries, decoded);
    }

    #[test]
    fn empty_blob() {
        let entries: Vec<Vec<u8>> = vec![];
        let blob = serialize_chunk_blob(&entries);
        assert!(blob.is_empty());
        let decoded = deserialize_chunk_blob(&blob).expect("decode empty blob");
        assert!(decoded.is_empty());
    }

    #[test]
    fn single_entry_uses_fixed() {
        let entries = vec![b"only".to_vec()];
        let blob = serialize_chunk_blob(&entries);
        assert_eq!(blob[0], FORMAT_FIXED);
        let decoded = deserialize_chunk_blob(&blob).expect("decode single-entry blob");
        assert_eq!(entries, decoded);
    }

    #[test]
    fn variable_length_entries() {
        let entries = vec![
            vec![],
            vec![1],
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
            b"a long string value for testing".to_vec(),
        ];
        let blob = serialize_chunk_blob(&entries);
        assert_eq!(blob[0], FORMAT_VARIABLE);
        let decoded = deserialize_chunk_blob(&blob).expect("decode variable-length entries");
        assert_eq!(entries, decoded);
    }

    #[test]
    fn fixed_size_savings() {
        // 8 entries of 32 bytes each (typical hash commitments)
        let entries: Vec<Vec<u8>> = (0..8).map(|i| vec![i; 32]).collect();
        let blob = serialize_chunk_blob(&entries);
        assert_eq!(blob[0], FORMAT_FIXED);
        // Fixed: 1 + 4 + 4 + 8*32 = 265
        // Variable would be: 1 + 8*(4+32) = 289
        assert_eq!(blob.len(), 265);
        let decoded = deserialize_chunk_blob(&blob).expect("decode fixed-size savings blob");
        assert_eq!(entries, decoded);
    }

    #[test]
    fn fixed_zero_length_entries() {
        // All entries are empty -- count in header tells us how many
        let entries = vec![vec![], vec![], vec![]];
        let blob = serialize_chunk_blob(&entries);
        assert_eq!(blob[0], FORMAT_FIXED);
        // 1 (flag) + 4 (count=3) + 4 (entry_size=0) + 0 = 9
        assert_eq!(blob.len(), 9);
        let decoded = deserialize_chunk_blob(&blob).expect("decode zero-length entries blob");
        assert_eq!(entries, decoded);
    }

    #[test]
    fn truncated_variable_at_length() {
        let blob = vec![FORMAT_VARIABLE, 0, 0];
        let err = deserialize_chunk_blob(&blob).expect_err("should fail for truncated length");
        assert!(matches!(err, BulkAppendError::CorruptedData(_)));
    }

    #[test]
    fn truncated_variable_at_data() {
        let mut blob = vec![FORMAT_VARIABLE];
        blob.extend_from_slice(&10u32.to_be_bytes());
        blob.extend_from_slice(&[1, 2, 3]);
        let err = deserialize_chunk_blob(&blob).expect_err("should fail for truncated data");
        assert!(matches!(err, BulkAppendError::CorruptedData(_)));
    }

    #[test]
    fn truncated_fixed_at_header() {
        // Fixed format but only 5 bytes for the header (needs 8)
        let blob = vec![FORMAT_FIXED, 0, 0, 0, 1, 0];
        let err = deserialize_chunk_blob(&blob).expect_err("should fail for truncated header");
        assert!(matches!(err, BulkAppendError::CorruptedData(_)));
    }

    #[test]
    fn fixed_payload_size_mismatch() {
        // count=2, entry_size=3 -> expects 6 bytes payload, but has 7
        let mut blob = vec![FORMAT_FIXED];
        blob.extend_from_slice(&2u32.to_be_bytes());
        blob.extend_from_slice(&3u32.to_be_bytes());
        blob.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7]);
        let err = deserialize_chunk_blob(&blob).expect_err("should fail for payload mismatch");
        assert!(matches!(err, BulkAppendError::CorruptedData(_)));
    }

    #[test]
    fn unknown_format_flag() {
        let blob = vec![0xFF, 1, 2, 3];
        let err = deserialize_chunk_blob(&blob).expect_err("should fail for unknown format");
        assert!(matches!(err, BulkAppendError::CorruptedData(_)));
    }

    #[test]
    fn fixed_excessive_count_rejected() {
        // count exceeds MAX_CHUNK_ENTRIES → rejected before allocation
        let mut blob = vec![FORMAT_FIXED];
        blob.extend_from_slice(&((MAX_CHUNK_ENTRIES as u32) + 1).to_be_bytes());
        blob.extend_from_slice(&0u32.to_be_bytes());
        let err = deserialize_chunk_blob(&blob)
            .expect_err("should reject count exceeding MAX_CHUNK_ENTRIES");
        assert!(matches!(err, BulkAppendError::CorruptedData(_)));
    }

    #[test]
    fn fixed_huge_count_and_entry_size_rejected() {
        // count=u32::MAX exceeds MAX_CHUNK_ENTRIES, so it's rejected by the
        // count cap before reaching checked_mul. The checked_mul guard is
        // defense-in-depth for 32-bit platforms where the cap might not
        // prevent overflow.
        let mut blob = vec![FORMAT_FIXED];
        blob.extend_from_slice(&u32::MAX.to_be_bytes());
        blob.extend_from_slice(&u32::MAX.to_be_bytes());
        let err = deserialize_chunk_blob(&blob).expect_err("should reject huge count/entry_size");
        assert!(matches!(err, BulkAppendError::CorruptedData(_)));
    }
}
