# Dodatek A: Kompletna referencja typow elementow

| Dyskryminator | Wariant | TreeType | Pola | Rozmiar kosztu | Przeznaczenie |
|---|---|---|---|---|---|
| 0 | `Item` | N/D | `(value, flags)` | rozny | Podstawowe magazynowanie klucz-wartosc |
| 1 | `Reference` | N/D | `(path, max_hop, flags)` | rozny | Lacze miedzy elementami |
| 2 | `Tree` | 0 (NormalTree) | `(root_key, flags)` | TREE_COST_SIZE | Kontener dla poddrzew |
| 3 | `SumItem` | N/D | `(value, flags)` | rozny | Wklad do sumy nadrzednej |
| 4 | `SumTree` | 1 (SumTree) | `(root_key, sum, flags)` | SUM_TREE_COST_SIZE | Utrzymuje sume potomkow |
| 5 | `BigSumTree` | 4 (BigSumTree) | `(root_key, sum128, flags)` | BIG_SUM_TREE_COST_SIZE | Drzewo sum 128-bitowych |
| 6 | `CountTree` | 2 (CountTree) | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Drzewo zliczajace elementy |
| 7 | `CountSumTree` | 3 (CountSumTree) | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Polaczone zliczanie + suma |
| 8 | `ItemWithSumItem` | N/D | `(value, sum, flags)` | rozny | Element z wkladem do sumy |
| 9 | `ProvableCountTree` | 5 | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Dowodliwe drzewo zliczajace |
| 10 | `ProvableCountSumTree` | 6 | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Dowodliwe zliczanie + suma |
| 11 | `CommitmentTree` | 7 | `(total_count: u64, chunk_power: u8, flags)` | 12 | Sinsemilla + BulkAppendTree przyjazne ZK |
| 12 | `MmrTree` | 8 | `(mmr_size: u64, flags)` | 11 | Dziennik MMR append-only |
| 13 | `BulkAppendTree` | 9 | `(total_count: u64, chunk_power: u8, flags)` | 12 | Dziennik append-only o wysokiej przepustowosci |
| 14 | `DenseAppendOnlyFixedSizeTree` | 10 | `(count: u16, height: u8, flags)` | 6 | Geste magazynowanie Merkle o stalej pojemnosci |

**Uwagi:**
- Dyskryminatory 11–14 to **drzewa nie-Merk**: dane zyja poza potomnym poddrzewem Merk
  - Wszystkie cztery przechowuja dane nie-Merk w kolumnie **data**
  - `CommitmentTree` przechowuje swoj frontier Sinsemilla obok wpisow BulkAppendTree w tej samej kolumnie data (klucz `b"__ct_data__"`)
- Drzewa nie-Merk NIE maja pola `root_key` — ich hasz korzenia specyficzny dla typu przeplywa jako hasz potomny Merk poprzez `insert_subtree`
- `CommitmentTree` uzywa haszowania Sinsemilla (krzywa Pallas); wszystkie pozostale uzywaja Blake3
- Zachowanie kosztowe drzew nie-Merk podaza za `NormalTree` (BasicMerkNode, bez agregacji)
- Licznik `DenseAppendOnlyFixedSizeTree` to `u16` (maks. 65 535); wysokosci ograniczone do 1..=16

---

