# Cryptographie Quantique — Analyse des Menaces Post-Quantiques

Ce chapitre analyse comment les ordinateurs quantiques affecteraient les primitives
cryptographiques utilisees dans GroveDB et les protocoles de transactions blindees
construits par-dessus (Orchard, Dash Platform). Il couvre quels composants sont
vulnerables, lesquels sont surs, ce que signifie "recolter maintenant, dechiffrer
plus tard" pour les donnees stockees, et quelles strategies d'attenuation existent
— y compris les conceptions de KEM hybride.

## Deux Algorithmes Quantiques Importants

Seuls deux algorithmes quantiques sont pertinents pour la cryptographie en pratique :

**L'algorithme de Shor** resout le probleme du logarithme discret (et la factorisation
d'entiers) en temps polynomial. Pour une courbe elliptique de 255 bits comme Pallas,
cela necessite environ 510 qubits logiques — mais avec la surcharge de correction
d'erreurs, l'exigence reelle est d'environ 4 millions de qubits physiques.
L'algorithme de Shor **casse completement** toute la cryptographie sur courbes
elliptiques, quelle que soit la taille de la cle.

**L'algorithme de Grover** fournit une acceleration quadratique pour la recherche
par force brute. Une cle symetrique de 256 bits devient effectivement 128 bits.
Cependant, la profondeur de circuit pour l'algorithme de Grover sur un espace de
cles de 128 bits reste de 2^64 operations quantiques — de nombreux cryptographes
pensent que cela ne sera jamais pratique sur du materiel reel en raison des limites
de decoherence. L'algorithme de Grover reduit les marges de securite mais ne casse
pas la cryptographie symetrique bien parametree.

| Algorithme | Cibles | Acceleration | Impact pratique |
|------------|--------|-------------|-----------------|
| **Shor** | Logarithme discret ECC, factorisation RSA | Exponentielle (temps polynomial) | **Rupture totale** de l'ECC |
| **Grover** | Recherche de cles symetriques, preimage de hash | Quadratique (divise les bits de cle par deux) | 256-bit → 128-bit (toujours sur) |

## Primitives Cryptographiques de GroveDB

GroveDB et le protocole blinde base sur Orchard utilisent un melange de primitives
de courbes elliptiques et de primitives symetriques/basees sur le hachage. Le tableau
ci-dessous classe chaque primitive selon sa vulnerabilite quantique :

### Vulnerable au Quantique (algorithme de Shor — 0 bits post-quantiques)

| Primitive | Utilisation | Ce qui est casse |
|-----------|------------|-----------------|
| **Pallas ECDLP** | Engagements de notes (cmx), cles ephemeres (epk/esk), cles de visualisation (ivk), cles de paiement (pk_d), derivation de nullifieurs | Recuperer toute cle privee a partir de sa contrepartie publique |
| **Accord de cles ECDH** (Pallas) | Derivation de cles de chiffrement symetriques pour les textes chiffres de notes | Recuperer le secret partage → dechiffrer toutes les notes |
| **Hachage Sinsemilla** | Chemins Merkle du CommitmentTree, hachage dans le circuit | La resistance aux collisions depend de l'ECDLP ; se degrade quand Pallas est casse |
| **Halo 2 IPA** | Systeme de preuves ZK (engagement polynomial sur les courbes Pasta) | Forger des preuves pour des declarations fausses (contrefacon, depenses non autorisees) |
| **Engagements de Pedersen** | Engagements de valeur (cv_net) cachant les montants des transactions | Recuperer les montants caches ; forger des preuves d'equilibre |

### Sur face au Quantique (algorithme de Grover — 128+ bits post-quantiques)

