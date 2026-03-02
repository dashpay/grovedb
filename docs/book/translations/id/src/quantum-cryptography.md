# Kriptografi Kuantum — Analisis Ancaman Pasca-Kuantum

Bab ini menganalisis bagaimana komputer kuantum akan memengaruhi primitif
kriptografis yang digunakan dalam GroveDB dan protokol transaksi terlindung yang
dibangun di atasnya (Orchard, Dash Platform). Bab ini mencakup komponen mana
yang rentan, mana yang aman, apa arti "harvest now, decrypt later" bagi data
yang tersimpan, dan strategi mitigasi apa yang ada — termasuk desain KEM hibrid.

## Dua Algoritma Kuantum yang Penting

Hanya dua algoritma kuantum yang relevan dengan kriptografi dalam praktiknya:

**Algoritma Shor** memecahkan masalah logaritma diskret (dan faktorisasi bilangan
bulat) dalam waktu polinomial. Untuk kurva eliptik 255-bit seperti Pallas, ini
memerlukan sekitar 510 qubit logis — tetapi dengan overhead koreksi kesalahan,
kebutuhan sebenarnya adalah sekitar 4 juta qubit fisik. Algoritma Shor
**sepenuhnya memecahkan** semua kriptografi kurva eliptik terlepas dari ukuran
kunci.

**Algoritma Grover** memberikan percepatan kuadratik untuk pencarian brute-force.
Kunci simetris 256-bit secara efektif menjadi 128-bit. Namun, kedalaman sirkuit
untuk algoritma Grover pada ruang kunci 128-bit masih 2^64 operasi kuantum —
banyak kriptografer percaya ini tidak akan pernah praktis pada perangkat keras
nyata karena batas dekoherensi. Algoritma Grover mengurangi margin keamanan
tetapi tidak memecahkan kriptografi simetris yang terparameterisasi dengan baik.

| Algoritma | Target | Percepatan | Dampak praktis |
|-----------|--------|------------|----------------|
| **Shor** | ECC discrete log, RSA factoring | Eksponensial (waktu polinomial) | **Pemecahan total** ECC |
| **Grover** | Pencarian kunci simetris, hash preimage | Kuadratik (membelah dua bit kunci) | 256-bit → 128-bit (masih aman) |

## Primitif Kriptografis GroveDB

GroveDB dan protokol terlindung berbasis Orchard menggunakan campuran primitif
kurva eliptik dan simetris/berbasis hash. Tabel di bawah ini mengklasifikasikan
setiap primitif berdasarkan kerentanan kuantumnya:

### Rentan Kuantum (Algoritma Shor — 0 bit pasca-kuantum)

| Primitif | Tempat penggunaan | Apa yang rusak |
|----------|-------------------|----------------|
| **Pallas ECDLP** | Note commitments (cmx), ephemeral keys (epk/esk), viewing keys (ivk), payment keys (pk_d), nullifier derivation | Memulihkan kunci privat apa pun dari pasangan publiknya |
| **ECDH key agreement** (Pallas) | Menurunkan kunci enkripsi simetris untuk note ciphertexts | Memulihkan shared secret → mendekripsi semua notes |
| **Sinsemilla hash** | Jalur Merkle CommitmentTree, in-circuit hashing | Ketahanan kolisi bergantung pada ECDLP; menurun ketika Pallas terpecahkan |
| **Halo 2 IPA** | Sistem bukti ZK (polynomial commitment over Pasta curves) | Memalsukan bukti untuk pernyataan palsu (pemalsuan, pengeluaran tidak sah) |
| **Pedersen commitments** | Value commitments (cv_net) menyembunyikan jumlah transaksi | Memulihkan jumlah tersembunyi; memalsukan bukti saldo |

### Aman Kuantum (Algoritma Grover — 128+ bit pasca-kuantum)

