// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Tests

use super::test_utils::TempStorage;
use crate::Batch;

mod immediate_storage {
    use super::*;
    use crate::{RawIterator, Storage, StorageContext};

    #[test]
    fn test_aux_cf_methods() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let context_ayya = storage
            .get_immediate_storage_context([b"ayya"].as_ref().into(), &tx)
            .unwrap();
        let context_ayyb = storage
            .get_immediate_storage_context([b"ayyb"].as_ref().into(), &tx)
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
            .get_immediate_storage_context([b"ayya"].as_ref().into(), &tx2)
            .unwrap();
        let tx3 = storage.start_transaction();
        let context_ayya_after_no_tx = storage
            .get_immediate_storage_context([b"ayya"].as_ref().into(), &tx3)
            .unwrap();

        context_ayya_after_tx
            .delete_aux(b"key1", None)
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

        // ... and no longer accessible at all after transaction got committed
        assert!(context_ayya_after_no_tx
            .get_aux(b"key1")
            .unwrap()
            .expect("cannot get from aux cf")
            .is_none());
    }

    #[test]
    fn test_roots_cf_methods() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let context_ayya = storage
            .get_immediate_storage_context([b"ayya"].as_ref().into(), &tx)
            .unwrap();
        let context_ayyb = storage
            .get_immediate_storage_context([b"ayyb"].as_ref().into(), &tx)
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
            .get_immediate_storage_context([b"ayya"].as_ref().into(), &tx2)
            .unwrap();
        let tx3 = storage.start_transaction();
        let context_ayya_after_no_tx = storage
            .get_immediate_storage_context([b"ayya"].as_ref().into(), &tx3)
            .unwrap();

        context_ayya_after_tx
            .delete_root(b"key1", None)
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

        // ... and no longer accessible at all after transaction got committed
        assert!(context_ayya_after_no_tx
            .get_root(b"key1")
            .unwrap()
            .expect("cannot get from roots cf")
            .is_none());
    }

    #[test]
    fn test_meta_cf_methods() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let context_ayya = storage
            .get_immediate_storage_context([b"ayya"].as_ref().into(), &tx)
            .unwrap();
        let context_ayyb = storage
            .get_immediate_storage_context([b"ayyb"].as_ref().into(), &tx)
            .unwrap();

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
            .delete_meta(b"key1", None)
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
        let tx = storage.start_transaction();
        let context_ayya = storage
            .get_immediate_storage_context([b"ayya"].as_ref().into(), &tx)
            .unwrap();
        let context_ayyb = storage
            .get_immediate_storage_context([b"ayyb"].as_ref().into(), &tx)
            .unwrap();

        context_ayya
            .put(b"key1", b"ayyavalue1", None, None)
            .unwrap()
            .expect("cannot insert into storage");
        context_ayya
            .put(b"key2", b"ayyavalue2", None, None)
            .unwrap()
            .expect("cannot insert into storage");
        context_ayyb
            .put(b"key1", b"ayybvalue1", None, None)
            .unwrap()
            .expect("cannot insert into storage");
        context_ayyb
            .put(b"key2", b"ayybvalue2", None, None)
            .unwrap()
            .expect("cannot insert into storage");

        assert_eq!(
            context_ayya
                .get(b"key1")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from storage"),
            b"ayyavalue1"
        );

        context_ayya
            .delete(b"key1", None)
            .unwrap()
            .expect("cannot delete from storage");

        assert!(context_ayya
            .get(b"key1")
            .unwrap()
            .expect("cannot get from storage")
            .is_none());
        assert_eq!(
            context_ayya
                .get(b"key2")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from storage"),
            b"ayyavalue2"
        );
        assert_eq!(
            context_ayyb
                .get(b"key1")
                .unwrap()
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
        let context_ayya = storage
            .get_immediate_storage_context([b"ayya"].as_ref().into(), &tx)
            .unwrap();

        context_ayya
            .put(b"key1", b"ayyavalue1", None, None)
            .unwrap()
            .expect("cannot insert into storage");
        context_ayya
            .put(b"key2", b"ayyavalue2", None, None)
            .unwrap()
            .expect("cannot insert into storage");

        assert!(context_ayya
            .get(b"key3")
            .unwrap()
            .expect("cannot get from storage")
            .is_none());

        let mut batch = context_ayya.new_batch();
        batch.delete(b"key1", None);
        batch.put(b"key3", b"ayyavalue3", None, None).unwrap();

        assert!(context_ayya
            .get(b"key1")
            .unwrap()
            .expect("cannot get from storage")
            .is_some());

        context_ayya
            .commit_batch(batch)
            .unwrap()
            .expect("cannot commit a batch");

        assert!(context_ayya
            .get(b"key1")
            .unwrap()
            .expect("cannot get from storage")
            .is_none());

        storage
            .commit_transaction(tx)
            .unwrap()
            .expect("cannot commit transaction");

        let tx = storage.start_transaction();
        let context_ayya = storage
            .get_immediate_storage_context([b"ayya"].as_ref().into(), &tx)
            .unwrap();
        assert_eq!(
            context_ayya
                .get(b"key3")
                .unwrap()
                .ok()
                .flatten()
                .expect("cannot get from storage"),
            b"ayyavalue3"
        );
        assert!(context_ayya
            .get(b"key1")
            .unwrap()
            .expect("cannot get from storage")
            .is_none());
    }

    #[test]
    fn test_raw_iterator() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let context = storage
            .get_immediate_storage_context([b"someprefix"].as_ref().into(), &tx)
            .unwrap();

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
            .get_immediate_storage_context([b"anothersomeprefix"].as_ref().into(), &tx)
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
            .get_immediate_storage_context([b"zanothersomeprefix"].as_ref().into(), &tx)
            .unwrap();
        context_after
            .put(b"key1", b"value1", None, None)
            .unwrap()
            .expect("expected successful insertion");
        context_after
            .put(b"key5", b"value5", None, None)
            .unwrap()
            .expect("expected successful insertion");

        let _ = storage.commit_transaction(tx).unwrap();

        // Test uncommitted changes
        {
            let tx = storage.start_transaction();
            let context_tx = storage
                .get_immediate_storage_context([b"someprefix"].as_ref().into(), &tx)
                .unwrap();

            context_tx
                .delete(b"key1", None)
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
            iter.seek_to_first().unwrap();
            while iter.valid().unwrap() {
                assert_eq!(
                    (iter.key().unwrap().unwrap(), iter.value().unwrap().unwrap()),
                    expected_iter.next().unwrap()
                );
                iter.next().unwrap();
            }
            assert!(expected_iter.next().is_none());

            // Test `seek_to_last` on a storage_cost with elements

            let mut iter = context_tx.raw_iter();
            iter.seek_to_last().unwrap();
            assert_eq!(
                (iter.key().unwrap().unwrap(), iter.value().unwrap().unwrap()),
                expected.last().unwrap().clone(),
            );
            iter.next().unwrap();
            assert!(!iter.valid().unwrap());
        }

        // Test committed data stay intact
        {
            let expected: [(&'static [u8], &'static [u8]); 4] = [
                (b"key0", b"value0"),
                (b"key1", b"value1"),
                (b"key2", b"value2"),
                (b"key3", b"value3"),
            ];
            let mut expected_iter = expected.into_iter();
            let tx = storage.start_transaction();
            let context = storage
                .get_immediate_storage_context([b"someprefix"].as_ref().into(), &tx)
                .unwrap();

            let mut iter = context.raw_iter();
            iter.seek_to_first().unwrap();
            while iter.valid().unwrap() {
                assert_eq!(
                    (iter.key().unwrap().unwrap(), iter.value().unwrap().unwrap()),
                    expected_iter.next().unwrap()
                );
                iter.next().unwrap();
            }
            assert!(expected_iter.next().is_none());
        }
    }

    #[test]
    fn test_raw_iter_aux() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let context = storage
            .get_immediate_storage_context([b"someprefix"].as_ref().into(), &tx)
            .unwrap();
        let other_context = storage
            .get_immediate_storage_context([b"anotherprefix"].as_ref().into(), &tx)
            .unwrap();

        for (key, value) in [
            (b"key2".as_slice(), b"value2".as_slice()),
            (b"key0", b"value0"),
            (b"key1", b"value1"),
        ] {
            context
                .put_aux(key, value, None)
                .unwrap()
                .expect("expected successful aux insertion");
        }
        // The same keys under another subtree prefix must stay invisible.
        other_context
            .put_aux(b"key0", b"other0", None)
            .unwrap()
            .expect("expected successful aux insertion");
        other_context
            .put_aux(b"key9", b"other9", None)
            .unwrap()
            .expect("expected successful aux insertion");
        // A data entry of the same context must not show up in the aux listing.
        context
            .put(b"key5", b"data5", None, None)
            .unwrap()
            .expect("expected successful insertion");

        let mut iter = context.raw_iter_aux();
        iter.seek_to_first().unwrap();
        let mut entries = Vec::new();
        while iter.valid().unwrap() {
            entries.push((
                iter.key().unwrap().expect("key").to_vec(),
                iter.value().unwrap().expect("value").to_vec(),
            ));
            iter.next().unwrap();
        }
        assert_eq!(
            entries,
            vec![
                (b"key0".to_vec(), b"value0".to_vec()),
                (b"key1".to_vec(), b"value1".to_vec()),
                (b"key2".to_vec(), b"value2".to_vec()),
            ]
        );

        // Seeking inside the prefix lands on the requested key.
        let mut iter = context.raw_iter_aux();
        iter.seek(b"key1").unwrap();
        assert_eq!(iter.key().unwrap(), Some(b"key1".as_slice()));
        iter.seek_to_last().unwrap();
        assert_eq!(iter.key().unwrap(), Some(b"key2".as_slice()));

        // The data iterator sees the data entry and none of the aux entries.
        let mut iter = context.raw_iter();
        iter.seek_to_first().unwrap();
        assert_eq!(iter.key().unwrap(), Some(b"key5".as_slice()));
        iter.next().unwrap();
        assert!(!iter.valid().unwrap());

        // The transactional context, the one GroveDb reads through, sees the
        // same aux entries.
        let tx_context = storage
            .get_transactional_storage_context([b"someprefix"].as_ref().into(), None, &tx)
            .unwrap();
        let mut iter = tx_context.raw_iter_aux();
        iter.seek_to_first().unwrap();
        let mut keys = Vec::new();
        while iter.valid().unwrap() {
            keys.push(iter.key().unwrap().expect("key").to_vec());
            iter.next().unwrap();
        }
        assert_eq!(
            keys,
            vec![b"key0".to_vec(), b"key1".to_vec(), b"key2".to_vec()]
        );
    }
}

mod batch_no_transaction {
    use super::*;
    use crate::{Batch, Storage, StorageBatch, StorageContext};

