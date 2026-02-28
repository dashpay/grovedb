# Lampiran A: Referensi Lengkap Tipe Element

| Diskriminan | Varian | TreeType | Field | Ukuran Biaya | Tujuan |
|---|---|---|---|---|---|
| 0 | `Item` | N/A | `(value, flags)` | bervariasi | Penyimpanan key-value dasar |
| 1 | `Reference` | N/A | `(path, max_hop, flags)` | bervariasi | Tautan antar element |
| 2 | `Tree` | 0 (NormalTree) | `(root_key, flags)` | TREE_COST_SIZE | Kontainer untuk subtree |
| 3 | `SumItem` | N/A | `(value, flags)` | bervariasi | Berkontribusi pada sum induk |
| 4 | `SumTree` | 1 (SumTree) | `(root_key, sum, flags)` | SUM_TREE_COST_SIZE | Memelihara jumlah keturunan |
| 5 | `BigSumTree` | 4 (BigSumTree) | `(root_key, sum128, flags)` | BIG_SUM_TREE_COST_SIZE | Pohon sum 128-bit |
| 6 | `CountTree` | 2 (CountTree) | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Pohon penghitung element |
| 7 | `CountSumTree` | 3 (CountSumTree) | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Gabungan count + sum |
| 8 | `ItemWithSumItem` | N/A | `(value, sum, flags)` | bervariasi | Item dengan kontribusi sum |
| 9 | `ProvableCountTree` | 5 | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Pohon count yang dapat dibuktikan |
| 10 | `ProvableCountSumTree` | 6 | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Count + sum yang dapat dibuktikan |
| 11 | `CommitmentTree` | 7 | `(total_count: u64, chunk_power: u8, flags)` | 12 | Sinsemilla + BulkAppendTree ramah-ZK |
| 12 | `MmrTree` | 8 | `(mmr_size: u64, flags)` | 11 | Log MMR append-only |
| 13 | `BulkAppendTree` | 9 | `(total_count: u64, chunk_power: u8, flags)` | 12 | Log append-only throughput tinggi |
| 14 | `DenseAppendOnlyFixedSizeTree` | 10 | `(count: u16, height: u8, flags)` | 6 | Penyimpanan Merkle padat berkapasitas tetap |

**Catatan:**
- Diskriminan 11-14 adalah **pohon non-Merk**: data berada di luar subtree Merk anak
  - Keempat tipe menyimpan data non-Merk di kolom **data**
  - `CommitmentTree` menyimpan frontier Sinsemilla-nya bersama entri BulkAppendTree di kolom data yang sama (key `b"__ct_data__"`)
- Pohon non-Merk TIDAK memiliki field `root_key` — root hash spesifik-tipe mereka mengalir sebagai Merk child hash melalui `insert_subtree`
- `CommitmentTree` menggunakan hashing Sinsemilla (kurva Pallas); semua yang lain menggunakan Blake3
- Perilaku biaya untuk pohon non-Merk mengikuti `NormalTree` (BasicMerkNode, tanpa agregasi)
- Count `DenseAppendOnlyFixedSizeTree` adalah `u16` (maks 65.535); tinggi dibatasi ke 1..=16

---