| Primitif | Tempat penggunaan | Keamanan pasca-kuantum |
|----------|-------------------|------------------------|
| **Blake3** | Hash node Merk tree, MMR nodes, BulkAppendTree state roots, subtree path prefixes | 128-bit preimage, 128-bit second-preimage |
| **BLAKE2b-256** | KDF untuk penurunan kunci simetris, outgoing cipher key, PRF^expand | 128-bit preimage |
| **ChaCha20-Poly1305** | Mengenkripsi enc_ciphertext dan out_ciphertext (kunci 256-bit) | 128-bit key search (aman, tetapi jalur penurunan kunci melalui ECDH tidak aman) |
| **PRF^expand** (BLAKE2b-512) | Menurunkan esk, rcm, psi dari rseed | 128-bit PRF security |

### Infrastruktur GroveDB: Sepenuhnya Aman Kuantum

Seluruh struktur data GroveDB bergantung secara eksklusif pada hashing Blake3:

- **Merk AVL trees** — hash node, combined_value_hash, propagasi child hash
- **MMR trees** — hash node internal, komputasi puncak, derivasi root
- **BulkAppendTree** — rantai hash buffer, dense Merkle roots, epoch MMR
- **CommitmentTree state root** — `blake3("ct_state" || sinsemilla_root || bulk_state_root)`
- **Subtree path prefixes** — hashing Blake3 dari segmen jalur
- **V1 proofs** — rantai autentikasi melalui hierarki Merk

**Tidak diperlukan perubahan.** Bukti Merk tree GroveDB, pemeriksaan konsistensi
MMR, root epoch BulkAppendTree, dan semua rantai autentikasi bukti V1 tetap
aman terhadap komputer kuantum. Infrastruktur berbasis hash adalah bagian
terkuat dari sistem pasca-kuantum.

## Ancaman Retroaktif vs Real-Time

Perbedaan ini sangat penting untuk memprioritaskan apa yang harus diperbaiki
dan kapan.

**Ancaman retroaktif** mengkompromikan data yang sudah tersimpan. Penyerang
merekam data hari ini dan mendekripsinya ketika komputer kuantum tersedia.
Ancaman ini **tidak dapat dimitigasi setelah terjadi** — begitu data ada di
on-chain, data tersebut tidak dapat dienkripsi ulang atau ditarik kembali.

**Ancaman real-time** hanya memengaruhi transaksi yang dibuat di masa depan.
Penyerang dengan komputer kuantum dapat memalsukan tanda tangan atau bukti,
tetapi hanya untuk transaksi baru. Transaksi lama sudah divalidasi dan
dikonfirmasi oleh jaringan.

| Ancaman | Tipe | Apa yang terekspos | Urgensi |
|---------|------|--------------------|---------|
| **Dekripsi note** (enc_ciphertext) | **Retroaktif** | Isi note: penerima, jumlah, memo, rseed | **Tinggi** — tersimpan selamanya |
| **Pembukaan value commitment** (cv_net) | **Retroaktif** | Jumlah transaksi (tetapi bukan pengirim/penerima) | **Sedang** — hanya jumlah |
| **Data pemulihan pengirim** (out_ciphertext) | **Retroaktif** | Kunci pemulihan pengirim untuk note yang dikirim | **Tinggi** — tersimpan selamanya |
| Pemalsuan otorisasi pengeluaran | Real-time | Dapat memalsukan tanda tangan pengeluaran baru | Rendah — upgrade sebelum QC tiba |
| Pemalsuan bukti Halo 2 | Real-time | Dapat memalsukan bukti baru (pemalsuan) | Rendah — upgrade sebelum QC tiba |
| Kolisi Sinsemilla | Real-time | Dapat memalsukan jalur Merkle baru | Rendah — tercakup oleh pemalsuan bukti |
| Pemalsuan tanda tangan binding | Real-time | Dapat memalsukan bukti saldo baru | Rendah — upgrade sebelum QC tiba |

### Apa Tepatnya yang Terungkap?

**Jika enkripsi note terpecahkan** (ancaman HNDL utama):

Penyerang kuantum memulihkan `esk` dari `epk` yang tersimpan melalui algoritma
Shor, menghitung shared secret ECDH, menurunkan kunci simetris, dan mendekripsi
`enc_ciphertext`. Ini mengungkapkan plaintext note lengkap:

| Field | Ukuran | Apa yang diungkapkan |
|-------|--------|----------------------|
| version | 1 byte | Versi protokol (tidak sensitif) |
| diversifier | 11 bytes | Komponen alamat penerima |
| value | 8 bytes | Jumlah transaksi persis |
| rseed | 32 bytes | Memungkinkan keterkaitan nullifier (deanonimisasi grafik transaksi) |
| memo | 36 bytes (DashMemo) | Data aplikasi, berpotensi mengidentifikasi |

Dengan `rseed` dan `rho` (disimpan bersama ciphertext), penyerang dapat
menghitung `esk = PRF(rseed, rho)` dan memverifikasi binding kunci ephemeral.
Dikombinasikan dengan diversifier, ini menghubungkan input ke output di seluruh
riwayat transaksi — **deanonimisasi penuh dari shielded pool**.

**Jika hanya value commitments yang terpecahkan** (ancaman HNDL sekunder):

Penyerang memulihkan `v` dari `cv_net = [v]*V + [rcv]*R` dengan memecahkan
ECDLP. Ini mengungkapkan **jumlah transaksi tetapi bukan identitas pengirim
atau penerima**. Penyerang melihat "seseorang mengirim 5.0 Dash ke seseorang"
tetapi tidak dapat menghubungkan jumlah tersebut ke alamat atau identitas mana
pun tanpa juga memecahkan enkripsi note.

Dengan sendirinya, jumlah tanpa keterkaitan terbatas kegunaannya. Tetapi
dikombinasikan dengan data eksternal (waktu, faktur yang diketahui, jumlah yang
cocok dengan permintaan publik), serangan korelasi menjadi mungkin.

## Serangan "Harvest Now, Decrypt Later"

Ini adalah ancaman kuantum yang paling mendesak dan praktis.

**Model serangan:** Penyerang tingkat negara (atau pihak mana pun dengan
penyimpanan yang memadai) merekam semua data transaksi terlindung on-chain hari
ini. Data ini tersedia secara publik di blockchain dan tidak dapat diubah.
Penyerang menunggu komputer kuantum yang relevan secara kriptografis (CRQC),
kemudian:

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

**Wawasan kunci:** Enkripsi simetris (ChaCha20-Poly1305) sangat aman terhadap
kuantum. Kerentanannya sepenuhnya ada pada **jalur penurunan kunci** — kunci
simetris diturunkan dari shared secret ECDH, dan ECDH dipecahkan oleh algoritma
Shor. Penyerang tidak memecahkan enkripsinya; mereka memulihkan kuncinya.

**Retroaktivitas:** Serangan ini **sepenuhnya retroaktif**. Setiap note
terenkripsi yang pernah disimpan on-chain dapat didekripsi begitu CRQC ada.
Data tersebut tidak dapat dienkripsi ulang atau dilindungi setelah terjadi.
Inilah mengapa harus ditangani sebelum data disimpan, bukan sesudahnya.

## Mitigasi: KEM Hibrid (ML-KEM + ECDH)

Pertahanan terhadap HNDL adalah menurunkan kunci enkripsi simetris dari
**dua mekanisme kesepakatan kunci independen**, sehingga memecahkan hanya satu
saja tidak cukup. Ini disebut KEM hibrid.

### ML-KEM-768 (CRYSTALS-Kyber)

ML-KEM adalah mekanisme enkapsulasi kunci pasca-kuantum yang distandarisasi
NIST (FIPS 203, Agustus 2024) berdasarkan masalah Module Learning With Errors
(MLWE).

| Parameter | ML-KEM-512 | ML-KEM-768 | ML-KEM-1024 |
|-----------|-----------|-----------|------------|
| Public key (ek) | 800 bytes | **1.184 bytes** | 1.568 bytes |
| Ciphertext (ct) | 768 bytes | **1.088 bytes** | 1.568 bytes |
| Shared secret | 32 bytes | 32 bytes | 32 bytes |
| Kategori NIST | 1 (128-bit) | **3 (192-bit)** | 5 (256-bit) |

