# DenseAppendOnlyFixedSizeTree -- Lưu trữ Merkle dày đặc dung lượng cố định

DenseAppendOnlyFixedSizeTree là một cây nhị phân hoàn chỉnh có chiều cao cố định, trong đó **mọi nút** -- cả nút bên trong lẫn nút lá -- đều lưu trữ một giá trị dữ liệu. Các vị trí được điền tuần tự theo thứ tự cấp (BFS): gốc trước (vị trí 0), sau đó từ trái sang phải ở mỗi cấp. Không có hash trung gian nào được lưu trữ; root hash được tính lại ngay lập tức bằng cách hash đệ quy từ lá lên gốc.

Thiết kế này lý tưởng cho các cấu trúc dữ liệu nhỏ, có giới hạn, nơi dung lượng tối đa được biết trước và bạn cần O(1) thêm vào, O(1) truy xuất theo vị trí, và một cam kết root hash 32 byte nhỏ gọn thay đổi sau mỗi lần chèn.

## Cấu trúc cây

Một cây có chiều cao *h* có dung lượng `2^h - 1` vị trí. Vị trí sử dụng chỉ mục thứ tự cấp bắt đầu từ 0:

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

Các giá trị được thêm tuần tự: giá trị đầu tiên vào vị trí 0 (gốc), sau đó vị trí 1, 2, 3, v.v. Điều này có nghĩa là gốc luôn có dữ liệu, và cây được điền theo thứ tự cấp -- thứ tự duyệt tự nhiên nhất cho cây nhị phân hoàn chỉnh.

## Tính toán hash

Root hash không được lưu trữ riêng -- nó được tính lại từ đầu mỗi khi cần. Thuật toán đệ quy chỉ duyệt qua các vị trí đã được điền:

```text
hash(position, store):
  value = store.get(position)

  if position is unfilled (>= count):
    return [0; 32]                                    ← sentinel rỗng

  value_hash = blake3(value)
  left_hash  = hash(2 * position + 1, store)
  right_hash = hash(2 * position + 2, store)
  return blake3(value_hash || left_hash || right_hash)
```

**Thuộc tính chính:**
- Tất cả nút (lá và bên trong): `blake3(blake3(value) || H(left) || H(right))`
- Nút lá: left_hash và right_hash đều là `[0; 32]` (con chưa điền)
- Vị trí chưa điền: `[0u8; 32]` (hash zero)
- Cây rỗng (count = 0): `[0u8; 32]`

**Không sử dụng thẻ phân tách miền lá/nút bên trong.** Cấu trúc cây (`height` và `count`) được xác thực bên ngoài trong `Element::DenseAppendOnlyFixedSizeTree` cha, phần tử này chảy qua phân cấp Merk. Trình xác minh luôn biết chính xác vị trí nào là lá so với nút bên trong từ height và count, vì vậy kẻ tấn công không thể thay thế cái này bằng cái kia mà không phá vỡ chuỗi xác thực cha.

Điều này có nghĩa root hash mã hóa một cam kết đến mọi giá trị được lưu trữ và vị trí chính xác của nó trong cây. Thay đổi bất kỳ giá trị nào (nếu có thể thay đổi) sẽ lan truyền qua tất cả hash tổ tiên lên đến gốc.

**Chi phí hash:** Tính toán root hash duyệt tất cả vị trí đã điền cộng thêm các con chưa điền. Cho cây có *n* giá trị, trường hợp xấu nhất là O(*n*) lần gọi blake3. Điều này chấp nhận được vì cây được thiết kế cho dung lượng nhỏ, giới hạn (chiều cao tối đa 16, tối đa 65.535 vị trí).

## Biến thể Element

```rust
Element::DenseAppendOnlyFixedSizeTree(
    u16,                   // count — số giá trị đã lưu (tối đa 65.535)
    u8,                    // height — bất biến sau khi tạo (1..=16)
    Option<ElementFlags>,  // flags — cờ lưu trữ
)
```

| Trường | Kiểu | Mô tả |
|---|---|---|
| `count` | `u16` | Số giá trị đã chèn (tối đa 65.535) |
| `height` | `u8` | Chiều cao cây (1..=16), bất biến sau khi tạo |
| `flags` | `Option<ElementFlags>` | Cờ lưu trữ tùy chọn |

Root hash KHÔNG được lưu trong Element -- nó chảy như Merk child hash qua tham số `subtree_root_hash` của `insert_subtree`.

