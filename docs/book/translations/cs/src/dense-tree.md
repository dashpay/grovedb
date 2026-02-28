# DenseAppendOnlyFixedSizeTree -- Huste Merklovo uloziste s pevnou kapacitou

DenseAppendOnlyFixedSizeTree je uplny binarni strom pevne vysky, kde **kazdy
uzel** -- jak vnitrni, tak listovy -- uklada datovou hodnotu. Pozice se plni
sekvencne v poradi po urovních (BFS): nejprve koren (pozice 0), pak zleva doprava
na kazde urovni. Zadne mezilehle hashe se neukladaji; korenovy hash se prepocitava
za behu rekurzivnim hashovanim od listu ke koreni.

Tento navrh je idealni pro male, ohranicene datove struktury, kde je maximalni
kapacita znama predem a potrebujete O(1) pripojeni, O(1) ziskani podle pozice
a kompaktni 32-bajtovy korenovy hash zavazek, ktery se meni po kazdem vlozeni.

## Struktura stromu

Strom vysky *h* ma kapacitu `2^h - 1` pozic. Pozice pouzivaji 0-indexovane
usporadani po urovnich:

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

Hodnoty se pripojuji sekvencne: prvni hodnota jde na pozici 0 (koren), pak
pozici 1, 2, 3 a tak dale. To znamena, ze koren vzdy obsahuje data a strom
se plni v poradi po urovnich -- nejprirozenejsi poradn prochazeni uplneho
binarniho stromu.

## Vypocet hashe

Korenovy hash se neuklada oddelene -- prepocitava se od nuly vzdy, kdyz je
potreba. Rekurzivni algoritmus navstevuje pouze obsazene pozice:

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

**Klicove vlastnosti:**
- Vsechny uzly (listove i vnitrni): `blake3(blake3(value) || H(left) || H(right))`
- Listove uzly: left_hash a right_hash jsou oba `[0; 32]` (neobsazeni potomci)
- Neobsazene pozice: `[0u8; 32]` (nulovy hash)
- Prazdny strom (count = 0): `[0u8; 32]`

**Nepouzivaji se zadne znacky oddeleni domeny list/vnitrni uzel.** Struktura
stromu (`height` a `count`) je externe autentizovana v rodicovskem
`Element::DenseAppendOnlyFixedSizeTree`, ktery proudi pres hierarchii Merk.
Overovatel vzdy presne vi, ktere pozice jsou listy vs vnitrni uzly z vysky
a poctu, takze utocnik nemuze nahradit jedno za druhe, aniz by narusil
rodicovsky autentizacni retezec.

To znamena, ze korenovy hash koduje zavazek ke kazde ulozene hodnote a jeji
presne pozici ve strome. Zmena jakekoliv hodnoty (kdyby byla mutovatelna)
by kaskadovala pres vsechny hashe predku az ke koreni.

**Naklady hashe:** Vypocet korenoveho hashe navstevi vsechny obsazene pozice
plus vsechny neobsazene potomky. Pro strom s *n* hodnotami je nejhorsi pripad
O(*n*) volani blake3. Toto je prijatelne, protoze strom je navrzen pro male,
ohranicene kapacity (maximalni vyska 16, maximalne 65 535 pozic).

## Varianta elementu

```rust
Element::DenseAppendOnlyFixedSizeTree(
    u16,                   // count — number of values stored (max 65,535)
    u8,                    // height — immutable after creation (1..=16)
    Option<ElementFlags>,  // flags — storage flags
)
```

| Pole | Typ | Popis |
|---|---|---|
| `count` | `u16` | Pocet dosud vlozenych hodnot (max 65 535) |
| `height` | `u8` | Vyska stromu (1..=16), nemenna po vytvoreni |
| `flags` | `Option<ElementFlags>` | Volitelne priznaky uloziste |

Korenovy hash NENI ulozen v Elementu -- proudi jako Merk child hash
pres parametr `subtree_root_hash` metody `insert_subtree`.

**Diskriminant:** 14 (ElementType), TreeType = 10

**Velikost nakladu:** `DENSE_TREE_COST_SIZE = 6` bajtu (2 count + 1 height +
1 diskriminant + 2 rezijni)

## Rozlozeni uloziste

Stejne jako MmrTree a BulkAppendTree, DenseAppendOnlyFixedSizeTree uklada data
v **datovem** prostoru jmen (ne v detskem Merk). Hodnoty jsou klicovany jejich
pozici jako big-endian `u64`:

```text
Subtree path: blake3(parent_path || key)

Storage keys:
  [0, 0, 0, 0, 0, 0, 0, 0] → value at position 0 (root)
  [0, 0, 0, 0, 0, 0, 0, 1] → value at position 1
  [0, 0, 0, 0, 0, 0, 0, 2] → value at position 2
  ...
```

