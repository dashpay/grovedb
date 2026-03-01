# Apendice A: Referencia Completa de Tipos de Elemento

| Discriminante | Variante | TreeType | Campos | Tamanho do Custo | Proposito |
|---|---|---|---|---|---|
| 0 | `Item` | N/A | `(value, flags)` | varia | Armazenamento basico chave-valor |
| 1 | `Reference` | N/A | `(path, max_hop, flags)` | varia | Link entre elementos |
| 2 | `Tree` | 0 (NormalTree) | `(root_key, flags)` | TREE_COST_SIZE | Container para subarvores |
| 3 | `SumItem` | N/A | `(value, flags)` | varia | Contribui para soma do pai |
| 4 | `SumTree` | 1 (SumTree) | `(root_key, sum, flags)` | SUM_TREE_COST_SIZE | Mantem soma dos descendentes |
| 5 | `BigSumTree` | 4 (BigSumTree) | `(root_key, sum128, flags)` | BIG_SUM_TREE_COST_SIZE | Arvore de soma de 128 bits |
| 6 | `CountTree` | 2 (CountTree) | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Arvore de contagem de elementos |
| 7 | `CountSumTree` | 3 (CountSumTree) | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Contagem + soma combinados |
| 8 | `ItemWithSumItem` | N/A | `(value, sum, flags)` | varia | Item com contribuicao de soma |
| 9 | `ProvableCountTree` | 5 | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Arvore de contagem provavel |
| 10 | `ProvableCountSumTree` | 6 | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Contagem + soma provavel |
| 11 | `CommitmentTree` | 7 | `(total_count: u64, chunk_power: u8, flags)` | 12 | Sinsemilla amigavel a ZK + BulkAppendTree |
| 12 | `MmrTree` | 8 | `(mmr_size: u64, flags)` | 11 | Log MMR append-only |
| 13 | `BulkAppendTree` | 9 | `(total_count: u64, chunk_power: u8, flags)` | 12 | Log append-only de alto desempenho |
| 14 | `DenseAppendOnlyFixedSizeTree` | 10 | `(count: u16, height: u8, flags)` | 6 | Armazenamento denso de Merkle de capacidade fixa |

**Notas:**
- Discriminantes 11-14 sao **arvores nao-Merk**: dados residem fora de uma subarvore Merk filha
  - Todos os quatro armazenam dados nao-Merk na coluna de **dados**
  - `CommitmentTree` armazena sua fronteira Sinsemilla junto com entradas BulkAppendTree na mesma coluna de dados (chave `b"__ct_data__"`)
- Arvores nao-Merk NAO tem um campo `root_key` — seu hash raiz especifico do tipo flui como o hash filho da Merk via `insert_subtree`
- `CommitmentTree` usa hashing Sinsemilla (curva Pallas); todos os outros usam Blake3
- Comportamento de custo para arvores nao-Merk segue `NormalTree` (BasicMerkNode, sem agregacao)
- `DenseAppendOnlyFixedSizeTree` count e `u16` (max 65.535); alturas restritas a 1..=16

---
