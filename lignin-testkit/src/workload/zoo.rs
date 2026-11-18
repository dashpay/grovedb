//! Every element and every tree type, the append-only trees included.
//!
//! The zoo walks each kind of Merk tree as a parent and stores under it every
//! element that parent accepts: plain and flagged items, sum items,
//! references, each tree type as a child, the three wrappers, the
//! backward-reference family and the five append-only trees, which it then
//! appends to. Each parent is filled twice, once through direct calls and
//! once through batches (an indexed parent through batches only, the one way
//! GroveDB writes to it), and a second pass replaces, refreshes and deletes
//! part of what the first stored. What gets stored is fixed by the features;
//! the seed only chooses values, sizes and the order.

use grovedb::{element::reference_path::ReferencePathType, Element, TreeType};
use grovedb_merk::element::tree_type::ElementTreeTypeExtensions;

use super::{
    build::{self, batch, child, path},
    op::{AppendEntry, BatchOp, Op, Path, QueryItem},
    rng::Rng,
    Features, Prepared, Stream,
};

/// The serialized size of the note ciphertext a `CommitmentTree` accepts.
pub const COMMITMENT_PAYLOAD_LEN: u32 = 216;

const ROOT: &[u8] = b"zoo";
const TARGETS: &[u8] = b"targets";
const TARGET_COUNT: u64 = 4;
const BULK_CHUNK_POWER: u8 = 2;
const DENSE_HEIGHT: u8 = 3;
const DENSE_CAPACITY: u32 = 7;
const PRIVATE_DOCUMENT_ENTRY_SIZE: u32 = 16;
const PRIVATE_DOCUMENT_CHUNK_POWER: u8 = 1;
const BATCH_SIZE: usize = 6;

/// The workload producing every element and tree type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ElementZoo {
    /// What the target grove version can store.
    pub features: Features,
}

impl ElementZoo {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        let mut zoo = Zoo {
            features: self.features,
            rng: Rng::derive(seed, DOMAIN, 0),
            ops: Vec::new(),
            entries: Vec::new(),
            parents: Vec::new(),
        };
        zoo.build();
        Prepared::stream(zoo.ops)
    }
}

const DOMAIN: u64 = 0x5A00;

/// A kind of Merk tree: a parent the zoo stores children under, or a child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TreeKind {
    Tree,
    SumTree,
    BigSumTree,
    CountTree,
    CountSumTree,
    ProvableCountTree,
    ProvableCountSumTree,
    ProvableSumTree,
    ProvableCountProvableSumTree,
    ProvableCountProvableBigSumTree,
    SumIndexed,
    CountIndexed,
    CountSumIndexed,
}

impl TreeKind {
    /// Every kind, in the order the zoo fills them as parents.
    pub(crate) const ALL: [TreeKind; 13] = [
        TreeKind::Tree,
        TreeKind::SumTree,
        TreeKind::BigSumTree,
        TreeKind::CountTree,
        TreeKind::CountSumTree,
        TreeKind::ProvableCountTree,
        TreeKind::ProvableCountSumTree,
        TreeKind::ProvableSumTree,
        TreeKind::ProvableCountProvableSumTree,
        TreeKind::ProvableCountProvableBigSumTree,
        TreeKind::SumIndexed,
        TreeKind::CountIndexed,
        TreeKind::CountSumIndexed,
    ];

    /// Every kind, in the order the zoo stores them as children.
    const CHILD_ORDER: [TreeKind; 13] = [
        TreeKind::Tree,
        TreeKind::SumTree,
        TreeKind::BigSumTree,
        TreeKind::CountTree,
        TreeKind::CountSumTree,
        TreeKind::ProvableCountTree,
        TreeKind::ProvableCountSumTree,
        TreeKind::ProvableSumTree,
        TreeKind::ProvableCountProvableSumTree,
        TreeKind::SumIndexed,
        TreeKind::CountIndexed,
        TreeKind::CountSumIndexed,
        TreeKind::ProvableCountProvableBigSumTree,
    ];

