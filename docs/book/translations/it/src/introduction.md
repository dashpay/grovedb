# Introduzione — Cos'e GroveDB?

## L'idea fondamentale

GroveDB e una **struttura dati gerarchica autenticata** — essenzialmente un *grove* (bosco, ovvero albero di alberi) costruito su alberi Merkle AVL. Ogni nodo nel database fa parte di un albero autenticato crittograficamente, e ogni albero puo contenere altri alberi come figli, formando una gerarchia profonda di stato verificabile.

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

> Ogni riquadro colorato e un **albero Merk separato**. Le frecce tratteggiate mostrano la relazione di sotto-albero — un elemento Tree nel genitore contiene la chiave radice del Merk figlio.

In un database tradizionale, si potrebbero archiviare i dati in un archivio chiave-valore piatto con un singolo albero di Merkle (albero di Merkle) in cima per l'autenticazione. GroveDB adotta un approccio diverso: annida alberi di Merkle dentro altri alberi di Merkle. Questo offre:

1. **Indici secondari efficienti** — interrogazione per qualsiasi percorso, non solo per chiave primaria
2. **Prove crittografiche compatte** — dimostrare l'esistenza (o l'assenza) di qualsiasi dato
3. **Dati aggregati** — gli alberi possono automaticamente sommare, contare o aggregare in altro modo i propri figli
4. **Operazioni atomiche cross-albero** — le operazioni batch (operazioni in blocco) coprono piu sotto-alberi

## Perche esiste GroveDB

GroveDB e stato progettato per **Dash Platform**, una piattaforma applicativa decentralizzata dove ogni pezzo di stato deve essere:

- **Autenticato**: qualsiasi nodo puo dimostrare qualsiasi dato a un client leggero
- **Deterministico**: ogni nodo calcola esattamente la stessa radice di stato
- **Efficiente**: le operazioni devono completarsi entro i vincoli temporali del blocco
- **Interrogabile**: le applicazioni necessitano di query ricche, non solo ricerche per chiave

Gli approcci tradizionali presentano limiti:

| Approccio | Problema |
|----------|---------|
| Albero di Merkle semplice | Supporta solo ricerche per chiave, nessuna query su intervalli |
| Ethereum MPT | Ribilanciamento costoso, dimensioni delle prove elevate |
| Chiave-valore piatto + singolo albero | Nessuna query gerarchica, una singola prova copre tutto |
| B-tree | Non naturalmente Merklizzato, autenticazione complessa |

GroveDB risolve questi problemi combinando le **garanzie di bilanciamento comprovate degli alberi AVL** con l'**annidamento gerarchico** e un **ricco sistema di tipi di elementi**.

## Panoramica dell'architettura

GroveDB e organizzato in livelli distinti, ciascuno con una responsabilita chiara:

```mermaid
graph TD
    APP["<b>Livello Applicazione</b><br/>Dash Platform, ecc.<br/><code>insert() get() query() prove() apply_batch()</code>"]

    GROVE["<b>GroveDB Core</b> — <code>grovedb/src/</code><br/>Gestione gerarchica dei sotto-alberi · Sistema dei tipi di elementi<br/>Risoluzione dei riferimenti · Operazioni batch · Prove multi-livello"]

    MERK["<b>Livello Merk</b> — <code>merk/src/</code><br/>Albero Merkle AVL · Rotazioni autobilancianti<br/>Sistema di link · Hashing Blake3 · Codifica delle prove"]

    STORAGE["<b>Livello di archiviazione</b> — <code>storage/src/</code><br/>RocksDB OptimisticTransactionDB<br/>4 famiglie di colonne · Isolamento con prefisso Blake3 · Scritture in blocco"]

    COST["<b>Livello dei costi</b> — <code>costs/src/</code><br/>Tracciamento OperationCost · Monade CostContext<br/>Stima nel caso peggiore e nel caso medio"]

    APP ==>|"scritture ↓"| GROVE
    GROVE ==>|"operazioni albero"| MERK
    MERK ==>|"I/O disco"| STORAGE
    STORAGE -.->|"accumulo costi ↑"| COST
    COST -.-> MERK
    MERK -.-> GROVE
    GROVE ==>|"letture ↑"| APP

    style APP fill:#f5f5f5,stroke:#999,stroke-width:1px
    style GROVE fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style MERK fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style STORAGE fill:#fdebd0,stroke:#e67e22,stroke-width:2px
    style COST fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

I dati fluiscono **verso il basso** attraverso questi livelli durante le scritture e **verso l'alto** durante le letture. Ogni operazione accumula costi mentre attraversa lo stack, consentendo una contabilizzazione precisa delle risorse.

---
