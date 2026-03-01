# System referenci

## Proc reference existuji

V hierarchicke databazi casto potrebujete stejna data pristupna z vice cest.
Napriklad dokumenty mohou byt ulozeny pod svym kontraktem, ale take
dotazovatelne podle identity vlastnika. **Reference** jsou odpovedi GroveDB --
jsou to ukazatele z jednoho mista na druhe, podobne symbolicnym odkazu
v souborovem systemu.

```mermaid
graph LR
    subgraph primary["Primarni uloziste"]
        item["contracts/C1/docs/D1<br/><b>Item</b>(data)"]
    end
    subgraph secondary["Sekundarni index"]
        ref["identities/alice/docs/D1<br/><b>Reference</b>"]
    end
    ref -->|"ukazuje na"| item

    style primary fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style secondary fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style ref fill:#fef9e7,stroke:#f39c12,stroke-width:2px
```

Klicove vlastnosti:
- Reference jsou **autentizovane** -- value_hash reference zahrnuje jak
  samotnou referenci, tak referencovany element
- Reference mohou byt **retezene** -- reference muze ukazovat na dalsi referenci
- Detekce cyklu zabranuje nekonecnym smyckam
- Konfigurovatelny limit skoku (hop) zabranuje vycerpani zdroju

## Sedm typu referenci

```rust
// grovedb-element/src/reference_path/mod.rs
pub enum ReferencePathType {
    AbsolutePathReference(Vec<Vec<u8>>),
    UpstreamRootHeightReference(u8, Vec<Vec<u8>>),
    UpstreamRootHeightWithParentPathAdditionReference(u8, Vec<Vec<u8>>),
    UpstreamFromElementHeightReference(u8, Vec<Vec<u8>>),
    CousinReference(Vec<u8>),
    RemovedCousinReference(Vec<Vec<u8>>),
    SiblingReference(Vec<u8>),
}
```

Projdeme si kazdy s diagramy.

### AbsolutePathReference

Nejjednodussi typ. Uklada plnou cestu k cili:

```mermaid
graph TD
    subgraph root["Korenovy Merk — cesta: []"]
        A["A<br/>Tree"]
        P["P<br/>Tree"]
    end

    subgraph merkA["Merk [A]"]
        B["B<br/>Tree"]
    end

    subgraph merkP["Merk [P]"]
        Q["Q<br/>Tree"]
    end

    subgraph merkAB["Merk [A, B]"]
        X["X = Reference<br/>AbsolutePathRef([P, Q, R])"]
    end

    subgraph merkPQ["Merk [P, Q]"]
        R["R = Item<br/>&quot;target&quot;"]
    end

    A -.-> B
    P -.-> Q
    B -.-> X
    Q -.-> R
    X ==>|"rozlisi se na [P, Q, R]"| R

    style merkAB fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style merkPQ fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style X fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style R fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

> X uklada uplnou absolutni cestu `[P, Q, R]`. Bez ohledu na to, kde se X nachazi, vzdy se rozlisi na stejny cil.

### UpstreamRootHeightReference

Ponechava prvnich N segmentu aktualni cesty, pote pripoji novou cestu:

```mermaid
graph TD
    subgraph resolve["Rezoluce: ponechat prvni 2 segmenty + pripojit [P, Q]"]
        direction LR
        curr["aktualni: [A, B, C, D]"] --> keep["ponechat prvni 2: [A, B]"] --> append["pripojit: [A, B, <b>P, Q</b>]"]
    end

    subgraph grove["Hierarchie haje"]
        gA["A (vyska 0)"]
        gB["B (vyska 1)"]
        gC["C (vyska 2)"]
        gD["D (vyska 3)"]
        gX["X = Reference<br/>UpstreamRootHeight(2, [P,Q])"]
        gP["P (vyska 2)"]
        gQ["Q (vyska 3) — cil"]

        gA --> gB
        gB --> gC
        gB -->|"ponechat prvni 2 → [A,B]<br/>pak sestoupit [P,Q]"| gP
        gC --> gD
        gD -.-> gX
        gP --> gQ
    end

    gX ==>|"rozlisi se na"| gQ

    style resolve fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style gX fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style gQ fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

### UpstreamRootHeightWithParentPathAdditionReference

Podobne jako UpstreamRootHeight, ale znovu pripoji posledni segment aktualni cesty:

```text
    Reference na ceste [A, B, C, D, E] klic=X
    UpstreamRootHeightWithParentPathAdditionReference(2, [P, Q])

    Aktualni cesta:    [A, B, C, D, E]
    Ponechat prvni 2:  [A, B]
    Pripojit [P, Q]:   [A, B, P, Q]
    Znovu pripojit posledni: [A, B, P, Q, E]   ← "E" z puvodni cesty pridano zpet

    Uzitecne pro: indexy, kde se ma zachovat rodicovsky klic
```

### UpstreamFromElementHeightReference

Zahodí poslednich N segmentu, pote pripoji:

```text
    Reference na ceste [A, B, C, D] klic=X
    UpstreamFromElementHeightReference(1, [P, Q])

    Aktualni cesta:     [A, B, C, D]
    Zahodit posledni 1: [A, B, C]
    Pripojit [P, Q]:    [A, B, C, P, Q]
```

### CousinReference

Nahradi pouze bezprostredniho rodice novym klicem:

```mermaid
graph TD
    subgraph resolve["Rezoluce: odebrat posledni 2, vlozit bratrance C, vlozit klic X"]
        direction LR
        r1["cesta: [A, B, M, D]"] --> r2["odebrat posledni 2: [A, B]"] --> r3["vlozit C: [A, B, C]"] --> r4["vlozit klic X: [A, B, C, X]"]
    end

    subgraph merkAB["Merk [A, B]"]
        M["M<br/>Tree"]
        C["C<br/>Tree<br/>(bratranec M)"]
    end

    subgraph merkABM["Merk [A, B, M]"]
        D["D<br/>Tree"]
    end

    subgraph merkABMD["Merk [A, B, M, D]"]
        Xref["X = Reference<br/>CousinReference(C)"]
    end

    subgraph merkABC["Merk [A, B, C]"]
        Xtarget["X = Item<br/>(cil)"]
    end

    M -.-> D
    D -.-> Xref
    C -.-> Xtarget
    Xref ==>|"rozlisi se na [A, B, C, X]"| Xtarget

    style resolve fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style Xref fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style Xtarget fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style M fill:#fadbd8,stroke:#e74c3c
    style C fill:#d5f5e3,stroke:#27ae60
```

> "Bratranec" (cousin) je sourozenecky podstrom prarodice reference. Reference naviguje o dve urovne nahoru, pote sestoupi do bratrancova podstromu.

### RemovedCousinReference

Jako CousinReference, ale nahradi rodice vicesegmentovou cestou:

```text
    Reference na ceste [A, B, C, D] klic=X
    RemovedCousinReference([M, N])

    Aktualni cesta:   [A, B, C, D]
    Odebrat rodice C: [A, B]
    Pripojit [M, N]:  [A, B, M, N]
    Vlozit klic X:    [A, B, M, N, X]
```

### SiblingReference

Nejjednodussi relativni reference -- pouze zmeni klic v ramci stejneho rodice:

```mermaid
graph TD
    subgraph merk["Merk [A, B, C] — stejny strom, stejna cesta"]
        M_sib["M"]
        X_sib["X = Reference<br/>SiblingRef(Y)"]
        Y_sib["Y = Item<br/>(cil)"]
        Z_sib["Z = Item"]
        M_sib --> X_sib
        M_sib --> Y_sib
    end

    X_sib ==>|"rozlisi se na [A, B, C, Y]"| Y_sib

    style merk fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style X_sib fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style Y_sib fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

> Nejjednodussi typ reference. X a Y jsou sourozenci ve stejnem stromu Merk -- rezoluce pouze zmeni klic pri zachovani stejne cesty.

## Nasledovani referenci a limit skoku

Kdyz GroveDB narazi na element Reference, musi ho **nasledovat** pro nalezeni
skutecne hodnoty. Protoze reference mohou ukazovat na dalsi reference, zahrnuje
to smycku:

```rust
// grovedb/src/reference_path.rs
pub const MAX_REFERENCE_HOPS: usize = 10;

pub fn follow_reference(...) -> CostResult<ResolvedReference, Error> {
    let mut hops_left = MAX_REFERENCE_HOPS;
    let mut visited = HashSet::new();

    while hops_left > 0 {
        // Rezoluce cesty reference na absolutni cestu
        let target_path = current_ref.absolute_qualified_path(...);

        // Kontrola cyklu
        if !visited.insert(target_path.clone()) {
            return Err(Error::CyclicReference);
        }

        // Nacteni elementu na cili
        let element = Element::get(target_path);

        match element {
            Element::Reference(next_ref, ..) => {
                // Stale reference — pokracovat v nasledovani
                current_ref = next_ref;
                hops_left -= 1;
            }
            other => {
                // Nalezen skutecny element!
                return Ok(ResolvedReference { element: other, ... });
            }
        }
    }

    Err(Error::ReferenceLimit)  // Prekroceno 10 skoku
}
```

## Detekce cyklu

`HashSet` `visited` sleduje vsechny cesty, ktere jsme videli. Pokud narazime
na cestu, kterou jsme jiz navstivili, mame cyklus:

```mermaid
graph LR
    A["A<br/>Reference"] -->|"krok 1"| B["B<br/>Reference"]
    B -->|"krok 2"| C["C<br/>Reference"]
    C -->|"krok 3"| A

    style A fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style B fill:#fef9e7,stroke:#f39c12
    style C fill:#fef9e7,stroke:#f39c12
```

> **Sledovani detekce cyklu:**
>
> | Krok | Nasledovat | mnozina visited | Vysledek |
> |------|-----------|-----------------|----------|
> | 1 | Zacatek v A | { A } | A je Ref → nasledovat |
> | 2 | A → B | { A, B } | B je Ref → nasledovat |
> | 3 | B → C | { A, B, C } | C je Ref → nasledovat |
> | 4 | C → A | A jiz v visited! | **Error::CyclicRef** |
>
> Bez detekce cyklu by se to opetovalo donekonecna. `MAX_REFERENCE_HOPS = 10` take omezuje hloubku pruchodu pro dlouhe retezce.

## Reference v Merk -- Kombinovane hashe hodnot

Kdyz je Reference ulozen ve stromu Merk, jeho `value_hash` musi autentizovat
jak strukturu reference, tak referencovana data:

```rust
// merk/src/tree/kv.rs
pub fn update_hashes_using_reference_value_hash(
    mut self,
    reference_value_hash: CryptoHash,
) -> CostContext<Self> {
    // Hash bajtu samotneho elementu reference
    let actual_value_hash = value_hash(self.value_as_slice());

    // Kombinace: H(bajty_reference) ⊕ H(referencovana_data)
    let combined = combine_hash(&actual_value_hash, &reference_value_hash);

    self.value_hash = combined;
    self.hash = kv_digest_to_kv_hash(self.key(), self.value_hash());
    // ...
}
```

To znamena, ze zmena samotne reference NEBO dat, na ktera ukazuje,
zmeni korenovy hash -- obe jsou kryptograficky svazany.

---
