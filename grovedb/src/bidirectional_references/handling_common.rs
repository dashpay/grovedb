use std::io::Write;

use bincode::{config, Decode, Encode};
use bitvec::{array::BitArray, order::Lsb0};
use grovedb_costs::{cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt};

use super::SlotIdx;
use crate::{merk_cache::MerkHandle, reference_path::ReferencePathType, Error};

pub(super) const META_BACKWARD_REFERENCES_PREFIX: &[u8] = b"refs";

#[derive(Debug, Encode, Decode, PartialEq)]
pub(crate) struct BackwardReference {
    pub(crate) inverted_reference: ReferencePathType,
    pub(crate) cascade_on_update: bool,
}

impl BackwardReference {
    pub fn serialize(&self) -> Result<Vec<u8>, Error> {
        let config = config::standard().with_big_endian().with_no_limit();
        bincode::encode_to_vec(self, config).map_err(|e| {
            Error::CorruptedData(format!("unable to serialize backward reference {}", e))
        })
    }

    pub fn deserialize(bytes: &[u8]) -> Result<BackwardReference, Error> {
        let config = config::standard().with_big_endian().with_no_limit();
        Ok(bincode::decode_from_slice(bytes, config)
            .map_err(|e| Error::CorruptedData(format!("unable to deserialize element {}", e)))?
            .0)
    }
}

pub(super) type Prefix = Vec<u8>;

pub(super) fn make_meta_prefix(key: &[u8]) -> Vec<u8> {
    let mut backrefs_for_key = META_BACKWARD_REFERENCES_PREFIX.to_vec();
    backrefs_for_key.extend_from_slice(&key.len().to_be_bytes());
    backrefs_for_key.extend_from_slice(key);

    backrefs_for_key
}

/// Get bitvec of backward references' slots for a key of a subtree.
/// Prefix for a Merk's meta storage is made of constant keyword, lenght of the
/// key and the key itself. Under the prefix GroveDB stores bitvec, and slots
/// for backward references are integers appended to the prefix.
pub(super) fn get_backward_references_bitvec(
    merk: &mut MerkHandle<'_, '_>,
    key: &[u8],
) -> CostResult<(Prefix, BitArray<[u32; 1], Lsb0>), Error> {
    let mut cost = Default::default();

    let backrefs_for_key = make_meta_prefix(key);

    let stored_bytes = cost_return_on_error!(
        &mut cost,
        merk.for_merk(|m| m
            .get_meta(backrefs_for_key.clone())
            .map_ok(|opt_v| opt_v.map(|v| v.to_vec()))
            .map_err(Error::MerkError))
    );

    let bits: BitArray<[u32; 1], Lsb0> = if let Some(bytes) = stored_bytes {
        cost_return_on_error_no_add!(
            cost,
            bytes
                .try_into()
                .map(|b| BitArray::new([u32::from_be_bytes(b)]))
                .map_err(|_| Error::InternalError(
                    "backward references' bitvec is expected to be 4 bytes".to_owned()
                ))
        )
    } else {
        Default::default()
    };

    Ok((backrefs_for_key, bits)).wrap_with_cost(cost)
}

/// Return a vector of backward references to the item
pub(super) fn get_backward_references(
    merk: &mut MerkHandle<'_, '_>,
    key: &[u8],
) -> CostResult<Vec<(SlotIdx, BackwardReference)>, Error> {
    let mut cost = Default::default();

    let (prefix, bits) =
        cost_return_on_error!(&mut cost, get_backward_references_bitvec(merk, key));

    let mut backward_references = Vec::new();

    for idx in bits.iter_ones() {
        let mut indexed_prefix = prefix.clone();
        write!(&mut indexed_prefix, "{idx}").expect("no io involved");

        let bytes_opt = cost_return_on_error!(
            &mut cost,
            merk.for_merk(|m| m
                .get_meta(indexed_prefix)
                .map_err(Error::MerkError)
                .map_ok(|opt_v| opt_v.map(|v| v.to_vec())))
        );

        let bytes = cost_return_on_error_no_add!(
            cost,
            bytes_opt.ok_or_else(|| {
                Error::InternalError(
                    "backward references bitvec and slot are out of sync".to_owned(),
                )
            })
        );

        backward_references.push((
            idx,
            cost_return_on_error_no_add!(cost, BackwardReference::deserialize(&bytes)),
        ));
    }

    Ok(backward_references).wrap_with_cost(cost)
}
