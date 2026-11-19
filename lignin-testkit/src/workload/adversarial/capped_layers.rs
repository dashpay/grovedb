//! Pages of an index: a limit of a query node on a layer that has
//! subqueries.

use grovedb::{element::reference_path::ReferencePathType, Element, PathQuery, Query, QueryItem};

use super::DOMAIN_CAPPED_LAYERS;
use crate::workload::{
    build::{self, child, path, value},
    op::{Op, Path},
    rng::Rng,
    Features, Prepared, Stream,
};

const ROOT: &[u8] = b"capped-layers";
const INDEX: &[u8] = b"index";
const NESTED: &[u8] = b"nested";
const LOOKUP: &[u8] = b"lookup";
const GROUPS: u16 = 3;
const LOOKUPS: u16 = 4;

/// The fewest keys [`CappedLayers`] starts its index with: a page needs
/// keys before it and after it.
pub const MIN_CAPPED_KEYS: u32 = 8;

/// The most keys [`CappedLayers`] starts its index with: far below where
/// the number a key is made of would not fit its four bytes.
pub const MAX_CAPPED_KEYS: u32 = 1 << 20;

/// The most rounds [`CappedLayers`] runs: the documents a round adds to the
/// second index have keys of one byte each.
pub const MAX_CAPPED_ROUNDS: u32 = 32;

/// An index whose keys each hold a few documents, read a page at a time.
///
/// A page is a query that walks the keys of the index in order, descends
/// into each for its documents and stops when it has enough rows. The
/// limit is the one of the query node that walks the index
/// (`Query::limit`), so it counts the rows found below the keys and not the
/// keys. That is the form of a page that can be merged with other queries
/// into one proof: `PathQuery::merge` gives every query it merges a branch
/// of its own, and a limit of the whole query has no branch to belong to.
///
/// Grove version 4 answers such a page and proves the layer of the index
/// with every key in its range. From grove version 5 the proof ends the
/// layer after the key whose documents filled the page. The answers are
/// the same.
///
/// The index holds, beside keys with documents, keys with none, keys that
/// hold an item or a reference where the others hold a tree, and so are
/// rows themselves, and it loses and gains keys and documents each round.
/// A second index has two layers of keys above its documents. After each
/// block the pages are read and proven:
///
/// - a page of the index from either end, with and without a limit of the
///   whole query next to the limit of the node;
/// - a page whose node descends differently for some keys: into the first
///   document only, or through the documents from their end;
/// - a page of the second index, where the limit of the node at the top is
///   also in force on the layer below it, with and without a limit of its
///   own for that layer;
/// - a page merged with a query for some keys of another tree, built as
///   `PathQuery::merge` builds it.
///
/// A grove version that has no limit of a query node reads the same
/// layers under a limit of the whole query, through `GroveDb::query_raw`,
/// which is not proven, and proves the merged query without a limit. Those
/// versions charge a key without documents to the limit of the whole query
/// in one way when they read and in another when they prove, so the proof
/// of such a page may hold other rows than the read returns, or not
/// verify.
///
/// No page takes a tree as a row. A tree without documents that is a row
/// of a page is one the proof of grove version 4 counts and its verifier
/// does not, so that proof does not verify.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CappedLayers {
    /// How many keys the index starts with, from [`MIN_CAPPED_KEYS`] to
    /// [`MAX_CAPPED_KEYS`].
    pub keys: u32,
    /// How many rounds of writes and reads to run, at most
    /// [`MAX_CAPPED_ROUNDS`].
    pub rounds: u32,
    /// What the target grove version can store.
    pub features: Features,
}

impl Default for CappedLayers {
    fn default() -> Self {
        Self {
            keys: 24,
            rounds: 4,
            features: Features::ALL,
        }
    }
}

fn index_key(number: u32) -> Vec<u8> {
    number.to_be_bytes().to_vec()
}

fn short_key(tag: u8, number: u16) -> Vec<u8> {
    vec![tag, b'0' + number as u8]
}

struct Generator {
    rng: Rng,
    capped: bool,
    root: Path,
    /// The number the next key of the index is made of. A key takes every
    /// third number, so a range can start and end between two keys.
    next: u32,
    /// The keys of the index that hold a tree, each with how many documents
    /// the tree was given so far.
    trees: Vec<(Vec<u8>, u16)>,
    ops: Vec<Op>,
}

