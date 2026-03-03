# Ek A: Tam Element Tipi Referansi

| Ayirt Edici | Varyant | TreeType | Alanlar | Maliyet Boyutu | Amac |
|---|---|---|---|---|---|
| 0 | `Item` | Uygulanmaz | `(value, flags)` | degisir | Temel anahtar-deger depolamasi |
| 1 | `Reference` | Uygulanmaz | `(path, max_hop, flags)` | degisir | Elementler arasi baglanti |
| 2 | `Tree` | 0 (NormalTree) | `(root_key, flags)` | TREE_COST_SIZE | Alt agaclar icin kapsayici |
| 3 | `SumItem` | Uygulanmaz | `(value, flags)` | degisir | Ust toplama katki saglar |
| 4 | `SumTree` | 1 (SumTree) | `(root_key, sum, flags)` | SUM_TREE_COST_SIZE | Alt elemanlarin toplamini tutar |
| 5 | `BigSumTree` | 4 (BigSumTree) | `(root_key, sum128, flags)` | BIG_SUM_TREE_COST_SIZE | 128-bit toplam agaci |
| 6 | `CountTree` | 2 (CountTree) | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Element sayim agaci |
| 7 | `CountSumTree` | 3 (CountSumTree) | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Birlesik sayim + toplam |
| 8 | `ItemWithSumItem` | Uygulanmaz | `(value, sum, flags)` | degisir | Toplam katkili oge |
| 9 | `ProvableCountTree` | 5 | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Ispatlanabilir sayim agaci |
| 10 | `ProvableCountSumTree` | 6 | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Ispatlanabilir sayim + toplam |
| 11 | `CommitmentTree` | 7 | `(total_count: u64, chunk_power: u8, flags)` | 12 | ZK uyumlu Sinsemilla + BulkAppendTree |
| 12 | `MmrTree` | 8 | `(mmr_size: u64, flags)` | 11 | Yalnizca ekleme MMR gunlugu |
| 13 | `BulkAppendTree` | 9 | `(total_count: u64, chunk_power: u8, flags)` | 12 | Yuksek verimli yalnizca ekleme gunlugu |
| 14 | `DenseAppendOnlyFixedSizeTree` | 10 | `(count: u16, height: u8, flags)` | 6 | Yogun sabit kapasiteli Merkle depolamasi |

**Notlar:**
- Ayirt ediciler 11-14 **Merk-disi agaclardir**: veri bir alt Merk alt agacinin disinda yasir
  - Dort agac tipi de Merk-disi verileri **data** sutununda depolar
  - `CommitmentTree`, Sinsemilla frontier'ini BulkAppendTree girdileriyle birlikte ayni data sutununda depolar (anahtar `b"__ct_data__"`)
- Merk-disi agaclarin `root_key` alani YOKTUR — tipe ozgu kok hash'leri, `insert_subtree` araciligiyla Merk alt hash'i (child hash) olarak akar
- `CommitmentTree` Sinsemilla hash'lemesi (Pallas egrisi) kullanir; diger tum tipler Blake3 kullanir
- Merk-disi agaclar icin maliyet davranisi `NormalTree`'yi izler (BasicMerkNode, toplama yok)
- `DenseAppendOnlyFixedSizeTree` sayici `u16`'dir (maks 65.535); yukseklikler 1..=16 ile sinirlidir

---
