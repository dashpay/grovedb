# Kryptografia kwantowa — Analiza zagrozen postkwantowych

Ten rozdzial analizuje, jak komputery kwantowe wplynelby na prymitywy
kryptograficzne uzywane w GroveDB i protokolach transakcji chronionych
zbudowanych na nim (Orchard, Dash Platform). Obejmuje on, ktore komponenty sa
podatne, ktore sa bezpieczne, co oznacza "harvest now, decrypt later" dla
przechowywanych danych i jakie strategie lagodzenia istnieja — w tym hybrydowe
projekty KEM.

## Dwa algorytmy kwantowe, ktore maja znaczenie

Tylko dwa algorytmy kwantowe sa istotne dla kryptografii w praktyce:

**Algorytm Shora** rozwiazuje problem logarytmu dyskretnego (i faktoryzacji
liczb calkowitych) w czasie wielomianowym. Dla 255-bitowej krzywej eliptycznej
jak Pallas wymaga to okolo 510 kubitow logicznych — ale z narzutem korekcji
bledow rzeczywiste wymaganie wynosi okolo 4 milionow kubitow fizycznych.
Algorytm Shora **calkowicie lamie** cala kryptografie krzywych eliptycznych
niezaleznie od rozmiaru klucza.

**Algorytm Grovera** zapewnia kwadratowe przyspieszenie przeszukiwania
brute-force. 256-bitowy klucz symetryczny efektywnie staje sie 128-bitowym.
Jednak glebokosc obwodu dla algorytmu Grovera na 128-bitowej przestrzeni kluczy
wynosi nadal 2^64 operacji kwantowych — wielu kryptografow uwaza, ze nigdy nie
bedzie to praktyczne na rzeczywistym sprzecie z powodu limitow dekoherencji.
Algorytm Grovera zmniejsza marginesy bezpieczenstwa, ale nie lamie dobrze
sparametryzowanej kryptografii symetrycznej.

| Algorytm | Cele | Przyspieszenie | Praktyczny wplyw |
|----------|------|----------------|------------------|
| **Shor** | ECC discrete log, RSA factoring | Wykladnicze (czas wielomianowy) | **Calkowite zlamanie** ECC |
| **Grover** | Przeszukiwanie kluczy symetrycznych, hash preimage | Kwadratowe (polowi bity klucza) | 256-bit → 128-bit (nadal bezpieczne) |

## Prymitywy kryptograficzne GroveDB

GroveDB i protokol chroniony oparty na Orchard uzywaja mieszanki prymitywow
krzywych eliptycznych i symetrycznych/opartych na hashach. Ponizsze tabele
klasyfikuja kazdy prymityw wedlug jego podatnosci kwantowej:

### Podatne na atak kwantowy (algorytm Shora — 0 bitow postkwantowych)

| Prymityw | Miejsce uzycia | Co zostaje zlamane |
|----------|---------------|---------------------|
| **Pallas ECDLP** | Note commitments (cmx), ephemeral keys (epk/esk), viewing keys (ivk), payment keys (pk_d), nullifier derivation | Odzyskanie dowolnego klucza prywatnego z jego publicznego odpowiednika |
| **ECDH key agreement** (Pallas) | Wyprowadzanie kluczy szyfrowania symetrycznego dla note ciphertexts | Odzyskanie shared secret → odszyfrowanie wszystkich notes |
| **Sinsemilla hash** | Sciezki Merkle CommitmentTree, in-circuit hashing | Odpornosc na kolizje zalezy od ECDLP; maleje gdy Pallas zostaje zlamany |
| **Halo 2 IPA** | System dowodow ZK (polynomial commitment over Pasta curves) | Falszowanie dowodow dla falszywych stwierdzn (falszerstwo, nieautoryzowane wydatki) |
| **Pedersen commitments** | Value commitments (cv_net) ukrywajace kwoty transakcji | Odzyskanie ukrytych kwot; falszowanie dowodow salda |

### Bezpieczne kwantowo (algorytm Grovera — 128+ bitow postkwantowych)

| Prymityw | Miejsce uzycia | Bezpieczenstwo postkwantowe |
|----------|---------------|------------------------------|
| **Blake3** | Hashe wezlow Merk tree, MMR nodes, BulkAppendTree state roots, subtree path prefixes | 128-bit preimage, 128-bit second-preimage |
| **BLAKE2b-256** | KDF do wyprowadzania kluczy symetrycznych, outgoing cipher key, PRF^expand | 128-bit preimage |
| **ChaCha20-Poly1305** | Szyfruje enc_ciphertext i out_ciphertext (klucze 256-bit) | 128-bit key search (bezpieczne, ale sciezka wyprowadzania klucza przez ECDH nie jest) |
| **PRF^expand** (BLAKE2b-512) | Wyprowadza esk, rcm, psi z rseed | 128-bit PRF security |

