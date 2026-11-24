//! A session with a peer that does not serve what its source holds.
//!
//! A case is one session in which one payload is altered: every request
//! before it is answered as the source answers it, the altered payload is
//! given in the place of one answer, and every request after it is
//! answered by the source again, for as long as the session asks and the
//! source serves. The alterations are those of a peer that knows the
//! protocol: a chunk with a node in another form, a value or an element
//! changed under a hash the chunk states for it, a page with other
//! entries, a header with other hashes, parts that are missing or given
//! twice, and bytes changed at random.
//!
//! What is asked of an engine, for every case:
//!
//! - no call panics, and the session ends within a number of requests;
//! - a session that commits ends on the root hash of the source, with the
//!   records of the source, and its verification finds no issue;
//! - a session that was refused a payload refuses the source's own payload
//!   and the commit;
//! - a session that does not commit, and made no commit before its end,
//!   leaves the database as it was, and the closed database passes its
//!   check ([`Side::check`]);
//! - one that made a commit before its end leaves a database that says it
//!   holds an incomplete restore.
//!
//! And of the two engines, where both are compiled in: each case is taken
//! or refused by both at the same request, with the same requests asked
//! after it and the same end.

use std::{
    collections::{BTreeMap, VecDeque},
    panic::{self, AssertUnwindSafe},
};

use grovedb_merk::{
    proofs::{Node, Op},
    tree::value_hash,
    TreeFeatureType,
};
use grovedb_version::version::GroveVersion;

use super::{begin, open, records, whole, Made, Served, Side, PARTS, VERSION};
use crate::{
    replication::{
        indexed_sync::{is_indexed_header_request, IndexedHeader},
        non_merk_sync::{decode_non_merk_page, encode_non_merk_page, supports_entry_replay},
        utils::{
            decode_global_chunk_id, decode_vec_ops, encode_vec_ops, pack_nested_bytes,
            unpack_nested_bytes,
        },
        RestoreCommitMode,
    },
    Element, Engine, GroveDb,
};

/// What the part of one subtree in a payload is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// The chunks of a Merk.
    Chunks,
    /// The pages of a tree that keeps its data as flat records.
    Pages,
    /// The header of an indexed tree, with the first chunk of its primary.
    Header,
}

/// The kind of each part of the payload that answers `request`.
fn kinds(root: &[u8; 32], request: &[u8]) -> Vec<Kind> {
    if request == root {
        return vec![Kind::Chunks];
    }
    let ids = unpack_nested_bytes(request).expect("a packed request");
    let kind = |id: &Vec<u8>| {
        let (_, _, tree_type, local) = decode_global_chunk_id(id, root).expect("a chunk id");
        let header = local
            .first()
            .is_some_and(|id| is_indexed_header_request(id));
        if tree_type.is_indexed_primary() && header {
            Kind::Header
        } else if supports_entry_replay(tree_type) {
            Kind::Pages
        } else {
            Kind::Chunks
        }
    };
    ids.iter().map(kind).collect()
}

/// One way to alter the part of a subtree: its name, the kind of part it
/// is made on (any kind with `None`), and the altered part, or `None` for
/// a part it cannot be made on.
struct Alteration {
    name: &'static str,
    of: Option<Kind>,
    alter: fn(&[u8], &GroveVersion) -> Option<Vec<u8>>,
}

/// The part with its first chunk that holds anything as `change` leaves
/// its ops.
fn on_ops(
    part: &[u8],
    v: &GroveVersion,
    change: impl Fn(&mut Vec<Op>, &GroveVersion) -> bool,
) -> Option<Vec<u8>> {
    let mut chunks = unpack_nested_bytes(part).ok()?;
    let chunk = chunks.iter_mut().find(|chunk| !chunk.is_empty())?;
    let mut ops = decode_vec_ops(chunk).ok()?;
    if !change(&mut ops, v) {
        return None;
    }
    *chunk = encode_vec_ops(ops).ok()?;
    pack_nested_bytes(chunks).ok()
}

/// The part with its first page as `change` leaves it: whether more
/// follows, the side data and the entries.
fn on_page(
    part: &[u8],
    change: impl Fn(&mut bool, &mut Vec<u8>, &mut Vec<Vec<u8>>) -> bool,
) -> Option<Vec<u8>> {
    let mut pages = unpack_nested_bytes(part).ok()?;
    let page = pages.first_mut()?;
    let (mut more, mut aux, mut entries) = decode_non_merk_page(page).ok()?;
    if !change(&mut more, &mut aux, &mut entries) {
        return None;
    }
    *page = encode_non_merk_page(more, aux, entries).ok()?;
    pack_nested_bytes(pages).ok()
}

/// The part with its header and the bytes of the chunk beside it as
/// `change` leaves them.
fn on_header(
    part: &[u8],
    change: impl Fn(&mut IndexedHeader, &mut Vec<u8>) -> bool,
) -> Option<Vec<u8>> {
    let mut pages = unpack_nested_bytes(part).ok()?;
    let page = pages.first_mut()?;
    let [header, mut chunk]: [Vec<u8>; 2] = unpack_nested_bytes(page).ok()?.try_into().ok()?;
    let mut header = IndexedHeader::decode(&header).ok()?;
    if !change(&mut header, &mut chunk) {
        return None;
    }
    *page = pack_nested_bytes(vec![header.encode(), chunk]).ok()?;
    pack_nested_bytes(pages).ok()
}

