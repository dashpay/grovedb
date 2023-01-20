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
pub enum ProofType {
    Merk,
    SizedMerk,
    Root,
    EmptyTree,
    AbsentPath,
    Invalid,
}

#[cfg(any(feature = "full", feature = "verify"))]
impl From<ProofType> for u8 {
    fn from(proof_type: ProofType) -> Self {
        match proof_type {
            ProofType::Merk => 0x01,
            ProofType::SizedMerk => 0x02,
            ProofType::Root => 0x03,
            ProofType::EmptyTree => 0x04,
            ProofType::AbsentPath => 0x05,
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
            0x03 => ProofType::Root,
            0x04 => ProofType::EmptyTree,
            0x05 => ProofType::AbsentPath,
            _ => ProofType::Invalid,
        }
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
#[derive(Debug)]
/// Proof reader
pub struct ProofReader<'a> {
    proof_data: &'a [u8],
}

#[cfg(any(feature = "full", feature = "verify"))]
impl<'a> ProofReader<'a> {
    /// New proof data
    pub fn new(proof_data: &'a [u8]) -> Self {
        Self { proof_data }
    }

    /// Read proof
    pub fn read_proof(&mut self) -> Result<(ProofType, Vec<u8>), Error> {
        self.read_proof_with_optional_type(None)
    }

    /// Read proof of type
    pub fn read_proof_of_type(&mut self, expected_data_type: u8) -> Result<Vec<u8>, Error> {
        match self.read_proof_with_optional_type(Some(expected_data_type)) {
            Ok((_, proof)) => Ok(proof),
            Err(e) => Err(e),
        }
    }

    /// Read proof with optional type
    pub fn read_proof_with_optional_type(
        &mut self,
        expected_data_type_option: Option<u8>,
    ) -> Result<(ProofType, Vec<u8>), Error> {
        let mut data_type = [0; 1];
        self.proof_data
            .read(&mut data_type)
            .map_err(|_| Error::CorruptedData(String::from("failed to read proof data")))?;

        if let Some(expected_data_type) = expected_data_type_option {
            if data_type != [expected_data_type] {
                return Err(Error::InvalidProof("wrong data_type"));
            }
        }

        let proof_type: ProofType = data_type[0].into();

        if proof_type == ProofType::EmptyTree || proof_type == ProofType::AbsentPath {
            return Ok((proof_type, vec![]));
        }

        let mut proof_length = [0; 8_usize];
        self.proof_data
            .read(&mut proof_length)
            .map_err(|_| Error::CorruptedData(String::from("failed to read proof data")))?;
        let proof_length = usize::from_be_bytes(proof_length);

        let mut proof = vec![0; proof_length];
        self.proof_data
            .read(&mut proof)
            .map_err(|_| Error::CorruptedData(String::from("failed to read proof data")))?;

        Ok((proof_type, proof))
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

    use crate::operations::proof::util::ProvedPathKeyValue;

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
