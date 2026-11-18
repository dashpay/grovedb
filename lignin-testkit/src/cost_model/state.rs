//! The logical state the cost model runs on: for every subtree its four
//! keyspaces of ordered records, the writes of an open transaction over
//! them, and cursors.
//!
//! There is no engine here and no stored node record. A Merk node is kept
//! as its fields ([`LogicalNode`]); everything else is a key and its bytes.
//! Every charge comes from [`super::rules`], from lengths.
//!
//! The visibility rules are the ones the storage seams document
//! (`docs/lignindb/flat-store.md`, `docs/lignindb/transactions.md`): a
//! transaction reads its own applied writes and otherwise the latest
//! committed state, a batch is invisible until it is applied, and a cursor
//! sees the committed state as it was when the cursor was opened.

use std::{
    cell::OnceCell,
    collections::{btree_map, BTreeMap},
    ops::Bound,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use grovedb_costs::{
    error::Error as CostError, storage_cost::key_value_cost::KeyValueStorageCost,
    ChildrenSizesWithIsSumTree, OperationCost,
};

use super::{
    node::LogicalNode,
    rules::{self, hit, CursorMove, Keyspace, Rule},
};

/// The 32 bytes that name a subtree.
pub type Prefix = [u8; 32];

/// The key of a record: its subtree and its key there. Keys order by
/// subtree first, as the stored keys of today's engine do.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct StoredKey {
    /// The subtree.
    pub prefix: Prefix,
    /// The key inside the subtree.
    pub key: Vec<u8>,
}

impl StoredKey {
    /// The key `key` of the subtree `prefix`.
    pub fn new(prefix: Prefix, key: &[u8]) -> Self {
        StoredKey {
            prefix,
            key: key.to_vec(),
        }
    }

    /// The first key any record of the subtree `prefix` can have.
    pub fn first_of(prefix: Prefix) -> Self {
        StoredKey {
            prefix,
            key: Vec::new(),
        }
    }
}

/// The prefix that follows `prefix` in key order, wrapping around after the
/// last one as today's engine does.
pub fn next_prefix(prefix: Prefix) -> Prefix {
    let mut next = prefix;
    for byte in next.iter_mut().rev() {
        *byte = byte.wrapping_add(1);
        if *byte != 0 {
            break;
        }
    }
    next
}

/// One record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    /// A Merk node, as its fields.
    Node(Box<LogicalNode>),
    /// Any other record, as its bytes.
    Bytes(Vec<u8>),
}

impl Record {
    /// The record `bytes` are written as in `keyspace`. In the data
    /// keyspace, bytes that are a node record become the node's fields.
    pub fn new(keyspace: Keyspace, bytes: &[u8]) -> Self {
        let node = (keyspace == Keyspace::Data)
            .then(|| LogicalNode::parse(bytes))
            .flatten();
        match node {
            Some(node) => {
                hit(Rule::NodeKept);
                Record::Node(Box::new(node))
            }
            None => {
                if keyspace == Keyspace::Data {
                    hit(Rule::BytesKept);
                }
                Record::Bytes(bytes.to_vec())
            }
        }
    }

    /// The length today's engine stores the record in. For a node it is
    /// worked out from the sizes of its fields.
    pub fn len(&self) -> u64 {
        match self {
            Record::Node(node) => node.record_len(),
            Record::Bytes(bytes) => bytes.len() as u64,
        }
    }

    /// Whether the record is of no length. A node never is.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The bytes a reader of the record is handed.
    pub fn bytes(&self) -> Vec<u8> {
        match self {
            Record::Node(node) => node.encode(),
            Record::Bytes(bytes) => bytes.clone(),
        }
    }
}

/// One write to the state: a record put under a key, or the key removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Write {
    /// The keyspace written to.
    pub keyspace: Keyspace,
    /// The key.
    pub key: StoredKey,
    /// The record, or `None` to remove the key.
    pub record: Option<Record>,
}

/// The records of every subtree, by keyspace.
#[derive(Clone, Debug, Default)]
pub struct Tables([BTreeMap<StoredKey, Record>; 4]);

impl Tables {
    /// The record under `key`.
    pub fn get(&self, keyspace: Keyspace, key: &StoredKey) -> Option<&Record> {
        self.0[keyspace.index()].get(key)
    }

    fn write(&mut self, write: Write) {
        let table = &mut self.0[write.keyspace.index()];
        match write.record {
            Some(record) => {
                table.insert(write.key, record);
            }
            None => {
                table.remove(&write.key);
            }
        }
    }

