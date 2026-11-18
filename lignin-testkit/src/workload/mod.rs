//! Deterministic, seeded workload generators.
//!
//! A workload turns a seed into a stream of [`Op`]s: writes, batches, reads
//! and block boundaries. The golden corpus, the benchmarks, the tree-layout
//! model and the differential fuzzer all consume these streams, so the one
//! property everything rests on is that a seed gives the same stream on any
//! machine. That is why the random numbers come from [`rng`], which pins its
//! algorithm, and why nothing in a generator depends on `usize`, on a float
//! or on the iteration order of a hash map.
//!
//! Two families are provided:
//!
//! - [`drive`]: the shapes Dash Platform's Drive stores. Documents with
//!   index references, identity balances in sum trees, tokens in count
//!   trees, identity keys, the platform state, an index whose keys are
//!   rewritten, documents that expire and indexes that aggregate, writes
//!   that look before they write, identities read through the entry points
//!   Drive answers with, blocks that are rolled back, abandoned and read
//!   under a snapshot, and documents replaced, handed over and deleted
//!   across epochs, with the storage fees that go back to their owners.
//! - [`adversarial`] and [`zoo`]: deep nesting, 255-byte keys, large values,
//!   thousands of tiny subtrees, delete-heavy streams, reference chains,
//!   every element and tree type, the append-only trees included, flat
//!   drops, moves, partial batches and pages over every kind of tree,
//!   indexed trees written entry by entry, every typed read of the
//!   append-only trees, trees served in chunks, storage flags in every
//!   state Drive's callbacks leave them in, and trees deleted with what
//!   they hold by the transaction that filled or rewrote them.
//!
//! Between them they produce the GroveDB operations that reach storage in a
//! way of their own ([`coverage::Operation`]): patch, move, partial batches,
//! aux and meta records, query offsets, conditional subqueries and aggregate
//! queries. They also produce the remaining operations that charge a cost
//! of their own: conditional inserts and deletes, raw existence checks,
//! direct writes to indexed trees, typed reads of the append-only trees,
//! chunk proofs, the remaining query entry points, and transactions that
//! roll back, use savepoints or are read under a snapshot. And they apply
//! batches the way Drive applies every one of its own: with the two
//! callbacks that keep an element's storage flags and split the bytes it
//! frees by owner and epoch.
//!
//! [`Workload`] lists them all. [`apply()`] turns an operation into GroveDB
//! calls, [`coverage`] reports which element types, tree types and
//! operations a stream produced, [`flags`] follows what Drive's callbacks
//! keep in the flags of an element, and [`surface`] names every public
//! operation of GroveDB as produced or as left out, with the reason.

pub mod adversarial;
pub mod apply;
pub mod coverage;
pub mod drive;
pub mod flags;
pub mod op;
pub mod rng;
pub mod surface;
pub mod zoo;

mod build;
mod query_encoding;

use std::collections::VecDeque;

use grovedb::TreeType;
use grovedb_version::version::GroveVersion;

pub use self::{
    apply::{apply, Outcome, Transactions},
    coverage::{Coverage, Operation},
    op::{
        AppendEntry, BatchOp, ChunkIds, Fetch, HandoverBatch, IndexedTree, Op, PartialBatch, Path,
        Prove, Query, QueryItem, Read, StreamDigest, TransactionOp, TreeRead, Write,
    },
    rng::Rng,
};