**ML-KEM-768** adalah pilihan yang direkomendasikan — ini adalah set parameter
yang digunakan oleh X-Wing, PQXDH Signal, dan pertukaran kunci hibrid TLS
Chrome/Firefox. Kategori 3 memberikan margin yang nyaman terhadap kemajuan
kriptoanalisis lattice di masa depan.

### Cara Kerja Skema Hibrid

**Alur saat ini (rentan):**

```text
Sender:
  esk = PRF(rseed, rho)                    // deterministic from note
  epk = [esk] * g_d                         // Pallas curve point
  shared_secret = [esk] * pk_d              // ECDH (broken by Shor's)
  K_enc = BLAKE2b("Zcash_OrchardKDF", shared_secret || epk)
  enc_ciphertext = ChaCha20(K_enc, note_plaintext)
```

**Alur hibrid (tahan kuantum):**

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

**Dekripsi penerima:**

```text
Recipient:
  ss_ecdh = [ivk] * epk                    // same ECDH (using incoming viewing key)
  ss_pq = ML-KEM-768.Decaps(dk_pq, ct_pq)  // NEW: KEM decapsulation
  K_enc = BLAKE2b("DashPlatform_HybridKDF", ss_ecdh || ss_pq || ct_pq || epk)
  note_plaintext = ChaCha20.Decrypt(K_enc, enc_ciphertext)
```

### Jaminan Keamanan

