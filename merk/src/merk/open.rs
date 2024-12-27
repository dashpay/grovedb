use std::cell::Cell;

use grovedb_costs::CostResult;
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;

use crate::{
    tree::kv::ValueDefinedCostType,
    Error, Merk, MerkType,
    MerkType::{BaseMerk, LayeredMerk, StandaloneMerk},
};

impl<'db, S> Merk<S>
where
    S: StorageContext<'db>,
{
    /// Open empty tree
    pub fn open_empty(storage: S, merk_type: MerkType, is_sum_tree: bool) -> Self {
        Self {
            tree: Cell::new(None),
            root_tree_key: Cell::new(None),
            storage,
            merk_type,
            is_sum_tree,
            meta_cache: Default::default(),
        }
    }

    /// Open standalone tree
    pub fn open_standalone(
        storage: S,
        is_sum_tree: bool,
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
            is_sum_tree,
            meta_cache: Default::default(),
        };

        merk.load_base_root(value_defined_cost_fn, grove_version)
            .map_ok(|_| merk)
    }

    /// Open base tree
    pub fn open_base(
        storage: S,
        is_sum_tree: bool,
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
            is_sum_tree,
            meta_cache: Default::default(),
        };

        merk.load_base_root(value_defined_cost_fn, grove_version)
            .map_ok(|_| merk)
    }

    /// Open layered tree with root key
    pub fn open_layered_with_root_key(
        storage: S,
        root_key: Option<Vec<u8>>,
        is_sum_tree: bool,
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
            is_sum_tree,
            meta_cache: Default::default(),
        };

        merk.load_root(value_defined_cost_fn, grove_version)
            .map_ok(|_| merk)
    }
}

#[cfg(test)]
mod test {
    use grovedb_costs::OperationCost;
    use grovedb_path::SubtreePath;
    use grovedb_storage::{
        rocksdb_storage::{test_utils::TempStorage, RocksDbStorage},
        Storage, StorageBatch,
    };
    use grovedb_version::version::GroveVersion;
    use tempfile::TempDir;

    use crate::{tree::kv::ValueDefinedCostType, Merk, Op, TreeFeatureType::BasicMerkNode};

    #[test]
    fn test_reopen_root_hash() {
        let grove_version = GroveVersion::latest();
        let tmp_dir = TempDir::new().expect("cannot open tempdir");
        let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let test_prefix = [b"ayy"];

        let batch = StorageBatch::new();
        let tx = storage.start_transaction();
        let mut merk = Merk::open_base(
            storage
                .get_transactional_storage_context(
                    SubtreePath::from(test_prefix.as_ref()),
                    Some(&batch),
                    &tx,
                )
                .unwrap(),
            false,
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
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("cannot commit batch");

        let merk = Merk::open_base(
            storage
                .get_transactional_storage_context(
                    SubtreePath::from(test_prefix.as_ref()),
                    None,
                    &tx,
                )
                .unwrap(),
            false,
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
        let storage = TempStorage::new();
        let batch = StorageBatch::new();
        let tx = storage.start_transaction();

        let merk_fee_context = Merk::open_base(
            storage
                .get_transactional_storage_context(SubtreePath::empty(), Some(&batch), &tx)
                .unwrap(),
            false,
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
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("cannot commit batch");

        let merk_fee_context = Merk::open_base(
            storage
                .get_transactional_storage_context(SubtreePath::empty(), None, &tx)
                .unwrap(),
            false,
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
