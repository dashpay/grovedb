# Le DenseAppendOnlyFixedSizeTree — Stockage Merkle dense a capacite fixe

Le DenseAppendOnlyFixedSizeTree (arbre dense a ajout seulement et taille fixe) est un arbre binaire complet de hauteur fixe ou
**chaque noeud** — interne et feuille — stocke une valeur de donnees. Les positions sont remplies
sequentiellement en ordre de niveau (BFS) : la racine d'abord (position 0), puis de gauche a droite a chaque
niveau. Aucun hachage intermediaire n'est persiste ; le hachage racine est recalcule a la volee en
hachant recursivement des feuilles vers la racine.

Cette conception est ideale pour les petites structures de donnees bornees ou la capacite maximale est
connue a l'avance et ou l'on a besoin d'un ajout en O(1), d'une recuperation par position en O(1), et d'un
engagement compact de 32 octets sous forme de hachage racine qui change apres chaque insertion.

## Structure de l'arbre

Un arbre de hauteur *h* a une capacite de `2^h - 1` positions. Les positions utilisent un indexage
en ordre de niveau base a 0 :

```text
Height 3 tree (capacity = 7):

              pos 0          ← root (level 0)
             /     \
          pos 1    pos 2     ← level 1
         /   \    /   \
       pos 3 pos 4 pos 5 pos 6  ← level 2 (leaves)

Navigation:
  left_child(i)  = 2i + 1
  right_child(i) = 2i + 2
  parent(i)      = (i - 1) / 2
  is_leaf(i)     = 2i + 1 >= capacity
```

Les valeurs sont ajoutees sequentiellement : la premiere valeur va a la position 0 (racine), puis
position 1, 2, 3, et ainsi de suite. Cela signifie que la racine a toujours des donnees, et l'arbre se remplit
en ordre de niveau — l'ordre de parcours le plus naturel pour un arbre binaire complet.

## Calcul du hachage

Le hachage racine n'est pas stocke separement — il est recalcule a partir de zero quand c'est necessaire.
L'algorithme recursif ne visite que les positions remplies :

```text
hash(position, store):
  value = store.get(position)

  if position is unfilled (>= count):
    return [0; 32]                                    ← empty sentinel

  value_hash = blake3(value)
  left_hash  = hash(2 * position + 1, store)
  right_hash = hash(2 * position + 2, store)
  return blake3(value_hash || left_hash || right_hash)
```

**Proprietes cles :**
- Tous les noeuds (feuille et interne) : `blake3(blake3(value) || H(left) || H(right))`
- Noeuds feuilles : left_hash et right_hash sont tous deux `[0; 32]` (enfants non remplis)
- Positions non remplies : `[0u8; 32]` (hachage zero)
- Arbre vide (count = 0) : `[0u8; 32]`

**Aucune etiquette de separation de domaine feuille/interne n'est utilisee.** La structure de l'arbre (`height`
et `count`) est authentifiee de maniere externe dans l'element parent `Element::DenseAppendOnlyFixedSizeTree`,
qui circule a travers la hierarchie Merk. Le verificateur sait toujours exactement quelles
positions sont des feuilles par rapport aux noeuds internes grace a la hauteur et au nombre, donc un attaquant
ne peut pas substituer l'un par l'autre sans briser la chaine d'authentification parente.

Cela signifie que le hachage racine encode un engagement envers chaque valeur stockee et sa position
exacte dans l'arbre. Modifier une valeur (si c'etait possible) se propagerait en cascade a travers
tous les hachages des ancetres jusqu'a la racine.

**Cout du hachage :** Le calcul du hachage racine visite toutes les positions remplies plus tout
enfant non rempli. Pour un arbre avec *n* valeurs, le pire cas est O(*n*) appels blake3. C'est
acceptable car l'arbre est concu pour de petites capacites bornees (hauteur maximale 16,
maximum 65 535 positions).

## La variante Element

```rust
Element::DenseAppendOnlyFixedSizeTree(
    u16,                   // count — number of values stored (max 65,535)
    u8,                    // height — immutable after creation (1..=16)
    Option<ElementFlags>,  // flags — storage flags
)
```

| Champ | Type | Description |
|---|---|---|
| `count` | `u16` | Nombre de valeurs inserees jusqu'a present (max 65 535) |
| `height` | `u8` | Hauteur de l'arbre (1..=16), immuable apres creation |
| `flags` | `Option<ElementFlags>` | Drapeaux de stockage optionnels |

