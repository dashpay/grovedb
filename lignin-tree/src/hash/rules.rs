//! What evaluation is given from outside: the rules that make a hash, and
//! the values that are not in their cells.

use std::{error::Error, fmt};

use lignin_format::{tree_page::Hash, value_log::ValuePlace, PageId};

use crate::node::NodeRef;

/// The error of a rule, or of a reader of values: whatever its implementor
/// reports.
pub type Refusal = Box<dyn Error + Send + Sync>;

/// The rules that make the hashes a cell stores. They are their caller's:
/// this crate has none of its own, and calls these for a hash that is
/// marked and for nothing else.
///
/// A rule is a function of what it is given. It is called once for each
/// hash that is evaluated, and what it returns is stored as it is. A rule
/// that cannot make a hash from what it is given refuses, and the hash
/// stays marked.
///
/// Which hash is made from what:
///
/// | Stored hash | Rule | Made from |
/// | --- | --- | --- |
/// | value hash | [`value_hash`](Self::value_hash) | the node's value, and the hashes of the roots of the Merks it hosts |
/// | key-value hash | [`kv_hash`](Self::kv_hash) | the node's key and its value hash |
/// | hash of a child, in the link to it | [`node_hash`](Self::node_hash) of the child | what the child's cell holds: its key-value hash, the hashes of its children, and whatever else of the cell the rule takes in |
///
/// The hash of the root of a Merk is in no cell (`tree-layout.md` C3). It
/// is [`node_hash`](Self::node_hash) of the root, made when it is asked
/// for.
pub trait HashRules {
    /// The value hash of `node`, whose value is `value`.
    ///
    /// `hosted` has the hash of the root of each Merk the node hosts, by
    /// position, and `None` for a hosted Merk with no root. It is empty for
    /// a node that hosts none.
    ///
    /// The value hash and the key-value hash that `node` shows are the ones
    /// this call is about to replace. Everything else of the cell is up to
    /// date.
    ///
    /// A value whose hash is made from something that is not given here, as
    /// a reference's is from the value it names, is refused. Its writer
    /// makes that hash and never marks it, so being asked for it means that
    /// a mark is on the wrong node, and a hash made here would be wrong.
    fn value_hash(
        &mut self,
        node: NodeRef<'_>,
        value: &[u8],
        hosted: &[Option<Hash>],
    ) -> Result<Hash, Refusal>;

    /// The key-value hash of a node with the key `key` and the value hash
    /// `value_hash`.
    fn kv_hash(&mut self, key: &[u8], value_hash: &Hash) -> Result<Hash, Refusal>;

    /// The hash of `node`: what the link of its parent stores, or for the
    /// root of a Merk the Merk's root hash.
    ///
    /// Every hash that `node` shows is up to date, and so are its feature
    /// and the heights and aggregates in its links, which are their
    /// writer's to keep.
    fn node_hash(&mut self, node: NodeRef<'_>) -> Result<Hash, Refusal>;

    /// Another hash that a writer may have taken for the hash of `node`,
    /// the root of a tree that a node hosts, when it made the value hash
    /// of the host: `None` where the rules name none. The provided method
    /// names none.
    ///
    /// It is asked by [`made_with_other`](super::made_with_other) and by
    /// nothing else: no evaluation makes a hash with it, and
    /// [`check`](super::check) compares every stored hash with what the
    /// three rules above make.
    fn other_node_hash(&mut self, node: NodeRef<'_>) -> Result<Option<Hash>, Refusal> {
        let _ = node;
        Ok(None)
    }

    /// The value hash of `node`, whose value is `value`, as a writer made
    /// it that took for a hosted root the hash that
    /// [`other_node_hash`](Self::other_node_hash) names: `None` where the
    /// rules name no such value hash for the node. The provided method
    /// names none.
    ///
    /// `hosted` is as for [`value_hash`](Self::value_hash), with the other
    /// hash of each root that has one, and at least one root has.
    fn value_hash_with_other(
        &mut self,
        node: NodeRef<'_>,
        value: &[u8],
        hosted: &[Option<Hash>],
    ) -> Result<Option<Hash>, Refusal> {
        let _ = (node, value, hosted);
        Ok(None)
    }
}

impl<R: HashRules + ?Sized> HashRules for &mut R {
    fn value_hash(
        &mut self,
        node: NodeRef<'_>,
        value: &[u8],
        hosted: &[Option<Hash>],
    ) -> Result<Hash, Refusal> {
        (**self).value_hash(node, value, hosted)
    }

    fn kv_hash(&mut self, key: &[u8], value_hash: &Hash) -> Result<Hash, Refusal> {
        (**self).kv_hash(key, value_hash)
    }

    fn node_hash(&mut self, node: NodeRef<'_>) -> Result<Hash, Refusal> {
        (**self).node_hash(node)
    }

    fn other_node_hash(&mut self, node: NodeRef<'_>) -> Result<Option<Hash>, Refusal> {
        (**self).other_node_hash(node)
    }

    fn value_hash_with_other(
        &mut self,
        node: NodeRef<'_>,
        value: &[u8],
        hosted: &[Option<Hash>],
    ) -> Result<Option<Hash>, Refusal> {
        (**self).value_hash_with_other(node, value, hosted)
    }
}

/// Reads the values that are not in their cells: those in the value log,
/// and those a commit has not put there yet.
///
/// A value hash is made from the value's bytes. A value that is in its cell
/// is read from the page. For any other, evaluation asks here, between two
/// page steps and never inside one.
pub trait ValueReader {
    /// Appends to `into`, which is empty, the bytes of the value at `place`
    /// that a cell of `page` holds: `place.value_len()` bytes.
    fn read(&mut self, place: ValuePlace, page: PageId, into: &mut Vec<u8>) -> Result<(), Refusal>;
}

impl<V: ValueReader + ?Sized> ValueReader for &mut V {
    fn read(&mut self, place: ValuePlace, page: PageId, into: &mut Vec<u8>) -> Result<(), Refusal> {
        (**self).read(place, page, into)
    }
}

/// No value is read: for pages whose marked nodes all have their value in
/// their cell. A value that is not is refused.
impl ValueReader for () {
    fn read(&mut self, _: ValuePlace, _: PageId, _: &mut Vec<u8>) -> Result<(), Refusal> {
        Err(Box::new(NoValueReader))
    }
}

/// What `()` answers as a [`ValueReader`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoValueReader;

impl fmt::Display for NoValueReader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("no reader was given for a value that is not in its cell")
    }
}

impl Error for NoValueReader {}