    #[test]
    fn test_various_cf_methods() {
        let storage = TempStorage::new();
        let batch = StorageBatch::new();
        let transaction = storage.start_transaction();

        let context_ayya = storage
            .get_transactional_storage_context(
                [b"ayya"].as_ref().into(),
                Some(&batch),
                &transaction,
            )
            .unwrap();
        let context_ayyb = storage
            .get_transactional_storage_context(
                [b"ayyb"].as_ref().into(),
                Some(&batch),
                &transaction,
            )
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
            .commit_multi_context_batch(batch, Some(&transaction))
            .unwrap()
            .expect("cannot commit batch");

        let context_ayya = storage
            .get_transactional_storage_context([b"ayya"].as_ref().into(), None, &transaction)
            .unwrap();
        let context_ayyb = storage
            .get_transactional_storage_context([b"ayyb"].as_ref().into(), None, &transaction)
            .unwrap();

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
        let transaction = storage.start_transaction();

        let context_ayya = storage
            .get_transactional_storage_context(
                [b"ayya"].as_ref().into(),
                Some(&batch),
                &transaction,
            )
            .unwrap();
        let context_ayyb = storage
            .get_transactional_storage_context(
                [b"ayyb"].as_ref().into(),
                Some(&batch),
                &transaction,
            )
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

        // DB batches are not committed yet, so these operations are missing from
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

        // DB batches are "committed", but actually staged in multi-context batch to do
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
            .commit_multi_context_batch(batch, Some(&transaction))
            .unwrap()
            .expect("cannot commit multi context batch");

        let context_ayya = storage
            .get_transactional_storage_context([b"ayya"].as_ref().into(), None, &transaction)
            .unwrap();
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
        let other_transaction = storage.start_transaction();
        let transaction = storage.start_transaction();

        let batch = StorageBatch::new();
        let batch_tx = StorageBatch::new();
        let context_ayya = storage
            .get_transactional_storage_context(
                [b"ayya"].as_ref().into(),
                Some(&batch),
                &other_transaction,
            )
            .unwrap();
        let context_ayyb = storage
            .get_transactional_storage_context(
                [b"ayyb"].as_ref().into(),
                Some(&batch),
                &other_transaction,
            )
            .unwrap();
        let context_ayya_tx = storage
            .get_transactional_storage_context(
                [b"ayya"].as_ref().into(),
                Some(&batch_tx),
                &transaction,
            )
            .unwrap();
        let context_ayyb_tx = storage
            .get_transactional_storage_context(
                [b"ayyb"].as_ref().into(),
                Some(&batch_tx),
                &transaction,
            )
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

        storage
            .commit_multi_context_batch(batch_tx, Some(&transaction))
            .unwrap()
            .expect("cannot commit a non-tx multi context batch");

        let another_batch_tx = StorageBatch::new();
        let context_ayya_tx = storage
            .get_transactional_storage_context(
                [b"ayya"].as_ref().into(),
                Some(&another_batch_tx),
                &transaction,
            )
            .unwrap();
        let context_ayyb_tx = storage
            .get_transactional_storage_context(
                [b"ayyb"].as_ref().into(),
                Some(&another_batch_tx),
                &transaction,
            )
            .unwrap();

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
        // batch is committed

        let batch = StorageBatch::new();
        let context_ayya_batch = storage
            .get_transactional_storage_context(
                [b"ayya"].as_ref().into(),
                Some(&batch),
                &transaction,
            )
            .unwrap();
        let context_ayyb_batch = storage
            .get_transactional_storage_context(
                [b"ayyb"].as_ref().into(),
                Some(&batch),
                &transaction,
            )
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

        // Committed batch data is accessible in transaction but not outside
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

    /// A direct `put` through the transactional context that describes a
    /// NEW node must have its key cost completed with the path prefix by the
    /// context — the caller cannot know the prefix — exactly as the `Batch`
    /// implementations do. An update (`new_node: false`) is passed through
    /// unchanged. Both must then survive the commit-time verification.
    /// A fully prepaid put (`KeyValueStorageCost::prepaid`) is billed
    /// nothing at commit — no bytes, no key, no value verification and no
    /// seek — while an ordinary put beside it is still charged its seek.
    #[test]
    fn test_prepaid_put_is_charged_no_seek_at_commit() {
        use grovedb_costs::storage_cost::key_value_cost::KeyValueStorageCost;

        let storage = TempStorage::new();
        let transaction = storage.start_transaction();
        let batch = StorageBatch::new();
        let context = storage
            .get_transactional_storage_context(
                [b"ayya"].as_ref().into(),
                Some(&batch),
                &transaction,
            )
            .unwrap();
        context
            .put(
                b"prepaid",
                &[1u8; 100],
                None,
                Some(KeyValueStorageCost::prepaid()),
            )
            .unwrap()
            .expect("put prepaid");
        context
            .put(b"ordinary", &[2u8; 100], None, None)
            .unwrap()
            .expect("put ordinary");
        // A zero-cost DEFAULT is not prepaid: it pays its seek like any put
        // (Merk passes one for writes it charges no bytes for).
        context
            .put(
                b"default",
                &[3u8; 100],
                None,
                Some(KeyValueStorageCost::default()),
            )
            .unwrap()
            .expect("put default");
        let commit = storage.commit_multi_context_batch(batch, Some(&transaction));
        commit.value.expect("commit");
        let cost = commit.cost;
        assert_eq!(
            cost.seek_count, 2,
            "the ordinary and the default-cost puts seek, the prepaid one does not: {cost:?}"
        );
        // The ordinary put's bytes (prefixed key + value, each with its
        // length varint) are the whole storage figure.
        assert_eq!(
            cost.storage_cost.added_bytes,
            (32 + 8 + 1) + (100 + 1),
            "{cost:?}"
        );
        assert_eq!(cost.storage_cost.replaced_bytes, 0);
        let tx_ctx = storage
            .get_transactional_storage_context([b"ayya"].as_ref().into(), None, &transaction)
            .unwrap();
        assert_eq!(
            tx_ctx.get(b"prepaid").unwrap().expect("get"),
            Some(vec![1u8; 100]),
            "the prepaid put is written all the same"
        );
    }

    /// Every costed put variant honours the marker, on both commit paths:
    /// the `StorageBatch` committed through `continue_write_batch` (data,
    /// aux, roots, meta) and the direct `PrefixedRocksDbBatch` of an
    /// immediate context (data, aux, roots). Prepaid puts seek nothing;
    /// the ordinary puts beside them seek once each.
    #[test]
    fn test_prepaid_puts_of_every_variant_are_charged_no_seek() {
        use grovedb_costs::storage_cost::key_value_cost::KeyValueStorageCost;

        // StorageBatch path.
        let storage = TempStorage::new();
        let transaction = storage.start_transaction();
        let batch = StorageBatch::new();
        let context = storage
            .get_transactional_storage_context(
                [b"ayya"].as_ref().into(),
                Some(&batch),
                &transaction,
            )
            .unwrap();
        let prepaid = || Some(KeyValueStorageCost::prepaid());
        context
            .put(b"d", &[1u8; 10], None, prepaid())
            .unwrap()
            .expect("put");
        context
            .put_aux(b"a", &[2u8; 10], prepaid())
            .unwrap()
            .expect("put_aux");
        context
            .put_root(b"r", &[3u8; 10], prepaid())
            .unwrap()
            .expect("put_root");
        context
            .put_meta(b"m", &[4u8; 10], prepaid())
            .unwrap()
            .expect("put_meta");
        context
            .put(b"d2", &[5u8; 10], None, None)
            .unwrap()
            .expect("put");
        context
            .put_aux(b"a2", &[6u8; 10], None)
            .unwrap()
            .expect("put_aux");
        context
            .put_root(b"r2", &[7u8; 10], None)
            .unwrap()
            .expect("put_root");
        context
            .put_meta(b"m2", &[8u8; 10], None)
            .unwrap()
            .expect("put_meta");
        let commit = storage.commit_multi_context_batch(batch, Some(&transaction));
        commit.value.expect("commit");
        assert_eq!(
            commit.cost.seek_count, 4,
            "one seek per ordinary put, none for the prepaid ones: {:?}",
            commit.cost
        );
        let ctx = storage
            .get_transactional_storage_context([b"ayya"].as_ref().into(), None, &transaction)
            .unwrap();
        assert_eq!(ctx.get(b"d").unwrap().expect("get"), Some(vec![1u8; 10]));
        assert_eq!(
            ctx.get_aux(b"a").unwrap().expect("get_aux"),
            Some(vec![2u8; 10])
        );
        assert_eq!(
            ctx.get_root(b"r").unwrap().expect("get_root"),
            Some(vec![3u8; 10])
        );
        assert_eq!(
            ctx.get_meta(b"m").unwrap().expect("get_meta"),
            Some(vec![4u8; 10])
        );

        // Direct batch path of an immediate context.
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let context = storage
            .get_immediate_storage_context([b"ayya"].as_ref().into(), &tx)
            .unwrap();
        let mut db_batch = context.new_batch();
        db_batch
            .put(b"d", &[1u8; 10], None, prepaid())
            .expect("put");
        db_batch
            .put_aux(b"a", &[2u8; 10], prepaid())
            .expect("put_aux");
        db_batch
            .put_root(b"r", &[3u8; 10], prepaid())
            .expect("put_root");
        db_batch.put(b"d2", &[5u8; 10], None, None).expect("put");
        db_batch.put_aux(b"a2", &[6u8; 10], None).expect("put_aux");
        db_batch
            .put_root(b"r2", &[7u8; 10], None)
            .expect("put_root");
        let commit = context.commit_batch(db_batch);
        commit.value.expect("commit");
        assert_eq!(
            commit.cost.seek_count, 3,
            "one seek per ordinary put, none for the prepaid ones: {:?}",
            commit.cost
        );
        assert_eq!(
            context.get(b"d").unwrap().expect("get"),
            Some(vec![1u8; 10])
        );
        assert_eq!(
            context.get_aux(b"a").unwrap().expect("get_aux"),
            Some(vec![2u8; 10])
        );
        assert_eq!(
            context.get_root(b"r").unwrap().expect("get_root"),
            Some(vec![3u8; 10])
        );
    }

    #[test]
    fn test_transactional_put_completes_new_node_key_cost() {
        use grovedb_costs::storage_cost::{
            key_value_cost::KeyValueStorageCost, removal::StorageRemovedBytes::NoStorageRemoval,
            StorageCost,
        };
        use integer_encoding::VarInt;

        let storage = TempStorage::new();
        let transaction = storage.start_transaction();
        let batch = StorageBatch::new();
        let context = storage
            .get_transactional_storage_context(
                [b"ayya"].as_ref().into(),
                Some(&batch),
                &transaction,
            )
            .unwrap();

        // A new node: key cost supplied WITHOUT the prefix (zero), value cost
        // split into a replaced and an added part.
        let value = vec![7u8; 100];
        let paid_value = value.len() as u32 + value.len().required_space() as u32;
        context
            .put(
                b"new",
                &value,
                None,
                Some(KeyValueStorageCost {
                    key_storage_cost: StorageCost::default(),
                    value_storage_cost: StorageCost {
                        added_bytes: 11,
                        replaced_bytes: paid_value - 11,
                        removed_bytes: NoStorageRemoval,
                    },
                    new_node: true,
                    needs_value_verification: true,
                    prepaid: false,
                }),
            )
            .unwrap()
            .expect("put new node");

        // An update: key cost zero, value fully replaced.
        context
            .put(
                b"old",
                &value,
                None,
                Some(KeyValueStorageCost {
                    key_storage_cost: StorageCost::default(),
                    value_storage_cost: StorageCost {
                        added_bytes: 0,
                        replaced_bytes: paid_value,
                        removed_bytes: NoStorageRemoval,
                    },
                    new_node: false,
                    needs_value_verification: true,
                    prepaid: false,
                }),
            )
            .unwrap()
            .expect("put update");

        let commit = storage.commit_multi_context_batch(batch, Some(&transaction));
        commit.value.expect("commit must pass cost verification");
        let cost = commit.cost;

        // prefix (32) + "new" (3) = 35, + 1 byte of length.
        let new_key_paid = 32 + 3 + 1;
        assert_eq!(
            cost.storage_cost.added_bytes,
            new_key_paid + 11,
            "new node: prefixed key + the added part of the value"
        );
        assert_eq!(
            cost.storage_cost.replaced_bytes,
            (paid_value - 11) + paid_value,
            "replaced parts of both values; no key cost for the update"
        );
    }

    #[test]
    fn test_db_batch_in_transaction_merged_into_context_batch() {
        let storage = TempStorage::new();
        let transaction = storage.start_transaction();
        let batch = StorageBatch::new();

        let context_ayya = storage
            .get_transactional_storage_context(
                [b"ayya"].as_ref().into(),
                Some(&batch),
                &transaction,
            )
            .unwrap();
        let context_ayyb = storage
            .get_transactional_storage_context(
                [b"ayyb"].as_ref().into(),
                Some(&batch),
                &transaction,
            )
            .unwrap();

        let mut db_batch_a = context_ayya.new_batch();
        let mut db_batch_b = context_ayyb.new_batch();

        db_batch_a.put(b"key1", b"value1", None, None).unwrap();
        db_batch_b.put(b"key2", b"value2", None, None).unwrap();

        // Until db batches are committed our multi-context batch should be empty
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

        // Committed batch's data should be visible in transaction
        storage
            .commit_multi_context_batch(batch, Some(&transaction))
            .unwrap()
            .expect("cannot commit multi-context batch");

        // Obtaining new contexts outside a committed batch but still within a
        // transaction
        let context_ayya = storage
            .get_transactional_storage_context([b"ayya"].as_ref().into(), None, &transaction)
            .unwrap();
        let context_ayyb = storage
            .get_transactional_storage_context([b"ayyb"].as_ref().into(), None, &transaction)
            .unwrap();

        assert_eq!(
            context_ayya.get(b"key1").unwrap().expect("cannot get data"),
            Some(b"value1".to_vec())
        );
        assert_eq!(
            context_ayyb.get(b"key2").unwrap().expect("cannot get data"),
            Some(b"value2".to_vec())
        );

        // And still no data in the database until transaction is committed
        let other_transaction = storage.start_transaction();
        let context_ayya = storage
            .get_transactional_storage_context([b"ayya"].as_ref().into(), None, &other_transaction)
            .unwrap();
        let context_ayyb = storage
            .get_transactional_storage_context([b"ayyb"].as_ref().into(), None, &other_transaction)
            .unwrap();

        let mut iter = context_ayya.raw_iter();
        iter.seek_to_first().unwrap();
        assert!(!iter.valid().unwrap());

        let mut iter = context_ayyb.raw_iter();
        iter.seek_to_first().unwrap();
        assert!(!iter.valid().unwrap());

        storage
            .commit_transaction(transaction)
            .unwrap()
            .expect("cannot commit transaction");

        let other_transaction = storage.start_transaction();
        let context_ayya = storage
            .get_transactional_storage_context([b"ayya"].as_ref().into(), None, &other_transaction)
            .unwrap();
        let context_ayyb = storage
            .get_transactional_storage_context([b"ayyb"].as_ref().into(), None, &other_transaction)
            .unwrap();

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

mod storage_management {
    use tempfile::TempDir;

    use super::*;
    use crate::{rocksdb_storage::RocksDbStorage, Storage, StorageBatch, StorageContext};

    #[test]
    fn test_contexts_by_subtree_prefix_match_contexts_by_path() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();

        let prefix = RocksDbStorage::build_prefix([b"prefix-tree"].as_ref().into()).unwrap();
        let by_prefix = storage
            .get_immediate_storage_context_by_subtree_prefix(prefix, &tx)
            .unwrap();
        let by_path = storage
            .get_immediate_storage_context([b"prefix-tree"].as_ref().into(), &tx)
            .unwrap();

        by_prefix
            .put(b"key-a", b"value-a", None, None)
            .unwrap()
            .expect("put via subtree prefix should succeed");
        assert_eq!(
            by_path.get(b"key-a").unwrap().expect("get should succeed"),
            Some(b"value-a".to_vec())
        );

        let batch = StorageBatch::new();
        let tx_by_prefix =
            storage.get_transactional_storage_context_by_subtree_prefix(prefix, Some(&batch), &tx);
        tx_by_prefix
            .unwrap()
            .put(b"key-b", b"value-b", None, None)
            .unwrap()
            .expect("put in transactional context should succeed");
        storage
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit of staged writes should succeed");

        assert_eq!(
            by_path.get(b"key-b").unwrap().expect("get should succeed"),
            Some(b"value-b".to_vec())
        );
    }

    #[test]
    fn test_transactional_clear_removes_only_target_subtree() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();

        let initial_batch = StorageBatch::new();
        let context_a = storage
            .get_transactional_storage_context(
                [b"tree-a"].as_ref().into(),
                Some(&initial_batch),
                &tx,
            )
            .unwrap();
        let context_b = storage
            .get_transactional_storage_context(
                [b"tree-b"].as_ref().into(),
                Some(&initial_batch),
                &tx,
            )
            .unwrap();

        context_a.put(b"k1", b"v1", None, None).unwrap().unwrap();
        context_a.put(b"k2", b"v2", None, None).unwrap().unwrap();
        context_b.put(b"k3", b"v3", None, None).unwrap().unwrap();

        storage
            .commit_multi_context_batch(initial_batch, Some(&tx))
            .unwrap()
            .expect("initial commit should succeed");

        let clear_batch = StorageBatch::new();
        let mut clear_context = storage
            .get_transactional_storage_context([b"tree-a"].as_ref().into(), Some(&clear_batch), &tx)
            .unwrap();
        clear_context
            .clear()
            .unwrap()
            .expect("clear should succeed");

        storage
            .commit_multi_context_batch(clear_batch, Some(&tx))
            .unwrap()
            .expect("clear batch should commit");

        let verify_a = storage
            .get_transactional_storage_context([b"tree-a"].as_ref().into(), None, &tx)
            .unwrap();
        let verify_b = storage
            .get_transactional_storage_context([b"tree-b"].as_ref().into(), None, &tx)
            .unwrap();

        assert!(verify_a
            .get(b"k1")
            .unwrap()
            .expect("get should succeed")
            .is_none());
        assert!(verify_a
            .get(b"k2")
            .unwrap()
            .expect("get should succeed")
            .is_none());
        assert_eq!(
            verify_b.get(b"k3").unwrap().expect("get should succeed"),
            Some(b"v3".to_vec())
        );
    }

    #[test]
    fn test_rollback_and_flush() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let context = storage
            .get_immediate_storage_context([b"rollback"].as_ref().into(), &tx)
            .unwrap();

        context
            .put(b"key", b"value", None, None)
            .unwrap()
            .expect("put should succeed");

        storage
            .rollback_transaction(&tx)
            .expect("rollback should succeed");

        let tx_after = storage.start_transaction();
        let context_after = storage
            .get_immediate_storage_context([b"rollback"].as_ref().into(), &tx_after)
            .unwrap();
        assert!(context_after
            .get(b"key")
            .unwrap()
            .expect("get should succeed")
            .is_none());

        storage.flush().expect("flush should succeed");
    }

