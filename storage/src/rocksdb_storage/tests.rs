use super::test_utils::TempStorage;
use crate::Batch;

fn to_path(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::once(bytes)
}

mod no_transaction {
    use super::*;
    use crate::{Batch, RawIterator, Storage, StorageContext};

    #[test]
    fn test_aux_cf_methods() {
        let storage = TempStorage::new();
        let context_ayya = storage.get_storage_context(to_path(b"ayya"));
        let context_ayyb = storage.get_storage_context(to_path(b"ayyb"));

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
        let context_ayya = storage.get_storage_context(to_path(b"ayya"));
        let context_ayyb = storage.get_storage_context(to_path(b"ayyb"));

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
        let context_ayya = storage.get_storage_context(to_path(b"ayya"));
        let context_ayyb = storage.get_storage_context(to_path(b"ayyb"));

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
        let context_ayya = storage.get_storage_context(to_path(b"ayya"));
        let context_ayyb = storage.get_storage_context(to_path(b"ayyb"));

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

    #[test]
    fn test_batch() {
        let storage = TempStorage::new();
        let context_ayya = storage.get_storage_context(to_path(b"ayya"));

        context_ayya
            .put(b"key1", b"ayyavalue1")
            .expect("cannot insert into storage");
        context_ayya
            .put(b"key2", b"ayyavalue2")
            .expect("cannot insert into storage");

        assert!(context_ayya
            .get(b"key3")
            .expect("cannot get from storage")
            .is_none());

        let mut batch = context_ayya.new_batch();
        batch.delete(b"key1").expect("infallible");
        batch.put(b"key3", b"ayyavalue3").expect("infallible");

        assert!(context_ayya
            .get(b"key3")
            .expect("cannot get from storage")
            .is_none());

        context_ayya
            .commit_batch(batch)
            .expect("cannot commit a batch");

        assert_eq!(
            context_ayya
                .get(b"key3")
                .ok()
                .flatten()
                .expect("cannot get from storage"),
            b"ayyavalue3"
        );
        assert!(context_ayya
            .get(b"key1")
            .expect("cannot get from storage")
            .is_none());
    }

    #[test]
    fn test_raw_iterator() {
        let storage = TempStorage::new();
        let context = storage.get_storage_context(to_path(b"someprefix"));

        context
            .put(b"key1", b"value1")
            .expect("expected successful insertion");
        context
            .put(b"key0", b"value0")
            .expect("expected successful insertion");
        context
            .put(b"key3", b"value3")
            .expect("expected successful insertion");
        context
            .put(b"key2", b"value2")
            .expect("expected successful insertion");

        // Other storages are required to put something into rocksdb with other prefix
        // to see if there will be any conflicts and boundaries are met
        let context_before = storage.get_storage_context(to_path(b"anothersomeprefix"));
        context_before
            .put(b"key1", b"value1")
            .expect("expected successful insertion");
        context_before
            .put(b"key5", b"value5")
            .expect("expected successful insertion");
        let context_after = storage.get_storage_context(to_path(b"zanothersomeprefix"));
        context_after
            .put(b"key1", b"value1")
            .expect("expected successful insertion");
        context_after
            .put(b"key5", b"value5")
            .expect("expected successful insertion");

        let expected: [(&'static [u8], &'static [u8]); 4] = [
            (b"key0", b"value0"),
            (b"key1", b"value1"),
            (b"key2", b"value2"),
            (b"key3", b"value3"),
        ];
        let mut expected_iter = expected.into_iter();

        // Test iterator goes forward

        let mut iter = context.raw_iter();
        iter.seek_to_first();
        while iter.valid() {
            assert_eq!(
                (iter.key().unwrap(), iter.value().unwrap()),
                expected_iter.next().unwrap()
            );
            iter.next();
        }
        assert!(expected_iter.next().is_none());

        // Test `seek_to_last` on a storage with elements

        let mut iter = context.raw_iter();
        iter.seek_to_last();
        assert_eq!(
            (iter.key().unwrap(), iter.value().unwrap()),
            expected.last().unwrap().clone(),
        );
        iter.next();
        assert!(!iter.valid());

        // Test `seek_to_last` on empty storage
        let empty_storage = storage.get_storage_context(to_path(b"notexist"));
        let mut iter = empty_storage.raw_iter();
        iter.seek_to_last();
        assert!(!iter.valid());
        iter.next();
        assert!(!iter.valid());
    }
}

mod transaction {
    use super::*;
    use crate::{RawIterator, Storage, StorageContext};

