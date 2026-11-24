//! Histories that no workload makes, which a case can take beside the
//! streams of its workloads (GROVE-10).
//!
//! A workload steers around what the callers of GroveDB are told not to
//! do, and around what its grove version leaves wrong. Two histories
//! that a named open difference of the engines begins with
//! ([`super::families`]) are made here instead, each as a stream of its
//! own under root keys of its own, so that a case deals its blocks among
//! those of its workloads as it deals theirs:
//!
//! - [`Scene::PlainDelete`]: a tree that holds an element is removed by a
//!   batch's plain delete, which looks at nothing below its key, and a
//!   tree is made again at the key, with reads of the tree, of the key
//!   it held and of the tree above between the steps. It is a shape
//!   recorded from real blocks: there the tree holds one element, an
//!   empty sum tree under the key `01`, and the batch that makes the
//!   tree puts that element, with operations that declare no check of
//!   backward references. A stream varies what the tree holds, in how
//!   many batches it is made again, what is written into it before the
//!   old key, and whether its batches are of such operations
//!   ([`Op::FlagsBatch`]) or of checked ones ([`Op::Batch`]). Every
//!   grove version.
//! - [`Scene::RootDelete`]: trees at the root path that hold an element
//!   are deleted one by one by `GroveDb::delete`, each delete followed
//!   by reads and by a write. On grove versions 1 to 3 such a delete
//!   leaves the root-key record of the root Merk behind where it changes
//!   that Merk's root, which depends on the keys the case has at the
//!   root. Those versions alone.

use grovedb::Element;
use grovedb_version::version::GroveVersion;

use crate::workload::{BatchOp, Fetch, Op, Path, Query, QueryItem, Read, Rng, Write};

const SCENE: u64 = 0x0073_6365_6e65;

/// A history that no workload makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scene {
    /// A tree that holds an element, removed by a batch's plain delete
    /// and made again.
    PlainDelete,
    /// Trees that hold an element, deleted at the root path by
    /// `GroveDb::delete`.
    RootDelete,
}

impl Scene {
    /// Every scene.
    pub const ALL: [Scene; 2] = [Scene::PlainDelete, Scene::RootDelete];

    /// What a recipe calls the stream of the scene.
    pub fn name(self) -> &'static str {
        match self {
            Scene::PlainDelete => "scene-plain-delete",
            Scene::RootDelete => "scene-root-delete",
        }
    }

    /// Whether a case of `grove_version` can take the scene.
    pub fn is_for(self, grove_version: &GroveVersion) -> bool {
        match self {
            Scene::PlainDelete => true,
            Scene::RootDelete => grove_version.protocol_version <= 3,
        }
    }

    /// The scene a case of `seed` takes on `grove_version`, where `taken`
    /// of `out_of` cases take one: drawn from the seed apart from
    /// everything else of the case, so a case that takes none is the
    /// case it was before there were scenes.
    pub fn draw(
        seed: u64,
        grove_version: &GroveVersion,
        taken: u64,
        out_of: u64,
    ) -> Option<(Scene, Placing, u64)> {
        let mut rng = Rng::derive(seed, SCENE, 0);
        let scenes: Vec<Scene> = Scene::ALL
            .into_iter()
            .filter(|scene| scene.is_for(grove_version))
            .collect();
        if scenes.is_empty() || !rng.chance(taken, out_of.max(1)) {
            return None;
        }
        let scene = scenes[rng.index(scenes.len())];
        let placing = [Placing::First, Placing::Last, Placing::Dealt][rng.index(3)];
        Some((scene, placing, rng.next_u64()))
    }

    /// The stream of the scene for `seed`.
    pub fn stream(self, seed: u64) -> Vec<Op> {
        let mut rng = Rng::derive(seed, SCENE, 1);
        match self {
            Scene::PlainDelete => plain_delete(&mut rng),
            Scene::RootDelete => root_delete(&mut rng),
        }
    }
}

