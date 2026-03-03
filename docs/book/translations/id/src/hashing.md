# Hashing — Integritas Kriptografis

Setiap node dalam Merk tree di-hash untuk menghasilkan sebuah **root hash** — satu nilai
32-byte yang mengotentikasi seluruh pohon. Perubahan apa pun pada key, value, atau
hubungan struktural akan menghasilkan root hash yang berbeda.

## Hierarki Hash Tiga Tingkat

Merk menggunakan skema hashing tiga tingkat, dari paling dalam ke paling luar:

Contoh: key = `"bob"` (3 byte), value = `"hello"` (5 byte):

```mermaid
graph LR
    subgraph level1["Tingkat 1: value_hash"]
        V_IN["varint(5) ‖ &quot;hello&quot;"]
        V_BLAKE["Blake3"]
        V_OUT(["value_hash<br/><small>32 byte</small>"])
        V_IN --> V_BLAKE --> V_OUT
    end

    subgraph level2["Tingkat 2: kv_hash"]
        K_IN["varint(3) ‖ &quot;bob&quot; ‖ value_hash"]
        K_BLAKE["Blake3"]
        K_OUT(["kv_hash<br/><small>32 byte</small>"])
        K_IN --> K_BLAKE --> K_OUT
    end

    subgraph level3["Tingkat 3: node_hash"]
        N_LEFT(["left_child_hash<br/><small>32B (atau NULL_HASH)</small>"])
        N_KV(["kv_hash<br/><small>32B</small>"])
        N_RIGHT(["right_child_hash<br/><small>32B (atau NULL_HASH)</small>"])
        N_BLAKE["Blake3<br/><small>input 96B = 2 blok</small>"]
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

> ROOT dari pohon = `node_hash` dari node root — mengotentikasi **setiap** key, value, dan hubungan struktural. Anak yang hilang menggunakan `NULL_HASH = [0x00; 32]`.

### Tingkat 1: value_hash

```rust
// merk/src/tree/hash.rs
pub fn value_hash(value: &[u8]) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    let val_length = value.len().encode_var_vec();  // Encoding varint
    hasher.update(val_length.as_slice());
    hasher.update(value);
    // ...
}
```

Panjang value di-encode sebagai **varint** dan diawali. Ini kritis untuk
ketahanan terhadap tabrakan (collision resistance) — tanpanya, `H("AB" ‖ "C")` akan sama dengan `H("A" ‖ "BC")`.

### Tingkat 2: kv_hash

```rust
pub fn kv_hash(key: &[u8], value: &[u8]) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    let key_length = key.len().encode_var_vec();
    hasher.update(key_length.as_slice());
    hasher.update(key);
    let vh = value_hash(value);
    hasher.update(vh.as_slice());  // Hash bersarang
    // ...
}
```

Ini mengikat key ke value. Untuk verifikasi proof, ada juga varian
yang menerima value_hash yang sudah dihitung sebelumnya:

```rust
pub fn kv_digest_to_kv_hash(key: &[u8], value_hash: &CryptoHash) -> CostContext<CryptoHash>
```

Ini digunakan ketika verifier sudah memiliki value_hash (misalnya, untuk subtree
di mana value_hash adalah hash gabungan).

### Tingkat 3: node_hash

```rust
pub fn node_hash(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);       // 32 byte
    hasher.update(left);     // 32 byte
    hasher.update(right);    // 32 byte — total 96 byte
    // Selalu tepat 2 operasi hash (96 byte / blok 64-byte = 2)
}
```

Jika anak tidak ada, hash-nya adalah **NULL_HASH** — 32 byte nol:

```rust
pub const NULL_HASH: CryptoHash = [0; HASH_LENGTH];  // [0u8; 32]
```

## Blake3 sebagai Fungsi Hash

GroveDB menggunakan **Blake3** untuk semua hashing. Properti kunci:

- **Output 256-bit** (32 byte)
- **Ukuran blok**: 64 byte
- **Kecepatan**: ~3x lebih cepat dari SHA-256 pada perangkat keras modern
- **Streaming**: Dapat memasukkan data secara inkremental

Biaya operasi hash dihitung berdasarkan berapa banyak blok 64-byte yang
diproses:

```rust
let hashes = 1 + (hasher.count() - 1) / 64;  // Jumlah operasi hash
```

## Encoding Awalan Panjang untuk Ketahanan Tabrakan

Setiap input dengan panjang variabel diawali dengan panjangnya menggunakan **encoding varint**:

```mermaid
graph LR
    subgraph bad["Tanpa awalan panjang — RENTAN"]
        BAD1["H(&quot;AB&quot; + &quot;C&quot;) = H(0x41 0x42 0x43)"]
        BAD2["H(&quot;A&quot; + &quot;BC&quot;) = H(0x41 0x42 0x43)"]
        BAD1 --- SAME["HASH SAMA!"]
        BAD2 --- SAME
    end

    subgraph good["Dengan awalan panjang — tahan tabrakan"]
        GOOD1["H([02] 0x41 0x42)<br/><small>varint(2) ‖ &quot;AB&quot;</small>"]
        GOOD2["H([03] 0x41 0x42 0x43)<br/><small>varint(3) ‖ &quot;ABC&quot;</small>"]
        GOOD1 --- DIFF["BERBEDA"]
        GOOD2 --- DIFF
    end

    style bad fill:#fadbd8,stroke:#e74c3c
    style good fill:#d5f5e3,stroke:#27ae60
    style SAME fill:#e74c3c,color:#fff,stroke:#c0392b
    style DIFF fill:#27ae60,color:#fff,stroke:#229954
