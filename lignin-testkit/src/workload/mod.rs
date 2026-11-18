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
//!   trees.
//! - [`adversarial`] and [`zoo`]: deep nesting, 255-byte keys, large values,
//!   thousands of tiny subtrees, delete-heavy streams, reference chains, and
//!   every element and tree type, the append-only trees included.
//!
//! [`Workload`] lists them all. [`apply`] turns an operation into GroveDB
//! calls, and [`coverage`] reports which element and tree types a stream
//! produced.

pub mod adversarial;
pub mod apply;
pub mod coverage;
pub mod drive;
pub mod op;
pub mod rng;
pub mod zoo;

mod build;
mod query_encoding;

use std::collections::VecDeque;

use grovedb::TreeType;
use grovedb_version::version::GroveVersion;

pub use self::{
    apply::{apply, Outcome},
    coverage::{Coverage, Operation},
    op::{
        AppendEntry, BatchOp, Op, PartialBatch, Path, Query, QueryItem, Read, StreamDigest, Write,
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
    };

    /// What `grove_version` stores.
    pub fn for_grove_version(grove_version: &GroveVersion) -> Self {
        let grovedb = &grove_version.grovedb_versions;
        let insert = &grovedb.operations.insert;
        let flat_drop = &grovedb.operations.flat_drop;
        let proof = &grovedb.operations.proof;
        let path_query = &grovedb.path_query_methods;
        Features {
            backward_references: grovedb.apply_batch.backward_references_maintenance >= 1
                && insert.insert_on_transaction >= 1,
            private_document_store: grovedb.operations.private_document_store.element_creation >= 1,
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
}

impl Workload {
    /// Every workload at its default size, generated for `features`.
    ///
    /// Each one writes under root keys of its own, so the streams of a suite
    /// can also be applied one after another to the same database.
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
