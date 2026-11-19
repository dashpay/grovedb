//! The cursor of the store, and what Merk does through it and beside it:
//! raw iteration, `clear`, the emptiness checks, and the aux and meta
//! records (MERK-05). Where RocksDB can be asked, it is: the same calls on
//! a Merk on RocksDB give the same answers and charge the same.

use std::collections::BTreeSet;

use grovedb_costs::storage_cost::key_value_cost::KeyValueStorageCost;
use grovedb_query::Query;
use grovedb_storage::rocksdb_storage::RocksDbStorage;

use super::{
    super::access::{with_child_keys, Seek, StoredLink, Walk},
    *,
};
use crate::merk::KVIterator;

type RocksTransaction<'a> = <RocksDbStorage as Storage<'a>>::Transaction;

/// One call on a Merk, or on a cursor of its store.
#[derive(Clone, Debug)]
enum Act {
    Apply(Vec<(Vec<u8>, Op)>),
    Aux(Vec<(Vec<u8>, Op, Option<KeyValueStorageCost>)>),
    GetAux(Vec<u8>),
    PutMeta(Vec<u8>, Vec<u8>),
    GetMeta(Vec<u8>),
    DeleteMeta(Vec<u8>),
    Clear,
    IsEmpty,
    IsEmptyExcept(Vec<Vec<u8>>),
    /// Every record a query for all keys gives, forwards or backwards.
    Scan(bool),
    /// The records of a range, forwards or backwards.
    Range(Vec<u8>, Vec<u8>, bool),
    /// A cursor that is moved about by the numbers of a seed.
    Wander(u64),
}

/// What a cursor shows after one move.
#[derive(Debug, PartialEq)]
struct Seen {
    to: String,
    valid: bool,
    key: Option<Vec<u8>>,
    value: Option<Option<Vec<u8>>>,
}

fn rows<I: NodeCursor>(cursor: I, query: &Query, cost: &mut OperationCost) -> String {
    let mut rows = KVIterator::new(cursor, query).unwrap_add_cost(cost);
    let mut found = Vec::new();
    while let Some(row) = rows.next_kv().unwrap_add_cost(cost) {
        found.push(row);
    }
    format!("{found:?}")
}

thread_local! {
    /// The edges the walks of this thread's test came to.
    static EDGES: std::cell::RefCell<BTreeSet<&'static str>> = const {
        std::cell::RefCell::new(BTreeSet::new())
    };
}

/// Moves a cursor of `store` to places the numbers of `seed` choose, and
/// says what it showed after each move. A cursor that stands on no record
/// is sought again and not stepped: RocksDB does not say where a step from
/// there leads.
fn wander<'db, S: NodeStore<'db>>(store: &S, seed: u64, cost: &mut OperationCost) -> Vec<Seen> {
    let mut random = Random(seed | 1);
    let mut cursor = store.cursor();
    let mut seen = Vec::new();
    let mut valid = false;
    // The first and the last key of the Merk, once the walk has been there.
    let (mut first, mut last): (Option<Vec<u8>>, Option<Vec<u8>>) = (None, None);
    for _ in 0..240 {
        let mut target = match random.below(3) {
            0 => vec![random.below(256) as u8],
            _ => key(random.below(320)),
        };
        if random.below(4) == 0 {
            target.push(random.below(256) as u8);
        }
        let choice = match valid {
            true => random.below(12),
            false => random.below(4),
        };
        let sought = match choice {
            2 => Some((false, target.clone())),
            3 => Some((true, target.clone())),
            _ => None,
        };
        let to = match choice {
            0 => {
                cursor.seek_to_first().unwrap_add_cost(cost);
                "the first".to_owned()
            }
            1 => {
                cursor.seek_to_last().unwrap_add_cost(cost);
                "the last".to_owned()
            }
            2 => {
                cursor.seek(&target).unwrap_add_cost(cost);
                format!("from {target:?}")
            }
            3 => {
                cursor.seek_for_prev(&target).unwrap_add_cost(cost);
                format!("back from {target:?}")
            }
            4..=8 => {
                cursor.next().unwrap_add_cost(cost);
                "the next".to_owned()
            }
            _ => {
                cursor.prev().unwrap_add_cost(cost);
                "the one before".to_owned()
            }
        };
        valid = cursor.valid().unwrap_add_cost(cost);
        let key = cursor.key().unwrap_add_cost(cost).map(<[u8]>::to_vec);
        // A walk reads the record, reads it twice, or reads only the key.
        let reads = match random.below(4) {
            0 => 0,
            1 => 2,
            _ => 1,
        };
        if reads == 2 {
            cursor.value().unwrap_add_cost(cost);
        }
        let value = (reads > 0).then(|| cursor.value().unwrap_add_cost(cost).map(<[u8]>::to_vec));
        cursor.status().expect("the cursor has not failed");

        // Which edge of a walk this was.
        match (choice, &key) {
            (0, Some(key)) => first = Some(key.clone()),
            (1, Some(key)) => last = Some(key.clone()),
            _ => {}
        }
        let within = |target: &Vec<u8>| {
            first.as_ref().is_some_and(|first| first < target)
                && last.as_ref().is_some_and(|last| target < last)
        };
        let moved = match (choice, valid, &sought, &key) {
            (4..=8, true, ..) => "a step to the next record",
            (4..=8, false, ..) => "a step off the last record",
            (9.., true, ..) => "a step to the record before",
            (9.., false, ..) => "a step off the first record",
            (_, _, Some((false, _)), None) => "a seek beyond the last record",
            (_, _, Some((true, _)), None) => "a seek back beyond the first record",
            (_, _, Some((_, target)), Some(key)) if key == target => "a seek to a key",
            (_, _, Some((false, target)), Some(_)) if within(target) => "a seek between two keys",
            (_, _, Some((true, target)), Some(_)) if within(target) => {
                "a seek back between two keys"
            }
            _ => "another seek",
        };
        let read = match (valid, reads) {
            (true, 0) => "a record that was not read",
            (true, 1) => "a record that was read once",
            (true, _) => "a record that was read twice",
            (false, 0) => "no record, and none asked for",
            (false, _) => "no record to read",
        };
        EDGES.with_borrow_mut(|edges| edges.extend([moved, read]));
        seen.push(Seen {
            to,
            valid,
            key,
            value,
        });
    }
    seen
}