    #[test]
    fn test_checkpoint_is_independent_snapshot() {
        let db_dir = TempDir::new().expect("cannot create db directory");
        let checkpoint_parent = TempDir::new().expect("cannot create checkpoint directory");
        let checkpoint_dir = checkpoint_parent.path().join("checkpoint");

        let storage = RocksDbStorage::default_rocksdb_with_path(db_dir.path())
            .expect("cannot create storage");
        let tx = storage.start_transaction();
        let context = storage
            .get_immediate_storage_context([b"checkpoint"].as_ref().into(), &tx)
            .unwrap();
        context
            .put(b"k1", b"v1", None, None)
            .unwrap()
            .expect("put should succeed");
        storage
            .commit_transaction(tx)
            .unwrap()
            .expect("tx commit should succeed");

        storage
            .create_checkpoint(&checkpoint_dir)
            .expect("checkpoint should be created");

        // Write more data to the original after the checkpoint
        let tx2 = storage.start_transaction();
        let context2 = storage
            .get_immediate_storage_context([b"checkpoint"].as_ref().into(), &tx2)
            .unwrap();
        context2
            .put(b"k2", b"v2", None, None)
            .unwrap()
            .expect("put should succeed");
        storage
            .commit_transaction(tx2)
            .unwrap()
            .expect("tx commit should succeed");

        // Open the checkpoint and verify it has only the pre-checkpoint data
        let checkpoint_storage = RocksDbStorage::checkpoint_rocksdb_with_path(&checkpoint_dir)
            .expect("checkpoint should open");
        let checkpoint_tx = checkpoint_storage.start_transaction();
        let checkpoint_context = checkpoint_storage
            .get_immediate_storage_context([b"checkpoint"].as_ref().into(), &checkpoint_tx)
            .unwrap();

        assert_eq!(
            checkpoint_context
                .get(b"k1")
                .unwrap()
                .expect("get should succeed"),
            Some(b"v1".to_vec()),
            "checkpoint should contain data written before checkpoint"
        );
        assert!(
            checkpoint_context
                .get(b"k2")
                .unwrap()
                .expect("get should succeed")
                .is_none(),
            "checkpoint should NOT contain data written after checkpoint"
        );
    }

    #[test]
    fn test_wipe_clears_default_and_column_families() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let context = storage
            .get_immediate_storage_context([b"wipe"].as_ref().into(), &tx)
            .unwrap();

        context.put(b"k", b"v", None, None).unwrap().unwrap();
        context.put_aux(b"ak", b"av", None).unwrap().unwrap();
        context.put_root(b"rk", b"rv", None).unwrap().unwrap();
        context.put_meta(b"mk", b"mv", None).unwrap().unwrap();

        storage
            .commit_transaction(tx)
            .unwrap()
            .expect("tx commit should succeed");

        storage.wipe().expect("wipe should succeed");

        let verify_tx = storage.start_transaction();
        let verify_context = storage
            .get_immediate_storage_context([b"wipe"].as_ref().into(), &verify_tx)
            .unwrap();

        assert!(verify_context.get(b"k").unwrap().unwrap().is_none());
        assert!(verify_context.get_aux(b"ak").unwrap().unwrap().is_none());
        assert!(verify_context.get_root(b"rk").unwrap().unwrap().is_none());
        assert!(verify_context.get_meta(b"mk").unwrap().unwrap().is_none());
    }

    #[test]
    fn test_wipe_range_deletion_with_many_keys() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let context = storage
            .get_immediate_storage_context([b"many"].as_ref().into(), &tx)
            .unwrap();

        // Insert many keys across all column families
        for i in 0u32..100 {
            let key = format!("key_{:04}", i);
            let val = format!("val_{}", i);
            context
                .put(key.as_bytes(), val.as_bytes(), None, None)
                .unwrap()
                .unwrap();
            context
                .put_aux(key.as_bytes(), val.as_bytes(), None)
                .unwrap()
                .unwrap();
            context
                .put_root(key.as_bytes(), val.as_bytes(), None)
                .unwrap()
                .unwrap();
            context
                .put_meta(key.as_bytes(), val.as_bytes(), None)
                .unwrap()
                .unwrap();
        }

        storage
            .commit_transaction(tx)
            .unwrap()
            .expect("tx commit should succeed");

        storage.wipe().expect("wipe should succeed");

        // Verify all keys are gone
        let verify_tx = storage.start_transaction();
        let verify_ctx = storage
            .get_immediate_storage_context([b"many"].as_ref().into(), &verify_tx)
            .unwrap();

        for i in 0u32..100 {
            let key = format!("key_{:04}", i);
            assert!(
                verify_ctx.get(key.as_bytes()).unwrap().unwrap().is_none(),
                "default CF key {key} should be gone after wipe"
            );
            assert!(
                verify_ctx
                    .get_aux(key.as_bytes())
                    .unwrap()
                    .unwrap()
                    .is_none(),
                "aux CF key {key} should be gone after wipe"
            );
            assert!(
                verify_ctx
                    .get_root(key.as_bytes())
                    .unwrap()
                    .unwrap()
                    .is_none(),
                "roots CF key {key} should be gone after wipe"
            );
            assert!(
                verify_ctx
                    .get_meta(key.as_bytes())
                    .unwrap()
                    .unwrap()
                    .is_none(),
                "meta CF key {key} should be gone after wipe"
            );
        }

        // Verify DB is still functional after wipe — can insert new data
        let _ = verify_ctx;
        storage
            .commit_transaction(verify_tx)
            .unwrap()
            .expect("verify tx commit should succeed");

        let tx2 = storage.start_transaction();
        let ctx2 = storage
            .get_immediate_storage_context([b"many"].as_ref().into(), &tx2)
            .unwrap();
        ctx2.put(b"after_wipe", b"works", None, None)
            .unwrap()
            .unwrap();
        storage
            .commit_transaction(tx2)
            .unwrap()
            .expect("post-wipe tx commit should succeed");

        let tx3 = storage.start_transaction();
        let ctx3 = storage
            .get_immediate_storage_context([b"many"].as_ref().into(), &tx3)
            .unwrap();
        assert_eq!(
            ctx3.get(b"after_wipe").unwrap().unwrap().as_deref(),
            Some(b"works".as_ref()),
            "DB should be functional after wipe"
        );
    }
}

mod transactional_context_without_batch {
    use super::*;
    use crate::{Storage, StorageBatch, StorageContext};

