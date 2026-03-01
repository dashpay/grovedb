# Drzewo Merk -- Drzewo Merkle AVL

Drzewo Merk to podstawowy element budulcowy GroveDB. Kazde poddrzewo w gaju
jest drzewem Merk -- samorownowazacym sie binarnym drzewem wyszukiwania, w
ktorym kazdy wezel jest kryptograficznie haszowany, tworzac pojedynczy hasz
korzenia (root hash), ktory uwierzytelnia cala zawartosc drzewa.

## Czym jest wezel Merk?

W przeciwienstwie do wielu implementacji drzew Merkle, w ktorych dane znajduja
sie tylko w lisciach, w drzewie Merk **kazdy wezel przechowuje pare klucz-wartosc**.
Oznacza to, ze nie ma "pustych" wezlow wewnetrznych -- drzewo jest jednoczesnie
struktura wyszukiwania i magazynem danych.

```mermaid
graph TD
    subgraph TreeNode
        subgraph inner["inner: Box&lt;TreeNodeInner&gt;"]
            subgraph kv["kv: KV"]
                KEY["<b>key:</b> Vec&lt;u8&gt;<br/><i>np. b&quot;alice&quot;</i>"]
                VAL["<b>value:</b> Vec&lt;u8&gt;<br/><i>zserializowane bajty Element</i>"]
                FT["<b>feature_type:</b> TreeFeatureType<br/><i>BasicMerkNode | SummedMerkNode(n) | ...</i>"]
                VH["<b>value_hash:</b> [u8; 32]<br/><i>H(varint(value.len) ‖ value)</i>"]
                KVH["<b>hash:</b> [u8; 32] — kv_hash<br/><i>H(varint(key.len) ‖ key ‖ value_hash)</i>"]
            end
            LEFT["<b>left:</b> Option&lt;Link&gt;"]
            RIGHT["<b>right:</b> Option&lt;Link&gt;"]
        end
        OLD["<b>old_value:</b> Option&lt;Vec&lt;u8&gt;&gt; — poprzednia wartosc do obliczania delt kosztow"]
        KNOWN["<b>known_storage_cost:</b> Option&lt;KeyValueStorageCost&gt;"]
    end

    LEFT -->|"mniejsze klucze"| LC["Lewy potomek<br/><i>Link::Reference | Modified<br/>Uncommitted | Loaded</i>"]
    RIGHT -->|"wieksze klucze"| RC["Prawy potomek<br/><i>Link::Reference | Modified<br/>Uncommitted | Loaded</i>"]

    style kv fill:#eaf2f8,stroke:#2980b9
    style inner fill:#fef9e7,stroke:#f39c12
    style TreeNode fill:#f9f9f9,stroke:#333
    style LC fill:#d5f5e3,stroke:#27ae60
    style RC fill:#d5f5e3,stroke:#27ae60
```

W kodzie (`merk/src/tree/mod.rs`):

```rust
pub struct TreeNode {
    pub(crate) inner: Box<TreeNodeInner>,
    pub(crate) old_value: Option<Vec<u8>>,        // Poprzednia wartosc do sledzenia kosztow
    pub(crate) known_storage_cost: Option<KeyValueStorageCost>,
}

pub struct TreeNodeInner {
    pub(crate) left: Option<Link>,    // Lewy potomek (mniejsze klucze)
    pub(crate) right: Option<Link>,   // Prawy potomek (wieksze klucze)
    pub(crate) kv: KV,               // Ladunek klucz-wartosc
}
```

`Box<TreeNodeInner>` utrzymuje wezel na stercie (heap), co jest istotne, poniewaz
linki do potomkow moga rekurencyjnie zawierac calkowite instancje `TreeNode`.

## Struktura KV

Struktura `KV` przechowuje zarowno surowe dane, jak i ich skroty kryptograficzne
(`merk/src/tree/kv.rs`):

