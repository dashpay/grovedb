# Anhang A: Vollständige Element-Typ-Referenz

| Diskriminante | Variante | TreeType | Felder | Kostengröße | Zweck |
|---|---|---|---|---|---|
| 0 | `Item` | N/A | `(value, flags)` | variiert | Einfache Schlüssel-Wert-Speicherung |
| 1 | `Reference` | N/A | `(path, max_hop, flags)` | variiert | Verknüpfung zwischen Elementen |
| 2 | `Tree` | 0 (NormalTree) | `(root_key, flags)` | TREE_COST_SIZE | Container für Teilbäume |
| 3 | `SumItem` | N/A | `(value, flags)` | variiert | Trägt zur übergeordneten Summe bei |
| 4 | `SumTree` | 1 (SumTree) | `(root_key, sum, flags)` | SUM_TREE_COST_SIZE | Verwaltet die Summe der Nachkommen |
| 5 | `BigSumTree` | 4 (BigSumTree) | `(root_key, sum128, flags)` | BIG_SUM_TREE_COST_SIZE | 128-Bit-Summenbaum |
| 6 | `CountTree` | 2 (CountTree) | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Elementzählbaum |
| 7 | `CountSumTree` | 3 (CountSumTree) | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Kombinierter Zähl- + Summenbaum |
| 8 | `ItemWithSumItem` | N/A | `(value, sum, flags)` | variiert | Element mit Summenbeitrag |
| 9 | `ProvableCountTree` | 5 | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Beweisbarer Zählbaum |
| 10 | `ProvableCountSumTree` | 6 | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Beweisbarer Zähl- + Summenbaum |
| 11 | `CommitmentTree` | 7 | `(total_count: u64, chunk_power: u8, flags)` | 12 | ZK-freundlicher Sinsemilla + BulkAppendTree |
| 12 | `MmrTree` | 8 | `(mmr_size: u64, flags)` | 11 | Append-Only-MMR-Log |
| 13 | `BulkAppendTree` | 9 | `(total_count: u64, chunk_power: u8, flags)` | 12 | Hochdurchsatz-Append-Only-Log |
| 14 | `DenseAppendOnlyFixedSizeTree` | 10 | `(count: u16, height: u8, flags)` | 6 | Dichter Merkle-Speicher mit fester Kapazität |

**Anmerkungen:**
- Diskriminanten 11–14 sind **Nicht-Merk-Bäume**: Daten liegen außerhalb eines Kind-Merk-Teilbaums
  - Alle vier speichern Nicht-Merk-Daten in der **Daten**-Spalte
  - `CommitmentTree` speichert seine Sinsemilla-Frontier neben BulkAppendTree-Einträgen in derselben Datenspalte (Schlüssel `b"__ct_data__"`)
- Nicht-Merk-Bäume haben KEIN `root_key`-Feld — ihr typspezifischer Wurzel-Hash fließt als Merk-Kind-Hash über `insert_subtree`
- `CommitmentTree` verwendet Sinsemilla-Hashing (Pallas-Kurve); alle anderen verwenden Blake3
- Kostenverhalten für Nicht-Merk-Bäume folgt `NormalTree` (BasicMerkNode, keine Aggregation)
- `DenseAppendOnlyFixedSizeTree`-count ist `u16` (max 65.535); Höhen beschränkt auf 1..=16

---
