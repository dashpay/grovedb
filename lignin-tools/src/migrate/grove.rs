//! What the migrator makes of the records of one Merk: the nodes as their
//! records state them, the tree those nodes form, and which of them no
//! tree links.
//!
//! # The root of a Merk
//!
//! RocksDB holds the nodes of a Merk as records under their keys. Which of
//! them is the root is said elsewhere, and GroveDB reads it in one of two
//! places when it opens the Merk ([`Rooted`]):
//!
//! - **The element that hosts the Merk names it.** That is every Merk
//!   under an element but the secondary of an indexed tree's axis whose
//!   root the element does not name.
//! - **The record of the root key in the roots keyspace names it**, for a
//!   Merk that is opened with no root key given: the root Merk of the
//!   grove, and such a secondary.
//!
//! LigninDB keeps the root of a Merk in the subtree directory, from the
//! commits of the Merk, and finds every node from it. So the migrator has
//! to state the root that those commits left:
//!
//! - Named by the element: the node under that key, which has to be there
//!   ([`Fault::RootMissing`]). An element that names no root has a Merk
//!   with no node, and every record under its prefix is one that no tree
//!   links.
//! - Named by the record: the node under the record's key where no node
//!   links it. Where the record names no node, or one that another node
//!   links, the record is not what the last commit of the Merk left
//!   (GroveDB #1065: a delete of a tree at the root path on grove versions
//!   1 to 3 writes no such record). The root is then the one node that no
//!   node links, if the nodes are one tree under it ([`Shape::stale`]), and
//!   the Merk is refused otherwise ([`Fault::NoRoot`]). The record is
//!   carried as it is in either case.
//!
//! # Records that no tree links
//!
//! A node record that the root does not reach is one that GroveDB left
//! behind (`lignindb::txn`, "Unlinked records"). It is carried as a put
//! that the stated root does not reach, which the engine keeps as an
//! unlinked record: read by its key and under a cursor, in no hash.

use grovedb_merk::{
    store::lignin::{aggregate_bytes, feature_bytes},
    tree::{kv::ValueDefinedCostType, Link as MerkLink, TreeNode},
};
use grovedb_version::version::GroveVersion;
use lignin_format::tree_page::Hash;
use lignindb::txn::Subtree;

/// A link of a node, as its record states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Link {
    /// The child's key.
    pub(crate) key: Vec<u8>,
    /// The child's hash.
    pub(crate) hash: Hash,
    /// The heights of the child's left and right subtree.
    pub(crate) heights: (u8, u8),
    /// The child's aggregate, as the bytes a tree page holds.
    pub(crate) aggregate: Vec<u8>,
}

/// A node of a Merk, as its record states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Node {
    /// The node's key.
    pub(crate) key: Vec<u8>,
    /// The feature, as the bytes a tree page holds.
    pub(crate) feature: Vec<u8>,
    /// The value hash.
    pub(crate) value_hash: Hash,
    /// The key-value hash.
    pub(crate) kv_hash: Hash,
    /// The link to the left child.
    pub(crate) left: Option<Link>,
    /// The link to the right child.
    pub(crate) right: Option<Link>,
    /// The value.
    pub(crate) value: Vec<u8>,
    /// The Merks that the node's element hosts and the migration enters,
    /// by position: what the node's cell points at.
    pub(crate) hosted: Vec<Subtree>,
}

/// Merk's own decoding of the record `record` of the node under `key`.
pub(crate) fn decode(key: &[u8], record: &[u8]) -> Result<TreeNode, String> {
    let no_cost = None::<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>;
    let node = TreeNode::decode(key.to_vec(), record, no_cost, GroveVersion::latest());
    node.map_err(|error| error.to_string())
}

fn link(link: Option<&MerkLink>) -> Option<Link> {
    let MerkLink::Reference {
        hash,
        child_heights,
        key,
        aggregate_data,
    } = link?
    else {
        unreachable!("a node that was decoded from its record links records");
    };
    Some(Link {
        key: key.clone(),
        hash: *hash,
        heights: *child_heights,
        aggregate: aggregate_bytes(aggregate_data),
    })
}