### Infrastruktura GroveDB: Calkowicie bezpieczna kwantowo

Wszystkie struktury danych GroveDB opieraja sie wylacznie na hashowaniu Blake3:

- **Merk AVL trees** — hashe wezlow, combined_value_hash, propagacja child hash
- **MMR trees** — hashe wezlow wewnetrznych, obliczanie szczytow, wyprowadzanie root
- **BulkAppendTree** — lancuchy hashowe bufora, dense Merkle roots, epoch MMR
- **CommitmentTree state root** — `blake3("ct_state" || sinsemilla_root || bulk_state_root)`
- **Subtree path prefixes** — hashowanie Blake3 segmentow sciezki
- **V1 proofs** — lancuchy uwierzytelniania przez hierarchie Merk

**Nie sa potrzebne zadne zmiany.** Dowody Merk tree GroveDB, sprawdzanie
spojnosci MMR, rooty epok BulkAppendTree i wszystkie lancuchy uwierzytelniania
dowodow V1 pozostaja bezpieczne wobec komputerow kwantowych. Infrastruktura
oparta na hashach jest najsilniejsza czescia systemu postkwantowego.

## Zagrozenia retroaktywne vs zagrozenia w czasie rzeczywistym

To rozroznienie jest kluczowe dla priorytetyzacji tego, co naprawic i kiedy.

**Zagrozenia retroaktywne** kompromituja dane, ktore sa juz przechowywane.
Przeciwnik rejestruje dane dzisiaj i odszyfrowuje je, gdy komputery kwantowe
beda dostepne. Te zagrozenia **nie moga byc zlagodzone po fakcie** — gdy dane
sa na lancuchu, nie mozna ich ponownie zaszyfrowac ani odwolac.

**Zagrozenia w czasie rzeczywistym** wplywaja tylko na transakcje tworzone w
przyszlosci. Przeciwnik z komputerem kwantowym moze falszowac podpisy lub
dowody, ale tylko dla nowych transakcji. Stare transakcje zostaly juz
zwalidowane i potwierdzone przez siec.

| Zagrozenie | Typ | Co zostaje ujawnione | Pilnosc |
|-----------|------|----------------------|---------|
| **Odszyfrowanie note** (enc_ciphertext) | **Retroaktywne** | Zawartosc note: odbiorca, kwota, memo, rseed | **Wysoka** — przechowywane na zawsze |
| **Otwarcie value commitment** (cv_net) | **Retroaktywne** | Kwoty transakcji (ale nie nadawca/odbiorca) | **Srednia** — tylko kwoty |
| **Dane odzyskiwania nadawcy** (out_ciphertext) | **Retroaktywne** | Klucze odzyskiwania nadawcy dla wyslanych notes | **Wysoka** — przechowywane na zawsze |
| Falszowanie autoryzacji wydatkow | W czasie rzeczywistym | Moze falszowac nowe podpisy wydatkow | Niska — aktualizacja przed QC |
| Falszowanie dowodow Halo 2 | W czasie rzeczywistym | Moze falszowac nowe dowody (falszerstwo) | Niska — aktualizacja przed QC |
| Kolizja Sinsemilla | W czasie rzeczywistym | Moze falszowac nowe sciezki Merkle | Niska — obejmowane przez falszowanie dowodow |
| Falszowanie podpisu binding | W czasie rzeczywistym | Moze falszowac nowe dowody salda | Niska — aktualizacja przed QC |

### Co dokladnie zostaje ujawnione?

**Jesli szyfrowanie note zostanie zlamane** (glowne zagrozenie HNDL):

Przeciwnik kwantowy odzyskuje `esk` z przechowywanego `epk` za pomoca algorytmu
Shora, oblicza shared secret ECDH, wyprowadza klucz symetryczny i odszyfrowuje
`enc_ciphertext`. To ujawnia pelny plaintext note:

| Pole | Rozmiar | Co ujawnia |
|------|---------|-----------|
| version | 1 byte | Wersja protokolu (niewrazliwa) |
| diversifier | 11 bytes | Skladnik adresu odbiorcy |
| value | 8 bytes | Dokladna kwota transakcji |
| rseed | 32 bytes | Umozliwia powiazanie nullifier (deanonimizacja grafu transakcji) |
| memo | 36 bytes (DashMemo) | Dane aplikacji, potencjalnie identyfikujace |

Majac `rseed` i `rho` (przechowywane obok szyfrogramu), przeciwnik moze
obliczyc `esk = PRF(rseed, rho)` i zweryfikowac powiazanie klucza efemerycznego.
W polaczeniu z diversifier laczy to wejscia z wyjsciami w calej historii
transakcji — **pelna deanonimizacja chronionej puli**.

**Jesli tylko value commitments zostana zlamane** (wtorne zagrozenie HNDL):

Przeciwnik odzyskuje `v` z `cv_net = [v]*V + [rcv]*R` poprzez rozwiazanie
ECDLP. To ujawnia **kwoty transakcji, ale nie tozsamosc nadawcy ani odbiorcy**.
Przeciwnik widzi "ktos wyslal 5.0 Dash do kogos", ale nie moze powiazac kwoty
z zadnym adresem ani tozsamoscia bez zlamania rowniez szyfrowania note.

Kwoty bez powiazania same w sobie maja ograniczona przydatnosc. Ale w polaczeniu
z danymi zewnetrznymi (czas, znane faktury, kwoty pasujace do publicznych
wniosków), ataki korelacyjne staja sie mozliwe.

## Atak "Harvest Now, Decrypt Later"

To jest najpilniejsze i najbardziej praktyczne zagrozenie kwantowe.

**Model ataku:** Przeciwnik na poziomie panstwowym (lub dowolna strona z
wystarczajaca iloscia pamieci) rejestruje wszystkie dane chronionych transakcji
on-chain dzisiaj. Dane te sa publicznie dostepne na blockchainie i niezmienne.
Przeciwnik czeka na kryptograficznie istotny komputer kwantowy (CRQC), a
nastepnie:

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

**Kluczowy wniosek:** Szyfrowanie symetryczne (ChaCha20-Poly1305) jest
doskonale bezpieczne kwantowo. Podatnosc lezy calkowicie w **sciezce
wyprowadzania klucza** — klucz symetryczny jest wyprowadzany z shared secret
ECDH, a ECDH jest lamany przez algorytm Shora. Atakujacy nie lamie szyfrowania;
odzyskuje klucz.

**Retroaktywnosc:** Ten atak jest **calkowicie retroaktywny**. Kazdy
zaszyfrowany note kiedykolwiek przechowywany on-chain moze byc odszyfrowany,
gdy CRQC bedzie istnialo. Danych nie mozna ponownie zaszyfrowac ani ochronic
po fakcie. Dlatego nalezy to rozwiazac zanim dane zostana zapisane, nie po.

## Lagodzenie: Hybrydowy KEM (ML-KEM + ECDH)

Obrona przed HNDL polega na wyprowadzaniu klucza szyfrowania symetrycznego z
**dwoch niezaleznych mechanizmow uzgadniania kluczy**, tak aby zlamanie tylko
jednego bylo niewystarczajace. Nazywa sie to hybrydowym KEM.

### ML-KEM-768 (CRYSTALS-Kyber)

ML-KEM to standaryzowany przez NIST (FIPS 203, sierpien 2024) postkwantowy
mechanizm enkapsulacji kluczy oparty na problemie Module Learning With Errors
(MLWE).

| Parametr | ML-KEM-512 | ML-KEM-768 | ML-KEM-1024 |
|----------|-----------|-----------|------------|
| Public key (ek) | 800 bytes | **1 184 bytes** | 1 568 bytes |
| Ciphertext (ct) | 768 bytes | **1 088 bytes** | 1 568 bytes |
| Shared secret | 32 bytes | 32 bytes | 32 bytes |
| Kategoria NIST | 1 (128-bit) | **3 (192-bit)** | 5 (256-bit) |

**ML-KEM-768** to rekomendowany wybor — jest to zestaw parametrow uzywany przez
X-Wing, PQXDH Signal i hybrydowa wymiane kluczy TLS Chrome/Firefox. Kategoria 3
zapewnia komfortowy margines przed przyszlymi postepami kryptoanalizy kratek.

### Jak dziala schemat hybrydowy

**Obecny przeplyw (podatny):**

