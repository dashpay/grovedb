# System dowodow (proof)

System dowodow GroveDB pozwala dowolnej stronie zweryfikowac poprawnosc wynikow
zapytania bez posiadania pelnej bazy danych. Dowod (proof) jest kompaktowa
reprezentacja odpowiedniej struktury drzewa, ktora pozwala odtworzyc hasz korzenia.

## Operacje dowodow oparte na stosie

Dowody sa kodowane jako sekwencja **operacji**, ktore rekonstruuja czesciowe drzewo
za pomoca maszyny stosowej:

```rust
// merk/src/proofs/mod.rs
pub enum Op {
    Push(Node),        // Poloz wezel na stos (rosnacy porzadek kluczy)
    PushInverted(Node),// Poloz wezel (malejacy porzadek kluczy)
    Parent,            // Zdejmij rodzica, zdejmij potomka → podlacz potomka jako LEWY rodzica
    Child,             // Zdejmij potomka, zdejmij rodzica → podlacz potomka jako PRAWY rodzica
    ParentInverted,    // Zdejmij rodzica, zdejmij potomka → podlacz potomka jako PRAWY rodzica
    ChildInverted,     // Zdejmij potomka, zdejmij rodzica → podlacz potomka jako LEWY rodzica
}
```

Wykonanie uzywa stosu:

Operacje dowodu: `Push(B), Push(A), Parent, Push(C), Child`

| Krok | Operacja | Stos (gora→prawo) | Akcja |
|------|-----------|-------------------|--------|
| 1 | Push(B) | [ B ] | Poloz B na stos |
| 2 | Push(A) | [ B , A ] | Poloz A na stos |
| 3 | Parent | [ A{left:B} ] | Zdejmij A (rodzic), zdejmij B (potomek), B → LEWY A |
| 4 | Push(C) | [ A{left:B} , C ] | Poloz C na stos |
| 5 | Child | [ A{left:B, right:C} ] | Zdejmij C (potomek), zdejmij A (rodzic), C → PRAWY A |

Wynik koncowy -- jedno drzewo na stosie:

```mermaid
graph TD
    A_proof["A<br/>(korzen)"]
    B_proof["B<br/>(lewy)"]
    C_proof["C<br/>(prawy)"]
    A_proof --> B_proof
    A_proof --> C_proof

    style A_proof fill:#fef9e7,stroke:#f39c12,stroke-width:2px
```

> Weryfikator oblicza `node_hash(A) = Blake3(kv_hash_A || node_hash_B || node_hash_C)` i sprawdza, czy odpowiada oczekiwanemu haszowi korzenia.

Oto funkcja `execute` (`merk/src/proofs/tree.rs`):

```rust
pub fn execute<I, F>(ops: I, collapse: bool, mut visit_node: F) -> CostResult<Tree, Error>
where
    I: IntoIterator<Item = Result<Op, Error>>,
    F: FnMut(&Node) -> Result<(), Error>,
{
    let mut stack: Vec<Tree> = Vec::with_capacity(32);

    for op in ops {
        match op? {
            Op::Parent => {
                let (mut parent, child) = (try_pop(&mut stack), try_pop(&mut stack));
                parent.left = Some(Child { tree: Box::new(child), hash: child.hash() });
                stack.push(parent);
            }
            Op::Child => {
                let (child, mut parent) = (try_pop(&mut stack), try_pop(&mut stack));
                parent.right = Some(Child { tree: Box::new(child), hash: child.hash() });
                stack.push(parent);
            }
            Op::Push(node) => {
                visit_node(&node)?;
                stack.push(Tree::from(node));
            }
            // ... Warianty Inverted zamieniaja lewy/prawy
        }
    }
    // Ostatni element na stosie to korzen
}
```

## Typy wezlow w dowodach

Kazdy `Push` przenosi `Node` zawierajacy tyle informacji, ile wystarczy do
weryfikacji:

```rust
pub enum Node {
    // Minimum informacji — tylko hasz. Uzywane dla odleglego rodzenstwa.
    Hash(CryptoHash),

    // Hasz KV dla wezlow na sciezce, ale nie odpytywanych.
    KVHash(CryptoHash),

    // Pelny klucz-wartosc dla odpytywanych elementow.
    KV(Vec<u8>, Vec<u8>),

    // Klucz, wartosc i wstepnie obliczony value_hash.
    // Uzywane dla poddrzew, gdzie value_hash = combine_hash(...)
    KVValueHash(Vec<u8>, Vec<u8>, CryptoHash),

    // KV z typem cechy — dla ProvableCountTree lub przywracania chunkow.
    KVValueHashFeatureType(Vec<u8>, Vec<u8>, CryptoHash, TreeFeatureType),

    // Referencja: klucz, zdereferencjonowana wartosc, hasz elementu referencji.
    KVRefValueHash(Vec<u8>, Vec<u8>, CryptoHash),

    // Dla elementow w ProvableCountTree.
    KVCount(Vec<u8>, Vec<u8>, u64),

    // Hasz KV + licznik dla nie-odpytywanych wezlow ProvableCountTree.
    KVHashCount(CryptoHash, u64),

    // Referencja w ProvableCountTree.
    KVRefValueHashCount(Vec<u8>, Vec<u8>, CryptoHash, u64),

    // Dla dowodow granic/nieobecnosci w ProvableCountTree.
    KVDigestCount(Vec<u8>, CryptoHash, u64),

    // Klucz + value_hash dla dowodow nieobecnosci (zwykle drzewa).
    KVDigest(Vec<u8>, CryptoHash),
}
```

