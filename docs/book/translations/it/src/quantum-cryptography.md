# Crittografia Quantistica — Analisi delle Minacce Post-Quantistiche

Questo capitolo analizza come i computer quantistici influenzerebbero le primitive
crittografiche utilizzate in GroveDB e nei protocolli di transazioni schermate
costruiti sopra di esso (Orchard, Dash Platform). Copre quali componenti sono
vulnerabili, quali sono sicuri, cosa significa "raccogliere ora, decifrare dopo"
per i dati archiviati, e quali strategie di mitigazione esistono — inclusi i design
KEM ibridi.

## Due Algoritmi Quantistici Rilevanti

Solo due algoritmi quantistici sono rilevanti per la crittografia nella pratica:

**L'algoritmo di Shor** risolve il problema del logaritmo discreto (e la
fattorizzazione di interi) in tempo polinomiale. Per una curva ellittica a 255 bit
come Pallas, questo richiede circa 510 qubit logici — ma con l'overhead della
correzione degli errori, il requisito reale e circa 4 milioni di qubit fisici.
L'algoritmo di Shor **rompe completamente** tutta la crittografia a curve
ellittiche indipendentemente dalla dimensione della chiave.

**L'algoritmo di Grover** fornisce un'accelerazione quadratica per la ricerca a
forza bruta. Una chiave simmetrica a 256 bit diventa effettivamente 128 bit.
Tuttavia, la profondita del circuito per l'algoritmo di Grover su uno spazio di
chiavi a 128 bit e ancora 2^64 operazioni quantistiche — molti crittografi ritengono
che questo non sara mai pratico su hardware reale a causa dei limiti di decoerenza.
L'algoritmo di Grover riduce i margini di sicurezza ma non rompe la crittografia
simmetrica ben parametrizzata.

| Algoritmo | Obiettivi | Accelerazione | Impatto pratico |
|-----------|-----------|---------------|-----------------|
| **Shor** | Logaritmo discreto ECC, fattorizzazione RSA | Esponenziale (tempo polinomiale) | **Rottura totale** di ECC |
| **Grover** | Ricerca chiavi simmetriche, preimmagine hash | Quadratica (dimezza i bit della chiave) | 256-bit → 128-bit (ancora sicuro) |

## Primitive Crittografiche di GroveDB

GroveDB e il protocollo schermato basato su Orchard utilizzano un mix di primitive a
curve ellittiche e primitive simmetriche/basate su hash. La tabella seguente classifica
ogni primitiva in base alla sua vulnerabilita quantistica:

### Vulnerabile al Quantistico (algoritmo di Shor — 0 bit post-quantistici)

| Primitiva | Dove e usata | Cosa si rompe |
|-----------|-------------|--------------|
| **Pallas ECDLP** | Impegni di nota (cmx), chiavi effimere (epk/esk), chiavi di visualizzazione (ivk), chiavi di pagamento (pk_d), derivazione dei nullifier | Recuperare qualsiasi chiave privata dalla sua controparte pubblica |
| **Accordo di chiavi ECDH** (Pallas) | Derivazione di chiavi di cifratura simmetriche per testi cifrati delle note | Recuperare il segreto condiviso → decifrare tutte le note |
| **Hash Sinsemilla** | Percorsi Merkle del CommitmentTree, hashing all'interno del circuito | La resistenza alle collisioni dipende da ECDLP; si degrada quando Pallas viene rotto |
| **Halo 2 IPA** | Sistema di prove ZK (impegno polinomiale sulle curve Pasta) | Falsificare prove per dichiarazioni false (contraffazione, spese non autorizzate) |
| **Impegni di Pedersen** | Impegni di valore (cv_net) che nascondono gli importi delle transazioni | Recuperare importi nascosti; falsificare prove di bilancio |

### Sicuro contro il Quantistico (algoritmo di Grover — 128+ bit post-quantistici)

