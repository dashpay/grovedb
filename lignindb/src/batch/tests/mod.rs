mod model;
mod order;
mod rules;
mod storage_batch;
mod txn;

use std::collections::BTreeMap;

use lignin_format::tree_page::Hash;
use lignin_tree::hash::Stale;

use crate::txn::{Column, Hosted, LinkPut, NodePut, Subtree, Unit, ValuePut};

fn subtree(name: u8) -> Subtree {
    Subtree::new([name; 32])
}

fn unit(column: Column, name: u8) -> Unit {
    Unit::new(column, subtree(name))
}

fn hash_of(parts: &[&[u8]]) -> Hash {
    let mut hash = [0u8; 32];
    let mut at = 0usize;
    for part in parts {
        for byte in part.iter().chain(&[0xa5]) {
            hash[at % 32] = hash[at % 32].rotate_left(3) ^ byte.wrapping_add(at as u8);
            at += 1;
        }
    }
    hash
}

/// A link of a node record of the tests.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Link {
    key: Vec<u8>,
    hash: Hash,
    heights: (u8, u8),
    aggregate: Vec<u8>,
}

/// A node record of the tests: what Merk would hand to storage.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rec {
    key: Vec<u8>,
    value: Vec<u8>,
    feature: Vec<u8>,
    value_hash: Hash,
    kv_hash: Hash,
    left: Option<Link>,
    right: Option<Link>,
}

impl Rec {
    /// The put of the record, with its value stated or kept.
    fn put<'a>(&'a self, stated: bool, hosted: Hosted<'a>) -> NodePut<'a> {
        let link = |link: &'a Option<Link>| {
            link.as_ref().map(|link| LinkPut {
                key: &link.key,
                hash: &link.hash,
                heights: link.heights,
                aggregate: &link.aggregate,
            })
        };
        NodePut {
            key: &self.key,
            feature: &self.feature,
            value_hash: &self.value_hash,
            kv_hash: &self.kv_hash,
            left: link(&self.left),
            right: link(&self.right),
            hosted,
            value: match stated {
                true => ValuePut::Value(&self.value),
                false => ValuePut::Keep,
            },
            stale: Stale::NONE,
            at: None,
        }
    }
}

/// The Merk of `items`: its root key, and the record of each node. The
/// shape is a function of the keys, as a balanced tree over them.
fn tree(items: &BTreeMap<Vec<u8>, Vec<u8>>) -> (Option<Vec<u8>>, BTreeMap<Vec<u8>, Rec>) {
    fn build(
        items: &[(&Vec<u8>, &Vec<u8>)],
        out: &mut BTreeMap<Vec<u8>, Rec>,
    ) -> Option<(Link, u8)> {
        if items.is_empty() {
            return None;
        }
        let top = items.len() / 2;
        let (key, value) = items[top];
        let left = build(&items[..top], out);
        let right = build(&items[top + 1..], out);
        let value_hash = hash_of(&[value]);
        let kv_hash = hash_of(&[key, &value_hash]);
        let none = [0u8; 32];
        let hash = hash_of(&[
            &kv_hash,
            left.as_ref().map_or(&none, |(link, _)| &link.hash),
            right.as_ref().map_or(&none, |(link, _)| &link.hash),
        ]);
        let height = |side: &Option<(Link, u8)>| side.as_ref().map_or(0, |(_, height)| *height);
        let heights = (height(&left), height(&right));
        let mut feature = vec![3];
        feature.extend_from_slice(&(items.len() as u64).to_le_bytes());
        let rec = Rec {
            key: key.clone(),
            value: value.clone(),
            feature: feature.clone(),
            value_hash,
            kv_hash,
            left: left.map(|(link, _)| link),
            right: right.map(|(link, _)| link),
        };
        out.insert(key.clone(), rec);
        let link = Link {
            key: key.clone(),
            hash,
            heights,
            aggregate: feature,
        };
        Some((link, heights.0.max(heights.1) + 1))
    }
    let items: Vec<_> = items.iter().collect();
    let mut out = BTreeMap::new();
    let root = build(&items, &mut out).map(|(link, _)| link.key);
    (root, out)
}

fn items(pairs: &[(&[u8], &[u8])]) -> BTreeMap<Vec<u8>, Vec<u8>> {
    (pairs.iter())
        .map(|(key, value)| (key.to_vec(), value.to_vec()))
        .collect()
}

/// A leaf with no child, for the tests that look at one node.
fn leaf(key: &[u8], value: &[u8]) -> Rec {
    let (_, mut nodes) = tree(&items(&[(key, value)]));
    nodes.remove(key).expect("the node")
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}
