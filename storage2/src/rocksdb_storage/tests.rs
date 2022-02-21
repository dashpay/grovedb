use std::ops::Deref;

use tempfile::TempDir;

use super::*;

struct TempStorage {
    dir: TempDir,
    storage: RocksDbStorage,
}

impl TempStorage {
    fn new() -> Self {
        let dir = TempDir::new().expect("cannot create tempir");
        let storage = RocksDbStorage::default_rocksdb_with_path(dir.path())
            .expect("cannot open RocksDB storage");
        TempStorage { dir, storage }
    }
}

impl Deref for TempStorage {
    type Target = RocksDbStorage;

    fn deref(&self) -> &Self::Target {
        &self.storage
    }
}

mod no_transaction {
    use super::*;
    use crate::StorageContext;

    #[test]
    fn test_aux_cf_methods() {
        let storage = TempStorage::new();
        let context_ayya = storage.get_prefixed_context(b"ayya".to_vec());
        let context_ayyb = storage.get_prefixed_context(b"ayyb".to_vec());

        context_ayya
            .put_aux(b"key1", b"ayyavalue1")
            .expect("cannot insert into aux cf");
        context_ayya
            .put_aux(b"key2", b"ayyavalue2")
            .expect("cannot insert into aux cf");
        context_ayyb
            .put_aux(b"key1", b"ayybvalue1")
            .expect("cannot insert into aux cf");
        context_ayyb
            .put_aux(b"key2", b"ayybvalue2")
            .expect("cannot insert into aux cf");

        assert_eq!(
            context_ayya
                .get_aux(b"key1")
                .ok()
                .flatten()
                .expect("cannot get from aux cf"),
            b"ayyavalue1"
        );

        context_ayya
            .delete_aux(b"key1")
            .expect("cannot delete from aux cf");

        assert!(context_ayya
            .get_aux(b"key1")
            .expect("cannot get from aux cf")
            .is_none());
        assert_eq!(
            context_ayya
                .get_aux(b"key2")
                .ok()
                .flatten()
                .expect("cannot get from aux cf"),
            b"ayyavalue2"
        );
        assert_eq!(
            context_ayyb
                .get_aux(b"key1")
                .ok()
                .flatten()
                .expect("cannot get from aux cf"),
            b"ayybvalue1"
        );
    }

    #[test]
    fn test_roots_cf_methods() {
        let storage = TempStorage::new();
        let context_ayya = storage.get_prefixed_context(b"ayya".to_vec());
        let context_ayyb = storage.get_prefixed_context(b"ayyb".to_vec());

        context_ayya
            .put_root(b"key1", b"ayyavalue1")
            .expect("cannot insert into roots cf");
        context_ayya
            .put_root(b"key2", b"ayyavalue2")
            .expect("cannot insert into roots cf");
        context_ayyb
            .put_root(b"key1", b"ayybvalue1")
            .expect("cannot insert into roots cf");
        context_ayyb
            .put_root(b"key2", b"ayybvalue2")
            .expect("cannot insert into roots cf");

        assert_eq!(
            context_ayya
                .get_root(b"key1")
                .ok()
                .flatten()
                .expect("cannot get from roots cf"),
            b"ayyavalue1"
        );

        context_ayya
            .delete_root(b"key1")
            .expect("cannot delete from roots cf");

        assert!(context_ayya
            .get_root(b"key1")
            .expect("cannot get from roots cf")
            .is_none());
        assert_eq!(
            context_ayya
                .get_root(b"key2")
                .ok()
                .flatten()
                .expect("cannot get from roots cf"),
            b"ayyavalue2"
        );
        assert_eq!(
            context_ayyb
                .get_root(b"key1")
                .ok()
                .flatten()
                .expect("cannot get from roots cf"),
            b"ayybvalue1"
        );
    }

    #[test]
    fn test_meta_cf_methods() {
        let storage = TempStorage::new();
        let context_ayya = storage.get_prefixed_context(b"ayya".to_vec());
        let context_ayyb = storage.get_prefixed_context(b"ayyb".to_vec());

        context_ayya
            .put_meta(b"key1", b"ayyavalue1")
            .expect("cannot insert into meta cf");
        context_ayya
            .put_meta(b"key2", b"ayyavalue2")
            .expect("cannot insert into meta cf");
        context_ayyb
            .put_meta(b"key1", b"ayybvalue1")
            .expect("cannot insert into meta cf");
        context_ayyb
            .put_meta(b"key2", b"ayybvalue2")
            .expect("cannot insert into meta cf");

        assert_eq!(
            context_ayya
                .get_meta(b"key1")
                .ok()
                .flatten()
                .expect("cannot get from meta cf"),
            b"ayyavalue1"
        );

        context_ayya
            .delete_meta(b"key1")
            .expect("cannot delete from meta cf");

        assert!(context_ayya
            .get_meta(b"key1")
            .expect("cannot get from meta cf")
            .is_none());
        assert_eq!(
            context_ayya
                .get_meta(b"key2")
                .ok()
                .flatten()
                .expect("cannot get from meta cf"),
            b"ayyavalue2"
        );
        assert_eq!(
            context_ayyb
                .get_meta(b"key1")
                .ok()
                .flatten()
                .expect("cannot get from meta cf"),
            b"ayybvalue1"
        );
    }