**Discriminant:** 14 (ElementType), TreeType = 10

**Kích thước chi phí:** `DENSE_TREE_COST_SIZE = 6` byte (2 count + 1 height + 1 discriminant + 2 overhead)

## Bố cục lưu trữ

Giống như MmrTree và BulkAppendTree, DenseAppendOnlyFixedSizeTree lưu dữ liệu trong không gian tên **data** (không phải Merk con). Giá trị được đánh khóa theo vị trí dưới dạng `u64` big-endian:

```text
Subtree path: blake3(parent_path || key)

Storage keys:
  [0, 0, 0, 0, 0, 0, 0, 0] → value at position 0 (root)
  [0, 0, 0, 0, 0, 0, 0, 1] → value at position 1
  [0, 0, 0, 0, 0, 0, 0, 2] → value at position 2
  ...
```

Bản thân Element (lưu trong Merk cha) mang `count` và `height`. Root hash chảy như Merk child hash. Điều này có nghĩa:
- **Đọc root hash** yêu cầu tính toán lại từ lưu trữ (O(n) hash)
- **Đọc giá trị theo vị trí là O(1)** -- tra cứu lưu trữ đơn lẻ
- **Chèn là O(n) hash** -- một lần ghi lưu trữ + tính lại toàn bộ root hash

## Các thao tác

### `dense_tree_insert(path, key, value, tx, grove_version)`

Thêm một giá trị vào vị trí khả dụng tiếp theo. Trả về `(root_hash, position)`.

```text
Bước 1: Đọc element, trích xuất (count, height)
Bước 2: Kiểm tra dung lượng: nếu count >= 2^height - 1 → lỗi
Bước 3: Xây dựng đường dẫn cây con, mở ngữ cảnh lưu trữ
Bước 4: Ghi giá trị vào position = count
Bước 5: Tái tạo DenseFixedSizedMerkleTree từ trạng thái
Bước 6: Gọi tree.insert(value, store) → (root_hash, position, hash_calls)
Bước 7: Cập nhật element với root_hash mới và count + 1
Bước 8: Lan truyền thay đổi lên qua phân cấp Merk
Bước 9: Commit giao dịch
```

### `dense_tree_get(path, key, position, tx, grove_version)`

Truy xuất giá trị tại vị trí cho trước. Trả về `None` nếu position >= count.

### `dense_tree_root_hash(path, key, tx, grove_version)`

Trả về root hash được lưu trong element. Đây là hash được tính trong lần chèn gần nhất -- không cần tính lại.

### `dense_tree_count(path, key, tx, grove_version)`

Trả về số giá trị đã lưu (trường `count` từ element).

## Thao tác theo lô

Biến thể `GroveOp::DenseTreeInsert` hỗ trợ chèn theo lô qua đường ống lô chuẩn của GroveDB:

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

**Tiền xử lý:** Giống tất cả kiểu cây không phải Merk, các thao tác `DenseTreeInsert` được tiền xử lý trước khi thân lô chính thực thi. Phương thức `preprocess_dense_tree_ops`:

1. Nhóm tất cả thao tác `DenseTreeInsert` theo `(path, key)`
2. Cho mỗi nhóm, thực thi các chèn tuần tự (đọc element, chèn mỗi giá trị, cập nhật root hash)
3. Chuyển đổi mỗi nhóm thành thao tác `ReplaceNonMerkTreeRoot` mang `root_hash` cuối cùng và `count` qua máy xử lý lan truyền chuẩn

Nhiều lần chèn vào cùng cây dày đặc trong một lô duy nhất được hỗ trợ -- chúng được xử lý theo thứ tự và kiểm tra tính nhất quán cho phép khóa trùng lặp cho kiểu thao tác này.

**Lan truyền:** Root hash và count chảy qua biến thể `NonMerkTreeMeta::DenseTree` trong `ReplaceNonMerkTreeRoot`, theo cùng mẫu như MmrTree và BulkAppendTree.

## Chứng minh

DenseAppendOnlyFixedSizeTree hỗ trợ **chứng minh truy vấn con V1** qua biến thể `ProofBytes::DenseTree`. Các vị trí riêng lẻ có thể được chứng minh so với root hash của cây bằng chứng minh bao gồm (inclusion proof) mang giá trị tổ tiên và hash cây con anh em.

### Cấu trúc đường dẫn xác thực (Auth Path)

