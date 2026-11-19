use std::cell::Cell;

use grovedb_costs::CostResult;
#[cfg(feature = "lignin_storage")]
use grovedb_costs::{cost_return_on_error_default, CostsExt, OperationCost};
use grovedb_version::version::GroveVersion;

#[cfg(feature = "lignin_storage")]
use crate::store::lignin::LigninNodeStore;
use crate::store::NodeStore;
use crate::{
    tree::kv::ValueDefinedCostType,
    tree_type::TreeType,
    Error, Merk, MerkType,
    MerkType::{BaseMerk, LayeredMerk, StandaloneMerk},
};

impl<'db, S> Merk<S>
where
    S: NodeStore<'db>,
{
    /// Open empty tree
    pub fn open_empty(storage: S, merk_type: MerkType, tree_type: TreeType) -> Self {
        Self {
            tree: Cell::new(None),
            root_tree_key: Cell::new(None),
            storage,
            merk_type,
            meta_cache: Default::default(),
            tree_type,
        }
    }

    /// Open standalone tree
    pub fn open_standalone(
        storage: S,
        tree_type: TreeType,
        value_defined_cost_fn: Option<
            impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<Self, Error> {
        let mut merk = Self {
            tree: Cell::new(None),
            root_tree_key: Cell::new(None),
            storage,
            merk_type: StandaloneMerk,
            tree_type,
            meta_cache: Default::default(),
        };

        merk.load_base_root(value_defined_cost_fn, grove_version)
            .map_ok(|_| merk)
    }

    /// Open base tree
    pub fn open_base(
        storage: S,
        tree_type: TreeType,
        value_defined_cost_fn: Option<
            impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<Self, Error> {
        let mut merk = Self {
            tree: Cell::new(None),
            root_tree_key: Cell::new(None),
            storage,
            merk_type: BaseMerk,
            tree_type,
            meta_cache: Default::default(),
        };

        merk.load_base_root(value_defined_cost_fn, grove_version)
            .map_ok(|_| merk)
    }

    /// Open layered tree with root key
    pub fn open_layered_with_root_key(
        storage: S,
        root_key: Option<Vec<u8>>,
        tree_type: TreeType,
        value_defined_cost_fn: Option<
            impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<Self, Error> {
        let mut merk = Self {
            tree: Cell::new(None),
            root_tree_key: Cell::new(root_key),
            storage,
            merk_type: LayeredMerk,
            tree_type,
            meta_cache: Default::default(),
        };

        merk.load_root(value_defined_cost_fn, grove_version)
            .map_ok(|_| merk)
    }
}

#[cfg(feature = "lignin_storage")]
impl<'db> Merk<LigninNodeStore<'db>> {
    /// Opens the Merk of the handle's subtree as the subtree directory of
    /// LigninDB has it: the open by prefix, with nothing but the prefix
    /// known. `None` is a subtree that is not entered.
    ///
    /// The directory gives the tree type, the key of the root node and
    /// whether an element hosts the Merk, in one lookup that is charged
    /// nothing. The open is then the one a caller that knew them would
    /// make, and costs what that costs:
    ///
    /// - a Merk under a parent element is opened with the root key the
    ///   directory names, which is the one its parent element holds
    ///   ([`Self::open_layered_with_root_key`]);
    /// - a Merk with no host is opened as a base Merk
    ///   ([`Self::open_base`]): it keeps its root key in a record of its
    ///   own, and that record and the directory have to name the same
    ///   node.
    pub fn open_from_directory(
        storage: LigninNodeStore<'db>,
        value_defined_cost_fn: Option<
            impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<Self>, Error> {
        let Some(entry) = cost_return_on_error_default!(storage.entry()) else {
            return Ok(None).wrap_with_cost(Default::default());
        };
        let tree_type = cost_return_on_error_default!(entry.tree_type());
        if entry.host.is_some() {
            return Self::open_layered_with_root_key(
                storage,
                entry.root_key,
                tree_type,
                value_defined_cost_fn,
                grove_version,
            )
            .map_ok(Some);
        }
        Self::open_base(storage, tree_type, value_defined_cost_fn, grove_version).map(|opened| {
            let merk = opened?;
            if merk.root_key() != entry.root_key {
                return Err(Error::CorruptedState(
                    "the root key record of a Merk and the subtree directory name different roots",
                ));
            }
            Ok(Some(merk))
        })
    }
}

#[cfg(all(test, merk_test_engine))]
mod test {
    use grovedb_costs::OperationCost;
    use grovedb_path::SubtreePath;
    use grovedb_version::version::GroveVersion;
    use tempfile::TempDir;

    use crate::{
        test_utils::{SuiteBatch, SuiteEngine, TempEngine, TestEngine},
        tree::kv::ValueDefinedCostType,
        tree_type::TreeType,
        Merk, Op,
        TreeFeatureType::BasicMerkNode,
    };

    #[test]
    fn test_reopen_root_hash() {
        let grove_version = GroveVersion::latest();
        let tmp_dir = TempDir::new().expect("cannot open tempdir");
        let storage = SuiteEngine::open(tmp_dir.path());
        let batch = SuiteBatch::default();
        let transaction = storage.start_transaction();

        let test_prefix = [b"ayy"];

        let mut merk = Merk::open_base(
            storage
                .subtree(
                    SubtreePath::from(test_prefix.as_ref()),
                    Some(&batch),
                    &transaction,
                )
                .unwrap(),
            TreeType::NormalTree,
            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            grove_version,
        )
        .unwrap()
        .unwrap();

        merk.apply::<_, Vec<_>>(
            &[(vec![1, 2, 3], Op::Put(vec![4, 5, 6], BasicMerkNode))],
            &[],
            None,
            grove_version,
        )
        .unwrap()
        .expect("apply failed");

        let root_hash = merk.root_hash();

        storage
            .apply_batch(batch, &transaction)
            .unwrap()
            .expect("cannot commit batch");

        let merk = Merk::open_base(
            storage
                .subtree(SubtreePath::from(test_prefix.as_ref()), None, &transaction)
                .unwrap(),
            TreeType::NormalTree,
            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            grove_version,
        )
        .unwrap()
        .unwrap();
        assert_eq!(merk.root_hash(), root_hash);
    }

    #[test]
    fn test_open_fee() {
        let grove_version = GroveVersion::latest();
        let storage = TempEngine::new();
        let batch = SuiteBatch::default();
        let transaction = storage.start_transaction();

        let merk_fee_context = Merk::open_base(
            storage
                .subtree(SubtreePath::empty(), Some(&batch), &transaction)
                .unwrap(),
            TreeType::NormalTree,
            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            grove_version,
        );
        // Opening not existing merk should cost only root key seek (except context
        // creation)
        assert!(matches!(
            merk_fee_context.cost(),
            OperationCost { seek_count: 1, .. }
        ));

        let mut merk = merk_fee_context.unwrap().unwrap();
        merk.apply::<_, Vec<_>>(
            &[(vec![1, 2, 3], Op::Put(vec![4, 5, 6], BasicMerkNode))],
            &[],
            None,
            grove_version,
        )
        .unwrap()
        .expect("apply failed");

        storage
            .apply_batch(batch, &transaction)
            .unwrap()
            .expect("cannot commit batch");

        let merk_fee_context = Merk::open_base(
            storage
                .subtree(SubtreePath::empty(), None, &transaction)
                .unwrap(),
            TreeType::NormalTree,
            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            grove_version,
        );

        // Opening existing merk should cost two seeks. (except context creation)
        assert!(matches!(
            merk_fee_context.cost(),
            OperationCost { seek_count: 2, .. }
        ));
        assert!(merk_fee_context.cost().storage_loaded_bytes > 0);
    }
}