    #[test]
    fn test_write_operations_error_when_batch_is_none() {
        let storage = TempStorage::new();
        let transaction = storage.start_transaction();

        // Create a transactional context with batch = None (read-only context)
        let context = storage
            .get_transactional_storage_context([b"test"].as_ref().into(), None, &transaction)
            .unwrap();

        // All write operations should return an error instead of silently succeeding

        // put
        let result = context.put(b"key", b"value", None, None).unwrap();
        assert!(
            result.is_err(),
            "put should fail on transactional context without a batch"
        );

        // put_aux
        let result = context.put_aux(b"key", b"value", None).unwrap();
        assert!(
            result.is_err(),
            "put_aux should fail on transactional context without a batch"
        );

        // put_root
        let result = context.put_root(b"key", b"value", None).unwrap();
        assert!(
            result.is_err(),
            "put_root should fail on transactional context without a batch"
        );

        // put_meta
        let result = context.put_meta(b"key", b"value", None).unwrap();
        assert!(
            result.is_err(),
            "put_meta should fail on transactional context without a batch"
        );

        // delete
        let result = context.delete(b"key", None).unwrap();
        assert!(
            result.is_err(),
            "delete should fail on transactional context without a batch"
        );

        // delete_aux
        let result = context.delete_aux(b"key", None).unwrap();
        assert!(
            result.is_err(),
            "delete_aux should fail on transactional context without a batch"
        );

        // delete_root
        let result = context.delete_root(b"key", None).unwrap();
        assert!(
            result.is_err(),
            "delete_root should fail on transactional context without a batch"
        );

        // delete_meta
        let result = context.delete_meta(b"key", None).unwrap();
        assert!(
            result.is_err(),
            "delete_meta should fail on transactional context without a batch"
        );

        // commit_batch
        let new_batch = context.new_batch();
        let result = context.commit_batch(new_batch).unwrap();
        assert!(
            result.is_err(),
            "commit_batch should fail on transactional context without a batch"
        );
    }

    #[test]
    fn test_read_operations_succeed_when_batch_is_none() {
        let storage = TempStorage::new();
        let transaction = storage.start_transaction();

        // First, write some data using a context WITH a batch
        let batch = StorageBatch::new();
        let write_context = storage
            .get_transactional_storage_context(
                [b"test"].as_ref().into(),
                Some(&batch),
                &transaction,
            )
            .unwrap();
        write_context
            .put(b"key", b"value", None, None)
            .unwrap()
            .expect("put with batch should succeed");
        storage
            .commit_multi_context_batch(batch, Some(&transaction))
            .unwrap()
            .expect("commit batch should succeed");

        // Now create a read-only context (batch = None) and verify reads work
        let read_context = storage
            .get_transactional_storage_context([b"test"].as_ref().into(), None, &transaction)
            .unwrap();

        let result = read_context.get(b"key").unwrap();
        assert!(
            result.is_ok(),
            "get should succeed on transactional context without a batch"
        );
        assert_eq!(
            result.unwrap(),
            Some(b"value".to_vec()),
            "get should return the correct value"
        );
    }
}

/// Item 3 of the snapshot-read hardening (issue #832): correctness of a
/// snapshot read transaction depends on EVERY read method of both
/// prefixed transaction contexts threading the snapshot through its
/// read options. The trait's read surface is small and closed — get /
/// get_aux / get_root / get_meta and the raw iterator — so one
/// conformance walk per context covers it, and a read method added
/// later without snapshot plumbing fails here as an obvious
/// test-extension gap instead of silently reverting to
/// latest-committed reads.
mod snapshot_read_transactions {
    use super::*;
    use crate::{error::Error, RawIterator, Storage, StorageBatch, StorageContext};

    const PATH: &[&[u8]] = &[b"tree"];

    /// Seed every column family under one prefix, committed to the DB:
    /// `updated` and `removed` keys everywhere, plus `iter_a`..`iter_c`
    /// in the data CF for the iterator walks.
    fn seed(storage: &TempStorage) {
        let tx = storage.start_transaction();
        let batch = StorageBatch::new();
        let ctx = storage
            .get_transactional_storage_context(PATH.into(), Some(&batch), &tx)
            .unwrap();
        for (key, value) in [
            (b"updated".as_slice(), b"before".as_slice()),
            (b"removed", b"doomed"),
            (b"iter_a", b"va"),
            (b"iter_b", b"vb"),
            (b"iter_c", b"vc"),
        ] {
            ctx.put(key, value, None, None).unwrap().expect("seed put");
        }
        ctx.put_aux(b"updated", b"aux_before", None)
            .unwrap()
            .expect("seed put_aux");
        ctx.put_aux(b"removed", b"aux_doomed", None)
            .unwrap()
            .expect("seed put_aux");
        ctx.put_root(b"updated", b"root_before", None)
            .unwrap()
            .expect("seed put_root");
        ctx.put_root(b"removed", b"root_doomed", None)
            .unwrap()
            .expect("seed put_root");
        ctx.put_meta(b"updated", b"meta_before", None)
            .unwrap()
            .expect("seed put_meta");
        ctx.put_meta(b"removed", b"meta_doomed", None)
            .unwrap()
            .expect("seed put_meta");
        storage
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("seed batch commit");
        storage
            .commit_transaction(tx)
            .unwrap()
            .expect("seed tx commit");
    }

    /// The "concurrent block commit" that lands AFTER the snapshot
    /// transaction was created: overwrite `updated`, delete `removed`,
    /// and insert `fresh` in every column family (data CF also swaps
    /// `iter_c` for `iter_d`).
    fn commit_concurrent_change(storage: &TempStorage) {
        let tx = storage.start_transaction();
        let batch = StorageBatch::new();
        let ctx = storage
            .get_transactional_storage_context(PATH.into(), Some(&batch), &tx)
            .unwrap();
        ctx.put(b"updated", b"after", None, None)
            .unwrap()
            .expect("overwrite");
        ctx.put(b"fresh", b"created", None, None)
            .unwrap()
            .expect("insert");
        ctx.put(b"iter_d", b"vd", None, None)
            .unwrap()
            .expect("insert");
        ctx.delete(b"removed", None).unwrap().expect("delete");
        ctx.delete(b"iter_c", None).unwrap().expect("delete");
        ctx.put_aux(b"updated", b"aux_after", None)
            .unwrap()
            .expect("overwrite aux");
        ctx.put_aux(b"fresh", b"aux_created", None)
            .unwrap()
            .expect("insert aux");
        ctx.delete_aux(b"removed", None)
            .unwrap()
            .expect("delete aux");
        ctx.put_root(b"updated", b"root_after", None)
            .unwrap()
            .expect("overwrite root");
        ctx.put_root(b"fresh", b"root_created", None)
            .unwrap()
            .expect("insert root");
        ctx.delete_root(b"removed", None)
            .unwrap()
            .expect("delete root");
        ctx.put_meta(b"updated", b"meta_after", None)
            .unwrap()
            .expect("overwrite meta");
        ctx.put_meta(b"fresh", b"meta_created", None)
            .unwrap()
            .expect("insert meta");
        ctx.delete_meta(b"removed", None)
            .unwrap()
            .expect("delete meta");
        storage
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("concurrent batch commit");
        storage
            .commit_transaction(tx)
            .unwrap()
            .expect("concurrent tx commit");
    }

