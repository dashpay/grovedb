# 부록 B: 증명 시스템 빠른 참조

## V0 증명 (Merk 전용)

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

## V1 증명 (혼합 Merk + 비-Merk)

어떤 레이어에 CommitmentTree, MmrTree, BulkAppendTree, 또는 DenseAppendOnlyFixedSizeTree가 포함되면, V1 증명이 생성됩니다:

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

**V0/V1 선택**: 모든 레이어가 Merk이면 V0을 생성합니다(하위 호환). 어떤 레이어라도 비-Merk 트리이면 V1을 생성합니다.

---
