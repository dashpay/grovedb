# Cryptographie Quantique — Analyse des Menaces Post-Quantiques

Ce chapitre analyse comment les ordinateurs quantiques affecteraient les primitives
cryptographiques utilisées dans GroveDB et les protocoles de transactions blindées
construits par-dessus (Orchard, Dash Platform). Il couvre quels composants sont
vulnérables, lesquels sont sûrs, ce que signifie "récolter maintenant, déchiffrer
plus tard" pour les données stockées, et quelles stratégies d'atténuation existent
— y compris les conceptions de KEM hybride.

## Deux Algorithmes Quantiques Importants

Seuls deux algorithmes quantiques sont pertinents pour la cryptographie en pratique :

**L'algorithme de Shor** résout le problème du logarithme discret (et la factorisation
d'entiers) en temps polynomial. Pour une courbe elliptique de 255 bits comme Pallas,
cela nécessite environ 510 qubits logiques — mais avec la surcharge de correction
d'erreurs, l'exigence réelle est d'environ 4 millions de qubits physiques.
L'algorithme de Shor **casse complètement** toute la cryptographie sur courbes
elliptiques, quelle que soit la taille de la clé.

**L'algorithme de Grover** fournit une accélération quadratique pour la recherche
par force brute. Une clé symétrique de 256 bits devient effectivement 128 bits.
Cependant, la profondeur de circuit pour l'algorithme de Grover sur un espace de
clés de 128 bits reste de 2^64 opérations quantiques — de nombreux cryptographes
pensent que cela ne sera jamais pratique sur du matériel réel en raison des limites
de décohérence. L'algorithme de Grover réduit les marges de sécurité mais ne casse
pas la cryptographie symétrique bien paramétrée.

| Algorithme | Cibles | Accélération | Impact pratique |
|------------|--------|-------------|-----------------|
| **Shor** | Logarithme discret ECC, factorisation RSA | Temps polynomial (accélération exponentielle par rapport au classique) | **Rupture totale** de l'ECC |
| **Grover** | Recherche de clés symétriques, préimage de hash | Quadratique (divise les bits de clé par deux) | 256-bit → 128-bit (toujours sûr) |

## Primitives Cryptographiques de GroveDB

GroveDB et le protocole blindé basé sur Orchard utilisent un mélange de primitives
de courbes elliptiques et de primitives symétriques/basées sur le hachage. Le tableau
ci-dessous classe chaque primitive selon sa vulnérabilité quantique :

### Vulnérable au Quantique (algorithme de Shor — 0 bits post-quantiques)

| Primitive | Utilisation | Ce qui est cassé |
|-----------|------------|-----------------|
| **Pallas ECDLP** | Engagements de notes (cmx), clés éphémères (epk/esk), clés de visualisation (ivk), clés de paiement (pk_d), dérivation de nullifieurs | Récupérer toute clé privée à partir de sa contrepartie publique |
| **Accord de clés ECDH** (Pallas) | Dérivation de clés de chiffrement symétriques pour les textes chiffrés de notes | Récupérer le secret partagé → déchiffrer toutes les notes |
| **Hachage Sinsemilla** | Chemins Merkle du CommitmentTree, hachage dans le circuit | La résistance aux collisions dépend de l'ECDLP ; se dégrade quand Pallas est cassé |
| **Halo 2 IPA** | Système de preuves ZK (engagement polynomial sur les courbes Pasta) | Forger des preuves pour des déclarations fausses (contrefaçon, dépenses non autorisées) |
| **Engagements de Pedersen** | Engagements de valeur (cv_net) cachant les montants des transactions | Récupérer les montants cachés ; forger des preuves d'équilibre |

### Sûr face au Quantique (algorithme de Grover — 128+ bits post-quantiques)