| Primitive | Utilisation | Securite post-quantique |
|-----------|------------|------------------------|
| **Blake3** | Hachages des noeuds d'arbres Merk, noeuds MMR, racines d'etat de BulkAppendTree, prefixes de chemins de sous-arbres | 128-bit preimage, 128-bit seconde preimage |
| **BLAKE2b-256** | KDF pour la derivation de cles symetriques, cle de chiffrement sortante, PRF^expand | 128-bit preimage |
| **ChaCha20-Poly1305** | Chiffre enc_ciphertext et out_ciphertext (cles de 256 bits) | 128-bit recherche de cle (sur, mais le chemin de derivation de cle via ECDH ne l'est pas) |
| **PRF^expand** (BLAKE2b-512) | Derive esk, rcm, psi a partir de rseed | 128-bit securite PRF |

### Infrastructure de GroveDB : Entierement Sure face au Quantique

Toutes les structures de donnees propres a GroveDB reposent exclusivement sur le
hachage Blake3 :

- **Arbres AVL Merk** — hachages de noeuds, combined_value_hash, propagation du hash enfant
- **Arbres MMR** — hachages de noeuds internes, calcul des sommets, derivation de la racine
- **BulkAppendTree** — chaines de hachage de tampon, racines Merkle denses, MMR d'epoques
- **Racine d'etat du CommitmentTree** — `blake3("ct_state" || sinsemilla_root || bulk_state_root)`
- **Prefixes de chemins de sous-arbres** — hachage Blake3 des segments de chemin
- **Preuves V1** — chaines d'authentification a travers la hierarchie Merk

**Aucun changement necessaire.** Les preuves d'arbres Merk de GroveDB, les verifications
de coherence de MMR, les racines d'epoques de BulkAppendTree et toutes les chaines
d'authentification de preuves V1 restent securisees contre les ordinateurs quantiques.
L'infrastructure basee sur le hachage est la partie la plus solide du systeme
post-quantique.

## Menaces Retroactives vs. en Temps Reel

Cette distinction est essentielle pour prioriser ce qu'il faut corriger et quand.

**Les menaces retroactives** compromettent des donnees deja stockees. Un adversaire
enregistre des donnees aujourd'hui et les dechiffre lorsque les ordinateurs quantiques
deviennent disponibles. Ces menaces **ne peuvent pas etre attenuees apres coup** — une
fois que les donnees sont sur la blockchain, elles ne peuvent pas etre re-chiffrees
ni retirees.

**Les menaces en temps reel** n'affectent que les transactions creees dans le futur.
Un adversaire disposant d'un ordinateur quantique pourrait forger des signatures ou des
preuves, mais uniquement pour de nouvelles transactions. Les anciennes transactions ont
deja ete validees et confirmees par le reseau.

| Menace | Type | Ce qui est expose | Urgence |
|--------|------|------------------|---------|
| **Dechiffrement de notes** (enc_ciphertext) | **Retroactive** | Contenu des notes : destinataire, montant, memo, rseed | **Elevee** — stocke en permanence |
| **Ouverture d'engagement de valeur** (cv_net) | **Retroactive** | Montants des transactions (mais pas l'expediteur/destinataire) | **Moyenne** — montants uniquement |
| **Donnees de recuperation de l'expediteur** (out_ciphertext) | **Retroactive** | Cles de recuperation de l'expediteur pour les notes envoyees | **Elevee** — stocke en permanence |
| Falsification d'autorisation de depense | Temps reel | Pourrait forger de nouvelles signatures de depense | Faible — mettre a jour avant l'arrivee de l'OQ |
| Falsification de preuves Halo 2 | Temps reel | Pourrait forger de nouvelles preuves (contrefacon) | Faible — mettre a jour avant l'arrivee de l'OQ |
| Collision de Sinsemilla | Temps reel | Pourrait forger de nouveaux chemins Merkle | Faible — subsumee par la falsification de preuves |
| Falsification de signature de liaison | Temps reel | Pourrait forger de nouvelles preuves d'equilibre | Faible — mettre a jour avant l'arrivee de l'OQ |

### Qu'est-ce Qui Est Exactement Revele ?

**Si le chiffrement de notes est casse** (la menace HNDL principale) :

Un adversaire quantique recupere `esk` a partir de l'`epk` stocke via l'algorithme
de Shor, calcule le secret partage ECDH, derive la cle symetrique et dechiffre
`enc_ciphertext`. Cela revele le texte clair complet de la note :

| Champ | Taille | Ce que cela revele |
|-------|--------|-------------------|
| version | 1 byte | Version du protocole (non sensible) |
| diversifier | 11 bytes | Composant de l'adresse du destinataire |
| value | 8 bytes | Montant exact de la transaction |
| rseed | 32 bytes | Permet le chainage des nullifieurs (desanonymise le graphe de transactions) |
| memo | 36 bytes (DashMemo) | Donnees applicatives, potentiellement identifiantes |

Avec `rseed` et `rho` (stockes a cote du texte chiffre), l'adversaire peut calculer
`esk = PRF(rseed, rho)` et verifier la liaison de la cle ephemere. Combine avec le
diversifier, cela lie les entrees aux sorties a travers tout l'historique des
transactions — **desanonymisation complete du pool blinde**.

**Si seuls les engagements de valeur sont casses** (menace HNDL secondaire) :

