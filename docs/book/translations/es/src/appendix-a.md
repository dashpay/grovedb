# Apendice A: Referencia Completa de Tipos de Element

| Discriminante | Variante | TreeType | Campos | Tamano de Costo | Proposito |
|---|---|---|---|---|---|
| 0 | `Item` | N/A | `(value, flags)` | varia | Almacenamiento basico clave-valor |
| 1 | `Reference` | N/A | `(path, max_hop, flags)` | varia | Enlace entre elementos |
| 2 | `Tree` | 0 (NormalTree) | `(root_key, flags)` | TREE_COST_SIZE | Contenedor para subarboles |
| 3 | `SumItem` | N/A | `(value, flags)` | varia | Contribuye a la suma del padre |
| 4 | `SumTree` | 1 (SumTree) | `(root_key, sum, flags)` | SUM_TREE_COST_SIZE | Mantiene la suma de los descendientes |
| 5 | `BigSumTree` | 4 (BigSumTree) | `(root_key, sum128, flags)` | BIG_SUM_TREE_COST_SIZE | Arbol de suma de 128 bits |
| 6 | `CountTree` | 2 (CountTree) | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Arbol de conteo de elementos |
| 7 | `CountSumTree` | 3 (CountSumTree) | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Conteo + suma combinados |
| 8 | `ItemWithSumItem` | N/A | `(value, sum, flags)` | varia | Item con contribucion de suma |
| 9 | `ProvableCountTree` | 5 | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Arbol de conteo demostrable |
| 10 | `ProvableCountSumTree` | 6 | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Conteo + suma demostrables |
| 11 | `CommitmentTree` | 7 | `(total_count: u64, chunk_power: u8, flags)` | 12 | Sinsemilla compatible con ZK + BulkAppendTree |
| 12 | `MmrTree` | 8 | `(mmr_size: u64, flags)` | 11 | Registro MMR de solo-adicion |
| 13 | `BulkAppendTree` | 9 | `(total_count: u64, chunk_power: u8, flags)` | 12 | Registro de solo-adicion de alto rendimiento |
| 14 | `DenseAppendOnlyFixedSizeTree` | 10 | `(count: u16, height: u8, flags)` | 6 | Almacenamiento denso de Merkle con capacidad fija |

**Notas:**
- Los discriminantes 11-14 son **arboles no-Merk**: los datos residen fuera de un subarbol hijo Merk
  - Los cuatro almacenan datos no-Merk en la columna **data**
  - `CommitmentTree` almacena su frontera Sinsemilla junto con las entradas de BulkAppendTree en la misma columna data (clave `b"__ct_data__"`)
- Los arboles no-Merk NO tienen un campo `root_key` — su hash raiz especifico del tipo fluye como el hash hijo del Merk via `insert_subtree`
- `CommitmentTree` usa hashing Sinsemilla (curva Pallas); todos los demas usan Blake3
- El comportamiento de costo para arboles no-Merk sigue a `NormalTree` (BasicMerkNode, sin agregacion)
- El count de `DenseAppendOnlyFixedSizeTree` es `u16` (max 65,535); las alturas estan restringidas a 1..=16

---