/// Makes one call, and returns what it answered and what it charged.
fn act<'db, S: NodeStore<'db>>(merk: &mut Merk<S>, act: &Act) -> (String, OperationCost) {
    let mut cost = OperationCost::default();
    let answer = match act {
        Act::Apply(batch) => {
            let applied = merk.apply::<_, Vec<u8>>(batch, &[], None, version());
            format!("{:?}", applied.unwrap_add_cost(&mut cost))
        }
        Act::Aux(aux) => {
            let applied = merk.apply::<Vec<u8>, _>(&[], aux, None, version());
            format!("{:?}", applied.unwrap_add_cost(&mut cost))
        }
        Act::GetAux(key) => format!("{:?}", merk.get_aux(key).unwrap_add_cost(&mut cost)),
        Act::PutMeta(key, value) => {
            let put = merk.put_meta(key.clone(), value.clone());
            format!("{:?}", put.unwrap_add_cost(&mut cost))
        }
        Act::GetMeta(key) => {
            let meta = merk.get_meta(key.clone()).unwrap_add_cost(&mut cost);
            format!("{meta:?}")
        }
        Act::DeleteMeta(key) => {
            format!("{:?}", merk.delete_meta(key).unwrap_add_cost(&mut cost))
        }
        Act::Clear => format!("{:?}", merk.clear().unwrap_add_cost(&mut cost)),
        Act::IsEmpty => format!("{:?}", merk.is_empty_tree().unwrap_add_cost(&mut cost)),
        Act::IsEmptyExcept(keys) => {
            let except: BTreeSet<&[u8]> = keys.iter().map(Vec::as_slice).collect();
            let empty = merk.is_empty_tree_except(except);
            format!("{:?}", empty.unwrap_add_cost(&mut cost))
        }
        Act::Scan(left_to_right) => {
            let mut query = Query::new_with_direction(*left_to_right);
            query.insert_all();
            rows(merk.storage.cursor(), &query, &mut cost)
        }
        Act::Range(from, to, left_to_right) => {
            let mut query = Query::new_with_direction(*left_to_right);
            query.insert_range_inclusive(from.clone()..=to.clone());
            query.insert_key(key(7));
            rows(merk.storage.cursor(), &query, &mut cost)
        }
        Act::Wander(seed) => format!("{:#?}", wander(&merk.storage, *seed, &mut cost)),
    };
    (answer, cost)
}

/// What a Merk that is opened on a handle answered to `acts`, call by call,
/// with the cost of each.
fn acts<'db, S: NodeStore<'db>>(
    store: S,
    tree_type: TreeType,
    acts: &[Act],
) -> Vec<(String, OperationCost)> {
    let opened = Merk::open_base(store, tree_type, NO_VALUE_DEFINED_COST, version());
    let mut cost = OperationCost::default();
    let mut answers = Vec::new();
    match opened.unwrap_add_cost(&mut cost) {
        Ok(mut merk) => {
            answers.push(("opened".to_owned(), cost));
            answers.extend(acts.iter().map(|at| act(&mut merk, at)));
        }
        Err(error) => answers.push((format!("not opened: {error:?}"), cost)),
    }
    answers
}

/// The same Merk on RocksDB and on LigninDB, each inside a transaction.
struct Both<'a, 't, 's> {
    tree_type: TreeType,
    rocks: &'a TempStorage,
    rocks_transaction: &'a RocksTransaction<'a>,
    writer: &'a LigninWriter<'t, 's>,
}

impl Both<'_, '_, '_> {
    /// Makes `calls` on a Merk on a deferred handle of each engine and
    /// applies what they staged. The answers, the cost of every call and
    /// the cost of the batch are the same on both, and are returned.
    fn deferred(&self, calls: &[Act]) -> Vec<String> {
        let storage_batch = StorageBatch::new();
        let context = self
            .rocks
            .get_transactional_storage_context(
                SubtreePath::empty(),
                Some(&storage_batch),
                self.rocks_transaction,
            )
            .unwrap();
        let mut expected = acts(context, self.tree_type, calls);
        let applied = self
            .rocks
            .commit_multi_context_batch(storage_batch, Some(self.rocks_transaction));
        expected.push((format!("{:?}", applied.value), applied.cost));

        let staged = LigninBatch::new();
        let store = LigninNodeStore::deferred(self.writer, ROOT, Some(&staged));
        let mut found = acts(store, self.tree_type, calls);
        let applied = staged.apply(self.writer);
        found.push((format!("{:?}", applied.value.map(|_| ())), applied.cost));
        self.same(calls, found, expected)
    }

    /// [`Self::immediate`] or [`Self::deferred`].
    fn run(&self, immediate: bool, calls: &[Act]) -> Vec<String> {
        match immediate {
            true => self.immediate(calls),
            false => self.deferred(calls),
        }
    }

    /// As [`Self::deferred`], on handles that write at once.
    fn immediate(&self, calls: &[Act]) -> Vec<String> {
        let context = self
            .rocks
            .get_immediate_storage_context(SubtreePath::empty(), self.rocks_transaction)
            .unwrap();
        let expected = acts(context, self.tree_type, calls);
        let store = LigninNodeStore::immediate(self.writer, ROOT);
        let found = acts(store, self.tree_type, calls);
        self.same(calls, found, expected)
    }

    fn same(
        &self,
        calls: &[Act],
        found: Vec<(String, OperationCost)>,
        expected: Vec<(String, OperationCost)>,
    ) -> Vec<String> {
        assert_eq!(found.len(), expected.len());
        let named = |at: usize| match at.checked_sub(1).map(|at| calls.get(at)) {
            None => "the open".to_owned(),
            Some(None) => "the batch".to_owned(),
            Some(Some(call)) => {
                let mut name = format!("{call:?}");
                name.truncate(60);
                name
            }
        };
        for (at, (found, expected)) in found.iter().zip(&expected).enumerate() {
            let (tree_type, call) = (self.tree_type, named(at));
            assert_eq!(found.0, expected.0, "{tree_type:?}: the answer of {call}");
            assert_eq!(found.1, expected.1, "{tree_type:?}: the cost of {call}");
        }
        found.into_iter().map(|(answer, _)| answer).collect()
    }
}

