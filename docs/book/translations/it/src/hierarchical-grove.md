# Il bosco gerarchico — Albero di alberi

## Come i sotto-alberi si annidano dentro gli alberi genitori

La caratteristica distintiva di GroveDB e che un albero Merk puo contenere elementi che sono a loro volta alberi Merk. Cio crea uno **spazio dei nomi gerarchico**:

```mermaid
graph TD
    subgraph root["ALBERO MERK RADICE — percorso: []"]
        contracts["&quot;contracts&quot;<br/>Tree"]
        identities["&quot;identities&quot;<br/>Tree"]
        balances["&quot;balances&quot;<br/>SumTree, sum=0"]
        contracts --> identities
        contracts --> balances
    end

    subgraph ident["MERK IDENTITIES — percorso: [&quot;identities&quot;]"]
        bob456["&quot;bob456&quot;<br/>Tree"]
        alice123["&quot;alice123&quot;<br/>Tree"]
        eve["&quot;eve&quot;<br/>Item"]
        bob456 --> alice123
        bob456 --> eve
    end

    subgraph bal["MERK BALANCES (SumTree) — percorso: [&quot;balances&quot;]"]
        bob_bal["&quot;bob456&quot;<br/>SumItem(800)"]
    end

    subgraph alice_tree["MERK ALICE123 — percorso: [&quot;identities&quot;, &quot;alice123&quot;]"]
        name["&quot;name&quot;<br/>Item(&quot;Al&quot;)"]
        balance_item["&quot;balance&quot;<br/>SumItem(1000)"]
        docs["&quot;docs&quot;<br/>Tree"]
        name --> balance_item
        name --> docs
    end

    identities -.-> bob456
    balances -.-> bob_bal
    alice123 -.-> name
    docs -.->|"... altri sotto-alberi"| more["..."]

    style root fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style ident fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style bal fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style alice_tree fill:#e8daef,stroke:#8e44ad,stroke-width:2px
    style more fill:none,stroke:none
```

> Ogni riquadro colorato e un albero Merk separato. Le frecce tratteggiate rappresentano i collegamenti portale dagli elementi Tree ai loro alberi Merk figli. Il percorso verso ogni Merk e mostrato nella sua etichetta.

## Sistema di indirizzamento per percorso

Ogni elemento in GroveDB e indirizzato da un **percorso** (path) — una sequenza di stringhe di byte che navigano dalla radice attraverso i sotto-alberi fino alla chiave obiettivo:

```text
    Percorso: ["identities", "alice123", "name"]

    Passo 1: Nell'albero radice, cercare "identities" → elemento Tree
    Passo 2: Aprire il sotto-albero identities, cercare "alice123" → elemento Tree
    Passo 3: Aprire il sotto-albero alice123, cercare "name" → Item("Alice")
```

I percorsi sono rappresentati come `Vec<Vec<u8>>` o usando il tipo `SubtreePath` per una manipolazione efficiente senza allocazione:

```rust
// Il percorso verso l'elemento (tutti i segmenti tranne l'ultimo)
let path: &[&[u8]] = &[b"identities", b"alice123"];
// La chiave all'interno del sotto-albero finale
let key: &[u8] = b"name";
```

## Generazione del prefisso Blake3 per l'isolamento dell'archiviazione

Ogni sotto-albero in GroveDB ottiene il proprio **namespace di archiviazione isolato** in RocksDB. Il namespace e determinato dall'hashing del percorso con Blake3:

```rust
pub type SubtreePrefix = [u8; 32];

// Il prefisso viene calcolato facendo l'hash dei segmenti del percorso
// storage/src/rocksdb_storage/storage.rs
```

Per esempio:

```text
    Percorso: ["identities", "alice123"]
    Prefisso: Blake3(["identities", "alice123"]) = [0xab, 0x3f, ...]  (32 byte)

    In RocksDB, le chiavi per questo sotto-albero vengono memorizzate come:
    [prefisso: 32 byte][chiave_originale]

    Quindi "name" in questo sotto-albero diventa:
    [0xab, 0x3f, ...][0x6e, 0x61, 0x6d, 0x65]  ("name")
```

Cio garantisce:
- Nessuna collisione di chiavi tra sotto-alberi (prefisso a 32 byte = isolamento a 256 bit)
- Calcolo efficiente del prefisso (singolo hash Blake3 sui byte del percorso)
- I dati del sotto-albero sono collocati in RocksDB per efficienza della cache

## Propagazione dell'hash radice attraverso la gerarchia

Quando un valore cambia in profondita nel bosco, il cambiamento deve **propagarsi verso l'alto** per aggiornare l'hash radice:

```text
    Modifica: Aggiornare "name" a "ALICE" in identities/alice123/

    Passo 1: Aggiornare il valore nell'albero Merk di alice123
            → l'albero alice123 ottiene un nuovo hash radice: H_alice_nuovo

    Passo 2: Aggiornare l'elemento "alice123" nell'albero identities
            → il value_hash dell'albero identities per "alice123" =
              combine_hash(H(byte_elemento_albero), H_alice_nuovo)
            → l'albero identities ottiene un nuovo hash radice: H_ident_nuovo

    Passo 3: Aggiornare l'elemento "identities" nell'albero radice
            → il value_hash dell'albero radice per "identities" =
              combine_hash(H(byte_elemento_albero), H_ident_nuovo)
            → L'HASH RADICE cambia
```

```mermaid
graph TD
    subgraph step3["PASSO 3: Aggiornamento albero radice"]
        R3["L'albero radice ricalcola:<br/>value_hash per &quot;identities&quot; =<br/>combine_hash(H(tree_bytes), H_ident_NUOVO)<br/>→ nuovo HASH RADICE"]
    end
    subgraph step2["PASSO 2: Aggiornamento albero identities"]
        R2["L'albero identities ricalcola:<br/>value_hash per &quot;alice123&quot; =<br/>combine_hash(H(tree_bytes), H_alice_NUOVO)<br/>→ nuovo hash radice: H_ident_NUOVO"]
    end
    subgraph step1["PASSO 1: Aggiornamento Merk di alice123"]
        R1["L'albero alice123 ricalcola:<br/>value_hash(&quot;ALICE&quot;) → nuovo kv_hash<br/>→ nuovo hash radice: H_alice_NUOVO"]
    end

    R1 -->|"H_alice_NUOVO fluisce verso l'alto"| R2
    R2 -->|"H_ident_NUOVO fluisce verso l'alto"| R3

    style step1 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style step2 fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style step3 fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

**Prima vs Dopo** — nodi modificati contrassegnati in rosso:

```mermaid
graph TD
    subgraph before["PRIMA"]
        B_root["Radice: aabb1122"]
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

    subgraph after["DOPO"]
        A_root["Radice: ff990033"]
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

> Solo i nodi sul percorso dal valore modificato fino alla radice vengono ricalcolati. I fratelli e gli altri rami rimangono invariati.

La propagazione e implementata da `propagate_changes_with_transaction`, che risale il percorso dal sotto-albero modificato fino alla radice, aggiornando l'hash dell'elemento di ogni genitore lungo il cammino.

## Esempio di struttura del bosco multi-livello

Ecco un esempio completo che mostra come Dash Platform struttura il suo stato:

```mermaid
graph TD
    ROOT["Radice GroveDB"]

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

Ogni riquadro e un albero Merk separato, autenticato fino a un singolo hash radice su cui i validatori concordano.

---