Le hachage racine N'EST PAS stocke dans l'Element — il circule comme le hachage enfant du Merk
via le parametre `subtree_root_hash` de `insert_subtree`.

**Discriminant :** 14 (ElementType), TreeType = 10

**Taille de cout :** `DENSE_TREE_COST_SIZE = 6` octets (2 count + 1 height + 1 discriminant
+ 2 surcharge)

## Disposition du stockage

Comme MmrTree et BulkAppendTree, le DenseAppendOnlyFixedSizeTree stocke les donnees dans
l'espace de noms de **donnees** (pas un Merk enfant). Les valeurs sont indexees par leur position en tant que `u64` gros-boutiste :

```text
Subtree path: blake3(parent_path || key)

Storage keys:
  [0, 0, 0, 0, 0, 0, 0, 0] → value at position 0 (root)
  [0, 0, 0, 0, 0, 0, 0, 1] → value at position 1
  [0, 0, 0, 0, 0, 0, 0, 2] → value at position 2
  ...
```

L'Element lui-meme (stocke dans le Merk parent) porte le `count` et la `height`.
Le hachage racine circule comme le hachage enfant du Merk. Cela signifie :
- **Lire le hachage racine** necessite un recalcul depuis le stockage (hachage en O(n))
- **Lire une valeur par position est O(1)** — une seule recherche en stockage
- **L'insertion necessite un hachage en O(n)** — une ecriture en stockage + recalcul complet du hachage racine

## Operations

### `dense_tree_insert(path, key, value, tx, grove_version)`

Ajoute une valeur a la prochaine position disponible. Retourne `(root_hash, position)`.

```text
Step 1: Read element, extract (count, height)
Step 2: Check capacity: if count >= 2^height - 1 → error
Step 3: Build subtree path, open storage context
Step 4: Write value to position = count
Step 5: Reconstruct DenseFixedSizedMerkleTree from state
Step 6: Call tree.insert(value, store) → (root_hash, position, hash_calls)
Step 7: Update element with new root_hash and count + 1
Step 8: Propagate changes up through Merk hierarchy
Step 9: Commit transaction
```

### `dense_tree_get(path, key, position, tx, grove_version)`

Recupere la valeur a une position donnee. Retourne `None` si la position >= count.

### `dense_tree_root_hash(path, key, tx, grove_version)`

Retourne le hachage racine stocke dans l'element. C'est le hachage calcule lors de la
derniere insertion — aucun recalcul necessaire.

### `dense_tree_count(path, key, tx, grove_version)`