impl CappedLayers {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(
            (MIN_CAPPED_KEYS..=MAX_CAPPED_KEYS).contains(&self.keys),
            "the index starts with {MIN_CAPPED_KEYS} to {MAX_CAPPED_KEYS} keys"
        );
        assert!(
            self.rounds <= MAX_CAPPED_ROUNDS,
            "at most {MAX_CAPPED_ROUNDS} rounds"
        );
        let mut generator = Generator {
            rng: Rng::derive(seed, DOMAIN_CAPPED_LAYERS, 0),
            capped: self.features.instance_limits,
            root: path(&[ROOT]),
            next: 0,
            trees: Vec::new(),
            ops: Vec::new(),
        };
        generator.setup(self.keys);
        for _ in 0..self.rounds {
            generator.change();
            generator.ops.push(Op::Commit);
            generator.pages();
        }
        Prepared::stream(generator.ops)
    }
}

impl Generator {
    fn tree(&self, key: &[u8]) -> Path {
        child(&self.root, key)
    }

    fn document(&mut self) -> Element {
        Element::new_item(value(&mut self.rng, 1, 32))
    }

    /// A tree at `key` of `parent` with `documents` documents.
    fn tree_with_documents(&mut self, parent: &Path, key: &[u8], documents: u16) {
        self.ops
            .push(build::insert(parent, key, Element::empty_tree()));
        let tree = child(parent, key);
        for document in 0..documents {
            let item = self.document();
            self.ops
                .push(build::insert(&tree, &short_key(b'd', document), item));
        }
    }

    /// A key more for the index: a tree with up to three documents, more
    /// often than a tree with none, an item or a reference to an item of
    /// the other tree.
    fn add_key(&mut self) {
        let index = self.tree(INDEX);
        let key = index_key(self.next * 3);
        self.next += 1;
        let documents = match self.rng.below(10) {
            0 | 1 => 0,
            2 => {
                let item = self.document();
                self.ops.push(build::insert(&index, &key, item));
                return;
            }
            3 => {
                let target = short_key(b'l', self.rng.below(u64::from(LOOKUPS)) as u16);
                let reference = Element::new_reference(ReferencePathType::AbsolutePathReference(
                    child(&self.tree(LOOKUP), &target),
                ));
                self.ops.push(build::insert(&index, &key, reference));
                return;
            }
            _ => self.rng.between(1, 3) as u16,
        };
        self.tree_with_documents(&index, &key, documents);
        self.trees.push((key, documents));
    }

    fn setup(&mut self, keys: u32) {
        let root = self.root.clone();
        self.ops
            .push(build::insert(&[], ROOT, Element::empty_tree()));
        for tree in [INDEX, NESTED, LOOKUP] {
            self.ops
                .push(build::insert(&root, tree, Element::empty_tree()));
        }
        let lookup = self.tree(LOOKUP);
        for number in 0..LOOKUPS {
            let item = self.document();
            self.ops
                .push(build::insert(&lookup, &short_key(b'l', number), item));
        }
        for _ in 0..keys {
            self.add_key();
        }
        let nested = self.tree(NESTED);
        for group in 0..GROUPS {
            let group_key = short_key(b'g', group);
            self.ops
                .push(build::insert(&nested, &group_key, Element::empty_tree()));
            let group_path = child(&nested, &group_key);
            for key in 0..4 {
                let documents = self.rng.below(4) as u16;
                self.tree_with_documents(&group_path, &short_key(b'k', key), documents);
            }
        }
        self.ops.push(Op::Commit);
    }

    /// Two keys more and, where the index holds a tree, a document more
    /// for one, the newest document of one taken away if it has any, which
    /// may leave it empty, and one tree deleted with what it holds. A
    /// document more for the second index.
    fn change(&mut self) {
        let index = self.tree(INDEX);
        for _ in 0..2 {
            self.add_key();
        }
        if !self.trees.is_empty() {
            let grown = self.rng.index(self.trees.len());
            let (key, documents) = self.trees[grown].clone();
            let item = self.document();
            self.ops.push(build::insert(
                &child(&index, &key),
                &short_key(b'd', documents),
                item,
            ));
            self.trees[grown].1 += 1;
            let shrunk = self.rng.index(self.trees.len());
            let (key, documents) = self.trees[shrunk].clone();
            if documents > 0 {
                // The newest document is still there: only this takes one
                // away, and it takes the newest.
                self.ops.push(build::delete(
                    &child(&index, &key),
                    &short_key(b'd', documents - 1),
                ));
                self.trees[shrunk].1 -= 1;
            }
            let deleted = self.rng.index(self.trees.len());
            let (key, _) = self.trees.swap_remove(deleted);
            self.ops.push(build::delete(&index, &key));
        }

        let group = child(
            &self.tree(NESTED),
            &short_key(b'g', self.rng.below(u64::from(GROUPS)) as u16),
        );
        let key = short_key(b'k', self.rng.below(4) as u16);
        let item = self.document();
        // A document under a key of its own, so it replaces none: `next`
        // grows by two a round, and there are no more than
        // `MAX_CAPPED_ROUNDS` rounds.
        let document = short_key(b'e', self.next as u16 % 64);
        self.ops
            .push(build::insert(&child(&group, &key), &document, item));
    }

