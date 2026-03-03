# Kryptografia kwantowa — Analiza zagrożeń postkwantowych

Ten rozdział analizuje, jak komputery kwantowe wpłynęłyby na prymitywy
kryptograficzne używane w GroveDB i protokołach transakcji chronionych
zbudowanych na nim (Orchard, Dash Platform). Obejmuje on, które komponenty są
podatne, które są bezpieczne, co oznacza "harvest now, decrypt later" dla
przechowywanych danych i jakie strategie łagodzenia istnieją — w tym hybrydowe
projekty KEM.

## Dwa algorytmy kwantowe, które mają znaczenie

Tylko dwa algorytmy kwantowe są istotne dla kryptografii w praktyce:

**Algorytm Shora** rozwiązuje problem logarytmu dyskretnego (i faktoryzacji
liczb całkowitych) w czasie wielomianowym. Dla 255-bitowej krzywej eliptycznej
jak Pallas wymaga to około 510 kubitów logicznych — ale z narzutem korekcji
błędów rzeczywiste wymaganie wynosi około 4 milionów kubitów fizycznych.
Algorytm Shora **całkowicie łamie** całą kryptografię krzywych eliptycznych
niezależnie od rozmiaru klucza.

**Algorytm Grovera** zapewnia kwadratowe przyspieszenie przeszukiwania
brute-force. 256-bitowy klucz symetryczny efektywnie staje się 128-bitowym.
Jednak głębokość obwodu dla algorytmu Grovera na 128-bitowej przestrzeni kluczy
wynosi nadal 2^64 operacji kwantowych — wielu kryptografów uważa, że nigdy nie
będzie to praktyczne na rzeczywistym sprzęcie z powodu limitów dekoherencji.
Algorytm Grovera zmniejsza marginesy bezpieczeństwa, ale nie łamie dobrze
sparametryzowanej kryptografii symetrycznej.

| Algorytm | Cele | Przyspieszenie | Praktyczny wpływ |
|----------|------|----------------|------------------|
| **Shor** | ECC discrete log, RSA factoring | Czas wielomianowy (wykładnicze przyspieszenie nad klasycznym) | **Całkowite złamanie** ECC |
| **Grover** | Przeszukiwanie kluczy symetrycznych, hash preimage | Kwadratowe (połowi bity klucza) | 256-bit → 128-bit (nadal bezpieczne) |

## Prymitywy kryptograficzne GroveDB

GroveDB i protokół chroniony oparty na Orchard używają mieszanki prymitywów
krzywych eliptycznych i symetrycznych/opartych na hashach. Poniższe tabele
klasyfikują każdy prymityw według jego podatności kwantowej:

### Podatne na atak kwantowy (algorytm Shora — 0 bitów postkwantowych)

| Prymityw | Miejsce użycia | Co zostaje złamane |
|----------|---------------|---------------------|
| **Pallas ECDLP** | Note commitments (cmx), ephemeral keys (epk/esk), viewing keys (ivk), payment keys (pk_d), nullifier derivation | Odzyskanie dowolnego klucza prywatnego z jego publicznego odpowiednika |
| **ECDH key agreement** (Pallas) | Wyprowadzanie kluczy szyfrowania symetrycznego dla note ciphertexts | Odzyskanie shared secret → odszyfrowanie wszystkich notes |
| **Sinsemilla hash** | Ścieżki Merkle CommitmentTree, in-circuit hashing | Odporność na kolizje zależy od ECDLP; maleje, gdy Pallas zostaje złamany |
| **Halo 2 IPA** | System dowodów ZK (polynomial commitment over Pasta curves) | Fałszowanie dowodów dla fałszywych stwierdzeń (fałszerstwo, nieautoryzowane wydatki) |
| **Pedersen commitments** | Value commitments (cv_net) ukrywające kwoty transakcji | Odzyskanie ukrytych kwot; fałszowanie dowodów salda |

### Bezpieczne kwantowo (algorytm Grovera — 128+ bitów postkwantowych)

