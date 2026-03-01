# Priloha B: Strucna reference systemu dukazu

## Dukazy V0 (pouze Merk)

```text
Generate:
  1. Start at root Merk, execute query → collect matching elements
  2. For each matching tree element with subquery:
     Open child Merk → prove subquery recursively
  3. Serialize as GroveDBProof::V0(root_layer, Vec<(key, GroveDBProof)>)

Verify:
  1. Verify root Merk proof → root_hash, elements
  2. For each sub-proof:
     Verify child Merk proof → child_hash
     Check: combine_hash(value_hash(parent_element), child_hash) == expected
  3. Final root_hash must match known GroveDB root
```

## Dukazy V1 (Smisene Merk + ne-Merk)

Kdyz jakakoliv vrstva zahrnuje CommitmentTree, MmrTree, BulkAppendTree nebo
DenseAppendOnlyFixedSizeTree, generuje se V1 dukaz:

```text
Generate:
  1. Same as V0 for Merk layers
  2. When descending into CommitmentTree:
     → generate_commitment_tree_layer_proof(query_items) → ProofBytes::CommitmentTree(bytes)
     (sinsemilla_root (32 bytes) || BulkAppendTree proof bytes)
  3. When descending into MmrTree:
     → generate_mmr_layer_proof(query_items) → ProofBytes::MMR(bytes)
  4. When descending into BulkAppendTree:
     → generate_bulk_append_layer_proof(query_items) → ProofBytes::BulkAppendTree(bytes)
  5. When descending into DenseAppendOnlyFixedSizeTree:
     → generate_dense_tree_layer_proof(query_items) → ProofBytes::DenseTree(bytes)
  6. Store as LayerProof { merk_proof, lower_layers }

Verify:
  1. Same as V0 for Merk layers
  2. For ProofBytes::CommitmentTree: extract sinsemilla_root, verify inner
     BulkAppendTree proof, recompute combined root hash
  3. For ProofBytes::MMR: verify MMR proof against MMR root from parent child hash
  4. For ProofBytes::BulkAppendTree: verify against state_root from parent child hash
  5. For ProofBytes::DenseTree: verify against root_hash from parent child hash
     (recompute root from entries + ancestor values + sibling hashes)
  6. Type-specific root flows as Merk child hash (not NULL_HASH)
```

**Vyber V0/V1**: Pokud jsou vsechny vrstvy Merk, produkuje se V0 (zpetne kompatibilni).
Pokud je jakakoliv vrstva ne-Merk strom, produkuje se V1.

---
