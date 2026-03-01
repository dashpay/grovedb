# BulkAppendTree — Wysoko-przepustowe magazynowanie append-only

BulkAppendTree to odpowiedz GroveDB na specyficzne wyzwanie inzynieryjne: jak zbudowac
wysoko-przepustowy dziennik append-only (tylko dopisywanie), ktory wspiera wydajne dowody
zakresowe, minimalizuje haszowanie per-zapis i produkuje niezmienne migawki chunkow
odpowiednie do dystrybucji przez CDN?

Podczas gdy MmrTree (Rozdzial 13) jest idealny dla indywidualnych dowodow lisci,
BulkAppendTree jest zaprojektowany dla obciazen, gdzie tysiace wartosci przybywaja
na blok i klienci musza synchronizowac sie pobierajac zakresy danych. Osiaga to dzieki
**dwupoziomowej architekturze**: gestyemu buforowi drzewa Merkle absorbujacemu
przychodzace dopisywania oraz MMR na poziomie chunkow rejestrujacemu sfinalizowane
korzenie chunkow.

## Dwupoziomowa architektura

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

**Poziom 1 — Bufor.** Przychodzace wartosci sa zapisywane do `DenseFixedSizedMerkleTree`
(patrz Rozdzial 16). Pojemnosc bufora wynosi `2^height - 1` pozycji. Hasz korzenia
gestego drzewa (`dense_tree_root`) aktualizuje sie po kazdym wstawieniu.

**Poziom 2 — Chunk MMR.** Gdy bufor sie wypelni (osiagnie `chunk_size` wpisow),
wszystkie wpisy sa serializowane do niezmiennego **blobu chunka**, korzen gestego
drzewa Merkle jest obliczany na podstawie tych wpisow i ten korzen jest dopisywany
jako lisc do chunk MMR. Bufor jest nastepnie czyszczony.

**Korzen stanu** laczy oba poziomy w pojedyncze 32-bajtowe zobowiazanie, ktore zmienia
sie przy kazdym dopisaniu, zapewniajac ze nadrzedne drzewo Merk zawsze odzwierciedla
najnowszy stan.

## Jak wartosci wypelniaja bufor

Kazde wywolanie `append()` podaza nastepujaca sekwencja:

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

**Bufor JEST DenseFixedSizedMerkleTree** (patrz Rozdzial 16). Jego hasz korzenia
zmienia sie po kazdym wstawieniu, dostarczajac zobowiazanie do wszystkich biezacych
wpisow bufora. Ten hasz korzenia jest tym, co wplywa do obliczania korzenia stanu.

## Kompakcja chunkow

Gdy bufor sie wypelni (osiagnie `chunk_size` wpisow), kompakcja uruchamia sie automatycznie:

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

Po kompakcji blob chunka jest **permanentnie niezmienny** — nigdy sie juz nie zmienia.
To sprawia, ze bloby chunkow sa idealne do buforowania CDN, synchronizacji klientow
i magazynowania archiwalnego.

**Przyklad: 4 dopisywania z chunk_power=2 (chunk_size=4)**

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

## Korzen stanu

Korzen stanu laczy oba poziomy w jeden hasz:

```rust
fn compute_state_root(
    mmr_root: &[u8; 32],         // Chunk MMR root (or [0;32] if empty)
    dense_tree_root: &[u8; 32],  // Root hash of current buffer (dense tree)
) -> [u8; 32] {
    blake3("bulk_state" || mmr_root || dense_tree_root)
}
```

`total_count` i `chunk_power` **nie** sa zawarte w korzeniu stanu, poniewaz
sa juz uwierzytelnione przez hasz wartosci Merk — sa polami zserializowanego
`Element` przechowywanego w nadrzednym wezle Merk. Korzen stanu zawiera tylko
zobowiazania na poziomie danych (`mmr_root` i `dense_tree_root`). Jest to hasz,
ktory przeplywa jako hasz potomny Merk i propaguje sie w gore do hasz korzenia GroveDB.

