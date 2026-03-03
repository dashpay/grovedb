# Einführung — Was ist GroveDB?

## Die Kernidee

GroveDB ist eine **hierarchische authentifizierte Datenstruktur** — im Wesentlichen ein *Hain*
(Baum von Bäumen), der auf Merkle-AVL-Bäumen aufgebaut ist. Jeder Knoten in der Datenbank ist
Teil eines kryptographisch authentifizierten Baumes, und jeder Baum kann andere Bäume als
Kinder enthalten, wodurch eine tiefe Hierarchie verifizierbarer Zustände entsteht.

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

> Jeder farbige Kasten ist ein **separater Merk-Baum**. Gestrichelte Pfeile zeigen die Teilbaum-Beziehung — ein Tree-Element im Elternbaum enthält den Wurzelschlüssel des Kind-Merk-Baums.

In einer herkömmlichen Datenbank würde man Daten in einem flachen Schlüssel-Wert-Speicher
mit einem einzelnen Merkle-Baum darüber zur Authentifizierung ablegen. GroveDB verfolgt
einen anderen Ansatz: es verschachtelt Merkle-Bäume innerhalb von Merkle-Bäumen.
Das bietet folgende Vorteile:

1. **Effiziente Sekundärindizes** — Abfrage über beliebige Pfade, nicht nur über Primärschlüssel
2. **Kompakte kryptographische Beweise** — Nachweis der Existenz (oder Abwesenheit) beliebiger Daten
3. **Aggregierte Daten** — Bäume können ihre Kinder automatisch summieren, zählen oder anderweitig aggregieren
4. **Atomare baumübergreifende Operationen** — Stapeloperationen erstrecken sich über mehrere Teilbäume

## Warum GroveDB existiert

GroveDB wurde für **Dash Platform** entwickelt, eine dezentrale Anwendungsplattform,
in der jedes Stück Zustand folgende Anforderungen erfüllen muss:

- **Authentifiziert**: Jeder Knoten kann jedem Light-Client jeden Zustand beweisen
- **Deterministisch**: Jeder Knoten berechnet exakt denselben Zustandswurzel-Hash
- **Effizient**: Operationen müssen innerhalb der Blockzeit-Beschränkungen abgeschlossen werden
- **Abfragbar**: Anwendungen benötigen reichhaltige Abfragen, nicht nur Schlüsselsuchen

Herkömmliche Ansätze haben Defizite:

| Ansatz | Problem |
|--------|---------|
| Einfacher Merkle-Baum | Unterstützt nur Schlüsselsuchen, keine Bereichsabfragen |
| Ethereum MPT | Teures Rebalancing, große Beweisgrößen |
| Flacher Schlüssel-Wert-Speicher + einzelner Baum | Keine hierarchischen Abfragen, ein einzelner Beweis deckt alles ab |
| B-Baum | Nicht natürlich merklifiziert, komplexe Authentifizierung |

GroveDB löst diese Probleme durch die Kombination der **bewährten Balance-Garantien von AVL-Bäumen**
mit **hierarchischer Verschachtelung** und einem **reichhaltigen Element-Typsystem**.

## Architekturüberblick

GroveDB ist in klar abgegrenzte Schichten organisiert, jede mit einer eindeutigen Verantwortlichkeit:

```mermaid
graph TD
    APP["<b>Anwendungsschicht</b><br/>Dash Platform, etc.<br/><code>insert() get() query() prove() apply_batch()</code>"]

    GROVE["<b>GroveDB-Kern</b> — <code>grovedb/src/</code><br/>Hierarchische Teilbaumverwaltung · Element-Typsystem<br/>Referenzauflösung · Stapeloperationen · Mehrschicht-Beweise"]

    MERK["<b>Merk-Schicht</b> — <code>merk/src/</code><br/>Merkle-AVL-Baum · Selbstbalancierende Rotationen<br/>Link-System · Blake3-Hashing · Beweis-Kodierung"]

    STORAGE["<b>Speicherschicht</b> — <code>storage/src/</code><br/>RocksDB OptimisticTransactionDB<br/>4 Spaltenfamilien · Blake3-Präfix-Isolation · Gebündelte Schreibvorgänge"]

    COST["<b>Kostenschicht</b> — <code>costs/src/</code><br/>OperationCost-Tracking · CostContext-Monade<br/>Worst-Case- &amp; Average-Case-Schätzung"]

    APP ==>|"Schreibvorgänge ↓"| GROVE
    GROVE ==>|"Baum-Ops"| MERK
    MERK ==>|"Disk-I/O"| STORAGE
    STORAGE -.->|"Kostenakkumulation ↑"| COST
    COST -.-> MERK
    MERK -.-> GROVE
    GROVE ==>|"Lesevorgänge ↑"| APP

    style APP fill:#f5f5f5,stroke:#999,stroke-width:1px
    style GROVE fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style MERK fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style STORAGE fill:#fdebd0,stroke:#e67e22,stroke-width:2px
    style COST fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

Daten fließen bei Schreibvorgängen **nach unten** durch diese Schichten und bei Lesevorgängen **nach oben**.
Jede Operation akkumuliert Kosten, während sie den Stapel durchläuft, was eine präzise
Ressourcenabrechnung ermöglicht.

---
