# DenseAppendOnlyFixedSizeTree — Плотное хранилище Меркла фиксированной ёмкости

DenseAppendOnlyFixedSizeTree — это полное бинарное дерево фиксированной высоты, где
**каждый узел** — как внутренний, так и листовой — хранит значение данных. Позиции
заполняются последовательно в порядке обхода по уровням (BFS): сначала корень (позиция 0),
затем слева направо на каждом уровне. Промежуточные хеши не хранятся; корневой хеш
пересчитывается на лету рекурсивным хешированием от листьев к корню.

Такой дизайн идеален для малых, ограниченных структур данных, где максимальная ёмкость
известна заранее и нужны O(1) добавление, O(1) извлечение по позиции и компактный
32-байтовый коммитмент корневого хеша, изменяющийся после каждой вставки.

## Структура дерева

Дерево высоты *h* имеет ёмкость `2^h - 1` позиций. Позиции используют 0-индексированную
нумерацию обхода по уровням:

```text
Height 3 tree (capacity = 7):

              pos 0          ← root (level 0)
             /     \
          pos 1    pos 2     ← level 1
         /   \    /   \
       pos 3 pos 4 pos 5 pos 6  ← level 2 (leaves)

Navigation:
  left_child(i)  = 2i + 1
  right_child(i) = 2i + 2
  parent(i)      = (i - 1) / 2
  is_leaf(i)     = 2i + 1 >= capacity
```

Значения добавляются последовательно: первое значение попадает в позицию 0 (корень),
затем позиция 1, 2, 3 и так далее. Это означает, что корень всегда содержит данные,
и дерево заполняется в порядке обхода по уровням — наиболее естественном порядке
обхода для полного бинарного дерева.

## Вычисление хеша

Корневой хеш не хранится отдельно — он пересчитывается с нуля при каждом запросе.
Рекурсивный алгоритм посещает только заполненные позиции:

```text
hash(position, store):
  value = store.get(position)

  if position is unfilled (>= count):
    return [0; 32]                                    ← empty sentinel

  value_hash = blake3(value)
  left_hash  = hash(2 * position + 1, store)
  right_hash = hash(2 * position + 2, store)
  return blake3(value_hash || left_hash || right_hash)
```

**Ключевые свойства:**
- Все узлы (листовые и внутренние): `blake3(blake3(value) || H(left) || H(right))`
- Листовые узлы: left_hash и right_hash оба равны `[0; 32]` (незаполненные потомки)
- Незаполненные позиции: `[0u8; 32]` (нулевой хеш)
- Пустое дерево (count = 0): `[0u8; 32]`

**Теги разделения доменов для листьев и внутренних узлов не используются.** Структура дерева
(`height` и `count`) внешне аутентифицирована в родительском `Element::DenseAppendOnlyFixedSizeTree`,
который проходит через иерархию Merk. Верификатор всегда точно знает, какие позиции
являются листьями, а какие — внутренними узлами, по высоте и счётчику, поэтому злоумышленник
не может подменить одно другим без нарушения родительской цепочки аутентификации.

Это означает, что корневой хеш кодирует коммитмент к каждому хранимому значению и его
точной позиции в дереве. Изменение любого значения (если бы оно было изменяемым)
каскадировало бы через все хеши предков до корня.

**Стоимость хеширования:** Вычисление корневого хеша посещает все заполненные позиции
плюс незаполненные потомки. Для дерева с *n* значениями наихудший случай — O(*n*) вызовов
blake3. Это приемлемо, поскольку дерево спроектировано для малых, ограниченных ёмкостей
(максимальная высота 16, максимум 65 535 позиций).

## Вариант Element

```rust
Element::DenseAppendOnlyFixedSizeTree(
    u16,                   // count — number of values stored (max 65,535)
    u8,                    // height — immutable after creation (1..=16)
    Option<ElementFlags>,  // flags — storage flags
)
```

| Поле | Тип | Описание |
|---|---|---|
| `count` | `u16` | Количество вставленных значений (макс. 65 535) |
| `height` | `u8` | Высота дерева (1..=16), неизменяема после создания |
| `flags` | `Option<ElementFlags>` | Опциональные флаги хранения |

