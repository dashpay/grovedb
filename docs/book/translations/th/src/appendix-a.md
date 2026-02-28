# ภาคผนวก ก: ข้อมูลอ้างอิงประเภท Element ทั้งหมด

| Discriminant | Variant | TreeType | ฟิลด์ | ขนาดต้นทุน | วัตถุประสงค์ |
|---|---|---|---|---|---|
| 0 | `Item` | N/A | `(value, flags)` | แตกต่างกัน | จัดเก็บ key-value พื้นฐาน |
| 1 | `Reference` | N/A | `(path, max_hop, flags)` | แตกต่างกัน | ลิงก์ระหว่าง element |
| 2 | `Tree` | 0 (NormalTree) | `(root_key, flags)` | TREE_COST_SIZE | container สำหรับ subtree |
| 3 | `SumItem` | N/A | `(value, flags)` | แตกต่างกัน | มีส่วนในผลรวมของ parent |
| 4 | `SumTree` | 1 (SumTree) | `(root_key, sum, flags)` | SUM_TREE_COST_SIZE | รักษาผลรวมของ descendant |
| 5 | `BigSumTree` | 4 (BigSumTree) | `(root_key, sum128, flags)` | BIG_SUM_TREE_COST_SIZE | sum tree 128 บิต |
| 6 | `CountTree` | 2 (CountTree) | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | ต้นไม้นับจำนวน element |
| 7 | `CountSumTree` | 3 (CountSumTree) | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | นับจำนวน + ผลรวมรวมกัน |
| 8 | `ItemWithSumItem` | N/A | `(value, sum, flags)` | แตกต่างกัน | Item ที่มีส่วนในผลรวม |
| 9 | `ProvableCountTree` | 5 | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | ต้นไม้นับจำนวนที่พิสูจน์ได้ |
| 10 | `ProvableCountSumTree` | 6 | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | นับจำนวน + ผลรวมที่พิสูจน์ได้ |
| 11 | `CommitmentTree` | 7 | `(total_count: u64, chunk_power: u8, flags)` | 12 | Sinsemilla + BulkAppendTree ที่เป็นมิตรกับ ZK |
| 12 | `MmrTree` | 8 | `(mmr_size: u64, flags)` | 11 | ล็อก MMR แบบ append-only |
| 13 | `BulkAppendTree` | 9 | `(total_count: u64, chunk_power: u8, flags)` | 12 | ล็อก append-only ปริมาณมาก |
| 14 | `DenseAppendOnlyFixedSizeTree` | 10 | `(count: u16, height: u8, flags)` | 6 | ที่จัดเก็บ Merkle แบบ dense ขนาดคงที่ |

**หมายเหตุ:**
- Discriminant 11-14 เป็น **ต้นไม้ non-Merk**: ข้อมูลอยู่นอก child Merk subtree
  - ทั้งสี่ประเภทจัดเก็บข้อมูล non-Merk ใน **data** column
  - `CommitmentTree` จัดเก็บ Sinsemilla frontier ร่วมกับ entry ของ BulkAppendTree ใน data column เดียวกัน (key `b"__ct_data__"`)
- ต้นไม้ non-Merk ไม่มีฟิลด์ `root_key` — root hash เฉพาะประเภทของพวกมันจะไหลเป็น Merk child hash ผ่าน `insert_subtree`
- `CommitmentTree` ใช้การแฮช Sinsemilla (Pallas curve); ประเภทอื่น ๆ ทั้งหมดใช้ Blake3
- พฤติกรรมต้นทุนสำหรับต้นไม้ non-Merk เป็นไปตาม `NormalTree` (BasicMerkNode, ไม่มี aggregation)
- `DenseAppendOnlyFixedSizeTree` count เป็น `u16` (สูงสุด 65,535); height จำกัดที่ 1..=16

---
