# DenseAppendOnlyFixedSizeTree — Geste magazynowanie Merkle o stalej pojemnosci

DenseAppendOnlyFixedSizeTree to kompletne drzewo binarne o ustalonej wysokosci, w ktorym
**kazdy wezel** — zarowno wewnetrzny, jak i lisc — przechowuje wartosc danych. Pozycje
sa wypelniane sekwencyjnie w kolejnosci poziomowej (BFS): najpierw korzen (pozycja 0),
potem od lewej do prawej na kazdym poziomie. Zadne posrednie hasze nie sa utrwalane;
hasz korzenia jest przeliczany na biezaco przez rekurencyjne haszowanie od lisci do korzenia.

Ten projekt jest idealny dla malych, ograniczonych struktur danych, gdzie maksymalna
pojemnosc jest znana z gory i potrzebne jest dopisywanie O(1), pobieranie O(1) po pozycji
oraz zwiezly 32-bajtowy hasz korzenia zobowiazania, ktory zmienia sie po kazdym wstawieniu.

## Struktura drzewa

Drzewo o wysokosci *h* ma pojemnosc `2^h - 1` pozycji. Pozycje uzywaja indeksowania
poziomowego od 0:

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

Wartosci sa dopisywane sekwencyjnie: pierwsza wartosc trafia na pozycje 0 (korzen),
potem pozycje 1, 2, 3 i tak dalej. Oznacza to, ze korzen zawsze ma dane, a drzewo
wypelnia sie w kolejnosci poziomowej — najbardziej naturalnym porzadku przechodzenia
dla kompletnego drzewa binarnego.

## Obliczanie hasza

Hasz korzenia nie jest przechowywany osobno — jest przeliczany od nowa za kazdym razem,
gdy jest potrzebny. Algorytm rekurencyjny odwiedza tylko wypelnione pozycje:

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

**Kluczowe wlasciwosci:**
- Wszystkie wezly (lisc i wewnetrzny): `blake3(blake3(value) || H(left) || H(right))`
- Wezly-liscie: left_hash i right_hash to oba `[0; 32]` (niewypelnione dzieci)
- Niewypelnione pozycje: `[0u8; 32]` (zerowy hasz)
- Puste drzewo (count = 0): `[0u8; 32]`

**Nie sa uzywane znaczniki separacji domeny lisc/wewnetrzny.** Struktura drzewa
(`height` i `count`) jest uwierzytelniana zewnetrznie w nadrzednym
`Element::DenseAppendOnlyFixedSizeTree`, ktory przeplywa przez hierarchie Merk.
Weryfikator zawsze wie dokladnie, ktore pozycje sa liscmi, a ktore wezlami
wewnetrznymi na podstawie wysokosci i licznika, wiec atakujacy nie moze podstawic
jednego za drugie bez zlamania lancucha uwierzytelniania nadrzednego.

Oznacza to, ze hasz korzenia koduje zobowiazanie do kazdej przechowywanej wartosci
i jej dokladnej pozycji w drzewie. Zmiana jakiejkolwiek wartosci (gdyby byla mutowalna)
kaskadowalaby przez wszystkie hasze przodkow az do korzenia.

**Koszt haszowania:** Obliczanie hasza korzenia odwiedza wszystkie wypelnione pozycje
plus wszelkie niewypelnione dzieci. Dla drzewa z *n* wartosciami, najgorszy przypadek
to O(*n*) wywolan blake3. Jest to akceptowalne, poniewaz drzewo jest zaprojektowane
dla malych, ograniczonych pojemnosci (maksymalna wysokosc 16, maksymalnie 65 535 pozycji).

## Wariant elementu

```rust
Element::DenseAppendOnlyFixedSizeTree(
    u16,                   // count — number of values stored (max 65,535)
    u8,                    // height — immutable after creation (1..=16)
    Option<ElementFlags>,  // flags — storage flags
)
```