```rust
pub struct KV {
    pub(super) key: Vec<u8>,                        // Klucz wyszukiwania
    pub(super) value: Vec<u8>,                      // Przechowywana wartosc
    pub(super) feature_type: TreeFeatureType,       // Zachowanie agregacyjne
    pub(crate) value_defined_cost: Option<ValueDefinedCostType>,
    pub(super) hash: CryptoHash,                    // kv_hash
    pub(super) value_hash: CryptoHash,              // H(value)
}
```

Dwa wazne punkty:

1. **Klucze nie sa przechowywane na dysku jako czesc zakodowanego wezla.** Sa
   przechowywane jako klucz RocksDB. Gdy wezel jest dekodowany z magazynu, klucz
   jest wstrzykiwany z zewnatrz. Pozwala to uniknac duplikowania bajtow klucza.

2. **Utrzymywane sa dwa pola haszy.** `value_hash` to `H(value)`, a `hash`
   (kv_hash) to `H(key, value_hash)`. Przechowywanie obu pozwala systemowi
   dowodow wybrac, ile informacji ujawnic.

## Pol-zrownowazona natura -- Jak AVL sie "chwieje"

Drzewo Merk to **drzewo AVL** -- klasyczne samorownowazace sie binarne drzewo
wyszukiwania wynalezione przez Adelsona-Velsky'ego i Landisa. Kluczowy niezmiennik to:

> Dla kazdego wezla roznica wysokosci miedzy jego lewym a prawym poddrzewem
> wynosi co najwyzej 1.

Wyrazane jest to jako **wspolczynnik rownowagi** (balance factor):

```text
balance_factor = prawa_wysokosc - lewa_wysokosc
```

Poprawne wartosci: **{-1, 0, 1}**

```rust
// merk/src/tree/mod.rs
pub const fn balance_factor(&self) -> i8 {
    let left_height = self.child_height(true) as i8;
    let right_height = self.child_height(false) as i8;
    right_height - left_height
}
```

Jest tu jednak subtelna kwestia: chociaz kazdy pojedynczy wezel moze przechylac sie
tylko o jeden poziom, te przechylenia moga sie **kumulowac** w calosci drzewa.
Dlatego nazywamy je "pol-zrownowazonym" -- drzewo nie jest idealnie zrownowazone
jak pelne drzewo binarne.

Rozwazmy drzewo o 10 wezlach. Idealnie zrownowazone drzewo mialoby wysokosc 4
(ceil(log2(10+1))). Ale drzewo AVL moze miec wysokosc 5:

**Idealnie zrownowazone (wysokosc 4)** -- kazdy poziom jest calkowicie zapelniony:

```mermaid
graph TD
    N5["5<br/><small>bf=0</small>"]
    N3["3<br/><small>bf=0</small>"]
    N8["8<br/><small>bf=0</small>"]
    N2["2<br/><small>bf=0</small>"]
    N4["4<br/><small>bf=0</small>"]
    N6["6<br/><small>bf=0</small>"]
    N9["9<br/><small>bf=+1</small>"]
    N10["10<br/><small>bf=0</small>"]

    N5 --- N3
    N5 --- N8
    N3 --- N2
    N3 --- N4
    N8 --- N6
    N8 --- N9
    N9 --- N10

    style N5 fill:#d4e6f1,stroke:#2980b9
```

**Poprawne "chwiejne" drzewo AVL (wysokosc 5)** -- kazdy wezel przechyla sie co najwyzej o 1, ale kumuluje sie:

```mermaid
graph TD
    N4["4<br/><small>bf=+1</small>"]
    N2["2<br/><small>bf=-1</small>"]
    N7["7<br/><small>bf=+1</small>"]
    N1["1<br/><small>bf=-1</small>"]
    N3["3<br/><small>bf=0</small>"]
    N5["5<br/><small>bf=0</small>"]
    N9["9<br/><small>bf=-1</small>"]
    N0["0<br/><small>bf=0</small>"]
    N8["8<br/><small>bf=0</small>"]
    N10["10<br/><small>bf=0</small>"]

    N4 --- N2
    N4 --- N7
    N2 --- N1
    N2 --- N3
    N7 --- N5
    N7 --- N9
    N1 --- N0
    N9 --- N8
    N9 --- N10

    style N4 fill:#fadbd8,stroke:#e74c3c
```