Корневой хеш НЕ хранится в Element — он передаётся как дочерний хеш Merk
через параметр `subtree_root_hash` функции `insert_subtree`.

**Дискриминант:** 14 (ElementType), TreeType = 10

**Размер стоимости:** `DENSE_TREE_COST_SIZE = 6` байт (2 count + 1 height + 1 дискриминант
+ 2 накладные расходы)

## Схема хранения

Как и MmrTree с BulkAppendTree, DenseAppendOnlyFixedSizeTree хранит данные в
пространстве **data** (не в дочернем Merk). Значения индексируются по их позиции
как big-endian `u64`:

```text
Subtree path: blake3(parent_path || key)

Storage keys:
  [0, 0, 0, 0, 0, 0, 0, 0] → value at position 0 (root)
  [0, 0, 0, 0, 0, 0, 0, 1] → value at position 1
  [0, 0, 0, 0, 0, 0, 0, 2] → value at position 2
  ...
```

Сам Element (хранящийся в родительском Merk) несёт `count` и `height`.
Корневой хеш передаётся как дочерний хеш Merk. Это означает:
- **Чтение корневого хеша** требует пересчёта из хранилища (O(n) хеширования)
- **Чтение значения по позиции — O(1)** — один поиск в хранилище
- **Вставка — O(n) хеширования** — одна запись в хранилище + полный пересчёт корневого хеша

## Операции

### `dense_tree_insert(path, key, value, tx, grove_version)`

Добавляет значение в следующую доступную позицию. Возвращает `(root_hash, position)`.

```text
Step 1: Read element, extract (count, height)
Step 2: Check capacity: if count >= 2^height - 1 → error
Step 3: Build subtree path, open storage context
Step 4: Write value to position = count
Step 5: Reconstruct DenseFixedSizedMerkleTree from state
Step 6: Call tree.insert(value, store) → (root_hash, position, hash_calls)
Step 7: Update element with new root_hash and count + 1
Step 8: Propagate changes up through Merk hierarchy
Step 9: Commit transaction
```

### `dense_tree_get(path, key, position, tx, grove_version)`

Извлекает значение по заданной позиции. Возвращает `None`, если position >= count.

### `dense_tree_root_hash(path, key, tx, grove_version)`

Возвращает корневой хеш, хранящийся в элементе. Это хеш, вычисленный при последней
вставке — пересчёт не требуется.

### `dense_tree_count(path, key, tx, grove_version)`

Возвращает количество хранимых значений (поле `count` из элемента).

## Пакетные операции

Вариант `GroveOp::DenseTreeInsert` поддерживает пакетную вставку через стандартный
конвейер пакетной обработки GroveDB:

```rust
let ops = vec![
    QualifiedGroveDbOp::dense_tree_insert_op(
        vec![b"parent".to_vec()],
        b"my_dense_tree".to_vec(),
        b"value_data".to_vec(),
    ),
];
db.apply_batch(ops, None, None, grove_version)?;
```

**Предобработка:** Как и все типы не-Merk деревьев, операции `DenseTreeInsert` предобрабатываются
перед выполнением основного тела пакета. Метод `preprocess_dense_tree_ops`:

1. Группирует все операции `DenseTreeInsert` по `(path, key)`
2. Для каждой группы последовательно выполняет вставки (чтение элемента, вставка
   каждого значения, обновление корневого хеша)
3. Преобразует каждую группу в операцию `ReplaceNonMerkTreeRoot`, несущую итоговый
   `root_hash` и `count` через стандартный механизм распространения

Несколько вставок в одно плотное дерево в рамках одного пакета поддерживаются — они
обрабатываются по порядку, и проверка согласованности допускает дублирование ключей
для этого типа операций.

**Распространение:** Корневой хеш и счётчик передаются через вариант
`NonMerkTreeMeta::DenseTree` в `ReplaceNonMerkTreeRoot`, следуя тому же паттерну,
что и MmrTree и BulkAppendTree.

## Доказательства

