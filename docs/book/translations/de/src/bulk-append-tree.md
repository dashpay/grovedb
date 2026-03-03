# Der BulkAppendTree — Hochdurchsatz-Append-Only-Speicher

Der BulkAppendTree ist GroveDBs Antwort auf eine spezifische Ingenieurherausforderung: Wie baut
man ein Hochdurchsatz-Append-Only-Log, das effiziente Bereichsbeweise unterstützt, das
Hashing pro Schreibvorgang minimiert und unveränderliche Chunk-Snapshots erzeugt, die für
CDN-Verteilung geeignet sind?

Während ein MmrTree (Kapitel 13) ideal für individuelle Blattbeweise ist, ist der BulkAppendTree
für Arbeitslasten konzipiert, bei denen Tausende von Werten pro Block eintreffen und Clients
Daten durch Abruf von Bereichen synchronisieren müssen. Er erreicht dies mit einer
**Zweistufenarchitektur**: einem dichten Merkle-Baum-Puffer, der eingehende Anhänge aufnimmt,
und einem Chunk-MMR auf der übergeordneten Ebene, der finalisierte Chunk-Wurzeln aufzeichnet.

## Die Zweistufenarchitektur

```text
┌────────────────────────────────────────────────────────────────┐
│                      BulkAppendTree                            │
│                                                                │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  Chunk MMR                                               │  │
│  │  ┌────┐ ┌────┐ ┌────┐ ┌────┐                            │  │
│  │  │ R0 │ │ R1 │ │ R2 │ │ H  │ ← Dense Merkle roots      │  │
│  │  └────┘ └────┘ └────┘ └────┘   of each chunk blob       │  │
│  │                     peak hashes bagged together = MMR root│  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  Buffer (DenseFixedSizedMerkleTree, capacity = 2^h - 1) │  │
│  │  ┌───┐ ┌───┐ ┌───┐                                      │  │
│  │  │v_0│ │v_1│ │v_2│ ... (fills in level-order)           │  │
│  │  └───┘ └───┘ └───┘                                      │  │
│  │  dense_tree_root = recomputed root hash of dense tree     │  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                │
│  state_root = blake3("bulk_state" || mmr_root || dense_tree_root) │
│                                                                │
└────────────────────────────────────────────────────────────────┘
```

**Stufe 1 — Der Puffer.** Eingehende Werte werden in einen `DenseFixedSizedMerkleTree`
geschrieben (siehe Kapitel 16). Die Pufferkapazität beträgt `2^height - 1` Positionen. Der
Wurzel-Hash des dichten Baums (`dense_tree_root`) wird nach jedem Einfügen aktualisiert.

**Stufe 2 — Der Chunk-MMR.** Wenn der Puffer voll ist (die `chunk_size`-Einträge erreicht),
werden alle Einträge in einen unveränderlichen **Chunk-Blob** serialisiert, eine dichte
Merkle-Wurzel wird über diese Einträge berechnet, und diese Wurzel wird als Blatt an den
Chunk-MMR angehängt. Der Puffer wird dann geleert.

Die **Zustandswurzel** (State Root) kombiniert beide Stufen zu einer einzigen 32-Byte-Verpflichtung,
die sich bei jedem Anhängen ändert und sicherstellt, dass der übergeordnete Merk-Baum stets
den aktuellen Zustand widerspiegelt.

## Wie Werte den Puffer füllen

Jeder Aufruf von `append()` folgt dieser Sequenz:

```text
Step 1: Write value to dense tree buffer at next position
        dense_tree.insert(value, store)

Step 2: Increment total_count
        total_count += 1

Step 3: Check if buffer is full (dense tree at capacity)
        if dense_tree.count() == capacity:
            → trigger compaction (§14.3)

Step 4: Compute new state root (+1 blake3 call)
        dense_tree_root = dense_tree.root_hash(store)
        state_root = blake3("bulk_state" || mmr_root || dense_tree_root)
```

Der **Puffer IST ein DenseFixedSizedMerkleTree** (siehe Kapitel 16). Sein Wurzel-Hash
ändert sich nach jedem Einfügen und liefert eine Verpflichtung für alle aktuellen
Puffereinträge. Dieser Wurzel-Hash fließt in die Berechnung der Zustandswurzel ein.

## Chunk-Kompaktierung

Wenn der Puffer voll ist (die `chunk_size`-Einträge erreicht), wird die Kompaktierung
automatisch ausgelöst:

```text
Compaction Steps:
─────────────────
1. Read all chunk_size buffer entries

2. Compute dense Merkle root
   - Hash each entry: leaf[i] = blake3(entry[i])
   - Build complete binary tree bottom-up
   - Extract root hash
   Hash cost: chunk_size + (chunk_size - 1) = 2 * chunk_size - 1

3. Serialize entries into chunk blob
   - Auto-selects fixed-size or variable-size format (§14.6)
   - Store as: store.put(chunk_key(chunk_index), blob)

4. Append dense Merkle root to chunk MMR
   - MMR push with merge cascade (see Chapter 13)
   Hash cost: ~2 amortized (trailing_ones pattern)

5. Reset the dense tree (clear all buffer entries from storage)
   - Dense tree count reset to 0
```

Nach der Kompaktierung ist der Chunk-Blob **dauerhaft unveränderlich** — er ändert sich nie
wieder. Dies macht Chunk-Blobs ideal für CDN-Caching, Client-Synchronisation und
Archiv-Speicherung.

**Beispiel: 4 Anhänge mit chunk_power=2 (chunk_size=4)**

```text
Append v_0: dense_tree=[v_0],       dense_root=H(v_0), total=1
Append v_1: dense_tree=[v_0,v_1],   dense_root=H(v_0,v_1), total=2
Append v_2: dense_tree=[v_0..v_2],  dense_root=H(v_0..v_2), total=3
Append v_3: dense_tree=[v_0..v_3],  dense_root=H(v_0..v_3), total=4
  → COMPACTION:
    chunk_blob_0 = serialize([v_0, v_1, v_2, v_3])
    dense_root_0 = dense_merkle_root(v_0..v_3)
    mmr.push(dense_root_0)
    dense tree cleared (count=0)

Append v_4: dense_tree=[v_4],       dense_root=H(v_4), total=5
  → state_root = blake3("bulk_state" || mmr_root || dense_root)
```

## Die Zustandswurzel

Die Zustandswurzel verbindet beide Stufen in einem Hash:

```rust
fn compute_state_root(
    mmr_root: &[u8; 32],         // Chunk MMR root (or [0;32] if empty)
    dense_tree_root: &[u8; 32],  // Root hash of current buffer (dense tree)
) -> [u8; 32] {
    blake3("bulk_state" || mmr_root || dense_tree_root)
}
```

Die `total_count` und `chunk_power` sind **nicht** in der Zustandswurzel enthalten, da
sie bereits durch den Merk-Value-Hash authentifiziert werden — sie sind Felder des
serialisierten `Element`, das im übergeordneten Merk-Knoten gespeichert ist. Die
Zustandswurzel erfasst nur die Datenverpflichtungen (`mmr_root` und `dense_tree_root`).
Dies ist der Hash, der als Merk-Kind-Hash fließt und sich bis zum GroveDB-Wurzel-Hash
nach oben propagiert.

## Die dichte Merkle-Wurzel

Wenn ein Chunk kompaktiert wird, benötigen die Einträge eine einzelne 32-Byte-Verpflichtung.
Der BulkAppendTree verwendet einen **dichten (vollständigen) binären Merkle-Baum**:

```text
Given entries [e_0, e_1, e_2, e_3]:

Level 0 (leaves):  blake3(e_0)  blake3(e_1)  blake3(e_2)  blake3(e_3)
                      \__________/              \__________/
Level 1:           blake3(h_0 || h_1)       blake3(h_2 || h_3)
                          \____________________/
Level 2 (root):    blake3(h_01 || h_23)  ← this is the dense Merkle root
```

Da `chunk_size` immer eine Zweierpotenz ist (konstruktionsbedingt: `1u32 << chunk_power`),
ist der Baum immer vollständig (kein Padding oder Dummy-Blätter nötig). Die Hash-Anzahl
beträgt genau `2 * chunk_size - 1`:
- `chunk_size` Blatt-Hashes (einer pro Eintrag)
- `chunk_size - 1` interne Knoten-Hashes

Die Implementierung der dichten Merkle-Wurzel befindet sich in `grovedb-mmr/src/dense_merkle.rs`
und bietet zwei Funktionen:
- `compute_dense_merkle_root(hashes)` — aus vorgehashten Blättern
- `compute_dense_merkle_root_from_values(values)` — hasht Werte zuerst, baut dann den Baum

## Chunk-Blob-Serialisierung

