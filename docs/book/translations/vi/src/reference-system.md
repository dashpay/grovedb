# Hệ thống tham chiếu

## Tại sao cần tham chiếu

Trong cơ sở dữ liệu phân cấp, bạn thường cần cùng dữ liệu có thể truy cập từ nhiều đường dẫn. Ví dụ, tài liệu có thể được lưu dưới hợp đồng nhưng cũng có thể truy vấn theo danh tính chủ sở hữu. **Tham chiếu** (Reference) là câu trả lời của GroveDB — chúng là con trỏ từ một vị trí sang vị trí khác, tương tự như liên kết tượng trưng (symbolic link) trong hệ thống tệp.

```mermaid
graph LR
    subgraph primary["Lưu trữ chính"]
        item["contracts/C1/docs/D1<br/><b>Item</b>(data)"]
    end
    subgraph secondary["Chỉ mục thứ cấp"]
        ref["identities/alice/docs/D1<br/><b>Reference</b>"]
    end
    ref -->|"trỏ đến"| item

    style primary fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style secondary fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style ref fill:#fef9e7,stroke:#f39c12,stroke-width:2px
```

Các thuộc tính chính:
- Tham chiếu được **xác thực** — value_hash của tham chiếu bao gồm cả bản thân tham chiếu và phần tử được tham chiếu
- Tham chiếu có thể được **xâu chuỗi** — tham chiếu có thể trỏ đến tham chiếu khác
- Phát hiện chu trình (cycle) ngăn vòng lặp vô hạn
- Giới hạn bước nhảy có thể cấu hình ngăn cạn kiệt tài nguyên

## Bảy loại tham chiếu

```rust
// grovedb-element/src/reference_path/mod.rs
pub enum ReferencePathType {
    AbsolutePathReference(Vec<Vec<u8>>),
    UpstreamRootHeightReference(u8, Vec<Vec<u8>>),
    UpstreamRootHeightWithParentPathAdditionReference(u8, Vec<Vec<u8>>),
    UpstreamFromElementHeightReference(u8, Vec<Vec<u8>>),
    CousinReference(Vec<u8>),
    RemovedCousinReference(Vec<Vec<u8>>),
    SiblingReference(Vec<u8>),
}
```

Hãy cùng xem qua từng loại với sơ đồ.

### AbsolutePathReference

Loại đơn giản nhất. Lưu trữ đường dẫn đầy đủ đến đích:

```mermaid
graph TD
    subgraph root["Merk gốc — đường dẫn: []"]
        A["A<br/>Tree"]
        P["P<br/>Tree"]
    end

    subgraph merkA["Merk [A]"]
        B["B<br/>Tree"]
    end

    subgraph merkP["Merk [P]"]
        Q["Q<br/>Tree"]
    end

    subgraph merkAB["Merk [A, B]"]
        X["X = Reference<br/>AbsolutePathRef([P, Q, R])"]
    end

    subgraph merkPQ["Merk [P, Q]"]
        R["R = Item<br/>&quot;target&quot;"]
    end

    A -.-> B
    P -.-> Q
    B -.-> X
    Q -.-> R
    X ==>|"giải quyết thành [P, Q, R]"| R

    style merkAB fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style merkPQ fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style X fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style R fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

> X lưu đường dẫn tuyệt đối đầy đủ `[P, Q, R]`. Bất kể X nằm ở đâu, nó luôn giải quyết đến cùng đích.

### UpstreamRootHeightReference

Giữ N đoạn đầu tiên của đường dẫn hiện tại, sau đó thêm đường dẫn mới:

```mermaid
graph TD
    subgraph resolve["Giải quyết: giữ 2 đoạn đầu + thêm [P, Q]"]
        direction LR
        curr["hiện tại: [A, B, C, D]"] --> keep["giữ 2 đầu: [A, B]"] --> append["thêm: [A, B, <b>P, Q</b>]"]
    end

    subgraph grove["Hệ thống phân cấp Grove"]
        gA["A (độ cao 0)"]
        gB["B (độ cao 1)"]
        gC["C (độ cao 2)"]
        gD["D (độ cao 3)"]
        gX["X = Reference<br/>UpstreamRootHeight(2, [P,Q])"]
        gP["P (độ cao 2)"]
        gQ["Q (độ cao 3) — đích"]

        gA --> gB
        gB --> gC
        gB -->|"giữ 2 đầu → [A,B]<br/>rồi đi xuống [P,Q]"| gP
        gC --> gD
        gD -.-> gX
        gP --> gQ
    end

    gX ==>|"giải quyết thành"| gQ

    style resolve fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style gX fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style gQ fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

