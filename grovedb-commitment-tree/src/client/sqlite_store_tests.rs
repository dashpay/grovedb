#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        sync::{Arc, Mutex},
    };

    use incrementalmerkletree::{Address, Hashable, Level, Position};
    use orchard::tree::MerkleHashOrchard;
    use rusqlite::Connection;
    use shardtree::{
        store::{Checkpoint, ShardStore, TreeState},
        LocatedTree, Node, PrunableTree, RetentionFlags, Tree,
    };

    use crate::client::sqlite_store::{
        deserialize_tree, serialize_tree, SqliteShardStore, SHARD_HEIGHT,
    };

    fn test_store() -> SqliteShardStore {
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        SqliteShardStore::new(conn).expect("create store")
    }

    fn test_hash(i: u8) -> MerkleHashOrchard {
        let empty = MerkleHashOrchard::empty_leaf();
        MerkleHashOrchard::combine(Level::from(i % 31 + 1), &empty, &empty)
    }

    // -- Schema tests --

    #[test]
    fn test_schema_creation() {
        let store = test_store();
        let count: i64 = store
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name LIKE \
                     'commitment_tree_%'",
                    [],
                    |row| row.get(0),
                )
            })
            .expect("query tables");
        assert_eq!(count, 4, "expected 4 commitment_tree_ tables");
    }

    #[test]
    fn test_schema_idempotent() {
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        let _store = SqliteShardStore::new(conn).expect("first create");
    }

    #[test]
    fn test_shared_connection() {
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        let arc = Arc::new(Mutex::new(conn));
        let mut store = SqliteShardStore::new_shared(arc.clone()).expect("create shared store");

        // Store works
        let addr = Address::from_parts(Level::from(SHARD_HEIGHT), 0);
        let h = test_hash(1);
        let tree = Tree::leaf((h, RetentionFlags::MARKED));
        let located = LocatedTree::from_parts(addr, tree).expect("create located");
        store.put_shard(located).expect("put shard via shared");

        let retrieved = store.get_shard(addr).expect("get shard via shared");
        assert!(retrieved.is_some());

        // Original Arc is still usable (mutex not poisoned)
        let guard = arc.lock().expect("lock after store ops");
        let count: i64 = guard
            .query_row("SELECT COUNT(*) FROM commitment_tree_shards", [], |row| {
                row.get(0)
            })
            .expect("direct query");
        assert_eq!(count, 1);
    }

    // -- Serialization round-trip tests --

    #[test]
    fn test_serialize_nil() {
        let tree: PrunableTree<MerkleHashOrchard> = Tree::empty();
        let data = serialize_tree(&tree);
        let mut pos = 0;
        let decoded = deserialize_tree(&data, &mut pos).expect("deserialize nil");
        assert!(decoded.is_empty());
    }

    #[test]
    fn test_serialize_leaf() {
        let hash = test_hash(1);
        let flags = RetentionFlags::MARKED;
        let tree = Tree::leaf((hash, flags));
        let data = serialize_tree(&tree);
        assert_eq!(data.len(), 34); // 1 tag + 32 hash + 1 flags
        let mut pos = 0;
        let decoded = deserialize_tree(&data, &mut pos).expect("deserialize leaf");
        match &*decoded {
            Node::Leaf { value: (h, f) } => {
                assert_eq!(h.to_bytes(), hash.to_bytes());
                assert_eq!(*f, RetentionFlags::MARKED);
            }
            _ => panic!("expected leaf"),
        }
    }

    #[test]
    fn test_serialize_parent_with_annotation() {
        let h1 = test_hash(1);
        let h2 = test_hash(2);
        let h3 = test_hash(3);
        let left = Tree::leaf((h1, RetentionFlags::MARKED));
        let right = Tree::leaf((h2, RetentionFlags::CHECKPOINT));
        let tree = Tree::parent(Some(Arc::new(h3)), left, right);
        let data = serialize_tree(&tree);
        let mut pos = 0;
        let decoded = deserialize_tree(&data, &mut pos).expect("deserialize parent");
        match &*decoded {
            Node::Parent { ann, .. } => {
                assert!(ann.is_some());
                assert_eq!(ann.as_ref().expect("ann").to_bytes(), h3.to_bytes());
            }
            _ => panic!("expected parent"),
        }
    }

    #[test]
    fn test_serialize_parent_without_annotation() {
        let h1 = test_hash(1);
        let left = Tree::leaf((h1, RetentionFlags::EPHEMERAL));
        let right = Tree::empty();
        let tree: PrunableTree<MerkleHashOrchard> = Tree::parent(None, left, right);
        let data = serialize_tree(&tree);
        let mut pos = 0;
        let decoded = deserialize_tree(&data, &mut pos).expect("deserialize parent no ann");
        match &*decoded {
            Node::Parent { ann, .. } => {
                assert!(ann.is_none());
            }
            _ => panic!("expected parent"),
        }
    }

    #[test]
    fn test_serialize_deep_tree() {
        let h1 = test_hash(1);
        let h2 = test_hash(2);
        let h3 = test_hash(3);
        let leaf1 = Tree::leaf((h1, RetentionFlags::MARKED));
        let leaf2 = Tree::leaf((h2, RetentionFlags::EPHEMERAL));
        let inner: PrunableTree<MerkleHashOrchard> = Tree::parent(None, leaf1, leaf2);
        let leaf3 = Tree::leaf((h3, RetentionFlags::CHECKPOINT | RetentionFlags::MARKED));
        let root: PrunableTree<MerkleHashOrchard> = Tree::parent(Some(Arc::new(h1)), inner, leaf3);
        let data = serialize_tree(&root);
        let mut pos = 0;
        let decoded = deserialize_tree(&data, &mut pos).expect("deserialize deep tree");
        assert_eq!(pos, data.len(), "should consume all bytes");
        match &*decoded {
            Node::Parent { ann, left, right } => {
                assert!(ann.is_some());
                match &***left {
                    Node::Parent { ann: inner_ann, .. } => {
                        let _: &Option<Arc<MerkleHashOrchard>> = inner_ann;
                        assert!(inner_ann.is_none());
                    }
                    _ => panic!("expected inner parent"),
                }
                match &***right {
                    Node::Leaf { value: (_, f) } => {
                        assert!(f.is_checkpoint());
                        assert!(f.is_marked());
                    }
                    _ => panic!("expected leaf"),
                }
            }
            _ => panic!("expected root parent"),
        }
    }

    // -- Shard CRUD tests --

    #[test]
    fn test_shard_round_trip() {
        let mut store = test_store();
        let addr = Address::from_parts(Level::from(SHARD_HEIGHT), 0);
        let h1 = test_hash(1);
        let tree = Tree::leaf((h1, RetentionFlags::MARKED));
        let located = LocatedTree::from_parts(addr, tree).expect("create located tree");
        store.put_shard(located).expect("put shard");

        let retrieved = store.get_shard(addr).expect("get shard");
        assert!(retrieved.is_some());
        let retrieved = retrieved.expect("shard should exist");
        assert_eq!(retrieved.root_addr(), addr);
    }

    #[test]
    fn test_shard_not_found() {
        let store = test_store();
        let addr = Address::from_parts(Level::from(SHARD_HEIGHT), 42);
        let result = store.get_shard(addr).expect("get shard");
        assert!(result.is_none());
    }

    #[test]
    fn test_last_shard() {
        let mut store = test_store();
        assert!(store.last_shard().expect("last shard empty").is_none());

        for i in 0..3u64 {
            let addr = Address::from_parts(Level::from(SHARD_HEIGHT), i);
            let h = test_hash(i as u8);
            let tree = Tree::leaf((h, RetentionFlags::EPHEMERAL));
            let located = LocatedTree::from_parts(addr, tree).expect("create located");
            store.put_shard(located).expect("put shard");
        }

        let last = store
            .last_shard()
            .expect("last shard")
            .expect("should exist");
        assert_eq!(last.root_addr().index(), 2);
    }

    #[test]
    fn test_get_shard_roots() {
        let mut store = test_store();
        assert!(store.get_shard_roots().expect("empty roots").is_empty());

        for i in [0, 2, 5u64] {
            let addr = Address::from_parts(Level::from(SHARD_HEIGHT), i);
            let h = test_hash(i as u8);
            let tree = Tree::leaf((h, RetentionFlags::EPHEMERAL));
            let located = LocatedTree::from_parts(addr, tree).expect("create located");
            store.put_shard(located).expect("put shard");
        }

        let roots = store.get_shard_roots().expect("roots");
        assert_eq!(roots.len(), 3);
        assert_eq!(roots[0].index(), 0);
        assert_eq!(roots[1].index(), 2);
        assert_eq!(roots[2].index(), 5);
    }

    #[test]
    fn test_truncate_shards() {
        let mut store = test_store();
        for i in 0..5u64 {
            let addr = Address::from_parts(Level::from(SHARD_HEIGHT), i);
            let h = test_hash(i as u8);
            let tree = Tree::leaf((h, RetentionFlags::EPHEMERAL));
            let located = LocatedTree::from_parts(addr, tree).expect("create located");
            store.put_shard(located).expect("put shard");
        }

        store.truncate_shards(3).expect("truncate shards");
        let roots = store.get_shard_roots().expect("roots");
        assert_eq!(roots.len(), 3);
        assert_eq!(roots.last().expect("last root").index(), 2);
    }

    // -- Cap tests --

    #[test]
    fn test_cap_empty_default() {
        let store = test_store();
        let cap = store.get_cap().expect("get cap");
        assert!(cap.is_empty());
    }

    #[test]
    fn test_cap_round_trip() {
        let mut store = test_store();
        let h = test_hash(10);
        let cap: PrunableTree<MerkleHashOrchard> = Tree::leaf((h, RetentionFlags::EPHEMERAL));
        store.put_cap(cap).expect("put cap");
        let retrieved = store.get_cap().expect("get cap");
        match &*retrieved {
            Node::Leaf { value: (hash, _) } => {
                assert_eq!(hash.to_bytes(), h.to_bytes());
            }
            _ => panic!("expected leaf cap"),
        }
    }

    // -- Checkpoint tests --

    #[test]
    fn test_checkpoint_empty() {
        let store = test_store();
        assert_eq!(store.checkpoint_count().expect("count"), 0);
        assert!(store.min_checkpoint_id().expect("min").is_none());
        assert!(store.max_checkpoint_id().expect("max").is_none());
    }

    #[test]
    fn test_checkpoint_add_and_get() {
        let mut store = test_store();
        let cp = Checkpoint::at_position(Position::from(42));
        store.add_checkpoint(1, cp).expect("add checkpoint");

        assert_eq!(store.checkpoint_count().expect("count"), 1);
        assert_eq!(store.min_checkpoint_id().expect("min"), Some(1));
        assert_eq!(store.max_checkpoint_id().expect("max"), Some(1));

        let retrieved = store.get_checkpoint(&1).expect("get").expect("exists");
        assert_eq!(
            retrieved.tree_state(),
            TreeState::AtPosition(Position::from(42))
        );
        assert!(retrieved.marks_removed().is_empty());
    }

    #[test]
    fn test_checkpoint_with_marks_removed() {
        let mut store = test_store();
        let mut marks = BTreeSet::new();
        marks.insert(Position::from(10));
        marks.insert(Position::from(20));
        marks.insert(Position::from(30));
        let cp = Checkpoint::from_parts(TreeState::AtPosition(Position::from(50)), marks);
        store.add_checkpoint(5, cp).expect("add checkpoint");

        let retrieved = store.get_checkpoint(&5).expect("get").expect("exists");
        assert_eq!(retrieved.marks_removed().len(), 3);
        assert!(retrieved.marks_removed().contains(&Position::from(10)));
        assert!(retrieved.marks_removed().contains(&Position::from(20)));
        assert!(retrieved.marks_removed().contains(&Position::from(30)));
    }

    #[test]
    fn test_checkpoint_at_depth() {
        let mut store = test_store();
        for i in 1..=5u32 {
            let cp = Checkpoint::at_position(Position::from(i as u64 * 10));
            store.add_checkpoint(i, cp).expect("add checkpoint");
        }

        let (id, cp) = store
            .get_checkpoint_at_depth(0)
            .expect("depth 0")
            .expect("exists");
        assert_eq!(id, 5);
        assert_eq!(cp.tree_state(), TreeState::AtPosition(Position::from(50)));

        let (id, _) = store
            .get_checkpoint_at_depth(2)
            .expect("depth 2")
            .expect("exists");
        assert_eq!(id, 3);

        assert!(store
            .get_checkpoint_at_depth(10)
            .expect("depth 10")
            .is_none());
    }

    #[test]
    fn test_checkpoint_empty_tree_state() {
        let mut store = test_store();
        let cp = Checkpoint::tree_empty();
        store.add_checkpoint(1, cp).expect("add empty checkpoint");

        let retrieved = store.get_checkpoint(&1).expect("get").expect("exists");
        assert_eq!(retrieved.tree_state(), TreeState::Empty);
    }

    #[test]
    fn test_remove_checkpoint() {
        let mut store = test_store();
        let mut marks = BTreeSet::new();
        marks.insert(Position::from(5));
        let cp = Checkpoint::from_parts(TreeState::AtPosition(Position::from(10)), marks);
        store.add_checkpoint(1, cp).expect("add");
        store
            .add_checkpoint(2, Checkpoint::tree_empty())
            .expect("add");

        store.remove_checkpoint(&1).expect("remove");
        assert_eq!(store.checkpoint_count().expect("count"), 1);
        assert!(store.get_checkpoint(&1).expect("get").is_none());
        assert!(store.get_checkpoint(&2).expect("get").is_some());
    }

    #[test]
    fn test_truncate_checkpoints_retaining() {
        let mut store = test_store();
        for i in 1..=5u32 {
            let mut marks = BTreeSet::new();
            marks.insert(Position::from(i as u64));
            let cp =
                Checkpoint::from_parts(TreeState::AtPosition(Position::from(i as u64 * 10)), marks);
            store.add_checkpoint(i, cp).expect("add");
        }

        store.truncate_checkpoints_retaining(&3).expect("truncate");
        assert_eq!(store.checkpoint_count().expect("count"), 3);
        assert_eq!(store.max_checkpoint_id().expect("max"), Some(3));

        let cp3 = store.get_checkpoint(&3).expect("get").expect("exists");
        assert!(cp3.marks_removed().is_empty());

        let cp2 = store.get_checkpoint(&2).expect("get").expect("exists");
        assert_eq!(cp2.marks_removed().len(), 1);
    }

    #[test]
    fn test_update_checkpoint_with() {
        let mut store = test_store();
        let cp = Checkpoint::at_position(Position::from(10));
        store.add_checkpoint(1, cp).expect("add");

        let updated = store
            .update_checkpoint_with(&1, |_cp| Ok(()))
            .expect("update");
        assert!(updated);

        let updated = store
            .update_checkpoint_with(&999, |_| Ok(()))
            .expect("update nonexistent");
        assert!(!updated);
    }

    #[test]
    fn test_for_each_checkpoint() {
        let mut store = test_store();
        for i in 1..=5u32 {
            store
                .add_checkpoint(i, Checkpoint::at_position(Position::from(i as u64)))
                .expect("add");
        }

        let mut ids = Vec::new();
        store
            .for_each_checkpoint(3, |id, _| {
                ids.push(*id);
                Ok(())
            })
            .expect("for_each");
        assert_eq!(ids, vec![5, 4, 3]);
    }
}
