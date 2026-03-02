# Mật mã lượng tử — Phân tích mối đe dọa hậu lượng tử

Chương này phân tích cách máy tính lượng tử sẽ ảnh hưởng đến các thành phần mật mã cơ bản được sử dụng trong GroveDB và các giao thức giao dịch được bảo vệ xây dựng trên đó (Orchard, Dash Platform). Nó bao gồm các thành phần nào dễ bị tổn thương, thành phần nào an toàn, "thu hoạch bây giờ, giải mã sau" có ý nghĩa gì đối với dữ liệu đã lưu trữ, và các chiến lược giảm thiểu nào tồn tại — bao gồm cả thiết kế KEM lai.

## Hai thuật toán lượng tử quan trọng

Chỉ có hai thuật toán lượng tử liên quan đến mật mã trong thực tế:

**Thuật toán Shor** giải quyết vấn đề logarit rời rạc (và phân tích thừa số nguyên) trong thời gian đa thức. Đối với đường cong elliptic 255 bit như Pallas, điều này yêu cầu khoảng 510 qubit logic — nhưng với chi phí sửa lỗi, yêu cầu thực tế là khoảng 4 triệu qubit vật lý. Thuật toán Shor **phá hoàn toàn** tất cả mật mã đường cong elliptic bất kể kích thước khóa.

**Thuật toán Grover** cung cấp tăng tốc bậc hai cho tìm kiếm vét cạn. Khóa đối xứng 256 bit thực tế trở thành 128 bit. Tuy nhiên, độ sâu mạch của Grover trên không gian khóa 128 bit vẫn là 2^64 phép toán lượng tử — nhiều nhà mật mã học tin rằng điều này sẽ không bao giờ thực tế trên phần cứng thực do giới hạn mất kết hợp. Grover giảm biên an toàn nhưng không phá mật mã đối xứng được tham số hóa tốt.

| Thuật toán | Mục tiêu | Tăng tốc | Tác động thực tế |
|-----------|---------|---------|------------------|
| **Shor** | ECC logarit rời rạc, phân tích RSA | Thời gian đa thức (tăng tốc theo hàm mũ so với cổ điển) | **Phá hoàn toàn** ECC |
| **Grover** | Tìm kiếm khóa đối xứng, tiền ảnh hash | Bậc hai (giảm đôi bit khóa) | 256 bit → 128 bit (vẫn an toàn) |

## Các thành phần mật mã cơ bản của GroveDB

GroveDB và giao thức bảo vệ dựa trên Orchard sử dụng kết hợp các thành phần đường cong elliptic và đối xứng/dựa trên hash. Bảng dưới đây phân loại mỗi thành phần theo tính dễ bị tổn thương lượng tử:

### Dễ bị tổn thương trước lượng tử (Thuật toán Shor — 0 bit hậu lượng tử)

| Thành phần cơ bản | Nơi sử dụng | Gì bị phá |
|-----------|-----------|-------------|
| **Pallas ECDLP** | Cam kết ghi chú (cmx), khóa tạm thời (epk/esk), khóa xem (ivk), khóa thanh toán (pk_d), dẫn xuất nullifier | Khôi phục bất kỳ khóa riêng tư nào từ đối tác công khai |
| **Thỏa thuận khóa ECDH** (Pallas) | Dẫn xuất khóa mã hóa đối xứng cho bản mã ghi chú | Khôi phục bí mật chung → giải mã tất cả ghi chú |
| **Hash Sinsemilla** | Đường dẫn Merkle của CommitmentTree, băm trong mạch | Khả năng chống va chạm phụ thuộc vào ECDLP; suy yếu khi Pallas bị phá |
| **Halo 2 IPA** | Hệ thống chứng minh ZK (cam kết đa thức trên đường cong Pasta) | Giả mạo chứng minh cho mệnh đề sai (giả mạo, chi tiêu trái phép) |
| **Cam kết Pedersen** | Cam kết giá trị (cv_net) ẩn số tiền giao dịch | Khôi phục số tiền ẩn; giả mạo chứng minh cân bằng |

### An toàn trước lượng tử (Thuật toán Grover — 128+ bit hậu lượng tử)

