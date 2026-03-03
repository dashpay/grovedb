# Strom Merk -- Merkluv AVL strom

Strom Merk je zakladnim stavebnim kamenem GroveDB. Kazdy podstrom v haji
je strom Merk -- samobalancujici binarni vyhledavaci strom, kde je kazdy uzel
kryptograficky zahasovan, coz produkuje jediny korenovy hash (root hash),
ktery autentizuje veskery obsah stromu.

## Co je uzel Merk?

Na rozdil od mnoha implementaci Merklovych stromu, kde data ziji pouze v listech,
ve stromu Merk **kazdy uzel uklada par klic-hodnota**. To znamena, ze neexistuji
"prazdne" vnitrni uzly -- strom je zaroven vyhledavaci strukturou i ulozstem dat.

```mermaid
graph TD
    subgraph TreeNode
        subgraph inner["inner: Box&lt;TreeNodeInner&gt;"]
            subgraph kv["kv: KV"]
                KEY["<b>key:</b> Vec&lt;u8&gt;<br/><i>napr. b&quot;alice&quot;</i>"]
                VAL["<b>value:</b> Vec&lt;u8&gt;<br/><i>serializovane bajty elementu</i>"]
                FT["<b>feature_type:</b> TreeFeatureType<br/><i>BasicMerkNode | SummedMerkNode(n) | ...</i>"]
                VH["<b>value_hash:</b> [u8; 32]<br/><i>H(varint(value.len) ‖ value)</i>"]
                KVH["<b>hash:</b> [u8; 32] — kv_hash<br/><i>H(varint(key.len) ‖ key ‖ value_hash)</i>"]
            end
            LEFT["<b>left:</b> Option&lt;Link&gt;"]
            RIGHT["<b>right:</b> Option&lt;Link&gt;"]
        end
        OLD["<b>old_value:</b> Option&lt;Vec&lt;u8&gt;&gt; — predchozi hodnota pro delty nakladu"]
        KNOWN["<b>known_storage_cost:</b> Option&lt;KeyValueStorageCost&gt;"]
    end

    LEFT -->|"mensi klice"| LC["Levy potomek<br/><i>Link::Reference | Modified<br/>Uncommitted | Loaded</i>"]
    RIGHT -->|"vetsi klice"| RC["Pravy potomek<br/><i>Link::Reference | Modified<br/>Uncommitted | Loaded</i>"]

    style kv fill:#eaf2f8,stroke:#2980b9
    style inner fill:#fef9e7,stroke:#f39c12
    style TreeNode fill:#f9f9f9,stroke:#333
    style LC fill:#d5f5e3,stroke:#27ae60
    style RC fill:#d5f5e3,stroke:#27ae60
```

V kodu (`merk/src/tree/mod.rs`):

```rust
pub struct TreeNode {
    pub(crate) inner: Box<TreeNodeInner>,
    pub(crate) old_value: Option<Vec<u8>>,        // Predchozi hodnota pro sledovani nakladu
    pub(crate) known_storage_cost: Option<KeyValueStorageCost>,
}

pub struct TreeNodeInner {
    pub(crate) left: Option<Link>,    // Levy potomek (mensi klice)
    pub(crate) right: Option<Link>,   // Pravy potomek (vetsi klice)
    pub(crate) kv: KV,               // Datovy obsah klice a hodnoty
}
```

`Box<TreeNodeInner>` drzi uzel na halde, coz je nezbytne, protoze odkazy na potomky
mohou rekurzivne obsahovat cele instance `TreeNode`.

## Struktura KV

Struktura `KV` drzi surova data i jejich kryptograficke otiske
(`merk/src/tree/kv.rs`):

```rust
pub struct KV {
    pub(super) key: Vec<u8>,                        // Vyhledavaci klic
    pub(super) value: Vec<u8>,                      // Ulozena hodnota
    pub(super) feature_type: TreeFeatureType,       // Agregacni chovani
    pub(crate) value_defined_cost: Option<ValueDefinedCostType>,
    pub(super) hash: CryptoHash,                    // kv_hash
    pub(super) value_hash: CryptoHash,              // H(value)
}
```

Dva dulezite body:

1. **Klice se neukladaji na disk jako soucast zakodovaneho uzlu.** Ukladaji se
   jako klic v RocksDB. Kdyz je uzel dekodovan z uloziste, klic je vlozen
   zvenku. Timto se zabranuje duplicite bajtu klice.

2. **Udrzuji se dve hashovaci pole.** `value_hash` je `H(value)` a
   `hash` (kv_hash) je `H(key, value_hash)`. Udrzovani obou umoznuje systemu
   dukazu zvolit, kolik informaci odhalit.

