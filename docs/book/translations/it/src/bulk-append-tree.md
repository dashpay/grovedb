# Il BulkAppendTree — Archiviazione append-only ad alto throughput

Il BulkAppendTree e la risposta di GroveDB a una sfida ingegneristica specifica: come costruire un log append-only (solo aggiunta) ad alto throughput che supporti prove di intervallo efficienti, minimizzi l'hashing per scrittura e produca snapshot di chunk immutabili adatti alla distribuzione via CDN?

Mentre un MmrTree (Capitolo 13) e ideale per le prove su singole foglie, il BulkAppendTree e progettato per carichi di lavoro dove migliaia di valori arrivano per blocco e i client devono sincronizzarsi recuperando intervalli di dati. Raggiunge questo obiettivo con un'**architettura a due livelli**: un buffer ad albero denso di Merkle che assorbe le aggiunte in arrivo, e un MMR a livello di chunk che registra le radici dei chunk finalizzati.

## L'architettura a due livelli

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

**Livello 1 — Il buffer.** I valori in arrivo vengono scritti in un `DenseFixedSizedMerkleTree` (vedi Capitolo 16). La capacita del buffer e `2^altezza - 1` posizioni. L'hash radice dell'albero denso (`dense_tree_root`) si aggiorna dopo ogni inserimento.

**Livello 2 — Il Chunk MMR.** Quando il buffer si riempie (raggiunge `chunk_size` voci), tutte le voci vengono serializzate in un **blob di chunk** immutabile, una radice densa di Merkle viene calcolata su quelle voci, e quella radice viene aggiunta come foglia al Chunk MMR. Il buffer viene quindi svuotato.

La **radice di stato** (state root) combina entrambi i livelli in un singolo impegno (commitment) di 32 byte che cambia ad ogni aggiunta, assicurando che l'albero Merk genitore rifletta sempre lo stato piu recente.

## Come i valori riempiono il buffer

Ogni chiamata ad `append()` segue questa sequenza:

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

Il **buffer E un DenseFixedSizedMerkleTree** (vedi Capitolo 16). Il suo hash radice cambia dopo ogni inserimento, fornendo un impegno su tutte le voci attualmente nel buffer. Questo hash radice e quello che entra nel calcolo della radice di stato.

## Compattazione dei chunk

Quando il buffer si riempie (raggiunge `chunk_size` voci), la compattazione si attiva automaticamente:

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

Dopo la compattazione, il blob di chunk e **permanentemente immutabile** — non cambia mai piu. Questo rende i blob di chunk ideali per la cache CDN, la sincronizzazione dei client e l'archiviazione a lungo termine.

**Esempio: 4 aggiunte con chunk_power=2 (chunk_size=4)**

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

## La radice di stato

La radice di stato lega entrambi i livelli in un singolo hash:

```rust
fn compute_state_root(
    mmr_root: &[u8; 32],         // Chunk MMR root (or [0;32] if empty)
    dense_tree_root: &[u8; 32],  // Root hash of current buffer (dense tree)
) -> [u8; 32] {
    blake3("bulk_state" || mmr_root || dense_tree_root)
}
```

Il `total_count` e il `chunk_power` **non** sono inclusi nella radice di stato perche sono gia autenticati dall'hash del valore Merk — sono campi dell'`Element` serializzato memorizzato nel nodo Merk genitore. La radice di stato cattura solo gli impegni a livello di dati (`mmr_root` e `dense_tree_root`). Questo e l'hash che fluisce come hash figlio del Merk e si propaga fino all'hash radice di GroveDB.

## La radice densa di Merkle

Quando un chunk viene compattato, le voci necessitano di un singolo impegno di 32 byte. Il BulkAppendTree utilizza un **albero binario denso (completo) di Merkle**:

```text
Given entries [e_0, e_1, e_2, e_3]:

Level 0 (leaves):  blake3(e_0)  blake3(e_1)  blake3(e_2)  blake3(e_3)
                      \__________/              \__________/
Level 1:           blake3(h_0 || h_1)       blake3(h_2 || h_3)
                          \____________________/
Level 2 (root):    blake3(h_01 || h_23)  ← this is the dense Merkle root
```

Poiche `chunk_size` e sempre una potenza di 2 (per costruzione: `1u32 << chunk_power`), l'albero e sempre completo (nessun padding o foglie fittizie necessarie). Il conteggio degli hash e esattamente `2 * chunk_size - 1`:
- `chunk_size` hash delle foglie (uno per voce)
- `chunk_size - 1` hash dei nodi interni