/// Runs `test` on a Merk of `tree_type` that both engines hold: random
/// batches committed, with what `before` did, and a few more written by
/// the transaction that `test` is given, which rewrite some of the
/// committed nodes, remove some and add others.
fn with_both(
    tree_type: TreeType,
    seed: u64,
    rounds: usize,
    before: &[Act],
    test: impl FnOnce(&Both<'_, '_, '_>),
) {
    let rocks = TempStorage::new();
    let lignin = Lignin::new();
    let path = lignin.path();
    let mut random = Random(seed);
    let mut model = BTreeMap::new();
    let mut batches = |rounds: usize| -> Vec<Act> {
        let batch = |_| Act::Apply(random_batch(&mut random, tree_type, &mut model, true));
        (0..rounds).map(batch).collect()
    };
    // Many more nodes are committed than the transaction then writes, so
    // that it leaves some as they were.
    let (committed, written) = (batches(4 * rounds), batches(rounds.div_ceil(3)));

    let rocks_transaction = rocks.start_transaction();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let both = Both {
        tree_type,
        rocks: &rocks,
        rocks_transaction: &rocks_transaction,
        writer: &writer,
    };
    for batch in &committed {
        both.deferred(std::slice::from_ref(batch));
    }
    both.deferred(before);
    rocks
        .commit_transaction(rocks_transaction)
        .unwrap()
        .expect("the transaction commits");
    writer.into_scope().merge();
    lignin.commit(&transaction);

    let rocks_transaction = rocks.start_transaction();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let both = Both {
        tree_type,
        rocks: &rocks,
        rocks_transaction: &rocks_transaction,
        writer: &writer,
    };
    for batch in &written {
        both.deferred(std::slice::from_ref(batch));
    }
    // Whatever the random batches did, the transaction also removes a
    // committed node, gives another a new value and makes one.
    let reader = path.begin();
    let nodes = |store: &LigninNodeStore<'_>| -> BTreeMap<Vec<u8>, Vec<u8>> {
        walked(store).into_iter().collect()
    };
    let committed = nodes(&LigninNodeStore::read_only(&reader, ROOT));
    let now = nodes(&LigninNodeStore::deferred(&writer, ROOT, None));
    let mut still = committed.keys().filter(|key| now.contains_key(*key));
    let (gone, again) = (still.next(), still.next_back());
    let (gone, again) = (gone.expect("a node"), again.expect("another"));
    let sure = [
        (gone.clone(), Op::Delete),
        (
            again.clone(),
            Op::Put(vec![0xab; 77], feature(tree_type, 9)),
        ),
        (
            vec![0xfe; 9],
            Op::Put(vec![0xcd; 33], feature(tree_type, 8)),
        ),
    ];
    both.deferred(&[Act::Apply(sure.to_vec())]);

    // So the Merk the test is given has nodes that are as they were
    // committed, nodes the transaction rewrote, nodes it made and nodes it
    // removed.
    let now = nodes(&LigninNodeStore::deferred(&writer, ROOT, None));
    let (mut kept, mut rewritten, mut made) = (0, 0, 0);
    for (key, record) in &now {
        match committed.get(key) {
            Some(was) if was == record => kept += 1,
            Some(_) => rewritten += 1,
            None => made += 1,
        }
    }
    let removed = (committed.keys())
        .filter(|key| !now.contains_key(*key))
        .count();
    assert!(
        kept > 0 && rewritten > 0 && made > 0 && removed > 0,
        "{tree_type:?}: {kept} kept, {rewritten} rewritten, {made} made, {removed} removed"
    );
    test(&both);
}

#[test]
fn a_cursor_shows_the_records_rocksdb_shows_and_charges_what_it_charges() {
    // The thread of this test may have run another that walks.
    EDGES.take();
    for (seed, tree_type) in TREE_TYPES.into_iter().enumerate() {
        with_both(tree_type, 0x05ee_d000 + seed as u64, 5, &[], |both| {
            let walks = [
                Act::Wander(3 + seed as u64),
                Act::Scan(true),
                Act::Scan(false),
                Act::Range(key(40), key(170), true),
                Act::Range(key(100), key(260), false),
                Act::IsEmpty,
            ];
            let answers = both.deferred(&walks);
            // The walk came to records, and a scan shows them all.
            assert!(answers[1].contains("valid: true"), "{}", answers[1]);
            assert!(answers[2].len() > 1_000);
            assert_eq!(answers[6], "false");
            // A handle that writes at once reads the same records.
            both.immediate(&walks);
        });
    }
    // The walks came to every edge.
    let edges = EDGES.take();
    for edge in [
        "a step to the next record",
        "a step off the last record",
        "a step to the record before",
        "a step off the first record",
        "a seek beyond the last record",
        "a seek back beyond the first record",
        "a seek to a key",
        "a seek between two keys",
        "a seek back between two keys",
        "a record that was not read",
        "a record that was read once",
        "a record that was read twice",
        "no record to read",
    ] {
        assert!(edges.contains(edge), "no walk came to {edge}: {edges:?}");
    }
}

#[test]
fn clear_removes_every_node_and_charges_what_it_charges_on_rocksdb() {
    for (seed, tree_type) in TREE_TYPES.into_iter().enumerate() {
        for immediate in [false, true] {
            with_both(tree_type, 0x0c1e_a400 + seed as u64, 4, &[], |both| {
                let run = |calls: &[Act]| both.run(immediate, calls);
                // The nodes are those the last commit left and those the
                // transaction wrote since. Each is removed with no cost of
                // the writer's, and is charged from its committed record:
                // a node the transaction rewrote by what was committed, and
                // one it made by nothing (`cost-model.md` D2).
                let side = vec![
                    Act::Aux(vec![(b"aux".to_vec(), Op::Put(vec![7; 40], BASIC), None)]),
                    Act::PutMeta(b"meta".to_vec(), vec![8; 30]),
                ];
                run(&side);
                let answers = run(&[Act::IsEmpty, Act::Clear, Act::IsEmpty]);
                assert_eq!(answers[1..3], ["false", "Ok(())"]);
                // A handle that writes at once has no node from then on, and
                // a deferred one reads the nodes until its batch is applied.
                assert_eq!(answers[3], format!("{immediate}"));

                // Nothing is left to walk, on any handle. The records that
                // are no nodes are where they were.
                let after = [
                    Act::IsEmpty,
                    Act::IsEmptyExcept(Vec::new()),
                    Act::Scan(true),
                    Act::Wander(9),
                    Act::GetAux(b"aux".to_vec()),
                    Act::GetMeta(b"meta".to_vec()),
                ];
                let reader = LigninNodeStore::deferred(both.writer, ROOT, None);
                let mut cursor = reader.cursor();
                cursor.seek_to_first().unwrap();
                assert!(!cursor.valid().unwrap() && cursor.status().is_ok());
                assert!(!reader.has_node(&key(1)).unwrap().unwrap());
                assert!(reader.get_aux_record(b"aux").unwrap().unwrap().is_some());
                assert!(reader.get_meta_record(b"meta").unwrap().unwrap().is_some());
                let context = both
                    .rocks
                    .get_transactional_storage_context(
                        SubtreePath::empty(),
                        None,
                        both.rocks_transaction,
                    )
                    .unwrap();
                // A base Merk keeps the key of its root in a record of its
                // own, which `clear` leaves on both engines.
                assert_eq!(
                    reader.root_key().unwrap().unwrap(),
                    NodeStore::root_key(&context).unwrap().unwrap()
                );
                let opened = acts(reader, both.tree_type, &after);
                assert_eq!(opened, acts(context, both.tree_type, &after));
            });
        }
    }
}

