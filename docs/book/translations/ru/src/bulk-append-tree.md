# BulkAppendTree — Высокопроизводительное хранилище только для добавления

BulkAppendTree — это решение GroveDB для конкретной инженерной задачи: как построить
высокопроизводительный журнал «только для добавления» (append-only), поддерживающий эффективные
доказательства диапазонов, минимизирующий хеширование на запись и создающий неизменяемые
снимки чанков, пригодные для распространения через CDN?

В то время как MmrTree (Глава 13) идеален для доказательств отдельных листьев, BulkAppendTree
спроектирован для рабочих нагрузок, где тысячи значений поступают за блок и клиентам нужно
синхронизироваться, загружая диапазоны данных. Он достигает этого с помощью **двухуровневой архитектуры**:
плотного дерева Меркла в качестве буфера, поглощающего входящие добавления, и MMR на уровне чанков,
записывающего финализированные корни чанков.

## Двухуровневая архитектура

```text
┌────────────────────────────────────────────────────────────────┐
│                      BulkAppendTree                            │
│                                                                │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  Chunk MMR                                               │  │
│  │  ┌────┐ ┌────┐ ┌────┐ ┌────┐                            │  │
│  │  │ R0 │ │ R1 │ │ R2 │ │ H  │ ← Dense Merkle roots      │  │
│  │  └────┘ └────┘ └────┘ └────┘   of each chunk blob       │  │
│  │                     peak hashes bagged together = MMR root│  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  Buffer (DenseFixedSizedMerkleTree, capacity = 2^h - 1) │  │
│  │  ┌───┐ ┌───┐ ┌───┐                                      │  │
│  │  │v_0│ │v_1│ │v_2│ ... (fills in level-order)           │  │
│  │  └───┘ └───┘ └───┘                                      │  │
│  │  dense_tree_root = recomputed root hash of dense tree     │  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                │
│  state_root = blake3("bulk_state" || mmr_root || dense_tree_root) │
│                                                                │
└────────────────────────────────────────────────────────────────┘
```

**Уровень 1 — Буфер.** Входящие значения записываются в `DenseFixedSizedMerkleTree`
(см. Главу 16). Ёмкость буфера составляет `2^height - 1` позиций. Корневой хеш плотного
дерева (`dense_tree_root`) обновляется после каждой вставки.

**Уровень 2 — Chunk MMR.** Когда буфер заполняется (достигает `chunk_size` записей),
все записи сериализуются в неизменяемый **блоб чанка** (chunk blob), вычисляется плотный
корень Меркла по этим записям, и этот корень добавляется как лист в chunk MMR.
После этого буфер очищается.

**Корень состояния** (state root) объединяет оба уровня в единый 32-байтовый коммитмент,
который изменяется при каждом добавлении, гарантируя, что родительское дерево Merk всегда
отражает актуальное состояние.

## Как значения заполняют буфер

Каждый вызов `append()` следует такой последовательности:

```text
Step 1: Write value to dense tree buffer at next position
        dense_tree.insert(value, store)

Step 2: Increment total_count
        total_count += 1

Step 3: Check if buffer is full (dense tree at capacity)
        if dense_tree.count() == capacity:
            → trigger compaction (§14.3)

Step 4: Compute new state root (+1 blake3 call)
        dense_tree_root = dense_tree.root_hash(store)
        state_root = blake3("bulk_state" || mmr_root || dense_tree_root)
```

**Буфер ЯВЛЯЕТСЯ DenseFixedSizedMerkleTree** (см. Главу 16). Его корневой хеш
изменяется после каждой вставки, обеспечивая коммитмент ко всем текущим записям буфера.
Именно этот корневой хеш участвует в вычислении корня состояния.

## Компактификация чанков

Когда буфер заполняется (достигает `chunk_size` записей), компактификация (compaction) срабатывает автоматически:

