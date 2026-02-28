# Appendice A: Riferimento completo dei tipi Element

| Discriminante | Variante | TreeType | Campi | Dimensione costo | Scopo |
|---|---|---|---|---|---|
| 0 | `Item` | N/A | `(value, flags)` | variabile | Archiviazione chiave-valore di base |
| 1 | `Reference` | N/A | `(path, max_hop, flags)` | variabile | Collegamento tra elementi |
| 2 | `Tree` | 0 (NormalTree) | `(root_key, flags)` | TREE_COST_SIZE | Contenitore per sotto-alberi |
| 3 | `SumItem` | N/A | `(value, flags)` | variabile | Contribuisce alla somma del genitore |
| 4 | `SumTree` | 1 (SumTree) | `(root_key, sum, flags)` | SUM_TREE_COST_SIZE | Mantiene la somma dei discendenti |
| 5 | `BigSumTree` | 4 (BigSumTree) | `(root_key, sum128, flags)` | BIG_SUM_TREE_COST_SIZE | Albero somma a 128 bit |
| 6 | `CountTree` | 2 (CountTree) | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Albero con conteggio elementi |
| 7 | `CountSumTree` | 3 (CountSumTree) | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Conteggio + somma combinati |
| 8 | `ItemWithSumItem` | N/A | `(value, sum, flags)` | variabile | Elemento con contributo alla somma |
| 9 | `ProvableCountTree` | 5 | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Albero con conteggio dimostrabile |
| 10 | `ProvableCountSumTree` | 6 | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Conteggio + somma dimostrabili |
| 11 | `CommitmentTree` | 7 | `(total_count: u64, chunk_power: u8, flags)` | 12 | ZK-compatibile Sinsemilla + BulkAppendTree |
| 12 | `MmrTree` | 8 | `(mmr_size: u64, flags)` | 11 | Log MMR append-only |
| 13 | `BulkAppendTree` | 9 | `(total_count: u64, chunk_power: u8, flags)` | 12 | Log append-only ad alto throughput |
| 14 | `DenseAppendOnlyFixedSizeTree` | 10 | `(count: u16, height: u8, flags)` | 6 | Archiviazione Merkle densa a capacita fissa |

**Note:**
- I discriminanti 11-14 sono **alberi non-Merk**: i dati risiedono al di fuori di un sotto-albero Merk figlio
  - Tutti e quattro memorizzano dati non-Merk nella colonna **dati**
  - `CommitmentTree` memorizza la sua frontiera Sinsemilla insieme alle voci del BulkAppendTree nella stessa colonna dati (chiave `b"__ct_data__"`)
- Gli alberi non-Merk NON hanno un campo `root_key` — il loro hash radice specifico del tipo fluisce come hash figlio del Merk tramite `insert_subtree`
- `CommitmentTree` utilizza l'hashing Sinsemilla (curva Pallas); tutti gli altri usano Blake3
- Il comportamento dei costi per gli alberi non-Merk segue `NormalTree` (BasicMerkNode, nessuna aggregazione)
- Il conteggio di `DenseAppendOnlyFixedSizeTree` e `u16` (massimo 65.535); altezze limitate a 1..=16

---
