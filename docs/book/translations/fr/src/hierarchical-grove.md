# Le bosquet hiérarchique — Arbre d'arbres

## Comment les sous-arbres s'imbriquent dans les arbres parents

La caractéristique distinctive de GroveDB est qu'un arbre Merk peut contenir des éléments qui sont
eux-mêmes des arbres Merk. Cela crée un **espace de noms hiérarchique** :

```mermaid
graph TD
    subgraph root["ARBRE MERK RACINE — chemin : []"]
        contracts["&quot;contracts&quot;<br/>Tree"]
        identities["&quot;identities&quot;<br/>Tree"]
        balances["&quot;balances&quot;<br/>SumTree, sum=0"]
        contracts --> identities
        contracts --> balances
    end

    subgraph ident["MERK IDENTITÉS — chemin : [&quot;identities&quot;]"]
        bob456["&quot;bob456&quot;<br/>Tree"]
        alice123["&quot;alice123&quot;<br/>Tree"]
        eve["&quot;eve&quot;<br/>Item"]
        bob456 --> alice123
        bob456 --> eve
    end

    subgraph bal["MERK SOLDES (SumTree) — chemin : [&quot;balances&quot;]"]
        bob_bal["&quot;bob456&quot;<br/>SumItem(800)"]
    end

    subgraph alice_tree["MERK ALICE123 — chemin : [&quot;identities&quot;, &quot;alice123&quot;]"]
        name["&quot;name&quot;<br/>Item(&quot;Al&quot;)"]
        balance_item["&quot;balance&quot;<br/>SumItem(1000)"]
        docs["&quot;docs&quot;<br/>Tree"]
        name --> balance_item
        name --> docs
    end

    identities -.-> bob456
    balances -.-> bob_bal
    alice123 -.-> name
    docs -.->|"... plus de sous-arbres"| more["..."]

    style root fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style ident fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style bal fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style alice_tree fill:#e8daef,stroke:#8e44ad,stroke-width:2px
    style more fill:none,stroke:none
```

> Chaque boîte colorée est un arbre Merk distinct. Les flèches en pointillés représentent les liens portail des éléments Tree vers leurs arbres Merk enfants. Le chemin vers chaque Merk est indiqué dans son étiquette.

## Système d'adressage par chemin

Chaque élément dans GroveDB est adressé par un **chemin** (path) — une séquence de chaînes d'octets
qui navigue depuis la racine à travers les sous-arbres jusqu'à la clé cible :

```text
    Chemin : ["identities", "alice123", "name"]

    Étape 1 : Dans l'arbre racine, chercher "identities" → élément Tree
    Étape 2 : Ouvrir le sous-arbre identities, chercher "alice123" → élément Tree
    Étape 3 : Ouvrir le sous-arbre alice123, chercher "name" → Item("Alice")
```

Les chemins sont représentés comme `Vec<Vec<u8>>` ou en utilisant le type `SubtreePath` pour
une manipulation efficace sans allocation :

```rust
// The path to the element (all segments except the last)
let path: &[&[u8]] = &[b"identities", b"alice123"];
// The key within the final subtree
let key: &[u8] = b"name";
```

## Génération de préfixes Blake3 pour l'isolation du stockage

Chaque sous-arbre dans GroveDB obtient son propre **espace de noms de stockage isolé** dans RocksDB.
L'espace de noms est déterminé par le hachage du chemin avec Blake3 :

```rust
pub type SubtreePrefix = [u8; 32];

// The prefix is computed by hashing the path segments
// storage/src/rocksdb_storage/storage.rs
```

Par exemple :

```text
    Chemin : ["identities", "alice123"]
    Préfixe : Blake3(["identities", "alice123"]) = [0xab, 0x3f, ...]  (32 octets)

    Dans RocksDB, les clés de ce sous-arbre sont stockées comme :
    [préfixe : 32 octets][clé_originale]

    Donc "name" dans ce sous-arbre devient :
    [0xab, 0x3f, ...][0x6e, 0x61, 0x6d, 0x65]  ("name")
```

Cela garantit :
- Aucune collision de clés entre sous-arbres (préfixe de 32 octets = isolation sur 256 bits)
- Calcul de préfixe efficace (un seul hachage Blake3 sur les octets du chemin)
- Les données du sous-arbre sont colocalisées dans RocksDB pour l'efficacité du cache

## Propagation du hachage racine à travers la hiérarchie

Lorsqu'une valeur change en profondeur dans le bosquet, le changement doit se **propager vers le haut** pour
mettre à jour le hachage racine :

```text
    Changement : Mettre à jour "name" en "ALICE" dans identities/alice123/

    Étape 1 : Mettre à jour la valeur dans l'arbre Merk d'alice123
            → l'arbre alice123 obtient un nouveau hachage racine : H_alice_new

    Étape 2 : Mettre à jour l'élément "alice123" dans l'arbre identities
            → le value_hash de l'arbre identities pour "alice123" =
              combine_hash(H(tree_element_bytes), H_alice_new)
            → l'arbre identities obtient un nouveau hachage racine : H_ident_new

    Étape 3 : Mettre à jour l'élément "identities" dans l'arbre racine
            → le value_hash de l'arbre racine pour "identities" =
              combine_hash(H(tree_element_bytes), H_ident_new)
            → LE HACHAGE RACINE change
```

