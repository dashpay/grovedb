//! A Merk in key order, for a loader that takes a child before its parent.

use lignin_format::tree_page::Hash;

use super::{
    error::{LoadError, ShapeError},
    load::Loader,
    node::{HostedLoad, LinkLoad, LoadValues, Loaded, NodeLoad},
};
use crate::{fragment::Pages, hash::HashRules};

/// The heights of the two subtrees of a node.
type Heights = (u16, u16);

fn wide(heights: (u8, u8)) -> Heights {
    (u16::from(heights.0), u16::from(heights.1))
}

/// The height of the subtree whose root has `heights`.
fn above(heights: (u8, u8)) -> u16 {
    1 + u16::from(heights.0.max(heights.1))
}

/// A node that waits for its right subtree.
#[derive(Debug)]
struct Waiting {
    key: Vec<u8>,
    feature: Vec<u8>,
    value: Vec<u8>,
    value_hash: Option<Hash>,
    left: Option<(Loaded, (u8, u8), Vec<u8>)>,
    right: ((u8, u8), Vec<u8>),
    hosted: Vec<HostedLoad>,
}

/// The nodes of one Merk in the order of their keys, as a scan of its
/// records gives them, for a [`Loader`].
///
/// A node says whether it has a child on each side, and the heights of that
/// child's two subtrees, which is what Merk's link stores. That is the
/// shape: the subtree that ends right before a node is its left child, and
/// a node's right child is the first subtree after it whose root has the
/// heights the node's link states. No key is read to find it.
///
/// A node is handed to the loader when its right subtree is complete. Until
/// then its key, feature and value are kept here: as many nodes as the Merk
/// is high.
#[derive(Debug, Default)]
pub struct InOrder {
    waiting: Vec<Waiting>,
    done: Option<(Loaded, Heights)>,
}

impl InOrder {
    /// A Merk of which no node was given yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes the next node of the Merk. Its links name no child: the
    /// children are found by their place in the stream.
    pub fn push<P: Pages, V: LoadValues, R: HashRules>(
        &mut self,
        loader: &mut Loader,
        pages: &mut P,
        values: &mut V,
        rules: &mut R,
        node: &NodeLoad<'_, ()>,
    ) -> Result<(), LoadError> {
        loader.check()?;
        let pushed = self.place(loader, pages, values, rules, node);
        loader.settle(pushed)
    }

    /// Ends the Merk and returns its root, or `None` for a Merk with no
    /// node.
    pub fn finish(self, loader: &mut Loader) -> Result<Option<Loaded>, LoadError> {
        loader.check()?;
        let waiting = self.waiting.len();
        let root = match waiting {
            0 => Ok(self.done.map(|(root, _)| root)),
            _ => Err(ShapeError::NoRight { waiting }.into()),
        };
        loader.settle(root)
    }

    fn place<P: Pages, V: LoadValues, R: HashRules>(
        &mut self,
        loader: &mut Loader,
        pages: &mut P,
        values: &mut V,
        rules: &mut R,
        node: &NodeLoad<'_, ()>,
    ) -> Result<(), LoadError> {
        let left = match (node.left, self.done.take()) {
            (None, None) => None,
            (None, Some(_)) => return Err(ShapeError::NoParent.into()),
            (Some(_), None) => return Err(ShapeError::NoLeft.into()),
            (Some(link), Some((child, found))) => {
                let stated = link.heights;
                if wide(stated) != found {
                    return Err(ShapeError::LeftHeights { stated, found }.into());
                }
                Some(LinkLoad {
                    child,
                    heights: stated,
                    aggregate: link.aggregate,
                })
            }
        };
        let own = |link: Option<(u8, u8)>| link.map_or(0, above);
        let left_height = own(left.map(|link| link.heights));
        if let Some(right) = node.right {
            self.waiting.push(Waiting {
                key: node.key.to_vec(),
                feature: node.feature.to_vec(),
                value: node.value.to_vec(),
                value_hash: node.value_hash.copied(),
                left: left.map(|link| (link.child, link.heights, link.aggregate.to_vec())),
                right: (right.heights, right.aggregate.to_vec()),
                hosted: node.hosted.to_vec(),
            });
            return Ok(());
        }
        let leaf = NodeLoad {
            key: node.key,
            feature: node.feature,
            value: node.value,
            value_hash: node.value_hash,
            left,
            right: None,
            hosted: node.hosted,
        };
        let mut done = (loader.node(pages, values, rules, &leaf)?, (left_height, 0));
        while let Some(parent) = self.waiting.pop_if(|parent| wide(parent.right.0) == done.1) {
            let left = parent.left.as_ref();
            let left = left.map(|(child, heights, aggregate)| LinkLoad {
                child: *child,
                heights: *heights,
                aggregate,
            });
            let right = LinkLoad {
                child: done.0,
                heights: parent.right.0,
                aggregate: &parent.right.1,
            };
            let node = NodeLoad {
                key: &parent.key,
                feature: &parent.feature,
                value: &parent.value,
                value_hash: parent.value_hash.as_ref(),
                left,
                right: Some(right),
                hosted: &parent.hosted,
            };
            let heights = (own(left.map(|link| link.heights)), above(right.heights));
            done = (loader.node(pages, values, rules, &node)?, heights);
        }
        self.done = Some(done);
        Ok(())
    }
}