## Polobalancovana povaha -- Jak AVL "kymaci"

Strom Merk je **strom AVL** -- klasicky samobalancujici binarni vyhledavaci strom
vynalezeny Adelsonem-Velskym a Landisem. Klicovy invariant je:

> Pro kazdy uzel je rozdil vysek mezi jeho levym a pravym podstromem
> nejvyse 1.

To se vyjadruje jako **faktor rovnovahy** (balance factor):

```text
balance_factor = prava_vyska - leva_vyska
```

Platne hodnoty: **{-1, 0, 1}**

```rust
// merk/src/tree/mod.rs
pub const fn balance_factor(&self) -> i8 {
    let left_height = self.child_height(true) as i8;
    let right_height = self.child_height(false) as i8;
    right_height - left_height
}
```

Ale zde je subtilni bod: zatimco kazdy jednotlivy uzel se muze naklonit pouze
o jednu uroven, tyto naklony se mohou **kumulovat** skrze strom. Proto ho nazyvame
"polobalancovany" -- strom neni dokonale vyvazeny jako uplny binarni strom.

Uvazujme strom s 10 uzly. Dokonale vyvazeny strom by mel vysku 4
(ceil(log2(10+1))). Ale AVL strom muze mit vysku 5:

**Dokonale vyvazeny (vyska 4)** -- kazda uroven je plne obsazena:

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

**Platne AVL "kymitnuti" (vyska 5)** -- kazdy uzel se naklani maximalne o 1, ale kumuluje se to:

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

> Vyska 5 oproti dokonalym 4 -- to je to "kymitnuti". Nejhorsi pripad: h <= 1,44 * log2(n+2).

Oba stromy jsou platne stromy AVL! Nejhorsi pripad vysky stromu AVL je:

```text
h <= 1,4404 * log2(n + 2) - 0,3277
```

Takze pro **n = 1 000 000** uzlu:
- Dokonale vyvazeni: vyska 20
- Nejhorsi pripad AVL: vyska priblizne 29

Tato priblizne 44% rezie je cenou za jednoducha rotacni pravidla AVL. V praxi
nahodne vlozeni produkuji stromy mnohem blize dokonalemu vyvazeni.

Takto vypadaji platne a neplatne stromy:

**PLATNY** -- vsechny faktory rovnovahy v {-1, 0, +1}:

```mermaid
graph TD
    subgraph balanced["Vyvazeny (bf=0)"]
        D1["D<br/>bf=0"] --- B1["B<br/>bf=0"]
        D1 --- F1["F<br/>bf=0"]
        B1 --- A1["A"] & C1["C"]
        F1 --- E1["E"] & G1["G"]
    end
    subgraph rightlean["Nakloneny vpravo (bf=+1)"]
        D2["D<br/>bf=+1"] --- B2["B<br/>bf=0"]
        D2 --- F2["F<br/>bf=0"]
        B2 --- A2["A"] & C2["C"]
        F2 --- E2["E"] & G2["G"]
    end
    subgraph leftlean["Nakloneny vlevo (bf=-1)"]
        D3["D<br/>bf=-1"] --- B3["B<br/>bf=-1"]
        D3 --- E3["E"]
        B3 --- A3["A"]
    end

    style balanced fill:#d5f5e3,stroke:#27ae60
    style rightlean fill:#d5f5e3,stroke:#27ae60
    style leftlean fill:#d5f5e3,stroke:#27ae60
```

**NEPLATNY** -- faktor rovnovahy = +2 (potrebuje rotaci!):

```mermaid
graph TD
    B["B<br/><b>bf=+2 ✗</b>"]
    D["D<br/>bf=+1"]
    F["F<br/>bf=0"]
    B --- D
    D --- F

    style B fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
```

> Pravy podstrom je o 2 urovne vyssi nez levy (ktery je prazdny). To spousti **levou rotaci** pro obnoveni invariantu AVL.

## Rotace -- Obnoveni rovnovahy

Kdyz vlozeni nebo smazani zpusobi, ze faktor rovnovahy dosahne +/-2, strom se
musi **rotovat** pro obnoveni invariantu AVL. Existuji ctyri pripady, redukovatelne
na dve zakladni operace.

### Jednoducha leva rotace

Pouziva se, kdyz je uzel **tezky vpravo** (bf = +2) a jeho pravy potomek je
**tezky vpravo nebo vyvazeny** (bf >= 0):

**Pred** (bf=+2):

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

**Po** leve rotaci -- B povyseno na koren:

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