    fn get<'db>(ctx: &impl StorageContext<'db>, key: &[u8]) -> Option<Vec<u8>> {
        ctx.get(key).unwrap().expect("get")
    }

    /// Walk the whole prefix forward from `seek_to_first`, collecting
    /// (key, value) pairs.
    fn collect_forward<'db>(ctx: &impl StorageContext<'db>) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut iter = ctx.raw_iter();
        iter.seek_to_first().unwrap();
        let mut out = Vec::new();
        while iter.valid().unwrap() {
            out.push((
                iter.key().unwrap().expect("key").to_vec(),
                iter.value().unwrap().expect("value").to_vec(),
            ));
            iter.next().unwrap();
        }
        out
    }

    /// Walk the whole prefix backward from `seek_to_last`.
    fn collect_backward<'db>(ctx: &impl StorageContext<'db>) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut iter = ctx.raw_iter();
        iter.seek_to_last().unwrap();
        let mut out = Vec::new();
        while iter.valid().unwrap() {
            out.push((
                iter.key().unwrap().expect("key").to_vec(),
                iter.value().unwrap().expect("value").to_vec(),
            ));
            iter.prev().unwrap();
        }
        out
    }

    /// Every read method of the given context must observe the
    /// creation-time committed state: the pre-commit value of the
    /// overwritten key, the still-present deleted key, no trace of the
    /// post-snapshot insertions.
    fn assert_reads_pinned<'db>(ctx: &impl StorageContext<'db>) {
        // Point reads, one per column family.
        assert_eq!(get(ctx, b"updated"), Some(b"before".to_vec()));
        assert_eq!(get(ctx, b"removed"), Some(b"doomed".to_vec()));
        assert_eq!(get(ctx, b"fresh"), None);
        assert_eq!(
            ctx.get_aux(b"updated").unwrap().expect("get_aux"),
            Some(b"aux_before".to_vec())
        );
        assert_eq!(
            ctx.get_aux(b"removed").unwrap().expect("get_aux"),
            Some(b"aux_doomed".to_vec())
        );
        assert_eq!(ctx.get_aux(b"fresh").unwrap().expect("get_aux"), None);
        assert_eq!(
            ctx.get_root(b"updated").unwrap().expect("get_root"),
            Some(b"root_before".to_vec())
        );
        assert_eq!(
            ctx.get_root(b"removed").unwrap().expect("get_root"),
            Some(b"root_doomed".to_vec())
        );
        assert_eq!(ctx.get_root(b"fresh").unwrap().expect("get_root"), None);
        assert_eq!(
            ctx.get_meta(b"updated").unwrap().expect("get_meta"),
            Some(b"meta_before".to_vec())
        );
        assert_eq!(
            ctx.get_meta(b"removed").unwrap().expect("get_meta"),
            Some(b"meta_doomed".to_vec())
        );
        assert_eq!(ctx.get_meta(b"fresh").unwrap().expect("get_meta"), None);

        // Raw iterator, every navigation variant. The pre-commit data
        // CF holds exactly these five keys under the prefix.
        let pinned: Vec<(Vec<u8>, Vec<u8>)> = [
            (b"iter_a".as_slice(), b"va".as_slice()),
            (b"iter_b", b"vb"),
            (b"iter_c", b"vc"),
            (b"removed", b"doomed"),
            (b"updated", b"before"),
        ]
        .iter()
        .map(|(k, v)| (k.to_vec(), v.to_vec()))
        .collect();
        assert_eq!(collect_forward(ctx), pinned, "forward walk");
        assert_eq!(
            collect_backward(ctx),
            pinned.iter().cloned().rev().collect::<Vec<_>>(),
            "backward walk"
        );

        // seek lands on the deleted-after-snapshot key, seek_for_prev
        // must not see the post-snapshot `iter_d`.
        let mut iter = ctx.raw_iter();
        iter.seek(b"iter_c").unwrap();
        assert_eq!(
            iter.key().unwrap().expect("seek key"),
            b"iter_c",
            "seek sees the pre-commit key"
        );
        let mut iter = ctx.raw_iter();
        iter.seek_for_prev(b"iter_d").unwrap();
        assert_eq!(
            iter.key().unwrap().expect("seek_for_prev key"),
            b"iter_c",
            "seek_for_prev lands before the invisible post-snapshot key"
        );
    }

    /// The mirror-image control: a context on latest-committed state
    /// must see the post-commit values.
    fn assert_reads_latest<'db>(ctx: &impl StorageContext<'db>) {
        assert_eq!(get(ctx, b"updated"), Some(b"after".to_vec()));
        assert_eq!(get(ctx, b"removed"), None);
        assert_eq!(get(ctx, b"fresh"), Some(b"created".to_vec()));
        assert_eq!(
            ctx.get_aux(b"fresh").unwrap().expect("get_aux"),
            Some(b"aux_created".to_vec())
        );
        assert_eq!(
            ctx.get_root(b"fresh").unwrap().expect("get_root"),
            Some(b"root_created".to_vec())
        );
        assert_eq!(
            ctx.get_meta(b"fresh").unwrap().expect("get_meta"),
            Some(b"meta_created".to_vec())
        );
        let keys: Vec<Vec<u8>> = collect_forward(ctx).into_iter().map(|(k, _)| k).collect();
        assert!(keys.contains(&b"iter_d".to_vec()), "latest sees iter_d");
        assert!(!keys.contains(&b"iter_c".to_vec()), "iter_c is gone");
    }

    #[test]
    fn every_read_method_of_the_transactional_context_is_pinned() {
        let storage = TempStorage::new();
        seed(&storage);
        let snapshot_tx = storage.start_snapshot_read_transaction();
        let plain_tx = storage.start_transaction();
        commit_concurrent_change(&storage);

        let pinned_ctx = storage
            .get_transactional_storage_context(PATH.into(), None, &snapshot_tx)
            .unwrap();
        assert_reads_pinned(&pinned_ctx);

        // A plain transaction reads latest-committed on every
        // operation — the exact behavior the snapshot suppresses.
        let latest_ctx = storage
            .get_transactional_storage_context(PATH.into(), None, &plain_tx)
            .unwrap();
        assert_reads_latest(&latest_ctx);
    }

    #[test]
    fn every_read_method_of_the_immediate_context_is_pinned() {
        let storage = TempStorage::new();
        seed(&storage);
        let snapshot_tx = storage.start_snapshot_read_transaction();
        let plain_tx = storage.start_transaction();
        commit_concurrent_change(&storage);

        let pinned_ctx = storage
            .get_immediate_storage_context(PATH.into(), &snapshot_tx)
            .unwrap();
        assert_reads_pinned(&pinned_ctx);

        let latest_ctx = storage
            .get_immediate_storage_context(PATH.into(), &plain_tx)
            .unwrap();
        assert_reads_latest(&latest_ctx);
    }

    /// Item 1: a snapshot read transaction is read-only by
    /// construction, not by documentation — every write entry point and
    /// commit refuse it with the typed error, and rollback (a harmless
    /// no-op) still succeeds.
    #[test]
    fn snapshot_read_transaction_refuses_writes_and_commit() {
        let storage = TempStorage::new();
        seed(&storage);
        let snapshot_tx = storage.start_snapshot_read_transaction();
        assert!(snapshot_tx.is_snapshot_read());

        // Immediate context: every write refuses immediately.
        let ctx = storage
            .get_immediate_storage_context(PATH.into(), &snapshot_tx)
            .unwrap();
        let refused = |result: Result<(), Error>| {
            assert!(
                matches!(result, Err(Error::SnapshotReadOnlyTransaction(_))),
                "expected the typed snapshot-read-only refusal, got {result:?}"
            );
        };
        refused(ctx.put(b"updated", b"smuggled", None, None).unwrap());
        refused(ctx.put_aux(b"updated", b"smuggled", None).unwrap());
        refused(ctx.put_root(b"updated", b"smuggled", None).unwrap());
        refused(ctx.put_meta(b"updated", b"smuggled", None).unwrap());
        refused(ctx.delete(b"updated", None).unwrap());
        refused(ctx.delete_aux(b"updated", None).unwrap());
        refused(ctx.delete_root(b"updated", None).unwrap());
        refused(ctx.delete_meta(b"updated", None).unwrap());
        refused(ctx.commit_batch(ctx.new_batch()).unwrap());

        // Batch-apply entry point: puts into a deferred StorageBatch
        // are accepted (they touch nothing), the apply refuses.
        let batch = StorageBatch::new();
        let batched_ctx = storage
            .get_transactional_storage_context(PATH.into(), Some(&batch), &snapshot_tx)
            .unwrap();
        batched_ctx
            .put(b"updated", b"smuggled", None, None)
            .unwrap()
            .expect("a deferred batch put touches nothing yet");
        refused(
            storage
                .commit_multi_context_batch(batch, Some(&snapshot_tx))
                .unwrap(),
        );

        // Rollback is allowed; commit refuses and consumes.
        storage
            .rollback_transaction(&snapshot_tx)
            .expect("rollback is a harmless no-op on a snapshot read transaction");
        refused(storage.commit_transaction(snapshot_tx).unwrap());

        // Nothing leaked through: latest-committed state is untouched.
        let control_tx = storage.start_transaction();
        let control = storage
            .get_transactional_storage_context(PATH.into(), None, &control_tx)
            .unwrap();
        assert_eq!(get(&control, b"updated"), Some(b"before".to_vec()));
    }

    /// Item 2: snapshot lifetime is observable. A plain transaction has
    /// no snapshot age; a snapshot read transaction reports a
    /// monotonically growing hold time.
    #[test]
    fn snapshot_age_is_exposed_and_grows() {
        let storage = TempStorage::new();

        let plain_tx = storage.start_transaction();
        assert!(!plain_tx.is_snapshot_read());
        assert_eq!(plain_tx.snapshot_age(), None);

        let snapshot_tx = storage.start_snapshot_read_transaction();
        assert!(snapshot_tx.is_snapshot_read());
        let first = snapshot_tx.snapshot_age().expect("snapshot age");
        let second = snapshot_tx.snapshot_age().expect("snapshot age");
        assert!(second >= first, "age must not run backwards");
    }

    /// The savepoint family is part of the wrapper's public surface —
    /// platform sets a savepoint per state transition and rewinds one
    /// failed group without discarding the transaction. Pin the
    /// pass-through: writes since `set_savepoint` are undone by
    /// `rollback_to_savepoint`, writes before it survive, and the
    /// rollback family stays callable on a snapshot read transaction.
    #[test]
    fn savepoints_unwind_writes_since_the_savepoint() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let ctx = storage
            .get_immediate_storage_context(PATH.into(), &tx)
            .unwrap();

        ctx.put(b"kept", b"kept_value", None, None)
            .unwrap()
            .expect("pre-savepoint put");
        tx.set_savepoint();
        ctx.put(b"unwound", b"gone", None, None)
            .unwrap()
            .expect("post-savepoint put");
        assert_eq!(get(&ctx, b"unwound"), Some(b"gone".to_vec()));

        tx.rollback_to_savepoint().expect("rollback to savepoint");
        assert_eq!(get(&ctx, b"kept"), Some(b"kept_value".to_vec()));
        assert_eq!(get(&ctx, b"unwound"), None);

        storage
            .commit_transaction(tx)
            .unwrap()
            .expect("commit after savepoint rewind");

        // The rollback family is allowed on a snapshot read
        // transaction — it can only unwind writes, which such a
        // transaction cannot accumulate.
        let snapshot_tx = storage.start_snapshot_read_transaction();
        snapshot_tx.set_savepoint();
        snapshot_tx
            .rollback_to_savepoint()
            .expect("savepoint family is a harmless no-op on a snapshot read transaction");
    }

    /// Holding the snapshot past the debug warning threshold and then
    /// reading exercises the loud-log path (fires once per
    /// transaction); reads keep returning pinned data regardless.
    #[test]
    fn long_held_snapshot_still_reads_pinned_data() {
        let storage = TempStorage::new();
        seed(&storage);
        let snapshot_tx = storage.start_snapshot_read_transaction();
        commit_concurrent_change(&storage);

        // Cross the debug-build warning threshold (1s) before reading.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(
            snapshot_tx.snapshot_age().expect("snapshot age") > std::time::Duration::from_secs(1)
        );

        let ctx = storage
            .get_transactional_storage_context(PATH.into(), None, &snapshot_tx)
            .unwrap();
        // Two reads: the first trips the once-per-transaction warning,
        // the second takes the already-warned path.
        assert_eq!(get(&ctx, b"updated"), Some(b"before".to_vec()));
        assert_eq!(get(&ctx, b"removed"), Some(b"doomed".to_vec()));
    }
}

mod flat_store {
    use grovedb_costs::{storage_cost::key_value_cost::KeyValueStorageCost, OperationCost};

    use super::*;
    use crate::{
        flat::{
            pending_prefix_drops_namespace, stage_pending_prefix_drop, PendingPrefixDropRecord,
            PendingPrefixDrops, SubtreePrefix,
        },
        rocksdb_storage::{PrefixedRocksDbTransactionContext, RocksDbStorage},
        Error, FlatStore, RawIterator, Storage, StorageBatch, StorageContext,
    };

    const SUBTREE: &[&[u8]] = &[b"flat"];
    const OTHER_SUBTREE: &[&[u8]] = &[b"other"];

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Keyspace {
        Data,
        Aux,
        Meta,
    }

    const KEYSPACES: [Keyspace; 3] = [Keyspace::Data, Keyspace::Aux, Keyspace::Meta];

    /// Run `$body` with `$store` bound to the `FlatStore` handle of one
    /// keyspace of `$context`.
    macro_rules! with_keyspace {
        ($keyspace:expr, $context:expr, |$store:ident| $body:expr) => {
            match $keyspace {
                Keyspace::Data => {
                    let $store = &$context;
                    $body
                }
                Keyspace::Aux => {
                    let $store = &$context.aux();
                    $body
                }
                Keyspace::Meta => {
                    let $store = &$context.meta();
                    $body
                }
            }
        };
    }

    /// The same write as `FlatStore::put`, through the context method the
    /// keyspace maps to.
    fn put_through_context<'db, C: StorageContext<'db>>(
        context: &C,
        keyspace: Keyspace,
        key: &[u8],
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> OperationCost {
        let result = match keyspace {
            Keyspace::Data => context.put(key, value, None, cost_info),
            Keyspace::Aux => context.put_aux(key, value, cost_info),
            Keyspace::Meta => context.put_meta(key, value, cost_info),
        };
        result.value.expect("put through the context");
        result.cost
    }

    fn delete_through_context<'db, C: StorageContext<'db>>(
        context: &C,
        keyspace: Keyspace,
        key: &[u8],
    ) -> OperationCost {
        let result = match keyspace {
            Keyspace::Data => context.delete(key, None),
            Keyspace::Aux => context.delete_aux(key, None),
            Keyspace::Meta => context.delete_meta(key, None),
        };
        result.value.expect("delete through the context");
        result.cost
    }

    fn get_through_context<'db, C: StorageContext<'db>>(
        context: &C,
        keyspace: Keyspace,
        key: &[u8],
    ) -> (Option<Vec<u8>>, OperationCost) {
        let result = match keyspace {
            Keyspace::Data => context.get(key),
            Keyspace::Aux => context.get_aux(key),
            Keyspace::Meta => context.get_meta(key),
        };
        (result.value.expect("get through the context"), result.cost)
    }

    fn read<S: FlatStore>(store: &S, key: &[u8]) -> (Option<Vec<u8>>, OperationCost) {
        let result = store.get(key);
        (result.value.expect("get"), result.cost)
    }

    fn forward<S: FlatStore>(store: &S) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut cursor = store.cursor();
        let mut records = Vec::new();
        cursor.seek_to_first().unwrap();
        while cursor.valid().unwrap() {
            records.push((
                cursor.key().unwrap().expect("key").to_vec(),
                cursor.value().unwrap().expect("value").to_vec(),
            ));
            cursor.next().unwrap();
        }
        cursor.status().expect("cursor status");
        records
    }