DenseAppendOnlyFixedSizeTree поддерживает **V1-доказательства подзапросов** через вариант
`ProofBytes::DenseTree`. Отдельные позиции могут быть доказаны относительно корневого
хеша дерева с помощью доказательств включения, несущих хеши значений предков и хеши
поддеревьев соседних узлов.

### Структура пути аутентификации

Поскольку внутренние узлы хешируют **собственное значение** (а не только хеши потомков),
путь аутентификации отличается от стандартного дерева Меркла. Для верификации листа
в позиции `p` верификатору необходимы:

1. **Значение листа** (доказываемая запись)
2. **Хеши значений предков** для каждого внутреннего узла на пути от `p` к корню (только 32-байтовый хеш, не полное значение)
3. **Хеши поддеревьев соседних узлов** для каждого потомка, не лежащего на пути

Поскольку все узлы используют `blake3(H(value) || H(left) || H(right))` (без тегов доменов),
доказательство несёт только 32-байтовые хеши значений для предков — не полные значения.
Это делает доказательства компактными независимо от размера отдельных значений.

```rust
pub struct DenseTreeProof {
    pub entries: Vec<(u16, Vec<u8>)>,            // proved (position, value) pairs
    pub node_value_hashes: Vec<(u16, [u8; 32])>, // ancestor value hashes on the auth path
    pub node_hashes: Vec<(u16, [u8; 32])>,       // precomputed sibling subtree hashes
}
```

> **Примечание:** `height` и `count` не входят в структуру доказательства — верификатор получает их из родительского Element, аутентифицированного иерархией Merk.

### Пошаговый пример

Дерево с height=3, capacity=7, count=5, доказательство позиции 4:

```text
        0
       / \
      1   2
     / \ / \
    3  4 5  6
```

Путь от 4 к корню: `4 → 1 → 0`. Расширенное множество: `{0, 1, 4}`.

Доказательство содержит:
- **entries**: `[(4, value[4])]` — доказываемая позиция
- **node_value_hashes**: `[(0, H(value[0])), (1, H(value[1]))]` — хеши значений предков (по 32 байта, не полные значения)
- **node_hashes**: `[(2, H(subtree_2)), (3, H(node_3))]` — соседние узлы не на пути

Верификация пересчитывает корневой хеш снизу вверх:
1. `H(4) = blake3(blake3(value[4]) || [0;32] || [0;32])` — лист (потомки незаполнены)
2. `H(3)` — из `node_hashes`
3. `H(1) = blake3(H(value[1]) || H(3) || H(4))` — внутренний узел использует хеш значения из `node_value_hashes`
4. `H(2)` — из `node_hashes`
5. `H(0) = blake3(H(value[0]) || H(1) || H(2))` — корень использует хеш значения из `node_value_hashes`
6. Сравнение `H(0)` с ожидаемым корневым хешем

### Доказательства нескольких позиций

При доказательстве нескольких позиций расширенное множество объединяет пересекающиеся
пути аутентификации. Общие предки включаются только один раз, делая многопозиционные
доказательства компактнее, чем независимые однопозиционные доказательства.

### Ограничение V0

V0-доказательства не могут спускаться в плотные деревья. Если V0-запрос совпадает с
`DenseAppendOnlyFixedSizeTree` с подзапросом, система возвращает
`Error::NotSupported`, направляя вызывающего использовать `prove_query_v1`.

### Кодирование ключей запроса

Позиции плотного дерева кодируются как **big-endian u16** (2-байтовые) ключи запроса,
в отличие от MmrTree и BulkAppendTree, использующих u64. Поддерживаются все стандартные
типы диапазонов `QueryItem`.

## Сравнение с другими не-Merk деревьями