| Thành phần cơ bản | Nơi sử dụng | Bảo mật hậu lượng tử |
|-----------|-----------|----------------------|
| **Blake3** | Hash nút cây Merk, nút MMR, gốc trạng thái BulkAppendTree, tiền tố đường dẫn cây con | 128 bit tiền ảnh, 128 bit tiền ảnh thứ hai |
| **BLAKE2b-256** | KDF cho dẫn xuất khóa đối xứng, khóa mã đi, PRF^expand | 128 bit tiền ảnh |
| **ChaCha20-Poly1305** | Mã hóa enc_ciphertext và out_ciphertext (khóa 256 bit) | 128 bit tìm kiếm khóa (an toàn, nhưng đường dẫn xuất khóa qua ECDH thì không) |
| **PRF^expand** (BLAKE2b-512) | Dẫn xuất esk, rcm, psi từ rseed | 128 bit bảo mật PRF |

### Hạ tầng GroveDB: an toàn trước lượng tử theo thiết kế

Tất cả cấu trúc dữ liệu riêng của GroveDB chỉ dựa vào băm Blake3:

- **Cây Merk AVL** — hash nút, combined_value_hash, lan truyền hash con
- **Cây MMR** — hash nút nội bộ, tính toán đỉnh, dẫn xuất gốc
- **BulkAppendTree** — chuỗi hash bộ đệm, gốc Merkle dày đặc, MMR kỷ nguyên
- **Gốc trạng thái CommitmentTree** — `blake3("ct_state" || sinsemilla_root || bulk_state_root)`
- **Tiền tố đường dẫn cây con** — Băm Blake3 các phân đoạn đường dẫn
- **Chứng minh V1** — chuỗi xác thực qua phân cấp Merk

**Hiện không cần thay đổi.** Chứng minh cây Merk của GroveDB, kiểm tra tính nhất quán MMR, gốc kỷ nguyên BulkAppendTree và tất cả chuỗi xác thực chứng minh V1 vẫn an toàn trước máy tính lượng tử. Hạ tầng dựa trên hash là phần mạnh nhất của hệ thống sau lượng tử.

## Đe dọa hồi tố và đe dọa thời gian thực

Sự phân biệt này rất quan trọng để ưu tiên những gì cần sửa và khi nào.

**Đe dọa hồi tố** xâm phạm dữ liệu đã được lưu trữ. Kẻ tấn công ghi lại dữ liệu hôm nay và giải mã khi máy tính lượng tử khả dụng. Những đe dọa này **không thể giảm thiểu sau khi xảy ra** — khi dữ liệu đã trên chuỗi, nó không thể được mã hóa lại hoặc thu hồi.

**Đe dọa thời gian thực** chỉ ảnh hưởng đến các giao dịch được tạo trong tương lai. Kẻ tấn công có máy tính lượng tử có thể giả mạo chữ ký hoặc chứng minh, nhưng chỉ cho các giao dịch mới. Các giao dịch cũ đã được mạng xác minh và xác nhận.

| Đe dọa | Loại | Gì bị lộ | Mức độ khẩn cấp |
|--------|------|---------------|---------|
| **Giải mã ghi chú** (enc_ciphertext) | **Hồi tố** | Nội dung ghi chú: người nhận, số tiền, ghi nhớ, rseed | **Cao** — lưu trữ vĩnh viễn |
| **Mở cam kết giá trị** (cv_net) | **Hồi tố** | Số tiền giao dịch (nhưng không phải người gửi/nhận) | **Trung bình** — chỉ số tiền |
| **Dữ liệu khôi phục người gửi** (out_ciphertext) | **Hồi tố** | Khóa khôi phục của người gửi cho ghi chú đã gửi | **Cao** — lưu trữ vĩnh viễn |
| Giả mạo ủy quyền chi tiêu | Thời gian thực | Có thể giả mạo chữ ký chi tiêu mới | Thấp — nâng cấp trước khi QC đến |
| Giả mạo chứng minh Halo 2 | Thời gian thực | Có thể giả mạo chứng minh mới (giả mạo) | Thấp — nâng cấp trước khi QC đến |
| Va chạm Sinsemilla | Thời gian thực | Có thể giả mạo đường dẫn Merkle mới | Thấp — bao hàm bởi giả mạo chứng minh |
| Giả mạo chữ ký ràng buộc | Thời gian thực | Có thể giả mạo chứng minh cân bằng mới | Thấp — nâng cấp trước khi QC đến |

### Chính xác những gì bị lộ?

**Nếu mã hóa ghi chú bị phá** (đe dọa HNDL chính):

