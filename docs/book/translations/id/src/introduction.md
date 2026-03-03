# Pendahuluan — Apa itu GroveDB?

## Ide Inti

GroveDB adalah sebuah **struktur data terotentikasi hierarkis** — pada dasarnya sebuah *grove*
(pohon dari pohon-pohon) yang dibangun di atas pohon Merkle AVL. Setiap node dalam database
merupakan bagian dari pohon yang terotentikasi secara kriptografis, dan setiap pohon dapat
memuat pohon-pohon lain sebagai anak, membentuk hierarki mendalam dari state yang dapat diverifikasi.

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

> Setiap kotak berwarna adalah **Merk tree terpisah**. Panah putus-putus menunjukkan hubungan subtree — sebuah element Tree di induk berisi root key dari Merk anak.

Dalam database tradisional, Anda mungkin menyimpan data di penyimpanan key-value datar dengan
satu pohon Merkle di atasnya untuk otentikasi. GroveDB mengambil pendekatan berbeda:
ia menyarangkan pohon Merkle di dalam pohon Merkle. Ini memberikan Anda:

1. **Indeks sekunder yang efisien** — query berdasarkan path apa pun, bukan hanya primary key
2. **Proof (bukti) kriptografis yang ringkas** — membuktikan keberadaan (atau ketiadaan) data apa pun
3. **Data agregat** — pohon dapat secara otomatis menjumlahkan, menghitung, atau mengagregasi
   anak-anaknya
4. **Operasi atomik lintas-pohon** — operasi batch mencakup beberapa subtree

## Mengapa GroveDB Ada

GroveDB dirancang untuk **Dash Platform**, sebuah platform aplikasi terdesentralisasi
di mana setiap bagian state harus:

- **Terotentikasi**: Setiap node dapat membuktikan bagian state apa pun ke light client
- **Deterministik**: Setiap node menghitung root state yang persis sama
- **Efisien**: Operasi harus selesai dalam batasan waktu blok
- **Dapat di-query**: Aplikasi membutuhkan query yang kaya, bukan hanya pencarian key

Pendekatan tradisional memiliki kekurangan:

| Pendekatan | Masalah |
|----------|---------|
| Plain Merkle Tree | Hanya mendukung pencarian key, tidak ada range query |
| Ethereum MPT | Rebalancing mahal, ukuran proof besar |
| Key-value datar + satu pohon | Tidak ada query hierarkis, satu proof mencakup semuanya |
| B-tree | Tidak secara alami ter-Merkle-kan, otentikasi rumit |

GroveDB mengatasi ini dengan menggabungkan **jaminan keseimbangan yang terbukti dari pohon AVL**
dengan **penyarangan hierarkis** dan **sistem tipe element yang kaya**.

## Gambaran Arsitektur

GroveDB diorganisasi menjadi lapisan-lapisan yang berbeda, masing-masing dengan tanggung jawab yang jelas:

```mermaid
graph TD
    APP["<b>Lapisan Aplikasi</b><br/>Dash Platform, dll.<br/><code>insert() get() query() prove() apply_batch()</code>"]

    GROVE["<b>GroveDB Core</b> — <code>grovedb/src/</code><br/>Manajemen subtree hierarkis · Sistem tipe element<br/>Resolusi referensi · Operasi batch · Proof multi-lapisan"]

    MERK["<b>Lapisan Merk</b> — <code>merk/src/</code><br/>Pohon Merkle AVL · Rotasi penyeimbangan mandiri<br/>Sistem link · Hashing Blake3 · Encoding proof"]

    STORAGE["<b>Lapisan Penyimpanan</b> — <code>storage/src/</code><br/>RocksDB OptimisticTransactionDB<br/>4 column family · Isolasi prefiks Blake3 · Penulisan batch"]

    COST["<b>Lapisan Biaya</b> — <code>costs/src/</code><br/>Pelacakan OperationCost · Monad CostContext<br/>Estimasi worst-case &amp; average-case"]

    APP ==>|"tulis ↓"| GROVE
    GROVE ==>|"operasi pohon"| MERK
    MERK ==>|"I/O disk"| STORAGE
    STORAGE -.->|"akumulasi biaya ↑"| COST
    COST -.-> MERK
    MERK -.-> GROVE
    GROVE ==>|"baca ↑"| APP

    style APP fill:#f5f5f5,stroke:#999,stroke-width:1px
    style GROVE fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style MERK fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style STORAGE fill:#fdebd0,stroke:#e67e22,stroke-width:2px
    style COST fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

Data mengalir **ke bawah** melalui lapisan-lapisan ini saat penulisan dan **ke atas** saat pembacaan.
Setiap operasi mengakumulasi biaya saat melewati tumpukan, memungkinkan
akuntansi sumber daya yang presisi.

---