| Primitiva | Dove e usata | Sicurezza post-quantistica |
|-----------|-------------|---------------------------|
| **Blake3** | Hash dei nodi degli alberi Merk, nodi MMR, radici di stato di BulkAppendTree, prefissi dei percorsi dei sottoalberi | 128-bit preimmagine, 128-bit seconda preimmagine |
| **BLAKE2b-256** | KDF per derivazione di chiavi simmetriche, chiave di cifratura in uscita, PRF^expand | 128-bit preimmagine |
| **ChaCha20-Poly1305** | Cifra enc_ciphertext e out_ciphertext (chiavi a 256 bit) | 128-bit ricerca chiave (sicuro, ma il percorso di derivazione della chiave tramite ECDH non lo e) |
| **PRF^expand** (BLAKE2b-512) | Deriva esk, rcm, psi da rseed | 128-bit sicurezza PRF |

### Infrastruttura di GroveDB: Completamente Sicura contro il Quantistico

Tutte le strutture dati proprie di GroveDB si basano esclusivamente sull'hashing Blake3:

- **Alberi AVL Merk** — hash dei nodi, combined_value_hash, propagazione dell'hash figlio
- **Alberi MMR** — hash dei nodi interni, calcolo dei picchi, derivazione della radice
- **BulkAppendTree** — catene di hash del buffer, radici Merkle dense, MMR delle epoche
- **Radice di stato del CommitmentTree** — `blake3("ct_state" || sinsemilla_root || bulk_state_root)`
- **Prefissi dei percorsi dei sottoalberi** — hashing Blake3 dei segmenti di percorso
- **Prove V1** — catene di autenticazione attraverso la gerarchia Merk

**Nessuna modifica necessaria.** Le prove degli alberi Merk di GroveDB, i controlli di
coerenza MMR, le radici delle epoche di BulkAppendTree e tutte le catene di
autenticazione delle prove V1 restano sicure contro i computer quantistici.
L'infrastruttura basata su hash e la parte piu solida del sistema post-quantistico.

## Minacce Retroattive vs. in Tempo Reale

Questa distinzione e fondamentale per stabilire le priorita su cosa correggere e quando.

**Le minacce retroattive** compromettono dati gia archiviati. Un avversario registra
i dati oggi e li decifra quando i computer quantistici saranno disponibili. Queste
minacce **non possono essere mitigate a posteriori** — una volta che i dati sono sulla
blockchain, non possono essere ri-cifrati o ritirati.

**Le minacce in tempo reale** influenzano solo le transazioni create in futuro. Un
avversario con un computer quantistico potrebbe falsificare firme o prove, ma solo
per nuove transazioni. Le vecchie transazioni sono gia state validate e confermate
dalla rete.

| Minaccia | Tipo | Cosa viene esposto | Urgenza |
|----------|------|--------------------|---------|
| **Decifratura delle note** (enc_ciphertext) | **Retroattiva** | Contenuto delle note: destinatario, importo, memo, rseed | **Alta** — archiviato permanentemente |
| **Apertura dell'impegno di valore** (cv_net) | **Retroattiva** | Importi delle transazioni (ma non mittente/destinatario) | **Media** — solo importi |
| **Dati di recupero del mittente** (out_ciphertext) | **Retroattiva** | Chiavi di recupero del mittente per note inviate | **Alta** — archiviato permanentemente |
| Falsificazione dell'autorizzazione di spesa | Tempo reale | Potrebbe falsificare nuove firme di spesa | Bassa — aggiornare prima dell'arrivo del CQ |
| Falsificazione di prove Halo 2 | Tempo reale | Potrebbe falsificare nuove prove (contraffazione) | Bassa — aggiornare prima dell'arrivo del CQ |
| Collisione di Sinsemilla | Tempo reale | Potrebbe falsificare nuovi percorsi Merkle | Bassa — sussunta dalla falsificazione di prove |
| Falsificazione della firma di vincolo | Tempo reale | Potrebbe falsificare nuove prove di bilancio | Bassa — aggiornare prima dell'arrivo del CQ |

### Cosa Viene Rivelato Esattamente?

**Se la cifratura delle note viene rotta** (la minaccia HNDL principale):

Un avversario quantistico recupera `esk` dall'`epk` archiviato tramite l'algoritmo
di Shor, calcola il segreto condiviso ECDH, deriva la chiave simmetrica e decifra
`enc_ciphertext`. Questo rivela il testo in chiaro completo della nota:

| Campo | Dimensione | Cosa rivela |
|-------|-----------|------------|
| version | 1 byte | Versione del protocollo (non sensibile) |
| diversifier | 11 bytes | Componente dell'indirizzo del destinatario |
| value | 8 bytes | Importo esatto della transazione |
| rseed | 32 bytes | Permette il collegamento dei nullifier (deanonimizza il grafo delle transazioni) |
| memo | 36 bytes (DashMemo) | Dati applicativi, potenzialmente identificativi |

Con `rseed` e `rho` (archiviati accanto al testo cifrato), l'avversario puo calcolare
`esk = PRF(rseed, rho)` e verificare il vincolo della chiave effimera. Combinato con
il diversifier, questo collega input a output attraverso l'intera storia delle
transazioni — **deanonimizzazione completa del pool schermato**.

**Se vengono rotti solo gli impegni di valore** (minaccia HNDL secondaria):

L'avversario recupera `v` da `cv_net = [v]*V + [rcv]*R` risolvendo l'ECDLP. Questo
rivela **gli importi delle transazioni ma non le identita del mittente o del
destinatario**. L'avversario vede "qualcuno ha inviato 5.0 Dash a qualcuno" ma non
puo collegare l'importo a nessun indirizzo o identita senza rompere anche la
cifratura delle note.

Di per se, gli importi senza collegamento hanno utilita limitata. Ma combinati con
dati esterni (tempistica, fatture note, importi corrispondenti a richieste pubbliche),
gli attacchi di correlazione diventano possibili.

## L'Attacco "Raccogli Ora, Decifra Dopo"

Questa e la minaccia quantistica piu urgente e pratica.

**Modello di attacco:** Un avversario statale (o qualsiasi parte con sufficiente
capacita di archiviazione) registra tutti i dati delle transazioni schermate on-chain
oggi. Questi dati sono pubblicamente disponibili sulla blockchain e sono immutabili.
L'avversario attende un computer quantistico crittograficamente rilevante (CRQC),
poi:

```text
Passo 1: Leggere il record archiviato dal BulkAppendTree del CommitmentTree:
         cmx (32) || rho (32) || epk (32) || enc_ciphertext (104) || out_ciphertext (80)

Passo 2: Risolvere l'ECDLP su Pallas tramite l'algoritmo di Shor:
         epk = [esk] * g_d  →  recuperare esk

Passo 3: Calcolare il segreto condiviso:
         shared_secret = [esk] * pk_d

Passo 4: Derivare la chiave simmetrica (BLAKE2b e sicuro quantisticamente, ma l'input e compromesso):
         K_enc = BLAKE2b-256("Zcash_OrchardKDF", shared_secret || epk)

Passo 5: Decifrare enc_ciphertext usando ChaCha20-Poly1305:
         → version || diversifier || value || rseed || memo

Passo 6: Con rseed + rho, collegare i nullifier agli impegni di nota:
         esk = PRF(rseed, rho)
         → ricostruzione completa del grafo delle transazioni
```

**Punto chiave:** La cifratura simmetrica (ChaCha20-Poly1305) e perfettamente sicura
contro il quantistico. La vulnerabilita risiede interamente nel **percorso di
derivazione della chiave** — la chiave simmetrica e derivata da un segreto condiviso
ECDH, e l'ECDH e rotto dall'algoritmo di Shor. L'attaccante non rompe la cifratura;
recupera la chiave.

**Retroattivita:** Questo attacco e **completamente retroattivo**. Ogni nota cifrata
mai archiviata sulla blockchain puo essere decifrata una volta che esiste un CRQC.
I dati non possono essere ri-cifrati o protetti a posteriori. Ecco perche deve essere
affrontato prima che i dati vengano archiviati, non dopo.

## Mitigazione: KEM Ibrido (ML-KEM + ECDH)

La difesa contro l'HNDL consiste nel derivare la chiave di cifratura simmetrica da
**due meccanismi indipendenti di accordo di chiavi**, in modo che romperne solo uno
sia insufficiente. Questo si chiama KEM ibrido.

