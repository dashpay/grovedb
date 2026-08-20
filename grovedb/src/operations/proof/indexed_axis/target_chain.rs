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
//! the terminal's, and the head's commitment is what the row binds. The
//! row's hash is bound into the secondary root, which the axis verifier
//! binds to the indexed element, which chains to the grove root — so
//! substituting any value in the chain moves the root and is rejected.
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
//!
//! # Feature split
//!
//! BUILDING a chain reads storage, so it needs `minimal`. AUTHENTICATING
//! one is pure arithmetic over bytes the proof already carries, so it
//! compiles in a verify-only build — which is the whole point: a light
//! client with no Merk must still be able to check a chain.

use grovedb_merk::{
    tree::{axes_digest, combine_hash, combine_hash_three, value_hash},
    CryptoHash,
};
use grovedb_version::version::GroveVersion;

use super::{IndexedTargetChain, IndexedTargetCommitment, IndexedTargetNode};
use crate::{Element, Error};

#[cfg(feature = "minimal")]
mod build {
    use grovedb_costs::{
        cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
    };
    use grovedb_element::indexed::IndexAxis;
    use grovedb_merk::{element::get::ElementFetchFromStorageExtensions, tree::CryptoHash};
    use grovedb_path::{SubtreePath, SubtreePathBuilder};
    use grovedb_storage::StorageBatch;
    use grovedb_version::version::GroveVersion;

    use super::super::{IndexedTargetChain, IndexedTargetCommitment, IndexedTargetNode};
    use crate::{
        operations::MAX_REFERENCE_HOPS, reference_path::path_from_reference_path_type, Element,
        Error, GroveDb, Transaction,
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

    /// Build the chain for one secondary row.
    ///
    /// The chain is at most TWO entries: the immediate primary entry, and —
    /// only when that entry is itself a reference — the TERMINAL it resolves
    /// to. Intermediate hops are deliberately not carried, because nothing
    /// binds them: a GroveDB reference commits its terminal's value hash
    /// directly (`follow_reference_get_value_hash` recurses past every
    /// intermediate reference before the hash is baked into
    /// `PutCombinedReference`), so the head's commitment reaches the terminal
    /// in one step no matter how many hops the path takes. Carrying the
    /// middle of the chain would hand a verifier bytes it cannot check.
    pub(crate) fn build_target_chain<'db>(
        grovedb: &'db GroveDb,
        indexed_path: &[Vec<u8>],
        primary_key: &[u8],
        transaction: &'db Transaction,
        batch: &'db StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<IndexedTargetChain, Error> {
        let mut cost = OperationCost::default();

        // The head: the primary entry itself.
        let head_parent: Vec<&[u8]> = indexed_path.iter().map(Vec::as_slice).collect();
        let head_merk = cost_return_on_error!(
            &mut cost,
            grovedb.open_transactional_merk_at_path(
                head_parent.as_slice().into(),
                transaction,
                Some(batch),
                grove_version,
            )
        );
        let head_element = cost_return_on_error!(
            &mut cost,
            Element::get(&head_merk, primary_key, true, grove_version).map_err(|e| {
                Error::CorruptedData(format!(
                    "indexed target chain: primary entry {} is missing: {e}",
                    hex::encode(primary_key)
                ))
            })
        );
        drop(head_merk);

        let mut head_qualified = indexed_path.to_vec();
        head_qualified.push(primary_key.to_vec());
        let head = cost_return_on_error!(
            &mut cost,
            build_chain_node(
                grovedb,
                &head_element,
                &head_qualified,
                transaction,
                batch,
                grove_version,
            )
        );
        let mut nodes = vec![head];

        // If the primary entry is a reference, walk to the terminal. Only the
        // terminal is recorded — see this function's doc for why.
        let mut current_element = head_element;
        let mut current_parent = indexed_path.to_vec();
        let mut current_key = primary_key.to_vec();
        let mut visited = std::collections::HashSet::new();
        visited.insert(head_qualified);

        // `0..`, not `0..=`: `follow_reference` allows exactly
        // MAX_REFERENCE_HOPS hops, and an inclusive bound here would let
        // the prover build a chain one hop deeper than `db.get` will
        // follow — a proof that succeeds where the direct read refuses.
        for _ in 0..MAX_REFERENCE_HOPS {
            let reference_path = match current_element.underlying() {
                Element::Reference(reference_path, ..)
                | Element::ReferenceWithSumItem(reference_path, ..) => reference_path.clone(),
                // Not a reference: the head was the terminal, or we just
                // reached it.
                _ => {
                    if nodes.len() > 1 || !nodes[0].is_reference() {
                        return Ok(IndexedTargetChain { nodes }).wrap_with_cost(cost);
                    }
                    // Unreachable: a reference head always takes the branch
                    // below at least once before we get here.
                    return Err(Error::CorruptedCodeExecution(
                        "indexed target chain: reference head produced no terminal",
                    ))
                    .wrap_with_cost(cost);
                }
            };

            // `current_parent` — NOT the node's own qualified path. A relative
            // reference resolves against its PARENT (`SiblingReference`
            // appends its key to what it is given), so passing the node's own
            // path would look for a child underneath the entry itself.
            let next_qualified = match path_from_reference_path_type(
                reference_path,
                &current_parent,
                Some(current_key.as_slice()),
            ) {
                Ok(p) => p,
                Err(e) => return Err(Error::from(e)).wrap_with_cost(cost),
            };
            if !visited.insert(next_qualified.clone()) {
                return Err(Error::CyclicReference).wrap_with_cost(cost);
            }
            let Some((next_key, next_parent)) = next_qualified.split_last() else {
                return Err(Error::CorruptedPath(
                    "indexed target chain resolved an empty path".to_string(),
                ))
                .wrap_with_cost(cost);
            };
            let next_parent_slices: Vec<&[u8]> = next_parent.iter().map(Vec::as_slice).collect();
            let next_merk = cost_return_on_error!(
                &mut cost,
                grovedb.open_transactional_merk_at_path(
                    next_parent_slices.as_slice().into(),
                    transaction,
                    Some(batch),
                    grove_version,
                )
            );
            let next_element = cost_return_on_error!(
                &mut cost,
                Element::get(&next_merk, next_key, true, grove_version).map_err(|e| {
                    Error::CorruptedReferencePathKeyNotFound(format!(
                        "indexed target chain: reference target {} is missing: {e}",
                        hex::encode(next_key)
                    ))
                })
            );
            drop(next_merk);

            if !next_element.underlying().is_reference() {
                let terminal = cost_return_on_error!(
                    &mut cost,
                    build_chain_node(
                        grovedb,
                        &next_element,
                        &next_qualified,
                        transaction,
                        batch,
                        grove_version,
                    )
                );
                nodes.push(terminal);
                return Ok(IndexedTargetChain { nodes }).wrap_with_cost(cost);
            }

            current_parent = next_parent.to_vec();
            current_key = next_key.clone();
            current_element = next_element;
        }

        Err(Error::ReferenceLimit).wrap_with_cost(cost)
    }

