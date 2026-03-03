# Der DenseAppendOnlyFixedSizeTree — Dichter Merkle-Speicher mit fester Kapazität

Der DenseAppendOnlyFixedSizeTree ist ein vollständiger Binärbaum fester Höhe, bei dem
**jeder Knoten** — sowohl interne als auch Blattknoten — einen Datenwert speichert.
Positionen werden sequentiell in Ebenenreihenfolge (BFS) gefüllt: zuerst die Wurzel
(Position 0), dann von links nach rechts auf jeder Ebene. Keine Zwischen-Hashes werden
persistiert; der Wurzel-Hash wird bei Bedarf durch rekursives Hashing von den Blättern
zur Wurzel neu berechnet.

Dieses Design ist ideal für kleine, begrenzte Datenstrukturen, bei denen die maximale
Kapazität im Voraus bekannt ist und man O(1)-Anhängen, O(1)-Abruf nach Position und
eine kompakte 32-Byte-Wurzel-Hash-Verpflichtung benötigt, die sich nach jedem Einfügen
ändert.

## Baumstruktur

Ein Baum der Höhe *h* hat eine Kapazität von `2^h - 1` Positionen. Positionen verwenden
0-basierte Ebenenreihenfolge-Indizierung:

```text
Height 3 tree (capacity = 7):

              pos 0          ← root (level 0)
             /     \
          pos 1    pos 2     ← level 1
         /   \    /   \
       pos 3 pos 4 pos 5 pos 6  ← level 2 (leaves)

Navigation:
  left_child(i)  = 2i + 1
  right_child(i) = 2i + 2
  parent(i)      = (i - 1) / 2
  is_leaf(i)     = 2i + 1 >= capacity
```

Werte werden sequentiell angehängt: der erste Wert geht an Position 0 (Wurzel), dann
Position 1, 2, 3 und so weiter. Das bedeutet, die Wurzel hat immer Daten, und der Baum
füllt sich in Ebenenreihenfolge — der natürlichsten Durchlaufreihenfolge für einen
vollständigen Binärbaum.

## Hash-Berechnung

Der Wurzel-Hash wird nicht separat gespeichert — er wird bei Bedarf von Grund auf neu
berechnet. Der rekursive Algorithmus besucht nur gefüllte Positionen:

```text
hash(position, store):
  value = store.get(position)

  if position is unfilled (>= count):
    return [0; 32]                                    ← empty sentinel

  value_hash = blake3(value)
  left_hash  = hash(2 * position + 1, store)
  right_hash = hash(2 * position + 2, store)
  return blake3(value_hash || left_hash || right_hash)
```

**Schlüsseleigenschaften:**
- Alle Knoten (Blatt und intern): `blake3(blake3(value) || H(left) || H(right))`
- Blattknoten: left_hash und right_hash sind beide `[0; 32]` (ungefüllte Kinder)
- Ungefüllte Positionen: `[0u8; 32]` (Null-Hash)
- Leerer Baum (count = 0): `[0u8; 32]`

**Keine Blatt/Intern-Domänentrennung wird verwendet.** Die Baumstruktur (`height`
und `count`) wird extern im übergeordneten `Element::DenseAppendOnlyFixedSizeTree`
authentifiziert, das durch die Merk-Hierarchie fließt. Der Verifizierer weiß immer
genau, welche Positionen Blätter vs. interne Knoten sind (anhand von Höhe und Anzahl),
sodass ein Angreifer nicht eines für das andere substituieren kann, ohne die
übergeordnete Authentifizierungskette zu brechen.

Das bedeutet, der Wurzel-Hash kodiert eine Verpflichtung für jeden gespeicherten Wert
und seine exakte Position im Baum. Das Ändern eines Wertes (wenn er veränderbar wäre)
würde durch alle Vorfahren-Hashes bis zur Wurzel kaskadieren.

**Hash-Kosten:** Die Berechnung des Wurzel-Hashs besucht alle gefüllten Positionen plus
alle ungefüllten Kinder. Für einen Baum mit *n* Werten sind im schlimmsten Fall O(*n*)
Blake3-Aufrufe nötig. Dies ist akzeptabel, da der Baum für kleine, begrenzte Kapazitäten
ausgelegt ist (maximale Höhe 16, maximal 65.535 Positionen).

## Die Element-Variante

```rust
Element::DenseAppendOnlyFixedSizeTree(
    u16,                   // count — number of values stored (max 65,535)
    u8,                    // height — immutable after creation (1..=16)
    Option<ElementFlags>,  // flags — storage flags
)
```

| Feld | Typ | Beschreibung |
|---|---|---|
| `count` | `u16` | Anzahl bisher eingefügter Werte (max 65.535) |
| `height` | `u8` | Baumhöhe (1..=16), unveränderlich nach Erstellung |
| `flags` | `Option<ElementFlags>` | Optionale Speicher-Flags |