### ML-KEM-768 (CRYSTALS-Kyber)

ML-KEM e il meccanismo di incapsulamento di chiavi post-quantistico standardizzato
dal NIST (FIPS 203, agosto 2024) basato sul problema Module Learning With Errors
(MLWE).

| Parametro | ML-KEM-512 | ML-KEM-768 | ML-KEM-1024 |
|-----------|-----------|-----------|------------|
| Chiave pubblica (ek) | 800 bytes | **1,184 bytes** | 1,568 bytes |
| Testo cifrato (ct) | 768 bytes | **1,088 bytes** | 1,568 bytes |
| Segreto condiviso | 32 bytes | 32 bytes | 32 bytes |
| Categoria NIST | 1 (128-bit) | **3 (192-bit)** | 5 (256-bit) |

**ML-KEM-768** e la scelta raccomandata — e il set di parametri utilizzato da X-Wing,
PQXDH di Signal e lo scambio di chiavi ibrido TLS di Chrome/Firefox. La Categoria 3
fornisce un margine confortevole contro futuri avanzamenti nella crittoanalisi dei
reticoli.

### Come Funziona lo Schema Ibrido

**Flusso attuale (vulnerabile):**

```text
Mittente:
  esk = PRF(rseed, rho)                    // deterministico dalla nota
  epk = [esk] * g_d                         // punto della curva Pallas
  shared_secret = [esk] * pk_d              // ECDH (rotto da Shor)
  K_enc = BLAKE2b("Zcash_OrchardKDF", shared_secret || epk)
  enc_ciphertext = ChaCha20(K_enc, note_plaintext)
```

**Flusso ibrido (resistente al quantistico):**

```text
Mittente:
  esk = PRF(rseed, rho)                    // invariato
  epk = [esk] * g_d                         // invariato
  ss_ecdh = [esk] * pk_d                    // stesso ECDH

  (ct_pq, ss_pq) = ML-KEM-768.Encaps(ek_pq)  // NUOVO: KEM basato su reticoli
                                                // ek_pq dall'indirizzo del destinatario

  K_enc = BLAKE2b(                          // MODIFICATO: combina entrambi i segreti
      "DashPlatform_HybridKDF",
      ss_ecdh || ss_pq || ct_pq || epk
  )

  enc_ciphertext = ChaCha20(K_enc, note_plaintext)  // invariato
```

**Decifratura del destinatario:**

```text
Destinatario:
  ss_ecdh = [ivk] * epk                    // stesso ECDH (usando la chiave di visualizzazione in entrata)
  ss_pq = ML-KEM-768.Decaps(dk_pq, ct_pq)  // NUOVO: decapsulamento KEM
  K_enc = BLAKE2b("DashPlatform_HybridKDF", ss_ecdh || ss_pq || ct_pq || epk)
  note_plaintext = ChaCha20.Decrypt(K_enc, enc_ciphertext)
```

### Garanzia di Sicurezza

