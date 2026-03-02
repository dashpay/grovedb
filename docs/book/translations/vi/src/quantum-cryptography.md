# Mat ma luong tu -- Phan tich moi de doa hau luong tu

Chuong nay phan tich cach may tinh luong tu se anh huong den cac thanh phan mat ma co ban duoc su dung trong GroveDB va cac giao thuc giao dich duoc bao ve xay dung tren do (Orchard, Dash Platform). No bao gom cac thanh phan nao de bi ton thuong, thanh phan nao an toan, "thu hoach bay gio, giai ma sau" co y nghia gi doi voi du lieu da luu tru, va cac chien luoc giam thieu nao ton tai -- bao gom ca thiet ke KEM lai.

## Hai thuat toan luong tu quan trong

Chi co hai thuat toan luong tu lien quan den mat ma trong thuc te:

**Thuat toan Shor** giai quyet van de logarit roi rac (va phan tich thua so nguyen) trong thoi gian da thuc. Doi voi duong cong elliptic 255 bit nhu Pallas, dieu nay yeu cau khoang 510 qubit logic -- nhung voi chi phi sua loi, yeu cau thuc te la khoang 4 trieu qubit vat ly. Thuat toan Shor **pha hoan toan** tat ca mat ma duong cong elliptic bat ke kich thuoc khoa.

**Thuat toan Grover** cung cap tang toc bac hai cho tim kiem vet can. Khoa doi xung 256 bit thuc te tro thanh 128 bit. Tuy nhien, do sau mach cua Grover tren khong gian khoa 128 bit van la 2^64 phep toan luong tu -- nhieu nha mat ma hoc tin rang dieu nay se khong bao gio thuc te tren phan cung thuc do gioi han mat ket hop. Grover giam bien an toan nhung khong pha mat ma doi xung duoc tham so hoa tot.

| Thuat toan | Muc tieu | Tang toc | Tac dong thuc te |
|-----------|---------|---------|------------------|
| **Shor** | ECC logarit roi rac, phan tich RSA | Theo ham mu (thoi gian da thuc) | **Pha hoan toan** ECC |
| **Grover** | Tim kiem khoa doi xung, tien anh hash | Bac hai (giam doi bit khoa) | 256 bit -> 128 bit (van an toan) |

## Cac thanh phan mat ma co ban cua GroveDB

GroveDB va giao thuc bao ve dua tren Orchard su dung ket hop cac thanh phan duong cong elliptic va doi xung/dua tren hash. Bang duoi day phan loai moi thanh phan theo tinh de bi ton thuong luong tu:

### De bi ton thuong truoc luong tu (Thuat toan Shor -- 0 bit hau luong tu)

| Thanh phan co ban | Noi su dung | Gi bi pha |
|-----------|-----------|-------------|
| **Pallas ECDLP** | Cam ket ghi chu (cmx), khoa tam thoi (epk/esk), khoa xem (ivk), khoa thanh toan (pk_d), dan xuat nullifier | Khoi phuc bat ky khoa rieng tu nao tu doi tac cong khai |
| **Thoa thuan khoa ECDH** (Pallas) | Dan xuat khoa ma hoa doi xung cho ban ma ghi chu | Khoi phuc bi mat chung -> giai ma tat ca ghi chu |
| **Hash Sinsemilla** | Duong dan Merkle cua CommitmentTree, bam trong mach | Kha nang chong va cham phu thuoc vao ECDLP; suy yeu khi Pallas bi pha |
| **Halo 2 IPA** | He thong chung minh ZK (cam ket da thuc tren duong cong Pasta) | Gia mao chung minh cho menh de sai (gia mao, chi tieu trai phep) |
| **Cam ket Pedersen** | Cam ket gia tri (cv_net) an so tien giao dich | Khoi phuc so tien an; gia mao chung minh can bang |

### An toan truoc luong tu (Thuat toan Grover -- 128+ bit hau luong tu)

