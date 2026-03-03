# Grove Hierarkis — Pohon dari Pohon-Pohon

## Bagaimana Subtree Bersarang di Dalam Pohon Induk

Fitur pembeda GroveDB adalah bahwa sebuah Merk tree dapat memuat element yang
merupakan Merk tree itu sendiri. Ini membuat sebuah **namespace hierarkis**:

```mermaid
graph TD
    subgraph root["MERK TREE ROOT — path: []"]
        contracts["&quot;contracts&quot;<br/>Tree"]
        identities["&quot;identities&quot;<br/>Tree"]
        balances["&quot;balances&quot;<br/>SumTree, sum=0"]
        contracts --> identities
        contracts --> balances
    end

    subgraph ident["MERK IDENTITIES — path: [&quot;identities&quot;]"]
        bob456["&quot;bob456&quot;<br/>Tree"]
        alice123["&quot;alice123&quot;<br/>Tree"]
        eve["&quot;eve&quot;<br/>Item"]
        bob456 --> alice123
        bob456 --> eve
    end

    subgraph bal["MERK BALANCES (SumTree) — path: [&quot;balances&quot;]"]
        bob_bal["&quot;bob456&quot;<br/>SumItem(800)"]
    end

    subgraph alice_tree["MERK ALICE123 — path: [&quot;identities&quot;, &quot;alice123&quot;]"]
        name["&quot;name&quot;<br/>Item(&quot;Al&quot;)"]
        balance_item["&quot;balance&quot;<br/>SumItem(1000)"]
        docs["&quot;docs&quot;<br/>Tree"]
        name --> balance_item
        name --> docs
    end

    identities -.-> bob456
    balances -.-> bob_bal
    alice123 -.-> name
    docs -.->|"... subtree lebih lanjut"| more["..."]

    style root fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style ident fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style bal fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style alice_tree fill:#e8daef,stroke:#8e44ad,stroke-width:2px
    style more fill:none,stroke:none
```

> Setiap kotak berwarna adalah Merk tree terpisah. Panah putus-putus merepresentasikan tautan portal dari element Tree ke Merk tree anak mereka. Path ke setiap Merk ditampilkan di labelnya.

## Sistem Pengalamatan Path

Setiap element dalam GroveDB dialamatkan oleh sebuah **path** — urutan string byte
yang menavigasi dari root melalui subtree ke key target:

```text
    Path: ["identities", "alice123", "name"]

    Langkah 1: Di pohon root, cari "identities" → element Tree
    Langkah 2: Buka subtree identities, cari "alice123" → element Tree
    Langkah 3: Buka subtree alice123, cari "name" → Item("Alice")
```

Path direpresentasikan sebagai `Vec<Vec<u8>>` atau menggunakan tipe `SubtreePath` untuk
manipulasi efisien tanpa alokasi:

```rust
// Path ke element (semua segmen kecuali yang terakhir)
let path: &[&[u8]] = &[b"identities", b"alice123"];
// Key di dalam subtree terakhir
let key: &[u8] = b"name";
```

## Generasi Prefiks Blake3 untuk Isolasi Penyimpanan

Setiap subtree dalam GroveDB mendapat **namespace penyimpanan terisolasi** sendiri di RocksDB.
Namespace ditentukan dengan meng-hash path menggunakan Blake3:

```rust
pub type SubtreePrefix = [u8; 32];

// Prefiks dihitung dengan meng-hash segmen path
// storage/src/rocksdb_storage/storage.rs
```

Contoh:

```text
    Path: ["identities", "alice123"]
    Prefiks: Blake3(["identities", "alice123"]) = [0xab, 0x3f, ...]  (32 byte)

    Di RocksDB, key untuk subtree ini disimpan sebagai:
    [prefix: 32 byte][original_key]

    Jadi "name" di subtree ini menjadi:
    [0xab, 0x3f, ...][0x6e, 0x61, 0x6d, 0x65]  ("name")
```

Ini memastikan:
- Tidak ada tabrakan key antar subtree (prefiks 32-byte = isolasi 256-bit)
- Komputasi prefiks yang efisien (satu hash Blake3 atas byte path)
- Data subtree berdekatan di RocksDB untuk efisiensi cache

## Propagasi Root Hash Melalui Hierarki

Ketika sebuah nilai berubah jauh di dalam grove, perubahan harus **merambat ke atas** untuk
memperbarui root hash:

```text
    Perubahan: Perbarui "name" menjadi "ALICE" di identities/alice123/

    Langkah 1: Perbarui value di Merk tree alice123
            → pohon alice123 mendapat root hash baru: H_alice_new

    Langkah 2: Perbarui element "alice123" di pohon identities
            → value_hash pohon identities untuk "alice123" =
              combine_hash(H(tree_element_bytes), H_alice_new)
            → pohon identities mendapat root hash baru: H_ident_new

    Langkah 3: Perbarui element "identities" di pohon root
            → value_hash pohon root untuk "identities" =
              combine_hash(H(tree_element_bytes), H_ident_new)
            → ROOT HASH berubah
```

```mermaid
graph TD
    subgraph step3["LANGKAH 3: Perbarui pohon root"]
        R3["Pohon root menghitung ulang:<br/>value_hash untuk &quot;identities&quot; =<br/>combine_hash(H(tree_bytes), H_ident_BARU)<br/>→ ROOT HASH baru"]
    end
    subgraph step2["LANGKAH 2: Perbarui pohon identities"]
        R2["Pohon identities menghitung ulang:<br/>value_hash untuk &quot;alice123&quot; =<br/>combine_hash(H(tree_bytes), H_alice_BARU)<br/>→ root hash baru: H_ident_BARU"]
    end
    subgraph step1["LANGKAH 1: Perbarui Merk alice123"]
        R1["Pohon alice123 menghitung ulang:<br/>value_hash(&quot;ALICE&quot;) → kv_hash baru<br/>→ root hash baru: H_alice_BARU"]
    end

    R1 -->|"H_alice_BARU mengalir ke atas"| R2
    R2 -->|"H_ident_BARU mengalir ke atas"| R3

    style step1 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style step2 fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style step3 fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

**Sebelum vs Sesudah** — node yang berubah ditandai dengan merah:

```mermaid
graph TD
    subgraph before["SEBELUM"]
        B_root["Root: aabb1122"]
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

    subgraph after["SESUDAH"]
        A_root["Root: ff990033"]
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

> Hanya node pada jalur dari nilai yang diubah ke root yang dihitung ulang. Saudara dan cabang lain tetap tidak berubah.

Propagasi diimplementasikan oleh `propagate_changes_with_transaction`, yang berjalan
naik di sepanjang path dari subtree yang dimodifikasi ke root, memperbarui hash element
setiap induk di sepanjang jalan.

## Contoh Struktur Grove Multi-Level

Berikut contoh lengkap yang menunjukkan bagaimana Dash Platform menyusun state-nya:

```mermaid
graph TD
    ROOT["GroveDB Root"]

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

Setiap kotak adalah Merk tree terpisah, terotentikasi sampai ke satu root
hash yang disetujui oleh validator.

---