    fn backward<S: FlatStore>(store: &S) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut cursor = store.cursor();
        let mut records = Vec::new();
        cursor.seek_to_last().unwrap();
        while cursor.valid().unwrap() {
            records.push((
                cursor.key().unwrap().expect("key").to_vec(),
                cursor.value().unwrap().expect("value").to_vec(),
            ));
            cursor.prev().unwrap();
        }
        records
    }

    fn pair(key: &[u8], value: &[u8]) -> (Vec<u8>, Vec<u8>) {
        (key.to_vec(), value.to_vec())
    }

    fn commit(
        storage: &TempStorage,
        batch: StorageBatch,
        tx: &<RocksDbStorage as Storage<'_>>::Transaction,
    ) -> OperationCost {
        let result = storage.commit_multi_context_batch(batch, Some(tx));
        result.value.expect("batch applies");
        result.cost
    }

    #[test]
    fn a_deferred_handle_stages_writes_and_costs_what_the_context_costs() {
        for keyspace in KEYSPACES {
            let storage = TempStorage::new();
            let tx = storage.start_transaction();
            let twin_storage = TempStorage::new();
            let twin_tx = twin_storage.start_transaction();

            let writes: [(&[u8], &[u8], Option<KeyValueStorageCost>); 3] = [
                (b"k2", b"value-2", None),
                (b"k1", b"v1", Some(KeyValueStorageCost::prepaid())),
                (b"k3", b"value-three", None),
            ];

            let batch = StorageBatch::new();
            let context = storage
                .get_transactional_storage_context(SUBTREE.into(), Some(&batch), &tx)
                .unwrap();
            let twin_batch = StorageBatch::new();
            let twin_context = twin_storage
                .get_transactional_storage_context(SUBTREE.into(), Some(&twin_batch), &twin_tx)
                .unwrap();

            for (key, value, cost_info) in writes {
                let staged = with_keyspace!(keyspace, context, |store| FlatStore::put(
                    store,
                    key,
                    value,
                    cost_info.clone()
                ));
                staged.value.expect("put is staged");
                assert_eq!(
                    staged.cost,
                    put_through_context(&twin_context, keyspace, key, value, cost_info),
                    "{keyspace:?}"
                );
            }

            // Nothing staged is visible to a read or to the cursor.
            with_keyspace!(keyspace, context, |store| {
                assert_eq!(read(store, b"k1").0, None, "{keyspace:?}");
                assert!(forward(store).is_empty(), "{keyspace:?}");
            });

            assert_eq!(
                commit(&storage, batch, &tx),
                commit(&twin_storage, twin_batch, &twin_tx),
                "{keyspace:?}"
            );

            let applied = storage
                .get_transactional_storage_context(SUBTREE.into(), None, &tx)
                .unwrap();
            let twin_applied = twin_storage
                .get_transactional_storage_context(SUBTREE.into(), None, &twin_tx)
                .unwrap();
            with_keyspace!(keyspace, applied, |store| {
                assert_eq!(
                    read(store, b"k3"),
                    (
                        Some(b"value-three".to_vec()),
                        OperationCost {
                            seek_count: 1,
                            storage_loaded_bytes: 11,
                            ..Default::default()
                        }
                    ),
                    "{keyspace:?}"
                );
                assert_eq!(
                    read(store, b"k3"),
                    get_through_context(&twin_applied, keyspace, b"k3"),
                    "{keyspace:?}"
                );
                assert_eq!(
                    read(store, b"absent"),
                    get_through_context(&twin_applied, keyspace, b"absent"),
                    "{keyspace:?}"
                );
                let expected = vec![
                    pair(b"k1", b"v1"),
                    pair(b"k2", b"value-2"),
                    pair(b"k3", b"value-three"),
                ];
                assert_eq!(forward(store), expected, "{keyspace:?}");
                assert_eq!(
                    backward(store),
                    expected.into_iter().rev().collect::<Vec<_>>(),
                    "{keyspace:?}"
                );

                // A handle opened with no batch refuses to write.
                assert!(matches!(
                    FlatStore::put(store, b"k4", b"v4", None).value,
                    Err(Error::StorageError(_))
                ));
                assert!(matches!(
                    FlatStore::delete(store, b"k1", None).value,
                    Err(Error::StorageError(_))
                ));
                assert!(matches!(
                    FlatStore::clear(store).value,
                    Err(Error::StorageError(_))
                ));
            });

            // A delete is staged and charged like the context's.
            let batch = StorageBatch::new();
            let context = storage
                .get_transactional_storage_context(SUBTREE.into(), Some(&batch), &tx)
                .unwrap();
            let twin_batch = StorageBatch::new();
            let twin_context = twin_storage
                .get_transactional_storage_context(SUBTREE.into(), Some(&twin_batch), &twin_tx)
                .unwrap();
            let staged = with_keyspace!(keyspace, context, |store| FlatStore::delete(
                store, b"k2", None
            ));
            staged.value.expect("delete is staged");
            assert_eq!(
                staged.cost,
                delete_through_context(&twin_context, keyspace, b"k2"),
                "{keyspace:?}"
            );
            with_keyspace!(keyspace, context, |store| assert_eq!(
                read(store, b"k2").0,
                Some(b"value-2".to_vec()),
                "{keyspace:?}"
            ));
            assert_eq!(
                commit(&storage, batch, &tx),
                commit(&twin_storage, twin_batch, &twin_tx),
                "{keyspace:?}"
            );
            with_keyspace!(keyspace, applied, |store| assert_eq!(
                forward(store),
                vec![pair(b"k1", b"v1"), pair(b"k3", b"value-three")],
                "{keyspace:?}"
            ));
        }
    }

    #[test]
    fn the_keyspaces_of_a_subtree_and_of_other_subtrees_are_disjoint() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();

        let batch = StorageBatch::new();
        let context = storage
            .get_transactional_storage_context(SUBTREE.into(), Some(&batch), &tx)
            .unwrap();
        let other = storage
            .get_transactional_storage_context(OTHER_SUBTREE.into(), Some(&batch), &tx)
            .unwrap();
        FlatStore::put(&context, b"key", b"data", None)
            .unwrap()
            .unwrap();
        FlatStore::put(&context.aux(), b"key", b"aux", None)
            .unwrap()
            .unwrap();
        FlatStore::put(&context.meta(), b"key", b"meta", None)
            .unwrap()
            .unwrap();
        context
            .put_root(b"key", b"root", None)
            .unwrap()
            .expect("root pointer");
        FlatStore::put(&other, b"key", b"other-data", None)
            .unwrap()
            .unwrap();
        FlatStore::put(&other.aux(), b"key", b"other-aux", None)
            .unwrap()
            .unwrap();
        FlatStore::put(&other.meta(), b"key", b"other-meta", None)
            .unwrap()
            .unwrap();
        commit(&storage, batch, &tx);

        let assert_state = |cleared: &[Keyspace]| {
            let context = storage
                .get_transactional_storage_context(SUBTREE.into(), None, &tx)
                .unwrap();
            let other = storage
                .get_transactional_storage_context(OTHER_SUBTREE.into(), None, &tx)
                .unwrap();
            for (keyspace, value, other_value) in [
                (Keyspace::Data, b"data".as_slice(), b"other-data".as_slice()),
                (Keyspace::Aux, b"aux", b"other-aux"),
                (Keyspace::Meta, b"meta", b"other-meta"),
            ] {
                let expected = if cleared.contains(&keyspace) {
                    vec![]
                } else {
                    vec![pair(b"key", value)]
                };
                with_keyspace!(keyspace, context, |store| assert_eq!(
                    forward(store),
                    expected,
                    "{keyspace:?}"
                ));
                with_keyspace!(keyspace, other, |store| assert_eq!(
                    forward(store),
                    vec![pair(b"key", other_value)],
                    "{keyspace:?}"
                ));
            }
            assert_eq!(
                context.get_root(b"key").unwrap().expect("root pointer"),
                Some(b"root".to_vec())
            );
        };
        assert_state(&[]);

        let mut cleared = Vec::new();
        for keyspace in [Keyspace::Aux, Keyspace::Data, Keyspace::Meta] {
            let batch = StorageBatch::new();
            let context = storage
                .get_transactional_storage_context(SUBTREE.into(), Some(&batch), &tx)
                .unwrap();
            with_keyspace!(keyspace, context, |store| FlatStore::clear(store)
                .unwrap()
                .expect("clear"));
            commit(&storage, batch, &tx);
            cleared.push(keyspace);
            assert_state(&cleared);
        }
    }

    #[test]
    fn clear_costs_the_walk_and_one_uncosted_delete_per_record() {
        let storage = TempStorage::new();
        let twin_storage = TempStorage::new();
        let inherent_storage = TempStorage::new();

        for storage in [&storage, &twin_storage, &inherent_storage] {
            let tx = storage.start_transaction();
            let batch = StorageBatch::new();
            let context = storage
                .get_transactional_storage_context(SUBTREE.into(), Some(&batch), &tx)
                .unwrap();
            for (key, value) in [
                (b"a".as_slice(), b"1".as_slice()),
                (b"bb", b"22"),
                (b"ccc", b"333"),
            ] {
                FlatStore::put(&context, key, value, None).unwrap().unwrap();
            }
            commit(storage, batch, &tx);
            storage
                .commit_transaction(tx)
                .unwrap()
                .expect("transaction commits");
        }
        let tx = storage.start_transaction();
        let twin_tx = twin_storage.start_transaction();
        let inherent_tx = inherent_storage.start_transaction();

        let batch = StorageBatch::new();
        let context = storage
            .get_transactional_storage_context(SUBTREE.into(), Some(&batch), &tx)
            .unwrap();
        let cleared = FlatStore::clear(&context);
        cleared.value.expect("clear");

        // The walk the contract names, written out against the context.
        let twin_batch = StorageBatch::new();
        let twin_context = twin_storage
            .get_transactional_storage_context(SUBTREE.into(), Some(&twin_batch), &twin_tx)
            .unwrap();
        let mut walk_cost = OperationCost::default();
        {
            let mut iterator = twin_context.raw_iter();
            iterator.seek_to_first().unwrap_add_cost(&mut walk_cost);
            while iterator.valid().unwrap_add_cost(&mut walk_cost) {
                let key = iterator
                    .key()
                    .unwrap_add_cost(&mut walk_cost)
                    .expect("key")
                    .to_vec();
                StorageContext::delete(&twin_context, key, None)
                    .unwrap_add_cost(&mut walk_cost)
                    .expect("delete");
                iterator.next().unwrap_add_cost(&mut walk_cost);
            }
        }

        let inherent_batch = StorageBatch::new();
        let mut inherent_context = inherent_storage
            .get_transactional_storage_context(SUBTREE.into(), Some(&inherent_batch), &inherent_tx)
            .unwrap();
        let inherent_cleared = PrefixedRocksDbTransactionContext::clear(&mut inherent_context);
        inherent_cleared.value.expect("clear");

        assert_eq!(cleared.cost, walk_cost);
        assert_eq!(cleared.cost, inherent_cleared.cost);
        assert_eq!(
            cleared.cost,
            OperationCost {
                // One seek to the first record and one step per record.
                seek_count: 4,
                // Each record's prefixed key twice (`valid` and `key`), then
                // one check past the last record.
                storage_loaded_bytes: 2 * (33 + 34 + 35) + 288,
                ..Default::default()
            }
        );

        let commit_cost = commit(&storage, batch, &tx);
        assert_eq!(commit_cost, commit(&twin_storage, twin_batch, &twin_tx));
        assert_eq!(
            commit_cost,
            commit(&inherent_storage, inherent_batch, &inherent_tx)
        );
        // A delete with no cost info reads the committed record it removes
        // and reports its key and value as removed.
        assert_eq!(commit_cost.seek_count, 6);
        assert_eq!(commit_cost.storage_loaded_bytes, 1 + 2 + 3);
        assert_eq!(
            commit_cost.storage_cost.removed_bytes.total_removed_bytes(),
            (33 + 1 + 1 + 1) + (34 + 1 + 2 + 1) + (35 + 1 + 3 + 1)
        );

        let applied = storage
            .get_transactional_storage_context(SUBTREE.into(), None, &tx)
            .unwrap();
        assert!(forward(&applied).is_empty());
    }

    #[test]
    fn an_immediate_handle_applies_writes_at_once_and_charges_nothing() {
        for keyspace in KEYSPACES {
            let storage = TempStorage::new();
            let tx = storage.start_transaction();
            let context = storage
                .get_immediate_storage_context(SUBTREE.into(), &tx)
                .unwrap();
            let other = storage
                .get_immediate_storage_context(OTHER_SUBTREE.into(), &tx)
                .unwrap();

            with_keyspace!(keyspace, other, |store| FlatStore::put(
                store, b"k1", b"other", None
            )
            .unwrap()
            .expect("put"));

            with_keyspace!(keyspace, context, |store| {
                for (key, value) in [(b"k2", b"v2"), (b"k1", b"v1"), (b"k3", b"v3")] {
                    let written = FlatStore::put(store, key, value, None);
                    written.value.expect("put");
                    assert_eq!(written.cost, OperationCost::default(), "{keyspace:?}");
                }
                assert_eq!(
                    read(store, b"k1"),
                    (Some(b"v1".to_vec()), OperationCost::default()),
                    "{keyspace:?}"
                );
                assert_eq!(
                    forward(store),
                    vec![pair(b"k1", b"v1"), pair(b"k2", b"v2"), pair(b"k3", b"v3")],
                    "{keyspace:?}"
                );
                assert_eq!(
                    backward(store),
                    vec![pair(b"k3", b"v3"), pair(b"k2", b"v2"), pair(b"k1", b"v1")],
                    "{keyspace:?}"
                );

                let deleted = FlatStore::delete(store, b"k2", None);
                deleted.value.expect("delete");
                assert_eq!(deleted.cost, OperationCost::default(), "{keyspace:?}");
                assert_eq!(read(store, b"k2").0, None, "{keyspace:?}");
                FlatStore::delete(store, b"k2", None)
                    .unwrap()
                    .expect("deleting an absent key");

                FlatStore::clear(store).unwrap().expect("clear");
                assert!(forward(store).is_empty(), "{keyspace:?}");
            });

            // The other keyspaces of the subtree were never written, and
            // the other subtree kept its record.
            for untouched in KEYSPACES.into_iter().filter(|k| *k != keyspace) {
                with_keyspace!(untouched, context, |store| assert!(
                    forward(store).is_empty(),
                    "{keyspace:?} leaked into {untouched:?}"
                ));
            }
            with_keyspace!(keyspace, other, |store| assert_eq!(
                forward(store),
                vec![pair(b"k1", b"other")],
                "{keyspace:?}"
            ));
        }
    }

    fn drain<D: PendingPrefixDrops>(database: &D) -> Result<usize, Error> {
        let records = database.pending_prefix_drop_records()?;
        for record in &records {
            database.delete_prefix_ranges(&record.doomed_prefixes)?;
            database.remove_pending_prefix_drop_record(&record.primary_prefix)?;
        }
        Ok(records.len())
    }

    fn listed<D: PendingPrefixDrops>(database: &D) -> Vec<PendingPrefixDropRecord> {
        database.pending_prefix_drop_records().expect("list")
    }

    #[test]
    fn a_staged_drop_is_listed_only_once_committed_and_drains_idempotently() {
        let storage = TempStorage::new();
        let doomed_a: SubtreePrefix = [0xa1; 32];
        let doomed_a_secondary: SubtreePrefix = [0xa2; 32];
        let doomed_b: SubtreePrefix = [0x0b; 32];
        let survivor: SubtreePrefix = [0x55; 32];

        let record_a = PendingPrefixDropRecord {
            primary_prefix: doomed_a,
            path: vec![b"a".to_vec()],
            doomed_prefixes: vec![doomed_a, doomed_a_secondary],
        };
        let record_b = PendingPrefixDropRecord {
            primary_prefix: doomed_b,
            path: vec![b"b".to_vec(), vec![]],
            doomed_prefixes: vec![doomed_b],
        };

        let tx = storage.start_transaction();
        let batch = StorageBatch::new();
        for prefix in [doomed_a, doomed_a_secondary, doomed_b, survivor] {
            let context = storage
                .get_transactional_storage_context_by_subtree_prefix(prefix, Some(&batch), &tx)
                .unwrap();
            FlatStore::put(&context, b"key", b"data", None)
                .unwrap()
                .unwrap();
            FlatStore::put(&context.aux(), b"key", b"aux", None)
                .unwrap()
                .unwrap();
            FlatStore::put(&context.meta(), b"key", b"meta", None)
                .unwrap()
                .unwrap();
            context.put_root(b"key", b"root", None).unwrap().unwrap();
        }
        let namespace = storage
            .get_transactional_storage_context_by_subtree_prefix(
                *pending_prefix_drops_namespace(),
                Some(&batch),
                &tx,
            )
            .unwrap();
        // Staged with the larger primary prefix first.
        for record in [&record_a, &record_b] {
            let staged = stage_pending_prefix_drop(&namespace.meta(), record);
            staged.value.expect("staged");
            assert_eq!(staged.cost, OperationCost::default());
        }
        assert!(listed(&*storage).is_empty(), "staged, batch not applied");

        commit(&storage, batch, &tx);
        // The transaction sees its records through the meta keyspace.
        let namespace = storage
            .get_transactional_storage_context_by_subtree_prefix(
                *pending_prefix_drops_namespace(),
                None,
                &tx,
            )
            .unwrap();
        assert_eq!(
            forward(&namespace.meta()),
            vec![
                (doomed_b.to_vec(), record_b.encode().expect("encode")),
                (doomed_a.to_vec(), record_a.encode().expect("encode")),
            ]
        );
        assert!(listed(&*storage).is_empty(), "transaction not committed");
        assert_eq!(drain(&*storage).expect("drain"), 0);

        storage
            .commit_transaction(tx)
            .unwrap()
            .expect("transaction commits");
        assert_eq!(listed(&*storage), vec![record_b.clone(), record_a.clone()]);

        // A drain interrupted after the ranges of the first record were
        // deleted leaves both records listed.
        PendingPrefixDrops::delete_prefix_ranges(&*storage, &record_b.doomed_prefixes)
            .expect("delete ranges");
        assert_eq!(listed(&*storage), vec![record_b, record_a]);

        assert_eq!(drain(&*storage).expect("drain"), 2);
        assert!(listed(&*storage).is_empty());
        assert_eq!(drain(&*storage).expect("second drain"), 0);
        PendingPrefixDrops::remove_pending_prefix_drop_record(&*storage, &doomed_a)
            .expect("removing an absent record");

        let tx = storage.start_transaction();
        for prefix in [doomed_a, doomed_a_secondary, doomed_b] {
            let context = storage
                .get_transactional_storage_context_by_subtree_prefix(prefix, None, &tx)
                .unwrap();
            assert!(forward(&context).is_empty());
            assert!(forward(&context.aux()).is_empty());
            assert!(forward(&context.meta()).is_empty());
            assert_eq!(context.get_root(b"key").unwrap().expect("get"), None);
        }
        let context = storage
            .get_transactional_storage_context_by_subtree_prefix(survivor, None, &tx)
            .unwrap();
        assert_eq!(forward(&context), vec![pair(b"key", b"data")]);
        assert_eq!(forward(&context.aux()), vec![pair(b"key", b"aux")]);
        assert_eq!(forward(&context.meta()), vec![pair(b"key", b"meta")]);
        assert_eq!(
            context.get_root(b"key").unwrap().expect("get"),
            Some(b"root".to_vec())
        );

        assert!(matches!(
            PendingPrefixDrops::delete_prefix_ranges(&*storage, &[[0xff; 32]]),
            Err(Error::StorageError(_))
        ));
    }

    #[test]
    fn a_rolled_back_drop_is_never_listed() {
        let storage = TempStorage::new();
        let record = PendingPrefixDropRecord {
            primary_prefix: [9; 32],
            path: vec![b"gone".to_vec()],
            doomed_prefixes: vec![[9; 32]],
        };

        let tx = storage.start_transaction();
        let batch = StorageBatch::new();
        let namespace = storage
            .get_transactional_storage_context_by_subtree_prefix(
                *pending_prefix_drops_namespace(),
                Some(&batch),
                &tx,
            )
            .unwrap();
        stage_pending_prefix_drop(&namespace.meta(), &record)
            .unwrap()
            .expect("staged");
        commit(&storage, batch, &tx);
        storage.rollback_transaction(&tx).expect("rollback");
        drop(tx);

        assert!(listed(&*storage).is_empty());
    }
}

