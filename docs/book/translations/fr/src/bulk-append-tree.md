# Le BulkAppendTree — Stockage en ajout seulement à haut débit

Le BulkAppendTree est la réponse de GroveDB à un défi d'ingénierie spécifique : comment construire
un journal en ajout seulement à haut débit qui supporte des preuves de plage efficaces, minimise
le hachage par écriture, et produit des instantanés de chunks immuables adaptés à la distribution CDN ?

Alors qu'un MmrTree (chapitre 13) est idéal pour les preuves de feuilles individuelles, le BulkAppendTree
est conçu pour les charges de travail où des milliers de valeurs arrivent par bloc et les clients doivent
se synchroniser en récupérant des plages de données. Il y parvient grâce à une **architecture à deux niveaux** :
un tampon d'arbre de Merkle dense qui absorbe les ajouts entrants, et un MMR au niveau des chunks qui
enregistre les racines de chunks finalisés.

## L'architecture à deux niveaux

```text
┌────────────────────────────────────────────────────────────────┐
│                      BulkAppendTree                            │
│                                                                │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  MMR de chunks                                           │  │
│  │  ┌────┐ ┌────┐ ┌────┐ ┌────┐                            │  │
│  │  │ R0 │ │ R1 │ │ R2 │ │ H  │ ← Racines de Merkle dense │  │
│  │  └────┘ └────┘ └────┘ └────┘   de chaque blob de chunk  │  │
│  │                     hachages des pics emballés = racine MMR│  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  Tampon (DenseFixedSizedMerkleTree, capacité = 2^h - 1) │  │
│  │  ┌───┐ ┌───┐ ┌───┐                                      │  │
│  │  │v_0│ │v_1│ │v_2│ ... (remplit en ordre de niveau)      │  │
│  │  └───┘ └───┘ └───┘                                      │  │
│  │  dense_tree_root = hachage racine recalculé de l'arbre dense│  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                │
│  state_root = blake3("bulk_state" || mmr_root || dense_tree_root) │
│                                                                │
└────────────────────────────────────────────────────────────────┘
```

**Niveau 1 — Le tampon.** Les valeurs entrantes sont écrites dans un `DenseFixedSizedMerkleTree`
(voir chapitre 16). La capacité du tampon est de `2^height - 1` positions. Le hachage racine de l'arbre dense
(`dense_tree_root`) se met à jour après chaque insertion.

**Niveau 2 — Le MMR de chunks.** Quand le tampon est plein (atteint `chunk_size` entrées),
toutes les entrées sont sérialisées en un **blob de chunk** immuable, une racine de Merkle dense est
calculée sur ces entrées, et cette racine est ajoutée comme feuille au MMR de chunks.
Le tampon est ensuite vidé.

La **racine d'état** combine les deux niveaux en un seul engagement de 32 octets qui change
à chaque ajout, garantissant que l'arbre Merk parent reflète toujours le dernier état.

## Comment les valeurs remplissent le tampon

Chaque appel à `append()` suit cette séquence :

```text
Étape 1 : Écrire la valeur dans le tampon de l'arbre dense à la position suivante
        dense_tree.insert(value, store)

Étape 2 : Incrémenter total_count
        total_count += 1

Étape 3 : Vérifier si le tampon est plein (arbre dense à capacité)
        si dense_tree.count() == capacity:
            → déclencher la compaction (paragraphe 14.3)

Étape 4 : Calculer la nouvelle racine d'état (+1 appel blake3)
        dense_tree_root = dense_tree.root_hash(store)
        state_root = blake3("bulk_state" || mmr_root || dense_tree_root)
```

Le **tampon EST un DenseFixedSizedMerkleTree** (voir chapitre 16). Son hachage racine
change après chaque insertion, fournissant un engagement sur toutes les entrées actuelles du tampon.
Ce hachage racine est ce qui entre dans le calcul de la racine d'état.

## Compaction de chunks

Quand le tampon est plein (atteint `chunk_size` entrées), la compaction se déclenche automatiquement :

