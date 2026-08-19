//! Building and authenticating a secondary row's resolved-target chain.
//!
//! A canonical secondary row is a one-hop reference to its primary entry,
//! committed as `combine_hash(H(row bytes), primary_node_commitment)`.
//! A chain hands the verifier the pieces it needs to rebuild that
//! commitment and, when the primary entry is itself a reference, to keep
//! rebuilding through to the terminal value.
//!
//! # Why this carries no path proofs
//!
//! Each chain entry's commitment is reconstructed from its own bytes plus
//! the NEXT entry's commitment, and the head's commitment is what the row
//! binds. The row's hash is bound into the secondary root, which the axis
//! verifier binds to the indexed element, which chains to the grove root
//! — so substituting any value anywhere in the chain moves the root and
//! is rejected.
//!
//! This is the same trust model shipped GroveDB reference proofs already
//! use: `KVRefValueHash*` binds a reference's committed target hash to the
//! returned value without separately proving the target's path inclusion.
//! A chain is therefore neither weaker nor stronger than reading the same
//! reference through an ordinary proof, and it costs one value plus a
//! hash per row instead of a full inclusion proof per row.
//!
//! A row whose committed target hash disagrees with what actually lives
//! at the referenced path is stale state, not an unsound proof — and
//! `verify_grovedb`'s stale-target-hash check is what detects that.

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_element::indexed::IndexAxis;
use grovedb_merk::{
    element::get::ElementFetchFromStorageExtensions,
    tree::{axes_digest, combine_hash, combine_hash_three, value_hash},
    CryptoHash,
};
use grovedb_path::{SubtreePath, SubtreePathBuilder};
use grovedb_storage::StorageBatch;
use grovedb_version::version::GroveVersion;

use super::{IndexedTargetChain, IndexedTargetCommitment, IndexedTargetNode};
use crate::{
    operations::MAX_REFERENCE_HOPS, reference_path::path_from_reference_path_type, Element, Error,
    GroveDb, Transaction,
};

/// Derive the commitment shape for one node, reading whatever roots that
/// shape folds in.
fn build_commitment<'db>(
    grovedb: &'db GroveDb,
    element: &Element,
    qualified_path: &[Vec<u8>],
    transaction: &'db Transaction,
    batch: &'db StorageBatch,
    grove_version: &GroveVersion,
) -> CostResult<IndexedTargetCommitment, Error> {
    let mut cost = OperationCost::default();
    let underlying = element.underlying();

    if underlying.is_reference() {
        return Ok(IndexedTargetCommitment::Reference).wrap_with_cost(cost);
    }
    if !underlying.is_any_tree() {
        return Ok(IndexedTargetCommitment::Simple).wrap_with_cost(cost);
    }

    let path_owned: SubtreePathBuilder<Vec<u8>> =
        SubtreePathBuilder::owned_from_iter(qualified_path.iter().cloned());
    let node_path = SubtreePath::from(&path_owned);

    // Read the primary/inner root every tree-ish shape folds in.
    let inner_root = |cost: &mut OperationCost| -> Result<CryptoHash, Error> {
        let merk = grovedb
            .open_transactional_merk_at_path(
                node_path.clone(),
                transaction,
                Some(batch),
                grove_version,
            )
            .unwrap_add_cost(cost)?;
        merk.root_hash_key_and_aggregate_data()
            .unwrap_add_cost(cost)
            .map(|(hash, ..)| hash)
            .map_err(Error::MerkError)
    };

    let commitment = match underlying {
        Element::ProvableCountIndexedTree(_, secondary_root_key, ..)
        | Element::ProvableSumIndexedTree(_, secondary_root_key, ..) => {
            let axis = match underlying {
                Element::ProvableCountIndexedTree(..) => IndexAxis::Count,
                _ => IndexAxis::Sum,
            };
            let primary_root_hash = cost_return_on_error_no_add!(cost, inner_root(&mut cost));
            let secondary = cost_return_on_error!(
                &mut cost,
                grovedb.open_indexed_secondary_at_path(
                    node_path.clone(),
                    axis,
                    secondary_root_key.clone(),
                    transaction,
                    Some(batch),
                    grove_version,
                )
            );
            let (secondary_root_hash, ..) = cost_return_on_error!(
                &mut cost,
                secondary
                    .root_hash_key_and_aggregate_data()
                    .map_err(Error::MerkError)
            );
            IndexedTargetCommitment::IndexedSingle {
                primary_root_hash,
                secondary_root_hash,
            }
        }
        Element::ProvableCountProvableSumIndexedTree(_, _, _, axes, _) => {
            let primary_root_hash = cost_return_on_error_no_add!(cost, inner_root(&mut cost));
            let mut axis_hashes: Vec<(u8, CryptoHash)> = Vec::with_capacity(axes.len());
            for (tag, secondary_root_key) in axes {
                let axis = cost_return_on_error_no_add!(
                    cost,
                    IndexAxis::try_from_tag(*tag).map_err(|e| Error::CorruptedData(format!(
                        "indexed target chain: invalid PCPSIT axis tag: {e}"
                    )))
                );
                let secondary = cost_return_on_error!(
                    &mut cost,
                    grovedb.open_indexed_secondary_at_path(
                        node_path.clone(),
                        axis,
                        secondary_root_key.clone(),
                        transaction,
                        Some(batch),
                        grove_version,
                    )
                );
                let (secondary_root_hash, ..) = cost_return_on_error!(
                    &mut cost,
                    secondary
                        .root_hash_key_and_aggregate_data()
                        .map_err(Error::MerkError)
                );
                axis_hashes.push((*tag, secondary_root_hash));
            }
            IndexedTargetCommitment::IndexedMulti {
                primary_root_hash,
                axes: axis_hashes,
            }
        }
        // Every other tree-ish shape — ordinary/sum/count Merk trees and
        // the non-Merk append trees — commits as
        // `combine_hash(H(value), child_root)`. Non-Merk trees keep their
        // state root in the same position, so they need no special case.
        _ => {
            let child_root = cost_return_on_error_no_add!(cost, inner_root(&mut cost));
            IndexedTargetCommitment::Layered(child_root)
        }
    };
    Ok(commitment).wrap_with_cost(cost)
}

