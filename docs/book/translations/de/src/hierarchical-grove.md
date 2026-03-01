# Der hierarchische Hain — Baum von Bäumen

## Wie Teilbäume in Elternbäumen verschachtelt werden

Das definierende Merkmal von GroveDB ist, dass ein Merk-Baum Elemente enthalten kann, die
selbst Merk-Bäume sind. Dies erzeugt einen **hierarchischen Namensraum**:

```mermaid
graph TD
    subgraph root["WURZEL-MERK-BAUM — Pfad: []"]
        contracts["&quot;contracts&quot;<br/>Tree"]
        identities["&quot;identities&quot;<br/>Tree"]
        balances["&quot;balances&quot;<br/>SumTree, sum=0"]
        contracts --> identities
        contracts --> balances
    end

    subgraph ident["IDENTITIES MERK — Pfad: [&quot;identities&quot;]"]
        bob456["&quot;bob456&quot;<br/>Tree"]
        alice123["&quot;alice123&quot;<br/>Tree"]
        eve["&quot;eve&quot;<br/>Item"]
        bob456 --> alice123
        bob456 --> eve
    end

    subgraph bal["BALANCES MERK (SumTree) — Pfad: [&quot;balances&quot;]"]
        bob_bal["&quot;bob456&quot;<br/>SumItem(800)"]
    end

    subgraph alice_tree["ALICE123 MERK — Pfad: [&quot;identities&quot;, &quot;alice123&quot;]"]
        name["&quot;name&quot;<br/>Item(&quot;Al&quot;)"]
        balance_item["&quot;balance&quot;<br/>SumItem(1000)"]
        docs["&quot;docs&quot;<br/>Tree"]
        name --> balance_item
        name --> docs
    end

    identities -.-> bob456
    balances -.-> bob_bal
    alice123 -.-> name
    docs -.->|"... weitere Teilbäume"| more["..."]

    style root fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style ident fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style bal fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style alice_tree fill:#e8daef,stroke:#8e44ad,stroke-width:2px
    style more fill:none,stroke:none
```

> Jeder farbige Kasten ist ein separater Merk-Baum. Gestrichelte Pfeile repräsentieren die Portal-Links von Tree-Elementen zu ihren Kind-Merk-Bäumen. Der Pfad zu jedem Merk-Baum wird in seinem Label angezeigt.

## Pfad-Adressierungssystem

Jedes Element in GroveDB wird über einen **Pfad** adressiert — eine Folge von Byte-Strings,
die vom Wurzelverzeichnis durch Teilbäume zum Zielschlüssel navigieren:

```text
    Pfad: ["identities", "alice123", "name"]

    Schritt 1: Im Wurzelbaum "identities" nachschlagen → Tree-Element
    Schritt 2: Identities-Teilbaum öffnen, "alice123" nachschlagen → Tree-Element
    Schritt 3: Alice123-Teilbaum öffnen, "name" nachschlagen → Item("Alice")
```

Pfade werden als `Vec<Vec<u8>>` oder mittels des `SubtreePath`-Typs für
effiziente Manipulation ohne Allokation dargestellt:

```rust
// Der Pfad zum Element (alle Segmente außer dem letzten)
let path: &[&[u8]] = &[b"identities", b"alice123"];
// Der Schlüssel innerhalb des endgültigen Teilbaums
let key: &[u8] = b"name";
```

## Blake3-Präfixgenerierung für Speicherisolation

Jeder Teilbaum in GroveDB erhält seinen eigenen **isolierten Speicher-Namensraum** in RocksDB.
Der Namensraum wird durch Hashing des Pfads mit Blake3 bestimmt:

```rust
pub type SubtreePrefix = [u8; 32];

// Das Präfix wird durch Hashing der Pfadsegmente berechnet
// storage/src/rocksdb_storage/storage.rs
```

Zum Beispiel:

```text
    Pfad: ["identities", "alice123"]
    Präfix: Blake3(["identities", "alice123"]) = [0xab, 0x3f, ...]  (32 Bytes)

    In RocksDB werden Schlüssel für diesen Teilbaum wie folgt gespeichert:
    [Präfix: 32 Bytes][originaler_schlüssel]

    Also wird "name" in diesem Teilbaum zu:
    [0xab, 0x3f, ...][0x6e, 0x61, 0x6d, 0x65]  ("name")
```

Das stellt sicher:
- Keine Schlüsselkollisionen zwischen Teilbäumen (32-Byte-Präfix = 256-Bit-Isolation)
- Effiziente Präfixberechnung (ein einzelner Blake3-Hash über die Pfad-Bytes)
- Teilbaumdaten sind in RocksDB für Cache-Effizienz zusammen gruppiert

## Wurzel-Hash-Propagierung durch die Hierarchie

Wenn sich ein Wert tief im Hain ändert, muss die Änderung **nach oben propagieren**, um
den Wurzel-Hash zu aktualisieren:

```text
    Änderung: "name" auf "ALICE" aktualisieren in identities/alice123/

    Schritt 1: Wert im Merk-Baum von alice123 aktualisieren
               → alice123-Baum bekommt neuen Wurzel-Hash: H_alice_neu

    Schritt 2: "alice123"-Element im Identities-Baum aktualisieren
               → value_hash des Identities-Baums für "alice123" =
                 combine_hash(H(tree_element_bytes), H_alice_neu)
               → Identities-Baum bekommt neuen Wurzel-Hash: H_ident_neu

    Schritt 3: "identities"-Element im Wurzelbaum aktualisieren
               → value_hash des Wurzelbaums für "identities" =
                 combine_hash(H(tree_element_bytes), H_ident_neu)
               → WURZEL-HASH ändert sich
```

```mermaid
graph TD
    subgraph step3["SCHRITT 3: Wurzelbaum aktualisieren"]
        R3["Wurzelbaum berechnet neu:<br/>value_hash für &quot;identities&quot; =<br/>combine_hash(H(tree_bytes), H_ident_NEU)<br/>→ neuer WURZEL-HASH"]
    end
    subgraph step2["SCHRITT 2: Identities-Baum aktualisieren"]
        R2["Identities-Baum berechnet neu:<br/>value_hash für &quot;alice123&quot; =<br/>combine_hash(H(tree_bytes), H_alice_NEU)<br/>→ neuer Wurzel-Hash: H_ident_NEU"]
    end
    subgraph step1["SCHRITT 1: Alice123-Merk aktualisieren"]
        R1["Alice123-Baum berechnet neu:<br/>value_hash(&quot;ALICE&quot;) → neuer kv_hash<br/>→ neuer Wurzel-Hash: H_alice_NEU"]
    end

    R1 -->|"H_alice_NEU fließt nach oben"| R2
    R2 -->|"H_ident_NEU fließt nach oben"| R3

    style step1 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style step2 fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style step3 fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

**Vorher vs. Nachher** — geänderte Knoten rot markiert:

```mermaid
graph TD
    subgraph before["VORHER"]
        B_root["Wurzel: aabb1122"]
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

    subgraph after["NACHHER"]
        A_root["Wurzel: ff990033"]
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

> Nur die Knoten auf dem Pfad vom geänderten Wert bis zur Wurzel werden neu berechnet. Geschwister und andere Zweige bleiben unverändert.

Die Propagierung wird durch `propagate_changes_with_transaction` implementiert, welches
vom modifizierten Teilbaum zur Wurzel aufsteigt und den Element-Hash jedes Elternknotens
auf dem Weg aktualisiert.

## Mehrstufige Hain-Struktur — Beispiel

Hier ein vollständiges Beispiel, das zeigt, wie Dash Platform seinen Zustand strukturiert:

```mermaid
graph TD
    ROOT["GroveDB-Wurzel"]

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

Jeder Kasten ist ein separater Merk-Baum, authentifiziert bis hin zu einem einzigen
Wurzel-Hash, auf den sich die Validatoren einigen.

---