L'adversaire recupere `v` de `cv_net = [v]*V + [rcv]*R` en resolvant l'ECDLP.
Cela revele **les montants des transactions mais pas les identites de l'expediteur
ni du destinataire**. L'adversaire voit "quelqu'un a envoye 5.0 Dash a quelqu'un"
mais ne peut pas lier le montant a une adresse ou une identite sans casser aussi
le chiffrement des notes.

En soi, les montants sans liaison ont une utilite limitee. Mais combines avec des
donnees externes (temporalite, factures connues, montants correspondant a des
demandes publiques), les attaques par correlation deviennent possibles.

## L'Attaque "Recolter Maintenant, Dechiffrer Plus Tard"

C'est la menace quantique la plus urgente et la plus pratique.

**Modele d'attaque :** Un adversaire etatique (ou toute partie disposant d'un stockage
suffisant) enregistre toutes les donnees de transactions blindees sur la blockchain
aujourd'hui. Ces donnees sont publiquement disponibles sur la blockchain et sont
immuables. L'adversaire attend un ordinateur quantique cryptographiquement pertinent
(CRQC), puis :

```text
Etape 1: Lire l'enregistrement stocke du BulkAppendTree du CommitmentTree :
         cmx (32) || rho (32) || epk (32) || enc_ciphertext (104) || out_ciphertext (80)

Etape 2: Resoudre l'ECDLP sur Pallas via l'algorithme de Shor :
         epk = [esk] * g_d  →  recuperer esk

Etape 3: Calculer le secret partage :
         shared_secret = [esk] * pk_d

Etape 4: Deriver la cle symetrique (BLAKE2b est sur quantiquement, mais l'entree est compromise) :
         K_enc = BLAKE2b-256("Zcash_OrchardKDF", shared_secret || epk)

Etape 5: Dechiffrer enc_ciphertext avec ChaCha20-Poly1305 :
         → version || diversifier || value || rseed || memo

Etape 6: Avec rseed + rho, lier les nullifieurs aux engagements de notes :
         esk = PRF(rseed, rho)
         → reconstruction complete du graphe de transactions
```

**Constat essentiel :** Le chiffrement symetrique (ChaCha20-Poly1305) est parfaitement
sur face au quantique. La vulnerabilite reside entierement dans le **chemin de
derivation de la cle** — la cle symetrique est derivee d'un secret partage ECDH,
et l'ECDH est casse par l'algorithme de Shor. L'attaquant ne casse pas le chiffrement ;
il recupere la cle.

**Retroactivite :** Cette attaque est **entierement retroactive**. Chaque note chiffree
stockee sur la blockchain peut etre dechiffree une fois qu'un CRQC existe. Les donnees
ne peuvent pas etre re-chiffrees ni protegees apres coup. C'est pourquoi cela doit
etre traite avant que les donnees ne soient stockees, pas apres.

## Attenuation : KEM Hybride (ML-KEM + ECDH)

La defense contre le HNDL consiste a deriver la cle de chiffrement symetrique a
partir de **deux mecanismes independants d'accord de cles**, de sorte que la rupture
d'un seul soit insuffisante. C'est ce qu'on appelle un KEM hybride.

### ML-KEM-768 (CRYSTALS-Kyber)

ML-KEM est le mecanisme d'encapsulation de cles post-quantique standardise par le
NIST (FIPS 203, aout 2024), base sur le probleme Module Learning With Errors (MLWE).

| Parametre | ML-KEM-512 | ML-KEM-768 | ML-KEM-1024 |
|-----------|-----------|-----------|------------|
| Cle publique (ek) | 800 bytes | **1,184 bytes** | 1,568 bytes |
| Texte chiffre (ct) | 768 bytes | **1,088 bytes** | 1,568 bytes |
| Secret partage | 32 bytes | 32 bytes | 32 bytes |
| Categorie NIST | 1 (128-bit) | **3 (192-bit)** | 5 (256-bit) |

**ML-KEM-768** est le choix recommande — c'est le jeu de parametres utilise par
X-Wing, PQXDH de Signal et l'echange de cles hybride TLS de Chrome/Firefox. La
Categorie 3 fournit une marge confortable contre les avancees futures en cryptanalyse
des reseaux.

### Comment Fonctionne le Schema Hybride

**Flux actuel (vulnerable) :**