    #[test]
    fn test_aux_cf_methods() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let context_ayya = storage.get_transactional_storage_context(to_path(b"ayya"), &tx);
        let context_ayyb = storage.get_transactional_storage_context(to_path(b"ayyb"), &tx);

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
            storage.get_transactional_storage_context(to_path(b"ayya"), &tx2);
        let context_ayya_after_no_tx = storage.get_storage_context(to_path(b"ayya"));

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
        let context_ayya = storage.get_transactional_storage_context(to_path(b"ayya"), &tx);
        let context_ayyb = storage.get_transactional_storage_context(to_path(b"ayyb"), &tx);

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
            storage.get_transactional_storage_context(to_path(b"ayya"), &tx2);
        let context_ayya_after_no_tx = storage.get_storage_context(to_path(b"ayya"));

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
        let context_ayya = storage.get_storage_context(to_path(b"ayya"));
        let context_ayyb = storage.get_storage_context(to_path(b"ayyb"));

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
        let context_ayya = storage.get_storage_context(to_path(b"ayya"));
        let context_ayyb = storage.get_storage_context(to_path(b"ayyb"));

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

    #[test]
    fn test_batch() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let context_ayya = storage.get_transactional_storage_context(to_path(b"ayya"), &tx);

        context_ayya
            .put(b"key1", b"ayyavalue1")
            .expect("cannot insert into storage");
        context_ayya
            .put(b"key2", b"ayyavalue2")
            .expect("cannot insert into storage");

        assert!(context_ayya
            .get(b"key3")
            .expect("cannot get from storage")
            .is_none());

        let mut batch = context_ayya.new_batch();
        batch.delete(b"key1").expect("infallible");
        batch.put(b"key3", b"ayyavalue3").expect("infallible");

        assert!(context_ayya
            .get(b"key1")
            .expect("cannot get from storage")
            .is_some());

        context_ayya
            .commit_batch(batch)
            .expect("cannot commit a batch");

        assert!(context_ayya
            .get(b"key1")
            .expect("cannot get from storage")
            .is_none());

        storage
            .commit_transaction(tx)
            .expect("cannot commit transaction");

        let context_ayya = storage.get_storage_context(to_path(b"ayya"));
        assert_eq!(
            context_ayya
                .get(b"key3")
                .ok()
                .flatten()
                .expect("cannot get from storage"),
            b"ayyavalue3"
        );
        assert!(context_ayya
            .get(b"key1")
            .expect("cannot get from storage")
            .is_none());
    }

    #[test]
    fn test_raw_iterator() {
        let storage = TempStorage::new();
        let context = storage.get_storage_context(to_path(b"someprefix"));

        context
            .put(b"key1", b"value1")
            .expect("expected successful insertion");
        context
            .put(b"key0", b"value0")
            .expect("expected successful insertion");
        context
            .put(b"key3", b"value3")
            .expect("expected successful insertion");
        context
            .put(b"key2", b"value2")
            .expect("expected successful insertion");

        // Other storages are required to put something into rocksdb with other prefix
        // to see if there will be any conflicts and boundaries are met
        let context_before = storage.get_storage_context(to_path(b"anothersomeprefix"));
        context_before
            .put(b"key1", b"value1")
            .expect("expected successful insertion");
        context_before
            .put(b"key5", b"value5")
            .expect("expected successful insertion");
        let context_after = storage.get_storage_context(to_path(b"zanothersomeprefix"));
        context_after
            .put(b"key1", b"value1")
            .expect("expected successful insertion");
        context_after
            .put(b"key5", b"value5")
            .expect("expected successful insertion");

        // Test uncommited changes
        {
            let tx = storage.start_transaction();
            let context_tx = storage.get_transactional_storage_context(to_path(b"someprefix"), &tx);

            context_tx
                .delete(b"key1")
                .expect("unable to delete an item");
            context_tx
                .put(b"key4", b"value4")
                .expect("unable to insert an item");

            let expected: [(&'static [u8], &'static [u8]); 4] = [
                (b"key0", b"value0"),
                (b"key2", b"value2"),
                (b"key3", b"value3"),
                (b"key4", b"value4"),
            ];
            let mut expected_iter = expected.into_iter();

            // Test iterator goes forward

            let mut iter = context_tx.raw_iter();
            iter.seek_to_first();
            while iter.valid() {
                assert_eq!(
                    (iter.key().unwrap(), iter.value().unwrap()),
                    expected_iter.next().unwrap()
                );
                iter.next();
            }
            assert!(expected_iter.next().is_none());

            // Test `seek_to_last` on a storage with elements

            let mut iter = context_tx.raw_iter();
            iter.seek_to_last();
            assert_eq!(
                (iter.key().unwrap(), iter.value().unwrap()),
                expected.last().unwrap().clone(),
            );
            iter.next();
            assert!(!iter.valid());
        }

        // Test commited data stay intact
        {
            let expected: [(&'static [u8], &'static [u8]); 4] = [
                (b"key0", b"value0"),
                (b"key1", b"value1"),
                (b"key2", b"value2"),
                (b"key3", b"value3"),
            ];
            let mut expected_iter = expected.into_iter();

            let mut iter = context.raw_iter();
            iter.seek_to_first();
            while iter.valid() {
                assert_eq!(
                    (iter.key().unwrap(), iter.value().unwrap()),
                    expected_iter.next().unwrap()
                );
                iter.next();
            }
            assert!(expected_iter.next().is_none());
        }
    }
}