/// What the target grove version can store.
///
/// A workload leaves out what its features switch off, so a stream generated
/// for a grove version applies to that version without a refused operation.
/// The features are part of a workload's parameters: a different set gives a
/// different stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Features {
    /// `BidirectionalReference` and the three `…WithBackwardsReferences`
    /// items.
    pub backward_references: bool,
    /// `PrivateDocumentStore`.
    pub private_document_store: bool,
    /// `ProvableCountProvableBigSumTree`.
    pub provable_count_provable_big_sum_tree: bool,
    /// A bare `SumItem` under a `ProvableCountProvableSumIndexedTree`.
    pub sum_item_in_count_sum_indexed_tree: bool,
    /// The same append-only tree appended to more than once in one batch.
    pub repeated_appends_in_batch: bool,
    /// `GroveDb::delete` of a populated tree when it or its parent commits
    /// an aggregate into its node hashes (the provable and the indexed
    /// trees). Grove versions 1 to 3 reopen the parent as the child's tree
    /// type: they panic when only the child is of that kind and commit a
    /// link hash no proof verifies against when only the parent is. Both are
    /// kept for replay, so a workload for those versions deletes such a tree
    /// through a batch.
    pub direct_delete_of_provable_trees: bool,
    /// Proofs in the V1 envelope. Without it a query hosted by a
    /// `ProvableCountProvableSumTree` or by an indexed tree is refused, and
    /// the proof of a reference stored in a `ProvableCountSumTree` does not
    /// verify.
    pub v1_proofs: bool,
    /// A batch `Move`.
    pub move_element: bool,
    /// Flat drops, by `GroveDb::drop_flat_subtree` and by a batch
    /// `DeleteTree`: the one way a GroveDB operation writes a meta record.
    pub flat_drop: bool,
    /// The read modes of a query: the axis reads of indexed trees and the
    /// sum-budget read, run and proven as path queries.
    pub read_modes: bool,
    /// `Query::limit`, a limit of its own for each subquery.
    pub instance_limits: bool,
    /// `Query::offset`, a page in count units for each subquery.
    pub instance_offsets: bool,
    /// The range read of a `PrivateDocumentStore` and the proof of its
    /// entries.
    pub private_document_ranges: bool,
    /// An update that stores flags of another length than the stored ones
    /// without the flags callback changing them: an element written
    /// without flags over one with flags, and an update in a partial batch
    /// that keeps the size of an element whose flags hold several epochs.
    /// Grove versions 1 to 3 charge such an update for the length of the
    /// stored flags, and the commit fails with a storage cost mismatch.
    pub flag_length_changes: bool,
    /// An update accounted as a replacement: what the callback of an
    /// [`Op::FlagsHandoverBatch`] answers to a full handover.
    pub full_handovers: bool,
}

impl Features {
    /// Everything the newest grove version stores.
    pub const ALL: Features = Features {
        backward_references: true,
        private_document_store: true,
        provable_count_provable_big_sum_tree: true,
        sum_item_in_count_sum_indexed_tree: true,
        repeated_appends_in_batch: true,
        direct_delete_of_provable_trees: true,
        v1_proofs: true,
        move_element: true,
        flat_drop: true,
        read_modes: true,
        instance_limits: true,
        instance_offsets: true,
        private_document_ranges: true,
        flag_length_changes: true,
        full_handovers: true,
    };

    /// What `grove_version` stores.
    pub fn for_grove_version(grove_version: &GroveVersion) -> Self {
        let grovedb = &grove_version.grovedb_versions;
        let insert = &grovedb.operations.insert;
        let flat_drop = &grovedb.operations.flat_drop;
        let proof = &grovedb.operations.proof;
        let path_query = &grovedb.path_query_methods;
        let private_document_store = &grovedb.operations.private_document_store;
        Features {
            backward_references: grovedb.apply_batch.backward_references_maintenance >= 1
                && insert.insert_on_transaction >= 1,
            private_document_store: private_document_store.element_creation >= 1,
            provable_count_provable_big_sum_tree: insert
                .provable_count_provable_big_sum_tree_creation
                >= 1,
            sum_item_in_count_sum_indexed_tree: insert.validate_indexed_child_for_variant >= 1,
            repeated_appends_in_batch: grovedb.apply_batch.keyless_append_repeats >= 1,
            direct_delete_of_provable_trees: grovedb
                .operations
                .delete
                .delete_internal_on_transaction
                >= 1,
            v1_proofs: proof.prove_query_non_serialized >= 1,
            move_element: grovedb.apply_batch.move_element >= 1,
            flat_drop: flat_drop.drop_flat_subtree >= 1
                && flat_drop.batch_delete_tree_drop_flat >= 1,
            read_modes: path_query.unified_read_mode >= 1
                && proof.axis_descent_in_v1_envelope >= 1
                && proof.sum_budget_in_v1_envelope >= 1,
            instance_limits: path_query.per_instance_query_limits >= 1,
            instance_offsets: path_query.per_instance_query_offsets >= 1,
            private_document_ranges: private_document_store.get_range >= 1
                && private_document_store.range_proof >= 1,
            flag_length_changes: grove_version.merk_versions.tree.just_in_time_value_update >= 1,
            full_handovers: grove_version.merk_versions.tree.just_in_time_value_update >= 2,
        }
    }