```text
Compaction Steps:
─────────────────
1. Read all chunk_size buffer entries

2. Compute dense Merkle root
   - Hash each entry: leaf[i] = blake3(entry[i])
   - Build complete binary tree bottom-up
   - Extract root hash
   Hash cost: chunk_size + (chunk_size - 1) = 2 * chunk_size - 1

3. Serialize entries into chunk blob
   - Auto-selects fixed-size or variable-size format (§14.6)
   - Store as: store.put(chunk_key(chunk_index), blob)

4. Append dense Merkle root to chunk MMR
   - MMR push with merge cascade (see Chapter 13)
   Hash cost: ~2 amortized (trailing_ones pattern)

5. Reset the dense tree (clear all buffer entries from storage)
   - Dense tree count reset to 0
```

После компактификации блоб чанка становится **навсегда неизменяемым** — он больше никогда
не изменится. Это делает блобы чанков идеальными для кеширования на CDN, синхронизации
клиентов и архивного хранения.

**Пример: 4 добавления с chunk_power=2 (chunk_size=4)**

```text
Append v_0: dense_tree=[v_0],       dense_root=H(v_0), total=1
Append v_1: dense_tree=[v_0,v_1],   dense_root=H(v_0,v_1), total=2
Append v_2: dense_tree=[v_0..v_2],  dense_root=H(v_0..v_2), total=3
Append v_3: dense_tree=[v_0..v_3],  dense_root=H(v_0..v_3), total=4
  → COMPACTION:
    chunk_blob_0 = serialize([v_0, v_1, v_2, v_3])
    dense_root_0 = dense_merkle_root(v_0..v_3)
    mmr.push(dense_root_0)
    dense tree cleared (count=0)

Append v_4: dense_tree=[v_4],       dense_root=H(v_4), total=5
  → state_root = blake3("bulk_state" || mmr_root || dense_root)
```

## Корень состояния

Корень состояния (state root) связывает оба уровня в один хеш:

```rust
fn compute_state_root(
    mmr_root: &[u8; 32],         // Chunk MMR root (or [0;32] if empty)
    dense_tree_root: &[u8; 32],  // Root hash of current buffer (dense tree)
) -> [u8; 32] {
    blake3("bulk_state" || mmr_root || dense_tree_root)
}
```

`total_count` и `chunk_power` **не** включаются в корень состояния, поскольку
они уже аутентифицированы хешем значения Merk — это поля сериализованного `Element`,
хранящегося в родительском узле Merk. Корень состояния фиксирует только коммитменты
на уровне данных (`mmr_root` и `dense_tree_root`). Именно этот хеш передаётся как
дочерний хеш Merk и распространяется вверх до корневого хеша GroveDB.

## Плотный корень Меркла

Когда чанк компактифицируется, записям нужен единый 32-байтовый коммитмент.
BulkAppendTree использует **плотное (полное) бинарное дерево Меркла**:

```text
Given entries [e_0, e_1, e_2, e_3]:

Level 0 (leaves):  blake3(e_0)  blake3(e_1)  blake3(e_2)  blake3(e_3)
                      \__________/              \__________/
Level 1:           blake3(h_0 || h_1)       blake3(h_2 || h_3)
                          \____________________/
Level 2 (root):    blake3(h_01 || h_23)  ← this is the dense Merkle root
```

Поскольку `chunk_size` всегда является степенью двойки (по построению: `1u32 << chunk_power`),
дерево всегда полное (не нужны заполнители или фиктивные листья). Количество хешей
точно равно `2 * chunk_size - 1`:
- `chunk_size` хешей листьев (по одному на запись)
- `chunk_size - 1` хешей внутренних узлов

Реализация плотного корня Меркла находится в `grovedb-mmr/src/dense_merkle.rs` и
предоставляет две функции:
- `compute_dense_merkle_root(hashes)` — из предварительно хешированных листьев
- `compute_dense_merkle_root_from_values(values)` — сначала хеширует значения, затем строит дерево

## Сериализация блобов чанков

Блобы чанков — это неизменяемые архивы, создаваемые компактификацией. Сериализатор
автоматически выбирает наиболее компактный формат на основе размеров записей:

**Формат фиксированного размера** (флаг `0x01`) — когда все записи одинаковой длины:

```text
┌──────┬──────────┬─────────────┬─────────┬─────────┬─────────┐
│ 0x01 │ count    │ entry_size  │ entry_0 │ entry_1 │ ...     │
│ 1B   │ 4B (BE)  │ 4B (BE)     │ N bytes │ N bytes │         │
└──────┴──────────┴─────────────┴─────────┴─────────┴─────────┘
Total: 1 + 4 + 4 + (count × entry_size) bytes
```

**Формат переменного размера** (флаг `0x00`) — когда записи имеют разную длину:

```text
┌──────┬──────────┬─────────┬──────────┬─────────┬─────────────┐
│ 0x00 │ len_0    │ entry_0 │ len_1    │ entry_1 │ ...         │
│ 1B   │ 4B (BE)  │ N bytes │ 4B (BE)  │ M bytes │             │
└──────┴──────────┴─────────┴──────────┴─────────┴─────────────┘
Total: 1 + Σ(4 + len_i) bytes
```

Формат фиксированного размера экономит 4 байта на запись по сравнению с форматом
переменного размера, что существенно при больших чанках с данными одинакового размера
(например, 32-байтовые хеш-коммитменты).
Для 1024 записей по 32 байта каждая:
- Фиксированный: `1 + 4 + 4 + 32768 = 32 777 байт`
- Переменный: `1 + 1024 × (4 + 32) = 36 865 байт`
- Экономия: ~11%

## Схема ключей хранения

Все данные BulkAppendTree хранятся в пространстве имён **data**, с однобуквенными префиксами:

| Шаблон ключа | Формат | Размер | Назначение |
|---|---|---|---|
| `M` | 1 байт | 1Б | Ключ метаданных |
| `b` + `{index}` | `b` + u32 BE | 5Б | Запись буфера по индексу |
| `e` + `{index}` | `e` + u64 BE | 9Б | Блоб чанка по индексу |
| `m` + `{pos}` | `m` + u64 BE | 9Б | Узел MMR по позиции |

**Метаданные** хранят `mmr_size` (8 байт BE). `total_count` и `chunk_power` хранятся
в самом Element (в родительском Merk), а не в метаданных пространства данных.
Такое разделение означает, что чтение счётчика — это простой поиск элемента без
открытия контекста хранения данных.

Ключи буфера используют u32-индексы (от 0 до `chunk_size - 1`), поскольку ёмкость
буфера ограничена `chunk_size` (u32, вычисляемый как `1u32 << chunk_power`). Ключи
чанков используют u64-индексы, поскольку количество завершённых чанков может расти
неограниченно.

## Структура BulkAppendTree

```rust
pub struct BulkAppendTree<S> {
    pub total_count: u64,                        // Total values ever appended
    pub dense_tree: DenseFixedSizedMerkleTree<S>, // The buffer (dense tree)
}
```

Буфер ЯВЛЯЕТСЯ `DenseFixedSizedMerkleTree` — его корневой хеш и есть `dense_tree_root`.

**Аксессоры:**
- `capacity() -> u16`: `dense_tree.capacity()` (= `2^height - 1`)
- `epoch_size() -> u64`: `capacity + 1` (= `2^height`, количество записей на чанк)
- `height() -> u8`: `dense_tree.height()`

**Производные значения** (не хранятся):

| Значение | Формула |
|---|---|
| `chunk_count` | `total_count / epoch_size` |
| `buffer_count` | `dense_tree.count()` |
| `mmr_size` | `leaf_count_to_mmr_size(chunk_count)` |

## Операции GroveDB

BulkAppendTree интегрируется с GroveDB через шесть операций, определённых в
`grovedb/src/operations/bulk_append_tree.rs`:

### bulk_append

Основная мутирующая операция. Следует стандартному паттерну хранения не-Merk данных GroveDB:

```text
1. Validate element is BulkAppendTree
2. Open data storage context
3. Load tree from store
4. Append value (may trigger compaction)
5. Update element in parent Merk with new state_root + total_count
6. Propagate changes up through Merk hierarchy
7. Commit transaction
```

