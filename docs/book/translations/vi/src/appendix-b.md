# Phụ lục B: Tham chiếu nhanh hệ thống chứng minh

## Chứng minh V0 (chỉ Merk)

```text
Tạo:
  1. Bắt đầu tại Merk gốc, thực thi truy vấn → thu thập phần tử khớp
  2. Cho mỗi phần tử cây khớp có truy vấn con:
     Mở Merk con → chứng minh truy vấn con đệ quy
  3. Tuần tự hóa thành GroveDBProof::V0(root_layer, Vec<(key, GroveDBProof)>)

Xác minh:
  1. Xác minh chứng minh Merk gốc → root_hash, phần tử
  2. Cho mỗi chứng minh con:
     Xác minh chứng minh Merk con → child_hash
     Kiểm tra: combine_hash(value_hash(parent_element), child_hash) == mong đợi
  3. root_hash cuối cùng phải khớp với root GroveDB đã biết
```

## Chứng minh V1 (Merk kết hợp + không phải Merk)

Khi bất kỳ lớp nào liên quan đến CommitmentTree, MmrTree, BulkAppendTree, hoặc DenseAppendOnlyFixedSizeTree, chứng minh V1 được tạo:

```text
Tạo:
  1. Giống V0 cho các lớp Merk
  2. Khi đi sâu vào CommitmentTree:
     → generate_commitment_tree_layer_proof(query_items) → ProofBytes::CommitmentTree(bytes)
     (sinsemilla_root (32 byte) || byte chứng minh BulkAppendTree)
  3. Khi đi sâu vào MmrTree:
     → generate_mmr_layer_proof(query_items) → ProofBytes::MMR(bytes)
  4. Khi đi sâu vào BulkAppendTree:
     → generate_bulk_append_layer_proof(query_items) → ProofBytes::BulkAppendTree(bytes)
  5. Khi đi sâu vào DenseAppendOnlyFixedSizeTree:
     → generate_dense_tree_layer_proof(query_items) → ProofBytes::DenseTree(bytes)
  6. Lưu trữ dưới dạng LayerProof { merk_proof, lower_layers }

Xác minh:
  1. Giống V0 cho các lớp Merk
  2. Cho ProofBytes::CommitmentTree: trích xuất sinsemilla_root, xác minh chứng minh
     BulkAppendTree bên trong, tính lại root hash kết hợp
  3. Cho ProofBytes::MMR: xác minh chứng minh MMR so với MMR root từ child hash cha
  4. Cho ProofBytes::BulkAppendTree: xác minh so với state_root từ child hash cha
  5. Cho ProofBytes::DenseTree: xác minh so với root_hash từ child hash cha
     (tính lại root từ entries + giá trị tổ tiên + hash anh em)
  6. Root đặc trưng theo kiểu chảy như Merk child hash (không phải NULL_HASH)
```

**Lựa chọn V0/V1**: Nếu tất cả các lớp đều là Merk, tạo V0 (tương thích ngược). Nếu bất kỳ lớp nào là cây không phải Merk, tạo V1.

---