Wybor typu Node determinuje, jakie informacje potrzebuje weryfikator:

**Zapytanie: "Pobierz wartosc dla klucza 'bob'"**

```mermaid
graph TD
    dave["dave<br/><b>KVHash</b><br/>(na sciezce, nie odpytywany)"]
    bob["bob<br/><b>KVValueHash</b><br/>klucz + wartosc + value_hash<br/><i>ODPYTYWANY WEZEL</i>"]
    frank["frank<br/><b>Hash</b><br/>(odlegle rodzenstwo,<br/>tylko 32-bajtowy hasz)"]
    alice["alice<br/><b>Hash</b><br/>(tylko 32-bajtowy hasz)"]
    carol["carol<br/><b>Hash</b><br/>(tylko 32-bajtowy hasz)"]

    dave --> bob
    dave --> frank
    bob --> alice
    bob --> carol

    style bob fill:#d5f5e3,stroke:#27ae60,stroke-width:3px
    style dave fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style frank fill:#e8e8e8,stroke:#999
    style alice fill:#e8e8e8,stroke:#999
    style carol fill:#e8e8e8,stroke:#999
```

> Zielony = odpytywany wezel (pelne dane ujawnione). Zolty = na sciezce (tylko kv_hash). Szary = rodzenstwo (tylko 32-bajtowe hasze wezlow).

Zakodowane jako operacje dowodu:

| # | Op | Efekt |
|---|----|----|
| 1 | Push(Hash(alice_node_hash)) | Poloz hasz alice |
| 2 | Push(KVValueHash("bob", value, value_hash)) | Poloz bob z pelnymi danymi |
| 3 | Parent | alice staje sie lewym potomkiem bob |
| 4 | Push(Hash(carol_node_hash)) | Poloz hasz carol |
| 5 | Child | carol staje sie prawym potomkiem bob |
| 6 | Push(KVHash(dave_kv_hash)) | Poloz kv_hash dave |
| 7 | Parent | poddrzewo bob staje sie lewym dave |
| 8 | Push(Hash(frank_node_hash)) | Poloz hasz frank |
| 9 | Child | frank staje sie prawym potomkiem dave |

## Typy wezlow dowodowych wedlug typu drzewa

Kazdy typ drzewa w GroveDB uzywa okreslonego zestawu typow wezlow dowodowych
w zaleznosci od **roli** wezla w dowodzie. Istnieja cztery role:

| Rola | Opis |
|------|------|
| **Odpytywany** | Wezel pasuje do zapytania — pelny klucz + wartosc ujawnione |
| **Na sciezce** | Wezel jest przodkiem odpytywanych wezlow — potrzebny tylko kv_hash |
| **Graniczny** | Sasiaduje z brakujacym kluczem — dowodzi nieobecnosci |
| **Odlegly** | Poddrzewo rodzenstwa nie na sciezce dowodu — potrzebny tylko node_hash |

### Zwykle drzewa (Tree, SumTree, BigSumTree, CountTree, CountSumTree)

Wszystkie piec typow drzew uzywa identycznych typow wezlow dowodowych i tej
samej funkcji haszujacej: `compute_hash` (= `node_hash(kv_hash, left, right)`).
**Nie ma roznicy** w sposobie ich dowodzenia na poziomie merk.

Kazdy wezel merk wewnetrznie niesie `feature_type` (BasicMerkNode,
SummedMerkNode, BigSummedMerkNode, CountedMerkNode, CountedSummedMerkNode),
ale jest on **nie wlaczony w hasz** i **nie wlaczony w dowod**. Dane
zagregowane (suma, licznik) dla tych typow drzew znajduja sie w
zserializowanych bajtach **nadrzednego** Element, ktore sa weryfikowane haszem
przez dowod drzewa nadrzednego:

| Typ drzewa | Element przechowuje | Merk feature_type (nie haszowany) |
|-----------|---------------------|-------------------------------|
| Tree | `Element::Tree(root_key, flags)` | `BasicMerkNode` |
| SumTree | `Element::SumTree(root_key, sum, flags)` | `SummedMerkNode(sum)` |
| BigSumTree | `Element::BigSumTree(root_key, sum, flags)` | `BigSummedMerkNode(sum)` |
| CountTree | `Element::CountTree(root_key, count, flags)` | `CountedMerkNode(count)` |
| CountSumTree | `Element::CountSumTree(root_key, count, sum, flags)` | `CountedSummedMerkNode(count, sum)` |

