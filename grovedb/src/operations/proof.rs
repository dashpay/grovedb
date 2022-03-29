use crate::{Error, GroveDb, PathQuery};

impl GroveDb {
    pub fn proof(query: PathQuery) -> Result<Vec<u8>, Error> {
        Err(Error::InvalidQuery("invalid query"))
    }

    pub fn execute_proof(proof: Vec<u8>) -> Result<([u8; 32], Vec<(Vec<u8>, Vec<u8>)>), Error> {
        Err(Error::InvalidProof("proof invalid"))
    }
}
