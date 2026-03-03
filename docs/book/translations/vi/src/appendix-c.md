# Phụ lục C: Tham chiếu tính toán hash

## Hash nút Merk

```text
node_hash = blake3(key_len(1) || key || value_hash(32) || left_hash(32) || right_hash(32))
```

## Tiền tố cây con GroveDB

```text
prefix = blake3(parent_prefix || key) → 32 byte
```

## State Root -- BulkAppendTree

```text
state_root = blake3("bulk_state" || mmr_root || dense_tree_root)
```

Buffer (bộ đệm) là một DenseFixedSizedMerkleTree -- `dense_tree_root` là root hash của nó.

## Dense Merkle Root -- Chunk của BulkAppendTree

```text
leaves[i] = blake3(entry[i])
internal[parent] = blake3(left_child || right_child)
root = internal[0] (đỉnh cây nhị phân hoàn chỉnh)
```

## Root Hash cây dày đặc -- DenseAppendOnlyFixedSizeTree

```text
Đệ quy từ gốc (vị trí 0):
  tất cả nút có dữ liệu: blake3(blake3(value) || H(left) || H(right))
                          (không có thẻ phân tách miền -- cấu trúc được xác thực bên ngoài)
  nút lá:                 left_hash = right_hash = [0; 32]
  vị trí chưa điền:       [0; 32]
  cây rỗng:               [0; 32]
```

## Xác minh chứng minh cây dày đặc -- DenseAppendOnlyFixedSizeTree

```text
Cho: entries, node_value_hashes, node_hashes, expected_root

Kiểm tra trước:
  1. Xác thực height trong [1, 16]
  2. Xác thực count <= capacity (= 2^height - 1)
  3. Từ chối nếu bất kỳ trường nào có > 100.000 phần tử (phòng chống DoS)
  4. Từ chối vị trí trùng lặp trong entries, node_value_hashes, node_hashes
  5. Từ chối vị trí chồng chéo giữa ba trường
  6. Từ chối node_hashes tại tổ tiên của bất kỳ entry được chứng minh (ngăn giả mạo)

recompute_hash(position):
  if position >= capacity or position >= count → [0; 32]
  if position in node_hashes → trả về hash đã tính trước
  value_hash = blake3(entries[position]) or node_value_hashes[position]
  left_hash  = recompute_hash(2*pos+1)
  right_hash = recompute_hash(2*pos+2)
  return blake3(value_hash || left_hash || right_hash)

Xác minh: recompute_hash(0) == expected_root

Xác thực chéo GroveDB:
  proof.height phải khớp element.height
  proof.count phải khớp element.count
```

## Merge nút MMR -- MmrTree / Chunk MMR của BulkAppendTree

```text
parent.hash = blake3(left.hash || right.hash)
```

## Sinsemilla Root -- CommitmentTree

```text
Hash Sinsemilla trên đường cong Pallas (xem Zcash ZIP-244)
MerkleHashOrchard::empty_root(Level::from(32)) cho cây rỗng
```

## combine_hash (Ràng buộc cha-con)

```text
combine_hash(value_hash, child_hash) = blake3(value_hash || child_hash)
Cho cây Merk: child_hash = root hash Merk con
Cho cây không phải Merk: child_hash = root đặc trưng theo kiểu (mmr_root, state_root, dense_root, v.v.)
```

---

*Sách GroveDB -- tài liệu nội bộ của GroveDB cho lập trình viên và nhà nghiên cứu. Dựa trên mã nguồn GroveDB.*