| Primitive | Utilisation | Sécurité post-quantique |
|-----------|------------|------------------------|
| **Blake3** | Hachages des noeuds d'arbres Merk, noeuds MMR, racines d'état de BulkAppendTree, préfixes de chemins de sous-arbres | 128-bit préimage, 128-bit seconde préimage |
| **BLAKE2b-256** | KDF pour la dérivation de clés symétriques, clé de chiffrement sortante, PRF^expand | 128-bit préimage |
| **ChaCha20-Poly1305** | Chiffre enc_ciphertext et out_ciphertext (clés de 256 bits) | 128-bit recherche de clé (sûr, mais le chemin de dérivation de clé via ECDH ne l'est pas) |
| **PRF^expand** (BLAKE2b-512) | Dérive esk, rcm, psi à partir de rseed | 128-bit sécurité PRF |

### Infrastructure de GroveDB : Entièrement Sûre face au Quantique

Toutes les structures de données propres à GroveDB reposent exclusivement sur le
hachage Blake3 :

- **Arbres AVL Merk** — hachages de noeuds, combined_value_hash, propagation du hash enfant
- **Arbres MMR** — hachages de noeuds internes, calcul des sommets, dérivation de la racine
- **BulkAppendTree** — chaînes de hachage de tampon, racines Merkle denses, MMR d'époques
- **Racine d'état du CommitmentTree** — `blake3("ct_state" || sinsemilla_root || bulk_state_root)`
- **Préfixes de chemins de sous-arbres** — hachage Blake3 des segments de chemin
- **Preuves V1** — chaînes d'authentification à travers la hiérarchie Merk

**Aucun changement nécessaire.** Les preuves d'arbres Merk de GroveDB, les vérifications
de cohérence de MMR, les racines d'époques de BulkAppendTree et toutes les chaînes
d'authentification de preuves V1 restent sécurisées contre les ordinateurs quantiques.
L'infrastructure basée sur le hachage est la partie la plus solide du système
post-quantique.

## Menaces Rétroactives vs. en Temps Réel

Cette distinction est essentielle pour prioriser ce qu'il faut corriger et quand.

**Les menaces rétroactives** compromettent des données déjà stockées. Un adversaire
enregistre des données aujourd'hui et les déchiffre lorsque les ordinateurs quantiques
deviennent disponibles. Ces menaces **ne peuvent pas être atténuées après coup** — une
fois que les données sont sur la blockchain, elles ne peuvent pas être re-chiffrées
ni retirées.

**Les menaces en temps réel** n'affectent que les transactions créées dans le futur.
Un adversaire disposant d'un ordinateur quantique pourrait forger des signatures ou des
preuves, mais uniquement pour de nouvelles transactions. Les anciennes transactions ont
déjà été validées et confirmées par le réseau.

| Menace | Type | Ce qui est exposé | Urgence |
|--------|------|------------------|---------|
| **Déchiffrement de notes** (enc_ciphertext) | **Rétroactive** | Contenu des notes : destinataire, montant, mémo, rseed | **Élevée** — stocké en permanence |
| **Ouverture d'engagement de valeur** (cv_net) | **Rétroactive** | Montants des transactions (mais pas l'expéditeur/destinataire) | **Moyenne** — montants uniquement |
| **Données de récupération de l'expéditeur** (out_ciphertext) | **Rétroactive** | Clés de récupération de l'expéditeur pour les notes envoyées | **Élevée** — stocké en permanence |
| Falsification d'autorisation de dépense | Temps réel | Pourrait forger de nouvelles signatures de dépense | Faible — mettre à jour avant l'arrivée de l'OQ |
| Falsification de preuves Halo 2 | Temps réel | Pourrait forger de nouvelles preuves (contrefaçon) | Faible — mettre à jour avant l'arrivée de l'OQ |
| Collision de Sinsemilla | Temps réel | Pourrait forger de nouveaux chemins Merkle | Faible — subsumée par la falsification de preuves |
| Falsification de signature de liaison | Temps réel | Pourrait forger de nouvelles preuves d'équilibre | Faible — mettre à jour avant l'arrivée de l'OQ |

### Qu'est-ce Qui Est Exactement Révélé ?

**Si le chiffrement de notes est cassé** (la menace HNDL principale) :

Un adversaire quantique récupère `esk` à partir de l'`epk` stocké via l'algorithme
de Shor, calcule le secret partagé ECDH, dérive la clé symétrique et déchiffre
`enc_ciphertext`. Cela révèle le texte clair complet de la note :

