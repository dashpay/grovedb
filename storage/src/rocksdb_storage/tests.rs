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
        let context_ayya = storage.get_storage_context(to_path(b"ayya")).unwrap();
        let context_ayyb = storage.get_storage_context(to_path(b"ayyb")).unwrap();

        context_ayya
            .put_aux(b"key1", b"ayyavalue1", None)
            .unwrap()
            .expect("cannot insert into aux cf");
        context_ayya
            .put_aux(b"key2", b"ayyavalue2", None)
            .unwrap()
            .expect("cannot insert into aux cf");
        context_ayyb
            .put_aux(b"key1", b"ayybvalue1", None)
            .unwrap()
            .expect("cannot insert into aux cf");
        context_ayyb
            .put_aux(b"key2", b"ayybvalue2", None)
            .unwrap()
            .expect("cannot insert into aux cf");

        assert_eq!(
            context_ayya
                .get_aux(b"key1")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from aux cf"),
            b"ayyavalue1"
        );

        context_ayya
            .delete_aux(b"key1")
            .unwrap()
            .expect("cannot delete from aux cf");

        assert!(context_ayya
            .get_aux(b"key1")
            .unwrap()
            .expect("cannot get from aux cf")
            .is_none());
        assert_eq!(
            context_ayya
                .get_aux(b"key2")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from aux cf"),
            b"ayyavalue2"
        );
        assert_eq!(
            context_ayyb
                .get_aux(b"key1")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from aux cf"),
            b"ayybvalue1"
        );
    }

    #[test]
    fn test_roots_cf_methods() {
        let storage = TempStorage::new();
        let context_ayya = storage.get_storage_context(to_path(b"ayya")).unwrap();
        let context_ayyb = storage.get_storage_context(to_path(b"ayyb")).unwrap();

        context_ayya
            .put_root(b"key1", b"ayyavalue1", None)
            .unwrap()
            .expect("cannot insert into roots cf");
        context_ayya
            .put_root(b"key2", b"ayyavalue2", None)
            .unwrap()
            .expect("cannot insert into roots cf");
        context_ayyb
            .put_root(b"key1", b"ayybvalue1", None)
            .unwrap()
            .expect("cannot insert into roots cf");
        context_ayyb
            .put_root(b"key2", b"ayybvalue2", None)
            .unwrap()
            .expect("cannot insert into roots cf");

        assert_eq!(
            context_ayya
                .get_root(b"key1")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from roots cf"),
            b"ayyavalue1"
        );

        context_ayya
            .delete_root(b"key1")
            .unwrap()
            .expect("cannot delete from roots cf");

        assert!(context_ayya
            .get_root(b"key1")
            .unwrap()
            .expect("cannot get from roots cf")
            .is_none());
        assert_eq!(
            context_ayya
                .get_root(b"key2")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from roots cf"),
            b"ayyavalue2"
        );
        assert_eq!(
            context_ayyb
                .get_root(b"key1")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from roots cf"),
            b"ayybvalue1"
        );
    }

    #[test]
    fn test_meta_cf_methods() {
        let storage = TempStorage::new();
        let context_ayya = storage.get_storage_context(to_path(b"ayya")).unwrap();
        let context_ayyb = storage.get_storage_context(to_path(b"ayyb")).unwrap();

        context_ayya
            .put_meta(b"key1", b"ayyavalue1", None)
            .unwrap()
            .expect("cannot insert into meta cf");
        context_ayya
            .put_meta(b"key2", b"ayyavalue2", None)
            .unwrap()
            .expect("cannot insert into meta cf");
        context_ayyb
            .put_meta(b"key1", b"ayybvalue1", None)
            .unwrap()
            .expect("cannot insert into meta cf");
        context_ayyb
            .put_meta(b"key2", b"ayybvalue2", None)
            .unwrap()
            .expect("cannot insert into meta cf");

        assert_eq!(
            context_ayya
                .get_meta(b"key1")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from meta cf"),
            b"ayyavalue1"
        );

        context_ayya
            .delete_meta(b"key1")
            .unwrap()
            .expect("cannot delete from meta cf");

        assert!(context_ayya
            .get_meta(b"key1")
            .unwrap()
            .expect("cannot get from meta cf")
            .is_none());
        assert_eq!(
            context_ayya
                .get_meta(b"key2")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from meta cf"),
            b"ayyavalue2"
        );
        assert_eq!(
            context_ayyb
                .get_meta(b"key1")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from meta cf"),
            b"ayybvalue1"
        );
    }

    #[test]
    fn test_default_cf_methods() {
        let storage = TempStorage::new();
        let context_ayya = storage.get_storage_context(to_path(b"ayya")).unwrap();
        let context_ayyb = storage.get_storage_context(to_path(b"ayyb")).unwrap();

        context_ayya
            .put(b"key1", b"ayyavalue1", None, None)
            .unwrap()
            .expect("cannot insert into storage_cost");
        context_ayya
            .put(b"key2", b"ayyavalue2", None, None)
            .unwrap()
            .expect("cannot insert into storage_cost");
        context_ayyb
            .put(b"key1", b"ayybvalue1", None, None)
            .unwrap()
            .expect("cannot insert into storage_cost");
        context_ayyb
            .put(b"key2", b"ayybvalue2", None, None)
            .unwrap()
            .expect("cannot insert into storage_cost");

        assert_eq!(
            context_ayya
                .get(b"key1")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from storage_cost"),
            b"ayyavalue1"
        );

        context_ayya
            .delete(b"key1")
            .unwrap()
            .expect("cannot delete from storage_cost");

        assert!(context_ayya
            .get(b"key1")
            .unwrap()
            .expect("cannot get from storage_cost")
            .is_none());
        assert_eq!(
            context_ayya
                .get(b"key2")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from storage_cost"),
            b"ayyavalue2"
        );
        assert_eq!(
            context_ayyb
                .get(b"key1")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from storage_cost"),
            b"ayybvalue1"
        );
    }

    #[test]
    fn test_batch() {
        let storage = TempStorage::new();
        let context_ayya = storage.get_storage_context(to_path(b"ayya")).unwrap();

        context_ayya
            .put(b"key1", b"ayyavalue1", None, None)
            .unwrap()
            .expect("cannot insert into storage_cost");
        context_ayya
            .put(b"key2", b"ayyavalue2", None, None)
            .unwrap()
            .expect("cannot insert into storage_cost");

        assert!(context_ayya
            .get(b"key3")
            .unwrap()
            .expect("cannot get from storage_cost")
            .is_none());

        let mut batch = context_ayya.new_batch();
        batch.delete(b"key1", );
        batch.put(b"key3", b"ayyavalue3", None, None);

        assert!(context_ayya
            .get(b"key3")
            .unwrap()
            .expect("cannot get from storage_cost")
            .is_none());

        context_ayya
            .commit_batch(batch)
            .unwrap()
            .expect("cannot commit a batch");

        assert_eq!(
            context_ayya
                .get(b"key3")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from storage_cost"),
            b"ayyavalue3"
        );
        assert!(context_ayya
            .get(b"key1")
            .unwrap()
            .expect("cannot get from storage_cost")
            .is_none());
    }

    #[test]
    fn test_raw_iterator() {
        let storage = TempStorage::new();
        let context = storage.get_storage_context(to_path(b"someprefix")).unwrap();

        context
            .put(b"key1", b"value1", None, None)
            .unwrap()
            .expect("expected successful insertion");
        context
            .put(b"key0", b"value0", None, None)
            .unwrap()
            .expect("expected successful insertion");
        context
            .put(b"key3", b"value3", None, None)
            .unwrap()
            .expect("expected successful insertion");
        context
            .put(b"key2", b"value2", None, None)
            .unwrap()
            .expect("expected successful insertion");

        // Other storages are required to put something into rocksdb with other prefix
        // to see if there will be any conflicts and boundaries are met
        let context_before = storage
            .get_storage_context(to_path(b"anothersomeprefix"))
            .unwrap();
        context_before
            .put(b"key1", b"value1", None, None)
            .unwrap()
            .expect("expected successful insertion");
        context_before
            .put(b"key5", b"value5", None, None)
            .unwrap()
            .expect("expected successful insertion");
        let context_after = storage
            .get_storage_context(to_path(b"zanothersomeprefix"))
            .unwrap();
        context_after
            .put(b"key1", b"value1", None, None)
            .unwrap()
            .expect("expected successful insertion");
        context_after
            .put(b"key5", b"value5", None, None)
            .unwrap()
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
        while iter.valid().unwrap() {
            assert_eq!(
                (iter.key().unwrap().unwrap(), iter.value().unwrap().unwrap()),
                expected_iter.next().unwrap()
            );
            iter.next();
        }
        assert!(expected_iter.next().is_none());

        // Test `seek_to_last` on a storage_cost with elements

        let mut iter = context.raw_iter();
        iter.seek_to_last();
        assert_eq!(
            (iter.key().unwrap().unwrap(), iter.value().unwrap().unwrap()),
            expected.last().unwrap().clone(),
        );
        iter.next();
        assert!(!iter.valid().unwrap());

        // Test `seek_to_last` on empty storage_cost
        let empty_storage = storage.get_storage_context(to_path(b"notexist")).unwrap();
        let mut iter = empty_storage.raw_iter();
        iter.seek_to_last();
        assert!(!iter.valid().unwrap());
        iter.next();
        assert!(!iter.valid().unwrap());
    }
}