/// Build the chain for one secondary row: the immediate primary entry,
/// then any ordinary reference hops through to the terminal.
pub(crate) fn build_target_chain<'db>(
    grovedb: &'db GroveDb,
    indexed_path: &[Vec<u8>],
    primary_key: &[u8],
    transaction: &'db Transaction,
    batch: &'db StorageBatch,
    grove_version: &GroveVersion,
) -> CostResult<IndexedTargetChain, Error> {
    let mut cost = OperationCost::default();
    let mut qualified_path = indexed_path.to_vec();
    qualified_path.push(primary_key.to_vec());
    let mut visited = std::collections::HashSet::new();
    let mut nodes = Vec::new();

    for _ in 0..=MAX_REFERENCE_HOPS {
        if !visited.insert(qualified_path.clone()) {
            return Err(Error::CyclicReference).wrap_with_cost(cost);
        }
        let Some((key, parent_segments)) = qualified_path.split_last() else {
            return Err(Error::CorruptedPath(
                "indexed target chain resolved an empty path".to_string(),
            ))
            .wrap_with_cost(cost);
        };
        let parent_slices: Vec<&[u8]> = parent_segments.iter().map(Vec::as_slice).collect();
        let parent_merk = cost_return_on_error!(
            &mut cost,
            grovedb.open_transactional_merk_at_path(
                parent_slices.as_slice().into(),
                transaction,
                Some(batch),
                grove_version,
            )
        );
        let element = cost_return_on_error!(
            &mut cost,
            Element::get(&parent_merk, key, true, grove_version).map_err(|e| {
                Error::CorruptedData(format!(
                    "indexed target chain: entry {} is missing: {e}",
                    hex::encode(key)
                ))
            })
        );
        drop(parent_merk);

        let value = cost_return_on_error_no_add!(
            cost,
            element.serialize(grove_version).map_err(|e| {
                Error::CorruptedData(format!("indexed target chain: serializing entry: {e}"))
            })
        );
        let commitment = cost_return_on_error!(
            &mut cost,
            build_commitment(
                grovedb,
                &element,
                &qualified_path,
                transaction,
                batch,
                grove_version,
            )
        );

        let next_path = match element.underlying() {
            Element::Reference(reference_path, ..)
            | Element::ReferenceWithSumItem(reference_path, ..) => {
                let mut current = parent_segments.to_vec();
                current.push(key.clone());
                match path_from_reference_path_type(
                    reference_path.clone(),
                    &current,
                    Some(key.as_slice()),
                ) {
                    Ok(p) => Some(p),
                    Err(e) => return Err(Error::from(e)).wrap_with_cost(cost),
                }
            }
            _ => None,
        };

        nodes.push(IndexedTargetNode { value, commitment });
        match next_path {
            Some(next) => qualified_path = next,
            None => return Ok(IndexedTargetChain { nodes }).wrap_with_cost(cost),
        }
    }

    Err(Error::ReferenceLimit).wrap_with_cost(cost)
}