| Prymityw | Miejsce użycia | Bezpieczeństwo postkwantowe |
|----------|---------------|------------------------------|
| **Blake3** | Hashe węzłów Merk tree, MMR nodes, BulkAppendTree state roots, subtree path prefixes | 128-bit preimage, 128-bit second-preimage |
| **BLAKE2b-256** | KDF do wyprowadzania kluczy symetrycznych, outgoing cipher key, PRF^expand | 128-bit preimage |
| **ChaCha20-Poly1305** | Szyfruje enc_ciphertext i out_ciphertext (klucze 256-bit) | 128-bit key search (bezpieczne, ale ścieżka wyprowadzania klucza przez ECDH nie jest) |
| **PRF^expand** (BLAKE2b-512) | Wyprowadza esk, rcm, psi z rseed | 128-bit PRF security |

### Infrastruktura GroveDB: bezpieczna kwantowo z założenia

Wszystkie struktury danych GroveDB opierają się wyłącznie na hashowaniu Blake3:

- **Merk AVL trees** — hashe węzłów, combined_value_hash, propagacja child hash
- **MMR trees** — hashe węzłów wewnętrznych, obliczanie szczytów, wyprowadzanie root
- **BulkAppendTree** — łańcuchy hashowe bufora, dense Merkle roots, epoch MMR
- **CommitmentTree state root** — `blake3("ct_state" || sinsemilla_root || bulk_state_root)`
- **Subtree path prefixes** — hashowanie Blake3 segmentów ścieżki
- **V1 proofs** — łańcuchy uwierzytelniania przez hierarchię Merk

**Obecnie nie są potrzebne żadne zmiany.** Dowody Merk tree GroveDB, sprawdzanie
spójności MMR, rooty epok BulkAppendTree i wszystkie łańcuchy uwierzytelniania
dowodów V1 pozostają bezpieczne wobec komputerów kwantowych. Infrastruktura
oparta na hashach jest najsilniejszą częścią systemu postkwantowego.

## Zagrożenia retroaktywne vs zagrożenia w czasie rzeczywistym

To rozróżnienie jest kluczowe dla priorytetyzacji tego, co naprawić i kiedy.

**Zagrożenia retroaktywne** kompromitują dane, które są już przechowywane.
Przeciwnik rejestruje dane dzisiaj i odszyfrowuje je, gdy komputery kwantowe
będą dostępne. Te zagrożenia **nie mogą być złagodzone po fakcie** — gdy dane
są na łańcuchu, nie można ich ponownie zaszyfrować ani odwołać.

**Zagrożenia w czasie rzeczywistym** wpływają tylko na transakcje tworzone w
przyszłości. Przeciwnik z komputerem kwantowym może fałszować podpisy lub
dowody, ale tylko dla nowych transakcji. Stare transakcje zostały już
zwalidowane i potwierdzone przez sieć.

| Zagrożenie | Typ | Co zostaje ujawnione | Pilność |
|-----------|------|----------------------|---------|
| **Odszyfrowanie note** (enc_ciphertext) | **Retroaktywne** | Zawartość note: odbiorca, kwota, memo, rseed | **Wysoka** — przechowywane na zawsze |
| **Otwarcie value commitment** (cv_net) | **Retroaktywne** | Kwoty transakcji (ale nie nadawca/odbiorca) | **Średnia** — tylko kwoty |
| **Dane odzyskiwania nadawcy** (out_ciphertext) | **Retroaktywne** | Klucze odzyskiwania nadawcy dla wysłanych notes | **Wysoka** — przechowywane na zawsze |
| Fałszowanie autoryzacji wydatków | W czasie rzeczywistym | Może fałszować nowe podpisy wydatków | Niska — aktualizacja przed QC |
| Fałszowanie dowodów Halo 2 | W czasie rzeczywistym | Może fałszować nowe dowody (fałszerstwo) | Niska — aktualizacja przed QC |
| Kolizja Sinsemilla | W czasie rzeczywistym | Może fałszować nowe ścieżki Merkle | Niska — obejmowane przez fałszowanie dowodów |
| Fałszowanie podpisu binding | W czasie rzeczywistym | Może fałszować nowe dowody salda | Niska — aktualizacja przed QC |

### Co dokładnie zostaje ujawnione?

**Jeśli szyfrowanie note zostanie złamane** (główne zagrożenie HNDL):

