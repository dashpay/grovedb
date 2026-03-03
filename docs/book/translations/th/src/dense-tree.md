# DenseAppendOnlyFixedSizeTree — ที่จัดเก็บ Merkle แบบ Dense ขนาดคงที่

DenseAppendOnlyFixedSizeTree คือ complete binary tree ที่มีความสูงคงที่ โดย **ทุกโหนด** — ทั้ง internal และ leaf — จัดเก็บค่าข้อมูล ตำแหน่งจะถูกเติมตามลำดับ level-order (BFS): root ก่อน (ตำแหน่ง 0) จากนั้นจากซ้ายไปขวาในแต่ละระดับ ไม่มีการเก็บ intermediate hash; root hash ถูกคำนวณใหม่ทันทีโดยการแฮชแบบ recursive จาก leaf ถึง root

การออกแบบนี้เหมาะสำหรับโครงสร้างข้อมูลขนาดเล็กที่มีขอบเขตจำกัด โดยที่ความจุสูงสุดทราบล่วงหน้า และคุณต้องการ O(1) append, O(1) retrieval ตามตำแหน่ง และ root hash commitment ขนาด 32 ไบต์ที่เปลี่ยนแปลงหลังจากทุกการแทรก

## โครงสร้างต้นไม้

ต้นไม้ที่มีความสูง *h* มีความจุ `2^h - 1` ตำแหน่ง ตำแหน่งใช้การจัดดัชนีแบบ level-order เริ่มต้นที่ 0:

```text
Height 3 tree (capacity = 7):

              pos 0          ← root (level 0)
             /     \
          pos 1    pos 2     ← level 1
         /   \    /   \
       pos 3 pos 4 pos 5 pos 6  ← level 2 (leaves)

Navigation:
  left_child(i)  = 2i + 1
  right_child(i) = 2i + 2
  parent(i)      = (i - 1) / 2
  is_leaf(i)     = 2i + 1 >= capacity
```

ค่าจะถูก append ตามลำดับ: ค่าแรกจะไปที่ตำแหน่ง 0 (root) จากนั้นตำแหน่ง 1, 2, 3 และต่อไปเรื่อย ๆ ซึ่งหมายความว่า root จะมีข้อมูลเสมอ และต้นไม้จะเติมตาม level-order — ลำดับการท่องที่เป็นธรรมชาติที่สุดสำหรับ complete binary tree

## การคำนวณแฮช

root hash ไม่ได้ถูกจัดเก็บแยกต่างหาก — มันถูกคำนวณใหม่ทุกครั้งที่ต้องการ อัลกอริทึมแบบ recursive เข้าเยี่ยมเฉพาะตำแหน่งที่มีข้อมูล:

```text
hash(position, store):
  value = store.get(position)

  if position is unfilled (>= count):
    return [0; 32]                                    ← empty sentinel

  value_hash = blake3(value)
  left_hash  = hash(2 * position + 1, store)
  right_hash = hash(2 * position + 2, store)
  return blake3(value_hash || left_hash || right_hash)
```

**คุณสมบัติสำคัญ:**
- ทุกโหนด (leaf และ internal): `blake3(blake3(value) || H(left) || H(right))`
- Leaf node: left_hash และ right_hash เป็น `[0; 32]` ทั้งคู่ (children ที่ยังไม่มีข้อมูล)
- ตำแหน่งที่ยังไม่มีข้อมูล: `[0u8; 32]` (zero hash)
- ต้นไม้ว่าง (count = 0): `[0u8; 32]`

**ไม่มีการใช้ domain separation tag สำหรับ leaf/internal** โครงสร้างต้นไม้ (`height` และ `count`) ถูก authenticate จากภายนอกใน parent `Element::DenseAppendOnlyFixedSizeTree` ซึ่งไหลผ่าน Merk hierarchy ผู้ตรวจสอบจะทราบเสมอว่าตำแหน่งใดเป็น leaf เทียบกับ internal node จาก height และ count ดังนั้นผู้โจมตีไม่สามารถแทนที่อันหนึ่งด้วยอีกอันหนึ่งได้โดยไม่ทำลายสายการ authenticate ของ parent

