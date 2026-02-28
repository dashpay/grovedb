# Giới thiệu — GroveDB là gì?

## Ý tưởng cốt lõi

GroveDB là một **cấu trúc dữ liệu xác thực phân cấp** — về bản chất là một *grove*
(rừng cây, tức cây chứa cây) được xây dựng trên các cây AVL Merkle. Mỗi nút trong
cơ sở dữ liệu là một phần của cây được xác thực bằng mật mã, và mỗi cây có thể chứa
các cây con khác, tạo thành một hệ thống phân cấp sâu của trạng thái có thể kiểm chứng.

```mermaid
graph TD
    subgraph root["Root Merk Tree"]
        R_contracts["&quot;contracts&quot;<br/><i>Tree</i>"]
        R_identities["&quot;identities&quot;<br/><i>Tree</i>"]
        R_balances["&quot;balances&quot;<br/><i>SumTree</i>"]
        R_contracts --- R_identities
        R_contracts --- R_balances
    end

    subgraph ident["Identities Merk"]
        I_bob["&quot;bob&quot;<br/><i>Tree</i>"]
        I_alice["&quot;alice&quot;<br/><i>Tree</i>"]
        I_carol["&quot;carol&quot;<br/><i>Item</i>"]
        I_bob --- I_alice
        I_bob --- I_carol
    end

    subgraph contracts["Contracts Merk"]
        C_c2["&quot;C2&quot;<br/><i>Item</i>"]
        C_c1["&quot;C1&quot;<br/><i>Item</i>"]
        C_c3["&quot;C3&quot;<br/><i>Item</i>"]
        C_c2 --- C_c1
        C_c2 --- C_c3
    end

    subgraph balances["Balances SumTree — sum=5300"]
        B_bob["&quot;bob&quot;<br/>SumItem(2500)"]
        B_al["&quot;alice&quot;<br/>SumItem(2000)"]
        B_eve["&quot;eve&quot;<br/>SumItem(800)"]
        B_bob --- B_al
        B_bob --- B_eve
    end

    subgraph alice_merk["Alice Merk"]
        A_name["&quot;name&quot; → Alice"]
        A_bal["&quot;balance&quot; → 1000"]
    end

    subgraph bob_merk["Bob Merk"]
        Bo_name["&quot;name&quot; → Bob"]
    end

    R_identities -.->|subtree| ident
    R_contracts -.->|subtree| contracts
    R_balances -.->|subtree| balances
    I_alice -.->|subtree| alice_merk
    I_bob -.->|subtree| bob_merk

    style root fill:#e8f4fd,stroke:#2980b9,stroke-width:2px
    style ident fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style contracts fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style balances fill:#fdedec,stroke:#e74c3c,stroke-width:2px
    style alice_merk fill:#eafaf1,stroke:#27ae60,stroke-width:2px
    style bob_merk fill:#eafaf1,stroke:#27ae60,stroke-width:2px
```

> Mỗi ô màu là một **cây Merk riêng biệt**. Các mũi tên nét đứt thể hiện mối quan hệ cây con — một phần tử Tree trong cây cha chứa root key (khóa gốc) của cây Merk con.

Trong cơ sở dữ liệu truyền thống, bạn có thể lưu trữ dữ liệu trong một kho
khóa-giá trị phẳng với một cây Merkle duy nhất ở trên để xác thực. GroveDB áp dụng
một cách tiếp cận khác: nó lồng các cây Merkle bên trong các cây Merkle. Điều này
mang lại cho bạn:

1. **Chỉ mục thứ cấp hiệu quả** — truy vấn theo bất kỳ đường dẫn nào, không chỉ khóa chính
2. **Bằng chứng mật mã nhỏ gọn** — chứng minh sự tồn tại (hoặc vắng mặt) của bất kỳ dữ liệu nào
3. **Dữ liệu tổng hợp** — các cây có thể tự động tính tổng, đếm, hoặc tổng hợp
   các phần tử con của chúng
4. **Thao tác nguyên tử xuyên cây** — các thao tác theo lô trải rộng trên nhiều cây con

## Tại sao GroveDB tồn tại

GroveDB được thiết kế cho **Dash Platform**, một nền tảng ứng dụng phi tập trung
nơi mà mọi phần trạng thái phải:

- **Được xác thực**: Bất kỳ nút mạng nào cũng có thể chứng minh bất kỳ phần trạng thái nào cho máy khách nhẹ (light client)
- **Tất định**: Mọi nút mạng tính toán chính xác cùng một root hash (băm gốc) trạng thái
- **Hiệu quả**: Các thao tác phải hoàn thành trong giới hạn thời gian khối
- **Có thể truy vấn**: Ứng dụng cần các truy vấn phong phú, không chỉ tra cứu khóa

Các cách tiếp cận truyền thống có hạn chế:

| Cách tiếp cận | Vấn đề |
|----------|---------|
| Cây Merkle thuần | Chỉ hỗ trợ tra cứu khóa, không có truy vấn phạm vi |
| Ethereum MPT | Tái cân bằng tốn kém, kích thước bằng chứng lớn |
| Khóa-giá trị phẳng + cây đơn | Không có truy vấn phân cấp, một bằng chứng bao phủ mọi thứ |
| B-tree | Không tự nhiên Merkle hóa, xác thực phức tạp |

GroveDB giải quyết những vấn đề này bằng cách kết hợp **đảm bảo cân bằng đã được chứng minh của cây AVL** với **lồng ghép phân cấp** và **hệ thống kiểu phần tử phong phú**.

## Tổng quan kiến trúc

GroveDB được tổ chức thành các tầng riêng biệt, mỗi tầng có trách nhiệm rõ ràng:

```mermaid
graph TD
    APP["<b>Tầng ứng dụng</b><br/>Dash Platform, v.v.<br/><code>insert() get() query() prove() apply_batch()</code>"]

    GROVE["<b>GroveDB Core</b> — <code>grovedb/src/</code><br/>Quản lý cây con phân cấp · Hệ thống kiểu Element<br/>Giải quyết tham chiếu · Thao tác lô · Bằng chứng đa tầng"]

    MERK["<b>Tầng Merk</b> — <code>merk/src/</code><br/>Cây AVL Merkle · Phép quay tự cân bằng<br/>Hệ thống Link · Băm Blake3 · Mã hóa bằng chứng"]

    STORAGE["<b>Tầng lưu trữ</b> — <code>storage/src/</code><br/>RocksDB OptimisticTransactionDB<br/>4 column family · Cách ly tiền tố Blake3 · Ghi theo lô"]

    COST["<b>Tầng chi phí</b> — <code>costs/src/</code><br/>Theo dõi OperationCost · Monad CostContext<br/>Ước tính trường hợp xấu nhất &amp; trung bình"]

    APP ==>|"ghi ↓"| GROVE
    GROVE ==>|"thao tác cây"| MERK
    MERK ==>|"I/O đĩa"| STORAGE
    STORAGE -.->|"tích lũy chi phí ↑"| COST
    COST -.-> MERK
    MERK -.-> GROVE
    GROVE ==>|"đọc ↑"| APP

    style APP fill:#f5f5f5,stroke:#999,stroke-width:1px
    style GROVE fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style MERK fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style STORAGE fill:#fdebd0,stroke:#e67e22,stroke-width:2px
    style COST fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

Dữ liệu chảy **xuống** qua các tầng này trong quá trình ghi và **lên** trong quá trình đọc. Mỗi thao tác tích lũy chi phí khi đi qua ngăn xếp, cho phép tính toán
tài nguyên chính xác.

---