/// Recompute the committed value hash of one chain entry, given the
/// commitment of the entry that follows it.
fn node_commitment(
    node: &IndexedTargetNode,
    next: Option<&CryptoHash>,
) -> Result<CryptoHash, Error> {
    let serialized_hash = value_hash(&node.value).value().to_owned();
    Ok(match &node.commitment {
        IndexedTargetCommitment::Simple => serialized_hash,
        IndexedTargetCommitment::Layered(child_root) => combine_hash(&serialized_hash, child_root)
            .value()
            .to_owned(),
        IndexedTargetCommitment::IndexedSingle {
            primary_root_hash,
            secondary_root_hash,
        } => combine_hash_three(&serialized_hash, primary_root_hash, secondary_root_hash)
            .value()
            .to_owned(),
        IndexedTargetCommitment::IndexedMulti {
            primary_root_hash,
            axes,
        } => {
            let digest = axes_digest(axes).value().to_owned();
            combine_hash_three(&serialized_hash, primary_root_hash, &digest)
                .value()
                .to_owned()
        }
        IndexedTargetCommitment::Reference => {
            let next = next.ok_or_else(|| {
                Error::CorruptedData(
                    "indexed target chain: a Reference node is the last entry — a reference \
                     commits its target's hash, so the chain must continue"
                        .to_string(),
                )
            })?;
            combine_hash(&serialized_hash, next).value().to_owned()
        }
    })
}

/// Authenticate a chain and return `(immediate primary element, its
/// committed value hash, terminal element)`.
///
/// The caller checks the returned commitment against what the row bound;
/// this function only proves the chain is internally consistent and
/// decodes its ends.
pub(crate) fn authenticate_target_chain(
    chain: &IndexedTargetChain,
    grove_version: &GroveVersion,
) -> Result<(Element, CryptoHash, Element), Error> {
    if chain.nodes.is_empty() {
        return Err(Error::CorruptedData(
            "indexed target chain is empty — every row resolves to at least its immediate \
             primary entry"
                .to_string(),
        ));
    }
    if chain.nodes.len() > MAX_REFERENCE_HOPS + 1 {
        return Err(Error::CorruptedData(format!(
            "indexed target chain has {} entries, above the {MAX_REFERENCE_HOPS}-hop limit",
            chain.nodes.len()
        )));
    }
    // Only the last entry may be a non-reference, and only a reference may
    // be followed — otherwise a prover could append unbound entries after
    // a terminal and pass off the last one as the resolved value.
    for (i, node) in chain.nodes.iter().enumerate() {
        let is_last = i + 1 == chain.nodes.len();
        let is_reference = matches!(node.commitment, IndexedTargetCommitment::Reference);
        if is_reference == is_last {
            return Err(Error::CorruptedData(format!(
                "indexed target chain entry {i} is {} but {} the last — a chain is a run of \
                 references ending in exactly one non-reference terminal",
                if is_reference {
                    "a reference"
                } else {
                    "not a reference"
                },
                if is_last { "is" } else { "is not" }
            )));
        }
    }

    // Fold back-to-front: each entry's commitment needs the next one's.
    let mut next: Option<CryptoHash> = None;
    for node in chain.nodes.iter().rev() {
        next = Some(node_commitment(node, next.as_ref())?);
    }
    let immediate_commitment = next.expect("chain is non-empty");

    let decode = |bytes: &[u8]| -> Result<Element, Error> {
        Element::deserialize(bytes, grove_version).map_err(|e| {
            Error::CorruptedData(format!("indexed target chain: undecodable element: {e}"))
        })
    };
    let immediate = decode(&chain.nodes[0].value)?;
    let terminal = decode(&chain.nodes[chain.nodes.len() - 1].value)?;
    Ok((immediate, immediate_commitment, terminal))
}
