//! A grove whose writes stop at stop points and are finalized apart, made
//! of the calls GroveDB makes of Merk today.
//!
//! Stop points and finalize are built by a later change. Until then this is
//! what applies a write and a finalize as the rules of
//! `docs/lignindb/cost-model.md`, section 16, say they are charged, so that
//! the rules can be held to numbers. A write is the direct insert of
//! GroveDB without its walk to the root, opened as SP2 says. A finalize is
//! the walk a batch makes for its `ReplaceTreeRootKey` operations, one
//! application for each Merk.

use std::collections::{BTreeMap, BTreeSet};

use grovedb_costs::{
    storage_cost::removal::{
        Identifier,
        StorageRemovedBytes::{BasicStorageRemoval, SectionedStorageRemoval},
    },
    OperationCost,
};
use grovedb_element::indexed::IndexAxis;
use grovedb_merk::{
    cost_model::{propagation_cost_model, Carrier, FinalizeCharge, PropagationCostModel},
    element::{
        costs::ElementCostExtensions, delete::ElementDeleteFromStorageExtensions,
        get::ElementFetchFromStorageExtensions, insert::ElementInsertToStorageExtensions,
        tree_type::ElementTreeTypeExtensions,
    },
    tree::AggregateData,
    tree_type::TreeType,
    CryptoHash, Merk,
};
use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;
use intmap::IntMap;

use crate::{
    batch::NonMerkTreeMeta,
    get_element_from_subtree,
    operations::insert::InsertOptions,
    tests::{SuiteBatch, SuiteEngine},
    update_indexed_tree_item_preserve_flag_into_batch_operations,
    update_tree_item_preserve_flag_into_batch_operations, Element, Engine, GroveDb, Transaction,
};

pub(super) type Path = Vec<Vec<u8>>;

/// What the subtree directory names for a Merk, and what its last write
/// left at its stop point.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Root {
    pub root_key: Option<Vec<u8>>,
    pub root_hash: CryptoHash,
    pub aggregate: AggregateData,
    pub tree_type: TreeType,
    /// For an indexed tree, each axis with the root of its secondary.
    pub axes: Vec<(u8, CryptoHash, Option<Vec<u8>>)>,
    /// For a tree that keeps its data flat, its counts after the write.
    pub counts: Option<NonMerkTreeMeta>,
}

/// One change of a key.
#[derive(Clone, Debug)]
pub(super) enum Change {
    Put(Element),
    Delete,
    /// A removal whose bytes the client's callback gives to an owner
    /// (`cost-model.md`, section 6.6). GroveDB asks the client for the
    /// owner of an element by its flags, and not at all for an element
    /// that has none: the bytes of that one are a plain number.
    DeleteOwnedBy(Identifier),
}

/// The parts of what a write was charged.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct WriteCost {
    /// SP2: the open of each Merk the write changed.
    pub open: OperationCost,
    /// SP1: what the write did inside those Merks.
    pub merks: OperationCost,
    /// SP3: the root of each Merk at its stop point.
    pub roots: OperationCost,
    /// SP3: the marks.
    pub marks: OperationCost,
    /// The batch of the write.
    pub batch: OperationCost,
}

impl WriteCost {
    pub fn total(&self) -> OperationCost {
        self.open.clone()
            + self.merks.clone()
            + self.roots.clone()
            + self.marks.clone()
            + self.batch.clone()
    }
}

pub(super) struct Grove<'db> {
    db: &'db GroveDb<SuiteEngine>,
    tx: &'db Transaction<'db, SuiteEngine>,
    version: &'static GroveVersion,
    model: PropagationCostModel,
    /// The subtree directory, for the Merks that were looked up or written.
    directory: BTreeMap<Path, Root>,
    /// The pending marks: the path of a host's Merk and the host's key.
    marks: BTreeSet<(Path, Vec<u8>)>,
    /// The path of every Merk the last finalize opened, in the order it
    /// opened them.
    pub finalized: Vec<Path>,
    /// The path of every Merk that was opened, by a write or by a finalize.
    opened: Vec<Path>,
}

fn subtree_path(path: &[Vec<u8>]) -> Vec<&[u8]> {
    path.iter().map(Vec::as_slice).collect()
}