```text
Étapes de compaction :
─────────────────
1. Lire toutes les chunk_size entrées du tampon

2. Calculer la racine de Merkle dense
   - Hacher chaque entrée : leaf[i] = blake3(entry[i])
   - Construire l'arbre binaire complet du bas vers le haut
   - Extraire le hachage racine
   Coût de hachage : chunk_size + (chunk_size - 1) = 2 * chunk_size - 1

3. Sérialiser les entrées en blob de chunk
   - Sélectionne automatiquement le format taille fixe ou taille variable (paragraphe 14.6)
   - Stocker comme : store.put(chunk_key(chunk_index), blob)

4. Ajouter la racine de Merkle dense au MMR de chunks
   - Push MMR avec cascade de fusions (voir chapitre 13)
   Coût de hachage : ~2 amorti (patron trailing_ones)

5. Réinitialiser l'arbre dense (effacer toutes les entrées du tampon du stockage)
   - Compteur de l'arbre dense remis à 0
```

Après la compaction, le blob de chunk est **définitivement immuable** — il ne change plus
jamais. Cela rend les blobs de chunks idéaux pour la mise en cache CDN, la synchronisation client et le stockage
d'archivage.

**Exemple : 4 ajouts avec chunk_power=2 (chunk_size=4)**

```text
Ajout v_0 : dense_tree=[v_0],       dense_root=H(v_0), total=1
Ajout v_1 : dense_tree=[v_0,v_1],   dense_root=H(v_0,v_1), total=2
Ajout v_2 : dense_tree=[v_0..v_2],  dense_root=H(v_0..v_2), total=3
Ajout v_3 : dense_tree=[v_0..v_3],  dense_root=H(v_0..v_3), total=4
  → COMPACTION :
    chunk_blob_0 = serialize([v_0, v_1, v_2, v_3])
    dense_root_0 = dense_merkle_root(v_0..v_3)
    mmr.push(dense_root_0)
    arbre dense vidé (count=0)

Ajout v_4 : dense_tree=[v_4],       dense_root=H(v_4), total=5
  → state_root = blake3("bulk_state" || mmr_root || dense_root)
```

## La racine d'état

La racine d'état lie les deux niveaux en un seul hachage :

```rust
fn compute_state_root(
    mmr_root: &[u8; 32],         // Chunk MMR root (or [0;32] if empty)
    dense_tree_root: &[u8; 32],  // Root hash of current buffer (dense tree)
) -> [u8; 32] {
    blake3("bulk_state" || mmr_root || dense_tree_root)
}
```

Le `total_count` et `chunk_power` ne sont **pas** inclus dans la racine d'état car
ils sont déjà authentifiés par le hachage de valeur Merk — ce sont des champs de
l'`Element` sérialisé stocké dans le nœud Merk parent. La racine d'état capture uniquement les
engagements au niveau des données (`mmr_root` et `dense_tree_root`). C'est le hachage qui
circule comme le hachage enfant Merk et se propage jusqu'au hachage racine de GroveDB.

## La racine de Merkle dense

Quand un chunk se compacte, les entrées ont besoin d'un unique engagement de 32 octets. Le
BulkAppendTree utilise un **arbre binaire de Merkle dense (complet)** :

```text
Étant donné les entrées [e_0, e_1, e_2, e_3] :

Niveau 0 (feuilles) :  blake3(e_0)  blake3(e_1)  blake3(e_2)  blake3(e_3)
                          \__________/              \__________/
Niveau 1 :            blake3(h_0 || h_1)       blake3(h_2 || h_3)
                              \____________________/
Niveau 2 (racine) :   blake3(h_01 || h_23)  ← c'est la racine de Merkle dense
```

Comme `chunk_size` est toujours une puissance de 2 (par construction : `1u32 << chunk_power`),
l'arbre est toujours complet (pas de remplissage ou de feuilles factices nécessaires). Le nombre de hachages est
exactement `2 * chunk_size - 1` :
- `chunk_size` hachages de feuilles (un par entrée)
- `chunk_size - 1` hachages de nœuds internes

L'implémentation de la racine de Merkle dense se trouve dans `grovedb-mmr/src/dense_merkle.rs` et
fournit deux fonctions :
- `compute_dense_merkle_root(hashes)` — depuis des feuilles pré-hachées
- `compute_dense_merkle_root_from_values(values)` — hache les valeurs d'abord, puis construit
  l'arbre

## Sérialisation des blobs de chunks

Les blobs de chunks sont les archives immuables produites par la compaction. Le sérialiseur
sélectionne automatiquement le format le plus compact basé sur les tailles des entrées :