Il KEM combinato e sicuro IND-CCA2 se **uno qualsiasi** dei KEM componenti e sicuro.
Questo e formalmente dimostrato da [Giacon, Heuer e Poettering (2018)](https://eprint.iacr.org/2018/024)
per i combinatori di KEM che usano un PRF (BLAKE2b si qualifica), e indipendentemente
dalla [prova di sicurezza di X-Wing](https://eprint.iacr.org/2024/039).

| Scenario | ECDH | ML-KEM | Chiave combinata | Stato |
|----------|------|--------|-----------------|-------|
| Mondo classico | Sicuro | Sicuro | **Sicuro** | Entrambi intatti |
| Il quantistico rompe ECC | **Rotto** | Sicuro | **Sicuro** | ML-KEM protegge |
| Avanzamenti sui reticoli rompono ML-KEM | Sicuro | **Rotto** | **Sicuro** | ECDH protegge (come oggi) |
| Entrambi rotti | Rotto | Rotto | **Rotto** | Richiede due scoperte simultanee |

### Impatto sulla Dimensione

Il KEM ibrido aggiunge il testo cifrato ML-KEM-768 (1,088 bytes) a ogni nota
archiviata e espande il testo cifrato in uscita per includere il segreto condiviso
ML-KEM per il recupero del mittente:

**Record archiviato per nota:**

```text
┌──────────────────────────────────────────────────────────────────┐
│  Attuale (280 bytes)           Ibrido (1,400 bytes)              │
│                                                                  │
│  cmx:             32         cmx:              32               │
│  rho:             32         rho:              32               │
│  epk:             32         epk:              32               │
│  enc_ciphertext: 104         ct_pq:         1,088  ← NUOVO     │
│  out_ciphertext:  80         enc_ciphertext:  104               │
│                              out_ciphertext:  112  ← +32        │
│  ─────────────────           ──────────────────────             │
│  Total:          280         Total:          1,400  (5.0x)      │
└──────────────────────────────────────────────────────────────────┘
```

**Archiviazione su scala:**

| Note | Attuale (280 B) | Ibrido (1,400 B) | Delta |
|------|----------------|------------------|-------|
| 100,000 | 26.7 MB | 133 MB | +106 MB |
| 1,000,000 | 267 MB | 1.33 GB | +1.07 GB |
| 10,000,000 | 2.67 GB | 13.3 GB | +10.7 GB |

**Dimensione dell'indirizzo:**

```text
Attuale: diversifier (11) + pk_d (32) = 43 bytes
Ibrido:  diversifier (11) + pk_d (32) + ek_pq (1,184) = 1,227 bytes
```

La chiave pubblica ML-KEM di 1,184 byte deve essere inclusa nell'indirizzo affinche
i mittenti possano eseguire l'incapsulamento. Con circa 1,960 caratteri Bech32m, e
grande ma rientra ancora in un codice QR (massimo ~2,953 caratteri alfanumerici).

### Gestione delle Chiavi

La coppia di chiavi ML-KEM e derivata deterministicamente dalla chiave di spesa:

```text
SpendingKey (sk) [32 bytes]
  |
  +-> ... (tutta la derivazione di chiavi Orchard esistente invariata)
  |
  +-> ml_kem_d = PRF^expand(sk, [0x09])[0..32]
  +-> ml_kem_z = PRF^expand(sk, [0x0A])[0..32]
        |
        +-> (ek_pq, dk_pq) = ML-KEM-768.KeyGen(d=ml_kem_d, z=ml_kem_z)
              ek_pq: 1,184 bytes (pubblica, inclusa nell'indirizzo)
              dk_pq: 2,400 bytes (privata, parte della chiave di visualizzazione)
```

**Nessuna modifica ai backup necessaria.** La frase seed di 24 parole esistente copre
la chiave ML-KEM perche e derivata deterministicamente dalla chiave di spesa. Il
recupero del portafoglio funziona come prima.

**Gli indirizzi diversificati** condividono tutti lo stesso `ek_pq` perche ML-KEM non
ha un meccanismo di diversificazione naturale come la moltiplicazione scalare di
Pallas. Questo significa che un osservatore con due indirizzi di un utente puo
collegarli confrontando `ek_pq`.

### Prestazioni della Decifratura di Prova

| Passo | Attuale | Ibrido | Delta |
|-------|---------|--------|-------|
| Pallas ECDH | ~100 us | ~100 us | — |
| ML-KEM-768 Decaps | — | ~40 us | +40 us |
| BLAKE2b KDF | ~0.5 us | ~1 us | — |
| ChaCha20 (52 bytes) | ~0.1 us | ~0.1 us | — |
| **Totale per nota** | **~101 us** | **~141 us** | **+40% overhead** |

Scansione di 100,000 note: ~10.1 sec → ~14.1 sec. L'overhead e significativo ma non
proibitivo. Il decapsulamento ML-KEM e in tempo costante senza vantaggio di
elaborazione in batch (a differenza delle operazioni su curve ellittiche), quindi scala
linearmente.

### Impatto sui Circuiti ZK

**Nessuno.** Il KEM ibrido e interamente nel livello di trasporto/cifratura. Il
circuito Halo 2 dimostra l'esistenza delle note, la correttezza dei nullifier e il
bilancio dei valori — non dimostra nulla riguardo alla cifratura. Nessuna modifica
alle chiavi di prova, chiavi di verifica o vincoli del circuito.

### Confronto con l'Industria

| Sistema | Approccio | Stato |
|---------|-----------|-------|
| **Signal** (PQXDH) | X25519 + ML-KEM-768, obbligatorio per tutti gli utenti | **Distribuito** (2023) |
| **Chrome/Firefox TLS** | X25519 + ML-KEM-768 scambio di chiavi ibrido | **Distribuito** (2024) |
| **X-Wing** (bozza IETF) | X25519 + ML-KEM-768, combinatore dedicato | Bozza di standard |
| **Zcash** | Bozza ZIP per recuperabilita quantistica (recupero fondi, non cifratura) | Solo discussione |
| **Dash Platform** | Pallas ECDH + ML-KEM-768 (proposto) | Fase di progettazione |

## Quando Distribuire

### La Questione della Tempistica

- **Stato attuale (2026):** Nessun computer quantistico puo rompere ECC a 255 bit.
  La piu grande fattorizzazione quantistica dimostrata: ~50 bit. Divario: ordini di
  grandezza.
- **Breve termine (2030-2035):** Le roadmap hardware di IBM, Google, Quantinuum puntano
  a milioni di qubit. Le implementazioni e i set di parametri ML-KEM saranno maturati.
- **Medio termine (2035-2050):** La maggior parte delle stime colloca l'arrivo del CRQC
  in questa finestra. I dati HNDL raccolti oggi sono a rischio.
- **Lungo termine (2050+):** Consenso tra i crittografi: i computer quantistici su
  larga scala sono una questione di "quando", non di "se".

### Strategia Raccomandata

**1. Progettare per l'aggiornabilita ora.** Assicurarsi che il formato del record
archiviato, la struttura `TransmittedNoteCiphertext` e il layout delle voci del
BulkAppendTree siano versionati e estensibili. Questo ha un basso costo e preserva
l'opzione di aggiungere il KEM ibrido in seguito.

**2. Distribuire il KEM ibrido quando pronto, renderlo obbligatorio.** Non offrire due
pool (classico e ibrido). Dividere l'insieme di anonimato vanifica lo scopo delle
transazioni schermate — gli utenti che si nascondono in un gruppo piu piccolo hanno
meno privacy, non di piu. Quando distribuito, ogni nota utilizza lo schema ibrido.

**3. Puntare alla finestra 2028-2030.** Questo e ben prima di qualsiasi minaccia
quantistica realistica ma dopo che le implementazioni di ML-KEM e le dimensioni dei
parametri si saranno stabilizzate. Consente anche di apprendere dall'esperienza di
distribuzione di Zcash e Signal.

**4. Monitorare gli eventi scatenanti:**
- NIST o NSA che impongono scadenze di migrazione post-quantistica
- Avanzamenti significativi nell'hardware quantistico (>100,000 qubit fisici con
  correzione degli errori)
- Avanzamenti crittoanalitici contro problemi di reticoli (influenzerebbero la scelta
  di ML-KEM)

### Cosa Non Richiede Azione Urgente

| Componente | Perche puo aspettare |
|------------|---------------------|
| Firme di autorizzazione di spesa | La falsificazione e in tempo reale, non retroattiva. Aggiornare a ML-DSA/SLH-DSA prima dell'arrivo del CRQC. |
| Sistema di prove Halo 2 | La falsificazione di prove e in tempo reale. Migrare a un sistema basato su STARK quando necessario. |
| Resistenza alle collisioni di Sinsemilla | Utile solo per nuovi attacchi, non retroattivi. Sussunta dalla migrazione del sistema di prove. |
| Infrastruttura GroveDB Merk/MMR/Blake3 | **Già sicura sotto le attuali assunzioni crittografiche.** Nessuna azione necessaria in base agli attacchi noti. |

## Riferimento delle Alternative Post-Quantistiche

### Per la Cifratura (sostituzione di ECDH)

| Schema | Tipo | Chiave pubblica | Testo cifrato | Categoria NIST | Note |
|--------|------|----------------|--------------|----------------|------|
| ML-KEM-768 | Lattice (MLWE) | 1,184 B | 1,088 B | 3 (192-bit) | FIPS 203, standard industriale |
| ML-KEM-512 | Lattice (MLWE) | 800 B | 768 B | 1 (128-bit) | Piu piccolo, margine inferiore |
| ML-KEM-1024 | Lattice (MLWE) | 1,568 B | 1,568 B | 5 (256-bit) | Eccessivo per l'ibrido |

### Per le Firme (sostituzione di RedPallas/Schnorr)

| Schema | Tipo | Chiave pubblica | Firma | Categoria NIST | Note |
|--------|------|----------------|-------|----------------|------|
| ML-DSA-65 (Dilithium) | Lattice | 1,952 B | 3,293 B | 3 | FIPS 204, veloce |
| SLH-DSA (SPHINCS+) | Basato su hash | 32-64 B | 7,856-49,856 B | 1-5 | FIPS 205, conservativo |
| XMSS/LMS | Basato su hash (con stato) | 60 B | 2,500 B | variabile | Con stato — riutilizzare = rompere |

### Per le Prove ZK (sostituzione di Halo 2)

| Sistema | Ipotesi | Dimensione prova | Post-quantistico | Note |
|---------|---------|-----------------|-----------------|------|
| STARKs | Funzioni hash (resistenza alle collisioni) | ~100-400 KB | **Si** | Usato da StarkNet |
| Plonky3 | FRI (impegno polinomiale basato su hash) | ~50-200 KB | **Si** | Sviluppo attivo |
| Halo 2 (attuale) | ECDLP sulle curve Pasta | ~5 KB | **No** | Sistema attuale di Orchard |
| Lattice SNARKs | MLWE | Ricerca | **Si** | Non pronto per la produzione |

### Ecosistema di Crate Rust

| Crate | Fonte | FIPS 203 | Verificato | Note |
|-------|-------|----------|------------|------|
| `libcrux-ml-kem` | Cryspen | Si | Formalmente verificato (hax/F*) | Massima garanzia |
| `ml-kem` | RustCrypto | Si | Tempo costante, non verificato | Compatibilita con l'ecosistema |
| `fips203` | integritychain | Si | Tempo costante | Rust puro, no_std |

## Riepilogo

```text
┌─────────────────────────────────────────────────────────────────────┐
│  RIEPILOGO DELLE MINACCE QUANTISTICHE PER GROVEDB + ORCHARD       │
│                                                                     │
│  SICURO SOTTO LE ATTUALI ASSUNZIONI (basato su hash):              │
│    ✓ Alberi Merk Blake3, MMR, BulkAppendTree                       │
│    ✓ BLAKE2b KDF, PRF^expand                                       │
│    ✓ Cifratura simmetrica ChaCha20-Poly1305                        │
│    ✓ Tutte le catene di autenticazione delle prove di GroveDB      │
│                                                                     │
│  CORREGGERE PRIMA DI ARCHIVIARE I DATI (HNDL retroattivo):        │
│    ✗ Cifratura delle note (accordo di chiavi ECDH) → KEM Ibrido   │
│    ✗ Impegni di valore (Pedersen) → importi rivelati               │
│                                                                     │
│  CORREGGERE PRIMA DELL'ARRIVO DEI COMPUTER QUANTISTICI             │
│  (solo tempo reale):                                                │
│    ~ Autorizzazione di spesa → ML-DSA / SLH-DSA                   │
│    ~ Prove ZK → STARKs / Plonky3                                  │
│    ~ Sinsemilla → albero Merkle basato su hash                     │
│                                                                     │
│  TEMPISTICA RACCOMANDATA:                                           │
│    2026-2028: Progettare per aggiornabilita, versionare i formati  │
│    2028-2030: Distribuire KEM ibrido obbligatorio per la cifratura │
│    2035+: Migrare firme e sistema di prove se necessario           │
└─────────────────────────────────────────────────────────────────────┘
```

---