impl<'db> Grove<'db> {
    pub fn new(
        db: &'db GroveDb<SuiteEngine>,
        tx: &'db Transaction<'db, SuiteEngine>,
        version: &'static GroveVersion,
    ) -> Self {
        let model = propagation_cost_model(version).expect("a known propagation version");
        assert_eq!(
            model.carrier(),
            Carrier::Finalize,
            "the model applies the rules of a version whose writes stop"
        );
        Grove {
            db,
            tx,
            version,
            model,
            directory: BTreeMap::new(),
            marks: BTreeSet::new(),
            finalized: Vec::new(),
            opened: Vec::new(),
        }
    }

    /// The root that `stopped` leaves a subtree with.
    pub const LEFT_ROOT: CryptoHash = [5; 32];

    /// The root that `stopped` leaves the secondary of the axis `tag` of an
    /// indexed tree with: another for each axis.
    pub fn left_secondary_root(tag: u8) -> CryptoHash {
        [9 + tag; 32]
    }

    pub fn marks(&self) -> &BTreeSet<(Path, Vec<u8>)> {
        &self.marks
    }

    /// Leaves the subtree at `path` as a write that stopped there would:
    /// the root it gives, and a mark on its host. For a tree whose writes
    /// the model does not make: one that keeps its data flat, and an
    /// indexed tree. The roots are made up, for what a carry costs does not
    /// follow a hash. A tree that keeps its data flat is left with one
    /// entry more than its host says.
    pub fn stopped(&mut self, path: &[Vec<u8>], root_key: Option<Vec<u8>>) {
        assert_eq!(self.stop_point(path), path.len(), "SP5");
        let mut root = self.entry(path);
        root.root_key = root_key;
        root.root_hash = Self::LEFT_ROOT;
        root.aggregate = AggregateData::from(root.tree_type.empty_tree_feature_type());
        if root.tree_type.uses_non_merk_data_storage() {
            let (key, parent) = path.split_last().expect("the root Merk has no host");
            let host = self.element(parent, key).expect("the host of the tree");
            root.counts = Some(match host.underlying() {
                // The size of an MMR counts its nodes: that of one leaf
                // more.
                Element::MmrTree(mmr_size, _) => NonMerkTreeMeta::MmrTree {
                    mmr_size: grovedb_merkle_mountain_range::leaf_index_to_mmr_size(
                        grovedb_merkle_mountain_range::mmr_size_to_leaf_count(*mmr_size),
                    ),
                },
                Element::BulkAppendTree(total_count, chunk_power, _) => {
                    NonMerkTreeMeta::BulkAppendTree {
                        total_count: total_count + 1,
                        chunk_power: *chunk_power,
                    }
                }
                Element::DenseAppendOnlyFixedSizeTree(count, height, _) => {
                    NonMerkTreeMeta::DenseTree {
                        count: count + 1,
                        height: *height,
                    }
                }
                Element::CommitmentTree(total_count, chunk_power, _) => {
                    NonMerkTreeMeta::CommitmentTree {
                        total_count: total_count + 1,
                        chunk_power: *chunk_power,
                    }
                }
                Element::PrivateDocumentStore(total_count, entry_size, chunk_power, _) => {
                    NonMerkTreeMeta::PrivateDocumentStore {
                        total_count: total_count + 1,
                        entry_size: *entry_size,
                        chunk_power: *chunk_power,
                    }
                }
                other => unreachable!("{other} keeps its data in a Merk"),
            });
        }
        self.directory.insert(path.to_vec(), root);
        let (key, parent) = path.split_last().expect("the root Merk has no host");
        self.marks.insert((parent.to_vec(), key.clone()));
    }

    /// As `stopped`, for an indexed tree whose secondaries are left with
    /// roots under the keys of `secondaries`, one for each axis it names.
    pub fn stopped_naming(
        &mut self,
        path: &[Vec<u8>],
        root_key: Option<Vec<u8>>,
        secondaries: &[Vec<u8>],
    ) {
        self.stopped(path, root_key);
        let root = self.directory.get_mut(path).expect("the tree was left");
        assert!(secondaries.is_empty() || secondaries.len() == root.axes.len());
        for (axis, secondary) in root.axes.iter_mut().zip(secondaries) {
            axis.2 = Some(secondary.clone());
        }
    }