Samotny Element (ulozeny v rodicovskem Merk) nese `count` a `height`.
Korenovy hash proudi jako Merk child hash. To znamena:
- **Cteni korenoveho hashe** vyzaduje prepocet z uloziste (O(n) hashovani)
- **Cteni hodnoty podle pozice je O(1)** -- jedine vyhledani v ulozisti
- **Vkladani je O(n) hashovani** -- jeden zapis do uloziste + uplny prepocet korenoveho hashe

## Operace

### `dense_tree_insert(path, key, value, tx, grove_version)`

Pripoji hodnotu na dalsi volnou pozici. Vraci `(root_hash, position)`.

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

Ziska hodnotu na dane pozici. Vraci `None`, pokud je pozice >= count.

### `dense_tree_root_hash(path, key, tx, grove_version)`

Vraci korenovy hash ulozeny v elementu. Toto je hash vypocteny behem
posledniho vkladani -- neni treba prepocet.

### `dense_tree_count(path, key, tx, grove_version)`

Vraci pocet ulozenych hodnot (pole `count` z elementu).

## Davkove operace

Varianta `GroveOp::DenseTreeInsert` podporuje davkove vkladani pres standardni
davkove potrubi GroveDB:

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

**Predzpracovani:** Stejne jako vsechny ne-Merk typy stromu, operace `DenseTreeInsert`
jsou predzpracovany pred provedenim hlavniho tela davky. Metoda
`preprocess_dense_tree_ops`:

1. Seskupi vsechny operace `DenseTreeInsert` podle `(path, key)`
2. Pro kazdou skupinu provede vkladani sekvencne (cteni elementu, vlozeni
   kazde hodnoty, aktualizace korenoveho hashe)
3. Prevede kazdou skupinu na operaci `ReplaceNonMerkTreeRoot`, ktera nese
   finalni `root_hash` a `count` pres standardni propagacni stroj

Vice vkladani do stejneho husteho stromu v ramci jedne davky je podporovano --
zpracovavaji se v poradi a kontrola konzistence povoluje duplicitni klice
pro tento typ operace.

**Propagace:** Korenovy hash a pocet proudi pres variantu
`NonMerkTreeMeta::DenseTree` v `ReplaceNonMerkTreeRoot`, nasledujic stejny
vzor jako MmrTree a BulkAppendTree.

## Dukazy

DenseAppendOnlyFixedSizeTree podporuje **V1 dukazy poddotazu** pres variantu
`ProofBytes::DenseTree`. Jednotlive pozice mohou byt dokazovany vuci
korenovemu hashi stromu pomoci dukazu o zahrnutí, ktere nesou hodnoty predku
a sourozenecke hashe podstromu.

### Struktura autentizacni cesty

Protoze vnitrni uzly hashuji svou **vlastni hodnotu** (ne jen hashe deti),
autentizacni cesta se lisi od standardniho Merklova stromu. Pro overeni listu
na pozici `p` overovatel potrebuje:

1. **Hodnotu listu** (dokazovany zaznam)
2. **Hashe hodnot predku** pro kazdy vnitrni uzel na ceste od `p` ke koreni (pouze 32-bajtovy hash, ne plna hodnota)
3. **Sourozenecke hashe podstromu** pro kazde dite, ktere NENI na ceste

Protoze vsechny uzly pouzivaji `blake3(H(value) || H(left) || H(right))` (bez
znacek domeny), dukaz nese pouze 32-bajtove hashe hodnot pro predky -- ne plne
hodnoty. To udrzuje dukazy kompaktni bez ohledu na to, jak velke jsou jednotlive
hodnoty.

```rust
pub struct DenseTreeProof {
    pub entries: Vec<(u16, Vec<u8>)>,            // proved (position, value) pairs
    pub node_value_hashes: Vec<(u16, [u8; 32])>, // ancestor value hashes on the auth path
    pub node_hashes: Vec<(u16, [u8; 32])>,       // precomputed sibling subtree hashes
}
```

> **Poznamka:** `height` a `count` nejsou ve strukture dukazu -- overovatel je ziskava z rodicovskeho Elementu, ktery je autentizovan hierarchii Merk.

### Priklad pruchodu

Strom s height=3, capacity=7, count=5, dokazovani pozice 4:

```text
        0
       / \
      1   2
     / \ / \
    3  4 5  6
```

Cesta od 4 ke koreni: `4 → 1 → 0`. Rozsirena mnozina: `{0, 1, 4}`.

Dukaz obsahuje:
- **entries**: `[(4, value[4])]` -- dokazovana pozice
- **node_value_hashes**: `[(0, H(value[0])), (1, H(value[1]))]` -- hashe hodnot predku (32 bajtu kazdy, ne plne hodnoty)
- **node_hashes**: `[(2, H(subtree_2)), (3, H(node_3))]` -- sourozenci mimo cestu

Overeni prepocita korenovy hash zdola nahoru:
1. `H(4) = blake3(blake3(value[4]) || [0;32] || [0;32])` -- list (deti jsou neobsazene)
2. `H(3)` -- z `node_hashes`
3. `H(1) = blake3(H(value[1]) || H(3) || H(4))` -- vnitrni uzel pouziva hash hodnoty z `node_value_hashes`
4. `H(2)` -- z `node_hashes`
5. `H(0) = blake3(H(value[0]) || H(1) || H(2))` -- koren pouziva hash hodnoty z `node_value_hashes`
6. Porovnani `H(0)` s ocekavanym korenovym hashem

