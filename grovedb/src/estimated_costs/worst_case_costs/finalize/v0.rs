//! `worst_case_finalize` **v0** (`GROVE_V6`), the first. See the module
//! docs in [`super`][`mod@super`].
//!
//! For every Merk that hosts a marked element: its open, what the finalize
//! does inside it (`add_worst_case_merk_finalize`), and for each marked
//! element the hashes of its new value.

use std::collections::HashMap;

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    estimated_costs::worst_case_costs::{add_worst_case_merk_finalize, WorstCaseLayerInformation},
    tree_type::TreeType,
    HASH_LENGTH_U32,
};
use grovedb_storage::worst_case_costs::WorstKeyLength;
use grovedb_version::version::GroveVersion;
use integer_encoding::VarInt;

use crate::{
    batch::KeyInfoPath,
    estimated_costs::{
        add_in_full, hash_blocks, hash_calls, marked_count, marked_hosts, missing_layer,
    },
    Error, GroveDb,
};

/// The hash calls of the value a finalize gives a marked element of at most
/// `biggest_host_element` bytes: the hash of the element with its length;
/// its combination with the roots below it, which for an indexed tree is
/// two hashes, and for one of several axes two more for the digest of their
/// roots; and the hash of the key with it.
fn marked_element_hashes(key_len: u32, biggest_host_element: u32) -> Result<u32, Error> {
    let overflow = || Error::Overflow("overflow for the hash calls of a marked element");
    let hashed = biggest_host_element
        .checked_add(biggest_host_element.required_space() as u32)
        .ok_or_else(overflow)?;
    let hashed_with_key = key_len.required_space() as u32 + key_len + HASH_LENGTH_U32;
    hash_blocks(hashed)
        .checked_add(2 + 2 + hash_blocks(hashed_with_key))
        .ok_or_else(overflow)
}

impl GroveDb {
    /// `worst_case_finalize` v0: see the module documentation.
    pub(super) fn worst_case_finalize_v0(
        stop_points: &[KeyInfoPath],
        paths: &HashMap<KeyInfoPath, WorstCaseLayerInformation>,
        biggest_host_element: u32,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        // The estimate in full, or a refusal with nothing added.
        match Self::worst_case_finalize_cost(
            stop_points,
            paths,
            biggest_host_element,
            grove_version,
        ) {
            Ok(cost) => Ok(()).wrap_with_cost(cost),
            Err(error) => Err(error).wrap_with_cost(OperationCost::default()),
        }
    }

    fn worst_case_finalize_cost(
        stop_points: &[KeyInfoPath],
        paths: &HashMap<KeyInfoPath, WorstCaseLayerInformation>,
        biggest_host_element: u32,
        grove_version: &GroveVersion,
    ) -> Result<OperationCost, Error> {
        let mut cost = OperationCost::default();
        for (host_path, keys) in marked_hosts(stop_points) {
            let layer = paths
                .get(&host_path)
                .ok_or_else(|| missing_layer("worst case finalize", &host_path))?;
            let marked = marked_count(keys.len())?;
            // The root Merk is opened by its root key record: the open has
            // its seek and its bytes.
            let mut host = OperationCost::default();
            Self::add_worst_case_get_merk_at_path_engine_free(
                &mut host,
                &host_path,
                TreeType::NormalTree,
                grove_version,
            )?;
            add_worst_case_merk_finalize(&mut host, layer, marked, grove_version)
                .map_err(Error::MerkError)?;
            for key in &keys {
                add_in_full(
                    &mut host,
                    hash_calls(marked_element_hashes(
                        u32::from(key.max_length()),
                        biggest_host_element,
                    )?),
                )?;
            }
            add_in_full(&mut cost, host)?;
        }
        Ok(cost)
    }
}