Vì các nút bên trong hash **giá trị riêng của chúng** (không chỉ hash con), đường dẫn xác thực khác với cây Merkle tiêu chuẩn. Để xác minh một lá tại vị trí `p`, trình xác minh cần:

1. **Giá trị lá** (mục được chứng minh)
2. **Hash giá trị tổ tiên** cho mọi nút bên trong trên đường dẫn từ `p` đến gốc (chỉ hash 32 byte, không phải giá trị đầy đủ)
3. **Hash cây con anh em** cho mọi con KHÔNG nằm trên đường dẫn

Vì tất cả nút sử dụng `blake3(H(value) || H(left) || H(right))` (không có thẻ miền), chứng minh chỉ mang hash giá trị 32 byte cho tổ tiên -- không phải giá trị đầy đủ. Điều này giữ cho chứng minh nhỏ gọn bất kể giá trị riêng lẻ lớn đến đâu.

```rust
pub struct DenseTreeProof {
    pub entries: Vec<(u16, Vec<u8>)>,            // cặp (vị trí, giá trị) được chứng minh
    pub node_value_hashes: Vec<(u16, [u8; 32])>, // hash giá trị tổ tiên trên đường xác thực
    pub node_hashes: Vec<(u16, [u8; 32])>,       // hash cây con anh em đã tính trước
}
```

> **Lưu ý:** `height` và `count` không có trong struct chứng minh -- trình xác minh lấy chúng từ Element cha, được xác thực bởi phân cấp Merk.

### Ví dụ chi tiết

Cây với height=3, capacity=7, count=5, chứng minh vị trí 4:

```text
        0
       / \
      1   2
     / \ / \
    3  4 5  6
```

Đường dẫn từ 4 đến gốc: `4 → 1 → 0`. Tập mở rộng: `{0, 1, 4}`.

Chứng minh chứa:
- **entries**: `[(4, value[4])]` -- vị trí được chứng minh
- **node_value_hashes**: `[(0, H(value[0])), (1, H(value[1]))]` -- hash giá trị tổ tiên (mỗi cái 32 byte, không phải giá trị đầy đủ)
- **node_hashes**: `[(2, H(subtree_2)), (3, H(node_3))]` -- anh em không nằm trên đường dẫn

Xác minh tính lại root hash từ dưới lên:
1. `H(4) = blake3(blake3(value[4]) || [0;32] || [0;32])` -- lá (con chưa điền)
2. `H(3)` -- từ `node_hashes`
3. `H(1) = blake3(H(value[1]) || H(3) || H(4))` -- bên trong sử dụng hash giá trị từ `node_value_hashes`
4. `H(2)` -- từ `node_hashes`
5. `H(0) = blake3(H(value[0]) || H(1) || H(2))` -- gốc sử dụng hash giá trị từ `node_value_hashes`
6. So sánh `H(0)` với root hash mong đợi

### Chứng minh nhiều vị trí

Khi chứng minh nhiều vị trí, tập mở rộng hợp nhất các đường dẫn xác thực chồng chéo. Các tổ tiên chung chỉ được bao gồm một lần, làm cho chứng minh nhiều vị trí nhỏ gọn hơn so với các chứng minh đơn vị trí độc lập.

### Hạn chế V0

Chứng minh V0 không thể đi sâu vào cây dày đặc. Nếu truy vấn V0 khớp với `DenseAppendOnlyFixedSizeTree` có truy vấn con, hệ thống trả về `Error::NotSupported` hướng dẫn người gọi sử dụng `prove_query_v1`.

### Mã hóa khóa truy vấn

Vị trí cây dày đặc được mã hóa thành khóa truy vấn **big-endian u16** (2 byte), khác với MmrTree và BulkAppendTree sử dụng u64. Tất cả kiểu phạm vi `QueryItem` chuẩn đều được hỗ trợ.

## So sánh với các cây không phải Merk khác

| | DenseTree | MmrTree | BulkAppendTree | CommitmentTree |
|---|---|---|---|---|
| **Element discriminant** | 14 | 12 | 13 | 11 |
| **TreeType** | 10 | 8 | 9 | 7 |
| **Dung lượng** | Cố định (`2^h - 1`, tối đa 65.535) | Không giới hạn | Không giới hạn | Không giới hạn |
| **Mô hình dữ liệu** | Mọi vị trí lưu giá trị | Chỉ lá | Buffer cây dày đặc + chunk | Chỉ lá |
| **Hash trong Element?** | Không (chảy như child hash) | Không (chảy như child hash) | Không (chảy như child hash) | Không (chảy như child hash) |
| **Chi phí chèn (hash)** | O(n) blake3 | O(1) trung bình | O(1) trung bình | ~33 Sinsemilla |
| **Kích thước chi phí** | 6 byte | 11 byte | 12 byte | 12 byte |
| **Hỗ trợ chứng minh** | V1 (Dense) | V1 (MMR) | V1 (Bulk) | V1 (CommitmentTree) |
| **Phù hợp nhất cho** | Cấu trúc nhỏ có giới hạn | Nhật ký sự kiện | Nhật ký thông lượng cao | Cam kết ZK |

