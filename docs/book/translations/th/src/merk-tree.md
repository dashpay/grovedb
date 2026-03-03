# Merk Tree — ต้นไม้ Merkle AVL

Merk tree เป็นหน่วยสร้างพื้นฐานของ GroveDB ทุก subtree (ต้นไม้ย่อย) ใน grove คือ Merk tree — ต้นไม้ค้นหาแบบทวิภาค (binary search tree) ที่ปรับสมดุลตัวเอง โดยทุกโหนดจะถูกแฮชด้วยวิธีการเข้ารหัส ทำให้ได้ root hash (แฮชราก) เพียงค่าเดียวที่รับรองเนื้อหาทั้งหมดของต้นไม้

## Merk Node คืออะไร?

ต่างจากการนำ Merkle tree ไปใช้งานหลายแบบที่ข้อมูลอยู่เฉพาะที่ใบ (leaf) ใน Merk tree **ทุกโหนดจัดเก็บคู่ key-value** ซึ่งหมายความว่าไม่มีโหนดภายใน (internal node) ที่ "ว่างเปล่า" — ต้นไม้เป็นทั้งโครงสร้างค้นหาและที่เก็บข้อมูลพร้อมกัน

```mermaid
graph TD
    subgraph TreeNode
        subgraph inner["inner: Box&lt;TreeNodeInner&gt;"]
            subgraph kv["kv: KV"]
                KEY["<b>key:</b> Vec&lt;u8&gt;<br/><i>e.g. b&quot;alice&quot;</i>"]
                VAL["<b>value:</b> Vec&lt;u8&gt;<br/><i>serialized Element bytes</i>"]
                FT["<b>feature_type:</b> TreeFeatureType<br/><i>BasicMerkNode | SummedMerkNode(n) | ...</i>"]
                VH["<b>value_hash:</b> [u8; 32]<br/><i>H(varint(value.len) ‖ value)</i>"]
                KVH["<b>hash:</b> [u8; 32] — the kv_hash<br/><i>H(varint(key.len) ‖ key ‖ value_hash)</i>"]
            end
            LEFT["<b>left:</b> Option&lt;Link&gt;"]
            RIGHT["<b>right:</b> Option&lt;Link&gt;"]
        end
        OLD["<b>old_value:</b> Option&lt;Vec&lt;u8&gt;&gt; — previous value for cost deltas"]
        KNOWN["<b>known_storage_cost:</b> Option&lt;KeyValueStorageCost&gt;"]
    end

    LEFT -->|"smaller keys"| LC["Left Child<br/><i>Link::Reference | Modified<br/>Uncommitted | Loaded</i>"]
    RIGHT -->|"larger keys"| RC["Right Child<br/><i>Link::Reference | Modified<br/>Uncommitted | Loaded</i>"]

    style kv fill:#eaf2f8,stroke:#2980b9
    style inner fill:#fef9e7,stroke:#f39c12
    style TreeNode fill:#f9f9f9,stroke:#333
    style LC fill:#d5f5e3,stroke:#27ae60
    style RC fill:#d5f5e3,stroke:#27ae60
```

ในโค้ด (`merk/src/tree/mod.rs`):

```rust
pub struct TreeNode {
    pub(crate) inner: Box<TreeNodeInner>,
    pub(crate) old_value: Option<Vec<u8>>,        // ค่าก่อนหน้าสำหรับการติดตามต้นทุน
    pub(crate) known_storage_cost: Option<KeyValueStorageCost>,
}

pub struct TreeNodeInner {
    pub(crate) left: Option<Link>,    // ลูกซ้าย (key ที่เล็กกว่า)
    pub(crate) right: Option<Link>,   // ลูกขวา (key ที่ใหญ่กว่า)
    pub(crate) kv: KV,               // ข้อมูล key-value
}
```

`Box<TreeNodeInner>` เก็บโหนดไว้บน heap ซึ่งจำเป็นเพราะลิงก์ลูกสามารถมี `TreeNode` instance ทั้งตัวแบบเรียกซ้ำ (recursive) ได้

## โครงสร้าง KV

struct `KV` เก็บทั้งข้อมูลดิบและ digest ทางการเข้ารหัส (`merk/src/tree/kv.rs`):

