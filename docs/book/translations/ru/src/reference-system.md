# Система ссылок

## Зачем нужны ссылки

В иерархической базе данных часто необходим доступ к одним и тем же данным по нескольким путям. Например, документы могут храниться в рамках контракта, но также быть доступными для запроса по идентификатору владельца. **Ссылки** (References) — это ответ GroveDB: они являются указателями из одного места в другое, подобно символическим ссылкам в файловой системе.

```mermaid
graph LR
    subgraph primary["Основное хранилище"]
        item["contracts/C1/docs/D1<br/><b>Item</b>(data)"]
    end
    subgraph secondary["Вторичный индекс"]
        ref["identities/alice/docs/D1<br/><b>Reference</b>"]
    end
    ref -->|"указывает на"| item

    style primary fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style secondary fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style ref fill:#fef9e7,stroke:#f39c12,stroke-width:2px
```

Ключевые свойства:
- Ссылки **аутентифицированы** — value_hash ссылки включает как саму ссылку, так и целевой элемент
- Ссылки могут быть **цепочечными** — ссылка может указывать на другую ссылку
- Обнаружение циклов предотвращает бесконечные петли
- Настраиваемый лимит переходов предотвращает исчерпание ресурсов

## Семь типов ссылок

```rust
// grovedb-element/src/reference_path/mod.rs
pub enum ReferencePathType {
    AbsolutePathReference(Vec<Vec<u8>>),
    UpstreamRootHeightReference(u8, Vec<Vec<u8>>),
    UpstreamRootHeightWithParentPathAdditionReference(u8, Vec<Vec<u8>>),
    UpstreamFromElementHeightReference(u8, Vec<Vec<u8>>),
    CousinReference(Vec<u8>),
    RemovedCousinReference(Vec<Vec<u8>>),
    SiblingReference(Vec<u8>),
}
```

Рассмотрим каждый тип с диаграммами.

### AbsolutePathReference

Простейший тип. Хранит полный путь к цели:

```mermaid
graph TD
    subgraph root["Корневой Merk — путь: []"]
        A["A<br/>Tree"]
        P["P<br/>Tree"]
    end

    subgraph merkA["Merk [A]"]
        B["B<br/>Tree"]
    end

    subgraph merkP["Merk [P]"]
        Q["Q<br/>Tree"]
    end

    subgraph merkAB["Merk [A, B]"]
        X["X = Reference<br/>AbsolutePathRef([P, Q, R])"]
    end

    subgraph merkPQ["Merk [P, Q]"]
        R["R = Item<br/>&quot;target&quot;"]
    end

    A -.-> B
    P -.-> Q
    B -.-> X
    Q -.-> R
    X ==>|"разрешается в [P, Q, R]"| R

    style merkAB fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style merkPQ fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style X fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style R fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

> X хранит полный абсолютный путь `[P, Q, R]`. Независимо от расположения X, он всегда разрешается в ту же цель.

### UpstreamRootHeightReference

Сохраняет первые N сегментов текущего пути, затем добавляет новый путь:

```mermaid
graph TD
    subgraph resolve["Разрешение: сохранить первые 2 сегмента + добавить [P, Q]"]
        direction LR
        curr["текущий: [A, B, C, D]"] --> keep["сохранить первые 2: [A, B]"] --> append["добавить: [A, B, <b>P, Q</b>]"]
    end

    subgraph grove["Иерархия рощи"]
        gA["A (высота 0)"]
        gB["B (высота 1)"]
        gC["C (высота 2)"]
        gD["D (высота 3)"]
        gX["X = Reference<br/>UpstreamRootHeight(2, [P,Q])"]
        gP["P (высота 2)"]
        gQ["Q (высота 3) — цель"]

        gA --> gB
        gB --> gC
        gB -->|"сохранить первые 2 → [A,B]<br/>затем спуститься по [P,Q]"| gP
        gC --> gD
        gD -.-> gX
        gP --> gQ
    end

    gX ==>|"разрешается в"| gQ

    style resolve fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style gX fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style gQ fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

### UpstreamRootHeightWithParentPathAdditionReference

Как UpstreamRootHeight, но добавляет обратно последний сегмент текущего пути:

```text
    Ссылка по пути [A, B, C, D, E] ключ=X
    UpstreamRootHeightWithParentPathAdditionReference(2, [P, Q])

    Текущий путь:       [A, B, C, D, E]
    Сохранить первые 2: [A, B]
    Добавить [P, Q]:    [A, B, P, Q]
    Добавить последний: [A, B, P, Q, E]   ← "E" из исходного пути добавлен обратно

    Полезно для: индексов, где нужно сохранить ключ родителя
```

### UpstreamFromElementHeightReference

Отбрасывает последние N сегментов, затем добавляет:

```text
    Ссылка по пути [A, B, C, D] ключ=X
    UpstreamFromElementHeightReference(1, [P, Q])

    Текущий путь:       [A, B, C, D]
    Отбросить последний: [A, B, C]
    Добавить [P, Q]:    [A, B, C, P, Q]
```

### CousinReference

Заменяет только непосредственного родителя новым ключом:

```mermaid
graph TD
    subgraph resolve["Разрешение: убрать последние 2, добавить cousin C, добавить ключ X"]
        direction LR
        r1["путь: [A, B, M, D]"] --> r2["убрать последние 2: [A, B]"] --> r3["добавить C: [A, B, C]"] --> r4["добавить ключ X: [A, B, C, X]"]
    end

    subgraph merkAB["Merk [A, B]"]
        M["M<br/>Tree"]
        C["C<br/>Tree<br/>(кузен M)"]
    end

    subgraph merkABM["Merk [A, B, M]"]
        D["D<br/>Tree"]
    end

    subgraph merkABMD["Merk [A, B, M, D]"]
        Xref["X = Reference<br/>CousinReference(C)"]
    end

    subgraph merkABC["Merk [A, B, C]"]
        Xtarget["X = Item<br/>(цель)"]
    end

    M -.-> D
    D -.-> Xref
    C -.-> Xtarget
    Xref ==>|"разрешается в [A, B, C, X]"| Xtarget

    style resolve fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style Xref fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style Xtarget fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style M fill:#fadbd8,stroke:#e74c3c
    style C fill:#d5f5e3,stroke:#27ae60
```

> «Кузен» — это соседнее поддерево прародителя ссылки. Ссылка поднимается на два уровня вверх, затем спускается в поддерево кузена.

### RemovedCousinReference

Как CousinReference, но заменяет родителя многосегментным путём:

```text
    Ссылка по пути [A, B, C, D] ключ=X
    RemovedCousinReference([M, N])

    Текущий путь:   [A, B, C, D]
    Убрать родителя C: [A, B]
    Добавить [M, N]: [A, B, M, N]
    Добавить ключ X: [A, B, M, N, X]
```

### SiblingReference

Простейшая относительная ссылка — просто меняет ключ в рамках того же родителя:

```mermaid
graph TD
    subgraph merk["Merk [A, B, C] — одно дерево, один путь"]
        M_sib["M"]
        X_sib["X = Reference<br/>SiblingRef(Y)"]
        Y_sib["Y = Item<br/>(цель)"]
        Z_sib["Z = Item"]
        M_sib --> X_sib
        M_sib --> Y_sib
    end

    X_sib ==>|"разрешается в [A, B, C, Y]"| Y_sib

    style merk fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style X_sib fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style Y_sib fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

> Простейший тип ссылки. X и Y — братья в одном дереве Merk: разрешение просто меняет ключ, сохраняя тот же путь.

## Следование по ссылкам и лимит переходов

Когда GroveDB встречает элемент Reference, она должна **пройти по нему**, чтобы найти фактическое значение. Поскольку ссылки могут указывать на другие ссылки, это включает цикл:

```rust
// grovedb/src/reference_path.rs
pub const MAX_REFERENCE_HOPS: usize = 10;

pub fn follow_reference(...) -> CostResult<ResolvedReference, Error> {
    let mut hops_left = MAX_REFERENCE_HOPS;
    let mut visited = HashSet::new();

    while hops_left > 0 {
        // Resolve reference path to absolute path
        let target_path = current_ref.absolute_qualified_path(...);

        // Check for cycles
        if !visited.insert(target_path.clone()) {
            return Err(Error::CyclicReference);
        }

        // Fetch element at target
        let element = Element::get(target_path);

        match element {
            Element::Reference(next_ref, ..) => {
                // Still a reference — keep following
                current_ref = next_ref;
                hops_left -= 1;
            }
            other => {
                // Found the actual element!
                return Ok(ResolvedReference { element: other, ... });
            }
        }
    }

    Err(Error::ReferenceLimit)  // Exceeded 10 hops
}
```

## Обнаружение циклов

`HashSet` `visited` отслеживает все пути, которые мы уже видели. Если мы встречаем путь, который уже посещали, значит, обнаружен цикл:

```mermaid
graph LR
    A["A<br/>Reference"] -->|"шаг 1"| B["B<br/>Reference"]
    B -->|"шаг 2"| C["C<br/>Reference"]
    C -->|"шаг 3"| A

    style A fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style B fill:#fef9e7,stroke:#f39c12
    style C fill:#fef9e7,stroke:#f39c12
```

> **Трассировка обнаружения циклов:**
>
> | Шаг | Переход | Множество visited | Результат |
> |------|--------|-------------|--------|
> | 1 | Начинаем с A | { A } | A — Ref → переходим |
> | 2 | A → B | { A, B } | B — Ref → переходим |
> | 3 | B → C | { A, B, C } | C — Ref → переходим |
> | 4 | C → A | A уже в visited! | **Error::CyclicRef** |
>
> Без обнаружения циклов это зациклилось бы навсегда. `MAX_REFERENCE_HOPS = 10` также ограничивает глубину обхода для длинных цепочек.

## Ссылки в Merk — Комбинированные хеши значений

Когда Reference хранится в дереве Merk, его `value_hash` должен аутентифицировать и структуру ссылки, и целевые данные:

```rust
// merk/src/tree/kv.rs
pub fn update_hashes_using_reference_value_hash(
    mut self,
    reference_value_hash: CryptoHash,
) -> CostContext<Self> {
    // Hash the reference element's own bytes
    let actual_value_hash = value_hash(self.value_as_slice());

    // Combine: H(reference_bytes) ⊕ H(referenced_data)
    let combined = combine_hash(&actual_value_hash, &reference_value_hash);

    self.value_hash = combined;
    self.hash = kv_digest_to_kv_hash(self.key(), self.value_hash());
    // ...
}
```

Это означает, что изменение либо самой ссылки, ЛИБО данных, на которые она указывает, изменит корневой хеш — оба криптографически связаны.

---
