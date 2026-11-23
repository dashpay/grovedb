//! The dump of one subtree to a stream, and the load of such a stream into
//! a subtree that is empty, on any engine.
//!
//! These are the hatches that take the place of `GroveDb::raw_storage` and
//! `GroveDb::ingest_subtree_sst`, which the RocksDB engine alone has: a
//! snapshot of one subtree is baked on one grove and applied to another,
//! whatever engine each is on. `docs/lignindb/engine.md`, "Dump and load",
//! has the contract and what a dump does not carry.
//!
//! An apply is three calls on the grove that takes the subtree, which a
//! caller makes in one transaction:
//!
//! 1. [`GroveDb::load_subtree`] writes the records of the dump into the
//!    subtree under an element whose subtree holds nothing.
//! 2. [`GroveDb::subtree_root_as`] gives the root that those records have
//!    when they are read as the subtree of the dumped element. The caller
//!    compares it with the root the dump states
//!    ([`DumpedSubtree::root`]).
//! 3. [`GroveDb::replace_subtree_root`] writes the dumped element and that
//!    root into the parent Merk and carries the change to the root of the
//!    grove.
//!
//! A load makes no hash of a node and does not ask whether the keys of a
//! Merk stand as the keys of a search tree do: it writes the records the
//! stream holds. Between the load and the replacement the element in the
//! parent does not commit to what its subtree holds: `verify_grovedb`
//! reports such a grove, and a LigninDB database whose loaded Merk has
//! nodes does not pass the engine's physical check. A caller that gives
//! the three calls one transaction commits no such state. Gated behind
//! the `unsafe-dump-load` Cargo feature.

mod format;
#[cfg(all(test, grovedb_test_engine))]
mod tests;

use std::io::{Read, Write};

