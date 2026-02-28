# Le hachage — Intégrité cryptographique

Chaque nœud d'un arbre Merk est haché pour produire un **hachage racine** (root hash) — une seule valeur de 32 octets
qui authentifie l'arbre entier. Tout changement dans une clé, une valeur, ou
une relation structurelle produira un hachage racine différent.

## Hiérarchie de hachage à trois niveaux

Merk utilise un schéma de hachage à trois niveaux, du plus interne au plus externe :

Exemple : key = `"bob"` (3 octets), value = `"hello"` (5 octets) :

```mermaid
graph LR
    subgraph level1["Niveau 1 : value_hash"]
        V_IN["varint(5) ‖ &quot;hello&quot;"]
        V_BLAKE["Blake3"]
        V_OUT(["value_hash<br/><small>32 octets</small>"])
        V_IN --> V_BLAKE --> V_OUT
    end

    subgraph level2["Niveau 2 : kv_hash"]
        K_IN["varint(3) ‖ &quot;bob&quot; ‖ value_hash"]
        K_BLAKE["Blake3"]
        K_OUT(["kv_hash<br/><small>32 octets</small>"])
        K_IN --> K_BLAKE --> K_OUT
    end

    subgraph level3["Niveau 3 : node_hash"]
        N_LEFT(["left_child_hash<br/><small>32o (ou NULL_HASH)</small>"])
        N_KV(["kv_hash<br/><small>32o</small>"])
        N_RIGHT(["right_child_hash<br/><small>32o (ou NULL_HASH)</small>"])
        N_BLAKE["Blake3<br/><small>entrée 96o = 2 blocs</small>"]
        N_OUT(["node_hash<br/><small>32 octets</small>"])
        N_LEFT --> N_BLAKE
        N_KV --> N_BLAKE
        N_RIGHT --> N_BLAKE
        N_BLAKE --> N_OUT
    end

    V_OUT -.-> K_IN
    K_OUT -.-> N_KV

    style level1 fill:#eaf2f8,stroke:#2980b9
    style level2 fill:#fef9e7,stroke:#f39c12
    style level3 fill:#fdedec,stroke:#e74c3c
```

> La RACINE de l'arbre = `node_hash` du nœud racine — authentifie **chaque** clé, valeur et relation structurelle. Les enfants manquants utilisent `NULL_HASH = [0x00; 32]`.

### Niveau 1 : value_hash

```rust
// merk/src/tree/hash.rs
pub fn value_hash(value: &[u8]) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    let val_length = value.len().encode_var_vec();  // Varint encoding
    hasher.update(val_length.as_slice());
    hasher.update(value);
    // ...
}
```

La longueur de la valeur est **encodée en varint** et préfixée. Ceci est crucial pour la
résistance aux collisions — sans cela, `H("AB" || "C")` serait égal à `H("A" || "BC")`.

### Niveau 2 : kv_hash

```rust
pub fn kv_hash(key: &[u8], value: &[u8]) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    let key_length = key.len().encode_var_vec();
    hasher.update(key_length.as_slice());
    hasher.update(key);
    let vh = value_hash(value);
    hasher.update(vh.as_slice());  // Nested hash
    // ...
}
```

Cela lie la clé à la valeur. Pour la vérification des preuves, il existe aussi une variante
qui prend un value_hash pré-calculé :

```rust
pub fn kv_digest_to_kv_hash(key: &[u8], value_hash: &CryptoHash) -> CostContext<CryptoHash>
```

Celle-ci est utilisée lorsque le vérificateur possède déjà le value_hash (par ex. pour les sous-arbres
où value_hash est un hachage combiné).

### Niveau 3 : node_hash

```rust
pub fn node_hash(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);       // 32 bytes
    hasher.update(left);     // 32 bytes
    hasher.update(right);    // 32 bytes — total 96 bytes
    // Always exactly 2 hash operations (96 bytes / 64-byte block = 2)
}
```

Si un enfant est absent, son hachage est le **NULL_HASH** — 32 octets à zéro :

```rust
pub const NULL_HASH: CryptoHash = [0; HASH_LENGTH];  // [0u8; 32]
```

## Blake3 comme fonction de hachage

GroveDB utilise **Blake3** pour tous les hachages. Propriétés clés :

- **Sortie de 256 bits** (32 octets)
- **Taille de bloc** : 64 octets
- **Vitesse** : environ 3 fois plus rapide que SHA-256 sur le matériel moderne
- **Flux** : peut alimenter les données de manière incrémentale

Le coût de l'opération de hachage est calculé en fonction du nombre de blocs de 64 octets
traités :