Der Wurzel-Hash wird NICHT im Element gespeichert — er fließt als Merk-Kind-Hash
über den `subtree_root_hash`-Parameter von `insert_subtree`.

**Diskriminante:** 14 (ElementType), TreeType = 10

**Kostengröße:** `DENSE_TREE_COST_SIZE = 6` Bytes (2 count + 1 height + 1 Diskriminante
+ 2 Overhead)

## Speicher-Layout

Wie MmrTree und BulkAppendTree speichert der DenseAppendOnlyFixedSizeTree Daten im
**Daten**-Namensraum (nicht in einem Kind-Merk). Werte werden durch ihre Position als
Big-Endian-`u64` indexiert:

```text
Subtree path: blake3(parent_path || key)

Storage keys:
  [0, 0, 0, 0, 0, 0, 0, 0] → value at position 0 (root)
  [0, 0, 0, 0, 0, 0, 0, 1] → value at position 1
  [0, 0, 0, 0, 0, 0, 0, 2] → value at position 2
  ...
```

Das Element selbst (im übergeordneten Merk gespeichert) trägt `count` und `height`.
Der Wurzel-Hash fließt als Merk-Kind-Hash. Das bedeutet:
- **Lesen des Wurzel-Hashs** erfordert Neuberechnung aus dem Speicher (O(n) Hashing)
- **Lesen eines Wertes nach Position ist O(1)** — einzelner Speicherzugriff
- **Einfügen ist O(n) Hashing** — ein Speicherschreibvorgang + vollständige Wurzel-Hash-Neuberechnung

## Operationen

### `dense_tree_insert(path, key, value, tx, grove_version)`

Hängt einen Wert an die nächste verfügbare Position an. Gibt `(root_hash, position)` zurück.

```text
Step 1: Read element, extract (count, height)
Step 2: Check capacity: if count >= 2^height - 1 → error
Step 3: Build subtree path, open storage context
Step 4: Write value to position = count
Step 5: Reconstruct DenseFixedSizedMerkleTree from state
Step 6: Call tree.insert(value, store) → (root_hash, position, hash_calls)
Step 7: Update element with new root_hash and count + 1
Step 8: Propagate changes up through Merk hierarchy
Step 9: Commit transaction
```

### `dense_tree_get(path, key, position, tx, grove_version)`

Ruft den Wert an einer gegebenen Position ab. Gibt `None` zurück, wenn position >= count.

### `dense_tree_root_hash(path, key, tx, grove_version)`

Gibt den im Element gespeicherten Wurzel-Hash zurück. Dies ist der Hash, der beim
letzten Einfügen berechnet wurde — keine Neuberechnung erforderlich.

### `dense_tree_count(path, key, tx, grove_version)`

Gibt die Anzahl der gespeicherten Werte zurück (das `count`-Feld aus dem Element).

## Stapeloperationen

Die `GroveOp::DenseTreeInsert`-Variante unterstützt Stapeleinfügungen über die
Standard-GroveDB-Stapelpipeline:

```rust
let ops = vec![
    QualifiedGroveDbOp::dense_tree_insert_op(
        vec![b"parent".to_vec()],
        b"my_dense_tree".to_vec(),
        b"value_data".to_vec(),
    ),
];
db.apply_batch(ops, None, None, grove_version)?;
```

**Vorverarbeitung:** Wie alle Nicht-Merk-Baumtypen werden `DenseTreeInsert`-Ops
vorverarbeitet, bevor der Hauptteil des Stapels ausgeführt wird. Die
`preprocess_dense_tree_ops`-Methode:

1. Gruppiert alle `DenseTreeInsert`-Ops nach `(path, key)`
2. Führt für jede Gruppe die Einfügungen sequentiell aus (Element lesen, jeden Wert
   einfügen, Wurzel-Hash aktualisieren)
3. Konvertiert jede Gruppe in eine `ReplaceNonMerkTreeRoot`-Op, die den finalen
   `root_hash` und `count` durch die Standard-Propagierungsmaschinerie transportiert

Mehrere Einfügungen in denselben dichten Baum innerhalb eines einzelnen Stapels werden
unterstützt — sie werden in Reihenfolge verarbeitet und die Konsistenzprüfung erlaubt
doppelte Schlüssel für diesen Op-Typ.

**Propagierung:** Der Wurzel-Hash und count fließen durch die
`NonMerkTreeMeta::DenseTree`-Variante in `ReplaceNonMerkTreeRoot` und folgen
demselben Muster wie MmrTree und BulkAppendTree.

## Beweise