Kẻ tấn công lượng tử khôi phục `esk` từ `epk` đã lưu trữ bằng thuật toán Shor, tính bí mật chia sẻ ECDH, dẫn xuất khóa đối xứng và giải mã `enc_ciphertext`. Điều này tiết lộ toàn bộ bản rõ ghi chú:

| Trường | Kích thước | Tiết lộ gì |
|-------|------|----------------|
| version | 1 byte | Phiên bản giao thức (không nhạy cảm) |
| diversifier | 11 bytes | Thành phần địa chỉ người nhận |
| value | 8 bytes | Số tiền giao dịch chính xác |
| rseed | 32 bytes | Cho phép liên kết nullifier (khử ẩn danh đồ thị giao dịch) |
| memo | 36 bytes (DashMemo) | Dữ liệu ứng dụng, có khả năng nhận dạng |

Với `rseed` và `rho` (lưu trữ cùng bản mã), kẻ tấn công có thể tính `esk = PRF(rseed, rho)` và xác minh ràng buộc khóa tạm thời. Kết hợp với diversifier, điều này liên kết đầu vào với đầu ra trên toàn bộ lịch sử giao dịch — **khử ẩn danh hoàn toàn hồ bảo vệ**.

**Nếu chỉ cam kết giá trị bị phá** (đe dọa HNDL thứ cấp):

Kẻ tấn công khôi phục `v` từ `cv_net = [v]*V + [rcv]*R` bằng cách giải ECDLP. Điều này tiết lộ **số tiền giao dịch nhưng không phải danh tính người gửi hoặc người nhận**. Kẻ tấn công thấy "ai đó gửi 5.0 Dash cho ai đó" nhưng không thể liên kết số tiền với bất kỳ địa chỉ hoặc danh tính nào mà không đồng thời phá mã hóa ghi chú.

Tự nó, số tiền không có liên kết có ích hạn chế. Nhưng kết hợp với dữ liệu bên ngoài (thời gian, hóa đơn đã biết, số tiền khớp với yêu cầu công khai), các cuộc tấn công tương quan trở nên khả thi.

## Cuộc tấn công "Thu hoạch Bây giờ, Giải mã Sau"

Đây là đe dọa lượng tử khẩn cấp và thực tế nhất.

**Mô hình tấn công:** Kẻ tấn công cấp nhà nước (hoặc bất kỳ bên nào có đủ bộ nhớ) ghi lại tất cả dữ liệu giao dịch được bảo vệ trên chuỗi hôm nay. Dữ liệu này có sẵn công khai trên blockchain và bất biến. Kẻ tấn công chờ một máy tính lượng tử có liên quan về mật mã (CRQC), sau đó:

```text
Step 1: Read stored record from CommitmentTree BulkAppendTree:
        cmx (32) || rho (32) || epk (32) || enc_ciphertext (104) || out_ciphertext (80)

Step 2: Solve ECDLP on Pallas via Shor's algorithm:
        epk = [esk] * g_d  →  recover esk

Step 3: Compute shared secret:
        shared_secret = [esk] * pk_d

Step 4: Derive symmetric key (BLAKE2b is quantum-safe, but input is compromised):
        K_enc = BLAKE2b-256("Zcash_OrchardKDF", shared_secret || epk)

Step 5: Decrypt enc_ciphertext using ChaCha20-Poly1305:
        → version || diversifier || value || rseed || memo

Step 6: With rseed + rho, link nullifiers to note commitments:
        esk = PRF(rseed, rho)
        → full transaction graph reconstruction
```

**Hiểu biết then chốt:** Mã hóa đối xứng (ChaCha20-Poly1305) hoàn toàn an toàn trước lượng tử. Lỗ hổng hoàn toàn nằm ở **đường dẫn dẫn xuất khóa** — khóa đối xứng được dẫn xuất từ bí mật chia sẻ ECDH, và ECDH bị phá bởi thuật toán Shor. Kẻ tấn công không phá mã hóa; họ khôi phục khóa.

**Tính hồi tố:** Cuộc tấn công này **hoàn toàn hồi tố**. Mọi ghi chú đã mã hóa từng lưu trữ trên chuỗi đều có thể được giải mã khi CRQC tồn tại. Dữ liệu không thể được mã hóa lại hoặc bảo vệ sau đó. Đây là lý do tại sao phải giải quyết trước khi dữ liệu được lưu trữ, không phải sau đó.

## Giảm thiểu: KEM lai (ML-KEM + ECDH)