**Khi nào chọn DenseAppendOnlyFixedSizeTree:**
- Số lượng mục tối đa được biết tại thời điểm tạo
- Bạn cần mọi vị trí (bao gồm nút bên trong) để lưu dữ liệu
- Bạn muốn mô hình dữ liệu đơn giản nhất có thể mà không có tăng trưởng không giới hạn
- Tính lại root hash O(n) là chấp nhận được (chiều cao cây nhỏ)

**Khi KHÔNG nên chọn:**
- Bạn cần dung lượng không giới hạn -> sử dụng MmrTree hoặc BulkAppendTree
- Bạn cần tương thích ZK -> sử dụng CommitmentTree

## Ví dụ sử dụng

```rust
use grovedb::Element;
use grovedb_version::version::GroveVersion;

let grove_version = GroveVersion::latest();

// Tạo cây dày đặc chiều cao 4 (dung lượng = 15 giá trị)
db.insert(
    &[b"state"],
    b"validator_slots",
    Element::empty_dense_tree(4),
    None,
    None,
    grove_version,
)?;

// Thêm giá trị — vị trí được điền 0, 1, 2, ...
let (root_hash, pos) = db.dense_tree_insert(
    &[b"state"],
    b"validator_slots",
    validator_pubkey.to_vec(),
    None,
    grove_version,
)?;
// pos == 0, root_hash = blake3(validator_pubkey)

// Đọc lại theo vị trí
let value = db.dense_tree_get(
    &[b"state"],
    b"validator_slots",
    0,     // vị trí
    None,
    grove_version,
)?;
assert_eq!(value, Some(validator_pubkey.to_vec()));

// Truy vấn metadata
let count = db.dense_tree_count(&[b"state"], b"validator_slots", None, grove_version)?;
let hash = db.dense_tree_root_hash(&[b"state"], b"validator_slots", None, grove_version)?;
```

## Tệp triển khai

| Tệp | Nội dung |
|---|---|
| `grovedb-dense-fixed-sized-merkle-tree/src/lib.rs` | Trait `DenseTreeStore`, struct `DenseFixedSizedMerkleTree`, hash đệ quy |
| `grovedb-dense-fixed-sized-merkle-tree/src/proof.rs` | Struct `DenseTreeProof`, `generate()`, `encode_to_vec()`, `decode_from_slice()` |
| `grovedb-dense-fixed-sized-merkle-tree/src/verify.rs` | `DenseTreeProof::verify()` -- hàm thuần túy, không cần lưu trữ |
| `grovedb-element/src/element/mod.rs` | `Element::DenseAppendOnlyFixedSizeTree` (discriminant 14) |
| `grovedb-element/src/element/constructor.rs` | `empty_dense_tree()`, `new_dense_tree()` |
| `merk/src/tree_type/mod.rs` | `TreeType::DenseAppendOnlyFixedSizeTree = 10` |
| `merk/src/tree_type/costs.rs` | `DENSE_TREE_COST_SIZE = 6` |
| `grovedb/src/operations/dense_tree.rs` | Các thao tác GroveDB, `AuxDenseTreeStore`, tiền xử lý lô |
| `grovedb/src/operations/proof/generate.rs` | `generate_dense_tree_layer_proof()`, `query_items_to_positions()` |
| `grovedb/src/operations/proof/verify.rs` | `verify_dense_tree_lower_layer()` |
| `grovedb/src/operations/proof/mod.rs` | Biến thể `ProofBytes::DenseTree` |
| `grovedb/src/batch/estimated_costs/average_case_costs.rs` | Mô hình chi phí trường hợp trung bình |
| `grovedb/src/batch/estimated_costs/worst_case_costs.rs` | Mô hình chi phí trường hợp xấu nhất |
| `grovedb/src/tests/dense_tree_tests.rs` | 22 bài kiểm thử tích hợp |

---
