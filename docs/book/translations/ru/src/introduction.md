# Введение — Что такое GroveDB?

## Основная идея

GroveDB — это **иерархическая аутентифицированная структура данных** — по сути, *роща* (дерево деревьев), построенная на АВЛ-деревьях Меркла. Каждый узел в базе данных является частью криптографически аутентифицированного дерева, и каждое дерево может содержать другие деревья в качестве дочерних элементов, формируя глубокую иерархию верифицируемого состояния.

```mermaid
graph TD
    subgraph root["Root Merk Tree"]
        R_contracts["&quot;contracts&quot;<br/><i>Tree</i>"]
        R_identities["&quot;identities&quot;<br/><i>Tree</i>"]
        R_balances["&quot;balances&quot;<br/><i>SumTree</i>"]
        R_contracts --- R_identities
        R_contracts --- R_balances
    end

    subgraph ident["Identities Merk"]
        I_bob["&quot;bob&quot;<br/><i>Tree</i>"]
        I_alice["&quot;alice&quot;<br/><i>Tree</i>"]
        I_carol["&quot;carol&quot;<br/><i>Item</i>"]
        I_bob --- I_alice
        I_bob --- I_carol
    end

    subgraph contracts["Contracts Merk"]
        C_c2["&quot;C2&quot;<br/><i>Item</i>"]
        C_c1["&quot;C1&quot;<br/><i>Item</i>"]
        C_c3["&quot;C3&quot;<br/><i>Item</i>"]
        C_c2 --- C_c1
        C_c2 --- C_c3
    end

    subgraph balances["Balances SumTree — sum=5300"]
        B_bob["&quot;bob&quot;<br/>SumItem(2500)"]
        B_al["&quot;alice&quot;<br/>SumItem(2000)"]
        B_eve["&quot;eve&quot;<br/>SumItem(800)"]
        B_bob --- B_al
        B_bob --- B_eve
    end

    subgraph alice_merk["Alice Merk"]
        A_name["&quot;name&quot; → Alice"]
        A_bal["&quot;balance&quot; → 1000"]
    end

    subgraph bob_merk["Bob Merk"]
        Bo_name["&quot;name&quot; → Bob"]
    end

    R_identities -.->|subtree| ident
    R_contracts -.->|subtree| contracts
    R_balances -.->|subtree| balances
    I_alice -.->|subtree| alice_merk
    I_bob -.->|subtree| bob_merk

    style root fill:#e8f4fd,stroke:#2980b9,stroke-width:2px
    style ident fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style contracts fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style balances fill:#fdedec,stroke:#e74c3c,stroke-width:2px
    style alice_merk fill:#eafaf1,stroke:#27ae60,stroke-width:2px
    style bob_merk fill:#eafaf1,stroke:#27ae60,stroke-width:2px
```

> Каждый цветной блок — это **отдельное дерево Merk**. Пунктирные стрелки показывают связь поддеревьев — элемент Tree в родительском дереве содержит корневой ключ дочернего дерева Merk.

В традиционной базе данных данные могут храниться в плоском хранилище «ключ-значение» с единственным деревом Меркла поверх него для аутентификации. GroveDB использует другой подход: дерево Меркла вложено в другие деревья Меркла. Это обеспечивает:

1. **Эффективные вторичные индексы** — запросы по любому пути, а не только по первичному ключу
2. **Компактные криптографические доказательства** — подтверждение существования (или отсутствия) любых данных
3. **Агрегированные данные** — деревья могут автоматически суммировать, подсчитывать или иным образом агрегировать дочерние элементы
4. **Атомарные операции между деревьями** — пакетные операции охватывают несколько поддеревьев

## Зачем нужна GroveDB

GroveDB была спроектирована для **Dash Platform** — децентрализованной платформы приложений, где каждый элемент состояния должен быть:

- **Аутентифицированным**: любой узел может доказать любой элемент состояния лёгкому клиенту
- **Детерминированным**: каждый узел вычисляет ровно тот же корневой хеш состояния
- **Эффективным**: операции должны укладываться в ограничения времени блока
- **Запрашиваемым**: приложениям нужны сложные запросы, а не только поиск по ключу

Традиционные подходы не справляются:

| Подход | Проблема |
|--------|----------|
| Простое дерево Меркла | Поддерживает только поиск по ключу, без диапазонных запросов |
| Ethereum MPT | Дорогая перебалансировка, большой размер доказательств |
| Плоское хранилище «ключ-значение» + одно дерево | Нет иерархических запросов, одно доказательство покрывает всё |
| B-дерево | Не является естественно меркелизированным, сложная аутентификация |

GroveDB решает эти проблемы, комбинируя **проверенные гарантии балансировки АВЛ-деревьев** с **иерархическим вложением** и **развитой системой типов элементов**.

## Обзор архитектуры

GroveDB организована в отдельные уровни, каждый с чёткой зоной ответственности:

```mermaid
graph TD
    APP["<b>Уровень приложения</b><br/>Dash Platform и др.<br/><code>insert() get() query() prove() apply_batch()</code>"]

    GROVE["<b>Ядро GroveDB</b> — <code>grovedb/src/</code><br/>Иерархическое управление поддеревьями · Система типов элементов<br/>Разрешение ссылок · Пакетные операции · Многоуровневые доказательства"]

    MERK["<b>Уровень Merk</b> — <code>merk/src/</code><br/>АВЛ-дерево Меркла · Самобалансирующиеся повороты<br/>Система связей · Хеширование Blake3 · Кодирование доказательств"]

    STORAGE["<b>Уровень хранения</b> — <code>storage/src/</code><br/>RocksDB OptimisticTransactionDB<br/>4 семейства столбцов · Изоляция по префиксам Blake3 · Пакетная запись"]

    COST["<b>Уровень затрат</b> — <code>costs/src/</code><br/>Отслеживание OperationCost · Монада CostContext<br/>Оценка наихудшего и среднего случаев"]

    APP ==>|"запись ↓"| GROVE
    GROVE ==>|"операции с деревом"| MERK
    MERK ==>|"дисковый ввод-вывод"| STORAGE
    STORAGE -.->|"накопление затрат ↑"| COST
    COST -.-> MERK
    MERK -.-> GROVE
    GROVE ==>|"чтение ↑"| APP

    style APP fill:#f5f5f5,stroke:#999,stroke-width:1px
    style GROVE fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style MERK fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style STORAGE fill:#fdebd0,stroke:#e67e22,stroke-width:2px
    style COST fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

Данные проходят **вниз** через эти уровни при записи и **вверх** при чтении. Каждая операция накапливает затраты по мере прохождения стека, обеспечивая точный учёт ресурсов.

---