> **Skad pochodzi suma/licznik?** Gdy weryfikator przetwarza dowod dla
> `[root, my_sum_tree]`, dowod drzewa nadrzednego zawiera wezel `KVValueHash`
> dla klucza `my_sum_tree`. Pole `value` zawiera zserializowany
> `Element::SumTree(root_key, 42, flags)`. Poniewaz ta wartosc jest
> weryfikowana haszem (jej hasz jest zatwierdzony w nadrzednym korzeniu
> Merkle), suma `42` jest wiarygodna. feature_type na poziomie merk jest
> nieistotny.

| Rola | Typ wezla V0 | Typ wezla V1 | Funkcja haszujaca |
|------|-------------|-------------|---------------|
| Odpytywany element | `KV` | `KV` | `node_hash(kv_hash(key, H(value)), left, right)` |
| Odpytywane niepuste drzewo (bez subquery) | `KVValueHash` | `KVValueHashFeatureTypeWithChildHash` | `node_hash(kv_hash(key, value_hash), left, right)` |
| Odpytywane puste drzewo | `KVValueHash` | `KVValueHash` | `node_hash(kv_hash(key, value_hash), left, right)` |
| Odpytywana referencja | `KVRefValueHash` | `KVRefValueHash` | `node_hash(kv_hash(key, combine_hash(ref_hash, H(deref_value))), left, right)` |
| Na sciezce | `KVHash` | `KVHash` | `node_hash(kv_hash, left, right)` |
| Graniczny | `KVDigest` | `KVDigest` | `node_hash(kv_hash(key, value_hash), left, right)` |
| Odlegly | `Hash` | `Hash` | Uzywany bezposrednio |

> **Niepuste drzewa Z subquery** schodza do warstwy potomnej — wezel drzewa
> pojawia sie jako `KVValueHash` w dowodzie warstwy nadrzednej, a warstwa
> potomna ma wlasny dowod.

> **Dlaczego `KVValueHash` dla poddrzew?** value_hash poddrzewa to
> `combine_hash(H(element_bytes), child_root_hash)` — weryfikator nie moze
> tego przeliczyc z samych bajtow elementu (potrzebowalby hasza korzenia
> potomnego). Dlatego dowodzacy dostarcza wstepnie obliczony value_hash.
>
> **Dlaczego `KV` dla elementow?** value_hash elementu to po prostu
> `H(value)`, ktory weryfikator moze przeliczyc. Uzycie `KV` jest odporne na
> manipulacje: jezeli dowodzacy zmieni wartosc, hasz nie bedzie pasowac.

**Ulepszenie V1 — `KVValueHashFeatureTypeWithChildHash`:** W dowodach V1,
gdy odpytywane niepuste drzewo nie ma subquery (zapytanie zatrzymuje sie na
tym drzewie — element drzewa sam jest wynikiem), warstwa GroveDB uaktualnia
wezel merk do `KVValueHashFeatureTypeWithChildHash(key, value, value_hash,
feature_type, child_hash)`. Pozwala to weryfikatorowi sprawdzic
`combine_hash(H(value), child_hash) == value_hash`, zapobiegajac podmianie
bajtow elementu przez atakujacego przy ponownym uzyciu oryginalnego
value_hash. Puste drzewa nie sa uaktualniane, poniewaz nie maja potomnego
merk dostarczajacego hasz korzenia.

> **Uwaga bezpieczenstwa dotyczaca feature_type:** Dla zwyklych (nie-provable)
> drzew, pole `feature_type` w `KVValueHashFeatureType` i
> `KVValueHashFeatureTypeWithChildHash` jest dekodowane, ale **nie uzywane**
> do obliczania hasza ani zwracane do wywolujacych. Kanoniczny typ drzewa
> znajduje sie w bajtach Element weryfikowanych haszem. To pole ma znaczenie
> tylko dla ProvableCountTree (patrz ponizej), gdzie przenosi licznik
> potrzebny do `node_hash_with_count`.

### ProvableCountTree i ProvableCountSumTree

Te typy drzew uzywaja `node_hash_with_count(kv_hash, left, right, count)`
zamiast `node_hash`. **Licznik** jest wlaczony w hasz, wiec weryfikator
potrzebuje licznika dla kazdego wezla, aby przeliczyc korzen Merkle.

