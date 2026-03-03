# BulkAppendTree -- Vysoko-propustne append-only uloziste

BulkAppendTree je odpovedi GroveDB na specificky inzenyrsky problem: jak postavit
vysoko-propustny append-only log, ktery podporuje efektivni rozsahove dukazy, minimalizuje
hashovani na zapis a produkuje nemenne chunkove snimky vhodne pro distribuci pres CDN?

Zatimco MmrTree (Kapitola 13) je idealni pro jednotlive dukazy listu, BulkAppendTree
je navrzen pro zateze, kde tisice hodnot prichazi v kazdem bloku a klienti potrebuji
synchronizovat stahovanim rozsahu dat. Dosahuje toho **dvouurovnovou architekturou**:
hustym Merklovym stromovym bufferem, ktery absorbuje prichozi pripojeni, a chunkovou
MMR, ktera zaznamenava finalizovane korenove hashe chunku.

## Dvouurovnova architektura

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

**Uroven 1 -- Buffer.** Prichozi hodnoty se zapisuji do `DenseFixedSizedMerkleTree`
(viz Kapitola 16). Kapacita bufferu je `2^height - 1` pozic. Korenovy hash husteho
stromu (`dense_tree_root`) se aktualizuje po kazdem vlozeni.

**Uroven 2 -- Chunk MMR.** Kdyz se buffer naplni (dosahne `chunk_size` zaznamu),
vsechny zaznamy se serializuji do nemenneho **chunk blobu**, nad temito zaznamy se
vypocte husty Merkluv koren a ten se pripoji jako list do chunkove MMR.
Buffer se pote vymaze.

**Korenovy stav** (state root) kombinuje obe urovne do jednoho 32-bajtoveho
zavazku, ktery se meni pri kazdem pripojeni, cimz zajistuje, ze rodicovsky
Merk strom vzdy odrázi nejnovejsi stav.

## Jak hodnoty plni buffer

Kazde volani `append()` nasleduje tuto sekvenci:

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

**Buffer JE DenseFixedSizedMerkleTree** (viz Kapitola 16). Jeho korenovy hash
se meni po kazdem vlozeni a poskytuje zavazek ke vsem aktualnim zaznamum v bufferu.
Tento korenovy hash je to, co vstupuje do vypoctu state root.

## Kompaktace chunku

Kdyz se buffer naplni (dosahne `chunk_size` zaznamu), kompaktace se spusti automaticky:

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

Po kompaktaci je chunk blob **trvale nemenný** -- uz se nikdy nezmeni.
To cini chunk bloby idealni pro CDN cachovani, klientskou synchronizaci
a archivni uloziste.

**Priklad: 4 pripojeni s chunk_power=2 (chunk_size=4)**

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

## State Root

State root svazuje obe urovne do jednoho hashe:

```rust
fn compute_state_root(
    mmr_root: &[u8; 32],         // Chunk MMR root (or [0;32] if empty)
    dense_tree_root: &[u8; 32],  // Root hash of current buffer (dense tree)
) -> [u8; 32] {
    blake3("bulk_state" || mmr_root || dense_tree_root)
}
```

`total_count` a `chunk_power` **nejsou** zahrnuty ve state root, protoze
jsou jiz autentizovany hashem hodnoty Merk -- jsou poli serializovaneho
`Element` ulozeneho v rodicovskem uzlu Merk. State root zachycuje pouze
datove zavazky (`mmr_root` a `dense_tree_root`). Toto je hash, ktery
proudí jako Merk child hash a propaguje se nahoru ke korenovemu hashi GroveDB.

## Husty Merkluv koren

Kdyz se chunk kompaktuje, zaznamy potrebuji jediny 32-bajtovy zavazek.
BulkAppendTree pouziva **husty (uplny) binarni Merkluv strom**:

```text
Given entries [e_0, e_1, e_2, e_3]:

Level 0 (leaves):  blake3(e_0)  blake3(e_1)  blake3(e_2)  blake3(e_3)
                      \__________/              \__________/
Level 1:           blake3(h_0 || h_1)       blake3(h_2 || h_3)
                          \____________________/
Level 2 (root):    blake3(h_01 || h_23)  ← this is the dense Merkle root
```

Protoze `chunk_size` je vzdy mocnina 2 (dle konstrukce: `1u32 << chunk_power`),
strom je vzdy uplny (neni potreba paddingu ani dummy listu). Pocet hashu je
presne `2 * chunk_size - 1`:
- `chunk_size` hashu listu (jeden na zaznam)
- `chunk_size - 1` hashu vnitrnich uzlu