Przeciwnik kwantowy odzyskuje `esk` z przechowywanego `epk` za pomocą algorytmu
Shora, oblicza shared secret ECDH, wyprowadza klucz symetryczny i odszyfrowuje
`enc_ciphertext`. To ujawnia pełny plaintext note:

| Pole | Rozmiar | Co ujawnia |
|------|---------|-----------|
| version | 1 byte | Wersja protokołu (niewrażliwa) |
| diversifier | 11 bytes | Składnik adresu odbiorcy |
| value | 8 bytes | Dokładna kwota transakcji |
| rseed | 32 bytes | Umożliwia powiązanie nullifier (deanonimizacja grafu transakcji) |
| memo | 36 bytes (DashMemo) | Dane aplikacji, potencjalnie identyfikujące |

Mając `rseed` i `rho` (przechowywane obok szyfrogramu), przeciwnik może
obliczyć `esk = PRF(rseed, rho)` i zweryfikować powiązanie klucza efemerycznego.
W połączeniu z diversifier łączy to wejścia z wyjściami w całej historii
transakcji — **pełna deanonimizacja chronionej puli**.

**Jeśli tylko value commitments zostaną złamane** (wtórne zagrożenie HNDL):

Przeciwnik odzyskuje `v` z `cv_net = [v]*V + [rcv]*R` poprzez rozwiązanie
ECDLP. To ujawnia **kwoty transakcji, ale nie tożsamość nadawcy ani odbiorcy**.
Przeciwnik widzi "ktoś wysłał 5.0 Dash do kogoś", ale nie może powiązać kwoty
z żadnym adresem ani tożsamością bez złamania również szyfrowania note.

Kwoty bez powiązania same w sobie mają ograniczoną przydatność. Ale w połączeniu
z danymi zewnętrznymi (czas, znane faktury, kwoty pasujące do publicznych
wniosków), ataki korelacyjne stają się możliwe.

## Atak "Harvest Now, Decrypt Later"

To jest najpilniejsze i najbardziej praktyczne zagrożenie kwantowe.

**Model ataku:** Przeciwnik na poziomie państwowym (lub dowolna strona z
wystarczającą ilością pamięci) rejestruje wszystkie dane chronionych transakcji
on-chain dzisiaj. Dane te są publicznie dostępne na blockchainie i niezmienne.
Przeciwnik czeka na kryptograficznie istotny komputer kwantowy (CRQC), a
następnie:

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
doskonale bezpieczne kwantowo. Podatność leży całkowicie w **ścieżce
wyprowadzania klucza** — klucz symetryczny jest wyprowadzany z shared secret
ECDH, a ECDH jest łamany przez algorytm Shora. Atakujący nie łamie szyfrowania;
odzyskuje klucz.

**Retroaktywność:** Ten atak jest **całkowicie retroaktywny**. Każdy
zaszyfrowany note kiedykolwiek przechowywany on-chain może być odszyfrowany,
gdy CRQC będzie istniało. Danych nie można ponownie zaszyfrować ani ochronić
po fakcie. Dlatego należy to rozwiązać, zanim dane zostaną zapisane, nie po.

## Łagodzenie: Hybrydowy KEM (ML-KEM + ECDH)

Obrona przed HNDL polega na wyprowadzaniu klucza szyfrowania symetrycznego z
**dwóch niezależnych mechanizmów uzgadniania kluczy**, tak aby złamanie tylko
jednego było niewystarczające. Nazywa się to hybrydowym KEM.

### ML-KEM-768 (CRYSTALS-Kyber)

ML-KEM to standaryzowany przez NIST (FIPS 203, sierpień 2024) postkwantowy
mechanizm enkapsulacji kluczy oparty na problemie Module Learning With Errors
(MLWE).

| Parametr | ML-KEM-512 | ML-KEM-768 | ML-KEM-1024 |
|----------|-----------|-----------|------------|
| Public key (ek) | 800 bytes | **1 184 bytes** | 1 568 bytes |
| Ciphertext (ct) | 768 bytes | **1 088 bytes** | 1 568 bytes |
| Shared secret | 32 bytes | 32 bytes | 32 bytes |
| Kategoria NIST | 1 (128-bit) | **3 (192-bit)** | 5 (256-bit) |