mod charging {
    use grovedb_costs::{
        cost_model::StorageCostModel,
        error::Error as CostError,
        storage_cost::{
            key_value_cost::KeyValueStorageCost,
            removal::{
                with_basic_sectioned_removal_addition_version, StorageRemovalPerEpochByIdentifier,
                StorageRemovedBytes,
                StorageRemovedBytes::{BasicStorageRemoval, SectionedStorageRemoval},
                UNKNOWN_EPOCH,
            },
            StorageCost,
        },
        CostResult, OperationCost,
    };

    use super::*;
    use crate::{
        rocksdb_storage::RocksDbStorage, worst_case_costs::WorstKeyLength, Error, FlatStore,
        Storage, StorageBatch, StorageContext,
    };

    const MODEL: StorageCostModel = StorageCostModel::V0;
    const SUBTREE: &[&[u8]] = &[b"charged"];
    const OTHER_SUBTREE: &[&[u8]] = &[b"charged too"];

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Keyspace {
        Meta,
        Aux,
        Roots,
        Data,
    }

    /// The keyspaces in the order a batch charges them.
    const IN_BATCH_ORDER: [Keyspace; 4] = [
        Keyspace::Meta,
        Keyspace::Aux,
        Keyspace::Roots,
        Keyspace::Data,
    ];

    fn sections(sections: &[(u16, u32)]) -> StorageRemovedBytes {
        let mut by_owner = StorageRemovalPerEpochByIdentifier::default();
        let default_owner = by_owner.entry([0; 32]).or_default();
        for (epoch, bytes) in sections {
            default_owner.insert(*epoch, *bytes);
        }
        SectionedStorageRemoval(by_owner)
    }

    /// A removal by owner and epoch, as a client's callback splits it.
    fn sectioned() -> StorageRemovedBytes {
        sections(&[(3, 50)])
    }

    /// What a sectioned removal and a plain one of `plain` bytes add up to
    /// when the sectioned one is charged first.
    fn sectioned_then_plain(plain: u32) -> StorageRemovedBytes {
        sections(&[(3, 50), (UNKNOWN_EPOCH, plain)])
    }

    /// What they add up to on grove versions 1 to 3 when the plain one is
    /// charged first: the default owner's section is lost.
    fn plain_then_sectioned() -> StorageRemovedBytes {
        SectionedStorageRemoval(Default::default())
    }

