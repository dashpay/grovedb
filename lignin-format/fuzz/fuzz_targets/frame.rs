//! The file header, the default record frame and the fixed-width ids
//! (`format.md` sections 3, 5 and 6).

#![no_main]

use libfuzzer_sys::fuzz_target;
use lignin_format::{
    header::{read_record, write_record},
    CellAddress, Checksum, CommitVersion, FileHeader, FileId, FileKind, FormatVersion, PageId,
    RecordHeader, SlotId,
};
use lignin_format_fuzz::{
    default_frame_len, ok, seal_file_header, sealed_records, take_u16, Input,
};

fuzz_target!(|data: &[u8]| {
    let Some(input) = Input::split(data) else {
        return;
    };
    ids(input.bytes);
    file_header(&input);
    records(&input);
});

fn ids(bytes: &[u8]) {
    for window in bytes.windows(8).take(64) {
        let page = PageId::decode(window[..4].try_into().unwrap());
        assert_eq!(PageId::encode_optional(page), window[..4]);
        let slot = SlotId::decode(window[..2].try_into().unwrap());
        assert_eq!(SlotId::encode_optional(slot), window[..2]);
        if let Some(slot) = slot {
            assert_eq!(SlotId::from_index(slot.index()), Some(slot));
        }
        if let Some(file) = ok(FileId::decode(window[..4].try_into().unwrap())) {
            assert_eq!(FileId::encode_optional(file), window[..4]);
        }
        if let Some(address) = ok(CellAddress::decode(window[..6].try_into().unwrap())) {
            assert_eq!(CellAddress::encode_optional(address), window[..6]);
            if let Some(address) = address {
                let _ = format!("{address} {address:?}");
            }
        }
        let version = CommitVersion::decode(window.try_into().unwrap());
        assert_eq!(CommitVersion::encode_optional(version), window);
        if let Some(version) = version {
            let _ = (version.next(), format!("{version} {version:?}"));
        }
        let checksum = Checksum::decode(window[..4].try_into().unwrap());
        assert_eq!(checksum.encode(), window[..4]);
        assert!(Checksum::of(&[window])
            .verify(&[&window[..4], &window[4..]])
            .is_ok());
    }
    let mut rest = bytes;
    while let (Some(raw), Some(code)) = (take_u16(&mut rest), take_u16(&mut rest)) {
        if let Some(version) = ok(FormatVersion::new(raw)) {
            assert_eq!(version.get(), raw);
            if let Some(kind) = ok(FileKind::from_code(code, version)) {
                assert_eq!(kind.code(), code);
            }
        }
    }
}

fn file_header(input: &Input<'_>) {
    let mut bytes = input.bytes.to_vec();
    if !input.keeps_checksums() {
        seal_file_header(&mut bytes);
    }
    let Some(header) = ok(FileHeader::decode(&bytes)) else {
        return;
    };
    assert_eq!(header.encode(), bytes[..FileHeader::ENCODED_LEN]);
    let again = FileHeader::new(header.kind(), header.version(), header.sequence());
    assert_eq!(ok(again), Some(header));
    assert_eq!(
        header.file_id().map(FileId::get),
        Some(header.sequence()).filter(|_| header.kind().is_numbered())
    );
}

fn records(input: &Input<'_>) {
    let bytes = sealed_records(input, default_frame_len);
    let max_payload_len = match input.control >> 1 {
        127 => usize::MAX,
        n => usize::from(n) << 9,
    };
    let mut rest = bytes.as_slice();
    while !rest.is_empty() {
        if let Some(header) = ok(RecordHeader::decode(rest, max_payload_len)) {
            assert_eq!(header.encode(), rest[..RecordHeader::ENCODED_LEN]);
            assert!(header.payload_len() <= max_payload_len.min(RecordHeader::MAX_PAYLOAD_LEN));
            let following = &rest[RecordHeader::ENCODED_LEN..];
            if let Some(payload) = ok(header.verified_payload(following)) {
                assert_eq!(ok(RecordHeader::for_payload(payload)), Some(header));
            }
        }
        let Some((payload, after)) = ok(read_record(rest, max_payload_len)) else {
            return;
        };
        let mut written = Vec::new();
        write_record(payload, &mut written).expect("a payload that was read");
        assert_eq!(written, rest[..rest.len() - after.len()]);
        rest = after;
    }
}
