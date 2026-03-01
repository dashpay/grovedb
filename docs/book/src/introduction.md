# Introduction — What is GroveDB?

## The Core Idea

GroveDB is a **hierarchical authenticated data structure** — essentially a *grove*
(tree of trees) built on Merkle AVL trees. Each node in the database is part of a
cryptographically authenticated tree, and each tree can contain other trees as
children, forming a deep hierarchy of verifiable state.

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

> Each colored box is a **separate Merk tree**. Dashed arrows show the subtree relationship — a Tree element in the parent contains the root key of the child Merk.

In a traditional database, you might store data in a flat key-value store with a
single Merkle tree on top for authentication. GroveDB takes a different approach:
it nests Merkle trees inside Merkle trees. This gives you:

1. **Efficient secondary indexes** — query by any path, not just primary key
2. **Compact cryptographic proofs** — prove the existence (or absence) of any data
3. **Aggregate data** — trees can automatically sum, count, or otherwise aggregate
   their children
4. **Atomic cross-tree operations** — batch operations span multiple subtrees

## Why GroveDB Exists

GroveDB was designed for **Dash Platform**, a decentralized application platform
where every piece of state must be:

- **Authenticated**: Any node can prove any piece of state to a light client
- **Deterministic**: Every node computes exactly the same state root
- **Efficient**: Operations must complete within block time constraints
- **Queryable**: Applications need rich queries, not just key lookups

Traditional approaches fall short:

| Approach | Problem |
|----------|---------|
| Plain Merkle Tree | Only supports key lookups, no range queries |
| Ethereum MPT | Expensive rebalancing, large proof sizes |
| Flat key-value + single tree | No hierarchical queries, single proof covers everything |
| B-tree | Not naturally Merklized, complex authentication |

GroveDB solves these by combining the **proven balance guarantees of AVL trees**
with **hierarchical nesting** and a **rich element type system**.

## Architecture Overview

GroveDB is organized into distinct layers, each with a clear responsibility:

```mermaid
graph TD
    APP["<b>Application Layer</b><br/>Dash Platform, etc.<br/><code>insert() get() query() prove() apply_batch()</code>"]

    GROVE["<b>GroveDB Core</b> — <code>grovedb/src/</code><br/>Hierarchical subtree management · Element type system<br/>Reference resolution · Batch ops · Multi-layer proofs"]

    MERK["<b>Merk Layer</b> — <code>merk/src/</code><br/>Merkle AVL tree · Self-balancing rotations<br/>Link system · Blake3 hashing · Proof encoding"]

    STORAGE["<b>Storage Layer</b> — <code>storage/src/</code><br/>RocksDB OptimisticTransactionDB<br/>4 column families · Blake3 prefix isolation · Batched writes"]

    COST["<b>Cost Layer</b> — <code>costs/src/</code><br/>OperationCost tracking · CostContext monad<br/>Worst-case &amp; average-case estimation"]

    APP ==>|"writes ↓"| GROVE
    GROVE ==>|"tree ops"| MERK
    MERK ==>|"disk I/O"| STORAGE
    STORAGE -.->|"cost accumulation ↑"| COST
    COST -.-> MERK
    MERK -.-> GROVE
    GROVE ==>|"reads ↑"| APP

    style APP fill:#f5f5f5,stroke:#999,stroke-width:1px
    style GROVE fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style MERK fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style STORAGE fill:#fdebd0,stroke:#e67e22,stroke-width:2px
    style COST fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

Data flows **down** through these layers during writes and **up** during reads.
Every operation accumulates costs as it traverses the stack, enabling precise
resource accounting.

---