    /// How many records `keyspace` holds, over every subtree.
    pub fn len(&self, keyspace: Keyspace) -> usize {
        self.0[keyspace.index()].len()
    }

    /// Whether no keyspace holds a record.
    pub fn is_empty(&self) -> bool {
        self.0.iter().all(BTreeMap::is_empty)
    }

    /// The records of the subtree `prefix` in `keyspace`, in key order.
    pub fn records_of(
        &self,
        keyspace: Keyspace,
        prefix: Prefix,
    ) -> impl Iterator<Item = (&StoredKey, &Record)> {
        self.0[keyspace.index()]
            .range(StoredKey::first_of(prefix)..)
            .take_while(move |(key, _)| key.prefix == prefix)
    }
}

/// The writes a transaction has applied and not committed: for each key it
/// wrote, the record or its removal.
#[derive(Clone, Debug, Default)]
pub struct Overlay([BTreeMap<StoredKey, Option<Record>>; 4]);

impl Overlay {
    fn write(&mut self, write: Write) {
        self.0[write.keyspace.index()].insert(write.key, write.record);
    }

    /// How many keys the transaction has written.
    pub fn len(&self) -> usize {
        self.0.iter().map(BTreeMap::len).sum()
    }

    /// Whether the transaction has written nothing.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn into_writes(self) -> impl Iterator<Item = Write> {
        Keyspace::ALL
            .into_iter()
            .zip(self.0)
            .flat_map(|(keyspace, table)| {
                table.into_iter().map(move |(key, record)| Write {
                    keyspace,
                    key,
                    record,
                })
            })
    }
}

/// What a reader sees: the writes of its transaction, if it has one, over a
/// committed state.
#[derive(Clone, Copy)]
pub struct View<'a> {
    /// The writes of the reader's transaction.
    pub overlay: Option<&'a Overlay>,
    /// The committed state under them.
    pub base: &'a Tables,
}

impl<'a> View<'a> {
    /// The record under `key`.
    pub fn get(&self, keyspace: Keyspace, key: &StoredKey) -> Option<&'a Record> {
        match self
            .overlay
            .and_then(|overlay| overlay.0[keyspace.index()].get(key))
        {
            Some(written) => written.as_ref(),
            None => self.base.get(keyspace, key),
        }
    }

    /// The first record at or after `from` when `forward`, the last at or
    /// before it otherwise, in the order of every subtree's keys.
    pub fn seek(
        &self,
        keyspace: Keyspace,
        from: Bound<&StoredKey>,
        forward: bool,
    ) -> Option<(&'a StoredKey, &'a Record)> {
        let base = &self.base.0[keyspace.index()];
        let overlay = self.overlay.map(|overlay| &overlay.0[keyspace.index()]);
        let mut from = from.cloned();
        loop {
            let bounds = if forward {
                (from.as_ref(), Bound::Unbounded)
            } else {
                (Bound::Unbounded, from.as_ref())
            };
            let committed = edge(base.range::<StoredKey, _>(bounds), forward).map(|(key, _)| key);
            let written =
                overlay.and_then(|overlay| edge(overlay.range::<StoredKey, _>(bounds), forward));
            let written_first = match (written, committed) {
                (Some((written, _)), Some(committed)) => {
                    if forward {
                        written <= committed
                    } else {
                        written >= committed
                    }
                }
                (Some(_), None) => true,
                (None, _) => false,
            };
            if written_first {
                let (key, record) = written?;
                match record {
                    Some(record) => return Some((key, record)),
                    None => from = Bound::Excluded(key.clone()),
                }
            } else {
                let key = committed?;
                return base.get_key_value(key);
            }
        }
    }
}

fn edge<'r, V>(
    mut range: btree_map::Range<'r, StoredKey, V>,
    forward: bool,
) -> Option<(&'r StoredKey, &'r V)> {
    if forward {
        range.next()
    } else {
        range.next_back()
    }
}

/// The committed state, shared by every transaction and cursor.
#[derive(Debug, Default)]
pub struct Database {
    committed: Mutex<Arc<Tables>>,
}

impl Database {
    /// An empty database.
    pub fn new() -> Self {
        Self::default()
    }

