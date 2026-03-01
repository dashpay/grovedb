# BulkAppendTree -- Lưu trữ chỉ thêm thông lượng cao

BulkAppendTree là giải pháp của GroveDB cho một thách thức kỹ thuật cụ thể: làm thế nào để xây dựng nhật ký chỉ thêm thông lượng cao hỗ trợ chứng minh phạm vi hiệu quả, giảm thiểu hash mỗi lần ghi, và tạo ảnh chụp chunk bất biến phù hợp cho phân phối CDN?

Trong khi MmrTree (Chương 13) lý tưởng cho chứng minh lá riêng lẻ, BulkAppendTree được thiết kế cho khối lượng công việc nơi hàng nghìn giá trị đến trong mỗi khối và các client cần đồng bộ bằng cách tải phạm vi dữ liệu. Nó đạt được điều này với **kiến trúc hai tầng**: một buffer cây Merkle dày đặc hấp thụ các giá trị thêm vào, và một MMR cấp chunk ghi lại các root chunk đã hoàn thành.

## Kiến trúc hai tầng

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

**Tầng 1 -- Buffer (bộ đệm).** Các giá trị đến được ghi vào `DenseFixedSizedMerkleTree` (xem Chương 16). Dung lượng buffer là `2^height - 1` vị trí. Root hash của cây dày đặc (`dense_tree_root`) cập nhật sau mỗi lần chèn.

**Tầng 2 -- Chunk MMR.** Khi buffer đầy (đạt `chunk_size` mục), tất cả mục được tuần tự hóa thành một **chunk blob** bất biến, root Merkle dày đặc được tính từ các mục đó, và root đó được thêm làm lá vào chunk MMR. Sau đó buffer được xóa.

**State root** kết hợp cả hai tầng thành một cam kết 32 byte duy nhất thay đổi mỗi lần thêm, đảm bảo cây Merk cha luôn phản ánh trạng thái mới nhất.

## Cách giá trị lấp đầy buffer

Mỗi lần gọi `append()` theo trình tự này:

```text
Bước 1: Ghi giá trị vào buffer cây dày đặc tại vị trí tiếp theo
        dense_tree.insert(value, store)

Bước 2: Tăng total_count
        total_count += 1

Bước 3: Kiểm tra buffer đã đầy chưa (cây dày đặc đạt dung lượng)
        if dense_tree.count() == capacity:
            → kích hoạt nén (§14.3)

Bước 4: Tính state root mới (+1 lần gọi blake3)
        dense_tree_root = dense_tree.root_hash(store)
        state_root = blake3("bulk_state" || mmr_root || dense_tree_root)
```

**Buffer LÀ một DenseFixedSizedMerkleTree** (xem Chương 16). Root hash của nó thay đổi sau mỗi lần chèn, cung cấp cam kết đến tất cả mục buffer hiện tại. Root hash này là thứ chảy vào tính toán state root.

## Nén chunk (Chunk Compaction)

Khi buffer đầy (đạt `chunk_size` mục), nén tự động kích hoạt:

```text
Các bước nén:
─────────────────
1. Đọc tất cả chunk_size mục buffer

2. Tính root Merkle dày đặc
   - Hash mỗi mục: leaf[i] = blake3(entry[i])
   - Xây dựng cây nhị phân hoàn chỉnh từ dưới lên
   - Trích xuất root hash
   Chi phí hash: chunk_size + (chunk_size - 1) = 2 * chunk_size - 1

3. Tuần tự hóa mục thành chunk blob
   - Tự động chọn định dạng kích thước cố định hoặc thay đổi (§14.6)
   - Lưu trữ: store.put(chunk_key(chunk_index), blob)

4. Thêm root Merkle dày đặc vào chunk MMR
   - MMR push với chuỗi hợp nhất (xem Chương 13)
   Chi phí hash: ~2 trung bình (mẫu trailing_ones)

5. Reset cây dày đặc (xóa tất cả mục buffer khỏi lưu trữ)
   - Đặt lại count cây dày đặc về 0
```

Sau khi nén, chunk blob **vĩnh viễn bất biến** -- không bao giờ thay đổi nữa. Điều này làm chunk blob lý tưởng cho cache CDN, đồng bộ client, và lưu trữ lưu trữ.