mod batch_no_transaction {
    use super::*;
    use crate::{Batch, Storage, StorageBatch, StorageContext};

    #[test]
    fn test_various_cf_methods() {
        let storage = TempStorage::new();
        let batch = StorageBatch::new();
        let context_ayya = storage.get_batch_storage_context(to_path(b"ayya"), &batch);
        let context_ayyb = storage.get_batch_storage_context(to_path(b"ayyb"), &batch);

        context_ayya
            .put_aux(b"key1", b"ayyavalue1")
            .expect("cannot insert into aux cf");
        context_ayya
            .put_meta(b"key2", b"ayyavalue2")
            .expect("cannot insert into meta cf");
        context_ayya
            .put_root(b"key3", b"ayyavalue3")
            .expect("cannot insert into roots cf");
        context_ayya
            .put(b"key4", b"ayyavalue4")
            .expect("cannot insert data");
        context_ayyb
            .put_aux(b"key1", b"ayybvalue1")
            .expect("cannot insert into aux cf");
        context_ayyb
            .put_meta(b"key2", b"ayybvalue2")
            .expect("cannot insert into meta cf");
        context_ayyb
            .put_root(b"key3", b"ayybvalue3")
            .expect("cannot insert into roots cf");
        context_ayyb
            .put(b"key4", b"ayybvalue4")
            .expect("cannot insert data");

        // There is no "staging" data for batch contexts: `get` will access only
        // pre-batch data (thus `None` until commit).
        assert!(context_ayya
            .get_aux(b"key1")
            .expect("cannot get from aux cf")
            .is_none());

        assert_eq!(batch.len(), 8);

        storage
            .commit_multi_context_batch(batch)
            .expect("cannot commit batch");

        let context_ayya = storage.get_storage_context(to_path(b"ayya"));
        let context_ayyb = storage.get_storage_context(to_path(b"ayyb"));

        assert_eq!(
            context_ayya
                .get_aux(b"key1")
                .ok()
                .flatten()
                .expect("cannot get from aux cf"),
            b"ayyavalue1",
        );
        assert_eq!(
            context_ayya
                .get_meta(b"key2")
                .ok()
                .flatten()
                .expect("cannot get from meta cf"),
            b"ayyavalue2",
        );
        assert_eq!(
            context_ayya
                .get_root(b"key3")
                .ok()
                .flatten()
                .expect("cannot get from roots cf"),
            b"ayyavalue3",
        );
        assert_eq!(
            context_ayya
                .get(b"key4")
                .ok()
                .flatten()
                .expect("cannot get data"),
            b"ayyavalue4",
        );

        assert_eq!(
            context_ayyb
                .get_aux(b"key1")
                .ok()
                .flatten()
                .expect("cannot get from aux cf"),
            b"ayybvalue1",
        );
        assert_eq!(
            context_ayyb
                .get_meta(b"key2")
                .ok()
                .flatten()
                .expect("cannot get from meta cf"),
            b"ayybvalue2",
        );
        assert_eq!(
            context_ayyb
                .get_root(b"key3")
                .ok()
                .flatten()
                .expect("cannot get from roots cf"),
            b"ayybvalue3",
        );
        assert_eq!(
            context_ayyb
                .get(b"key4")
                .ok()
                .flatten()
                .expect("cannot get data"),
            b"ayybvalue4",
        );
    }