```rust
pub struct KV {
    pub(super) key: Vec<u8>,                        // key สำหรับค้นหา
    pub(super) value: Vec<u8>,                      // ค่าที่จัดเก็บ
    pub(super) feature_type: TreeFeatureType,       // พฤติกรรมการรวมค่า (aggregation)
    pub(crate) value_defined_cost: Option<ValueDefinedCostType>,
    pub(super) hash: CryptoHash,                    // kv_hash
    pub(super) value_hash: CryptoHash,              // H(value)
}
```

สองประเด็นสำคัญ:

1. **Key ไม่ถูกจัดเก็บบนดิสก์เป็นส่วนหนึ่งของโหนดที่ถูก encode** มันถูกจัดเก็บเป็น key ของ RocksDB เมื่อโหนดถูกถอดรหัส (decode) จากที่เก็บข้อมูล key จะถูกแทรกเข้ามาจากภายนอก วิธีนี้หลีกเลี่ยงการซ้ำซ้อนของไบต์ key

2. **มีฟิลด์แฮชสองฟิลด์** `value_hash` คือ `H(value)` และ `hash` (kv_hash) คือ `H(key, value_hash)` การเก็บทั้งสองค่าช่วยให้ระบบ proof สามารถเลือกได้ว่าจะเปิดเผยข้อมูลมากเท่าใด

## ธรรมชาติกึ่งสมดุล — AVL "โยกเยก" อย่างไร

Merk tree คือ **AVL tree** — ต้นไม้ค้นหาแบบทวิภาคที่ปรับสมดุลตัวเองแบบคลาสสิกที่ประดิษฐ์โดย Adelson-Velsky และ Landis เงื่อนไขหลัก (invariant) คือ:

> สำหรับทุกโหนด ความแตกต่างของความสูงระหว่าง subtree ซ้ายและขวาต้องไม่เกิน 1

ซึ่งแสดงเป็น **balance factor (ตัวชี้วัดสมดุล)**:

```text
balance_factor = right_height - left_height
```

ค่าที่ถูกต้อง: **{-1, 0, 1}**

```rust
// merk/src/tree/mod.rs
pub const fn balance_factor(&self) -> i8 {
    let left_height = self.child_height(true) as i8;
    let right_height = self.child_height(false) as i8;
    right_height - left_height
}
```

แต่มีจุดละเอียดอ่อน: แม้แต่ละโหนดจะเอียงได้เพียงหนึ่งระดับ แต่การเอียงเหล่านี้สามารถ **สะสม** ตลอดทั้งต้นไม้ นี่คือเหตุผลที่เราเรียกมันว่า "กึ่งสมดุล" — ต้นไม้ไม่ได้สมดุลอย่างสมบูรณ์เหมือนต้นไม้ทวิภาคแบบสมบูรณ์ (complete binary tree)

พิจารณาต้นไม้ที่มี 10 โหนด ต้นไม้ที่สมดุลอย่างสมบูรณ์จะมีความสูง 4 (ceil(log2(10+1))) แต่ AVL tree อาจมีความสูง 5:

**สมดุลอย่างสมบูรณ์ (ความสูง 4)** — ทุกระดับเต็ม:

```mermaid
graph TD
    N5["5<br/><small>bf=0</small>"]
    N3["3<br/><small>bf=0</small>"]
    N8["8<br/><small>bf=0</small>"]
    N2["2<br/><small>bf=0</small>"]
    N4["4<br/><small>bf=0</small>"]
    N6["6<br/><small>bf=0</small>"]
    N9["9<br/><small>bf=+1</small>"]
    N10["10<br/><small>bf=0</small>"]

    N5 --- N3
    N5 --- N8
    N3 --- N2
    N3 --- N4
    N8 --- N6
    N8 --- N9
    N9 --- N10

    style N5 fill:#d4e6f1,stroke:#2980b9
```

**AVL ที่ถูกต้องแบบ "โยกเยก" (ความสูง 5)** — แต่ละโหนดเอียงไม่เกิน 1 แต่สะสมกัน:

```mermaid
graph TD
    N4["4<br/><small>bf=+1</small>"]
    N2["2<br/><small>bf=-1</small>"]
    N7["7<br/><small>bf=+1</small>"]
    N1["1<br/><small>bf=-1</small>"]
    N3["3<br/><small>bf=0</small>"]
    N5["5<br/><small>bf=0</small>"]
    N9["9<br/><small>bf=-1</small>"]
    N0["0<br/><small>bf=0</small>"]
    N8["8<br/><small>bf=0</small>"]
    N10["10<br/><small>bf=0</small>"]

    N4 --- N2
    N4 --- N7
    N2 --- N1
    N2 --- N3
    N7 --- N5
    N7 --- N9
    N1 --- N0
    N9 --- N8
    N9 --- N10

    style N4 fill:#fadbd8,stroke:#e74c3c
```