**ML-KEM-768** to rekomendowany wybór — jest to zestaw parametrów używany przez
X-Wing, PQXDH Signal i hybrydową wymianę kluczy TLS Chrome/Firefox. Kategoria 3
zapewnia komfortowy margines przed przyszłymi postępami kryptoanalizy kratek.

### Jak działa schemat hybrydowy

**Obecny przepływ (podatny):**

```text
Sender:
  esk = PRF(rseed, rho)                    // deterministic from note
  epk = [esk] * g_d                         // Pallas curve point
  shared_secret = [esk] * pk_d              // ECDH (broken by Shor's)
  K_enc = BLAKE2b("Zcash_OrchardKDF", shared_secret || epk)
  enc_ciphertext = ChaCha20(K_enc, note_plaintext)
```

**Przepływ hybrydowy (odporny kwantowo):**

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

### Gwarancja bezpieczeństwa

Połączony KEM jest bezpieczny IND-CCA2, jeśli **którykolwiek** z komponentów
KEM jest bezpieczny. Jest to formalnie udowodnione przez
[Giacon, Heuer i Poettering (2018)](https://eprint.iacr.org/2018/024)
dla kombinatorów KEM używających PRF (BLAKE2b się kwalifikuje) i niezależnie
przez [dowód bezpieczeństwa X-Wing](https://eprint.iacr.org/2024/039).

| Scenariusz | ECDH | ML-KEM | Połączony klucz | Status |
|-----------|------|--------|-----------------|--------|
| Świat klasyczny | Bezpieczny | Bezpieczny | **Bezpieczny** | Oba nienaruszone |
| Kwant łamie ECC | **Złamany** | Bezpieczny | **Bezpieczny** | ML-KEM chroni |
| Postępy kratek łamią ML-KEM | Bezpieczny | **Złamany** | **Bezpieczny** | ECDH chroni (tak jak dziś) |
| Oba złamane | Złamany | Złamany | **Złamany** | Wymaga dwóch jednoczesnych przełomów |

### Wpływ na rozmiar

Hybrydowy KEM dodaje szyfrogram ML-KEM-768 (1 088 bajtów) do każdego
przechowywanego note i rozszerza outgoing ciphertext, aby uwzględnić shared
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

1 184-bajtowy klucz publiczny ML-KEM musi być zawarty w adresie, aby nadawcy
mogli wykonać enkapsulację. Przy ~1 960 znakach Bech32m jest to duży rozmiar,
ale nadal mieści się w kodzie QR (maks. ~2 953 znaków alfanumerycznych).

### Zarządzanie kluczami

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

**Nie są potrzebne zmiany w kopiach zapasowych.** Istniejące 24-wyrazowe frazy
seed obejmują klucz ML-KEM, ponieważ jest on wyprowadzany ze spending key
deterministycznie. Odzyskiwanie portfela działa jak dotychczas.

**Zdywersyfikowane adresy** współdzielą ten sam `ek_pq`, ponieważ ML-KEM nie
posiada naturalnego mechanizmu dywersyfikacji jak mnożenie skalarne Pallas.
Oznacza to, że obserwator z dwoma adresami użytkownika może je powiązać,
porównując `ek_pq`.

### Wydajność trial decryption

| Krok | Obecne | Hybrydowe | Delta |
|------|--------|-----------|-------|
| Pallas ECDH | ~100 us | ~100 us | — |
| ML-KEM-768 Decaps | — | ~40 us | +40 us |
| BLAKE2b KDF | ~0,5 us | ~1 us | — |
| ChaCha20 (52 bytes) | ~0,1 us | ~0,1 us | — |
| **Łącznie na note** | **~101 us** | **~141 us** | **+40% overhead** |

Skanowanie 100 000 notes: ~10,1 s → ~14,1 s. Narzut jest znaczący, ale nie
przeszkadza. Dekapsulacja ML-KEM działa w stałym czasie bez korzyści z
przetwarzania wsadowego (w przeciwieństwie do operacji na krzywych eliptycznych),
więc skaluje się liniowo.

### Wpływ na obwody ZK

**Brak.** Hybrydowy KEM jest całkowicie w warstwie transportu/szyfrowania.
Obwód Halo 2 dowodzi istnienia note, poprawności nullifier i równowagi
wartości — nie dowodzi niczego na temat szyfrowania. Brak zmian w proving keys,
verifying keys ani ograniczeniach obwodu.

### Porównanie z branżą

| System | Podejście | Status |
|--------|-----------|--------|
| **Signal** (PQXDH) | X25519 + ML-KEM-768, obowiązkowe dla wszystkich użytkowników | **Wdrożony** (2023) |
| **Chrome/Firefox TLS** | X25519 + ML-KEM-768 hybrydowa wymiana kluczy | **Wdrożony** (2024) |
| **X-Wing** (projekt IETF) | X25519 + ML-KEM-768, dedykowany kombinator | Projekt standardu |
| **Zcash** | Projekt ZIP odzyskiwania kwantowego (odzyskiwanie środków, nie szyfrowanie) | Tylko dyskusja |
| **Dash Platform** | Pallas ECDH + ML-KEM-768 (proponowany) | Faza projektowa |

## Kiedy wdrożyć

### Pytanie o oś czasu

- **Stan obecny (2026):** Żaden komputer kwantowy nie może złamać 255-bitowego
  ECC. Największa zademonstrowana faktoryzacja kwantowa: ~50 bitów. Różnica:
  rzędy wielkości.
- **Krótkoterminowo (2030-2035):** Mapy drogowe sprzętu od IBM, Google,
  Quantinuum celują w miliony kubitów. Implementacje i zestawy parametrów ML-KEM
  dojrzeją.
- **Średnioterminowo (2035-2050):** Większość szacunków umieszcza pojawienie się
  CRQC w tym oknie. Dane HNDL zbierane dzisiaj są zagrożone.
- **Długoterminowo (2050+):** Konsensus wśród kryptografów: komputery kwantowe
  na dużą skalę to kwestia "kiedy", a nie "czy".

### Rekomendowana strategia

**1. Projektuj z myślą o aktualizowalności już teraz.** Upewnij się, że format
przechowywanego rekordu, struktura `TransmittedNoteCiphertext` i układ wpisów
BulkAppendTree są wersjonowane i rozszerzalne. To ma niski koszt i zachowuje
opcję dodania hybrydowego KEM później.

**2. Wdróż hybrydowy KEM, gdy będzie gotowy; uczyń go obowiązkowym.** Nie
oferuj dwóch pul (klasycznej i hybrydowej). Podzielenie zbioru anonimowości
niweczy cel transakcji chronionych — użytkownicy ukrywający się w mniejszej
grupie są mniej prywatni, nie bardziej. Po wdrożeniu każdy note używa schematu
hybrydowego.

**3. Celuj w okno 2028-2030.** Jest to na długo przed jakimkolwiek realistycznym
zagrożeniem kwantowym, ale po ustabilizowaniu implementacji i rozmiarów
parametrów ML-KEM. Pozwala to również uczyć się z doświadczeń wdrożeniowych
Zcash i Signal.

**4. Monitoruj zdarzenia wyzwalające:**
- NIST lub NSA narzucające terminy migracji postkwantowej
- Znaczące postępy w sprzęcie kwantowym (>100 000 kubitów fizycznych z korekcją
  błędów)
- Postępy kryptoanalityczne wobec problemów kratek (wpłynęłyby na wybór ML-KEM)

### Co nie wymaga pilnych działań

| Komponent | Dlaczego może poczekać |
|-----------|------------------------|
| Podpisy autoryzacji wydatków | Fałszowanie jest w czasie rzeczywistym, nie retroaktywne. Aktualizacja do ML-DSA/SLH-DSA przed przyjściem CRQC. |
| System dowodów Halo 2 | Fałszowanie dowodów jest w czasie rzeczywistym. Migracja do systemu opartego na STARK, gdy będzie potrzebna. |
| Odporność na kolizje Sinsemilla | Przydatna tylko dla nowych ataków, nie retroaktywna. Obejmowana przez migrację systemu dowodów. |
| Infrastruktura GroveDB Merk/MMR/Blake3 | **Już bezpieczna kwantowo przy obecnych założeniach kryptograficznych.** Nie są potrzebne żadne działania na podstawie znanych ataków. |

## Referencja alternatyw postkwantowych

### Do szyfrowania (zastępując ECDH)

| Schemat | Typ | Public key | Ciphertext | Kategoria NIST | Uwagi |
|---------|-----|-----------|-----------|----------------|-------|
| ML-KEM-768 | Lattice (MLWE) | 1 184 B | 1 088 B | 3 (192-bit) | FIPS 203, standard branżowy |
| ML-KEM-512 | Lattice (MLWE) | 800 B | 768 B | 1 (128-bit) | Mniejszy, niższy margines |
| ML-KEM-1024 | Lattice (MLWE) | 1 568 B | 1 568 B | 5 (256-bit) | Przesada dla hybrydy |

### Do podpisów (zastępując RedPallas/Schnorr)

| Schemat | Typ | Public key | Signature | Kategoria NIST | Uwagi |
|---------|-----|-----------|----------|----------------|-------|
| ML-DSA-65 (Dilithium) | Lattice | 1 952 B | 3 293 B | 3 | FIPS 204, szybki |
| SLH-DSA (SPHINCS+) | Oparty na hashach | 32-64 B | 7 856-49 856 B | 1-5 | FIPS 205, konserwatywny |
| XMSS/LMS | Oparty na hashach (stateful) | 60 B | 2 500 B | różne | Stateful — ponowne użycie = złamanie |

### Do dowodów ZK (zastępując Halo 2)

| System | Założenie | Rozmiar dowodu | Postkwantowy | Uwagi |
|--------|----------|---------------|-------------|-------|
| STARKs | Hash functions (collision resistance) | ~100-400 KB | **Tak** | Używany przez StarkNet |
| Plonky3 | FRI (hash-based polynomial commitment) | ~50-200 KB | **Tak** | Aktywny rozwój |
| Halo 2 (obecny) | ECDLP on Pasta curves | ~5 KB | **Nie** | Obecny system Orchard |
| Lattice SNARKs | MLWE | Badania | **Tak** | Nie gotowy produkcyjnie |

### Ekosystem crate'ów Rust

| Crate | Źródło | FIPS 203 | Zweryfikowany | Uwagi |
|-------|--------|----------|---------------|-------|
| `libcrux-ml-kem` | Cryspen | Tak | Formalnie zweryfikowany (hax/F*) | Najwyższe gwarancje |
| `ml-kem` | RustCrypto | Tak | Constant-time, nie audytowany | Zgodność z ekosystemem |
| `fips203` | integritychain | Tak | Constant-time | Pure Rust, no_std |

## Podsumowanie

```text
┌─────────────────────────────────────────────────────────────────────┐
│  PODSUMOWANIE ZAGROŻEŃ KWANTOWYCH DLA GROVEDB + ORCHARD             │
│                                                                     │
│  BEZPIECZNE PRZY OBECNYCH ZAŁOŻENIACH (oparte na hashach):           │
│    ✓ Blake3 drzewa Merk, MMR, BulkAppendTree                       │
│    ✓ BLAKE2b KDF, PRF^expand                                       │
│    ✓ Szyfrowanie symetryczne ChaCha20-Poly1305                     │
│    ✓ Wszystkie łańcuchy uwierzytelniania dowodów GroveDB            │
│                                                                     │
│  NAPRAWIĆ PRZED ZAPISANIEM DANYCH (retroaktywne HNDL):              │
│    ✗ Szyfrowanie notatek (uzgadnianie kluczy ECDH) → Hybrydowy KEM│
│    ✗ Zobowiązania wartości (Pedersen) → ujawnienie kwot             │
│                                                                     │
│  NAPRAWIĆ PRZED POJAWIENIEM SIĘ KOMPUTERÓW KWANTOWYCH (tylko czas  │
│  rzeczywisty):                                                      │
│    ~ Autoryzacja wydatków → ML-DSA / SLH-DSA                       │
│    ~ Dowody ZK → STARKs / Plonky3                                  │
│    ~ Sinsemilla → drzewo Merkle oparte na hashach                   │
│                                                                     │
│  ZALECANY HARMONOGRAM:                                              │
│    2026-2028: Projektowanie pod kątem rozszerzalności               │
│    2028-2030: Wdrożenie obowiązkowego hybrydowego KEM               │
│    2035+: Migracja podpisów i systemu dowodów w razie potrzeby      │
└─────────────────────────────────────────────────────────────────────┘
```

---
