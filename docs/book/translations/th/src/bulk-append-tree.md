# BulkAppendTree — ที่จัดเก็บ Append-Only ปริมาณมาก

BulkAppendTree คือคำตอบของ GroveDB สำหรับความท้าทายทางวิศวกรรมเฉพาะ: จะสร้างล็อก append-only ที่มีปริมาณงานสูงซึ่งรองรับ range proof อย่างมีประสิทธิภาพ ลดการแฮชต่อการเขียน และสร้าง snapshot ของ chunk ที่ไม่เปลี่ยนแปลง (immutable) ที่เหมาะสำหรับการแจกจ่ายผ่าน CDN ได้อย่างไร?

ในขณะที่ MmrTree (บทที่ 13) เหมาะสำหรับ proof ของ leaf แต่ละตัว BulkAppendTree ถูกออกแบบสำหรับ workload ที่มีค่าหลายพันค่ามาถึงต่อบล็อก และ client ต้องการซิงค์โดยดึงช่วงข้อมูล สิ่งนี้ทำได้ด้วย **สถาปัตยกรรมสองระดับ**: dense Merkle tree buffer ที่ดูดซับ append ที่เข้ามา และ chunk-level MMR ที่บันทึก chunk root ที่ finalize แล้ว

## สถาปัตยกรรมสองระดับ

```text
┌────────────────────────────────────────────────────────────────┐
│                      BulkAppendTree                            │
│                                                                │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  Chunk MMR                                               │  │
│  │  ┌────┐ ┌────┐ ┌────┐ ┌────┐                            │  │
│  │  │ R0 │ │ R1 │ │ R2 │ │ H  │ ← Dense Merkle roots      │  │
│  │  └────┘ └────┘ └────┘ └────┘   of each chunk blob       │  │
│  │                     peak hashes bagged together = MMR root│  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  Buffer (DenseFixedSizedMerkleTree, capacity = 2^h - 1) │  │
│  │  ┌───┐ ┌───┐ ┌───┐                                      │  │
│  │  │v_0│ │v_1│ │v_2│ ... (fills in level-order)           │  │
│  │  └───┘ └───┘ └───┘                                      │  │
│  │  dense_tree_root = recomputed root hash of dense tree     │  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                │
│  state_root = blake3("bulk_state" || mmr_root || dense_tree_root) │
│                                                                │
└────────────────────────────────────────────────────────────────┘
```

**ระดับ 1 — Buffer** ค่าที่เข้ามาจะถูกเขียนไปยัง `DenseFixedSizedMerkleTree` (ดูบทที่ 16) ความจุ buffer คือ `2^height - 1` ตำแหน่ง root hash ของ dense tree (`dense_tree_root`) อัปเดตหลังจากทุก insert

**ระดับ 2 — Chunk MMR** เมื่อ buffer เต็ม (ถึง `chunk_size` entry) entry ทั้งหมดจะถูก serialize เป็น **chunk blob** ที่ไม่เปลี่ยนแปลง, dense Merkle root ถูกคำนวณจาก entry เหล่านั้น และ root นั้นถูก append เป็น leaf ไปยัง chunk MMR จากนั้น buffer จะถูกล้าง

**state root** รวมทั้งสองระดับเป็น commitment ขนาด 32 ไบต์เดียวที่เปลี่ยนทุกครั้งที่ append ทำให้มั่นใจว่า parent Merk tree สะท้อนสถานะล่าสุดเสมอ

## ค่าเติม Buffer อย่างไร

แต่ละการเรียก `append()` ดำเนินตามลำดับนี้:

```text
Step 1: Write value to dense tree buffer at next position
        dense_tree.insert(value, store)

Step 2: Increment total_count
        total_count += 1

Step 3: Check if buffer is full (dense tree at capacity)
        if dense_tree.count() == capacity:
            → trigger compaction (§14.3)

Step 4: Compute new state root (+1 blake3 call)
        dense_tree_root = dense_tree.root_hash(store)
        state_root = blake3("bulk_state" || mmr_root || dense_tree_root)
```