L'implementazione della radice densa di Merkle risiede in `grovedb-mmr/src/dense_merkle.rs` e fornisce due funzioni:
- `compute_dense_merkle_root(hashes)` — da foglie gia hashate
- `compute_dense_merkle_root_from_values(values)` — hasha prima i valori, poi costruisce l'albero

## Serializzazione dei blob di chunk

I blob di chunk sono gli archivi immutabili prodotti dalla compattazione. Il serializzatore seleziona automaticamente il formato wire piu compatto in base alle dimensioni delle voci:

**Formato a dimensione fissa** (flag `0x01`) — quando tutte le voci hanno la stessa lunghezza:

```text
┌──────┬──────────┬─────────────┬─────────┬─────────┬─────────┐
│ 0x01 │ count    │ entry_size  │ entry_0 │ entry_1 │ ...     │
│ 1B   │ 4B (BE)  │ 4B (BE)     │ N bytes │ N bytes │         │
└──────┴──────────┴─────────────┴─────────┴─────────┴─────────┘
Total: 1 + 4 + 4 + (count × entry_size) bytes
```

**Formato a dimensione variabile** (flag `0x00`) — quando le voci hanno lunghezze diverse:

```text
┌──────┬──────────┬─────────┬──────────┬─────────┬─────────────┐
│ 0x00 │ len_0    │ entry_0 │ len_1    │ entry_1 │ ...         │
│ 1B   │ 4B (BE)  │ N bytes │ 4B (BE)  │ M bytes │             │
└──────┴──────────┴─────────┴──────────┴─────────┴─────────────┘
Total: 1 + Σ(4 + len_i) bytes
```

Il formato a dimensione fissa risparmia 4 byte per voce rispetto a quello variabile, il che si accumula significativamente per grandi chunk di dati di dimensione uniforme (come impegni hash da 32 byte). Per 1024 voci da 32 byte ciascuna:
- Fisso: `1 + 4 + 4 + 32768 = 32.777 byte`
- Variabile: `1 + 1024 × (4 + 32) = 36.865 byte`
- Risparmio: ~11%

## Layout delle chiavi di archiviazione

Tutti i dati del BulkAppendTree risiedono nello spazio dei nomi **dati**, con chiavi aventi prefissi a singolo carattere:

| Pattern chiave | Formato | Dimensione | Scopo |
|---|---|---|---|
| `M` | 1 byte | 1B | Chiave metadati |
| `b` + `{indice}` | `b` + u32 BE | 5B | Voce del buffer all'indice |
| `e` + `{indice}` | `e` + u64 BE | 9B | Blob di chunk all'indice |
| `m` + `{pos}` | `m` + u64 BE | 9B | Nodo MMR alla posizione |

I **metadati** memorizzano `mmr_size` (8 byte BE). Il `total_count` e il `chunk_power` sono memorizzati nell'Element stesso (nel Merk genitore), non nei metadati dello spazio dei nomi dati. Questa separazione significa che leggere il conteggio e un semplice lookup dell'elemento senza dover aprire il contesto di archiviazione dati.

Le chiavi del buffer usano indici u32 (da 0 a `chunk_size - 1`) perche la capacita del buffer e limitata da `chunk_size` (un u32, calcolato come `1u32 << chunk_power`). Le chiavi dei chunk usano indici u64 perche il numero di chunk completati puo crescere indefinitamente.

## La struct BulkAppendTree

```rust
pub struct BulkAppendTree<S> {
    pub total_count: u64,                        // Total values ever appended
    pub dense_tree: DenseFixedSizedMerkleTree<S>, // The buffer (dense tree)
}
```

Il buffer E un `DenseFixedSizedMerkleTree` — il suo hash radice e `dense_tree_root`.

**Accessori:**
- `capacity() -> u16`: `dense_tree.capacity()` (= `2^altezza - 1`)
- `epoch_size() -> u64`: `capacity + 1` (= `2^altezza`, il numero di voci per chunk)
- `height() -> u8`: `dense_tree.height()`

**Valori derivati** (non memorizzati):

| Valore | Formula |
|---|---|
| `chunk_count` | `total_count / epoch_size` |
| `buffer_count` | `dense_tree.count()` |
| `mmr_size` | `leaf_count_to_mmr_size(chunk_count)` |

## Operazioni GroveDB

Il BulkAppendTree si integra con GroveDB attraverso sei operazioni definite in `grovedb/src/operations/bulk_append_tree.rs`:

### bulk_append

L'operazione mutante primaria. Segue il pattern standard di archiviazione non-Merk di GroveDB:

```text
1. Validate element is BulkAppendTree
2. Open data storage context
3. Load tree from store
4. Append value (may trigger compaction)
5. Update element in parent Merk with new state_root + total_count
6. Propagate changes up through Merk hierarchy
7. Commit transaction
```

L'adattatore `AuxBulkStore` avvolge le chiamate `get_aux`/`put_aux`/`delete_aux` di GroveDB e accumula `OperationCost` in un `RefCell` per il tracciamento dei costi. I costi di hash dall'operazione di aggiunta vengono sommati a `cost.hash_node_calls`.

### Operazioni di lettura

| Operazione | Cosa restituisce | Archiviazione aux? |
|---|---|---|
| `bulk_get_value(path, key, position)` | Valore alla posizione globale | Si — legge dal blob di chunk o dal buffer |
| `bulk_get_chunk(path, key, chunk_index)` | Blob di chunk grezzo | Si — legge la chiave del chunk |
| `bulk_get_buffer(path, key)` | Tutte le voci correnti del buffer | Si — legge le chiavi del buffer |
| `bulk_count(path, key)` | Conteggio totale (u64) | No — legge dall'elemento |
| `bulk_chunk_count(path, key)` | Chunk completati (u64) | No — calcolato dall'elemento |

L'operazione `get_value` instrada in modo trasparente per posizione:

```text
if position < completed_chunks × chunk_size:
    chunk_idx = position / chunk_size
    intra_idx = position % chunk_size
    → read chunk blob, deserialize, return entry[intra_idx]
else:
    buffer_idx = position % chunk_size
    → read buffer_key(buffer_idx)
```

## Operazioni batch e pre-elaborazione

Il BulkAppendTree supporta le operazioni batch (in blocco) attraverso la variante `GroveOp::BulkAppend`. Poiche `execute_ops_on_path` non ha accesso al contesto di archiviazione dati, tutte le operazioni BulkAppend devono essere pre-elaborate prima di `apply_body`.

La pipeline di pre-elaborazione:

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

La variante `append_with_mem_buffer` evita problemi di lettura dopo scrittura: le voci del buffer vengono tracciate in un `Vec<Vec<u8>>` in memoria, cosi la compattazione puo leggerle anche se l'archiviazione transazionale non ha ancora effettuato il commit.

## Il trait BulkStore

```rust
pub trait BulkStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String>;
    fn delete(&self, key: &[u8]) -> Result<(), String>;
}
```

I metodi prendono `&self` (non `&mut self`) per corrispondere al pattern di mutabilita interna di GroveDB dove le scritture passano attraverso un batch. L'integrazione GroveDB implementa questo tramite `AuxBulkStore` che avvolge un `StorageContext` e accumula `OperationCost`.

Il `MmrAdapter` fa da ponte tra `BulkStore` e i trait `MMRStoreReadOps`/`MMRStoreWriteOps` dell'MMR ckb, aggiungendo una cache write-through per la correttezza di lettura dopo scrittura.

## Generazione delle prove

Le prove del BulkAppendTree supportano **query di intervallo** (range queries) sulle posizioni. La struttura della prova cattura tutto il necessario affinche un verificatore senza stato (stateless verifier) possa confermare che dati specifici esistono nell'albero:

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

**Passi di generazione** per un intervallo `[start, end)` (con `chunk_size = 1u32 << chunk_power`):

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

**Perche includere TUTTE le voci del buffer?** Il buffer e un albero denso di Merkle il cui hash radice si impegna su ogni voce. Il verificatore deve ricostruire l'albero da tutte le voci per verificare il `dense_tree_root`. Poiche il buffer e limitato dalla `capacita` (al massimo 65.535 voci), questo e un costo ragionevole.

## Verifica delle prove

La verifica e una funzione pura — non serve accesso al database. Esegue cinque controlli:

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

Dopo che la verifica ha successo, il `BulkAppendTreeProofResult` fornisce un metodo `values_in_range(start, end)` che estrae valori specifici dai blob di chunk verificati e dalle voci del buffer.

## Come si collega all'hash radice di GroveDB

Il BulkAppendTree e un **albero non-Merk** — memorizza i dati nello spazio dei nomi dati, non in un sotto-albero Merk figlio. Nel Merk genitore, l'elemento e memorizzato come:

```text
Element::BulkAppendTree(total_count, chunk_power, flags)
```

La radice di stato fluisce come hash figlio del Merk. L'hash del nodo Merk genitore e:

```text
combine_hash(value_hash(element_bytes), state_root)
```

