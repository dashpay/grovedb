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
