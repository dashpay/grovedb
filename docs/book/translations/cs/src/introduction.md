# Uvod -- Co je GroveDB?

## Zakladni myslenka

GroveDB je **hierarchicka autentizovana datova struktura** -- v podstate *haj*
(strom stromu) postaveny na Merklovych AVL stromech. Kazdy uzel v databazi je
soucasti kryptograficky autentizovaneho stromu a kazdy strom muze obsahovat
dalsi stromy jako potomky, cimz se vytvari hluboka hierarchie overitelneho stavu.

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

> Kazdy barevny ramecek je **samostatny strom Merk**. Prerusovane sipky znazornuji vztah podstromu -- element Tree v rodicovskem stromu obsahuje korenovy klic podrizeneho stromu Merk.

V tradicni databazi byste data ulozili do ploche uloziste klicu a hodnot
s jednim Merklovym stromem navrchu pro autentizaci. GroveDB pouziva jiny pristup:
vnoruje Merklovy stromy do Merklovych stromu. To vam dava:

1. **Efektivni sekundarni indexy** -- dotaz podle libovolne cesty, nejen primarniho klice
2. **Kompaktni kryptograficke dukazy (proof)** -- prokazani existence (nebo neexistence) jakychkoli dat
3. **Agregovana data** -- stromy mohou automaticky scitat, pocitat nebo jinak agregovat sve potomky
4. **Atomicke operace napric stromy** -- davkove operace zahrnuji vice podstromu

## Proc GroveDB existuje

GroveDB bylo navrzeno pro **Dash Platform**, decentralizovanou aplikacni platformu,
kde kazdy kousek stavu musi byt:

- **Autentizovany**: Libovolny uzel muze prokazat libovolny udaj lehkemu klientovi
- **Deterministicky**: Kazdy uzel vypocita presne stejny korenovy hash (root hash)
- **Efektivni**: Operace musi byt dokonceny v ramci casovych omezeni bloku
- **Dotazovatelny**: Aplikace potrebuji bohate dotazy, nejen vyhledavani podle klicu

Tradicni pristupy nestaci:

| Pristup | Problem |
|----------|---------|
| Prosty Merkluv strom | Podporuje pouze vyhledavani klicu, zadne rozsahove dotazy |
| Ethereum MPT | Drahe prevazovani, velke dukazy |
| Ploche klice-hodnoty + jeden strom | Zadne hierarchicke dotazy, jeden dukaz pokryva vse |
| B-strom | Neni prirozene merkleizovany, slozita autentizace |

GroveDB tyto problemy resi kombinaci **osvedcenych zaruk vyvazovani AVL stromu**
s **hierarchickym vnorenim** a **bohatym typovym systemem elementu**.

## Prehled architektury

GroveDB je organizovano do odlisnych vrstev, kazda s jasnou odpovednosti:

```mermaid
graph TD
    APP["<b>Aplikacni vrstva</b><br/>Dash Platform atd.<br/><code>insert() get() query() prove() apply_batch()</code>"]

    GROVE["<b>Jadro GroveDB</b> — <code>grovedb/src/</code><br/>Sprava hierarchickych podstromu · Typovy system elementu<br/>Rezoluce referenci · Davkove operace · Vicevrstvove dukazy"]

    MERK["<b>Vrstva Merk</b> — <code>merk/src/</code><br/>Merkluv AVL strom · Samobalancujici rotace<br/>System linku · Hashovani Blake3 · Kodovani dukazu"]

    STORAGE["<b>Vrstva uloziste</b> — <code>storage/src/</code><br/>RocksDB OptimisticTransactionDB<br/>4 rodiny sloupcu · Izolace prefixem Blake3 · Davkove zapisy"]

    COST["<b>Vrstva nakladu</b> — <code>costs/src/</code><br/>Sledovani OperationCost · Monada CostContext<br/>Odhad nejhorsiho a prumerneho pripadu"]

    APP ==>|"zapisy ↓"| GROVE
    GROVE ==>|"operace stromu"| MERK
    MERK ==>|"diskove I/O"| STORAGE
    STORAGE -.->|"akumulace nakladu ↑"| COST
    COST -.-> MERK
    MERK -.-> GROVE
    GROVE ==>|"cteni ↑"| APP

    style APP fill:#f5f5f5,stroke:#999,stroke-width:1px
    style GROVE fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style MERK fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style STORAGE fill:#fdebd0,stroke:#e67e22,stroke-width:2px
    style COST fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

Data prochazi temito vrstvami **smerem dolu** pri zapisech a **smerem nahoru** pri
ctenich. Kazda operace akumuluje naklady pri pruchodu zasobnikem, coz umoznuje
presne uctovani zdroju.

---
