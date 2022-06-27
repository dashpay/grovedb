use super::test_utils::TempStorage;

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

        let batch = context_ayya.new_batch();
        batch.delete(b"key1").expect("infallible");
        batch.put(b"key3", b"ayyavalue3").expect("infallible");

        context_ayya
            .commit_batch(batch)
            .expect("cannot commit a batch");

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