นี่หมายความว่า root hash เข้ารหัส commitment ต่อทุกค่าที่จัดเก็บและตำแหน่งที่แน่นอนของมันในต้นไม้ การเปลี่ยนแปลงค่าใด ๆ (หากมันเปลี่ยนแปลงได้) จะส่งผลต่อ ancestor hash ทั้งหมดขึ้นไปจนถึง root

**ต้นทุนแฮช:** การคำนวณ root hash เข้าเยี่ยมทุกตำแหน่งที่มีข้อมูลรวมถึง children ที่ยังไม่มีข้อมูล สำหรับต้นไม้ที่มี *n* ค่า กรณีเลวร้ายที่สุดคือ O(*n*) blake3 call สิ่งนี้ยอมรับได้เพราะต้นไม้ถูกออกแบบสำหรับความจุขนาดเล็กที่มีขอบเขต (height สูงสุด 16, ตำแหน่งสูงสุด 65,535)

## Element Variant

```rust
Element::DenseAppendOnlyFixedSizeTree(
    u16,                   // count — จำนวนค่าที่จัดเก็บ (สูงสุด 65,535)
    u8,                    // height — ไม่เปลี่ยนแปลงหลังสร้าง (1..=16)
    Option<ElementFlags>,  // flags — storage flags
)
```

| ฟิลด์ | ประเภท | คำอธิบาย |
|---|---|---|
| `count` | `u16` | จำนวนค่าที่แทรกจนถึงปัจจุบัน (สูงสุด 65,535) |
| `height` | `u8` | ความสูงต้นไม้ (1..=16) ไม่เปลี่ยนแปลงหลังสร้าง |
| `flags` | `Option<ElementFlags>` | Storage flag เพิ่มเติม |

root hash ไม่ได้ถูกจัดเก็บใน Element — มันไหลเป็น Merk child hash ผ่านพารามิเตอร์ `subtree_root_hash` ของ `insert_subtree`

**Discriminant:** 14 (ElementType), TreeType = 10

**ขนาดต้นทุน:** `DENSE_TREE_COST_SIZE = 6` ไบต์ (2 count + 1 height + 1 discriminant + 2 overhead)

## Storage Layout

เช่นเดียวกับ MmrTree และ BulkAppendTree, DenseAppendOnlyFixedSizeTree จัดเก็บข้อมูลใน **data** namespace (ไม่ใช่ child Merk) ค่าถูก key ด้วยตำแหน่งเป็น big-endian `u64`:

```text
Subtree path: blake3(parent_path || key)

Storage keys:
  [0, 0, 0, 0, 0, 0, 0, 0] → value at position 0 (root)
  [0, 0, 0, 0, 0, 0, 0, 1] → value at position 1
  [0, 0, 0, 0, 0, 0, 0, 2] → value at position 2
  ...
```

Element เอง (จัดเก็บใน parent Merk) ถือ `count` และ `height` root hash ไหลเป็น Merk child hash นี่หมายความว่า:
- **การอ่าน root hash** ต้องการคำนวณใหม่จาก storage (O(n) hashing)
- **การอ่านค่าตามตำแหน่งคือ O(1)** — อ่าน storage ครั้งเดียว
- **การแทรกคือ O(n) hashing** — เขียน storage หนึ่งครั้ง + คำนวณ root hash ใหม่ทั้งหมด

## การดำเนินการ

### `dense_tree_insert(path, key, value, tx, grove_version)`

เพิ่มค่าไปยังตำแหน่งถัดไปที่ว่าง ส่งคืน `(root_hash, position)`

```text
ขั้นตอน 1: อ่าน element, ดึง (count, height)
ขั้นตอน 2: ตรวจสอบความจุ: ถ้า count >= 2^height - 1 → error
ขั้นตอน 3: สร้าง subtree path, เปิด storage context
ขั้นตอน 4: เขียนค่าไปยังตำแหน่ง = count
ขั้นตอน 5: สร้าง DenseFixedSizedMerkleTree ใหม่จากสถานะ
ขั้นตอน 6: เรียก tree.insert(value, store) → (root_hash, position, hash_calls)
ขั้นตอน 7: อัปเดต element ด้วย root_hash ใหม่และ count + 1
ขั้นตอน 8: เผยแพร่การเปลี่ยนแปลงขึ้นไปผ่าน Merk hierarchy
ขั้นตอน 9: Commit transaction
```