/// Where the stream of a scene goes among the streams of a case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placing {
    /// Before every stream of a workload.
    First,
    /// After every stream of a workload.
    Last,
    /// Its blocks dealt among those of the workloads.
    Dealt,
}

fn joined(path: &[Vec<u8>], key: &[u8]) -> Path {
    let mut joined = path.to_vec();
    joined.push(key.to_vec());
    joined
}

fn insert(path: &[Vec<u8>], key: &[u8], element: Element) -> Op {
    Op::Write(Write::Insert {
        path: path.to_vec(),
        key: key.to_vec(),
        element,
    })
}

fn put(path: &[Vec<u8>], key: &[u8], element: Element) -> BatchOp {
    BatchOp::InsertOrReplace {
        path: path.to_vec(),
        key: key.to_vec(),
        element,
    }
}

fn plain(path: &[Vec<u8>], key: &[u8]) -> BatchOp {
    BatchOp::Delete {
        path: path.to_vec(),
        key: key.to_vec(),
    }
}

fn get(path: &[Vec<u8>], key: &[u8]) -> Op {
    Op::Read(Read::Get {
        path: path.to_vec(),
        key: key.to_vec(),
    })
}

fn has(path: &[Vec<u8>], key: &[u8]) -> Op {
    Op::Read(Read::HasRaw {
        path: path.to_vec(),
        key: key.to_vec(),
    })
}

/// Every element in `path` as stored, and with `below` every element of
/// the trees among them.
fn range(path: &[Vec<u8>], below: bool) -> Op {
    Op::Read(Read::Query(Query {
        path: path.to_vec(),
        items: vec![QueryItem::RangeFull],
        subquery_path: None,
        subquery_all: below,
        limit: None,
        left_to_right: true,
        raw: true,
    }))
}

/// The key under which a tree of [`plain_delete`] holds its element.
const HELD: &[u8] = &[1];
/// Keys on either side of it.
const BESIDE: [&[u8]; 2] = [&[0], &[2]];

/// The reads of the tree under `key` in `parent`: its element, the
/// element under [`HELD`] in it by each way of reading one key, whether
/// it is empty, and a range of it, of the tree above and of both. A
/// stream takes each with one chance in two, and one at least.
fn looked_at(ops: &mut Vec<Op>, rng: &mut Rng, parent: &[Vec<u8>], key: &[u8]) {
    let tree = joined(parent, key);
    let fetch = |fetch, cache| {
        Op::Read(Read::Fetch {
            path: tree.clone(),
            key: HELD.to_vec(),
            fetch,
            cache,
        })
    };
    let reads = [
        get(parent, key),
        has(parent, key),
        has(&tree, HELD),
        get(&tree, HELD),
        fetch(Fetch::Raw, false),
        fetch(Fetch::RawOptional, true),
        Op::Read(Read::IsEmptyTree { path: tree.clone() }),
        range(&tree, false),
        range(parent, false),
        range(parent, true),
    ];
    let sure = rng.index(reads.len());
    for (at, read) in reads.into_iter().enumerate() {
        if at == sure || rng.chance(1, 2) {
            ops.push(read);
        }
    }
}

/// The flags of every element of [`plain_delete`]: storage flags of one
/// epoch and no owner, which the callbacks of an [`Op::FlagsBatch`]
/// take.
fn flags() -> Option<Vec<u8>> {
    Some(vec![0, 0, 4])
}

/// What a tree of [`plain_delete`] holds under [`HELD`]: an empty sum
/// tree in three streams of four.
fn held(rng: &mut Rng) -> Element {
    match rng.index(8) {
        0 => Element::empty_tree_with_flags(flags()),
        1 => Element::new_item_with_flags(rng.bytes(12), flags()),
        _ => Element::empty_sum_tree_with_flags(flags()),
    }
}

