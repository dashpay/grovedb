//! Large values (MERK-08). A cell holds a value of up to 256 bytes in
//! line, and a longer one is kept out of line: with its transaction until
//! write-back places it, and in the value log from then on
//! (`lignin_tree::value`). What Merk reads, hashes and is charged does not
//! depend on where a value is kept, so a Merk of values of every size is
//! the Merk it is on RocksDB.

use lignin_tree::value::InlinePolicy;

use super::*;

/// The value lengths of the test: none, the longest a cell holds in line
/// with one byte less and one more, a page of the test's store with one
/// byte less and one more, more than three pages, and a megabyte.
fn lengths() -> [usize; 9] {
    let (inline, page) = (InlinePolicy::DEFAULT_THRESHOLD, SIZE.get());
    [
        0,
        inline - 1,
        inline,
        inline + 1,
        page - 1,
        page,
        page + 1,
        3 * page + 17,
        1 << 20,
    ]
}

/// A put of a value of `len` bytes. Each stretch of 256 bytes of it,
/// counted from its start, begins with its own number in two bytes, and
/// the other bytes carry `salt`.
fn sized(tree_type: TreeType, k: u64, len: usize, salt: u8) -> (Vec<u8>, Op) {
    let byte = |at: usize| match at % 256 {
        0 => (at >> 8) as u8,
        1 => (at >> 16) as u8,
        within => (within as u8).wrapping_mul(31).wrapping_add(salt),
    };
    let value: Vec<u8> = (0..len).map(byte).collect();
    (
        be(k),
        Op::Put(value, feature(tree_type, k + u64::from(salt))),
    )
}

/// Applies `batch` to the Merk on both engines, each in a transaction
/// that is committed, and holds the Merk on LigninDB to the one on
/// RocksDB: what the Merk and the batch are charged, the root, and every
/// node as its record. Returns the root key.
fn round(rocks: &Rocks, lignin: &Lignin, batch: &[(Vec<u8>, Op)], at: &str) -> Option<Vec<u8>> {
    let tree_type = rocks.tree_type;
    let (merk_cost, batch_cost, hash, root) = rocks.apply(batch);
    let transaction = lignin.path().begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let staged = LigninBatch::new();
    let mut merk = open(
        LigninNodeStore::deferred(&writer, ROOT, Some(&staged)),
        tree_type,
    );
    assert_eq!(apply(&mut merk, batch), merk_cost, "the Merk, {at}");
    let found = (merk.root_hash().unwrap(), merk.root_key());
    drop(merk);
    let applied = staged.apply(&writer);
    applied.value.expect("the batch is applied");
    assert_eq!(applied.cost, batch_cost, "the batch, {at}");
    assert_eq!(found, (hash, root.clone()), "the root, {at}");
    same_nodes(
        rocks,
        &LigninNodeStore::deferred(&writer, ROOT, None),
        &root,
        at,
    );
    writer.into_scope().merge();
    let committed = lignin.commit(&transaction);
    assert!(matches!(committed, Outcome::Committed(_)), "{committed:?}");
    root
}

/// Every node of the tree under `root` is read from `store` as the record
/// RocksDB holds, at the cost of the read on RocksDB.
fn same_nodes(rocks: &Rocks, store: &LigninNodeStore<'_>, root: &Option<Vec<u8>>, at: &str) {
    let expected = rocks.records(root.clone());
    assert_same_records(&records(store, root.clone()), &expected, at);
    let rocks_transaction = rocks.storage.start_transaction();
    let context = rocks
        .storage
        .get_transactional_storage_context(SubtreePath::empty(), None, &rocks_transaction)
        .unwrap();
    for (key, _) in &expected {
        let found = store.get_node(key, NO_VALUE_DEFINED_COST, version());
        let on_rocks = context.get_node(key, NO_VALUE_DEFINED_COST, version());
        assert_eq!(found.cost, on_rocks.cost, "the read of {key:?}, {at}");
    }
}

/// Reads every node of the Merk in a session of its own, which opens the
/// database as the close before it left it.
fn same_nodes_in_the_next_session(
    rocks: &Rocks,
    lignin: Lignin,
    root: &Option<Vec<u8>>,
    at: &str,
) -> Lignin {
    let lignin = lignin.reopened();
    let transaction = lignin.path().begin();
    let reader = LigninNodeStore::read_only(&transaction, ROOT);
    same_nodes(rocks, &reader, root, at);
    drop(reader);
    drop(transaction);
    lignin
}

#[test]
fn a_merk_of_large_values_is_the_merk_it_is_on_rocksdb() {
    let lengths = lengths();
    let count = lengths.len();
    let in_line = |len: usize| InlinePolicy::DEFAULT.is_inline(len);
    // The second batch gives each key the length of the key at the other
    // end, but the middle one, which is put again with the bytes it has,
    // and one that is removed. Among them a value goes each way between
    // a cell and the value log, and one stays out of line at another
    // length.
    let (again, removed) = (count / 2, 3);
    let moves: Vec<(bool, bool)> = (0..count)
        .filter(|k| ![again, removed].contains(k))
        .map(|k| (in_line(lengths[k]), in_line(lengths[count - 1 - k])))
        .collect();
    for way in [(true, false), (false, true), (false, false)] {
        assert!(moves.contains(&way), "no value goes {way:?}");
    }
    assert!(!in_line(lengths[again]) && !in_line(lengths[removed]));
    let (_, Op::Put(longest, _)) = sized(TreeType::NormalTree, 0, lengths[count - 1], 1) else {
        unreachable!("a put");
    };
    let stretches: std::collections::BTreeSet<&[u8]> = longest.chunks(256).collect();
    assert_eq!(
        stretches.len(),
        longest.len() / 256,
        "two stretches are the same"
    );

    for tree_type in TREE_TYPES {
        let at = |what: &str| format!("{tree_type:?}, {what}");
        let rocks = Rocks {
            storage: TempStorage::new(),
            tree_type,
        };
        let lignin = Lignin::new();

        // A value of every length, each under a key of its own.
        let first: Vec<_> = (lengths.iter().enumerate())
            .map(|(k, len)| sized(tree_type, k as u64, *len, 1))
            .collect();
        round(&rocks, &lignin, &first, &at("the first values"));

        let second: Vec<_> = (0..count)
            .map(|k| match k {
                k if k == removed => (be(k as u64), Op::Delete),
                k if k == again => sized(tree_type, k as u64, lengths[k], 1),
                k => sized(tree_type, k as u64, lengths[count - 1 - k], 2),
            })
            .collect();
        let root = round(&rocks, &lignin, &second, &at("the values rewritten"));

        // The next session reads every value where the close left it: one
        // that is out of line from the value log.
        let lignin = same_nodes_in_the_next_session(&rocks, lignin, &root, &at("reopened"));

        // And writes over them. The key that got the megabyte, which is
        // in the value log now, gets a value that a cell holds; one that
        // a cell holds gets the megabyte; and a key whose value is of
        // several pages is removed.
        let (long, short) = (0, count - 1);
        assert!(!in_line(lengths[count - 1 - long]) && in_line(lengths[count - 1 - short]));
        let third = vec![
            sized(tree_type, long as u64, lengths[1], 3),
            (be(1), Op::Delete),
            sized(tree_type, short as u64, lengths[count - 1], 3),
        ];
        let root = round(&rocks, &lignin, &third, &at("rewritten after the reopen"));
        same_nodes_in_the_next_session(&rocks, lignin, &root, &at("reopened again"));
    }
}
