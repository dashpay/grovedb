//! GroveDB batch operations support
mod apply;
mod subtrees;
#[cfg(test)]
mod tests;

use std::cmp::Ordering;

use intrusive_collections::RBTreeLink;
use visualize::{Drawer, Visualize};

use crate::Element;

#[derive(Debug, thiserror::Error)]
pub enum BatchError {
    #[error("deleted subtree access")]
    DeletedSubtreeAccess,
    #[error("merk open error: {0}")]
    MerkError(String),
}

/// Batch operation
#[derive(Clone)]
pub struct GroveDbOp {
    /// Path to a subtree - subject to an operation
    path: Vec<Vec<u8>>,
    /// Key of an element in the subtree
    key: Vec<u8>,
    /// Operation to perform on the key
    op: Op,
    /// Link used in intrusive tree to maintain operations order
    link: RBTreeLink,
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
enum Op {
    Insert { element: Element },
    Delete,
}

impl PartialOrd for Op {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Op::Delete, Op::Insert { .. }) => Some(Ordering::Less),
            (Op::Insert { .. }, Op::Delete) => Some(Ordering::Greater),
            _ => Some(Ordering::Equal),
        }
    }
}

impl Ord for Op {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other).expect("all ops have order")
    }
}

impl PartialEq for GroveDbOp {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && self.key == other.key && self.op == other.op
    }
}

impl std::fmt::Debug for GroveDbOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut path_out = Vec::new();
        let mut path_drawer = Drawer::new(&mut path_out);
        for p in &self.path {
            path_drawer = p.visualize(path_drawer).unwrap();
            path_drawer.write(b" ").unwrap();
        }
        let mut key_out = Vec::new();
        let key_drawer = Drawer::new(&mut key_out);
        self.key.visualize(key_drawer).unwrap();

        let op_dbg = match self.op {
            Op::Insert {
                element: Element::Tree(..),
            } => "Insert tree",
            Op::Insert { .. } => "Insert",
            Op::Delete => "Delete",
        };

        f.debug_struct("GroveDbOp")
            .field("path", &String::from_utf8_lossy(&path_out))
            .field("key", &String::from_utf8_lossy(&key_out))
            .field("op", &op_dbg)
            .finish()
    }
}
