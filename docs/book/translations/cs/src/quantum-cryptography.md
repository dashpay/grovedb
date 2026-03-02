# Kvantova kryptografie — Analyza postkvantovych hrozeb

Tato kapitola analyzuje, jak by kvantove pocitace ovlivnily kryptograficke
primitivy pouzivane v GroveDB a protokolech chranenych transakci postavenych
na nem (Orchard, Dash Platform). Pokryva, ktere komponenty jsou zranitelne,
ktere jsou bezpecne, co znamena "harvest now, decrypt later" pro ulozena data
a jake existuji strategie zmirnovani — vcetne hybridnich navrhu KEM.

## Dva kvantove algoritmy, na kterych zalezi

Pouze dva kvantove algoritmy jsou pro kryptografii v praxi relevantni:

**Shoruv algoritmus** resi problem diskretniho logaritmu (a faktorizace celych
cisel) v polynomialnim case. Pro 255-bitovou eliptickou krivku jako Pallas
to vyzaduje priblizne 510 logickych qubitu — ale s rezii opravy chyb je skutecny
pozadavek priblizne 4 miliony fyzickych qubitu. Shoruv algoritmus **uplne
prolomil** veskerou kryptografii eliptickych krivek bez ohledu na velikost
klice.

**Groveruv algoritmus** poskytuje kvadraticke zrychleni pro hledani hrubou silou.
256-bitovy symetricky klic se efektivne stane 128-bitovym. Nicmene hloubka
obvodu pro Groveruv algoritmus na 128-bitovem prostoru klicu je stale 2^64
kvantovych operaci — mnoho kryptografu veri, ze to nikdy nebude prakticke na
skutecnem hardwaru kvuli limitum dekoherence. Groveruv algoritmus snizuje
bezpecnostni marze, ale neprolomuje dobre parametrizovanou symetrickou
kryptografii.

| Algoritmus | Cile | Zrychleni | Prakticky dopad |
|-----------|------|-----------|-----------------|
| **Shor** | ECC discrete log, RSA factoring | Exponencialni (polynomialni cas) | **Uplne prolomeni** ECC |
| **Grover** | Hledani symetrickych klicu, hash preimage | Kvadraticke (puleni bitu klice) | 256-bit → 128-bit (stale bezpecne) |

## Kryptograficke primitivy GroveDB

GroveDB a chraneny protokol zalozeny na Orchard pouzivaji smes primitiv
eliptickych krivek a symetrickych/hashovacich primitiv. Nasledujici tabulka
klasifikuje kazdy primitiv podle jeho kvantove zranitelnosti:

### Kvantove zranitelne (Shoruv algoritmus — 0 bitu postkvantove)

| Primitiv | Kde se pouziva | Co se prolomuje |
|----------|---------------|-----------------|
| **Pallas ECDLP** | Note commitments (cmx), ephemeral keys (epk/esk), viewing keys (ivk), payment keys (pk_d), nullifier derivation | Obnoveni libovolneho soukromeho klice z jeho verejneho protejsku |
| **ECDH key agreement** (Pallas) | Odvozovani symetrickych sifrovacich klicu pro note ciphertexts | Obnoveni shared secret → desifrovani vsech notes |
| **Sinsemilla hash** | Merkle cesty CommitmentTree, in-circuit hashing | Odolnost proti kolizim zavisi na ECDLP; degraduje, kdyz je Pallas prolomen |
| **Halo 2 IPA** | ZK system dukazu (polynomial commitment over Pasta curves) | Paddelani dukazu pro nepravdive tvrzeni (padelky, neopravnene utraty) |
| **Pedersen commitments** | Value commitments (cv_net) skryvajici castky transakci | Obnoveni skrytych castek; paddelani dukazu zustatku |

### Kvantove bezpecne (Groveruv algoritmus — 128+ bitu postkvantove)

