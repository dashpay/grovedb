# Lampiran B: Referensi Cepat Sistem Proof

## Proof V0 (Hanya Merk)

```text
Pembuatan:
  1. Mulai dari Merk root, eksekusi query → kumpulkan element yang cocok
  2. Untuk setiap element pohon yang cocok dengan subquery:
     Buka Merk anak → buktikan subquery secara rekursif
  3. Serialisasi sebagai GroveDBProof::V0(root_layer, Vec<(key, GroveDBProof)>)

Verifikasi:
  1. Verifikasi proof Merk root → root_hash, element
  2. Untuk setiap sub-proof:
     Verifikasi proof Merk anak → child_hash
     Periksa: combine_hash(value_hash(parent_element), child_hash) == yang diharapkan
  3. root_hash akhir harus cocok dengan root GroveDB yang diketahui
```

## Proof V1 (Campuran Merk + Non-Merk)

Ketika lapisan mana pun melibatkan CommitmentTree, MmrTree, BulkAppendTree, atau
DenseAppendOnlyFixedSizeTree, proof V1 dihasilkan:

```text
Pembuatan:
  1. Sama dengan V0 untuk lapisan Merk
  2. Saat turun ke CommitmentTree:
     → generate_commitment_tree_layer_proof(query_items) → ProofBytes::CommitmentTree(bytes)
     (sinsemilla_root (32 byte) || byte proof BulkAppendTree)
  3. Saat turun ke MmrTree:
     → generate_mmr_layer_proof(query_items) → ProofBytes::MMR(bytes)
  4. Saat turun ke BulkAppendTree:
     → generate_bulk_append_layer_proof(query_items) → ProofBytes::BulkAppendTree(bytes)
  5. Saat turun ke DenseAppendOnlyFixedSizeTree:
     → generate_dense_tree_layer_proof(query_items) → ProofBytes::DenseTree(bytes)
  6. Simpan sebagai LayerProof { merk_proof, lower_layers }

Verifikasi:
  1. Sama dengan V0 untuk lapisan Merk
  2. Untuk ProofBytes::CommitmentTree: ekstrak sinsemilla_root, verifikasi proof
     BulkAppendTree bagian dalam, hitung ulang root hash gabungan
  3. Untuk ProofBytes::MMR: verifikasi proof MMR terhadap MMR root dari child hash induk
  4. Untuk ProofBytes::BulkAppendTree: verifikasi terhadap state_root dari child hash induk
  5. Untuk ProofBytes::DenseTree: verifikasi terhadap root_hash dari child hash induk
     (hitung ulang root dari entri + nilai ancestor + hash sibling)
  6. Root spesifik-tipe mengalir sebagai Merk child hash (bukan NULL_HASH)
```

**Pemilihan V0/V1**: Jika semua lapisan adalah Merk, hasilkan V0 (kompatibel mundur).
Jika ada lapisan yang merupakan pohon non-Merk, hasilkan V1.

---