const BASIC: TreeFeatureType = TreeFeatureType::BasicMerkNode;

/// The aux and the meta records of a Merk are flat records of its subtree.
/// Merk reads, writes and removes them on LigninDB as on RocksDB, and is
/// charged the same: also for a record that is removed with no cost of the
/// writer's, which is charged from what is committed under its key.
#[test]
fn the_aux_and_meta_records_of_a_merk_are_what_they_are_on_rocksdb() {
    let aux = |key: &[u8], op: Op| (key.to_vec(), op, None);
    let name = |key: &[u8]| key.to_vec();
    let committed = [
        Act::Aux(vec![
            aux(b"committed", Op::Put(vec![1; 300], BASIC)),
            aux(b"kept", Op::Put(vec![2; 9], BASIC)),
        ]),
        Act::PutMeta(name(b"committed"), vec![3; 70]),
        Act::PutMeta(name(b"kept"), Vec::new()),
    ];
    for immediate in [false, true] {
        with_both(TreeType::NormalTree, 0x0a11, 2, &committed, |both| {
            let run = |calls: &[Act]| both.run(immediate, calls);
            let all = [
                Act::GetAux(name(b"committed")),
                Act::GetAux(name(b"kept")),
                Act::GetAux(name(b"mine")),
                Act::GetAux(name(b"never")),
                Act::GetMeta(name(b"committed")),
                Act::GetMeta(name(b"kept")),
                Act::GetMeta(name(b"mine")),
                // A Merk keeps what it read of a meta record.
                Act::GetMeta(name(b"mine")),
            ];
            let answers = run(&all);
            assert!(answers[1].starts_with("Ok(Some([1, 1,"), "{}", answers[1]);
            assert_eq!(answers[3..5], ["Ok(None)", "Ok(None)"]);
            assert!(answers[5].starts_with("Ok(Some([3, 3,"), "{}", answers[5]);
            assert_eq!(answers[6..9], ["Ok(Some([]))", "Ok(None)", "Ok(None)"]);

            run(&[
                Act::Aux(vec![
                    aux(b"committed", Op::Delete),
                    aux(b"mine", Op::Put(vec![4; 40], BASIC)),
                    aux(b"never", Op::Delete),
                ]),
                Act::PutMeta(name(b"mine"), vec![5; 5]),
                Act::DeleteMeta(name(b"committed")),
                Act::DeleteMeta(name(b"never")),
                Act::GetMeta(name(b"committed")),
                Act::GetMeta(name(b"mine")),
            ]);
            let answers = run(&all);
            assert_eq!(answers[1], "Ok(None)");
            assert!(answers[3].starts_with("Ok(Some([4, 4,"), "{}", answers[3]);
            assert_eq!(answers[5], "Ok(None)");
            assert_eq!(answers[7], "Ok(Some([5, 5, 5, 5, 5]))");

            // What the transaction itself wrote is removed again.
            run(&[
                Act::Aux(vec![aux(b"mine", Op::Delete)]),
                Act::DeleteMeta(name(b"mine")),
            ]);
            let answers = run(&all);
            assert_eq!(answers[3], "Ok(None)");
            assert_eq!(answers[7], "Ok(None)");
        });
    }
}

#[test]
fn a_cleared_merk_has_no_root_and_is_written_again() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let first: Vec<_> = (0..40).map(|n| basic(n, b"first")).collect();
    let second: Vec<_> = (20..70).map(|n| basic(n, b"second")).collect();
    let mut root = None;
    commit(&writer, &ROOT, &mut root, &first);
    writer.into_scope().merge();
    lignin.commit(&transaction);
    assert_eq!(lignin.root_in_directory(ROOT.prefix), root);

    // The Merk is cleared in one transaction and written again in the
    // next.
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(&writer, ROOT, Some(&staged));
    let mut merk = open_layered(store, root.clone());
    merk.clear().unwrap().expect("the Merk is cleared");
    assert_eq!(merk.root_key(), None);
    drop(merk);
    staged
        .apply(&writer)
        .unwrap()
        .expect("the batch is applied");
    writer.into_scope().merge();
    lignin.commit(&transaction);
    assert_eq!(lignin.root_in_directory(ROOT.prefix), None);

    let transaction = path.begin();
    let reader = LigninNodeStore::read_only(&transaction, ROOT);
    assert!(open_layered(reader, None).is_empty_tree().unwrap());
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let mut root = None;
    commit(&writer, &ROOT, &mut root, &second);
    writer.into_scope().merge();
    lignin.commit(&transaction);
    assert_eq!(lignin.root_in_directory(ROOT.prefix), root);
    let transaction = path.begin();
    let reader = LigninNodeStore::read_only(&transaction, ROOT);
    let reference = MemNodeStore::new();
    apply(&mut open_layered(reference.clone(), None), &second);
    let expected = records(&reference, root.clone());
    assert_same_records(&records(&reader, root), &expected, "written again");
    assert_eq!(walked(&reader), expected);
}

/// Every record a cursor of `store` shows from the first to the last.
fn walked<'db, S: NodeStore<'db>>(store: &S) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut cursor = store.cursor();
    cursor.seek_to_first().unwrap();
    let mut found = Vec::new();
    while cursor.valid().unwrap() {
        let key = cursor.key().unwrap().expect("a key").to_vec();
        found.push((key, cursor.value().unwrap().expect("a record").to_vec()));
        cursor.next().unwrap();
    }
    cursor.status().expect("the walk came to the end");
    found
}