    #[test]
    fn test_with_db_batches() {
        let storage = TempStorage::new();
        let batch = StorageBatch::new();
        let context_ayya = storage.get_batch_storage_context(to_path(b"ayya"), &batch);
        let context_ayyb = storage.get_batch_storage_context(to_path(b"ayyb"), &batch);

        context_ayya
            .put(b"key1", b"ayyavalue1")
            .expect("cannot insert data");
        let mut db_batch_ayya = context_ayya.new_batch();
        db_batch_ayya
            .put(b"key2", b"ayyavalue2")
            .expect("cannot push into db batch");
        db_batch_ayya
            .put(b"key3", b"ayyavalue3")
            .expect("cannot push into db batch");

        context_ayyb
            .put(b"key1", b"ayybvalue1")
            .expect("cannot insert data");
        let mut db_batch_ayyb = context_ayyb.new_batch();
        db_batch_ayyb
            .put(b"key2", b"ayybvalue2")
            .expect("cannot push into db batch");
        db_batch_ayyb
            .put(b"key3", b"ayybvalue3")
            .expect("cannot push into db batch");

        // DB batches are not commited yet, so these operations are missing from
        // StorageBatch
        assert_eq!(batch.len(), 2);

        context_ayya
            .commit_batch(db_batch_ayya)
            .expect("cannot commit db batch");
        context_ayyb
            .commit_batch(db_batch_ayyb)
            .expect("cannot commit db batch");

        // DB batches are "commited", but actually staged in multi-context batch to do
        // it in a single run to the database
        assert_eq!(batch.len(), 6);

        assert!(context_ayya
            .get(b"key1")
            .expect("cannot get data")
            .is_none());
        assert!(context_ayya
            .get(b"key3")
            .expect("cannot get data")
            .is_none());

        storage
            .commit_multi_context_batch(batch)
            .expect("cannot commit multi context batch");

        let context_ayya = storage.get_storage_context(to_path(b"ayya"));
        assert_eq!(
            context_ayya
                .get(b"key3")
                .ok()
                .flatten()
                .expect("cannot get data"),
            b"ayyavalue3"
        );
    }
}

mod batch_transaction {
    use super::*;
    use crate::{Batch, RawIterator, Storage, StorageBatch, StorageContext};

    #[test]
    fn test_transaction_properties() {
        let storage = TempStorage::new();
        let transaction = storage.start_transaction();

        let context_ayya = storage.get_storage_context(to_path(b"ayya"));
        let context_ayyb = storage.get_storage_context(to_path(b"ayyb"));
        let context_ayya_tx =
            storage.get_transactional_storage_context(to_path(b"ayya"), &transaction);
        let context_ayyb_tx =
            storage.get_transactional_storage_context(to_path(b"ayyb"), &transaction);

        // Data should be visible in transaction...
        context_ayya_tx
            .put(b"key1", b"ayyavalue1")
            .expect("cannot insert data");
        context_ayyb_tx
            .put(b"key1", b"ayybvalue1")
            .expect("cannot insert data");

        assert_eq!(
            context_ayya_tx
                .get(b"key1")
                .ok()
                .flatten()
                .expect("cannot get data"),
            b"ayyavalue1"
        );
        assert_eq!(
            context_ayyb_tx
                .get(b"key1")
                .ok()
                .flatten()
                .expect("cannot get data"),
            b"ayybvalue1"
        );

        // ...but not outside of it
        assert!(context_ayya
            .get(b"key1")
            .expect("cannot get data")
            .is_none());
        assert!(context_ayyb
            .get(b"key1")
            .expect("cannot get data")
            .is_none());

        // Batches data won't be visible either in transaction and outside of it until
        // batch is commited

        let batch = StorageBatch::new();
        let context_ayya_batch =
            storage.get_batch_transactional_storage_context(to_path(b"ayya"), &batch, &transaction);
        let context_ayyb_batch =
            storage.get_batch_transactional_storage_context(to_path(b"ayyb"), &batch, &transaction);
        context_ayya_batch
            .put_aux(b"key2", b"ayyavalue2")
            .expect("cannot put aux data into batch");
        context_ayyb_batch
            .put_aux(b"key2", b"ayybvalue2")
            .expect("cannot put aux data into batch");

        assert_eq!(batch.len(), 2);

        assert!(context_ayya_tx
            .get_aux(b"key2")
            .expect("cannot get data")
            .is_none());
        assert!(context_ayyb_tx
            .get_aux(b"key2")
            .expect("cannot get data")
            .is_none());
        assert!(context_ayya
            .get_aux(b"key2")
            .expect("cannot get data")
            .is_none());
        assert!(context_ayyb
            .get_aux(b"key2")
            .expect("cannot get data")
            .is_none());

        storage
            .commit_multi_context_batch_with_transaction(batch, &transaction)
            .expect("cannot commit batch");

        // Commited batch data is accessible in transaction but not outside
        assert_eq!(
            context_ayya_tx
                .get_aux(b"key2")
                .ok()
                .flatten()
                .expect("cannot get data"),
            b"ayyavalue2"
        );

        assert!(context_ayya
            .get_aux(b"key2")
            .expect("cannot get data")
            .is_none());

        storage
            .commit_transaction(transaction)
            .expect("cannot commit transaction");
        assert_eq!(
            context_ayya
                .get_aux(b"key2")
                .ok()
                .flatten()
                .expect("cannot get data"),
            b"ayyavalue2"
        );
    }