/// The part with its first chunk, page or header that holds anything as
/// `change` leaves its bytes.
fn on_bytes(part: &[u8], change: impl Fn(&mut Vec<u8>) -> bool) -> Option<Vec<u8>> {
    let mut chunks = unpack_nested_bytes(part).ok()?;
    let chunk = chunks.iter_mut().find(|chunk| !chunk.is_empty())?;
    if !change(chunk) {
        return None;
    }
    pack_nested_bytes(chunks).ok()
}

fn nodes(ops: &mut [Op]) -> impl Iterator<Item = &mut Node> {
    ops.iter_mut().filter_map(|op| match op {
        Op::Push(node) | Op::PushInverted(node) => Some(node),
        _ => None,
    })
}

/// Puts `with` of the first item that a chunk gives with its value alone
/// in the place of that item.
fn restate(ops: &mut [Op], with: impl Fn(Vec<u8>, Vec<u8>) -> Node) -> bool {
    for node in nodes(ops) {
        if let Node::KV(key, value) = node {
            *node = with(std::mem::take(key), std::mem::take(value));
            return true;
        }
    }
    false
}

/// Puts what `change` makes of it in the place of the first element that
/// a chunk gives with a value hash stated beside it and that `change`
/// takes. The stated hash stays.
fn forge(ops: &mut [Op], v: &GroveVersion, change: impl Fn(&Element) -> Option<Element>) -> bool {
    for node in nodes(ops) {
        let value = match node {
            Node::KVValueHash(_, value, _) | Node::KVValueHashFeatureType(_, value, ..) => value,
            _ => continue,
        };
        let Ok(element) = Element::deserialize(value, v) else {
            continue;
        };
        let Some(forged) = change(&element) else {
            continue;
        };
        *value = forged.serialize(v).expect("an element is serialized");
        return true;
    }
    false
}

/// The element of a tree that keeps a Merk, with the root key `change`
/// makes of its own.
fn rekeyed(
    element: &Element,
    change: impl Fn(&Option<Vec<u8>>) -> Option<Vec<u8>>,
) -> Option<Element> {
    let mut forged = element.clone();
    match &mut forged {
        Element::Tree(root, _)
        | Element::SumTree(root, ..)
        | Element::BigSumTree(root, ..)
        | Element::CountTree(root, ..)
        | Element::CountSumTree(root, ..)
        | Element::ProvableCountTree(root, ..)
        | Element::ProvableCountSumTree(root, ..)
        | Element::ProvableSumTree(root, ..)
        | Element::ProvableCountProvableSumTree(root, ..) => *root = change(root),
        _ => return None,
    }
    Some(forged)
}

/// The element of an append-only tree that is said to hold `more` entries
/// more than it does.
fn grown(element: &Element, more: u64) -> Option<Element> {
    let mut forged = element.clone();
    match &mut forged {
        Element::MmrTree(size, _) => *size = size.checked_add(more)?,
        Element::BulkAppendTree(count, ..) | Element::CommitmentTree(count, ..) => {
            *count = count.checked_add(more)?;
        }
        Element::DenseAppendOnlyFixedSizeTree(count, ..) => {
            *count = count.checked_add(u16::try_from(more).ok()?)?;
        }
        _ => return None,
    }
    Some(forged)
}

fn flags_of(element: &Element) -> Option<crate::ElementFlags> {
    element.get_flags().clone()
}

