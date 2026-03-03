# 介绍 — 什么是 GroveDB？

## 核心理念

GroveDB 是一种**层级化的认证数据结构（hierarchical authenticated data structure）** — 本质上是一个基于 Merkle AVL tree（默克尔 AVL 树）构建的 *grove*（树丛，即树的树）。数据库中的每个节点都属于一棵密码学认证的树，而每棵树又可以包含其他树作为子树，从而形成一个深层的可验证状态层次结构。

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

> 每个彩色方框是一棵**独立的 Merk 树**。虚线箭头表示子树关系 — 父树中的 Tree 元素包含子 Merk 的根键。

在传统数据库中，你可能会将数据存储在一个扁平的键值存储中，并在顶部放置一棵默克尔树用于认证。GroveDB 采用了不同的方式：它将默克尔树嵌套在默克尔树中。这带来了以下优势：

1. **高效的二级索引** — 可以通过任意路径查询，而不仅仅是主键
2. **紧凑的密码学证明** — 证明任何数据的存在（或不存在）
3. **聚合数据** — 树可以自动对其子节点求和、计数或执行其他聚合操作
4. **跨树的原子操作** — 批量操作可以跨越多个子树

## 为什么创建 GroveDB

GroveDB 是为 **Dash Platform** 设计的，这是一个去中心化应用平台，其中每一条状态都必须满足以下要求：

- **可认证**：任何节点都能向轻客户端证明任何一条状态
- **确定性**：每个节点计算出完全相同的状态根
- **高效**：操作必须在出块时间约束内完成
- **可查询**：应用程序需要丰富的查询能力，而不仅仅是键查找

传统方案的不足：

| 方案 | 问题 |
|----------|---------|
| 普通默克尔树 | 只支持键查找，不支持范围查询 |
| 以太坊 MPT | 重平衡代价高昂，证明体积大 |
| 扁平键值存储 + 单棵树 | 不支持层级查询，单个证明覆盖所有内容 |
| B 树 | 天然不具备默克尔化特性，认证实现复杂 |

GroveDB 通过将 **AVL 树经过验证的平衡保证** 与 **层级嵌套** 和 **丰富的元素类型系统** 相结合来解决这些问题。

## 架构概览

GroveDB 被组织为多个层次分明的层，每层有明确的职责：

```mermaid
graph TD
    APP["<b>应用层</b><br/>Dash Platform 等<br/><code>insert() get() query() prove() apply_batch()</code>"]

    GROVE["<b>GroveDB 核心</b> — <code>grovedb/src/</code><br/>层级子树管理 · 元素类型系统<br/>引用解析 · 批量操作 · 多层证明"]

    MERK["<b>Merk 层</b> — <code>merk/src/</code><br/>默克尔 AVL 树 · 自平衡旋转<br/>链接系统 · Blake3 哈希 · 证明编码"]

    STORAGE["<b>存储层</b> — <code>storage/src/</code><br/>RocksDB OptimisticTransactionDB<br/>4 个列族 · Blake3 前缀隔离 · 批量写入"]

    COST["<b>开销层</b> — <code>costs/src/</code><br/>OperationCost 追踪 · CostContext 单子<br/>最坏情况与平均情况估算"]

    APP ==>|"写入 ↓"| GROVE
    GROVE ==>|"树操作"| MERK
    MERK ==>|"磁盘 I/O"| STORAGE
    STORAGE -.->|"开销累积 ↑"| COST
    COST -.-> MERK
    MERK -.-> GROVE
    GROVE ==>|"读取 ↑"| APP

    style APP fill:#f5f5f5,stroke:#999,stroke-width:1px
    style GROVE fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style MERK fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style STORAGE fill:#fdebd0,stroke:#e67e22,stroke-width:2px
    style COST fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

数据在写入时**向下**流经这些层，在读取时**向上**流动。每个操作在遍历堆栈时都会累积开销，从而实现精确的资源计量。

---
