# Lampiran C: Referensi Komputasi Hash

## Hash Node Merk

```text
node_hash = blake3(key_len(1) || key || value_hash(32) || left_hash(32) || right_hash(32))
```

## Prefiks Subtree GroveDB

```text
prefix = blake3(parent_prefix || key) → 32 byte
```

## State Root — BulkAppendTree

```text
state_root = blake3("bulk_state" || mmr_root || dense_tree_root)
```

Buffer adalah sebuah DenseFixedSizedMerkleTree — `dense_tree_root` adalah root hash-nya.

## Dense Merkle Root — Chunk BulkAppendTree

```text
leaves[i] = blake3(entry[i])
internal[parent] = blake3(left_child || right_child)
root = internal[0] (puncak pohon biner lengkap)
```

## Root Hash Dense Tree — DenseAppendOnlyFixedSizeTree

```text
Rekursif dari root (posisi 0):
  semua node dengan data: blake3(blake3(value) || H(left) || H(right))
                          (tanpa tag pemisah domain — struktur diautentikasi secara eksternal)
  node daun:              left_hash = right_hash = [0; 32]
  posisi tidak terisi:    [0; 32]
  pohon kosong:           [0; 32]
```

## Verifikasi Proof Dense Tree — DenseAppendOnlyFixedSizeTree

```text
Diberikan: entries, node_value_hashes, node_hashes, expected_root

Pemeriksaan awal:
  1. Validasi height dalam [1, 16]
  2. Validasi count <= kapasitas (= 2^height - 1)
  3. Tolak jika field mana pun memiliki > 100.000 element (pencegahan DoS)
  4. Tolak posisi duplikat dalam entries, node_value_hashes, node_hashes
  5. Tolak posisi yang tumpang tindih antara ketiga field
  6. Tolak node_hashes pada ancestor dari entri yang dibuktikan (mencegah pemalsuan)

recompute_hash(position):
  if position >= kapasitas or position >= count → [0; 32]
  if position in node_hashes → kembalikan hash yang sudah dihitung
  value_hash = blake3(entries[position]) or node_value_hashes[position]
  left_hash  = recompute_hash(2*pos+1)
  right_hash = recompute_hash(2*pos+2)
  return blake3(value_hash || left_hash || right_hash)

Verifikasi: recompute_hash(0) == expected_root

Validasi silang GroveDB:
  proof.height harus cocok dengan element.height
  proof.count harus cocok dengan element.count
```

## Penggabungan Node MMR — MmrTree / Chunk MMR BulkAppendTree

```text
parent.hash = blake3(left.hash || right.hash)
```

## Sinsemilla Root — CommitmentTree

```text
Hash Sinsemilla atas kurva Pallas (lihat Zcash ZIP-244)
MerkleHashOrchard::empty_root(Level::from(32)) untuk pohon kosong
```

## combine_hash (Pengikatan Induk-Anak)

```text
combine_hash(value_hash, child_hash) = blake3(value_hash || child_hash)
Untuk pohon Merk: child_hash = root hash Merk anak
Untuk pohon non-Merk: child_hash = root spesifik-tipe (mmr_root, state_root, dense_root, dll.)
```

---

*Buku GroveDB — mendokumentasikan internal GroveDB untuk pengembang dan
peneliti. Berdasarkan kode sumber GroveDB.*
