# Das Referenz-System

## Warum Referenzen existieren

In einer hierarchischen Datenbank müssen häufig dieselben Daten über mehrere
Pfade zugänglich sein. Beispielsweise könnten Dokumente unter ihrem Vertrag gespeichert,
aber auch nach Eigentümer-Identität abfragbar sein. **Referenzen** sind die Antwort von GroveDB —
es sind Zeiger von einem Ort zu einem anderen, ähnlich wie symbolische Links in einem Dateisystem.

```mermaid
graph LR
    subgraph primary["Primärspeicher"]
        item["contracts/C1/docs/D1<br/><b>Item</b>(data)"]
    end
    subgraph secondary["Sekundärindex"]
        ref["identities/alice/docs/D1<br/><b>Reference</b>"]
    end
    ref -->|"zeigt auf"| item

    style primary fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style secondary fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style ref fill:#fef9e7,stroke:#f39c12,stroke-width:2px
```

Wichtige Eigenschaften:
- Referenzen sind **authentifiziert** — der value_hash der Referenz enthält sowohl die
  Referenz selbst als auch das referenzierte Element
- Referenzen können **verkettet** werden — eine Referenz kann auf eine andere Referenz zeigen
- Zykluserkennung verhindert Endlosschleifen
- Ein konfigurierbares Hop-Limit verhindert Ressourcenerschöpfung

## Die sieben Referenztypen

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

Gehen wir jeden mit Diagrammen durch.

### AbsolutePathReference

Der einfachste Typ. Speichert den vollständigen Pfad zum Ziel:

```mermaid
graph TD
    subgraph root["Wurzel-Merk — Pfad: []"]
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
    X ==>|"löst auf zu [P, Q, R]"| R

    style merkAB fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style merkPQ fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style X fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style R fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

> X speichert den vollständigen absoluten Pfad `[P, Q, R]`. Unabhängig davon, wo sich X befindet, löst es immer zum selben Ziel auf.

### UpstreamRootHeightReference

Behält die ersten N Segmente des aktuellen Pfads bei und hängt dann einen neuen Pfad an:

```mermaid
graph TD
    subgraph resolve["Auflösung: erste 2 Segmente behalten + [P, Q] anhängen"]
        direction LR
        curr["aktuell: [A, B, C, D]"] --> keep["erste 2 behalten: [A, B]"] --> append["anhängen: [A, B, <b>P, Q</b>]"]
    end

    subgraph grove["Hain-Hierarchie"]
        gA["A (Höhe 0)"]
        gB["B (Höhe 1)"]
        gC["C (Höhe 2)"]
        gD["D (Höhe 3)"]
        gX["X = Reference<br/>UpstreamRootHeight(2, [P,Q])"]
        gP["P (Höhe 2)"]
        gQ["Q (Höhe 3) — Ziel"]

        gA --> gB
        gB --> gC
        gB -->|"erste 2 behalten → [A,B]<br/>dann absteigen [P,Q]"| gP
        gC --> gD
        gD -.-> gX
        gP --> gQ
    end

    gX ==>|"löst auf zu"| gQ

    style resolve fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style gX fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style gQ fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

### UpstreamRootHeightWithParentPathAdditionReference

Wie UpstreamRootHeight, aber hängt das letzte Segment des aktuellen Pfads wieder an:

```text
    Referenz bei Pfad [A, B, C, D, E] key=X
    UpstreamRootHeightWithParentPathAdditionReference(2, [P, Q])

    Aktueller Pfad:      [A, B, C, D, E]
    Erste 2 behalten:    [A, B]
    [P, Q] anhängen:     [A, B, P, Q]
    Letztes wieder anhängen: [A, B, P, Q, E]   ← "E" aus dem Originalpfad wieder hinzugefügt

    Nützlich für: Indizes, bei denen der Elternschlüssel erhalten bleiben soll
```

### UpstreamFromElementHeightReference

Verwirft die letzten N Segmente und hängt dann an:

```text
    Referenz bei Pfad [A, B, C, D] key=X
    UpstreamFromElementHeightReference(1, [P, Q])

    Aktueller Pfad:      [A, B, C, D]
    Letztes 1 verwerfen: [A, B, C]
    [P, Q] anhängen:     [A, B, C, P, Q]
```

### CousinReference

Ersetzt nur den unmittelbaren Elternknoten durch einen neuen Schlüssel:

```mermaid
graph TD
    subgraph resolve["Auflösung: letzte 2 entfernen, Cousin C einfügen, Schlüssel X einfügen"]
        direction LR
        r1["Pfad: [A, B, M, D]"] --> r2["letzte 2 entfernen: [A, B]"] --> r3["C einfügen: [A, B, C]"] --> r4["Schlüssel X einfügen: [A, B, C, X]"]
    end

    subgraph merkAB["Merk [A, B]"]
        M["M<br/>Tree"]
        C["C<br/>Tree<br/>(Cousin von M)"]
    end

    subgraph merkABM["Merk [A, B, M]"]
        D["D<br/>Tree"]
    end

    subgraph merkABMD["Merk [A, B, M, D]"]
        Xref["X = Reference<br/>CousinReference(C)"]
    end

    subgraph merkABC["Merk [A, B, C]"]
        Xtarget["X = Item<br/>(Ziel)"]
    end

    M -.-> D
    D -.-> Xref
    C -.-> Xtarget
    Xref ==>|"löst auf zu [A, B, C, X]"| Xtarget

    style resolve fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style Xref fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style Xtarget fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style M fill:#fadbd8,stroke:#e74c3c
    style C fill:#d5f5e3,stroke:#27ae60
```

