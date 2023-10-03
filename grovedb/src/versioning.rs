use std::io::Cursor;

use integer_encoding::{VarInt, VarIntReader};

use crate::Error;

pub(crate) const PROOF_VERSION: u32 = 1;

/// Reads a version number from the given byte slice using variable-length
/// encoding. Returns a Result containing the parsed u32 version number, or an
/// Error if the data is corrupted and could not be read.
pub fn read_proof_version(mut bytes: &[u8]) -> Result<u32, Error> {
    bytes
        .read_varint()
        .map_err(|_| Error::CorruptedData("could not read version info".to_string()))
}

/// Reads a version number from the given byte slice using variable-length
/// encoding, and returns the version number as well as a slice of the remaining
/// bytes.
pub fn read_and_consume_proof_version(bytes: &[u8]) -> Result<(u32, &[u8]), Error> {
    let mut cursor = Cursor::new(bytes);
    let version_number = cursor
        .read_varint()
        .map_err(|_| Error::CorruptedData("sdfs".to_string()))?;
    let version_length: usize = cursor.position() as usize;
    Ok((version_number, &bytes[version_length..]))
}

/// Encodes the given version number as variable-length bytes and adds it to the
/// beginning of the given Vec<u8>, returning the modified vector.
pub(crate) fn prepend_version_to_bytes(mut bytes: Vec<u8>, version: u32) -> Result<Vec<u8>, Error> {
    let version_bytes = version.encode_var_vec();
    bytes.splice(..0, version_bytes);
    Ok(bytes)
}

#[cfg(test)]
mod tests {

    use crate::versioning::{
        prepend_version_to_bytes, read_and_consume_proof_version, read_proof_version,
    };

    #[test]
    fn read_correct_version() {
        let data = vec![1, 2, 3];
        let version = 500_u32;

        // prepend the version information to the data vector
        let new_data = prepend_version_to_bytes(data, version).unwrap();
        assert_eq!(new_data, [244, 3, 1, 2, 3]);

        // show that read_version doesn't consume
        assert_eq!(read_proof_version(new_data.as_slice()).unwrap(), 500);
        assert_eq!(new_data, [244, 3, 1, 2, 3]);

        // show that we consume the version number and return the remaining vector
        let (version_number, data_vec) = read_and_consume_proof_version(&new_data).unwrap();
        assert_eq!(version_number, 500_u32);
        assert_eq!(data_vec, [1, 2, 3]);
    }
}