```text
Expediteur :
  esk = PRF(rseed, rho)                    // deterministe a partir de la note
  epk = [esk] * g_d                         // point de courbe Pallas
  shared_secret = [esk] * pk_d              // ECDH (casse par Shor)
  K_enc = BLAKE2b("Zcash_OrchardKDF", shared_secret || epk)
  enc_ciphertext = ChaCha20(K_enc, note_plaintext)
```

**Flux hybride (resistant au quantique) :**

```text
Expediteur :
  esk = PRF(rseed, rho)                    // inchange
  epk = [esk] * g_d                         // inchange
  ss_ecdh = [esk] * pk_d                    // meme ECDH

  (ct_pq, ss_pq) = ML-KEM-768.Encaps(ek_pq)  // NOUVEAU : KEM base sur les reseaux
                                                // ek_pq de l'adresse du destinataire

  K_enc = BLAKE2b(                          // MODIFIE : combine les deux secrets
      "DashPlatform_HybridKDF",
      ss_ecdh || ss_pq || ct_pq || epk
  )

  enc_ciphertext = ChaCha20(K_enc, note_plaintext)  // inchange
```

**Dechiffrement du destinataire :**

```text
Destinataire :
  ss_ecdh = [ivk] * epk                    // meme ECDH (avec la cle de visualisation entrante)
  ss_pq = ML-KEM-768.Decaps(dk_pq, ct_pq)  // NOUVEAU : desencapsulation KEM
  K_enc = BLAKE2b("DashPlatform_HybridKDF", ss_ecdh || ss_pq || ct_pq || epk)
  note_plaintext = ChaCha20.Decrypt(K_enc, enc_ciphertext)
```

### Garantie de Securite