Der DenseAppendOnlyFixedSizeTree unterstützt **V1-Unterabfrage-Beweise** über die
`ProofBytes::DenseTree`-Variante. Einzelne Positionen können gegen den Wurzel-Hash
des Baums mittels Inklusionsbeweisen bewiesen werden, die Vorfahrenwerte und
Geschwister-Teilbaum-Hashes enthalten.

### Authentifizierungspfad-Struktur

Da interne Knoten **ihren eigenen Wert** hashen (nicht nur Kind-Hashes), unterscheidet
sich der Authentifizierungspfad von einem Standard-Merkle-Baum. Um ein Blatt an Position
`p` zu verifizieren, benötigt der Verifizierer:

1. **Den Blattwert** (der bewiesene Eintrag)
2. **Vorfahren-Wert-Hashes** für jeden internen Knoten auf dem Pfad von `p` zur Wurzel (nur der 32-Byte-Hash, nicht der vollständige Wert)
3. **Geschwister-Teilbaum-Hashes** für jedes Kind, das NICHT auf dem Pfad liegt

Da alle Knoten `blake3(H(value) || H(left) || H(right))` verwenden (keine Domänen-Tags),
enthält der Beweis nur 32-Byte-Wert-Hashes für Vorfahren — keine vollständigen Werte.
Dies hält Beweise kompakt, unabhängig davon wie groß einzelne Werte sind.

```rust
pub struct DenseTreeProof {
    pub entries: Vec<(u16, Vec<u8>)>,            // proved (position, value) pairs
    pub node_value_hashes: Vec<(u16, [u8; 32])>, // ancestor value hashes on the auth path
    pub node_hashes: Vec<(u16, [u8; 32])>,       // precomputed sibling subtree hashes
}
```

> **Hinweis:** `height` und `count` befinden sich nicht in der Beweisstruktur — der Verifizierer erhält sie aus dem übergeordneten Element, das durch die Merk-Hierarchie authentifiziert ist.

### Durchlaufbeispiel

Baum mit height=3, capacity=7, count=5, Beweis für Position 4:

```text
        0
       / \
      1   2
     / \ / \
    3  4 5  6
```

Pfad von 4 zur Wurzel: `4 → 1 → 0`. Erweiterte Menge: `{0, 1, 4}`.

Der Beweis enthält:
- **entries**: `[(4, value[4])]` — die bewiesene Position
- **node_value_hashes**: `[(0, H(value[0])), (1, H(value[1]))]` — Vorfahren-Wert-Hashes (je 32 Bytes, nicht vollständige Werte)
- **node_hashes**: `[(2, H(subtree_2)), (3, H(node_3))]` — Geschwister, die nicht auf dem Pfad liegen

Die Verifikation berechnet den Wurzel-Hash von unten nach oben neu:
1. `H(4) = blake3(blake3(value[4]) || [0;32] || [0;32])` — Blatt (Kinder sind ungefüllt)
2. `H(3)` — aus `node_hashes`
3. `H(1) = blake3(H(value[1]) || H(3) || H(4))` — intern, verwendet Wert-Hash aus `node_value_hashes`
4. `H(2)` — aus `node_hashes`
5. `H(0) = blake3(H(value[0]) || H(1) || H(2))` — Wurzel, verwendet Wert-Hash aus `node_value_hashes`
6. Vergleiche `H(0)` mit erwartetem Wurzel-Hash

### Multi-Positions-Beweise

Beim Beweisen mehrerer Positionen werden die erweiterten Mengen überlappender
Authentifizierungspfade zusammengeführt. Gemeinsame Vorfahren werden nur einmal
einbezogen, was Multi-Positions-Beweise kompakter macht als unabhängige
Einzel-Positions-Beweise.

### V0-Einschränkung

V0-Beweise können nicht in dichte Bäume absteigen. Wenn eine V0-Abfrage einen
`DenseAppendOnlyFixedSizeTree` mit einer Unterabfrage trifft, gibt das System
`Error::NotSupported` zurück und verweist den Aufrufer auf `prove_query_v1`.

### Abfrage-Schlüssel-Kodierung

Dichte-Baum-Positionen werden als **Big-Endian-u16** (2 Byte) Abfrageschlüssel kodiert,
im Gegensatz zu MmrTree und BulkAppendTree, die u64 verwenden. Alle Standard-`QueryItem`-
Bereichstypen werden unterstützt.

## Vergleich mit anderen Nicht-Merk-Bäumen