| Pole | Typ | Opis |
|---|---|---|
| `count` | `u16` | Liczba dotychczas wstawionych wartosci (maks. 65 535) |
| `height` | `u8` | Wysokosc drzewa (1..=16), niezmienna po utworzeniu |
| `flags` | `Option<ElementFlags>` | Opcjonalne flagi magazynowania |

Hasz korzenia NIE jest przechowywany w elemencie — przeplywa jako hasz potomny Merk
poprzez parametr `subtree_root_hash` w `insert_subtree`.

**Dyskryminator:** 14 (ElementType), TreeType = 10

**Rozmiar kosztu:** `DENSE_TREE_COST_SIZE = 6` bajtow (2 count + 1 height + 1 dyskryminator
+ 2 narzut)

## Uklad magazynowania

Tak jak MmrTree i BulkAppendTree, DenseAppendOnlyFixedSizeTree przechowuje dane w
przestrzeni nazw **data** (nie w potomnym Merk). Wartosci sa kluczowane po pozycji
jako big-endian `u64`:

```text
Subtree path: blake3(parent_path || key)

Storage keys:
  [0, 0, 0, 0, 0, 0, 0, 0] → value at position 0 (root)
  [0, 0, 0, 0, 0, 0, 0, 1] → value at position 1
  [0, 0, 0, 0, 0, 0, 0, 2] → value at position 2
  ...
```

Sam element (przechowywany w nadrzednym Merk) przenosi `count` i `height`.
Hasz korzenia przeplywa jako hasz potomny Merk. Oznacza to:
- **Odczyt hasza korzenia** wymaga przeliczenia z magazynu (O(n) haszowania)
- **Odczyt wartosci po pozycji to O(1)** — pojedyncze wyszukiwanie w magazynie
- **Wstawianie to O(n) haszowania** — jeden zapis do magazynu + pelne przeliczenie hasza korzenia

## Operacje

### `dense_tree_insert(path, key, value, tx, grove_version)`

Dopisuje wartosc na nastepna dostepna pozycje. Zwraca `(root_hash, position)`.

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

Pobiera wartosc na danej pozycji. Zwraca `None` jesli pozycja >= count.

### `dense_tree_root_hash(path, key, tx, grove_version)`

Zwraca hasz korzenia przechowywany w elemencie. Jest to hasz obliczony podczas
ostatniego wstawiania — przeliczenie nie jest potrzebne.

### `dense_tree_count(path, key, tx, grove_version)`

Zwraca liczbe przechowywanych wartosci (pole `count` z elementu).

## Operacje wsadowe

Wariant `GroveOp::DenseTreeInsert` wspiera wsadowe wstawianie poprzez standardowy
potok wsadowy GroveDB:

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

**Preprocessing:** Jak wszystkie typy drzew nie-Merk, operacje `DenseTreeInsert` sa
przetwarzane wstepnie przed wykonaniem glownego ciala wsadu. Metoda
`preprocess_dense_tree_ops`:

1. Grupuje wszystkie operacje `DenseTreeInsert` po `(path, key)`
2. Dla kazdej grupy wykonuje wstawiania sekwencyjnie (odczytuje element, wstawia
   kazda wartosc, aktualizuje hasz korzenia)
3. Konwertuje kazda grupe w operacje `ReplaceNonMerkTreeRoot` przenoszaca koncowy
   `root_hash` i `count` przez standardowa maszynerie propagacji

Wielokrotne wstawiania do tego samego gestego drzewa w ramach jednego wsadu sa
wspierane — sa przetwarzane w kolejnosci, a sprawdzenie spojnosci pozwala na
zduplikowane klucze dla tego typu operacji.

**Propagacja:** Hasz korzenia i licznik przeplywa przez wariant
`NonMerkTreeMeta::DenseTree` w `ReplaceNonMerkTreeRoot`, podazajac tym samym
wzorcem co MmrTree i BulkAppendTree.

## Dowody

DenseAppendOnlyFixedSizeTree wspiera **dowody podzapytan V1** poprzez wariant
`ProofBytes::DenseTree`. Indywidualne pozycje moga byc dowodzone przeciwko haszowi
korzenia drzewa uzywajac dowodow wlaczenia, ktore przenosi wartosci przodkow
i hasze poddrzew rodzenstwa.