    fn removing(removed_bytes: StorageRemovedBytes) -> KeyValueStorageCost {
        KeyValueStorageCost {
            key_storage_cost: StorageCost {
                removed_bytes,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn put<'db, C: StorageContext<'db>>(
        context: &C,
        keyspace: Keyspace,
        key: &[u8],
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) {
        match keyspace {
            Keyspace::Meta => context.put_meta(key, value, cost_info),
            Keyspace::Aux => context.put_aux(key, value, cost_info),
            Keyspace::Roots => context.put_root(key, value, cost_info),
            Keyspace::Data => context.put(key, value, None, cost_info),
        }
        .unwrap()
        .expect("a staged put");
    }

    fn delete<'db, C: StorageContext<'db>>(
        context: &C,
        keyspace: Keyspace,
        key: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) {
        match keyspace {
            Keyspace::Meta => context.delete_meta(key, cost_info),
            Keyspace::Aux => context.delete_aux(key, cost_info),
            Keyspace::Roots => context.delete_root(key, cost_info),
            Keyspace::Data => context.delete(key, cost_info),
        }
        .unwrap()
        .expect("a staged delete");
    }

    fn get<'db, C: StorageContext<'db>>(
        context: &C,
        keyspace: Keyspace,
        key: &[u8],
    ) -> Option<Vec<u8>> {
        match keyspace {
            Keyspace::Meta => context.get_meta(key),
            Keyspace::Aux => context.get_aux(key),
            Keyspace::Roots => context.get_root(key),
            Keyspace::Data => context.get(key),
        }
        .unwrap()
        .expect("a read")
    }

    /// One removal of a batch: where it is staged, and whether it is the
    /// sectioned or the plain one.
    type Removal<'a> = (&'a [&'a [u8]], Keyspace, &'a [u8], bool);

    /// Applies a batch that removes two records, staged in the order given,
    /// and returns the removed bytes it is charged.
    fn removed_by(staged: [Removal; 2]) -> StorageRemovedBytes {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let batch = StorageBatch::new();
        for (subtree, keyspace, key, is_sectioned) in staged {
            let context = storage
                .get_transactional_storage_context(subtree.into(), Some(&batch), &tx)
                .unwrap();
            let removed = if is_sectioned {
                sectioned()
            } else {
                BasicStorageRemoval(7)
            };
            delete(&context, keyspace, key, Some(removing(removed)));
        }
        let applied = storage.commit_multi_context_batch(batch, Some(&tx));
        applied.value.expect("the batch applies");
        assert_eq!(applied.cost.seek_count, 2);
        applied.cost.storage_cost.removed_bytes
    }

    /// Asserts that a batch charges `first` before `second`, however the
    /// two were staged.
    fn assert_charged_in_order(
        first: (&[&[u8]], Keyspace, &[u8]),
        second: (&[&[u8]], Keyspace, &[u8]),
    ) {
        let plain_first = (first.0, first.1, first.2, false);
        let sectioned_second = (second.0, second.1, second.2, true);
        let sectioned_first = (first.0, first.1, first.2, true);
        let plain_second = (second.0, second.1, second.2, false);
        let case = format!("{first:?} before {second:?}");

        assert_eq!(
            removed_by([plain_first, sectioned_second]),
            plain_then_sectioned(),
            "{case}"
        );
        assert_eq!(
            removed_by([sectioned_second, plain_first]),
            plain_then_sectioned(),
            "{case}"
        );
        assert_eq!(
            removed_by([sectioned_first, plain_second]),
            sectioned_then_plain(7),
            "{case}"
        );
        assert_eq!(
            removed_by([plain_second, sectioned_first]),
            sectioned_then_plain(7),
            "{case}"
        );
    }

    #[test]
    fn a_batch_is_charged_by_keyspace_and_then_by_stored_key() {
        assert_eq!(
            MODEL.batch_order().map(|keyspace| format!("{keyspace:?}")),
            IN_BATCH_ORDER.map(|keyspace| format!("{keyspace:?}"))
        );
        for (position, earlier) in IN_BATCH_ORDER.into_iter().enumerate() {
            for later in &IN_BATCH_ORDER[position + 1..] {
                assert_charged_in_order((SUBTREE, earlier, b"z"), (SUBTREE, *later, b"a"));
            }
            assert_charged_in_order((SUBTREE, earlier, b"a"), (SUBTREE, earlier, b"b"));
            assert_charged_in_order((SUBTREE, earlier, b"a"), (SUBTREE, earlier, b"aa"));
        }

        let prefix = |subtree: &[&[u8]]| RocksDbStorage::build_prefix(subtree.into()).unwrap();
        let (low, high) = if prefix(SUBTREE) < prefix(OTHER_SUBTREE) {
            (SUBTREE, OTHER_SUBTREE)
        } else {
            (OTHER_SUBTREE, SUBTREE)
        };
        for keyspace in IN_BATCH_ORDER {
            assert_charged_in_order((low, keyspace, b"z"), (high, keyspace, b"a"));
        }
    }

    #[test]
    fn the_order_of_a_batch_does_not_show_from_grove_version_4() {
        with_basic_sectioned_removal_addition_version(1, || {
            for is_sectioned_first in [true, false] {
                assert_eq!(
                    removed_by([
                        (SUBTREE, Keyspace::Data, b"a", is_sectioned_first),
                        (SUBTREE, Keyspace::Data, b"b", !is_sectioned_first),
                    ]),
                    sectioned_then_plain(7)
                );
            }
        });
    }

    #[test]
    fn a_delete_with_no_cost_meets_the_other_removals_in_batch_order() {
        let removed_by = |sectioned_key: &[u8], uncosted_key: &[u8]| {
            let storage = TempStorage::new();
            let tx = storage.start_transaction();
            let batch = StorageBatch::new();
            let context = storage
                .get_transactional_storage_context(SUBTREE.into(), Some(&batch), &tx)
                .unwrap();
            delete(
                &context,
                Keyspace::Data,
                sectioned_key,
                Some(removing(sectioned())),
            );
            delete(&context, Keyspace::Data, uncosted_key, None);
            let applied = storage.commit_multi_context_batch(batch, Some(&tx));
            applied.value.expect("the batch applies");
            applied.cost.storage_cost.removed_bytes
        };
        assert_eq!(removed_by(b"b", b"a"), plain_then_sectioned());
        assert_eq!(removed_by(b"a", b"b"), sectioned_then_plain(32 + 1 + 1 + 1));
    }

    #[test]
    fn the_key_of_a_put_is_charged_before_its_value() {
        for keyspace in IN_BATCH_ORDER {
            let removed_by = |key: StorageRemovedBytes, value: StorageRemovedBytes| {
                let storage = TempStorage::new();
                let tx = storage.start_transaction();
                let batch = StorageBatch::new();
                let context = storage
                    .get_transactional_storage_context(SUBTREE.into(), Some(&batch), &tx)
                    .unwrap();
                let cost_info = KeyValueStorageCost {
                    key_storage_cost: StorageCost {
                        removed_bytes: key,
                        ..Default::default()
                    },
                    value_storage_cost: StorageCost {
                        removed_bytes: value,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                put(&context, keyspace, b"key", b"value", Some(cost_info));
                let applied = storage.commit_multi_context_batch(batch, Some(&tx));
                applied.value.expect("the batch applies");
                applied.cost.storage_cost.removed_bytes
            };
            assert_eq!(
                removed_by(BasicStorageRemoval(5), sectioned()),
                plain_then_sectioned(),
                "{keyspace:?}"
            );
            assert_eq!(
                removed_by(sectioned(), BasicStorageRemoval(5)),
                sectioned_then_plain(5),
                "{keyspace:?}"
            );
        }
    }

    fn refused(applied: CostResult<(), Error>) -> OperationCost {
        match applied.value {
            Err(Error::CostError(CostError::StorageCostMismatch { .. })) => applied.cost,
            other => panic!("expected a refused put, got {other:?}"),
        }
    }

    #[test]
    fn a_refused_put_keeps_what_was_charged_before_it_and_none_of_the_storage_costs() {
        let wrong = || KeyValueStorageCost {
            value_storage_cost: StorageCost {
                added_bytes: 1,
                ..Default::default()
            },
            needs_value_verification: true,
            ..Default::default()
        };
        for (position, refusing) in IN_BATCH_ORDER.into_iter().enumerate() {
            let storage = TempStorage::new();
            let tx = storage.start_transaction();
            let batch = StorageBatch::new();
            let context = storage
                .get_transactional_storage_context(SUBTREE.into(), Some(&batch), &tx)
                .unwrap();
            for keyspace in IN_BATCH_ORDER {
                put(&context, keyspace, b"committed", b"a record", None);
            }
            storage
                .commit_multi_context_batch(batch, Some(&tx))
                .unwrap()
                .expect("the batch applies");
            storage
                .commit_transaction(tx)
                .unwrap()
                .expect("the transaction commits");

            let fill = |batch: &StorageBatch, tx| {
                let context = storage
                    .get_transactional_storage_context(SUBTREE.into(), Some(batch), tx)
                    .unwrap();
                for keyspace in IN_BATCH_ORDER.into_iter().rev() {
                    put(&context, keyspace, b"put", b"value", None);
                    delete(&context, keyspace, b"committed", None);
                }
                put(&context, refusing, b"e", b"refused", Some(wrong()));
            };
            let before = position as u32;
            let expected = OperationCost {
                seek_count: before * (2 + 1) + 2 + 1,
                storage_loaded_bytes: (u64::from(before) + 1) * 8,
                ..Default::default()
            };

            let tx = storage.start_transaction();
            let batch = StorageBatch::new();
            fill(&batch, &tx);
            assert_eq!(
                refused(storage.commit_multi_context_batch(batch, Some(&tx))),
                expected,
                "{refusing:?}"
            );
            let context = storage
                .get_transactional_storage_context(SUBTREE.into(), None, &tx)
                .unwrap();
            for keyspace in IN_BATCH_ORDER {
                assert_eq!(get(&context, keyspace, b"put"), None);
                assert_eq!(
                    get(&context, keyspace, b"committed"),
                    Some(b"a record".to_vec())
                );
            }

            let batch = StorageBatch::new();
            fill(&batch, &tx);
            let built = storage.build_write_batch(batch);
            assert_eq!(built.cost, expected, "{refusing:?}");
            assert!(matches!(
                built.value,
                Err(Error::CostError(CostError::StorageCostMismatch { .. }))
            ));
        }
    }

    #[test]
    fn the_storage_cost_of_a_batch_is_owed_only_when_its_writes_are_taken() {
        let storage = TempStorage::new();
        let fill = |batch: &StorageBatch, tx| {
            let context = storage
                .get_transactional_storage_context(SUBTREE.into(), Some(batch), tx)
                .unwrap();
            put(&context, Keyspace::Aux, b"key", b"value", None);
            delete(&context, Keyspace::Data, b"absent", None);
        };
        let at_once = || OperationCost::with_seek_count(3);
        let storage_cost = OperationCost {
            storage_cost: StorageCost {
                added_bytes: 35 + 1 + 5 + 1,
                replaced_bytes: 0,
                removed_bytes: BasicStorageRemoval(38 + 1 + 1),
            },
            ..Default::default()
        };

        let read_only = storage.start_snapshot_read_transaction();
        let batch = StorageBatch::new();
        fill(&batch, &read_only);
        let refused = storage.commit_multi_context_batch(batch, Some(&read_only));
        assert!(matches!(
            refused.value,
            Err(Error::SnapshotReadOnlyTransaction(_))
        ));
        assert_eq!(refused.cost, at_once());

        let batch = StorageBatch::new();
        fill(&batch, &read_only);
        let built = storage.build_write_batch(batch);
        assert_eq!(built.cost, at_once());
        let (db_batch, pending) = built.value.expect("the batch is built");
        assert_eq!(pending, storage_cost);
        let refused = storage.commit_db_write_batch(db_batch, pending, Some(&read_only));
        assert!(refused.value.is_err());
        assert_eq!(refused.cost, OperationCost::default());

        let tx = storage.start_transaction();
        let batch = StorageBatch::new();
        fill(&batch, &tx);
        let (db_batch, pending) = storage
            .build_write_batch(batch)
            .unwrap()
            .expect("the batch is built");
        let written = storage.commit_db_write_batch(db_batch, pending, Some(&tx));
        written.value.expect("the batch is written");
        assert_eq!(written.cost, storage_cost);

        let batch = StorageBatch::new();
        fill(&batch, &tx);
        let applied = storage.commit_multi_context_batch(batch, Some(&tx));
        applied.value.expect("the batch applies");
        assert_eq!(
            applied.cost,
            OperationCost {
                seek_count: 3,
                storage_cost: storage_cost.storage_cost,
                ..Default::default()
            }
        );
    }

    #[test]
    fn clearing_a_keyspace_is_charged_as_the_walk_the_model_defines() {
        let records: [&[u8]; 4] = [b"", b"a", b"bb", &[7; 200]];
        let stored_key_lens = || records.iter().map(|key| MODEL.stored_key_len(key.len()));

        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let immediate = storage
            .get_immediate_storage_context(SUBTREE.into(), &tx)
            .unwrap();
        for key in records {
            StorageContext::put(&immediate, key, b"value", None, None)
                .unwrap()
                .expect("a put");
            immediate
                .put_aux(key, b"aux", None)
                .unwrap()
                .expect("a put");
        }

        let batch = StorageBatch::new();
        let deferred = storage
            .get_transactional_storage_context(SUBTREE.into(), Some(&batch), &tx)
            .unwrap();
        let cleared = FlatStore::clear(&deferred);
        cleared.value.expect("a keyspace cleared");
        assert_eq!(cleared.cost, MODEL.clear_walk(stored_key_lens()));
        let cleared = FlatStore::clear(&deferred.aux());
        cleared.value.expect("a keyspace cleared");
        assert_eq!(cleared.cost, MODEL.clear_walk(stored_key_lens()));

        let empty = storage
            .get_transactional_storage_context(OTHER_SUBTREE.into(), Some(&batch), &tx)
            .unwrap();
        let cleared = FlatStore::clear(&empty);
        cleared.value.expect("an empty keyspace cleared");
        assert_eq!(cleared.cost, MODEL.clear_walk([]));

        let cleared = FlatStore::clear(&immediate);
        cleared.value.expect("a keyspace cleared");
        assert_eq!(cleared.cost, MODEL.clear_walk(stored_key_lens()));
        assert_eq!(
            StorageContext::get(&immediate, b"a")
                .unwrap()
                .expect("a read"),
            None
        );
    }

    struct Longest(u8);

    impl WorstKeyLength for Longest {
        fn max_length(&self) -> u8 {
            self.0
        }
    }

    #[test]
    fn a_handle_and_its_estimate_are_charged_by_the_lengths_of_the_path() {
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let paths: [&[&[u8]]; 6] = [
            &[],
            &[b""],
            &[b"a"],
            &[&[2; 55]],
            &[&[2; 56]],
            &[&[1; 255], b"", &[3; 255]],
        ];
        for path in paths {
            let expected = MODEL.open_by_path(path.iter().map(|segment| segment.len()));
            assert_eq!(RocksDbStorage::build_prefix(path.into()).cost, expected);
            assert_eq!(
                storage
                    .get_transactional_storage_context(path.into(), None, &tx)
                    .cost,
                expected
            );
            assert_eq!(
                storage.get_immediate_storage_context(path.into(), &tx).cost,
                expected
            );
            let longest: Vec<Longest> = path
                .iter()
                .map(|segment| Longest(segment.len() as u8))
                .collect();
            assert_eq!(RocksDbStorage::get_storage_context_cost(&longest), expected);
        }
    }
}