| Champ | Taille | Ce que cela révèle |
|-------|--------|-------------------|
| version | 1 byte | Version du protocole (non sensible) |
| diversifier | 11 bytes | Composant de l'adresse du destinataire |
| value | 8 bytes | Montant exact de la transaction |
| rseed | 32 bytes | Permet le chaînage des nullifieurs (désanonymise le graphe de transactions) |
| memo | 36 bytes (DashMemo) | Données applicatives, potentiellement identifiantes |

Avec `rseed` et `rho` (stockés à côté du texte chiffré), l'adversaire peut calculer
`esk = PRF(rseed, rho)` et vérifier la liaison de la clé éphémère. Combiné avec le
diversifier, cela lie les entrées aux sorties à travers tout l'historique des
transactions — **désanonymisation complète du pool blindé**.

**Si seuls les engagements de valeur sont cassés** (menace HNDL secondaire) :

L'adversaire récupère `v` de `cv_net = [v]*V + [rcv]*R` en résolvant l'ECDLP.
Cela révèle **les montants des transactions mais pas les identités de l'expéditeur
ni du destinataire**. L'adversaire voit "quelqu'un a envoyé 5.0 Dash à quelqu'un"
mais ne peut pas lier le montant à une adresse ou une identité sans casser aussi
le chiffrement des notes.

En soi, les montants sans liaison ont une utilité limitée. Mais combinés avec des
données externes (temporalité, factures connues, montants correspondant à des
demandes publiques), les attaques par corrélation deviennent possibles.

## L'Attaque "Récolter Maintenant, Déchiffrer Plus Tard"

C'est la menace quantique la plus urgente et la plus pratique.

**Modèle d'attaque :** Un adversaire étatique (ou toute partie disposant d'un stockage
suffisant) enregistre toutes les données de transactions blindées sur la blockchain
aujourd'hui. Ces données sont publiquement disponibles sur la blockchain et sont
immuables. L'adversaire attend un ordinateur quantique cryptographiquement pertinent
(CRQC), puis :

```text
Étape 1: Lire l'enregistrement stocké du BulkAppendTree du CommitmentTree :
         cmx (32) || rho (32) || epk (32) || enc_ciphertext (104) || out_ciphertext (80)

Étape 2: Résoudre l'ECDLP sur Pallas via l'algorithme de Shor :
         epk = [esk] * g_d  →  récupérer esk

Étape 3: Calculer le secret partagé :
         shared_secret = [esk] * pk_d

Étape 4: Dériver la clé symétrique (BLAKE2b est sûr quantiquement, mais l'entrée est compromise) :
         K_enc = BLAKE2b-256("Zcash_OrchardKDF", shared_secret || epk)

Étape 5: Déchiffrer enc_ciphertext avec ChaCha20-Poly1305 :
         → version || diversifier || value || rseed || memo

Étape 6: Avec rseed + rho, lier les nullifieurs aux engagements de notes :
         esk = PRF(rseed, rho)
         → reconstruction complète du graphe de transactions
```

**Constat essentiel :** Le chiffrement symétrique (ChaCha20-Poly1305) est parfaitement
sûr face au quantique. La vulnérabilité réside entièrement dans le **chemin de
dérivation de la clé** — la clé symétrique est dérivée d'un secret partagé ECDH,
et l'ECDH est cassé par l'algorithme de Shor. L'attaquant ne casse pas le chiffrement ;
il récupère la clé.

**Rétroactivité :** Cette attaque est **entièrement rétroactive**. Chaque note chiffrée
stockée sur la blockchain peut être déchiffrée une fois qu'un CRQC existe. Les données
ne peuvent pas être re-chiffrées ni protégées après coup. C'est pourquoi cela doit
être traité avant que les données ne soient stockées, pas après.

## Atténuation : KEM Hybride (ML-KEM + ECDH)

La défense contre le HNDL consiste à dériver la clé de chiffrement symétrique à
partir de **deux mécanismes indépendants d'accord de clés**, de sorte que la rupture
d'un seul soit insuffisante. C'est ce qu'on appelle un KEM hybride.