Implementace husteho Merklova korene se nachazi v `grovedb-mmr/src/dense_merkle.rs`
a poskytuje dve funkce:
- `compute_dense_merkle_root(hashes)` -- z predhashovanych listu
- `compute_dense_merkle_root_from_values(values)` -- nejprve zahashuje hodnoty,
  potom sestavi strom

## Serializace chunk blobu

Chunk bloby jsou nemenne archivy produkovane kompaktaci. Serializer
automaticky vybira nejkompaktnejsi dratovy format na zaklade velikosti zaznamu:

**Format s pevnou velikosti** (priznak `0x01`) -- kdyz maji vsechny zaznamy stejnou delku:

```text
┌──────┬──────────┬─────────────┬─────────┬─────────┬─────────┐
│ 0x01 │ count    │ entry_size  │ entry_0 │ entry_1 │ ...     │
│ 1B   │ 4B (BE)  │ 4B (BE)     │ N bytes │ N bytes │         │
└──────┴──────────┴─────────────┴─────────┴─────────┴─────────┘
Total: 1 + 4 + 4 + (count × entry_size) bytes
```

**Format s promennou velikosti** (priznak `0x00`) -- kdyz maji zaznamy ruzne delky:

```text
┌──────┬──────────┬─────────┬──────────┬─────────┬─────────────┐
│ 0x00 │ len_0    │ entry_0 │ len_1    │ entry_1 │ ...         │
│ 1B   │ 4B (BE)  │ N bytes │ 4B (BE)  │ M bytes │             │
└──────┴──────────┴─────────┴──────────┴─────────┴─────────────┘
Total: 1 + Σ(4 + len_i) bytes
```

Format s pevnou velikosti usetri 4 bajty na zaznam oproti promennemu formatu, coz
se vyznamne nasci pro velke chunky dat jednotne velikosti (jako 32-bajtove
hashovane zavazky). Pro 1024 zaznamu po 32 bajtech:
- Pevny: `1 + 4 + 4 + 32768 = 32,777 bajtu`
- Promenlivy: `1 + 1024 × (4 + 32) = 36,865 bajtu`
- Uspora: ~11%

## Rozlozeni klicu v ulozisti

Vsechna data BulkAppendTree jsou ulozena v **datovem** prostoru jmen (namespace),
klicem s jednozmenovymi prefixy:

| Vzor klice | Format | Velikost | Ucel |
|---|---|---|---|
| `M` | 1 bajt | 1B | Klic metadat |
| `b` + `{index}` | `b` + u32 BE | 5B | Zaznam bufferu na indexu |
| `e` + `{index}` | `e` + u64 BE | 9B | Chunk blob na indexu |
| `m` + `{pos}` | `m` + u64 BE | 9B | MMR uzel na pozici |

**Metadata** ukladaji `mmr_size` (8 bajtu BE). `total_count` a `chunk_power` jsou
ulozeny v samotnem Elementu (v rodicovskem Merk), nikoli v metadatech datoveho
prostoru jmen. Toto rozdeleni znamena, ze cteni poctu je jednoduche vyhledani
elementu bez otevirani kontextu datoveho uloziste.

Klice bufferu pouzivaji u32 indexy (0 az `chunk_size - 1`), protoze kapacita
bufferu je omezena `chunk_size` (u32, vypocteny jako `1u32 << chunk_power`).
Klice chunku pouzivaji u64 indexy, protoze pocet dokoncecnych chunku muze rust
neomezenene.

## Struktura BulkAppendTree

```rust
pub struct BulkAppendTree<S> {
    pub total_count: u64,                        // Total values ever appended
    pub dense_tree: DenseFixedSizedMerkleTree<S>, // The buffer (dense tree)
}
```

Buffer JE `DenseFixedSizedMerkleTree` -- jeho korenovy hash je `dense_tree_root`.

**Pristupove metody:**
- `capacity() -> u16`: `dense_tree.capacity()` (= `2^height - 1`)
- `epoch_size() -> u64`: `capacity + 1` (= `2^height`, pocet zaznamu na chunk)
- `height() -> u8`: `dense_tree.height()`

**Odvozene hodnoty** (neukladaji se):

| Hodnota | Vzorec |
|---|---|
| `chunk_count` | `total_count / epoch_size` |
| `buffer_count` | `dense_tree.count()` |
| `mmr_size` | `leaf_count_to_mmr_size(chunk_count)` |

## Operace GroveDB

BulkAppendTree se integruje s GroveDB prostrednictvim sesti operaci definovanych v
`grovedb/src/operations/bulk_append_tree.rs`:

### bulk_append