/// Whether `store` has the aux and the meta record that the test below
/// gives each Merk, and how many nodes its cursor shows.
fn side_records<'db, S: NodeStore<'db>>(store: &S) -> (bool, bool, usize) {
    let aux = store.get_aux_record(b"aux").unwrap().unwrap();
    let meta = store.get_meta_record(b"meta").unwrap().unwrap();
    (aux.is_some(), meta.is_some(), walked(store).len())
}

/// D-083: a drop takes every record of every column under the dropped
/// prefix, and under the subtrees below it. A Merk that is cleared loses
/// its nodes and keeps its other records, as on RocksDB.
#[test]
fn a_drop_takes_the_side_records_below_it_and_a_clear_keeps_those_of_its_merk() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let outer = LigninSubtree::alone([1; 32]);
    let hosts = |subtree: [u8; 32], key: u64, hosted: &[[u8; 32]]| LigninHost {
        subtree,
        key: be(key),
        hosted: hosted.to_vec(),
    };
    let (middle_at, inner_at) = ([221; 32], [222; 32]);
    let middle = LigninSubtree {
        prefix: middle_at,
        host: Some(hosts(outer.prefix, 5, &[middle_at])),
    };
    let inner = LigninSubtree {
        prefix: inner_at,
        host: Some(hosts(middle_at, 3, &[inner_at])),
    };
    // Writes `keys` to the Merk of `subtree` and gives it an aux and a
    // meta record, through a deferred handle of `staged`.
    let fill = |staged: &LigninBatch, subtree: &LigninSubtree, keys: std::ops::Range<u64>| {
        let store = LigninNodeStore::deferred(&writer, subtree.clone(), Some(staged));
        let mut merk = open_layered(store, None);
        let nodes: Vec<_> = keys.map(|n| basic(n, &subtree.prefix[..9])).collect();
        let aux = [(
            b"aux".to_vec(),
            Op::Put(subtree.prefix.to_vec(), BASIC),
            None,
        )];
        (merk.apply(&nodes, &aux, None, version()).unwrap()).expect("the Merk is written");
        (merk
            .put_meta(b"meta".to_vec(), subtree.prefix.to_vec())
            .unwrap())
        .expect("a record");
        merk.root_key()
    };
    let side = |subtree: &LigninSubtree| {
        side_records(&LigninNodeStore::deferred(&writer, subtree.clone(), None))
    };
    let apply_staged = |staged: &LigninBatch| {
        (staged.apply(&writer).unwrap()).expect("the batch is applied");
    };

    // Three Merks, one inside the other, each with nodes and with records
    // in the aux and the meta column.
    let staged = LigninBatch::new();
    staged.enter(&middle, b"tree").unwrap();
    staged.enter(&inner, b"tree").unwrap();
    let outer_root = fill(&staged, &outer, 0..12);
    fill(&staged, &middle, 0..8);
    fill(&staged, &inner, 0..6);
    apply_staged(&staged);
    assert_eq!(side(&outer), (true, true, 12));
    assert_eq!(side(&middle), (true, true, 8));
    assert_eq!(side(&inner), (true, true, 6));
    writer.into_scope().merge();
    lignin.commit(&transaction);

    // The outer Merk is cleared, and with the element that hosted it the
    // middle one is dropped. The batch names the middle subtree alone.
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let staged = LigninBatch::new();
    staged.drop_subtree(&LigninSubtree {
        prefix: middle_at,
        host: Some(hosts(outer.prefix, 5, &[])),
    });
    let store = LigninNodeStore::deferred(&writer, outer.clone(), Some(&staged));
    let mut merk = open_layered(store, outer_root);
    merk.clear().unwrap().expect("the Merk is cleared");
    drop(merk);
    let side = |subtree: &LigninSubtree| {
        side_records(&LigninNodeStore::deferred(&writer, subtree.clone(), None))
    };
    (staged.apply(&writer).unwrap()).expect("the batch is applied");
    assert_eq!(side(&outer), (true, true, 0), "a clear takes the nodes");
    assert_eq!(
        side(&middle),
        (false, false, 0),
        "a drop takes every column"
    );
    assert_eq!(side(&inner), (false, false, 0), "and what is below");

    // The middle and the inner subtree are entered again under the prefixes
    // they had. Each starts with nothing: no node and no record of before.
    let staged = LigninBatch::new();
    staged.enter(&middle, b"tree").unwrap();
    staged.enter(&inner, b"tree").unwrap();
    let store = LigninNodeStore::deferred(&writer, outer.clone(), Some(&staged));
    let mut merk = open_layered(store, None);
    apply(&mut merk, &[basic(5, b"a tree again")]);
    drop(merk);
    let store = LigninNodeStore::deferred(&writer, middle.clone(), Some(&staged));
    let mut merk = open_layered(store, None);
    apply(&mut merk, &[basic(3, b"a tree again")]);
    drop(merk);
    (staged.apply(&writer).unwrap()).expect("the batch is applied");
    assert_eq!(side(&middle), (false, false, 1));
    assert_eq!(side(&inner), (false, false, 0));
    writer.into_scope().merge();
    lignin.commit(&transaction);

    // So it is for the next transaction, which reads what was committed.
    let transaction = path.begin();
    let side = |subtree: &LigninSubtree| {
        side_records(&LigninNodeStore::read_only(&transaction, subtree.clone()))
    };
    assert_eq!(side(&outer), (true, true, 1));
    assert_eq!(side(&middle), (false, false, 1));
    assert_eq!(side(&inner), (false, false, 0));
}