| Primitiv | Kde se pouziva | Postkvantove zabezpeceni |
|----------|---------------|--------------------------|
| **Blake3** | Hashe uzlu Merk tree, MMR nodes, BulkAppendTree state roots, subtree path prefixes | 128-bit preimage, 128-bit second-preimage |
| **BLAKE2b-256** | KDF pro odvozovani symetrickych klicu, outgoing cipher key, PRF^expand | 128-bit preimage |
| **ChaCha20-Poly1305** | Sifruje enc_ciphertext a out_ciphertext (256-bit klice) | 128-bit key search (bezpecne, ale cesta odvozovani klice pres ECDH neni) |
| **PRF^expand** (BLAKE2b-512) | Odvozuje esk, rcm, psi z rseed | 128-bit PRF security |

### Infrastruktura GroveDB: Plne kvantove bezpecna

Vsechny datove struktury GroveDB se spolehaji vyhradne na hashovani Blake3:

- **Merk AVL trees** — hashe uzlu, combined_value_hash, propagace child hash
- **MMR trees** — hashe internich uzlu, vypocet spicek, odvozovani root
- **BulkAppendTree** — hashove retezce bufferu, dense Merkle roots, epoch MMR
- **CommitmentTree state root** — `blake3("ct_state" || sinsemilla_root || bulk_state_root)`
- **Subtree path prefixes** — Blake3 hashovani segmentu cest
- **V1 proofs** — retezce autentizace skrze hierarchii Merk

**Nejsou potreba zadne zmeny.** Dukazy Merk tree GroveDB, kontroly konzistence
MMR, rooty epoch BulkAppendTree a vsechny retezce autentizace dukazu V1
zustavaji bezpecne proti kvantovym pocitacum. Infrastruktura zalozena na
hashich je nejsilnejsi cast systemu v postkvantovem svete.

## Retroaktivni vs hrozby v realnem case

Toto rozliseni je klicove pro stanoveni priorit, co opravit a kdy.

**Retroaktivni hrozby** kompromituji data, ktera jsou jiz ulozena. Protivnik
zaznamenava data dnes a desifruje je, az budou kvantove pocitace k dispozici.
Tyto hrozby **nelze zmirnil dodatecne** — jakmile jsou data na retezci, nelze
je znovu zasifrovat ani stahnout.

**Hrozby v realnem case** ovlivnuji pouze transakce vytvorene v budoucnosti.
Protivnik s kvantovym pocitacem by mohl padelat podpisy nebo dukazy, ale pouze
pro nove transakce. Stare transakce jiz byly validovany a potvrzeny siti.

| Hrozba | Typ | Co se odhalí | Naléhavost |
|--------|-----|-------------|------------|
| **Desifrovani note** (enc_ciphertext) | **Retroaktivni** | Obsah note: prijemce, castka, memo, rseed | **Vysoka** — ulozeno navzdy |
| **Otevreni value commitment** (cv_net) | **Retroaktivni** | Castky transakci (ale ne odesilatel/prijemce) | **Stredni** — pouze castky |
| **Data obnovy odesilatele** (out_ciphertext) | **Retroaktivni** | Klice obnovy odesilatele pro odeslane notes | **Vysoka** — ulozeno navzdy |
| Paddelani autorizace utraty | V realnem case | Mohl by padelat nove podpisy utraty | Nizka — aktualizace pred prichodem QC |
| Paddelani dukazu Halo 2 | V realnem case | Mohl by padelat nove dukazy (padelky) | Nizka — aktualizace pred prichodem QC |
| Kolize Sinsemilla | V realnem case | Mohl by padelat nove Merkle cesty | Nizka — zahrnuto v paddelani dukazu |
| Paddelani binding podpisu | V realnem case | Mohl by padelat nove dukazy zustatku | Nizka — aktualizace pred prichodem QC |

### Co presne se odhali?

**Pokud je sifrovani note prolomeno** (hlavni hrozba HNDL):

Kvantovy protivnik obnovi `esk` z ulozeneho `epk` pomoci Shorova algoritmu,
vypocita shared secret ECDH, odvodi symetricky klic a desifruje `enc_ciphertext`.
To odhali uplny plaintext note:

| Pole | Velikost | Co odhaluje |
|------|----------|------------|
| version | 1 byte | Verze protokolu (necitlive) |
| diversifier | 11 bytes | Komponenta adresy prijemce |
| value | 8 bytes | Presna castka transakce |
| rseed | 32 bytes | Umoznuje propojeni nullifier (deanonymizace grafu transakci) |
| memo | 36 bytes (DashMemo) | Aplikacni data, potencialne identifikujici |

S `rseed` a `rho` (ulozenych vedle sifrovaneho textu) muze protivnik vypocitat
`esk = PRF(rseed, rho)` a overit vazbu efemernich klicu. V kombinaci s
diversifier to propojuje vstupy s vystupy napric celou historii transakci —
**uplna deanonymizace chraneného poolu**.

**Pokud jsou prolomeny pouze value commitments** (sekundarni hrozba HNDL):

Protivnik obnovi `v` z `cv_net = [v]*V + [rcv]*R` resenim ECDLP. To odhali
**castky transakci, ale ne identitu odesilatele nebo prijemce**. Protivnik
vidi "nekdo poslal 5.0 Dash nekome", ale nemuze propojit castku s zadnou
adresou nebo identitou bez prolomeni sifrovani note.

Castky bez propojeni maji samy o sobe omezenou uzitecnost. Ale v kombinaci s
externimi daty (casovani, zname faktury, castky odpovidajici verejnym
pozadavkum) se korelacni utoky stavaji moznymi.

## Utok "Harvest Now, Decrypt Later"

Toto je nejnalehavejsi a nejpraktictejsi kvantova hrozba.

**Model utoku:** Statni protivnik (nebo jakekoliv strana s dostatecnym
ulozistem) zaznamenava vsechna data chranenych transakci na retezci dnes. Tato
data jsou verejne dostupna na blockchainu a nemenitelna. Protivnik ceka na
kryptograficky relevantni kvantovy pocitac (CRQC) a pote:

```text
Step 1: Read stored record from CommitmentTree BulkAppendTree:
        cmx (32) || rho (32) || epk (32) || enc_ciphertext (104) || out_ciphertext (80)

Step 2: Solve ECDLP on Pallas via Shor's algorithm:
        epk = [esk] * g_d  →  recover esk

Step 3: Compute shared secret:
        shared_secret = [esk] * pk_d

Step 4: Derive symmetric key (BLAKE2b is quantum-safe, but input is compromised):
        K_enc = BLAKE2b-256("Zcash_OrchardKDF", shared_secret || epk)

Step 5: Decrypt enc_ciphertext using ChaCha20-Poly1305:
        → version || diversifier || value || rseed || memo

Step 6: With rseed + rho, link nullifiers to note commitments:
        esk = PRF(rseed, rho)
        → full transaction graph reconstruction
```

**Klicovy poznatek:** Symetricke sifrovani (ChaCha20-Poly1305) je dokonale
kvantove bezpecne. Zranitelnost je zcela v **ceste odvozovani klice** —
symetricky klic je odvozen ze shared secret ECDH a ECDH je prolomen Shorovym
algoritmem. Utocnik neprolomuje sifrovani; obnovuje klic.

**Retroaktivita:** Tento utok je **plne retroaktivni**. Kazdy zasifrovany note
kdykoli ulozeny na retezci muze byt desifrovana, jakmile CRQC existuje. Data
nelze znovu zasifrovat ani chranit dodatecne. Proto se to musi resit pred
ulozenim dat, ne az po.

## Zmirnovani: Hybridni KEM (ML-KEM + ECDH)

Obranou pred HNDL je odvozovani symetrickeho sifrovaciho klice z **dvou
nezavislych mechanismu dohody klicu**, takze prolomeni pouze jednoho je
nedostatecne. Tomu se rika hybridni KEM.

### ML-KEM-768 (CRYSTALS-Kyber)

ML-KEM je postkvantovy mechanismus zapouzdreni klicu standardizovany NIST
(FIPS 203, srpen 2024) zalozeny na problemu Module Learning With Errors (MLWE).

