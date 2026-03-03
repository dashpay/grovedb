# Иерархическая роща — Дерево деревьев

## Как поддеревья вкладываются в родительские деревья

Определяющая особенность GroveDB заключается в том, что дерево Merk может содержать элементы, которые сами являются деревьями Merk. Это создаёт **иерархическое пространство имён**:

```mermaid
graph TD
    subgraph root["КОРНЕВОЕ ДЕРЕВО MERK — путь: []"]
        contracts["&quot;contracts&quot;<br/>Tree"]
        identities["&quot;identities&quot;<br/>Tree"]
        balances["&quot;balances&quot;<br/>SumTree, sum=0"]
        contracts --> identities
        contracts --> balances
    end

    subgraph ident["MERK IDENTITIES — путь: [&quot;identities&quot;]"]
        bob456["&quot;bob456&quot;<br/>Tree"]
        alice123["&quot;alice123&quot;<br/>Tree"]
        eve["&quot;eve&quot;<br/>Item"]
        bob456 --> alice123
        bob456 --> eve
    end

    subgraph bal["MERK BALANCES (SumTree) — путь: [&quot;balances&quot;]"]
        bob_bal["&quot;bob456&quot;<br/>SumItem(800)"]
    end

    subgraph alice_tree["MERK ALICE123 — путь: [&quot;identities&quot;, &quot;alice123&quot;]"]
        name["&quot;name&quot;<br/>Item(&quot;Al&quot;)"]
        balance_item["&quot;balance&quot;<br/>SumItem(1000)"]
        docs["&quot;docs&quot;<br/>Tree"]
        name --> balance_item
        name --> docs
    end

    identities -.-> bob456
    balances -.-> bob_bal
    alice123 -.-> name
    docs -.->|"... ещё поддеревья"| more["..."]

    style root fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style ident fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style bal fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style alice_tree fill:#e8daef,stroke:#8e44ad,stroke-width:2px
    style more fill:none,stroke:none
```

> Каждый цветной блок — это отдельное дерево Merk. Пунктирные стрелки представляют связи-порталы из элементов Tree к их дочерним деревьям Merk. Путь к каждому Merk показан в его метке.

## Система адресации по путям

Каждый элемент в GroveDB адресуется **путём** (path) — последовательностью байтовых строк, которые ведут от корня через поддеревья к целевому ключу:

```text
    Путь: ["identities", "alice123", "name"]

    Шаг 1: В корневом дереве ищем "identities" → элемент Tree
    Шаг 2: Открываем поддерево identities, ищем "alice123" → элемент Tree
    Шаг 3: Открываем поддерево alice123, ищем "name" → Item("Alice")
```

Пути представляются как `Vec<Vec<u8>>` или с использованием типа `SubtreePath` для эффективной работы без аллокаций:

```rust
// The path to the element (all segments except the last)
let path: &[&[u8]] = &[b"identities", b"alice123"];
// The key within the final subtree
let key: &[u8] = b"name";
```

## Генерация префиксов Blake3 для изоляции хранилища

Каждое поддерево в GroveDB получает собственное **изолированное пространство имён** в RocksDB. Пространство имён определяется хешированием пути через Blake3:

```rust
pub type SubtreePrefix = [u8; 32];

// The prefix is computed by hashing the path segments
// storage/src/rocksdb_storage/storage.rs
```

Например:

```text
    Путь: ["identities", "alice123"]
    Префикс: Blake3(["identities", "alice123"]) = [0xab, 0x3f, ...]  (32 байта)

    В RocksDB ключи для этого поддерева хранятся как:
    [prefix: 32 байта][original_key]

    Таким образом, "name" в этом поддереве становится:
    [0xab, 0x3f, ...][0x6e, 0x61, 0x6d, 0x65]  ("name")
```

Это обеспечивает:
- Отсутствие коллизий ключей между поддеревьями (32-байтовый префикс = 256-битная изоляция)
- Эффективное вычисление префикса (один хеш Blake3 по байтам пути)
- Колокация данных поддерева в RocksDB для эффективности кеша

## Распространение корневого хеша через иерархию

Когда значение изменяется глубоко в роще, изменение должно **распространиться вверх** для обновления корневого хеша:

```text
    Изменение: Обновить "name" на "ALICE" в identities/alice123/

    Шаг 1: Обновить значение в дереве Merk alice123
            → дерево alice123 получает новый корневой хеш: H_alice_new

    Шаг 2: Обновить элемент "alice123" в дереве identities
            → value_hash дерева identities для "alice123" =
              combine_hash(H(tree_element_bytes), H_alice_new)
            → дерево identities получает новый корневой хеш: H_ident_new

    Шаг 3: Обновить элемент "identities" в корневом дереве
            → value_hash корневого дерева для "identities" =
              combine_hash(H(tree_element_bytes), H_ident_new)
            → КОРНЕВОЙ ХЕШ меняется
```