```text
Sender:
  esk = PRF(rseed, rho)                    // deterministic from note
  epk = [esk] * g_d                         // Pallas curve point
  shared_secret = [esk] * pk_d              // ECDH (broken by Shor's)
  K_enc = BLAKE2b("Zcash_OrchardKDF", shared_secret || epk)
  enc_ciphertext = ChaCha20(K_enc, note_plaintext)
```

**Przeplyw hybrydowy (odporny kwantowo):**

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

**Odszyfrowanie odbiorcy:**

```text
Recipient:
  ss_ecdh = [ivk] * epk                    // same ECDH (using incoming viewing key)
  ss_pq = ML-KEM-768.Decaps(dk_pq, ct_pq)  // NEW: KEM decapsulation
  K_enc = BLAKE2b("DashPlatform_HybridKDF", ss_ecdh || ss_pq || ct_pq || epk)
  note_plaintext = ChaCha20.Decrypt(K_enc, enc_ciphertext)
```

### Gwarancja bezpieczenstwa

Polaczony KEM jest bezpieczny IND-CCA2, jesli **ktoryskolwiek** z komponentow
KEM jest bezpieczny. Jest to formalnie udowodnione przez
[Giacon, Heuer i Poettering (2018)](https://eprint.iacr.org/2018/024)
dla kombinatorow KEM uzywajacych PRF (BLAKE2b sie kwalifikuje) i niezaleznie
przez [dowod bezpieczenstwa X-Wing](https://eprint.iacr.org/2024/039).

| Scenariusz | ECDH | ML-KEM | Polaczony klucz | Status |
|-----------|------|--------|-----------------|--------|
| Swiat klasyczny | Bezpieczny | Bezpieczny | **Bezpieczny** | Oba nienaruszone |
| Kwant lamie ECC | **Zlamany** | Bezpieczny | **Bezpieczny** | ML-KEM chroni |
| Postepy kratek lamia ML-KEM | Bezpieczny | **Zlamany** | **Bezpieczny** | ECDH chroni (tak jak dzisiaj) |
| Oba zlamane | Zlamany | Zlamany | **Zlamany** | Wymaga dwoch jednoczesnych przelomow |

### Wplyw na rozmiar

Hybrydowy KEM dodaje szyfrogram ML-KEM-768 (1 088 bajtow) do kazdego
przechowywanego note i rozszerza outgoing ciphertext, aby uwzglednic shared
secret ML-KEM dla odzyskiwania nadawcy:

**Przechowywany rekord na note:**

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

**Przechowywanie w skali:**

| Notes | Obecne (280 B) | Hybrydowe (1 400 B) | Delta |
|-------|---------------|---------------------|-------|
| 100 000 | 26,7 MB | 133 MB | +106 MB |
| 1 000 000 | 267 MB | 1,33 GB | +1,07 GB |
| 10 000 000 | 2,67 GB | 13,3 GB | +10,7 GB |

**Rozmiar adresu:**

```text
Current:  diversifier (11) + pk_d (32) = 43 bytes
Hybrid:   diversifier (11) + pk_d (32) + ek_pq (1,184) = 1,227 bytes
```

1 184-bajtowy klucz publiczny ML-KEM musi byc zawarty w adresie, aby nadawcy
mogli wykonac enkapsulacje. Przy ~1 960 znakach Bech32m jest to duzy rozmiar,
ale nadal miesci sie w kodzie QR (maks. ~2 953 znakow alfanumerycznych).

### Zarzadzanie kluczami

Para kluczy ML-KEM jest wyprowadzana deterministycznie z spending key:

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

**Nie sa potrzebne zmiany w kopiach zapasowych.** Istniejace 24-wyrazowe frazy
seed obejmuja klucz ML-KEM, poniewaz jest on wyprowadzany ze spending key
deterministycznie. Odzyskiwanie portfela dziala jak dotychczas.

**Zdywersyfikowane adresy** wspoldziela ten sam `ek_pq`, poniewaz ML-KEM nie
posiada naturalnego mechanizmu dywersyfikacji jak mnozenie skalarne Pallas.
Oznacza to, ze obserwator z dwoma adresami uzytkownika moze je powiazac
porownujac `ek_pq`.

### Wydajnosc trial decryption

| Krok | Obecne | Hybrydowe | Delta |
|------|--------|-----------|-------|
| Pallas ECDH | ~100 us | ~100 us | — |
| ML-KEM-768 Decaps | — | ~40 us | +40 us |
| BLAKE2b KDF | ~0,5 us | ~1 us | — |
| ChaCha20 (52 bytes) | ~0,1 us | ~0,1 us | — |
| **Lacznie na note** | **~101 us** | **~141 us** | **+40% overhead** |

Skanowanie 100 000 notes: ~10,1 s → ~14,1 s. Narzut jest znaczacy, ale nie
przeszkadza. Dekapsulacja ML-KEM dziala w stalym czasie bez korzysci z
przetwarzania wsadowego (w przeciwienstwie do operacji na krzywych eliptycznych),
wiec skaluje sie liniowo.

### Wplyw na obwody ZK

**Brak.** Hybrydowy KEM jest calkowicie w warstwie transportu/szyfrowania.
Obwod Halo 2 dowodzi istnienia note, poprawnosci nullifier i rownowagi
wartosci — nie dowodzi niczego na temat szyfrowania. Brak zmian w proving keys,
verifying keys ani ograniczeniach obwodu.

### Porownanie z branza

| System | Podejscie | Status |
|--------|-----------|--------|
| **Signal** (PQXDH) | X25519 + ML-KEM-768, obowiazkowe dla wszystkich uzytkownikow | **Wdrozony** (2023) |
| **Chrome/Firefox TLS** | X25519 + ML-KEM-768 hybrydowa wymiana kluczy | **Wdrozony** (2024) |
| **X-Wing** (projekt IETF) | X25519 + ML-KEM-768, dedykowany kombinator | Projekt standardu |
| **Zcash** | Projekt ZIP odzyskiwania kwantowego (odzyskiwanie srodkow, nie szyfrowanie) | Tylko dyskusja |
| **Dash Platform** | Pallas ECDH + ML-KEM-768 (proponowany) | Faza projektowa |

## Kiedy wdrozyc

### Pytanie o osi czasu

- **Stan obecny (2026):** Zaden komputer kwantowy nie moze zlamac 255-bitowego
  ECC. Najwieksza zademonstrowana faktoryzacja kwantowa: ~50 bitow. Roznica:
  rzedy wielkosci.
- **Krotkoterminowo (2030-2035):** Mapy drogowe sprzetu od IBM, Google,
  Quantinuum celuja w miliony kubitow. Implementacje i zestawy parametrow ML-KEM
  dojrzeja.
- **Srednioterminowo (2035-2050):** Wiekszosc szacunkow umieszcza pojawienie sie
  CRQC w tym oknie. Dane HNDL zbierane dzisiaj sa zagrozone.
- **Dlugoterminowo (2050+):** Konsensus wsrod kryptografow: komputery kwantowe
  na duza skale to kwestia "kiedy", a nie "czy".

### Rekomendowana strategia

**1. Projektuj z mysla o aktualizowalnosci juz teraz.** Upewnij sie, ze format
przechowywanego rekordu, struktura `TransmittedNoteCiphertext` i uklad wpisow
BulkAppendTree sa wersjonowane i rozszerzalne. To ma niski koszt i zachowuje
opcje dodania hybrydowego KEM pozniej.

**2. Wdroz hybrydowy KEM gdy bedzie gotowy, uczyni go obowiazkowym.** Nie
oferuj dwoch puli (klasycznej i hybrydowej). Podzielenie zbioru anonimowosci
niweczysz cel transakcji chronionych — uzytkownicy ukrywajacy sie w mniejszej
grupie sa mniej prywatni, nie bardziej. Po wdrozeniu kazdy note uzywa schematu
hybrydowego.

**3. Celuj w okno 2028-2030.** Jest to na dlugo przed jakimkolwiek realistycznym
zagrozeniem kwantowym, ale po ustabilizowaniu implementacji i rozmiarow
parametrow ML-KEM. Pozwala to rowniez uczyc sie z doswiadczen wdrozeniowych
Zcash i Signal.

**4. Monitoruj zdarzenia wyzwalajace:**
- NIST lub NSA narzucajace terminy migracji postkwantowej
- Znaczace postepy w sprzecie kwantowym (>100 000 kubitow fizycznych z korekcja
  bledow)
- Postepy kryptoanalityczne wobec problemow kratek (wplynelyby na wybor ML-KEM)

### Co nie wymaga pilnych dzialan

| Komponent | Dlaczego moze poczekac |
|-----------|------------------------|
| Podpisy autoryzacji wydatkow | Falszowanie jest w czasie rzeczywistym, nie retroaktywne. Aktualizacja do ML-DSA/SLH-DSA przed przyjsciem CRQC. |
| System dowodow Halo 2 | Falszowanie dowodow jest w czasie rzeczywistym. Migracja do systemu opartego na STARK gdy bedzie potrzebna. |
| Odpornosc na kolizje Sinsemilla | Przydatna tylko dla nowych atakow, nie retroaktywna. Obejmowana przez migracje systemu dowodow. |
| Infrastruktura GroveDB Merk/MMR/Blake3 | **Juz bezpieczna kwantowo.** Nie sa potrzebne zadne dzialania, teraz ani w przyszlosci. |

## Referencja alternatyw postkwantowych

### Do szyfrowania (zastepujac ECDH)

| Schemat | Typ | Public key | Ciphertext | Kategoria NIST | Uwagi |
|---------|-----|-----------|-----------|----------------|-------|
| ML-KEM-768 | Lattice (MLWE) | 1 184 B | 1 088 B | 3 (192-bit) | FIPS 203, standard branzowy |
| ML-KEM-512 | Lattice (MLWE) | 800 B | 768 B | 1 (128-bit) | Mniejszy, nizszy margines |
| ML-KEM-1024 | Lattice (MLWE) | 1 568 B | 1 568 B | 5 (256-bit) | Przesada dla hybrydy |

### Do podpisow (zastepujac RedPallas/Schnorr)

| Schemat | Typ | Public key | Signature | Kategoria NIST | Uwagi |
|---------|-----|-----------|----------|----------------|-------|
| ML-DSA-65 (Dilithium) | Lattice | 1 952 B | 3 293 B | 3 | FIPS 204, szybki |
| SLH-DSA (SPHINCS+) | Oparty na hashach | 32-64 B | 7 856-49 856 B | 1-5 | FIPS 205, konserwatywny |
| XMSS/LMS | Oparty na hashach (stateful) | 60 B | 2 500 B | rozne | Stateful — ponowne uzycie = zlamanie |

### Do dowodow ZK (zastepujac Halo 2)

| System | Zalozenie | Rozmiar dowodu | Postkwantowy | Uwagi |
|--------|----------|---------------|-------------|-------|
| STARKs | Hash functions (collision resistance) | ~100-400 KB | **Tak** | Uzywany przez StarkNet |
| Plonky3 | FRI (hash-based polynomial commitment) | ~50-200 KB | **Tak** | Aktywny rozwoj |
| Halo 2 (obecny) | ECDLP on Pasta curves | ~5 KB | **Nie** | Obecny system Orchard |
| Lattice SNARKs | MLWE | Badania | **Tak** | Nie gotowy produkcyjnie |

### Ekosystem crate'ow Rust

| Crate | Zrodlo | FIPS 203 | Zweryfikowany | Uwagi |
|-------|--------|----------|---------------|-------|
| `libcrux-ml-kem` | Cryspen | Tak | Formalnie zweryfikowany (hax/F*) | Najwyzsze gwarancje |
| `ml-kem` | RustCrypto | Tak | Constant-time, nie audytowany | Zgodnosc z ekosystemem |
| `fips203` | integritychain | Tak | Constant-time | Pure Rust, no_std |

## Podsumowanie

```text
┌─────────────────────────────────────────────────────────────────────┐
│  QUANTUM THREAT SUMMARY FOR GROVEDB + ORCHARD                      │
│                                                                     │
│  SAFE NOW AND FOREVER (hash-based):                                 │
│    ✓ Blake3 Merk trees, MMR, BulkAppendTree                        │
│    ✓ BLAKE2b KDF, PRF^expand                                       │
│    ✓ ChaCha20-Poly1305 symmetric encryption                        │
│    ✓ All GroveDB proof authentication chains                        │
│                                                                     │
│  FIX BEFORE DATA IS STORED (retroactive HNDL):                     │
│    ✗ Note encryption (ECDH key agreement) → Hybrid KEM             │
│    ✗ Value commitments (Pedersen) → amounts revealed                │
│                                                                     │
│  FIX BEFORE QUANTUM COMPUTERS ARRIVE (real-time only):              │
│    ~ Spend authorization → ML-DSA / SLH-DSA                        │
│    ~ ZK proofs → STARKs / Plonky3                                  │
│    ~ Sinsemilla → hash-based Merkle tree                            │
│                                                                     │
│  RECOMMENDED TIMELINE:                                              │
│    2026-2028: Design for upgradability, version stored formats      │
│    2028-2030: Deploy mandatory hybrid KEM for note encryption       │
│    2035+: Migrate signatures and proof system if needed             │
└─────────────────────────────────────────────────────────────────────┘
```

---
