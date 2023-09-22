use std::cell::Cell;

use grovedb_costs::CostResult;
use grovedb_storage::StorageContext;

use crate::{
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
        }
    }

    /// Open standalone tree
    pub fn open_standalone(storage: S, is_sum_tree: bool) -> CostResult<Self, Error> {
        let mut merk = Self {
            tree: Cell::new(None),
            root_tree_key: Cell::new(None),
            storage,
            merk_type: StandaloneMerk,
            is_sum_tree,
        };

        merk.load_base_root().map_ok(|_| merk)
    }

    /// Open base tree
    pub fn open_base(storage: S, is_sum_tree: bool) -> CostResult<Self, Error> {
        let mut merk = Self {
            tree: Cell::new(None),
            root_tree_key: Cell::new(None),
            storage,
            merk_type: BaseMerk,
            is_sum_tree,
        };

        merk.load_base_root().map_ok(|_| merk)
    }

    /// Open layered tree with root key
    pub fn open_layered_with_root_key(
        storage: S,
        root_key: Option<Vec<u8>>,
        is_sum_tree: bool,
    ) -> CostResult<Self, Error> {
        let mut merk = Self {
            tree: Cell::new(None),
            root_tree_key: Cell::new(root_key),
            storage,
            merk_type: LayeredMerk,
            is_sum_tree,
        };

        merk.load_root().map_ok(|_| merk)
    }
}

#[cfg(test)]
mod test {
    use grovedb_path::SubtreePath;
    use grovedb_storage::{
        rocksdb_storage::{test_utils::TempStorage, RocksDbStorage},
        Storage, StorageBatch,
    };
    use tempfile::TempDir;
    use grovedb_costs::OperationCost;

    use crate::{Merk, Op, TreeFeatureType::BasicMerk};

    #[test]
    fn test_reopen_root_hash() {
        let tmp_dir = TempDir::new().expect("cannot open tempdir");
        let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let test_prefix = [b"ayy"];

        let batch = StorageBatch::new();
        let mut merk = Merk::open_base(
            storage
                .get_storage_context(SubtreePath::from(test_prefix.as_ref()), Some(&batch))
                .unwrap(),
            false,
        )
        .unwrap()
        .unwrap();

        merk.apply::<_, Vec<_>>(
            &[(vec![1, 2, 3], Op::Put(vec![4, 5, 6], BasicMerk))],
            &[],
            None,
        )
        .unwrap()
        .expect("apply failed");

        let root_hash = merk.root_hash();

        storage
            .commit_multi_context_batch(batch, None)
            .unwrap()
            .expect("cannot commit batch");

        let merk = Merk::open_base(
            storage
                .get_storage_context(SubtreePath::from(test_prefix.as_ref()), None)
                .unwrap(),
            false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(merk.root_hash(), root_hash);
    }

    #[test]
    fn test_open_fee() {
        let storage = TempStorage::new();
        let batch = StorageBatch::new();

        let merk_fee_context = Merk::open_base(
            storage
                .get_storage_context(SubtreePath::empty(), Some(&batch))
                .unwrap(),
            false,
        );
        // Opening not existing merk should cost only root key seek (except context
        // creation)
        assert!(matches!(
            merk_fee_context.cost(),
            OperationCost { seek_count: 1, .. }
        ));

        let mut merk = merk_fee_context.unwrap().unwrap();
        merk.apply::<_, Vec<_>>(
            &[(vec![1, 2, 3], Op::Put(vec![4, 5, 6], BasicMerk))],
            &[],
            None,
        )
        .unwrap()
        .expect("apply failed");

        storage
            .commit_multi_context_batch(batch, None)
            .unwrap()
            .expect("cannot commit batch");

        let merk_fee_context = Merk::open_base(
            storage
                .get_storage_context(SubtreePath::empty(), None)
                .unwrap(),
            false,
        );

        // Opening existing merk should cost two seeks. (except context creation)
        assert!(matches!(
            merk_fee_context.cost(),
            OperationCost { seek_count: 2, .. }
        ));
        assert!(merk_fee_context.cost().storage_loaded_bytes > 0);
    }
}
