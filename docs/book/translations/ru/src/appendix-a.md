# Приложение A: Полный справочник типов элементов

| Дискриминант | Вариант | TreeType | Поля | Размер стоимости | Назначение |
|---|---|---|---|---|---|
| 0 | `Item` | Н/Д | `(value, flags)` | варьируется | Базовое хранение ключ-значение |
| 1 | `Reference` | Н/Д | `(path, max_hop, flags)` | варьируется | Связь между элементами |
| 2 | `Tree` | 0 (NormalTree) | `(root_key, flags)` | TREE_COST_SIZE | Контейнер для поддеревьев |
| 3 | `SumItem` | Н/Д | `(value, flags)` | варьируется | Участвует в родительской сумме |
| 4 | `SumTree` | 1 (SumTree) | `(root_key, sum, flags)` | SUM_TREE_COST_SIZE | Поддерживает сумму потомков |
| 5 | `BigSumTree` | 4 (BigSumTree) | `(root_key, sum128, flags)` | BIG_SUM_TREE_COST_SIZE | 128-битное дерево сумм |
| 6 | `CountTree` | 2 (CountTree) | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Дерево подсчёта элементов |
| 7 | `CountSumTree` | 3 (CountSumTree) | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Комбинированный подсчёт + сумма |
| 8 | `ItemWithSumItem` | Н/Д | `(value, sum, flags)` | варьируется | Элемент с участием в сумме |
| 9 | `ProvableCountTree` | 5 | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Доказуемое дерево подсчёта |
| 10 | `ProvableCountSumTree` | 6 | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Доказуемый подсчёт + сумма |
| 11 | `CommitmentTree` | 7 | `(total_count: u64, chunk_power: u8, flags)` | 12 | ZK-дружественное дерево Sinsemilla + BulkAppendTree |
| 12 | `MmrTree` | 8 | `(mmr_size: u64, flags)` | 11 | Журнал MMR только для добавления |
| 13 | `BulkAppendTree` | 9 | `(total_count: u64, chunk_power: u8, flags)` | 12 | Высокопроизводительный журнал только для добавления |
| 14 | `DenseAppendOnlyFixedSizeTree` | 10 | `(count: u16, height: u8, flags)` | 6 | Плотное хранилище Меркла фиксированной ёмкости |

**Примечания:**
- Дискриминанты 11–14 — это **не-Merk деревья**: данные хранятся за пределами дочернего поддерева Merk
  - Все четыре хранят данные не-Merk в колонке **data**
  - `CommitmentTree` хранит свой фронтир Sinsemilla рядом с записями BulkAppendTree в той же колонке data (ключ `b"__ct_data__"`)
- Не-Merk деревья НЕ имеют поля `root_key` — их типоспецифичный корневой хеш передаётся как дочерний хеш Merk через `insert_subtree`
- `CommitmentTree` использует хеширование Sinsemilla (кривая Pallas); все остальные используют Blake3
- Поведение стоимости для не-Merk деревьев следует `NormalTree` (BasicMerkNode, без агрегации)
- `DenseAppendOnlyFixedSizeTree` count имеет тип `u16` (макс. 65 535); высота ограничена 1..=16

---