Le KEM combine est sur IND-CCA2 si **l'un ou l'autre** des KEM composants est
securise. Cela est formellement prouve par [Giacon, Heuer et Poettering (2018)](https://eprint.iacr.org/2018/024)
pour les combineurs de KEM utilisant un PRF (BLAKE2b est eligible), et independamment
par la [preuve de securite de X-Wing](https://eprint.iacr.org/2024/039).

| Scenario | ECDH | ML-KEM | Cle combinee | Statut |
|----------|------|--------|-------------|--------|
| Monde classique | Sur | Sur | **Sur** | Les deux intacts |
| Le quantique casse l'ECC | **Casse** | Sur | **Sur** | ML-KEM protege |
| Des avancees sur les reseaux cassent ML-KEM | Sur | **Casse** | **Sur** | ECDH protege (comme aujourd'hui) |
| Les deux casses | Casse | Casse | **Casse** | Necessite deux percees simultanees |

### Impact sur la Taille

Le KEM hybride ajoute le texte chiffre ML-KEM-768 (1,088 bytes) a chaque note
stockee et agrandit le texte chiffre sortant pour inclure le secret partage ML-KEM
pour la recuperation de l'expediteur :

**Enregistrement stocke par note :**

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

**Stockage a grande echelle :**

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

La cle publique ML-KEM de 1,184 bytes doit etre incluse dans l'adresse pour que les
expediteurs puissent effectuer l'encapsulation. Avec environ 1,960 caracteres Bech32m,
c'est volumineux mais cela tient encore dans un code QR (maximum ~2,953 caracteres
alphanumeriques).

### Gestion des Cles

La paire de cles ML-KEM est derivee de maniere deterministe a partir de la cle
de depense :

```text
SpendingKey (sk) [32 bytes]
  |
  +-> ... (toute la derivation de cles Orchard existante inchangee)
  |
  +-> ml_kem_d = PRF^expand(sk, [0x09])[0..32]
  +-> ml_kem_z = PRF^expand(sk, [0x0A])[0..32]
        |
        +-> (ek_pq, dk_pq) = ML-KEM-768.KeyGen(d=ml_kem_d, z=ml_kem_z)
              ek_pq: 1,184 bytes (publique, incluse dans l'adresse)
              dk_pq: 2,400 bytes (privee, partie de la cle de visualisation)
```

**Aucun changement de sauvegarde necessaire.** La phrase de recuperation de 24 mots
existante couvre la cle ML-KEM car elle est derivee de la cle de depense de maniere
deterministe. La recuperation de portefeuille fonctionne comme avant.

**Les adresses diversifiees** partagent toutes le meme `ek_pq` car ML-KEM n'a pas
de mecanisme de diversification naturel comme la multiplication scalaire de Pallas.
Cela signifie qu'un observateur disposant de deux adresses d'un utilisateur peut les
lier en comparant `ek_pq`.

### Performance de Dechiffrement par Essai

| Etape | Actuel | Hybride | Delta |
|-------|--------|---------|-------|
| Pallas ECDH | ~100 us | ~100 us | — |
| ML-KEM-768 Decaps | — | ~40 us | +40 us |
| BLAKE2b KDF | ~0.5 us | ~1 us | — |
| ChaCha20 (52 bytes) | ~0.1 us | ~0.1 us | — |
| **Total par note** | **~101 us** | **~141 us** | **+40% de surcharge** |

Analyse de 100,000 notes : ~10.1 sec → ~14.1 sec. La surcharge est significative mais
pas prohibitive. La desencapsulation ML-KEM est en temps constant sans avantage de
traitement par lots (contrairement aux operations sur courbes elliptiques), elle evolue
donc lineairement.

### Impact sur les Circuits ZK

**Aucun.** Le KEM hybride est entierement dans la couche de transport/chiffrement. Le
circuit Halo 2 prouve l'existence des notes, la correction des nullifieurs et l'equilibre
des valeurs — il ne prouve rien concernant le chiffrement. Aucun changement aux cles
de preuve, cles de verification ni aux contraintes de circuit.

### Comparaison avec l'Industrie

| Systeme | Approche | Statut |
|---------|----------|--------|
| **Signal** (PQXDH) | X25519 + ML-KEM-768, obligatoire pour tous les utilisateurs | **Deploye** (2023) |
| **Chrome/Firefox TLS** | X25519 + ML-KEM-768 echange de cles hybride | **Deploye** (2024) |
| **X-Wing** (brouillon IETF) | X25519 + ML-KEM-768, combineur dedie | Brouillon de standard |
| **Zcash** | Brouillon ZIP de recuperabilite quantique (recuperation de fonds, pas de chiffrement) | Discussion uniquement |
| **Dash Platform** | Pallas ECDH + ML-KEM-768 (propose) | Phase de conception |

## Quand Deployer

### La Question du Calendrier

- **Etat actuel (2026) :** Aucun ordinateur quantique ne peut casser l'ECC a 255 bits.
  La plus grande factorisation quantique demontree : ~50 bits. Ecart : ordres de grandeur.
- **Court terme (2030-2035) :** Les feuilles de route materielles d'IBM, Google,
  Quantinuum visent des millions de qubits. Les implementations et jeux de parametres
  ML-KEM auront muri.
- **Moyen terme (2035-2050) :** La plupart des estimations situent l'arrivee de la
  CRQC dans cette fenetre. Les donnees HNDL collectees aujourd'hui sont a risque.
- **Long terme (2050+) :** Consensus parmi les cryptographes : les ordinateurs
  quantiques a grande echelle sont une question de "quand", pas de "si".

### Strategie Recommandee

**1. Concevoir pour l'evolutivite des maintenant.** S'assurer que le format
d'enregistrement stocke, la structure `TransmittedNoteCiphertext` et la disposition
des entrees du BulkAppendTree sont versionnes et extensibles. C'est peu couteux et
preserve l'option d'ajouter un KEM hybride plus tard.

**2. Deployer le KEM hybride quand il est pret, le rendre obligatoire.** Ne pas offrir
deux pools (classique et hybride). Diviser l'ensemble d'anonymat annule l'objectif des
transactions blindees — les utilisateurs se cachant parmi un groupe plus petit ont
moins de confidentialite, pas plus. Lors du deploiement, chaque note utilise le
schema hybride.

**3. Viser la fenetre 2028-2030.** C'est bien avant toute menace quantique realiste
mais apres que les implementations de ML-KEM et les tailles de parametres se soient
stabilisees. Cela permet egalement d'apprendre de l'experience de deploiement de
Zcash et Signal.

**4. Surveiller les evenements declencheurs :**
- Le NIST ou la NSA imposant des delais de migration post-quantique
- Des avancees significatives dans le materiel quantique (>100,000 qubits physiques
  avec correction d'erreurs)
- Des avancees cryptanalytiques contre les problemes de reseaux (affecteraient le
  choix de ML-KEM)

### Ce Qui Ne Necessite Pas d'Action Urgente

| Composant | Pourquoi cela peut attendre |
|-----------|---------------------------|
| Signatures d'autorisation de depense | La falsification est en temps reel, pas retroactive. Passer a ML-DSA/SLH-DSA avant l'arrivee de la CRQC. |
| Systeme de preuves Halo 2 | La falsification de preuves est en temps reel. Migrer vers un systeme base sur STARK si necessaire. |
| Resistance aux collisions de Sinsemilla | Utile uniquement pour de nouvelles attaques, pas retroactives. Subsumee par la migration du systeme de preuves. |
| Infrastructure GroveDB Merk/MMR/Blake3 | **Deja sure face au quantique sous les hypotheses cryptographiques actuelles.** Aucune action necessaire basee sur les attaques connues. |

## Reference des Alternatives Post-Quantiques

### Pour le Chiffrement (remplacement de l'ECDH)

| Schema | Type | Cle publique | Texte chiffre | Categorie NIST | Notes |
|--------|------|-------------|--------------|----------------|-------|
| ML-KEM-768 | Lattice (MLWE) | 1,184 B | 1,088 B | 3 (192-bit) | FIPS 203, standard industriel |
| ML-KEM-512 | Lattice (MLWE) | 800 B | 768 B | 1 (128-bit) | Plus petit, marge inferieure |
| ML-KEM-1024 | Lattice (MLWE) | 1,568 B | 1,568 B | 5 (256-bit) | Excessif pour l'hybride |

### Pour les Signatures (remplacement de RedPallas/Schnorr)

| Schema | Type | Cle publique | Signature | Categorie NIST | Notes |
|--------|------|-------------|----------|----------------|-------|
| ML-DSA-65 (Dilithium) | Lattice | 1,952 B | 3,293 B | 3 | FIPS 204, rapide |
| SLH-DSA (SPHINCS+) | Base sur le hachage | 32-64 B | 7,856-49,856 B | 1-5 | FIPS 205, conservateur |
| XMSS/LMS | Base sur le hachage (a etat) | 60 B | 2,500 B | variable | A etat — reutiliser = casser |

### Pour les Preuves ZK (remplacement de Halo 2)

| Systeme | Hypothese | Taille de preuve | Post-quantique | Notes |
|---------|-----------|-----------------|----------------|-------|
| STARKs | Fonctions de hachage (resistance aux collisions) | ~100-400 KB | **Oui** | Utilise par StarkNet |
| Plonky3 | FRI (engagement polynomial base sur le hachage) | ~50-200 KB | **Oui** | Developpement actif |
| Halo 2 (actuel) | ECDLP sur les courbes Pasta | ~5 KB | **Non** | Systeme actuel d'Orchard |
| Lattice SNARKs | MLWE | Recherche | **Oui** | Pas pret pour la production |

### Ecosysteme de Crates Rust

| Crate | Source | FIPS 203 | Verifie | Notes |
|-------|--------|----------|---------|-------|
| `libcrux-ml-kem` | Cryspen | Oui | Formellement verifie (hax/F*) | Plus haute assurance |
| `ml-kem` | RustCrypto | Oui | Temps constant, non audite | Compatibilite ecosysteme |
| `fips203` | integritychain | Oui | Temps constant | Rust pur, no_std |

## Resume

```text
┌─────────────────────────────────────────────────────────────────────┐
│  RESUME DES MENACES QUANTIQUES POUR GROVEDB + ORCHARD              │
│                                                                     │
│  SUR SOUS LES HYPOTHESES ACTUELLES (base sur le hachage) :         │
│    ✓ Arbres Merk Blake3, MMR, BulkAppendTree                       │
│    ✓ BLAKE2b KDF, PRF^expand                                       │
│    ✓ Chiffrement symetrique ChaCha20-Poly1305                      │
│    ✓ Toutes les chaines d'authentification de preuves GroveDB      │
│                                                                     │
│  CORRIGER AVANT LE STOCKAGE DES DONNEES (HNDL retroactif) :        │
│    ✗ Chiffrement de notes (accord de cles ECDH) → KEM Hybride     │
│    ✗ Engagements de valeur (Pedersen) → montants reveles           │
│                                                                     │
│  CORRIGER AVANT L'ARRIVEE DES ORDINATEURS QUANTIQUES               │
│  (temps reel uniquement) :                                          │
│    ~ Autorisation de depense → ML-DSA / SLH-DSA                   │
│    ~ Preuves ZK → STARKs / Plonky3                                │
│    ~ Sinsemilla → arbre Merkle base sur le hachage                 │
│                                                                     │
│  CALENDRIER RECOMMANDE :                                            │
│    2026-2028 : Concevoir pour l'evolutivite, versionner les formats│
│    2028-2030 : Deployer le KEM hybride obligatoire pour le chiffr. │
│    2035+ : Migrer les signatures et le systeme de preuves si besoin│
└─────────────────────────────────────────────────────────────────────┘
```

---