Primární mutujici operace. Nasleduje standardni vzor GroveDB pro ne-Merk uloziste:

```text
1. Validate element is BulkAppendTree
2. Open data storage context
3. Load tree from store
4. Append value (may trigger compaction)
5. Update element in parent Merk with new state_root + total_count
6. Propagate changes up through Merk hierarchy
7. Commit transaction
```

Adapter `AuxBulkStore` obaluje volani `get_aux`/`put_aux`/`delete_aux` GroveDB a
akumuluje `OperationCost` v `RefCell` pro sledovani nakladu. Naklady hashu z operace
pripojeni se pridavaji do `cost.hash_node_calls`.

### Operace cteni

| Operace | Co vraci | Aux uloziste? |
|---|---|---|
| `bulk_get_value(path, key, position)` | Hodnota na globalni pozici | Ano -- cte z chunk blobu nebo bufferu |
| `bulk_get_chunk(path, key, chunk_index)` | Surovy chunk blob | Ano -- cte klic chunku |
| `bulk_get_buffer(path, key)` | Vsechny aktualni zaznamy bufferu | Ano -- cte klice bufferu |
| `bulk_count(path, key)` | Celkovy pocet (u64) | Ne -- cte z elementu |
| `bulk_chunk_count(path, key)` | Dokoncene chunky (u64) | Ne -- vypocteno z elementu |

Operace `get_value` transparentne smeruje podle pozice:

```text
if position < completed_chunks × chunk_size:
    chunk_idx = position / chunk_size
    intra_idx = position % chunk_size
    → read chunk blob, deserialize, return entry[intra_idx]
else:
    buffer_idx = position % chunk_size
    → read buffer_key(buffer_idx)
```

## Davkove operace a predzpracovani

BulkAppendTree podporuje davkove operace prostrednictvim varianty `GroveOp::BulkAppend`.
Protoze `execute_ops_on_path` nema pristup ke kontextu datoveho uloziste, vsechny
BulkAppend operace musi byt predzpracovany pred `apply_body`.

Potrubi predzpracovani:

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

Varianta `append_with_mem_buffer` zamezuje problemum s ctenim po zapisu: zaznamy
bufferu jsou sledovany ve `Vec<Vec<u8>>` v pameti, takze kompaktace muze cist
zaznamy i kdyz transakční uloziste jeste nebylo commitnuto.

## Trait BulkStore

```rust
pub trait BulkStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String>;
    fn delete(&self, key: &[u8]) -> Result<(), String>;
}
```

Metody prijimaji `&self` (ne `&mut self`), aby odpovidaly vzoru GroveDB s interni
mutabilitou, kde zapisy prochazi pres davku. Integrace GroveDB to implementuje
prostrednictvim `AuxBulkStore`, ktery obaluje `StorageContext` a akumuluje
`OperationCost`.

`MmrAdapter` premostuje `BulkStore` na traity `MMRStoreReadOps`/
`MMRStoreWriteOps` z ckb MMR a pridava write-through cache pro korektnost
cteni po zapisu.

## Generovani dukazu

Dukazy BulkAppendTree podporuji **rozsahove dotazy** nad pozicemi. Struktura
dukazu zachycuje vse, co bezstavovy overovatel potrebuje k potvrzeni, ze
specificka data existuji ve strome:

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

**Kroky generovani** pro rozsah `[start, end)` (s `chunk_size = 1u32 << chunk_power`):

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

**Proc zahrnout VSECHNY zaznamy bufferu?** Buffer je husty Merkluv strom, jehoz
korenovy hash se zaväzuje ke kazdemu zaznamu. Overovatel musi znovu sestavit
strom ze vsech zaznamu, aby overil `dense_tree_root`. Protoze buffer je ohranicon
`capacity` (maximalne 65 535 zaznamu), jsou naklady primerene.

## Overovani dukazu

Overovani je cista funkce -- neni potreba pristup k databazi. Provadi pet kontrol:

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

Po uspesnem overeni poskytuje `BulkAppendTreeProofResult` metodu
`values_in_range(start, end)`, ktera extrahuje specificke hodnoty z overenych
chunk blobu a zaznamu bufferu.

## Jak se napojuje na korenovy hash GroveDB

BulkAppendTree je **ne-Merk strom** -- uklada data v datovem prostoru jmen,
nikoli v detskem Merk podstromu. V rodicovskem Merk je element ulozen jako:

```text
Element::BulkAppendTree(total_count, chunk_power, flags)
```

State root proudi jako Merk child hash. Hash rodicovskeho uzlu Merk je:

```text
combine_hash(value_hash(element_bytes), state_root)
```