const ALTERATIONS: &[Alteration] = &[
    Alteration {
        name: "no chunk at all",
        of: None,
        alter: |_, _| pack_nested_bytes(Vec::new()).ok(),
    },
    Alteration {
        name: "its first chunk twice",
        of: None,
        alter: |part, _| {
            let mut chunks = unpack_nested_bytes(part).ok()?;
            chunks.push(chunks.first()?.clone());
            pack_nested_bytes(chunks).ok()
        },
    },
    Alteration {
        name: "an empty chunk in the place of its first",
        of: None,
        alter: |part, _| {
            on_bytes(part, |chunk| {
                chunk.clear();
                true
            })
        },
    },
    Alteration {
        name: "a byte more at the end of a chunk",
        of: None,
        alter: |part, _| {
            on_bytes(part, |chunk| {
                chunk.push(0);
                true
            })
        },
    },
    Alteration {
        name: "a chunk without its last byte",
        of: None,
        alter: |part, _| on_bytes(part, |chunk| chunk.pop().is_some()),
    },
    Alteration {
        name: "a chunk without its second half",
        of: None,
        alter: |part, _| {
            on_bytes(part, |chunk| {
                chunk.truncate(chunk.len() / 2);
                true
            })
        },
    },
    Alteration {
        name: "an item with its value hash stated",
        of: Some(Kind::Chunks),
        alter: |part, v| {
            on_ops(part, v, |ops, _| {
                restate(ops, |key, value| {
                    let hash = value_hash(&value).unwrap();
                    Node::KVValueHash(key, value, hash)
                })
            })
        },
    },
    Alteration {
        name: "an item with its feature stated",
        of: Some(Kind::Chunks),
        alter: |part, v| {
            on_ops(part, v, |ops, _| {
                restate(ops, |key, value| {
                    let hash = value_hash(&value).unwrap();
                    Node::KVValueHashFeatureType(key, value, hash, TreeFeatureType::BasicMerkNode)
                })
            })
        },
    },
    Alteration {
        name: "an item as a node of a tree with sums",
        of: Some(Kind::Chunks),
        alter: |part, v| {
            on_ops(part, v, |ops, _| {
                restate(ops, |key, value| {
                    let hash = value_hash(&value).unwrap();
                    Node::KVValueHashFeatureType(
                        key,
                        value,
                        hash,
                        TreeFeatureType::SummedMerkNode(1),
                    )
                })
            })
        },
    },
    Alteration {
        name: "an item with another value under its stated hash",
        of: Some(Kind::Chunks),
        alter: |part, v| {
            on_ops(part, v, |ops, _| {
                restate(ops, |key, mut value| {
                    let hash = value_hash(&value).unwrap();
                    *value.last_mut().expect("a value") ^= 1;
                    Node::KVValueHash(key, value, hash)
                })
            })
        },
    },
    Alteration {
        name: "an item with a longer value under its stated hash",
        of: Some(Kind::Chunks),
        alter: |part, v| {
            on_ops(part, v, |ops, _| {
                restate(ops, |key, mut value| {
                    let hash = value_hash(&value).unwrap();
                    value.extend([0; 300]);
                    Node::KVValueHash(key, value, hash)
                })
            })
        },
    },
    Alteration {
        name: "a node that adds another amount to a sum or a count",
        of: Some(Kind::Chunks),
        alter: |part, v| {
            on_ops(part, v, |ops, _| {
                for node in nodes(ops) {
                    let Node::KVValueHashFeatureType(.., feature) = node else {
                        continue;
                    };
                    match feature {
                        TreeFeatureType::SummedMerkNode(sum) => *sum += 5,
                        TreeFeatureType::BigSummedMerkNode(sum) => *sum += 5,
                        TreeFeatureType::CountedMerkNode(count) => *count += 5,
                        TreeFeatureType::CountedSummedMerkNode(count, _) => *count += 5,
                        _ => continue,
                    }
                    return true;
                }
                false
            })
        },
    },
    Alteration {
        name: "a node of a tree with sums as one of a plain tree",
        of: Some(Kind::Chunks),
        alter: |part, v| {
            on_ops(part, v, |ops, _| {
                for node in nodes(ops) {
                    let Node::KVValueHashFeatureType(.., feature) = node else {
                        continue;
                    };
                    if *feature != TreeFeatureType::BasicMerkNode {
                        *feature = TreeFeatureType::BasicMerkNode;
                        return true;
                    }
                }
                false
            })
        },
    },
    Alteration {
        name: "an amount of a node of a provable tree changed",
        of: Some(Kind::Chunks),
        alter: |part, v| {
            on_ops(part, v, |ops, _| {
                for node in nodes(ops) {
                    match node {
                        Node::KVCount(.., count) => *count += 1,
                        Node::KVSum(.., sum) => *sum += 1,
                        Node::KVCountSum(.., count, _) => *count += 1,
                        _ => continue,
                    }
                    return true;
                }
                false
            })
        },
    },
    Alteration {
        name: "a key changed",
        of: Some(Kind::Chunks),
        alter: |part, v| {
            on_ops(part, v, |ops, _| {
                for node in nodes(ops) {
                    let key = match node {
                        Node::KV(key, _)
                        | Node::KVValueHash(key, ..)
                        | Node::KVValueHashFeatureType(key, ..)
                        | Node::KVCount(key, ..)
                        | Node::KVSum(key, ..) => key,
                        _ => continue,
                    };
                    key.push(b'!');
                    return true;
                }
                false
            })
        },
    },
    Alteration {
        name: "the hash of what hangs below a node changed",
        of: Some(Kind::Chunks),
        alter: |part, v| {
            on_ops(part, v, |ops, _| {
                for node in nodes(ops) {
                    if let Node::Hash(hash) = node {
                        hash[7] ^= 0x10;
                        return true;
                    }
                }
                false
            })
        },
    },
    Alteration {
        name: "the first two ops exchanged",
        of: Some(Kind::Chunks),
        alter: |part, v| {
            on_ops(part, v, |ops, _| {
                if ops.len() < 2 || ops[0] == ops[1] {
                    return false;
                }
                ops.swap(0, 1);
                true
            })
        },
    },
    Alteration {
        name: "a chunk without its last op",
        of: Some(Kind::Chunks),
        alter: |part, v| on_ops(part, v, |ops, _| ops.pop().is_some()),
    },
    Alteration {
        name: "an element with other flags under its stated hash",
        of: Some(Kind::Chunks),
        alter: |part, v| {
            on_ops(part, v, |ops, v| {
                forge(ops, v, |element| {
                    let mut forged = element.clone();
                    let mut flags = flags_of(element).unwrap_or_default();
                    flags.push(7);
                    forged.set_flags(Some(flags));
                    Some(forged)
                })
            })
        },
    },
    Alteration {
        name: "an item of a tree with sums with another amount under its stated hash",
        of: Some(Kind::Chunks),
        alter: |part, v| {
            on_ops(part, v, |ops, v| {
                forge(ops, v, |element| match element {
                    Element::SumItem(sum, flags) => {
                        Some(Element::SumItem(sum + 100, flags.clone()))
                    }
                    _ => None,
                })
            })
        },
    },
    Alteration {
        name: "a tree said to have another root key",
        of: Some(Kind::Chunks),
        alter: |part, v| {
            on_ops(part, v, |ops, v| {
                forge(ops, v, |element| {
                    rekeyed(element, |root| {
                        let mut root = root.clone().unwrap_or_default();
                        root.push(b'x');
                        Some(root)
                    })
                })
            })
        },
    },
    Alteration {
        name: "a tree with nodes said to have no root",
        of: Some(Kind::Chunks),
        alter: |part, v| {
            on_ops(part, v, |ops, v| {
                forge(ops, v, |element| {
                    rekeyed(element, |root| root.is_none().then(|| b"x".to_vec()))
                        .filter(|forged| forged != element)
                        .and(rekeyed(element, |_| None))
                        .filter(|forged| forged != element)
                })
            })
        },
    },
    Alteration {
        name: "a tree said to be of another kind",
        of: Some(Kind::Chunks),
        alter: |part, v| {
            on_ops(part, v, |ops, v| {
                forge(ops, v, |element| match element {
                    Element::Tree(root, flags) => {
                        Some(Element::SumTree(root.clone(), 0, flags.clone()))
                    }
                    Element::SumTree(root, _, flags) | Element::CountTree(root, _, flags) => {
                        Some(Element::Tree(root.clone(), flags.clone()))
                    }
                    _ => None,
                })
            })
        },
    },
    Alteration {
        name: "a tree with a Merk said to be an append-only tree",
        of: Some(Kind::Chunks),
        alter: |part, v| {
            on_ops(part, v, |ops, v| {
                forge(ops, v, |element| match element {
                    Element::Tree(_, flags) => Some(Element::MmrTree(3, flags.clone())),
                    _ => None,
                })
            })
        },
    },
    Alteration {
        name: "an append-only tree said to be a tree with a Merk",
        of: Some(Kind::Chunks),
        alter: |part, v| {
            on_ops(part, v, |ops, v| {
                forge(ops, v, |element| {
                    grown(element, 0).map(|_| Element::Tree(None, flags_of(element)))
                })
            })
        },
    },
    Alteration {
        name: "an append-only tree said to hold one entry more",
        of: Some(Kind::Chunks),
        alter: |part, v| on_ops(part, v, |ops, v| forge(ops, v, |element| grown(element, 1))),
    },
    Alteration {
        name: "an append-only tree said to hold many entries more",
        of: Some(Kind::Chunks),
        alter: |part, v| {
            on_ops(part, v, |ops, v| {
                forge(ops, v, |element| grown(element, 1 << 40))
            })
        },
    },
    Alteration {
        name: "an entry of a page changed",
        of: Some(Kind::Pages),
        alter: |part, _| {
            on_page(part, |_, _, entries| {
                let Some(byte) = entries.first_mut().and_then(|entry| entry.last_mut()) else {
                    return false;
                };
                *byte ^= 1;
                true
            })
        },
    },
    Alteration {
        name: "the last entry of a page twice",
        of: Some(Kind::Pages),
        alter: |part, _| {
            on_page(part, |_, _, entries| {
                let Some(last) = entries.last().cloned() else {
                    return false;
                };
                entries.push(last);
                true
            })
        },
    },
    Alteration {
        name: "a page without its last entry",
        of: Some(Kind::Pages),
        alter: |part, _| on_page(part, |_, _, entries| entries.pop().is_some()),
    },
    Alteration {
        name: "a page that says the other of whether more follows",
        of: Some(Kind::Pages),
        alter: |part, _| {
            on_page(part, |more, _, _| {
                *more = !*more;
                true
            })
        },
    },
    Alteration {
        name: "a page with other side data",
        of: Some(Kind::Pages),
        alter: |part, _| {
            on_page(part, |_, aux, _| {
                match aux.last_mut() {
                    Some(byte) => *byte ^= 1,
                    None => aux.push(1),
                }
                true
            })
        },
    },
    Alteration {
        name: "a page of more entries than a page holds",
        of: Some(Kind::Pages),
        alter: |part, _| {
            on_page(part, |_, _, entries| {
                entries.resize(
                    crate::replication::non_merk_sync::MAX_PAGE_ENTRIES + 1,
                    vec![1],
                );
                true
            })
        },
    },
    Alteration {
        name: "a header with another root hash of the primary",
        of: Some(Kind::Header),
        alter: |part, _| {
            on_header(part, |header, _| {
                header.primary_root_hash[3] ^= 1;
                true
            })
        },
    },
    Alteration {
        name: "a header with another root hash of a secondary",
        of: Some(Kind::Header),
        alter: |part, _| {
            on_header(part, |header, _| {
                let Some((_, hash)) = header.axes.first_mut() else {
                    return false;
                };
                hash[3] ^= 1;
                true
            })
        },
    },
    Alteration {
        name: "a header without its last axis",
        of: Some(Kind::Header),
        alter: |part, _| on_header(part, |header, _| header.axes.pop().is_some()),
    },
    Alteration {
        name: "a header with an axis twice",
        of: Some(Kind::Header),
        alter: |part, _| {
            on_header(part, |header, _| {
                let Some(last) = header.axes.last().copied() else {
                    return false;
                };
                header.axes.push(last);
                true
            })
        },
    },
    Alteration {
        name: "a header with no chunk of the primary beside it",
        of: Some(Kind::Header),
        alter: |part, _| {
            on_header(part, |_, chunk| {
                let held = !chunk.is_empty();
                chunk.clear();
                held
            })
        },
    },
];