Chunk-Blobs sind die unveränderlichen Archive, die durch Kompaktierung erzeugt werden.
Der Serialisierer wählt automatisch das kompakteste Drahtformat basierend auf
Eintragsgrößen:

**Festes Format** (Flag `0x01`) — wenn alle Einträge die gleiche Länge haben:

```text
┌──────┬──────────┬─────────────┬─────────┬─────────┬─────────┐
│ 0x01 │ count    │ entry_size  │ entry_0 │ entry_1 │ ...     │
│ 1B   │ 4B (BE)  │ 4B (BE)     │ N bytes │ N bytes │         │
└──────┴──────────┴─────────────┴─────────┴─────────┴─────────┘
Total: 1 + 4 + 4 + (count × entry_size) bytes
```

**Variables Format** (Flag `0x00`) — wenn Einträge unterschiedliche Längen haben:

```text
┌──────┬──────────┬─────────┬──────────┬─────────┬─────────────┐
│ 0x00 │ len_0    │ entry_0 │ len_1    │ entry_1 │ ...         │
│ 1B   │ 4B (BE)  │ N bytes │ 4B (BE)  │ M bytes │             │
└──────┴──────────┴─────────┴──────────┴─────────┴─────────────┘
Total: 1 + Σ(4 + len_i) bytes
```

Das feste Format spart 4 Bytes pro Eintrag im Vergleich zum variablen Format, was sich
bei großen Chunks gleichförmiger Daten (wie 32-Byte-Hash-Verpflichtungen) erheblich summiert.
Für 1024 Einträge zu je 32 Bytes:
- Fest: `1 + 4 + 4 + 32768 = 32.777 Bytes`
- Variabel: `1 + 1024 × (4 + 32) = 36.865 Bytes`
- Ersparnis: ~11%

## Speicher-Schlüssel-Layout

Alle BulkAppendTree-Daten befinden sich im **Daten**-Namensraum, mit Einzelzeichen-Präfixen als Schlüssel:

| Schlüsselmuster | Format | Größe | Zweck |
|---|---|---|---|
| `M` | 1 Byte | 1B | Metadaten-Schlüssel |
| `b` + `{index}` | `b` + u32 BE | 5B | Puffereintrag am Index |
| `e` + `{index}` | `e` + u64 BE | 9B | Chunk-Blob am Index |
| `m` + `{pos}` | `m` + u64 BE | 9B | MMR-Knoten an Position |

**Metadaten** speichern `mmr_size` (8 Bytes BE). Die `total_count` und `chunk_power` werden
im Element selbst (im übergeordneten Merk) gespeichert, nicht in den Metadaten des
Daten-Namensraums. Diese Aufteilung bedeutet, dass das Lesen der Anzahl eine einfache
Element-Abfrage ist, ohne den Datenspeicherkontext öffnen zu müssen.

Puffer-Schlüssel verwenden u32-Indizes (0 bis `chunk_size - 1`), da die Pufferkapazität
durch `chunk_size` begrenzt ist (ein u32, berechnet als `1u32 << chunk_power`).
Chunk-Schlüssel verwenden u64-Indizes, da die Anzahl abgeschlossener Chunks unbegrenzt
wachsen kann.

## Die BulkAppendTree-Struktur

```rust
pub struct BulkAppendTree<S> {
    pub total_count: u64,                        // Total values ever appended
    pub dense_tree: DenseFixedSizedMerkleTree<S>, // The buffer (dense tree)
}
```

Der Puffer IST ein `DenseFixedSizedMerkleTree` — sein Wurzel-Hash ist `dense_tree_root`.

**Zugriffsmethoden:**
- `capacity() -> u16`: `dense_tree.capacity()` (= `2^height - 1`)
- `epoch_size() -> u64`: `capacity + 1` (= `2^height`, die Anzahl der Einträge pro Chunk)
- `height() -> u8`: `dense_tree.height()`

**Abgeleitete Werte** (nicht gespeichert):

| Wert | Formel |
|---|---|
| `chunk_count` | `total_count / epoch_size` |
| `buffer_count` | `dense_tree.count()` |
| `mmr_size` | `leaf_count_to_mmr_size(chunk_count)` |

## GroveDB-Operationen

Der BulkAppendTree integriert sich über sechs Operationen in GroveDB, definiert in
`grovedb/src/operations/bulk_append_tree.rs`:

### bulk_append

Die primäre mutierende Operation. Folgt dem Standard-GroveDB-Nicht-Merk-Speichermuster:

```text
1. Validate element is BulkAppendTree
2. Open data storage context
3. Load tree from store
4. Append value (may trigger compaction)
5. Update element in parent Merk with new state_root + total_count
6. Propagate changes up through Merk hierarchy
7. Commit transaction
```

Der `AuxBulkStore`-Adapter umhüllt GroveDBs `get_aux`/`put_aux`/`delete_aux`-Aufrufe und
akkumuliert `OperationCost` in einem `RefCell` für die Kostenverfolgung. Hash-Kosten
der Anhänge-Operation werden zu `cost.hash_node_calls` addiert.

### Leseoperationen

| Operation | Rückgabewert | Datenspeicher? |
|---|---|---|
| `bulk_get_value(path, key, position)` | Wert an globaler Position | Ja — liest aus Chunk-Blob oder Puffer |
| `bulk_get_chunk(path, key, chunk_index)` | Roher Chunk-Blob | Ja — liest Chunk-Schlüssel |
| `bulk_get_buffer(path, key)` | Alle aktuellen Puffereinträge | Ja — liest Puffer-Schlüssel |
| `bulk_count(path, key)` | Gesamtanzahl (u64) | Nein — liest aus Element |
| `bulk_chunk_count(path, key)` | Abgeschlossene Chunks (u64) | Nein — berechnet aus Element |

Die `get_value`-Operation routet transparent nach Position:

```text
if position < completed_chunks × chunk_size:
    chunk_idx = position / chunk_size
    intra_idx = position % chunk_size
    → read chunk blob, deserialize, return entry[intra_idx]
else:
    buffer_idx = position % chunk_size
    → read buffer_key(buffer_idx)
```

## Stapeloperationen und Vorverarbeitung

Der BulkAppendTree unterstützt Stapeloperationen über die `GroveOp::BulkAppend`-Variante.
Da `execute_ops_on_path` keinen Zugriff auf den Datenspeicherkontext hat, müssen alle
BulkAppend-Ops vor `apply_body` vorverarbeitet werden.

Die Vorverarbeitungspipeline:

```text
Input: [BulkAppend{v1}, Insert{...}, BulkAppend{v2}, BulkAppend{v3}]
                                     ↑ same (path,key) as v1

Step 1: Group BulkAppend ops by (path, key)
        group_1: [v1, v2, v3]

Step 2: For each group:
        a. Read existing element → get (total_count, chunk_power)
        b. Open transactional storage context
        c. Load BulkAppendTree from store
        d. Load existing buffer into memory (Vec<Vec<u8>>)
        e. For each value:
           tree.append_with_mem_buffer(store, value, &mut mem_buffer)
        f. Save metadata
        g. Compute final state_root

Step 3: Replace all BulkAppend ops with one ReplaceNonMerkTreeRoot per group
        carrying: hash=state_root, meta=BulkAppendTree{total_count, chunk_power}

Output: [ReplaceNonMerkTreeRoot{...}, Insert{...}]
```

Die `append_with_mem_buffer`-Variante vermeidet Lese-nach-Schreib-Probleme: Puffereinträge
werden in einem `Vec<Vec<u8>>` im Speicher gehalten, sodass die Kompaktierung sie lesen kann,
obwohl der transaktionale Speicher noch nicht committet wurde.

## Das BulkStore-Trait

```rust
pub trait BulkStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String>;
    fn delete(&self, key: &[u8]) -> Result<(), String>;
}
```

Die Methoden nehmen `&self` (nicht `&mut self`), um dem Interior-Mutability-Muster von
GroveDB zu entsprechen, bei dem Schreibvorgänge über einen Batch laufen. Die GroveDB-Integration
implementiert dies über `AuxBulkStore`, der einen `StorageContext` umhüllt und
`OperationCost` akkumuliert.

Der `MmrAdapter` verbindet `BulkStore` mit den `MMRStoreReadOps`/`MMRStoreWriteOps`-Traits
des ckb-MMR und fügt einen Write-Through-Cache für Lese-nach-Schreib-Korrektheit hinzu.

## Beweis-Erzeugung

BulkAppendTree-Beweise unterstützen **Bereichsabfragen** über Positionen. Die Beweisstruktur
erfasst alles, was ein zustandsloser Verifizierer benötigt, um zu bestätigen, dass
bestimmte Daten im Baum existieren:

```rust
pub struct BulkAppendTreeProof {
    pub chunk_power: u8,
    pub total_count: u64,
    pub chunk_blobs: Vec<(u64, Vec<u8>)>,         // Full chunk blobs
    pub chunk_mmr_size: u64,
    pub chunk_mmr_proof_items: Vec<[u8; 32]>,      // MMR sibling hashes
    pub chunk_mmr_leaves: Vec<(u64, [u8; 32])>,    // (leaf_idx, dense_root)
    pub buffer_entries: Vec<Vec<u8>>,               // ALL buffer entries
    pub chunk_mmr_root: [u8; 32],
}
```

**Erzeugungsschritte** für einen Bereich `[start, end)` (mit `chunk_size = 1u32 << chunk_power`):

```text
1. Determine overlapping chunks
   first_chunk = start / chunk_size
   last_chunk  = min((end-1) / chunk_size, completed_chunks - 1)

2. Read chunk blobs for overlapping chunks
   For each chunk_idx in [first_chunk, last_chunk]:
     chunk_blobs.push((chunk_idx, store.get(chunk_key(idx))))

3. Compute dense Merkle root for each chunk blob
   For each blob:
     deserialize → values
     dense_root = compute_dense_merkle_root_from_values(values)
     chunk_mmr_leaves.push((chunk_idx, dense_root))

4. Generate MMR proof for those chunk positions
   positions = chunk_indices.map(|idx| leaf_to_pos(idx))
   proof = mmr.gen_proof(positions)
   chunk_mmr_proof_items = proof.proof_items().map(|n| n.hash)

5. Get chunk MMR root

6. Read ALL buffer entries (bounded by chunk_size)
   for i in 0..buffer_count:
     buffer_entries.push(store.get(buffer_key(i)))
```

**Warum ALLE Puffereinträge einschließen?** Der Puffer ist ein dichter Merkle-Baum, dessen
Wurzel-Hash sich auf jeden Eintrag verpflichtet. Der Verifizierer muss den Baum aus allen
Einträgen neu aufbauen, um den `dense_tree_root` zu verifizieren. Da der Puffer durch
`capacity` begrenzt ist (höchstens 65.535 Einträge), sind die Kosten vertretbar.

## Beweis-Verifikation

Die Verifikation ist eine reine Funktion — kein Datenbankzugriff nötig. Sie führt fünf
Prüfungen durch:

```text
Step 0: Metadata consistency
        - chunk_power <= 31
        - buffer_entries.len() == total_count % chunk_size
        - MMR leaf count == completed_chunks

Step 1: Chunk blob integrity
        For each (chunk_idx, blob):
          values = deserialize(blob)
          computed_root = dense_merkle_root(values)
          assert computed_root == chunk_mmr_leaves[chunk_idx]

Step 2: Chunk MMR proof
        Reconstruct MmrNode leaves and proof items
        proof.verify(chunk_mmr_root, leaves) == true

Step 3: Buffer (dense tree) integrity
        Rebuild DenseFixedSizedMerkleTree from buffer_entries
        dense_tree_root = compute root hash of rebuilt tree

Step 4: State root
        computed = blake3("bulk_state" || chunk_mmr_root || dense_tree_root)
        assert computed == expected_state_root
```

Nach erfolgreicher Verifikation bietet das `BulkAppendTreeProofResult` eine
`values_in_range(start, end)`-Methode, die spezifische Werte aus den verifizierten
Chunk-Blobs und Puffereinträgen extrahiert.

## Wie es mit dem GroveDB-Wurzel-Hash zusammenhängt

Der BulkAppendTree ist ein **Nicht-Merk-Baum** — er speichert Daten im Daten-Namensraum,
nicht in einem Kind-Merk-Teilbaum. Im übergeordneten Merk wird das Element gespeichert als:

```text
Element::BulkAppendTree(total_count, chunk_power, flags)
```

Die Zustandswurzel fließt als Merk-Kind-Hash. Der Hash des übergeordneten Merk-Knotens ist:

```text
combine_hash(value_hash(element_bytes), state_root)
```

Die `state_root` fließt als Merk-Kind-Hash (über den `subtree_root_hash`-Parameter
von `insert_subtree`). Jede Änderung der Zustandswurzel propagiert sich nach oben
durch die GroveDB-Merk-Hierarchie zum Wurzel-Hash.

In V1-Beweisen (§9.6) beweist der übergeordnete Merk-Beweis die Element-Bytes und die
Kind-Hash-Bindung, und der `BulkAppendTreeProof` beweist, dass die abgefragten Daten
konsistent mit der als Kind-Hash verwendeten `state_root` sind.