| Rola | Typ wezla V0 | Typ wezla V1 | Funkcja haszujaca |
|------|-------------|-------------|---------------|
| Odpytywany element | `KVCount` | `KVCount` | `node_hash_with_count(kv_hash(key, H(value)), left, right, count)` |
| Odpytywane niepuste drzewo (bez subquery) | `KVValueHashFeatureType` | `KVValueHashFeatureTypeWithChildHash` | `node_hash_with_count(kv_hash(key, value_hash), left, right, feature_type.count())` |
| Odpytywane puste drzewo | `KVValueHashFeatureType` | `KVValueHashFeatureType` | `node_hash_with_count(kv_hash(key, value_hash), left, right, feature_type.count())` |
| Odpytywana referencja | `KVRefValueHashCount` | `KVRefValueHashCount` | `node_hash_with_count(kv_hash(key, combine_hash(...)), left, right, count)` |
| Na sciezce | `KVHashCount` | `KVHashCount` | `node_hash_with_count(kv_hash, left, right, count)` |
| Graniczny | `KVDigestCount` | `KVDigestCount` | `node_hash_with_count(kv_hash(key, value_hash), left, right, count)` |
| Odlegly | `Hash` | `Hash` | Uzywany bezposrednio |

> **Niepuste drzewa Z subquery** schodza do warstwy potomnej, tak samo jak
> zwykle drzewa.

> **Dlaczego kazdy wezel niesie licznik?** Poniewaz uzywa sie
> `node_hash_with_count` zamiast `node_hash`. Bez licznika weryfikator nie
> moze odtworzyc zadnego posredniego hasza na sciezce do korzenia — nawet
> dla nie-odpytywanych wezlow.

**Ulepszenie V1:** Tak samo jak dla zwyklych drzew — odpytywane niepuste
drzewa bez subquery sa uaktualniane do
`KVValueHashFeatureTypeWithChildHash` w celu weryfikacji `combine_hash`.

> **Uwaga o ProvableCountSumTree:** Tylko **licznik** jest wlaczony w hasz.
> Suma jest przenoszona w feature_type (`ProvableCountedSummedMerkNode(count,
> sum)`), ale **nie jest haszowana**. Podobnie jak powyzsze zwykle typy drzew,
> kanoniczna wartosc sumy znajduje sie w zserializowanych bajtach nadrzednego
> Element (np. `Element::ProvableCountSumTree(root_key, count, sum, flags)`),
> ktore sa weryfikowane haszem w dowodzie drzewa nadrzednego.

### Podsumowanie: Macierz typ wezla → typ drzewa

| Typ wezla | Zwykle drzewa | Drzewa ProvableCount |
|-----------|:------------:|:-------------------:|
| `KV` | Odpytywane elementy | — |
| `KVCount` | — | Odpytywane elementy |
| `KVValueHash` | Odpytywane poddrzewa | — |
| `KVValueHashFeatureType` | — | Odpytywane poddrzewa |
| `KVRefValueHash` | Odpytywane referencje | — |
| `KVRefValueHashCount` | — | Odpytywane referencje |
| `KVHash` | Na sciezce | — |
| `KVHashCount` | — | Na sciezce |
| `KVDigest` | Granica/nieobecnosc | — |
| `KVDigestCount` | — | Granica/nieobecnosc |
| `Hash` | Odlegle rodzenstwo | Odlegle rodzenstwo |
| `KVValueHashFeatureTypeWithChildHash` | — | Niepuste drzewa bez subquery |

## Generowanie dowodow wielowarstwowych

Poniewaz GroveDB jest drzewem drzew, dowody obejmuja wiele warstw. Kazda warstwa
dowodzi odpowiednia czesc jednego drzewa Merk, a warstwy sa polaczone mechanizmem
combined value_hash:

**Zapytanie:** `Pobierz ["identities", "alice", "name"]`

```mermaid
graph TD
    subgraph layer0["WARSTWA 0: Dowod drzewa korzeniowego"]
        L0["Dowodzi, ze &quot;identities&quot; istnieje<br/>Node: KVValueHash<br/>value_hash = combine_hash(<br/>  H(tree_element_bytes),<br/>  identities_root_hash<br/>)"]
    end

    subgraph layer1["WARSTWA 1: Dowod drzewa identities"]
        L1["Dowodzi, ze &quot;alice&quot; istnieje<br/>Node: KVValueHash<br/>value_hash = combine_hash(<br/>  H(tree_element_bytes),<br/>  alice_root_hash<br/>)"]
    end

    subgraph layer2["WARSTWA 2: Dowod poddrzewa Alice"]
        L2["Dowodzi, ze &quot;name&quot; = &quot;Alice&quot;<br/>Node: KV (pelny klucz + wartosc)<br/>Wynik: <b>&quot;Alice&quot;</b>"]
    end

    state_root["Znany korzen stanu"] -->|"weryfikuj"| L0
    L0 -->|"identities_root_hash<br/>musi pasowac"| L1
    L1 -->|"alice_root_hash<br/>musi pasowac"| L2

    style layer0 fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style layer1 fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style layer2 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style state_root fill:#2c3e50,stroke:#2c3e50,color:#fff
```