Адаптер `AuxBulkStore` оборачивает вызовы `get_aux`/`put_aux`/`delete_aux` GroveDB и
накапливает `OperationCost` в `RefCell` для отслеживания затрат. Затраты хеширования
от операции добавления прибавляются к `cost.hash_node_calls`.

### Операции чтения

| Операция | Что возвращает | Использует хранилище данных? |
|---|---|---|
| `bulk_get_value(path, key, position)` | Значение по глобальной позиции | Да — читает из блоба чанка или буфера |
| `bulk_get_chunk(path, key, chunk_index)` | Сырой блоб чанка | Да — читает ключ чанка |
| `bulk_get_buffer(path, key)` | Все текущие записи буфера | Да — читает ключи буфера |
| `bulk_count(path, key)` | Общий счётчик (u64) | Нет — читает из элемента |
| `bulk_chunk_count(path, key)` | Завершённые чанки (u64) | Нет — вычисляется из элемента |

Операция `get_value` прозрачно маршрутизирует по позиции:

```text
if position < completed_chunks × chunk_size:
    chunk_idx = position / chunk_size
    intra_idx = position % chunk_size
    → read chunk blob, deserialize, return entry[intra_idx]
else:
    buffer_idx = position % chunk_size
    → read buffer_key(buffer_idx)
```

## Пакетные операции и предобработка

BulkAppendTree поддерживает пакетные операции через вариант `GroveOp::BulkAppend`.
Поскольку `execute_ops_on_path` не имеет доступа к контексту хранения данных, все операции
BulkAppend должны быть предобработаны до `apply_body`.

Конвейер предобработки:

```text
Input: [BulkAppend{v1}, Insert{...}, BulkAppend{v2}, BulkAppend{v3}]
                                     ↑ same (path,key) as v1

Step 1: Group BulkAppend ops by (path, key)
        group_1: [v1, v2, v3]

Step 2: For each group:
        a. Read existing element → get (total_count, chunk_power)
        b. Open transactional storage context
        c. Load BulkAppendTree from store
        d. Load existing buffer into memory (Vec<Vec<u8>>)
        e. For each value:
           tree.append_with_mem_buffer(store, value, &mut mem_buffer)
        f. Save metadata
        g. Compute final state_root

Step 3: Replace all BulkAppend ops with one ReplaceNonMerkTreeRoot per group
        carrying: hash=state_root, meta=BulkAppendTree{total_count, chunk_power}

Output: [ReplaceNonMerkTreeRoot{...}, Insert{...}]
```

Вариант `append_with_mem_buffer` избегает проблем чтения-после-записи: записи буфера
отслеживаются в `Vec<Vec<u8>>` в памяти, поэтому компактификация может их прочитать,
даже если транзакционное хранилище ещё не зафиксировано.

## Трейт BulkStore

```rust
pub trait BulkStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String>;
    fn delete(&self, key: &[u8]) -> Result<(), String>;
}
```

Методы принимают `&self` (не `&mut self`) для соответствия паттерну внутренней
изменяемости GroveDB, где записи идут через пакет. Интеграция с GroveDB реализует это
через `AuxBulkStore`, оборачивающий `StorageContext` и накапливающий `OperationCost`.

`MmrAdapter` связывает `BulkStore` с трейтами `MMRStoreReadOps`/`MMRStoreWriteOps`
из ckb MMR, добавляя сквозной кеш для корректности чтения-после-записи.

## Генерация доказательств

Доказательства BulkAppendTree поддерживают **запросы диапазонов** по позициям. Структура
доказательства содержит всё необходимое для того, чтобы верификатор без доступа к базе
данных мог подтвердить наличие определённых данных в дереве:

```rust
pub struct BulkAppendTreeProof {
    pub chunk_power: u8,
    pub total_count: u64,
    pub chunk_blobs: Vec<(u64, Vec<u8>)>,         // Full chunk blobs
    pub chunk_mmr_size: u64,
    pub chunk_mmr_proof_items: Vec<[u8; 32]>,      // MMR sibling hashes
    pub chunk_mmr_leaves: Vec<(u64, [u8; 32])>,    // (leaf_idx, dense_root)
    pub buffer_entries: Vec<Vec<u8>>,               // ALL buffer entries
    pub chunk_mmr_root: [u8; 32],
}
```

**Шаги генерации** для диапазона `[start, end)` (с `chunk_size = 1u32 << chunk_power`):

```text
1. Determine overlapping chunks
   first_chunk = start / chunk_size
   last_chunk  = min((end-1) / chunk_size, completed_chunks - 1)

2. Read chunk blobs for overlapping chunks
   For each chunk_idx in [first_chunk, last_chunk]:
     chunk_blobs.push((chunk_idx, store.get(chunk_key(idx))))

3. Compute dense Merkle root for each chunk blob
   For each blob:
     deserialize → values
     dense_root = compute_dense_merkle_root_from_values(values)
     chunk_mmr_leaves.push((chunk_idx, dense_root))

4. Generate MMR proof for those chunk positions
   positions = chunk_indices.map(|idx| leaf_to_pos(idx))
   proof = mmr.gen_proof(positions)
   chunk_mmr_proof_items = proof.proof_items().map(|n| n.hash)

5. Get chunk MMR root

6. Read ALL buffer entries (bounded by chunk_size)
   for i in 0..buffer_count:
     buffer_entries.push(store.get(buffer_key(i)))
```

**Почему включаются ВСЕ записи буфера?** Буфер — это плотное дерево Меркла, чей корневой
хеш фиксирует каждую запись. Верификатор должен перестроить дерево из всех записей, чтобы
проверить `dense_tree_root`. Поскольку буфер ограничен `capacity` (не более 65 535 записей),
это разумная цена.

## Верификация доказательств

Верификация — это чистая функция, не требующая доступа к базе данных. Она выполняет пять проверок:

```text
Step 0: Metadata consistency
        - chunk_power <= 31
        - buffer_entries.len() == total_count % chunk_size
        - MMR leaf count == completed_chunks

Step 1: Chunk blob integrity
        For each (chunk_idx, blob):
          values = deserialize(blob)
          computed_root = dense_merkle_root(values)
          assert computed_root == chunk_mmr_leaves[chunk_idx]

Step 2: Chunk MMR proof
        Reconstruct MmrNode leaves and proof items
        proof.verify(chunk_mmr_root, leaves) == true

Step 3: Buffer (dense tree) integrity
        Rebuild DenseFixedSizedMerkleTree from buffer_entries
        dense_tree_root = compute root hash of rebuilt tree

Step 4: State root
        computed = blake3("bulk_state" || chunk_mmr_root || dense_tree_root)
        assert computed == expected_state_root
```

После успешной верификации `BulkAppendTreeProofResult` предоставляет метод
`values_in_range(start, end)`, извлекающий конкретные значения из верифицированных
блобов чанков и записей буфера.

## Связь с корневым хешем GroveDB

BulkAppendTree — это **не-Merk дерево**: данные хранятся в пространстве данных (data namespace),
а не в дочернем поддереве Merk. В родительском Merk элемент хранится как:

```text
Element::BulkAppendTree(total_count, chunk_power, flags)
```

Корень состояния передаётся как дочерний хеш Merk. Хеш родительского узла Merk:

```text
combine_hash(value_hash(element_bytes), state_root)
```

`state_root` передаётся как дочерний хеш Merk (через параметр `subtree_root_hash`
функции `insert_subtree`). Любое изменение корня состояния распространяется вверх через
иерархию Merk GroveDB до корневого хеша.

В V1-доказательствах (§9.6) доказательство родительского Merk подтверждает байты элемента
и привязку дочернего хеша, а `BulkAppendTreeProof` доказывает, что запрошенные данные
согласуются с `state_root`, использованным как дочерний хеш.

## Отслеживание затрат

