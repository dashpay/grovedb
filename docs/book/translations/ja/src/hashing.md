# ハッシング — 暗号学的完全性

Merk ツリーの各ノードはハッシュされて**ルートハッシュ**を生成します — ツリー全体を認証する32バイトの値です。任意のキー、値、または構造的関係の変更により、異なるルートハッシュが生成されます。

## 3レベルのハッシュ階層

Merk は3段階のハッシュ方式を使用します。内側から外側へ：

例：key = `"bob"`（3バイト）、value = `"hello"`（5バイト）：

```mermaid
graph LR
    subgraph level1["Level 1: value_hash"]
        V_IN["varint(5) ‖ &quot;hello&quot;"]
        V_BLAKE["Blake3"]
        V_OUT(["value_hash<br/><small>32 bytes</small>"])
        V_IN --> V_BLAKE --> V_OUT
    end

    subgraph level2["Level 2: kv_hash"]
        K_IN["varint(3) ‖ &quot;bob&quot; ‖ value_hash"]
        K_BLAKE["Blake3"]
        K_OUT(["kv_hash<br/><small>32 bytes</small>"])
        K_IN --> K_BLAKE --> K_OUT
    end

    subgraph level3["Level 3: node_hash"]
        N_LEFT(["left_child_hash<br/><small>32B (or NULL_HASH)</small>"])
        N_KV(["kv_hash<br/><small>32B</small>"])
        N_RIGHT(["right_child_hash<br/><small>32B (or NULL_HASH)</small>"])
        N_BLAKE["Blake3<br/><small>96B input = 2 blocks</small>"]
        N_OUT(["node_hash<br/><small>32 bytes</small>"])
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

> ツリーの ROOT = ルートノードの `node_hash` — **すべての**キー、値、構造的関係を認証します。子がない場合は `NULL_HASH = [0x00; 32]` を使用します。

### レベル 1：value_hash

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

値の長さは **varint エンコード**されて先頭に付加されます。これは衝突耐性にとって重要です — これがないと `H("AB" ‖ "C")` は `H("A" ‖ "BC")` と等しくなってしまいます。

### レベル 2：kv_hash

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

これはキーを値にバインドします。証明検証用に、事前計算された value_hash を受け取る変種もあります：

```rust
pub fn kv_digest_to_kv_hash(key: &[u8], value_hash: &CryptoHash) -> CostContext<CryptoHash>
```

これは検証者が既に value_hash を持っている場合（例：value_hash が結合ハッシュであるサブツリー）に使用されます。

### レベル 3：node_hash

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

子がない場合、そのハッシュは **NULL_HASH** — 32バイトのゼロです：

```rust
pub const NULL_HASH: CryptoHash = [0; HASH_LENGTH];  // [0u8; 32]
```

## ハッシュ関数としての Blake3

GroveDB はすべてのハッシュに **Blake3** を使用します。主な特性：

- **256ビット出力**（32バイト）
- **ブロックサイズ**：64バイト
- **速度**：最新のハードウェアで SHA-256 の約3倍
- **ストリーミング**：データを段階的に供給可能

ハッシュ操作のコストは、処理される64バイトブロックの数に基づいて計算されます：

```rust
let hashes = 1 + (hasher.count() - 1) / 64;  // Number of hash operations
```

## 衝突耐性のための長さプレフィックスエンコーディング

すべての可変長入力は **varint エンコーディング**を使用してその長さがプレフィックスとして付加されます：

```mermaid
graph LR
    subgraph bad["Without length prefix — VULNERABLE"]
        BAD1["H(&quot;AB&quot; + &quot;C&quot;) = H(0x41 0x42 0x43)"]
        BAD2["H(&quot;A&quot; + &quot;BC&quot;) = H(0x41 0x42 0x43)"]
        BAD1 --- SAME["SAME HASH!"]
        BAD2 --- SAME
    end

    subgraph good["With length prefix — collision resistant"]
        GOOD1["H([02] 0x41 0x42)<br/><small>varint(2) ‖ &quot;AB&quot;</small>"]
        GOOD2["H([03] 0x41 0x42 0x43)<br/><small>varint(3) ‖ &quot;ABC&quot;</small>"]
        GOOD1 --- DIFF["DIFFERENT"]
        GOOD2 --- DIFF
    end

    style bad fill:#fadbd8,stroke:#e74c3c
    style good fill:#d5f5e3,stroke:#27ae60
    style SAME fill:#e74c3c,color:#fff,stroke:#c0392b
    style DIFF fill:#27ae60,color:#fff,stroke:#229954
```

> **value_hash の入力**: `[varint(value.len)] [value bytes]`
> **kv_hash の入力**: `[varint(key.len)] [key bytes] [value_hash: 32 bytes]`

長さプレフィックスがなければ、攻撃者は同じダイジェストにハッシュされる異なるキーバリューペアを作成できてしまいます。長さプレフィックスにより、これは暗号学的に不可能になります。

## 特殊エレメントの結合ハッシュ

**サブツリー**と**参照**では、`value_hash` は単なる `H(value)` ではありません。代わりに、エレメントをその対象にバインドする**結合ハッシュ**（combined hash）です：

```mermaid
graph LR
    subgraph item["Regular Item"]
        I_val["value bytes"] --> I_hash["H(len ‖ bytes)"] --> I_vh(["value_hash"])
    end

    subgraph subtree["Subtree Element"]
        S_elem["tree element bytes"] --> S_hash1["H(len ‖ bytes)"]
        S_root(["child Merk<br/>root hash"])
        S_hash1 --> S_combine["combine_hash()<br/><small>Blake3(a ‖ b)</small>"]
        S_root --> S_combine
        S_combine --> S_vh(["value_hash"])
    end

    subgraph reference["Reference Element"]
        R_elem["ref element bytes"] --> R_hash1["H(len ‖ bytes)"]
        R_target["target value"] --> R_hash2["H(len ‖ bytes)"]
        R_hash1 --> R_combine["combine_hash()"]
        R_hash2 --> R_combine
        R_combine --> R_vh(["value_hash"])
    end

    style item fill:#eaf2f8,stroke:#2980b9
    style subtree fill:#fef9e7,stroke:#f39c12
    style reference fill:#fdedec,stroke:#e74c3c
```

> **サブツリー:** 子 Merk のルートハッシュを親にバインドします。**参照:** 参照パスとターゲット値の両方をバインドします。どちらを変更してもルートハッシュが変わります。

`combine_hash` 関数：

```rust
pub fn combine_hash(hash_one: &CryptoHash, hash_two: &CryptoHash) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(hash_one);   // 32 bytes
    hasher.update(hash_two);   // 32 bytes — total 64 bytes, exactly 1 hash op
    // ...
}
```

これにより GroveDB は単一のルートハッシュを通じて階層全体を認証できます — サブツリーエレメントに対する各親ツリーの value_hash が子ツリーのルートハッシュを含んでいるからです。

## ProvableCountTree の集約ハッシュ

`ProvableCountTree` ノードはノードハッシュに集約カウントを含みます：

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

これは、カウントの証明に実際のデータを公開する必要がないことを意味します — カウントは暗号学的コミットメントに組み込まれています。

---
