# โครงสร้าง Grove แบบลำดับชั้น — ต้นไม้ของต้นไม้

## Subtree ซ้อนกันภายในต้นไม้แม่อย่างไร

คุณลักษณะที่โดดเด่นของ GroveDB คือ Merk tree สามารถมี element ที่ตัวเองก็เป็น Merk tree ได้ สิ่งนี้สร้าง **namespace แบบลำดับชั้น (hierarchical namespace)**:

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

> กล่องสีแต่ละกล่องคือ Merk tree แยกกัน ลูกศรเส้นประแสดงลิงก์ portal (ประตูทางเข้า) จาก element ชนิด Tree ไปยัง Merk tree ลูก เส้นทาง (path) ไปยังแต่ละ Merk แสดงอยู่ในป้ายชื่อ

## ระบบการระบุที่อยู่ด้วย Path

ทุก element ใน GroveDB ถูกระบุที่อยู่ด้วย **path (เส้นทาง)** — ลำดับของสตริงไบต์ที่นำทางจากรากผ่าน subtree ไปยัง key เป้าหมาย:

```text
    Path: ["identities", "alice123", "name"]

    ขั้นตอน 1: ในต้นไม้ราก ค้นหา "identities" → element ชนิด Tree
    ขั้นตอน 2: เปิด subtree identities ค้นหา "alice123" → element ชนิด Tree
    ขั้นตอน 3: เปิด subtree alice123 ค้นหา "name" → Item("Alice")
```

Path ถูกแสดงเป็น `Vec<Vec<u8>>` หรือใช้ type `SubtreePath` เพื่อการจัดการที่มีประสิทธิภาพโดยไม่ต้องจัดสรรหน่วยความจำ:

```rust
// เส้นทางไปยัง element (ทุกส่วนยกเว้นส่วนสุดท้าย)
let path: &[&[u8]] = &[b"identities", b"alice123"];
// key ภายใน subtree สุดท้าย
let key: &[u8] = b"name";
```

## การสร้าง Prefix ด้วย Blake3 สำหรับการแยก Storage

แต่ละ subtree ใน GroveDB ได้ **namespace ที่เก็บข้อมูลที่แยกจากกัน** ใน RocksDB namespace ถูกกำหนดโดยการแฮช path ด้วย Blake3:

```rust
pub type SubtreePrefix = [u8; 32];

// prefix ถูกคำนวณโดยการแฮชส่วนของ path
// storage/src/rocksdb_storage/storage.rs
```

ตัวอย่าง:

```text
    Path: ["identities", "alice123"]
    Prefix: Blake3(["identities", "alice123"]) = [0xab, 0x3f, ...]  (32 ไบต์)

    ใน RocksDB key สำหรับ subtree นี้ถูกจัดเก็บเป็น:
    [prefix: 32 bytes][original_key]

    ดังนั้น "name" ใน subtree นี้จะกลายเป็น:
    [0xab, 0x3f, ...][0x6e, 0x61, 0x6d, 0x65]  ("name")
```

สิ่งนี้รับประกัน:
- ไม่มีการชนกันของ key ระหว่าง subtree (prefix 32 ไบต์ = การแยก 256 บิต)
- การคำนวณ prefix ที่มีประสิทธิภาพ (แฮช Blake3 เดียวบนไบต์ path)
- ข้อมูล subtree อยู่ใกล้กันใน RocksDB เพื่อประสิทธิภาพของ cache

## การเผยแพร่ Root Hash ผ่านลำดับชั้น

เมื่อค่าเปลี่ยนแปลงลึกใน grove การเปลี่ยนแปลงต้อง **เผยแพร่ขึ้น (propagate upward)** เพื่ออัปเดต root hash:

```text
    การเปลี่ยนแปลง: อัปเดต "name" เป็น "ALICE" ใน identities/alice123/

    ขั้นตอน 1: อัปเดตค่าใน Merk tree ของ alice123
            → ต้นไม้ alice123 ได้ root hash ใหม่: H_alice_new

    ขั้นตอน 2: อัปเดต element "alice123" ในต้นไม้ identities
            → value_hash ของต้นไม้ identities สำหรับ "alice123" =
              combine_hash(H(tree_element_bytes), H_alice_new)
            → ต้นไม้ identities ได้ root hash ใหม่: H_ident_new

    ขั้นตอน 3: อัปเดต element "identities" ในต้นไม้ราก
            → value_hash ของต้นไม้รากสำหรับ "identities" =
              combine_hash(H(tree_element_bytes), H_ident_new)
            → ROOT HASH เปลี่ยน
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

**ก่อน เทียบกับ หลัง** — โหนดที่เปลี่ยนถูกทำเครื่องหมายด้วยสีแดง:

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

> เฉพาะโหนดบนเส้นทางจากค่าที่เปลี่ยนขึ้นไปถึงรากเท่านั้นที่ถูกคำนวณใหม่ โหนดพี่น้อง (sibling) และสาขาอื่นยังคงไม่เปลี่ยนแปลง

การเผยแพร่ถูก implement โดย `propagate_changes_with_transaction` ซึ่งเดินขึ้นจาก path ของ subtree ที่ถูกแก้ไขไปยัง root โดยอัปเดตแฮชของ element แม่ตลอดเส้นทาง

## ตัวอย่างโครงสร้าง Grove หลายระดับ

ต่อไปนี้คือตัวอย่างสมบูรณ์ที่แสดงวิธีที่ Dash Platform จัดโครงสร้าง state ของมัน:

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

แต่ละกล่องคือ Merk tree แยกกัน ที่ได้รับการรับรองตลอดทางขึ้นไปถึง root hash เดียวที่ validator ทั้งหมดเห็นพ้องกัน

---