## Korzen gestego drzewa Merkle

Gdy chunk jest kompaktowany, wpisy potrzebuja pojedynczego 32-bajtowego zobowiazania.
BulkAppendTree uzywa **gestego (kompletnego) binarnego drzewa Merkle**:

```text
Given entries [e_0, e_1, e_2, e_3]:

Level 0 (leaves):  blake3(e_0)  blake3(e_1)  blake3(e_2)  blake3(e_3)
                      \__________/              \__________/
Level 1:           blake3(h_0 || h_1)       blake3(h_2 || h_3)
                          \____________________/
Level 2 (root):    blake3(h_01 || h_23)  ← this is the dense Merkle root
```

Poniewaz `chunk_size` jest zawsze potega dwojki (z definicji: `1u32 << chunk_power`),
drzewo jest zawsze kompletne (nie sa potrzebne wypelnienia ani sztuczne liscie).
Liczba haszy wynosi dokladnie `2 * chunk_size - 1`:
- `chunk_size` haszy lisci (jeden na wpis)
- `chunk_size - 1` haszy wezlow wewnetrznych

Implementacja korzenia gestego drzewa Merkle znajduje sie w `grovedb-mmr/src/dense_merkle.rs`
i udostepnia dwie funkcje:
- `compute_dense_merkle_root(hashes)` — z wczesniej zahaszowanych lisci
- `compute_dense_merkle_root_from_values(values)` — najpierw haszuje wartosci,
  potem buduje drzewo

## Serializacja blobów chunkow

Bloby chunkow to niezmienne archiwa produkowane przez kompakcje. Serializator
automatycznie wybiera najbardziej zwiezly format przewodowy na podstawie rozmiarow wpisow:

**Format o stalym rozmiarze** (flaga `0x01`) — gdy wszystkie wpisy maja taka sama dlugosc:

```text
┌──────┬──────────┬─────────────┬─────────┬─────────┬─────────┐
│ 0x01 │ count    │ entry_size  │ entry_0 │ entry_1 │ ...     │
│ 1B   │ 4B (BE)  │ 4B (BE)     │ N bytes │ N bytes │         │
└──────┴──────────┴─────────────┴─────────┴─────────┴─────────┘
Total: 1 + 4 + 4 + (count × entry_size) bytes
```

**Format o zmiennym rozmiarze** (flaga `0x00`) — gdy wpisy maja rozne dlugosci:

```text
┌──────┬──────────┬─────────┬──────────┬─────────┬─────────────┐
│ 0x00 │ len_0    │ entry_0 │ len_1    │ entry_1 │ ...         │
│ 1B   │ 4B (BE)  │ N bytes │ 4B (BE)  │ M bytes │             │
└──────┴──────────┴─────────┴──────────┴─────────┴─────────────┘
Total: 1 + Σ(4 + len_i) bytes
```

Format o stalym rozmiarze oszczedza 4 bajty na wpis w porownaniu do zmiennego, co
sumuje sie znaczaco dla duzych chunkow danych o jednolitym rozmiarze (jak 32-bajtowe
zobowiazania haszowe). Dla 1024 wpisow po 32 bajty kazdy:
- Staly: `1 + 4 + 4 + 32768 = 32 777 bajtow`
- Zmienny: `1 + 1024 × (4 + 32) = 36 865 bajtow`
- Oszczednosc: ~11%

## Uklad kluczy magazynowania

Wszystkie dane BulkAppendTree znajduja sie w przestrzeni nazw **data**, z kluczami
z jednobajtowymi prefiksami:

| Wzorzec klucza | Format | Rozmiar | Przeznaczenie |
|---|---|---|---|
| `M` | 1 bajt | 1B | Klucz metadanych |
| `b` + `{index}` | `b` + u32 BE | 5B | Wpis bufora pod indeksem |
| `e` + `{index}` | `e` + u64 BE | 9B | Blob chunka pod indeksem |
| `m` + `{pos}` | `m` + u64 BE | 9B | Wezel MMR pod pozycja |