**Ví dụ: 4 lần thêm với chunk_power=2 (chunk_size=4)**

```text
Append v_0: dense_tree=[v_0],       dense_root=H(v_0), total=1
Append v_1: dense_tree=[v_0,v_1],   dense_root=H(v_0,v_1), total=2
Append v_2: dense_tree=[v_0..v_2],  dense_root=H(v_0..v_2), total=3
Append v_3: dense_tree=[v_0..v_3],  dense_root=H(v_0..v_3), total=4
  → NÉN:
    chunk_blob_0 = serialize([v_0, v_1, v_2, v_3])
    dense_root_0 = dense_merkle_root(v_0..v_3)
    mmr.push(dense_root_0)
    cây dày đặc được xóa (count=0)

Append v_4: dense_tree=[v_4],       dense_root=H(v_4), total=5
  → state_root = blake3("bulk_state" || mmr_root || dense_root)
```

## State Root

State root ràng buộc cả hai tầng vào một hash:

```rust
fn compute_state_root(
    mmr_root: &[u8; 32],         // Root Chunk MMR (hoặc [0;32] nếu rỗng)
    dense_tree_root: &[u8; 32],  // Root hash của buffer hiện tại (cây dày đặc)
) -> [u8; 32] {
    blake3("bulk_state" || mmr_root || dense_tree_root)
}
```

`total_count` và `chunk_power` **không** được bao gồm trong state root vì chúng đã được xác thực bởi value hash của Merk -- chúng là trường của `Element` tuần tự hóa được lưu trong nút Merk cha. State root chỉ nắm bắt các cam kết cấp dữ liệu (`mmr_root` và `dense_tree_root`). Đây là hash chảy như Merk child hash và lan truyền lên đến root hash của GroveDB.

## Root Merkle dày đặc

Khi chunk được nén, các mục cần một cam kết 32 byte duy nhất. BulkAppendTree sử dụng **cây Merkle nhị phân dày đặc (hoàn chỉnh)**:

```text
Cho mục [e_0, e_1, e_2, e_3]:

Level 0 (lá):     blake3(e_0)  blake3(e_1)  blake3(e_2)  blake3(e_3)
                      \__________/              \__________/
Level 1:           blake3(h_0 || h_1)       blake3(h_2 || h_3)
                          \____________________/
Level 2 (root):    blake3(h_01 || h_23)  ← đây là root Merkle dày đặc
```

Vì `chunk_size` luôn là lũy thừa của 2 (theo thiết kế: `1u32 << chunk_power`), cây luôn hoàn chỉnh (không cần padding hay lá giả). Số hash chính xác là `2 * chunk_size - 1`:
- `chunk_size` hash lá (một cho mỗi mục)
- `chunk_size - 1` hash nút bên trong

Triển khai root Merkle dày đặc nằm trong `grovedb-mmr/src/dense_merkle.rs` và cung cấp hai hàm:
- `compute_dense_merkle_root(hashes)` -- từ lá đã hash sẵn
- `compute_dense_merkle_root_from_values(values)` -- hash giá trị trước, rồi xây cây

## Tuần tự hóa Chunk Blob

Chunk blob là các lưu trữ bất biến được tạo bởi nén. Bộ tuần tự hóa tự động chọn định dạng truyền tải nhỏ gọn nhất dựa trên kích thước mục:

**Định dạng kích thước cố định** (cờ `0x01`) -- khi tất cả mục có cùng độ dài:

```text
┌──────┬──────────┬─────────────┬─────────┬─────────┬─────────┐
│ 0x01 │ count    │ entry_size  │ entry_0 │ entry_1 │ ...     │
│ 1B   │ 4B (BE)  │ 4B (BE)     │ N bytes │ N bytes │         │
└──────┴──────────┴─────────────┴─────────┴─────────┴─────────┘
Tổng: 1 + 4 + 4 + (count × entry_size) byte
```

**Định dạng kích thước thay đổi** (cờ `0x00`) -- khi mục có độ dài khác nhau:

```text
┌──────┬──────────┬─────────┬──────────┬─────────┬─────────────┐
│ 0x00 │ len_0    │ entry_0 │ len_1    │ entry_1 │ ...         │
│ 1B   │ 4B (BE)  │ N bytes │ 4B (BE)  │ M bytes │             │
└──────┴──────────┴─────────┴──────────┴─────────┴─────────────┘
Tổng: 1 + Σ(4 + len_i) byte
```

Định dạng kích thước cố định tiết kiệm 4 byte mỗi mục so với kích thước thay đổi, tích lũy đáng kể cho chunk lớn có dữ liệu kích thước đồng nhất (như cam kết hash 32 byte). Cho 1024 mục 32 byte mỗi mục:
- Cố định: `1 + 4 + 4 + 32768 = 32.777 byte`
- Thay đổi: `1 + 1024 × (4 + 32) = 36.865 byte`
- Tiết kiệm: ~11%

## Bố cục khóa lưu trữ

Tất cả dữ liệu BulkAppendTree nằm trong không gian tên **data**, được đánh khóa với tiền tố một ký tự:

| Mẫu khóa | Định dạng | Kích thước | Mục đích |
|---|---|---|---|
| `M` | 1 byte | 1B | Khóa metadata |
| `b` + `{index}` | `b` + u32 BE | 5B | Mục buffer tại index |
| `e` + `{index}` | `e` + u64 BE | 9B | Chunk blob tại index |
| `m` + `{pos}` | `m` + u64 BE | 9B | Nút MMR tại vị trí |

**Metadata** lưu `mmr_size` (8 byte BE). `total_count` và `chunk_power` được lưu trong bản thân Element (trong Merk cha), không phải trong metadata không gian tên data. Sự phân chia này có nghĩa đọc count là tra cứu element đơn giản mà không cần mở ngữ cảnh lưu trữ data.

Khóa buffer sử dụng chỉ mục u32 (0 đến `chunk_size - 1`) vì dung lượng buffer bị giới hạn bởi `chunk_size` (một u32, tính bằng `1u32 << chunk_power`). Khóa chunk sử dụng chỉ mục u64 vì số chunk đã hoàn thành có thể tăng không giới hạn.

## Struct BulkAppendTree

```rust
pub struct BulkAppendTree<S> {
    pub total_count: u64,                        // Tổng giá trị đã thêm
    pub dense_tree: DenseFixedSizedMerkleTree<S>, // Buffer (cây dày đặc)
}
```

Buffer LÀ `DenseFixedSizedMerkleTree` -- root hash của nó là `dense_tree_root`.

**Accessor:**
- `capacity() -> u16`: `dense_tree.capacity()` (= `2^height - 1`)
- `epoch_size() -> u64`: `capacity + 1` (= `2^height`, số mục mỗi chunk)
- `height() -> u8`: `dense_tree.height()`

**Giá trị dẫn xuất** (không lưu trữ):

| Giá trị | Công thức |
|---|---|
| `chunk_count` | `total_count / epoch_size` |
| `buffer_count` | `dense_tree.count()` |
| `mmr_size` | `leaf_count_to_mmr_size(chunk_count)` |

## Các thao tác GroveDB

BulkAppendTree tích hợp với GroveDB qua sáu thao tác được định nghĩa trong `grovedb/src/operations/bulk_append_tree.rs`:

### bulk_append

Thao tác thay đổi chính. Tuân theo mẫu lưu trữ không phải Merk chuẩn của GroveDB:

```text
1. Xác thực element là BulkAppendTree
2. Mở ngữ cảnh lưu trữ data
3. Tải cây từ store
4. Thêm giá trị (có thể kích hoạt nén)
5. Cập nhật element trong Merk cha với state_root + total_count mới
6. Lan truyền thay đổi lên qua phân cấp Merk
7. Commit giao dịch
```

Adapter `AuxBulkStore` bọc các lời gọi `get_aux`/`put_aux`/`delete_aux` của GroveDB và tích lũy `OperationCost` trong `RefCell` để theo dõi chi phí. Chi phí hash từ thao tác thêm được cộng vào `cost.hash_node_calls`.

### Thao tác đọc

