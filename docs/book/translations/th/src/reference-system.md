# ระบบ Reference

## ทำไมจึงมี Reference

ในฐานข้อมูลแบบลำดับชั้น คุณมักจะต้องการให้ข้อมูลเดียวกันเข้าถึงได้จากหลาย path ตัวอย่างเช่น เอกสารอาจถูกจัดเก็บภายใต้ contract แต่ก็สามารถสืบค้นได้ตาม owner identity ด้วย **Reference** คือคำตอบของ GroveDB — เป็นตัวชี้ (pointer) จากตำแหน่งหนึ่งไปยังอีกตำแหน่งหนึ่ง คล้ายกับ symbolic link ในระบบไฟล์

```mermaid
graph LR
    subgraph primary["Primary Storage"]
        item["contracts/C1/docs/D1<br/><b>Item</b>(data)"]
    end
    subgraph secondary["Secondary Index"]
        ref["identities/alice/docs/D1<br/><b>Reference</b>"]
    end
    ref -->|"points to"| item

    style primary fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style secondary fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style ref fill:#fef9e7,stroke:#f39c12,stroke-width:2px
```

คุณสมบัติหลัก:
- Reference ได้รับ **การรับรอง (authenticated)** — value_hash ของ reference รวมทั้ง reference เองและ element ที่ถูกอ้างอิง
- Reference สามารถ **ต่อเป็นลูกโซ่ (chained)** ได้ — reference สามารถชี้ไปยัง reference อื่น
- การตรวจจับวงจร (cycle detection) ป้องกันการวนซ้ำไม่สิ้นสุด
- ขีดจำกัดการกระโดด (hop limit) ที่ปรับได้ป้องกันการใช้ทรัพยากรจนหมด

## Reference ทั้งเจ็ดประเภท

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

มาดูแต่ละประเภทพร้อมแผนภาพ

### AbsolutePathReference

ประเภทที่ง่ายที่สุด จัดเก็บ path เต็มไปยังเป้าหมาย:

```mermaid
graph TD
    subgraph root["Root Merk — path: []"]
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
    X ==>|"resolves to [P, Q, R]"| R

    style merkAB fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style merkPQ fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style X fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style R fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

> X จัดเก็บ path แบบสัมบูรณ์เต็ม `[P, Q, R]` ไม่ว่า X จะอยู่ที่ไหน มันจะ resolve (แก้ไข) ไปยังเป้าหมายเดียวกันเสมอ

### UpstreamRootHeightReference

เก็บ N ส่วนแรกของ path ปัจจุบัน แล้วต่อด้วย path ใหม่:

```mermaid
graph TD
    subgraph resolve["Resolution: keep first 2 segments + append [P, Q]"]
        direction LR
        curr["current: [A, B, C, D]"] --> keep["keep first 2: [A, B]"] --> append["append: [A, B, <b>P, Q</b>]"]
    end

    subgraph grove["Grove Hierarchy"]
        gA["A (height 0)"]
        gB["B (height 1)"]
        gC["C (height 2)"]
        gD["D (height 3)"]
        gX["X = Reference<br/>UpstreamRootHeight(2, [P,Q])"]
        gP["P (height 2)"]
        gQ["Q (height 3) — target"]

        gA --> gB
        gB --> gC
        gB -->|"keep first 2 → [A,B]<br/>then descend [P,Q]"| gP
        gC --> gD
        gD -.-> gX
        gP --> gQ
    end

    gX ==>|"resolves to"| gQ

    style resolve fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style gX fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style gQ fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

### UpstreamRootHeightWithParentPathAdditionReference

เหมือน UpstreamRootHeight แต่เพิ่มส่วนสุดท้ายของ path ปัจจุบันกลับเข้าไป:

```text
    Reference ที่ path [A, B, C, D, E] key=X
    UpstreamRootHeightWithParentPathAdditionReference(2, [P, Q])

    Current path:    [A, B, C, D, E]
    เก็บ 2 ส่วนแรก: [A, B]
    ต่อ [P, Q]:      [A, B, P, Q]
    เพิ่มส่วนสุดท้ายกลับ: [A, B, P, Q, E]   ← "E" จาก path ดั้งเดิมถูกเพิ่มกลับ

    มีประโยชน์สำหรับ: ดัชนีที่ parent key ควรถูกรักษาไว้
```

### UpstreamFromElementHeightReference

ตัด N ส่วนท้ายออก แล้วต่อด้วย:

```text
    Reference ที่ path [A, B, C, D] key=X
    UpstreamFromElementHeightReference(1, [P, Q])

    Current path:     [A, B, C, D]
    ตัดส่วนท้าย 1:    [A, B, C]
    ต่อ [P, Q]:       [A, B, C, P, Q]
```

### CousinReference

แทนที่เฉพาะ parent โดยตรงด้วย key ใหม่:

```mermaid
graph TD
    subgraph resolve["Resolution: pop last 2, push cousin C, push key X"]
        direction LR
        r1["path: [A, B, M, D]"] --> r2["pop last 2: [A, B]"] --> r3["push C: [A, B, C]"] --> r4["push key X: [A, B, C, X]"]
    end

    subgraph merkAB["Merk [A, B]"]
        M["M<br/>Tree"]
        C["C<br/>Tree<br/>(cousin of M)"]
    end

    subgraph merkABM["Merk [A, B, M]"]
        D["D<br/>Tree"]
    end

    subgraph merkABMD["Merk [A, B, M, D]"]
        Xref["X = Reference<br/>CousinReference(C)"]
    end

    subgraph merkABC["Merk [A, B, C]"]
        Xtarget["X = Item<br/>(target)"]
    end

    M -.-> D
    D -.-> Xref
    C -.-> Xtarget
    Xref ==>|"resolves to [A, B, C, X]"| Xtarget

    style resolve fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style Xref fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style Xtarget fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style M fill:#fadbd8,stroke:#e74c3c
    style C fill:#d5f5e3,stroke:#27ae60
```

