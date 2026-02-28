# Priloha A: Uplna reference typu elementu

| Diskriminant | Varianta | TreeType | Pole | Velikost nakladu | Ucel |
|---|---|---|---|---|---|
| 0 | `Item` | N/A | `(value, flags)` | promenliva | Zakladni uloziste klic-hodnota |
| 1 | `Reference` | N/A | `(path, max_hop, flags)` | promenliva | Odkaz mezi elementy |
| 2 | `Tree` | 0 (NormalTree) | `(root_key, flags)` | TREE_COST_SIZE | Kontejner pro podstromy |
| 3 | `SumItem` | N/A | `(value, flags)` | promenliva | Prispiva k rodicovskemu souctu |
| 4 | `SumTree` | 1 (SumTree) | `(root_key, sum, flags)` | SUM_TREE_COST_SIZE | Udrzuje soucet potomku |
| 5 | `BigSumTree` | 4 (BigSumTree) | `(root_key, sum128, flags)` | BIG_SUM_TREE_COST_SIZE | 128-bitovy souctovy strom |
| 6 | `CountTree` | 2 (CountTree) | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Strom pocitajici elementy |
| 7 | `CountSumTree` | 3 (CountSumTree) | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Kombinovany pocet + soucet |
| 8 | `ItemWithSumItem` | N/A | `(value, sum, flags)` | promenliva | Polozka s prispevkem k souctu |
| 9 | `ProvableCountTree` | 5 | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Dokazatelny strom poctu |
| 10 | `ProvableCountSumTree` | 6 | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Dokazatelny pocet + soucet |
| 11 | `CommitmentTree` | 7 | `(total_count: u64, chunk_power: u8, flags)` | 12 | ZK-pritelsky Sinsemilla + BulkAppendTree |
| 12 | `MmrTree` | 8 | `(mmr_size: u64, flags)` | 11 | Append-only MMR log |
| 13 | `BulkAppendTree` | 9 | `(total_count: u64, chunk_power: u8, flags)` | 12 | Vysoko-propustny append-only log |
| 14 | `DenseAppendOnlyFixedSizeTree` | 10 | `(count: u16, height: u8, flags)` | 6 | Huste Merklovo uloziste s pevnou kapacitou |

**Poznamky:**
- Diskriminanty 11-14 jsou **ne-Merk stromy**: data ziji mimo detsky podstrom Merk
  - Vsechny ctyri ukladaji ne-Merk data v **datovem** sloupci
  - `CommitmentTree` uklada svou frontieru Sinsemilla vedle zaznamu BulkAppendTree ve stejnem datovem sloupci (klic `b"__ct_data__"`)
- Ne-Merk stromy NEMAJI pole `root_key` -- jejich typove specificky korenovy hash proudi jako Merk child hash pres `insert_subtree`
- `CommitmentTree` pouziva hashovani Sinsemilla (krivka Pallas); vsechny ostatni pouzivaji Blake3
- Chovani nakladu pro ne-Merk stromy nasleduje `NormalTree` (BasicMerkNode, bez agregace)
- Pocet `DenseAppendOnlyFixedSizeTree` je `u16` (max 65 535); vysky omezeny na 1..=16

---