| Parametr | ML-KEM-512 | ML-KEM-768 | ML-KEM-1024 |
|----------|-----------|-----------|------------|
| Public key (ek) | 800 bytes | **1 184 bytes** | 1 568 bytes |
| Ciphertext (ct) | 768 bytes | **1 088 bytes** | 1 568 bytes |
| Shared secret | 32 bytes | 32 bytes | 32 bytes |
| Kategorie NIST | 1 (128-bit) | **3 (192-bit)** | 5 (256-bit) |

**ML-KEM-768** je doporucena volba — je to sada parametru pouzivana X-Wing,
PQXDH protokolem Signal a hybridni vymenou klicu TLS v Chrome/Firefox.
Kategorie 3 poskytuje pohodlnou marzi proti budoucim pokrokum kryptoanalyzy
mrizek.

### Jak hybridni schema funguje

**Soucasny tok (zranitelny):**

```text
Sender:
  esk = PRF(rseed, rho)                    // deterministic from note
  epk = [esk] * g_d                         // Pallas curve point
  shared_secret = [esk] * pk_d              // ECDH (broken by Shor's)
  K_enc = BLAKE2b("Zcash_OrchardKDF", shared_secret || epk)
  enc_ciphertext = ChaCha20(K_enc, note_plaintext)
```

**Hybridni tok (kvantove odolny):**

```text
Sender:
  esk = PRF(rseed, rho)                    // unchanged
  epk = [esk] * g_d                         // unchanged
  ss_ecdh = [esk] * pk_d                    // same ECDH

  (ct_pq, ss_pq) = ML-KEM-768.Encaps(ek_pq)  // NEW: lattice-based KEM
                                                // ek_pq from recipient's address

  K_enc = BLAKE2b(                          // MODIFIED: combines both secrets
      "DashPlatform_HybridKDF",
      ss_ecdh || ss_pq || ct_pq || epk
  )

  enc_ciphertext = ChaCha20(K_enc, note_plaintext)  // unchanged
```

**Desifrovani prijemcem:**

```text
Recipient:
  ss_ecdh = [ivk] * epk                    // same ECDH (using incoming viewing key)
  ss_pq = ML-KEM-768.Decaps(dk_pq, ct_pq)  // NEW: KEM decapsulation
  K_enc = BLAKE2b("DashPlatform_HybridKDF", ss_ecdh || ss_pq || ct_pq || epk)
  note_plaintext = ChaCha20.Decrypt(K_enc, enc_ciphertext)
```

### Zaruka bezpecnosti