    #[test]
    fn test_default_cf_methods() {
        let storage = TempStorage::new();
        let context_ayya = storage.get_prefixed_context(b"ayya".to_vec());
        let context_ayyb = storage.get_prefixed_context(b"ayyb".to_vec());

        context_ayya
            .put(b"key1", b"ayyavalue1")
            .expect("cannot insert into storage");
        context_ayya
            .put(b"key2", b"ayyavalue2")
            .expect("cannot insert into storage");
        context_ayyb
            .put(b"key1", b"ayybvalue1")
            .expect("cannot insert into storage");
        context_ayyb
            .put(b"key2", b"ayybvalue2")
            .expect("cannot insert into storage");

        assert_eq!(
            context_ayya
                .get(b"key1")
                .ok()
                .flatten()
                .expect("cannot get from storage"),
            b"ayyavalue1"
        );

        context_ayya
            .delete(b"key1")
            .expect("cannot delete from storage");

        assert!(context_ayya
            .get(b"key1")
            .expect("cannot get from storage")
            .is_none());
        assert_eq!(
            context_ayya
                .get(b"key2")
                .ok()
                .flatten()
                .expect("cannot get from storage"),
            b"ayyavalue2"
        );
        assert_eq!(
            context_ayyb
                .get(b"key1")
                .ok()
                .flatten()
                .expect("cannot get from storage"),
            b"ayybvalue1"
        );
    }
}

mod transaction {
    use super::*;
    use crate::{Storage, StorageContext};

    #[test]
    fn test_aux_cf_methods() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let context_ayya = storage.get_prefixed_transactional_context(b"ayya".to_vec(), &tx);
        let context_ayyb = storage.get_prefixed_transactional_context(b"ayyb".to_vec(), &tx);

        context_ayya
            .put_aux(b"key1", b"ayyavalue1")
            .expect("cannot insert into aux cf");
        context_ayya
            .put_aux(b"key2", b"ayyavalue2")
            .expect("cannot insert into aux cf");
        context_ayyb
            .put_aux(b"key1", b"ayybvalue1")
            .expect("cannot insert into aux cf");
        context_ayyb
            .put_aux(b"key2", b"ayybvalue2")
            .expect("cannot insert into aux cf");

        assert_eq!(
            context_ayya
                .get_aux(b"key1")
                .ok()
                .flatten()
                .expect("cannot get from aux cf"),
            b"ayyavalue1"
        );

        storage
            .commit_transaction(tx)
            .expect("cannot commit transaction");

        let tx2 = storage.start_transaction();
        let context_ayya_after_tx =
            storage.get_prefixed_transactional_context(b"ayya".to_vec(), &tx2);
        let context_ayya_after_no_tx = storage.get_prefixed_context(b"ayya".to_vec());

        context_ayya_after_tx
            .delete_aux(b"key1")
            .expect("cannot delete from aux cf");

        // Should be deleted inside transaction:
        assert!(context_ayya_after_tx
            .get_aux(b"key1")
            .expect("cannot get from aux cf")
            .is_none());

        // But still accessible outside of it:
        assert_eq!(
            context_ayya_after_no_tx
                .get_aux(b"key1")
                .ok()
                .flatten()
                .expect("cannot get from aux cf"),
            b"ayyavalue1"
        );

        storage
            .commit_transaction(tx2)
            .expect("cannot commit transaction");