impl Node {
    /// The node `decoded`, which hosts `hosted`.
    pub(crate) fn of(decoded: &TreeNode, hosted: Vec<Subtree>) -> Self {
        Self {
            key: decoded.key().to_vec(),
            feature: feature_bytes(&decoded.feature_type()),
            value_hash: *decoded.value_hash(),
            kv_hash: *decoded.kv_hash(),
            left: link(decoded.link(true)),
            right: link(decoded.link(false)),
            value: decoded.value_as_slice().to_vec(),
            hosted,
        }
    }
}

/// The keys that the record of one node states: its own, and those of its
/// children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Keys {
    /// The node's key.
    pub(crate) key: Vec<u8>,
    /// The key of the left child.
    pub(crate) left: Option<Vec<u8>>,
    /// The key of the right child.
    pub(crate) right: Option<Vec<u8>>,
}

impl Keys {
    /// The keys of `decoded`.
    pub(crate) fn of(decoded: &TreeNode) -> Self {
        let child = |left| decoded.link(left).map(|link| link.key().to_vec());
        Self {
            key: decoded.key().to_vec(),
            left: child(true),
            right: child(false),
        }
    }
}

/// Where GroveDB reads the root key of a Merk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Rooted {
    /// In the element that hosts the Merk, which names this key or none.
    ByElement(Option<Vec<u8>>),
    /// In the record of the root key, which holds this key, or is not
    /// there.
    ByRecord(Option<Vec<u8>>),
}

/// Why the records of a Merk are not carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fault {
    /// The element names a root key that no record is under.
    RootMissing {
        /// The key the element names.
        root: Vec<u8>,
    },
    /// The record of the root key names no node that no node links, and
    /// the nodes are not one tree.
    NoRoot {
        /// How many nodes no node links.
        tops: usize,
    },
    /// A node of the tree links a key that no record is under.
    NoChild {
        /// The node.
        node: Vec<u8>,
        /// The key its link names.
        child: Vec<u8>,
    },
    /// Two links of the tree name one node.
    LinkedTwice {
        /// The node.
        node: Vec<u8>,
    },
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hex = crate::hex;
        match self {
            Self::RootMissing { root } => write!(
                f,
                "its element names the root key {}, and no record is under that key",
                hex(root)
            ),
            Self::NoRoot { tops } => write!(
                f,
                "its root-key record names no node that could be its root, and its records \
                 are not one tree ({tops} nodes that no node links)"
            ),
            Self::NoChild { node, child } => write!(
                f,
                "the node {} links the key {}, and no record is under that key",
                hex(node),
                hex(child)
            ),
            Self::LinkedTwice { node } => {
                write!(f, "two links of its tree name the node {}", hex(node))
            }
        }
    }
}

/// The tree that the records of one Merk form.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Shape {
    /// The place of the root among the records, or `None` for a Merk with
    /// no node.
    pub(crate) root: Option<usize>,
    /// The places of the nodes of the tree, a child before its parent.
    pub(crate) order: Vec<usize>,
    /// The places of the records that the tree does not link, in the
    /// order of their keys.
    pub(crate) unlinked: Vec<usize>,
    /// Whether the record of the root key does not name the root that the
    /// Merk's last commit left (GroveDB #1065).
    pub(crate) stale: bool,
}

/// The place of the record under `key` among `records`, which are in the
/// order of their keys.
fn place(records: &[Keys], key: &[u8]) -> Option<usize> {
    (records.binary_search_by(|record| record.key.as_slice().cmp(key))).ok()
}