/// What the tests' own random numbers come from.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

/// One to three changes of `bytes` at places `rng` chooses: a byte set, a
/// bit turned, a byte put in, a byte taken out, or a few bytes copied from
/// another place.
fn change(bytes: &mut Vec<u8>, rng: &mut Rng) {
    for _ in 0..1 + rng.below(3) {
        if bytes.is_empty() {
            bytes.push(rng.next() as u8);
            continue;
        }
        let at = rng.below(bytes.len());
        match rng.below(5) {
            0 => bytes[at] = rng.next() as u8,
            1 => bytes[at] ^= 1 << rng.below(8),
            2 => bytes.insert(at, rng.next() as u8),
            3 => {
                bytes.remove(at);
            }
            _ => {
                let from = rng.below(bytes.len());
                for offset in 0..1 + rng.below(8) {
                    if at + offset < bytes.len() && from + offset < bytes.len() {
                        bytes[at + offset] = bytes[from + offset];
                    }
                }
            }
        }
    }
}

/// One session with one altered payload.
struct Case {
    name: String,
    /// Which request of the session is answered with the altered payload.
    step: usize,
    payload: Vec<u8>,
}

/// A source with every request of a session against it, in the order the
/// session makes them, each with the payload the source answers it with,
/// and with the records a grove synced from it holds.
struct Honest<S: Engine> {
    source: Made<S>,
    served: Vec<Served>,
    records: Vec<String>,
}