    /// Whether a query hosted by a tree of this type proves and verifies
    /// whatever the tree holds.
    pub fn proves_queries_in(&self, host: TreeType) -> bool {
        self.v1_proofs
            || !matches!(
                host,
                TreeType::ProvableCountSumTree
                    | TreeType::ProvableCountProvableSumTree
                    | TreeType::ProvableSumIndexedTree
                    | TreeType::ProvableCountIndexedTree
                    | TreeType::ProvableCountProvableSumIndexedTree
            )
    }

    /// Whether `GroveDb::delete` may remove a populated `child` tree from a
    /// `parent` tree. Where it may not, a batch `DeleteTree` does it.
    pub fn deletes_tree_directly(&self, parent: TreeType, child: TreeType) -> bool {
        self.direct_delete_of_provable_trees
            || !(hashes_aggregates(parent) || hashes_aggregates(child))
    }
}

/// Whether a tree of this type commits an aggregate into its node hashes.
fn hashes_aggregates(tree_type: TreeType) -> bool {
    matches!(
        tree_type,
        TreeType::ProvableCountTree
            | TreeType::ProvableCountSumTree
            | TreeType::ProvableSumTree
            | TreeType::ProvableCountProvableSumTree
            | TreeType::ProvableCountProvableBigSumTree
            | TreeType::ProvableSumIndexedTree
            | TreeType::ProvableCountIndexedTree
            | TreeType::ProvableCountProvableSumIndexedTree
    )
}

/// The operations of one workload for one seed, generated as they are read.
pub struct Stream {
    source: Box<dyn Source + Send>,
    pending: VecDeque<Op>,
    exhausted: bool,
}

impl Stream {
    pub(crate) fn new(source: impl Source + Send + 'static) -> Self {
        Self {
            source: Box::new(source),
            pending: VecDeque::new(),
            exhausted: false,
        }
    }
}

impl Iterator for Stream {
    type Item = Op;

    fn next(&mut self) -> Option<Op> {
        loop {
            if let Some(op) = self.pending.pop_front() {
                return Some(op);
            }
            if self.exhausted {
                return None;
            }
            let mut chunk = Vec::new();
            self.exhausted = !self.source.fill(&mut chunk);
            self.pending.extend(chunk);
        }
    }
}

/// A generator, asked for its stream one chunk at a time so a million-document
/// workload never sits in memory at once.
pub(crate) trait Source {
    /// Appends the next chunk of operations. Returns false once the stream
    /// has ended; a chunk pushed by that last call is still part of it.
    fn fill(&mut self, out: &mut Vec<Op>) -> bool;
}

/// A source whose whole stream is built up front, for the small workloads.
pub(crate) struct Prepared(Option<Vec<Op>>);

impl Prepared {
    pub(crate) fn stream(ops: Vec<Op>) -> Stream {
        Stream::new(Prepared(Some(ops)))
    }
}

impl Source for Prepared {
    fn fill(&mut self, out: &mut Vec<Op>) -> bool {
        if let Some(ops) = self.0.take() {
            out.extend(ops);
        }
        false
    }
}