> "cousin (ลูกพี่ลูกน้อง)" คือ subtree พี่น้องของปู่/ย่าของ reference โดย reference จะนำทางขึ้น 2 ระดับ แล้วลงไปใน cousin subtree

### RemovedCousinReference

เหมือน CousinReference แต่แทนที่ parent ด้วย path หลายส่วน:

```text
    Reference ที่ path [A, B, C, D] key=X
    RemovedCousinReference([M, N])

    Current path:  [A, B, C, D]
    Pop parent C:  [A, B]
    ต่อ [M, N]:    [A, B, M, N]
    Push key X:    [A, B, M, N, X]
```

### SiblingReference

reference แบบสัมพัทธ์ที่ง่ายที่สุด — แค่เปลี่ยน key ภายใน parent เดียวกัน:

```mermaid
graph TD
    subgraph merk["Merk [A, B, C] — same tree, same path"]
        M_sib["M"]
        X_sib["X = Reference<br/>SiblingRef(Y)"]
        Y_sib["Y = Item<br/>(target)"]
        Z_sib["Z = Item"]
        M_sib --> X_sib
        M_sib --> Y_sib
    end

    X_sib ==>|"resolves to [A, B, C, Y]"| Y_sib

    style merk fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style X_sib fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style Y_sib fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

> ประเภท reference ที่ง่ายที่สุด X และ Y เป็นพี่น้อง (sibling) ใน Merk tree เดียวกัน — การ resolve แค่เปลี่ยน key โดยรักษา path เดิม

## การตาม Reference และขีดจำกัดการกระโดด (Hop Limit)

เมื่อ GroveDB พบ element ชนิด Reference มันต้อง **ตาม (follow)** ไปเพื่อหาค่าจริง เนื่องจาก reference สามารถชี้ไปยัง reference อื่น สิ่งนี้เกี่ยวข้องกับลูป:

```rust
// grovedb/src/reference_path.rs
pub const MAX_REFERENCE_HOPS: usize = 10;

pub fn follow_reference(...) -> CostResult<ResolvedReference, Error> {
    let mut hops_left = MAX_REFERENCE_HOPS;
    let mut visited = HashSet::new();

    while hops_left > 0 {
        // Resolve reference path เป็น absolute path
        let target_path = current_ref.absolute_qualified_path(...);

        // ตรวจสอบวงจร
        if !visited.insert(target_path.clone()) {
            return Err(Error::CyclicReference);
        }

        // ดึง element ที่เป้าหมาย
        let element = Element::get(target_path);

        match element {
            Element::Reference(next_ref, ..) => {
                // ยังเป็น reference — ตามต่อ
                current_ref = next_ref;
                hops_left -= 1;
            }
            other => {
                // เจอ element จริงแล้ว!
                return Ok(ResolvedReference { element: other, ... });
            }
        }
    }

    Err(Error::ReferenceLimit)  // เกินขีดจำกัด 10 hops
}
```

## การตรวจจับวงจร (Cycle Detection)

HashSet `visited` ติดตามทุก path ที่เคยเห็น ถ้าพบ path ที่เคยเยือนแล้ว แสดงว่ามีวงจร:

```mermaid
graph LR
    A["A<br/>Reference"] -->|"step 1"| B["B<br/>Reference"]
    B -->|"step 2"| C["C<br/>Reference"]
    C -->|"step 3"| A

    style A fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style B fill:#fef9e7,stroke:#f39c12
    style C fill:#fef9e7,stroke:#f39c12
```

> **การตรวจสอบการตรวจจับวงจร:**
>
> | ขั้นตอน | ตาม | visited set | ผลลัพธ์ |
> |------|--------|-------------|--------|
> | 1 | เริ่มที่ A | { A } | A เป็น Ref → ตามต่อ |
> | 2 | A → B | { A, B } | B เป็น Ref → ตามต่อ |
> | 3 | B → C | { A, B, C } | C เป็น Ref → ตามต่อ |
> | 4 | C → A | A อยู่ใน visited แล้ว! | **Error::CyclicRef** |
>
> หากไม่มีการตรวจจับวงจร สิ่งนี้จะวนซ้ำตลอดไป `MAX_REFERENCE_HOPS = 10` ยังจำกัดความลึกของการเดินทางสำหรับ chain ที่ยาวด้วย

## Reference ใน Merk — Combined Value Hash

เมื่อ Reference ถูกจัดเก็บใน Merk tree `value_hash` ของมันต้องรับรองทั้งโครงสร้าง reference และข้อมูลที่ถูกอ้างอิง:

```rust
// merk/src/tree/kv.rs
pub fn update_hashes_using_reference_value_hash(
    mut self,
    reference_value_hash: CryptoHash,
) -> CostContext<Self> {
    // แฮชไบต์ของ reference element เอง
    let actual_value_hash = value_hash(self.value_as_slice());

    // รวม: H(reference_bytes) + H(referenced_data)
    let combined = combine_hash(&actual_value_hash, &reference_value_hash);

    self.value_hash = combined;
    self.hash = kv_digest_to_kv_hash(self.key(), self.value_hash());
    // ...
}
```

ซึ่งหมายความว่าการเปลี่ยนทั้ง reference เองหรือข้อมูลที่มันชี้ไป จะเปลี่ยน root hash — ทั้งสองถูกผูกทางการเข้ารหัส

---
