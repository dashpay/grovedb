// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

#[cfg(any(feature = "full", feature = "verify"))]
use std::io::Read;
#[cfg(feature = "full")]
use std::io::Write;

use merk::{
    proofs::query::{Key, Path, ProvedKeyValue},
    CryptoHash,
};

use crate::operations::proof::verify::ProvedKeyValues;
#[cfg(any(feature = "full", feature = "verify"))]
use crate::Error;

#[cfg(any(feature = "full", feature = "verify"))]
pub const EMPTY_TREE_HASH: [u8; 32] = [0; 32];

#[cfg(any(feature = "full", feature = "verify"))]
#[derive(Debug, PartialEq, Eq)]
/// Proof type
// TODO: there might be a better name for this
pub enum ProofType {
    Merk,
    SizedMerk,
    EmptyTree,
    AbsentPath,
    PathInfo,
    Invalid,
}

#[cfg(any(feature = "full", feature = "verify"))]
impl From<ProofType> for u8 {
    fn from(proof_type: ProofType) -> Self {
        match proof_type {
            ProofType::Merk => 0x01,
            ProofType::SizedMerk => 0x02,
            ProofType::EmptyTree => 0x04,
            ProofType::AbsentPath => 0x05,
            ProofType::PathInfo => 0x06,
            ProofType::Invalid => 0x10,
        }
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
impl From<u8> for ProofType {
    fn from(val: u8) -> Self {
        match val {
            0x01 => ProofType::Merk,
            0x02 => ProofType::SizedMerk,
            0x04 => ProofType::EmptyTree,
            0x05 => ProofType::AbsentPath,
            0x06 => ProofType::PathInfo,
            _ => ProofType::Invalid,
        }
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
#[derive(Debug)]
/// Proof reader
pub struct ProofReader<'a> {
    proof_data: &'a [u8],
    is_verbose: bool,
}

#[cfg(any(feature = "full", feature = "verify"))]
impl<'a> ProofReader<'a> {
    /// New proof reader
    pub fn new(proof_data: &'a [u8]) -> Self {
        Self {
            proof_data,
            is_verbose: false,
        }
    }

    /// New proof reader with verbose_status
    pub fn new_with_verbose_status(proof_data: &'a [u8], is_verbose: bool) -> Self {
        Self {
            proof_data,
            is_verbose,
        }
    }

    // TODO: add documentation
    pub fn read_next_proof(&mut self, key: &[u8]) -> Result<(ProofType, Vec<u8>), Error> {
        if self.is_verbose {
            self.read_verbose_proof_at_key(key)
        } else {
            let (proof_type, proof, _) = self.read_proof_with_optional_type(None)?;
            Ok((proof_type, proof))
        }
    }

    /// Read the next proof, return the proof type
    pub fn read_proof(&mut self) -> Result<(ProofType, Vec<u8>, Option<Vec<u8>>), Error> {
        if self.is_verbose {
            self.read_verbose_proof_with_optional_type(None)
        } else {
            self.read_proof_with_optional_type(None)
        }
    }

    /// Read verbose proof
    pub fn read_verbose_proof(&mut self) -> Result<(ProofType, Vec<u8>, Option<Vec<u8>>), Error> {
        self.read_verbose_proof_with_optional_type(None)
    }

    /// Read proof of type
    pub fn read_proof_of_type(
        &mut self,
        expected_data_type: u8,
    ) -> Result<(Vec<u8>, Option<Vec<u8>>), Error> {
        match self.read_proof_with_optional_type(Some(expected_data_type)) {
            Ok((_, proof, ..)) => Ok((proof, None)),
            Err(e) => Err(e),
        }
    }

    /// Read verbose proof of type
    pub fn read_verbose_proof_of_type(
        &mut self,
        expected_data_type: u8,
    ) -> Result<(Vec<u8>, Option<Vec<u8>>), Error> {
        match self.read_verbose_proof_with_optional_type(Some(expected_data_type)) {
            Ok((_, proof, key)) => Ok((proof, key)),
            Err(e) => Err(e),
        }
    }

    /// Reads data from proof into slice of specific size
    fn read_into_slice(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        self.proof_data
            .read(buf)
            .map_err(|_| Error::CorruptedData(String::from("failed to read proof data")))
    }

    /// Read proof with optional type
    pub fn read_proof_with_optional_type(
        &mut self,
        expected_data_type_option: Option<u8>,
    ) -> Result<(ProofType, Vec<u8>, Option<Vec<u8>>), Error> {
        let (proof_type, proof, _) =
            self.read_proof_internal_with_optional_type(expected_data_type_option, false)?;
        Ok((proof_type, proof, None))
    }

    /// Read verbose proof with optional type
    pub fn read_verbose_proof_with_optional_type(
        &mut self,
        expected_data_type_option: Option<u8>,
    ) -> Result<(ProofType, Vec<u8>, Option<Vec<u8>>), Error> {
        let (proof_type, proof, key) =
            self.read_proof_internal_with_optional_type(expected_data_type_option, true)?;
        Ok((
            proof_type,
            proof,
            Some(key.ok_or(Error::InvalidProof(
                "key must exist for verbose merk proofs",
            ))?),
        ))
    }

    /// Read verbose proof at key
    /// Returns an error if it can't find a proof for that key
    pub fn read_verbose_proof_at_key(
        &mut self,
        expected_key: &[u8],
    ) -> Result<(ProofType, Vec<u8>), Error> {
        let (proof_type, proof, key) = loop {
            let (proof_type, proof, key) = self.read_verbose_proof()?;
            let key = key.expect("read_verbose_proof enforces that this exists");
            if key.as_slice() == expected_key {
                break (proof_type, proof, key);
            }
        };

        Ok((proof_type, proof))
    }

    /// Read proof with optional type
    pub fn read_proof_internal_with_optional_type(
        &mut self,
        expected_data_type_option: Option<u8>,
        is_verbose: bool,
    ) -> Result<(ProofType, Vec<u8>, Option<Vec<u8>>), Error> {
        let mut data_type = [0; 1];
        self.read_into_slice(&mut data_type)?;

        if let Some(expected_data_type) = expected_data_type_option {
            if data_type != [expected_data_type] {
                return Err(Error::InvalidProof("wrong data_type"));
            }
        }

        let proof_type: ProofType = data_type[0].into();

        if proof_type == ProofType::EmptyTree || proof_type == ProofType::AbsentPath {
            // TODO: should this really be none, don't we store this information
            //  fix this!!
            return Ok((proof_type, vec![], None));
        }

        let (proof, key) = if proof_type == ProofType::Merk || proof_type == ProofType::SizedMerk {
            // if verbose we need to read the key first
            let key = if is_verbose {
                let mut key_length = [0; 8_usize];
                self.read_into_slice(&mut key_length)?;
                let key_length = usize::from_be_bytes(key_length);

                let mut key = vec![0; key_length];
                self.read_into_slice(&mut key);

                Some(key)
            } else {
                None
            };

            let mut proof_length = [0; 8_usize];
            self.read_into_slice(&mut proof_length)?;
            let proof_length = usize::from_be_bytes(proof_length);

            let mut proof = vec![0; proof_length];
            self.read_into_slice(&mut proof)?;

            (proof, key)
        } else {
            return Err(Error::InvalidProof("expected merk or sized merk proof"));
        };

        Ok((proof_type, proof, key))
    }

    /// Reads path information from the proof vector
    pub fn read_path_info(&mut self) -> Result<Vec<Vec<u8>>, Error> {
        let mut data_type = [0; 1];
        self.read_into_slice(&mut data_type)?;

        if data_type != [ProofType::PathInfo.into()] {
            return Err(Error::InvalidProof("wrong data_type, expected path_info"));
        }

        let mut path = vec![];
        let mut path_slice_len = [0; 8_usize];
        self.read_into_slice(&mut path_slice_len)?;
        let path_slice_len = usize::from_be_bytes(path_slice_len);

        for _ in 0..path_slice_len {
            let mut path_len = [0; 8_usize];
            self.read_into_slice(&mut path_len)?;
            let path_len = usize::from_be_bytes(path_len);
            let mut path_value = vec![0; path_len];
            self.read_into_slice(&mut path_value)?;
            path.push(path_value);
        }

        Ok(path)
    }
}

#[cfg(feature = "full")]
/// Write to vec
pub fn write_to_vec<W: Write>(dest: &mut W, value: &[u8]) {
    dest.write_all(value).expect("TODO what if it fails?");
}

#[cfg(any(feature = "full", feature = "verify"))]
pub fn reduce_limit_and_offset_by(
    limit: &mut Option<u16>,
    offset: &mut Option<u16>,
    n: u16,
) -> bool {
    let mut skip_limit = false;
    let mut n = n;

    if let Some(offset_value) = *offset {
        if offset_value > 0 {
            if offset_value >= n {
                *offset = Some(offset_value - n);
                n = 0;
            } else {
                *offset = Some(0);
                n -= offset_value;
            }
            skip_limit = true;
        }
    }

    if let Some(limit_value) = *limit {
        if !skip_limit && limit_value > 0 {
            if limit_value >= n {
                *limit = Some(limit_value - n);
            } else {
                *limit = Some(0);
            }
        }
    }

    skip_limit
}

/// Proved path-key-values
pub type ProvedPathKeyValues = Vec<ProvedPathKeyValue>;

/// Proved path-key-value
#[cfg(any(feature = "full", feature = "verify"))]
#[derive(Debug, PartialEq)]
pub struct ProvedPathKeyValue {
    /// Path
    pub path: Path,
    /// Key
    pub key: Key,
    /// Value
    pub value: Vec<u8>,
    /// Proof
    pub proof: CryptoHash,
}

impl ProvedPathKeyValue {
    // TODO: make path a reference
    /// Consumes the ProvedKeyValue and returns a ProvedPathKeyValue given a
    /// Path
    pub fn from_proved_key_value(path: Path, proved_key_value: ProvedKeyValue) -> Self {
        Self {
            path,
            key: proved_key_value.key,
            value: proved_key_value.value,
            proof: proved_key_value.proof,
        }
    }

    /// Transforms multiple ProvedKeyValues to their equivalent
    /// ProvedPathKeyValue given a Path
    pub fn from_proved_key_values(path: Path, proved_key_values: ProvedKeyValues) -> Vec<Self> {
        proved_key_values
            .into_iter()
            .map(|pkv| Self::from_proved_key_value(path.clone(), pkv))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use merk::proofs::query::ProvedKeyValue;

    use crate::operations::proof::util::{ProofType, ProvedPathKeyValue};

    #[test]
    fn test_proof_type_encoding() {
        assert_eq!(0x01_u8, ProofType::Merk.into());
        assert_eq!(0x02_u8, ProofType::SizedMerk.into());
        assert_eq!(0x04_u8, ProofType::EmptyTree.into());
        assert_eq!(0x05_u8, ProofType::AbsentPath.into());
        assert_eq!(0x06_u8, ProofType::PathInfo.into());
        assert_eq!(0x10_u8, ProofType::Invalid.into());
    }

    #[test]
    fn test_proof_type_decoding() {
        assert_eq!(ProofType::Merk, 0x01_u8.into());
        assert_eq!(ProofType::SizedMerk, 0x02_u8.into());
        assert_eq!(ProofType::EmptyTree, 0x04_u8.into());
        assert_eq!(ProofType::AbsentPath, 0x05_u8.into());
        assert_eq!(ProofType::PathInfo, 0x06_u8.into());
        assert_eq!(ProofType::Invalid, 0x10_u8.into());
    }

    #[test]
    fn test_proved_path_from_single_proved_key_value() {
        let path = vec![b"1".to_vec(), b"2".to_vec()];
        let proved_key_value = ProvedKeyValue {
            key: b"a".to_vec(),
            value: vec![5, 6],
            proof: [0; 32],
        };
        let proved_path_key_value =
            ProvedPathKeyValue::from_proved_key_value(path.clone(), proved_key_value);
        assert_eq!(
            proved_path_key_value,
            ProvedPathKeyValue {
                path,
                key: b"a".to_vec(),
                value: vec![5, 6],
                proof: [0; 32]
            }
        );
    }

    #[test]
    fn test_many_proved_path_from_many_proved_key_value() {
        let path = vec![b"1".to_vec(), b"2".to_vec()];
        let proved_key_value_a = ProvedKeyValue {
            key: b"a".to_vec(),
            value: vec![5, 6],
            proof: [0; 32],
        };
        let proved_key_value_b = ProvedKeyValue {
            key: b"b".to_vec(),
            value: vec![5, 7],
            proof: [1; 32],
        };
        let proved_key_value_c = ProvedKeyValue {
            key: b"c".to_vec(),
            value: vec![6, 7],
            proof: [2; 32],
        };
        let proved_key_values = vec![proved_key_value_a, proved_key_value_b, proved_key_value_c];
        let proved_path_key_values =
            ProvedPathKeyValue::from_proved_key_values(path.clone(), proved_key_values);
        assert_eq!(proved_path_key_values.len(), 3);
        assert_eq!(
            proved_path_key_values[0],
            ProvedPathKeyValue {
                path: path.clone(),
                key: b"a".to_vec(),
                value: vec![5, 6],
                proof: [0; 32]
            }
        );
        assert_eq!(
            proved_path_key_values[1],
            ProvedPathKeyValue {
                path: path.clone(),
                key: b"b".to_vec(),
                value: vec![5, 7],
                proof: [1; 32]
            }
        );
        assert_eq!(
            proved_path_key_values[2],
            ProvedPathKeyValue {
                path,
                key: b"c".to_vec(),
                value: vec![6, 7],
                proof: [2; 32]
            }
        );
    }
}