/// RD6: a cursor reads the committed nodes of the moment it was opened,
/// whatever is committed afterwards, and the transaction's own writes as
/// they are at each call. Nothing that is only staged is shown.
#[test]
fn a_cursor_keeps_the_state_it_was_opened_on_and_sees_its_transactions_writes() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let mut root = None;
    let committed: Vec<_> = (0..30).map(|n| basic(n * 2, b"committed")).collect();
    commit(&writer, &ROOT, &mut root, &committed);
    writer.into_scope().merge();
    lignin.commit(&transaction);

    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let store = LigninNodeStore::deferred(&writer, ROOT, None);
    let mut cursor = store.cursor();
    cursor.seek(be(10)).unwrap();
    assert_eq!(cursor.key().unwrap(), Some(&be(10)[..]));

    // Another transaction commits a node and removes one.
    let other = path.begin();
    let other_writer = LigninWriter::new(other.scope().unwrap());
    let mut other_root = root.clone();
    let outside = [basic(11, b"outside"), (be(12), Op::Delete)];
    commit(&other_writer, &ROOT, &mut other_root, &outside);
    other_writer.into_scope().merge();
    lignin.commit(&other);
    cursor.next().unwrap();
    assert_eq!(cursor.key().unwrap(), Some(&be(12)[..]), "as it was opened");
    assert!(cursor.value().unwrap().is_some());
    // A cursor that is opened now reads the commit.
    let mut late = store.cursor();
    late.seek(be(11)).unwrap();
    assert_eq!(late.key().unwrap(), Some(&be(11)[..]));
    late.next().unwrap();
    assert_eq!(late.key().unwrap(), Some(&be(14)[..]));
    drop(late);

    // The transaction's own write is staged, and then applied: the cursor
    // sees it once the transaction holds it.
    let staged = LigninBatch::new();
    let writing = LigninNodeStore::deferred(&writer, ROOT, Some(&staged));
    let mut merk = open_layered(writing, other_root);
    apply(&mut merk, &[basic(13, b"mine"), (be(14), Op::Delete)]);
    let root = merk.root_key();
    drop(merk);
    let mut second = store.cursor();
    second.seek(be(13)).unwrap();
    assert_eq!(
        second.key().unwrap(),
        Some(&be(14)[..]),
        "not what is staged"
    );
    staged
        .apply(&writer)
        .unwrap()
        .expect("the batch is applied");
    second.seek(be(13)).unwrap();
    assert_eq!(second.key().unwrap(), Some(&be(13)[..]));
    second.next().unwrap();
    assert_eq!(second.key().unwrap(), Some(&be(16)[..]));
    cursor.next().unwrap();
    assert_eq!(cursor.key().unwrap(), Some(&be(13)[..]), "its own write");
    cursor.next().unwrap();
    assert_eq!(cursor.key().unwrap(), Some(&be(16)[..]), "and its delete");

    // The node a cursor stands on is removed: the cursor stands on its
    // key, shows no record there, and steps on from the key.
    let mut merk = open_layered(LigninNodeStore::immediate(&writer, ROOT), root);
    apply(&mut merk, &[(be(16), Op::Delete)]);
    assert_eq!(cursor.key().unwrap(), Some(&be(16)[..]));
    assert!(cursor.valid().unwrap());
    let nothing = cursor.value();
    assert_eq!(nothing.value, None);
    assert_eq!(nothing.cost, cursor.key().cost, "charged as a key is");
    assert!(cursor.status().is_ok());
    cursor.next().unwrap();
    assert_eq!(cursor.key().unwrap(), Some(&be(18)[..]));
    assert!(format!("{cursor:?}").contains("LigninCursor"));

    // A scan that stands on a key whose node is then removed passes over
    // it, and has come to the end when it ends.
    let mut all_keys = Query::new();
    all_keys.insert_all();
    let mut scan = KVIterator::new(store.cursor(), &all_keys).unwrap();
    let first = scan.next_kv().unwrap().expect("a record").0;
    assert_eq!(first, be(0));
    apply(&mut merk, &[(be(2), Op::Delete)]);
    assert_eq!(scan.next_kv().unwrap().expect("a record").0, be(4));
    let mut scanned = 2;
    while scan.next_kv().unwrap().is_some() {
        scanned += 1;
    }
    assert!(scan.status().is_ok());
    assert_eq!(scanned, walked(&store).len());
}

#[test]
fn a_cursor_moves_to_another_thread_with_its_merk() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let mut root = None;
    let nodes: Vec<_> = (0..50).map(|n| basic(n, b"here")).collect();
    commit(&writer, &ROOT, &mut root, &nodes);
    let store = LigninNodeStore::immediate(&writer, ROOT);
    let mut cursor = store.cursor();
    cursor.seek(be(20)).unwrap();
    let mut merk = open_layered(store, root);
    let (key, shown) = std::thread::scope(|threads| {
        let there = threads.spawn(move || {
            // The Merk writes through the writer while the cursor of its
            // store is open, and the cursor reads what it wrote.
            apply(&mut merk, &[(be(21), Op::Delete)]);
            cursor.next().unwrap();
            let key = cursor.key().unwrap().map(<[u8]>::to_vec);
            (key, cursor.value().unwrap().is_some())
        });
        there.join().expect("the other thread moved the cursor")
    });
    assert_eq!((key, shown), (Some(be(22)), true));
}

/// A transaction whose cursors fail: when they are opened, at a move, or
/// at the read of a node.
struct Failing<'a> {
    under: &'a dyn Access,
    opens: bool,
    /// How many moves a cursor makes before one fails.
    moves: usize,
    reads: bool,
}

struct FailingWalk<'a> {
    under: Box<dyn Walk + 'a>,
    moves: usize,
    reads: bool,
}

impl Walk for FailingWalk<'_> {
    fn go(&mut self, to: Seek<'_>, key: &mut Vec<u8>) -> Result<bool, Error> {
        match self.moves.checked_sub(1) {
            Some(left) => {
                self.moves = left;
                self.under.go(to, key)
            }
            None => {
                self.moves = usize::MAX;
                Err(Error::StorageError(grovedb_storage::Error::StorageError(
                    "the move was refused".to_owned(),
                )))
            }
        }
    }

    fn node(&mut self) -> Result<Option<StoredNode>, Error> {
        match self.reads {
            true => self.under.node(),
            false => Err(Error::CorruptedData("the node was not read".to_owned())),
        }
    }
}