> **Lancuch zaufania:** `znany_korzen_stanu → weryfikuj Warstwe 0 → weryfikuj Warstwe 1 → weryfikuj Warstwe 2 → "Alice"`. Odtworzony hasz korzenia kazdej warstwy musi odpowiadac value_hash z warstwy powyzej.

Weryfikator sprawdza kazda warstwe, potwierdzajac, ze:
1. Dowod warstwy rekonstruuje sie do oczekiwanego hasza korzenia
2. Hasz korzenia odpowiada value_hash z warstwy nadrzednej
3. Hasz korzenia najwyzszego poziomu odpowiada znanemu korzeniowi stanu

## Weryfikacja dowodow

Weryfikacja podaza za warstwami dowodu od dolu do gory lub od gory do dolu,
uzywajac funkcji `execute` do rekonstrukcji drzewa kazdej warstwy. Metoda
`Tree::hash()` w drzewie dowodowym oblicza hasz na podstawie typu wezla:

```rust
impl Tree {
    pub fn hash(&self) -> CostContext<CryptoHash> {
        match &self.node {
            Node::Hash(hash) => *hash,  // Juz hasz, zwroc bezposrednio

            Node::KVHash(kv_hash) =>
                node_hash(kv_hash, &self.child_hash(true), &self.child_hash(false)),

            Node::KV(key, value) =>
                kv_hash(key, value)
                    .flat_map(|kv_hash| node_hash(&kv_hash, &left, &right)),

            Node::KVValueHash(key, _, value_hash) =>
                kv_digest_to_kv_hash(key, value_hash)
                    .flat_map(|kv_hash| node_hash(&kv_hash, &left, &right)),

            Node::KVValueHashFeatureType(key, _, value_hash, feature_type) => {
                let kv = kv_digest_to_kv_hash(key, value_hash);
                match feature_type {
                    ProvableCountedMerkNode(count) =>
                        node_hash_with_count(&kv, &left, &right, *count),
                    _ => node_hash(&kv, &left, &right),
                }
            }

            Node::KVRefValueHash(key, referenced_value, ref_element_hash) => {
                let ref_value_hash = value_hash(referenced_value);
                let combined = combine_hash(ref_element_hash, &ref_value_hash);
                let kv = kv_digest_to_kv_hash(key, &combined);
                node_hash(&kv, &left, &right)
            }
            // ... inne warianty
        }
    }
}
```

## Dowody nieobecnosci

GroveDB moze udowodnic, ze klucz **nie istnieje**. Uzywa do tego wezlow granicznych --
wezlow, ktore bylyby sasiadami brakujacego klucza, gdyby istnial:

**Udowodnij:** "charlie" NIE istnieje

```mermaid
graph TD
    dave_abs["dave<br/><b>KVDigest</b><br/>(prawa granica)"]
    bob_abs["bob"]
    frank_abs["frank<br/>Hash"]
    alice_abs["alice<br/>Hash"]
    carol_abs["carol<br/><b>KVDigest</b><br/>(lewa granica)"]
    missing["(brak prawego potomka!)<br/>&quot;charlie&quot; byloby tutaj"]

    dave_abs --> bob_abs
    dave_abs --> frank_abs
    bob_abs --> alice_abs
    bob_abs --> carol_abs
    carol_abs -.->|"right = None"| missing

    style carol_abs fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style dave_abs fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style missing fill:none,stroke:#e74c3c,stroke-dasharray:5 5
    style alice_abs fill:#e8e8e8,stroke:#999
    style frank_abs fill:#e8e8e8,stroke:#999
```

> **Wyszukiwanie binarne:** alice < bob < carol < **"charlie"** < dave < frank. "charlie" znajdowalby sie miedzy carol a dave. Prawy potomek carol to `None`, co dowodzi, ze nic nie istnieje miedzy carol a dave. Zatem "charlie" nie moze istniec w tym drzewie.

Dla zapytan zakresowych dowody nieobecnosci pokazuja, ze nie ma kluczy w
odpytywanym zakresie, ktore nie zostaly uwzglednione w zbiorze wynikow.

## Wykrywanie kluczy granicznych

Podczas weryfikacji dowodu z zapytania o zakres wylaczajacy moze byc konieczne
potwierdzenie, ze konkretny klucz istnieje jako **element graniczny** — klucz,
ktory zakotwicza zakres, ale nie jest czescia zbioru wynikow.

Na przyklad, przy `RangeAfter(10)` (wszystkie klucze scisle po 10), dowod
zawiera klucz 10 jako wezel `KVDigest`. Dowodzi to, ze klucz 10 istnieje w
drzewie i zakotwicza poczatek zakresu, ale klucz 10 nie jest zwracany w
wynikach.

### Kiedy pojawiaja sie wezly graniczne

