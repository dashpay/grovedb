# Sistem Referensi

## Mengapa Referensi Ada

Dalam database hierarkis, Anda sering memerlukan data yang sama dapat diakses dari beberapa
path. Misalnya, dokumen mungkin disimpan di bawah kontrak mereka tetapi juga
dapat di-query berdasarkan identitas pemilik. **Referensi** adalah jawaban GroveDB — mereka adalah
pointer dari satu lokasi ke lokasi lain, mirip dengan symbolic link di filesystem.

```mermaid
graph LR
    subgraph primary["Penyimpanan Utama"]
        item["contracts/C1/docs/D1<br/><b>Item</b>(data)"]
    end
    subgraph secondary["Indeks Sekunder"]
        ref["identities/alice/docs/D1<br/><b>Reference</b>"]
    end
    ref -->|"menunjuk ke"| item

    style primary fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style secondary fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style ref fill:#fef9e7,stroke:#f39c12,stroke-width:2px
```

Properti kunci:
- Referensi **terotentikasi** — value_hash referensi mencakup baik
  referensi itu sendiri maupun element yang direferensikan
- Referensi dapat **dirantai** — sebuah referensi dapat menunjuk ke referensi lain
- Deteksi siklus mencegah loop tak terbatas
- Batas hop yang dapat dikonfigurasi mencegah kelelahan sumber daya

## Tujuh Tipe Referensi

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

Mari kita bahas masing-masing dengan diagram.

### AbsolutePathReference

Tipe paling sederhana. Menyimpan path lengkap ke target:

```mermaid
graph TD
    subgraph root["Root Merk — path: []"]
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
    X ==>|"resolusi ke [P, Q, R]"| R

    style merkAB fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style merkPQ fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style X fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style R fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

> X menyimpan path absolut lengkap `[P, Q, R]`. Di mana pun X berada, ia selalu ter-resolve ke target yang sama.

### UpstreamRootHeightReference

Mempertahankan N segmen pertama dari path saat ini, lalu menambahkan path baru:

```mermaid
graph TD
    subgraph resolve["Resolusi: pertahankan 2 segmen pertama + tambahkan [P, Q]"]
        direction LR
        curr["saat ini: [A, B, C, D]"] --> keep["pertahankan 2 pertama: [A, B]"] --> append["tambahkan: [A, B, <b>P, Q</b>]"]
    end

    subgraph grove["Hierarki Grove"]
        gA["A (tinggi 0)"]
        gB["B (tinggi 1)"]
        gC["C (tinggi 2)"]
        gD["D (tinggi 3)"]
        gX["X = Reference<br/>UpstreamRootHeight(2, [P,Q])"]
        gP["P (tinggi 2)"]
        gQ["Q (tinggi 3) — target"]

        gA --> gB
        gB --> gC
        gB -->|"pertahankan 2 pertama → [A,B]<br/>lalu turun [P,Q]"| gP
        gC --> gD
        gD -.-> gX
        gP --> gQ
    end

    gX ==>|"resolusi ke"| gQ

    style resolve fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style gX fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style gQ fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

### UpstreamRootHeightWithParentPathAdditionReference

Seperti UpstreamRootHeight, tapi menambahkan kembali segmen terakhir dari path saat ini:

```text
    Referensi di path [A, B, C, D, E] key=X
    UpstreamRootHeightWithParentPathAdditionReference(2, [P, Q])

    Path saat ini:       [A, B, C, D, E]
    Pertahankan 2 pertama: [A, B]
    Tambahkan [P, Q]:    [A, B, P, Q]
    Tambahkan ulang terakhir: [A, B, P, Q, E]   ← "E" dari path asli ditambahkan kembali

    Berguna untuk: indeks di mana key induk harus dipertahankan
```

### UpstreamFromElementHeightReference

Membuang N segmen terakhir, lalu menambahkan:

```text
    Referensi di path [A, B, C, D] key=X
    UpstreamFromElementHeightReference(1, [P, Q])

    Path saat ini:      [A, B, C, D]
    Buang 1 terakhir:   [A, B, C]
    Tambahkan [P, Q]:   [A, B, C, P, Q]
```

### CousinReference

Mengganti hanya induk langsung dengan key baru:

```mermaid
graph TD
    subgraph resolve["Resolusi: pop 2 terakhir, push cousin C, push key X"]
        direction LR
        r1["path: [A, B, M, D]"] --> r2["pop 2 terakhir: [A, B]"] --> r3["push C: [A, B, C]"] --> r4["push key X: [A, B, C, X]"]
    end

    subgraph merkAB["Merk [A, B]"]
        M["M<br/>Tree"]
        C["C<br/>Tree<br/>(cousin dari M)"]
    end

    subgraph merkABM["Merk [A, B, M]"]
        D["D<br/>Tree"]
    end

    subgraph merkABMD["Merk [A, B, M, D]"]
        Xref["X = Reference<br/>CousinReference(C)"]
    end

    subgraph merkABC["Merk [A, B, C]"]
        Xtarget["X = Item<br/>(target)"]
    end

    M -.-> D
    D -.-> Xref
    C -.-> Xtarget
    Xref ==>|"resolusi ke [A, B, C, X]"| Xtarget

    style resolve fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style Xref fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style Xtarget fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style M fill:#fadbd8,stroke:#e74c3c
    style C fill:#d5f5e3,stroke:#27ae60
```