**buffer คือ DenseFixedSizedMerkleTree** (ดูบทที่ 16) root hash ของมันเปลี่ยนหลังจากทุก insert ให้ commitment ต่อ entry ทั้งหมดใน buffer ปัจจุบัน root hash นี้คือสิ่งที่ไหลเข้าการคำนวณ state root

## Chunk Compaction

เมื่อ buffer เต็ม (ถึง `chunk_size` entry) compaction จะเกิดขึ้นโดยอัตโนมัติ:

```text
Compaction Steps:
─────────────────
1. Read all chunk_size buffer entries

2. Compute dense Merkle root
   - Hash each entry: leaf[i] = blake3(entry[i])
   - Build complete binary tree bottom-up
   - Extract root hash
   Hash cost: chunk_size + (chunk_size - 1) = 2 * chunk_size - 1

3. Serialize entries into chunk blob
   - Auto-selects fixed-size or variable-size format (§14.6)
   - Store as: store.put(chunk_key(chunk_index), blob)

4. Append dense Merkle root to chunk MMR
   - MMR push with merge cascade (see Chapter 13)
   Hash cost: ~2 amortized (trailing_ones pattern)

5. Reset the dense tree (clear all buffer entries from storage)
   - Dense tree count reset to 0
```

หลัง compaction, chunk blob จะ **ไม่เปลี่ยนแปลงอีกตลอดกาล** — มันจะไม่ถูกแก้ไขอีก สิ่งนี้ทำให้ chunk blob เหมาะสำหรับ CDN caching, client sync และ archival storage

**ตัวอย่าง: 4 append ด้วย chunk_power=2 (chunk_size=4)**

```text
Append v_0: dense_tree=[v_0],       dense_root=H(v_0), total=1
Append v_1: dense_tree=[v_0,v_1],   dense_root=H(v_0,v_1), total=2
Append v_2: dense_tree=[v_0..v_2],  dense_root=H(v_0..v_2), total=3
Append v_3: dense_tree=[v_0..v_3],  dense_root=H(v_0..v_3), total=4
  → COMPACTION:
    chunk_blob_0 = serialize([v_0, v_1, v_2, v_3])
    dense_root_0 = dense_merkle_root(v_0..v_3)
    mmr.push(dense_root_0)
    dense tree cleared (count=0)

Append v_4: dense_tree=[v_4],       dense_root=H(v_4), total=5
  → state_root = blake3("bulk_state" || mmr_root || dense_root)
```

## State Root

state root ผูกทั้งสองระดับเข้าด้วยกันเป็น hash เดียว:

```rust
fn compute_state_root(
    mmr_root: &[u8; 32],         // Chunk MMR root (or [0;32] if empty)
    dense_tree_root: &[u8; 32],  // Root hash of current buffer (dense tree)
) -> [u8; 32] {
    blake3("bulk_state" || mmr_root || dense_tree_root)
}
```

`total_count` และ `chunk_power` **ไม่ได้** รวมอยู่ใน state root เพราะพวกมันถูก authenticate อยู่แล้วโดย Merk value hash — เป็นฟิลด์ของ `Element` ที่ serialize แล้วซึ่งจัดเก็บใน parent Merk node state root จับเฉพาะ data-level commitment (`mmr_root` และ `dense_tree_root`) นี่คือ hash ที่ไหลเป็น Merk child hash และเผยแพร่ขึ้นไปจนถึง GroveDB root hash

## Dense Merkle Root

เมื่อ chunk compact, entry ต้องการ commitment ขนาด 32 ไบต์เดียว BulkAppendTree ใช้ **dense (complete) binary Merkle tree**:

```text
Given entries [e_0, e_1, e_2, e_3]:

Level 0 (leaves):  blake3(e_0)  blake3(e_1)  blake3(e_2)  blake3(e_3)
                      \__________/              \__________/
Level 1:           blake3(h_0 || h_1)       blake3(h_2 || h_3)
                          \____________________/
Level 2 (root):    blake3(h_01 || h_23)  ← this is the dense Merkle root
```

เนื่องจาก `chunk_size` เป็นเลขยกกำลัง 2 เสมอ (จากการสร้าง: `1u32 << chunk_power`) ต้นไม้จะ complete เสมอ (ไม่จำเป็นต้อง padding หรือ dummy leaf) จำนวนแฮชคือ `2 * chunk_size - 1` อย่างแน่นอน:
- `chunk_size` leaf hash (หนึ่งต่อ entry)
- `chunk_size - 1` internal node hash

implementation ของ dense Merkle root อยู่ใน `grovedb-mmr/src/dense_merkle.rs` และมีสองฟังก์ชัน:
- `compute_dense_merkle_root(hashes)` — จาก leaf ที่แฮชไว้แล้ว
- `compute_dense_merkle_root_from_values(values)` — แฮชค่าก่อน จากนั้นสร้างต้นไม้

## Chunk Blob Serialization

Chunk blob คือ archive ที่ไม่เปลี่ยนแปลงซึ่งสร้างโดย compaction ตัว serializer เลือกรูปแบบ wire ที่กระทัดรัดที่สุดโดยอัตโนมัติตามขนาด entry:

**รูปแบบขนาดคงที่** (flag `0x01`) — เมื่อ entry ทั้งหมดมีความยาวเท่ากัน:

```text
┌──────┬──────────┬─────────────┬─────────┬─────────┬─────────┐
│ 0x01 │ count    │ entry_size  │ entry_0 │ entry_1 │ ...     │
│ 1B   │ 4B (BE)  │ 4B (BE)     │ N bytes │ N bytes │         │
└──────┴──────────┴─────────────┴─────────┴─────────┴─────────┘
Total: 1 + 4 + 4 + (count × entry_size) bytes
```

**รูปแบบขนาดแปรผัน** (flag `0x00`) — เมื่อ entry มีความยาวต่างกัน:

```text
┌──────┬──────────┬─────────┬──────────┬─────────┬─────────────┐
│ 0x00 │ len_0    │ entry_0 │ len_1    │ entry_1 │ ...         │
│ 1B   │ 4B (BE)  │ N bytes │ 4B (BE)  │ M bytes │             │
└──────┴──────────┴─────────┴──────────┴─────────┴─────────────┘
Total: 1 + Σ(4 + len_i) bytes
```

รูปแบบขนาดคงที่ประหยัด 4 ไบต์ต่อ entry เมื่อเทียบกับขนาดแปรผัน ซึ่งรวมกันเป็นจำนวนมากสำหรับ chunk ขนาดใหญ่ที่มีข้อมูลขนาดสม่ำเสมอ (เช่น hash commitment ขนาด 32 ไบต์) สำหรับ 1024 entry ขนาด 32 ไบต์แต่ละตัว:
- ขนาดคงที่: `1 + 4 + 4 + 32768 = 32,777 ไบต์`
- ขนาดแปรผัน: `1 + 1024 × (4 + 32) = 36,865 ไบต์`
- ประหยัด: ~11%

## Storage Key Layout

ข้อมูล BulkAppendTree ทั้งหมดอยู่ใน **data** namespace โดย key มี prefix อักขระเดียว:

| รูปแบบ Key | Format | ขนาด | วัตถุประสงค์ |
|---|---|---|---|
| `M` | 1 ไบต์ | 1B | Metadata key |
| `b` + `{index}` | `b` + u32 BE | 5B | Buffer entry ที่ index |
| `e` + `{index}` | `e` + u64 BE | 9B | Chunk blob ที่ index |
| `m` + `{pos}` | `m` + u64 BE | 9B | MMR node ที่ตำแหน่ง |