    fn name(self) -> &'static str {
        match self {
            TreeKind::Tree => "tree",
            TreeKind::SumTree => "sum",
            TreeKind::BigSumTree => "big-sum",
            TreeKind::CountTree => "count",
            TreeKind::CountSumTree => "count-sum",
            TreeKind::ProvableCountTree => "provable-count",
            TreeKind::ProvableCountSumTree => "provable-count-sum",
            TreeKind::ProvableSumTree => "provable-sum",
            TreeKind::ProvableCountProvableSumTree => "provable-count-provable-sum",
            TreeKind::ProvableCountProvableBigSumTree => "provable-count-provable-big-sum",
            TreeKind::SumIndexed => "sum-indexed",
            TreeKind::CountIndexed => "count-indexed",
            TreeKind::CountSumIndexed => "count-sum-indexed",
        }
    }

    /// The kind as an empty element.
    pub(crate) fn element(self) -> Element {
        match self {
            TreeKind::Tree => Element::empty_tree(),
            TreeKind::SumTree => Element::empty_sum_tree(),
            TreeKind::BigSumTree => Element::empty_big_sum_tree(),
            TreeKind::CountTree => Element::empty_count_tree(),
            TreeKind::CountSumTree => Element::empty_count_sum_tree(),
            TreeKind::ProvableCountTree => Element::empty_provable_count_tree(),
            TreeKind::ProvableCountSumTree => Element::empty_provable_count_sum_tree(),
            TreeKind::ProvableSumTree => Element::empty_provable_sum_tree(),
            TreeKind::ProvableCountProvableSumTree => {
                Element::empty_provable_count_provable_sum_tree()
            }
            TreeKind::ProvableCountProvableBigSumTree => {
                Element::empty_provable_count_provable_big_sum_tree()
            }
            TreeKind::SumIndexed => Element::empty_provable_sum_indexed_tree(),
            TreeKind::CountIndexed => Element::empty_provable_count_indexed_tree(),
            TreeKind::CountSumIndexed => Element::ProvableCountProvableSumIndexedTree(
                None,
                0,
                0,
                vec![(0, None), (1, None), (2, None)],
                None,
            ),
        }
    }

    pub(crate) fn indexed(self) -> bool {
        matches!(
            self,
            TreeKind::SumIndexed | TreeKind::CountIndexed | TreeKind::CountSumIndexed
        )
    }

    /// Whether children add to a sum kept by this tree.
    pub(crate) fn sums(self) -> bool {
        matches!(
            self,
            TreeKind::SumTree
                | TreeKind::BigSumTree
                | TreeKind::CountSumTree
                | TreeKind::ProvableCountSumTree
                | TreeKind::ProvableSumTree
                | TreeKind::ProvableCountProvableSumTree
                | TreeKind::ProvableCountProvableBigSumTree
        )
    }

    /// Whether the aggregates of this tree are left out of its node hashes,
    /// which is what the backward-reference family needs of its parent.
    fn unprovable(self) -> bool {
        matches!(
            self,
            TreeKind::Tree
                | TreeKind::SumTree
                | TreeKind::BigSumTree
                | TreeKind::CountTree
                | TreeKind::CountSumTree
        )
    }

    pub(crate) fn supported(self, features: Features) -> bool {
        self != TreeKind::ProvableCountProvableBigSumTree
            || features.provable_count_provable_big_sum_tree
    }

    /// What a child tree of this kind is filled with.
    fn contents(self) -> Fill {
        match self {
            TreeKind::Tree
            | TreeKind::CountTree
            | TreeKind::ProvableCountTree
            | TreeKind::CountIndexed => Fill::Items,
            TreeKind::SumTree
            | TreeKind::BigSumTree
            | TreeKind::CountSumTree
            | TreeKind::ProvableCountSumTree
            | TreeKind::ProvableSumTree
            | TreeKind::ProvableCountProvableSumTree
            | TreeKind::ProvableCountProvableBigSumTree
            | TreeKind::SumIndexed => Fill::SumItems,
            TreeKind::CountSumIndexed => Fill::ItemsWithSums,
        }
    }
}

/// How a parent is filled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Direct,
    Batch,
}