Kombinovany KEM je bezpecny IND-CCA2, pokud je **kterykoli** komponentni KEM
bezpecny. To je formalne dokazano
[Giaconem, Heuerem a Poetteringem (2018)](https://eprint.iacr.org/2018/024)
pro kombinatory KEM pouzivajici PRF (BLAKE2b splnuje podminky) a nezavisle
[dukazem bezpecnosti X-Wing](https://eprint.iacr.org/2024/039).

| Scenar | ECDH | ML-KEM | Kombinovany klic | Status |
|--------|------|--------|-----------------|--------|
| Klasicky svet | Bezpecny | Bezpecny | **Bezpecny** | Oba nedotcene |
| Kvant prolomuje ECC | **Prolomeny** | Bezpecny | **Bezpecny** | ML-KEM chrani |
| Pokroky mrizek prolomuji ML-KEM | Bezpecny | **Prolomeny** | **Bezpecny** | ECDH chrani (stejne jako dnes) |
| Oba prolomeny | Prolomeny | Prolomeny | **Prolomeny** | Vyzaduje dva soucasne prurivy |

### Dopad na velikost

Hybridni KEM pridava sifrovy text ML-KEM-768 (1 088 bajtu) ke kazdemu
ulozenemu note a rozsiruje odchozi sifrovy text o shared secret ML-KEM pro
obnoveni odesilatele:

**Ulozeny zaznam na note:**

```text
┌──────────────────────────────────────────────────────────────────┐
│  Current (280 bytes)         Hybrid (1,400 bytes)               │
│                                                                  │
│  cmx:             32         cmx:              32               │
│  rho:             32         rho:              32               │
│  epk:             32         epk:              32               │
│  enc_ciphertext: 104         ct_pq:         1,088  ← NEW       │
│  out_ciphertext:  80         enc_ciphertext:  104               │
│                              out_ciphertext:  112  ← +32        │
│  ─────────────────           ──────────────────────             │
│  Total:          280         Total:          1,400  (5.0x)      │
└──────────────────────────────────────────────────────────────────┘
```

**Uloziste ve velkém meritku:**

| Notes | Soucasne (280 B) | Hybridni (1 400 B) | Delta |
|-------|-----------------|---------------------|-------|
| 100 000 | 26,7 MB | 133 MB | +106 MB |
| 1 000 000 | 267 MB | 1,33 GB | +1,07 GB |
| 10 000 000 | 2,67 GB | 13,3 GB | +10,7 GB |

**Velikost adresy:**

```text
Current:  diversifier (11) + pk_d (32) = 43 bytes
Hybrid:   diversifier (11) + pk_d (32) + ek_pq (1,184) = 1,227 bytes
```

1 184-bajtovy verejny klic ML-KEM musi byt obsazen v adrese, aby odesilatel
mohl provest zapouzdreni. Pri ~1 960 znacich Bech32m je to velke, ale stale se
vejde do QR kodu (max. ~2 953 alfanumerickych znaku).

### Sprava klicu

Par klicu ML-KEM je deterministicky odvozen ze spending key:

```text
SpendingKey (sk) [32 bytes]
  |
  +-> ... (all existing Orchard key derivation unchanged)
  |
  +-> ml_kem_d = PRF^expand(sk, [0x09])[0..32]
  +-> ml_kem_z = PRF^expand(sk, [0x0A])[0..32]
        |
        +-> (ek_pq, dk_pq) = ML-KEM-768.KeyGen(d=ml_kem_d, z=ml_kem_z)
              ek_pq: 1,184 bytes (public, included in address)
              dk_pq: 2,400 bytes (private, part of viewing key)
```

**Nejsou potreba zmeny v zalohach.** Stavajici 24-slovni seed fraz pokryva
klic ML-KEM, protoze je odvozen ze spending key deterministicky. Obnoveni
penezenky funguje jako drive.

**Diverzifikovane adresy** vsechny sdileji stejny `ek_pq`, protoze ML-KEM nema
prirozeny mechanismus diverzifikace jako nasobeni skalarem Pallas. To znamena,
ze pozorovatel se dvema adresami uzivatele je muze propojit porovnanim `ek_pq`.

### Vykon trial decryption

| Krok | Soucasne | Hybridni | Delta |
|------|---------|----------|-------|
| Pallas ECDH | ~100 us | ~100 us | — |
| ML-KEM-768 Decaps | — | ~40 us | +40 us |
| BLAKE2b KDF | ~0,5 us | ~1 us | — |
| ChaCha20 (52 bytes) | ~0,1 us | ~0,1 us | — |
| **Celkem na note** | **~101 us** | **~141 us** | **+40% overhead** |

Skenovani 100 000 notes: ~10,1 s → ~14,1 s. Rezie je vyznamna, ale ne
neprijatelna. Dekapsulace ML-KEM bezi v konstantnim case bez vyhody davkoveho
zpracovani (na rozdil od operaci s eliptickymi krivkami), takze se skaluje
linearne.

### Dopad na ZK obvody

**Zadny.** Hybridni KEM je zcela ve vrstve transportu/sifrovani. Obvod Halo 2
prokazuje existenci note, spravnost nullifier a rovnovahu hodnot — nedokazuje
nic o sifrovani. Zadne zmeny proving keys, verifying keys ani omezeni obvodu.

### Srovnani s odvetvim

| System | Pristup | Status |
|--------|---------|--------|
| **Signal** (PQXDH) | X25519 + ML-KEM-768, povinne pro vsechny uzivatele | **Nasazeno** (2023) |
| **Chrome/Firefox TLS** | X25519 + ML-KEM-768 hybridni vymena klicu | **Nasazeno** (2024) |
| **X-Wing** (navrh IETF) | X25519 + ML-KEM-768, ucelovy kombinator | Navrh standardu |
| **Zcash** | Navrh ZIP kvantove obnovitelnosti (obnoveni fondu, ne sifrovani) | Pouze diskuze |
| **Dash Platform** | Pallas ECDH + ML-KEM-768 (navrhovano) | Faze navrhu |

## Kdy nasadit

### Otazka casoveho harmonogramu

- **Soucasny stav (2026):** Zadny kvantovy pocitac nemuze prolomit 255-bitove
  ECC. Nejvetsi demonstrovana kvantova faktorizace: ~50 bitu. Mezera: rady
  velikosti.
- **Kratkodobe (2030-2035):** Hardwarove plany od IBM, Google, Quantinuum
  ciluji na miliony qubitu. Implementace a sady parametru ML-KEM dozraji.
- **Strednedobne (2035-2050):** Vetsina odhadu umistuje prichod CRQC do tohoto
  okna. Data HNDL sbirana dnes jsou ohrozena.
- **Dlouhodobe (2050+):** Konsensus mezi kryptografy: velke kvantove pocitace
  jsou otazkou "kdy", ne "jestli".

### Doporucena strategie

**1. Navrhujte s ohledem na aktualizovatelnost jiz nyni.** Zajistete, aby format
ulozeneho zaznamu, struktura `TransmittedNoteCiphertext` a rozlozeni polozek
BulkAppendTree byly verzovane a rozsiritelne. To je malo nakladne a uchovava
moznost pridani hybridniho KEM pozdeji.

**2. Nasadte hybridni KEM, az bude pripraven, a ucinte ho povinnym.** Nenabizejte
dva pooly (klasicky a hybridni). Rozdeleni anonymni mnoziny porazi ucel
chranenych transakci — uzivatele schovavajici se v mensi skupine jsou mene
soukromi, ne vice. Po nasazeni kazdy note pouziva hybridni schema.

**3. Ciloujte na okno 2028-2030.** To je dobre pred jakoukoli realistickou
kvantovou hrozbou, ale po stabilizaci implementaci a velikosti parametru ML-KEM.
Umoznuje to take ucit se ze zkusenosti s nasazenim Zcash a Signal.

**4. Monitorujte spousteci udalosti:**
- NIST nebo NSA narizujici terminy postkvantove migrace
- Vyznamne pokroky v kvantovem hardwaru (>100 000 fyzickych qubitu s opravou
  chyb)
- Kryptoanalyticke pokroky proti problemum mrizek (ovlivnily by volbu ML-KEM)

### Co nevyzaduje nalehave akce

| Komponenta | Proc muze pockat |
|-----------|------------------|
| Podpisy autorizace utraty | Paddelani je v realnem case, ne retroaktivni. Aktualizace na ML-DSA/SLH-DSA pred prichodem CRQC. |
| System dukazu Halo 2 | Paddelani dukazu je v realnem case. Migrace na system zalozeny na STARK, az bude potreba. |
| Odolnost Sinsemilla proti kolizim | Uzitecna pouze pro nove utoky, ne retroaktivni. Zahrnuta v migraci systemu dukazu. |
| Infrastruktura GroveDB Merk/MMR/Blake3 | **Jiz kvantove bezpecna na zaklade soucasnych kryptografickych predpokladu.** Neni potreba zadnych akci na zaklade znamych utoku. |

## Reference postkvantovych alternativ

### Pro sifrovani (nahrada ECDH)

| Schema | Typ | Public key | Ciphertext | Kategorie NIST | Poznamky |
|--------|-----|-----------|-----------|----------------|---------|
| ML-KEM-768 | Lattice (MLWE) | 1 184 B | 1 088 B | 3 (192-bit) | FIPS 203, prumyslovy standard |
| ML-KEM-512 | Lattice (MLWE) | 800 B | 768 B | 1 (128-bit) | Mensi, nizsi marze |
| ML-KEM-1024 | Lattice (MLWE) | 1 568 B | 1 568 B | 5 (256-bit) | Zbytecne pro hybrid |

### Pro podpisy (nahrada RedPallas/Schnorr)

| Schema | Typ | Public key | Signature | Kategorie NIST | Poznamky |
|--------|-----|-----------|----------|----------------|---------|
| ML-DSA-65 (Dilithium) | Lattice | 1 952 B | 3 293 B | 3 | FIPS 204, rychly |
| SLH-DSA (SPHINCS+) | Zalozeny na hashich | 32-64 B | 7 856-49 856 B | 1-5 | FIPS 205, konzervativni |
| XMSS/LMS | Zalozeny na hashich (stateful) | 60 B | 2 500 B | ruzne | Stateful — znovupouziti = prolomeni |

### Pro ZK dukazy (nahrada Halo 2)

| System | Predpoklad | Velikost dukazu | Postkvantovy | Poznamky |
|--------|-----------|----------------|-------------|---------|
| STARKs | Hash functions (collision resistance) | ~100-400 KB | **Ano** | Pouzivano StarkNet |
| Plonky3 | FRI (hash-based polynomial commitment) | ~50-200 KB | **Ano** | Aktivni vyvoj |
| Halo 2 (soucasny) | ECDLP on Pasta curves | ~5 KB | **Ne** | Soucasny system Orchard |
| Lattice SNARKs | MLWE | Vyzkum | **Ano** | Neni pripraveno pro produkci |

### Ekosystem Rust crate

| Crate | Zdroj | FIPS 203 | Overeny | Poznamky |
|-------|-------|----------|---------|---------|
| `libcrux-ml-kem` | Cryspen | Ano | Formalne overeny (hax/F*) | Nejvyssi zaruka |
| `ml-kem` | RustCrypto | Ano | Constant-time, neauditovany | Kompatibilita s ekosystemem |
| `fips203` | integritychain | Ano | Constant-time | Pure Rust, no_std |

## Shrnutí

```text
┌─────────────────────────────────────────────────────────────────────┐
│  SHRNUTÍ KVANTOVÝCH HROZEB PRO GROVEDB + ORCHARD                    │
│                                                                     │
│  BEZPEČNÉ ZA SOUČASNÝCH PŘEDPOKLADŮ (založené na hašování):         │
│    ✓ Blake3 Merk stromy, MMR, BulkAppendTree                       │
│    ✓ BLAKE2b KDF, PRF^expand                                       │
│    ✓ Symetrické šifrování ChaCha20-Poly1305                        │
│    ✓ Všechny autentizační řetězce důkazů GroveDB                   │
│                                                                     │
│  OPRAVIT PŘED ULOŽENÍM DAT (retroaktivní HNDL):                    │
│    ✗ Šifrování poznámek (dohoda klíčů ECDH) → Hybridní KEM        │
│    ✗ Závazky hodnot (Pedersen) → odhalení částek                   │
│                                                                     │
│  OPRAVIT PŘED PŘÍCHODEM KVANTOVÝCH POČÍTAČŮ (pouze reálný čas):    │
│    ~ Autorizace útraty → ML-DSA / SLH-DSA                         │
│    ~ ZK důkazy → STARKs / Plonky3                                  │
│    ~ Sinsemilla → hašovací Merkle strom                             │
│                                                                     │
│  DOPORUČENÝ HARMONOGRAM:                                            │
│    2026-2028: Návrh pro rozšiřitelnost, verzování formátů          │
│    2028-2030: Nasazení povinného hybridního KEM pro šifrování      │
│    2035+: Migrace podpisů a systému důkazů dle potřeby             │
└─────────────────────────────────────────────────────────────────────┘
```

---