**Metadata** จัดเก็บ `mmr_size` (8 ไบต์ BE) `total_count` และ `chunk_power` จัดเก็บใน Element เอง (ใน parent Merk) ไม่ใช่ใน metadata ของ data namespace การแยกนี้หมายความว่าการอ่าน count เป็นเพียง element lookup ง่าย ๆ โดยไม่ต้องเปิด data storage context

Buffer key ใช้ index แบบ u32 (0 ถึง `chunk_size - 1`) เพราะความจุ buffer ถูกจำกัดโดย `chunk_size` (u32, คำนวณเป็น `1u32 << chunk_power`) Chunk key ใช้ index แบบ u64 เพราะจำนวน chunk ที่เสร็จสมบูรณ์สามารถเติบโตได้ไม่จำกัด

## BulkAppendTree Struct

```rust
pub struct BulkAppendTree<S> {
    pub total_count: u64,                        // จำนวนค่าทั้งหมดที่เคย append
    pub dense_tree: DenseFixedSizedMerkleTree<S>, // buffer (dense tree)
}
```

buffer คือ `DenseFixedSizedMerkleTree` — root hash ของมันคือ `dense_tree_root`

**Accessor:**
- `capacity() -> u16`: `dense_tree.capacity()` (= `2^height - 1`)
- `epoch_size() -> u64`: `capacity + 1` (= `2^height`, จำนวน entry ต่อ chunk)
- `height() -> u8`: `dense_tree.height()`

**ค่าที่คำนวณได้** (ไม่ได้จัดเก็บ):

| ค่า | สูตร |
|---|---|
| `chunk_count` | `total_count / epoch_size` |
| `buffer_count` | `dense_tree.count()` |
| `mmr_size` | `leaf_count_to_mmr_size(chunk_count)` |

## การดำเนินการ GroveDB

BulkAppendTree รวมเข้ากับ GroveDB ผ่านหกการดำเนินการที่กำหนดใน `grovedb/src/operations/bulk_append_tree.rs`:

### bulk_append

การดำเนินการเปลี่ยนแปลงหลัก เป็นไปตามรูปแบบ non-Merk storage มาตรฐานของ GroveDB:

```text
1. Validate element is BulkAppendTree
2. Open data storage context
3. Load tree from store
4. Append value (may trigger compaction)
5. Update element in parent Merk with new state_root + total_count
6. Propagate changes up through Merk hierarchy
7. Commit transaction
```

adapter `AuxBulkStore` ห่อ `get_aux`/`put_aux`/`delete_aux` call ของ GroveDB และสะสม `OperationCost` ใน `RefCell` สำหรับการติดตามต้นทุน hash cost จากการดำเนินการ append จะถูกเพิ่มเข้า `cost.hash_node_calls`

### การดำเนินการอ่าน

| การดำเนินการ | สิ่งที่ส่งคืน | ใช้ Aux storage? |
|---|---|---|
| `bulk_get_value(path, key, position)` | ค่าที่ตำแหน่ง global | ใช่ — อ่านจาก chunk blob หรือ buffer |
| `bulk_get_chunk(path, key, chunk_index)` | chunk blob ดิบ | ใช่ — อ่าน chunk key |
| `bulk_get_buffer(path, key)` | entry ทั้งหมดใน buffer ปัจจุบัน | ใช่ — อ่าน buffer key |
| `bulk_count(path, key)` | จำนวนรวม (u64) | ไม่ — อ่านจาก element |
| `bulk_chunk_count(path, key)` | chunk ที่เสร็จสมบูรณ์ (u64) | ไม่ — คำนวณจาก element |

การดำเนินการ `get_value` เลือกเส้นทางโดยอัตโนมัติตามตำแหน่ง:

```text
if position < completed_chunks × chunk_size:
    chunk_idx = position / chunk_size
    intra_idx = position % chunk_size
    → read chunk blob, deserialize, return entry[intra_idx]
else:
    buffer_idx = position % chunk_size
    → read buffer_key(buffer_idx)
```

