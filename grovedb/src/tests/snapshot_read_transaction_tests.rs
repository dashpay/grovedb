//! Snapshot read transactions are read-only by construction (issue #832).
//!
//! `start_snapshot_read_transaction` exists to pin a multi-operation READ
//! to one committed state (pinning itself is covered in
//! `run_path_query_tests`). These tests pin the hardening around the
//! primitive: every write entry point and `commit_transaction` refuse a
//! snapshot read transaction with the typed
//! `SnapshotReadOnlyTransaction` storage error, reads through it keep
//! working after a refusal, and the snapshot's lifetime is observable.

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use crate::{
        tests::{make_test_grovedb, TEST_LEAF},
        Element, Error,
    };

    fn assert_refused(result: Result<(), Error>) {
        assert!(
            matches!(
                result,
                Err(Error::StorageError(
                    grovedb_storage::Error::SnapshotReadOnlyTransaction(_)
                ))
            ),
            "expected the typed snapshot-read-only refusal, got {result:?}"
        );
    }

    #[test]
    fn snapshot_read_transaction_refuses_writes_but_keeps_reading() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key",
            Element::new_item(b"committed".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("seed insert");

        let snapshot_transaction = db.start_snapshot_read_transaction();

        // Inserting through the snapshot transaction is refused with the
        // typed error when the operation's batch is applied...
        assert_refused(
            db.insert(
                [TEST_LEAF].as_ref(),
                b"smuggled",
                Element::new_item(b"nope".to_vec()),
                None,
                Some(&snapshot_transaction),
                grove_version,
            )
            .unwrap(),
        );
        // ...and so is deleting.
        assert_refused(
            db.delete(
                [TEST_LEAF].as_ref(),
                b"key",
                None,
                Some(&snapshot_transaction),
                grove_version,
            )
            .unwrap(),
        );

        // The refusals poison nothing: the same transaction still serves
        // reads, pinned to its creation-time state.
        let element = db
            .get(
                [TEST_LEAF].as_ref(),
                b"key",
                Some(&snapshot_transaction),
                grove_version,
            )
            .unwrap()
            .expect("read through the snapshot transaction after refusals");
        assert_eq!(element, Element::new_item(b"committed".to_vec()));

        // Committing the snapshot transaction is refused too; rollback
        // (the harmless cleanup path) is allowed.
        db.rollback_transaction(&snapshot_transaction)
            .expect("rollback is a harmless no-op on a snapshot read transaction");
        assert_refused(db.commit_transaction(snapshot_transaction).unwrap());

        // Nothing leaked into committed state.
        assert_eq!(
            db.get([TEST_LEAF].as_ref(), b"key", None, grove_version)
                .unwrap()
                .expect("seeded key still present"),
            Element::new_item(b"committed".to_vec())
        );
        assert!(db
            .get([TEST_LEAF].as_ref(), b"smuggled", None, grove_version)
            .unwrap()
            .is_err());
    }

    #[test]
    fn snapshot_lifetime_is_observable() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let plain_transaction = db.start_transaction();
        assert!(!plain_transaction.is_snapshot_read());
        assert_eq!(plain_transaction.snapshot_age(), None);

        let snapshot_transaction = db.start_snapshot_read_transaction();
        assert!(snapshot_transaction.is_snapshot_read());
        let first = snapshot_transaction
            .snapshot_age()
            .expect("snapshot transactions report their hold time");
        let second = snapshot_transaction
            .snapshot_age()
            .expect("snapshot transactions report their hold time");
        assert!(second >= first, "hold time must not run backwards");
    }
}