    #[test]
    fn test_db_batch_in_transaction_merged_into_context_batch() {
        let storage = TempStorage::new();
        let transaction = storage.start_transaction();
        let batch = StorageBatch::new();

        let context_ayya =
            storage.get_batch_transactional_storage_context(to_path(b"ayya"), &batch, &transaction);
        let context_ayyb =
            storage.get_batch_transactional_storage_context(to_path(b"ayyb"), &batch, &transaction);

        let mut db_batch_a = context_ayya.new_batch();
        let mut db_batch_b = context_ayyb.new_batch();

        db_batch_a
            .put(b"key1", b"value1")
            .expect("cannot put into db batch");
        db_batch_b
            .put(b"key2", b"value2")
            .expect("cannot put into db batch");

        // Until db batches are commited our multi-context batch should be empty
        assert_eq!(batch.len(), 0);

        context_ayya
            .commit_batch(db_batch_a)
            .expect("cannot commit batch");
        context_ayya
            .commit_batch(db_batch_b)
            .expect("cannot commit batch");

        // All operations are in multi-context batch, but not visible in DB yet
        assert_eq!(batch.len(), 2);
        assert!(context_ayya
            .get(b"key1")
            .expect("cannot get data")
            .is_none());
        assert!(context_ayyb
            .get(b"key2")
            .expect("cannot get data")
            .is_none());

        // Commited batch's data should be visible in transaction
        storage
            .commit_multi_context_batch_with_transaction(batch, &transaction)
            .expect("cannot commit multi-context batch");

        // Obtaining new contexts outside a commited batch but still within a
        // transaction
        let context_ayya =
            storage.get_transactional_storage_context(to_path(b"ayya"), &transaction);
        let context_ayyb =
            storage.get_transactional_storage_context(to_path(b"ayyb"), &transaction);

        assert_eq!(
            context_ayya.get(b"key1").expect("cannot get data"),
            Some(b"value1".to_vec())
        );
        assert_eq!(
            context_ayyb.get(b"key2").expect("cannot get data"),
            Some(b"value2".to_vec())
        );

        // And still no data in the database until transaction is commited
        let context_ayya = storage.get_storage_context(to_path(b"ayya"));
        let context_ayyb = storage.get_storage_context(to_path(b"ayyb"));

        let mut iter = context_ayya.raw_iter();
        iter.seek_to_first();
        assert!(!iter.valid());

        let mut iter = context_ayyb.raw_iter();
        iter.seek_to_first();
        assert!(!iter.valid());

        storage
            .commit_transaction(transaction)
            .expect("cannot commit transaction");

        let context_ayya = storage.get_storage_context(to_path(b"ayya"));
        let context_ayyb = storage.get_storage_context(to_path(b"ayyb"));

        assert_eq!(
            context_ayya.get(b"key1").expect("cannot get data"),
            Some(b"value1".to_vec())
        );
        assert_eq!(
            context_ayyb.get(b"key2").expect("cannot get data"),
            Some(b"value2".to_vec())
        );
    }
}
