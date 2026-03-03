# Il DenseAppendOnlyFixedSizeTree — Archiviazione Merkle densa a capacita fissa

Il DenseAppendOnlyFixedSizeTree e un albero binario completo di altezza fissa dove **ogni nodo** — sia interno che foglia — memorizza un valore di dati. Le posizioni vengono riempite sequenzialmente in ordine per livelli (BFS): prima la radice (posizione 0), poi da sinistra a destra ad ogni livello. Nessun hash intermedio viene persistito; l'hash radice viene ricalcolato al volo attraverso un hashing ricorsivo dalle foglie alla radice.

Questo design e ideale per piccole strutture dati a dimensione fissa dove la capacita massima e nota in anticipo e serve un'aggiunta O(1), un recupero per posizione O(1) e un impegno (commitment) compatto di 32 byte sotto forma di hash radice che cambia dopo ogni inserimento.

## Struttura dell'albero

Un albero di altezza *h* ha una capacita di `2^h - 1` posizioni. Le posizioni usano indicizzazione a base 0 in ordine per livelli:

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

I valori vengono aggiunti sequenzialmente: il primo valore va alla posizione 0 (radice), poi posizione 1, 2, 3, e cosi via. Questo significa che la radice ha sempre dati, e l'albero si riempie in ordine per livelli — l'ordine di attraversamento piu naturale per un albero binario completo.

## Calcolo dell'hash

L'hash radice non e memorizzato separatamente — viene ricalcolato da zero ogni volta che e necessario. L'algoritmo ricorsivo visita solo le posizioni riempite:

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

**Proprieta chiave:**
- Tutti i nodi (foglia e interni): `blake3(blake3(valore) || H(sinistro) || H(destro))`
- Nodi foglia: left_hash e right_hash sono entrambi `[0; 32]` (figli non riempiti)
- Posizioni non riempite: `[0u8; 32]` (hash zero)
- Albero vuoto (count = 0): `[0u8; 32]`

**Non vengono usati tag di separazione di dominio foglia/interno.** La struttura dell'albero (`height` e `count`) e autenticata esternamente nell'`Element::DenseAppendOnlyFixedSizeTree` genitore, che fluisce attraverso la gerarchia Merk. Il verificatore sa sempre esattamente quali posizioni sono foglie vs nodi interni dall'altezza e dal conteggio, quindi un attaccante non puo sostituire l'uno con l'altro senza rompere la catena di autenticazione del genitore.

Questo significa che l'hash radice codifica un impegno su ogni valore memorizzato e la sua esatta posizione nell'albero. Cambiare qualsiasi valore (se fosse mutabile) si propagherebbe a cascata attraverso tutti gli hash degli antenati fino alla radice.

**Costo degli hash:** Calcolare l'hash radice visita tutte le posizioni riempite piu eventuali figli non riempiti. Per un albero con *n* valori, il caso peggiore e O(*n*) chiamate blake3. Questo e accettabile perche l'albero e progettato per capacita piccole e limitate (altezza massima 16, massimo 65.535 posizioni).

## La variante Element

```rust
Element::DenseAppendOnlyFixedSizeTree(
    u16,                   // count — number of values stored (max 65,535)
    u8,                    // height — immutable after creation (1..=16)
    Option<ElementFlags>,  // flags — storage flags
)
```

| Campo | Tipo | Descrizione |
|---|---|---|
| `count` | `u16` | Numero di valori inseriti finora (massimo 65.535) |
| `height` | `u8` | Altezza dell'albero (1..=16), immutabile dopo la creazione |
| `flags` | `Option<ElementFlags>` | Flag di archiviazione opzionali |

L'hash radice NON e memorizzato nell'Element — fluisce come hash figlio del Merk tramite il parametro `subtree_root_hash` di `insert_subtree`.

**Discriminante:** 14 (ElementType), TreeType = 10

**Dimensione costo:** `DENSE_TREE_COST_SIZE = 6` byte (2 count + 1 height + 1 discriminante + 2 overhead)

## Layout di archiviazione

Come MmrTree e BulkAppendTree, il DenseAppendOnlyFixedSizeTree memorizza i dati nello spazio dei nomi **dati** (non in un Merk figlio). I valori sono indicizzati dalla loro posizione come `u64` big-endian:

```text
Subtree path: blake3(parent_path || key)

Storage keys:
  [0, 0, 0, 0, 0, 0, 0, 0] → value at position 0 (root)
  [0, 0, 0, 0, 0, 0, 0, 1] → value at position 1
  [0, 0, 0, 0, 0, 0, 0, 2] → value at position 2
  ...
```

L'Element stesso (memorizzato nel Merk genitore) trasporta il `count` e l'`height`. L'hash radice fluisce come hash figlio del Merk. Questo significa:
- **Leggere l'hash radice** richiede il ricalcolo dall'archiviazione (O(n) di hashing)
- **Leggere un valore per posizione e O(1)** — singolo lookup nell'archiviazione
- **L'inserimento richiede O(n) di hashing** — una scrittura nell'archiviazione + ricalcolo completo dell'hash radice