    /// The aggregate that the write `stopped` stands for left the subtree
    /// at `path` with, where it is not that of a tree with no element.
    pub fn stopped_with(&mut self, path: &[Vec<u8>], aggregate: AggregateData) {
        self.directory
            .get_mut(path)
            .expect("the tree was left")
            .aggregate = aggregate;
    }

    /// SP5: how many segments of `path` lead to the stop point of a write
    /// that changes the subtree at `path`.
    pub fn stop_point(&mut self, path: &[Vec<u8>]) -> usize {
        let mut on_the_path = vec![TreeType::NormalTree];
        for length in 1..=path.len() {
            on_the_path.push(self.entry(&path[..length]).tree_type);
        }
        self.model.stop_point(&on_the_path)
    }

    /// The element under `key` of the Merk at `path`, as the transaction
    /// holds it, read through the root the directory names: a host on the
    /// way may be behind. The model's own look, charged to nobody.
    fn element(&mut self, path: &[Vec<u8>], key: &[u8]) -> Option<Element> {
        let refs = subtree_path(path);
        let storage = self
            .db
            .engine()
            .subtree(SubtreePath::from(refs.as_slice()), None, self.tx)
            .unwrap();
        let merk = if path.is_empty() {
            Merk::open_base(
                storage,
                TreeType::NormalTree,
                Some(&Element::value_defined_cost_for_serialized_value),
                self.version,
            )
        } else {
            let entry = self.entry(path);
            Merk::open_layered_with_root_key(
                storage,
                entry.root_key,
                entry.tree_type,
                Some(&Element::value_defined_cost_for_serialized_value),
                self.version,
            )
        }
        .unwrap()
        .expect("the model opens a Merk");
        Element::get_optional(&merk, key, true, self.version)
            .unwrap()
            .expect("the model reads an element")
    }

    /// SP6: a write that removes or replaces the element under `key` of the
    /// Merk at `path` takes the mark on it, and every mark under it, with
    /// it.
    fn host_removed(&mut self, path: &[Vec<u8>], key: &[u8], cost: &mut OperationCost) {
        let mut removed = path.to_vec();
        removed.push(key.to_vec());
        let before = self.marks.len();
        self.marks.retain(|(host_path, host_key)| {
            let mut host = host_path.clone();
            host.push(host_key.clone());
            !host.starts_with(&removed)
        });
        if self.marks.len() < before {
            *cost += self
                .model
                .marks_under_a_removed_host()
                .expect("a version with marks");
        }
        self.directory.retain(|at, _| !at.starts_with(&removed));
    }

    /// The directory's entry of the Merk at `path`. Before the transaction
    /// wrote the Merk, the entry is what its host names. Looking it up is
    /// the engine's work and is charged nothing (SP2).
    fn entry(&mut self, path: &[Vec<u8>]) -> Root {
        if let Some(root) = self.directory.get(path) {
            return root.clone();
        }
        let (key, parent) = path.split_last().expect("the root Merk has no host");
        let host = self.element(parent, key).expect("the host of a Merk");
        let axis = |tag: u8, secondary: &Option<Vec<u8>>| {
            (tag, Self::left_secondary_root(tag), secondary.clone())
        };
        let axes = match host.underlying() {
            Element::ProvableCountIndexedTree(_, secondary, ..) => {
                vec![axis(IndexAxis::Count.tag(), secondary)]
            }
            Element::ProvableSumIndexedTree(_, secondary, ..) => {
                vec![axis(IndexAxis::Sum.tag(), secondary)]
            }
            Element::ProvableBigSumIndexedTree(_, secondary, ..) => {
                vec![axis(IndexAxis::BigSum.tag(), secondary)]
            }
            Element::ProvableCountProvableSumIndexedTree(_, _, _, axes, _)
            | Element::ProvableCountProvableBigSumIndexedTree(_, _, _, axes, _) => axes
                .iter()
                .map(|(tag, secondary)| axis(*tag, secondary))
                .collect(),
            _ => Vec::new(),
        };
        let (root_key, tree_type) = host
            .root_key_and_tree_type_owned()
            .expect("the host of a Merk is a tree");
        let root = Root {
            root_key,
            // Not known before a write of the Merk leaves it, and not used
            // before then.
            root_hash: [0; 32],
            aggregate: AggregateData::NoAggregateData,
            tree_type,
            axes,
            counts: None,
        };
        self.directory.insert(path.to_vec(), root.clone());
        root
    }