/// The stream of [`Scene::PlainDelete`].
///
/// Up to four trees in one tree, which is up to three trees below a root
/// key of the scene, under keys of 2 to 32 bytes. In each of two to four rounds every tree is made
/// with an element under [`HELD`], by one batch or step by step: the
/// tree, sometimes an element beside the key, then the element. Then
/// sometimes an element is put below a held sum tree, and then every
/// tree is removed by one batch: a tree that is emptied first, with one
/// chance in four, after the elements it holds, and the others by a
/// plain delete alone. After every batch its block is committed and the
/// trees are read ([`looked_at`]); one removal in eight is rolled back
/// first and made again. One stream in two applies its batches with
/// operations that declare no check of backward references, and the
/// flags callbacks ([`Op::FlagsBatch`]); the other with checked
/// operations ([`Op::Batch`]).
fn plain_delete(rng: &mut Rng) -> Vec<Op> {
    let unchecked = rng.chance(1, 2);
    let batch = |ops: Vec<BatchOp>| match unchecked {
        true => Op::FlagsBatch(ops),
        false => Op::Batch(ops),
    };
    let a_tree = || Element::empty_tree_with_flags(flags());
    let mut ops = Vec::new();
    let mut parent: Path = Vec::new();
    let depth = rng.between(1, 4);
    for level in 0..depth {
        let key = match level {
            0 => b"removed-by-a-plain-delete".to_vec(),
            _ => vec![b'l', level as u8],
        };
        ops.push(insert(&parent, &key, Element::empty_tree()));
        parent.push(key);
    }
    ops.push(Op::Commit);
    let trees: Vec<Vec<u8>> = (0..rng.between(1, 4))
        .map(|n| {
            let mut key = vec![0; rng.between(2, 32) as usize];
            *key.last_mut().expect("a key of two bytes at least") = n as u8 + 1;
            key
        })
        .collect();
    let look = |ops: &mut Vec<Op>, rng: &mut Rng| {
        ops.push(Op::Commit);
        for tree in &trees {
            looked_at(ops, rng, &parent, tree);
        }
    };
    for _ in 0..rng.between(2, 4) {
        // What each tree holds this round, and whether a sum item is
        // under its held element.
        let mut holds: Vec<(Element, Vec<&[u8]>, bool)> = Vec::new();
        let mut together = Vec::new();
        for key in &trees {
            let (element, inside) = (held(rng), joined(&parent, key));
            let mut beside = Vec::new();
            if rng.chance(1, 2) {
                together.push(put(&parent, key, a_tree()));
                together.push(put(&inside, HELD, element.clone()));
            } else {
                ops.push(batch(vec![put(&parent, key, a_tree())]));
                look(&mut ops, rng);
                for beside_key in BESIDE {
                    if rng.chance(1, 3) {
                        let item = Element::new_item_with_flags(rng.bytes(8), flags());
                        ops.push(batch(vec![put(&inside, beside_key, item)]));
                        beside.push(beside_key);
                        look(&mut ops, rng);
                    }
                }
                ops.push(batch(vec![put(&inside, HELD, element.clone())]));
                look(&mut ops, rng);
            }
            holds.push((element, beside, false));
        }
        if !together.is_empty() {
            ops.push(batch(together));
            look(&mut ops, rng);
        }
        for (key, (element, _, filled)) in trees.iter().zip(&mut holds) {
            if element.is_sum_tree() && rng.chance(1, 2) {
                let below = joined(&joined(&parent, key), HELD);
                let sum = Element::new_sum_item_with_flags(rng.between(1, 9) as i64, flags());
                ops.push(batch(vec![put(&below, &[0xee; 32], sum)]));
                *filled = true;
                look(&mut ops, rng);
            }
        }
        let mut removal = Vec::new();
        for (tree, (_, beside, filled)) in trees.iter().zip(&holds) {
            if rng.chance(1, 4) {
                let inside = joined(&parent, tree);
                if *filled {
                    removal.push(plain(&joined(&inside, HELD), &[0xee; 32]));
                }
                removal.push(plain(&inside, HELD));
                removal.extend(beside.iter().map(|key| plain(&inside, key)));
            }
            removal.push(plain(&parent, tree));
        }
        if rng.chance(1, 8) {
            ops.push(batch(removal.clone()));
            ops.push(Op::Transaction(crate::workload::TransactionOp::Rollback));
            for tree in &trees {
                looked_at(&mut ops, rng, &parent, tree);
            }
        }
        ops.push(batch(removal));
        look(&mut ops, rng);
    }
    ops
}