Graniczne wezly `KVDigest` (lub `KVDigestCount` dla ProvableCountTree) pojawiaja
sie w dowodach dla typow zapytan o zakres wylaczajacy:

| Typ zapytania | Klucz graniczny | Co dowodzi |
|------------|-------------|----------------|
| `RangeAfter(start..)` | `start` | Wylaczajacy poczatek istnieje w drzewie |
| `RangeAfterTo(start..end)` | `start` | Wylaczajacy poczatek istnieje w drzewie |
| `RangeAfterToInclusive(start..=end)` | `start` | Wylaczajacy poczatek istnieje w drzewie |

Wezly graniczne pojawiaja sie rowniez w dowodach nieobecnosci, gdzie sasiednie
klucze dowodza istnienia luki (patrz [Dowody nieobecnosci](#dowody-nieobecnosci)
powyzej).

### Sprawdzanie kluczy granicznych

Po weryfikacji dowodu mozna sprawdzic, czy klucz istnieje jako element graniczny
za pomoca `key_exists_as_boundary` na zdekodowanym `GroveDBProof`:

```rust
// Dekodowanie i weryfikacja dowodu
let (grovedb_proof, _): (GroveDBProof, _) =
    bincode::decode_from_slice(&proof_bytes, config)?;
let (root_hash, results) = grovedb_proof.verify(&path_query, grove_version)?;

// Sprawdzenie, czy klucz graniczny istnieje w dowodzie
let cursor_exists = grovedb_proof
    .key_exists_as_boundary(&[b"documents", b"notes"], &cursor_key)?;
```

Argument `path` wskazuje, ktora warstwe dowodu sprawdzic (odpowiadajaca
sciezce poddrzewa GroveDB, w ktorym wykonano zapytanie zakresowe), a `key` to
klucz graniczny do wyszukania.

### Praktyczne zastosowanie: weryfikacja paginacji

Jest to szczegolnie przydatne przy **paginacji**. Gdy klient prosi o "nastepne
100 dokumentow po dokumencie X", zapytanie to `RangeAfter(document_X_id)`. Dowod
zwraca dokumenty 101-200, ale klient moze rowniez chciec potwierdzic, ze
dokument X (kursor paginacji) nadal istnieje w drzewie:

- Jesli `key_exists_as_boundary` zwraca `true`, kursor jest prawidlowy — klient
  moze ufac, ze paginacja jest zakotwiczona w rzeczywistym dokumencie.
- Jesli zwraca `false`, dokument kursora mogl zostac usuniety miedzy stronami
  i klient powinien rozwazyc ponowne rozpoczecie paginacji.

> **Wazne:** `key_exists_as_boundary` wykonuje syntaktyczne skanowanie wezlow
> `KVDigest`/`KVDigestCount` w dowodzie. Samo w sobie nie zapewnia gwarancji
> kryptograficznej — zawsze najpierw zweryfikuj dowod wobec zaufanego hasza
> korzenia. Te same typy wezlow pojawiaja sie rowniez w dowodach nieobecnosci,
> wiec wywolujacy powinien interpretowac wynik w kontekscie zapytania, ktore
> wygenerowalo dowod.

Na poziomie merk ta sama weryfikacja jest dostepna przez
`key_exists_as_boundary_in_proof(proof_bytes, key)` do pracy bezposrednio z
surowymi bajtami dowodu merk.

## Dowody V1 -- Drzewa nie-Merk

System dowodow V0 dziala wylacznie z poddrzewami Merk, schodzac warstwa po
warstwie przez hierarchie gaju. Jednak elementy **CommitmentTree**, **MmrTree**,
**BulkAppendTree** i **DenseAppendOnlyFixedSizeTree** przechowuja swoje dane
poza potomnym drzewem Merk. Nie maja potomnego Merk, do ktorego mozna by
zejsc -- ich specyficzny dla typu hasz korzenia plynie jako hasz potomny Merk.

**Format dowodow V1** rozszerza V0, aby obslugiwac te drzewa nie-Merk za pomoca
struktur dowodowych specyficznych dla typu:

```rust
/// Ktory format dowodu uzywa warstwa.
pub enum ProofBytes {
    Merk(Vec<u8>),            // Standardowe operacje dowodu Merk
    MMR(Vec<u8>),             // Dowod czlonkostwa MMR
    BulkAppendTree(Vec<u8>),  // Dowod zakresowy BulkAppendTree
    DenseTree(Vec<u8>),       // Dowod wlaczenia gestego drzewa
    CommitmentTree(Vec<u8>),  // Korzen Sinsemilla (32 bajty) + bajty dowodu BulkAppendTree
}

/// Jedna warstwa dowodu V1.
pub struct LayerProof {
    pub merk_proof: ProofBytes,
    pub lower_layers: BTreeMap<Vec<u8>, LayerProof>,
}
```

**Regula wyboru V0/V1:** Jezeli kazda warstwa w dowodzie jest standardowym drzewem
Merk, `prove_query` produkuje `GroveDBProof::V0` (wstecznie kompatybilny). Jezeli
dowolna warstwa obejmuje MmrTree, BulkAppendTree lub DenseAppendOnlyFixedSizeTree,
produkuje `GroveDBProof::V1`.

### Jak dowody drzew nie-Merk wiaza sie z haszem korzenia

Nadrzedne drzewo Merk dowodzi zserializowanych bajtow elementu przez standardowy
wezel dowodu Merk (`KVValueHash`). Korzen specyficzny dla typu (np. `mmr_root` lub
`state_root`) plynie jako **hasz potomny** Merk -- NIE jest osadzony w bajtach
elementu:

```text
combined_value_hash = combine_hash(
    Blake3(varint(len) || element_bytes),   ← zawiera count, height itp.
    type_specific_root                      ← mmr_root / state_root / dense_root
)
```

Dowod specyficzny dla typu nastepnie dowodzi, ze odpytywane dane sa zgodne
z korzeniem specyficznym dla typu, ktory byl uzywany jako hasz potomny.

### Dowody MMR Tree

Dowod MMR demonstruje, ze konkretne liscie istnieja na znanych pozycjach w MMR
i ze hasz korzenia MMR odpowiada haszowi potomnemu przechowywanego w
nadrzednym wezle Merk:

```rust
pub struct MmrProof {
    pub mmr_size: u64,
    pub proof: MerkleProof,  // ckb_merkle_mountain_range::MerkleProof
    pub leaves: Vec<MmrProofLeaf>,
}

pub struct MmrProofLeaf {
    pub position: u64,       // Pozycja MMR
    pub leaf_index: u64,     // Logiczny indeks liscia
    pub hash: [u8; 32],      // Hasz liscia
    pub value: Vec<u8>,      // Bajty wartosci liscia
}
```

```mermaid
graph TD
    subgraph parent_merk["Nadrzedne Merk (warstwa V0)"]
        elem["&quot;my_mmr&quot;<br/><b>KVValueHash</b><br/>bajty elementu zawieraja mmr_root"]
    end

    subgraph mmr_proof["Dowod MMR (warstwa V1)"]
        peak1["Szczyt 1<br/>hasz"]
        peak2["Szczyt 2<br/>hasz"]
        leaf_a["Lisc 5<br/><b>udowodniony</b><br/>wartosc = 0xABCD"]
        sibling["Rodzenstwo<br/>hasz"]
        peak2 --> leaf_a
        peak2 --> sibling
    end

    elem -->|"mmr_root musi odpowiadac<br/>korzeniowi MMR ze szczytow"| mmr_proof

    style parent_merk fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style mmr_proof fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style leaf_a fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

**Klucze zapytania to pozycje:** Elementy zapytan koduja pozycje jako bajty u64
big-endian (co zachowuje porzadek sortowania). `QueryItem::RangeInclusive` z
pozycjami start/end kodowanymi w BE wybiera ciagly zakres lisci MMR.

**Weryfikacja:**
1. Zrekonstruuj liscie `MmrNode` z dowodu
2. Zweryfikuj `MerkleProof` ckb wobec oczekiwanego korzenia MMR z hasza potomnego nadrzednego Merk
3. Waliduj skrzyzowanie, ze `proof.mmr_size` odpowiada rozmiarowi przechowywanego w elemencie
4. Zwroc udowodnione wartosci lisci

### Dowody BulkAppendTree

Dowody BulkAppendTree sa bardziej zlozone, poniewaz dane znajduja sie w dwoch
miejscach: zapieczętowanych blobach chunkow i w aktywnym buforze. Dowod
zakresowy musi zwrocic:

- **Pelne bloby chunkow** dla kazdego ukonczonego chunka nakladajacego sie z zapytanym zakresem
- **Poszczegolne wpisy bufora** dla pozycji nadal w buforze

```rust
pub struct BulkAppendTreeProof {
    pub chunk_power: u8,
    pub total_count: u64,
    pub chunk_blobs: Vec<(u64, Vec<u8>)>,       // (chunk_index, blob_bytes)
    pub chunk_mmr_size: u64,
    pub chunk_mmr_proof_items: Vec<[u8; 32]>,    // Hasze rodzenstwa MMR
    pub chunk_mmr_leaves: Vec<(u64, [u8; 32])>,  // (mmr_pos, dense_merkle_root)
    pub buffer_entries: Vec<Vec<u8>>,             // WSZYSTKIE biezace wpisy bufora (gestego drzewa)
    pub chunk_mmr_root: [u8; 32],
}
```

```mermaid
graph TD
    subgraph verify["Kroki weryfikacji"]
        step1["1. Dla kazdego bloba chunka:<br/>oblicz dense_merkle_root<br/>zweryfikuj zgodnosc z chunk_mmr_leaves"]
        step2["2. Zweryfikuj dowod MMR chunkow<br/>wobec chunk_mmr_root"]
        step3["3. Przelicz dense_tree_root<br/>ze WSZYSTKICH wpisow bufora<br/>za pomoca gestego drzewa Merkle"]
        step4["4. Zweryfikuj state_root =<br/>blake3(&quot;bulk_state&quot; ||<br/>chunk_mmr_root ||<br/>dense_tree_root)"]
        step5["5. Wyodrebnij wpisy w<br/>odpytywanym zakresie pozycji"]

        step1 --> step2 --> step3 --> step4 --> step5
    end

    style verify fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style step4 fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

> **Dlaczego uwzgledniamy WSZYSTKIE wpisy bufora?** Bufor to geste drzewo Merkle,
> ktorego hasz korzenia zobowiazuje do kazdego wpisu. Aby zweryfikowac `dense_tree_root`,
> weryfikator musi odbudowac drzewo ze wszystkich wpisow. Poniewaz bufor jest ograniczony
> przez `capacity` wpisow (co najwyzej 65 535), jest to akceptowalne.

**Rozliczanie limitow:** Kazda pojedyncza wartosc (wewnatrz chunka lub bufora)
liczy sie do limitu zapytania, nie kazdy blob chunka jako calosc. Jezeli zapytanie
ma `limit: 100`, a chunk zawiera 1024 wpisow z 500 nakladajacymi sie z zakresem,
wszystkie 500 wpisow liczy sie do limitu.

### Dowody DenseAppendOnlyFixedSizeTree

Dowod gestego drzewa demonstruje, ze konkretne pozycje zawieraja konkretne
wartosci, uwierzytelnione wobec hasza korzenia drzewa (ktory plynie jako hasz
potomny Merk). Wszystkie wezly uzywaja `blake3(H(value) || H(left) || H(right))`,
wiec wezly przodkow na sciezce uwierzytelniania potrzebuja tylko swoich
32-bajtowych **haszy wartosci** -- nie pelnych wartosci.

```rust
pub struct DenseTreeProof {
    pub entries: Vec<(u16, Vec<u8>)>,            // udowodnione (pozycja, wartosc)
    pub node_value_hashes: Vec<(u16, [u8; 32])>, // hasze wartosci przodkow na sciezce uwierzytelniania
    pub node_hashes: Vec<(u16, [u8; 32])>,       // wstepnie obliczone hasze poddrzew rodzenstwa
}
```

> `height` i `count` pochodza z nadrzednego elementu (uwierzytelnionego przez hierarchie Merk), nie z dowodu.

```mermaid
graph TD
    subgraph parent_merk["Nadrzedne Merk (warstwa V0)"]
        elem["&quot;my_dense&quot;<br/><b>KVValueHash</b><br/>bajty elementu zawieraja root_hash"]
    end

    subgraph dense_proof["Dowod gestego drzewa (warstwa V1)"]
        root["Pozycja 0<br/>node_value_hashes<br/>H(value[0])"]
        node1["Pozycja 1<br/>node_value_hashes<br/>H(value[1])"]
        hash2["Pozycja 2<br/>node_hashes<br/>H(poddrzewo)"]
        hash3["Pozycja 3<br/>node_hashes<br/>H(wezel)"]
        leaf4["Pozycja 4<br/><b>entries</b><br/>value[4] (udowodniona)"]
        root --> node1
        root --> hash2
        node1 --> hash3
        node1 --> leaf4
    end

    elem -->|"root_hash musi odpowiadac<br/>przeliczonemu H(0)"| dense_proof

    style parent_merk fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style dense_proof fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style leaf4 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

**Weryfikacja** jest czysta funkcja nie wymagajaca magazynu:
1. Zbuduj mapy wyszukiwania z `entries`, `node_value_hashes` i `node_hashes`
2. Rekurencyjnie przelicz hasz korzenia od pozycji 0:
   - Pozycja ma wstepnie obliczony hasz w `node_hashes` → uzyj go bezposrednio
   - Pozycja z wartoscia w `entries` → `blake3(blake3(value) || H(left) || H(right))`
   - Pozycja z haszem w `node_value_hashes` → `blake3(hash || H(left) || H(right))`
   - Pozycja `>= count` lub `>= capacity` → `[0u8; 32]`
3. Porownaj obliczony korzen z oczekiwanym haszem korzenia z nadrzednego elementu
4. Zwroc udowodnione wpisy w przypadku sukcesu

**Dowody wielopozycyjne** scalaja nakladajace sie sciezki uwierzytelniania:
wspolni przodkowie i ich wartosci pojawiaja sie tylko raz, co sprawia, ze sa
bardziej kompaktowe niz niezalezne dowody.

---