> "Cousin" adalah subtree saudara dari kakek referensi. Referensi menavigasi naik dua level, lalu turun ke subtree cousin.

### RemovedCousinReference

Seperti CousinReference tapi mengganti induk dengan path multi-segmen:

```text
    Referensi di path [A, B, C, D] key=X
    RemovedCousinReference([M, N])

    Path saat ini:   [A, B, C, D]
    Pop induk C:     [A, B]
    Tambahkan [M, N]: [A, B, M, N]
    Push key X:      [A, B, M, N, X]
```

### SiblingReference

Referensi relatif paling sederhana — hanya mengubah key dalam induk yang sama:

```mermaid
graph TD
    subgraph merk["Merk [A, B, C] — pohon yang sama, path yang sama"]
        M_sib["M"]
        X_sib["X = Reference<br/>SiblingRef(Y)"]
        Y_sib["Y = Item<br/>(target)"]
        Z_sib["Z = Item"]
        M_sib --> X_sib
        M_sib --> Y_sib
    end

    X_sib ==>|"resolusi ke [A, B, C, Y]"| Y_sib

    style merk fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style X_sib fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style Y_sib fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

> Tipe referensi paling sederhana. X dan Y adalah saudara dalam Merk tree yang sama — resolusi hanya mengubah key sambil mempertahankan path yang sama.

## Penyusuran Referensi dan Batas Hop

Ketika GroveDB menemukan element Reference, ia harus **mengikutinya** untuk menemukan
value sebenarnya. Karena referensi dapat menunjuk ke referensi lain, ini melibatkan loop:

```rust
// grovedb/src/reference_path.rs
pub const MAX_REFERENCE_HOPS: usize = 10;

pub fn follow_reference(...) -> CostResult<ResolvedReference, Error> {
    let mut hops_left = MAX_REFERENCE_HOPS;
    let mut visited = HashSet::new();

    while hops_left > 0 {
        // Resolve path referensi ke path absolut
        let target_path = current_ref.absolute_qualified_path(...);

        // Periksa siklus
        if !visited.insert(target_path.clone()) {
            return Err(Error::CyclicReference);
        }

        // Ambil element di target
        let element = Element::get(target_path);

        match element {
            Element::Reference(next_ref, ..) => {
                // Masih referensi — terus ikuti
                current_ref = next_ref;
                hops_left -= 1;
            }
            other => {
                // Menemukan element sebenarnya!
                return Ok(ResolvedReference { element: other, ... });
            }
        }
    }

    Err(Error::ReferenceLimit)  // Melebihi 10 hop
}
```

## Deteksi Siklus

HashSet `visited` melacak semua path yang sudah dikunjungi. Jika kita menemukan path yang sudah
pernah dikunjungi, kita memiliki siklus:

```mermaid
graph LR
    A["A<br/>Reference"] -->|"langkah 1"| B["B<br/>Reference"]
    B -->|"langkah 2"| C["C<br/>Reference"]
    C -->|"langkah 3"| A

    style A fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style B fill:#fef9e7,stroke:#f39c12
    style C fill:#fef9e7,stroke:#f39c12
```

> **Jejak deteksi siklus:**
>
> | Langkah | Ikuti | Set visited | Hasil |
> |------|--------|-------------|--------|
> | 1 | Mulai di A | { A } | A adalah Ref → ikuti |
> | 2 | A → B | { A, B } | B adalah Ref → ikuti |
> | 3 | B → C | { A, B, C } | C adalah Ref → ikuti |
> | 4 | C → A | A sudah ada di visited! | **Error::CyclicRef** |
>
> Tanpa deteksi siklus, ini akan loop selamanya. `MAX_REFERENCE_HOPS = 10` juga membatasi kedalaman traversal untuk rantai panjang.

## Referensi dalam Merk — Hash Value Gabungan

Ketika sebuah Reference disimpan di Merk tree, `value_hash`-nya harus mengotentikasi
baik struktur referensi maupun data yang direferensikan:

```rust
// merk/src/tree/kv.rs
pub fn update_hashes_using_reference_value_hash(
    mut self,
    reference_value_hash: CryptoHash,
) -> CostContext<Self> {
    // Hash byte element referensi itu sendiri
    let actual_value_hash = value_hash(self.value_as_slice());

    // Gabungkan: H(reference_bytes) ⊕ H(referenced_data)
    let combined = combine_hash(&actual_value_hash, &reference_value_hash);

    self.value_hash = combined;
    self.hash = kv_digest_to_kv_hash(self.key(), self.value_hash());
    // ...
}
```

Ini berarti mengubah referensi itu sendiri ATAU data yang ditunjuknya akan
mengubah root hash — keduanya terikat secara kriptografis.

---