> ความสูง 5 เทียบกับ 4 ที่สมบูรณ์แบบ — นั่นคือ "การโยกเยก" กรณีเลวร้ายที่สุด: h <= 1.44 x log2(n+2)

ต้นไม้ทั้งสองเป็น AVL tree ที่ถูกต้อง! ความสูงกรณีเลวร้ายที่สุดของ AVL tree คือ:

```text
h <= 1.4404 x log2(n + 2) - 0.3277
```

ดังนั้นสำหรับ **n = 1,000,000** โหนด:
- สมดุลสมบูรณ์แบบ: ความสูง 20
- กรณีเลวร้ายที่สุดของ AVL: ความสูง ~29

ค่าใช้จ่ายเพิ่ม ~44% นี้คือราคาของกฎการหมุน (rotation) ที่เรียบง่ายของ AVL ในทางปฏิบัติ การแทรกแบบสุ่มจะสร้างต้นไม้ที่ใกล้เคียงกับสมดุลสมบูรณ์แบบมากกว่า

ต่อไปนี้คือลักษณะของต้นไม้ที่ถูกต้องและไม่ถูกต้อง:

**ถูกต้อง** — balance factor ทั้งหมดอยู่ใน {-1, 0, +1}:

```mermaid
graph TD
    subgraph balanced["Balanced (bf=0)"]
        D1["D<br/>bf=0"] --- B1["B<br/>bf=0"]
        D1 --- F1["F<br/>bf=0"]
        B1 --- A1["A"] & C1["C"]
        F1 --- E1["E"] & G1["G"]
    end
    subgraph rightlean["Right-leaning (bf=+1)"]
        D2["D<br/>bf=+1"] --- B2["B<br/>bf=0"]
        D2 --- F2["F<br/>bf=0"]
        B2 --- A2["A"] & C2["C"]
        F2 --- E2["E"] & G2["G"]
    end
    subgraph leftlean["Left-leaning (bf=-1)"]
        D3["D<br/>bf=-1"] --- B3["B<br/>bf=-1"]
        D3 --- E3["E"]
        B3 --- A3["A"]
    end

    style balanced fill:#d5f5e3,stroke:#27ae60
    style rightlean fill:#d5f5e3,stroke:#27ae60
    style leftlean fill:#d5f5e3,stroke:#27ae60
```

**ไม่ถูกต้อง** — balance factor = +2 (ต้องหมุน!):

```mermaid
graph TD
    B["B<br/><b>bf=+2 ✗</b>"]
    D["D<br/>bf=+1"]
    F["F<br/>bf=0"]
    B --- D
    D --- F

    style B fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
```

> subtree ด้านขวาสูงกว่าด้านซ้าย (ซึ่งว่างเปล่า) 2 ระดับ สิ่งนี้จะกระตุ้นให้เกิด **left rotation (การหมุนซ้าย)** เพื่อคืนค่า invariant ของ AVL

## การหมุน (Rotation) — คืนสมดุล

เมื่อการแทรกหรือลบทำให้ balance factor ถึง +/-2 ต้นไม้ต้องถูก **หมุน** เพื่อคืนค่า invariant ของ AVL มีสี่กรณี ซึ่งย่อได้เป็นสองการดำเนินการพื้นฐาน

### การหมุนซ้ายเดี่ยว (Single Left Rotation)

ใช้เมื่อโหนด **หนักด้านขวา** (bf = +2) และลูกขวาของมัน **หนักด้านขวาหรือสมดุล** (bf >= 0):

**ก่อน** (bf=+2):

```mermaid
graph TD
    A["A<br/><small>bf=+2</small>"]
    t1["t₁"]
    B["B<br/><small>bf≥0</small>"]
    X["X"]
    C["C"]
    A --- t1
    A --- B
    B --- X
    B --- C
    style A fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
```

**หลัง** การหมุนซ้าย — B ถูกเลื่อนขึ้นเป็น root:

```mermaid
graph TD
    B2["B<br/><small>bf=0</small>"]
    A2["A"]
    C2["C"]
    t12["t₁"]
    X2["X"]
    B2 --- A2
    B2 --- C2
    A2 --- t12
    A2 --- X2
    style B2 fill:#d5f5e3,stroke:#27ae60,stroke-width:3px
```