impl Access for Failing<'_> {
    fn cursor<'a>(&'a self, subtree: &Prefix) -> Result<Box<dyn Walk + 'a>, Error> {
        match self.opens {
            true => Ok(Box::new(FailingWalk {
                under: self.under.cursor(subtree)?,
                moves: self.moves,
                reads: self.reads,
            })),
            false => Err(Error::StorageError(grovedb_storage::Error::StorageError(
                "the cursor was refused".to_owned(),
            ))),
        }
    }

    fn node(
        &self,
        subtree: &Prefix,
        key: &[u8],
        hint: Option<Location>,
        with_value: bool,
    ) -> Result<Option<StoredNode>, Error> {
        self.under.node(subtree, key, hint, with_value)
    }

    fn committed_node(&self, subtree: &Prefix, key: &[u8]) -> Result<Option<StoredNode>, Error> {
        self.under.committed_node(subtree, key)
    }

    fn child(&self, subtree: &Prefix, parent: Location, left: bool) -> Result<Child, Error> {
        self.under.child(subtree, parent, left)
    }

    fn record(
        &self,
        keyspace: Keyspace,
        subtree: &Prefix,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, Error> {
        self.under.record(keyspace, subtree, key)
    }

    fn committed_record(
        &self,
        keyspace: Keyspace,
        subtree: &Prefix,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, Error> {
        self.under.committed_record(keyspace, subtree, key)
    }

    fn apply(&self, write: &Write) -> Result<Written, Error> {
        self.under.apply(write)
    }
}

/// A walk that stops on an error is not the end of the nodes: the cursor
/// says so, `clear` removes nothing, and a tree whose nodes could not be
/// read is not empty.
#[test]
fn a_walk_that_fails_is_not_taken_for_the_end_of_the_nodes() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let mut root = None;
    let nodes: Vec<_> = (0..20).map(|n| basic(n, b"kept")).collect();
    commit(&writer, &ROOT, &mut root, &nodes);
    let failing = |opens: bool, moves: usize, reads: bool| Failing {
        under: &writer,
        opens,
        moves,
        reads,
    };
    let all: Vec<Vec<u8>> = (0..20).map(be).collect();
    fn except(keys: &[Vec<u8>]) -> BTreeSet<&[u8]> {
        keys.iter().map(Vec::as_slice).collect()
    }

    for (opens, moves) in [(false, 0), (true, 0), (true, 1), (true, 7), (true, 20)] {
        let access = failing(opens, moves, true);
        let staged = LigninBatch::new();
        let store = LigninNodeStore::new(&access, ROOT, Mode::Deferred(Some(&staged)));

        // The cursor shows the nodes it came to, then none, and an error.
        let mut cursor = store.cursor();
        cursor.seek_to_first().unwrap();
        let mut shown = 0;
        while cursor.valid().unwrap() {
            assert_eq!(cursor.key().unwrap(), Some(&be(shown)[..]));
            shown += 1;
            cursor.next().unwrap();
        }
        assert_eq!(
            shown as usize, moves,
            "every move before the one that failed"
        );
        let status = cursor.status().expect_err("the walk failed");
        let refused = match opens {
            true => "the move was refused",
            false => "the cursor was refused",
        };
        assert!(status.to_string().contains(refused), "{status}");
        assert_eq!(cursor.key().unwrap(), None);
        let nothing = cursor.value();
        assert_eq!((nothing.value, nothing.cost), (None, cursor.valid().cost));
        // A step from no key goes nowhere, and a seek starts again where
        // the cursor could be opened.
        cursor.prev().unwrap();
        assert!(!cursor.valid().unwrap() && cursor.status().is_err());
        cursor.seek_to_last().unwrap();
        assert_eq!(cursor.valid().unwrap(), opens);
        assert_eq!(cursor.status().is_ok(), opens);
        drop(cursor);

        // Merk's walks. A scan gives the records it came to and says that
        // it did not come to the end.
        let mut query = Query::new();
        query.insert_range(be(0)..be(9));
        query.insert_range(be(9)..be(30));
        let mut scan = KVIterator::new(store.cursor(), &query).unwrap();
        let mut scanned = 0;
        while let Some((key, _)) = scan.next_kv().unwrap() {
            assert_eq!(key, be(scanned));
            scanned += 1;
        }
        // The second range is another seek, which is one of the moves.
        let before = moves - usize::from(moves > 10);
        assert_eq!(scanned as usize, before, "every record before the failure");
        let status = scan.status().expect_err("the scan failed");
        assert!(status.to_string().contains(refused), "{status}");
        assert_eq!(scan.next_kv().unwrap(), None, "and it stays ended");
        drop(scan);

        let mut merk = open_layered(store, root.clone());
        assert!(!merk.is_empty_tree_except(except(&all)).unwrap());
        if moves == 0 {
            assert!(!merk.is_empty_tree().unwrap(), "no node was read");
        }
        let cleared = merk.clear().unwrap().expect_err("the walk failed");
        assert!(matches!(cleared, Error::StorageError(_)), "{cleared:?}");
        assert!(cleared.to_string().contains(refused), "{cleared}");
        assert_eq!(merk.root_key(), root, "the Merk has its tree");
        drop(merk);
        assert!(staged.is_empty(), "nothing is removed");
    }
    assert_eq!(
        walked(&LigninNodeStore::deferred(&writer, ROOT, None)).len(),
        20
    );

    // A cursor that walks to the end and whose transaction then reads
    // nodes is not disturbed by the checks above.
    let access = failing(true, usize::MAX - 1, true);
    let store = LigninNodeStore::new(&access, ROOT, Mode::Deferred(None));
    let merk = open_layered(store, root.clone());
    assert!(merk.is_empty_tree_except(except(&all)).unwrap());
    assert!(!merk.is_empty_tree_except(except(&all[1..])).unwrap());
    assert!(!merk.is_empty_tree().unwrap());
    let mut all_keys = Query::new();
    all_keys.insert_all();
    let mut scan = KVIterator::new(merk.storage.cursor(), &all_keys).unwrap();
    let mut scanned = 0;
    while scan.next_kv().unwrap().is_some() {
        scanned += 1;
    }
    assert_eq!((scanned, scan.status().is_ok()), (20, true));
    drop(scan);

    // A record that cannot be read: the cursor stands on its key, shows no
    // value, and its status is the error, until it is sought again.
    let access = failing(true, usize::MAX - 1, false);
    let store = LigninNodeStore::new(&access, ROOT, Mode::Deferred(None));
    let mut cursor = store.cursor();
    cursor.seek(be(4)).unwrap();
    assert!(cursor.status().is_ok());
    assert_eq!(cursor.value().unwrap(), None);
    assert_eq!(cursor.key().unwrap(), Some(&be(4)[..]));
    let status = cursor.status().expect_err("the read failed");
    assert!(
        status.to_string().contains("the node was not read"),
        "{status}"
    );
    cursor.next().unwrap();
    assert_eq!(cursor.key().unwrap(), Some(&be(5)[..]));
    assert!(
        cursor.status().is_err(),
        "a step does not take the error off"
    );
    cursor.seek(be(6)).unwrap();
    assert!(cursor.status().is_ok());
    assert!(format!("{cursor:?}").contains("failure: None"));
    // A scan ends at a record it cannot read, and says so.
    let mut scan = KVIterator::new(store.cursor(), &all_keys).unwrap();
    assert_eq!(scan.next_kv().unwrap(), None);
    let status = scan.status().expect_err("no record was read");
    assert!(
        status.to_string().contains("the node was not read"),
        "{status}"
    );

    // The read of one node through a view is refused with the error, and
    // is no missing node.
    let mut view = store.cursor();
    let read = crate::proofs::query::count_offset::read_value_from_view(&mut view, &be(4));
    assert_eq!(read.cost.seek_count, 1, "the seek is charged");
    let error = read.value.expect_err("the record was not read");
    assert!(matches!(&error, Error::StorageError(_)), "{error:?}");
    assert!(
        error.to_string().contains("the node was not read"),
        "{error}"
    );
    let absent = crate::proofs::query::count_offset::read_value_from_view(&mut view, b"no key");
    assert_eq!(absent.value.expect("a key that is not there"), None);

    // A link whose child's key was not read with the node is no record.
    let mut unread = StoredNode {
        feature: BASIC,
        value_hash: [1; 32],
        kv_hash: [2; 32],
        value_len: 0,
        value: Some(Vec::new()),
        left: None,
        right: None,
        location: None,
    };
    let keys = &mut lignindb::txn::ChildKeys::new();
    assert_eq!(with_child_keys(unread.clone(), keys).unwrap(), unread);
    unread.right = Some(StoredLink {
        hash: [3; 32],
        heights: (0, 0),
        key_len: 3,
        key: None,
        aggregate: AggregateData::NoAggregateData,
    });
    let error = with_child_keys(unread.clone(), keys).unwrap_err();
    assert!(matches!(error, Error::CorruptedState(_)), "{error:?}");
    unread.right.as_mut().unwrap().key = Some(b"abc".to_vec());
    assert_eq!(with_child_keys(unread.clone(), keys).unwrap(), unread);
}