### ML-KEM-768 (CRYSTALS-Kyber)

ML-KEM est le mécanisme d'encapsulation de clés post-quantique standardisé par le
NIST (FIPS 203, août 2024), basé sur le problème Module Learning With Errors (MLWE).

| Paramètre | ML-KEM-512 | ML-KEM-768 | ML-KEM-1024 |
|-----------|-----------|-----------|------------|
| Clé publique (ek) | 800 bytes | **1,184 bytes** | 1,568 bytes |
| Texte chiffré (ct) | 768 bytes | **1,088 bytes** | 1,568 bytes |
| Secret partagé | 32 bytes | 32 bytes | 32 bytes |
| Catégorie NIST | 1 (128-bit) | **3 (192-bit)** | 5 (256-bit) |

**ML-KEM-768** est le choix recommandé — c'est le jeu de paramètres utilisé par
X-Wing, PQXDH de Signal et l'échange de clés hybride TLS de Chrome/Firefox. La
Catégorie 3 fournit une marge confortable contre les avancées futures en cryptanalyse
des réseaux.

### Comment Fonctionne le Schéma Hybride

**Flux actuel (vulnérable) :**

```text
Expéditeur :
  esk = PRF(rseed, rho)                    // déterministe à partir de la note
  epk = [esk] * g_d                         // point de courbe Pallas
  shared_secret = [esk] * pk_d              // ECDH (cassé par Shor)
  K_enc = BLAKE2b("Zcash_OrchardKDF", shared_secret || epk)
  enc_ciphertext = ChaCha20(K_enc, note_plaintext)
```

**Flux hybride (résistant au quantique) :**

```text
Expéditeur :
  esk = PRF(rseed, rho)                    // inchangé
  epk = [esk] * g_d                         // inchangé
  ss_ecdh = [esk] * pk_d                    // même ECDH

  (ct_pq, ss_pq) = ML-KEM-768.Encaps(ek_pq)  // NOUVEAU : KEM basé sur les réseaux
                                                // ek_pq de l'adresse du destinataire

  K_enc = BLAKE2b(                          // MODIFIÉ : combine les deux secrets
      "DashPlatform_HybridKDF",
      ss_ecdh || ss_pq || ct_pq || epk
  )

  enc_ciphertext = ChaCha20(K_enc, note_plaintext)  // inchangé
```

**Déchiffrement du destinataire :**

```text
Destinataire :
  ss_ecdh = [ivk] * epk                    // même ECDH (avec la clé de visualisation entrante)
  ss_pq = ML-KEM-768.Decaps(dk_pq, ct_pq)  // NOUVEAU : désencapsulation KEM
  K_enc = BLAKE2b("DashPlatform_HybridKDF", ss_ecdh || ss_pq || ct_pq || epk)
  note_plaintext = ChaCha20.Decrypt(K_enc, enc_ciphertext)
```

### Garantie de Sécurité