Retourne le nombre de valeurs stockees (le champ `count` de l'element).

## Operations par lots

La variante `GroveOp::DenseTreeInsert` supporte l'insertion par lots a travers le pipeline
de lots standard de GroveDB :

```rust
let ops = vec![
    QualifiedGroveDbOp::dense_tree_insert_op(
        vec![b"parent".to_vec()],
        b"my_dense_tree".to_vec(),
        b"value_data".to_vec(),
    ),
];
db.apply_batch(ops, None, None, grove_version)?;
```

**Pretraitement :** Comme tous les types d'arbres non-Merk, les operations `DenseTreeInsert` sont pretraitees
avant l'execution du corps principal du lot. La methode `preprocess_dense_tree_ops` :

1. Regroupe toutes les operations `DenseTreeInsert` par `(path, key)`
2. Pour chaque groupe, execute les insertions sequentiellement (lecture de l'element, insertion
   de chaque valeur, mise a jour du hachage racine)
3. Convertit chaque groupe en une operation `ReplaceNonMerkTreeRoot` qui transporte le
   `root_hash` final et le `count` a travers la machinerie de propagation standard

Plusieurs insertions dans le meme arbre dense au sein d'un seul lot sont supportees — elles
sont traitees dans l'ordre et la verification de coherence autorise les cles dupliquees pour ce type d'operation.

**Propagation :** Le hachage racine et le nombre circulent a travers la variante `NonMerkTreeMeta::DenseTree`
dans `ReplaceNonMerkTreeRoot`, suivant le meme patron que MmrTree et
BulkAppendTree.

## Preuves

Le DenseAppendOnlyFixedSizeTree supporte les **preuves de sous-requetes V1** via la variante `ProofBytes::DenseTree`.
Les positions individuelles peuvent etre prouvees contre le hachage racine de l'arbre en utilisant des preuves
d'inclusion qui transportent les valeurs des ancetres et les hachages des sous-arbres freres.

### Structure du chemin d'authentification

Comme les noeuds internes hachent leur **propre valeur** (pas seulement les hachages des enfants), le
chemin d'authentification differe d'un arbre de Merkle standard. Pour verifier une feuille a la position
`p`, le verificateur a besoin de :

1. **La valeur de la feuille** (l'entree prouvee)
2. **Les hachages de valeur des ancetres** pour chaque noeud interne sur le chemin de `p` a la racine (seulement le hachage de 32 octets, pas la valeur complete)
3. **Les hachages des sous-arbres freres** pour chaque enfant qui N'EST PAS sur le chemin

Puisque tous les noeuds utilisent `blake3(H(value) || H(left) || H(right))` (pas d'etiquettes de domaine),
la preuve ne transporte que des hachages de valeur de 32 octets pour les ancetres — pas les valeurs completes. Cela
garde les preuves compactes quelle que soit la taille des valeurs individuelles.

```rust
pub struct DenseTreeProof {
    pub entries: Vec<(u16, Vec<u8>)>,            // proved (position, value) pairs
    pub node_value_hashes: Vec<(u16, [u8; 32])>, // ancestor value hashes on the auth path
    pub node_hashes: Vec<(u16, [u8; 32])>,       // precomputed sibling subtree hashes
}
```

> **Note :** `height` et `count` ne sont pas dans la structure de preuve — le verificateur les obtient de l'Element parent, qui est authentifie par la hierarchie Merk.

### Exemple detaille

Arbre avec height=3, capacity=7, count=5, preuve de la position 4 :

```text
        0
       / \
      1   2
     / \ / \
    3  4 5  6
```

Chemin de 4 a la racine : `4 → 1 → 0`. Ensemble elargi : `{0, 1, 4}`.

La preuve contient :
- **entries** : `[(4, value[4])]` — la position prouvee
- **node_value_hashes** : `[(0, H(value[0])), (1, H(value[1]))]` — hachages des valeurs des ancetres (32 octets chacun, pas les valeurs completes)
- **node_hashes** : `[(2, H(subtree_2)), (3, H(node_3))]` — freres et soeurs hors du chemin

La verification recalcule le hachage racine de bas en haut :
1. `H(4) = blake3(blake3(value[4]) || [0;32] || [0;32])` — feuille (les enfants ne sont pas remplis)
2. `H(3)` — depuis `node_hashes`
3. `H(1) = blake3(H(value[1]) || H(3) || H(4))` — l'interne utilise le hachage de valeur depuis `node_value_hashes`
4. `H(2)` — depuis `node_hashes`
5. `H(0) = blake3(H(value[0]) || H(1) || H(2))` — la racine utilise le hachage de valeur depuis `node_value_hashes`
6. Comparer `H(0)` avec le hachage racine attendu

### Preuves multi-positions

Lors de la preuve de plusieurs positions, l'ensemble elargi fusionne les chemins d'authentification qui se chevauchent. Les
ancetres partages ne sont inclus qu'une seule fois, rendant les preuves multi-positions plus compactes que
des preuves independantes a position unique.

### Limitation V0

Les preuves V0 ne peuvent pas descendre dans les arbres denses. Si une requete V0 correspond a un
`DenseAppendOnlyFixedSizeTree` avec une sous-requete, le systeme retourne
`Error::NotSupported` orientant l'appelant vers `prove_query_v1`.

### Encodage des cles de requete

Les positions d'arbre dense sont encodees en **u16 gros-boutiste** (2 octets) comme cles de requete, contrairement
au MmrTree et BulkAppendTree qui utilisent u64. Tous les types standard de `QueryItem` avec des plages
sont supportes.

## Comparaison avec les autres arbres non-Merk

| | DenseTree | MmrTree | BulkAppendTree | CommitmentTree |
|---|---|---|---|---|
| **Discriminant de l'element** | 14 | 12 | 13 | 11 |
| **TreeType** | 10 | 8 | 9 | 7 |
| **Capacite** | Fixe (`2^h - 1`, max 65 535) | Illimitee | Illimitee | Illimitee |
| **Modele de donnees** | Chaque position stocke une valeur | Feuilles uniquement | Tampon d'arbre dense + chunks | Feuilles uniquement |
| **Hachage dans l'Element ?** | Non (circule comme hachage enfant) | Non (circule comme hachage enfant) | Non (circule comme hachage enfant) | Non (circule comme hachage enfant) |
| **Cout d'insertion (hachage)** | O(n) blake3 | O(1) amorti | O(1) amorti | ~33 Sinsemilla |
| **Taille de cout** | 6 octets | 11 octets | 12 octets | 12 octets |
| **Support de preuve** | V1 (Dense) | V1 (MMR) | V1 (Bulk) | V1 (CommitmentTree) |
| **Ideal pour** | Petites structures bornees | Journaux d'evenements | Journaux a haut debit | Commitments ZK |

**Quand choisir DenseAppendOnlyFixedSizeTree :**
- Le nombre maximum d'entrees est connu au moment de la creation
- Vous avez besoin que chaque position (y compris les noeuds internes) stocke des donnees
- Vous voulez le modele de donnees le plus simple possible sans croissance illimitee
- Le recalcul du hachage racine en O(n) est acceptable (petites hauteurs d'arbre)

**Quand NE PAS le choisir :**
- Vous avez besoin d'une capacite illimitee → utilisez MmrTree ou BulkAppendTree
- Vous avez besoin de compatibilite ZK → utilisez CommitmentTree

## Exemple d'utilisation

```rust
use grovedb::Element;
use grovedb_version::version::GroveVersion;

let grove_version = GroveVersion::latest();

// Create a dense tree of height 4 (capacity = 15 values)
db.insert(
    &[b"state"],
    b"validator_slots",
    Element::empty_dense_tree(4),
    None,
    None,
    grove_version,
)?;

// Append values — positions filled 0, 1, 2, ...
let (root_hash, pos) = db.dense_tree_insert(
    &[b"state"],
    b"validator_slots",
    validator_pubkey.to_vec(),
    None,
    grove_version,
)?;
// pos == 0, root_hash = blake3(validator_pubkey)

// Read back by position
let value = db.dense_tree_get(
    &[b"state"],
    b"validator_slots",
    0,     // position
    None,
    grove_version,
)?;
assert_eq!(value, Some(validator_pubkey.to_vec()));

// Query metadata
let count = db.dense_tree_count(&[b"state"], b"validator_slots", None, grove_version)?;
let hash = db.dense_tree_root_hash(&[b"state"], b"validator_slots", None, grove_version)?;
```

## Fichiers d'implementation

| Fichier | Contenu |
|---|---|
| `grovedb-dense-fixed-sized-merkle-tree/src/lib.rs` | Trait `DenseTreeStore`, structure `DenseFixedSizedMerkleTree`, hachage recursif |
| `grovedb-dense-fixed-sized-merkle-tree/src/proof.rs` | Structure `DenseTreeProof`, `generate()`, `encode_to_vec()`, `decode_from_slice()` |
| `grovedb-dense-fixed-sized-merkle-tree/src/verify.rs` | `DenseTreeProof::verify()` — fonction pure, pas de stockage necessaire |
| `grovedb-element/src/element/mod.rs` | `Element::DenseAppendOnlyFixedSizeTree` (discriminant 14) |
| `grovedb-element/src/element/constructor.rs` | `empty_dense_tree()`, `new_dense_tree()` |
| `merk/src/tree_type/mod.rs` | `TreeType::DenseAppendOnlyFixedSizeTree = 10` |
| `merk/src/tree_type/costs.rs` | `DENSE_TREE_COST_SIZE = 6` |
| `grovedb/src/operations/dense_tree.rs` | Operations GroveDB, `AuxDenseTreeStore`, pretraitement par lots |
| `grovedb/src/operations/proof/generate.rs` | `generate_dense_tree_layer_proof()`, `query_items_to_positions()` |
| `grovedb/src/operations/proof/verify.rs` | `verify_dense_tree_lower_layer()` |
| `grovedb/src/operations/proof/mod.rs` | Variante `ProofBytes::DenseTree` |
| `grovedb/src/batch/estimated_costs/average_case_costs.rs` | Modele de cout en cas moyen |
| `grovedb/src/batch/estimated_costs/worst_case_costs.rs` | Modele de cout en pire cas |
| `grovedb/src/tests/dense_tree_tests.rs` | 22 tests d'integration |

---