/// A read under a cursor is a read of its scope (`transactions.md` SC3):
/// where another open scope holds the Merk for writing it is refused. The
/// cursor says so, and none of Merk's walks takes the refusal for the end
/// of the nodes.
#[test]
fn a_cursor_that_another_scopes_write_claim_refuses_is_no_empty_merk() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let mut root = None;
    let nodes: Vec<_> = (0..10).map(|n| basic(n, b"committed")).collect();
    commit(&writer, &ROOT, &mut root, &nodes);
    writer.into_scope().merge();
    lignin.commit(&transaction);

    // Two scopes of one transaction. The first opens a cursor over the
    // Merk and reads nothing yet, and the second writes the Merk.
    let transaction = path.begin();
    let first = LigninWriter::new(transaction.scope().unwrap());
    let second = LigninWriter::new(transaction.scope().unwrap());
    let staged = LigninBatch::new();
    let store = LigninNodeStore::deferred(&first, ROOT, Some(&staged));
    let mut cursor = store.cursor();
    let mut theirs = root.clone();
    commit(&second, &ROOT, &mut theirs, &[basic(20, b"theirs")]);

    cursor.seek_to_first().unwrap();
    assert!(!cursor.valid().unwrap());
    let status = cursor.status().expect_err("the other scope holds the Merk");
    let held = "another open scope of the transaction holds";
    assert!(status.to_string().contains(held), "{status}");
    // The handle cannot read the root either, so the Merk is opened as
    // the caller that knows its root key has it: with no node loaded.
    let mut merk = Merk::open_layered_with_root_key(
        store,
        None,
        TreeType::NormalTree,
        NO_VALUE_DEFINED_COST,
        version(),
    )
    .unwrap()
    .expect("a Merk with no root key reads nothing when it is opened");
    assert!(!merk.is_empty_tree().unwrap(), "refused is not empty");
    assert!(!merk.is_empty_tree_except(BTreeSet::new()).unwrap());
    let cleared = merk.clear().unwrap().expect_err("the walk was refused");
    assert!(matches!(cleared, Error::StorageError(_)), "{cleared:?}");
    assert!(staged.is_empty(), "nothing is removed");
    drop(merk);

    // The second scope ends, and the first reads the Merk with what the
    // second wrote: its cursor is sought again and walks to the end.
    second.into_scope().merge();
    cursor.seek_to_first().unwrap();
    assert!(cursor.status().is_ok());
    let mut keys = Vec::new();
    while let Some(key) = cursor.key().unwrap() {
        keys.push(key.to_vec());
        cursor.next().unwrap();
    }
    assert!(cursor.status().is_ok());
    let expected: Vec<_> = (0..10).chain([20]).map(be).collect();
    assert_eq!(keys, expected);
}

/// A handle that only reads has a cursor of its transaction, and that
/// transaction refuses a read beside a scope that is writing the Merk.
#[test]
fn a_cursor_of_a_handle_that_only_reads_is_refused_beside_a_writing_scope() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let reader = LigninNodeStore::read_only(&transaction, ROOT);
    let mut before = reader.cursor();
    before.seek_to_first().unwrap();
    assert!(!before.valid().unwrap() && before.status().is_ok());

    let writer = LigninWriter::new(transaction.scope().unwrap());
    let mut root = None;
    commit(
        &writer,
        &ROOT,
        &mut root,
        &[basic(1, b"one"), basic(2, b"two")],
    );
    before.seek_to_first().unwrap();
    let status = before.status().expect_err("a scope is writing the Merk");
    let held = "another open scope of the transaction holds";
    assert!(status.to_string().contains(held), "{status}");
    let merk = open_layered(LigninNodeStore::read_only(&transaction, ROOT), None);
    assert!(!merk.is_empty_tree().unwrap(), "it could not be read");

    // Once the scope has merged, the transaction reads its writes.
    writer.into_scope().merge();
    before.seek_to_first().unwrap();
    assert_eq!(before.key().unwrap(), Some(&be(1)[..]));
    assert!(before.status().is_ok());
    assert_eq!(walked(&reader).len(), 2);
}
