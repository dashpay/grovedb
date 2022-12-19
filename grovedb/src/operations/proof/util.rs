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
pub struct ProofReader<'a> {
    proof_data: &'a [u8],
}

#[cfg(any(feature = "full", feature = "verify"))]
impl<'a> ProofReader<'a> {
    pub fn new(proof_data: &'a [u8]) -> Self {
        Self { proof_data }
    }

    pub fn read_proof(&mut self) -> Result<(ProofType, Vec<u8>), Error> {
        self.read_proof_with_optional_type(None)
    }

    pub fn read_proof_of_type(&mut self, expected_data_type: u8) -> Result<Vec<u8>, Error> {
        match self.read_proof_with_optional_type(Some(expected_data_type)) {
            Ok((_, proof)) => Ok(proof),
            Err(e) => Err(e),
        }
    }

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
pub fn write_to_vec<W: Write>(dest: &mut W, value: &[u8]) {
    dest.write_all(value).expect("TODO what if it fails?");
}
