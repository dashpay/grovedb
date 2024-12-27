use std::collections::HashSet;

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{CryptoHash, Merk};
use grovedb_path::{SubtreePath, SubtreePathBuilder};
use grovedb_storage::{
    rocksdb_storage::{PrefixedRocksDbTransactionContext, RocksDbStorage},
    Storage, StorageBatch,
};
use grovedb_version::{check_grovedb_v0_with_cost, version::GroveVersion};
use grovedb_visualize::DebugByteVectors;

use crate::{
    bidirectional_references::BidirectionalReference,
    merk_cache::{MerkCache, MerkHandle},
    operations::MAX_REFERENCE_HOPS,
    reference_path::ReferencePathType,
    Element, Error, Transaction, TransactionArg,
};

pub(crate) enum TxRef<'a, 'db: 'a> {
    Owned(Transaction<'db>),
    Borrowed(&'a Transaction<'db>),
}

impl<'a, 'db> TxRef<'a, 'db> {
    pub(crate) fn new(db: &'db RocksDbStorage, transaction_arg: TransactionArg<'db, 'a>) -> Self {
        if let Some(tx) = transaction_arg {
            Self::Borrowed(tx)
        } else {
            Self::Owned(db.start_transaction())
        }
    }

    /// Commit the transaction if it wasn't received from outside
    pub(crate) fn commit_local(self) -> Result<(), Error> {
        match self {
            TxRef::Owned(tx) => tx
                .commit()
                .map_err(|e| grovedb_storage::Error::from(e).into()),
            TxRef::Borrowed(_) => Ok(()),
        }
    }
}

impl<'a, 'db> AsRef<Transaction<'db>> for TxRef<'a, 'db> {
    fn as_ref(&self) -> &Transaction<'db> {
        match self {
            TxRef::Owned(tx) => tx,
            TxRef::Borrowed(tx) => tx,
        }
    }
}

pub(crate) fn open_transactional_merk_at_path<'db, 'b, B>(
    db: &'db RocksDbStorage,
    path: SubtreePath<'b, B>,
    tx: &'db Transaction,
    batch: Option<&'db StorageBatch>,
    grove_version: &GroveVersion,
) -> CostResult<Merk<PrefixedRocksDbTransactionContext<'db>>, Error>
where
    B: AsRef<[u8]> + 'b,
{
    let mut cost = OperationCost::default();

    let storage = db
        .get_transactional_storage_context(path.clone(), batch, tx)
        .unwrap_add_cost(&mut cost);
    if let Some((parent_path, parent_key)) = path.derive_parent() {
        let parent_storage = db
            .get_transactional_storage_context(parent_path.clone(), batch, tx)
            .unwrap_add_cost(&mut cost);
        let element = cost_return_on_error!(
            &mut cost,
            Element::get_from_storage(&parent_storage, parent_key, grove_version).map_err(|e| {
                Error::InvalidParentLayerPath(format!(
                    "could not get key {} for parent {:?} of subtree: {}",
                    hex::encode(parent_key),
                    DebugByteVectors(parent_path.to_vec()),
                    e
                ))
            })
        );
        let is_sum_tree = element.is_sum_tree();
        if let Element::Tree(root_key, _) | Element::SumTree(root_key, ..) = element {
            Merk::open_layered_with_root_key(
                storage,
                root_key,
                is_sum_tree,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .map_err(|_| {
                Error::CorruptedData("cannot open a subtree with given root key".to_owned())
            })
            .add_cost(cost)
        } else {
            Err(Error::CorruptedPath(
                "cannot open a subtree as parent exists but is not a tree".to_string(),
            ))
            .wrap_with_cost(cost)
        }
    } else {
        Merk::open_base(
            storage,
            false,
            Some(&Element::value_defined_cost_for_serialized_value),
            grove_version,
        )
        .map_err(|_| Error::CorruptedData("cannot open a the root subtree".to_owned()))
        .add_cost(cost)
    }
}

// /// Wrapper type that keeps path and key used to perform an operation.
// pub(crate) struct WithOrigin<'b, 'k, B, T> {
//     pub(crate) path: SubtreePath<'b, B>,
//     pub(crate) key: &'k [u8],
//     pub(crate) value: T,
// }

// impl<'b, 'k, B, T> WithOrigin<'b, 'k, B, T> {
//     pub(crate) fn run(
//         path: SubtreePath<'b, B>,
//         key: &'k [u8],
//         f: impl FnOnce(SubtreePath<'b, B>, &'k [u8]) -> T,
//     ) -> Self {
//         WithOrigin {
//             path: path.clone(),
//             key,
//             value: f(path, key),
//         }
//     }
// }

// impl<'b, 'k, B, T, E> WithOrigin<'b, 'k, B, CostResult<T, E>> {
//     pub(crate) fn into_cost_result(self) -> CostResult<WithOrigin<'b, 'k, B,
// T>, E> {         let mut cost = Default::default();
//         let value = cost_return_on_error!(&mut cost, self.value);
//         Ok(WithOrigin {
//             value,
//             path: self.path,
//             key: self.key,
//         })
//         .wrap_with_cost(cost)
//     }
// }

pub(crate) struct ResolvedReference<'db, 'b, 'c, B> {
    pub target_merk: MerkHandle<'db, 'c>,
    pub target_path: SubtreePathBuilder<'b, B>,
    pub target_key: Vec<u8>,
    pub target_element: Element,
    pub target_node_value_hash: CryptoHash,
}

