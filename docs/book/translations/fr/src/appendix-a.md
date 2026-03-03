# Annexe A : Reference complete des types d'elements

| Discriminant | Variante | TreeType | Champs | Taille de cout | Objectif |
|---|---|---|---|---|---|
| 0 | `Item` | N/A | `(value, flags)` | variable | Stockage cle-valeur de base |
| 1 | `Reference` | N/A | `(path, max_hop, flags)` | variable | Lien entre elements |
| 2 | `Tree` | 0 (NormalTree) | `(root_key, flags)` | TREE_COST_SIZE | Conteneur pour sous-arbres |
| 3 | `SumItem` | N/A | `(value, flags)` | variable | Contribue a la somme parente |
| 4 | `SumTree` | 1 (SumTree) | `(root_key, sum, flags)` | SUM_TREE_COST_SIZE | Maintient la somme des descendants |
| 5 | `BigSumTree` | 4 (BigSumTree) | `(root_key, sum128, flags)` | BIG_SUM_TREE_COST_SIZE | Arbre de somme sur 128 bits |
| 6 | `CountTree` | 2 (CountTree) | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Arbre de comptage d'elements |
| 7 | `CountSumTree` | 3 (CountSumTree) | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Comptage + somme combines |
| 8 | `ItemWithSumItem` | N/A | `(value, sum, flags)` | variable | Element avec contribution a la somme |
| 9 | `ProvableCountTree` | 5 | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Arbre de comptage prouvable |
| 10 | `ProvableCountSumTree` | 6 | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Comptage + somme prouvable |
| 11 | `CommitmentTree` | 7 | `(total_count: u64, chunk_power: u8, flags)` | 12 | Compatible ZK avec Sinsemilla + BulkAppendTree |
| 12 | `MmrTree` | 8 | `(mmr_size: u64, flags)` | 11 | Journal en ajout seulement MMR |
| 13 | `BulkAppendTree` | 9 | `(total_count: u64, chunk_power: u8, flags)` | 12 | Journal en ajout seulement a haut debit |
| 14 | `DenseAppendOnlyFixedSizeTree` | 10 | `(count: u16, height: u8, flags)` | 6 | Stockage Merkle dense a capacite fixe |

**Notes :**
- Les discriminants 11 a 14 sont des **arbres non-Merk** : les donnees resident en dehors d'un sous-arbre Merk enfant
  - Les quatre stockent des donnees non-Merk dans la colonne de **donnees**
  - `CommitmentTree` stocke sa frontiere Sinsemilla a cote des entrees BulkAppendTree dans la meme colonne de donnees (cle `b"__ct_data__"`)
- Les arbres non-Merk N'ONT PAS de champ `root_key` — leur hachage racine specifique au type circule comme le hachage enfant du Merk via `insert_subtree`
- `CommitmentTree` utilise le hachage Sinsemilla (courbe Pallas) ; tous les autres utilisent Blake3
- Le comportement de cout pour les arbres non-Merk suit `NormalTree` (BasicMerkNode, pas d'agregation)
- Le champ count de `DenseAppendOnlyFixedSizeTree` est un `u16` (max 65 535) ; les hauteurs sont restreintes a 1..=16

---