/// Every workload, at a size of its own choosing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Workload {
    /// [`drive::Documents`].
    Documents(drive::Documents),
    /// [`drive::Balances`].
    Balances(drive::Balances),
    /// [`drive::Tokens`].
    Tokens(drive::Tokens),
    /// [`zoo::ElementZoo`].
    ElementZoo(zoo::ElementZoo),
    /// [`adversarial::DeepNesting`].
    DeepNesting(adversarial::DeepNesting),
    /// [`adversarial::LongKeys`].
    LongKeys(adversarial::LongKeys),
    /// [`adversarial::LargeValues`].
    LargeValues(adversarial::LargeValues),
    /// [`adversarial::TinySubtrees`].
    TinySubtrees(adversarial::TinySubtrees),
    /// [`adversarial::DeleteHeavy`].
    DeleteHeavy(adversarial::DeleteHeavy),
    /// [`adversarial::ReferenceChains`].
    ReferenceChains(adversarial::ReferenceChains),
    /// [`drive::IdentityKeys`].
    IdentityKeys(drive::IdentityKeys),
    /// [`drive::PlatformState`].
    PlatformState(drive::PlatformState),
    /// [`drive::IndexRekey`].
    IndexRekey(drive::IndexRekey),
    /// [`drive::Expiration`].
    Expiration(drive::Expiration),
    /// [`drive::IndexAggregates`].
    IndexAggregates(drive::IndexAggregates),
    /// [`adversarial::FlatDrops`].
    FlatDrops(adversarial::FlatDrops),
    /// [`adversarial::Moves`].
    Moves(adversarial::Moves),
    /// [`adversarial::PartialBatches`].
    PartialBatches(adversarial::PartialBatches),
    /// [`adversarial::Pagination`].
    Pagination(adversarial::Pagination),
    /// [`drive::ConditionalWrites`].
    ConditionalWrites(drive::ConditionalWrites),
    /// [`adversarial::IndexedWrites`].
    IndexedWrites(adversarial::IndexedWrites),
    /// [`adversarial::TypedReads`].
    TypedReads(adversarial::TypedReads),
    /// [`drive::IdentityReads`].
    IdentityReads(drive::IdentityReads),
    /// [`adversarial::ChunkProofs`].
    ChunkProofs(adversarial::ChunkProofs),
    /// [`drive::BlockExecution`].
    BlockExecution(drive::BlockExecution),
    /// [`drive::StorageFees`].
    StorageFees(drive::StorageFees),
    /// [`adversarial::FlagEpochs`].
    FlagEpochs(adversarial::FlagEpochs),
    /// [`adversarial::TreeDeletes`].
    TreeDeletes(adversarial::TreeDeletes),
}

impl Workload {
    /// Every workload at its default size, generated for `features`.
    ///
    /// Each one writes under root keys of its own, so the streams of a suite
    /// can also be applied one after another to the same database. The
    /// order is fixed: a workload added later goes last, so the suite keeps
    /// the streams it had as a prefix.
    pub fn suite(features: Features) -> Vec<Workload> {
        vec![
            Workload::Documents(drive::Documents::default()),
            Workload::Balances(drive::Balances::default()),
            Workload::Tokens(drive::Tokens::default()),
            Workload::ElementZoo(zoo::ElementZoo { features }),
            Workload::DeepNesting(adversarial::DeepNesting::default()),
            Workload::LongKeys(adversarial::LongKeys::default()),
            Workload::LargeValues(adversarial::LargeValues::default()),
            Workload::TinySubtrees(adversarial::TinySubtrees {
                features,
                ..Default::default()
            }),
            Workload::DeleteHeavy(adversarial::DeleteHeavy {
                features,
                ..Default::default()
            }),
            Workload::ReferenceChains(adversarial::ReferenceChains {
                features,
                ..Default::default()
            }),
            Workload::IdentityKeys(drive::IdentityKeys {
                features,
                ..Default::default()
            }),
            Workload::PlatformState(drive::PlatformState::default()),
            Workload::IndexRekey(drive::IndexRekey {
                features,
                ..Default::default()
            }),
            Workload::Expiration(drive::Expiration {
                features,
                ..Default::default()
            }),
            Workload::IndexAggregates(drive::IndexAggregates {
                features,
                ..Default::default()
            }),
            Workload::FlatDrops(adversarial::FlatDrops {
                features,
                ..Default::default()
            }),
            Workload::Moves(adversarial::Moves {
                features,
                ..Default::default()
            }),
            Workload::PartialBatches(adversarial::PartialBatches::default()),
            Workload::Pagination(adversarial::Pagination {
                features,
                ..Default::default()
            }),
            Workload::ConditionalWrites(drive::ConditionalWrites::default()),
            Workload::IndexedWrites(adversarial::IndexedWrites {
                features,
                ..Default::default()
            }),
            Workload::TypedReads(adversarial::TypedReads {
                features,
                ..Default::default()
            }),
            Workload::IdentityReads(drive::IdentityReads {
                features,
                ..Default::default()
            }),
            Workload::ChunkProofs(adversarial::ChunkProofs {
                features,
                ..Default::default()
            }),
            Workload::BlockExecution(drive::BlockExecution::default()),
            Workload::StorageFees(drive::StorageFees {
                features,
                ..Default::default()
            }),
            Workload::FlagEpochs(adversarial::FlagEpochs {
                features,
                ..Default::default()
            }),
            Workload::TreeDeletes(adversarial::TreeDeletes::default()),
        ]
    }