**Format taille fixe** (drapeau `0x01`) — quand toutes les entrées ont la même longueur :

```text
┌──────┬──────────┬─────────────┬─────────┬─────────┬─────────┐
│ 0x01 │ count    │ entry_size  │ entry_0 │ entry_1 │ ...     │
│ 1o   │ 4o (BE)  │ 4o (BE)     │ N oct.  │ N oct.  │         │
└──────┴──────────┴─────────────┴─────────┴─────────┴─────────┘
Total : 1 + 4 + 4 + (count × entry_size) octets
```

**Format taille variable** (drapeau `0x00`) — quand les entrées ont des longueurs différentes :

```text
┌──────┬──────────┬─────────┬──────────┬─────────┬─────────────┐
│ 0x00 │ len_0    │ entry_0 │ len_1    │ entry_1 │ ...         │
│ 1o   │ 4o (BE)  │ N oct.  │ 4o (BE)  │ M oct.  │             │
└──────┴──────────┴─────────┴──────────┴─────────┴─────────────┘
Total : 1 + Σ(4 + len_i) octets
```

Le format taille fixe économise 4 octets par entrée par rapport au format variable, ce qui s'accumule
significativement pour de grands chunks de données de taille uniforme (comme les engagements de hachage de 32 octets).
Pour 1024 entrées de 32 octets chacune :
- Fixe : `1 + 4 + 4 + 32768 = 32 777 octets`
- Variable : `1 + 1024 × (4 + 32) = 36 865 octets`
- Économie : ~11 %

## Disposition des clés de stockage

Toutes les données du BulkAppendTree résident dans l'espace de noms **data**, identifiées par des préfixes à un seul caractère :

| Patron de clé | Format | Taille | Objectif |
|---|---|---|---|
| `M` | 1 octet | 1o | Clé de métadonnées |
| `b` + `{index}` | `b` + u32 BE | 5o | Entrée du tampon à l'index |
| `e` + `{index}` | `e` + u64 BE | 9o | Blob de chunk à l'index |
| `m` + `{pos}` | `m` + u64 BE | 9o | Nœud MMR à la position |

Les **métadonnées** stockent `mmr_size` (8 octets BE). Le `total_count` et `chunk_power` sont
stockés dans l'Element lui-même (dans le Merk parent), pas dans les métadonnées de l'espace de noms data.
Cette séparation signifie que la lecture du compteur est une simple recherche d'élément sans ouvrir le
contexte de stockage data.

Les clés du tampon utilisent des indices u32 (0 à `chunk_size - 1`) car la capacité du tampon est
limitée par `chunk_size` (un u32, calculé comme `1u32 << chunk_power`). Les clés de chunk utilisent des
indices u64 car le nombre de chunks terminés peut croître indéfiniment.

## La structure BulkAppendTree

```rust
pub struct BulkAppendTree<S> {
    pub total_count: u64,                        // Total values ever appended
    pub dense_tree: DenseFixedSizedMerkleTree<S>, // The buffer (dense tree)
}
```

Le tampon EST un `DenseFixedSizedMerkleTree` — son hachage racine est `dense_tree_root`.

