# Wprowadzenie -- Czym jest GroveDB?

## Glowna idea

GroveDB to **hierarchiczna uwierzytelniona struktura danych** -- zasadniczo *gaj*
(drzewo drzew) zbudowany na drzewach Merkle AVL. Kazdy wezel w bazie danych jest
czescia kryptograficznie uwierzytelnionego drzewa, a kazde drzewo moze zawierac
inne drzewa jako potomkow, tworzac gleboka hierarchie weryfikowalnego stanu.

```mermaid
graph TD
    subgraph root["Root Merk Tree"]
        R_contracts["&quot;contracts&quot;<br/><i>Tree</i>"]
        R_identities["&quot;identities&quot;<br/><i>Tree</i>"]
        R_balances["&quot;balances&quot;<br/><i>SumTree</i>"]
        R_contracts --- R_identities
        R_contracts --- R_balances
    end

    subgraph ident["Identities Merk"]
        I_bob["&quot;bob&quot;<br/><i>Tree</i>"]
        I_alice["&quot;alice&quot;<br/><i>Tree</i>"]
        I_carol["&quot;carol&quot;<br/><i>Item</i>"]
        I_bob --- I_alice
        I_bob --- I_carol
    end

    subgraph contracts["Contracts Merk"]
        C_c2["&quot;C2&quot;<br/><i>Item</i>"]
        C_c1["&quot;C1&quot;<br/><i>Item</i>"]
        C_c3["&quot;C3&quot;<br/><i>Item</i>"]
        C_c2 --- C_c1
        C_c2 --- C_c3
    end

    subgraph balances["Balances SumTree — sum=5300"]
        B_bob["&quot;bob&quot;<br/>SumItem(2500)"]
        B_al["&quot;alice&quot;<br/>SumItem(2000)"]
        B_eve["&quot;eve&quot;<br/>SumItem(800)"]
        B_bob --- B_al
        B_bob --- B_eve
    end

    subgraph alice_merk["Alice Merk"]
        A_name["&quot;name&quot; → Alice"]
        A_bal["&quot;balance&quot; → 1000"]
    end

    subgraph bob_merk["Bob Merk"]
        Bo_name["&quot;name&quot; → Bob"]
    end

    R_identities -.->|subtree| ident
    R_contracts -.->|subtree| contracts
    R_balances -.->|subtree| balances
    I_alice -.->|subtree| alice_merk
    I_bob -.->|subtree| bob_merk

    style root fill:#e8f4fd,stroke:#2980b9,stroke-width:2px
    style ident fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style contracts fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style balances fill:#fdedec,stroke:#e74c3c,stroke-width:2px
    style alice_merk fill:#eafaf1,stroke:#27ae60,stroke-width:2px
    style bob_merk fill:#eafaf1,stroke:#27ae60,stroke-width:2px
```

> Kazde kolorowe pole to **oddzielne drzewo Merk**. Przerywane strzalki pokazuja relacje poddrzew -- element Tree w rodzicu zawiera klucz korzenia potomnego drzewa Merk.

W tradycyjnej bazie danych mozna by przechowywac dane w plaskim magazynie klucz-wartosc
z pojedynczym drzewem Merkle na wierzchu do uwierzytelniania. GroveDB przyjmuje inne
podejscie: zagniezdza drzewa Merkle wewnatrz drzew Merkle. Daje to:

1. **Wydajne indeksy wtorne** -- zapytania po dowolnej sciezce, nie tylko po kluczu glownym
2. **Kompaktowe dowody kryptograficzne (proof)** -- dowod istnienia (lub nieobecnosci) dowolnych danych
3. **Agregowane dane** -- drzewa moga automatycznie sumowac, liczyc lub w inny sposob agregowac swoje potomki
4. **Atomowe operacje miedzy drzewami** -- operacje wsadowe (batch) obejmuja wiele poddrzew

## Dlaczego GroveDB istnieje

GroveDB zostal zaprojektowany dla **Dash Platform**, zdecentralizowanej platformy
aplikacyjnej, gdzie kazdy element stanu musi byc:

- **Uwierzytelniony**: Kazdy wezel moze udowodnic dowolny element stanu lekkiemu klientowi
- **Deterministyczny**: Kazdy wezel oblicza dokladnie ten sam korzeń stanu (state root)
- **Wydajny**: Operacje musza zakonczyc sie w ramach ograniczen czasowych bloku
- **Odpytywalny**: Aplikacje potrzebuja bogatych zapytan, nie tylko wyszukiwan po kluczu

Tradycyjne podejscia sa niewystarczajace:

| Podejscie | Problem |
|----------|---------|
| Zwykle drzewo Merkle | Obsluguje tylko wyszukiwania po kluczu, brak zapytan zakresowych |
| Ethereum MPT | Kosztowne rebalansowanie, duze rozmiary dowodow |
| Plaski magazyn klucz-wartosc + pojedyncze drzewo | Brak hierarchicznych zapytan, pojedynczy dowod obejmuje wszystko |
| B-drzewo | Nie jest naturalnie zmerklizowane, zlozony proces uwierzytelniania |

GroveDB rozwiazuje te problemy, laczac **sprawdzone gwarancje rownowazenía drzew AVL**
z **hierarchicznym zaglezdzaniem** i **bogatym systemem typow elementow**.

## Przeglad architektury

GroveDB jest zorganizowany w oddzielne warstwy, z ktorych kazda ma jasno okreslona odpowiedzialnosc:

```mermaid
graph TD
    APP["<b>Warstwa aplikacji</b><br/>Dash Platform itp.<br/><code>insert() get() query() prove() apply_batch()</code>"]

    GROVE["<b>Rdzen GroveDB</b> — <code>grovedb/src/</code><br/>Zarzadzanie hierarchicznymi poddrzewami · System typow elementow<br/>Rozwiazywanie referencji · Operacje wsadowe · Wielowarstwowe dowody"]

    MERK["<b>Warstwa Merk</b> — <code>merk/src/</code><br/>Drzewo Merkle AVL · Samorownoważace sie rotacje<br/>System linkow · Haszowanie Blake3 · Kodowanie dowodow"]

    STORAGE["<b>Warstwa magazynowania</b> — <code>storage/src/</code><br/>RocksDB OptimisticTransactionDB<br/>4 rodziny kolumn · Izolacja prefiksami Blake3 · Zapisywanie wsadowe"]

    COST["<b>Warstwa kosztow</b> — <code>costs/src/</code><br/>Sledzenie OperationCost · Monada CostContext<br/>Szacowanie najgorszego i sredniego przypadku"]

    APP ==>|"zapisy ↓"| GROVE
    GROVE ==>|"operacje na drzewach"| MERK
    MERK ==>|"operacje dyskowe"| STORAGE
    STORAGE -.->|"akumulacja kosztow ↑"| COST
    COST -.-> MERK
    MERK -.-> GROVE
    GROVE ==>|"odczyty ↑"| APP

    style APP fill:#f5f5f5,stroke:#999,stroke-width:1px
    style GROVE fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style MERK fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style STORAGE fill:#fdebd0,stroke:#e67e22,stroke-width:2px
    style COST fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

Dane plyna **w dol** przez te warstwy podczas zapisow i **w gore** podczas odczytow.
Kazda operacja akumuluje koszty przechodzac przez stos, umozliwiajac precyzyjne
rozliczanie zasobow.

---