/// The places of the nodes that `root` reaches, a child before its parent.
fn reached(records: &[Keys], root: usize) -> Result<Vec<usize>, Fault> {
    let mut seen = vec![false; records.len()];
    let mut order = Vec::new();
    // Each node with how many of its children were gone into.
    let mut stack = vec![(root, 0u8)];
    seen[root] = true;
    while let Some((at, done)) = stack.last_mut() {
        let record = &records[*at];
        let child = match *done {
            0 => &record.left,
            1 => &record.right,
            _ => {
                order.push(*at);
                stack.pop();
                continue;
            }
        };
        *done += 1;
        let Some(child) = child else { continue };
        let Some(below) = place(records, child) else {
            return Err(Fault::NoChild {
                node: record.key.clone(),
                child: child.clone(),
            });
        };
        if std::mem::replace(&mut seen[below], true) {
            return Err(Fault::LinkedTwice {
                node: child.clone(),
            });
        }
        stack.push((below, 0));
    }
    Ok(order)
}

/// The tree that `records` form, which are the records of one Merk in the
/// order of their keys, with its root read as `rooted` says.
pub(crate) fn shape(records: &[Keys], rooted: &Rooted) -> Result<Shape, Fault> {
    let (root, stale) = match rooted {
        Rooted::ByElement(None) => (None, false),
        Rooted::ByElement(Some(named)) => match place(records, named) {
            Some(root) => (Some(root), false),
            None => {
                return Err(Fault::RootMissing {
                    root: named.clone(),
                });
            }
        },
        Rooted::ByRecord(_) if records.is_empty() => (None, false),
        Rooted::ByRecord(named) => {
            let mut linked = vec![false; records.len()];
            for record in records {
                for child in [&record.left, &record.right].into_iter().flatten() {
                    if let Some(below) = place(records, child) {
                        linked[below] = true;
                    }
                }
            }
            let named = named.as_ref().and_then(|named| place(records, named));
            match named.filter(|named| !linked[*named]) {
                Some(root) => (Some(root), false),
                None => {
                    let mut tops = (0..records.len()).filter(|at| !linked[*at]);
                    let (top, more) = (tops.next(), tops.count());
                    let one_tree = top.filter(|_| more == 0).and_then(|top| {
                        let order = reached(records, top).ok()?;
                        (order.len() == records.len()).then_some(top)
                    });
                    match one_tree {
                        Some(root) => (Some(root), true),
                        None => {
                            return Err(Fault::NoRoot {
                                tops: usize::from(top.is_some()) + more,
                            });
                        }
                    }
                }
            }
        }
    };
    let order = match root {
        Some(root) => reached(records, root)?,
        None => Vec::new(),
    };
    let mut in_tree = vec![false; records.len()];
    for at in &order {
        in_tree[*at] = true;
    }
    let unlinked = (0..records.len()).filter(|at| !in_tree[*at]).collect();
    Ok(Shape {
        root,
        order,
        unlinked,
        stale,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(key: &str, left: Option<&str>, right: Option<&str>) -> Keys {
        let bytes = |key: &str| key.as_bytes().to_vec();
        Keys {
            key: bytes(key),
            left: left.map(bytes),
            right: right.map(bytes),
        }
    }

    /// A tree of five nodes under `c`, in the order of the keys.
    fn five() -> Vec<Keys> {
        vec![
            node("a", None, None),
            node("b", Some("a"), None),
            node("c", Some("b"), Some("e")),
            node("d", None, None),
            node("e", Some("d"), None),
        ]
    }

    fn named(key: &str) -> Option<Vec<u8>> {
        Some(key.as_bytes().to_vec())
    }

    fn keys(records: &[Keys], places: &[usize]) -> String {
        let key = |at: &usize| String::from_utf8(records[*at].key.clone()).unwrap();
        places.iter().map(key).collect()
    }

    #[test]
    fn a_tree_is_stated_a_child_before_its_parent() {
        let records = five();
        for rooted in [Rooted::ByElement(named("c")), Rooted::ByRecord(named("c"))] {
            let shape = shape(&records, &rooted).unwrap();
            assert_eq!(shape.root, Some(2));
            assert_eq!(keys(&records, &shape.order), "abdec");
            assert!(shape.unlinked.is_empty() && !shape.stale);
        }
    }

    #[test]
    fn a_merk_with_no_record_has_no_root_and_an_element_that_names_none_links_nothing() {
        let none = Shape::default();
        assert_eq!(shape(&[], &Rooted::ByElement(None)).unwrap(), none);
        assert_eq!(shape(&[], &Rooted::ByRecord(None)).unwrap(), none);
        // A record that outlived its Merk names a key with no node.
        assert_eq!(shape(&[], &Rooted::ByRecord(named("gone"))).unwrap(), none);

        let records = five();
        let shape = shape(&records, &Rooted::ByElement(None)).unwrap();
        assert_eq!((shape.root, shape.order.len()), (None, 0));
        assert_eq!(keys(&records, &shape.unlinked), "abcde");
        assert!(!shape.stale);
    }

    #[test]
    fn records_that_the_root_does_not_reach_are_unlinked() {
        // `x` is a record that a commit left behind, with a link of its
        // own to a node of the tree, and `y` one with no link.
        let mut records = five();
        records.push(node("x", Some("e"), None));
        records.push(node("y", None, None));
        for rooted in [Rooted::ByElement(named("c")), Rooted::ByRecord(named("c"))] {
            let shape = shape(&records, &rooted).unwrap();
            assert_eq!(keys(&records, &shape.order), "abdec");
            assert_eq!(keys(&records, &shape.unlinked), "xy");
            assert!(!shape.stale);
        }
        // Under the element's word a subtree of the records is the tree.
        let shape = shape(&records, &Rooted::ByElement(named("e"))).unwrap();
        assert_eq!(keys(&records, &shape.order), "de");
        assert_eq!(keys(&records, &shape.unlinked), "abcxy");
    }

    #[test]
    fn a_root_key_record_that_does_not_name_the_root_is_stale() {
        let records = five();
        // The record names a node that is gone, a node inside the tree, or
        // is not there: the root is the one node that no node links.
        for stale in [named("gone"), named("e"), None] {
            let shape = shape(&records, &Rooted::ByRecord(stale)).unwrap();
            assert_eq!(shape.root, Some(2));
            assert_eq!(keys(&records, &shape.order), "abdec");
            assert!(shape.unlinked.is_empty() && shape.stale);
        }
    }

    #[test]
    fn records_that_are_no_tree_under_the_word_of_their_root_are_refused() {
        let records = five();
        let missing = shape(&records, &Rooted::ByElement(named("gone")));
        assert_eq!(
            missing,
            Err(Fault::RootMissing {
                root: b"gone".to_vec()
            })
        );

        // Two trees and a record that names neither.
        let mut two = five();
        two.push(node("x", None, None));
        let refused = shape(&two, &Rooted::ByRecord(named("gone")));
        assert_eq!(refused, Err(Fault::NoRoot { tops: 2 }));
        // Every node is linked: a cycle has no root.
        let cycle = vec![node("a", Some("b"), None), node("b", Some("a"), None)];
        let refused = shape(&cycle, &Rooted::ByRecord(named("gone")));
        assert_eq!(refused, Err(Fault::NoRoot { tops: 0 }));

        let mut dangling = five();
        dangling[0].left = named("gone");
        let refused = shape(&dangling, &Rooted::ByElement(named("c")));
        let no_child = Fault::NoChild {
            node: b"a".to_vec(),
            child: b"gone".to_vec(),
        };
        assert_eq!(refused, Err(no_child.clone()));
        assert!(no_child.to_string().contains("links the key 676f6e65"));

        let mut twice = five();
        twice[3].left = named("a");
        let refused = shape(&twice, &Rooted::ByElement(named("c")));
        let linked_twice = Fault::LinkedTwice {
            node: b"a".to_vec(),
        };
        assert_eq!(refused, Err(linked_twice.clone()));
        for fault in [
            linked_twice,
            Fault::NoRoot { tops: 2 },
            Fault::RootMissing {
                root: b"gone".to_vec(),
            },
        ] {
            assert!(!fault.to_string().is_empty());
        }
    }
}