/// What the second pass may do with a stored child.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Role {
    /// An item-like value: replaced or deleted.
    Value,
    /// A reference: refreshed or deleted.
    Reference,
    /// A Merk subtree, with the kind of element it holds.
    Tree(Fill),
    /// An append-only tree.
    Append(AppendKind),
    /// Something another child points at: left alone.
    Pinned,
}

/// What a child Merk tree is filled with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fill {
    Items,
    SumItems,
    ItemsWithSums,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppendKind {
    Mmr,
    Bulk,
    Dense,
    PrivateDocument,
    Commitment,
}

#[derive(Clone, Debug)]
struct Child {
    key: Vec<u8>,
    element: Element,
    role: Role,
}

#[derive(Clone, Debug)]
struct Entry {
    parent: TreeKind,
    mode: Mode,
    path: Path,
    key: Vec<u8>,
    element: Element,
    role: Role,
    appended: u32,
}

struct Zoo {
    features: Features,
    rng: Rng,
    ops: Vec<Op>,
    entries: Vec<Entry>,
    parents: Vec<(TreeKind, Mode, Path)>,
}

impl Zoo {
    fn build(&mut self) {
        let root = path(&[ROOT]);
        let targets = child(&root, TARGETS);
        self.ops
            .push(build::insert(&[], ROOT, Element::empty_tree()));
        self.ops
            .push(build::insert(&root, TARGETS, Element::empty_tree()));
        for index in 0..TARGET_COUNT {
            let value = self.value(8, 64);
            self.ops.push(build::insert(
                &targets,
                &target_key(index),
                Element::new_item(value),
            ));
        }
        self.ops.push(Op::Commit);
        self.ops.push(build::get(&targets, &target_key(0)));

        for parent in TreeKind::ALL {
            if !parent.supported(self.features) {
                continue;
            }
            if !parent.indexed() {
                self.fill(parent, Mode::Direct);
            }
            self.fill(parent, Mode::Batch);
        }
        for index in 0..self.parents.len() {
            self.revisit(index);
        }
    }

    /// Creates one parent and stores every child it accepts under it.
    fn fill(&mut self, parent: TreeKind, mode: Mode) {
        let root = path(&[ROOT]);
        let name = match mode {
            Mode::Direct => format!("{}:direct", parent.name()),
            Mode::Batch => format!("{}:batch", parent.name()),
        };
        let parent_path = child(&root, name.as_bytes());
        self.parents.push((parent, mode, parent_path.clone()));

        let (free, dependent) = self.children(parent, &parent_path);
        let first = self.entries.len();
        let mut pending = Vec::new();
        match mode {
            Mode::Direct => {
                self.ops
                    .push(build::insert(&root, name.as_bytes(), parent.element()));
            }
            Mode::Batch => {
                pending.push(batch::insert_only(&root, name.as_bytes(), parent.element()));
            }
        }
        for child in free.into_iter().chain(dependent) {
            match mode {
                Mode::Direct => self.ops.push(build::insert(
                    &parent_path,
                    &child.key,
                    child.element.clone(),
                )),
                Mode::Batch => {
                    pending.push(batch::insert_or_replace(
                        &parent_path,
                        &child.key,
                        child.element.clone(),
                    ));
                    if pending.len() >= BATCH_SIZE {
                        self.ops.push(Op::Batch(std::mem::take(&mut pending)));
                    }
                }
            }
            self.entries.push(Entry {
                parent,
                mode,
                path: parent_path.clone(),
                key: child.key,
                element: child.element,
                role: child.role,
                appended: 0,
            });
        }
        if !pending.is_empty() {
            self.ops.push(Op::Batch(pending));
        }
        self.ops.push(Op::Commit);

        for index in first..self.entries.len() {
            self.populate(index);
        }
        self.ops.push(Op::Commit);
        self.read_parent(parent, &parent_path);
    }