Phòng thủ chống HNDL là dẫn xuất khóa mã hóa đối xứng từ **hai cơ chế thỏa thuận khóa độc lập**, sao cho chỉ phá một cái là không đủ. Đây được gọi là KEM lai.

### ML-KEM-768 (CRYSTALS-Kyber)

ML-KEM là cơ chế đóng gói khóa hậu lượng tử được NIST chuẩn hóa (FIPS 203, tháng 8 năm 2024) dựa trên bài toán Học với Lỗi Mô-đun (MLWE).

| Tham số | ML-KEM-512 | ML-KEM-768 | ML-KEM-1024 |
|-----------|-----------|-----------|------------|
| Khóa công khai (ek) | 800 bytes | **1,184 bytes** | 1,568 bytes |
| Bản mã (ct) | 768 bytes | **1,088 bytes** | 1,568 bytes |
| Bí mật chia sẻ | 32 bytes | 32 bytes | 32 bytes |
| Danh mục NIST | 1 (128 bit) | **3 (192 bit)** | 5 (256 bit) |

**ML-KEM-768** là lựa chọn được khuyến nghị — là bộ tham số được sử dụng bởi X-Wing, PQXDH của Signal và trao đổi khóa lai Chrome/Firefox TLS. Danh mục 3 cung cấp biên thoải mái chống lại những tiến bộ phân tích mật mã lưới trong tương lai.

### Cách hoạt động của sơ đồ lai

**Luồng hiện tại (dễ bị tổn thương):**

```text
Sender:
  esk = PRF(rseed, rho)                    // deterministic from note
  epk = [esk] * g_d                         // Pallas curve point
  shared_secret = [esk] * pk_d              // ECDH (broken by Shor's)
  K_enc = BLAKE2b("Zcash_OrchardKDF", shared_secret || epk)
  enc_ciphertext = ChaCha20(K_enc, note_plaintext)
```

**Luồng lai (kháng lượng tử):**

```text
Sender:
  esk = PRF(rseed, rho)                    // unchanged
  epk = [esk] * g_d                         // unchanged
  ss_ecdh = [esk] * pk_d                    // same ECDH

  (ct_pq, ss_pq) = ML-KEM-768.Encaps(ek_pq)  // NEW: lattice-based KEM
                                                // ek_pq from recipient's address

  K_enc = BLAKE2b(                          // MODIFIED: combines both secrets
      "DashPlatform_HybridKDF",
      ss_ecdh || ss_pq || ct_pq || epk
  )

  enc_ciphertext = ChaCha20(K_enc, note_plaintext)  // unchanged
```

**Giải mã phía người nhận:**

```text
Recipient:
  ss_ecdh = [ivk] * epk                    // same ECDH (using incoming viewing key)
  ss_pq = ML-KEM-768.Decaps(dk_pq, ct_pq)  // NEW: KEM decapsulation
  K_enc = BLAKE2b("DashPlatform_HybridKDF", ss_ecdh || ss_pq || ct_pq || epk)
  note_plaintext = ChaCha20.Decrypt(K_enc, enc_ciphertext)
```

### Đảm bảo bảo mật