> Wysokosc 5 zamiast idealnej 4 -- to jest wlasnie to "chwiejnosc". Najgorszy przypadek: h <= 1,44 * log2(n+2).

Oba drzewa sa poprawnymi drzewami AVL! Najgorsza mozliwa wysokosc drzewa AVL wynosi:

```text
h <= 1,4404 * log2(n + 2) - 0,3277
```

Wiec dla **n = 1 000 000** wezlow:
- Idealna rownowaga: wysokosc 20
- Najgorszy przypadek AVL: wysokosc ok. 29

Ten ok. 44% narzut to cena prostych regul rotacji AVL. W praktyce losowe
wstawienia daja drzewa znacznie blizsze idealnej rownowadze.

Oto jak wygladaja poprawne i niepoprawne drzewa:

**POPRAWNE** -- wszystkie wspolczynniki rownowagi naleza do {-1, 0, +1}:

```mermaid
graph TD
    subgraph balanced["Zrownowazone (bf=0)"]
        D1["D<br/>bf=0"] --- B1["B<br/>bf=0"]
        D1 --- F1["F<br/>bf=0"]
        B1 --- A1["A"] & C1["C"]
        F1 --- E1["E"] & G1["G"]
    end
    subgraph rightlean["Przechylone w prawo (bf=+1)"]
        D2["D<br/>bf=+1"] --- B2["B<br/>bf=0"]
        D2 --- F2["F<br/>bf=0"]
        B2 --- A2["A"] & C2["C"]
        F2 --- E2["E"] & G2["G"]
    end
    subgraph leftlean["Przechylone w lewo (bf=-1)"]
        D3["D<br/>bf=-1"] --- B3["B<br/>bf=-1"]
        D3 --- E3["E"]
        B3 --- A3["A"]
    end

    style balanced fill:#d5f5e3,stroke:#27ae60
    style rightlean fill:#d5f5e3,stroke:#27ae60
    style leftlean fill:#d5f5e3,stroke:#27ae60
```

**NIEPOPRAWNE** -- wspolczynnik rownowagi = +2 (potrzebna rotacja!):

```mermaid
graph TD
    B["B<br/><b>bf=+2 ✗</b>"]
    D["D<br/>bf=+1"]
    F["F<br/>bf=0"]
    B --- D
    D --- F

    style B fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
```

> Prawe poddrzewo jest o 2 poziomy wyzsze niz lewe (ktore jest puste). Wyzwala to **rotacje w lewo** w celu przywrocenia niezmiennika AVL.

## Rotacje -- Przywracanie rownowagi

Gdy wstawianie lub usuwanie powoduje, ze wspolczynnik rownowagi osiaga +/-2,
drzewo musi zostac **obrocene** w celu przywrocenia niezmiennika AVL. Istnieja
cztery przypadki, sprowadzalne do dwoch podstawowych operacji.

### Pojedyncza rotacja w lewo

Uzywana, gdy wezel jest **przechylony w prawo** (bf = +2), a jego prawy potomek
jest **przechylony w prawo lub zrownowazony** (bf >= 0):

**Przed** (bf=+2):

```mermaid
graph TD
    A["A<br/><small>bf=+2</small>"]
    t1["t₁"]
    B["B<br/><small>bf≥0</small>"]
    X["X"]
    C["C"]
    A --- t1
    A --- B
    B --- X
    B --- C
    style A fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
```

**Po** rotacji w lewo -- B awansuje na korzen:

```mermaid
graph TD
    B2["B<br/><small>bf=0</small>"]
    A2["A"]
    C2["C"]
    t12["t₁"]
    X2["X"]
    B2 --- A2
    B2 --- C2
    A2 --- t12
    A2 --- X2
    style B2 fill:#d5f5e3,stroke:#27ae60,stroke-width:3px
```