**Accesseurs :**
- `capacity() -> u16` : `dense_tree.capacity()` (= `2^height - 1`)
- `epoch_size() -> u64` : `capacity + 1` (= `2^height`, le nombre d'entrées par chunk)
- `height() -> u8` : `dense_tree.height()`

**Valeurs dérivées** (non stockées) :

| Valeur | Formule |
|---|---|
| `chunk_count` | `total_count / epoch_size` |
| `buffer_count` | `dense_tree.count()` |
| `mmr_size` | `leaf_count_to_mmr_size(chunk_count)` |

## Opérations GroveDB

Le BulkAppendTree s'intègre avec GroveDB via six opérations définies dans
`grovedb/src/operations/bulk_append_tree.rs` :

### bulk_append

L'opération de mutation principale. Suit le patron standard de stockage non-Merk de GroveDB :

```text
1. Valider que l'élément est un BulkAppendTree
2. Ouvrir le contexte de stockage data
3. Charger l'arbre depuis le magasin
4. Ajouter la valeur (peut déclencher la compaction)
5. Mettre à jour l'élément dans le Merk parent avec la nouvelle state_root + total_count
6. Propager les changements vers le haut à travers la hiérarchie Merk
7. Valider la transaction
```

L'adaptateur `AuxBulkStore` enveloppe les appels `get_aux`/`put_aux`/`delete_aux` de GroveDB et
accumule les `OperationCost` dans un `RefCell` pour le suivi des coûts. Les coûts de hachage de
l'opération d'ajout sont ajoutés à `cost.hash_node_calls`.

### Opérations de lecture

| Opération | Ce qu'elle retourne | Stockage aux ? |
|---|---|---|
| `bulk_get_value(path, key, position)` | Valeur à la position globale | Oui — lit depuis le blob de chunk ou le tampon |
| `bulk_get_chunk(path, key, chunk_index)` | Blob de chunk brut | Oui — lit la clé de chunk |
| `bulk_get_buffer(path, key)` | Toutes les entrées actuelles du tampon | Oui — lit les clés du tampon |
| `bulk_count(path, key)` | Compteur total (u64) | Non — lit depuis l'élément |
| `bulk_chunk_count(path, key)` | Chunks terminés (u64) | Non — calculé depuis l'élément |

L'opération `get_value` route de manière transparente par position :

```text
si position < chunks_terminés × chunk_size :
    chunk_idx = position / chunk_size
    intra_idx = position % chunk_size
    → lire le blob de chunk, désérialiser, retourner entry[intra_idx]
sinon :
    buffer_idx = position % chunk_size
    → lire buffer_key(buffer_idx)
```

## Opérations par lots et prétraitement

Le BulkAppendTree supporte les opérations par lots via la variante `GroveOp::BulkAppend`.
Puisque `execute_ops_on_path` n'a pas accès au contexte de stockage data, toutes les ops BulkAppend
doivent être prétraitées avant `apply_body`.

Le pipeline de prétraitement :

```text
Entrée : [BulkAppend{v1}, Insert{...}, BulkAppend{v2}, BulkAppend{v3}]
                                     ↑ même (path,key) que v1

Étape 1 : Grouper les ops BulkAppend par (path, key)
        group_1 : [v1, v2, v3]

Étape 2 : Pour chaque groupe :
        a. Lire l'élément existant → obtenir (total_count, chunk_power)
        b. Ouvrir le contexte de stockage transactionnel
        c. Charger le BulkAppendTree depuis le magasin
        d. Charger le tampon existant en mémoire (Vec<Vec<u8>>)
        e. Pour chaque valeur :
           tree.append_with_mem_buffer(store, value, &mut mem_buffer)
        f. Sauvegarder les métadonnées
        g. Calculer la state_root finale

Étape 3 : Remplacer toutes les ops BulkAppend par un ReplaceNonMerkTreeRoot par groupe
        portant : hash=state_root, meta=BulkAppendTree{total_count, chunk_power}

Sortie : [ReplaceNonMerkTreeRoot{...}, Insert{...}]
```

La variante `append_with_mem_buffer` évite les problèmes de lecture après écriture : les entrées du tampon
sont suivies dans un `Vec<Vec<u8>>` en mémoire, donc la compaction peut les lire même si
le stockage transactionnel n'a pas encore été validé.

## Le trait BulkStore

```rust
pub trait BulkStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String>;
    fn delete(&self, key: &[u8]) -> Result<(), String>;
}
```

Les méthodes prennent `&self` (pas `&mut self`) pour correspondre au patron de mutabilité intérieure de GroveDB
où les écritures passent par un lot. L'intégration GroveDB implémente cela via
`AuxBulkStore` qui enveloppe un `StorageContext` et accumule les `OperationCost`.

Le `MmrAdapter` fait le pont entre `BulkStore` et les traits `MMRStoreReadOps`/
`MMRStoreWriteOps` du MMR ckb, ajoutant un cache de lecture après écriture pour la
cohérence.

## Génération de preuves

Les preuves BulkAppendTree supportent les **requêtes de plage** sur les positions. La structure de preuve
capture tout ce dont un vérificateur sans état a besoin pour confirmer que des données spécifiques
existent dans l'arbre :

```rust
pub struct BulkAppendTreeProof {
    pub chunk_power: u8,
    pub total_count: u64,
    pub chunk_blobs: Vec<(u64, Vec<u8>)>,         // Full chunk blobs
    pub chunk_mmr_size: u64,
    pub chunk_mmr_proof_items: Vec<[u8; 32]>,      // MMR sibling hashes
    pub chunk_mmr_leaves: Vec<(u64, [u8; 32])>,    // (leaf_idx, dense_root)
    pub buffer_entries: Vec<Vec<u8>>,               // ALL buffer entries
    pub chunk_mmr_root: [u8; 32],
}
```

**Étapes de génération** pour une plage `[start, end)` (avec `chunk_size = 1u32 << chunk_power`) :

```text
1. Déterminer les chunks chevauchants
   first_chunk = start / chunk_size
   last_chunk  = min((end-1) / chunk_size, completed_chunks - 1)

2. Lire les blobs de chunk pour les chunks chevauchants
   Pour chaque chunk_idx dans [first_chunk, last_chunk] :
     chunk_blobs.push((chunk_idx, store.get(chunk_key(idx))))

3. Calculer la racine de Merkle dense pour chaque blob de chunk
   Pour chaque blob :
     désérialiser → valeurs
     dense_root = compute_dense_merkle_root_from_values(values)
     chunk_mmr_leaves.push((chunk_idx, dense_root))

4. Générer la preuve MMR pour ces positions de chunks
   positions = chunk_indices.map(|idx| leaf_to_pos(idx))
   proof = mmr.gen_proof(positions)
   chunk_mmr_proof_items = proof.proof_items().map(|n| n.hash)

5. Obtenir la racine du MMR de chunks

6. Lire TOUTES les entrées du tampon (bornées par chunk_size)
   pour i dans 0..buffer_count :
     buffer_entries.push(store.get(buffer_key(i)))
```

**Pourquoi inclure TOUTES les entrées du tampon ?** Le tampon est un arbre de Merkle dense dont le hachage racine
engage sur chaque entrée. Le vérificateur doit reconstruire l'arbre depuis toutes les entrées pour vérifier
le `dense_tree_root`. Comme le tampon est borné par `capacity` (au plus 65 535
entrées), c'est un coût raisonnable.

## Vérification des preuves

La vérification est une fonction pure — aucun accès à la base de données n'est nécessaire. Elle effectue cinq vérifications :

```text
Étape 0 : Cohérence des métadonnées
        - chunk_power <= 31
        - buffer_entries.len() == total_count % chunk_size
        - Nombre de feuilles MMR == completed_chunks

Étape 1 : Intégrité des blobs de chunks
        Pour chaque (chunk_idx, blob) :
          values = deserialize(blob)
          computed_root = dense_merkle_root(values)
          assert computed_root == chunk_mmr_leaves[chunk_idx]

Étape 2 : Preuve MMR de chunks
        Reconstruire les feuilles MmrNode et les éléments de preuve
        proof.verify(chunk_mmr_root, leaves) == true

Étape 3 : Intégrité du tampon (arbre dense)
        Reconstruire le DenseFixedSizedMerkleTree depuis buffer_entries
        dense_tree_root = calculer le hachage racine de l'arbre reconstruit

Étape 4 : Racine d'état
        computed = blake3("bulk_state" || chunk_mmr_root || dense_tree_root)
        assert computed == expected_state_root
```

Après une vérification réussie, le `BulkAppendTreeProofResult` fournit une
méthode `values_in_range(start, end)` qui extrait des valeurs spécifiques des
blobs de chunks et entrées de tampon vérifiés.

## Comment cela se relie au hachage racine de GroveDB

Le BulkAppendTree est un **arbre non-Merk** — il stocke les données dans l'espace de noms data,
pas dans un sous-arbre Merk enfant. Dans le Merk parent, l'élément est stocké comme :

```text
Element::BulkAppendTree(total_count, chunk_power, flags)
```

La racine d'état circule comme le hachage enfant Merk. Le hachage du nœud Merk parent est :

```text
combine_hash(value_hash(element_bytes), state_root)
```

La `state_root` circule comme le hachage enfant Merk (via le paramètre `subtree_root_hash`
de `insert_subtree`). Tout changement de la racine d'état se propage vers le haut à travers
la hiérarchie Merk de GroveDB jusqu'au hachage racine.

Dans les preuves V1 (paragraphe 9.6), la preuve Merk parente prouve les octets de l'élément et la liaison
du hachage enfant, et la `BulkAppendTreeProof` prouve que les données interrogées sont cohérentes
avec la `state_root` utilisée comme hachage enfant.

## Suivi des coûts

Le coût de hachage de chaque opération est suivi explicitement :

| Opération | Appels Blake3 | Notes |
|---|---|---|
| Ajout unique (sans compaction) | 3 | 2 pour la chaîne de hachage du tampon + 1 pour la racine d'état |
| Ajout unique (avec compaction) | 3 + 2C - 1 + ~2 | Chaîne + Merkle dense (C=chunk_size) + push MMR + racine d'état |
| `get_value` depuis un chunk | 0 | Désérialisation pure, pas de hachage |
| `get_value` depuis le tampon | 0 | Recherche directe par clé |
| Génération de preuve | Dépend du nombre de chunks | Racine Merkle dense par chunk + preuve MMR |
| Vérification de preuve | 2C·K - K + B·2 + 1 | K chunks, B entrées de tampon, C chunk_size |

**Coût amorti par ajout** : Pour chunk_size=1024 (chunk_power=10), le surcoût de compaction d'environ 2047
hachages (racine Merkle dense) est amorti sur 1024 ajouts, ajoutant environ 2 hachages par
ajout. Combiné avec les 3 hachages par ajout, le total amorti est d'**environ 5 appels Blake3
par ajout** — très efficace pour une structure authentifiée cryptographiquement.

## Comparaison avec MmrTree

| | BulkAppendTree | MmrTree |
|---|---|---|
| **Architecture** | Deux niveaux (tampon + MMR de chunks) | MMR unique |
| **Coût de hachage par ajout** | 3 (+ amorti ~2 pour compaction) | ~2 |
| **Granularité des preuves** | Requêtes de plage sur positions | Preuves de feuilles individuelles |
| **Instantanés immuables** | Oui (blobs de chunks) | Non |
| **Compatible CDN** | Oui (blobs de chunks cacheables) | Non |
| **Entrées de tampon** | Oui (toutes nécessaires pour la preuve) | N/A |
| **Idéal pour** | Journaux à haut débit, synchronisation en masse | Journaux d'événements, recherches individuelles |
| **Discriminant d'élément** | 13 | 12 |
| **TreeType** | 9 | 8 |

Choisissez MmrTree quand vous avez besoin de preuves de feuilles individuelles avec un surcoût minimal. Choisissez
BulkAppendTree quand vous avez besoin de requêtes de plage, de synchronisation en masse et d'instantanés basés
sur les chunks.

## Fichiers d'implémentation

| Fichier | Objectif |
|------|---------|
| `grovedb-bulk-append-tree/src/lib.rs` | Racine du crate, ré-exportations |
| `grovedb-bulk-append-tree/src/tree/mod.rs` | Structure `BulkAppendTree`, accesseurs d'état, persistance des métadonnées |
| `grovedb-bulk-append-tree/src/tree/append.rs` | `append()`, `append_with_mem_buffer()`, `compact_entries()` |
| `grovedb-bulk-append-tree/src/tree/hash.rs` | `compute_state_root` |
| `grovedb-bulk-append-tree/src/tree/keys.rs` | `META_KEY`, `buffer_key`, `chunk_key`, `mmr_node_key` |
| `grovedb-bulk-append-tree/src/tree/query.rs` | `get_value`, `get_chunk`, `get_buffer` |
| `grovedb-bulk-append-tree/src/tree/mmr_adapter.rs` | `MmrAdapter` avec cache de lecture après écriture |
| `grovedb-bulk-append-tree/src/chunk.rs` | Sérialisation de blobs de chunks (formats fixe + variable) |
| `grovedb-bulk-append-tree/src/proof.rs` | Génération et vérification de `BulkAppendTreeProof` |
| `grovedb-bulk-append-tree/src/store.rs` | Trait `BulkStore` |
| `grovedb-bulk-append-tree/src/error.rs` | Énumération `BulkAppendError` |
| `grovedb/src/operations/bulk_append_tree.rs` | Opérations GroveDB, `AuxBulkStore`, prétraitement par lots |
| `grovedb/src/tests/bulk_append_tree_tests.rs` | 27 tests d'intégration |

---