        // ... and no longer accessible at all after transaciton got commited
        assert!(context_ayya_after_no_tx
            .get_aux(b"key1")
            .ok()
            .expect("cannot get from aux cf")
            .is_none());
    }

    #[test]
    fn test_roots_cf_methods() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let context_ayya = storage.get_prefixed_transactional_context(b"ayya".to_vec(), &tx);
        let context_ayyb = storage.get_prefixed_transactional_context(b"ayyb".to_vec(), &tx);

        context_ayya
            .put_root(b"key1", b"ayyavalue1")
            .expect("cannot insert into roots cf");
        context_ayya
            .put_root(b"key2", b"ayyavalue2")
            .expect("cannot insert into roots cf");
        context_ayyb
            .put_root(b"key1", b"ayybvalue1")
            .expect("cannot insert into roots cf");
        context_ayyb
            .put_root(b"key2", b"ayybvalue2")
            .expect("cannot insert into roots cf");

        assert_eq!(
            context_ayya
                .get_root(b"key1")
                .ok()
                .flatten()
                .expect("cannot get from roots cf"),
            b"ayyavalue1"
        );

        storage
            .commit_transaction(tx)
            .expect("cannot commit transaction");

        let tx2 = storage.start_transaction();
        let context_ayya_after_tx =
            storage.get_prefixed_transactional_context(b"ayya".to_vec(), &tx2);
        let context_ayya_after_no_tx = storage.get_prefixed_context(b"ayya".to_vec());

        context_ayya_after_tx
            .delete_root(b"key1")
            .expect("cannot delete from roots cf");

        // Should be deleted inside transaction:
        assert!(context_ayya_after_tx
            .get_root(b"key1")
            .expect("cannot get from roots cf")
            .is_none());

        // But still accessible outside of it:
        assert_eq!(
            context_ayya_after_no_tx
                .get_root(b"key1")
                .ok()
                .flatten()
                .expect("cannot get from roots cf"),
            b"ayyavalue1"
        );

        storage
            .commit_transaction(tx2)
            .expect("cannot commit transaction");

        // ... and no longer accessible at all after transaciton got commited
        assert!(context_ayya_after_no_tx
            .get_root(b"key1")
            .ok()
            .expect("cannot get from roots cf")
            .is_none());
    }

    #[test]
    fn test_meta_cf_methods() {
        let storage = TempStorage::new();
        let context_ayya = storage.get_prefixed_context(b"ayya".to_vec());
        let context_ayyb = storage.get_prefixed_context(b"ayyb".to_vec());

        context_ayya
            .put_meta(b"key1", b"ayyavalue1")
            .expect("cannot insert into meta cf");
        context_ayya
            .put_meta(b"key2", b"ayyavalue2")
            .expect("cannot insert into meta cf");
        context_ayyb
            .put_meta(b"key1", b"ayybvalue1")
            .expect("cannot insert into meta cf");
        context_ayyb
            .put_meta(b"key2", b"ayybvalue2")
            .expect("cannot insert into meta cf");

        assert_eq!(
            context_ayya
                .get_meta(b"key1")
                .ok()
                .flatten()
                .expect("cannot get from meta cf"),
            b"ayyavalue1"
        );

        context_ayya
            .delete_meta(b"key1")
            .expect("cannot delete from meta cf");

        assert!(context_ayya
            .get_meta(b"key1")
            .expect("cannot get from meta cf")
            .is_none());
        assert_eq!(
            context_ayya
                .get_meta(b"key2")
                .ok()
                .flatten()
                .expect("cannot get from meta cf"),
            b"ayyavalue2"
        );
        assert_eq!(
            context_ayyb
                .get_meta(b"key1")
                .ok()
                .flatten()
                .expect("cannot get from meta cf"),
            b"ayybvalue1"
        );
    }

    #[test]
    fn test_default_cf_methods() {
        let storage = TempStorage::new();
        let context_ayya = storage.get_prefixed_context(b"ayya".to_vec());
        let context_ayyb = storage.get_prefixed_context(b"ayyb".to_vec());

        context_ayya
            .put(b"key1", b"ayyavalue1")
            .expect("cannot insert into storage");
        context_ayya
            .put(b"key2", b"ayyavalue2")
            .expect("cannot insert into storage");
        context_ayyb
            .put(b"key1", b"ayybvalue1")
            .expect("cannot insert into storage");
        context_ayyb
            .put(b"key2", b"ayybvalue2")
            .expect("cannot insert into storage");

        assert_eq!(
            context_ayya
                .get(b"key1")
                .ok()
                .flatten()
                .expect("cannot get from storage"),
            b"ayyavalue1"
        );

        context_ayya
            .delete(b"key1")
            .expect("cannot delete from storage");

        assert!(context_ayya
            .get(b"key1")
            .expect("cannot get from storage")
            .is_none());
        assert_eq!(
            context_ayya
                .get(b"key2")
                .ok()
                .flatten()
                .expect("cannot get from storage"),
            b"ayyavalue2"
        );
        assert_eq!(
            context_ayyb
                .get(b"key1")
                .ok()
                .flatten()
                .expect("cannot get from storage"),
            b"ayybvalue1"
        );
    }
}
