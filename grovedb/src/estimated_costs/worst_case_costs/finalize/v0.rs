//! `worst_case_finalize` **v0** (`GROVE_V6`), the first. See the module
//! docs in [`super`][`mod@super`].
//!
//! For every Merk that hosts a marked element: its open, what the finalize
//! does inside it (`add_worst_case_merk_finalize`), and for each marked
//! element the hashes of its new value.

use std::collections::HashMap;

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    estimated_costs::worst_case_costs::{
        add_worst_case_merk_finalize, WorstCaseLayerInformation, MERK_BIGGEST_KEY_SIZE,
    },
    tree_type::TreeType,
    HASH_BLOCK_SIZE_U32, HASH_LENGTH_U32,
};
use grovedb_storage::worst_case_costs::WorstKeyLength;
use grovedb_version::version::GroveVersion;
use integer_encoding::VarInt;

use super::super::WORST_CASE_FLAGS_LEN;
use crate::{
    batch::KeyInfoPath,
    estimated_costs::{add_in_full, hash_calls, marked_count, marked_hosts, missing_layer},
    Error, GroveDb,
};

/// The bytes a tree element is taken to serialize to at the most: its flags
/// at `WORST_CASE_FLAGS_LEN`, which is what the worst-case estimates of
/// every grove version take flags to be and no limit of GroveDB; the root
/// key of its Merk and of the secondaries of three axes, each at the
/// longest a key can be with its length; and 64 bytes for its tags, its
/// aggregates and its wrapper.
const BIGGEST_TREE_ELEMENT_LEN: u32 = WORST_CASE_FLAGS_LEN + 4 * (MERK_BIGGEST_KEY_SIZE + 3) + 64;

/// The hash calls of the value a finalize gives a marked element: the hash
/// of the element with its length; its combination with the roots below it,
/// which for an indexed tree is two hashes, and for one of several axes two
/// more for the digest of their roots; and the hash of the key with it.
fn marked_element_hashes(key_len: u32) -> u32 {
    let hashed = BIGGEST_TREE_ELEMENT_LEN + BIGGEST_TREE_ELEMENT_LEN.required_space() as u32;
    let value_hash = 1 + (hashed - 1) / HASH_BLOCK_SIZE_U32;
    let hashed_with_key = key_len.required_space() as u32 + key_len + HASH_LENGTH_U32;
    let key_value_hash = 1 + (hashed_with_key - 1) / HASH_BLOCK_SIZE_U32;
    value_hash + 2 + 2 + key_value_hash
}

impl GroveDb {
    /// `worst_case_finalize` v0: see the module documentation.
    pub(super) fn worst_case_finalize_v0(
        stop_points: &[KeyInfoPath],
        paths: &HashMap<KeyInfoPath, WorstCaseLayerInformation>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        // The estimate in full, or a refusal with nothing added.
        match Self::worst_case_finalize_cost(stop_points, paths, grove_version) {
            Ok(cost) => Ok(()).wrap_with_cost(cost),
            Err(error) => Err(error).wrap_with_cost(OperationCost::default()),
        }
    }

    fn worst_case_finalize_cost(
        stop_points: &[KeyInfoPath],
        paths: &HashMap<KeyInfoPath, WorstCaseLayerInformation>,
        grove_version: &GroveVersion,
    ) -> Result<OperationCost, Error> {
        let mut cost = OperationCost::default();
        for (host_path, keys) in marked_hosts(stop_points) {
            let layer = paths
                .get(&host_path)
                .ok_or_else(|| missing_layer("worst case finalize", &host_path))?;
            let marked = marked_count(keys.len())?;
            let mut host = OperationCost::default();
            Self::add_worst_case_get_merk_at_path_engine_free(
                &mut host,
                &host_path,
                TreeType::NormalTree,
                grove_version,
            )?;
            add_worst_case_merk_finalize(&mut host, layer, marked, grove_version)
                .map_err(Error::MerkError)?;
            if host_path.is_empty() {
                // The root Merk is opened by its root key record, whose
                // seek the open has: its bytes are those the worst case
                // takes a key to be.
                add_in_full(
                    &mut host,
                    OperationCost {
                        storage_loaded_bytes: u64::from(MERK_BIGGEST_KEY_SIZE),
                        ..Default::default()
                    },
                )?;
            }
            for key in &keys {
                add_in_full(
                    &mut host,
                    hash_calls(marked_element_hashes(u32::from(key.max_length()))),
                )?;
            }
            add_in_full(&mut cost, host)?;
        }
        Ok(cost)
    }
}