/// The stream of [`Scene::RootDelete`].
///
/// Three to nine trees at the root path, under keys that begin with a
/// byte of the stream's own and so fall among the root keys of the
/// workloads, each with an item in it. Then the trees are deleted in an
/// order of the stream's own by `GroveDb::delete`, each delete in a
/// block with reads before and after it, and followed by a write: an
/// item into a tree that stands, or a tree at the root. One delete in
/// eight is rolled back and made again.
fn root_delete(rng: &mut Rng) -> Vec<Op> {
    let root: Path = Vec::new();
    let mut trees: Vec<Vec<u8>> = (0..rng.between(3, 9))
        .map(|n| {
            let mut key = vec![rng.below(256) as u8];
            key.extend_from_slice(b"-deleted-at-the-root-");
            key.push(n as u8);
            key
        })
        .collect();
    let mut ops = Vec::new();
    for tree in &trees {
        ops.push(insert(&root, tree, Element::empty_tree()));
        let item = Element::new_item(rng.bytes(8));
        ops.push(insert(&joined(&root, tree), b"held", item));
    }
    ops.push(Op::Commit);
    rng.shuffle(&mut trees);
    let delete = |tree: &[u8]| {
        Op::Write(Write::Delete {
            path: Vec::new(),
            key: tree.to_vec(),
        })
    };
    let mut written = 0u8;
    while let Some(tree) = trees.pop() {
        ops.push(get(&root, &tree));
        if rng.chance(1, 8) {
            ops.push(delete(&tree));
            ops.push(Op::Transaction(crate::workload::TransactionOp::Rollback));
            ops.push(get(&root, &tree));
        }
        ops.push(delete(&tree));
        ops.push(Op::Commit);
        ops.push(get(&root, &tree));
        ops.push(has(&root, &tree));
        ops.push(range(&joined(&root, &tree), false));
        written += 1;
        let write = match trees.first() {
            Some(standing) if rng.chance(2, 3) => insert(
                &joined(&root, standing),
                &[b'w', written],
                Element::new_item(rng.bytes(8)),
            ),
            _ => insert(
                &root,
                &[rng.below(256) as u8, b'-', b'w', written],
                Element::empty_tree(),
            ),
        };
        ops.push(write);
        ops.push(Op::Commit);
        if let Some(standing) = trees.first() {
            ops.push(range(&joined(&root, standing), false));
        }
    }
    ops
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use grovedb_version::version::GROVE_VERSIONS;

    use super::*;
    use crate::workload::TransactionOp;

    /// A scene is for the grove versions its history is ruled on, and a
    /// case takes one by its seed alone: one case in four where the
    /// shape says so, each scene of the version and each placing among
    /// them, and none where the shape takes none.
    #[test]
    fn a_scene_is_drawn_from_the_seed_for_the_grove_versions_it_is_for() {
        let versions = |scene: Scene| -> Vec<u32> {
            let versions = GROVE_VERSIONS.iter().filter(|v| scene.is_for(v));
            versions.map(|v| v.protocol_version).collect()
        };
        assert_eq!(versions(Scene::PlainDelete), [1, 2, 3, 4, 5]);
        assert_eq!(versions(Scene::RootDelete), [1, 2, 3]);

        const SEEDS: u64 = 4000;
        for grove_version in GROVE_VERSIONS.iter() {
            let number = grove_version.protocol_version;
            let mut drawn = BTreeSet::new();
            let mut taken = 0;
            for seed in 0..SEEDS {
                let scene = Scene::draw(seed, grove_version, 1, 4);
                assert_eq!(scene, Scene::draw(seed, grove_version, 1, 4));
                assert_eq!(Scene::draw(seed, grove_version, 0, 4), None);
                if let Some((scene, placing, _)) = scene {
                    assert!(scene.is_for(grove_version));
                    drawn.insert((scene.name(), format!("{placing:?}")));
                    taken += 1;
                }
            }
            let scenes = Scene::ALL
                .iter()
                .filter(|scene| scene.is_for(grove_version));
            assert_eq!(drawn.len(), 3 * scenes.count(), "grove version {number}");
            assert!((800..1200).contains(&taken), "{taken} on {number}");
        }
    }

    fn batch_ops(op: &Op) -> &[BatchOp] {
        match op {
            Op::Batch(ops) | Op::FlagsBatch(ops) => ops,
            _ => &[],
        }
    }

    /// The streams of a plain delete, of 64 seeds. Every stream is a
    /// function of its seed, removes a tree by a plain delete whose key
    /// a batch put a tree under before, and puts a tree under that key
    /// again afterwards. Among them are the kinds that a stream varies:
    /// a tree removed with its element in it and one emptied first, an
    /// element under the held key that is a sum tree, a tree and an
    /// item, a tree made with its element by one batch and step by
    /// step, an element put beside the key before it, an element below
    /// the held sum tree, a removal that is rolled back, and batches of
    /// checked operations and of operations that declare no check.
    #[test]
    fn the_streams_of_a_plain_delete_remove_trees_with_an_element_and_make_them_again() {
        let mut kinds = BTreeSet::new();
        for seed in 0..64 {
            let ops = Scene::PlainDelete.stream(seed);
            assert_eq!(ops, Scene::PlainDelete.stream(seed));
            // The trees: the places that a batch puts an empty tree at.
            let mut trees = BTreeSet::new();
            let mut removed = BTreeSet::new();
            let mut made_again = 0;
            let unchecked = ops.iter().any(|op| matches!(op, Op::FlagsBatch(_)));
            let checked = ops.iter().any(|op| matches!(op, Op::Batch(_)));
            assert!(unchecked != checked, "seed {seed}");
            kinds.insert(match unchecked {
                true => "operations that declare no check",
                false => "checked operations",
            });
            for op in &ops {
                let batch = batch_ops(op);
                for batch_op in batch {
                    match batch_op {
                        BatchOp::InsertOrReplace { path, key, element } => {
                            let place = joined(path, key);
                            assert!(element.get_flags().is_some(), "seed {seed}");
                            if matches!(element, Element::Tree(..)) && key.as_slice() != HELD {
                                made_again += u64::from(removed.contains(&place));
                                trees.insert(place);
                                continue;
                            }
                            if key.as_slice() != HELD {
                                kinds.insert(match path.last() {
                                    Some(last) if last.as_slice() == HELD => "an element below",
                                    _ => "an element beside the key",
                                });
                                continue;
                            }
                            kinds.insert(match element {
                                Element::SumTree(..) => "a sum tree held",
                                Element::Tree(..) => "a tree held",
                                _ => "an item held",
                            });
                            kinds.insert(match batch.len() {
                                1 => "made step by step",
                                _ => "made by one batch",
                            });
                        }
                        BatchOp::Delete { path, key } => {
                            let place = joined(path, key);
                            if !trees.contains(&place) {
                                continue;
                            }
                            removed.insert(place.clone());
                            let emptied = batch.iter().any(|other| {
                                matches!(other, BatchOp::Delete { path, key }
                                    if *path == place && key.as_slice() == HELD)
                            });
                            kinds.insert(match emptied {
                                true => "emptied first",
                                false => "removed with its element",
                            });
                        }
                        other => panic!("seed {seed}: {other:?}"),
                    }
                }
                if matches!(op, Op::Transaction(TransactionOp::Rollback)) {
                    kinds.insert("a removal rolled back");
                }
            }
            assert!((1..=4).contains(&trees.len()), "seed {seed}");
            assert_eq!(removed, trees, "seed {seed}");
            assert!(made_again >= trees.len() as u64, "seed {seed}");
            let reads = ops.iter().filter(|op| matches!(op, Op::Read(_))).count();
            assert!(reads >= 4, "seed {seed}: {reads} reads");
        }
        let expected = [
            "a removal rolled back",
            "a sum tree held",
            "a tree held",
            "an element below",
            "an element beside the key",
            "an item held",
            "checked operations",
            "emptied first",
            "made by one batch",
            "made step by step",
            "operations that declare no check",
            "removed with its element",
        ];
        assert_eq!(kinds.into_iter().collect::<Vec<_>>(), expected);
    }

    /// The streams of a delete at the root path, of 64 seeds. Every
    /// stream is a function of its seed, makes three to nine trees at
    /// the root path with an item in each, deletes each of them there by
    /// `GroveDb::delete`, and follows each delete with a write. Among
    /// them are a write into a tree that stands, a tree made at the
    /// root, and a delete that is rolled back and made again.
    #[test]
    fn the_streams_of_a_root_delete_delete_every_tree_they_made_and_write_after_each() {
        let mut kinds = BTreeSet::new();
        for seed in 0..64 {
            let ops = Scene::RootDelete.stream(seed);
            assert_eq!(ops, Scene::RootDelete.stream(seed));
            let mut made = BTreeSet::new();
            let mut filled = BTreeSet::new();
            let mut deleted = Vec::new();
            let mut deletes = 0;
            let mut rollbacks = 0;
            let mut writes_after = 0;
            for op in &ops {
                match op {
                    Op::Write(Write::Insert { path, key, element }) if deleted.is_empty() => {
                        match path.as_slice() {
                            [] => {
                                assert_eq!(*element, Element::empty_tree());
                                made.insert(key.clone());
                            }
                            [tree] => {
                                assert!(made.contains(tree) && key.as_slice() == b"held");
                                filled.insert(tree.clone());
                            }
                            _ => panic!("seed {seed}: {op:?}"),
                        }
                    }
                    Op::Write(Write::Insert { path, .. }) => {
                        writes_after += 1;
                        kinds.insert(match path.len() {
                            0 => "a tree made at the root",
                            _ => "a write into a tree that stands",
                        });
                    }
                    Op::Write(Write::Delete { path, key }) => {
                        assert!(path.is_empty() && made.contains(key), "seed {seed}");
                        deletes += 1;
                        if deleted.last() != Some(key) {
                            deleted.push(key.clone());
                        }
                    }
                    Op::Transaction(TransactionOp::Rollback) => {
                        rollbacks += 1;
                        kinds.insert("a delete rolled back");
                    }
                    Op::Read(_) | Op::Commit => {}
                    other => panic!("seed {seed}: {other:?}"),
                }
            }
            assert!((3..=9).contains(&made.len()), "seed {seed}");
            assert_eq!(filled, made, "seed {seed}");
            let all: BTreeSet<_> = deleted.iter().cloned().collect();
            assert_eq!((all.len(), &all), (deleted.len(), &made), "seed {seed}");
            assert_eq!(deletes, made.len() + rollbacks, "seed {seed}");
            assert_eq!(writes_after, made.len(), "seed {seed}");
        }
        let expected = [
            "a delete rolled back",
            "a tree made at the root",
            "a write into a tree that stands",
        ];
        assert_eq!(kinds.into_iter().collect::<Vec<_>>(), expected);
    }
}
