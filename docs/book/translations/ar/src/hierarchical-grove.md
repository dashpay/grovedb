# البستان الهرمي — شجرة من الأشجار

## كيف تتعشّش الأشجار الفرعية داخل الأشجار الأب

الميزة المُعرِّفة لـ GroveDB هي أن شجرة Merk يمكن أن تحتوي على عناصر هي
بذاتها أشجار Merk. هذا يُنشئ **فضاء اسم هرمي**:

```mermaid
graph TD
    subgraph root["ROOT MERK TREE — path: []"]
        contracts["&quot;contracts&quot;<br/>Tree"]
        identities["&quot;identities&quot;<br/>Tree"]
        balances["&quot;balances&quot;<br/>SumTree, sum=0"]
        contracts --> identities
        contracts --> balances
    end

    subgraph ident["IDENTITIES MERK — path: [&quot;identities&quot;]"]
        bob456["&quot;bob456&quot;<br/>Tree"]
        alice123["&quot;alice123&quot;<br/>Tree"]
        eve["&quot;eve&quot;<br/>Item"]
        bob456 --> alice123
        bob456 --> eve
    end

    subgraph bal["BALANCES MERK (SumTree) — path: [&quot;balances&quot;]"]
        bob_bal["&quot;bob456&quot;<br/>SumItem(800)"]
    end

    subgraph alice_tree["ALICE123 MERK — path: [&quot;identities&quot;, &quot;alice123&quot;]"]
        name["&quot;name&quot;<br/>Item(&quot;Al&quot;)"]
        balance_item["&quot;balance&quot;<br/>SumItem(1000)"]
        docs["&quot;docs&quot;<br/>Tree"]
        name --> balance_item
        name --> docs
    end

    identities -.-> bob456
    balances -.-> bob_bal
    alice123 -.-> name
    docs -.->|"... more subtrees"| more["..."]

    style root fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style ident fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style bal fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style alice_tree fill:#e8daef,stroke:#8e44ad,stroke-width:2px
    style more fill:none,stroke:none
```

> كل مربع ملون هو شجرة Merk منفصلة. الأسهم المتقطعة تمثل روابط البوابة من عناصر Tree إلى أشجار Merk الأبناء. المسار إلى كل Merk مُوضّح في تسميته.

## نظام عنونة المسارات

كل عنصر في GroveDB يُعنوَن بـ **مسار** (path) — سلسلة من سلاسل بايتات
تُوجّه من الجذر عبر الأشجار الفرعية إلى المفتاح الهدف:

```text
    Path: ["identities", "alice123", "name"]

    Step 1: In root tree, look up "identities" → Tree element
    Step 2: Open identities subtree, look up "alice123" → Tree element
    Step 3: Open alice123 subtree, look up "name" → Item("Alice")
```

تُمثَّل المسارات كـ `Vec<Vec<u8>>` أو باستخدام نوع `SubtreePath` للتعامل
الفعّال بدون تخصيص ذاكرة:

```rust
// The path to the element (all segments except the last)
let path: &[&[u8]] = &[b"identities", b"alice123"];
// The key within the final subtree
let key: &[u8] = b"name";
```

## توليد بادئة Blake3 لعزل التخزين

كل شجرة فرعية في GroveDB تحصل على **فضاء اسم تخزين معزول** خاص بها في RocksDB.
يتم تحديد فضاء الاسم بتجزئة المسار بـ Blake3:

```rust
pub type SubtreePrefix = [u8; 32];

// The prefix is computed by hashing the path segments
// storage/src/rocksdb_storage/storage.rs
```

على سبيل المثال:

```text
    Path: ["identities", "alice123"]
    Prefix: Blake3(["identities", "alice123"]) = [0xab, 0x3f, ...]  (32 bytes)

    In RocksDB, keys for this subtree are stored as:
    [prefix: 32 bytes][original_key]

    So "name" in this subtree becomes:
    [0xab, 0x3f, ...][0x6e, 0x61, 0x6d, 0x65]  ("name")
```

هذا يضمن:
- عدم تصادم المفاتيح بين الأشجار الفرعية (بادئة 32 بايت = عزل 256 بت)
- كفاءة حساب البادئة (تجزئة Blake3 واحدة على بايتات المسار)
- تجميع بيانات الشجرة الفرعية في RocksDB لكفاءة الذاكرة المؤقتة

## انتشار تجزئة الجذر عبر التسلسل الهرمي

عندما تتغير قيمة عميقة في البستان، يجب أن ينتشر التغيير **صعوداً** لتحديث
تجزئة الجذر:

```text
    Change: Update "name" to "ALICE" in identities/alice123/

    Step 1: Update value in alice123's Merk tree
            → alice123 tree gets new root hash: H_alice_new

    Step 2: Update "alice123" element in identities tree
            → identities tree's value_hash for "alice123" =
              combine_hash(H(tree_element_bytes), H_alice_new)
            → identities tree gets new root hash: H_ident_new

    Step 3: Update "identities" element in root tree
            → root tree's value_hash for "identities" =
              combine_hash(H(tree_element_bytes), H_ident_new)
            → ROOT HASH changes
```

```mermaid
graph TD
    subgraph step3["STEP 3: Update root tree"]
        R3["Root tree recalculates:<br/>value_hash for &quot;identities&quot; =<br/>combine_hash(H(tree_bytes), H_ident_NEW)<br/>→ new ROOT HASH"]
    end
    subgraph step2["STEP 2: Update identities tree"]
        R2["identities tree recalculates:<br/>value_hash for &quot;alice123&quot; =<br/>combine_hash(H(tree_bytes), H_alice_NEW)<br/>→ new root hash: H_ident_NEW"]
    end
    subgraph step1["STEP 1: Update alice123 Merk"]
        R1["alice123 tree recalculates:<br/>value_hash(&quot;ALICE&quot;) → new kv_hash<br/>→ new root hash: H_alice_NEW"]
    end

    R1 -->|"H_alice_NEW flows up"| R2
    R2 -->|"H_ident_NEW flows up"| R3

    style step1 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style step2 fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style step3 fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

**قبل مقابل بعد** — العقد المتغيرة مُعلَّمة بالأحمر:

```mermaid
graph TD
    subgraph before["BEFORE"]
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

    subgraph after["AFTER"]
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

> فقط العقد على المسار من القيمة المُتغيّرة صعوداً إلى الجذر يتم إعادة حسابها. الأشقاء والفروع الأخرى تبقى بدون تغيير.

الانتشار مُنفَّذ بواسطة `propagate_changes_with_transaction`، التي تمشي
صعوداً من الشجرة الفرعية المُعدَّلة إلى الجذر، مُحدِّثة تجزئة عنصر كل أب
على طول الطريق.

## مثال بنية بستان متعدد المستويات

إليك مثالاً كاملاً يُوضّح كيف تُهيكل Dash Platform حالتها:

```mermaid
graph TD
    ROOT["GroveDB Root"]

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

كل مربع هو شجرة Merk منفصلة، موثّقة بالكامل وصولاً إلى تجزئة جذر واحدة
يتفق عليها المُدقّقون.

---
