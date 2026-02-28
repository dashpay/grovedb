# System referencji

## Dlaczego referencje istnieja

W hierarchicznej bazie danych czesto potrzebny jest dostep do tych samych danych
z wielu sciezek. Na przyklad dokumenty moga byc przechowywane pod ich kontraktem,
ale rowniez odpytywalne po tozsamosci wlasciciela. **Referencje** to odpowiedz
GroveDB -- sa wskaznikami z jednej lokalizacji do drugiej, podobnymi do dowiazan
symbolicznych (symbolic links) w systemie plikow.

```mermaid
graph LR
    subgraph primary["Magazyn glowny"]
        item["contracts/C1/docs/D1<br/><b>Item</b>(data)"]
    end
    subgraph secondary["Indeks wtorny"]
        ref["identities/alice/docs/D1<br/><b>Reference</b>"]
    end
    ref -->|"wskazuje na"| item

    style primary fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style secondary fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style ref fill:#fef9e7,stroke:#f39c12,stroke-width:2px
```

Kluczowe wlasciwosci:
- Referencje sa **uwierzytelniane** -- value_hash referencji zawiera zarowno sama
  referencje, jak i element referencyjny
- Referencje moga byc **lancuchowe** -- referencja moze wskazywac na inna referencje
- Wykrywanie cykli zapobiega nieskonczonym petlom
- Konfigurowalny limit skokow (hop limit) zapobiega wyczerpaniu zasobow

## Siedem typow referencji

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

Przyjrzyjmy sie kazdemu z diagramami.

### AbsolutePathReference

Najprostszy typ. Przechowuje pelna sciezke do celu:

```mermaid
graph TD
    subgraph root["Korzeniowe Merk — sciezka: []"]
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
        R["R = Item<br/>&quot;cel&quot;"]
    end

    A -.-> B
    P -.-> Q
    B -.-> X
    Q -.-> R
    X ==>|"rozwiazuje na [P, Q, R]"| R

    style merkAB fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style merkPQ fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style X fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style R fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

> X przechowuje pelna bezwzgledna sciezke `[P, Q, R]`. Niezaleznie od lokalizacji X, zawsze rozwiazuje sie na ten sam cel.

### UpstreamRootHeightReference

Zachowuje pierwsze N segmentow biezacej sciezki, a nastepnie dopisuje nowa sciezke:

```mermaid
graph TD
    subgraph resolve["Rozwiazywanie: zachowaj pierwsze 2 segmenty + dopisz [P, Q]"]
        direction LR
        curr["biezaca: [A, B, C, D]"] --> keep["zachowaj pierwsze 2: [A, B]"] --> append["dopisz: [A, B, <b>P, Q</b>]"]
    end

    subgraph grove["Hierarchia gaju"]
        gA["A (wysokosc 0)"]
        gB["B (wysokosc 1)"]
        gC["C (wysokosc 2)"]
        gD["D (wysokosc 3)"]
        gX["X = Reference<br/>UpstreamRootHeight(2, [P,Q])"]
        gP["P (wysokosc 2)"]
        gQ["Q (wysokosc 3) — cel"]

        gA --> gB
        gB --> gC
        gB -->|"zachowaj pierwsze 2 → [A,B]<br/>potem zejdz [P,Q]"| gP
        gC --> gD
        gD -.-> gX
        gP --> gQ
    end

    gX ==>|"rozwiazuje na"| gQ

    style resolve fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style gX fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style gQ fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

### UpstreamRootHeightWithParentPathAdditionReference

Jak UpstreamRootHeight, ale ponownie dopisuje ostatni segment biezacej sciezki:

```text
    Referencja na sciezce [A, B, C, D, E] klucz=X
    UpstreamRootHeightWithParentPathAdditionReference(2, [P, Q])

    Biezaca sciezka:    [A, B, C, D, E]
    Zachowaj pierwsze 2: [A, B]
    Dopisz [P, Q]:       [A, B, P, Q]
    Dopisz ostatni:      [A, B, P, Q, E]   ← "E" z oryginalnej sciezki dodane z powrotem

    Przydatne dla: indeksow, gdzie klucz nadrzedny powinien byc zachowany
```

### UpstreamFromElementHeightReference

Odrzuca ostatnie N segmentow, a nastepnie dopisuje:

```text
    Referencja na sciezce [A, B, C, D] klucz=X
    UpstreamFromElementHeightReference(1, [P, Q])

    Biezaca sciezka:     [A, B, C, D]
    Odrzuc ostatni 1:    [A, B, C]
    Dopisz [P, Q]:       [A, B, C, P, Q]
```

### CousinReference

Zastepuje tylko bezposredniego rodzica nowym kluczem:

```mermaid
graph TD
    subgraph resolve["Rozwiazywanie: usun ostatnie 2, dodaj kuzyna C, dodaj klucz X"]
        direction LR
        r1["sciezka: [A, B, M, D]"] --> r2["usun ostatnie 2: [A, B]"] --> r3["dodaj C: [A, B, C]"] --> r4["dodaj klucz X: [A, B, C, X]"]
    end

    subgraph merkAB["Merk [A, B]"]
        M["M<br/>Tree"]
        C["C<br/>Tree<br/>(kuzyn M)"]
    end

    subgraph merkABM["Merk [A, B, M]"]
        D["D<br/>Tree"]
    end

    subgraph merkABMD["Merk [A, B, M, D]"]
        Xref["X = Reference<br/>CousinReference(C)"]
    end

    subgraph merkABC["Merk [A, B, C]"]
        Xtarget["X = Item<br/>(cel)"]
    end

    M -.-> D
    D -.-> Xref
    C -.-> Xtarget
    Xref ==>|"rozwiazuje na [A, B, C, X]"| Xtarget

    style resolve fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style Xref fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style Xtarget fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style M fill:#fadbd8,stroke:#e74c3c
    style C fill:#d5f5e3,stroke:#27ae60
```