```

> **Input value_hash**: `[varint(value.len)] [byte value]`
> **Input kv_hash**: `[varint(key.len)] [byte key] [value_hash: 32 byte]`

Tanpa awalan panjang, penyerang bisa membuat pasangan key-value berbeda yang
menghasilkan digest yang sama. Awalan panjang membuat ini tidak mungkin secara kriptografis.

## Hashing Gabungan untuk Element Khusus

Untuk **subtree** dan **referensi**, `value_hash` bukan sekadar `H(value)`.
Sebaliknya, ini adalah **hash gabungan** (combined hash) yang mengikat element ke targetnya:

```mermaid
graph LR
    subgraph item["Item Biasa"]
        I_val["byte value"] --> I_hash["H(len ‖ bytes)"] --> I_vh(["value_hash"])
    end

    subgraph subtree["Element Subtree"]
        S_elem["byte element tree"] --> S_hash1["H(len ‖ bytes)"]
        S_root(["root hash<br/>Merk anak"])
        S_hash1 --> S_combine["combine_hash()<br/><small>Blake3(a ‖ b)</small>"]
        S_root --> S_combine
        S_combine --> S_vh(["value_hash"])
    end

    subgraph reference["Element Referensi"]
        R_elem["byte element ref"] --> R_hash1["H(len ‖ bytes)"]
        R_target["value target"] --> R_hash2["H(len ‖ bytes)"]
        R_hash1 --> R_combine["combine_hash()"]
        R_hash2 --> R_combine
        R_combine --> R_vh(["value_hash"])
    end

    style item fill:#eaf2f8,stroke:#2980b9
    style subtree fill:#fef9e7,stroke:#f39c12
    style reference fill:#fdedec,stroke:#e74c3c
```

> **Subtree:** mengikat root hash Merk anak ke induk. **Referensi:** mengikat baik path referensi DAN value target. Mengubah salah satu mengubah root hash.

Fungsi `combine_hash`:

```rust
pub fn combine_hash(hash_one: &CryptoHash, hash_two: &CryptoHash) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(hash_one);   // 32 byte
    hasher.update(hash_two);   // 32 byte — total 64 byte, tepat 1 operasi hash
    // ...
}
```

Inilah yang memungkinkan GroveDB mengotentikasi seluruh hierarki melalui satu
root hash — setiap value_hash pohon induk untuk element subtree menyertakan
root hash pohon anak.

## Hashing Agregat untuk ProvableCountTree

Node `ProvableCountTree` menyertakan hitungan agregat dalam node hash:

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
    hasher.update(&count.to_be_bytes());      // 8 byte — total 104 byte
    // Tetap tepat 2 operasi hash (104 < 128 = 2 × 64)
}
```

Ini berarti proof hitungan tidak memerlukan pengungkapan data aktual — hitungan
sudah tertanam dalam komitmen kriptografis.

---