## Operazioni

### `dense_tree_insert(path, key, value, tx, grove_version)`

Aggiunge un valore alla prossima posizione disponibile. Restituisce `(root_hash, position)`.

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

Recupera il valore a una data posizione. Restituisce `None` se posizione >= count.

### `dense_tree_root_hash(path, key, tx, grove_version)`

Restituisce l'hash radice memorizzato nell'elemento. Questo e l'hash calcolato durante l'inserimento piu recente — non serve ricalcolo.

### `dense_tree_count(path, key, tx, grove_version)`

Restituisce il numero di valori memorizzati (il campo `count` dall'elemento).

## Operazioni batch

La variante `GroveOp::DenseTreeInsert` supporta l'inserimento batch attraverso la pipeline batch standard di GroveDB:

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

**Pre-elaborazione:** Come tutti i tipi di albero non-Merk, le operazioni `DenseTreeInsert` vengono pre-elaborate prima che il corpo principale del batch venga eseguito. Il metodo `preprocess_dense_tree_ops`:

1. Raggruppa tutte le operazioni `DenseTreeInsert` per `(path, key)`
2. Per ogni gruppo, esegue gli inserimenti sequenzialmente (leggendo l'elemento, inserendo ogni valore, aggiornando l'hash radice)
3. Converte ogni gruppo in un'operazione `ReplaceNonMerkTreeRoot` che trasporta l'`root_hash` finale e il `count` attraverso il meccanismo standard di propagazione

Inserimenti multipli nello stesso albero denso all'interno di un singolo batch sono supportati — vengono elaborati in ordine e il controllo di consistenza permette chiavi duplicate per questo tipo di operazione.

**Propagazione:** L'hash radice e il conteggio fluiscono attraverso la variante `NonMerkTreeMeta::DenseTree` in `ReplaceNonMerkTreeRoot`, seguendo lo stesso pattern di MmrTree e BulkAppendTree.

## Prove

Il DenseAppendOnlyFixedSizeTree supporta **prove di subquery V1** tramite la variante `ProofBytes::DenseTree`. Le singole posizioni possono essere dimostrate rispetto all'hash radice dell'albero usando prove di inclusione che trasportano i valori degli antenati e gli hash dei sotto-alberi fratelli.

### Struttura del percorso di autenticazione

Poiche i nodi interni hashano il **proprio valore** (non solo gli hash dei figli), il percorso di autenticazione differisce da un albero di Merkle standard. Per verificare una foglia alla posizione `p`, il verificatore necessita di:

1. **Il valore della foglia** (la voce dimostrata)
2. **Hash dei valori degli antenati** per ogni nodo interno sul percorso da `p` alla radice (solo l'hash di 32 byte, non il valore completo)
3. **Hash dei sotto-alberi fratelli** per ogni figlio che NON e sul percorso

Poiche tutti i nodi usano `blake3(H(valore) || H(sinistro) || H(destro))` (senza tag di dominio), la prova trasporta solo hash di valori da 32 byte per gli antenati — non i valori completi. Questo mantiene le prove compatte indipendentemente da quanto siano grandi i singoli valori.

```rust
pub struct DenseTreeProof {
    pub entries: Vec<(u16, Vec<u8>)>,            // proved (position, value) pairs
    pub node_value_hashes: Vec<(u16, [u8; 32])>, // ancestor value hashes on the auth path
    pub node_hashes: Vec<(u16, [u8; 32])>,       // precomputed sibling subtree hashes
}
```

> **Nota:** `height` e `count` non sono nella struct della prova — il verificatore li ottiene dall'Element genitore, che e autenticato dalla gerarchia Merk.

### Esempio dettagliato

Albero con height=3, capacita=7, count=5, dimostrazione della posizione 4:

```text
        0
       / \
      1   2
     / \ / \
    3  4 5  6
```

Percorso da 4 alla radice: `4 → 1 → 0`. Insieme espanso: `{0, 1, 4}`.

La prova contiene:
- **entries**: `[(4, valore[4])]` — la posizione dimostrata
- **node_value_hashes**: `[(0, H(valore[0])), (1, H(valore[1]))]` — hash dei valori degli antenati (32 byte ciascuno, non i valori completi)
- **node_hashes**: `[(2, H(sottoalbero_2)), (3, H(nodo_3))]` — fratelli non sul percorso

La verifica ricalcola l'hash radice dal basso verso l'alto:
1. `H(4) = blake3(blake3(valore[4]) || [0;32] || [0;32])` — foglia (figli non riempiti)
2. `H(3)` — da `node_hashes`
3. `H(1) = blake3(H(valore[1]) || H(3) || H(4))` — interno, usa hash del valore da `node_value_hashes`
4. `H(2)` — da `node_hashes`
5. `H(0) = blake3(H(valore[0]) || H(1) || H(2))` — radice, usa hash del valore da `node_value_hashes`
6. Confronta `H(0)` con l'hash radice atteso

### Prove multi-posizione

Quando si dimostrano piu posizioni, l'insieme espanso fonde i percorsi di autenticazione sovrapposti. Gli antenati condivisi sono inclusi solo una volta, rendendo le prove multi-posizione piu compatte di prove indipendenti per singola posizione.

### Limitazione V0

Le prove V0 non possono discendere negli alberi densi. Se una query V0 corrisponde a un `DenseAppendOnlyFixedSizeTree` con una subquery, il sistema restituisce `Error::NotSupported` indirizzando il chiamante a usare `prove_query_v1`.

### Codifica delle chiavi di query

Le posizioni dell'albero denso sono codificate come chiavi di query **u16 big-endian** (2 byte), a differenza di MmrTree e BulkAppendTree che usano u64. Tutti i tipi standard di intervallo `QueryItem` sono supportati.

## Confronto con gli altri alberi non-Merk

| | DenseTree | MmrTree | BulkAppendTree | CommitmentTree |
|---|---|---|---|---|
| **Discriminante Element** | 14 | 12 | 13 | 11 |
| **TreeType** | 10 | 8 | 9 | 7 |
| **Capacita** | Fissa (`2^h - 1`, massimo 65.535) | Illimitata | Illimitata | Illimitata |
| **Modello dati** | Ogni posizione memorizza un valore | Solo foglie | Buffer ad albero denso + chunk | Solo foglie |
| **Hash nell'Element?** | No (fluisce come hash figlio) | No (fluisce come hash figlio) | No (fluisce come hash figlio) | No (fluisce come hash figlio) |
| **Costo inserimento (hashing)** | O(n) blake3 | O(1) ammortizzato | O(1) ammortizzato | ~33 Sinsemilla |
| **Dimensione costo** | 6 byte | 11 byte | 12 byte | 12 byte |
| **Supporto prove** | V1 (Dense) | V1 (MMR) | V1 (Bulk) | V1 (CommitmentTree) |
| **Ideale per** | Piccole strutture a dimensione fissa | Log di eventi | Log ad alto throughput | Impegni ZK |

**Quando scegliere DenseAppendOnlyFixedSizeTree:**
- Il numero massimo di voci e noto al momento della creazione
- Serve che ogni posizione (inclusi i nodi interni) memorizzi dati
- Si desidera il modello dati piu semplice possibile senza crescita illimitata
- Il ricalcolo dell'hash radice O(n) e accettabile (altezze piccole dell'albero)

**Quando NON sceglierlo:**
- Serve capacita illimitata → usare MmrTree o BulkAppendTree
- Serve compatibilita ZK → usare CommitmentTree

## Esempio d'uso

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

## File di implementazione

| File | Contenuto |
|---|---|
| `grovedb-dense-fixed-sized-merkle-tree/src/lib.rs` | Trait `DenseTreeStore`, struct `DenseFixedSizedMerkleTree`, hash ricorsivo |
| `grovedb-dense-fixed-sized-merkle-tree/src/proof.rs` | Struct `DenseTreeProof`, `generate()`, `encode_to_vec()`, `decode_from_slice()` |
| `grovedb-dense-fixed-sized-merkle-tree/src/verify.rs` | `DenseTreeProof::verify()` — funzione pura, nessun accesso all'archiviazione necessario |
| `grovedb-element/src/element/mod.rs` | `Element::DenseAppendOnlyFixedSizeTree` (discriminante 14) |
| `grovedb-element/src/element/constructor.rs` | `empty_dense_tree()`, `new_dense_tree()` |
| `merk/src/tree_type/mod.rs` | `TreeType::DenseAppendOnlyFixedSizeTree = 10` |
| `merk/src/tree_type/costs.rs` | `DENSE_TREE_COST_SIZE = 6` |
| `grovedb/src/operations/dense_tree.rs` | Operazioni GroveDB, `AuxDenseTreeStore`, pre-elaborazione batch |
| `grovedb/src/operations/proof/generate.rs` | `generate_dense_tree_layer_proof()`, `query_items_to_positions()` |
| `grovedb/src/operations/proof/verify.rs` | `verify_dense_tree_lower_layer()` |
| `grovedb/src/operations/proof/mod.rs` | Variante `ProofBytes::DenseTree` |
| `grovedb/src/batch/estimated_costs/average_case_costs.rs` | Modello di costo caso medio |
| `grovedb/src/batch/estimated_costs/worst_case_costs.rs` | Modello di costo caso peggiore |
| `grovedb/src/tests/dense_tree_tests.rs` | 22 test di integrazione |

---