impl<S: Side> Honest<S> {
    fn new(v: &GroveVersion) -> Self {
        let source = Made::<S>::new(v);
        let dir = tempfile::tempdir().expect("a directory");
        let target = open::<S>(dir.path());
        let (served, _) = whole(&source.db, &target, RestoreCommitMode::Atomic, v);
        // The honest sync above proves the source is restorable; the
        // records a committed case must match are the source's own.
        drop(target);
        let records = records(&source.db, &source.shape);
        Self {
            source,
            served,
            records,
        }
    }

    /// The cases of the alterations of the list: each at the first and at
    /// the last part of the session that it can be made on, or with `wide`
    /// at every part.
    fn altered(&self, wide: bool, v: &GroveVersion) -> Vec<Case> {
        let mut cases = Vec::new();
        for alteration in ALTERATIONS {
            let mut placed = Vec::new();
            for (step, (request, payload)) in self.served.iter().enumerate() {
                let parts = unpack_nested_bytes(payload).expect("a packed payload");
                let kinds = kinds(&self.source.root, request);
                assert_eq!(kinds.len(), parts.len());
                for (at, kind) in kinds.iter().enumerate() {
                    if alteration.of.is_some_and(|of| of != *kind) {
                        continue;
                    }
                    let Some(altered) = (alteration.alter)(&parts[at], v) else {
                        continue;
                    };
                    if altered == parts[at] {
                        continue;
                    }
                    let mut parts = parts.clone();
                    parts[at] = altered;
                    placed.push(Case {
                        name: format!(
                            "{}, in part {at} ({kind:?}) of request {step}",
                            alteration.name
                        ),
                        step,
                        payload: pack_nested_bytes(parts).expect("a payload is packed"),
                    });
                }
            }
            assert!(!placed.is_empty(), "no part takes: {}", alteration.name);
            if !wide && placed.len() > 2 {
                let last = placed.pop().expect("a case");
                placed.truncate(1);
                placed.push(last);
            }
            cases.extend(placed);
        }
        cases
    }

    /// The case of `seed`: the payload of a request it chooses, changed at
    /// random as a whole, or in one chunk of one part.
    fn at_random(&self, seed: u64) -> Case {
        let mut rng = Rng(seed);
        let step = rng.below(self.served.len());
        let honest = self.served[step].1.clone();
        let mut payload = honest.clone();
        let whole = rng.below(4) == 0;
        if whole {
            change(&mut payload, &mut rng);
        } else {
            let mut parts = unpack_nested_bytes(&payload).expect("a packed payload");
            let at = rng.below(parts.len());
            let mut chunks = unpack_nested_bytes(&parts[at]).expect("a packed part");
            if chunks.is_empty() {
                chunks.push(Vec::new());
            }
            let chunk = rng.below(chunks.len());
            change(&mut chunks[chunk], &mut rng);
            parts[at] = pack_nested_bytes(chunks).expect("a part is packed");
            payload = pack_nested_bytes(parts).expect("a payload is packed");
        }
        // A change that happened to make the same bytes would test
        // nothing, so a difference is guaranteed.
        if payload == honest {
            match payload.last_mut() {
                Some(byte) => *byte ^= 1,
                None => payload.push(1),
            }
        }
        Case {
            name: format!("seed {seed}, request {step}"),
            step,
            payload,
        }
    }
}