```mermaid
graph TD
    subgraph step3["ШАГ 3: Обновление корневого дерева"]
        R3["Корневое дерево пересчитывает:<br/>value_hash для &quot;identities&quot; =<br/>combine_hash(H(tree_bytes), H_ident_NEW)<br/>→ новый КОРНЕВОЙ ХЕШ"]
    end
    subgraph step2["ШАГ 2: Обновление дерева identities"]
        R2["Дерево identities пересчитывает:<br/>value_hash для &quot;alice123&quot; =<br/>combine_hash(H(tree_bytes), H_alice_NEW)<br/>→ новый корневой хеш: H_ident_NEW"]
    end
    subgraph step1["ШАГ 1: Обновление Merk alice123"]
        R1["Дерево alice123 пересчитывает:<br/>value_hash(&quot;ALICE&quot;) → новый kv_hash<br/>→ новый корневой хеш: H_alice_NEW"]
    end

    R1 -->|"H_alice_NEW передаётся вверх"| R2
    R2 -->|"H_ident_NEW передаётся вверх"| R3

    style step1 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style step2 fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style step3 fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

**До и после** — изменённые узлы выделены красным:

```mermaid
graph TD
    subgraph before["ДО"]
        B_root["Root: aabb1122"]
        B_ident["&quot;identities&quot;: cc44.."]
        B_contracts["&quot;contracts&quot;: 1234.."]
        B_balances["&quot;balances&quot;: 5678.."]
        B_alice["&quot;alice123&quot;: ee55.."]
        B_bob["&quot;bob456&quot;: bb22.."]
        B_name["&quot;name&quot;: 7f.."]
        B_docs["&quot;docs&quot;: a1.."]
        B_root --- B_ident
        B_root --- B_contracts
        B_root --- B_balances
        B_ident --- B_alice
        B_ident --- B_bob
        B_alice --- B_name
        B_alice --- B_docs
    end

    subgraph after["ПОСЛЕ"]
        A_root["Root: ff990033"]
        A_ident["&quot;identities&quot;: dd88.."]
        A_contracts["&quot;contracts&quot;: 1234.."]
        A_balances["&quot;balances&quot;: 5678.."]
        A_alice["&quot;alice123&quot;: 1a2b.."]
        A_bob["&quot;bob456&quot;: bb22.."]
        A_name["&quot;name&quot;: 3c.."]
        A_docs["&quot;docs&quot;: a1.."]
        A_root --- A_ident
        A_root --- A_contracts
        A_root --- A_balances
        A_ident --- A_alice
        A_ident --- A_bob
        A_alice --- A_name
        A_alice --- A_docs
    end

    style A_root fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style A_ident fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style A_alice fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style A_name fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
```

> Пересчитываются только узлы на пути от изменённого значения до корня. Соседние узлы и другие ветви остаются неизменными.

Распространение реализовано через `propagate_changes_with_transaction`, который обходит путь от изменённого поддерева до корня, обновляя хеш элемента каждого родителя по ходу.

## Пример многоуровневой структуры рощи

Вот полный пример, показывающий, как Dash Platform структурирует своё состояние:

```mermaid
graph TD
    ROOT["Корень GroveDB"]

    ROOT --> contracts["[01] &quot;data_contracts&quot;<br/>Tree"]
    ROOT --> identities["[02] &quot;identities&quot;<br/>Tree"]
    ROOT --> balances["[03] &quot;balances&quot;<br/>SumTree"]
    ROOT --> pools["[04] &quot;pools&quot;<br/>Tree"]

    contracts --> c1["contract_id_1<br/>Tree"]
    contracts --> c2["contract_id_2<br/>Tree"]
    c1 --> docs["&quot;documents&quot;<br/>Tree"]
    docs --> profile["&quot;profile&quot;<br/>Tree"]
    docs --> note["&quot;note&quot;<br/>Tree"]
    profile --> d1["doc_id_1<br/>Item"]
    profile --> d2["doc_id_2<br/>Item"]
    note --> d3["doc_id_3<br/>Item"]

    identities --> id1["identity_id_1<br/>Tree"]
    identities --> id2["identity_id_2<br/>Tree"]
    id1 --> keys["&quot;keys&quot;<br/>Tree"]
    id1 --> rev["&quot;revision&quot;<br/>Item(u64)"]
    keys --> k1["key_id_1<br/>Item(pubkey)"]
    keys --> k2["key_id_2<br/>Item(pubkey)"]

    balances --> b1["identity_id_1<br/>SumItem(balance)"]
    balances --> b2["identity_id_2<br/>SumItem(balance)"]

    style ROOT fill:#2c3e50,stroke:#2c3e50,color:#fff
    style contracts fill:#d4e6f1,stroke:#2980b9
    style identities fill:#d5f5e3,stroke:#27ae60
    style balances fill:#fef9e7,stroke:#f39c12
    style pools fill:#e8daef,stroke:#8e44ad
```

Каждый блок — это отдельное дерево Merk, аутентифицированное вплоть до единственного корневого хеша, на котором сходятся валидаторы.

---