KEM kết hợp có bảo mật IND-CCA2 nếu **bất kỳ** KEM thành phần nào an toàn. Điều này được chứng minh chính thức bởi [Giacon, Heuer, and Poettering (2018)](https://eprint.iacr.org/2018/024) cho các bộ kết hợp KEM sử dụng PRF (BLAKE2b đủ điều kiện), và được chứng minh độc lập bởi [chứng minh bảo mật X-Wing](https://eprint.iacr.org/2024/039).

| Kịch bản | ECDH | ML-KEM | Khóa kết hợp | Trạng thái |
|----------|------|--------|-------------|--------|
| Thế giới cổ điển | An toàn | An toàn | **An toàn** | Cả hai nguyên vẹn |
| Lượng tử phá ECC | **Bị phá** | An toàn | **An toàn** | ML-KEM bảo vệ |
| Tiến bộ lưới phá ML-KEM | An toàn | **Bị phá** | **An toàn** | ECDH bảo vệ (giống như hiện tại) |
| Cả hai bị phá | Bị phá | Bị phá | **Bị phá** | Cần hai đột phá đồng thời |

### Tác động kích thước

KEM lai thêm bản mã ML-KEM-768 (1,088 bytes) vào mỗi ghi chú đã lưu và mở rộng bản mã đi để bao gồm bí mật chia sẻ ML-KEM cho khôi phục người gửi:

**Bản ghi lưu trữ mỗi ghi chú:**

```text
┌──────────────────────────────────────────────────────────────────┐
│  Current (280 bytes)         Hybrid (1,400 bytes)               │
│                                                                  │
│  cmx:             32         cmx:              32               │
│  rho:             32         rho:              32               │
│  epk:             32         epk:              32               │
│  enc_ciphertext: 104         ct_pq:         1,088  ← NEW       │
│  out_ciphertext:  80         enc_ciphertext:  104               │
│                              out_ciphertext:  112  ← +32        │
│  ─────────────────           ──────────────────────             │
│  Total:          280         Total:          1,400  (5.0x)      │
└──────────────────────────────────────────────────────────────────┘
```

**Lưu trữ theo quy mô:**

| Số ghi chú | Hiện tại (280 B) | Lai (1,400 B) | Chênh lệch |
|-------|----------------|------------------|-------|
| 100,000 | 26.7 MB | 133 MB | +106 MB |
| 1,000,000 | 267 MB | 1.33 GB | +1.07 GB |
| 10,000,000 | 2.67 GB | 13.3 GB | +10.7 GB |

**Kích thước địa chỉ:**

```text
Current:  diversifier (11) + pk_d (32) = 43 bytes
Hybrid:   diversifier (11) + pk_d (32) + ek_pq (1,184) = 1,227 bytes
```

Khóa công khai ML-KEM 1,184 byte phải được bao gồm trong địa chỉ để người gửi có thể thực hiện đóng gói. Với khoảng 1,960 ký tự Bech32m, điều này lớn nhưng vẫn vừa với mã QR (tối đa ~2,953 ký tự chữ số).

### Quản lý khóa

Cặp khóa ML-KEM được dẫn xuất tất định từ khóa chi tiêu:

```text
SpendingKey (sk) [32 bytes]
  |
  +-> ... (all existing Orchard key derivation unchanged)
  |
  +-> ml_kem_d = PRF^expand(sk, [0x09])[0..32]
  +-> ml_kem_z = PRF^expand(sk, [0x0A])[0..32]
        |
        +-> (ek_pq, dk_pq) = ML-KEM-768.KeyGen(d=ml_kem_d, z=ml_kem_z)
              ek_pq: 1,184 bytes (public, included in address)
              dk_pq: 2,400 bytes (private, part of viewing key)
```

**Không cần thay đổi sao lưu.** Cụm từ hạt giống 24 từ hiện tại bao gồm khóa ML-KEM vì nó được dẫn xuất tất định từ khóa chi tiêu. Khôi phục ví hoạt động như trước.

**Địa chỉ đa dạng hóa** đều chia sẻ cùng `ek_pq` vì ML-KEM không có cơ chế đa dạng hóa tự nhiên như phép nhân vô hướng Pallas. Điều này có nghĩa là người quan sát có hai địa chỉ của một người dùng có thể liên kết chúng bằng cách so sánh `ek_pq`.

### Hiệu suất giải mã thử

| Bước | Hiện tại | Lai | Chênh lệch |
|------|---------|--------|-------|
| Pallas ECDH | ~100 us | ~100 us | — |
| ML-KEM-768 Decaps | — | ~40 us | +40 us |
| BLAKE2b KDF | ~0.5 us | ~1 us | — |
| ChaCha20 (52 bytes) | ~0.1 us | ~0.1 us | — |
| **Tổng mỗi ghi chú** | **~101 us** | **~141 us** | **+40% chi phí thêm** |

Quét 100,000 ghi chú: ~10.1 giây → ~14.1 giây. Chi phí thêm có ý nghĩa nhưng không cản trở. Giải đóng gói ML-KEM là thời gian hằng số không có lợi thế theo lô (không giống phép toán đường cong elliptic), nên nó tăng tuyến tính.

### Tác động lên mạch ZK

**Không có.** KEM lai hoàn toàn nằm trong tầng vận chuyển/mã hóa. Mạch Halo 2 chứng minh sự tồn tại ghi chú, tính đúng nullifier và cân bằng giá trị — nó không chứng minh bất cứ điều gì về mã hóa. Không thay đổi khóa chứng minh, khóa xác minh hoặc ràng buộc mạch.

### So sánh với ngành

| Hệ thống | Cách tiếp cận | Trạng thái |
|--------|----------|--------|
| **Signal** (PQXDH) | X25519 + ML-KEM-768, bắt buộc cho tất cả người dùng | **Đã triển khai** (2023) |
| **Chrome/Firefox TLS** | X25519 + ML-KEM-768 trao đổi khóa lai | **Đã triển khai** (2024) |
| **X-Wing** (bản nháp IETF) | X25519 + ML-KEM-768, bộ kết hợp chuyên dụng | Bản nháp tiêu chuẩn |
| **Zcash** | Bản nháp ZIP khả năng khôi phục lượng tử (khôi phục quỹ, không phải mã hóa) | Chỉ thảo luận |
| **Dash Platform** | Pallas ECDH + ML-KEM-768 (đề xuất) | Giai đoạn thiết kế |

## Khi nào triển khai

### Câu hỏi về lịch trình

- **Trạng thái hiện tại (2026):** Không có máy tính lượng tử nào có thể phá ECC 255 bit. Phân tích thừa số lượng tử lớn nhất được chứng minh: ~50 bit. Khoảng cách: nhiều bậc độ lớn.
- **Ngắn hạn (2030-2035):** Lộ trình phần cứng từ IBM, Google, Quantinuum nhắm tới hàng triệu qubit. Các triển khai ML-KEM và bộ tham số sẽ đã trưởng thành.
- **Trung hạn (2035-2050):** Hầu hết các ước tính đặt CRQC đến trong khung thời gian này. Dữ liệu HNDL thu thập hôm nay có nguy cơ.
- **Dài hạn (2050+):** Đồng thuận giữa các nhà mật mã học: máy tính lượng tử quy mô lớn là vấn đề "khi nào", không phải "nếu".

### Chiến lược khuyến nghị

**1. Thiết kế cho khả năng nâng cấp ngay bây giờ.** Đảm bảo định dạng bản ghi lưu trữ, cấu trúc `TransmittedNoteCiphertext` và bố cục mục nhập BulkAppendTree có phiên bản và có thể mở rộng. Điều này có chi phí thấp và bảo toàn tùy chọn thêm KEM lai sau này.

**2. Triển khai KEM lai khi sẵn sàng, bắt buộc.** Không cung cấp hai hồ (cổ điển và lai). Chia tập ẩn danh làm mất mục đích của giao dịch được bảo vệ — người dùng ẩn trong nhóm nhỏ hơn ít riêng tư hơn, không phải nhiều hơn. Khi triển khai, mọi ghi chú sử dụng sơ đồ lai.

**3. Nhắm tới khung thời gian 2028-2030.** Điều này trước bất kỳ mối đe dọa lượng tử thực tế nào nhưng sau khi các triển khai ML-KEM và kích thước tham số đã ổn định. Nó cũng cho phép học hỏi từ kinh nghiệm triển khai của Zcash và Signal.

**4. Theo dõi các sự kiện kích hoạt:**
- NIST hoặc NSA áp đặt thời hạn di chuyển hậu lượng tử
- Tiến bộ đáng kể trong phần cứng lượng tử (>100,000 qubit vật lý với sửa lỗi)
- Tiến bộ phân tích mật mã chống lại các bài toán lưới (sẽ ảnh hưởng đến lựa chọn ML-KEM)

### Những gì không cần hành động khẩn cấp

| Thành phần | Tại sao có thể đợi |
|-----------|----------------|
| Chữ ký ủy quyền chi tiêu | Giả mạo là thời gian thực, không hồi tố. Nâng cấp lên ML-DSA/SLH-DSA trước khi CRQC đến. |
| Hệ thống chứng minh Halo 2 | Giả mạo chứng minh là thời gian thực. Di chuyển sang hệ thống dựa trên STARK khi cần. |
| Khả năng chống va chạm Sinsemilla | Chỉ hữu ích cho các cuộc tấn công mới, không hồi tố. Bao hàm bởi việc di chuyển hệ thống chứng minh. |
| Hạ tầng GroveDB Merk/MMR/Blake3 | **Được coi là an toàn trước lượng tử theo các giả định mật mã hiện tại.** Không cần hành động dựa trên các cuộc tấn công đã biết. |

## Tham chiếu các phương án thay thế hậu lượng tử

### Cho mã hóa (thay thế ECDH)

| Sơ đồ | Loại | Khóa công khai | Bản mã | Danh mục NIST | Ghi chú |
|--------|------|-----------|-----------|---------------|-------|
| ML-KEM-768 | Lattice (MLWE) | 1,184 B | 1,088 B | 3 (192 bit) | FIPS 203, tiêu chuẩn ngành |
| ML-KEM-512 | Lattice (MLWE) | 800 B | 768 B | 1 (128 bit) | Nhỏ hơn, biên thấp hơn |
| ML-KEM-1024 | Lattice (MLWE) | 1,568 B | 1,568 B | 5 (256 bit) | Quá mức cho lai |

### Cho chữ ký (thay thế RedPallas/Schnorr)

| Sơ đồ | Loại | Khóa công khai | Chữ ký | Danh mục NIST | Ghi chú |
|--------|------|-----------|----------|---------------|-------|
| ML-DSA-65 (Dilithium) | Lattice | 1,952 B | 3,293 B | 3 | FIPS 204, nhanh |
| SLH-DSA (SPHINCS+) | Dựa trên hash | 32-64 B | 7,856-49,856 B | 1-5 | FIPS 205, thận trọng |
| XMSS/LMS | Dựa trên hash (có trạng thái) | 60 B | 2,500 B | khác nhau | Có trạng thái — tái sử dụng = phá |

### Cho chứng minh ZK (thay thế Halo 2)

| Hệ thống | Giả định | Kích thước chứng minh | Hậu lượng tử | Ghi chú |
|--------|-----------|-----------|-------------|-------|
| STARKs | Hàm băm (khả năng chống va chạm) | ~100-400 KB | **Có** | Được sử dụng bởi StarkNet |
| Plonky3 | FRI (cam kết đa thức dựa trên hash) | ~50-200 KB | **Có** | Đang phát triển tích cực |
| Halo 2 (hiện tại) | ECDLP trên đường cong Pasta | ~5 KB | **Không** | Hệ thống Orchard hiện tại |
| Lattice SNARKs | MLWE | Nghiên cứu | **Có** | Chưa sẵn sàng cho sản xuất |

### Hệ sinh thái Rust crate

| Crate | Nguồn | FIPS 203 | Đã xác minh | Ghi chú |
|-------|--------|----------|----------|-------|
| `libcrux-ml-kem` | Cryspen | Có | Xác minh chính thức (hax/F*) | Đảm bảo cao nhất |
| `ml-kem` | RustCrypto | Có | Thời gian hằng số, chưa kiểm toán | Tương thích hệ sinh thái |
| `fips203` | integritychain | Có | Thời gian hằng số | Rust thuần, no_std |

## Tóm tắt

```text
┌─────────────────────────────────────────────────────────────────────┐
│  TÓM TẮT MỐI ĐE DỌA LƯỢNG TỬ CHO GROVEDB + ORCHARD               │
│                                                                     │
│  AN TOÀN THEO GIẢ ĐỊNH HIỆN TẠI (dựa trên hàm băm):               │
│    ✓ Cây Blake3 Merk, MMR, BulkAppendTree                          │
│    ✓ BLAKE2b KDF, PRF^expand                                       │
│    ✓ Mã hóa đối xứng ChaCha20-Poly1305                             │
│    ✓ Tất cả chuỗi xác thực chứng minh GroveDB                     │
│                                                                     │
│  SỬA TRƯỚC KHI LƯU TRỮ DỮ LIỆU (HNDL hồi tố):                   │
│    ✗ Mã hóa ghi chú (thỏa thuận khóa ECDH) → KEM lai             │
│    ✗ Cam kết giá trị (Pedersen) → số tiền bị lộ                    │
│                                                                     │
│  SỬA TRƯỚC KHI MÁY TÍNH LƯỢNG TỬ XUẤT HIỆN (chỉ thời gian thực): │
│    ~ Ủy quyền chi tiêu → ML-DSA / SLH-DSA                         │
│    ~ Chứng minh ZK → STARKs / Plonky3                              │
│    ~ Sinsemilla → cây Merkle dựa trên hàm băm                      │
│                                                                     │
│  LỊCH TRÌNH KHUYẾN NGHỊ:                                            │
│    2026-2028: Thiết kế khả năng nâng cấp, phiên bản định dạng     │
│    2028-2030: Triển khai KEM lai bắt buộc cho mã hóa ghi chú      │
│    2035+: Di chuyển chữ ký và hệ thống chứng minh nếu cần         │
└─────────────────────────────────────────────────────────────────────┘
```

---