> **Kroky:** (1) Odpojit B od A. (2) Odpojit X (levy potomek B). (3) Pripojit X jako praveho potomka A. (4) Pripojit A jako leveho potomka B. Podstrom zakoreneny v B je nyni vyvazeny.

V kodu (`merk/src/tree/ops.rs`):

```rust
fn rotate<V>(self, left: bool, ...) -> CostResult<Self, Error> {
    // Odpojit potomka na tezke strane
    let (tree, child) = self.detach_expect(left, ...);
    // Odpojit vnuka z opacne strany potomka
    let (child, maybe_grandchild) = child.detach(!left, ...);

    // Pripojit vnuka k puvodnimu koreni
    tree.attach(left, maybe_grandchild)
        .maybe_balance(...)
        .flat_map_ok(|tree| {
            // Pripojit puvodni koren jako potomka povyseneho uzlu
            child.attach(!left, Some(tree))
                .maybe_balance(...)
        })
}
```

Vsimnete si, ze `maybe_balance` je volano rekurzivne -- samotna rotace muze
vytvorit nove nevyvazenosti, ktere vyzaduji dalsi korekci.

### Dvojita rotace (levo-prava)

Pouziva se, kdyz je uzel **tezky vlevo** (bf = -2), ale jeho levy potomek je
**tezky vpravo** (bf > 0). Jednoducha rotace by to nevyresila:

**Krok 0: Pred** -- C je tezky vlevo (bf=-2), ale jeho levy potomek A se naklani vpravo (bf=+1). Jednoducha rotace to nevyresi:

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

**Krok 1: Leva rotace potomka A** -- nyni se C i B naklaneji vlevo, opravitelne jednoduchou rotaci:

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

**Krok 2: Prava rotace korene C** -- vyvazeno!

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

Algoritmus detekuje tento pripad porovnanim smeru naklonu rodice s faktorem
rovnovahy potomka:

```rust
fn maybe_balance<V>(self, ...) -> CostResult<Self, Error> {
    let balance_factor = self.balance_factor();
    if balance_factor.abs() <= 1 {
        return Ok(self);  // Jiz vyvazeno
    }

    let left = balance_factor < 0;  // true pokud tezky vlevo

    // Dvojita rotace je potreba, kdyz se potomek naklani opacne nez rodic
    let tree = if left == (self.tree().link(left).unwrap().balance_factor() > 0) {
        // Prvni rotace: rotace potomka v opacnem smeru
        self.walk_expect(left, |child|
            child.rotate(!left, ...).map_ok(Some), ...
        )
    } else {
        self
    };

    // Druha (nebo jedina) rotace
    tree.rotate(left, ...)
}
```

## Davkove operace -- Sestaveni a aplikace

Namisto vkladani elementu po jednom, Merk podporuje davkove operace, ktere
aplikuji vice zmen v jednom pruchodu. To je klicove pro efektivitu: davka
N operaci na stromu M elementu zabere **O((M + N) log(M + N))** casu
oproti O(N log M) pro sekvencni vlozeni.

### Typ MerkBatch

```rust
type MerkBatch<K> = [(K, Op)];

enum Op {
    Put(Vec<u8>, TreeFeatureType),  // Vlozeni nebo aktualizace s hodnotou a typem vlastnosti
    PutWithSpecializedCost(...),     // Vlozeni s preddefinovanymi naklady
    PutCombinedReference(...),       // Vlozeni reference s kombinovanym hashem
    Replace(Vec<u8>, TreeFeatureType),
    Patch { .. },                    // Castecna aktualizace hodnoty
    Delete,                          // Smazani klice
    DeleteLayered,                   // Smazani s vrstvovanymi naklady
    DeleteMaybeSpecialized,          // Smazani s volitelnymi specializovanymi naklady
}
```

### Strategie 1: build() -- Sestaveni od nuly

Kdyz je strom prazdny, `build()` konstruuje vyvazeny strom primo ze
serazene davky pomoci algoritmu **rozdeleni medianem**:

Vstupni davka (serazena): `[A, B, C, D, E, F, G]` -- vyberte prostredni (D) jako koren, rekurzivne na kazdou polovinu:

```mermaid
graph TD
    D["<b>D</b><br/><small>koren = mid(0..6)</small>"]
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

> Vysledek: dokonale vyvazeny strom s vyskou = 3 = ceil(log2(7)).

```rust
fn build(batch: &MerkBatch<K>, ...) -> CostResult<Option<TreeNode>, Error> {
    let mid_index = batch.len() / 2;
    let (mid_key, mid_op) = &batch[mid_index];

    // Vytvoreni korenoveho uzlu z prostredniho elementu
    let mid_tree = TreeNode::new(mid_key.clone(), value.clone(), None, feature_type)?;

    // Rekurzivni sestaveni leveho a praveho podstromu
    let left = Self::build(&batch[..mid_index], ...);
    let right = Self::build(&batch[mid_index + 1..], ...);

    // Pripojeni potomku
    mid_tree.attach(true, left).attach(false, right)
}
```

To produkuje strom s vyskou ceil(log2(n)) -- dokonale vyvazeny.

### Strategie 2: apply_sorted() -- Slouceni do existujiciho stromu

Kdyz strom jiz obsahuje data, `apply_sorted()` pouziva **binarni vyhledavani**
pro nalezeni mista, kam kazda operace davky patri, a pote rekurzivne aplikuje
operace na levy a pravy podstrom:

Existujici strom s davkou `[(B, Put), (F, Delete)]`:

Binarni vyhledavani: B < D (jdi vlevo), F > D (jdi vpravo).

**Pred:**
```mermaid
graph TD
    D0["D"] --- C0["C"]
    D0 --- E0["E"]
    E0 --- F0["F"]
    style D0 fill:#d4e6f1,stroke:#2980b9
```

**Po** aplikaci davky a prevyvazeni:
```mermaid
graph TD
    D1["D"] --- B1["B"]
    D1 --- E1["E"]
    B1 --- C1["C"]
    style D1 fill:#d5f5e3,stroke:#27ae60
```

> B vlozeno jako levy podstrom, F smazano z praveho podstromu. `maybe_balance()` potvrzuje bf(D) = 0.

```rust
fn apply_sorted(self, batch: &MerkBatch<K>, ...) -> CostResult<...> {
    let search = batch.binary_search_by(|(key, _)| key.cmp(self.tree().key()));

    match search {
        Ok(index) => {
            // Klic odpovida tomuto uzlu — aplikovat operaci primo
            // (Put nahradi hodnotu, Delete smaze uzel)
        }
        Err(mid) => {
            // Klic nenalezen — mid je bod rozdeleni
            // Rekurze na left_batch[..mid] a right_batch[mid..]
        }
    }

    self.recurse(batch, mid, exclusive, ...)
}
```

Metoda `recurse` rozdeli davku a projde vlevo a vpravo:

```rust
fn recurse(self, batch: &MerkBatch<K>, mid: usize, ...) {
    let left_batch = &batch[..mid];
    let right_batch = &batch[mid..];  // nebo mid+1 pokud exkluzivni

    // Aplikovat levou davku na levy podstrom
    let tree = self.walk(true, |maybe_left| {
        Self::apply_to(maybe_left, left_batch, ...)
    });

    // Aplikovat pravou davku na pravy podstrom
    let tree = tree.walk(false, |maybe_right| {
        Self::apply_to(maybe_right, right_batch, ...)
    });

    // Prevyvazit po modifikacich
    tree.maybe_balance(...)
}
```

### Odstraneni uzlu

Pri mazani uzlu se dvema potomky Merk povysi **krajni uzel** z vyssiho podstromu.
To minimalizuje pravdepodobnost dalsich rotaci:

**Pred** -- mazani D (ma dva potomky, vyska praveho podstromu >= leveho):

```mermaid
graph TD
    D["D ✗ smazat"]
    B0["B"]
    F0["F"]
    A0["A"]
    C0["C"]
    E0["E ← naslednik"]
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

**Po** -- E (nejlevejsi v pravem podstromu = naslednik v poradi) povysen na pozici D:

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

> **Pravidlo:** Pokud leva vyska > prava -> povysit pravou hranu leveho podstromu. Pokud prava vyska >= leva -> povysit levou hranu praveho podstromu. To minimalizuje prevyvazovani po smazani.

```rust
pub fn remove(self, ...) -> CostResult<Option<Self>, Error> {
    let has_left = tree.link(true).is_some();
    let has_right = tree.link(false).is_some();
    let left = tree.child_height(true) > tree.child_height(false);

    if has_left && has_right {
        // Dva potomci: povysit hranu vyssiho potomka
        let (tree, tall_child) = self.detach_expect(left, ...);
        let (_, short_child) = tree.detach_expect(!left, ...);
        tall_child.promote_edge(!left, short_child, ...)
    } else if has_left || has_right {
        // Jeden potomek: povysit primo
        self.detach_expect(left, ...).1
    } else {
        // Listovy uzel: pouze smazat
        None
    }
}
```

---