Стоимость хеширования каждой операции отслеживается явно:

| Операция | Вызовы Blake3 | Примечания |
|---|---|---|
| Одно добавление (без компактификации) | 3 | 2 для хеш-цепочки буфера + 1 для корня состояния |
| Одно добавление (с компактификацией) | 3 + 2C - 1 + ~2 | Цепочка + плотный корень Меркла (C=chunk_size) + MMR push + корень состояния |
| `get_value` из чанка | 0 | Чистая десериализация, без хеширования |
| `get_value` из буфера | 0 | Прямой поиск по ключу |
| Генерация доказательства | Зависит от количества чанков | Плотный корень Меркла на чанк + доказательство MMR |
| Верификация доказательства | 2C·K - K + B·2 + 1 | K чанков, B записей буфера, C размер чанка |

**Амортизированная стоимость на добавление**: Для chunk_size=1024 (chunk_power=10) накладные расходы
компактификации в ~2047 хешей (плотный корень Меркла) амортизируются по 1024 добавлениям,
добавляя ~2 хеша на добавление. В сочетании с 3 хешами на добавление амортизированный итог
составляет **~5 вызовов blake3 на добавление** — очень эффективно для криптографически
аутентифицированной структуры.

## Сравнение с MmrTree

| | BulkAppendTree | MmrTree |
|---|---|---|
| **Архитектура** | Двухуровневая (буфер + chunk MMR) | Один MMR |
| **Стоимость хеширования на добавление** | 3 (+ амортизированные ~2 для компактификации) | ~2 |
| **Гранулярность доказательств** | Запросы диапазонов по позициям | Доказательства отдельных листьев |
| **Неизменяемые снимки** | Да (блобы чанков) | Нет |
| **Пригодность для CDN** | Да (блобы чанков кешируемы) | Нет |
| **Записи буфера** | Да (все нужны для доказательства) | Неприменимо |
| **Лучше всего для** | Высокопроизводительные журналы, массовая синхронизация | Журналы событий, индивидуальные поиски |
| **Дискриминант элемента** | 13 | 12 |
| **TreeType** | 9 | 8 |

Выбирайте MmrTree, когда вам нужны доказательства отдельных листьев с минимальными
накладными расходами. Выбирайте BulkAppendTree, когда вам нужны запросы диапазонов,
массовая синхронизация и снимки на основе чанков.

## Файлы реализации

| Файл | Назначение |
|------|---------|
| `grovedb-bulk-append-tree/src/lib.rs` | Корень крейта, реэкспорты |
| `grovedb-bulk-append-tree/src/tree/mod.rs` | Структура `BulkAppendTree`, аксессоры состояния, хранение метаданных |
| `grovedb-bulk-append-tree/src/tree/append.rs` | `append()`, `append_with_mem_buffer()`, `compact_entries()` |
| `grovedb-bulk-append-tree/src/tree/hash.rs` | `compute_state_root` |
| `grovedb-bulk-append-tree/src/tree/keys.rs` | `META_KEY`, `buffer_key`, `chunk_key`, `mmr_node_key` |
| `grovedb-bulk-append-tree/src/tree/query.rs` | `get_value`, `get_chunk`, `get_buffer` |
| `grovedb-bulk-append-tree/src/tree/mmr_adapter.rs` | `MmrAdapter` со сквозным кешем |
| `grovedb-bulk-append-tree/src/chunk.rs` | Сериализация блобов чанков (фиксированный + переменный форматы) |
| `grovedb-bulk-append-tree/src/proof.rs` | `BulkAppendTreeProof` генерация и верификация |
| `grovedb-bulk-append-tree/src/store.rs` | Трейт `BulkStore` |
| `grovedb-bulk-append-tree/src/error.rs` | Перечисление `BulkAppendError` |
| `grovedb/src/operations/bulk_append_tree.rs` | Операции GroveDB, `AuxBulkStore`, предобработка пакетов |
| `grovedb/src/tests/bulk_append_tree_tests.rs` | 27 интеграционных тестов |

---
