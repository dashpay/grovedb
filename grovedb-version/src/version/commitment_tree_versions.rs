//! Version gates for the commitment tree crate.
//!
//! As in [`super::mmr_versions`] and [`super::bulk_append_tree_versions`],
//! only cost accounting is versioned: the frontier bytes, the anchors and the
//! state roots are identical under every version. The gate reaches a live
//! fee — the shielded pool's every append rewrites the frontier — so the
//! corrected figure arrives as a new version rather than replacing the old
//! one.

use versioned_feature_core::FeatureVersion;

#[derive(Clone, Debug, Default)]
pub struct CommitmentTreeVersions {
    pub cost: CommitmentTreeCostVersions,
}

#[derive(Clone, Debug, Default)]
pub struct CommitmentTreeCostVersions {
    /// How `CommitmentTree::save` reports the frontier rewrite to the storage
    /// cost layer.
    ///
    /// Version 0 issues the put with no cost information, so the commit path
    /// charges the key and the whole serialized frontier as NEW storage on
    /// every append — even though `__ct_data__` is one value rewritten in
    /// place. Version 1 reports the rewrite as a replacement of the bytes
    /// loaded at open (`replaced_bytes` = the smaller of the previous and new
    /// paid size, `added_bytes` = growth only, shrink not credited); the very
    /// first save of a tree, which creates the key, stays fully added.
    pub frontier_save_storage_accounting: FeatureVersion,
    /// How the Sinsemilla frontier's per-append cost is charged.
    ///
    /// Version 0 charges the actual work of the position: `32 +
    /// trailing_ones(position)` Sinsemilla hashes on an append, the frontier's
    /// actual serialized size (`42 + 32 · popcount(position)` bytes) when it
    /// is loaded at open and rewritten at save. Version 1 charges a fixed
    /// model derived from the tree's depth instead — `depth + 1` Sinsemilla
    /// hashes (the root walk plus the average ommer merge) and a
    /// `42 + 32 · depth/2`-byte frontier (the average over the position
    /// space) loaded and replaced — so every append to a commitment tree
    /// costs the same whatever its position. When set, it also decides the
    /// save's storage accounting (a replacement of the model size), taking
    /// precedence over `frontier_save_storage_accounting`. Frontier bytes
    /// and anchors are identical under both.
    pub frontier_cost_model: FeatureVersion,
}
