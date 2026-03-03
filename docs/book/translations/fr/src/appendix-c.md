# Annexe C : Reference du calcul des hachages

## Hachage de noeud Merk

```text
node_hash = blake3(key_len(1) || key || value_hash(32) || left_hash(32) || right_hash(32))
```

## Prefixe de sous-arbre GroveDB

```text
prefix = blake3(parent_prefix || key) → 32 bytes
```

## Racine d'etat — BulkAppendTree

```text
state_root = blake3("bulk_state" || mmr_root || dense_tree_root)
```

Le tampon est un DenseFixedSizedMerkleTree — `dense_tree_root` est son hachage racine.

## Racine Merkle dense — Chunks du BulkAppendTree

```text
leaves[i] = blake3(entry[i])
internal[parent] = blake3(left_child || right_child)
root = internal[0] (top of complete binary tree)
```

## Hachage racine de l'arbre dense — DenseAppendOnlyFixedSizeTree

```text
Recursive from root (position 0):
  all nodes with data: blake3(blake3(value) || H(left) || H(right))
                       (no domain separation tags — structure externally authenticated)
  leaf nodes:          left_hash = right_hash = [0; 32]
  unfilled position:   [0; 32]
  empty tree:          [0; 32]
```

## Verification de preuve de l'arbre dense — DenseAppendOnlyFixedSizeTree

```text
Given: entries, node_value_hashes, node_hashes, expected_root

Pre-checks:
  1. Validate height in [1, 16]
  2. Validate count <= capacity (= 2^height - 1)
  3. Reject if any field has > 100,000 elements (DoS prevention)
  4. Reject duplicate positions within entries, node_value_hashes, node_hashes
  5. Reject overlapping positions between the three fields
  6. Reject node_hashes at ancestors of any proved entry (prevents forgery)

recompute_hash(position):
  if position >= capacity or position >= count → [0; 32]
  if position in node_hashes → return precomputed hash
  value_hash = blake3(entries[position]) or node_value_hashes[position]
  left_hash  = recompute_hash(2*pos+1)
  right_hash = recompute_hash(2*pos+2)
  return blake3(value_hash || left_hash || right_hash)

Verify: recompute_hash(0) == expected_root

GroveDB cross-validation:
  proof.height must match element.height
  proof.count must match element.count
```

## Fusion de noeuds MMR — MmrTree / MMR de chunks du BulkAppendTree

```text
parent.hash = blake3(left.hash || right.hash)
```

## Racine Sinsemilla — CommitmentTree

```text
Sinsemilla hash over Pallas curve (see Zcash ZIP-244)
MerkleHashOrchard::empty_root(Level::from(32)) for empty tree
```

## combine_hash (liaison parent-enfant)

```text
combine_hash(value_hash, child_hash) = blake3(value_hash || child_hash)
For Merk trees: child_hash = child Merk root hash
For non-Merk trees: child_hash = type-specific root (mmr_root, state_root, dense_root, etc.)
```

---

*Le livre GroveDB — documentation des mecanismes internes de GroveDB pour les developpeurs et
les chercheurs. Base sur le code source de GroveDB.*