## การดำเนินการแบบ Batch และ Preprocessing

BulkAppendTree รองรับ batch operation ผ่าน variant `GroveOp::BulkAppend` เนื่องจาก `execute_ops_on_path` ไม่สามารถเข้าถึง data storage context ทุก BulkAppend op จึงต้องถูก preprocess ก่อน `apply_body`

pipeline ของ preprocessing:

```text
Input: [BulkAppend{v1}, Insert{...}, BulkAppend{v2}, BulkAppend{v3}]
                                     ↑ same (path,key) as v1

Step 1: Group BulkAppend ops by (path, key)
        group_1: [v1, v2, v3]

Step 2: For each group:
        a. Read existing element → get (total_count, chunk_power)
        b. Open transactional storage context
        c. Load BulkAppendTree from store
        d. Load existing buffer into memory (Vec<Vec<u8>>)
        e. For each value:
           tree.append_with_mem_buffer(store, value, &mut mem_buffer)
        f. Save metadata
        g. Compute final state_root

Step 3: Replace all BulkAppend ops with one ReplaceNonMerkTreeRoot per group
        carrying: hash=state_root, meta=BulkAppendTree{total_count, chunk_power}

Output: [ReplaceNonMerkTreeRoot{...}, Insert{...}]
```

variant `append_with_mem_buffer` หลีกเลี่ยงปัญหา read-after-write: buffer entry ถูกติดตามใน `Vec<Vec<u8>>` ในหน่วยความจำ ดังนั้น compaction สามารถอ่านพวกมันได้แม้ว่า transactional storage ยังไม่ได้ commit

## BulkStore Trait

```rust
pub trait BulkStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String>;
    fn delete(&self, key: &[u8]) -> Result<(), String>;
}
```

method รับ `&self` (ไม่ใช่ `&mut self`) เพื่อให้ตรงกับรูปแบบ interior mutability ของ GroveDB ที่การเขียนผ่าน batch การรวมเข้ากับ GroveDB ทำผ่าน `AuxBulkStore` ซึ่งห่อ `StorageContext` และสะสม `OperationCost`

`MmrAdapter` เชื่อม `BulkStore` กับ trait `MMRStoreReadOps`/`MMRStoreWriteOps` ของ ckb MMR โดยเพิ่ม write-through cache เพื่อความถูกต้องของ read-after-write

## การสร้าง Proof

BulkAppendTree proof รองรับ **range query** ข้ามตำแหน่ง โครงสร้าง proof จับทุกสิ่งที่จำเป็นสำหรับ stateless verifier เพื่อยืนยันว่าข้อมูลเฉพาะมีอยู่ในต้นไม้:

```rust
pub struct BulkAppendTreeProof {
    pub chunk_power: u8,
    pub total_count: u64,
    pub chunk_blobs: Vec<(u64, Vec<u8>)>,         // chunk blob เต็ม
    pub chunk_mmr_size: u64,
    pub chunk_mmr_proof_items: Vec<[u8; 32]>,      // MMR sibling hash
    pub chunk_mmr_leaves: Vec<(u64, [u8; 32])>,    // (leaf_idx, dense_root)
    pub buffer_entries: Vec<Vec<u8>>,               // buffer entry ทั้งหมด
    pub chunk_mmr_root: [u8; 32],
}
```

**ขั้นตอนการสร้าง** สำหรับช่วง `[start, end)` (โดยที่ `chunk_size = 1u32 << chunk_power`):

