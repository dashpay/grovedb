# Grove phân cấp — Cây chứa cây

## Cách cây con lồng nhau bên trong cây cha

Tính năng định nghĩa của GroveDB là cây Merk có thể chứa các phần tử mà bản thân chúng cũng là cây Merk. Điều này tạo ra một **không gian tên phân cấp**:

```mermaid
graph TD
    subgraph root["CÂY MERK GỐC — đường dẫn: []"]
        contracts["&quot;contracts&quot;<br/>Tree"]
        identities["&quot;identities&quot;<br/>Tree"]
        balances["&quot;balances&quot;<br/>SumTree, sum=0"]
        contracts --> identities
        contracts --> balances
    end

    subgraph ident["MERK IDENTITIES — đường dẫn: [&quot;identities&quot;]"]
        bob456["&quot;bob456&quot;<br/>Tree"]
        alice123["&quot;alice123&quot;<br/>Tree"]
        eve["&quot;eve&quot;<br/>Item"]
        bob456 --> alice123
        bob456 --> eve
    end

    subgraph bal["MERK BALANCES (SumTree) — đường dẫn: [&quot;balances&quot;]"]
        bob_bal["&quot;bob456&quot;<br/>SumItem(800)"]
    end

    subgraph alice_tree["MERK ALICE123 — đường dẫn: [&quot;identities&quot;, &quot;alice123&quot;]"]
        name["&quot;name&quot;<br/>Item(&quot;Al&quot;)"]
        balance_item["&quot;balance&quot;<br/>SumItem(1000)"]
        docs["&quot;docs&quot;<br/>Tree"]
        name --> balance_item
        name --> docs
    end

    identities -.-> bob456
    balances -.-> bob_bal
    alice123 -.-> name
    docs -.->|"... thêm cây con"| more["..."]

    style root fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style ident fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style bal fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style alice_tree fill:#e8daef,stroke:#8e44ad,stroke-width:2px
    style more fill:none,stroke:none
```

> Mỗi ô màu là một cây Merk riêng biệt. Các mũi tên nét đứt đại diện cho liên kết cổng từ phần tử Tree đến cây Merk con. Đường dẫn đến mỗi Merk được hiển thị trong nhãn.

## Hệ thống địa chỉ đường dẫn

Mọi phần tử trong GroveDB được xác định bằng **đường dẫn** (path) — một chuỗi chuỗi byte điều hướng từ gốc qua các cây con đến khóa đích:

```text
    Đường dẫn: ["identities", "alice123", "name"]

    Bước 1: Trong cây gốc, tra cứu "identities" → phần tử Tree
    Bước 2: Mở cây con identities, tra cứu "alice123" → phần tử Tree
    Bước 3: Mở cây con alice123, tra cứu "name" → Item("Alice")
```

Đường dẫn được biểu diễn dưới dạng `Vec<Vec<u8>>` hoặc sử dụng kiểu `SubtreePath` để thao tác hiệu quả mà không cần cấp phát:

```rust
// Đường dẫn đến phần tử (tất cả các đoạn trừ đoạn cuối)
let path: &[&[u8]] = &[b"identities", b"alice123"];
// Khóa trong cây con cuối
let key: &[u8] = b"name";
```

## Tạo tiền tố Blake3 để cách ly lưu trữ

Mỗi cây con trong GroveDB có **không gian tên lưu trữ cách ly** riêng trong RocksDB. Không gian tên được xác định bằng cách băm đường dẫn với Blake3:

```rust
pub type SubtreePrefix = [u8; 32];

// Tiền tố được tính bằng cách băm các đoạn đường dẫn
// storage/src/rocksdb_storage/storage.rs
```

Ví dụ:

```text
    Đường dẫn: ["identities", "alice123"]
    Tiền tố: Blake3(["identities", "alice123"]) = [0xab, 0x3f, ...]  (32 byte)

    Trong RocksDB, khóa của cây con này được lưu dưới dạng:
    [tiền tố: 32 byte][khóa_gốc]

    Vậy "name" trong cây con này trở thành:
    [0xab, 0x3f, ...][0x6e, 0x61, 0x6d, 0x65]  ("name")
```

Điều này đảm bảo:
- Không có va chạm khóa giữa các cây con (tiền tố 32 byte = cách ly 256-bit)
- Tính toán tiền tố hiệu quả (một lần băm Blake3 trên byte đường dẫn)
- Dữ liệu cây con nằm gần nhau trong RocksDB để hiệu quả cache

## Lan truyền Root Hash qua hệ thống phân cấp

Khi một giá trị thay đổi sâu trong grove, thay đổi phải **lan truyền lên trên** để cập nhật root hash:

```text
    Thay đổi: Cập nhật "name" thành "ALICE" trong identities/alice123/

    Bước 1: Cập nhật giá trị trong cây Merk alice123
            → cây alice123 nhận root hash mới: H_alice_new

    Bước 2: Cập nhật phần tử "alice123" trong cây identities
            → value_hash của cây identities cho "alice123" =
              combine_hash(H(tree_element_bytes), H_alice_new)
            → cây identities nhận root hash mới: H_ident_new

    Bước 3: Cập nhật phần tử "identities" trong cây gốc
            → value_hash của cây gốc cho "identities" =
              combine_hash(H(tree_element_bytes), H_ident_new)
            → ROOT HASH thay đổi
```