### Struktura sciezki uwierzytelniania

Poniewaz wezly wewnetrzne haszuja **wlasna wartosc** (nie tylko hasze dzieci),
sciezka uwierzytelniania rozni sie od standardowego drzewa Merkle. Aby zweryfikowac
lisc na pozycji `p`, weryfikator potrzebuje:

1. **Wartosci liscia** (dowodzony wpis)
2. **Haszy wartosci przodkow** dla kazdego wezla wewnetrznego na sciezce od `p` do
   korzenia (tylko 32-bajtowy hasz, nie pelna wartosc)
3. **Haszy poddrzew rodzenstwa** dla kazdego dziecka, ktore NIE jest na sciezce

Poniewaz wszystkie wezly uzywaja `blake3(H(value) || H(left) || H(right))` (bez
znacznikow domen), dowod przenosi tylko 32-bajtowe hasze wartosci dla przodkow —
nie pelne wartosci. To utrzymuje dowody zwiezle niezaleznie od wielkosci
poszczegolnych wartosci.

```rust
pub struct DenseTreeProof {
    pub entries: Vec<(u16, Vec<u8>)>,            // proved (position, value) pairs
    pub node_value_hashes: Vec<(u16, [u8; 32])>, // ancestor value hashes on the auth path
    pub node_hashes: Vec<(u16, [u8; 32])>,       // precomputed sibling subtree hashes
}
```

> **Uwaga:** `height` i `count` nie sa w strukturze dowodu — weryfikator otrzymuje je
> z nadrzednego elementu, ktory jest uwierzytelniony przez hierarchie Merk.

### Przyklad z objasnieniem

Drzewo z height=3, capacity=7, count=5, dowodzenie pozycji 4:

```text
        0
       / \
      1   2
     / \ / \
    3  4 5  6
```

Sciezka od 4 do korzenia: `4 → 1 → 0`. Rozszerzony zbior: `{0, 1, 4}`.

Dowod zawiera:
- **entries**: `[(4, value[4])]` — dowodzona pozycja
- **node_value_hashes**: `[(0, H(value[0])), (1, H(value[1]))]` — hasze wartosci przodkow (32 bajty kazdy, nie pelne wartosci)
- **node_hashes**: `[(2, H(subtree_2)), (3, H(node_3))]` — rodzenstwo nie na sciezce

Weryfikacja przelicza hasz korzenia od dolu do gory:
1. `H(4) = blake3(blake3(value[4]) || [0;32] || [0;32])` — lisc (dzieci niewypelnione)
2. `H(3)` — z `node_hashes`
3. `H(1) = blake3(H(value[1]) || H(3) || H(4))` — wewnetrzny uzywa hasza wartosci z `node_value_hashes`
4. `H(2)` — z `node_hashes`
5. `H(0) = blake3(H(value[0]) || H(1) || H(2))` — korzen uzywa hasza wartosci z `node_value_hashes`
6. Porownaj `H(0)` z oczekiwanym haszem korzenia

### Dowody wielo-pozycyjne

Przy dowodzeniu wielu pozycji, rozszerzony zbior laczy nakladajace sie sciezki
uwierzytelniania. Wspolni przodkowie sa wlaczani tylko raz, co sprawia, ze dowody
wielo-pozycyjne sa bardziej zwiezle niz niezalezne dowody jedno-pozycyjne.

### Ograniczenie V0

Dowody V0 nie moga schodzic do gestych drzew. Jesli zapytanie V0 dopasuje
`DenseAppendOnlyFixedSizeTree` z podzapytaniem, system zwraca
`Error::NotSupported` kierujac wywolujacego do uzycia `prove_query_v1`.

### Kodowanie kluczy zapytan

Pozycje gestego drzewa sa kodowane jako klucze zapytan **big-endian u16** (2 bajty),
w odroznieniu od MmrTree i BulkAppendTree ktore uzywaja u64. Wszystkie standardowe
typy zakresow `QueryItem` sa wspierane.

## Porownanie z innymi drzewami nie-Merk