| Thao tác | Trả về gì | Lưu trữ aux? |
|---|---|---|
| `bulk_get_value(path, key, position)` | Giá trị tại vị trí toàn cục | Có -- đọc từ chunk blob hoặc buffer |
| `bulk_get_chunk(path, key, chunk_index)` | Chunk blob thô | Có -- đọc khóa chunk |
| `bulk_get_buffer(path, key)` | Tất cả mục buffer hiện tại | Có -- đọc khóa buffer |
| `bulk_count(path, key)` | Tổng count (u64) | Không -- đọc từ element |
| `bulk_chunk_count(path, key)` | Chunk đã hoàn thành (u64) | Không -- tính từ element |

Thao tác `get_value` định tuyến trong suốt theo vị trí:

```text
if position < completed_chunks × chunk_size:
    chunk_idx = position / chunk_size
    intra_idx = position % chunk_size
    → đọc chunk blob, giải tuần tự hóa, trả về entry[intra_idx]
else:
    buffer_idx = position % chunk_size
    → đọc buffer_key(buffer_idx)
```

## Thao tác theo lô và tiền xử lý

BulkAppendTree hỗ trợ thao tác lô qua biến thể `GroveOp::BulkAppend`. Vì `execute_ops_on_path` không có quyền truy cập ngữ cảnh lưu trữ data, tất cả thao tác BulkAppend phải được tiền xử lý trước `apply_body`.

Đường ống tiền xử lý:

```text
Đầu vào: [BulkAppend{v1}, Insert{...}, BulkAppend{v2}, BulkAppend{v3}]
                                     ↑ cùng (path,key) với v1

Bước 1: Nhóm thao tác BulkAppend theo (path, key)
        group_1: [v1, v2, v3]

Bước 2: Cho mỗi nhóm:
        a. Đọc element hiện có → lấy (total_count, chunk_power)
        b. Mở ngữ cảnh lưu trữ giao dịch
        c. Tải BulkAppendTree từ store
        d. Tải buffer hiện có vào bộ nhớ (Vec<Vec<u8>>)
        e. Cho mỗi giá trị:
           tree.append_with_mem_buffer(store, value, &mut mem_buffer)
        f. Lưu metadata
        g. Tính state_root cuối cùng

Bước 3: Thay thế tất cả thao tác BulkAppend bằng một ReplaceNonMerkTreeRoot cho mỗi nhóm
        mang: hash=state_root, meta=BulkAppendTree{total_count, chunk_power}

Đầu ra: [ReplaceNonMerkTreeRoot{...}, Insert{...}]
```

Biến thể `append_with_mem_buffer` tránh vấn đề đọc-sau-ghi: mục buffer được theo dõi trong `Vec<Vec<u8>>` trong bộ nhớ, nên nén có thể đọc chúng ngay cả khi lưu trữ giao dịch chưa commit.

## Trait BulkStore

```rust
pub trait BulkStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String>;
    fn delete(&self, key: &[u8]) -> Result<(), String>;
}
```

Các phương thức nhận `&self` (không phải `&mut self`) để khớp với mẫu khả biến bên trong (interior mutability) của GroveDB nơi ghi đi qua lô. Tích hợp GroveDB triển khai qua `AuxBulkStore` bọc `StorageContext` và tích lũy `OperationCost`.

`MmrAdapter` cầu nối `BulkStore` đến trait `MMRStoreReadOps`/`MMRStoreWriteOps` của ckb MMR, thêm bộ nhớ đệm ghi xuyên suốt cho tính đúng đắn đọc-sau-ghi.

## Tạo chứng minh

Chứng minh BulkAppendTree hỗ trợ **truy vấn phạm vi** theo vị trí. Cấu trúc chứng minh nắm bắt mọi thứ cần thiết cho trình xác minh không trạng thái xác nhận dữ liệu cụ thể tồn tại trong cây:

```rust
pub struct BulkAppendTreeProof {
    pub chunk_power: u8,
    pub total_count: u64,
    pub chunk_blobs: Vec<(u64, Vec<u8>)>,         // Chunk blob đầy đủ
    pub chunk_mmr_size: u64,
    pub chunk_mmr_proof_items: Vec<[u8; 32]>,      // Hash anh em MMR
    pub chunk_mmr_leaves: Vec<(u64, [u8; 32])>,    // (leaf_idx, dense_root)
    pub buffer_entries: Vec<Vec<u8>>,               // TẤT CẢ mục buffer
    pub chunk_mmr_root: [u8; 32],
}
```

