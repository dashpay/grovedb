#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use incrementalmerkletree::{Marking, Position, Retention};
    use orchard::tree::Anchor;
    use rusqlite::Connection;

    use crate::{
        test_utils::test_leaf, ClientMemoryCommitmentTree, ClientPersistentCommitmentTree,
        CommitmentTreeError,
    };

    fn checkpoint_retention(id: u32) -> Retention<u32> {
        Retention::Checkpoint {
            id,
            marking: Marking::None,
        }
    }

    fn memory_tree() -> ClientPersistentCommitmentTree {
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        ClientPersistentCommitmentTree::open(conn, 100).expect("open tree")
    }

    #[test]
    fn test_empty_tree() {
        let tree = memory_tree();
        assert_eq!(tree.max_leaf_position().expect("max_leaf_position"), None);
        assert_eq!(tree.anchor().expect("anchor"), Anchor::empty_tree());
    }

    #[test]
    fn test_append_and_position() {
        let mut tree = memory_tree();

        tree.append(test_leaf(0), Retention::Marked)
            .expect("append 0");
        assert_eq!(
            tree.max_leaf_position().expect("pos"),
            Some(Position::from(0))
        );

        tree.append(test_leaf(1), Retention::Ephemeral)
            .expect("append 1");
        assert_eq!(
            tree.max_leaf_position().expect("pos"),
            Some(Position::from(1))
        );
    }

    #[test]
    fn test_anchor_changes() {
        let mut tree = memory_tree();
        let empty_anchor = tree.anchor().expect("anchor");

        tree.append(test_leaf(0), Retention::Marked)
            .expect("append 0");
        let anchor1 = tree.anchor().expect("anchor");
        assert_ne!(empty_anchor, anchor1);

        tree.append(test_leaf(1), Retention::Marked)
            .expect("append 1");
        let anchor2 = tree.anchor().expect("anchor");
        assert_ne!(anchor1, anchor2);
    }

    #[test]
    fn test_witness_generation() {
        let mut tree = memory_tree();

        tree.append(test_leaf(0), Retention::Marked)
            .expect("append 0");
        tree.append(test_leaf(1), Retention::Ephemeral)
            .expect("append 1");
        tree.checkpoint(1).expect("checkpoint");

        let path = tree.witness(Position::from(0), 0).expect("witness");
        assert!(path.is_some(), "should produce witness for marked leaf");
    }

    #[test]
    fn test_persistence_across_reopen() {
        // Use a temp file so we can reopen it
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join("test_commitment.db");

        // Phase 1: create tree, append leaves, get anchor
        let anchor_before;
        let position_before;
        {
            let mut tree =
                ClientPersistentCommitmentTree::open_path(&db_path, 100).expect("open tree");
            for i in 0..20u64 {
                tree.append(test_leaf(i), Retention::Marked)
                    .expect("append");
            }
            tree.checkpoint(1).expect("checkpoint");
            anchor_before = tree.anchor().expect("anchor");
            position_before = tree.max_leaf_position().expect("position");
            // tree is dropped here, connection closed
        }

        // Phase 2: reopen from same file, verify state matches
        {
            let tree =
                ClientPersistentCommitmentTree::open_path(&db_path, 100).expect("reopen tree");
            let anchor_after = tree.anchor().expect("anchor");
            let position_after = tree.max_leaf_position().expect("position");

            assert_eq!(anchor_before, anchor_after, "anchor should survive restart");
            assert_eq!(
                position_before, position_after,
                "position should survive restart"
            );
        }
    }

    #[test]
    fn test_bring_your_own_connection() {
        // Verify the store coexists with other tables
        let conn = Connection::open_in_memory().expect("open sqlite");
        conn.execute(
            "CREATE TABLE my_app_data (id INTEGER PRIMARY KEY, value TEXT)",
            [],
        )
        .expect("create app table");
        conn.execute(
            "INSERT INTO my_app_data (id, value) VALUES (1, 'hello')",
            [],
        )
        .expect("insert app data");

        // Use shared connection so we can verify app data after tree writes
        let arc = Arc::new(Mutex::new(conn));
        let mut tree = ClientPersistentCommitmentTree::open_on_shared_connection(arc.clone(), 100)
            .expect("open tree");
        tree.append(test_leaf(0), Retention::Marked)
            .expect("append");

        // Verify app table is still readable after commitment tree writes
        let guard = arc.lock().expect("lock");
        let value: String = guard
            .query_row("SELECT value FROM my_app_data WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("query app data");
        assert_eq!(
            value, "hello",
            "app data should survive commitment tree writes"
        );
    }

    #[test]
    fn test_witness_after_reopen() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join("test_witness_reopen.db");

        // Phase 1: append a marked leaf and checkpoint
        {
            let mut tree =
                ClientPersistentCommitmentTree::open_path(&db_path, 100).expect("open tree");
            tree.append(test_leaf(0), Retention::Marked)
                .expect("append marked");
            tree.append(test_leaf(1), Retention::Ephemeral)
                .expect("append ephemeral");
            tree.checkpoint(1).expect("checkpoint");
        }

        // Phase 2: reopen and generate witness
        {
            let tree =
                ClientPersistentCommitmentTree::open_path(&db_path, 100).expect("reopen tree");
            let path = tree
                .witness(Position::from(0), 0)
                .expect("witness after reopen");
            assert!(
                path.is_some(),
                "should produce witness for marked leaf after reopen"
            );
        }
    }

    #[test]
    fn test_append_invalid_field_element() {
        let mut tree = memory_tree();
        // All 0xFF bytes is not a valid Pallas field element
        let result = tree.append([0xFF; 32], Retention::Marked);
        assert!(result.is_err(), "should reject invalid field element");
        let msg = format!("{}", result.expect_err("should be error"));
        assert!(
            msg.contains("invalid Pallas field element"),
            "error should mention field element: {msg}"
        );
    }

    #[test]
    fn test_witness_survives_checkpoint_pruning() {
        // Regression test: checkpoint listing used to iterate DESC, which
        // violates the ShardStore contract (ascending checkpoint ID order).
        // shardtree's prune_excess_checkpoints assumes oldest-first, so the
        // wrong order made it delete the newest checkpoints and clear the
        // retention flags on leaves that surviving checkpoints still need,
        // destroying witness data once max_checkpoints was exceeded. Drive
        // both stores through enough checkpoints to force pruning and assert
        // they stay in lockstep.
        const MAX_CHECKPOINTS: usize = 2;

        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        let mut sqlite_tree =
            ClientPersistentCommitmentTree::open(conn, MAX_CHECKPOINTS).expect("open sqlite tree");
        let mut memory_tree = ClientMemoryCommitmentTree::new(MAX_CHECKPOINTS);

        // A marked note early in the tree, whose witness must survive.
        sqlite_tree
            .append(test_leaf(0), Retention::Marked)
            .expect("append marked");
        memory_tree
            .append(test_leaf(0), Retention::Marked)
            .expect("append marked");

        // Enough checkpoints to trigger pruning several times over.
        for i in 1..=6u32 {
            sqlite_tree
                .append(test_leaf(i as u64), Retention::Ephemeral)
                .expect("append");
            memory_tree
                .append(test_leaf(i as u64), Retention::Ephemeral)
                .expect("append");
            assert!(sqlite_tree.checkpoint(i).expect("sqlite checkpoint"));
            assert!(memory_tree.checkpoint(i).expect("memory checkpoint"));
        }

        assert_eq!(
            sqlite_tree.anchor().expect("sqlite anchor"),
            memory_tree.anchor().expect("memory anchor"),
            "anchors should match after pruning"
        );

        let sqlite_witness = sqlite_tree
            .witness(Position::from(0), 0)
            .expect("sqlite witness")
            .expect("sqlite witness should exist for marked leaf after pruning");
        let memory_witness = memory_tree
            .witness(Position::from(0), 0)
            .expect("memory witness")
            .expect("memory witness should exist for marked leaf after pruning");

        assert_eq!(sqlite_witness.position(), memory_witness.position());
        assert_eq!(
            sqlite_witness.auth_path(),
            memory_witness.auth_path(),
            "witness auth paths should match after pruning"
        );
    }

    #[test]
    fn test_append_duplicate_checkpoint_id_refused_before_mutation() {
        // Regression test for issue #882: appending with a duplicate
        // checkpoint id used to persist the leaf (shard write committed)
        // and then fail on the checkpoint insert, returning an error while
        // the database had already changed. The ordering must now be
        // validated before any write.
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join("test_dup_checkpoint.db");

        let anchor_before;
        {
            let mut tree =
                ClientPersistentCommitmentTree::open_path(&db_path, 100).expect("open tree");
            tree.append(test_leaf(0), checkpoint_retention(1))
                .expect("first checkpointed append");
            anchor_before = tree.anchor().expect("anchor");

            let err = tree
                .append(test_leaf(1), checkpoint_retention(1))
                .expect_err("duplicate checkpoint id must be refused");
            assert!(
                matches!(
                    err,
                    CommitmentTreeError::CheckpointOutOfOrder {
                        provided: 1,
                        max: 1
                    }
                ),
                "unexpected error: {err}"
            );

            // Nothing may have been persisted by the refused append.
            assert_eq!(
                tree.max_leaf_position().expect("pos"),
                Some(Position::from(0)),
                "refused append must not persist the leaf"
            );
            assert_eq!(
                tree.anchor().expect("anchor"),
                anchor_before,
                "refused append must not change the anchor"
            );

            // A lower id is refused the same way.
            let err = tree
                .append(test_leaf(1), checkpoint_retention(0))
                .expect_err("lower checkpoint id must be refused");
            assert!(matches!(
                err,
                CommitmentTreeError::CheckpointOutOfOrder {
                    provided: 0,
                    max: 1
                }
            ));
        }

        // Reopen: on-disk state is coherent, and the append can be retried
        // with the next checkpoint id.
        let mut tree =
            ClientPersistentCommitmentTree::open_path(&db_path, 100).expect("reopen tree");
        assert_eq!(
            tree.max_leaf_position().expect("pos"),
            Some(Position::from(0))
        );
        assert_eq!(tree.anchor().expect("anchor"), anchor_before);
        tree.append(test_leaf(1), checkpoint_retention(2))
            .expect("retry with next checkpoint id");
        assert_eq!(
            tree.max_leaf_position().expect("pos"),
            Some(Position::from(1))
        );
    }

    #[test]
    fn test_memory_and_persistent_agree_on_duplicate_checkpoint_id() {
        // Regression test for issue #882: the memory backend used to
        // silently replace an existing checkpoint on a duplicate id while
        // the SQLite backend errored (after persisting the leaf). Both
        // backends must refuse identically, before mutating.
        let conn = Connection::open_in_memory().expect("open sqlite");
        let mut sqlite_tree =
            ClientPersistentCommitmentTree::open(conn, 100).expect("open sqlite tree");
        let mut memory_tree = ClientMemoryCommitmentTree::new(100);

        sqlite_tree
            .append(test_leaf(0), checkpoint_retention(1))
            .expect("sqlite append");
        memory_tree
            .append(test_leaf(0), checkpoint_retention(1))
            .expect("memory append");

        let sqlite_err = sqlite_tree
            .append(test_leaf(1), checkpoint_retention(1))
            .expect_err("sqlite duplicate checkpoint id must be refused");
        let memory_err = memory_tree
            .append(test_leaf(1), checkpoint_retention(1))
            .expect_err("memory duplicate checkpoint id must be refused");
        assert_eq!(
            sqlite_err.to_string(),
            memory_err.to_string(),
            "backends must report the same error"
        );

        assert_eq!(
            sqlite_tree.max_leaf_position().expect("sqlite pos"),
            memory_tree.max_leaf_position().expect("memory pos"),
            "backends must agree on position after a refused append"
        );
        assert_eq!(
            sqlite_tree.anchor().expect("sqlite anchor"),
            memory_tree.anchor().expect("memory anchor"),
            "backends must agree on anchor after a refused append"
        );
    }

    #[test]
    fn test_append_storage_error_rolls_back_shard_write() {
        // Regression test for issue #882: a storage error partway through an
        // append (after the shard write, during the checkpoint insert) used
        // to leave the leaf committed while `append` returned an error.
        // Inject a failure on the checkpoint insert with a SQL trigger and
        // verify the operation-level savepoint rolls the shard write back.
        let conn = Connection::open_in_memory().expect("open sqlite");
        let arc = Arc::new(Mutex::new(conn));
        let mut tree = ClientPersistentCommitmentTree::open_on_shared_connection(arc.clone(), 100)
            .expect("open shared tree");

        tree.append(test_leaf(0), checkpoint_retention(1))
            .expect("first append");
        let anchor_before = tree.anchor().expect("anchor");

        {
            let guard = arc.lock().expect("lock");
            guard
                .execute_batch(
                    "CREATE TABLE test_fail_checkpoint (flag INTEGER);
                     CREATE TRIGGER test_fail_checkpoint_insert
                     BEFORE INSERT ON commitment_tree_checkpoints
                     WHEN EXISTS (SELECT 1 FROM test_fail_checkpoint)
                     BEGIN SELECT RAISE(ABORT, 'injected checkpoint failure'); END;
                     INSERT INTO test_fail_checkpoint VALUES (1);",
                )
                .expect("install failure trigger");
        }

        let err = tree
            .append(test_leaf(1), checkpoint_retention(2))
            .expect_err("append must fail while the trigger is armed");
        assert!(
            err.to_string().contains("injected checkpoint failure"),
            "unexpected error: {err}"
        );

        // The failed append must have been rolled back completely.
        assert_eq!(
            tree.max_leaf_position().expect("pos"),
            Some(Position::from(0)),
            "failed append must not persist the leaf"
        );
        assert_eq!(
            tree.anchor().expect("anchor"),
            anchor_before,
            "failed append must not change the anchor"
        );
        {
            let guard = arc.lock().expect("lock");
            let checkpoints: i64 = guard
                .query_row(
                    "SELECT COUNT(*) FROM commitment_tree_checkpoints",
                    [],
                    |row| row.get(0),
                )
                .expect("count checkpoints");
            assert_eq!(checkpoints, 1, "failed append must not add a checkpoint");
            guard
                .execute("DELETE FROM test_fail_checkpoint", [])
                .expect("disarm trigger");
        }

        // Error recovery: the same append succeeds once the fault clears,
        // without double-inserting the leaf.
        tree.append(test_leaf(1), checkpoint_retention(2))
            .expect("retry after transient failure");
        assert_eq!(
            tree.max_leaf_position().expect("pos"),
            Some(Position::from(1))
        );
    }

    #[test]
    fn test_mutations_compose_with_wallet_transaction() {
        // The store's multi-statement helpers use savepoints (not BEGIN), so
        // tree mutations nest inside a wallet-owned transaction on a shared
        // connection; the wallet owns the final commit or rollback.
        let conn = Connection::open_in_memory().expect("open sqlite");
        let arc = Arc::new(Mutex::new(conn));
        let mut tree = ClientPersistentCommitmentTree::open_on_shared_connection(arc.clone(), 100)
            .expect("open shared tree");

        // Wallet rolls back: the appends and checkpoint vanish with it.
        arc.lock()
            .expect("lock")
            .execute_batch("BEGIN IMMEDIATE")
            .expect("wallet begin");
        tree.append(test_leaf(0), checkpoint_retention(1))
            .expect("append inside wallet transaction");
        assert!(tree.checkpoint(2).expect("checkpoint inside wallet tx"));
        arc.lock()
            .expect("lock")
            .execute_batch("ROLLBACK")
            .expect("wallet rollback");
        assert_eq!(
            tree.max_leaf_position().expect("pos"),
            None,
            "wallet rollback must undo tree mutations"
        );
        assert_eq!(tree.anchor().expect("anchor"), Anchor::empty_tree());

        // Wallet commits: the mutations persist.
        arc.lock()
            .expect("lock")
            .execute_batch("BEGIN IMMEDIATE")
            .expect("wallet begin");
        tree.append(test_leaf(0), checkpoint_retention(1))
            .expect("append inside wallet transaction");
        arc.lock()
            .expect("lock")
            .execute_batch("COMMIT")
            .expect("wallet commit");
        assert_eq!(
            tree.max_leaf_position().expect("pos"),
            Some(Position::from(0))
        );
    }

    #[test]
    fn test_shared_connection_append_and_anchor() {
        let conn = Connection::open_in_memory().expect("open sqlite");
        let arc = Arc::new(Mutex::new(conn));

        let mut tree = ClientPersistentCommitmentTree::open_on_shared_connection(arc.clone(), 100)
            .expect("open shared tree");

        let empty_anchor = tree.anchor().expect("anchor");

        tree.append(test_leaf(0), Retention::Marked)
            .expect("append via shared");
        let anchor1 = tree.anchor().expect("anchor");
        assert_ne!(empty_anchor, anchor1);

        // The Arc is still usable from outside
        let guard = arc.lock().expect("lock");
        let count: i64 = guard
            .query_row("SELECT COUNT(*) FROM commitment_tree_shards", [], |row| {
                row.get(0)
            })
            .expect("direct query");
        assert!(count > 0, "shards should have been written");
    }

    /// Commit failures must undo the leaf, checkpoints, and pruning while
    /// releasing the transaction even before the blocking reader finishes.
    #[test]
    fn test_append_commit_failure_rolls_back_and_allows_retry() {
        for retention in [Retention::Marked, checkpoint_retention(2)] {
            let dir = tempfile::tempdir().unwrap();
            let db_path = dir.path().join("commit_busy.db");
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch("PRAGMA journal_mode=DELETE").unwrap();
            conn.busy_timeout(std::time::Duration::ZERO).unwrap();
            let arc = Arc::new(Mutex::new(conn));
            // Retaining one checkpoint makes the checkpointed append prune
            // the old checkpoint, which must also be restored on failure.
            let mut tree =
                ClientPersistentCommitmentTree::open_on_shared_connection(arc.clone(), 1).unwrap();
            tree.append(test_leaf(0), checkpoint_retention(1)).unwrap();
            let before_anchor = tree.anchor().unwrap();

            let reader = Connection::open(&db_path).unwrap();
            reader.execute_batch("BEGIN").unwrap();
            let _: i64 = reader
                .query_row("SELECT COUNT(*) FROM commitment_tree_shards", [], |r| {
                    r.get(0)
                })
                .unwrap();
            let err = tree
                .append(test_leaf(1), retention.clone())
                .expect_err("reader must block commit");
            assert!(
                err.to_string().contains("savepoint release failed"),
                "{err}"
            );
            assert!(
                arc.lock().unwrap().is_autocommit(),
                "failed append left a transaction open"
            );
            assert_eq!(tree.max_leaf_position().unwrap(), Some(Position::from(0)));
            assert_eq!(tree.anchor().unwrap(), before_anchor);
            let checkpoints: (u32, u32, u32) = arc.lock().unwrap().query_row(
                "SELECT MIN(checkpoint_id), MAX(checkpoint_id), COUNT(*) FROM commitment_tree_checkpoints",
                [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            ).unwrap();
            assert_eq!(
                checkpoints,
                (1, 1, 1),
                "failed commit changed historical checkpoints"
            );

            reader.execute_batch("ROLLBACK").unwrap();
            tree.append(test_leaf(1), retention)
                .expect("retry after reader releases lock");
            assert_eq!(tree.max_leaf_position().unwrap(), Some(Position::from(1)));
            assert!(arc.lock().unwrap().is_autocommit());
            let after_anchor = tree.anchor().unwrap();
            drop(tree);
            drop(arc);
            let reopened = ClientPersistentCommitmentTree::open_path(&db_path, 1).unwrap();
            assert_eq!(
                reopened.max_leaf_position().unwrap(),
                Some(Position::from(1))
            );
            assert_eq!(
                reopened.anchor().unwrap(),
                after_anchor,
                "successful retry was not durable"
            );
        }
    }

    /// A failed checkpoint commit must restore the checkpoint it pruned.
    #[test]
    fn test_checkpoint_commit_failure_rolls_back_and_allows_retry() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("checkpoint_busy.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=DELETE").unwrap();
        conn.busy_timeout(std::time::Duration::ZERO).unwrap();
        let arc = Arc::new(Mutex::new(conn));
        let mut tree =
            ClientPersistentCommitmentTree::open_on_shared_connection(arc.clone(), 1).unwrap();
        tree.append(test_leaf(0), checkpoint_retention(1)).unwrap();
        let before_anchor = tree.anchor().unwrap();
        let reader = Connection::open(&db_path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let _: i64 = reader
            .query_row(
                "SELECT COUNT(*) FROM commitment_tree_checkpoints",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let err = tree
            .checkpoint(2)
            .expect_err("reader must block checkpoint commit");
        assert!(
            err.to_string().contains("savepoint release failed"),
            "{err}"
        );
        assert!(arc.lock().unwrap().is_autocommit());
        assert_eq!(tree.anchor().unwrap(), before_anchor);
        let ids: (u32, u32) = arc
            .lock()
            .unwrap()
            .query_row(
                "SELECT MIN(checkpoint_id), MAX(checkpoint_id) FROM commitment_tree_checkpoints",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(ids, (1, 1));
        reader.execute_batch("ROLLBACK").unwrap();
        assert!(
            tree.checkpoint(2).unwrap(),
            "failed checkpoint must not consume its id"
        );
        assert!(arc.lock().unwrap().is_autocommit());
        drop(tree);
        drop(arc);
        let reopened_conn = Connection::open(&db_path).unwrap();
        let ids: (u32, u32) = reopened_conn
            .query_row(
                "SELECT MIN(checkpoint_id), MAX(checkpoint_id) FROM commitment_tree_checkpoints",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            ids,
            (2, 2),
            "retry must commit the new checkpoint and prune the old one"
        );
    }

    /// Operation cleanup must preserve writes owned by an outer wallet
    /// transaction and permit a retry within that same transaction.
    #[test]
    fn test_append_failure_preserves_wallet_transaction() {
        let conn = Connection::open_in_memory().unwrap();
        let arc = Arc::new(Mutex::new(conn));
        let mut tree =
            ClientPersistentCommitmentTree::open_on_shared_connection(arc.clone(), 1).unwrap();
        tree.append(test_leaf(0), checkpoint_retention(1)).unwrap();
        let before_anchor = tree.anchor().unwrap();
        arc.lock()
            .unwrap()
            .execute_batch(
                "CREATE TABLE wallet_data (value INTEGER);
             CREATE TRIGGER fail_checkpoint BEFORE INSERT ON commitment_tree_checkpoints
             BEGIN SELECT RAISE(ABORT, 'injected checkpoint failure'); END;
             BEGIN;
             INSERT INTO wallet_data VALUES (42);",
            )
            .unwrap();
        let err = tree
            .append(test_leaf(1), checkpoint_retention(2))
            .expect_err("injected failure");
        assert!(
            err.to_string().contains("injected checkpoint failure"),
            "{err}"
        );
        assert_eq!(tree.max_leaf_position().unwrap(), Some(Position::from(0)));
        assert_eq!(tree.anchor().unwrap(), before_anchor);
        {
            let guard = arc.lock().unwrap();
            assert!(
                !guard.is_autocommit(),
                "tree cleanup must not end the wallet transaction"
            );
            let value: i64 = guard
                .query_row("SELECT value FROM wallet_data", [], |r| r.get(0))
                .unwrap();
            assert_eq!(
                value, 42,
                "tree cleanup must not roll back the wallet's writes"
            );
            guard.execute_batch("DROP TRIGGER fail_checkpoint").unwrap();
        }
        tree.append(test_leaf(1), checkpoint_retention(2))
            .expect("retry in wallet transaction");
        assert!(!arc.lock().unwrap().is_autocommit());
        arc.lock()
            .unwrap()
            .execute_batch("COMMIT")
            .expect("wallet still owns final commit");
        assert_eq!(tree.max_leaf_position().unwrap(), Some(Position::from(1)));
        let value: i64 = arc
            .lock()
            .unwrap()
            .query_row("SELECT value FROM wallet_data", [], |r| r.get(0))
            .unwrap();
        assert_eq!(value, 42);
    }
}
