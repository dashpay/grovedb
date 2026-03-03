# Hàm băm — Tính toàn vẹn mật mã

Mọi nút trong cây Merk đều được băm để tạo ra **root hash** (băm gốc) — một giá trị
32 byte duy nhất xác thực toàn bộ nội dung cây. Bất kỳ thay đổi nào đối với khóa,
giá trị, hoặc mối quan hệ cấu trúc nào đều sẽ tạo ra root hash khác.

## Hệ thống phân cấp hash ba tầng

Merk sử dụng sơ đồ băm ba tầng, từ trong ra ngoài:

Ví dụ: key = `"bob"` (3 byte), value = `"hello"` (5 byte):

```mermaid
graph LR
    subgraph level1["Tầng 1: value_hash"]
        V_IN["varint(5) ‖ &quot;hello&quot;"]
        V_BLAKE["Blake3"]
        V_OUT(["value_hash<br/><small>32 byte</small>"])
        V_IN --> V_BLAKE --> V_OUT
    end

    subgraph level2["Tầng 2: kv_hash"]
        K_IN["varint(3) ‖ &quot;bob&quot; ‖ value_hash"]
        K_BLAKE["Blake3"]
        K_OUT(["kv_hash<br/><small>32 byte</small>"])
        K_IN --> K_BLAKE --> K_OUT
    end

    subgraph level3["Tầng 3: node_hash"]
        N_LEFT(["left_child_hash<br/><small>32B (hoặc NULL_HASH)</small>"])
        N_KV(["kv_hash<br/><small>32B</small>"])
        N_RIGHT(["right_child_hash<br/><small>32B (hoặc NULL_HASH)</small>"])
        N_BLAKE["Blake3<br/><small>đầu vào 96B = 2 khối</small>"]
        N_OUT(["node_hash<br/><small>32 byte</small>"])
        N_LEFT --> N_BLAKE
        N_KV --> N_BLAKE
        N_RIGHT --> N_BLAKE
        N_BLAKE --> N_OUT
    end

    V_OUT -.-> K_IN
    K_OUT -.-> N_KV

    style level1 fill:#eaf2f8,stroke:#2980b9
    style level2 fill:#fef9e7,stroke:#f39c12
    style level3 fill:#fdedec,stroke:#e74c3c
```

> GỐC của cây = `node_hash` của nút gốc — xác thực **mọi** khóa, giá trị và mối quan hệ cấu trúc. Các nút con vắng mặt sử dụng `NULL_HASH = [0x00; 32]`.

### Tầng 1: value_hash

```rust
// merk/src/tree/hash.rs
pub fn value_hash(value: &[u8]) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    let val_length = value.len().encode_var_vec();  // Mã hóa varint
    hasher.update(val_length.as_slice());
    hasher.update(value);
    // ...
}
```

Độ dài của giá trị được **mã hóa varint** và thêm vào đầu. Điều này rất quan trọng
cho khả năng chống va chạm (collision resistance) — nếu không có nó,
`H("AB" ‖ "C")` sẽ bằng `H("A" ‖ "BC")`.

### Tầng 2: kv_hash

```rust
pub fn kv_hash(key: &[u8], value: &[u8]) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    let key_length = key.len().encode_var_vec();
    hasher.update(key_length.as_slice());
    hasher.update(key);
    let vh = value_hash(value);
    hasher.update(vh.as_slice());  // Hash lồng nhau
    // ...
}
```

Điều này ràng buộc khóa với giá trị. Để xác minh bằng chứng, cũng có biến thể
nhận value_hash đã tính trước:

```rust
pub fn kv_digest_to_kv_hash(key: &[u8], value_hash: &CryptoHash) -> CostContext<CryptoHash>
```

Biến thể này được sử dụng khi bên xác minh đã có value_hash (ví dụ: cho cây con
nơi value_hash là hash kết hợp).

### Tầng 3: node_hash

```rust
pub fn node_hash(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);       // 32 byte
    hasher.update(left);     // 32 byte
    hasher.update(right);    // 32 byte — tổng 96 byte
    // Luôn chính xác 2 thao tác hash (96 byte / khối 64 byte = 2)
}
```

Nếu một nút con vắng mặt, hash của nó là **NULL_HASH** — 32 byte zero:

```rust
pub const NULL_HASH: CryptoHash = [0; HASH_LENGTH];  // [0u8; 32]
```

## Blake3 là hàm băm

GroveDB sử dụng **Blake3** cho tất cả việc băm. Các thuộc tính chính:

- **Đầu ra 256-bit** (32 byte)
- **Kích thước khối**: 64 byte
- **Tốc độ**: nhanh hơn SHA-256 ~3 lần trên phần cứng hiện đại
- **Streaming**: Có thể nạp dữ liệu dần dần

Chi phí thao tác hash được tính dựa trên số khối 64 byte được xử lý:

```rust
let hashes = 1 + (hasher.count() - 1) / 64;  // Số thao tác hash
```

## Mã hóa tiền tố độ dài cho khả năng chống va chạm

Mọi đầu vào có độ dài thay đổi đều được thêm tiền tố độ dài bằng **mã hóa varint**:

```mermaid
graph LR
    subgraph bad["Không có tiền tố độ dài — DỄ BỊ TẤN CÔNG"]
        BAD1["H(&quot;AB&quot; + &quot;C&quot;) = H(0x41 0x42 0x43)"]
        BAD2["H(&quot;A&quot; + &quot;BC&quot;) = H(0x41 0x42 0x43)"]
        BAD1 --- SAME["CÙNG HASH!"]
        BAD2 --- SAME
    end

    subgraph good["Có tiền tố độ dài — chống va chạm"]
        GOOD1["H([02] 0x41 0x42)<br/><small>varint(2) ‖ &quot;AB&quot;</small>"]
        GOOD2["H([03] 0x41 0x42 0x43)<br/><small>varint(3) ‖ &quot;ABC&quot;</small>"]
        GOOD1 --- DIFF["KHÁC NHAU"]
        GOOD2 --- DIFF
    end

    style bad fill:#fadbd8,stroke:#e74c3c
    style good fill:#d5f5e3,stroke:#27ae60
    style SAME fill:#e74c3c,color:#fff,stroke:#c0392b
    style DIFF fill:#27ae60,color:#fff,stroke:#229954
```

> **Đầu vào value_hash**: `[varint(value.len)] [byte giá trị]`
> **Đầu vào kv_hash**: `[varint(key.len)] [byte khóa] [value_hash: 32 byte]`

Nếu không có tiền tố độ dài, kẻ tấn công có thể tạo ra các cặp khóa-giá trị
khác nhau nhưng băm ra cùng một digest. Tiền tố độ dài khiến điều này trở nên
không khả thi về mặt mật mã.

## Hàm băm kết hợp cho các phần tử đặc biệt

Đối với **cây con** và **tham chiếu** (reference), `value_hash` không đơn giản là
`H(value)`. Thay vào đó, nó là một **hash kết hợp** ràng buộc phần tử với đích
của nó:

```mermaid
graph LR
    subgraph item["Item thông thường"]
        I_val["byte giá trị"] --> I_hash["H(len ‖ bytes)"] --> I_vh(["value_hash"])
    end

    subgraph subtree["Phần tử cây con"]
        S_elem["byte phần tử tree"] --> S_hash1["H(len ‖ bytes)"]
        S_root(["root hash<br/>cây Merk con"])
        S_hash1 --> S_combine["combine_hash()<br/><small>Blake3(a ‖ b)</small>"]
        S_root --> S_combine
        S_combine --> S_vh(["value_hash"])
    end

    subgraph reference["Phần tử tham chiếu"]
        R_elem["byte phần tử ref"] --> R_hash1["H(len ‖ bytes)"]
        R_target["giá trị đích"] --> R_hash2["H(len ‖ bytes)"]
        R_hash1 --> R_combine["combine_hash()"]
        R_hash2 --> R_combine
        R_combine --> R_vh(["value_hash"])
    end

    style item fill:#eaf2f8,stroke:#2980b9
    style subtree fill:#fef9e7,stroke:#f39c12
    style reference fill:#fdedec,stroke:#e74c3c
```

> **Cây con:** ràng buộc root hash của cây Merk con vào cây cha. **Tham chiếu:** ràng buộc cả đường dẫn tham chiếu VÀ giá trị đích. Thay đổi bất kỳ cái nào cũng thay đổi root hash.

Hàm `combine_hash`:

```rust
pub fn combine_hash(hash_one: &CryptoHash, hash_two: &CryptoHash) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(hash_one);   // 32 byte
    hasher.update(hash_two);   // 32 byte — tổng 64 byte, chính xác 1 thao tác hash
    // ...
}
```

Đây là thứ cho phép GroveDB xác thực toàn bộ hệ thống phân cấp thông qua một root hash duy nhất — mỗi value_hash của cây cha cho phần tử cây con đều bao gồm root hash của cây con.

## Hàm băm tổng hợp cho ProvableCountTree

Các nút `ProvableCountTree` bao gồm số đếm tổng hợp trong node hash:

```rust
pub fn node_hash_with_count(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
    count: u64,
) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);                        // 32 byte
    hasher.update(left);                      // 32 byte
    hasher.update(right);                     // 32 byte
    hasher.update(&count.to_be_bytes());      // 8 byte — tổng 104 byte
    // Vẫn chính xác 2 thao tác hash (104 < 128 = 2 × 64)
}
```

Điều này có nghĩa là bằng chứng về số đếm không yêu cầu tiết lộ dữ liệu thực — số đếm được nhúng vào cam kết mật mã (cryptographic commitment).

---