    /// Serialize one element and derive its commitment shape.
    fn build_chain_node<'db>(
        grovedb: &'db GroveDb,
        element: &Element,
        qualified_path: &[Vec<u8>],
        transaction: &'db Transaction,
        batch: &'db StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<IndexedTargetNode, Error> {
        let mut cost = OperationCost::default();
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
                element,
                qualified_path,
                transaction,
                batch,
                grove_version,
            )
        );
        Ok(IndexedTargetNode { value, commitment }).wrap_with_cost(cost)
    }
}

#[cfg(feature = "minimal")]
pub(crate) use build::build_target_chain;

/// The commitment a node's own shape implies, for every shape except
/// `Reference` (whose commitment needs the terminal and is folded by the
/// caller).
fn shape_commitment(node: &IndexedTargetNode) -> Result<CryptoHash, Error> {
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
            return Err(Error::CorruptedCodeExecution(
                "indexed target chain: a Reference node has no standalone shape commitment",
            ));
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
    let decode = |bytes: &[u8]| -> Result<Element, Error> {
        Element::deserialize(bytes, grove_version).map_err(|e| {
            Error::CorruptedData(format!("indexed target chain: undecodable element: {e}"))
        })
    };
    let head = &chain.nodes[0];

    let (immediate_commitment, terminal) = match chain.nodes.len() {
        1 => {
            if head.is_reference() {
                return Err(Error::CorruptedData(
                    "indexed target chain: a reference head carries no terminal — a reference \
                     commits its terminal's hash, so the terminal is required to rebuild it"
                        .to_string(),
                ));
            }
            (shape_commitment(head)?, decode(&head.value)?)
        }
        2 => {
            let terminal_node = &chain.nodes[1];
            if !head.is_reference() {
                return Err(Error::CorruptedData(
                    "indexed target chain: a directly-valued head carries a terminal — only a \
                     reference resolves onward"
                        .to_string(),
                ));
            }
            if terminal_node.is_reference() {
                return Err(Error::CorruptedData(
                    "indexed target chain: the terminal entry is itself a reference — the \
                     chain must end at a directly-valued element"
                        .to_string(),
                ));
            }
            // The head commits the TERMINAL's commitment directly, however
            // many hops the reference path actually takes.
            let terminal_commitment = shape_commitment(terminal_node)?;
            let head_hash = value_hash(&head.value).value().to_owned();
            (
                combine_hash(&head_hash, &terminal_commitment)
                    .value()
                    .to_owned(),
                decode(&terminal_node.value)?,
            )
        }
        n => {
            return Err(Error::CorruptedData(format!(
                "indexed target chain has {n} entries; a chain is either a directly-valued \
                 primary or a reference plus its terminal"
            )));
        }
    };

    let immediate = decode(&head.value)?;
    Ok((immediate, immediate_commitment, terminal))
}
