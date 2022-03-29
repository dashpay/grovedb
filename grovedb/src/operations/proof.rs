use crate::{
    util::{merk_optional_tx},
    Element, Error, GroveDb, PathQuery};

impl GroveDb {
    pub fn proof(&self, query: PathQuery) -> Result<Vec<u8>, Error> {
        // A path query has a path and then a query
        // First we find the merk at the defined path
        // if there is no merk found at that path, then we return an error
        // if there is then we construct a proof on the merk with the query
        // then subsequently construct proofs for all parents up to the
        // root tree.
        // As we do this we aggregate the proofs in a reproducible structure

        // 1. Get the merk at the path defined by the query
        merk_optional_tx!(self.db, query.path.into_iter(), None, subtree, {
            dbg!(subtree)
        });

        Err(Error::InvalidQuery("invalid query"))
    }

    pub fn execute_proof(proof: Vec<u8>) -> Result<([u8; 32], Vec<(Vec<u8>, Vec<u8>)>), Error> {
        Err(Error::InvalidProof("proof invalid"))
    }
}