KEM gabungan bersifat IND-CCA2 secure jika **salah satu** komponen KEM aman.
Ini dibuktikan secara formal oleh [Giacon, Heuer, dan Poettering (2018)](https://eprint.iacr.org/2018/024)
untuk kombinator KEM menggunakan PRF (BLAKE2b memenuhi syarat), dan secara
independen oleh [bukti keamanan X-Wing](https://eprint.iacr.org/2024/039).

| Skenario | ECDH | ML-KEM | Kunci gabungan | Status |
|----------|------|--------|----------------|--------|
| Dunia klasik | Aman | Aman | **Aman** | Keduanya utuh |
| Kuantum memecahkan ECC | **Terpecahkan** | Aman | **Aman** | ML-KEM melindungi |
| Kemajuan lattice memecahkan ML-KEM | Aman | **Terpecahkan** | **Aman** | ECDH melindungi (sama seperti sekarang) |
| Keduanya terpecahkan | Terpecahkan | Terpecahkan | **Terpecahkan** | Memerlukan dua terobosan simultan |

### Dampak Ukuran

KEM hibrid menambahkan ciphertext ML-KEM-768 (1.088 bytes) ke setiap note yang
disimpan dan memperluas outgoing ciphertext untuk menyertakan shared secret
ML-KEM untuk pemulihan pengirim:

**Catatan tersimpan per note:**

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

**Penyimpanan dalam skala besar:**

| Notes | Saat ini (280 B) | Hibrid (1.400 B) | Delta |
|-------|------------------|-------------------|-------|
| 100.000 | 26,7 MB | 133 MB | +106 MB |
| 1.000.000 | 267 MB | 1,33 GB | +1,07 GB |
| 10.000.000 | 2,67 GB | 13,3 GB | +10,7 GB |

**Ukuran alamat:**

```text
Current:  diversifier (11) + pk_d (32) = 43 bytes
Hybrid:   diversifier (11) + pk_d (32) + ek_pq (1,184) = 1,227 bytes
```

Public key ML-KEM 1.184 bytes harus disertakan dalam alamat agar pengirim dapat
melakukan enkapsulasi. Dengan ~1.960 karakter Bech32m, ini besar tetapi masih
muat dalam kode QR (maks ~2.953 karakter alfanumerik).

### Manajemen Kunci

Pasangan kunci ML-KEM diturunkan secara deterministik dari spending key:

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

**Tidak diperlukan perubahan cadangan.** Frasa seed 24 kata yang ada mencakup
kunci ML-KEM karena diturunkan dari spending key secara deterministik. Pemulihan
dompet bekerja seperti sebelumnya.

**Alamat terdiversifikasi** semuanya berbagi `ek_pq` yang sama karena ML-KEM
tidak memiliki mekanisme diversifikasi alami seperti perkalian skalar Pallas.
Ini berarti pengamat dengan dua alamat pengguna dapat menghubungkannya dengan
membandingkan `ek_pq`.

### Performa Trial Decryption

| Langkah | Saat ini | Hibrid | Delta |
|---------|----------|--------|-------|
| Pallas ECDH | ~100 us | ~100 us | — |
| ML-KEM-768 Decaps | — | ~40 us | +40 us |
| BLAKE2b KDF | ~0,5 us | ~1 us | — |
| ChaCha20 (52 bytes) | ~0,1 us | ~0,1 us | — |
| **Total per note** | **~101 us** | **~141 us** | **+40% overhead** |

Pemindaian 100.000 notes: ~10,1 detik → ~14,1 detik. Overhead-nya signifikan
tetapi tidak berlebihan. Dekapsulasi ML-KEM berjalan dalam waktu konstan tanpa
keuntungan batching (tidak seperti operasi kurva eliptik), sehingga skalanya
linear.

### Dampak pada Sirkuit ZK

**Tidak ada.** KEM hibrid sepenuhnya berada di lapisan transport/enkripsi.
Sirkuit Halo 2 membuktikan keberadaan note, kebenaran nullifier, dan
keseimbangan nilai — sirkuit ini tidak membuktikan apa pun tentang enkripsi.
Tidak ada perubahan pada proving keys, verifying keys, atau batasan sirkuit.

### Perbandingan dengan Industri

| Sistem | Pendekatan | Status |
|--------|------------|--------|
| **Signal** (PQXDH) | X25519 + ML-KEM-768, wajib untuk semua pengguna | **Diterapkan** (2023) |
| **Chrome/Firefox TLS** | X25519 + ML-KEM-768 pertukaran kunci hibrid | **Diterapkan** (2024) |
| **X-Wing** (draf IETF) | X25519 + ML-KEM-768, kombinator yang dibuat khusus | Draf standar |
| **Zcash** | Draf ZIP pemulihan kuantum (pemulihan dana, bukan enkripsi) | Hanya diskusi |
| **Dash Platform** | Pallas ECDH + ML-KEM-768 (diusulkan) | Fase desain |

## Kapan Harus Diterapkan

### Pertanyaan Garis Waktu

- **Kondisi saat ini (2026):** Tidak ada komputer kuantum yang dapat memecahkan
  ECC 255-bit. Faktorisasi kuantum terbesar yang didemonstrasikan: ~50 bit.
  Kesenjangan: beberapa orde besaran.
- **Jangka pendek (2030-2035):** Peta jalan perangkat keras dari IBM, Google,
  Quantinuum menargetkan jutaan qubit. Implementasi dan set parameter ML-KEM
  akan matang.
- **Jangka menengah (2035-2050):** Sebagian besar perkiraan menempatkan
  kedatangan CRQC di jendela ini. Data HNDL yang dikumpulkan hari ini berisiko.
- **Jangka panjang (2050+):** Konsensus di antara kriptografer: komputer
  kuantum skala besar adalah masalah "kapan," bukan "apakah."

### Strategi yang Direkomendasikan

**1. Desain untuk kemampuan upgrade sekarang.** Pastikan format catatan
tersimpan, struct `TransmittedNoteCiphertext`, dan tata letak entri
BulkAppendTree memiliki versi dan dapat diperluas. Ini berbiaya rendah dan
mempertahankan opsi untuk menambahkan KEM hibrid nanti.

**2. Terapkan KEM hibrid ketika siap, buat wajib.** Jangan tawarkan dua pool
(klasik dan hibrid). Memisahkan set anonimitas mengalahkan tujuan transaksi
terlindung — pengguna yang bersembunyi di antara kelompok yang lebih kecil
kurang privat, bukan lebih. Ketika diterapkan, setiap note menggunakan skema
hibrid.

**3. Targetkan jendela 2028-2030.** Ini jauh sebelum ancaman kuantum realistis
mana pun tetapi setelah implementasi dan ukuran parameter ML-KEM telah stabil.
Ini juga memungkinkan pembelajaran dari pengalaman penerapan Zcash dan Signal.

**4. Pantau peristiwa pemicu:**
- NIST atau NSA mewajibkan tenggat migrasi pasca-kuantum
- Kemajuan signifikan dalam perangkat keras kuantum (>100.000 qubit fisik
  dengan koreksi kesalahan)
- Kemajuan kriptoanalitik terhadap masalah lattice (akan memengaruhi pilihan
  ML-KEM)

### Yang Tidak Memerlukan Tindakan Mendesak

| Komponen | Mengapa bisa ditunda |
|----------|----------------------|
| Tanda tangan otorisasi pengeluaran | Pemalsuan bersifat real-time, bukan retroaktif. Upgrade ke ML-DSA/SLH-DSA sebelum CRQC tiba. |
| Sistem bukti Halo 2 | Pemalsuan bukti bersifat real-time. Migrasi ke sistem berbasis STARK saat diperlukan. |
| Ketahanan kolisi Sinsemilla | Hanya berguna untuk serangan baru, bukan retroaktif. Tercakup oleh migrasi sistem bukti. |
| Infrastruktur GroveDB Merk/MMR/Blake3 | **Sudah aman kuantum.** Tidak diperlukan tindakan, sekarang atau nanti. |

## Referensi Alternatif Pasca-Kuantum

### Untuk Enkripsi (menggantikan ECDH)

| Skema | Tipe | Public key | Ciphertext | Kategori NIST | Catatan |
|-------|------|-----------|-----------|---------------|---------|
| ML-KEM-768 | Lattice (MLWE) | 1.184 B | 1.088 B | 3 (192-bit) | FIPS 203, standar industri |
| ML-KEM-512 | Lattice (MLWE) | 800 B | 768 B | 1 (128-bit) | Lebih kecil, margin lebih rendah |
| ML-KEM-1024 | Lattice (MLWE) | 1.568 B | 1.568 B | 5 (256-bit) | Berlebihan untuk hibrid |

### Untuk Tanda Tangan (menggantikan RedPallas/Schnorr)

| Skema | Tipe | Public key | Signature | Kategori NIST | Catatan |
|-------|------|-----------|----------|---------------|---------|
| ML-DSA-65 (Dilithium) | Lattice | 1.952 B | 3.293 B | 3 | FIPS 204, cepat |
| SLH-DSA (SPHINCS+) | Berbasis hash | 32-64 B | 7.856-49.856 B | 1-5 | FIPS 205, konservatif |
| XMSS/LMS | Berbasis hash (stateful) | 60 B | 2.500 B | bervariasi | Stateful — penggunaan ulang = pecah |

### Untuk Bukti ZK (menggantikan Halo 2)

| Sistem | Asumsi | Ukuran bukti | Pasca-kuantum | Catatan |
|--------|--------|--------------|---------------|---------|
| STARKs | Hash functions (collision resistance) | ~100-400 KB | **Ya** | Digunakan oleh StarkNet |
| Plonky3 | FRI (hash-based polynomial commitment) | ~50-200 KB | **Ya** | Pengembangan aktif |
| Halo 2 (saat ini) | ECDLP on Pasta curves | ~5 KB | **Tidak** | Sistem Orchard saat ini |
| Lattice SNARKs | MLWE | Riset | **Ya** | Belum siap produksi |

### Ekosistem Crate Rust

| Crate | Sumber | FIPS 203 | Terverifikasi | Catatan |
|-------|--------|----------|---------------|---------|
| `libcrux-ml-kem` | Cryspen | Ya | Terverifikasi secara formal (hax/F*) | Jaminan tertinggi |
| `ml-kem` | RustCrypto | Ya | Constant-time, belum diaudit | Kompatibilitas ekosistem |
| `fips203` | integritychain | Ya | Constant-time | Pure Rust, no_std |

## Ringkasan

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