**Metadane** przechowuja `mmr_size` (8 bajtow BE). `total_count` i `chunk_power` sa
przechowywane w samym elemencie (w nadrzednym Merk), nie w metadanych przestrzeni nazw data.
Ten podzial oznacza, ze odczyt licznika to proste wyszukiwanie elementu bez otwierania
kontekstu magazynowania danych.

Klucze bufora uzywaja indeksow u32 (od 0 do `chunk_size - 1`), poniewaz pojemnosc bufora
jest ograniczona przez `chunk_size` (u32, obliczany jako `1u32 << chunk_power`). Klucze
chunkow uzywaja indeksow u64, poniewaz liczba ukonczonych chunkow moze rosnac w nieskonczonosc.

## Struktura BulkAppendTree

```rust
pub struct BulkAppendTree<S> {
    pub total_count: u64,                        // Total values ever appended
    pub dense_tree: DenseFixedSizedMerkleTree<S>, // The buffer (dense tree)
}
```

Bufor JEST `DenseFixedSizedMerkleTree` — jego hasz korzenia to `dense_tree_root`.

**Akcesory:**
- `capacity() -> u16`: `dense_tree.capacity()` (= `2^height - 1`)
- `epoch_size() -> u64`: `capacity + 1` (= `2^height`, liczba wpisow na chunk)
- `height() -> u8`: `dense_tree.height()`

**Wartosci pochodne** (nieprzechowywane):

| Wartosc | Formula |
|---|---|
| `chunk_count` | `total_count / epoch_size` |
| `buffer_count` | `dense_tree.count()` |
| `mmr_size` | `leaf_count_to_mmr_size(chunk_count)` |

## Operacje GroveDB

BulkAppendTree integruje sie z GroveDB poprzez szesc operacji zdefiniowanych w
`grovedb/src/operations/bulk_append_tree.rs`:

### bulk_append

Glowna operacja mutujaca. Podaza standardowy wzorzec magazynowania nie-Merk w GroveDB:

```text
1. Validate element is BulkAppendTree
2. Open data storage context
3. Load tree from store
4. Append value (may trigger compaction)
5. Update element in parent Merk with new state_root + total_count
6. Propagate changes up through Merk hierarchy
7. Commit transaction
```

Adapter `AuxBulkStore` opakowuje wywolania `get_aux`/`put_aux`/`delete_aux` GroveDB
i akumuluje `OperationCost` w `RefCell` do sledzenia kosztow. Koszty haszowania
z operacji dopisywania sa dodawane do `cost.hash_node_calls`.

### Operacje odczytu

| Operacja | Co zwraca | Magazyn aux? |
|---|---|---|
| `bulk_get_value(path, key, position)` | Wartosc pod globalna pozycja | Tak — czyta z blobu chunka lub bufora |
| `bulk_get_chunk(path, key, chunk_index)` | Surowy blob chunka | Tak — czyta klucz chunka |
| `bulk_get_buffer(path, key)` | Wszystkie biezace wpisy bufora | Tak — czyta klucze bufora |
| `bulk_count(path, key)` | Calkowita liczba (u64) | Nie — czyta z elementu |
| `bulk_chunk_count(path, key)` | Ukonczone chunki (u64) | Nie — obliczane z elementu |

Operacja `get_value` transparentnie kieruje wedlug pozycji:

```text
if position < completed_chunks × chunk_size:
    chunk_idx = position / chunk_size
    intra_idx = position % chunk_size
    → read chunk blob, deserialize, return entry[intra_idx]
else:
    buffer_idx = position % chunk_size
    → read buffer_key(buffer_idx)
```

## Operacje wsadowe i preprocessing

