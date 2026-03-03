# Хеширование — Криптографическая целостность

Каждый узел в дереве Merk хешируется для создания **корневого хеша** — единственного 32-байтового значения, аутентифицирующего всё дерево. Любое изменение любого ключа, значения или структурной связи приводит к другому корневому хешу.

## Трёхуровневая иерархия хешей

Merk использует трёхуровневую схему хеширования, от внутреннего к внешнему:

Пример: key = `"bob"` (3 байта), value = `"hello"` (5 байт):

```mermaid
graph LR
    subgraph level1["Уровень 1: value_hash"]
        V_IN["varint(5) ‖ &quot;hello&quot;"]
        V_BLAKE["Blake3"]
        V_OUT(["value_hash<br/><small>32 байта</small>"])
        V_IN --> V_BLAKE --> V_OUT
    end

    subgraph level2["Уровень 2: kv_hash"]
        K_IN["varint(3) ‖ &quot;bob&quot; ‖ value_hash"]
        K_BLAKE["Blake3"]
        K_OUT(["kv_hash<br/><small>32 байта</small>"])
        K_IN --> K_BLAKE --> K_OUT
    end

    subgraph level3["Уровень 3: node_hash"]
        N_LEFT(["left_child_hash<br/><small>32Б (или NULL_HASH)</small>"])
        N_KV(["kv_hash<br/><small>32Б</small>"])
        N_RIGHT(["right_child_hash<br/><small>32Б (или NULL_HASH)</small>"])
        N_BLAKE["Blake3<br/><small>96Б вход = 2 блока</small>"]
        N_OUT(["node_hash<br/><small>32 байта</small>"])
        N_LEFT --> N_BLAKE
        N_KV --> N_BLAKE
        N_RIGHT --> N_BLAKE
        N_BLAKE --> N_OUT
    end

    V_OUT -.-> K_IN
    K_OUT -.-> N_KV

    style level1 fill:#eaf2f8,stroke:#2980b9
    style level2 fill:#fef9e7,stroke:#f39c12
    style level3 fill:#fdedec,stroke:#e74c3c
```

> КОРЕНЬ дерева = `node_hash` корневого узла — аутентифицирует **каждый** ключ, значение и структурную связь. Отсутствующие потомки используют `NULL_HASH = [0x00; 32]`.

### Уровень 1: value_hash

```rust
// merk/src/tree/hash.rs
pub fn value_hash(value: &[u8]) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    let val_length = value.len().encode_var_vec();  // Varint encoding
    hasher.update(val_length.as_slice());
    hasher.update(value);
    // ...
}
```

Длина значения кодируется в формате **varint** и добавляется в начало. Это критически важно для устойчивости к коллизиям — без этого `H("AB" ‖ "C")` было бы равно `H("A" ‖ "BC")`.

### Уровень 2: kv_hash

```rust
pub fn kv_hash(key: &[u8], value: &[u8]) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    let key_length = key.len().encode_var_vec();
    hasher.update(key_length.as_slice());
    hasher.update(key);
    let vh = value_hash(value);
    hasher.update(vh.as_slice());  // Nested hash
    // ...
}
```

Это привязывает ключ к значению. Для верификации доказательств существует также вариант, принимающий предвычисленный value_hash:

```rust
pub fn kv_digest_to_kv_hash(key: &[u8], value_hash: &CryptoHash) -> CostContext<CryptoHash>
```

Используется, когда верификатор уже имеет value_hash (например, для поддеревьев, где value_hash является комбинированным хешем).

### Уровень 3: node_hash

```rust
pub fn node_hash(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);       // 32 bytes
    hasher.update(left);     // 32 bytes
    hasher.update(right);    // 32 bytes — total 96 bytes
    // Always exactly 2 hash operations (96 bytes / 64-byte block = 2)
}
```

Если потомок отсутствует, его хеш — это **NULL_HASH** — 32 нулевых байта:

```rust
pub const NULL_HASH: CryptoHash = [0; HASH_LENGTH];  // [0u8; 32]
```

## Blake3 как хеш-функция

GroveDB использует **Blake3** для всего хеширования. Ключевые свойства:

- **256-битный выход** (32 байта)
- **Размер блока**: 64 байта
- **Скорость**: ~в 3 раза быстрее SHA-256 на современном оборудовании
- **Потоковая обработка**: можно инкрементально подавать данные

Стоимость операции хеширования рассчитывается на основе количества обрабатываемых 64-байтовых блоков:

```rust
let hashes = 1 + (hasher.count() - 1) / 64;  // Number of hash operations
```

## Кодирование длины для устойчивости к коллизиям

Каждый вход переменной длины предваряется своей длиной с использованием **кодирования varint**:

```mermaid
graph LR
    subgraph bad["Без префикса длины — УЯЗВИМО"]
        BAD1["H(&quot;AB&quot; + &quot;C&quot;) = H(0x41 0x42 0x43)"]
        BAD2["H(&quot;A&quot; + &quot;BC&quot;) = H(0x41 0x42 0x43)"]
        BAD1 --- SAME["ОДИНАКОВЫЙ ХЕШ!"]
        BAD2 --- SAME
    end

    subgraph good["С префиксом длины — устойчиво к коллизиям"]
        GOOD1["H([02] 0x41 0x42)<br/><small>varint(2) ‖ &quot;AB&quot;</small>"]
        GOOD2["H([03] 0x41 0x42 0x43)<br/><small>varint(3) ‖ &quot;ABC&quot;</small>"]
        GOOD1 --- DIFF["РАЗЛИЧНЫЙ"]
        GOOD2 --- DIFF
    end

    style bad fill:#fadbd8,stroke:#e74c3c
    style good fill:#d5f5e3,stroke:#27ae60
    style SAME fill:#e74c3c,color:#fff,stroke:#c0392b
    style DIFF fill:#27ae60,color:#fff,stroke:#229954
```

> **Вход value_hash**: `[varint(value.len)] [байты значения]`
> **Вход kv_hash**: `[varint(key.len)] [байты ключа] [value_hash: 32 байта]`

Без префиксов длины злоумышленник мог бы создать разные пары ключ-значение, хешируемые в один и тот же дайджест. Префикс длины делает это криптографически невозможным.

## Комбинированное хеширование для специальных элементов

Для **поддеревьев** и **ссылок** `value_hash` вычисляется не просто как `H(value)`. Вместо этого используется **комбинированный хеш**, привязывающий элемент к его цели:

```mermaid
graph LR
    subgraph item["Обычный Item"]
        I_val["байты значения"] --> I_hash["H(len ‖ bytes)"] --> I_vh(["value_hash"])
    end

    subgraph subtree["Элемент поддерева"]
        S_elem["байты элемента дерева"] --> S_hash1["H(len ‖ bytes)"]
        S_root(["корневой хеш<br/>дочернего Merk"])
        S_hash1 --> S_combine["combine_hash()<br/><small>Blake3(a ‖ b)</small>"]
        S_root --> S_combine
        S_combine --> S_vh(["value_hash"])
    end

    subgraph reference["Элемент ссылки"]
        R_elem["байты элемента ссылки"] --> R_hash1["H(len ‖ bytes)"]
        R_target["целевое значение"] --> R_hash2["H(len ‖ bytes)"]
        R_hash1 --> R_combine["combine_hash()"]
        R_hash2 --> R_combine
        R_combine --> R_vh(["value_hash"])
    end

    style item fill:#eaf2f8,stroke:#2980b9
    style subtree fill:#fef9e7,stroke:#f39c12
    style reference fill:#fdedec,stroke:#e74c3c
```

> **Поддерево:** привязывает корневой хеш дочернего Merk к родителю. **Ссылка:** привязывает и путь ссылки, И целевое значение. Изменение любого из них изменяет корневой хеш.

Функция `combine_hash`:

```rust
pub fn combine_hash(hash_one: &CryptoHash, hash_two: &CryptoHash) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(hash_one);   // 32 bytes
    hasher.update(hash_two);   // 32 bytes — total 64 bytes, exactly 1 hash op
    // ...
}
```

Именно это позволяет GroveDB аутентифицировать всю иерархию через единственный корневой хеш — value_hash каждого родительского дерева для элемента-поддерева включает корневой хеш дочернего дерева.

## Агрегированное хеширование для ProvableCountTree

Узлы `ProvableCountTree` включают агрегированный счётчик в хеш узла:

```rust
pub fn node_hash_with_count(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
    count: u64,
) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);                        // 32 bytes
    hasher.update(left);                      // 32 bytes
    hasher.update(right);                     // 32 bytes
    hasher.update(&count.to_be_bytes());      // 8 bytes — total 104 bytes
    // Still exactly 2 hash ops (104 < 128 = 2 × 64)
}
```

Это означает, что доказательство количества не требует раскрытия фактических данных — счётчик заложен в криптографическое обязательство.

---