| Thanh phan co ban | Noi su dung | Bao mat hau luong tu |
|-----------|-----------|----------------------|
| **Blake3** | Hash nut cay Merk, nut MMR, goc trang thai BulkAppendTree, tien to duong dan cay con | 128 bit tien anh, 128 bit tien anh thu hai |
| **BLAKE2b-256** | KDF cho dan xuat khoa doi xung, khoa ma di, PRF^expand | 128 bit tien anh |
| **ChaCha20-Poly1305** | Ma hoa enc_ciphertext va out_ciphertext (khoa 256 bit) | 128 bit tim kiem khoa (an toan, nhung duong dan xuat khoa qua ECDH thi khong) |
| **PRF^expand** (BLAKE2b-512) | Dan xuat esk, rcm, psi tu rseed | 128 bit bao mat PRF |

### Ha tang GroveDB: Hoan toan an toan truoc luong tu

Tat ca cau truc du lieu rieng cua GroveDB chi dua vao bam Blake3:

- **Cay Merk AVL** -- hash nut, combined_value_hash, lan truyen hash con
- **Cay MMR** -- hash nut noi bo, tinh toan dinh, dan xuat goc
- **BulkAppendTree** -- chuoi hash bo dem, goc Merkle day dac, MMR ky nguyen
- **Goc trang thai CommitmentTree** -- `blake3("ct_state" || sinsemilla_root || bulk_state_root)`
- **Tien to duong dan cay con** -- Bam Blake3 cac phan doan duong dan
- **Chung minh V1** -- chuoi xac thuc qua phan cap Merk

**Khong can thay doi.** Chung minh cay Merk cua GroveDB, kiem tra tinh nhat quan MMR, goc ky nguyen BulkAppendTree va tat ca chuoi xac thuc chung minh V1 van an toan truoc may tinh luong tu. Ha tang dua tren hash la phan manh nhat cua he thong sau luong tu.

## De doa hoi to va de doa thoi gian thuc

Su phan biet nay rat quan trong de uu tien nhung gi can sua va khi nao.

**De doa hoi to** xam pham du lieu da duoc luu tru. Ke tan cong ghi lai du lieu hom nay va giai ma khi may tinh luong tu kha dung. Nhung de doa nay **khong the giam thieu sau khi xay ra** -- khi du lieu da tren chuoi, no khong the duoc ma hoa lai hoac thu hoi.

**De doa thoi gian thuc** chi anh huong den cac giao dich duoc tao trong tuong lai. Ke tan cong co may tinh luong tu co the gia mao chu ky hoac chung minh, nhung chi cho cac giao dich moi. Cac giao dich cu da duoc mang xac minh va xac nhan.

| De doa | Loai | Gi bi lo | Muc do khan cap |
|--------|------|---------------|---------|
| **Giai ma ghi chu** (enc_ciphertext) | **Hoi to** | Noi dung ghi chu: nguoi nhan, so tien, ghi nho, rseed | **Cao** -- luu tru vinh vien |
| **Mo cam ket gia tri** (cv_net) | **Hoi to** | So tien giao dich (nhung khong phai nguoi gui/nhan) | **Trung binh** -- chi so tien |
| **Du lieu khoi phuc nguoi gui** (out_ciphertext) | **Hoi to** | Khoa khoi phuc cua nguoi gui cho ghi chu da gui | **Cao** -- luu tru vinh vien |
| Gia mao uy quyen chi tieu | Thoi gian thuc | Co the gia mao chu ky chi tieu moi | Thap -- nang cap truoc khi QC den |
| Gia mao chung minh Halo 2 | Thoi gian thuc | Co the gia mao chung minh moi (gia mao) | Thap -- nang cap truoc khi QC den |
| Va cham Sinsemilla | Thoi gian thuc | Co the gia mao duong dan Merkle moi | Thap -- bao ham boi gia mao chung minh |
| Gia mao chu ky rang buoc | Thoi gian thuc | Co the gia mao chung minh can bang moi | Thap -- nang cap truoc khi QC den |

### Chinh xac nhung gi bi lo?

**Neu ma hoa ghi chu bi pha** (de doa HNDL chinh):

Ke tan cong luong tu khoi phuc `esk` tu `epk` da luu tru bang thuat toan Shor, tinh bi mat chia se ECDH, dan xuat khoa doi xung va giai ma `enc_ciphertext`. Dieu nay tiet lo toan bo ban ro ghi chu:

| Truong | Kich thuoc | Tiet lo gi |
|-------|------|----------------|
| version | 1 byte | Phien ban giao thuc (khong nhay cam) |
| diversifier | 11 bytes | Thanh phan dia chi nguoi nhan |
| value | 8 bytes | So tien giao dich chinh xac |
| rseed | 32 bytes | Cho phep lien ket nullifier (khu an danh do thi giao dich) |
| memo | 36 bytes (DashMemo) | Du lieu ung dung, co kha nang nhan dang |