> **ขั้นตอน:** (1) แยก B ออกจาก A (2) แยก X (ลูกซ้ายของ B) (3) เชื่อม X เป็นลูกขวาของ A (4) เชื่อม A เป็นลูกซ้ายของ B subtree ที่มี root เป็น B ตอนนี้สมดุลแล้ว

ในโค้ด (`merk/src/tree/ops.rs`):

```rust
fn rotate<V>(self, left: bool, ...) -> CostResult<Self, Error> {
    // แยกลูกฝั่งที่หนัก
    let (tree, child) = self.detach_expect(left, ...);
    // แยกหลานจากฝั่งตรงข้ามของลูก
    let (child, maybe_grandchild) = child.detach(!left, ...);

    // เชื่อมหลานกับ root ดั้งเดิม
    tree.attach(left, maybe_grandchild)
        .maybe_balance(...)
        .flat_map_ok(|tree| {
            // เชื่อม root ดั้งเดิมเป็นลูกของโหนดที่ถูกเลื่อนขึ้น
            child.attach(!left, Some(tree))
                .maybe_balance(...)
        })
}
```

สังเกตว่า `maybe_balance` ถูกเรียกแบบเรียกซ้ำ (recursive) — การหมุนเองอาจสร้างความไม่สมดุลใหม่ที่ต้องแก้ไขเพิ่มเติม

### การหมุนสองครั้ง (Double Rotation) (ซ้าย-ขวา)

ใช้เมื่อโหนด **หนักด้านซ้าย** (bf = -2) แต่ลูกซ้าย **หนักด้านขวา** (bf > 0) การหมุนเดี่ยวจะไม่แก้ปัญหานี้:

**ขั้นตอนที่ 0: ก่อน** — C หนักด้านซ้าย (bf=-2) แต่ลูกซ้าย A เอียงขวา (bf=+1) การหมุนเดี่ยวไม่สามารถแก้ได้:

```mermaid
graph TD
    C0["C<br/><small>bf=-2</small>"]
    A0["A<br/><small>bf=+1</small>"]
    t4["t₄"]
    t1["t₁"]
    B0["B"]
    t2["t₂"]
    t3["t₃"]
    C0 --- A0
    C0 --- t4
    A0 --- t1
    A0 --- B0
    B0 --- t2
    B0 --- t3
    style C0 fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
```

**ขั้นตอนที่ 1: หมุนซ้ายที่ลูก A** — ตอนนี้ทั้ง C และ B เอียงซ้าย แก้ได้ด้วยการหมุนเดี่ยว:

```mermaid
graph TD
    C1["C<br/><small>bf=-2</small>"]
    B1["B"]
    t41["t₄"]
    A1["A"]
    t31["t₃"]
    t11["t₁"]
    t21["t₂"]
    C1 --- B1
    C1 --- t41
    B1 --- A1
    B1 --- t31
    A1 --- t11
    A1 --- t21
    style C1 fill:#fdebd0,stroke:#e67e22,stroke-width:2px
```

**ขั้นตอนที่ 2: หมุนขวาที่ root C** — สมดุลแล้ว!

```mermaid
graph TD
    B2["B<br/><small>bf=0</small>"]
    A2["A"]
    C2["C"]
    t12["t₁"]
    t22["t₂"]
    t32["t₃"]
    t42["t₄"]
    B2 --- A2
    B2 --- C2
    A2 --- t12
    A2 --- t22
    C2 --- t32
    C2 --- t42
    style B2 fill:#d5f5e3,stroke:#27ae60,stroke-width:3px
```

อัลกอริทึมตรวจจับกรณีนี้โดยเปรียบเทียบทิศทางการเอียงของแม่กับ balance factor ของลูก:

```rust
fn maybe_balance<V>(self, ...) -> CostResult<Self, Error> {
    let balance_factor = self.balance_factor();
    if balance_factor.abs() <= 1 {
        return Ok(self);  // สมดุลอยู่แล้ว
    }

    let left = balance_factor < 0;  // true ถ้าหนักด้านซ้าย

    // ต้องหมุนสองครั้งเมื่อลูกเอียงตรงข้ามกับแม่
    let tree = if left == (self.tree().link(left).unwrap().balance_factor() > 0) {
        // การหมุนแรก: หมุนลูกในทิศทางตรงข้าม
        self.walk_expect(left, |child|
            child.rotate(!left, ...).map_ok(Some), ...
        )
    } else {
        self
    };

    // การหมุนที่สอง (หรือเพียงครั้งเดียว)
    tree.rotate(left, ...)
}
```

