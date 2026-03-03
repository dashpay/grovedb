# Hiyerarsik Grove -- Agaclar Agaci

## Alt Agaclar Ust Agaclarin Icine Nasil Yerlestir

GroveDB'nin belirleyici ozelligi, bir Merk agacinin kendisi de Merk agaci olan elementler icerebilmesidir. Bu, bir **hiyerarsik ad alani** olusturur:

```mermaid
graph TD
    subgraph root["KOK MERK AGACI — yol: []"]
        contracts["&quot;contracts&quot;<br/>Tree"]
        identities["&quot;identities&quot;<br/>Tree"]
        balances["&quot;balances&quot;<br/>SumTree, sum=0"]
        contracts --> identities
        contracts --> balances
    end

    subgraph ident["IDENTITIES MERK — yol: [&quot;identities&quot;]"]
        bob456["&quot;bob456&quot;<br/>Tree"]
        alice123["&quot;alice123&quot;<br/>Tree"]
        eve["&quot;eve&quot;<br/>Item"]
        bob456 --> alice123
        bob456 --> eve
    end

    subgraph bal["BALANCES MERK (SumTree) — yol: [&quot;balances&quot;]"]
        bob_bal["&quot;bob456&quot;<br/>SumItem(800)"]
    end

    subgraph alice_tree["ALICE123 MERK — yol: [&quot;identities&quot;, &quot;alice123&quot;]"]
        name["&quot;name&quot;<br/>Item(&quot;Al&quot;)"]
        balance_item["&quot;balance&quot;<br/>SumItem(1000)"]
        docs["&quot;docs&quot;<br/>Tree"]
        name --> balance_item
        name --> docs
    end

    identities -.-> bob456
    balances -.-> bob_bal
    alice123 -.-> name
    docs -.->|"... daha fazla alt agac"| more["..."]

    style root fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style ident fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style bal fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style alice_tree fill:#e8daef,stroke:#8e44ad,stroke-width:2px
    style more fill:none,stroke:none
```

> Her renkli kutu ayri bir Merk agacidir. Kesikli oklar, Tree elementlerinden cocuk Merk agaclarina olan gecit baglantilarini temsil eder. Her Merk'in yolu etiketinde gosterilir.

## Yol Adresleme Sistemi

GroveDB'deki her element, bir **yol** (path) ile adreslenir -- kok agactan alt agaclar boyunca hedef anahtara ulasan bir bayt dizileri serisi:

```text
    Yol: ["identities", "alice123", "name"]

    Adim 1: Kok agacta "identities" ara → Tree elementi
    Adim 2: identities alt agacini ac, "alice123" ara → Tree elementi
    Adim 3: alice123 alt agacini ac, "name" ara → Item("Alice")
```

Yollar `Vec<Vec<u8>>` olarak veya bellek ayirmadan verimli manipulasyon icin `SubtreePath` tipi kullanilarak temsil edilir:

```rust
// Elemente giden yol (son segment haric tum segmentler)
let path: &[&[u8]] = &[b"identities", b"alice123"];
// Son alt agac icindeki anahtar
let key: &[u8] = b"name";
```

## Depolama Izolasyonu icin Blake3 Onek Uretimi

GroveDB'deki her alt agac, RocksDB icinde kendi **izole depolama ad alanini** alir. Ad alani, yolun Blake3 ile hashlenmesiyle belirlenir:

```rust
pub type SubtreePrefix = [u8; 32];

// Onek, yol segmentlerinin hashlenmesiyle hesaplanir
// storage/src/rocksdb_storage/storage.rs
```

Ornegin:

```text
    Yol: ["identities", "alice123"]
    Onek: Blake3(["identities", "alice123"]) = [0xab, 0x3f, ...]  (32 bayt)

    RocksDB'de bu alt agacin anahtarlari su sekilde depolanir:
    [onek: 32 bayt][orijinal_anahtar]

    Yani bu alt agactaki "name" su olur:
    [0xab, 0x3f, ...][0x6e, 0x61, 0x6d, 0x65]  ("name")
```

Bu, sunlari saglar:
- Alt agaclar arasinda anahtar carpismalari yok (32 bayt onek = 256 bit izolasyon)
- Verimli onek hesaplamasi (yol baytlari uzerinde tek Blake3 hash'i)
- Alt agac verileri onbellek verimliligi icin RocksDB'de bir arada bulunur

## Hiyerarsi Boyunca Kok Hash Yayilimi

Bir deger grove'un derinliklerinde degistiginde, degisiklik kok hash'i guncellemek icin **yukari dogru yayilmalidir**:

```text
    Degisiklik: identities/alice123/ icinde "name" degerini "ALICE" olarak guncelle

    Adim 1: alice123'un Merk agacinda degeri guncelle
            → alice123 agaci yeni kok hash alir: H_alice_yeni

    Adim 2: identities agacinda "alice123" elementini guncelle
            → identities agacinin "alice123" icin value_hash'i =
              combine_hash(H(tree_element_baytlari), H_alice_yeni)
            → identities agaci yeni kok hash alir: H_ident_yeni

    Adim 3: Kok agacta "identities" elementini guncelle
            → kok agacin "identities" icin value_hash'i =
              combine_hash(H(tree_element_baytlari), H_ident_yeni)
            → KOK HASH degisir
```

```mermaid
graph TD
    subgraph step3["ADIM 3: Kok agaci guncelle"]
        R3["Kok agac yeniden hesaplar:<br/>value_hash for &quot;identities&quot; =<br/>combine_hash(H(tree_bytes), H_ident_YENI)<br/>→ yeni KOK HASH"]
    end
    subgraph step2["ADIM 2: identities agacini guncelle"]
        R2["identities agaci yeniden hesaplar:<br/>value_hash for &quot;alice123&quot; =<br/>combine_hash(H(tree_bytes), H_alice_YENI)<br/>→ yeni kok hash: H_ident_YENI"]
    end
    subgraph step1["ADIM 1: alice123 Merk'ini guncelle"]
        R1["alice123 agaci yeniden hesaplar:<br/>value_hash(&quot;ALICE&quot;) → yeni kv_hash<br/>→ yeni kok hash: H_alice_YENI"]
    end

    R1 -->|"H_alice_YENI yukari akar"| R2
    R2 -->|"H_ident_YENI yukari akar"| R3

    style step1 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style step2 fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style step3 fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

**Once ve Sonra** -- degisen dugumler kirmiziyla isaretli:

```mermaid
graph TD
    subgraph before["ONCE"]
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

    subgraph after["SONRA"]
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

> Yalnizca degisen degerden koke giden yol uzerindeki dugumler yeniden hesaplanir. Kardes dugumler ve diger dallar degismeden kalir.

Yayilim, degistirilen alt agactan koke kadar yolu yuruyen ve her ust elementin hash'ini guncelleyen `propagate_changes_with_transaction` tarafindan uygulanir.

## Cok Katmanli Grove Yapi Ornegi

Dash Platform'un durumunu nasil yapilandirdigini gosteren eksiksiz bir ornek:

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

Her kutu, dogrulayicilarin (validator) uzerinde uzlastigi tek bir kok hash'e kadar dogrulanan ayri bir Merk agacidir.

---