Voi `rseed` va `rho` (luu tru cung ban ma), ke tan cong co the tinh `esk = PRF(rseed, rho)` va xac minh rang buoc khoa tam thoi. Ket hop voi diversifier, dieu nay lien ket dau vao voi dau ra tren toan bo lich su giao dich -- **khu an danh hoan toan ho bao ve**.

**Neu chi cam ket gia tri bi pha** (de doa HNDL thu cap):

Ke tan cong khoi phuc `v` tu `cv_net = [v]*V + [rcv]*R` bang cach giai ECDLP. Dieu nay tiet lo **so tien giao dich nhung khong phai danh tinh nguoi gui hoac nguoi nhan**. Ke tan cong thay "ai do gui 5.0 Dash cho ai do" nhung khong the lien ket so tien voi bat ky dia chi hoac danh tinh nao ma khong dong thoi pha ma hoa ghi chu.

Tu no, so tien khong co lien ket co ich han che. Nhung ket hop voi du lieu ben ngoai (thoi gian, hoa don da biet, so tien khop voi yeu cau cong khai), cac cuoc tan cong tuong quan tro nen kha thi.

## Cuoc tan cong "Thu hoach Bay gio, Giai ma Sau"

Day la de doa luong tu khan cap va thuc te nhat.

**Mo hinh tan cong:** Ke tan cong cap nha nuoc (hoac bat ky ben nao co du bo nho) ghi lai tat ca du lieu giao dich duoc bao ve tren chuoi hom nay. Du lieu nay co san cong khai tren blockchain va bat bien. Ke tan cong cho mot may tinh luong tu co lien quan ve mat ma (CRQC), sau do:

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

**Hieu biet then chot:** Ma hoa doi xung (ChaCha20-Poly1305) hoan toan an toan truoc luong tu. Lo hong hoan toan nam o **duong dan dan xuat khoa** -- khoa doi xung duoc dan xuat tu bi mat chia se ECDH, va ECDH bi pha boi thuat toan Shor. Ke tan cong khong pha ma hoa; ho khoi phuc khoa.

**Tinh hoi to:** Cuoc tan cong nay **hoan toan hoi to**. Moi ghi chu da ma hoa tung luu tru tren chuoi deu co the duoc giai ma khi CRQC ton tai. Du lieu khong the duoc ma hoa lai hoac bao ve sau do. Day la ly do tai sao phai giai quyet truoc khi du lieu duoc luu tru, khong phai sau do.

## Giam thieu: KEM lai (ML-KEM + ECDH)

Phong thu chong HNDL la dan xuat khoa ma hoa doi xung tu **hai co che thoa thuan khoa doc lap**, sao cho chi pha mot cai la khong du. Day duoc goi la KEM lai.

### ML-KEM-768 (CRYSTALS-Kyber)

ML-KEM la co che dong goi khoa hau luong tu duoc NIST chuan hoa (FIPS 203, thang 8 nam 2024) dua tren bai toan Hoc voi Loi Modun (MLWE).

| Tham so | ML-KEM-512 | ML-KEM-768 | ML-KEM-1024 |
|-----------|-----------|-----------|------------|
| Khoa cong khai (ek) | 800 bytes | **1,184 bytes** | 1,568 bytes |
| Ban ma (ct) | 768 bytes | **1,088 bytes** | 1,568 bytes |
| Bi mat chia se | 32 bytes | 32 bytes | 32 bytes |
| Danh muc NIST | 1 (128 bit) | **3 (192 bit)** | 5 (256 bit) |

**ML-KEM-768** la lua chon duoc khuyen nghi -- la bo tham so duoc su dung boi X-Wing, Signal's PQXDH va trao doi khoa lai Chrome/Firefox TLS. Danh muc 3 cung cap bien thoai mai chong lai nhung tien bo phan tich mat ma luoi trong tuong lai.

### Cach hoat dong cua so do lai

**Luong hien tai (de bi ton thuong):**