/// How a request after the last one the source answered was answered.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    /// The payload was taken, and these requests were asked for it.
    Taken(Vec<Vec<u8>>),
    Refused,
}

/// How a session ended.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum End {
    /// A payload was refused after its application began: the session is
    /// abandoned and refuses every later call.
    #[default]
    Failed,
    /// A payload was refused before its application began (the decode of
    /// the packed payload, the protocol version or the count of ids): the
    /// session is in the state it was before the call and takes the
    /// source's own payload for the same request. Nothing was applied.
    Resumable,
    /// The session asked for what the source does not serve.
    Starved,
    /// The session asked for more than [`REQUESTS`] times what a session
    /// against the source asks for.
    Endless,
    /// The session had asked for nothing more and was not completed. This
    /// is an allowed end: `is_sync_completed` is false, so the caller makes
    /// no final commit. With no intermediate commit the database matches its
    /// prior root and records; otherwise the incomplete-restore marker must
    /// survive a reopen (the oracle checks whichever of the two holds).
    Stopped,
    /// Every request was answered and the commit was refused.
    CommitRefused,
    Committed,
}

/// How many times the requests of a session against the source a session
/// with an altered payload may ask for before it is said not to end.
const REQUESTS: usize = 4;

/// How one case went on one engine.
#[derive(Clone, Debug, Default)]
struct Went {
    /// The altered request and those after it.
    steps: Vec<Step>,
    end: End,
    /// How many commits the session made before its end.
    commits: usize,
    /// What the engine said where it refused.
    said: Vec<String>,
    /// What is asked of an engine and did not hold.
    broken: Vec<String>,
}

impl Went {
    /// What two engines have to answer alike.
    fn answers(&self) -> (&[Step], &End, usize) {
        (&self.steps, &self.end, self.commits)
    }

    fn in_short(&self) -> String {
        let taken = self
            .steps
            .iter()
            .filter(|step| matches!(step, Step::Taken(_)));
        let said = self
            .said
            .last()
            .map_or(String::new(), |said| format!(": {said}"));
        format!(
            "{:?} after {} payloads taken{said}",
            self.end,
            taken.count()
        )
    }
}