    /// The children a parent accepts: those with no dependency, shuffled,
    /// then those that point at a sibling.
    fn children(&mut self, parent: TreeKind, parent_path: &[Vec<u8>]) -> (Vec<Child>, Vec<Child>) {
        let features = self.features;
        let mut free = Vec::new();
        let mut dependent = Vec::new();
        let target = |zoo: &mut Zoo| {
            let index = zoo.rng.below(TARGET_COUNT);
            ReferencePathType::AbsolutePathReference(vec![
                ROOT.to_vec(),
                TARGETS.to_vec(),
                target_key(index),
            ])
        };

        if parent != TreeKind::SumIndexed && parent != TreeKind::CountSumIndexed {
            let value = self.value(0, 96);
            free.push(Child {
                key: b"item".to_vec(),
                element: Element::new_item(value),
                role: Role::Pinned,
            });
            let value = self.value(1, 300);
            let flags = self.flags();
            free.push(Child {
                key: b"item-flagged".to_vec(),
                element: Element::new_item_with_flags(value, flags),
                role: Role::Value,
            });
            free.push(Child {
                key: b"reference".to_vec(),
                element: Element::new_reference(target(self)),
                role: Role::Reference,
            });
            dependent.push(Child {
                key: b"reference-sibling".to_vec(),
                element: Element::new_reference_with_hops(
                    ReferencePathType::SiblingReference(b"item".to_vec()),
                    Some(2),
                ),
                role: Role::Reference,
            });
        }
        if parent.sums() || parent.indexed() && parent != TreeKind::CountIndexed {
            let sum = self.sum();
            if parent != TreeKind::CountSumIndexed || features.sum_item_in_count_sum_indexed_tree {
                free.push(Child {
                    key: b"sum-item".to_vec(),
                    element: Element::new_sum_item(sum),
                    role: Role::Value,
                });
            }
            let value = self.value(1, 80);
            let sum = self.sum();
            let flags = self.flags();
            free.push(Child {
                key: b"item-with-sum".to_vec(),
                element: Element::new_item_with_sum_item_with_flags(value, sum, flags),
                role: Role::Value,
            });
        }
        let sum = self.sum();
        let flags = self.flags();
        free.push(Child {
            key: b"reference-with-sum".to_vec(),
            element: Element::new_reference_with_sum_item_with_max_hops_and_flags(
                target(self),
                Some(1),
                sum,
                flags,
            ),
            role: Role::Reference,
        });

        for kind in TreeKind::CHILD_ORDER {
            if !kind.supported(features) {
                continue;
            }
            let (name, element, fill) = (kind.name(), kind.element(), kind.contents());
            let accepted = match parent {
                TreeKind::SumIndexed => {
                    element.is_sum_bearing_child() && kind != TreeKind::BigSumTree
                }
                TreeKind::CountSumIndexed => element.is_count_and_sum_bearing_child(),
                _ => true,
            };
            if !accepted {
                continue;
            }
            let sum_tree = kind.sums();
            free.push(Child {
                key: format!("tree:{name}").into_bytes(),
                element: element.clone(),
                role: Role::Tree(fill),
            });
            if parent == TreeKind::CountTree || parent == TreeKind::CountSumTree {
                free.push(Child {
                    key: format!("non-counted:{name}").into_bytes(),
                    element: Element::new_non_counted(element.clone())
                        .expect("a tree can be wrapped in NonCounted"),
                    role: Role::Tree(fill),
                });
            }
            if parent.sums() && sum_tree {
                free.push(Child {
                    key: format!("not-summed:{name}").into_bytes(),
                    element: Element::new_not_summed(element.clone())
                        .expect("a sum tree can be wrapped in NotSummed"),
                    role: Role::Tree(fill),
                });
            }
            if parent == TreeKind::CountSumTree && sum_tree {
                free.push(Child {
                    key: format!("not-counted-or-summed:{name}").into_bytes(),
                    element: Element::new_not_counted_or_summed(element.clone())
                        .expect("a sum tree can be wrapped in NotCountedOrSummed"),
                    role: Role::Tree(fill),
                });
            }
        }

        if !matches!(parent, TreeKind::SumIndexed | TreeKind::CountSumIndexed) {
            for (name, element, kind) in self.append_trees() {
                free.push(Child {
                    key: format!("append:{name}").into_bytes(),
                    element: element.clone(),
                    role: Role::Append(kind),
                });
            }
        }

        if parent == TreeKind::CountTree || parent == TreeKind::CountSumTree {
            let value = self.value(1, 40);
            free.push(Child {
                key: b"non-counted:item".to_vec(),
                element: Element::new_non_counted(Element::new_item(value))
                    .expect("an item can be wrapped in NonCounted"),
                role: Role::Value,
            });
            free.push(Child {
                key: b"non-counted:reference".to_vec(),
                element: Element::new_non_counted(Element::new_reference(target(self)))
                    .expect("a reference can be wrapped in NonCounted"),
                role: Role::Reference,
            });
        }
        if parent == TreeKind::CountSumTree {
            let sum = self.sum();
            free.push(Child {
                key: b"non-counted:sum-item".to_vec(),
                element: Element::new_non_counted(Element::new_sum_item(sum))
                    .expect("a sum item can be wrapped in NonCounted"),
                role: Role::Value,
            });
        }

        if features.backward_references && parent.unprovable() {
            let value = self.value(1, 50);
            free.push(Child {
                key: b"referable-item".to_vec(),
                element: Element::new_item_allowing_bidirectional_references(value),
                role: Role::Pinned,
            });
            dependent.push(Child {
                key: b"bidirectional-reference".to_vec(),
                element: Element::new_bidirectional_reference(
                    ReferencePathType::AbsolutePathReference(child(parent_path, b"referable-item")),
                ),
                role: Role::Pinned,
            });
            if parent.sums() {
                let sum = self.sum();
                free.push(Child {
                    key: b"referable-sum-item".to_vec(),
                    element: Element::new_sum_item_allowing_bidirectional_references(sum),
                    role: Role::Pinned,
                });
                let value = self.value(1, 50);
                let sum = self.sum();
                free.push(Child {
                    key: b"referable-item-with-sum".to_vec(),
                    element: Element::new_item_with_sum_item_allowing_bidirectional_references(
                        value, sum,
                    ),
                    role: Role::Pinned,
                });
            }
        }

        self.rng.shuffle(&mut free);
        (free, dependent)
    }