```mermaid
graph TD
    subgraph step3["ÉTAPE 3 : Mise à jour de l'arbre racine"]
        R3["L'arbre racine recalcule :<br/>value_hash pour &quot;identities&quot; =<br/>combine_hash(H(tree_bytes), H_ident_NEW)<br/>→ nouveau HACHAGE RACINE"]
    end
    subgraph step2["ÉTAPE 2 : Mise à jour de l'arbre identities"]
        R2["L'arbre identities recalcule :<br/>value_hash pour &quot;alice123&quot; =<br/>combine_hash(H(tree_bytes), H_alice_NEW)<br/>→ nouveau hachage racine : H_ident_NEW"]
    end
    subgraph step1["ÉTAPE 1 : Mise à jour du Merk alice123"]
        R1["L'arbre alice123 recalcule :<br/>value_hash(&quot;ALICE&quot;) → nouveau kv_hash<br/>→ nouveau hachage racine : H_alice_NEW"]
    end

    R1 -->|"H_alice_NEW remonte"| R2
    R2 -->|"H_ident_NEW remonte"| R3

    style step1 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style step2 fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style step3 fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

**Avant vs Après** — les nœuds modifiés sont marqués en rouge :

```mermaid
graph TD
    subgraph before["AVANT"]
        B_root["Racine : aabb1122"]
        B_ident["&quot;identities&quot; : cc44.."]
        B_contracts["&quot;contracts&quot; : 1234.."]
        B_balances["&quot;balances&quot; : 5678.."]
        B_alice["&quot;alice123&quot; : ee55.."]
        B_bob["&quot;bob456&quot; : bb22.."]
        B_name["&quot;name&quot; : 7f.."]
        B_docs["&quot;docs&quot; : a1.."]
        B_root --- B_ident
        B_root --- B_contracts
        B_root --- B_balances
        B_ident --- B_alice
        B_ident --- B_bob
        B_alice --- B_name
        B_alice --- B_docs
    end

    subgraph after["APRÈS"]
        A_root["Racine : ff990033"]
        A_ident["&quot;identities&quot; : dd88.."]
        A_contracts["&quot;contracts&quot; : 1234.."]
        A_balances["&quot;balances&quot; : 5678.."]
        A_alice["&quot;alice123&quot; : 1a2b.."]
        A_bob["&quot;bob456&quot; : bb22.."]
        A_name["&quot;name&quot; : 3c.."]
        A_docs["&quot;docs&quot; : a1.."]
        A_root --- A_ident
        A_root --- A_contracts
        A_root --- A_balances
        A_ident --- A_alice
        A_ident --- A_bob
        A_alice --- A_name
        A_alice --- A_docs
    end

    style A_root fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style A_ident fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style A_alice fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style A_name fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
```

> Seuls les nœuds sur le chemin de la valeur modifiée jusqu'à la racine sont recalculés. Les frères et les autres branches restent inchangés.

La propagation est implémentée par `propagate_changes_with_transaction`, qui remonte
le chemin depuis le sous-arbre modifié jusqu'à la racine, en mettant à jour le hachage de chaque élément parent
en cours de route.

## Exemple de structure de bosquet multi-niveaux

Voici un exemple complet montrant comment Dash Platform structure son état :

```mermaid
graph TD
    ROOT["Racine GroveDB"]

    ROOT --> contracts["[01] &quot;data_contracts&quot;<br/>Tree"]
    ROOT --> identities["[02] &quot;identities&quot;<br/>Tree"]
    ROOT --> balances["[03] &quot;balances&quot;<br/>SumTree"]
    ROOT --> pools["[04] &quot;pools&quot;<br/>Tree"]

    contracts --> c1["contract_id_1<br/>Tree"]
    contracts --> c2["contract_id_2<br/>Tree"]
    c1 --> docs["&quot;documents&quot;<br/>Tree"]
    docs --> profile["&quot;profile&quot;<br/>Tree"]
    docs --> note["&quot;note&quot;<br/>Tree"]
    profile --> d1["doc_id_1<br/>Item"]
    profile --> d2["doc_id_2<br/>Item"]
    note --> d3["doc_id_3<br/>Item"]

    identities --> id1["identity_id_1<br/>Tree"]
    identities --> id2["identity_id_2<br/>Tree"]
    id1 --> keys["&quot;keys&quot;<br/>Tree"]
    id1 --> rev["&quot;revision&quot;<br/>Item(u64)"]
    keys --> k1["key_id_1<br/>Item(pubkey)"]
    keys --> k2["key_id_2<br/>Item(pubkey)"]

    balances --> b1["identity_id_1<br/>SumItem(balance)"]
    balances --> b2["identity_id_2<br/>SumItem(balance)"]

    style ROOT fill:#2c3e50,stroke:#2c3e50,color:#fff
    style contracts fill:#d4e6f1,stroke:#2980b9
    style identities fill:#d5f5e3,stroke:#27ae60
    style balances fill:#fef9e7,stroke:#f39c12
    style pools fill:#e8daef,stroke:#8e44ad
```

Chaque boîte est un arbre Merk distinct, authentifié jusqu'à un unique hachage racine
sur lequel les validateurs s'accordent.

---