mod transaction {
    use super::*;
    use crate::{RawIterator, Storage, StorageContext};

    #[test]
    fn test_aux_cf_methods() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let context_ayya = storage
            .get_transactional_storage_context(to_path(b"ayya"), &tx)
            .unwrap();
        let context_ayyb = storage
            .get_transactional_storage_context(to_path(b"ayyb"), &tx)
            .unwrap();

        context_ayya
            .put_aux(b"key1", b"ayyavalue1", None)
            .unwrap()
            .expect("cannot insert into aux cf");
        context_ayya
            .put_aux(b"key2", b"ayyavalue2", None)
            .unwrap()
            .expect("cannot insert into aux cf");
        context_ayyb
            .put_aux(b"key1", b"ayybvalue1", None)
            .unwrap()
            .expect("cannot insert into aux cf");
        context_ayyb
            .put_aux(b"key2", b"ayybvalue2", None)
            .unwrap()
            .expect("cannot insert into aux cf");

        assert_eq!(
            context_ayya
                .get_aux(b"key1")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from aux cf"),
            b"ayyavalue1"
        );

        storage
            .commit_transaction(tx)
            .unwrap()
            .expect("cannot commit transaction");

        let tx2 = storage.start_transaction();
        let context_ayya_after_tx = storage
            .get_transactional_storage_context(to_path(b"ayya"), &tx2)
            .unwrap();
        let context_ayya_after_no_tx = storage.get_storage_context(to_path(b"ayya")).unwrap();

