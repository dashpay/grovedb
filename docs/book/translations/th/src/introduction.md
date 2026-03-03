# บทนำ — GroveDB คืออะไร?

## แนวคิดหลัก

GroveDB คือ **โครงสร้างข้อมูลแบบลำดับชั้นที่มีการรับรองความถูกต้อง (hierarchical authenticated data structure)** — โดยพื้นฐานแล้วคือ *grove* (ต้นไม้ของต้นไม้) ที่สร้างบน Merkle AVL tree แต่ละโหนด (node) ในฐานข้อมูลเป็นส่วนหนึ่งของต้นไม้ที่ได้รับการรับรองทางการเข้ารหัส (cryptographically authenticated tree) และแต่ละต้นไม้สามารถมีต้นไม้อื่นเป็นลูกได้ ทำให้เกิดลำดับชั้นลึกของสถานะที่ตรวจสอบได้

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

> กล่องสีแต่ละกล่องคือ **Merk tree แยกกัน** ลูกศรเส้นประแสดงความสัมพันธ์ subtree (ต้นไม้ย่อย) — element ชนิด Tree ในต้นไม้แม่จะมี root key ของ Merk ลูก

ในฐานข้อมูลแบบดั้งเดิม คุณอาจจัดเก็บข้อมูลใน key-value store แบบแบนราบโดยมี Merkle tree เพียงต้นเดียวอยู่ด้านบนเพื่อการรับรองความถูกต้อง GroveDB ใช้แนวทางที่แตกต่าง: มันซ้อน Merkle tree ไว้ภายใน Merkle tree ซึ่งให้ประโยชน์ดังนี้:

1. **ดัชนีรอง (secondary index) ที่มีประสิทธิภาพ** — สืบค้นด้วยเส้นทาง (path) ใดก็ได้ ไม่จำกัดเฉพาะ primary key
2. **proof (หลักฐาน) ทางการเข้ารหัสที่กระชับ** — พิสูจน์การมีอยู่ (หรือไม่มีอยู่) ของข้อมูลใดก็ได้
3. **ข้อมูลแบบรวม (aggregate data)** — ต้นไม้สามารถรวมค่า นับ หรือคำนวณรวมลูก ๆ ได้โดยอัตโนมัติ
4. **การดำเนินการแบบ atomic ข้ามต้นไม้** — batch operation (การดำเนินการเป็นชุด) สามารถครอบคลุมหลาย subtree ได้

## ทำไม GroveDB จึงถูกสร้างขึ้น

GroveDB ถูกออกแบบมาสำหรับ **Dash Platform** ซึ่งเป็นแพลตฟอร์มแอปพลิเคชันแบบกระจายศูนย์ (decentralized application platform) ที่ทุกชิ้นส่วนของสถานะ (state) ต้อง:

- **รับรองได้ (Authenticated)**: โหนดใดก็ได้สามารถพิสูจน์สถานะใดก็ได้ให้ light client ตรวจสอบ
- **กำหนดผลลัพธ์ได้แน่นอน (Deterministic)**: ทุกโหนดคำนวณ state root เดียวกันเป๊ะ
- **มีประสิทธิภาพ (Efficient)**: การดำเนินการต้องเสร็จภายในเวลาจำกัดของบล็อก
- **สืบค้นได้ (Queryable)**: แอปพลิเคชันต้องการ query (การสืบค้น) ที่หลากหลาย ไม่ใช่แค่ค้นหาด้วย key เพียงอย่างเดียว

แนวทางแบบดั้งเดิมมีข้อจำกัด:

| แนวทาง | ปัญหา |
|----------|---------|
| Plain Merkle Tree | รองรับเฉพาะการค้นหาด้วย key ไม่มี range query |
| Ethereum MPT | การปรับสมดุลมีค่าใช้จ่ายสูง ขนาด proof ใหญ่ |
| Flat key-value + single tree | ไม่มี hierarchical query, proof เดียวครอบคลุมทุกอย่าง |
| B-tree | ไม่ได้ถูก Merklize โดยธรรมชาติ การรับรองมีความซับซ้อน |

GroveDB แก้ปัญหาเหล่านี้โดยรวม **การรับประกันสมดุลที่พิสูจน์แล้วของ AVL tree** เข้ากับ **การซ้อนแบบลำดับชั้น** และ **ระบบประเภท element ที่หลากหลาย**

## ภาพรวมสถาปัตยกรรม

GroveDB ถูกจัดระเบียบเป็นชั้น (layer) ที่แตกต่างกัน โดยแต่ละชั้นมีหน้าที่ชัดเจน:

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

ข้อมูลไหล **ลง** ผ่านชั้นเหล่านี้ในระหว่างการเขียน และไหล **ขึ้น** ในระหว่างการอ่าน ทุกการดำเนินการจะสะสมต้นทุน (cost) ขณะที่ผ่านแต่ละชั้น ทำให้สามารถคิดค่าใช้จ่ายทรัพยากรได้อย่างแม่นยำ

---