    /// A database whose committed state is `tables`: a checkpoint, opened.
    pub fn from_committed(tables: Arc<Tables>) -> Self {
        Database {
            committed: Mutex::new(tables),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Arc<Tables>> {
        self.committed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The committed state as it is now. It stays as it is for the holder
    /// whatever is committed later.
    pub fn committed(&self) -> Arc<Tables> {
        Arc::clone(&self.lock())
    }

    /// Commits `writes`, in order.
    pub fn write(&self, writes: impl IntoIterator<Item = Write>) {
        let mut committed = self.lock();
        let tables = Arc::make_mut(&mut committed);
        for write in writes {
            tables.write(write);
        }
    }

    /// Removes every record of every keyspace.
    pub fn wipe(&self) {
        *self.lock() = Arc::default();
    }

    /// Removes every record of the subtrees `prefixes`, in all four
    /// keyspaces.
    pub fn delete_prefixes(&self, prefixes: &[Prefix]) {
        let mut committed = self.lock();
        let tables = Arc::make_mut(&mut committed);
        for table in &mut tables.0 {
            table.retain(|key, _| !prefixes.contains(&key.prefix));
        }
    }
}

/// An open transaction: what it has written, and the savepoints set in it.
#[derive(Debug, Default)]
pub struct Transaction {
    overlay: Overlay,
    savepoints: Vec<Overlay>,
    pinned: Option<Arc<Tables>>,
}

impl Transaction {
    /// A transaction that reads the latest committed state under its own
    /// writes.
    pub fn new() -> Self {
        Self::default()
    }

    /// A transaction that reads the state `db` has committed now, whatever
    /// is committed later.
    pub fn pinned(db: &Database) -> Self {
        Transaction {
            pinned: Some(db.committed()),
            ..Self::default()
        }
    }

    /// Whether the transaction reads a pinned state.
    pub fn is_pinned(&self) -> bool {
        self.pinned.is_some()
    }

    /// What the transaction has written.
    pub fn overlay(&self) -> &Overlay {
        &self.overlay
    }

    /// The committed state the transaction reads under its writes.
    pub fn base(&self, db: &Database) -> Arc<Tables> {
        match &self.pinned {
            Some(pinned) => Arc::clone(pinned),
            None => db.committed(),
        }
    }

    /// The record the transaction sees under `key`.
    pub fn get(&self, db: &Database, keyspace: Keyspace, key: &StoredKey) -> Option<Record> {
        let base = self.base(db);
        View {
            overlay: Some(&self.overlay),
            base: &base,
        }
        .get(keyspace, key)
        .cloned()
    }

    /// Applies `writes` to the transaction, in order.
    pub fn write(&mut self, writes: impl IntoIterator<Item = Write>) {
        for write in writes {
            self.overlay.write(write);
        }
    }

    /// Commits what the transaction wrote.
    pub fn commit(self, db: &Database) {
        db.write(self.overlay.into_writes());
    }

    /// Discards every write and every savepoint.
    pub fn rollback(&mut self) {
        self.overlay = Overlay::default();
        self.savepoints.clear();
    }

    /// Sets a savepoint.
    pub fn set_savepoint(&mut self) {
        self.savepoints.push(self.overlay.clone());
    }

    /// Discards every write since the newest savepoint and removes that
    /// savepoint. Returns false when there is none.
    pub fn rollback_to_savepoint(&mut self) -> bool {
        match self.savepoints.pop() {
            Some(overlay) => {
                self.overlay = overlay;
                true
            }
            None => false,
        }
    }
}

/// One operation of a batch, as it is staged.
pub enum BatchOp {
    /// A record put under a key.
    Put {
        /// The keyspace.
        keyspace: Keyspace,
        /// The key.
        key: StoredKey,
        /// The bytes of the record.
        value: Vec<u8>,
        /// What a Merk gave with a node: the cost class and length of its
        /// feature, and the link sizes of its children.
        children_sizes: ChildrenSizesWithIsSumTree,
        /// The writer's own account of the cost, if it gave one.
        cost_info: Option<KeyValueStorageCost>,
    },
    /// A key removed.
    Delete {
        /// The keyspace.
        keyspace: Keyspace,
        /// The key.
        key: StoredKey,
        /// The writer's own account of the cost, if it gave one.
        cost_info: Option<KeyValueStorageCost>,
    },
}

/// Charges the operations of a batch, in order, and appends the writes they
/// stand for to `writes`.
///
/// Returns the storage cost, which is owed once the writes are taken, and
/// next to it what is charged at once. When a put is refused the storage
/// cost is not owed and what was charged at once so far stays.
///
/// A delete with no cost is charged from what `db` has committed at this
/// moment.
pub fn charge_batch(
    db: &Database,
    ops: impl IntoIterator<Item = BatchOp>,
    writes: &mut Vec<Write>,
) -> (Result<OperationCost, CostError>, OperationCost) {
    let mut at_once = OperationCost::default();
    let mut on_success = OperationCost::default();
    for op in ops {
        match op {
            BatchOp::Put {
                keyspace,
                key,
                value,
                children_sizes,
                cost_info,
            } => {
                let record = Record::new(keyspace, &value);
                match link_sizes_agree(&record, &children_sizes) {
                    Some(true) => hit(Rule::LinkSizesAgree),
                    Some(false) => hit(Rule::LinkSizesDiffer),
                    None => {}
                }
                match rules::put(
                    keyspace,
                    key.key.len(),
                    record.len(),
                    &children_sizes,
                    cost_info.as_ref(),
                ) {
                    Ok(charge) => charge.add_to(&mut at_once, &mut on_success),
                    Err((error, owed)) => return (Err(error), at_once + owed),
                }
                writes.push(Write {
                    keyspace,
                    key,
                    record: Some(record),
                });
            }
            BatchOp::Delete {
                keyspace,
                key,
                cost_info,
            } => {
                let committed_len = match cost_info {
                    Some(_) => None,
                    None => db.committed().get(keyspace, &key).map(Record::len),
                };
                rules::delete(keyspace, key.key.len(), cost_info, committed_len)
                    .add_to(&mut at_once, &mut on_success);
                writes.push(Write {
                    keyspace,
                    key,
                    record: None,
                });
            }
        }
    }
    (Ok(on_success), at_once)
}

/// Whether the sizes a Merk gave with a node are the ones the node's fields
/// have: the link of each child as its key and 36 bytes next to the length
/// of its aggregate, and the feature as its cost class and length. `None`
/// when the record is not a node or came with no sizes.
pub fn link_sizes_agree(
    record: &Record,
    children_sizes: &ChildrenSizesWithIsSumTree,
) -> Option<bool> {
    let (Record::Node(node), Some((feature, left, right))) = (record, children_sizes) else {
        return None;
    };
    let sizes = node.sizes();
    let link = |child: Option<(usize, u32)>| {
        child.map(|(key_len, aggregate_len)| (key_len as u32 + 36, aggregate_len))
    };
    let given_feature = feature
        .as_ref()
        .map(|(cost_type, len)| (rules::feature_cost_size(cost_type), *len));
    let own_feature = node
        .feature
        .cost_class()
        .map(|class| (class.size(), sizes.feature_payload_len));
    Some(
        given_feature == own_feature
            && *left == link(sizes.children[0])
            && *right == link(sizes.children[1]),
    )
}

struct Position {
    key: StoredKey,
    record: Record,
    bytes: OnceCell<Vec<u8>>,
}

/// An ordered cursor over one keyspace.
///
/// It walks the keys of every subtree, as a cursor of today's engine does.
/// Each call names the subtree it is used for, and the cursor is on a
/// record only while it stands inside that subtree. It sees the committed
/// state of the moment it was opened, under the writes its transaction has
/// applied by the time of each move.
pub struct Cursor {
    keyspace: Keyspace,
    base: Arc<Tables>,
    position: Option<Position>,
}

impl Cursor {
    /// An unpositioned cursor over `keyspace`, on the committed state
    /// `base`.
    pub fn new(keyspace: Keyspace, base: Arc<Tables>) -> Self {
        Cursor {
            keyspace,
            base,
            position: None,
        }
    }

    fn go(&mut self, overlay: &Overlay, from: Bound<&StoredKey>, forward: bool) {
        let view = View {
            overlay: Some(overlay),
            base: &self.base,
        };
        self.position = view
            .seek(self.keyspace, from, forward)
            .map(|(key, record)| Position {
                key: key.clone(),
                record: record.clone(),
                bytes: OnceCell::new(),
            });
    }

    /// Moves the cursor within the subtree `prefix`. `key` is the key a
    /// seek goes to, and is not used by the other moves. `overlay` is what
    /// the cursor's transaction has written.
    pub fn move_to(
        &mut self,
        prefix: Prefix,
        movement: CursorMove,
        key: &[u8],
        overlay: &Overlay,
    ) -> OperationCost {
        match movement {
            CursorMove::SeekToFirst => {
                let from = StoredKey::first_of(prefix);
                self.go(overlay, Bound::Included(&from), true);
            }
            CursorMove::SeekToLast => {
                let from = StoredKey::first_of(next_prefix(prefix));
                self.go(overlay, Bound::Included(&from), false);
            }
            CursorMove::Seek => {
                let from = StoredKey::new(prefix, key);
                self.go(overlay, Bound::Included(&from), true);
            }
            CursorMove::SeekForPrev => {
                let from = StoredKey::new(prefix, key);
                self.go(overlay, Bound::Included(&from), false);
            }
            CursorMove::Next | CursorMove::Prev => {
                if let Some(position) = self.position.take() {
                    self.go(
                        overlay,
                        Bound::Excluded(&position.key),
                        movement == CursorMove::Next,
                    );
                }
            }
        }
        rules::cursor_move(movement)
    }

    fn on_record(&self, prefix: Prefix) -> Option<&Position> {
        self.position
            .as_ref()
            .filter(|position| position.key.prefix == prefix)
    }

    /// Whether the cursor is on a record of the subtree `prefix`.
    pub fn valid(&self, prefix: Prefix) -> (bool, OperationCost) {
        let (key, cost) = self.key(prefix);
        (key.is_some(), cost)
    }

    /// The key of the record the cursor is on, without the prefix, when it
    /// is a record of the subtree `prefix`.
    pub fn key(&self, prefix: Prefix) -> (Option<&[u8]>, OperationCost) {
        let on_record = self.on_record(prefix);
        (
            on_record.map(|position| position.key.key.as_slice()),
            rules::cursor_key(on_record.map(|position| position.key.key.len())),
        )
    }

    /// The bytes of the record the cursor is on, when it is a record of the
    /// subtree `prefix`.
    pub fn value(&self, prefix: Prefix) -> (Option<&[u8]>, OperationCost) {
        let on_record = self.on_record(prefix);
        (
            on_record.map(|position| {
                position
                    .bytes
                    .get_or_init(|| position.record.bytes())
                    .as_slice()
            }),
            rules::cursor_value(
                on_record.map(|position| (position.key.key.len(), position.record.len())),
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Prefix = [1; 32];
    const B: Prefix = [2; 32];

    fn put(keyspace: Keyspace, prefix: Prefix, key: &[u8], value: &[u8]) -> Write {
        Write {
            keyspace,
            key: StoredKey::new(prefix, key),
            record: Some(Record::new(keyspace, value)),
        }
    }

    fn remove(keyspace: Keyspace, prefix: Prefix, key: &[u8]) -> Write {
        Write {
            keyspace,
            key: StoredKey::new(prefix, key),
            record: None,
        }
    }

    fn walk(
        cursor: &mut Cursor,
        prefix: Prefix,
        overlay: &Overlay,
        forward: bool,
    ) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut seen = Vec::new();
        while cursor.valid(prefix).0 {
            let key = cursor.key(prefix).0.expect("a key").to_vec();
            let value = cursor.value(prefix).0.expect("a value").to_vec();
            seen.push((key, value));
            let movement = if forward {
                CursorMove::Next
            } else {
                CursorMove::Prev
            };
            cursor.move_to(prefix, movement, &[], overlay);
        }
        seen
    }

    fn pair(key: &[u8], value: &[u8]) -> (Vec<u8>, Vec<u8>) {
        (key.to_vec(), value.to_vec())
    }

    #[test]
    fn the_next_prefix_carries_and_wraps() {
        let mut low = [0; 32];
        low[31] = 0xFF;
        let mut carried = [0; 32];
        carried[30] = 1;
        assert_eq!(next_prefix(low), carried);
        assert_eq!(next_prefix([0xFF; 32]), [0; 32]);
        assert_eq!(next_prefix(A)[31], 2);
    }

    #[test]
    fn a_node_record_is_kept_as_fields_and_reads_back_the_same() {
        let node = LogicalNode {
            left: None,
            right: None,
            feature: super::super::node::Aggregate::Count(9),
            kv_hash: [3; 32],
            value_hash: [4; 32],
            value: b"element".to_vec(),
        };
        let bytes = node.encode();
        let record = Record::new(Keyspace::Data, &bytes);
        assert_eq!(record, Record::Node(Box::new(node)));
        assert_eq!(record.len(), bytes.len() as u64);
        assert_eq!(record.bytes(), bytes);
        assert!(!record.is_empty());

        let elsewhere = Record::new(Keyspace::Aux, &bytes);
        assert_eq!(elsewhere, Record::Bytes(bytes));
        let flat = Record::new(Keyspace::Data, b"not a node");
        assert_eq!(flat.len(), 10);
        assert!(Record::new(Keyspace::Data, b"").is_empty());
    }

    #[test]
    fn a_transaction_reads_its_writes_over_the_latest_committed_state() {
        let db = Database::new();
        db.write([put(Keyspace::Data, A, b"k", b"old")]);
        let mut tx = Transaction::new();
        let key = StoredKey::new(A, b"k");
        assert_eq!(
            tx.get(&db, Keyspace::Data, &key),
            Some(Record::Bytes(b"old".to_vec()))
        );

        db.write([put(Keyspace::Data, A, b"k", b"newer")]);
        assert_eq!(
            tx.get(&db, Keyspace::Data, &key),
            Some(Record::Bytes(b"newer".to_vec()))
        );

        tx.write([put(Keyspace::Data, A, b"k", b"mine")]);
        assert_eq!(
            tx.get(&db, Keyspace::Data, &key),
            Some(Record::Bytes(b"mine".to_vec()))
        );
        assert_eq!(tx.get(&db, Keyspace::Aux, &key), None);
        tx.write([remove(Keyspace::Data, A, b"k")]);
        assert_eq!(tx.get(&db, Keyspace::Data, &key), None);
        assert_eq!(tx.overlay().len(), 1);
        assert!(!tx.overlay().is_empty());
        assert!(!tx.is_pinned());

        tx.commit(&db);
        assert_eq!(db.committed().get(Keyspace::Data, &key), None);
        assert!(db.committed().is_empty());
    }

    #[test]
    fn a_pinned_transaction_keeps_the_state_it_started_on() {
        let db = Database::new();
        db.write([put(Keyspace::Meta, A, b"k", b"old")]);
        let tx = Transaction::pinned(&db);
        db.write([put(Keyspace::Meta, A, b"k", b"new")]);
        assert!(tx.is_pinned());
        assert_eq!(
            tx.get(&db, Keyspace::Meta, &StoredKey::new(A, b"k")),
            Some(Record::Bytes(b"old".to_vec()))
        );
    }

    #[test]
    fn savepoints_and_rollback_undo_writes() {
        let db = Database::new();
        let mut tx = Transaction::new();
        let get = |tx: &Transaction, key: &[u8]| {
            tx.get(&db, Keyspace::Data, &StoredKey::new(A, key))
                .map(|record| record.bytes())
        };
        assert!(!tx.rollback_to_savepoint());
        tx.write([put(Keyspace::Data, A, b"1", b"one")]);
        tx.set_savepoint();
        tx.write([put(Keyspace::Data, A, b"2", b"two")]);
        tx.set_savepoint();
        tx.write([remove(Keyspace::Data, A, b"1")]);
        assert_eq!(get(&tx, b"1"), None);

        assert!(tx.rollback_to_savepoint());
        assert_eq!(get(&tx, b"1"), Some(b"one".to_vec()));
        assert_eq!(get(&tx, b"2"), Some(b"two".to_vec()));
        assert!(tx.rollback_to_savepoint());
        assert_eq!(get(&tx, b"2"), None);
        assert!(!tx.rollback_to_savepoint());

        tx.set_savepoint();
        tx.rollback();
        assert!(tx.overlay().is_empty());
        assert!(!tx.rollback_to_savepoint());
    }

    #[test]
    fn a_cursor_walks_its_subtree_in_key_order_both_ways() {
        let db = Database::new();
        db.write([
            put(Keyspace::Data, A, b"a", b"1"),
            put(Keyspace::Data, A, b"c", b"3"),
            put(Keyspace::Data, B, b"a", b"other subtree"),
            put(Keyspace::Aux, A, b"b", b"other keyspace"),
        ]);
        let mut tx = Transaction::new();
        tx.write([
            put(Keyspace::Data, A, b"b", b"2"),
            remove(Keyspace::Data, A, b"c"),
            put(Keyspace::Data, A, b"d", b"4"),
            remove(Keyspace::Data, A, b"zz"),
        ]);
        let overlay = tx.overlay();
        let mut cursor = Cursor::new(Keyspace::Data, tx.base(&db));
        assert!(!cursor.valid(A).0);

        cursor.move_to(A, CursorMove::SeekToFirst, &[], overlay);
        assert_eq!(
            walk(&mut cursor, A, overlay, true),
            [pair(b"a", b"1"), pair(b"b", b"2"), pair(b"d", b"4")]
        );
        assert_eq!(cursor.key(A).0, None);
        assert_eq!(cursor.value(A).0, None);

        cursor.move_to(A, CursorMove::SeekToLast, &[], overlay);
        assert_eq!(
            walk(&mut cursor, A, overlay, false),
            [pair(b"d", b"4"), pair(b"b", b"2"), pair(b"a", b"1")]
        );

        cursor.move_to(A, CursorMove::Seek, b"bb", overlay);
        assert_eq!(cursor.key(A).0, Some(b"d".as_slice()));
        cursor.move_to(A, CursorMove::SeekForPrev, b"c", overlay);
        assert_eq!(cursor.key(A).0, Some(b"b".as_slice()));
        cursor.move_to(A, CursorMove::Seek, b"e", overlay);
        assert!(!cursor.valid(A).0);
        cursor.move_to(A, CursorMove::Prev, &[], overlay);
        assert_eq!(cursor.key(A).0, Some(b"d".as_slice()));

        let mut aux = Cursor::new(Keyspace::Aux, tx.base(&db));
        aux.move_to(A, CursorMove::SeekToFirst, &[], overlay);
        assert_eq!(
            walk(&mut aux, A, overlay, true),
            [pair(b"b", b"other keyspace")]
        );
    }

    #[test]
    fn a_cursor_charges_by_where_it_stands() {
        let db = Database::new();
        db.write([put(Keyspace::Data, A, b"key", b"value")]);
        let overlay = Overlay::default();
        let mut cursor = Cursor::new(Keyspace::Data, db.committed());
        assert_eq!(
            cursor.move_to(A, CursorMove::SeekToFirst, &[], &overlay),
            OperationCost::with_seek_count(1)
        );
        assert_eq!(
            cursor.valid(A).1,
            OperationCost::with_storage_loaded_bytes(35)
        );
        assert_eq!(
            cursor.key(A).1,
            OperationCost::with_storage_loaded_bytes(35)
        );
        assert_eq!(
            cursor.value(A).1,
            OperationCost::with_storage_loaded_bytes(40)
        );
        cursor.move_to(A, CursorMove::Next, &[], &overlay);
        for cost in [cursor.valid(A).1, cursor.key(A).1, cursor.value(A).1] {
            assert_eq!(cost, OperationCost::with_storage_loaded_bytes(288));
        }
        assert_eq!(
            cursor.move_to(A, CursorMove::Next, &[], &overlay),
            OperationCost::with_seek_count(1)
        );
        assert!(!cursor.valid(A).0);
    }

    #[test]
    fn a_cursor_keeps_the_committed_state_it_was_opened_on() {
        let db = Database::new();
        db.write([put(Keyspace::Data, A, b"a", b"1")]);
        let overlay = Overlay::default();
        let mut cursor = Cursor::new(Keyspace::Data, db.committed());
        db.write([
            put(Keyspace::Data, A, b"b", b"2"),
            put(Keyspace::Data, B, b"z", b"later"),
        ]);
        cursor.move_to(A, CursorMove::SeekToFirst, &[], &overlay);
        assert_eq!(walk(&mut cursor, A, &overlay, true), [pair(b"a", b"1")]);
        cursor.move_to(B, CursorMove::SeekToFirst, &[], &overlay);
        assert!(!cursor.valid(B).0);
    }

    #[test]
    fn a_cursor_is_on_a_record_only_in_the_subtree_it_is_used_for() {
        let db = Database::new();
        db.write([
            put(Keyspace::Data, A, b"a", b"1"),
            put(Keyspace::Data, B, b"b", b"2"),
        ]);
        let overlay = Overlay::default();
        let mut cursor = Cursor::new(Keyspace::Data, db.committed());
        cursor.move_to(B, CursorMove::SeekForPrev, b"a", &overlay);
        assert!(!cursor.valid(B).0);
        assert_eq!(cursor.key(A).0, Some(b"a".as_slice()));
        cursor.move_to(B, CursorMove::Next, &[], &overlay);
        assert_eq!(cursor.key(B).0, Some(b"b".as_slice()));
        assert_eq!(cursor.value(B).0, Some(b"2".as_slice()));
        assert_eq!(cursor.value(A).0, None);
    }

    #[test]
    fn the_database_drops_subtrees_and_everything() {
        let db = Database::new();
        for keyspace in Keyspace::ALL {
            db.write([put(keyspace, A, b"k", b"a"), put(keyspace, B, b"k", b"b")]);
        }
        let before = db.committed();
        db.delete_prefixes(&[A]);
        let after = db.committed();
        for keyspace in Keyspace::ALL {
            assert_eq!(before.len(keyspace), 2);
            assert_eq!(after.len(keyspace), 1);
            assert_eq!(after.records_of(keyspace, A).count(), 0);
            assert_eq!(after.records_of(keyspace, B).count(), 1);
        }
        db.wipe();
        assert!(db.committed().is_empty());
        assert!(!before.is_empty());

        let checkpoint = Database::from_committed(before);
        checkpoint.write([remove(Keyspace::Data, A, b"k")]);
        assert_eq!(checkpoint.committed().len(Keyspace::Data), 1);
        assert!(db.committed().is_empty());
    }

    #[test]
    fn a_batch_is_charged_write_by_write() {
        let db = Database::new();
        db.write([put(Keyspace::Data, A, b"committed", b"12345")]);
        let ops = vec![
            BatchOp::Put {
                keyspace: Keyspace::Aux,
                key: StoredKey::new(A, b"k"),
                value: b"value".to_vec(),
                children_sizes: None,
                cost_info: None,
            },
            BatchOp::Delete {
                keyspace: Keyspace::Data,
                key: StoredKey::new(A, b"committed"),
                cost_info: None,
            },
            BatchOp::Delete {
                keyspace: Keyspace::Data,
                key: StoredKey::new(A, b"absent"),
                cost_info: Some(KeyValueStorageCost::default()),
            },
        ];
        let mut writes = Vec::new();
        let (on_success, at_once) = charge_batch(&db, ops, &mut writes);
        let on_success = on_success.expect("charged");
        assert_eq!(
            at_once,
            OperationCost {
                seek_count: 1 + 2 + 1,
                storage_loaded_bytes: 5,
                ..Default::default()
            }
        );
        assert_eq!(writes.len(), 3);
        assert_eq!(on_success.storage_cost.added_bytes, 33 + 1 + 5 + 1);
        assert_eq!(
            on_success.storage_cost.removed_bytes.total_removed_bytes(),
            41 + 5 + 1 + 1
        );
    }

    #[test]
    fn a_refused_put_stops_the_batch_and_keeps_the_seeks() {
        let db = Database::new();
        let wrong = KeyValueStorageCost {
            needs_value_verification: true,
            ..Default::default()
        };
        let put_meta = |key: &[u8], cost_info| BatchOp::Put {
            keyspace: Keyspace::Meta,
            key: StoredKey::new(A, key),
            value: b"v".to_vec(),
            children_sizes: None,
            cost_info,
        };
        let ops = vec![
            put_meta(b"1", None),
            put_meta(b"2", Some(wrong)),
            put_meta(b"3", None),
        ];
        let mut writes = Vec::new();
        let (result, at_once) = charge_batch(&db, ops, &mut writes);
        assert!(matches!(
            result,
            Err(CostError::StorageCostMismatch {
                actual_total_bytes: 2,
                ..
            })
        ));
        assert_eq!(at_once, OperationCost::with_seek_count(2));
        assert_eq!(writes.len(), 1);
    }

    #[test]
    fn the_sizes_given_with_a_node_are_compared_with_its_fields() {
        use grovedb_costs::TreeCostType;

        use super::super::node::{Aggregate, LogicalLink};

        let node = Record::Node(Box::new(LogicalNode {
            left: Some(LogicalLink {
                key: b"left".to_vec(),
                hash: [0; 32],
                left_height: 0,
                right_height: 0,
                aggregate: Aggregate::Sum(1000),
            }),
            right: None,
            feature: Aggregate::Sum(5),
            kv_hash: [0; 32],
            value_hash: [0; 32],
            value: Vec::new(),
        }));
        let sizes = |feature_len, left_key_len| {
            Some((
                Some((TreeCostType::TreeFeatureUsesVarIntCostAs8Bytes, feature_len)),
                Some((left_key_len, 2)),
                None,
            ))
        };
        assert_eq!(link_sizes_agree(&node, &sizes(1, 40)), Some(true));
        assert_eq!(link_sizes_agree(&node, &sizes(2, 40)), Some(false));
        assert_eq!(link_sizes_agree(&node, &sizes(1, 41)), Some(false));
        assert_eq!(link_sizes_agree(&node, &None), None);
        assert_eq!(
            link_sizes_agree(&Record::Bytes(Vec::new()), &sizes(1, 40)),
            None
        );
    }
}