**Các bước tạo** cho phạm vi `[start, end)` (với `chunk_size = 1u32 << chunk_power`):

```text
1. Xác định chunk chồng chéo
   first_chunk = start / chunk_size
   last_chunk  = min((end-1) / chunk_size, completed_chunks - 1)

2. Đọc chunk blob cho các chunk chồng chéo
   Cho mỗi chunk_idx trong [first_chunk, last_chunk]:
     chunk_blobs.push((chunk_idx, store.get(chunk_key(idx))))

3. Tính root Merkle dày đặc cho mỗi chunk blob
   Cho mỗi blob:
     giải tuần tự hóa → giá trị
     dense_root = compute_dense_merkle_root_from_values(values)
     chunk_mmr_leaves.push((chunk_idx, dense_root))

4. Tạo chứng minh MMR cho các vị trí chunk đó
   positions = chunk_indices.map(|idx| leaf_to_pos(idx))
   proof = mmr.gen_proof(positions)
   chunk_mmr_proof_items = proof.proof_items().map(|n| n.hash)

5. Lấy root chunk MMR

6. Đọc TẤT CẢ mục buffer (giới hạn bởi chunk_size)
   for i in 0..buffer_count:
     buffer_entries.push(store.get(buffer_key(i)))
```

**Tại sao bao gồm TẤT CẢ mục buffer?** Buffer là cây Merkle dày đặc mà root hash cam kết đến mọi mục. Trình xác minh phải xây lại cây từ tất cả mục để xác minh `dense_tree_root`. Vì buffer bị giới hạn bởi `capacity` (tối đa 65.535 mục), đây là chi phí hợp lý.

## Xác minh chứng minh

Xác minh là hàm thuần túy -- không cần truy cập cơ sở dữ liệu. Nó thực hiện năm kiểm tra:

```text
Bước 0: Nhất quán metadata
        - chunk_power <= 31
        - buffer_entries.len() == total_count % chunk_size
        - Số lá MMR == completed_chunks

Bước 1: Toàn vẹn chunk blob
        Cho mỗi (chunk_idx, blob):
          values = deserialize(blob)
          computed_root = dense_merkle_root(values)
          assert computed_root == chunk_mmr_leaves[chunk_idx]

Bước 2: Chứng minh chunk MMR
        Tái tạo lá MmrNode và proof items
        proof.verify(chunk_mmr_root, leaves) == true

Bước 3: Toàn vẹn buffer (cây dày đặc)
        Xây lại DenseFixedSizedMerkleTree từ buffer_entries
        dense_tree_root = tính root hash của cây xây lại

Bước 4: State root
        computed = blake3("bulk_state" || chunk_mmr_root || dense_tree_root)
        assert computed == expected_state_root
```

Sau khi xác minh thành công, `BulkAppendTreeProofResult` cung cấp phương thức `values_in_range(start, end)` trích xuất giá trị cụ thể từ chunk blob và mục buffer đã xác minh.

## Liên kết với Root Hash của GroveDB

BulkAppendTree là **cây không phải Merk** -- nó lưu dữ liệu trong không gian tên data, không phải trong cây con Merk. Trong Merk cha, element được lưu dưới dạng:

```text
Element::BulkAppendTree(total_count, chunk_power, flags)
```

State root chảy như Merk child hash. Hash nút Merk cha là:

```text
combine_hash(value_hash(element_bytes), state_root)
```

`state_root` chảy như Merk child hash (qua tham số `subtree_root_hash` của `insert_subtree`). Bất kỳ thay đổi nào đến state root lan truyền lên qua phân cấp Merk của GroveDB đến root hash.

Trong chứng minh V1 (mục 9.6), chứng minh Merk cha chứng minh byte element và ràng buộc child hash, và `BulkAppendTreeProof` chứng minh dữ liệu truy vấn nhất quán với `state_root` được sử dụng làm child hash.

## Theo dõi chi phí

Chi phí hash của mỗi thao tác được theo dõi rõ ràng:

| Thao tác | Số lần gọi Blake3 | Ghi chú |
|---|---|---|
| Thêm đơn (không nén) | 3 | 2 cho chuỗi hash buffer + 1 cho state root |
| Thêm đơn (có nén) | 3 + 2C - 1 + ~2 | Chuỗi + Merkle dày đặc (C=chunk_size) + MMR push + state root |
| `get_value` từ chunk | 0 | Giải tuần tự hóa thuần túy, không hash |
| `get_value` từ buffer | 0 | Tra cứu khóa trực tiếp |
| Tạo chứng minh | Phụ thuộc số chunk | Root Merkle dày đặc mỗi chunk + chứng minh MMR |
| Xác minh chứng minh | 2C·K - K + B·2 + 1 | K chunk, B mục buffer, C chunk_size |

**Chi phí trung bình mỗi lần thêm**: Cho chunk_size=1024 (chunk_power=10), chi phí nén ~2047 hash (root Merkle dày đặc) được phân bổ qua 1024 lần thêm, thêm ~2 hash mỗi lần thêm. Kết hợp với 3 hash mỗi lần thêm, tổng trung bình là **~5 lần gọi blake3 mỗi lần thêm** -- rất hiệu quả cho cấu trúc được xác thực mật mã.

## So sánh với MmrTree

| | BulkAppendTree | MmrTree |
|---|---|---|
| **Kiến trúc** | Hai tầng (buffer + chunk MMR) | MMR đơn |
| **Chi phí hash mỗi lần thêm** | 3 (+ trung bình ~2 cho nén) | ~2 |
| **Độ chi tiết chứng minh** | Truy vấn phạm vi theo vị trí | Chứng minh lá riêng lẻ |
| **Ảnh chụp bất biến** | Có (chunk blob) | Không |
| **Thân thiện CDN** | Có (chunk blob có thể cache) | Không |
| **Mục buffer** | Có (cần tất cả cho chứng minh) | Không áp dụng |
| **Phù hợp nhất cho** | Nhật ký thông lượng cao, đồng bộ hàng loạt | Nhật ký sự kiện, tra cứu riêng lẻ |
| **Element discriminant** | 13 | 12 |
| **TreeType** | 9 | 8 |

Chọn MmrTree khi bạn cần chứng minh lá riêng lẻ với chi phí tối thiểu. Chọn BulkAppendTree khi bạn cần truy vấn phạm vi, đồng bộ hàng loạt, và ảnh chụp dựa trên chunk.

## Tệp triển khai

| Tệp | Mục đích |
|------|---------|
| `grovedb-bulk-append-tree/src/lib.rs` | Root crate, re-export |
| `grovedb-bulk-append-tree/src/tree/mod.rs` | Struct `BulkAppendTree`, accessor trạng thái, lưu trữ metadata |
| `grovedb-bulk-append-tree/src/tree/append.rs` | `append()`, `append_with_mem_buffer()`, `compact_entries()` |
| `grovedb-bulk-append-tree/src/tree/hash.rs` | `compute_state_root` |
| `grovedb-bulk-append-tree/src/tree/keys.rs` | `META_KEY`, `buffer_key`, `chunk_key`, `mmr_node_key` |
| `grovedb-bulk-append-tree/src/tree/query.rs` | `get_value`, `get_chunk`, `get_buffer` |
| `grovedb-bulk-append-tree/src/tree/mmr_adapter.rs` | `MmrAdapter` với bộ nhớ đệm ghi xuyên suốt |
| `grovedb-bulk-append-tree/src/chunk.rs` | Tuần tự hóa chunk blob (định dạng cố định + thay đổi) |
| `grovedb-bulk-append-tree/src/proof.rs` | Tạo và xác minh `BulkAppendTreeProof` |
| `grovedb-bulk-append-tree/src/store.rs` | Trait `BulkStore` |
| `grovedb-bulk-append-tree/src/error.rs` | Enum `BulkAppendError` |
| `grovedb/src/operations/bulk_append_tree.rs` | Thao tác GroveDB, `AuxBulkStore`, tiền xử lý lô |
| `grovedb/src/tests/bulk_append_tree_tests.rs` | 27 bài kiểm thử tích hợp |

---