## Kostenverfolgung

Die Hash-Kosten jeder Operation werden explizit verfolgt:

| Operation | Blake3-Aufrufe | Anmerkungen |
|---|---|---|
| Einzelnes Anhängen (ohne Kompaktierung) | 3 | 2 für Puffer-Hash-Kette + 1 für Zustandswurzel |
| Einzelnes Anhängen (mit Kompaktierung) | 3 + 2C - 1 + ~2 | Kette + dichter Merkle (C=chunk_size) + MMR-Push + Zustandswurzel |
| `get_value` aus Chunk | 0 | Reine Deserialisierung, kein Hashing |
| `get_value` aus Puffer | 0 | Direkter Schlüsselzugriff |
| Beweis-Erzeugung | Abhängig von Chunk-Anzahl | Dichte Merkle-Wurzel pro Chunk + MMR-Beweis |
| Beweis-Verifikation | 2C·K - K + B·2 + 1 | K Chunks, B Puffereinträge, C chunk_size |

**Amortisierte Kosten pro Anhängen**: Für chunk_size=1024 (chunk_power=10) wird der
Kompaktierungsaufwand von ~2047 Hashes (dichte Merkle-Wurzel) über 1024 Anhänge amortisiert,
was ~2 Hashes pro Anhängen hinzufügt. Zusammen mit den 3 Hashes pro Anhängen ergibt sich
ein amortisierter Gesamtwert von **~5 Blake3-Aufrufen pro Anhängen** — sehr effizient für
eine kryptographisch authentifizierte Struktur.

## Vergleich mit MmrTree

| | BulkAppendTree | MmrTree |
|---|---|---|
| **Architektur** | Zweistufig (Puffer + Chunk-MMR) | Einzelner MMR |
| **Hash-Kosten pro Anhängen** | 3 (+ amortisiert ~2 für Kompaktierung) | ~2 |
| **Beweis-Granularität** | Bereichsabfragen über Positionen | Individuelle Blattbeweise |
| **Unveränderliche Snapshots** | Ja (Chunk-Blobs) | Nein |
| **CDN-freundlich** | Ja (Chunk-Blobs cachebar) | Nein |
| **Puffereinträge** | Ja (alle für Beweis nötig) | Entfällt |
| **Am besten für** | Hochdurchsatz-Logs, Massen-Sync | Ereignis-Logs, einzelne Abfragen |
| **Element-Diskriminante** | 13 | 12 |
| **TreeType** | 9 | 8 |

Wählen Sie MmrTree, wenn Sie individuelle Blattbeweise mit minimalem Overhead benötigen.
Wählen Sie BulkAppendTree, wenn Sie Bereichsabfragen, Massensynchronisation und
chunk-basierte Snapshots benötigen.

## Implementierungsdateien

| Datei | Zweck |
|------|---------|
| `grovedb-bulk-append-tree/src/lib.rs` | Crate-Wurzel, Re-Exports |
| `grovedb-bulk-append-tree/src/tree/mod.rs` | `BulkAppendTree`-Struktur, Zustandszugriffe, Metadaten-Persistenz |
| `grovedb-bulk-append-tree/src/tree/append.rs` | `append()`, `append_with_mem_buffer()`, `compact_entries()` |
| `grovedb-bulk-append-tree/src/tree/hash.rs` | `compute_state_root` |
| `grovedb-bulk-append-tree/src/tree/keys.rs` | `META_KEY`, `buffer_key`, `chunk_key`, `mmr_node_key` |
| `grovedb-bulk-append-tree/src/tree/query.rs` | `get_value`, `get_chunk`, `get_buffer` |
| `grovedb-bulk-append-tree/src/tree/mmr_adapter.rs` | `MmrAdapter` mit Write-Through-Cache |
| `grovedb-bulk-append-tree/src/chunk.rs` | Chunk-Blob-Serialisierung (feste + variable Formate) |
| `grovedb-bulk-append-tree/src/proof.rs` | `BulkAppendTreeProof`-Erzeugung und -Verifikation |
| `grovedb-bulk-append-tree/src/store.rs` | `BulkStore`-Trait |
| `grovedb-bulk-append-tree/src/error.rs` | `BulkAppendError`-Aufzählung |
| `grovedb/src/operations/bulk_append_tree.rs` | GroveDB-Operationen, `AuxBulkStore`, Stapelvorverarbeitung |
| `grovedb/src/tests/bulk_append_tree_tests.rs` | 27 Integrationstests |

---