```text
Sender:
  esk = PRF(rseed, rho)                    // deterministic from note
  epk = [esk] * g_d                         // Pallas curve point
  shared_secret = [esk] * pk_d              // ECDH (broken by Shor's)
  K_enc = BLAKE2b("Zcash_OrchardKDF", shared_secret || epk)
  enc_ciphertext = ChaCha20(K_enc, note_plaintext)
```

**Luong lai (khang luong tu):**

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

**Giai ma phia nguoi nhan:**

```text
Recipient:
  ss_ecdh = [ivk] * epk                    // same ECDH (using incoming viewing key)
  ss_pq = ML-KEM-768.Decaps(dk_pq, ct_pq)  // NEW: KEM decapsulation
  K_enc = BLAKE2b("DashPlatform_HybridKDF", ss_ecdh || ss_pq || ct_pq || epk)
  note_plaintext = ChaCha20.Decrypt(K_enc, enc_ciphertext)
```

### Dam bao bao mat

KEM ket hop co bao mat IND-CCA2 neu **bat ky** KEM thanh phan nao an toan. Dieu nay duoc chung minh chinh thuc boi [Giacon, Heuer, and Poettering (2018)](https://eprint.iacr.org/2018/024) cho cac bo ket hop KEM su dung PRF (BLAKE2b du dieu kien), va duoc chung minh doc lap boi [chung minh bao mat X-Wing](https://eprint.iacr.org/2024/039).

| Kich ban | ECDH | ML-KEM | Khoa ket hop | Trang thai |
|----------|------|--------|-------------|--------|
| The gioi co dien | An toan | An toan | **An toan** | Ca hai nguyen ven |
| Luong tu pha ECC | **Bi pha** | An toan | **An toan** | ML-KEM bao ve |
| Tien bo luoi pha ML-KEM | An toan | **Bi pha** | **An toan** | ECDH bao ve (giong nhu hien tai) |
| Ca hai bi pha | Bi pha | Bi pha | **Bi pha** | Can hai dot pha dong thoi |

### Tac dong kich thuoc

KEM lai them ban ma ML-KEM-768 (1,088 bytes) vao moi ghi chu da luu va mo rong ban ma di de bao gom bi mat chia se ML-KEM cho khoi phuc nguoi gui:

**Ban ghi luu tru moi ghi chu:**

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

**Luu tru theo quy mo:**

| So ghi chu | Hien tai (280 B) | Lai (1,400 B) | Chenh lech |
|-------|----------------|------------------|-------|
| 100,000 | 26.7 MB | 133 MB | +106 MB |
| 1,000,000 | 267 MB | 1.33 GB | +1.07 GB |
| 10,000,000 | 2.67 GB | 13.3 GB | +10.7 GB |

**Kich thuoc dia chi:**

```text
Current:  diversifier (11) + pk_d (32) = 43 bytes
Hybrid:   diversifier (11) + pk_d (32) + ek_pq (1,184) = 1,227 bytes
```

Khoa cong khai ML-KEM 1,184 byte phai duoc bao gom trong dia chi de nguoi gui co the thuc hien dong goi. Voi khoang 1,960 ky tu Bech32m, dieu nay lon nhung van vua voi ma QR (toi da ~2,953 ky tu chu so).

### Quan ly khoa

Cap khoa ML-KEM duoc dan xuat tat dinh tu khoa chi tieu:

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

**Khong can thay doi sao luu.** Cum tu hat giong 24 tu hien tai bao gom khoa ML-KEM vi no duoc dan xuat tat dinh tu khoa chi tieu. Khoi phuc vi hoat dong nhu truoc.

**Dia chi da dang hoa** deu chia se cung `ek_pq` vi ML-KEM khong co co che da dang hoa tu nhien nhu phep nhan vo huong Pallas. Dieu nay co nghia la nguoi quan sat co hai dia chi cua mot nguoi dung co the lien ket chung bang cach so sanh `ek_pq`.

### Hieu suat giai ma thu

| Buoc | Hien tai | Lai | Chenh lech |
|------|---------|--------|-------|
| Pallas ECDH | ~100 us | ~100 us | -- |
| ML-KEM-768 Decaps | -- | ~40 us | +40 us |
| BLAKE2b KDF | ~0.5 us | ~1 us | -- |
| ChaCha20 (52 bytes) | ~0.1 us | ~0.1 us | -- |
| **Tong moi ghi chu** | **~101 us** | **~141 us** | **+40% chi phi them** |

Quet 100,000 ghi chu: ~10.1 giay -> ~14.1 giay. Chi phi them co y nghia nhung khong cam. Giai dong goi ML-KEM la thoi gian hang so khong co loi the theo lo (khong giong phep toan duong cong elliptic), nen no tang tuyen tinh.

### Tac dong len mach ZK

**Khong co.** KEM lai hoan toan nam trong tang van chuyen/ma hoa. Mach Halo 2 chung minh su ton tai ghi chu, tinh dung nullifier va can bang gia tri -- no khong chung minh bat cu dieu gi ve ma hoa. Khong thay doi khoa chung minh, khoa xac minh hoac rang buoc mach.

### So sanh voi nganh

| He thong | Cach tiep can | Trang thai |
|--------|----------|--------|
| **Signal** (PQXDH) | X25519 + ML-KEM-768, bat buoc cho tat ca nguoi dung | **Da trien khai** (2023) |
| **Chrome/Firefox TLS** | X25519 + ML-KEM-768 trao doi khoa lai | **Da trien khai** (2024) |
| **X-Wing** (ban nhap IETF) | X25519 + ML-KEM-768, bo ket hop chuyen dung | Ban nhap tieu chuan |
| **Zcash** | Ban nhap ZIP kha nang khoi phuc luong tu (khoi phuc quy, khong phai ma hoa) | Chi thao luan |
| **Dash Platform** | Pallas ECDH + ML-KEM-768 (de xuat) | Giai doan thiet ke |

## Khi nao trien khai

### Cau hoi ve lich trinh

- **Trang thai hien tai (2026):** Khong co may tinh luong tu nao co the pha ECC 255 bit. Phan tich thua so luong tu lon nhat duoc chung minh: ~50 bit. Khoang cach: nhieu bac do lon.
- **Ngan han (2030-2035):** Lo trinh phan cung tu IBM, Google, Quantinuum nham toi hang trieu qubit. Cac trien khai ML-KEM va bo tham so se da truong thanh.
- **Trung han (2035-2050):** Hau het cac uoc tinh dat CRQC den trong khung thoi gian nay. Du lieu HNDL thu thap hom nay co nguy co.
- **Dai han (2050+):** Dong thuan giua cac nha mat ma hoc: may tinh luong tu quy mo lon la van de "khi nao", khong phai "neu".

### Chien luoc khuyen nghi

**1. Thiet ke cho kha nang nang cap ngay bay gio.** Dam bao dinh dang ban ghi luu tru, cau truc `TransmittedNoteCiphertext` va bo cuc muc nhap BulkAppendTree co phien ban va co the mo rong. Dieu nay co chi phi thap va bao toan tuy chon them KEM lai sau nay.

**2. Trien khai KEM lai khi san sang, bat buoc.** Khong cung cap hai ho (co dien va lai). Chia tap an danh lam mat muc dich cua giao dich duoc bao ve -- nguoi dung an trong nhom nho hon it rieng tu hon, khong phai nhieu hon. Khi trien khai, moi ghi chu su dung so do lai.

**3. Nham toi khung thoi gian 2028-2030.** Dieu nay truoc bat ky moi de doa luong tu thuc te nao nhung sau khi cac trien khai ML-KEM va kich thuoc tham so da on dinh. No cung cho phep hoc hoi tu kinh nghiem trien khai cua Zcash va Signal.

**4. Theo doi cac su kien kich hoat:**
- NIST hoac NSA ap dat thoi han di chuyen hau luong tu
- Tien bo dang ke trong phan cung luong tu (>100,000 qubit vat ly voi sua loi)
- Tien bo phan tich mat ma chong lai cac bai toan luoi (se anh huong den lua chon ML-KEM)

### Nhung gi khong can hanh dong khan cap

| Thanh phan | Tai sao co the doi |
|-----------|----------------|
| Chu ky uy quyen chi tieu | Gia mao la thoi gian thuc, khong hoi to. Nang cap len ML-DSA/SLH-DSA truoc khi CRQC den. |
| He thong chung minh Halo 2 | Gia mao chung minh la thoi gian thuc. Di chuyen sang he thong dua tren STARK khi can. |
| Kha nang chong va cham Sinsemilla | Chi huu ich cho cac cuoc tan cong moi, khong hoi to. Bao ham boi viec di chuyen he thong chung minh. |
| Ha tang GroveDB Merk/MMR/Blake3 | **Da an toan truoc luong tu.** Khong can hanh dong, bay gio hay bat cu khi nao. |

## Tham chieu cac phuong an thay the hau luong tu

### Cho ma hoa (thay the ECDH)

| So do | Loai | Khoa cong khai | Ban ma | Danh muc NIST | Ghi chu |
|--------|------|-----------|-----------|---------------|-------|
| ML-KEM-768 | Lattice (MLWE) | 1,184 B | 1,088 B | 3 (192 bit) | FIPS 203, tieu chuan nganh |
| ML-KEM-512 | Lattice (MLWE) | 800 B | 768 B | 1 (128 bit) | Nho hon, bien thap hon |
| ML-KEM-1024 | Lattice (MLWE) | 1,568 B | 1,568 B | 5 (256 bit) | Qua muc cho lai |

### Cho chu ky (thay the RedPallas/Schnorr)

| So do | Loai | Khoa cong khai | Chu ky | Danh muc NIST | Ghi chu |
|--------|------|-----------|----------|---------------|-------|
| ML-DSA-65 (Dilithium) | Lattice | 1,952 B | 3,293 B | 3 | FIPS 204, nhanh |
| SLH-DSA (SPHINCS+) | Hash-based | 32-64 B | 7,856-49,856 B | 1-5 | FIPS 205, than trong |
| XMSS/LMS | Hash-based (stateful) | 60 B | 2,500 B | varies | Co trang thai -- tai su dung = pha |

### Cho chung minh ZK (thay the Halo 2)

| He thong | Gia dinh | Kich thuoc chung minh | Hau luong tu | Ghi chu |
|--------|-----------|-----------|-------------|-------|
| STARKs | Ham bam (kha nang chong va cham) | ~100-400 KB | **Yes** | Duoc su dung boi StarkNet |
| Plonky3 | FRI (cam ket da thuc dua tren hash) | ~50-200 KB | **Yes** | Dang phat trien tich cuc |
| Halo 2 (hien tai) | ECDLP tren duong cong Pasta | ~5 KB | **No** | He thong Orchard hien tai |
| Lattice SNARKs | MLWE | Nghien cuu | **Yes** | Chua san sang cho san xuat |

### He sinh thai Rust crate

| Crate | Nguon | FIPS 203 | Da xac minh | Ghi chu |
|-------|--------|----------|----------|-------|
| `libcrux-ml-kem` | Cryspen | Yes | Xac minh chinh thuc (hax/F*) | Dam bao cao nhat |
| `ml-kem` | RustCrypto | Yes | Thoi gian hang so, chua kiem toan | Tuong thich he sinh thai |
| `fips203` | integritychain | Yes | Thoi gian hang so | Rust thuan, no_std |

## Tom tat

```text
┌─────────────────────────────────────────────────────────────────────┐
│  QUANTUM THREAT SUMMARY FOR GROVEDB + ORCHARD                      │
│                                                                     │
│  SAFE NOW AND FOREVER (hash-based):                                 │
│    ✓ Blake3 Merk trees, MMR, BulkAppendTree                        │
│    ✓ BLAKE2b KDF, PRF^expand                                       │
│    ✓ ChaCha20-Poly1305 symmetric encryption                        │
│    ✓ All GroveDB proof authentication chains                        │
│                                                                     │
│  FIX BEFORE DATA IS STORED (retroactive HNDL):                     │
│    ✗ Note encryption (ECDH key agreement) → Hybrid KEM             │
│    ✗ Value commitments (Pedersen) → amounts revealed                │
│                                                                     │
│  FIX BEFORE QUANTUM COMPUTERS ARRIVE (real-time only):              │
│    ~ Spend authorization → ML-DSA / SLH-DSA                        │
│    ~ ZK proofs → STARKs / Plonky3                                  │
│    ~ Sinsemilla → hash-based Merkle tree                            │
│                                                                     │
│  RECOMMENDED TIMELINE:                                              │
│    2026-2028: Design for upgradability, version stored formats      │
│    2028-2030: Deploy mandatory hybrid KEM for note encryption       │
│    2035+: Migrate signatures and proof system if needed             │
└─────────────────────────────────────────────────────────────────────┘
```

---