/// Runs `case` against a new database of the engine `T`.
fn went<S: Side, T: Side>(
    honest: &Honest<S>,
    case: &Case,
    mode: RestoreCommitMode,
    v: &GroveVersion,
) -> Went {
    let source = &honest.source;
    let dir = tempfile::tempdir().expect("a directory");
    let target = open::<T>(dir.path());
    let root_of = |db: &GroveDb<T>| db.root_hash(None, v).unwrap().expect("a root hash");
    let before = (root_of(&target), records(&target, &source.shape));
    let mut went = Went::default();
    let mut committed = false;
    {
        let mut session = begin(&target, source.root, mode, v).expect("a session starts");
        let mut asked = VecDeque::from([source.root.to_vec()]);
        for (request, payload) in &honest.served[..case.step] {
            assert_eq!(asked.pop_front().as_ref(), Some(request));
            let more = session.apply_chunk(request, payload, VERSION, v);
            asked.extend(more.expect("the payload of the source is taken"));
        }
        let (request, own) = &honest.served[case.step];
        assert_eq!(asked.pop_front().as_ref(), Some(request));
        let mut next = Some((request.clone(), case.payload.clone(), own.clone()));
        let mut refused = None;
        while let Some((request, payload, own)) = next.take() {
            match session.apply_chunk(&request, &payload, VERSION, v) {
                Ok(more) => {
                    went.steps.push(Step::Taken(more.clone()));
                    asked.extend(more);
                }
                Err(error) => {
                    went.steps.push(Step::Refused);
                    went.said.push(error.to_string());
                    refused = Some((request, own));
                    break;
                }
            }
            let Some(request) = asked.pop_front() else {
                break;
            };
            if went.steps.len() > REQUESTS * honest.served.len() {
                went.end = End::Endless;
                break;
            }
            match source.db.fetch_chunk(&request, None, VERSION, v) {
                Ok(payload) => next = Some((request, payload.clone(), payload)),
                Err(_) => went.end = End::Starved,
            }
        }
        went.commits = session.intermediate_commits();
        if let Some((request, own)) = refused {
            // Probe with the source's own payload for the refused request.
            // A session that was abandoned (the refusal came once the
            // payload was being applied) refuses it; one that was refused
            // before application took it, and takes this.
            let again = session.apply_chunk(&request, &own, VERSION, v);
            if matches!(&again, Err(error) if error.to_string().contains("session has failed")) {
                went.end = End::Failed;
                if target.commit_session(session, v).is_ok() {
                    committed = true;
                    (went.broken).push("a session that failed committed".to_owned());
                }
            } else if let Err(error) = &again {
                (went.broken).push(format!(
                    "a session that refused a payload before applying it then refused the                      source's own payload: {error}"
                ));
                went.end = End::Failed;
                drop(session);
            } else {
                // The honest retry was accepted: the refusal came before
                // the altered payload was applied, so the session is where
                // it was before the call and takes the source's own
                // payload. It is dropped here (the observation is that the
                // retry was accepted, not that the session is driven to a
                // finish); the checks below hold the database to what it
                // was, bar an intermediate commit the retry itself made,
                // which the commit count is re-read for.
                went.end = End::Resumable;
                went.commits = session.intermediate_commits();
                drop(session);
            }
        } else if !matches!(went.end, End::Failed) {
            // Starved or endless: the session is given up.
        } else if !session.is_sync_completed() {
            went.end = End::Stopped;
        } else {
            match target.commit_session(session, v) {
                Ok(()) => {
                    committed = true;
                    went.end = End::Committed;
                }
                Err(error) => {
                    went.end = End::CommitRefused;
                    went.said.push(error.to_string());
                }
            }
        }
    }

    let marked = !committed && went.commits > 0;
    if committed {
        if root_of(&target) != source.root {
            (went.broken).push("the session committed another root hash".to_owned());
        }
        if records(&target, &source.shape) != honest.records {
            (went.broken).push("the session committed other records".to_owned());
        }
        match target.verify_grovedb(None, true, false, v) {
            Ok(issues) if issues.is_empty() => {}
            other => (went.broken).push(format!("the verification says {other:?}")),
        }
    } else if marked {
        if !matches!(target.has_incomplete_restore(), Ok(true)) {
            (went.broken).push("commits of a session that failed are not marked".to_owned());
        }
    } else if (root_of(&target), records(&target, &source.shape)) != before {
        (went.broken).push("a session that did not commit changed the database".to_owned());
    }
    target.flush().expect("a flush");
    drop(target);
    if marked {
        // Such a database is opened to be told so, and closed.
        let again = open::<T>(dir.path());
        if !matches!(again.has_incomplete_restore(), Ok(true)) {
            (went.broken).push("the mark of a restore is gone after a close".to_owned());
        }
        if begin(&again, source.root, mode, v).is_ok() {
            (went.broken).push("a session starts over an incomplete restore".to_owned());
        }
    } else {
        T::check(dir.path(), v);
    }
    if matches!(went.end, End::Endless) {
        (went.broken).push(format!(
            "the session asked for more than {REQUESTS} times the source's requests"
        ));
    }
    went
}

/// [`went`], with a panic of it as something that did not hold.
fn caught<S: Side, T: Side>(
    honest: &Honest<S>,
    case: &Case,
    mode: RestoreCommitMode,
    v: &GroveVersion,
) -> Went {
    let run = AssertUnwindSafe(|| went::<S, T>(honest, case, mode, v));
    panic::catch_unwind(run).unwrap_or_else(|payload| {
        let message = payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| {
                payload
                    .downcast_ref::<&str>()
                    .map(|said| (*said).to_owned())
            })
            .unwrap_or_else(|| "a panic with no message".to_owned());
        Went {
            broken: vec![format!("panicked: {message}")],
            ..Went::default()
        }
    })
}

/// The cases a test runs: those of the list, and those of the seeds that
/// `LIGNIN_HOSTILE_PEER_SEEDS` names as `first..end`, or of a short range.
/// With `LIGNIN_HOSTILE_PEERS=wide` the alterations of the list are made
/// at every part they can be made on. `LIGNIN_HOSTILE_PEER_SHARD=i/n`
/// keeps every `n`th case, from the `i`th on.
fn cases<S: Side>(honest: &Honest<S>, v: &GroveVersion) -> Vec<Case> {
    let wide = std::env::var("LIGNIN_HOSTILE_PEERS").is_ok_and(|said| said == "wide");
    let seeds = match std::env::var("LIGNIN_HOSTILE_PEER_SEEDS") {
        Ok(named) => {
            let (first, end) = named.split_once("..").expect("first..end");
            first.parse().expect("a seed")..end.parse().expect("a seed")
        }
        Err(_) => 0..12,
    };
    let mut cases = honest.altered(wide, v);
    cases.extend(seeds.map(|seed| honest.at_random(seed)));
    if let Ok(shard) = std::env::var("LIGNIN_HOSTILE_PEER_SHARD") {
        let (index, of) = shard.split_once('/').expect("i/n");
        let (index, of): (usize, usize) = (index.parse().expect("i"), of.parse().expect("n"));
        let mut at = 0;
        cases.retain(|_| {
            at += 1;
            (at - 1) % of == index
        });
    }
    cases
}