    /// Every append-only tree type as an empty element.
    fn append_trees(&self) -> Vec<(&'static str, Element, AppendKind)> {
        let mut trees = vec![
            ("mmr", Element::empty_mmr_tree(), AppendKind::Mmr),
            (
                "bulk",
                Element::empty_bulk_append_tree(BULK_CHUNK_POWER)
                    .expect("the chunk power is in range"),
                AppendKind::Bulk,
            ),
            (
                "dense",
                Element::empty_dense_tree(DENSE_HEIGHT),
                AppendKind::Dense,
            ),
            (
                "commitment",
                Element::empty_commitment_tree(BULK_CHUNK_POWER)
                    .expect("the chunk power is in range"),
                AppendKind::Commitment,
            ),
        ];
        if self.features.private_document_store {
            trees.push((
                "private-documents",
                Element::empty_private_document_store(
                    PRIVATE_DOCUMENT_ENTRY_SIZE,
                    PRIVATE_DOCUMENT_CHUNK_POWER,
                )
                .expect("the store configuration is in range"),
                AppendKind::PrivateDocument,
            ));
        }
        trees
    }

    /// Fills a freshly stored child tree: elements under a Merk tree,
    /// appends to an append-only one.
    fn populate(&mut self, index: usize) {
        let entry = self.entries[index].clone();
        let tree = child(&entry.path, &entry.key);
        match entry.role {
            Role::Tree(fill) => {
                let indexed = entry.element.underlying().is_indexed_tree();
                let mut pending = Vec::new();
                for slot in 0..2u8 {
                    let key = [b'g', b'0' + slot];
                    let element = self.filler(fill);
                    if indexed || entry.mode == Mode::Batch {
                        pending.push(batch::insert_or_replace(&tree, &key, element));
                    } else {
                        self.ops.push(build::insert(&tree, &key, element));
                    }
                }
                if !pending.is_empty() {
                    self.ops.push(Op::Batch(pending));
                }
            }
            Role::Append(kind) => {
                let count = match kind {
                    AppendKind::Mmr => 5,
                    AppendKind::Bulk => 6,
                    AppendKind::Dense => 4,
                    AppendKind::PrivateDocument => 5,
                    AppendKind::Commitment => {
                        if entry.parent == TreeKind::Tree {
                            5
                        } else {
                            1
                        }
                    }
                };
                self.append(index, count);
            }
            Role::Value | Role::Reference | Role::Pinned => {}
        }
    }

