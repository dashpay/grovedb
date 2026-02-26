#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use incrementalmerkletree::{Position, Retention};
    use orchard::tree::Anchor;
    use rusqlite::Connection;

    use crate::{test_utils::test_leaf, ClientPersistentCommitmentTree};

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
            .query_row(
                "SELECT value FROM my_app_data WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .expect("query app data");
        assert_eq!(value, "hello", "app data should survive commitment tree writes");
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
}