> Der "Cousin" ist ein Geschwister-Teilbaum des Großelternknotens der Referenz. Die Referenz navigiert zwei Ebenen nach oben und steigt dann in den Cousin-Teilbaum ab.

### RemovedCousinReference

Wie CousinReference, ersetzt aber den Elternknoten durch einen mehrsegmentigen Pfad:

```text
    Referenz bei Pfad [A, B, C, D] key=X
    RemovedCousinReference([M, N])

    Aktueller Pfad:    [A, B, C, D]
    Eltern C entfernen: [A, B]
    [M, N] anhängen:   [A, B, M, N]
    Schlüssel X einfügen: [A, B, M, N, X]
```

### SiblingReference

Die einfachste relative Referenz — ändert nur den Schlüssel innerhalb desselben Elternknotens:

```mermaid
graph TD
    subgraph merk["Merk [A, B, C] — selber Baum, selber Pfad"]
        M_sib["M"]
        X_sib["X = Reference<br/>SiblingRef(Y)"]
        Y_sib["Y = Item<br/>(Ziel)"]
        Z_sib["Z = Item"]
        M_sib --> X_sib
        M_sib --> Y_sib
    end

    X_sib ==>|"löst auf zu [A, B, C, Y]"| Y_sib

    style merk fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style X_sib fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style Y_sib fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

> Der einfachste Referenztyp. X und Y sind Geschwister im selben Merk-Baum — die Auflösung ändert nur den Schlüssel, behält aber denselben Pfad bei.

## Referenzverfolgung und das Hop-Limit

Wenn GroveDB auf ein Reference-Element trifft, muss es ihm **folgen**, um den
tatsächlichen Wert zu finden. Da Referenzen auf andere Referenzen zeigen können, beinhaltet dies eine Schleife:

```rust
// grovedb/src/reference_path.rs
pub const MAX_REFERENCE_HOPS: usize = 10;

pub fn follow_reference(...) -> CostResult<ResolvedReference, Error> {
    let mut hops_left = MAX_REFERENCE_HOPS;
    let mut visited = HashSet::new();

    while hops_left > 0 {
        // Referenzpfad in absoluten Pfad auflösen
        let target_path = current_ref.absolute_qualified_path(...);

        // Auf Zyklen prüfen
        if !visited.insert(target_path.clone()) {
            return Err(Error::CyclicReference);
        }

        // Element am Ziel abrufen
        let element = Element::get(target_path);

        match element {
            Element::Reference(next_ref, ..) => {
                // Immer noch eine Referenz — weiter folgen
                current_ref = next_ref;
                hops_left -= 1;
            }
            other => {
                // Tatsächliches Element gefunden!
                return Ok(ResolvedReference { element: other, ... });
            }
        }
    }

    Err(Error::ReferenceLimit)  // 10 Hops überschritten
}
```

## Zykluserkennung

Das `visited`-HashSet verfolgt alle Pfade, die wir gesehen haben. Wenn wir auf einen Pfad stoßen,
den wir bereits besucht haben, haben wir einen Zyklus:

```mermaid
graph LR
    A["A<br/>Reference"] -->|"Schritt 1"| B["B<br/>Reference"]
    B -->|"Schritt 2"| C["C<br/>Reference"]
    C -->|"Schritt 3"| A

    style A fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style B fill:#fef9e7,stroke:#f39c12
    style C fill:#fef9e7,stroke:#f39c12
```

> **Zykluserkennungs-Ablauf:**
>
> | Schritt | Folge | visited-Menge | Ergebnis |
> |---------|-------|---------------|----------|
> | 1 | Start bei A | { A } | A ist Ref → folgen |
> | 2 | A → B | { A, B } | B ist Ref → folgen |
> | 3 | B → C | { A, B, C } | C ist Ref → folgen |
> | 4 | C → A | A bereits in visited! | **Error::CyclicRef** |
>
> Ohne Zykluserkennung würde dies endlos schleifen. `MAX_REFERENCE_HOPS = 10` begrenzt zusätzlich die Traversierungstiefe für lange Ketten.

## Referenzen in Merk — Kombinierte Wert-Hashes

Wenn eine Referenz in einem Merk-Baum gespeichert wird, muss ihr `value_hash`
sowohl die Referenzstruktur als auch die referenzierten Daten authentifizieren:

```rust
// merk/src/tree/kv.rs
pub fn update_hashes_using_reference_value_hash(
    mut self,
    reference_value_hash: CryptoHash,
) -> CostContext<Self> {
    // Die eigenen Bytes des Referenz-Elements hashen
    let actual_value_hash = value_hash(self.value_as_slice());

    // Kombinieren: H(referenz_bytes) ⊕ H(referenzierte_daten)
    let combined = combine_hash(&actual_value_hash, &reference_value_hash);

    self.value_hash = combined;
    self.hash = kv_digest_to_kv_hash(self.key(), self.value_hash());
    // ...
}
```

Das bedeutet, dass eine Änderung entweder der Referenz selbst ODER der Daten, auf die sie zeigt,
den Wurzel-Hash ändert — beides ist kryptographisch gebunden.

---