Le KEM combiné est sûr IND-CCA2 si **l'un ou l'autre** des KEM composants est
sécurisé. Cela est formellement prouvé par [Giacon, Heuer et Poettering (2018)](https://eprint.iacr.org/2018/024)
pour les combineurs de KEM utilisant un PRF (BLAKE2b est éligible), et indépendamment
par la [preuve de sécurité de X-Wing](https://eprint.iacr.org/2024/039).

| Scénario | ECDH | ML-KEM | Clé combinée | Statut |
|----------|------|--------|-------------|--------|
| Monde classique | Sûr | Sûr | **Sûr** | Les deux intacts |
| Le quantique casse l'ECC | **Cassé** | Sûr | **Sûr** | ML-KEM protège |
| Des avancées sur les réseaux cassent ML-KEM | Sûr | **Cassé** | **Sûr** | ECDH protège (comme aujourd'hui) |
| Les deux cassés | Cassé | Cassé | **Cassé** | Nécessite deux percées simultanées |

### Impact sur la Taille

Le KEM hybride ajoute le texte chiffré ML-KEM-768 (1,088 bytes) à chaque note
stockée et agrandit le texte chiffré sortant pour inclure le secret partagé ML-KEM
pour la récupération de l'expéditeur :

**Enregistrement stocké par note :**

```text
┌──────────────────────────────────────────────────────────────────┐
│  Actuel (280 bytes)            Hybride (1,400 bytes)             │
│                                                                  │
│  cmx:             32         cmx:              32               │
│  rho:             32         rho:              32               │
│  epk:             32         epk:              32               │
│  enc_ciphertext: 104         ct_pq:         1,088  ← NOUVEAU   │
│  out_ciphertext:  80         enc_ciphertext:  104               │
│                              out_ciphertext:  112  ← +32        │
│  ─────────────────           ──────────────────────             │
│  Total:          280         Total:          1,400  (5.0x)      │
└──────────────────────────────────────────────────────────────────┘
```

**Stockage à grande échelle :**

| Notes | Actuel (280 B) | Hybride (1,400 B) | Delta |
|-------|----------------|-------------------|-------|
| 100,000 | 26.7 MB | 133 MB | +106 MB |
| 1,000,000 | 267 MB | 1.33 GB | +1.07 GB |
| 10,000,000 | 2.67 GB | 13.3 GB | +10.7 GB |

**Taille d'adresse :**

```text
Actuel :  diversifier (11) + pk_d (32) = 43 bytes
Hybride : diversifier (11) + pk_d (32) + ek_pq (1,184) = 1,227 bytes
```

La clé publique ML-KEM de 1,184 bytes doit être incluse dans l'adresse pour que les
expéditeurs puissent effectuer l'encapsulation. Avec environ 1,960 caractères Bech32m,
c'est volumineux mais cela tient encore dans un code QR (maximum ~2,953 caractères
alphanumériques).

### Gestion des Clés

La paire de clés ML-KEM est dérivée de manière déterministe à partir de la clé
de dépense :

```text
SpendingKey (sk) [32 bytes]
  |
  +-> ... (toute la dérivation de clés Orchard existante inchangée)
  |
  +-> ml_kem_d = PRF^expand(sk, [0x09])[0..32]
  +-> ml_kem_z = PRF^expand(sk, [0x0A])[0..32]
        |
        +-> (ek_pq, dk_pq) = ML-KEM-768.KeyGen(d=ml_kem_d, z=ml_kem_z)
              ek_pq: 1,184 bytes (publique, incluse dans l'adresse)
              dk_pq: 2,400 bytes (privée, partie de la clé de visualisation)
```

**Aucun changement de sauvegarde nécessaire.** La phrase de récupération de 24 mots
existante couvre la clé ML-KEM car elle est dérivée de la clé de dépense de manière
déterministe. La récupération de portefeuille fonctionne comme avant.

**Les adresses diversifiées** partagent toutes le même `ek_pq` car ML-KEM n'a pas
de mécanisme de diversification naturel comme la multiplication scalaire de Pallas.
Cela signifie qu'un observateur disposant de deux adresses d'un utilisateur peut les
lier en comparant `ek_pq`.

### Performance de Déchiffrement par Essai

| Étape | Actuel | Hybride | Delta |
|-------|--------|---------|-------|
| Pallas ECDH | ~100 us | ~100 us | — |
| ML-KEM-768 Decaps | — | ~40 us | +40 us |
| BLAKE2b KDF | ~0.5 us | ~1 us | — |
| ChaCha20 (52 bytes) | ~0.1 us | ~0.1 us | — |
| **Total par note** | **~101 us** | **~141 us** | **+40% de surcharge** |

Analyse de 100,000 notes : ~10.1 sec → ~14.1 sec. La surcharge est significative mais
pas prohibitive. La désencapsulation ML-KEM est en temps constant sans avantage de
traitement par lots (contrairement aux opérations sur courbes elliptiques), elle évolue
donc linéairement.

### Impact sur les Circuits ZK

**Aucun.** Le KEM hybride est entièrement dans la couche de transport/chiffrement. Le
circuit Halo 2 prouve l'existence des notes, la correction des nullifieurs et l'équilibre
des valeurs — il ne prouve rien concernant le chiffrement. Aucun changement aux clés
de preuve, clés de vérification ni aux contraintes de circuit.

### Comparaison avec l'Industrie

| Système | Approche | Statut |
|---------|----------|--------|
| **Signal** (PQXDH) | X25519 + ML-KEM-768, obligatoire pour tous les utilisateurs | **Déployé** (2023) |
| **Chrome/Firefox TLS** | X25519 + ML-KEM-768 échange de clés hybride | **Déployé** (2024) |
| **X-Wing** (brouillon IETF) | X25519 + ML-KEM-768, combineur dédié | Brouillon de standard |
| **Zcash** | Brouillon ZIP de récupérabilité quantique (récupération de fonds, pas de chiffrement) | Discussion uniquement |
| **Dash Platform** | Pallas ECDH + ML-KEM-768 (proposé) | Phase de conception |

## Quand Déployer

### La Question du Calendrier

- **État actuel (2026) :** Aucun ordinateur quantique ne peut casser l'ECC à 255 bits.
  La plus grande factorisation quantique démontrée : ~50 bits. Écart : ordres de grandeur.
- **Court terme (2030-2035) :** Les feuilles de route matérielles d'IBM, Google,
  Quantinuum visent des millions de qubits. Les implémentations et jeux de paramètres
  ML-KEM auront mûri.
- **Moyen terme (2035-2050) :** La plupart des estimations situent l'arrivée de la
  CRQC dans cette fenêtre. Les données HNDL collectées aujourd'hui sont à risque.
- **Long terme (2050+) :** Consensus parmi les cryptographes : les ordinateurs
  quantiques à grande échelle sont une question de "quand", pas de "si".

### Stratégie Recommandée

**1. Concevoir pour l'évolutivité dès maintenant.** S'assurer que le format
d'enregistrement stocké, la structure `TransmittedNoteCiphertext` et la disposition
des entrées du BulkAppendTree sont versionnés et extensibles. C'est peu coûteux et
préserve l'option d'ajouter un KEM hybride plus tard.

**2. Déployer le KEM hybride quand il est prêt, le rendre obligatoire.** Ne pas offrir
deux pools (classique et hybride). Diviser l'ensemble d'anonymat annule l'objectif des
transactions blindées — les utilisateurs se cachant parmi un groupe plus petit ont
moins de confidentialité, pas plus. Lors du déploiement, chaque note utilise le
schéma hybride.

**3. Viser la fenêtre 2028-2030.** C'est bien avant toute menace quantique réaliste
mais après que les implémentations de ML-KEM et les tailles de paramètres se soient
stabilisées. Cela permet également d'apprendre de l'expérience de déploiement de
Zcash et Signal.

**4. Surveiller les événements déclencheurs :**
- Le NIST ou la NSA imposant des délais de migration post-quantique
- Des avancées significatives dans le matériel quantique (>100,000 qubits physiques
  avec correction d'erreurs)
- Des avancées cryptanalytiques contre les problèmes de réseaux (affecteraient le
  choix de ML-KEM)

### Ce Qui Ne Nécessite Pas d'Action Urgente

| Composant | Pourquoi cela peut attendre |
|-----------|---------------------------|
| Signatures d'autorisation de dépense | La falsification est en temps réel, pas rétroactive. Passer à ML-DSA/SLH-DSA avant l'arrivée de la CRQC. |
| Système de preuves Halo 2 | La falsification de preuves est en temps réel. Migrer vers un système basé sur STARK si nécessaire. |
| Résistance aux collisions de Sinsemilla | Utile uniquement pour de nouvelles attaques, pas rétroactives. Subsumée par la migration du système de preuves. |
| Infrastructure GroveDB Merk/MMR/Blake3 | **Déjà sûre face au quantique sous les hypothèses cryptographiques actuelles.** Aucune action nécessaire basée sur les attaques connues. |

## Référence des Alternatives Post-Quantiques

### Pour le Chiffrement (remplacement de l'ECDH)

| Schéma | Type | Clé publique | Texte chiffré | Catégorie NIST | Notes |
|--------|------|-------------|--------------|----------------|-------|
| ML-KEM-768 | Lattice (MLWE) | 1,184 B | 1,088 B | 3 (192-bit) | FIPS 203, standard industriel |
| ML-KEM-512 | Lattice (MLWE) | 800 B | 768 B | 1 (128-bit) | Plus petit, marge inférieure |
| ML-KEM-1024 | Lattice (MLWE) | 1,568 B | 1,568 B | 5 (256-bit) | Excessif pour l'hybride |

### Pour les Signatures (remplacement de RedPallas/Schnorr)

| Schéma | Type | Clé publique | Signature | Catégorie NIST | Notes |
|--------|------|-------------|----------|----------------|-------|
| ML-DSA-65 (Dilithium) | Lattice | 1,952 B | 3,293 B | 3 | FIPS 204, rapide |
| SLH-DSA (SPHINCS+) | Basé sur le hachage | 32-64 B | 7,856-49,856 B | 1-5 | FIPS 205, conservateur |
| XMSS/LMS | Basé sur le hachage (à état) | 60 B | 2,500 B | variable | À état — réutiliser = casser |

### Pour les Preuves ZK (remplacement de Halo 2)

| Système | Hypothèse | Taille de preuve | Post-quantique | Notes |
|---------|-----------|-----------------|----------------|-------|
| STARKs | Fonctions de hachage (résistance aux collisions) | ~100-400 KB | **Oui** | Utilisé par StarkNet |
| Plonky3 | FRI (engagement polynomial basé sur le hachage) | ~50-200 KB | **Oui** | Développement actif |
| Halo 2 (actuel) | ECDLP sur les courbes Pasta | ~5 KB | **Non** | Système actuel d'Orchard |
| Lattice SNARKs | MLWE | Recherche | **Oui** | Pas prêt pour la production |

### Écosystème de Crates Rust

| Crate | Source | FIPS 203 | Vérifié | Notes |
|-------|--------|----------|---------|-------|
| `libcrux-ml-kem` | Cryspen | Oui | Formellement vérifié (hax/F*) | Plus haute assurance |
| `ml-kem` | RustCrypto | Oui | Temps constant, non audité | Compatibilité écosystème |
| `fips203` | integritychain | Oui | Temps constant | Rust pur, no_std |

## Résumé

```text
┌─────────────────────────────────────────────────────────────────────┐
│  RÉSUMÉ DES MENACES QUANTIQUES POUR GROVEDB + ORCHARD              │
│                                                                     │
│  SÛR SOUS LES HYPOTHÈSES ACTUELLES (basé sur le hachage) :        │
│    ✓ Arbres Merk Blake3, MMR, BulkAppendTree                       │
│    ✓ BLAKE2b KDF, PRF^expand                                       │
│    ✓ Chiffrement symétrique ChaCha20-Poly1305                      │
│    ✓ Toutes les chaînes d'authentification de preuves GroveDB      │
│                                                                     │
│  CORRIGER AVANT LE STOCKAGE DES DONNÉES (HNDL rétroactif) :       │
│    ✗ Chiffrement de notes (accord de clés ECDH) → KEM Hybride     │
│    ✗ Engagements de valeur (Pedersen) → montants révélés           │
│                                                                     │
│  CORRIGER AVANT L'ARRIVÉE DES ORDINATEURS QUANTIQUES               │
│  (temps réel uniquement) :                                          │
│    ~ Autorisation de dépense → ML-DSA / SLH-DSA                   │
│    ~ Preuves ZK → STARKs / Plonky3                                │
│    ~ Sinsemilla → arbre Merkle basé sur le hachage                 │
│                                                                     │
│  CALENDRIER RECOMMANDÉ :                                            │
│    2026-2028 : Concevoir pour l'évolutivité, versionner les formats│
│    2028-2030 : Déployer le KEM hybride obligatoire pour le chiffr. │
│    2035+ : Migrer les signatures et le système de preuves si besoin│
└─────────────────────────────────────────────────────────────────────┘
```

---
