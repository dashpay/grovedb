# Introduction — Qu'est-ce que GroveDB ?

## L'idée fondamentale

GroveDB est une **structure de données authentifiée hiérarchique** — essentiellement un *bosquet*
(arbre d'arbres) construit sur des arbres AVL de Merkle (Merkle AVL trees). Chaque nœud dans la base de données fait partie d'un
arbre authentifié cryptographiquement, et chaque arbre peut contenir d'autres arbres en tant
qu'enfants, formant une hiérarchie profonde d'états vérifiables.

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

> Chaque boîte colorée est un **arbre Merk distinct**. Les flèches en pointillés montrent la relation de sous-arbre — un élément Tree dans le parent contient la clé racine du Merk enfant.

Dans une base de données traditionnelle, on stockerait les données dans un magasin clé-valeur plat avec
un seul arbre de Merkle (Merkle tree) au sommet pour l'authentification. GroveDB adopte une approche différente :
il imbrique des arbres de Merkle à l'intérieur d'autres arbres de Merkle. Cela vous donne :

1. **Des index secondaires efficaces** — interrogation par n'importe quel chemin, pas seulement par clé primaire
2. **Des preuves cryptographiques compactes** — prouver l'existence (ou l'absence) de toute donnée
3. **Des données agrégées** — les arbres peuvent automatiquement additionner, compter, ou autrement agréger
   leurs enfants
4. **Des opérations atomiques inter-arbres** — les opérations par lots s'étendent sur plusieurs sous-arbres

## Pourquoi GroveDB existe

GroveDB a été conçu pour **Dash Platform**, une plateforme d'applications décentralisées
où chaque élément d'état doit être :

- **Authentifié** : N'importe quel nœud peut prouver n'importe quel élément d'état à un client léger
- **Déterministe** : Chaque nœud calcule exactement la même racine d'état
- **Efficace** : Les opérations doivent se terminer dans les contraintes de temps de bloc
- **Interrogeable** : Les applications nécessitent des requêtes riches, pas seulement des recherches par clé

Les approches traditionnelles sont insuffisantes :

| Approche | Problème |
|----------|---------|
| Arbre de Merkle simple | Ne supporte que les recherches par clé, pas les requêtes par plage |
| MPT d'Ethereum | Rééquilibrage coûteux, preuves de grande taille |
| Clé-valeur plat + arbre unique | Pas de requêtes hiérarchiques, une seule preuve couvre tout |
| Arbre B | Pas naturellement « merklisé », authentification complexe |

GroveDB résout ces problèmes en combinant les **garanties d'équilibre éprouvées des arbres AVL**
avec l'**imbrication hiérarchique** et un **système de types d'éléments riche**.

## Vue d'ensemble de l'architecture

GroveDB est organisé en couches distinctes, chacune avec une responsabilité claire :

```mermaid
graph TD
    APP["<b>Couche application</b><br/>Dash Platform, etc.<br/><code>insert() get() query() prove() apply_batch()</code>"]

    GROVE["<b>Cœur de GroveDB</b> — <code>grovedb/src/</code><br/>Gestion hiérarchique des sous-arbres · Système de types d'éléments<br/>Résolution de références · Opérations par lots · Preuves multi-couches"]

    MERK["<b>Couche Merk</b> — <code>merk/src/</code><br/>Arbre AVL de Merkle · Rotations auto-équilibrantes<br/>Système de liens · Hachage Blake3 · Encodage des preuves"]

    STORAGE["<b>Couche de stockage</b> — <code>storage/src/</code><br/>RocksDB OptimisticTransactionDB<br/>4 familles de colonnes · Isolation par préfixe Blake3 · Écritures par lots"]

    COST["<b>Couche de coûts</b> — <code>costs/src/</code><br/>Suivi OperationCost · Monade CostContext<br/>Estimation pire cas &amp; cas moyen"]

    APP ==>|"écritures ↓"| GROVE
    GROVE ==>|"opérations sur arbre"| MERK
    MERK ==>|"E/S disque"| STORAGE
    STORAGE -.->|"accumulation des coûts ↑"| COST
    COST -.-> MERK
    MERK -.-> GROVE
    GROVE ==>|"lectures ↑"| APP

    style APP fill:#f5f5f5,stroke:#999,stroke-width:1px
    style GROVE fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style MERK fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style STORAGE fill:#fdebd0,stroke:#e67e22,stroke-width:2px
    style COST fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

Les données circulent vers le **bas** à travers ces couches lors des écritures et vers le **haut** lors des lectures.
Chaque opération accumule des coûts en traversant la pile, permettant une comptabilisation
précise des ressources.

---