pub(crate) fn follow_reference<'db, 'b, 'c, B: AsRef<[u8]>>(
    merk_cache: &'c MerkCache<'db, 'b, B>,
    path: SubtreePathBuilder<'b, B>,
    key: &[u8],
    ref_path: ReferencePathType,
) -> CostResult<ResolvedReference<'db, 'b, 'c, B>, Error> {
    check_grovedb_v0_with_cost!(
        "follow_reference",
        merk_cache
            .version
            .grovedb_versions
            .operations
            .get
            .follow_reference
    );

    let mut cost = OperationCost::default();

    let mut hops_left = MAX_REFERENCE_HOPS;
    let mut visited = HashSet::new();

    let mut qualified_path = path.clone();
    qualified_path.push_segment(key);

    visited.insert(qualified_path);

    let mut current_path = path;
    let mut current_key = key.to_vec();
    let mut current_ref = ref_path;

    while hops_left > 0 {
        let referred_qualified_path = cost_return_on_error_no_add!(
            cost,
            current_ref.absolute_qualified_path(current_path, &current_key)
        );

        if !visited.insert(referred_qualified_path.clone()) {
            return Err(Error::CyclicReference).wrap_with_cost(cost);
        }

        let Some((referred_path, referred_key)) = referred_qualified_path.derive_parent_owned()
        else {
            return Err(Error::InvalidCodeExecution("empty reference")).wrap_with_cost(cost);
        };

        let mut referred_merk =
            cost_return_on_error!(&mut cost, merk_cache.get_merk(referred_path.clone()));
        let (element, value_hash) = cost_return_on_error!(
            &mut cost,
            referred_merk
                .for_merk(|m| {
                    Element::get_with_value_hash(m, &referred_key, true, merk_cache.version)
                })
                .map_err(|e| match e {
                    Error::PathKeyNotFound(s) => Error::CorruptedReferencePathKeyNotFound(s),
                    e => e,
                })
        );

        match element {
            Element::Reference(ref_path, ..)
            | Element::BidirectionalReference(
                BidirectionalReference {
                    forward_reference_path: ref_path,
                    ..
                },
                ..,
            ) => {
                current_path = referred_path;
                current_key = referred_key;
                current_ref = ref_path;
                hops_left -= 1;
            }
            e => {
                return Ok(ResolvedReference {
                    target_merk: referred_merk,
                    target_path: referred_path,
                    target_key: referred_key,
                    target_element: e,
                    target_node_value_hash: value_hash,
                })
                .wrap_with_cost(cost)
            }
        }
    }

    Err(Error::ReferenceLimit).wrap_with_cost(cost)
}

/// Follow references stopping at the immediate element without following
/// further.
pub(crate) fn follow_reference_once<'db, 'b, 'c, B: AsRef<[u8]>>(
    merk_cache: &'c MerkCache<'db, 'b, B>,
    path: SubtreePathBuilder<'b, B>,
    key: &[u8],
    ref_path: ReferencePathType,
) -> CostResult<ResolvedReference<'db, 'b, 'c, B>, Error> {
    check_grovedb_v0_with_cost!(
        "follow_reference_once",
        merk_cache
            .version
            .grovedb_versions
            .operations
            .get
            .follow_reference_once
    );

    let mut cost = OperationCost::default();

    let referred_qualified_path =
        cost_return_on_error_no_add!(cost, ref_path.absolute_qualified_path(path.clone(), key));

    let Some((referred_path, referred_key)) = referred_qualified_path.derive_parent_owned() else {
        return Err(Error::InvalidCodeExecution("empty reference")).wrap_with_cost(cost);
    };

    if path == referred_path && key == referred_key {
        return Err(Error::CyclicReference).wrap_with_cost(cost);
    }

    let mut referred_merk =
        cost_return_on_error!(&mut cost, merk_cache.get_merk(referred_path.clone()));
    let (element, value_hash) = cost_return_on_error!(
        &mut cost,
        referred_merk
            .for_merk(|m| {
                Element::get_with_value_hash(m, &referred_key, true, merk_cache.version)
            })
            .map_err(|e| match e {
                Error::PathKeyNotFound(s) => Error::CorruptedReferencePathKeyNotFound(s),
                e => e,
            })
    );

    Ok(ResolvedReference {
        target_merk: referred_merk,
        target_path: referred_path,
        target_key: referred_key,
        target_element: element,
        target_node_value_hash: value_hash,
    })
    .wrap_with_cost(cost)
}

// #[cfg(test)]
// mod tests {
//     use pretty_assertions::assert_eq;

//     use super::*;
//     use crate::tests::{make_deep_tree, TEST_LEAF};

//     #[test]
//     fn with_origin() {
//         let version = GroveVersion::latest();
//         let db = make_deep_tree(&version);

//         let wo = WithOrigin::run(
//             SubtreePath::from(&[TEST_LEAF, b"innertree"]),
//             b"key1",
//             |path, key| db.get(path, key, None, &version),
//         );

//         assert_eq!(wo.path, SubtreePath::from(&[TEST_LEAF, b"innertree"]));
//         assert_eq!(wo.key, b"key1");

//         let with_origin_cost_result: CostResult<_, _> =
// wo.into_cost_result();

//         assert_eq!(
//             with_origin_cost_result.unwrap().unwrap().value,
//             Element::Item(b"value1".to_vec(), None)
//         );
//     }
// }
