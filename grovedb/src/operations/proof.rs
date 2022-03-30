use std::env::split_paths;

use crate::{util::merk_optional_tx, Element, Error, GroveDb, PathQuery, Query};

impl GroveDb {
    pub fn prove(&self, query: PathQuery) -> Result<Vec<u8>, Error> {
        // A path query has a path and then a query
        // First we find the merk at the defined path
        // if there is no merk found at that path, then we return an error
        // if there is then we construct a proof on the merk with the query
        // then subsequently construct proofs for all parents up to the
        // root tree.
        // As we do this we aggregate the proofs in a reproducible structure

        // 1. Get the merk at the path defined by the query
        let path_slices = query.path.iter().map(|x| x.as_slice()).collect::<Vec<_>>();

        // checks if the subtree exists
        self.check_subtree_exists_path_not_found(path_slices.clone(), None, None)?;

        merk_optional_tx!(self.db, path_slices.clone(), None, subtree, {
            // TODO: Not allowed to create proof for an empty tree (handle this)
            let proof = subtree.prove(query.query.query, None, None);
            dbg!(proof);
        });

        // Generate proof up to root
        let mut split_path = path_slices.split_last();
        while let Some((key, path_slice)) = split_path {
            if path_slice.is_empty() {
                dbg!("gotten to root");
            } else {
                let path_slices = path_slice.iter().map(|x| *x).collect::<Vec<_>>();

                merk_optional_tx!(self.db, path_slices, None, subtree, {
                    // TODO: Not allowed to create proof for an empty tree (handle this)
                    let mut query = Query::new();
                    query.insert_key(key.to_vec());

                    let proof = subtree.prove(query, None, None);
                    dbg!(proof);
                });
            }
            split_path = path_slice.split_last();
        }

        Err(Error::InvalidQuery("invalid query"))
    }

    pub fn execute_proof(proof: Vec<u8>) -> Result<([u8; 32], Vec<(Vec<u8>, Vec<u8>)>), Error> {
        Err(Error::InvalidProof("proof invalid"))
    }
}