| | DenseTree | MmrTree | BulkAppendTree | CommitmentTree |
|---|---|---|---|---|
| **Дискриминант элемента** | 14 | 12 | 13 | 11 |
| **TreeType** | 10 | 8 | 9 | 7 |
| **Ёмкость** | Фиксированная (`2^h - 1`, макс. 65 535) | Неограниченная | Неограниченная | Неограниченная |
| **Модель данных** | Каждая позиция хранит значение | Только листья | Буфер плотного дерева + чанки | Только листья |
| **Хеш в Element?** | Нет (передаётся как дочерний хеш) | Нет (передаётся как дочерний хеш) | Нет (передаётся как дочерний хеш) | Нет (передаётся как дочерний хеш) |
| **Стоимость вставки (хеширование)** | O(n) blake3 | O(1) амортизированная | O(1) амортизированная | ~33 Sinsemilla |
| **Размер стоимости** | 6 байт | 11 байт | 12 байт | 12 байт |
| **Поддержка доказательств** | V1 (Dense) | V1 (MMR) | V1 (Bulk) | V1 (CommitmentTree) |
| **Лучше всего для** | Малые ограниченные структуры | Журналы событий | Высокопроизводительные журналы | ZK-коммитменты |

**Когда выбирать DenseAppendOnlyFixedSizeTree:**
- Максимальное количество записей известно при создании
- Нужно, чтобы каждая позиция (включая внутренние узлы) хранила данные
- Нужна максимально простая модель данных без неограниченного роста
- O(n) пересчёт корневого хеша приемлем (малые высоты дерева)

**Когда НЕ выбирать:**
- Нужна неограниченная ёмкость → используйте MmrTree или BulkAppendTree
- Нужна ZK-совместимость → используйте CommitmentTree

## Пример использования

```rust
use grovedb::Element;
use grovedb_version::version::GroveVersion;

let grove_version = GroveVersion::latest();

// Create a dense tree of height 4 (capacity = 15 values)
db.insert(
    &[b"state"],
    b"validator_slots",
    Element::empty_dense_tree(4),
    None,
    None,
    grove_version,
)?;

// Append values — positions filled 0, 1, 2, ...
let (root_hash, pos) = db.dense_tree_insert(
    &[b"state"],
    b"validator_slots",
    validator_pubkey.to_vec(),
    None,
    grove_version,
)?;
// pos == 0, root_hash = blake3(validator_pubkey)

// Read back by position
let value = db.dense_tree_get(
    &[b"state"],
    b"validator_slots",
    0,     // position
    None,
    grove_version,
)?;
assert_eq!(value, Some(validator_pubkey.to_vec()));

// Query metadata
let count = db.dense_tree_count(&[b"state"], b"validator_slots", None, grove_version)?;
let hash = db.dense_tree_root_hash(&[b"state"], b"validator_slots", None, grove_version)?;
```

## Файлы реализации

| Файл | Содержимое |
|---|---|
| `grovedb-dense-fixed-sized-merkle-tree/src/lib.rs` | Трейт `DenseTreeStore`, структура `DenseFixedSizedMerkleTree`, рекурсивный хеш |
| `grovedb-dense-fixed-sized-merkle-tree/src/proof.rs` | Структура `DenseTreeProof`, `generate()`, `encode_to_vec()`, `decode_from_slice()` |
| `grovedb-dense-fixed-sized-merkle-tree/src/verify.rs` | `DenseTreeProof::verify()` — чистая функция, хранилище не требуется |
| `grovedb-element/src/element/mod.rs` | `Element::DenseAppendOnlyFixedSizeTree` (дискриминант 14) |
| `grovedb-element/src/element/constructor.rs` | `empty_dense_tree()`, `new_dense_tree()` |
| `merk/src/tree_type/mod.rs` | `TreeType::DenseAppendOnlyFixedSizeTree = 10` |
| `merk/src/tree_type/costs.rs` | `DENSE_TREE_COST_SIZE = 6` |
| `grovedb/src/operations/dense_tree.rs` | Операции GroveDB, `AuxDenseTreeStore`, предобработка пакетов |
| `grovedb/src/operations/proof/generate.rs` | `generate_dense_tree_layer_proof()`, `query_items_to_positions()` |
| `grovedb/src/operations/proof/verify.rs` | `verify_dense_tree_lower_layer()` |
| `grovedb/src/operations/proof/mod.rs` | Вариант `ProofBytes::DenseTree` |
| `grovedb/src/batch/estimated_costs/average_case_costs.rs` | Модель средних затрат |
| `grovedb/src/batch/estimated_costs/worst_case_costs.rs` | Модель наихудших затрат |
| `grovedb/src/tests/dense_tree_tests.rs` | 22 интеграционных теста |

---
