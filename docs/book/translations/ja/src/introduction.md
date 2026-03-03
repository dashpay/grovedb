# はじめに — GroveDB とは？

## 基本的な考え方

GroveDB は**階層型認証データ構造**（hierarchical authenticated data structure）です。マークル AVL 木（Merkle AVL tree）上に構築された*グローブ*（木の森）であり、データベース内の各ノードは暗号学的に認証されたツリーの一部です。そして各ツリーは子として他のツリーを含むことができ、検証可能な状態の深い階層を形成します。

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

> 各色付きボックスは**個別の Merk ツリー**です。破線の矢印はサブツリー関係を示します — 親の Tree エレメントが子 Merk のルートキーを保持しています。

従来のデータベースでは、フラットなキーバリューストアの上に単一のマークル木を置いて認証を行うことが一般的です。GroveDB は異なるアプローチを採ります：マークル木の中にマークル木をネストします。これにより以下が実現されます：

1. **効率的なセカンダリインデックス** — 主キーだけでなく、任意のパスでクエリ可能
2. **コンパクトな暗号証明** — 任意のデータの存在（または不在）を証明
3. **集約データ** — ツリーが子の合計、カウント、その他の集約を自動的に維持
4. **アトミックなクロスツリー操作** — バッチ操作が複数のサブツリーにまたがる

## GroveDB が存在する理由

GroveDB は **Dash Platform** 向けに設計されました。Dash Platform は分散型アプリケーションプラットフォームであり、すべての状態が以下の要件を満たす必要があります：

- **認証済み**: 任意のノードがライトクライアントに対して任意の状態を証明可能
- **決定論的**: すべてのノードがまったく同じステートルートを計算
- **効率的**: 操作がブロック時間の制約内で完了
- **クエリ可能**: アプリケーションがキー検索だけでなくリッチなクエリを必要とする

従来のアプローチには限界があります：

| アプローチ | 問題点 |
|----------|---------|
| 通常のマークル木 | キー検索のみ対応、範囲クエリ不可 |
| Ethereum MPT | リバランスが高コスト、証明サイズが大きい |
| フラットKV + 単一ツリー | 階層クエリ不可、単一の証明がすべてをカバー |
| B木 | 本質的にマークル化されておらず、認証が複雑 |

GroveDB は **AVL 木の実証済みのバランス保証**と**階層的なネスト**、そして**豊富なエレメント型システム**を組み合わせることでこれらの問題を解決します。

## アーキテクチャ概要

GroveDB は明確な責務を持つ個別のレイヤーで構成されています：

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

データは書き込み時にこれらのレイヤーを**下方向**に、読み取り時に**上方向**に流れます。すべての操作はスタックを通過する際にコストを蓄積し、正確なリソース計算を可能にします。

---