> **Kroki:** (1) Odlacz B od A. (2) Odlacz X (lewy potomek B). (3) Podlacz X jako prawy potomek A. (4) Podlacz A jako lewy potomek B. Poddrzewo z korzeniem w B jest teraz zrownowazone.

W kodzie (`merk/src/tree/ops.rs`):

```rust
fn rotate<V>(self, left: bool, ...) -> CostResult<Self, Error> {
    // Odlacz potomka po ciezszej stronie
    let (tree, child) = self.detach_expect(left, ...);
    // Odlacz wnuka z przeciwnej strony potomka
    let (child, maybe_grandchild) = child.detach(!left, ...);

    // Podlacz wnuka do pierwotnego korzenia
    tree.attach(left, maybe_grandchild)
        .maybe_balance(...)
        .flat_map_ok(|tree| {
            // Podlacz pierwotny korzen jako potomka awansowanego wezla
            child.attach(!left, Some(tree))
                .maybe_balance(...)
        })
}
```

Zwroc uwage, ze `maybe_balance` jest wywolywana rekurencyjnie -- sama rotacja
moze stworzyc nowe nierownowazone sytuacje wymagajace dalszej korekcji.

### Podwojna rotacja (lewo-prawo)

Uzywana, gdy wezel jest **przechylony w lewo** (bf = -2), ale jego lewy potomek
jest **przechylony w prawo** (bf > 0). Pojedyncza rotacja tego nie naprawi:

**Krok 0: Przed** -- C jest przechylony w lewo (bf=-2), ale jego lewy potomek A przechyla sie w prawo (bf=+1). Pojedyncza rotacja tego nie naprawi:

```mermaid
graph TD
    C0["C<br/><small>bf=-2</small>"]
    A0["A<br/><small>bf=+1</small>"]
    t4["t₄"]
    t1["t₁"]
    B0["B"]
    t2["t₂"]
    t3["t₃"]
    C0 --- A0
    C0 --- t4
    A0 --- t1
    A0 --- B0
    B0 --- t2
    B0 --- t3
    style C0 fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
```

**Krok 1: Rotacja w lewo potomka A** -- teraz zarowno C, jak i B przechylaja sie w lewo, co mozna naprawic pojedyncza rotacja:

```mermaid
graph TD
    C1["C<br/><small>bf=-2</small>"]
    B1["B"]
    t41["t₄"]
    A1["A"]
    t31["t₃"]
    t11["t₁"]
    t21["t₂"]
    C1 --- B1
    C1 --- t41
    B1 --- A1
    B1 --- t31
    A1 --- t11
    A1 --- t21
    style C1 fill:#fdebd0,stroke:#e67e22,stroke-width:2px
```

**Krok 2: Rotacja w prawo korzenia C** -- zrownowazone!

```mermaid
graph TD
    B2["B<br/><small>bf=0</small>"]
    A2["A"]
    C2["C"]
    t12["t₁"]
    t22["t₂"]
    t32["t₃"]
    t42["t₄"]
    B2 --- A2
    B2 --- C2
    A2 --- t12
    A2 --- t22
    C2 --- t32
    C2 --- t42
    style B2 fill:#d5f5e3,stroke:#27ae60,stroke-width:3px
```

Algorytm wykrywa ten przypadek porownujac kierunek przechylenia rodzica ze
wspolczynnikiem rownowagi potomka:

```rust
fn maybe_balance<V>(self, ...) -> CostResult<Self, Error> {
    let balance_factor = self.balance_factor();
    if balance_factor.abs() <= 1 {
        return Ok(self);  // Juz zrownowazone
    }

    let left = balance_factor < 0;  // true jezeli przechylone w lewo

    // Podwojna rotacja potrzebna, gdy potomek przechyla sie w przeciwna strone niz rodzic
    let tree = if left == (self.tree().link(left).unwrap().balance_factor() > 0) {
        // Pierwsza rotacja: obroc potomka w przeciwnym kierunku
        self.walk_expect(left, |child|
            child.rotate(!left, ...).map_ok(Some), ...
        )
    } else {
        self
    };

    // Druga (lub jedyna) rotacja
    tree.rotate(left, ...)
}
```

