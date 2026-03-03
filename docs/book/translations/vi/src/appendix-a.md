# Phụ lục A: Tham chiếu đầy đủ các kiểu Element

| Discriminant | Biến thể | TreeType | Trường | Kích thước chi phí | Mục đích |
|---|---|---|---|---|---|
| 0 | `Item` | N/A | `(value, flags)` | thay đổi | Lưu trữ khóa-giá trị cơ bản |
| 1 | `Reference` | N/A | `(path, max_hop, flags)` | thay đổi | Liên kết giữa các phần tử |
| 2 | `Tree` | 0 (NormalTree) | `(root_key, flags)` | TREE_COST_SIZE | Vùng chứa cho cây con |
| 3 | `SumItem` | N/A | `(value, flags)` | thay đổi | Đóng góp vào tổng cha |
| 4 | `SumTree` | 1 (SumTree) | `(root_key, sum, flags)` | SUM_TREE_COST_SIZE | Duy trì tổng các con |
| 5 | `BigSumTree` | 4 (BigSumTree) | `(root_key, sum128, flags)` | BIG_SUM_TREE_COST_SIZE | Cây tổng 128-bit |
| 6 | `CountTree` | 2 (CountTree) | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Cây đếm phần tử |
| 7 | `CountSumTree` | 3 (CountSumTree) | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Kết hợp đếm + tổng |
| 8 | `ItemWithSumItem` | N/A | `(value, sum, flags)` | thay đổi | Item với đóng góp tổng |
| 9 | `ProvableCountTree` | 5 | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Cây đếm có thể chứng minh |
| 10 | `ProvableCountSumTree` | 6 | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Đếm + tổng có thể chứng minh |
| 11 | `CommitmentTree` | 7 | `(total_count: u64, chunk_power: u8, flags)` | 12 | Sinsemilla thân thiện ZK + BulkAppendTree |
| 12 | `MmrTree` | 8 | `(mmr_size: u64, flags)` | 11 | Nhật ký MMR chỉ thêm |
| 13 | `BulkAppendTree` | 9 | `(total_count: u64, chunk_power: u8, flags)` | 12 | Nhật ký chỉ thêm thông lượng cao |
| 14 | `DenseAppendOnlyFixedSizeTree` | 10 | `(count: u16, height: u8, flags)` | 6 | Lưu trữ Merkle dày đặc dung lượng cố định |

**Ghi chú:**
- Discriminant 11-14 là **cây không phải Merk**: dữ liệu nằm ngoài cây con Merk
  - Cả bốn đều lưu trữ dữ liệu không phải Merk trong cột **data**
  - `CommitmentTree` lưu trữ frontier Sinsemilla cùng với các mục BulkAppendTree trong cùng cột data (khóa `b"__ct_data__"`)
- Cây không phải Merk KHÔNG có trường `root_key` -- root hash đặc trưng theo kiểu của chúng chảy như child hash của Merk qua `insert_subtree`
- `CommitmentTree` sử dụng hash Sinsemilla (đường cong Pallas); tất cả các kiểu khác sử dụng Blake3
- Hành vi chi phí cho cây không phải Merk tuân theo `NormalTree` (BasicMerkNode, không tổng hợp)
- `DenseAppendOnlyFixedSizeTree` count là `u16` (tối đa 65.535); chiều cao giới hạn trong 1..=16

---