| | DenseTree | MmrTree | BulkAppendTree | CommitmentTree |
|---|---|---|---|---|
| **Element-Diskriminante** | 14 | 12 | 13 | 11 |
| **TreeType** | 10 | 8 | 9 | 7 |
| **Kapazität** | Fest (`2^h - 1`, max 65.535) | Unbegrenzt | Unbegrenzt | Unbegrenzt |
| **Datenmodell** | Jede Position speichert einen Wert | Nur Blätter | Dichter-Baum-Puffer + Chunks | Nur Blätter |
| **Hash im Element?** | Nein (fließt als Kind-Hash) | Nein (fließt als Kind-Hash) | Nein (fließt als Kind-Hash) | Nein (fließt als Kind-Hash) |
| **Einfügekosten (Hashing)** | O(n) Blake3 | O(1) amortisiert | O(1) amortisiert | ~33 Sinsemilla |
| **Kostengröße** | 6 Bytes | 11 Bytes | 12 Bytes | 12 Bytes |
| **Beweis-Unterstützung** | V1 (Dense) | V1 (MMR) | V1 (Bulk) | V1 (CommitmentTree) |
| **Am besten für** | Kleine begrenzte Strukturen | Ereignis-Logs | Hochdurchsatz-Logs | ZK-Verpflichtungen |

**Wann DenseAppendOnlyFixedSizeTree wählen:**
- Die maximale Anzahl von Einträgen ist bei der Erstellung bekannt
- Jede Position (einschließlich interner Knoten) soll Daten speichern
- Das einfachstmögliche Datenmodell ohne unbegrenztes Wachstum ist gewünscht
- O(n) Wurzel-Hash-Neuberechnung ist akzeptabel (kleine Baumhöhen)

**Wann NICHT wählen:**
- Unbegrenzte Kapazität wird benötigt → MmrTree oder BulkAppendTree verwenden
- ZK-Kompatibilität wird benötigt → CommitmentTree verwenden

## Verwendungsbeispiel

```rust
use grovedb::Element;
use grovedb_version::version::GroveVersion;

let grove_version = GroveVersion::latest();

// Create a dense tree of height 4 (capacity = 15 values)
db.insert(
    &[b"state"],
    b"validator_slots",
    Element::empty_dense_tree(4),
    None,
    None,
    grove_version,
)?;

// Append values — positions filled 0, 1, 2, ...
let (root_hash, pos) = db.dense_tree_insert(
    &[b"state"],
    b"validator_slots",
    validator_pubkey.to_vec(),
    None,
    grove_version,
)?;
// pos == 0, root_hash = blake3(validator_pubkey)

// Read back by position
let value = db.dense_tree_get(
    &[b"state"],
    b"validator_slots",
    0,     // position
    None,
    grove_version,
)?;
assert_eq!(value, Some(validator_pubkey.to_vec()));

// Query metadata
let count = db.dense_tree_count(&[b"state"], b"validator_slots", None, grove_version)?;
let hash = db.dense_tree_root_hash(&[b"state"], b"validator_slots", None, grove_version)?;
```

## Implementierungsdateien

| Datei | Inhalt |
|---|---|
| `grovedb-dense-fixed-sized-merkle-tree/src/lib.rs` | `DenseTreeStore`-Trait, `DenseFixedSizedMerkleTree`-Struktur, rekursiver Hash |
| `grovedb-dense-fixed-sized-merkle-tree/src/proof.rs` | `DenseTreeProof`-Struktur, `generate()`, `encode_to_vec()`, `decode_from_slice()` |
| `grovedb-dense-fixed-sized-merkle-tree/src/verify.rs` | `DenseTreeProof::verify()` — reine Funktion, kein Speicher nötig |
| `grovedb-element/src/element/mod.rs` | `Element::DenseAppendOnlyFixedSizeTree` (Diskriminante 14) |
| `grovedb-element/src/element/constructor.rs` | `empty_dense_tree()`, `new_dense_tree()` |
| `merk/src/tree_type/mod.rs` | `TreeType::DenseAppendOnlyFixedSizeTree = 10` |
| `merk/src/tree_type/costs.rs` | `DENSE_TREE_COST_SIZE = 6` |
| `grovedb/src/operations/dense_tree.rs` | GroveDB-Operationen, `AuxDenseTreeStore`, Stapelvorverarbeitung |
| `grovedb/src/operations/proof/generate.rs` | `generate_dense_tree_layer_proof()`, `query_items_to_positions()` |
| `grovedb/src/operations/proof/verify.rs` | `verify_dense_tree_lower_layer()` |
| `grovedb/src/operations/proof/mod.rs` | `ProofBytes::DenseTree`-Variante |
| `grovedb/src/batch/estimated_costs/average_case_costs.rs` | Durchschnittliches Kostenmodell |
| `grovedb/src/batch/estimated_costs/worst_case_costs.rs` | Worst-Case-Kostenmodell |
| `grovedb/src/tests/dense_tree_tests.rs` | 22 Integrationstests |

---