    /// A short name, stable across releases, for file names and reports.
    pub fn name(&self) -> &'static str {
        match self {
            Workload::Documents(_) => "documents",
            Workload::Balances(_) => "balances",
            Workload::Tokens(_) => "tokens",
            Workload::ElementZoo(_) => "element-zoo",
            Workload::DeepNesting(_) => "deep-nesting",
            Workload::LongKeys(_) => "long-keys",
            Workload::LargeValues(_) => "large-values",
            Workload::TinySubtrees(_) => "tiny-subtrees",
            Workload::DeleteHeavy(_) => "delete-heavy",
            Workload::ReferenceChains(_) => "reference-chains",
            Workload::IdentityKeys(_) => "identity-keys",
            Workload::PlatformState(_) => "platform-state",
            Workload::IndexRekey(_) => "index-rekey",
            Workload::Expiration(_) => "expiration",
            Workload::IndexAggregates(_) => "index-aggregates",
            Workload::FlatDrops(_) => "flat-drops",
            Workload::Moves(_) => "moves",
            Workload::PartialBatches(_) => "partial-batches",
            Workload::Pagination(_) => "pagination",
            Workload::ConditionalWrites(_) => "conditional-writes",
            Workload::IndexedWrites(_) => "indexed-writes",
            Workload::TypedReads(_) => "typed-reads",
            Workload::IdentityReads(_) => "identity-reads",
            Workload::ChunkProofs(_) => "chunk-proofs",
            Workload::BlockExecution(_) => "block-execution",
            Workload::StorageFees(_) => "storage-fees",
            Workload::FlagEpochs(_) => "flag-epochs",
            Workload::TreeDeletes(_) => "tree-deletes",
        }
    }

    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        match self {
            Workload::Documents(workload) => workload.stream(seed),
            Workload::Balances(workload) => workload.stream(seed),
            Workload::Tokens(workload) => workload.stream(seed),
            Workload::ElementZoo(workload) => workload.stream(seed),
            Workload::DeepNesting(workload) => workload.stream(seed),
            Workload::LongKeys(workload) => workload.stream(seed),
            Workload::LargeValues(workload) => workload.stream(seed),
            Workload::TinySubtrees(workload) => workload.stream(seed),
            Workload::DeleteHeavy(workload) => workload.stream(seed),
            Workload::ReferenceChains(workload) => workload.stream(seed),
            Workload::IdentityKeys(workload) => workload.stream(seed),
            Workload::PlatformState(workload) => workload.stream(seed),
            Workload::IndexRekey(workload) => workload.stream(seed),
            Workload::Expiration(workload) => workload.stream(seed),
            Workload::IndexAggregates(workload) => workload.stream(seed),
            Workload::FlatDrops(workload) => workload.stream(seed),
            Workload::Moves(workload) => workload.stream(seed),
            Workload::PartialBatches(workload) => workload.stream(seed),
            Workload::Pagination(workload) => workload.stream(seed),
            Workload::ConditionalWrites(workload) => workload.stream(seed),
            Workload::IndexedWrites(workload) => workload.stream(seed),
            Workload::TypedReads(workload) => workload.stream(seed),
            Workload::IdentityReads(workload) => workload.stream(seed),
            Workload::ChunkProofs(workload) => workload.stream(seed),
            Workload::BlockExecution(workload) => workload.stream(seed),
            Workload::StorageFees(workload) => workload.stream(seed),
            Workload::FlagEpochs(workload) => workload.stream(seed),
            Workload::TreeDeletes(workload) => workload.stream(seed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reference_documents_stream_is_generated_as_it_is_read() {
        let mut stream = drive::Documents::reference().stream(1);
        for _ in 0..10_000 {
            stream.next().expect("a million documents are more");
            assert!(stream.pending.len() < 100, "a block was not the chunk");
        }
        assert!(!stream.exhausted);
    }

    #[test]
    fn a_stream_can_be_sent_to_another_thread() {
        fn assert_send<T: Send>() {}
        assert_send::<Stream>();
        assert_send::<Op>();
    }
}