```mermaid
graph TD
    subgraph step3["BƯỚC 3: Cập nhật cây gốc"]
        R3["Cây gốc tính lại:<br/>value_hash cho &quot;identities&quot; =<br/>combine_hash(H(tree_bytes), H_ident_MỚI)<br/>→ ROOT HASH mới"]
    end
    subgraph step2["BƯỚC 2: Cập nhật cây identities"]
        R2["Cây identities tính lại:<br/>value_hash cho &quot;alice123&quot; =<br/>combine_hash(H(tree_bytes), H_alice_MỚI)<br/>→ root hash mới: H_ident_MỚI"]
    end
    subgraph step1["BƯỚC 1: Cập nhật Merk alice123"]
        R1["Cây alice123 tính lại:<br/>value_hash(&quot;ALICE&quot;) → kv_hash mới<br/>→ root hash mới: H_alice_MỚI"]
    end

    R1 -->|"H_alice_MỚI chảy lên"| R2
    R2 -->|"H_ident_MỚI chảy lên"| R3

    style step1 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style step2 fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style step3 fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

**Trước và Sau** — các nút đã thay đổi được đánh dấu đỏ:

```mermaid
graph TD
    subgraph before["TRƯỚC"]
        B_root["Gốc: aabb1122"]
        B_ident["&quot;identities&quot;: cc44.."]
        B_contracts["&quot;contracts&quot;: 1234.."]
        B_balances["&quot;balances&quot;: 5678.."]
        B_alice["&quot;alice123&quot;: ee55.."]
        B_bob["&quot;bob456&quot;: bb22.."]
        B_name["&quot;name&quot;: 7f.."]
        B_docs["&quot;docs&quot;: a1.."]
        B_root --- B_ident
        B_root --- B_contracts
        B_root --- B_balances
        B_ident --- B_alice
        B_ident --- B_bob
        B_alice --- B_name
        B_alice --- B_docs
    end

    subgraph after["SAU"]
        A_root["Gốc: ff990033"]
        A_ident["&quot;identities&quot;: dd88.."]
        A_contracts["&quot;contracts&quot;: 1234.."]
        A_balances["&quot;balances&quot;: 5678.."]
        A_alice["&quot;alice123&quot;: 1a2b.."]
        A_bob["&quot;bob456&quot;: bb22.."]
        A_name["&quot;name&quot;: 3c.."]
        A_docs["&quot;docs&quot;: a1.."]
        A_root --- A_ident
        A_root --- A_contracts
        A_root --- A_balances
        A_ident --- A_alice
        A_ident --- A_bob
        A_alice --- A_name
        A_alice --- A_docs
    end

    style A_root fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style A_ident fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style A_alice fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style A_name fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
```

> Chỉ các nút trên đường dẫn từ giá trị đã thay đổi lên đến gốc mới được tính lại. Các nút anh em và nhánh khác vẫn không thay đổi.

Quá trình lan truyền được triển khai bởi `propagate_changes_with_transaction`, đi
lên đường dẫn từ cây con đã sửa đổi đến gốc, cập nhật hash phần tử của mỗi cha
dọc đường.

## Ví dụ cấu trúc Grove đa tầng

Đây là ví dụ hoàn chỉnh cho thấy cách Dash Platform cấu trúc trạng thái:

```mermaid
graph TD
    ROOT["Gốc GroveDB"]

    ROOT --> contracts["[01] &quot;data_contracts&quot;<br/>Tree"]
    ROOT --> identities["[02] &quot;identities&quot;<br/>Tree"]
    ROOT --> balances["[03] &quot;balances&quot;<br/>SumTree"]
    ROOT --> pools["[04] &quot;pools&quot;<br/>Tree"]

    contracts --> c1["contract_id_1<br/>Tree"]
    contracts --> c2["contract_id_2<br/>Tree"]
    c1 --> docs["&quot;documents&quot;<br/>Tree"]
    docs --> profile["&quot;profile&quot;<br/>Tree"]
    docs --> note["&quot;note&quot;<br/>Tree"]
    profile --> d1["doc_id_1<br/>Item"]
    profile --> d2["doc_id_2<br/>Item"]
    note --> d3["doc_id_3<br/>Item"]

    identities --> id1["identity_id_1<br/>Tree"]
    identities --> id2["identity_id_2<br/>Tree"]
    id1 --> keys["&quot;keys&quot;<br/>Tree"]
    id1 --> rev["&quot;revision&quot;<br/>Item(u64)"]
    keys --> k1["key_id_1<br/>Item(pubkey)"]
    keys --> k2["key_id_2<br/>Item(pubkey)"]

    balances --> b1["identity_id_1<br/>SumItem(balance)"]
    balances --> b2["identity_id_2<br/>SumItem(balance)"]

    style ROOT fill:#2c3e50,stroke:#2c3e50,color:#fff
    style contracts fill:#d4e6f1,stroke:#2980b9
    style identities fill:#d5f5e3,stroke:#27ae60
    style balances fill:#fef9e7,stroke:#f39c12
    style pools fill:#e8daef,stroke:#8e44ad
```

Mỗi ô là một cây Merk riêng biệt, được xác thực hoàn toàn lên đến một root hash duy nhất mà các validator đồng thuận.

---