### UpstreamRootHeightWithParentPathAdditionReference

Giống UpstreamRootHeight, nhưng thêm lại đoạn cuối của đường dẫn hiện tại:

```text
    Tham chiếu tại đường dẫn [A, B, C, D, E] khóa=X
    UpstreamRootHeightWithParentPathAdditionReference(2, [P, Q])

    Đường dẫn hiện tại: [A, B, C, D, E]
    Giữ 2 đầu:          [A, B]
    Thêm [P, Q]:         [A, B, P, Q]
    Thêm lại đoạn cuối:  [A, B, P, Q, E]   ← "E" từ đường dẫn gốc được thêm lại

    Hữu ích cho: chỉ mục nơi khóa cha cần được bảo toàn
```

### UpstreamFromElementHeightReference

Loại bỏ N đoạn cuối, sau đó thêm:

```text
    Tham chiếu tại đường dẫn [A, B, C, D] khóa=X
    UpstreamFromElementHeightReference(1, [P, Q])

    Đường dẫn hiện tại:  [A, B, C, D]
    Bỏ 1 đoạn cuối:      [A, B, C]
    Thêm [P, Q]:          [A, B, C, P, Q]
```

### CousinReference

Chỉ thay thế cha trực tiếp bằng khóa mới:

```mermaid
graph TD
    subgraph resolve["Giải quyết: bỏ 2 cuối, thêm cousin C, thêm khóa X"]
        direction LR
        r1["đường dẫn: [A, B, M, D]"] --> r2["bỏ 2 cuối: [A, B]"] --> r3["thêm C: [A, B, C]"] --> r4["thêm khóa X: [A, B, C, X]"]
    end

    subgraph merkAB["Merk [A, B]"]
        M["M<br/>Tree"]
        C["C<br/>Tree<br/>(cousin của M)"]
    end

    subgraph merkABM["Merk [A, B, M]"]
        D["D<br/>Tree"]
    end

    subgraph merkABMD["Merk [A, B, M, D]"]
        Xref["X = Reference<br/>CousinReference(C)"]
    end

    subgraph merkABC["Merk [A, B, C]"]
        Xtarget["X = Item<br/>(đích)"]
    end

    M -.-> D
    D -.-> Xref
    C -.-> Xtarget
    Xref ==>|"giải quyết thành [A, B, C, X]"| Xtarget

    style resolve fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style Xref fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style Xtarget fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style M fill:#fadbd8,stroke:#e74c3c
    style C fill:#d5f5e3,stroke:#27ae60
```

> "Cousin" là cây con anh em của ông bà tham chiếu. Tham chiếu đi lên hai tầng, rồi đi xuống cây con cousin.

### RemovedCousinReference

Giống CousinReference nhưng thay cha bằng đường dẫn nhiều đoạn:

```text
    Tham chiếu tại đường dẫn [A, B, C, D] khóa=X
    RemovedCousinReference([M, N])

    Đường dẫn hiện tại: [A, B, C, D]
    Bỏ cha C:            [A, B]
    Thêm [M, N]:         [A, B, M, N]
    Thêm khóa X:         [A, B, M, N, X]
```

### SiblingReference

Tham chiếu tương đối đơn giản nhất — chỉ đổi khóa trong cùng cây cha:

```mermaid
graph TD
    subgraph merk["Merk [A, B, C] — cùng cây, cùng đường dẫn"]
        M_sib["M"]
        X_sib["X = Reference<br/>SiblingRef(Y)"]
        Y_sib["Y = Item<br/>(đích)"]
        Z_sib["Z = Item"]
        M_sib --> X_sib
        M_sib --> Y_sib
    end

    X_sib ==>|"giải quyết thành [A, B, C, Y]"| Y_sib

    style merk fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style X_sib fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style Y_sib fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

> Loại tham chiếu đơn giản nhất. X và Y là anh em trong cùng cây Merk — giải quyết chỉ đổi khóa trong khi giữ nguyên đường dẫn.

## Theo dõi tham chiếu và giới hạn bước nhảy

Khi GroveDB gặp phần tử Reference, nó phải **theo** tham chiếu để tìm giá trị thực. Vì tham chiếu có thể trỏ đến tham chiếu khác, điều này liên quan đến một vòng lặp:

```rust
// grovedb/src/reference_path.rs
pub const MAX_REFERENCE_HOPS: usize = 10;

pub fn follow_reference(...) -> CostResult<ResolvedReference, Error> {
    let mut hops_left = MAX_REFERENCE_HOPS;
    let mut visited = HashSet::new();

    while hops_left > 0 {
        // Giải quyết đường dẫn tham chiếu thành đường dẫn tuyệt đối
        let target_path = current_ref.absolute_qualified_path(...);

        // Kiểm tra chu trình
        if !visited.insert(target_path.clone()) {
            return Err(Error::CyclicReference);
        }

        // Lấy phần tử tại đích
        let element = Element::get(target_path);

        match element {
            Element::Reference(next_ref, ..) => {
                // Vẫn là tham chiếu — tiếp tục theo
                current_ref = next_ref;
                hops_left -= 1;
            }
            other => {
                // Tìm thấy phần tử thực!
                return Ok(ResolvedReference { element: other, ... });
            }
        }
    }

    Err(Error::ReferenceLimit)  // Vượt quá 10 bước nhảy
}
```

## Phát hiện chu trình

HashSet `visited` theo dõi tất cả đường dẫn đã thấy. Nếu gặp đường dẫn đã từng thăm, ta có chu trình:

```mermaid
graph LR
    A["A<br/>Reference"] -->|"bước 1"| B["B<br/>Reference"]
    B -->|"bước 2"| C["C<br/>Reference"]
    C -->|"bước 3"| A

    style A fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style B fill:#fef9e7,stroke:#f39c12
    style C fill:#fef9e7,stroke:#f39c12
```

> **Truy vết phát hiện chu trình:**
>
> | Bước | Theo dõi | Tập visited | Kết quả |
> |------|--------|-------------|--------|
> | 1 | Bắt đầu tại A | { A } | A là Ref → theo |
> | 2 | A → B | { A, B } | B là Ref → theo |
> | 3 | B → C | { A, B, C } | C là Ref → theo |
> | 4 | C → A | A đã trong visited! | **Error::CyclicRef** |
>
> Nếu không phát hiện chu trình, sẽ lặp mãi mãi. `MAX_REFERENCE_HOPS = 10` cũng giới hạn độ sâu duyệt cho chuỗi dài.

## Tham chiếu trong Merk — Hash giá trị kết hợp

Khi Reference được lưu trong cây Merk, `value_hash` của nó phải xác thực cả cấu trúc tham chiếu và dữ liệu được tham chiếu:

```rust
// merk/src/tree/kv.rs
pub fn update_hashes_using_reference_value_hash(
    mut self,
    reference_value_hash: CryptoHash,
) -> CostContext<Self> {
    // Băm byte của bản thân phần tử tham chiếu
    let actual_value_hash = value_hash(self.value_as_slice());

    // Kết hợp: H(reference_bytes) ⊕ H(referenced_data)
    let combined = combine_hash(&actual_value_hash, &reference_value_hash);

    self.value_hash = combined;
    self.hash = kv_digest_to_kv_hash(self.key(), self.value_hash());
    // ...
}
```

Điều này có nghĩa là thay đổi tham chiếu HOẶC dữ liệu nó trỏ đến đều sẽ thay đổi root hash — cả hai đều được ràng buộc bằng mật mã.

---