> "Kuzyn" to poddrzewo bedace rodzenistwem dziadka referencji. Referencja nawiguje dwa poziomy w gore, a nastepnie schodzi do poddrzewa kuzyna.

### RemovedCousinReference

Jak CousinReference, ale zastepuje rodzica wielosegmentowa sciezka:

```text
    Referencja na sciezce [A, B, C, D] klucz=X
    RemovedCousinReference([M, N])

    Biezaca sciezka:  [A, B, C, D]
    Usun rodzica C:   [A, B]
    Dopisz [M, N]:    [A, B, M, N]
    Dodaj klucz X:    [A, B, M, N, X]
```

### SiblingReference

Najprostsza referencja wzgledna -- po prostu zmienia klucz w ramach tego samego rodzica:

```mermaid
graph TD
    subgraph merk["Merk [A, B, C] — to samo drzewo, ta sama sciezka"]
        M_sib["M"]
        X_sib["X = Reference<br/>SiblingRef(Y)"]
        Y_sib["Y = Item<br/>(cel)"]
        Z_sib["Z = Item"]
        M_sib --> X_sib
        M_sib --> Y_sib
    end

    X_sib ==>|"rozwiazuje na [A, B, C, Y]"| Y_sib

    style merk fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style X_sib fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style Y_sib fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

> Najprostszy typ referencji. X i Y sa rodzenstwem w tym samym drzewie Merk -- rozwiazywanie po prostu zmienia klucz, zachowujac ta sama sciezke.

## Podazanie za referencjami i limit skokow

Gdy GroveDB napotyka element Reference, musi **podazyc** za nim, aby znalezc
rzeczywista wartosc. Poniewaz referencje moga wskazywac na inne referencje,
obejmuje to petle:

```rust
// grovedb/src/reference_path.rs
pub const MAX_REFERENCE_HOPS: usize = 10;

pub fn follow_reference(...) -> CostResult<ResolvedReference, Error> {
    let mut hops_left = MAX_REFERENCE_HOPS;
    let mut visited = HashSet::new();

    while hops_left > 0 {
        // Rozwiaz sciezke referencji na sciezke bezwzgledna
        let target_path = current_ref.absolute_qualified_path(...);

        // Sprawdz cykle
        if !visited.insert(target_path.clone()) {
            return Err(Error::CyclicReference);
        }

        // Pobierz element w celu
        let element = Element::get(target_path);

        match element {
            Element::Reference(next_ref, ..) => {
                // Nadal referencja -- kontynuuj podazanie
                current_ref = next_ref;
                hops_left -= 1;
            }
            other => {
                // Znaleziono rzeczywisty element!
                return Ok(ResolvedReference { element: other, ... });
            }
        }
    }

    Err(Error::ReferenceLimit)  // Przekroczono 10 skokow
}
```

## Wykrywanie cykli

`HashSet` `visited` sledzi wszystkie sciezki, ktore juz widzielismy. Jezeli
napotykamy sciezke, ktora juz odwiedzilismy, mamy cykl:

```mermaid
graph LR
    A["A<br/>Reference"] -->|"krok 1"| B["B<br/>Reference"]
    B -->|"krok 2"| C["C<br/>Reference"]
    C -->|"krok 3"| A

    style A fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style B fill:#fef9e7,stroke:#f39c12
    style C fill:#fef9e7,stroke:#f39c12
```

> **Slad wykrywania cykli:**
>
> | Krok | Podazaj | Zbior visited | Wynik |
> |------|--------|-------------|--------|
> | 1 | Zacznij od A | { A } | A jest Ref → podazaj |
> | 2 | A → B | { A, B } | B jest Ref → podazaj |
> | 3 | B → C | { A, B, C } | C jest Ref → podazaj |
> | 4 | C → A | A juz w visited! | **Error::CyclicRef** |
>
> Bez wykrywania cykli, to petliloby sie w nieskonczonosc. `MAX_REFERENCE_HOPS = 10` rowniez ogranicza glebokosc przechodzenia dla dlugich lancuchow.

## Referencje w Merk -- Polaczone hasze wartosci

Gdy referencja jest przechowywana w drzewie Merk, jej `value_hash` musi
uwierzytelnic zarowno strukture referencji, jak i dane referencyjne:

```rust
// merk/src/tree/kv.rs
pub fn update_hashes_using_reference_value_hash(
    mut self,
    reference_value_hash: CryptoHash,
) -> CostContext<Self> {
    // Haszuj wlasne bajty elementu referencji
    let actual_value_hash = value_hash(self.value_as_slice());

    // Polacz: H(bajty_referencji) + H(dane_referencyjne)
    let combined = combine_hash(&actual_value_hash, &reference_value_hash);

    self.value_hash = combined;
    self.hash = kv_digest_to_kv_hash(self.key(), self.value_hash());
    // ...
}
```

Oznacza to, ze zmiana zarowno samej referencji, JAK I danych, na ktore wskazuje,
zmieni hasz korzenia -- oba sa kryptograficznie powiazane.

---

