# Система связей — Архитектура ленивой загрузки

Загрузка всего дерева Merk в память была бы чрезмерно затратной для больших деревьев. Система связей (Link system) решает эту проблему, представляя дочерние соединения в четырёх возможных состояниях, что обеспечивает **ленивую загрузку** (lazy loading) — потомки извлекаются из хранилища только при реальной необходимости.

## Четыре состояния связи

```rust
// merk/src/tree/link.rs
pub enum Link {
    Reference {                    // Pruned: only metadata, no tree in memory
        hash: CryptoHash,
        child_heights: (u8, u8),
        key: Vec<u8>,
        aggregate_data: AggregateData,
    },
    Modified {                     // Recently changed, hash not yet computed
        pending_writes: usize,
        child_heights: (u8, u8),
        tree: TreeNode,
    },
    Uncommitted {                  // Hashed but not yet persisted to storage
        hash: CryptoHash,
        child_heights: (u8, u8),
        tree: TreeNode,
        aggregate_data: AggregateData,
    },
    Loaded {                       // Fully loaded from storage
        hash: CryptoHash,
        child_heights: (u8, u8),
        tree: TreeNode,
        aggregate_data: AggregateData,
    },
}
```

## Диаграмма переходов состояний

```mermaid
stateDiagram-v2
    [*] --> Reference : decode from storage<br/>(hash + key + child_heights)

    Reference --> Loaded : fetch()<br/>load from RocksDB

    Loaded --> Modified : put / delete<br/>any mutation invalidates hash

    Modified --> Uncommitted : commit()<br/>recompute hashes bottom-up

    Uncommitted --> Loaded : write to disk<br/>persist to RocksDB

    Loaded --> Reference : into_reference()<br/>prune to free memory

    state Reference {
        [*] : hash ✓ · tree ✗ · key ✓
    }
    state Loaded {
        [*] : hash ✓ · tree ✓
    }
    state Modified {
        [*] : hash ✗ INVALID · tree ✓ dirty<br/>pending_writes: n
    }
    state Uncommitted {
        [*] : hash ✓ fresh · tree ✓ clean
    }
```

## Что хранит каждое состояние

| Состояние | Хеш? | Дерево в памяти? | Назначение |
|-----------|-------|------------------|------------|
| **Reference** | Да | Нет | Компактное представление на диске. Хранит только ключ, хеш, высоты потомков и агрегированные данные. |
| **Modified** | Нет | Да | После любой мутации. Отслеживает счётчик `pending_writes` для оптимизации пакетной обработки. |
| **Uncommitted** | Да | Да | После вычисления хеша, но до записи в хранилище. Промежуточное состояние при фиксации. |
| **Loaded** | Да | Да | Полностью материализовано. Готово к чтению или дальнейшей модификации. |

Поле `pending_writes` в `Modified` заслуживает внимания:

```rust
// Computed as: 1 + left_pending_writes + right_pending_writes
pending_writes: 1 + tree.child_pending_writes(true)
                  + tree.child_pending_writes(false),
```

Этот счётчик помогает фазе фиксации определить порядок записей для оптимальной производительности.

## Паттерн обратного вызова Fetch

Система связей использует **трейт Fetch** для абстрагирования способа загрузки дочерних узлов:

```rust
pub trait Fetch {
    fn fetch(
        &self,
        link: &Link,
        value_defined_cost_fn: Option<&impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
        grove_version: &GroveVersion,
    ) -> CostResult<TreeNode, Error>;
}
```

Различные реализации Fetch служат разным целям:

- **StorageFetch**: загрузка из RocksDB (обычный путь)
- **PanicSource**: используется в тестах, где загрузка не должна происходить
- **MockSource**: возвращает контролируемые тестовые данные

Этот паттерн позволяет операциям с деревом быть **независимыми от хранилища** — одна и та же логика балансировки и мутации работает вне зависимости от источника данных.

## Паттерн Walker

`Walker` оборачивает `TreeNode` с источником `Fetch`, обеспечивая безопасный обход дерева с автоматической ленивой загрузкой (`merk/src/tree/walk/mod.rs`):

```rust
pub struct Walker<S: Fetch + Sized + Clone> {
    tree: Owner<TreeNode>,
    source: S,
}
```

Walker предоставляет три ключевые операции:

**walk()** — Отсоединить потомка, преобразовать его и присоединить обратно:

```rust
pub fn walk<F, T>(self, left: bool, f: F, ...) -> CostResult<Self, Error>
where
    F: FnOnce(Option<Self>) -> CostResult<Option<T>, Error>,
    T: Into<TreeNode>,
```

**detach()** — Удалить потомка, загружая его из хранилища при необходимости:

```rust
pub fn detach(self, left: bool, ...) -> CostResult<(Self, Option<Self>), Error>
```

Если потомок находится в состоянии `Link::Reference` (обрезан), detach вызовет `fetch()` для его загрузки. Если потомок уже в памяти (`Modified`, `Uncommitted`, `Loaded`), он просто извлекается.

**attach()** — Присоединить потомка к родителю:

```rust
pub fn attach(self, left: bool, maybe_child: Option<Self>) -> Self
```

Присоединение всегда создаёт `Link::Modified`, поскольку связь родитель-потомок изменилась.

## Экономия памяти через обрезку

После фиксации изменений дерево может **обрезать** загруженные поддеревья обратно до `Link::Reference`, высвобождая память, но сохраняя хеш, необходимый для генерации доказательств:

**До обрезки** — все 7 узлов в памяти:

```mermaid
graph TD
    D["D<br/><small>Loaded</small>"]
    B["B<br/><small>Loaded</small>"]
    F["F<br/><small>Loaded</small>"]
    A["A<br/><small>Loaded</small>"]
    C["C<br/><small>Loaded</small>"]
    E["E<br/><small>Loaded</small>"]
    G["G<br/><small>Loaded</small>"]
    D --- B & F
    B --- A & C
    F --- E & G
    style D fill:#d4e6f1,stroke:#2980b9
    style B fill:#d4e6f1,stroke:#2980b9
    style F fill:#d4e6f1,stroke:#2980b9
    style A fill:#d4e6f1,stroke:#2980b9
    style C fill:#d4e6f1,stroke:#2980b9
    style E fill:#d4e6f1,stroke:#2980b9
    style G fill:#d4e6f1,stroke:#2980b9
```

**После обрезки** — только корень в памяти, потомки в состоянии `Link::Reference` (только хеш + ключ):

```mermaid
graph TD
    D2["D<br/><small>Loaded (в памяти)</small>"]
    B2["B<br/><small>Reference<br/>только хеш + ключ</small>"]
    F2["F<br/><small>Reference<br/>только хеш + ключ</small>"]
    D2 --- B2 & F2
    style D2 fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style B2 fill:#f5f5f5,stroke:#999,stroke-dasharray: 5 5
    style F2 fill:#f5f5f5,stroke:#999,stroke-dasharray: 5 5
```

> **Link::Loaded** содержит `hash + child_heights + tree (TreeNode)`. **Link::Reference** содержит только `hash + child_heights + key` — TreeNode освобождается из памяти.

Преобразование простое:

```rust
pub fn into_reference(self) -> Link {
    Link::Reference {
        hash: self.hash(),
        child_heights: self.child_heights(),
        key: self.key().to_vec(),
        aggregate_data: self.aggregate_data(),
    }
}
```

Это критически важно для ограничения потребления памяти в больших деревьях — только активно используемые узлы должны находиться в памяти.

---
