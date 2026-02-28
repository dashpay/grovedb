# ภาคผนวก ค: ข้อมูลอ้างอิงการคำนวณแฮช

## Merk Node Hash

```text
node_hash = blake3(key_len(1) || key || value_hash(32) || left_hash(32) || right_hash(32))
```

## GroveDB Subtree Prefix

```text
prefix = blake3(parent_prefix || key) → 32 ไบต์
```

## State Root — BulkAppendTree

```text
state_root = blake3("bulk_state" || mmr_root || dense_tree_root)
```

buffer คือ DenseFixedSizedMerkleTree — `dense_tree_root` คือ root hash ของมัน

## Dense Merkle Root — BulkAppendTree Chunks

```text
leaves[i] = blake3(entry[i])
internal[parent] = blake3(left_child || right_child)
root = internal[0] (ยอดของ complete binary tree)
```

## Dense Tree Root Hash — DenseAppendOnlyFixedSizeTree

```text
Recursive จาก root (ตำแหน่ง 0):
  ทุกโหนดที่มีข้อมูล: blake3(blake3(value) || H(left) || H(right))
                       (ไม่มี domain separation tag — โครงสร้างถูก authenticate จากภายนอก)
  leaf node:          left_hash = right_hash = [0; 32]
  ตำแหน่งที่ยังไม่มีข้อมูล:   [0; 32]
  ต้นไม้ว่าง:          [0; 32]
```

## การตรวจสอบ Dense Tree Proof — DenseAppendOnlyFixedSizeTree

```text
กำหนดให้: entries, node_value_hashes, node_hashes, expected_root

การตรวจสอบเบื้องต้น:
  1. ตรวจสอบ height อยู่ใน [1, 16]
  2. ตรวจสอบ count <= capacity (= 2^height - 1)
  3. ปฏิเสธถ้าฟิลด์ใดมี > 100,000 element (ป้องกัน DoS)
  4. ปฏิเสธตำแหน่งซ้ำภายใน entries, node_value_hashes, node_hashes
  5. ปฏิเสธตำแหน่งที่ทับซ้อนกันระหว่างสามฟิลด์
  6. ปฏิเสธ node_hashes ที่ ancestor ของ entry ที่ถูกพิสูจน์ (ป้องกันการปลอม)

recompute_hash(position):
  if position >= capacity or position >= count → [0; 32]
  if position in node_hashes → return แฮชที่คำนวณไว้แล้ว
  value_hash = blake3(entries[position]) or node_value_hashes[position]
  left_hash  = recompute_hash(2*pos+1)
  right_hash = recompute_hash(2*pos+2)
  return blake3(value_hash || left_hash || right_hash)

ตรวจสอบ: recompute_hash(0) == expected_root

การตรวจสอบข้าม GroveDB:
  proof.height ต้องตรงกับ element.height
  proof.count ต้องตรงกับ element.count
```

## MMR Node Merge — MmrTree / BulkAppendTree Chunk MMR

```text
parent.hash = blake3(left.hash || right.hash)
```

## Sinsemilla Root — CommitmentTree

```text
แฮช Sinsemilla บน Pallas curve (ดู Zcash ZIP-244)
MerkleHashOrchard::empty_root(Level::from(32)) สำหรับต้นไม้ว่าง
```

## combine_hash (การผูก Parent-Child)

```text
combine_hash(value_hash, child_hash) = blake3(value_hash || child_hash)
สำหรับต้นไม้ Merk: child_hash = child Merk root hash
สำหรับต้นไม้ non-Merk: child_hash = root เฉพาะประเภท (mmr_root, state_root, dense_root เป็นต้น)
```

---

*หนังสือ GroveDB — จัดทำเอกสารเกี่ยวกับรายละเอียดภายในของ GroveDB สำหรับนักพัฒนาและนักวิจัย อ้างอิงจากซอร์สโค้ดของ GroveDB*