        context_ayya_after_tx
            .delete_aux(b"key1")
            .unwrap()
            .expect("cannot delete from aux cf");

        // Should be deleted inside transaction:
        assert!(context_ayya_after_tx
            .get_aux(b"key1")
            .unwrap()
            .expect("cannot get from aux cf")
            .is_none());

        // But still accessible outside of it:
        assert_eq!(
            context_ayya_after_no_tx
                .get_aux(b"key1")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from aux cf"),
            b"ayyavalue1"
        );

        storage
            .commit_transaction(tx2)
            .unwrap()
            .expect("cannot commit transaction");

        // ... and no longer accessible at all after transaciton got commited
        assert!(context_ayya_after_no_tx
            .get_aux(b"key1")
            .unwrap()
            .ok()
            .expect("cannot get from aux cf")
            .is_none());
    }

    #[test]
    fn test_roots_cf_methods() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let context_ayya = storage
            .get_transactional_storage_context(to_path(b"ayya"), &tx)
            .unwrap();
        let context_ayyb = storage
            .get_transactional_storage_context(to_path(b"ayyb"), &tx)
            .unwrap();

        context_ayya
            .put_root(b"key1", b"ayyavalue1", None)
            .unwrap()
            .expect("cannot insert into roots cf");
        context_ayya
            .put_root(b"key2", b"ayyavalue2", None)
            .unwrap()
            .expect("cannot insert into roots cf");
        context_ayyb
            .put_root(b"key1", b"ayybvalue1", None)
            .unwrap()
            .expect("cannot insert into roots cf");
        context_ayyb
            .put_root(b"key2", b"ayybvalue2", None)
            .unwrap()
            .expect("cannot insert into roots cf");

        assert_eq!(
            context_ayya
                .get_root(b"key1")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from roots cf"),
            b"ayyavalue1"
        );

        storage
            .commit_transaction(tx)
            .unwrap()
            .expect("cannot commit transaction");

        let tx2 = storage.start_transaction();
        let context_ayya_after_tx = storage
            .get_transactional_storage_context(to_path(b"ayya"), &tx2)
            .unwrap();
        let context_ayya_after_no_tx = storage.get_storage_context(to_path(b"ayya")).unwrap();

        context_ayya_after_tx
            .delete_root(b"key1")
            .unwrap()
            .expect("cannot delete from roots cf");

        // Should be deleted inside transaction:
        assert!(context_ayya_after_tx
            .get_root(b"key1")
            .unwrap()
            .expect("cannot get from roots cf")
            .is_none());

        // But still accessible outside of it:
        assert_eq!(
            context_ayya_after_no_tx
                .get_root(b"key1")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from roots cf"),
            b"ayyavalue1"
        );

        storage
            .commit_transaction(tx2)
            .unwrap()
            .expect("cannot commit transaction");

        // ... and no longer accessible at all after transaciton got commited
        assert!(context_ayya_after_no_tx
            .get_root(b"key1")
            .unwrap()
            .ok()
            .expect("cannot get from roots cf")
            .is_none());
    }

    #[test]
    fn test_meta_cf_methods() {
        let storage = TempStorage::new();
        let context_ayya = storage.get_storage_context(to_path(b"ayya")).unwrap();
        let context_ayyb = storage.get_storage_context(to_path(b"ayyb")).unwrap();

        context_ayya
            .put_meta(b"key1", b"ayyavalue1", None)
            .unwrap()
            .expect("cannot insert into meta cf");
        context_ayya
            .put_meta(b"key2", b"ayyavalue2", None)
            .unwrap()
            .expect("cannot insert into meta cf");
        context_ayyb
            .put_meta(b"key1", b"ayybvalue1", None)
            .unwrap()
            .expect("cannot insert into meta cf");
        context_ayyb
            .put_meta(b"key2", b"ayybvalue2", None)
            .unwrap()
            .expect("cannot insert into meta cf");

        assert_eq!(
            context_ayya
                .get_meta(b"key1")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from meta cf"),
            b"ayyavalue1"
        );

        context_ayya
            .delete_meta(b"key1")
            .unwrap()
            .expect("cannot delete from meta cf");

        assert!(context_ayya
            .get_meta(b"key1")
            .unwrap()
            .expect("cannot get from meta cf")
            .is_none());
        assert_eq!(
            context_ayya
                .get_meta(b"key2")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from meta cf"),
            b"ayyavalue2"
        );
        assert_eq!(
            context_ayyb
                .get_meta(b"key1")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from meta cf"),
            b"ayybvalue1"
        );
    }

    #[test]
    fn test_default_cf_methods() {
        let storage = TempStorage::new();
        let context_ayya = storage.get_storage_context(to_path(b"ayya")).unwrap();
        let context_ayyb = storage.get_storage_context(to_path(b"ayyb")).unwrap();

        context_ayya
            .put(b"key1", b"ayyavalue1", None, None)
            .unwrap()
            .expect("cannot insert into storage_cost");
        context_ayya
            .put(b"key2", b"ayyavalue2", None, None)
            .unwrap()
            .expect("cannot insert into storage_cost");
        context_ayyb
            .put(b"key1", b"ayybvalue1", None, None)
            .unwrap()
            .expect("cannot insert into storage_cost");
        context_ayyb
            .put(b"key2", b"ayybvalue2", None, None)
            .unwrap()
            .expect("cannot insert into storage_cost");

        assert_eq!(
            context_ayya
                .get(b"key1")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from storage_cost"),
            b"ayyavalue1"
        );

        context_ayya
            .delete(b"key1")
            .unwrap()
            .expect("cannot delete from storage_cost");

        assert!(context_ayya
            .get(b"key1")
            .unwrap()
            .expect("cannot get from storage_cost")
            .is_none());
        assert_eq!(
            context_ayya
                .get(b"key2")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from storage_cost"),
            b"ayyavalue2"
        );
        assert_eq!(
            context_ayyb
                .get(b"key1")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from storage_cost"),
            b"ayybvalue1"
        );
    }

    #[test]
    fn test_batch() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let context_ayya = storage
            .get_transactional_storage_context(to_path(b"ayya"), &tx)
            .unwrap();

        context_ayya
            .put(b"key1", b"ayyavalue1", None, None)
            .unwrap()
            .expect("cannot insert into storage_cost");
        context_ayya
            .put(b"key2", b"ayyavalue2", None, None)
            .unwrap()
            .expect("cannot insert into storage_cost");

        assert!(context_ayya
            .get(b"key3")
            .unwrap()
            .expect("cannot get from storage_cost")
            .is_none());

        let mut batch = context_ayya.new_batch();
        batch.delete(b"key1", );
        batch.put(b"key3", b"ayyavalue3", None, None);

        assert!(context_ayya
            .get(b"key1")
            .unwrap()
            .expect("cannot get from storage_cost")
            .is_some());

        context_ayya
            .commit_batch(batch)
            .unwrap()
            .expect("cannot commit a batch");

        assert!(context_ayya
            .get(b"key1")
            .unwrap()
            .expect("cannot get from storage_cost")
            .is_none());

        storage
            .commit_transaction(tx)
            .unwrap()
            .expect("cannot commit transaction");

        let context_ayya = storage.get_storage_context(to_path(b"ayya")).unwrap();
        assert_eq!(
            context_ayya
                .get(b"key3")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from storage_cost"),
            b"ayyavalue3"
        );
        assert!(context_ayya
            .get(b"key1")
            .unwrap()
            .expect("cannot get from storage_cost")
            .is_none());
    }

    #[test]
    fn test_raw_iterator() {
        let storage = TempStorage::new();
        let context = storage.get_storage_context(to_path(b"someprefix")).unwrap();

        context
            .put(b"key1", b"value1", None, None)
            .unwrap()
            .expect("expected successful insertion");
        context
            .put(b"key0", b"value0", None, None)
            .unwrap()
            .expect("expected successful insertion");
        context
            .put(b"key3", b"value3", None, None)
            .unwrap()
            .expect("expected successful insertion");
        context
            .put(b"key2", b"value2", None, None)
            .unwrap()
            .expect("expected successful insertion");

        // Other storages are required to put something into rocksdb with other prefix
        // to see if there will be any conflicts and boundaries are met
        let context_before = storage
            .get_storage_context(to_path(b"anothersomeprefix"))
            .unwrap();
        context_before
            .put(b"key1", b"value1", None, None)
            .unwrap()
            .expect("expected successful insertion");
        context_before
            .put(b"key5", b"value5", None, None)
            .unwrap()
            .expect("expected successful insertion");
        let context_after = storage
            .get_storage_context(to_path(b"zanothersomeprefix"))
            .unwrap();
        context_after
            .put(b"key1", b"value1", None, None)
            .unwrap()
            .expect("expected successful insertion");
        context_after
            .put(b"key5", b"value5", None, None)
            .unwrap()
            .expect("expected successful insertion");

        // Test uncommited changes
        {
            let tx = storage.start_transaction();
            let context_tx = storage
                .get_transactional_storage_context(to_path(b"someprefix"), &tx)
                .unwrap();

            context_tx
                .delete(b"key1")
                .unwrap()
                .expect("unable to delete an item");
            context_tx
                .put(b"key4", b"value4", None, None)
                .unwrap()
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
            while iter.valid().unwrap() {
                assert_eq!(
                    (iter.key().unwrap().unwrap(), iter.value().unwrap().unwrap()),
                    expected_iter.next().unwrap()
                );
                iter.next();
            }
            assert!(expected_iter.next().is_none());

            // Test `seek_to_last` on a storage_cost with elements

            let mut iter = context_tx.raw_iter();
            iter.seek_to_last();
            assert_eq!(
                (iter.key().unwrap().unwrap(), iter.value().unwrap().unwrap()),
                expected.last().unwrap().clone(),
            );
            iter.next();
            assert!(!iter.valid().unwrap());
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
            while iter.valid().unwrap() {
                assert_eq!(
                    (iter.key().unwrap().unwrap(), iter.value().unwrap().unwrap()),
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
        let context_ayya = storage
            .get_batch_storage_context(to_path(b"ayya"), &batch)
            .unwrap();
        let context_ayyb = storage
            .get_batch_storage_context(to_path(b"ayyb"), &batch)
            .unwrap();

        context_ayya
            .put_aux(b"key1", b"ayyavalue1", None)
            .unwrap()
            .expect("cannot insert into aux cf");
        context_ayya
            .put_meta(b"key2", b"ayyavalue2", None)
            .unwrap()
            .expect("cannot insert into meta cf");
        context_ayya
            .put_root(b"key3", b"ayyavalue3", None)
            .unwrap()
            .expect("cannot insert into roots cf");
        context_ayya
            .put(b"key4", b"ayyavalue4", None, None)
            .unwrap()
            .expect("cannot insert data");
        context_ayyb
            .put_aux(b"key1", b"ayybvalue1", None)
            .unwrap()
            .expect("cannot insert into aux cf");
        context_ayyb
            .put_meta(b"key2", b"ayybvalue2", None)
            .unwrap()
            .expect("cannot insert into meta cf");
        context_ayyb
            .put_root(b"key3", b"ayybvalue3", None)
            .unwrap()
            .expect("cannot insert into roots cf");
        context_ayyb
            .put(b"key4", b"ayybvalue4", None, None)
            .unwrap()
            .expect("cannot insert data");

        // There is no "staging" data for batch contexts: `get` will access only
        // pre-batch data (thus `None` until commit).
        assert!(context_ayya
            .get_aux(b"key1")
            .unwrap()
            .expect("cannot get from aux cf")
            .is_none());

        assert_eq!(batch.len(), 8);

        storage
            .commit_multi_context_batch(batch, None)
            .unwrap()
            .expect("cannot commit batch");

        let context_ayya = storage.get_storage_context(to_path(b"ayya")).unwrap();
        let context_ayyb = storage.get_storage_context(to_path(b"ayyb")).unwrap();

        assert_eq!(
            context_ayya
                .get_aux(b"key1")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from aux cf"),
            b"ayyavalue1",
        );
        assert_eq!(
            context_ayya
                .get_meta(b"key2")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from meta cf"),
            b"ayyavalue2",
        );
        assert_eq!(
            context_ayya
                .get_root(b"key3")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from roots cf"),
            b"ayyavalue3",
        );
        assert_eq!(
            context_ayya
                .get(b"key4")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get data"),
            b"ayyavalue4",
        );

        assert_eq!(
            context_ayyb
                .get_aux(b"key1")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from aux cf"),
            b"ayybvalue1",
        );
        assert_eq!(
            context_ayyb
                .get_meta(b"key2")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from meta cf"),
            b"ayybvalue2",
        );
        assert_eq!(
            context_ayyb
                .get_root(b"key3")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from roots cf"),
            b"ayybvalue3",
        );
        assert_eq!(
            context_ayyb
                .get(b"key4")
                .unwrap()
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
        let context_ayya = storage
            .get_batch_storage_context(to_path(b"ayya"), &batch)
            .unwrap();
        let context_ayyb = storage
            .get_batch_storage_context(to_path(b"ayyb"), &batch)
            .unwrap();

        context_ayya
            .put(b"key1", b"ayyavalue1", None, None)
            .unwrap()
            .expect("cannot insert data");
        let mut db_batch_ayya = context_ayya.new_batch();
        db_batch_ayya
            .put(b"key2", b"ayyavalue2", None, None)
            .expect("should not error");
        db_batch_ayya
            .put(b"key3", b"ayyavalue3", None, None)
            .expect("should not error");

        context_ayyb
            .put(b"key1", b"ayybvalue1", None, None)
            .unwrap()
            .expect("cannot insert data");
        let mut db_batch_ayyb = context_ayyb.new_batch();
        db_batch_ayyb
            .put(b"key2", b"ayybvalue2", None, None)
            .expect("should not error");
        db_batch_ayyb
            .put(b"key3", b"ayybvalue3", None, None)
            .expect("should not error");

        // DB batches are not commited yet, so these operations are missing from
        // StorageBatch
        assert_eq!(batch.len(), 2);

        context_ayya
            .commit_batch(db_batch_ayya)
            .unwrap()
            .expect("cannot commit db batch");
        context_ayyb
            .commit_batch(db_batch_ayyb)
            .unwrap()
            .expect("cannot commit db batch");

        // DB batches are "commited", but actually staged in multi-context batch to do
        // it in a single run to the database
        assert_eq!(batch.len(), 6);

        assert!(context_ayya
            .get(b"key1")
            .unwrap()
            .expect("cannot get data")
            .is_none());
        assert!(context_ayya
            .get(b"key3")
            .unwrap()
            .expect("cannot get data")
            .is_none());

        storage
            .commit_multi_context_batch(batch, None)
            .unwrap()
            .expect("cannot commit multi context batch");

        let context_ayya = storage.get_storage_context(to_path(b"ayya")).unwrap();
        assert_eq!(
            context_ayya
                .get(b"key3")
                .unwrap()
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

        let context_ayya = storage.get_storage_context(to_path(b"ayya")).unwrap();
        let context_ayyb = storage.get_storage_context(to_path(b"ayyb")).unwrap();
        let context_ayya_tx = storage
            .get_transactional_storage_context(to_path(b"ayya"), &transaction)
            .unwrap();
        let context_ayyb_tx = storage
            .get_transactional_storage_context(to_path(b"ayyb"), &transaction)
            .unwrap();

        // Data should be visible in transaction...
        context_ayya_tx
            .put(b"key1", b"ayyavalue1", None, None)
            .unwrap()
            .expect("cannot insert data");
        context_ayyb_tx
            .put(b"key1", b"ayybvalue1", None, None)
            .unwrap()
            .expect("cannot insert data");

        assert_eq!(
            context_ayya_tx
                .get(b"key1")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get data"),
            b"ayyavalue1"
        );
        assert_eq!(
            context_ayyb_tx
                .get(b"key1")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get data"),
            b"ayybvalue1"
        );

        // ...but not outside of it
        assert!(context_ayya
            .get(b"key1")
            .unwrap()
            .expect("cannot get data")
            .is_none());
        assert!(context_ayyb
            .get(b"key1")
            .unwrap()
            .expect("cannot get data")
            .is_none());

        // Batches data won't be visible either in transaction and outside of it until
        // batch is commited

        let batch = StorageBatch::new();
        let context_ayya_batch = storage
            .get_batch_transactional_storage_context(to_path(b"ayya"), &batch, &transaction)
            .unwrap();
        let context_ayyb_batch = storage
            .get_batch_transactional_storage_context(to_path(b"ayyb"), &batch, &transaction)
            .unwrap();
        context_ayya_batch
            .put_aux(b"key2", b"ayyavalue2", None)
            .unwrap()
            .expect("cannot put aux data into batch");
        context_ayyb_batch
            .put_aux(b"key2", b"ayybvalue2", None)
            .unwrap()
            .expect("cannot put aux data into batch");

        assert_eq!(batch.len(), 2);

        assert!(context_ayya_tx
            .get_aux(b"key2")
            .unwrap()
            .expect("cannot get data")
            .is_none());
        assert!(context_ayyb_tx
            .get_aux(b"key2")
            .unwrap()
            .expect("cannot get data")
            .is_none());
        assert!(context_ayya
            .get_aux(b"key2")
            .unwrap()
            .expect("cannot get data")
            .is_none());
        assert!(context_ayyb
            .get_aux(b"key2")
            .unwrap()
            .expect("cannot get data")
            .is_none());

        storage
            .commit_multi_context_batch(batch, Some(&transaction))
            .unwrap()
            .expect("cannot commit batch");

        // Commited batch data is accessible in transaction but not outside
        assert_eq!(
            context_ayya_tx
                .get_aux(b"key2")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get data"),
            b"ayyavalue2"
        );

        assert!(context_ayya
            .get_aux(b"key2")
            .unwrap()
            .expect("cannot get data")
            .is_none());

        storage
            .commit_transaction(transaction)
            .unwrap()
            .expect("cannot commit transaction");
        assert_eq!(
            context_ayya
                .get_aux(b"key2")
                .unwrap()
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

        let context_ayya = storage
            .get_batch_transactional_storage_context(to_path(b"ayya"), &batch, &transaction)
            .unwrap();
        let context_ayyb = storage
            .get_batch_transactional_storage_context(to_path(b"ayyb"), &batch, &transaction)
            .unwrap();

        let mut db_batch_a = context_ayya.new_batch();
        let mut db_batch_b = context_ayyb.new_batch();

        db_batch_a.put(b"key1", b"value1", None, None).unwrap();
        db_batch_b.put(b"key2", b"value2", None, None).unwrap();

        // Until db batches are commited our multi-context batch should be empty
        assert_eq!(batch.len(), 0);

        context_ayya
            .commit_batch(db_batch_a)
            .unwrap()
            .expect("cannot commit batch");
        context_ayya
            .commit_batch(db_batch_b)
            .unwrap()
            .expect("cannot commit batch");

        // All operations are in multi-context batch, but not visible in DB yet
        assert_eq!(batch.len(), 2);
        assert!(context_ayya
            .get(b"key1")
            .unwrap()
            .expect("cannot get data")
            .is_none());
        assert!(context_ayyb
            .get(b"key2")
            .unwrap()
            .expect("cannot get data")
            .is_none());

        // Commited batch's data should be visible in transaction
        storage
            .commit_multi_context_batch(batch, Some(&transaction))
            .unwrap()
            .expect("cannot commit multi-context batch");

        // Obtaining new contexts outside a commited batch but still within a
        // transaction
        let context_ayya = storage
            .get_transactional_storage_context(to_path(b"ayya"), &transaction)
            .unwrap();
        let context_ayyb = storage
            .get_transactional_storage_context(to_path(b"ayyb"), &transaction)
            .unwrap();

        assert_eq!(
            context_ayya.get(b"key1").unwrap().expect("cannot get data"),
            Some(b"value1".to_vec())
        );
        assert_eq!(
            context_ayyb.get(b"key2").unwrap().expect("cannot get data"),
            Some(b"value2".to_vec())
        );

        // And still no data in the database until transaction is commited
        let context_ayya = storage.get_storage_context(to_path(b"ayya")).unwrap();
        let context_ayyb = storage.get_storage_context(to_path(b"ayyb")).unwrap();

        let mut iter = context_ayya.raw_iter();
        iter.seek_to_first();
        assert!(!iter.valid().unwrap());

        let mut iter = context_ayyb.raw_iter();
        iter.seek_to_first();
        assert!(!iter.valid().unwrap());

        storage
            .commit_transaction(transaction)
            .unwrap()
            .expect("cannot commit transaction");

        let context_ayya = storage.get_storage_context(to_path(b"ayya")).unwrap();
        let context_ayyb = storage.get_storage_context(to_path(b"ayyb")).unwrap();

        assert_eq!(
            context_ayya.get(b"key1").unwrap().expect("cannot get data"),
            Some(b"value1".to_vec())
        );
        assert_eq!(
            context_ayyb.get(b"key2").unwrap().expect("cannot get data"),
            Some(b"value2".to_vec())
        );
    }
}