    /// A walk over a range of the keys of the index, in either direction,
    /// and through the documents under each.
    fn walk(&mut self) -> Query {
        let top = u64::from(self.next) * 3;
        let low = index_key(self.rng.below(top / 2) as u32);
        let high = index_key(self.rng.between(top / 2, top + 2) as u32);
        let item = match self.rng.below(5) {
            0 | 1 => QueryItem::RangeFull(..),
            2 => QueryItem::Range(low..high),
            3 => QueryItem::RangeFrom(low..),
            _ => QueryItem::RangeToInclusive(..=high),
        };
        let mut keys = Query::new_single_query_item_with_direction(item, self.rng.chance(2, 3));
        keys.set_subquery(Query::new_range_full());
        keys
    }

    /// A read of `query` at `path` with a page of `page` rows: the limit
    /// of the node `query` where the target has one, and otherwise of the
    /// whole query.
    fn page(&mut self, path: &Path, mut query: Query, page: u16) {
        let read = if self.capped {
            query.limit = Some(page);
            PathQuery::new_unsized(path.clone(), query)
        } else {
            build::sized(path, query, Some(page), None)
        };
        self.limited(read);
    }

    /// A read under a limit: proven where the target has the limit of a
    /// query node, and otherwise run by a reader that is never proven.
    fn limited(&mut self, read: PathQuery) {
        self.ops.push(if self.capped {
            build::proven(read)
        } else {
            build::rows(read, true)
        });
    }

    fn pages(&mut self) {
        let index = self.tree(INDEX);

        // Pages of the index.
        for _ in 0..2 {
            let body = self.walk();
            let page = self.rng.between(1, 7) as u16;
            self.page(&index, body, page);
        }

        // A limit of the whole query next to the limit of the node, smaller
        // or larger than it.
        let mut body = self.walk();
        let page = self.rng.between(2, 6) as u16;
        let whole = self.rng.between(1, 9) as u16;
        if self.capped {
            body.limit = Some(page);
        }
        self.limited(build::sized(&index, body, Some(whole), None));

        // A node that descends differently for some keys: into the first
        // document only, and through the documents from their end.
        let mut body = self.walk();
        let top = self.next * 3;
        let first_only = QueryItem::Range(index_key(top / 4)..index_key(top / 2));
        body.add_conditional_subquery(
            first_only,
            None,
            Some(Query::new_single_query_item(QueryItem::RangeToInclusive(
                ..=short_key(b'd', 0),
            ))),
        );
        let from_the_end = QueryItem::RangeFrom(index_key(top / 4 * 3)..);
        body.add_conditional_subquery(
            from_the_end,
            None,
            Some(Query::new_single_query_item_with_direction(
                QueryItem::RangeFull(..),
                false,
            )),
        );
        let page = self.rng.between(2, 8) as u16;
        self.page(&index, body, page);

        // The second index: the limit of the node at the top is in force
        // on the layer below it as well.
        let nested = self.tree(NESTED);
        for own_limit in [false, true] {
            let mut keys = Query::new_single_query_item_with_direction(
                QueryItem::RangeFull(..),
                self.rng.chance(1, 2),
            );
            keys.set_subquery(Query::new_range_full());
            if own_limit && self.capped {
                keys.limit = Some(self.rng.between(1, 3) as u16);
            }
            let mut groups = Query::new_range_full();
            groups.set_subquery(keys);
            let page = self.rng.between(2, 9) as u16;
            self.page(&nested, groups, page);
        }

        // A page merged with a query for some keys of another tree.
        let mut body = self.walk();
        // Merged queries walk in one direction.
        body.left_to_right = true;
        if self.capped {
            body.limit = Some(self.rng.between(1, 6) as u16);
        }
        let mut lookups = Query::new();
        lookups.insert_key(short_key(b'l', self.rng.below(u64::from(LOOKUPS)) as u16));
        lookups.insert_key(short_key(b'l', LOOKUPS + 2));
        self.ops.push(build::proven(merged(
            &self.root,
            [(LOOKUP, lookups), (INDEX, body)],
        )));
    }
}

/// The query `PathQuery::merge` makes of queries for trees under `common`:
/// a query for the keys of the trees, each with the query for its tree as
/// the subquery of its key. The queries without a limit come first.
fn merged<const N: usize>(common: &Path, branches: [(&[u8], Query); N]) -> PathQuery {
    let mut root = Query::new();
    for (key, body) in branches {
        root.insert_item(QueryItem::Key(key.to_vec()));
        root.add_conditional_subquery(QueryItem::Key(key.to_vec()), None, Some(body));
    }
    PathQuery::new_unsized(common.clone(), root)
}
