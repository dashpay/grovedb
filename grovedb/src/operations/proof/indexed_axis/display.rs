//! Canonical op-level rendering for the branched indexed-axis
//! envelopes — the same `Merk(0: Push(...) 1: Parent ...)` notation
//! [`crate::GroveDBProof`]'s `Display` uses, so a decoded branched
//! proof reads exactly like every other GroveDB proof dump.

use std::fmt;

use crate::operations::proof::decode_merk_proof;

use super::{
    BranchedProofBranch, IndexedAxisBranchedPaginatedProof, IndexedAxisBranchedRangeProof,
};

fn write_layers(
    f: &mut fmt::Formatter<'_>,
    indent: &str,
    label: &str,
    layers: &[Vec<u8>],
) -> fmt::Result {
    writeln!(f, "{indent}{label}: [")?;
    for (i, layer) in layers.iter().enumerate() {
        for (line_no, line) in format!("{i}: Merk({})", decode_merk_proof(layer)?)
            .lines()
            .enumerate()
        {
            if line_no == 0 {
                writeln!(f, "{indent}  {line}")?;
            } else {
                writeln!(f, "{indent}  {line}")?;
            }
        }
    }
    writeln!(f, "{indent}]")
}

fn write_branch(
    f: &mut fmt::Formatter<'_>,
    indent: &str,
    index: usize,
    branch: &Option<BranchedProofBranch>,
) -> fmt::Result {
    match branch {
        None => writeln!(
            f,
            "{indent}{index} => ABSENT (authenticated by branching layer)"
        ),
        Some(branch) => {
            writeln!(f, "{indent}{index} => Branch {{")?;
            let inner = format!("{indent}  ");
            writeln!(
                f,
                "{inner}ancestor_attestations: {:?}",
                branch.ancestor_attestations
            )?;
            write_layers(f, &inner, "tail_layers", &branch.tail_layer_proofs)?;
            writeln!(
                f,
                "{inner}primary_root_hash: HASH[{}]",
                hex::encode(branch.primary_root_hash)
            )?;
            if !branch.other_axes_root_hashes.is_empty() {
                writeln!(
                    f,
                    "{inner}other_axes_root_hashes: {:?}",
                    branch
                        .other_axes_root_hashes
                        .iter()
                        .map(|(tag, hash)| (tag, hex::encode(hash)))
                        .collect::<Vec<_>>()
                )?;
            }
            writeln!(f, "{inner}target_is_pcpsit: {}", branch.target_is_pcpsit)?;
            writeln!(
                f,
                "{inner}secondary_proof: Merk({})",
                decode_merk_proof(&branch.secondary_proof)?
            )?;
            writeln!(f, "{indent}}}")
        }
    }
}

impl fmt::Display for IndexedAxisBranchedPaginatedProof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "IndexedAxisBranchedPaginatedProof {{")?;
        writeln!(
            f,
            "  axis_tag: {}, k: {}, offset: {}, descending: {}",
            self.axis_tag, self.requested_k, self.requested_offset, self.descending
        )?;
        write_layers(f, "  ", "shared_layers", &self.shared_layer_proofs)?;
        writeln!(
            f,
            "  shared_ancestor_attestations: {:?}",
            self.shared_ancestor_attestations
        )?;
        writeln!(
            f,
            "  branching_layer: Merk({})",
            decode_merk_proof(&self.branching_layer_proof)?
        )?;
        writeln!(f, "  branches: {{")?;
        for (i, branch) in self.branches.iter().enumerate() {
            write_branch(f, "    ", i, branch)?;
        }
        writeln!(f, "  }}")?;
        write!(f, "}}")
    }
}

impl fmt::Display for IndexedAxisBranchedRangeProof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "IndexedAxisBranchedRangeProof {{")?;
        writeln!(
            f,
            "  axis_tag: {}, limit: {:?}, descending: {}",
            self.axis_tag, self.requested_limit, self.descending
        )?;
        write_layers(f, "  ", "shared_layers", &self.shared_layer_proofs)?;
        writeln!(
            f,
            "  shared_ancestor_attestations: {:?}",
            self.shared_ancestor_attestations
        )?;
        writeln!(
            f,
            "  branching_layer: Merk({})",
            decode_merk_proof(&self.branching_layer_proof)?
        )?;
        writeln!(f, "  branches: {{")?;
        for (i, branch) in self.branches.iter().enumerate() {
            write_branch(f, "    ", i, branch)?;
        }
        writeln!(f, "  }}")?;
        write!(f, "}}")
    }
}