    /// Appends `count` entries to the append-only tree at `index`.
    fn append(&mut self, index: usize, count: u32) {
        let entry = self.entries[index].clone();
        let Role::Append(kind) = entry.role else {
            return;
        };
        let tree = child(&entry.path, &entry.key);
        let per_batch = if self.features.repeated_appends_in_batch {
            2
        } else {
            1
        };
        let mut pending = Vec::new();
        for _ in 0..count {
            let value = self.append_entry(kind);
            if entry.mode == Mode::Batch || kind == AppendKind::Commitment {
                pending.push(batch::append(&tree, value));
                if pending.len() >= per_batch {
                    self.ops.push(Op::Batch(std::mem::take(&mut pending)));
                }
            } else {
                self.ops.push(build::append(&tree, value));
            }
        }
        if !pending.is_empty() {
            self.ops.push(Op::Batch(pending));
        }
        self.entries[index].appended += count;
    }

    fn append_entry(&mut self, kind: AppendKind) -> AppendEntry {
        match kind {
            AppendKind::Mmr => AppendEntry::Mmr(self.value(1, 120)),
            AppendKind::Bulk => AppendEntry::Bulk(self.value(1, 120)),
            AppendKind::Dense => AppendEntry::Dense(self.value(1, 60)),
            AppendKind::PrivateDocument => {
                AppendEntry::PrivateDocument(self.rng.bytes(PRIVATE_DOCUMENT_ENTRY_SIZE))
            }
            AppendKind::Commitment => commitment_entry(&mut self.rng),
        }
    }

    /// The second pass over one parent: replace, refresh, append and delete.
    fn revisit(&mut self, index: usize) {
        let (parent, mode, parent_path) = self.parents[index].clone();
        let batched = mode == Mode::Batch || parent.indexed();
        let parent_type = parent.element().tree_type().expect("a parent is a tree");
        let members: Vec<usize> = (0..self.entries.len())
            .filter(|&entry| self.entries[entry].path == parent_path)
            .collect();
        for member in members {
            let entry = self.entries[member].clone();
            let roll = self.rng.below(12);
            match entry.role {
                Role::Value => {
                    if roll < 5 {
                        let element = self.remade(&entry.element);
                        if batched {
                            self.ops.push(Op::Batch(vec![batch::replace(
                                &entry.path,
                                &entry.key,
                                element,
                            )]));
                        } else {
                            self.ops
                                .push(build::insert(&entry.path, &entry.key, element));
                        }
                    } else if roll < 8 {
                        self.remove(&entry, batched, None);
                    }
                }
                Role::Reference => {
                    if roll < 4 {
                        let (stored, non_counted) = match &entry.element {
                            Element::NonCounted(inner) => (inner.as_ref(), true),
                            stored => (stored, false),
                        };
                        if let Element::Reference(reference, max_hop, flags) = stored {
                            self.ops.push(Op::Batch(vec![BatchOp::RefreshReference {
                                path: entry.path.clone(),
                                key: entry.key.clone(),
                                reference: reference.clone(),
                                max_hop: *max_hop,
                                flags: flags.clone(),
                                non_counted,
                                trusted: roll.is_multiple_of(2),
                            }]));
                        }
                    } else if roll < 7 {
                        self.remove(&entry, batched, None);
                    }
                }
                Role::Tree(fill) => {
                    let tree = child(&entry.path, &entry.key);
                    let indexed = entry.element.underlying().is_indexed_tree();
                    if roll < 4 {
                        let element = self.filler(fill);
                        if batched || indexed {
                            self.ops.push(Op::Batch(vec![batch::insert_or_replace(
                                &tree, b"g2", element,
                            )]));
                        } else {
                            self.ops.push(build::insert(&tree, b"g2", element));
                        }
                    } else if roll < 6 {
                        if batched || indexed {
                            self.ops.push(Op::Batch(vec![batch::delete(&tree, b"g0")]));
                        } else {
                            self.ops.push(build::delete(&tree, b"g0"));
                        }
                    } else if roll < 9 {
                        self.remove_tree(&entry, batched, parent_type);
                    }
                }
                Role::Append(kind) => {
                    if roll < 5 {
                        let count = match kind {
                            AppendKind::Commitment => 1,
                            AppendKind::Dense => (DENSE_CAPACITY - entry.appended).min(3),
                            _ => 3,
                        };
                        self.append(member, count);
                    } else if roll < 7 {
                        self.remove_tree(&entry, batched, parent_type);
                    }
                }
                Role::Pinned => {}
            }
        }
        self.ops.push(Op::Commit);
        self.read_parent(parent, &parent_path);
    }

