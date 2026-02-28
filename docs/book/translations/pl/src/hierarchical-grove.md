# Hierarchiczny gaj -- Drzewo drzew

## Jak poddrzewa zaglezdzaja sie wewnatrz drzew nadrzednych

Definiujaca cecha GroveDB jest to, ze drzewo Merk moze zawierac elementy, ktore
same sa drzewami Merk. Tworzy to **hierarchiczna przestrzen nazw**:

```mermaid
graph TD
    subgraph root["KORZENIOWE DRZEWO MERK — sciezka: []"]
        contracts["&quot;contracts&quot;<br/>Tree"]
        identities["&quot;identities&quot;<br/>Tree"]
        balances["&quot;balances&quot;<br/>SumTree, sum=0"]
        contracts --> identities
        contracts --> balances
    end

    subgraph ident["DRZEWO MERK IDENTITIES — sciezka: [&quot;identities&quot;]"]
        bob456["&quot;bob456&quot;<br/>Tree"]
        alice123["&quot;alice123&quot;<br/>Tree"]
        eve["&quot;eve&quot;<br/>Item"]
        bob456 --> alice123
        bob456 --> eve
    end

    subgraph bal["DRZEWO MERK BALANCES (SumTree) — sciezka: [&quot;balances&quot;]"]
        bob_bal["&quot;bob456&quot;<br/>SumItem(800)"]
    end

    subgraph alice_tree["DRZEWO MERK ALICE123 — sciezka: [&quot;identities&quot;, &quot;alice123&quot;]"]
        name["&quot;name&quot;<br/>Item(&quot;Al&quot;)"]
        balance_item["&quot;balance&quot;<br/>SumItem(1000)"]
        docs["&quot;docs&quot;<br/>Tree"]
        name --> balance_item
        name --> docs
    end

    identities -.-> bob456
    balances -.-> bob_bal
    alice123 -.-> name
    docs -.->|"... wiecej poddrzew"| more["..."]

    style root fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style ident fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style bal fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style alice_tree fill:#e8daef,stroke:#8e44ad,stroke-width:2px
    style more fill:none,stroke:none
```

> Kazde kolorowe pole to oddzielne drzewo Merk. Przerywane strzalki reprezentuja lacza portalowe z elementow Tree do ich potomnych drzew Merk. Sciezka do kazdego drzewa Merk jest pokazana w jego etykiecie.

## System adresowania sciezkowego

Kazdy element w GroveDB jest adresowany przez **sciezke** -- sekwencje ciagow
bajtow nawigujacych od korzenia przez poddrzewa do docelowego klucza:

```text
    Sciezka: ["identities", "alice123", "name"]

    Krok 1: W korzeniowym drzewie, wyszukaj "identities" → element Tree
    Krok 2: Otworz poddrzewo identities, wyszukaj "alice123" → element Tree
    Krok 3: Otworz poddrzewo alice123, wyszukaj "name" → Item("Alice")
```

Sciezki sa reprezentowane jako `Vec<Vec<u8>>` lub przy uzyciu typu `SubtreePath`
dla wydajnej manipulacji bez alokacji:

```rust
// Sciezka do elementu (wszystkie segmenty oprocz ostatniego)
let path: &[&[u8]] = &[b"identities", b"alice123"];
// Klucz w ostatnim poddrzewie
let key: &[u8] = b"name";
```

## Generowanie prefiksow Blake3 dla izolacji magazynu

Kazde poddrzewo w GroveDB otrzymuje swoja wlasna **izolowana przestrzen nazw magazynu**
w RocksDB. Przestrzen nazw jest okreslana przez haszowanie sciezki za pomoca Blake3:

```rust
pub type SubtreePrefix = [u8; 32];

// Prefiks jest obliczany przez haszowanie segmentow sciezki
// storage/src/rocksdb_storage/storage.rs
```

Na przyklad:

```text
    Sciezka: ["identities", "alice123"]
    Prefiks: Blake3(["identities", "alice123"]) = [0xab, 0x3f, ...]  (32 bajty)

    W RocksDB klucze dla tego poddrzewa sa przechowywane jako:
    [prefiks: 32 bajty][oryginalny_klucz]

    Wiec "name" w tym poddrzewie staje sie:
    [0xab, 0x3f, ...][0x6e, 0x61, 0x6d, 0x65]  ("name")
```

To zapewnia:
- Brak kolizji kluczy miedzy poddrzewami (32-bajtowy prefiks = 256-bitowa izolacja)
- Wydajne obliczanie prefiksow (pojedyncze haszowanie Blake3 po bajtach sciezki)
- Dane poddrzewa sa kolokowane w RocksDB dla wydajnosci cache

## Propagacja hasza korzenia w gore hierarchii

Gdy wartosc zmienia sie gleboko w gaju, zmiana musi **propagowac sie w gore**,
aby zaktualizowac hasz korzenia:

```text
    Zmiana: Aktualizacja "name" na "ALICE" w identities/alice123/

    Krok 1: Aktualizacja wartosci w drzewie Merk alice123
            → drzewo alice123 otrzymuje nowy hasz korzenia: H_alice_new

    Krok 2: Aktualizacja elementu "alice123" w drzewie identities
            → value_hash drzewa identities dla "alice123" =
              combine_hash(H(tree_element_bytes), H_alice_new)
            → drzewo identities otrzymuje nowy hasz korzenia: H_ident_new

    Krok 3: Aktualizacja elementu "identities" w korzeniowym drzewie
            → value_hash korzeniowego drzewa dla "identities" =
              combine_hash(H(tree_element_bytes), H_ident_new)
            → HASZ KORZENIA zmienia sie
```

```mermaid
graph TD
    subgraph step3["KROK 3: Aktualizacja korzeniowego drzewa"]
        R3["Korzeniowe drzewo przelicza:<br/>value_hash dla &quot;identities&quot; =<br/>combine_hash(H(tree_bytes), H_ident_NOWY)<br/>→ nowy HASZ KORZENIA"]
    end
    subgraph step2["KROK 2: Aktualizacja drzewa identities"]
        R2["Drzewo identities przelicza:<br/>value_hash dla &quot;alice123&quot; =<br/>combine_hash(H(tree_bytes), H_alice_NOWY)<br/>→ nowy hasz korzenia: H_ident_NOWY"]
    end
    subgraph step1["KROK 1: Aktualizacja drzewa Merk alice123"]
        R1["Drzewo alice123 przelicza:<br/>value_hash(&quot;ALICE&quot;) → nowy kv_hash<br/>→ nowy hasz korzenia: H_alice_NOWY"]
    end

    R1 -->|"H_alice_NOWY plynie w gore"| R2
    R2 -->|"H_ident_NOWY plynie w gore"| R3

    style step1 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style step2 fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style step3 fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

**Przed i po** -- zmienione wezly oznaczone na czerwono:

```mermaid
graph TD
    subgraph before["PRZED"]
        B_root["Korzen: aabb1122"]
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

    subgraph after["PO"]
        A_root["Korzen: ff990033"]
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

> Tylko wezly na sciezce od zmienionej wartosci do korzenia sa przeliczane. Rodzenstwo i inne galeze pozostaja bez zmian.

Propagacja jest implementowana przez `propagate_changes_with_transaction`, ktora
przechodzi w gore sciezki od zmodyfikowanego poddrzewa do korzenia, aktualizujac
hasz elementu kazdego rodzica po drodze.

## Przyklad wielopoziomowej struktury gaju

Oto kompletny przyklad pokazujacy, jak Dash Platform strukturyzuje swoj stan:

```mermaid
graph TD
    ROOT["Korzen GroveDB"]

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

Kazde pole to oddzielne drzewo Merk, uwierzytelnione az do pojedynczego hasza
korzenia, na ktory zgadzaja sie walidatorzy.

---