    /// SP2: opens the Merk at `path` by its own path and root.
    fn open<'a>(
        &mut self,
        path: &[Vec<u8>],
        batch: &'a SuiteBatch,
        cost: &mut OperationCost,
    ) -> Merk<<SuiteEngine as Engine>::Subtree<'a>>
    where
        'db: 'a,
    {
        self.opened.push(path.to_vec());
        // The code may read the host, as GroveDB's open does today. The
        // model does, and charges it what the rule says: nothing.
        *cost += self.model.host_read_at_open(self.host_read(path));
        let refs = subtree_path(path);
        let storage = self
            .db
            .engine()
            .subtree(SubtreePath::from(refs.as_slice()), Some(batch), self.tx)
            .unwrap_add_cost(cost);
        if path.is_empty() {
            return Merk::open_base(
                storage,
                TreeType::NormalTree,
                Some(&Element::value_defined_cost_for_serialized_value),
                self.version,
            )
            .unwrap_add_cost(cost)
            .expect("the root Merk opens");
        }
        let entry = self.entry(path);
        Merk::open_layered_with_root_key(
            storage,
            entry.root_key,
            entry.tree_type,
            Some(&Element::value_defined_cost_for_serialized_value),
            self.version,
        )
        .unwrap_add_cost(cost)
        .expect("a Merk opens by its root")
    }

    /// What the open of the Merk at `path` is charged on a grove version
    /// that opens through the host, for the read of the host: the prefix of
    /// the host's path and the read of the element.
    pub fn host_read(&self, path: &[Vec<u8>]) -> OperationCost {
        let Some((key, parent)) = path.split_last() else {
            return OperationCost::default();
        };
        let mut cost = OperationCost::default();
        let parent_refs = subtree_path(parent);
        let storage = self
            .db
            .engine()
            .subtree(SubtreePath::from(parent_refs.as_slice()), None, self.tx)
            .unwrap_add_cost(&mut cost);
        Element::get_from_storage(&storage, key, self.version)
            .unwrap_add_cost(&mut cost)
            .expect("the host of a Merk");
        cost
    }

    /// A write of one key of one Merk, which stops at that Merk: the put of
    /// an element that is no tree with a root, under a key that holds no
    /// tree, or the removal of an element that is there and is no tree.
    ///
    /// One key, because each change is an application of the Merk of its
    /// own, as a direct insert and a direct delete of GroveDB are, and what
    /// one application stages the next does not read before the batch of
    /// the write is applied. A batch changes several keys of a Merk in one
    /// application, which the model does not make.
    pub fn write(&mut self, path: &[Vec<u8>], changes: &[(Vec<u8>, Change)]) -> WriteCost {
        assert_eq!(changes.len(), 1, "the model's write changes one key");
        let (key, change) = &changes[0];
        let held = self.element(path, key);
        // GroveDB's removal of a tree opens it, frees what it holds and
        // follows the referrers of what it held, and its insert over a tree
        // refuses some. The model makes neither: `host_removed_by_a_write`
        // is the fixture of SP6.
        assert!(
            !held.as_ref().is_some_and(Element::is_any_tree),
            "the model's write neither removes a tree nor puts an element over one"
        );
        match change {
            // GroveDB's insert of a tree that names a root opens that Merk
            // to hold the element to it.
            Change::Put(element) => assert!(
                !element
                    .root_key_and_tree_type()
                    .is_some_and(|(root, _)| root.is_some()),
                "the model puts no tree that names a root"
            ),
            // A removal of nothing fails, and a write that fails leaves no
            // mark.
            Change::Delete | Change::DeleteOwnedBy(_) => {
                assert!(held.is_some(), "the model removes an element that is there")
            }
        }
        self.apply(path, changes)
    }

    /// The fixture of SP6, and no write of the model: the tree under `key`
    /// of the Merk at `path` is removed, or `over` is put over it, and the
    /// marks at it and under it go. It answers what the write is charged
    /// for those marks. The rest of what such a write is charged the model
    /// does not have: GroveDB's removal of a tree opens it, frees what it
    /// holds and follows the referrers of what it held, and here the
    /// element of the host is taken out and nothing else. A tree that holds
    /// an element with a list of referrers is refused, because the grove
    /// would not be GroveDB's.
    pub fn host_removed_by_a_write(
        &mut self,
        path: &[Vec<u8>],
        key: &[u8],
        over: Option<Element>,
    ) -> OperationCost {
        assert!(
            self.element(path, key)
                .is_some_and(|held| held.is_any_tree()),
            "the fixture removes a tree"
        );
        let mut host = path.to_vec();
        host.push(key.to_vec());
        assert!(
            self.db
                .backward_reference_participants(&host, self.tx, self.version)
                .unwrap()
                .expect("the tree is read")
                .is_empty(),
            "the model keeps no list of referrers"
        );
        let change = over.map_or(Change::Delete, Change::Put);
        self.apply(path, &[(key.to_vec(), change)]).marks
    }

    fn apply(&mut self, path: &[Vec<u8>], changes: &[(Vec<u8>, Change)]) -> WriteCost {
        // SP2: the Merk a reference's target is in is opened by its own path
        // and root. GroveDB's direct insert opens it through its host, so
        // the model takes the read of the host out of what that charged.
        // It can do so only where the host is not behind. A target in the
        // Merk the write has open is read there by a batch and through a
        // second open by the direct insert, and the model makes neither.
        // It takes out the read of one host: the reference is a plain one,
        // and its target no reference.
        let target_merk: Option<Path> = match &changes[0] {
            (key, Change::Put(Element::Reference(reference, ..))) => {
                let target = reference
                    .clone()
                    .absolute_path(path, Some(key))
                    .expect("the reference has a target");
                let (target_key, merk) = target.split_last().expect("a target has a key");
                assert!(
                    !self
                        .element(merk, target_key)
                        .is_some_and(|target| target.is_reference()),
                    "the model follows one hop of a reference"
                );
                assert_ne!(
                    merk, path,
                    "the model reads no target in the Merk it writes"
                );
                if let Some((host, above)) = merk.split_last() {
                    assert!(
                        !self.marks.contains(&(above.to_vec(), host.clone())),
                        "the model reads no target through a marked host"
                    );
                }
                Some(merk.to_vec())
            }
            (_, Change::Put(element)) => {
                assert!(
                    !element.is_reference(),
                    "the model puts no reference but a plain one"
                );
                None
            }
            _ => None,
        };
        // SP5: under an indexed tree a write carries to the primary, with
        // the rows it moves there. The model carries nothing in a write.
        assert_eq!(
            self.stop_point(path),
            path.len(),
            "a write under an indexed tree does not stop at the Merk it changed"
        );
        // And the model writes no row: a write of the primary itself, which
        // stops where it is, is not one it can make.
        assert!(
            path.is_empty() || !self.entry(path).tree_type.is_indexed_primary(),
            "the model does not write the rows of an indexed tree"
        );
        // Nor an entry of a tree that keeps its data flat, which has no
        // Merk for the write to change.
        assert!(
            path.is_empty() || !self.entry(path).tree_type.uses_non_merk_data_storage(),
            "the model does not write a tree that keeps its data flat"
        );
        // GroveDB's insert and delete keep the list of referrers of an
        // element that has one, and write the referrers, in other Merks
        // too. The model's write is the put or the removal alone.
        let (key, change) = &changes[0];
        let put_keeps_referrers =
            matches!(change, Change::Put(element) if element.supports_backward_references());
        assert!(
            !put_keeps_referrers
                && !self
                    .element(path, key)
                    .is_some_and(|held| held.supports_backward_references()),
            "the model keeps no list of referrers"
        );
        let mut cost = WriteCost::default();
        let batch = SuiteBatch::default();
        let refs = subtree_path(path);
        // Which of the keys hold a tree before the write: the model's own
        // look.
        let hosts: Vec<bool> = changes
            .iter()
            .map(|(key, _)| {
                self.element(path, key)
                    .is_some_and(|element| element.is_any_tree())
            })
            .collect();
        for ((key, _), host) in changes.iter().zip(&hosts) {
            if *host {
                self.host_removed(path, key, &mut cost.marks);
            }
        }
        let root = {
            let mut merk = self.open(path, &batch, &mut cost.open);
            for ((key, change), host) in changes.iter().zip(&hosts) {
                match change {
                    Change::Put(element) => {
                        merk.observe_old_value(
                            key,
                            Some(&Element::value_defined_cost_for_serialized_value),
                            &mut |_, _| {},
                            self.version,
                        )
                        .unwrap_add_cost(&mut cost.merks)
                        .expect("the old value is read");
                        self.db
                            .add_element_to_cached_merk(
                                &mut merk,
                                SubtreePath::from(refs.as_slice()),
                                key,
                                element.clone(),
                                // A put over a tree is one the caller means.
                                InsertOptions {
                                    validate_insertion_does_not_override_tree: !*host,
                                    ..Default::default()
                                },
                                self.tx,
                                &batch,
                                self.version,
                            )
                            .unwrap_add_cost(&mut cost.merks)
                            .expect("the element is put");
                        if let Some(target_merk) = &target_merk {
                            let host_read = self.host_read(target_merk);
                            assert_eq!(host_read.storage_cost, Default::default());
                            cost.merks.seek_count -= host_read.seek_count;
                            cost.merks.storage_loaded_bytes -= host_read.storage_loaded_bytes;
                            cost.merks.hash_node_calls -= host_read.hash_node_calls;
                            cost.merks += self.model.host_read_at_open(host_read);
                        }
                    }
                    Change::Delete => {
                        let in_tree_type = merk.tree_type;
                        Element::delete(&mut merk, key, None, *host, in_tree_type, self.version)
                            .unwrap_add_cost(&mut cost.merks)
                            .expect("the element is deleted");
                    }
                    Change::DeleteOwnedBy(owner) => {
                        let in_tree_type = merk.tree_type;
                        let version = self.version;
                        let owned = |bytes: u32| {
                            let mut by_epoch = IntMap::new();
                            by_epoch.insert(0u16, bytes);
                            SectionedStorageRemoval(BTreeMap::from([(*owner, by_epoch)]))
                        };
                        Element::delete_with_sectioned_removal_bytes(
                            &mut merk,
                            key,
                            None,
                            *host,
                            in_tree_type,
                            &mut |value, key_bytes, value_bytes| {
                                let removed = Element::deserialize(value.as_slice(), version)
                                    .map_err(|error| {
                                        grovedb_merk::Error::ClientCorruptionError(
                                            error.to_string(),
                                        )
                                    })?;
                                Ok(if removed.get_flags().is_some() {
                                    (owned(key_bytes), owned(value_bytes))
                                } else {
                                    (
                                        BasicStorageRemoval(key_bytes),
                                        BasicStorageRemoval(value_bytes),
                                    )
                                })
                            },
                            self.version,
                        )
                        .unwrap_add_cost(&mut cost.merks)
                        .expect("the element is deleted");
                    }
                }
            }
            // SP3: the root Merk has no host, and nobody asks for its root.
            (!path.is_empty()).then(|| {
                let (root_hash, root_key, aggregate) = merk
                    .root_hash_key_and_aggregate_data()
                    .unwrap_add_cost(&mut cost.roots)
                    .expect("a Merk has a root");
                Root {
                    root_key,
                    root_hash,
                    aggregate,
                    tree_type: merk.tree_type,
                    axes: Vec::new(),
                    counts: None,
                }
            })
        };
        self.db
            .engine()
            .apply_batch(batch, self.tx)
            .unwrap_add_cost(&mut cost.batch)
            .expect("the batch of a write is applied");
        if let (Some((key, parent)), Some(root)) = (path.split_last(), root) {
            self.directory.insert(path.to_vec(), root);
            cost.marks += self.model.mark().expect("a version with marks");
            self.marks.insert((parent.to_vec(), key.clone()));
        }
        cost
    }

    /// A finalize: every marked Merk is carried into its host, deepest
    /// first, up to the root of the grove.
    pub fn finalize(&mut self) -> OperationCost {
        let mut charge: FinalizeCharge = self.model.finalize().expect("a version with a finalize");
        let batch = SuiteBatch::default();
        let opened_before = self.opened.len();
        while let Some(host_path) = self
            .marks
            .iter()
            .map(|(path, _)| path)
            .max_by(|a, b| a.len().cmp(&b.len()).then_with(|| b.cmp(a)))
            .cloned()
        {
            let keys: Vec<Vec<u8>> = self
                .marks
                .iter()
                .filter(|(path, _)| *path == host_path)
                .map(|(_, key)| key.clone())
                .collect();
            let mut cost = OperationCost::default();
            let root = {
                let mut merk = self.open(&host_path, &batch, &mut cost);
                let mut operations = Vec::new();
                for key in &keys {
                    let mut below = host_path.clone();
                    below.push(key.clone());
                    let carried = self
                        .directory
                        .get(&below)
                        .cloned()
                        .expect("a marked Merk left its root at its stop point");
                    if carried.tree_type.is_indexed_primary() {
                        update_indexed_tree_item_preserve_flag_into_batch_operations(
                            &merk,
                            key.clone(),
                            carried.root_key,
                            carried.axes,
                            carried.aggregate,
                            carried.root_hash,
                            &mut operations,
                            self.version,
                        )
                        .unwrap_add_cost(&mut cost)
                        .expect("an indexed tree is carried");
                    } else if carried.tree_type.uses_non_merk_data_storage() {
                        // As a batch carries one (`ReplaceNonMerkTreeRoot`):
                        // the element is read for its flags and its wrapper
                        // and made again from the tree's counts.
                        let existing = get_element_from_subtree(&merk, key, self.version)
                            .unwrap_add_cost(&mut cost)
                            .expect("a marked element is there");
                        let counts = carried
                            .counts
                            .as_ref()
                            .expect("a tree that keeps its data flat left its counts");
                        let wrapped = existing.is_non_counted();
                        let host = GroveDb::<SuiteEngine>::rewrap_non_merk_tree_parent_element(
                            counts.to_element(existing.get_flags_owned()),
                            wrapped,
                            self.version,
                        )
                        .expect("the element of the tree is made again");
                        let feature = host
                            .get_feature_type(merk.tree_type)
                            .expect("a tree has a feature in its host");
                        host.insert_subtree_into_batch_operations(
                            key.clone(),
                            carried.root_hash,
                            true,
                            &mut operations,
                            feature,
                            self.version,
                        )
                        .unwrap_add_cost(&mut cost)
                        .expect("a tree that keeps its data flat is carried");
                    } else {
                        update_tree_item_preserve_flag_into_batch_operations(
                            &merk,
                            key.clone(),
                            carried.root_key,
                            carried.root_hash,
                            carried.aggregate,
                            &mut operations,
                            self.version,
                        )
                        .unwrap_add_cost(&mut cost)
                        .expect("a marked element is a tree");
                    }
                }
                let tree_type = merk.tree_type;
                let version = self.version;
                merk.apply_with_specialized_costs::<_, Vec<u8>>(
                    &operations,
                    &[],
                    None,
                    &|key, value| {
                        Element::specialized_costs_for_key_value(
                            key,
                            value,
                            tree_type.inner_node_type(),
                            version,
                        )
                        .map_err(|e| grovedb_merk::Error::ClientCorruptionError(e.to_string()))
                    },
                    Some(&Element::value_defined_cost_for_serialized_value),
                    self.version,
                )
                .unwrap_add_cost(&mut cost)
                .expect("the marked elements are rewritten");
                // The root Merk has no host, and nobody asks for its root.
                (!host_path.is_empty()).then(|| {
                    let (root_hash, root_key, aggregate) = merk
                        .root_hash_key_and_aggregate_data()
                        .unwrap_add_cost(&mut cost)
                        .expect("a Merk has a root");
                    Root {
                        root_key,
                        root_hash,
                        aggregate,
                        tree_type,
                        axes: Vec::new(),
                        counts: None,
                    }
                })
            };
            charge.event(cost).expect("an event of a finalize");
            for key in keys {
                self.marks.remove(&(host_path.clone(), key));
            }
            if let (Some((key, parent)), Some(root)) = (host_path.split_last(), root) {
                self.directory.insert(host_path.clone(), root);
                charge
                    .event(self.model.mark().expect("a version with marks"))
                    .expect("a mark is free");
                self.marks.insert((parent.to_vec(), key.clone()));
            }
        }
        let applied = self
            .db
            .engine()
            .apply_batch(batch, self.tx)
            .cost_as_result()
            .expect("the batch of a finalize is applied");
        charge.event(applied).expect("the batch of a finalize");
        self.finalized = self.opened[opened_before..].to_vec();
        charge.finish()
    }
}