    fn remove_tree(&mut self, entry: &Entry, batched: bool, parent_type: TreeType) {
        let tree_type = entry.element.tree_type().expect("the entry is a tree");
        let through_batch = batched || !self.features.deletes_tree_directly(parent_type, tree_type);
        self.remove(entry, through_batch, Some(tree_type));
    }

    fn remove(&mut self, entry: &Entry, batched: bool, tree_type: Option<TreeType>) {
        if batched {
            let op = match tree_type {
                Some(tree_type) => batch::delete_tree(&entry.path, &entry.key, tree_type),
                None => batch::delete(&entry.path, &entry.key),
            };
            self.ops.push(Op::Batch(vec![op]));
        } else {
            self.ops.push(build::delete(&entry.path, &entry.key));
        }
    }

    /// Reads a parent back: a slice of the whole tree where the target can
    /// prove that query, the parent element itself where it cannot.
    fn read_parent(&mut self, parent: TreeKind, parent_path: &[Vec<u8>]) {
        let limit = 4 + self.rng.below(5) as u16;
        let host = parent.element().tree_type().expect("a parent is a tree");
        if self.features.proves_queries_in(host) {
            self.ops.push(build::query(build::select_raw(
                parent_path,
                vec![QueryItem::RangeFull],
                Some(limit),
            )));
        } else if let Some((key, root)) = parent_path.split_last() {
            self.ops.push(build::get(root, key));
        }
    }

    /// The same kind of element with new contents.
    fn remade(&mut self, element: &Element) -> Element {
        match element {
            Element::Item(_, flags) => Element::Item(self.value(0, 200), flags.clone()),
            Element::SumItem(_, flags) => Element::SumItem(self.sum(), flags.clone()),
            Element::ItemWithSumItem(_, _, flags) => {
                Element::ItemWithSumItem(self.value(1, 120), self.sum(), flags.clone())
            }
            Element::NonCounted(inner) => Element::new_non_counted(self.remade(inner))
                .expect("the inner element kind is unchanged"),
            other => other.clone(),
        }
    }

    fn filler(&mut self, fill: Fill) -> Element {
        match fill {
            Fill::Items => Element::new_item(self.value(1, 48)),
            Fill::SumItems => Element::new_sum_item(self.sum()),
            Fill::ItemsWithSums => Element::new_item_with_sum_item(self.value(1, 48), self.sum()),
        }
    }

    fn value(&mut self, shortest: u64, longest: u64) -> Vec<u8> {
        build::value(&mut self.rng, shortest, longest)
    }

    fn sum(&mut self) -> i64 {
        build::sum(&mut self.rng)
    }

    fn flags(&mut self) -> Option<Vec<u8>> {
        if self.rng.chance(1, 3) {
            return None;
        }
        let epoch = self.rng.below(512) as u16;
        if self.rng.chance(1, 2) {
            build::storage_flags(epoch, None)
        } else {
            let owner = self.rng.id();
            build::storage_flags(epoch, Some(&owner))
        }
    }
}

fn target_key(index: u64) -> Vec<u8> {
    format!("target-{index}").into_bytes()
}

/// A note for a `CommitmentTree`. The commitment has its two top bits
/// cleared, which keeps it below the Pallas base field modulus.
pub(crate) fn commitment_entry(rng: &mut Rng) -> AppendEntry {
    let mut cmx = rng.id();
    cmx[31] &= 0x3f;
    AppendEntry::Commitment {
        cmx,
        rho: rng.id(),
        cv_net: rng.id(),
        payload: rng.bytes(COMMITMENT_PAYLOAD_LEN),
    }
}