## การดำเนินการแบบ Batch — การสร้างและการใช้งาน

แทนที่จะแทรก element ทีละตัว Merk รองรับ batch operation (การดำเนินการเป็นชุด) ที่ใช้การเปลี่ยนแปลงหลายอย่างในรอบเดียว สิ่งนี้สำคัญสำหรับประสิทธิภาพ: batch ของ N การดำเนินการบนต้นไม้ที่มี M element ใช้เวลา **O((M + N) log(M + N))** เทียบกับ O(N log M) สำหรับการแทรกแบบต่อเนื่อง

### ประเภท MerkBatch

```rust
type MerkBatch<K> = [(K, Op)];

enum Op {
    Put(Vec<u8>, TreeFeatureType),  // แทรกหรืออัปเดตด้วยค่าและประเภทคุณลักษณะ
    PutWithSpecializedCost(...),     // แทรกด้วยต้นทุนที่กำหนดไว้ล่วงหน้า
    PutCombinedReference(...),       // แทรก reference ด้วยแฮชรวม
    Replace(Vec<u8>, TreeFeatureType),
    Patch { .. },                    // อัปเดตค่าบางส่วน
    Delete,                          // ลบ key
    DeleteLayered,                   // ลบด้วยต้นทุนแบบชั้น
    DeleteMaybeSpecialized,          // ลบด้วยต้นทุนเฉพาะทางแบบตัวเลือก
}
```

### กลยุทธ์ที่ 1: build() — สร้างจากศูนย์

เมื่อต้นไม้ว่างเปล่า `build()` สร้างต้นไม้ที่สมดุลโดยตรงจาก batch ที่เรียงลำดับแล้วโดยใช้อัลกอริทึม **median-split (แบ่งครึ่งตรงกลาง)**:

batch นำเข้า (เรียงลำดับแล้ว): `[A, B, C, D, E, F, G]` — เลือกตรงกลาง (D) เป็น root, เรียกซ้ำกับแต่ละครึ่ง:

```mermaid
graph TD
    D["<b>D</b><br/><small>root = mid(0..6)</small>"]
    B["<b>B</b><br/><small>mid(A,B,C)</small>"]
    F["<b>F</b><br/><small>mid(E,F,G)</small>"]
    A["A"]
    C["C"]
    E["E"]
    G["G"]

    D --- B
    D --- F
    B --- A
    B --- C
    F --- E
    F --- G

    style D fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style B fill:#d5f5e3,stroke:#27ae60
    style F fill:#d5f5e3,stroke:#27ae60
```

> ผลลัพธ์: ต้นไม้ที่สมดุลอย่างสมบูรณ์แบบด้วยความสูง = 3 = ceil(log2(7))

```rust
fn build(batch: &MerkBatch<K>, ...) -> CostResult<Option<TreeNode>, Error> {
    let mid_index = batch.len() / 2;
    let (mid_key, mid_op) = &batch[mid_index];

    // สร้างโหนด root จาก element ตรงกลาง
    let mid_tree = TreeNode::new(mid_key.clone(), value.clone(), None, feature_type)?;

    // สร้าง subtree ซ้ายและขวาแบบเรียกซ้ำ
    let left = Self::build(&batch[..mid_index], ...);
    let right = Self::build(&batch[mid_index + 1..], ...);

    // เชื่อมลูก
    mid_tree.attach(true, left).attach(false, right)
}
```

วิธีนี้สร้างต้นไม้ที่มีความสูง ceil(log2(n)) — สมดุลอย่างสมบูรณ์แบบ

### กลยุทธ์ที่ 2: apply_sorted() — ผสานเข้ากับต้นไม้ที่มีอยู่

เมื่อต้นไม้มีข้อมูลอยู่แล้ว `apply_sorted()` ใช้ **binary search (การค้นหาแบบทวิภาค)** เพื่อหาว่าแต่ละ batch operation อยู่ตรงไหน จากนั้นใช้การดำเนินการกับ subtree ซ้ายและขวาแบบเรียกซ้ำ:

ต้นไม้ที่มีอยู่พร้อม batch `[(B, Put), (F, Delete)]`:

Binary search: B < D (ไปซ้าย), F > D (ไปขวา)

