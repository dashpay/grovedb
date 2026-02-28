# Hierarchicky haj -- Strom stromu

## Jak se podstromy vnoreji do rodicovskych stromu

Definujici vlastnosti GroveDB je, ze strom Merk muze obsahovat elementy, ktere
jsou samy o sobe stromy Merk. Toto vytvari **hierarchicky jmenny prostor**:

```mermaid
graph TD
    subgraph root["KORENOVY STROM MERK — cesta: []"]
        contracts["&quot;contracts&quot;<br/>Tree"]
        identities["&quot;identities&quot;<br/>Tree"]
        balances["&quot;balances&quot;<br/>SumTree, sum=0"]
        contracts --> identities
        contracts --> balances
    end

    subgraph ident["MERK IDENTITIES — cesta: [&quot;identities&quot;]"]
        bob456["&quot;bob456&quot;<br/>Tree"]
        alice123["&quot;alice123&quot;<br/>Tree"]
        eve["&quot;eve&quot;<br/>Item"]
        bob456 --> alice123
        bob456 --> eve
    end

    subgraph bal["MERK BALANCES (SumTree) — cesta: [&quot;balances&quot;]"]
        bob_bal["&quot;bob456&quot;<br/>SumItem(800)"]
    end

    subgraph alice_tree["MERK ALICE123 — cesta: [&quot;identities&quot;, &quot;alice123&quot;]"]
        name["&quot;name&quot;<br/>Item(&quot;Al&quot;)"]
        balance_item["&quot;balance&quot;<br/>SumItem(1000)"]
        docs["&quot;docs&quot;<br/>Tree"]
        name --> balance_item
        name --> docs
    end

    identities -.-> bob456
    balances -.-> bob_bal
    alice123 -.-> name
    docs -.->|"... dalsi podstromy"| more["..."]

    style root fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style ident fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style bal fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style alice_tree fill:#e8daef,stroke:#8e44ad,stroke-width:2px
    style more fill:none,stroke:none
```

> Kazdy barevny ramecek je samostatny strom Merk. Prerusovane sipky predstavuji portalove odkazy z elementu Tree do jejich podrizenych stromu Merk. Cesta ke kazdemu Merk je zobrazena v jeho popisku.

## System adresovani cestou

Kazdy element v GroveDB je adresovan **cestou** -- sekvenci retezcu bajtu,
ktera naviguje od korene pres podstromy k cilovemu klici:

```text
    Cesta: ["identities", "alice123", "name"]

    Krok 1: V korenovem stromu vyhledejte "identities" → element Tree
    Krok 2: Otevrete podstrom identities, vyhledejte "alice123" → element Tree
    Krok 3: Otevrete podstrom alice123, vyhledejte "name" → Item("Alice")
```

Cesty jsou reprezentovany jako `Vec<Vec<u8>>` nebo pomoci typu `SubtreePath`
pro efektivni manipulaci bez alokace:

```rust
// Cesta k elementu (vsechny segmenty krome posledniho)
let path: &[&[u8]] = &[b"identities", b"alice123"];
// Klic v koncovem podstromu
let key: &[u8] = b"name";
```

## Generovani prefixu Blake3 pro izolaci uloziste

Kazdy podstrom v GroveDB dostava svuj vlastni **izolovany jmenny prostor uloziste**
v RocksDB. Jmenny prostor je urcen hashovanim cesty pomoci Blake3:

```rust
pub type SubtreePrefix = [u8; 32];

// Prefix je vypocten hashovanim segmentu cesty
// storage/src/rocksdb_storage/storage.rs
```

Napriklad:

```text
    Cesta: ["identities", "alice123"]
    Prefix: Blake3(["identities", "alice123"]) = [0xab, 0x3f, ...]  (32 bajtu)

    V RocksDB jsou klice pro tento podstrom ulozeny jako:
    [prefix: 32 bajtu][puvodni_klic]

    Takze "name" v tomto podstromu se stava:
    [0xab, 0x3f, ...][0x6e, 0x61, 0x6d, 0x65]  ("name")
```

Toto zajistuje:
- Zadne kolize klicu mezi podstromy (32-bajtovy prefix = 256-bitova izolace)
- Efektivni vypocet prefixu (jediny hash Blake3 pres bajty cesty)
- Data podstromu jsou umistena spolecne v RocksDB pro efektivitu cache

## Propagace korenoveho hashe skrze hierarchii

Kdyz se hodnota zmeni hluboko v haji, zmena se musi **propagovat nahoru** pro
aktualizaci korenoveho hashe:

```text
    Zmena: Aktualizace "name" na "ALICE" v identities/alice123/

    Krok 1: Aktualizace hodnoty ve stromu Merk alice123
            → strom alice123 dostava novy korenovy hash: H_alice_novy

    Krok 2: Aktualizace elementu "alice123" ve stromu identities
            → value_hash stromu identities pro "alice123" =
              combine_hash(H(tree_element_bytes), H_alice_novy)
            → strom identities dostava novy korenovy hash: H_ident_novy

    Krok 3: Aktualizace elementu "identities" v korenovem stromu
            → value_hash korenoveho stromu pro "identities" =
              combine_hash(H(tree_element_bytes), H_ident_novy)
            → KORENOVY HASH se meni
```

```mermaid
graph TD
    subgraph step3["KROK 3: Aktualizace korenoveho stromu"]
        R3["Korenovy strom prepocitava:<br/>value_hash pro &quot;identities&quot; =<br/>combine_hash(H(tree_bytes), H_ident_NOVY)<br/>→ novy KORENOVY HASH"]
    end
    subgraph step2["KROK 2: Aktualizace stromu identities"]
        R2["Strom identities prepocitava:<br/>value_hash pro &quot;alice123&quot; =<br/>combine_hash(H(tree_bytes), H_alice_NOVY)<br/>→ novy korenovy hash: H_ident_NOVY"]
    end
    subgraph step1["KROK 1: Aktualizace Merk alice123"]
        R1["Strom alice123 prepocitava:<br/>value_hash(&quot;ALICE&quot;) → novy kv_hash<br/>→ novy korenovy hash: H_alice_NOVY"]
    end

    R1 -->|"H_alice_NOVY proudí nahoru"| R2
    R2 -->|"H_ident_NOVY proudí nahoru"| R3

    style step1 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style step2 fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style step3 fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

**Pred vs. po** -- zmenene uzly oznaceny cervenou:

```mermaid
graph TD
    subgraph before["PRED"]
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

    subgraph after["PO"]
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

> Pouze uzly na ceste od zmenene hodnoty nahoru ke koreni jsou prepocitany. Sourozenci a ostatni vetve zustavaji nezmeneni.

Propagace je implementovana funkci `propagate_changes_with_transaction`, ktera
prochazi cestu od zmeneneho podstromu ke koreni a aktualizuje hash elementu
kazdeho rodice na ceste.

## Priklad viceurovnove struktury haje

Zde je uplny priklad ukazujici, jak Dash Platform strukturuje svuj stav:

```mermaid
graph TD
    ROOT["Koren GroveDB"]

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

Kazdy ramecek je samostatny strom Merk, autentizovany az po jediny korenovy hash,
na kterem se validatori shoduji.

---