### `dense_tree_get(path, key, position, tx, grove_version)`

ดึงค่าที่ตำแหน่งที่กำหนด ส่งคืน `None` ถ้า position >= count

### `dense_tree_root_hash(path, key, tx, grove_version)`

ส่งคืน root hash ที่จัดเก็บใน element นี่คือ hash ที่คำนวณระหว่างการแทรกล่าสุด — ไม่จำเป็นต้องคำนวณใหม่

### `dense_tree_count(path, key, tx, grove_version)`

ส่งคืนจำนวนค่าที่จัดเก็บ (ฟิลด์ `count` จาก element)

## การดำเนินการแบบ Batch

Variant `GroveOp::DenseTreeInsert` รองรับการแทรกแบบ batch ผ่าน pipeline batch มาตรฐานของ GroveDB:

```rust
let ops = vec![
    QualifiedGroveDbOp::dense_tree_insert_op(
        vec![b"parent".to_vec()],
        b"my_dense_tree".to_vec(),
        b"value_data".to_vec(),
    ),
];
db.apply_batch(ops, None, None, grove_version)?;
```

**Preprocessing:** เช่นเดียวกับทุกประเภทต้นไม้ non-Merk, `DenseTreeInsert` op จะถูกประมวลผลล่วงหน้าก่อนที่ batch body หลักจะทำงาน method `preprocess_dense_tree_ops`:

1. จัดกลุ่ม `DenseTreeInsert` op ทั้งหมดตาม `(path, key)`
2. สำหรับแต่ละกลุ่ม ดำเนินการแทรกตามลำดับ (อ่าน element, แทรกแต่ละค่า, อัปเดต root hash)
3. แปลงแต่ละกลุ่มเป็น `ReplaceNonMerkTreeRoot` op ที่ส่ง `root_hash` สุดท้ายและ `count` ผ่านกลไก propagation มาตรฐาน

การแทรกหลายครั้งไปยัง dense tree เดียวกันภายใน batch เดียวได้รับการรองรับ — พวกมันจะถูกประมวลผลตามลำดับ และการตรวจสอบความสม่ำเสมออนุญาต key ซ้ำสำหรับ op ประเภทนี้

**Propagation:** root hash และ count ไหลผ่าน variant `NonMerkTreeMeta::DenseTree` ใน `ReplaceNonMerkTreeRoot` ตามรูปแบบเดียวกับ MmrTree และ BulkAppendTree

## Proof

DenseAppendOnlyFixedSizeTree รองรับ **V1 subquery proof** ผ่าน variant `ProofBytes::DenseTree` ตำแหน่งแต่ละตัวสามารถถูกพิสูจน์ต่อ root hash ของต้นไม้โดยใช้ inclusion proof ที่ถือค่า ancestor และ sibling subtree hash

### โครงสร้าง Auth Path

เนื่องจาก internal node แฮช **ค่าของตัวเอง** (ไม่ใช่แค่ child hash) authentication path จึงแตกต่างจาก Merkle tree มาตรฐาน ในการตรวจสอบ leaf ที่ตำแหน่ง `p` ผู้ตรวจสอบต้องการ:

1. **ค่า leaf** (entry ที่ถูกพิสูจน์)
2. **Ancestor value hash** สำหรับทุก internal node บนเส้นทางจาก `p` ถึง root (เฉพาะ hash 32 ไบต์ ไม่ใช่ค่าเต็ม)
3. **Sibling subtree hash** สำหรับทุก child ที่ไม่อยู่บนเส้นทาง

เนื่องจากทุกโหนดใช้ `blake3(H(value) || H(left) || H(right))` (ไม่มี domain tag) proof จึงมีเพียง value hash ขนาด 32 ไบต์สำหรับ ancestor — ไม่ใช่ค่าเต็ม สิ่งนี้ทำให้ proof มีขนาดกระทัดรัดไม่ว่าค่าแต่ละตัวจะใหญ่แค่ไหน

```rust
pub struct DenseTreeProof {
    pub entries: Vec<(u16, Vec<u8>)>,            // คู่ (ตำแหน่ง, ค่า) ที่ถูกพิสูจน์
    pub node_value_hashes: Vec<(u16, [u8; 32])>, // ancestor value hash บน auth path
    pub node_hashes: Vec<(u16, [u8; 32])>,       // sibling subtree hash ที่คำนวณไว้แล้ว
}
```