**ก่อน:**
```mermaid
graph TD
    D0["D"] --- C0["C"]
    D0 --- E0["E"]
    E0 --- F0["F"]
    style D0 fill:#d4e6f1,stroke:#2980b9
```

**หลัง** ใช้ batch และปรับสมดุล:
```mermaid
graph TD
    D1["D"] --- B1["B"]
    D1 --- E1["E"]
    B1 --- C1["C"]
    style D1 fill:#d5f5e3,stroke:#27ae60
```

> B ถูกแทรกเป็น subtree ซ้าย, F ถูกลบจาก subtree ขวา `maybe_balance()` ยืนยัน bf(D) = 0

```rust
fn apply_sorted(self, batch: &MerkBatch<K>, ...) -> CostResult<...> {
    let search = batch.binary_search_by(|(key, _)| key.cmp(self.tree().key()));

    match search {
        Ok(index) => {
            // Key ตรงกับโหนดนี้ — ใช้การดำเนินการโดยตรง
            // (Put แทนที่ค่า, Delete ลบโหนด)
        }
        Err(mid) => {
            // ไม่พบ key — mid คือจุดแบ่ง
            // เรียกซ้ำกับ left_batch[..mid] และ right_batch[mid..]
        }
    }

    self.recurse(batch, mid, exclusive, ...)
}
```

method `recurse` แบ่ง batch และเดินไปซ้ายและขวา:

```rust
fn recurse(self, batch: &MerkBatch<K>, mid: usize, ...) {
    let left_batch = &batch[..mid];
    let right_batch = &batch[mid..];  // หรือ mid+1 ถ้า exclusive

    // ใช้ left batch กับ subtree ซ้าย
    let tree = self.walk(true, |maybe_left| {
        Self::apply_to(maybe_left, left_batch, ...)
    });

    // ใช้ right batch กับ subtree ขวา
    let tree = tree.walk(false, |maybe_right| {
        Self::apply_to(maybe_right, right_batch, ...)
    });

    // ปรับสมดุลหลังแก้ไข
    tree.maybe_balance(...)
}
```

### การลบโหนด

เมื่อลบโหนดที่มีลูกสองตัว Merk จะเลื่อน **edge node (โหนดขอบ)** จาก subtree ที่สูงกว่าขึ้นมาแทน วิธีนี้ลดโอกาสที่จะต้องหมุนเพิ่มเติม:

**ก่อน** — ลบ D (มีลูกสองตัว, ความสูง subtree ขวา >= ซ้าย):

```mermaid
graph TD
    D["D ✗ delete"]
    B0["B"]
    F0["F"]
    A0["A"]
    C0["C"]
    E0["E ← successor"]
    G0["G"]
    D --- B0
    D --- F0
    B0 --- A0
    B0 --- C0
    F0 --- E0
    F0 --- G0
    style D fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style E0 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

**หลัง** — E (ซ้ายสุดใน subtree ขวา = in-order successor ตัวถัดไปตามลำดับ) ถูกเลื่อนขึ้นมาแทนตำแหน่งของ D:

```mermaid
graph TD
    E1["E"]
    B1["B"]
    F1["F"]
    A1["A"]
    C1["C"]
    G1["G"]
    E1 --- B1
    E1 --- F1
    B1 --- A1
    B1 --- C1
    F1 --- G1
    style E1 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

> **กฎ:** ถ้าความสูงซ้าย > ขวา -> เลื่อนขอบขวาของ subtree ซ้ายขึ้น ถ้าความสูงขวา >= ซ้าย -> เลื่อนขอบซ้ายของ subtree ขวาขึ้น วิธีนี้ลดการปรับสมดุลหลังการลบให้น้อยที่สุด

```rust
pub fn remove(self, ...) -> CostResult<Option<Self>, Error> {
    let has_left = tree.link(true).is_some();
    let has_right = tree.link(false).is_some();
    let left = tree.child_height(true) > tree.child_height(false);

    if has_left && has_right {
        // ลูกสองตัว: เลื่อนขอบของลูกที่สูงกว่า
        let (tree, tall_child) = self.detach_expect(left, ...);
        let (_, short_child) = tree.detach_expect(!left, ...);
        tall_child.promote_edge(!left, short_child, ...)
    } else if has_left || has_right {
        // ลูกตัวเดียว: เลื่อนขึ้นมาโดยตรง
        self.detach_expect(left, ...).1
    } else {
        // โหนดใบ: แค่ลบ
        None
    }
}
```

---