### Dukazy vice pozic

Pri dokazovani vice pozic se rozsirena mnozina slucuje prekryvajici se
autentizacni cesty. Sdileni predci jsou zahrnuti pouze jednou, cimz se
dukazy vice pozic stavaji kompaktnejsimi nez nezavisle dukazy jedne pozice.

### Omezeni V0

V0 dukazy nemohou sestoupit do hustych stromu. Pokud V0 dotaz odpovida
`DenseAppendOnlyFixedSizeTree` s poddotazem, system vraci
`Error::NotSupported` smerujici volajiciho k pouziti `prove_query_v1`.

### Kodovani klicu dotazu

Pozice husteho stromu jsou kodovany jako **big-endian u16** (2-bajtove) klice
dotazu, na rozdil od MmrTree a BulkAppendTree, ktere pouzivaji u64. Vsechny
standardni typy `QueryItem` s rozsahy jsou podporovany.

## Srovnani s ostatnimi ne-Merk stromy

| | DenseTree | MmrTree | BulkAppendTree | CommitmentTree |
|---|---|---|---|---|
| **Diskriminant elementu** | 14 | 12 | 13 | 11 |
| **TreeType** | 10 | 8 | 9 | 7 |
| **Kapacita** | Pevna (`2^h - 1`, max 65 535) | Neomezena | Neomezena | Neomezena |
| **Datovy model** | Kazda pozice uklada hodnotu | Pouze listy | Husty stromovy buffer + chunky | Pouze listy |
| **Hash v Elementu?** | Ne (proudi jako child hash) | Ne (proudi jako child hash) | Ne (proudi jako child hash) | Ne (proudi jako child hash) |
| **Naklady vlozeni (hashovani)** | O(n) blake3 | O(1) amortizovane | O(1) amortizovane | ~33 Sinsemilla |
| **Velikost nakladu** | 6 bajtu | 11 bajtu | 12 bajtu | 12 bajtu |
| **Podpora dukazu** | V1 (Dense) | V1 (MMR) | V1 (Bulk) | V1 (CommitmentTree) |
| **Nejlepsi pro** | Male ohranicene struktury | Logy udalosti | Vysoko-propustne logy | ZK zavazky |

**Kdy zvolit DenseAppendOnlyFixedSizeTree:**
- Maximalni pocet zaznamu je znam v dobe vytvoreni
- Potrebujete, aby kazda pozice (vcetne vnitrnich uzlu) ukladala data
- Chcete nejjednodussi mozny datovy model bez neomezeneho rustu
- O(n) prepocet korenoveho hashe je prijatelny (male vysky stromu)

**Kdy ho NEVOLIT:**
- Potrebujete neomezenou kapacitu → pouzijte MmrTree nebo BulkAppendTree
- Potrebujete ZK kompatibilitu → pouzijte CommitmentTree

## Priklad pouziti

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

## Implementacni soubory

| Soubor | Obsah |
|---|---|
| `grovedb-dense-fixed-sized-merkle-tree/src/lib.rs` | Trait `DenseTreeStore`, struktura `DenseFixedSizedMerkleTree`, rekurzivni hash |
| `grovedb-dense-fixed-sized-merkle-tree/src/proof.rs` | Struktura `DenseTreeProof`, `generate()`, `encode_to_vec()`, `decode_from_slice()` |
| `grovedb-dense-fixed-sized-merkle-tree/src/verify.rs` | `DenseTreeProof::verify()` -- cista funkce, bez uloziste |
| `grovedb-element/src/element/mod.rs` | `Element::DenseAppendOnlyFixedSizeTree` (diskriminant 14) |
| `grovedb-element/src/element/constructor.rs` | `empty_dense_tree()`, `new_dense_tree()` |
| `merk/src/tree_type/mod.rs` | `TreeType::DenseAppendOnlyFixedSizeTree = 10` |
| `merk/src/tree_type/costs.rs` | `DENSE_TREE_COST_SIZE = 6` |
| `grovedb/src/operations/dense_tree.rs` | Operace GroveDB, `AuxDenseTreeStore`, davkove predzpracovani |
| `grovedb/src/operations/proof/generate.rs` | `generate_dense_tree_layer_proof()`, `query_items_to_positions()` |
| `grovedb/src/operations/proof/verify.rs` | `verify_dense_tree_lower_layer()` |
| `grovedb/src/operations/proof/mod.rs` | Varianta `ProofBytes::DenseTree` |
| `grovedb/src/batch/estimated_costs/average_case_costs.rs` | Model prumernych nakladu |
| `grovedb/src/batch/estimated_costs/worst_case_costs.rs` | Model nejhorsich nakladu |
| `grovedb/src/tests/dense_tree_tests.rs` | 22 integracnich testu |

---