> **หมายเหตุ:** `height` และ `count` ไม่อยู่ใน proof struct — ผู้ตรวจสอบได้รับจาก parent Element ซึ่งถูก authenticate โดย Merk hierarchy

### ตัวอย่างทีละขั้นตอน

ต้นไม้ที่มี height=3, capacity=7, count=5, พิสูจน์ตำแหน่ง 4:

```text
        0
       / \
      1   2
     / \ / \
    3  4 5  6
```

เส้นทางจาก 4 ถึง root: `4 → 1 → 0` เซตที่ขยาย: `{0, 1, 4}`

proof ประกอบด้วย:
- **entries**: `[(4, value[4])]` — ตำแหน่งที่ถูกพิสูจน์
- **node_value_hashes**: `[(0, H(value[0])), (1, H(value[1]))]` — ancestor value hash (32 ไบต์แต่ละตัว ไม่ใช่ค่าเต็ม)
- **node_hashes**: `[(2, H(subtree_2)), (3, H(node_3))]` — sibling ที่ไม่อยู่บนเส้นทาง

การตรวจสอบคำนวณ root hash ใหม่จากล่างขึ้นบน:
1. `H(4) = blake3(blake3(value[4]) || [0;32] || [0;32])` — leaf (children ยังไม่มีข้อมูล)
2. `H(3)` — จาก `node_hashes`
3. `H(1) = blake3(H(value[1]) || H(3) || H(4))` — internal ใช้ value hash จาก `node_value_hashes`
4. `H(2)` — จาก `node_hashes`
5. `H(0) = blake3(H(value[0]) || H(1) || H(2))` — root ใช้ value hash จาก `node_value_hashes`
6. เปรียบเทียบ `H(0)` กับ root hash ที่คาดหวัง

### Multi-Position Proof

เมื่อพิสูจน์หลายตำแหน่ง expanded set จะรวม auth path ที่ทับซ้อนกัน Ancestor ที่ใช้ร่วมกันจะถูกรวมเพียงครั้งเดียว ทำให้ proof แบบหลายตำแหน่งมีขนาดกระทัดรัดกว่า proof แบบตำแหน่งเดียวที่เป็นอิสระ

### ข้อจำกัดของ V0

V0 proof ไม่สามารถลงไปใน dense tree ได้ หาก V0 query ตรงกับ `DenseAppendOnlyFixedSizeTree` ที่มี subquery ระบบจะส่งคืน `Error::NotSupported` เพื่อแนะนำให้ผู้เรียกใช้ `prove_query_v1`

### การเข้ารหัส Query Key

ตำแหน่งของ dense tree ถูกเข้ารหัสเป็น **big-endian u16** (2 ไบต์) query key ซึ่งต่างจาก MmrTree และ BulkAppendTree ที่ใช้ u64 ทุก variant มาตรฐานของ `QueryItem` range ได้รับการรองรับ

## การเปรียบเทียบกับต้นไม้ Non-Merk อื่น ๆ

| | DenseTree | MmrTree | BulkAppendTree | CommitmentTree |
|---|---|---|---|---|
| **Element discriminant** | 14 | 12 | 13 | 11 |
| **TreeType** | 10 | 8 | 9 | 7 |
| **ความจุ** | คงที่ (`2^h - 1`, สูงสุด 65,535) | ไม่จำกัด | ไม่จำกัด | ไม่จำกัด |
| **โมเดลข้อมูล** | ทุกตำแหน่งจัดเก็บค่า | เฉพาะ leaf | Dense tree buffer + chunks | เฉพาะ leaf |
| **Hash ใน Element?** | ไม่ (ไหลเป็น child hash) | ไม่ (ไหลเป็น child hash) | ไม่ (ไหลเป็น child hash) | ไม่ (ไหลเป็น child hash) |
| **ต้นทุนการแทรก (hashing)** | O(n) blake3 | O(1) amortized | O(1) amortized | ~33 Sinsemilla |
| **ขนาดต้นทุน** | 6 ไบต์ | 11 ไบต์ | 12 ไบต์ | 12 ไบต์ |
| **รองรับ Proof** | V1 (Dense) | V1 (MMR) | V1 (Bulk) | V1 (CommitmentTree) |
| **เหมาะสำหรับ** | โครงสร้างขนาดเล็กมีขอบเขต | ล็อกเหตุการณ์ | ล็อกปริมาณมาก | ZK commitment |

