# ภาคผนวก ข: ข้อมูลอ้างอิงด่วนระบบ Proof

## V0 Proof (เฉพาะ Merk)

```text
สร้าง:
  1. เริ่มที่ root Merk, ดำเนินการ query → รวบรวม element ที่ตรงกัน
  2. สำหรับ element ชนิด tree ที่ตรงกันแต่ละตัวที่มี subquery:
     เปิด child Merk → พิสูจน์ subquery แบบ recursive
  3. Serialize เป็น GroveDBProof::V0(root_layer, Vec<(key, GroveDBProof)>)

ตรวจสอบ:
  1. ตรวจสอบ root Merk proof → root_hash, elements
  2. สำหรับแต่ละ sub-proof:
     ตรวจสอบ child Merk proof → child_hash
     ตรวจสอบ: combine_hash(value_hash(parent_element), child_hash) == ค่าที่คาดหวัง
  3. root_hash สุดท้ายต้องตรงกับ GroveDB root ที่ทราบ
```

## V1 Proof (Merk + Non-Merk ผสมกัน)

เมื่อ layer ใดก็ตามเกี่ยวข้องกับ CommitmentTree, MmrTree, BulkAppendTree หรือ DenseAppendOnlyFixedSizeTree จะสร้าง V1 proof:

```text
สร้าง:
  1. เหมือน V0 สำหรับ layer ที่เป็น Merk
  2. เมื่อลงไปใน CommitmentTree:
     → generate_commitment_tree_layer_proof(query_items) → ProofBytes::CommitmentTree(bytes)
     (sinsemilla_root (32 ไบต์) || BulkAppendTree proof bytes)
  3. เมื่อลงไปใน MmrTree:
     → generate_mmr_layer_proof(query_items) → ProofBytes::MMR(bytes)
  4. เมื่อลงไปใน BulkAppendTree:
     → generate_bulk_append_layer_proof(query_items) → ProofBytes::BulkAppendTree(bytes)
  5. เมื่อลงไปใน DenseAppendOnlyFixedSizeTree:
     → generate_dense_tree_layer_proof(query_items) → ProofBytes::DenseTree(bytes)
  6. จัดเก็บเป็น LayerProof { merk_proof, lower_layers }

ตรวจสอบ:
  1. เหมือน V0 สำหรับ layer ที่เป็น Merk
  2. สำหรับ ProofBytes::CommitmentTree: ดึง sinsemilla_root, ตรวจสอบ
     BulkAppendTree proof ภายใน, คำนวณ combined root hash ใหม่
  3. สำหรับ ProofBytes::MMR: ตรวจสอบ MMR proof กับ MMR root จาก parent child hash
  4. สำหรับ ProofBytes::BulkAppendTree: ตรวจสอบกับ state_root จาก parent child hash
  5. สำหรับ ProofBytes::DenseTree: ตรวจสอบกับ root_hash จาก parent child hash
     (คำนวณ root ใหม่จาก entry + ค่า ancestor + sibling hash)
  6. Root เฉพาะประเภทไหลเป็น Merk child hash (ไม่ใช่ NULL_HASH)
```

**การเลือก V0/V1**: ถ้าทุก layer เป็น Merk จะสร้าง V0 (เข้ากันได้กับเวอร์ชันเก่า) ถ้า layer ใดเป็นต้นไม้ non-Merk จะสร้าง V1

---