| | DenseTree | MmrTree | BulkAppendTree | CommitmentTree |
|---|---|---|---|---|
| **Dyskryminator elementu** | 14 | 12 | 13 | 11 |
| **TreeType** | 10 | 8 | 9 | 7 |
| **Pojemnosc** | Stala (`2^h - 1`, maks. 65 535) | Nieograniczona | Nieograniczona | Nieograniczona |
| **Model danych** | Kazda pozycja przechowuje wartosc | Tylko liscie | Bufor gestego drzewa + chunki | Tylko liscie |
| **Hasz w elemencie?** | Nie (przeplywa jako hasz potomny) | Nie (przeplywa jako hasz potomny) | Nie (przeplywa jako hasz potomny) | Nie (przeplywa jako hasz potomny) |
| **Koszt wstawiania (haszowanie)** | O(n) blake3 | O(1) zamortyzowane | O(1) zamortyzowane | ~33 Sinsemilla |
| **Rozmiar kosztu** | 6 bajtow | 11 bajtow | 12 bajtow | 12 bajtow |
| **Wsparcie dowodow** | V1 (Dense) | V1 (MMR) | V1 (Bulk) | V1 (CommitmentTree) |
| **Najlepszy do** | Male ograniczone struktury | Dzienniki zdarzen | Dzienniki o wysokiej przepustowosci | Zobowiazania ZK |

**Kiedy wybrac DenseAppendOnlyFixedSizeTree:**
- Maksymalna liczba wpisow jest znana w momencie tworzenia
- Potrzebujesz, aby kazda pozycja (wlacznie z wezlami wewnetrznymi) przechowywala dane
- Chcesz najprostszy mozliwy model danych bez nieograniczonego wzrostu
- Przeliczanie hasza korzenia O(n) jest akceptowalne (male wysokosci drzew)

**Kiedy NIE wybierac:**
- Potrzebujesz nieograniczonej pojemnosci → uzyj MmrTree lub BulkAppendTree
- Potrzebujesz kompatybilnosci ZK → uzyj CommitmentTree

## Przyklad uzycia

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

## Pliki implementacji

| Plik | Zawartosc |
|---|---|
| `grovedb-dense-fixed-sized-merkle-tree/src/lib.rs` | Trait `DenseTreeStore`, struktura `DenseFixedSizedMerkleTree`, rekurencyjne haszowanie |
| `grovedb-dense-fixed-sized-merkle-tree/src/proof.rs` | Struktura `DenseTreeProof`, `generate()`, `encode_to_vec()`, `decode_from_slice()` |
| `grovedb-dense-fixed-sized-merkle-tree/src/verify.rs` | `DenseTreeProof::verify()` — czysta funkcja, bez potrzeby magazynu |
| `grovedb-element/src/element/mod.rs` | `Element::DenseAppendOnlyFixedSizeTree` (dyskryminator 14) |
| `grovedb-element/src/element/constructor.rs` | `empty_dense_tree()`, `new_dense_tree()` |
| `merk/src/tree_type/mod.rs` | `TreeType::DenseAppendOnlyFixedSizeTree = 10` |
| `merk/src/tree_type/costs.rs` | `DENSE_TREE_COST_SIZE = 6` |
| `grovedb/src/operations/dense_tree.rs` | Operacje GroveDB, `AuxDenseTreeStore`, preprocessing wsadowy |
| `grovedb/src/operations/proof/generate.rs` | `generate_dense_tree_layer_proof()`, `query_items_to_positions()` |
| `grovedb/src/operations/proof/verify.rs` | `verify_dense_tree_lower_layer()` |
| `grovedb/src/operations/proof/mod.rs` | Wariant `ProofBytes::DenseTree` |
| `grovedb/src/batch/estimated_costs/average_case_costs.rs` | Model kosztow sredniego przypadku |
| `grovedb/src/batch/estimated_costs/worst_case_costs.rs` | Model kosztow najgorszego przypadku |
| `grovedb/src/tests/dense_tree_tests.rs` | 22 testy integracyjne |

---