## Operacje wsadowe -- Budowanie i stosowanie

Zamiast wstawiac elementy jeden po drugim, Merk obsluguje operacje wsadowe (batch),
ktore stosuja wiele zmian w jednym przebiegu. Jest to kluczowe dla wydajnosci:
partia N operacji na drzewie o M elementach zajmuje **O((M + N) log(M + N))** czasu,
w porownaniu z O(N log M) dla sekwencyjnych wstawien.

### Typ MerkBatch

```rust
type MerkBatch<K> = [(K, Op)];

enum Op {
    Put(Vec<u8>, TreeFeatureType),  // Wstawianie lub aktualizacja z wartoscia i typem cechy
    PutWithSpecializedCost(...),     // Wstawianie z predefiniowanym kosztem
    PutCombinedReference(...),       // Wstawianie referencji z polaczonym haszem
    Replace(Vec<u8>, TreeFeatureType),
    Patch { .. },                    // Czesciowa aktualizacja wartosci
    Delete,                          // Usuwanie klucza
    DeleteLayered,                   // Usuwanie z kosztem warstwowym
    DeleteMaybeSpecialized,          // Usuwanie z opcjonalnym kosztem specjalistycznym
}
```

### Strategia 1: build() -- Budowanie od zera

Gdy drzewo jest puste, `build()` konstruuje zrownowazone drzewo bezposrednio
z posortowanej partii za pomoca algorytmu **podzial po medianie**:

Wsadowe dane wejsciowe (posortowane): `[A, B, C, D, E, F, G]` -- wybierz srodkowy (D) jako korzen, rekurencja na kazdej polowie:

```mermaid
graph TD
    D["<b>D</b><br/><small>korzen = mid(0..6)</small>"]
    B["<b>B</b><br/><small>mid(A,B,C)</small>"]
    F["<b>F</b><br/><small>mid(E,F,G)</small>"]
    A["A"]
    C["C"]
    E["E"]
    G["G"]

    D --- B
    D --- F
    B --- A
    B --- C
    F --- E
    F --- G

    style D fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style B fill:#d5f5e3,stroke:#27ae60
    style F fill:#d5f5e3,stroke:#27ae60
```

> Wynik: idealnie zrownowazone drzewo o wysokosci = 3 = ceil(log2(7)).

```rust
fn build(batch: &MerkBatch<K>, ...) -> CostResult<Option<TreeNode>, Error> {
    let mid_index = batch.len() / 2;
    let (mid_key, mid_op) = &batch[mid_index];

    // Utworz wezel korzenia ze srodkowego elementu
    let mid_tree = TreeNode::new(mid_key.clone(), value.clone(), None, feature_type)?;

    // Rekurencyjnie buduj lewe i prawe poddrzewa
    let left = Self::build(&batch[..mid_index], ...);
    let right = Self::build(&batch[mid_index + 1..], ...);

    // Podlacz potomkow
    mid_tree.attach(true, left).attach(false, right)
}
```

Produkuje to drzewo o wysokosci ceil(log2(n)) -- idealnie zrownowazone.

### Strategia 2: apply_sorted() -- Scalanie z istniejacym drzewem

Gdy drzewo juz zawiera dane, `apply_sorted()` uzywa **wyszukiwania binarnego**,
aby znalezc, gdzie kazda operacja wsadowa powinna zostac zastosowana, a nastepnie
rekurencyjnie stosuje operacje do lewego i prawego poddrzewa:

Istniejace drzewo z partia `[(B, Put), (F, Delete)]`:

Wyszukiwanie binarne: B < D (idz w lewo), F > D (idz w prawo).

**Przed:**
```mermaid
graph TD
    D0["D"] --- C0["C"]
    D0 --- E0["E"]
    E0 --- F0["F"]
    style D0 fill:#d4e6f1,stroke:#2980b9
```