BulkAppendTree wspiera operacje wsadowe poprzez wariant `GroveOp::BulkAppend`.
Poniewaz `execute_ops_on_path` nie ma dostepu do kontekstu magazynowania danych,
wszystkie operacje BulkAppend musza byc przetworzone wstepnie przed `apply_body`.

Potok preprocessingu:

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

Wariant `append_with_mem_buffer` unika problemow odczytu-po-zapisie: wpisy bufora
sa sledzone w `Vec<Vec<u8>>` w pamieci, wiec kompakcja moze je odczytac nawet gdy
magazyn transakcyjny nie zostal jeszcze zatwierdzony.

## Trait BulkStore

```rust
pub trait BulkStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String>;
    fn delete(&self, key: &[u8]) -> Result<(), String>;
}
```

Metody przyjmuja `&self` (nie `&mut self`) aby pasowac do wzorca wewnetrznej
mutowalnosci GroveDB, gdzie zapisy ida przez batch. Integracja z GroveDB implementuje
to poprzez `AuxBulkStore`, ktory opakowuje `StorageContext` i akumuluje `OperationCost`.

`MmrAdapter` laczy `BulkStore` z traitami `MMRStoreReadOps`/`MMRStoreWriteOps`
ckb MMR, dodajac cache write-through dla poprawnosci odczytu-po-zapisie.

## Generowanie dowodow

Dowody BulkAppendTree wspieraja **zapytania zakresowe** po pozycjach. Struktura dowodu
zawiera wszystko, co potrzebuje bezstanowy weryfikator, aby potwierdzic ze konkretne
dane istnieja w drzewie:

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

**Kroki generowania** dla zakresu `[start, end)` (z `chunk_size = 1u32 << chunk_power`):

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

**Dlaczego wlaczac WSZYSTKIE wpisy bufora?** Bufor jest gestym drzewem Merkle, ktorego
hasz korzenia zobowiazuje sie do kazdego wpisu. Weryfikator musi odbudowac drzewo
ze wszystkich wpisow, aby zweryfikowac `dense_tree_root`. Poniewaz bufor jest ograniczony
przez `capacity` (najwyzej 65 535 wpisow), jest to akceptowalny koszt.

## Weryfikacja dowodow

Weryfikacja jest czysta funkcja — nie wymaga dostepu do bazy danych. Wykonuje piec sprawdzen:

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

Po udanej weryfikacji, `BulkAppendTreeProofResult` udostepnia metode
`values_in_range(start, end)`, ktora wyodrębnia konkretne wartosci ze zweryfikowanych
blobów chunkow i wpisow bufora.

## Jak to sie laczy z haszem korzenia GroveDB

BulkAppendTree jest **drzewem nie-Merk** — przechowuje dane w przestrzeni nazw data,
nie w potomnym poddrzewie Merk. W nadrzednym Merk element jest przechowywany jako:

```text
Element::BulkAppendTree(total_count, chunk_power, flags)
```

Korzen stanu przeplywa jako hasz potomny Merk. Hasz wezla nadrzednego Merk to:

```text
combine_hash(value_hash(element_bytes), state_root)
```

`state_root` przeplywa jako hasz potomny Merk (poprzez parametr `subtree_root_hash`
w `insert_subtree`). Kazda zmiana korzenia stanu propaguje sie w gore przez hierarchie
Merk GroveDB do hasz korzenia.

W dowodach V1 (§9.6), dowod nadrzednego Merk dowodzi bajtow elementu i powiazania
hasza potomnego, a `BulkAppendTreeProof` dowodzi, ze zapytane dane sa spojne z
`state_root` uzytym jako hasz potomny.

## Sledzenie kosztow

Koszt haszowania kazdej operacji jest sledzony jawnie:

| Operacja | Wywolania Blake3 | Uwagi |
|---|---|---|
| Pojedyncze dopisanie (bez kompakcji) | 3 | 2 dla lancucha haszow bufora + 1 dla korzenia stanu |
| Pojedyncze dopisanie (z kompakcja) | 3 + 2C - 1 + ~2 | Lancuch + gestykorzen Merkle (C=chunk_size) + MMR push + korzen stanu |
| `get_value` z chunka | 0 | Czysta deserializacja, bez haszowania |
| `get_value` z bufora | 0 | Bezposrednie wyszukiwanie klucza |
| Generowanie dowodu | Zalezy od liczby chunkow | Korzen gestego Merkle na chunk + dowod MMR |
| Weryfikacja dowodu | 2C·K - K + B·2 + 1 | K chunkow, B wpisow bufora, C chunk_size |

**Zamortyzowany koszt na dopisanie**: Dla chunk_size=1024 (chunk_power=10), narzut
kompakcji ~2047 haszy (korzen gestego Merkle) jest amortyzowany na 1024 dopisywania,
dodajac ~2 hasze na dopisanie. W polaczeniu z 3 haszami per-dopisanie, zamortyzowany
calkowity to **~5 wywolan blake3 na dopisanie** — bardzo wydajne dla kryptograficznie
uwierzytelnionej struktury.

## Porownanie z MmrTree

| | BulkAppendTree | MmrTree |
|---|---|---|
| **Architektura** | Dwupoziomowa (bufor + chunk MMR) | Pojedynczy MMR |
| **Koszt haszowania per-dopisanie** | 3 (+ zamortyzowane ~2 dla kompakcji) | ~2 |
| **Granularnosc dowodow** | Zapytania zakresowe po pozycjach | Indywidualne dowody lisci |
| **Niezmienne migawki** | Tak (bloby chunkow) | Nie |
| **Przyjazny CDN** | Tak (bloby chunkow buforowalne) | Nie |
| **Wpisy bufora** | Tak (wszystkie potrzebne do dowodu) | N/D |
| **Najlepszy do** | Dzienniki o wysokiej przepustowosci, synchronizacja masowa | Dzienniki zdarzen, indywidualne wyszukiwania |
| **Dyskryminator elementu** | 13 | 12 |
| **TreeType** | 9 | 8 |

Wybierz MmrTree gdy potrzebujesz indywidualnych dowodow lisci z minimalnym narzutem.
Wybierz BulkAppendTree gdy potrzebujesz zapytan zakresowych, synchronizacji masowej
i migawek opartych na chunkach.

## Pliki implementacji

| Plik | Przeznaczenie |
|------|---------|
| `grovedb-bulk-append-tree/src/lib.rs` | Korzen crate, re-eksporty |
| `grovedb-bulk-append-tree/src/tree/mod.rs` | Struktura `BulkAppendTree`, akcesory stanu, persystencja metadanych |
| `grovedb-bulk-append-tree/src/tree/append.rs` | `append()`, `append_with_mem_buffer()`, `compact_entries()` |
| `grovedb-bulk-append-tree/src/tree/hash.rs` | `compute_state_root` |
| `grovedb-bulk-append-tree/src/tree/keys.rs` | `META_KEY`, `buffer_key`, `chunk_key`, `mmr_node_key` |
| `grovedb-bulk-append-tree/src/tree/query.rs` | `get_value`, `get_chunk`, `get_buffer` |
| `grovedb-bulk-append-tree/src/tree/mmr_adapter.rs` | `MmrAdapter` z cache write-through |
| `grovedb-bulk-append-tree/src/chunk.rs` | Serializacja blobów chunkow (formaty staly + zmienny) |
| `grovedb-bulk-append-tree/src/proof.rs` | Generowanie i weryfikacja `BulkAppendTreeProof` |
| `grovedb-bulk-append-tree/src/store.rs` | Trait `BulkStore` |
| `grovedb-bulk-append-tree/src/error.rs` | Enum `BulkAppendError` |
| `grovedb/src/operations/bulk_append_tree.rs` | Operacje GroveDB, `AuxBulkStore`, preprocessing wsadowy |
| `grovedb/src/tests/bulk_append_tree_tests.rs` | 27 testow integracyjnych |

---