Lo `state_root` fluisce come hash figlio del Merk (tramite il parametro `subtree_root_hash` di `insert_subtree`). Qualsiasi cambiamento alla radice di stato si propaga verso l'alto attraverso la gerarchia Merk di GroveDB fino all'hash radice.

Nelle prove V1 (§9.6), la prova del Merk genitore dimostra i byte dell'elemento e il legame con l'hash figlio, e la `BulkAppendTreeProof` dimostra che i dati interrogati sono coerenti con lo `state_root` usato come hash figlio.

## Tracciamento dei costi

Il costo di hash di ogni operazione e tracciato esplicitamente:

| Operazione | Chiamate Blake3 | Note |
|---|---|---|
| Singola aggiunta (senza compattazione) | 3 | 2 per la catena hash del buffer + 1 per la radice di stato |
| Singola aggiunta (con compattazione) | 3 + 2C - 1 + ~2 | Catena + Merkle denso (C=chunk_size) + push MMR + radice di stato |
| `get_value` da chunk | 0 | Pura deserializzazione, nessun hashing |
| `get_value` dal buffer | 0 | Lookup diretto della chiave |
| Generazione prova | Dipende dal conteggio chunk | Radice Merkle densa per chunk + prova MMR |
| Verifica prova | 2C·K - K + B·2 + 1 | K chunk, B voci buffer, C chunk_size |

**Costo ammortizzato per aggiunta**: Per chunk_size=1024 (chunk_power=10), l'overhead di compattazione di ~2047 hash (radice Merkle densa) e ammortizzato su 1024 aggiunte, aggiungendo ~2 hash per aggiunta. Combinato con i 3 hash per aggiunta, il totale ammortizzato e **~5 chiamate blake3 per aggiunta** — molto efficiente per una struttura autenticata crittograficamente.

## Confronto con MmrTree

| | BulkAppendTree | MmrTree |
|---|---|---|
| **Architettura** | Due livelli (buffer + Chunk MMR) | Singolo MMR |
| **Costo hash per aggiunta** | 3 (+ ~2 ammortizzati per compattazione) | ~2 |
| **Granularita delle prove** | Query di intervallo sulle posizioni | Prove su singole foglie |
| **Snapshot immutabili** | Si (blob di chunk) | No |
| **Compatibile CDN** | Si (blob di chunk memorizzabili in cache) | No |
| **Voci buffer** | Si (necessarie tutte per la prova) | N/A |
| **Ideale per** | Log ad alto throughput, sincronizzazione in blocco | Log di eventi, ricerche individuali |
| **Discriminante Element** | 13 | 12 |
| **TreeType** | 9 | 8 |

Scegli MmrTree quando hai bisogno di prove su singole foglie con overhead minimo. Scegli BulkAppendTree quando hai bisogno di query di intervallo, sincronizzazione in blocco e snapshot basati su chunk.

## File di implementazione

| File | Scopo |
|------|---------|
| `grovedb-bulk-append-tree/src/lib.rs` | Radice del crate, ri-esportazioni |
| `grovedb-bulk-append-tree/src/tree/mod.rs` | Struct `BulkAppendTree`, accessori di stato, persistenza metadati |
| `grovedb-bulk-append-tree/src/tree/append.rs` | `append()`, `append_with_mem_buffer()`, `compact_entries()` |
| `grovedb-bulk-append-tree/src/tree/hash.rs` | `compute_state_root` |
| `grovedb-bulk-append-tree/src/tree/keys.rs` | `META_KEY`, `buffer_key`, `chunk_key`, `mmr_node_key` |
| `grovedb-bulk-append-tree/src/tree/query.rs` | `get_value`, `get_chunk`, `get_buffer` |
| `grovedb-bulk-append-tree/src/tree/mmr_adapter.rs` | `MmrAdapter` con cache write-through |
| `grovedb-bulk-append-tree/src/chunk.rs` | Serializzazione blob di chunk (formati fisso + variabile) |
| `grovedb-bulk-append-tree/src/proof.rs` | Generazione e verifica `BulkAppendTreeProof` |
| `grovedb-bulk-append-tree/src/store.rs` | Trait `BulkStore` |
| `grovedb-bulk-append-tree/src/error.rs` | Enum `BulkAppendError` |
| `grovedb/src/operations/bulk_append_tree.rs` | Operazioni GroveDB, `AuxBulkStore`, pre-elaborazione batch |
| `grovedb/src/tests/bulk_append_tree_tests.rs` | 27 test di integrazione |

---