**Po** zastosowaniu partii i rebalansowaniu:
```mermaid
graph TD
    D1["D"] --- B1["B"]
    D1 --- E1["E"]
    B1 --- C1["C"]
    style D1 fill:#d5f5e3,stroke:#27ae60
```

> B wstawione jako lewe poddrzewo, F usuniete z prawego poddrzewa. `maybe_balance()` potwierdza bf(D) = 0.

```rust
fn apply_sorted(self, batch: &MerkBatch<K>, ...) -> CostResult<...> {
    let search = batch.binary_search_by(|(key, _)| key.cmp(self.tree().key()));

    match search {
        Ok(index) => {
            // Klucz pasuje do tego wezla -- zastosuj operacje bezposrednio
            // (Put zastepuje wartosc, Delete usuwa wezel)
        }
        Err(mid) => {
            // Klucz nie znaleziony -- mid jest punktem podzialu
            // Rekurencja na left_batch[..mid] i right_batch[mid..]
        }
    }

    self.recurse(batch, mid, exclusive, ...)
}
```

Metoda `recurse` dzieli partie i przechodzi w lewo i w prawo:

```rust
fn recurse(self, batch: &MerkBatch<K>, mid: usize, ...) {
    let left_batch = &batch[..mid];
    let right_batch = &batch[mid..];  // lub mid+1 jezeli wylaczne

    // Zastosuj lewa partie do lewego poddrzewa
    let tree = self.walk(true, |maybe_left| {
        Self::apply_to(maybe_left, left_batch, ...)
    });

    // Zastosuj prawa partie do prawego poddrzewa
    let tree = tree.walk(false, |maybe_right| {
        Self::apply_to(maybe_right, right_batch, ...)
    });

    // Ponowne zrownowazenie po modyfikacjach
    tree.maybe_balance(...)
}
```

### Usuwanie wezla

Podczas usuwania wezla z dwoma potomkami, Merk awansuje **wezel brzegowy** z
wyzszego poddrzewa. Minimalizuje to ryzyko koniecznosci dodatkowych rotacji:

**Przed** -- usuwanie D (ma dwoje potomkow, wysokosc prawego poddrzewa >= lewego):

```mermaid
graph TD
    D["D ✗ usun"]
    B0["B"]
    F0["F"]
    A0["A"]
    C0["C"]
    E0["E ← nastepnik"]
    G0["G"]
    D --- B0
    D --- F0
    B0 --- A0
    B0 --- C0
    F0 --- E0
    F0 --- G0
    style D fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style E0 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

**Po** -- E (najbardziej lewy w prawym poddrzewie = nastepnik w porzadku inorder) awansuje na pozycje D:

```mermaid
graph TD
    E1["E"]
    B1["B"]
    F1["F"]
    A1["A"]
    C1["C"]
    G1["G"]
    E1 --- B1
    E1 --- F1
    B1 --- A1
    B1 --- C1
    F1 --- G1
    style E1 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

> **Regula:** Jezeli lewa_wysokosc > prawa --> awansuj prawy brzeg lewego poddrzewa. Jezeli prawa_wysokosc >= lewa --> awansuj lewy brzeg prawego poddrzewa. Minimalizuje to rebalansowanie po usunieciu.

```rust
pub fn remove(self, ...) -> CostResult<Option<Self>, Error> {
    let has_left = tree.link(true).is_some();
    let has_right = tree.link(false).is_some();
    let left = tree.child_height(true) > tree.child_height(false);

    if has_left && has_right {
        // Dwoje potomkow: awansuj brzeg wyzszego potomka
        let (tree, tall_child) = self.detach_expect(left, ...);
        let (_, short_child) = tree.detach_expect(!left, ...);
        tall_child.promote_edge(!left, short_child, ...)
    } else if has_left || has_right {
        // Jeden potomek: awansuj go bezposrednio
        self.detach_expect(left, ...).1
    } else {
        // Wezel liscia: po prostu usun
        None
    }
}
```

---