**เมื่อใดควรเลือก DenseAppendOnlyFixedSizeTree:**
- จำนวน entry สูงสุดเป็นที่ทราบตอนสร้าง
- คุณต้องการให้ทุกตำแหน่ง (รวมถึง internal node) จัดเก็บข้อมูล
- คุณต้องการโมเดลข้อมูลที่เรียบง่ายที่สุดโดยไม่มีการเติบโตแบบไม่มีขอบเขต
- O(n) root hash recomputation เป็นที่ยอมรับได้ (ความสูงต้นไม้เล็ก)

**เมื่อใดไม่ควรเลือก:**
- คุณต้องการความจุไม่จำกัด → ใช้ MmrTree หรือ BulkAppendTree
- คุณต้องการความเข้ากันได้กับ ZK → ใช้ CommitmentTree

## ตัวอย่างการใช้งาน

```rust
use grovedb::Element;
use grovedb_version::version::GroveVersion;

let grove_version = GroveVersion::latest();

// สร้าง dense tree ที่มีความสูง 4 (ความจุ = 15 ค่า)
db.insert(
    &[b"state"],
    b"validator_slots",
    Element::empty_dense_tree(4),
    None,
    None,
    grove_version,
)?;

// เพิ่มค่า — ตำแหน่งจะเติม 0, 1, 2, ...
let (root_hash, pos) = db.dense_tree_insert(
    &[b"state"],
    b"validator_slots",
    validator_pubkey.to_vec(),
    None,
    grove_version,
)?;
// pos == 0, root_hash = blake3(validator_pubkey)

// อ่านกลับตามตำแหน่ง
let value = db.dense_tree_get(
    &[b"state"],
    b"validator_slots",
    0,     // ตำแหน่ง
    None,
    grove_version,
)?;
assert_eq!(value, Some(validator_pubkey.to_vec()));

// สืบค้น metadata
let count = db.dense_tree_count(&[b"state"], b"validator_slots", None, grove_version)?;
let hash = db.dense_tree_root_hash(&[b"state"], b"validator_slots", None, grove_version)?;
```

## ไฟล์ Implementation

| ไฟล์ | เนื้อหา |
|---|---|
| `grovedb-dense-fixed-sized-merkle-tree/src/lib.rs` | `DenseTreeStore` trait, `DenseFixedSizedMerkleTree` struct, recursive hash |
| `grovedb-dense-fixed-sized-merkle-tree/src/proof.rs` | `DenseTreeProof` struct, `generate()`, `encode_to_vec()`, `decode_from_slice()` |
| `grovedb-dense-fixed-sized-merkle-tree/src/verify.rs` | `DenseTreeProof::verify()` — pure function, ไม่ต้องการ storage |
| `grovedb-element/src/element/mod.rs` | `Element::DenseAppendOnlyFixedSizeTree` (discriminant 14) |
| `grovedb-element/src/element/constructor.rs` | `empty_dense_tree()`, `new_dense_tree()` |
| `merk/src/tree_type/mod.rs` | `TreeType::DenseAppendOnlyFixedSizeTree = 10` |
| `merk/src/tree_type/costs.rs` | `DENSE_TREE_COST_SIZE = 6` |
| `grovedb/src/operations/dense_tree.rs` | GroveDB operations, `AuxDenseTreeStore`, batch preprocessing |
| `grovedb/src/operations/proof/generate.rs` | `generate_dense_tree_layer_proof()`, `query_items_to_positions()` |
| `grovedb/src/operations/proof/verify.rs` | `verify_dense_tree_lower_layer()` |
| `grovedb/src/operations/proof/mod.rs` | `ProofBytes::DenseTree` variant |
| `grovedb/src/batch/estimated_costs/average_case_costs.rs` | โมเดลต้นทุนกรณีเฉลี่ย |
| `grovedb/src/batch/estimated_costs/worst_case_costs.rs` | โมเดลต้นทุนกรณีเลวร้ายที่สุด |
| `grovedb/src/tests/dense_tree_tests.rs` | 22 integration test |

---