```rust
let hashes = 1 + (hasher.count() - 1) / 64;  // Number of hash operations
```

## Encodage par préfixe de longueur pour la résistance aux collisions

Chaque entrée de longueur variable est préfixée avec sa longueur en utilisant l'**encodage varint** :

```mermaid
graph LR
    subgraph bad["Sans préfixe de longueur — VULNÉRABLE"]
        BAD1["H(&quot;AB&quot; + &quot;C&quot;) = H(0x41 0x42 0x43)"]
        BAD2["H(&quot;A&quot; + &quot;BC&quot;) = H(0x41 0x42 0x43)"]
        BAD1 --- SAME["MÊME HACHAGE !"]
        BAD2 --- SAME
    end

    subgraph good["Avec préfixe de longueur — résistant aux collisions"]
        GOOD1["H([02] 0x41 0x42)<br/><small>varint(2) ‖ &quot;AB&quot;</small>"]
        GOOD2["H([03] 0x41 0x42 0x43)<br/><small>varint(3) ‖ &quot;ABC&quot;</small>"]
        GOOD1 --- DIFF["DIFFÉRENT"]
        GOOD2 --- DIFF
    end

    style bad fill:#fadbd8,stroke:#e74c3c
    style good fill:#d5f5e3,stroke:#27ae60
    style SAME fill:#e74c3c,color:#fff,stroke:#c0392b
    style DIFF fill:#27ae60,color:#fff,stroke:#229954
```

> **Entrée de value_hash** : `[varint(value.len)] [octets de la valeur]`
> **Entrée de kv_hash** : `[varint(key.len)] [octets de la clé] [value_hash : 32 octets]`

Sans préfixes de longueur, un attaquant pourrait fabriquer différentes paires clé-valeur qui
produisent le même condensat. Le préfixe de longueur rend cela cryptographiquement
infaisable.

## Hachage combiné pour les éléments spéciaux

Pour les **sous-arbres** et les **références**, le `value_hash` n'est pas simplement `H(value)`.
C'est plutôt un **hachage combiné** qui lie l'élément à sa cible :

```mermaid
graph LR
    subgraph item["Élément standard (Item)"]
        I_val["octets de la valeur"] --> I_hash["H(len ‖ octets)"] --> I_vh(["value_hash"])
    end

    subgraph subtree["Élément sous-arbre (Subtree)"]
        S_elem["octets de l'élément arbre"] --> S_hash1["H(len ‖ octets)"]
        S_root(["hachage racine<br/>du Merk enfant"])
        S_hash1 --> S_combine["combine_hash()<br/><small>Blake3(a ‖ b)</small>"]
        S_root --> S_combine
        S_combine --> S_vh(["value_hash"])
    end

    subgraph reference["Élément référence"]
        R_elem["octets de l'élément ref"] --> R_hash1["H(len ‖ octets)"]
        R_target["valeur cible"] --> R_hash2["H(len ‖ octets)"]
        R_hash1 --> R_combine["combine_hash()"]
        R_hash2 --> R_combine
        R_combine --> R_vh(["value_hash"])
    end

    style item fill:#eaf2f8,stroke:#2980b9
    style subtree fill:#fef9e7,stroke:#f39c12
    style reference fill:#fdedec,stroke:#e74c3c
```

> **Sous-arbre :** lie le hachage racine du Merk enfant dans le parent. **Référence :** lie à la fois le chemin de la référence ET la valeur cible. Modifier l'un ou l'autre change le hachage racine.

La fonction `combine_hash` :

```rust
pub fn combine_hash(hash_one: &CryptoHash, hash_two: &CryptoHash) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(hash_one);   // 32 bytes
    hasher.update(hash_two);   // 32 bytes — total 64 bytes, exactly 1 hash op
    // ...
}
```

C'est ce qui permet à GroveDB d'authentifier toute la hiérarchie à travers un seul
hachage racine — le value_hash de chaque arbre parent pour un élément sous-arbre inclut le
hachage racine de l'arbre enfant.

## Hachage agrégé pour ProvableCountTree

Les nœuds `ProvableCountTree` incluent le compteur agrégé dans le hachage du nœud :

```rust
pub fn node_hash_with_count(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
    count: u64,
) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);                        // 32 bytes
    hasher.update(left);                      // 32 bytes
    hasher.update(right);                     // 32 bytes
    hasher.update(&count.to_be_bytes());      // 8 bytes — total 104 bytes
    // Still exactly 2 hash ops (104 < 128 = 2 × 64)
}
```

Cela signifie qu'une preuve de comptage ne nécessite pas de révéler les données réelles — le compteur
est intégré dans l'engagement cryptographique.

---