`state_root` proudi jako Merk child hash (pres parametr `subtree_root_hash`
metody `insert_subtree`). Jakakoliv zmena state root se propaguje nahoru
pres hierarchii Merk GroveDB az ke korenovemu hashi.

V dukazech V1 (§9.6) Merk dukaz rodice dokazuje bajty elementu a vazbu
child hashe a `BulkAppendTreeProof` dokazuje, ze dotazovana data jsou
konzistentni se `state_root` pouzitym jako child hash.

## Sledovani nakladu

Naklady hashu kazde operace jsou sledovany explicitne:

| Operace | Volani Blake3 | Poznamky |
|---|---|---|
| Jedno pripojeni (bez kompaktace) | 3 | 2 pro hashovaci retezec bufferu + 1 pro state root |
| Jedno pripojeni (s kompaktaci) | 3 + 2C - 1 + ~2 | Retezec + husty Merkle (C=chunk_size) + MMR push + state root |
| `get_value` z chunku | 0 | Cista deserializace, zadne hashovani |
| `get_value` z bufferu | 0 | Prime vyhledani klice |
| Generovani dukazu | Zavisi na poctu chunku | Husty Merkluv koren na chunk + MMR dukaz |
| Overovani dukazu | 2C·K - K + B·2 + 1 | K chunku, B zaznamu bufferu, C chunk_size |

**Amortizovane naklady na pripojeni**: Pro chunk_size=1024 (chunk_power=10) je rezie
kompaktace ~2047 hashu (husty Merkluv koren) amortizovana na 1024 pripojeni, coz pridava
~2 hashe na pripojeni. V kombinaci s 3 hashy na pripojeni je amortizovany celkovy pocet
**~5 volani blake3 na pripojeni** -- velmi efektivni pro kryptograficky autentizovanou
strukturu.

## Srovnani s MmrTree

| | BulkAppendTree | MmrTree |
|---|---|---|
| **Architektura** | Dvouurovnova (buffer + chunk MMR) | Jednoducha MMR |
| **Naklady hashu na pripojeni** | 3 (+ amortizovane ~2 pro kompaktaci) | ~2 |
| **Granularita dukazu** | Rozsahove dotazy nad pozicemi | Jednotlive dukazy listu |
| **Nemenne snimky** | Ano (chunk bloby) | Ne |
| **Vhodne pro CDN** | Ano (chunk bloby cachovatelne) | Ne |
| **Zaznamy bufferu** | Ano (vsechny potrebne pro dukaz) | N/A |
| **Nejlepsi pro** | Vysoko-propustne logy, hromadna synchronizace | Logy udalosti, jednotlive vyhledavani |
| **Diskriminant elementu** | 13 | 12 |
| **TreeType** | 9 | 8 |

Zvolte MmrTree, kdyz potrebujete jednotlive dukazy listu s minimalnimi naklady. Zvolte
BulkAppendTree, kdyz potrebujete rozsahove dotazy, hromadnou synchronizaci a chunkove
snimky.

## Implementacni soubory

| Soubor | Ucel |
|------|---------|
| `grovedb-bulk-append-tree/src/lib.rs` | Koren cratu, re-exporty |
| `grovedb-bulk-append-tree/src/tree/mod.rs` | Struktura `BulkAppendTree`, pristupove metody stavu, persistace metadat |
| `grovedb-bulk-append-tree/src/tree/append.rs` | `append()`, `append_with_mem_buffer()`, `compact_entries()` |
| `grovedb-bulk-append-tree/src/tree/hash.rs` | `compute_state_root` |
| `grovedb-bulk-append-tree/src/tree/keys.rs` | `META_KEY`, `buffer_key`, `chunk_key`, `mmr_node_key` |
| `grovedb-bulk-append-tree/src/tree/query.rs` | `get_value`, `get_chunk`, `get_buffer` |
| `grovedb-bulk-append-tree/src/tree/mmr_adapter.rs` | `MmrAdapter` s write-through cache |
| `grovedb-bulk-append-tree/src/chunk.rs` | Serializace chunk blobu (pevny + promenlivy format) |
| `grovedb-bulk-append-tree/src/proof.rs` | `BulkAppendTreeProof` generovani a overovani |
| `grovedb-bulk-append-tree/src/store.rs` | Trait `BulkStore` |
| `grovedb-bulk-append-tree/src/error.rs` | Vycet `BulkAppendError` |
| `grovedb/src/operations/bulk_append_tree.rs` | Operace GroveDB, `AuxBulkStore`, davkove predzpracovani |
| `grovedb/src/tests/bulk_append_tree_tests.rs` | 27 integracnich testu |

---