```text
1. Determine overlapping chunks
   first_chunk = start / chunk_size
   last_chunk  = min((end-1) / chunk_size, completed_chunks - 1)

2. Read chunk blobs for overlapping chunks
   For each chunk_idx in [first_chunk, last_chunk]:
     chunk_blobs.push((chunk_idx, store.get(chunk_key(idx))))

3. Compute dense Merkle root for each chunk blob
   For each blob:
     deserialize → values
     dense_root = compute_dense_merkle_root_from_values(values)
     chunk_mmr_leaves.push((chunk_idx, dense_root))

4. Generate MMR proof for those chunk positions
   positions = chunk_indices.map(|idx| leaf_to_pos(idx))
   proof = mmr.gen_proof(positions)
   chunk_mmr_proof_items = proof.proof_items().map(|n| n.hash)

5. Get chunk MMR root

6. Read ALL buffer entries (bounded by chunk_size)
   for i in 0..buffer_count:
     buffer_entries.push(store.get(buffer_key(i)))
```

**ทำไมต้องรวม buffer entry ทั้งหมด?** buffer คือ dense Merkle tree ที่ root hash commit ต่อทุก entry ผู้ตรวจสอบต้องสร้างต้นไม้ใหม่จาก entry ทั้งหมดเพื่อตรวจสอบ `dense_tree_root` เนื่องจาก buffer ถูกจำกัดโดย `capacity` (สูงสุด 65,535 entry) นี่เป็นต้นทุนที่สมเหตุสมผล

## การตรวจสอบ Proof

การตรวจสอบเป็น pure function — ไม่จำเป็นต้องเข้าถึงฐานข้อมูล ดำเนินการตรวจสอบห้าอย่าง:

```text
Step 0: Metadata consistency
        - chunk_power <= 31
        - buffer_entries.len() == total_count % chunk_size
        - MMR leaf count == completed_chunks

Step 1: Chunk blob integrity
        For each (chunk_idx, blob):
          values = deserialize(blob)
          computed_root = dense_merkle_root(values)
          assert computed_root == chunk_mmr_leaves[chunk_idx]

Step 2: Chunk MMR proof
        Reconstruct MmrNode leaves and proof items
        proof.verify(chunk_mmr_root, leaves) == true

Step 3: Buffer (dense tree) integrity
        Rebuild DenseFixedSizedMerkleTree from buffer_entries
        dense_tree_root = compute root hash of rebuilt tree

Step 4: State root
        computed = blake3("bulk_state" || chunk_mmr_root || dense_tree_root)
        assert computed == expected_state_root
```

หลังจากการตรวจสอบสำเร็จ `BulkAppendTreeProofResult` มี method `values_in_range(start, end)` ที่ดึงค่าเฉพาะจาก chunk blob และ buffer entry ที่ตรวจสอบแล้ว

## ความสัมพันธ์กับ GroveDB Root Hash

BulkAppendTree เป็น **ต้นไม้ non-Merk** — จัดเก็บข้อมูลใน data namespace ไม่ใช่ใน child Merk subtree ใน parent Merk, element ถูกจัดเก็บเป็น:

```text
Element::BulkAppendTree(total_count, chunk_power, flags)
```

state root ไหลเป็น Merk child hash hash ของ parent Merk node คือ:

```text
combine_hash(value_hash(element_bytes), state_root)
```

`state_root` ไหลเป็น Merk child hash (ผ่านพารามิเตอร์ `subtree_root_hash` ของ `insert_subtree`) การเปลี่ยนแปลงใด ๆ ของ state root จะเผยแพร่ขึ้นผ่าน GroveDB Merk hierarchy ไปจนถึง root hash

ใน V1 proof (9.6), parent Merk proof พิสูจน์ element bytes และ child hash binding, และ `BulkAppendTreeProof` พิสูจน์ว่าข้อมูลที่ query สอดคล้องกับ `state_root` ที่ใช้เป็น child hash

## การติดตามต้นทุน

ต้นทุนแฮชของแต่ละการดำเนินการถูกติดตามอย่างชัดเจน:

| การดำเนินการ | จำนวน Blake3 call | หมายเหตุ |
|---|---|---|
| Append เดียว (ไม่มี compaction) | 3 | 2 สำหรับ buffer hash chain + 1 สำหรับ state root |
| Append เดียว (มี compaction) | 3 + 2C - 1 + ~2 | Chain + dense Merkle (C=chunk_size) + MMR push + state root |
| `get_value` จาก chunk | 0 | Deserialization ล้วน ไม่มีการแฮช |
| `get_value` จาก buffer | 0 | ค้นหา key โดยตรง |
| การสร้าง Proof | ขึ้นกับจำนวน chunk | Dense Merkle root ต่อ chunk + MMR proof |
| การตรวจสอบ Proof | 2C·K - K + B·2 + 1 | K chunk, B buffer entry, C chunk_size |

**ต้นทุน amortized ต่อ append**: สำหรับ chunk_size=1024 (chunk_power=10), overhead ของ compaction ~2047 hash (dense Merkle root) ถูก amortize ข้าม 1024 append เพิ่ม ~2 hash ต่อ append เมื่อรวมกับ 3 hash ต่อ append ยอด amortized รวมคือ **~5 blake3 call ต่อ append** — มีประสิทธิภาพมากสำหรับโครงสร้างที่ authenticate ด้วย cryptography

## การเปรียบเทียบกับ MmrTree

| | BulkAppendTree | MmrTree |
|---|---|---|
| **สถาปัตยกรรม** | สองระดับ (buffer + chunk MMR) | MMR เดียว |
| **ต้นทุนแฮชต่อ append** | 3 (+ amortized ~2 สำหรับ compaction) | ~2 |
| **ความละเอียดของ Proof** | Range query ข้ามตำแหน่ง | Proof ของ leaf แต่ละตัว |
| **Immutable snapshot** | ใช่ (chunk blob) | ไม่ |
| **เป็นมิตรกับ CDN** | ใช่ (chunk blob cacheable) | ไม่ |
| **Buffer entry** | ใช่ (ต้องทั้งหมดสำหรับ proof) | N/A |
| **เหมาะสำหรับ** | ล็อกปริมาณมาก, bulk sync | ล็อกเหตุการณ์, ค้นหาทีละรายการ |
| **Element discriminant** | 13 | 12 |
| **TreeType** | 9 | 8 |

เลือก MmrTree เมื่อคุณต้องการ proof ของ leaf แต่ละตัวด้วย overhead น้อยที่สุด เลือก BulkAppendTree เมื่อคุณต้องการ range query, bulk synchronization และ snapshot แบบ chunk

## ไฟล์ Implementation

| ไฟล์ | วัตถุประสงค์ |
|------|---------|
| `grovedb-bulk-append-tree/src/lib.rs` | Crate root, re-export |
| `grovedb-bulk-append-tree/src/tree/mod.rs` | `BulkAppendTree` struct, state accessor, metadata persistence |
| `grovedb-bulk-append-tree/src/tree/append.rs` | `append()`, `append_with_mem_buffer()`, `compact_entries()` |
| `grovedb-bulk-append-tree/src/tree/hash.rs` | `compute_state_root` |
| `grovedb-bulk-append-tree/src/tree/keys.rs` | `META_KEY`, `buffer_key`, `chunk_key`, `mmr_node_key` |
| `grovedb-bulk-append-tree/src/tree/query.rs` | `get_value`, `get_chunk`, `get_buffer` |
| `grovedb-bulk-append-tree/src/tree/mmr_adapter.rs` | `MmrAdapter` ด้วย write-through cache |
| `grovedb-bulk-append-tree/src/chunk.rs` | Chunk blob serialization (fixed + variable format) |
| `grovedb-bulk-append-tree/src/proof.rs` | `BulkAppendTreeProof` generation and verification |
| `grovedb-bulk-append-tree/src/store.rs` | `BulkStore` trait |
| `grovedb-bulk-append-tree/src/error.rs` | `BulkAppendError` enum |
| `grovedb/src/operations/bulk_append_tree.rs` | GroveDB operations, `AuxBulkStore`, batch preprocessing |
| `grovedb/src/tests/bulk_append_tree_tests.rs` | 27 integration test |

---