use grovedb_merk::{
    element::{
        costs::ElementCostExtensions, get::ElementFetchFromStorageExtensions,
        tree_type::ElementTreeTypeExtensions,
    },
    proofs::chunk::chunk::{LEFT, RIGHT},
    store::{NodeChange, NodeChangeSet, NodeStore},
    tree::{kv::ValueDefinedCostType, TreeNode},
    Merk,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{FlatStore, RawIterator};
use grovedb_version::version::GroveVersion;

use self::format::{DumpReader, DumpWriter, Keyspace};
use crate::{
    engine::SubtreeHandle, util::TxRef, Element, Engine, Error, GroveDb, Transaction,
    TransactionArg,
};

/// Why a dump or a load did not happen.
#[derive(Debug, thiserror::Error)]
pub enum DumpLoadError {
    /// The stream could not be written or read. A stream that ends inside
    /// a dump is this, with the kind `UnexpectedEof`.
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    /// The stream is not a dump that this version reads: its magic, its
    /// version, the element it names, its records, their order, their
    /// count or its checksum; a node record is not as Merk encodes the
    /// node; the nodes are not one tree; or the element does not name
    /// what the records hold.
    #[error("malformed dump: {0}")]
    Malformed(&'static str),
    /// The subtree is not one that is dumped, or does not take the dump.
    #[error("refused: {0}")]
    Refused(&'static str),
    /// An error of the grove.
    #[error(transparent)]
    Grove(#[from] Error),
}

/// What a dump wrote, and what a load read.
#[derive(Clone, Debug, PartialEq)]
pub struct DumpedSubtree {
    /// The element that hosted the subtree when it was dumped.
    pub element: Element,
    /// The root that the records of the dumped subtree gave, read as the
    /// subtree of that element ([`GroveDb::subtree_root_as`]).
    pub root: [u8; 32],
    /// The records of the data keyspace: Merk nodes, or flat records.
    pub data_records: u64,
    /// The records of the aux keyspace.
    pub aux_records: u64,
    /// One, where the Merk keeps a record of its root key.
    pub root_key_records: u64,
    /// The records of the meta keyspace.
    pub meta_records: u64,
    /// The length of the stream.
    pub bytes: u64,
}

impl DumpedSubtree {
    fn new(element: Element, root: [u8; 32], counts: format::Counts) -> Self {
        let [data_records, aux_records, root_key_records, meta_records] = counts.records;
        Self {
            element,
            root,
            data_records,
            aux_records,
            root_key_records,
            meta_records,
            bytes: counts.bytes,
        }
    }
}

fn storage(error: grovedb_storage::Error) -> DumpLoadError {
    Error::from(error).into()
}

fn merk(error: grovedb_merk::Error) -> DumpLoadError {
    crate::engine::node_store_error(error).into()
}

/// Refuses an element whose subtree is not one that a dump carries.
fn carried(element: &Element) -> Result<(), DumpLoadError> {
    match element.tree_type() {
        None => Err(DumpLoadError::Refused("the element is no tree")),
        Some(tree) if tree.is_indexed_primary() => Err(DumpLoadError::Refused(
            "an indexed tree hosts several Merks, and a dump carries one subtree",
        )),
        Some(_) => Ok(()),
    }
}

/// The key of the root node that the element of a tree of nodes names.
fn root_key_of(element: &Element) -> Option<Vec<u8>> {
    (element.clone().root_key_and_tree_type_owned()).and_then(|(root_key, _)| root_key)
}

/// Whether the keyspace of `cursor` holds a record.
fn holds_a_record<C: RawIterator>(mut cursor: C) -> Result<bool, DumpLoadError> {
    cursor.seek_to_first().unwrap();
    let holds = cursor.valid().unwrap();
    cursor.status().map_err(storage)?;
    Ok(holds)
}

/// Writes every record of the keyspace of `cursor`.
fn dump_keyspace<C: RawIterator, W: Write>(
    mut cursor: C,
    keyspace: Keyspace,
    out: &mut DumpWriter<W>,
) -> Result<(), DumpLoadError> {
    cursor.seek_to_first().unwrap();
    while cursor.valid().unwrap() {
        let (Some(key), Some(value)) = (cursor.key().unwrap(), cursor.value().unwrap()) else {
            return Err(Error::CorruptedData(
                "a cursor at a record returned no key or value".to_owned(),
            )
            .into());
        };
        out.record(keyspace, key, value)?;
        cursor.next().unwrap();
    }
    cursor.status().map_err(storage)
}

/// The node records of one Merk, in the order of their keys, as the nodes
/// they encode: the place of the root among them and the places of all of
/// them, a child before its parent.
///
/// It holds each record to being what Merk encodes of the node it
/// decodes to, and the records to being one tree: every link names a node
/// of them, no node is named twice, and one node is named by none. It
/// makes no hash, and does not ask whether the keys stand as the keys of a
/// search tree do.
fn whole_merk(
    records: Vec<(Vec<u8>, Vec<u8>)>,
    grove_version: &GroveVersion,
) -> Result<(Vec<TreeNode>, usize, Vec<usize>), DumpLoadError> {
    const NOT_ONE_TREE: DumpLoadError =
        DumpLoadError::Malformed("the nodes of the dump are not one tree");
    let mut nodes = Vec::with_capacity(records.len());
    for (key, record) in records {
        let node = TreeNode::decode_raw(
            &record,
            key,
            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            grove_version,
        )
        .map_err(|_| DumpLoadError::Malformed("a record of the data keyspace is no node"))?;
        // A record that decodes and is encoded otherwise would be written
        // as other bytes than the dump holds.
        if node.encode() != record {
            return Err(DumpLoadError::Malformed(
                "a node record is not as Merk encodes its node",
            ));
        }
        nodes.push(node);
    }
    let mut children = Vec::with_capacity(nodes.len());
    let mut named = vec![false; nodes.len()];
    for node in &nodes {
        let mut sides = [None, None];
        for (side, child) in [LEFT, RIGHT].into_iter().zip(&mut sides) {
            let Some(link) = node.link(side) else {
                continue;
            };
            let at = nodes
                .binary_search_by(|other| other.key().cmp(link.key()))
                .map_err(|_| DumpLoadError::Malformed("a link names no node of the dump"))?;
            if std::mem::replace(&mut named[at], true) {
                return Err(DumpLoadError::Malformed("two links name one node"));
            }
            *child = Some(at);
        }
        children.push(sides);
    }
    let mut roots = (0..nodes.len()).filter(|at| !named[*at]);
    let (Some(root), None) = (roots.next(), roots.next()) else {
        return Err(NOT_ONE_TREE);
    };
    let mut order = Vec::with_capacity(nodes.len());
    let mut walk = vec![(root, 0)];
    while let Some((at, side)) = walk.pop() {
        if side == 2 {
            order.push(at);
            continue;
        }
        walk.push((at, side + 1));
        if let Some(child) = children[at][side] {
            walk.push((child, 0));
        }
    }
    if order.len() != nodes.len() {
        return Err(NOT_ONE_TREE);
    }
    Ok((nodes, root, order))
}

impl<E: Engine> GroveDb<E> {
    /// The element under `key` in the Merk at `path`, which has to be a
    /// tree whose subtree a dump carries.
    fn dumped_element<'db, 'b, B: AsRef<[u8]> + 'b>(
        &'db self,
        path: SubtreePath<'b, B>,
        key: &[u8],
        transaction: &Transaction<'db, E>,
        grove_version: &GroveVersion,
    ) -> Result<Element, DumpLoadError> {
        let parent = self
            .open_transactional_merk_at_path(path, transaction, None, grove_version)
            .unwrap()?;
        let element = Element::get(&parent, key, true, grove_version)
            .unwrap()
            .map_err(Error::MerkError)?;
        carried(&element)?;
        Ok(element)
    }

    /// The root that the records of the subtree under `key` give when they
    /// are read as the subtree of `element`: the root hash of the Merk
    /// that `element` names the root key and the tree type of, or the
    /// state root of the append-only tree that `element` is, as
    /// `verify_grovedb` makes it. The element that the grove holds under
    /// `key` is not read. Where the element names a root key that no node
    /// has, or the root node is not a node of a tree of that type, the
    /// call is an error.
    ///
    /// It is what [`GroveDb::replace_subtree_root`] takes as the root of a
    /// subtree that was loaded, and what a caller compares with the root a
    /// dump states. It reads the records the root is made from, which are
    /// not all the records of the subtree: the root node of a Merk, the
    /// stored peaks of an MMR and not the leaves under them. It is no
    /// check of every record.
    pub fn subtree_root_as<'db, 'b, B, P>(
        &'db self,
        path: P,
        key: &[u8],
        element: &Element,
        transaction: TransactionArg<'db, '_, E>,
        grove_version: &GroveVersion,
    ) -> Result<[u8; 32], Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let path: SubtreePath<B> = path.into();
        let tx = TxRef::for_read(&self.db, transaction);
        let subtree = path.derive_owned_with_child(key);
        self.root_as((&subtree).into(), element, tx.as_ref(), grove_version)
    }

    fn root_as<'db, 'b, B: AsRef<[u8]> + 'b>(
        &'db self,
        subtree: SubtreePath<'b, B>,
        element: &Element,
        transaction: &Transaction<'db, E>,
        grove_version: &GroveVersion,
    ) -> Result<[u8; 32], Error> {
        if element.uses_non_merk_data_storage() {
            return self.compute_non_merk_state_root(element, subtree, transaction, grove_version);
        }
        let Some((root_key, tree_type)) = element.clone().root_key_and_tree_type_owned() else {
            return Err(Error::InvalidInput(
                "subtree_root_as: element is not a tree variant",
            ));
        };
        if tree_type.is_indexed_primary() {
            return Err(Error::NotSupported(
                "the root of an indexed tree, which hosts several Merks".to_owned(),
            ));
        }
        let handle = self.db.subtree(subtree, None, transaction).unwrap();
        let named = root_key.is_some();
        let merk = Merk::open_layered_with_root_key(
            handle,
            root_key,
            tree_type,
            Some(&Element::value_defined_cost_for_serialized_value),
            grove_version,
        )
        .unwrap()
        .map_err(Error::MerkError)?;
        if named && !merk.has_root_key() {
            return Err(Error::CorruptedData(
                "the element names a root key that no node of the subtree has".to_owned(),
            ));
        }
        merk.try_root_hash().unwrap().map_err(Error::MerkError)
    }

    /// Writes the records of the subtree under `key` in the Merk at `path`
    /// to `out`: its Merk nodes or its flat records, its aux records, the
    /// record of its root key and its meta records, with the element that
    /// hosts it and the root its records give.
    ///
    /// With no transaction the dump reads through a snapshot read
    /// transaction of its own: the state that was committed when it began.
    /// Through `transaction` it reads as that transaction does. It makes
    /// many small writes, so `out` should be buffered. It is flushed at
    /// the end.
    ///
    /// What is under the subtree is not carried: the subtrees of the
    /// trees that its Merk holds, and the secondary Merks of an indexed
    /// tree, which is refused. A subtree whose element does not name what
    /// it holds is refused as well: a tree of nodes whose element names
    /// no root key or the key of no node, and an append-only tree that
    /// holds records under an element that counts none. That is a subtree
    /// between its load and the replacement of its element.
    pub fn dump_subtree<'db, 'b, B, P, W>(
        &'db self,
        path: P,
        key: &[u8],
        out: W,
        transaction: TransactionArg<'db, '_, E>,
        grove_version: &GroveVersion,
    ) -> Result<DumpedSubtree, DumpLoadError>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
        W: Write,
    {
        let path: SubtreePath<B> = path.into();
        let pinned;
        let tx = match transaction {
            Some(transaction) => transaction,
            None => {
                pinned = self.db.start_snapshot_read_transaction();
                &pinned
            }
        };
        let element = self.dumped_element(path.clone(), key, tx, grove_version)?;
        let subtree = path.derive_owned_with_child(key);
        let handle = self.db.subtree((&subtree).into(), None, tx).unwrap();
        let named = if element.uses_non_merk_data_storage() {
            element.non_merk_entry_count() != Some(0)
                || !holds_a_record(FlatStore::cursor(&handle))?
        } else {
            match root_key_of(&element) {
                Some(root_key) => handle.has_node(&root_key).unwrap().map_err(merk)?,
                None => !holds_a_record(NodeStore::cursor(&handle))?,
            }
        };
        if !named {
            return Err(DumpLoadError::Refused(
                "the element does not name what its subtree holds",
            ));
        }
        let root = self.root_as((&subtree).into(), &element, tx, grove_version)?;
        let stated = element.serialize(grove_version).map_err(Error::from)?;
        let mut out = DumpWriter::new(out, &stated, &root)?;

        if element.uses_non_merk_data_storage() {
            dump_keyspace(FlatStore::cursor(&handle), Keyspace::Data, &mut out)?;
        } else {
            dump_keyspace(NodeStore::cursor(&handle), Keyspace::Data, &mut out)?;
        }
        dump_keyspace(handle.aux().cursor(), Keyspace::Aux, &mut out)?;
        if let Some(root_key) = handle.root_key().unwrap().map_err(merk)? {
            out.record(Keyspace::Roots, &[], &root_key)?;
        }
        dump_keyspace(handle.meta().cursor(), Keyspace::Meta, &mut out)?;
        drop(handle);
        Ok(DumpedSubtree::new(element, root, out.finish()?))
    }

    /// Reads one dump from `input` and writes its records into the subtree
    /// under `key` in the Merk at `path`.
    ///
    /// It reads one whole dump, and no byte of `input` after it. The
    /// element under `key` has to be a tree of the tree type of the dumped
    /// element, and its subtree has to hold no record in any keyspace, as
    /// `transaction` reads it: anything else is refused. Nothing is
    /// written before the dump is read to its end, its checksum holds and
    /// those refusals are past.
    ///
    /// On the RocksDB engine nothing makes two writers of one subtree
    /// meet but a key that both wrote. A load writes no key outside its
    /// subtree, so a caller that commits a load apart from the replacement
    /// lets no other writer into the subtree until the replacement is
    /// committed.
    ///
    /// The load is a write of `transaction`, as an insert is: it is seen
    /// through that transaction alone until the transaction commits, and
    /// it leaves with a rollback. With no transaction it is a transaction
    /// of its own, which the load commits at its end: an error before that
    /// commit leaves nothing in the grove, and an error of the commit is
    /// what the engine says of a commit. A load writes in steps, the nodes
    /// of a Merk and then every other record, and in a transaction of the
    /// caller's an error of the engine at a later step leaves the earlier
    /// ones in the transaction: the caller rolls the transaction back, or
    /// back to a savepoint it set before the load. A Merk is handed to the
    /// engine whole, in one change set. The element is not written:
    /// [`GroveDb::replace_subtree_root`] is the caller's next step, with
    /// the root [`GroveDb::subtree_root_as`] gives.
    ///
    /// The records of the dump are held in memory until they are written.
    pub fn load_subtree<'db, 'b, B, P, R>(
        &'db self,
        path: P,
        key: &[u8],
        input: R,
        transaction: TransactionArg<'db, '_, E>,
        grove_version: &GroveVersion,
    ) -> Result<DumpedSubtree, DumpLoadError>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
        R: Read,
    {
        let path: SubtreePath<B> = path.into();
        let (mut input, stated, root) = DumpReader::open(input)?;
        let element = Element::deserialize(&stated, grove_version)
            .map_err(|_| DumpLoadError::Malformed("the element of the dump"))?;
        carried(&element).map_err(|_| DumpLoadError::Malformed("the element of the dump"))?;
        let mut records: [Vec<(Vec<u8>, Vec<u8>)>; 4] = Default::default();
        while let Some((keyspace, key, value)) = input.next()? {
            records[keyspace as usize - 1].push((key, value));
        }
        let [data, aux, roots, meta] = records;
        let flat = element.uses_non_merk_data_storage();
        let (merk_nodes, flat_records) = if flat || data.is_empty() {
            (None, data)
        } else {
            (Some(whole_merk(data, grove_version)?), Vec::new())
        };
        let named = if flat {
            element.non_merk_entry_count() != Some(0) || flat_records.is_empty()
        } else {
            let root = merk_nodes
                .as_ref()
                .map(|(nodes, root, _)| nodes[*root].key());
            root_key_of(&element).as_deref() == root
        };
        if !named {
            return Err(DumpLoadError::Malformed(
                "the element of the dump does not name what its records hold",
            ));
        }

        let tx = TxRef::new(&self.db, transaction);
        let target = self.dumped_element(path.clone(), key, tx.as_ref(), grove_version)?;
        if target.tree_type() != element.tree_type() {
            return Err(DumpLoadError::Refused(
                "the element is a tree of another type than the dumped one",
            ));
        }
        let subtree = path.derive_owned_with_child(key);
        let look = self
            .db
            .subtree((&subtree).into(), None, tx.as_ref())
            .unwrap();
        let held = if flat {
            holds_a_record(FlatStore::cursor(&look))?
        } else {
            holds_a_record(NodeStore::cursor(&look))?
        };
        if held
            || holds_a_record(look.aux().cursor())?
            || holds_a_record(look.meta().cursor())?
            || look.root_key().unwrap().map_err(merk)?.is_some()
        {
            return Err(DumpLoadError::Refused("the subtree holds a record"));
        }
        drop(look);

        // A Merk is handed over whole, which a handle that stages its
        // writes cannot say: it goes through a handle that writes at once,
        // as a restore's does, and the engine is told before it that the
        // element's subtree is written and after it that the element hosts
        // the Merk (`Engine::adopt_restored_subtrees`).
        if let Some((nodes, root, order)) = &merk_nodes {
            self.db
                .adopt_restored_subtrees((&subtree).into(), tx.as_ref())
                .map_err(storage)?;
            let handle = self
                .db
                .immediate_subtree((&subtree).into(), tx.as_ref())
                .unwrap();
            let mut change_set = handle.change_set();
            change_set.state_root(Some(nodes[*root].key()));
            change_set.state_whole_merk();
            for node in order.iter().map(|at| &nodes[*at]) {
                let prepared = change_set.prepare_node(node, NodeChange::ValueWritten);
                change_set
                    .put_node(node.key(), prepared, None)
                    .map_err(merk)?;
            }
            handle.commit(change_set).unwrap().map_err(merk)?;
            drop(handle);
            self.db
                .adopt_restored_subtrees((&subtree).into(), tx.as_ref())
                .map_err(storage)?;
        }

        let batch = E::Batch::default();
        let handle = self
            .db
            .subtree((&subtree).into(), Some(&batch), tx.as_ref())
            .unwrap();
        if let Some((_, root_key)) = roots.first() {
            let mut change_set = handle.change_set();
            change_set.put_root_key(root_key, None).map_err(merk)?;
            handle.commit(change_set).unwrap().map_err(merk)?;
        }
        for (key, value) in &flat_records {
            FlatStore::put(&handle, key, value, None)
                .unwrap()
                .map_err(storage)?;
        }
        for (key, value) in &aux {
            (handle.aux().put(key, value, None).unwrap()).map_err(storage)?;
        }
        for (key, value) in &meta {
            (handle.meta().put(key, value, None).unwrap()).map_err(storage)?;
        }
        drop(handle);
        self.db
            .apply_batch(batch, tx.as_ref())
            .unwrap()
            .map_err(Error::from)?;
        tx.commit_local()?;
        Ok(DumpedSubtree::new(element, root, input.counts()))
    }
}