/// The modes a test runs its cases in: a session that commits once, and
/// with `LIGNIN_HOSTILE_PEERS=wide` also one that commits in parts.
fn modes() -> Vec<(&'static str, RestoreCommitMode)> {
    let mut modes = vec![("once", RestoreCommitMode::Atomic)];
    if std::env::var("LIGNIN_HOSTILE_PEERS").is_ok_and(|said| said == "wide") {
        modes.push(("in parts", PARTS));
    }
    modes
}

/// The two alterations that send a node of a family foreign to the tree
/// (MERK-07's named input: a chunk that hashes right but is refused at a
/// node). A session that takes one must end `Failed` — the refusal comes as
/// the chunk is applied — never `Stopped`; pinned so a regression to a quiet
/// stop is caught.
fn foreign_family(name: &str) -> bool {
    name.starts_with("an item as a node of a tree with sums")
        || name.starts_with("a node of a tree with sums as one of a plain tree")
}

/// Every case on LigninDB, synced from LigninDB: what is asked of an
/// engine holds, and the alterations of the list do not all end one way.
#[test]
fn a_session_with_a_peer_that_alters_a_payload_ends_on_lignindb() {
    type Lignin = super::LigninEngine;
    let v = GroveVersion::latest();
    let honest = Honest::<Lignin>::new(v);
    let cases = cases(&honest, v);
    let mut ends: BTreeMap<String, usize> = BTreeMap::new();
    let mut broken = Vec::new();
    for (mode, commits) in modes() {
        for case in &cases {
            let went = caught::<Lignin, Lignin>(&honest, case, commits, v);
            println!("CASE {mode}: {}: {}", case.name, went.in_short());
            *ends.entry(format!("{:?}", went.end)).or_default() += 1;
            if foreign_family(&case.name) && !matches!(went.end, End::Failed) {
                broken.push(format!(
                    "{mode}: {}: a foreign-family chunk ended {:?}, not Failed",
                    case.name, went.end
                ));
            }
            for what in &went.broken {
                broken.push(format!("{mode}: {}: {what}", case.name));
            }
        }
    }
    println!("{} cases: {ends:?}", cases.len());
    for what in &broken {
        println!("BROKEN {what}");
    }
    assert!(broken.is_empty(), "{} cases did not hold", broken.len());
    // The mix holds over the whole set; a shard may hold one outcome.
    if std::env::var_os("LIGNIN_HOSTILE_PEER_SHARD").is_none() {
        assert!(
            ends.contains_key("Failed") && ends.contains_key("Committed"),
            "{ends:?}"
        );
    }
}

/// Every case on both engines, synced from RocksDB: what is asked of an
/// engine holds on each, and both answer a case alike.
#[cfg(feature = "rocksdb_storage")]
#[test]
fn both_engines_answer_a_peer_that_alters_a_payload_alike() {
    type Lignin = super::LigninEngine;
    type Rocks = crate::RocksDbEngine;
    let v = GroveVersion::latest();
    let honest = Honest::<Rocks>::new(v);
    let cases = cases(&honest, v);
    let mut ends: BTreeMap<String, usize> = BTreeMap::new();
    let (mut broken, mut differ) = (Vec::new(), Vec::new());
    let mut said: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (mode, commits) in modes() {
        for case in &cases {
            let rocks = caught::<Rocks, Rocks>(&honest, case, commits, v);
            let lignin = caught::<Rocks, Lignin>(&honest, case, commits, v);
            println!("CASE {mode}: {}: {}", case.name, lignin.in_short());
            *ends.entry(format!("{:?}", lignin.end)).or_default() += 1;
            if foreign_family(&case.name) && !matches!(lignin.end, End::Failed) {
                broken.push(format!(
                    "{mode}: {} on LigninDB: a foreign-family chunk ended {:?}, not Failed",
                    case.name, lignin.end
                ));
            }
            for (engine, went) in [("RocksDB", &rocks), ("LigninDB", &lignin)] {
                for what in &went.broken {
                    broken.push(format!("{mode}: {} on {engine}: {what}", case.name));
                }
            }
            if rocks.answers() != lignin.answers() {
                differ.push(format!(
                    "{mode}: {}:\n    RocksDB:  {}\n    LigninDB: {}",
                    case.name,
                    rocks.in_short(),
                    lignin.in_short()
                ));
            } else if rocks.said != lignin.said {
                let words = |went: &Went| went.said.last().cloned().unwrap_or_default();
                *said.entry((words(&rocks), words(&lignin))).or_default() += 1;
            }
        }
    }
    println!("{} cases: {ends:?}", cases.len());
    for ((rocks, lignin), cases) in &said {
        println!("SAID in {cases} cases:\n    RocksDB:  {rocks}\n    LigninDB: {lignin}");
    }
    for what in &broken {
        println!("BROKEN {what}");
    }
    for what in &differ {
        println!("DIFFER {what}");
    }
    assert!(broken.is_empty(), "{} cases did not hold", broken.len());
    assert!(
        differ.is_empty(),
        "{} cases are answered otherwise",
        differ.len()
    );
    if std::env::var_os("LIGNIN_HOSTILE_PEER_SHARD").is_none() {
        assert!(
            ends.contains_key("Failed") && ends.contains_key("Committed"),
            "{ends:?}"
        );
    }
}
