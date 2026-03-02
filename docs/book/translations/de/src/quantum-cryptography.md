# Quantenkryptographie — Post-Quanten-Bedrohungsanalyse

Dieses Kapitel analysiert, wie Quantencomputer die in GroveDB und den darauf
aufbauenden geschuetzten Transaktionsprotokollen (Orchard, Dash Platform) verwendeten
kryptographischen Primitive beeinflussen wuerden. Es behandelt, welche Komponenten
verwundbar sind, welche sicher sind, was "jetzt ernten, spaeter entschluesseln"
fuer gespeicherte Daten bedeutet, und welche Mitigationsstrategien existieren —
einschliesslich hybrider KEM-Designs.

## Zwei Relevante Quantenalgorithmen

Nur zwei Quantenalgorithmen sind fuer die Kryptographie in der Praxis relevant:

**Shors Algorithmus** loest das Problem des diskreten Logarithmus (und die
Ganzzahlfaktorisierung) in polynomialer Zeit. Fuer eine 255-Bit-elliptische Kurve
wie Pallas erfordert dies etwa 510 logische Qubits — aber mit dem Overhead der
Fehlerkorrektur liegt die tatsaechliche Anforderung bei etwa 4 Millionen physischen
Qubits. Shors Algorithmus **bricht vollstaendig** jede Kryptographie auf elliptischen
Kurven, unabhaengig von der Schluesselgroesse.

**Grovers Algorithmus** bietet eine quadratische Beschleunigung fuer die
Brute-Force-Suche. Ein 256-Bit-symmetrischer Schluessel wird effektiv zu 128 Bit.
Allerdings betraegt die Schaltungstiefe fuer Grovers Algorithmus bei einem
128-Bit-Schluesselraum immer noch 2^64 Quantenoperationen — viele Kryptographen
glauben, dass dies aufgrund von Dekohaerenzgrenzen auf realer Hardware nie praktikabel
sein wird. Grovers Algorithmus reduziert Sicherheitsmargen, bricht aber gut
parametrisierte symmetrische Kryptographie nicht.

| Algorithmus | Ziele | Beschleunigung | Praktische Auswirkung |
|-------------|-------|----------------|----------------------|
| **Shor** | ECC diskreter Logarithmus, RSA-Faktorisierung | Polynomiale Zeit (exponentielle Beschleunigung gegenueber klassisch) | **Vollstaendige Brechung** von ECC |
| **Grover** | Symmetrische Schluesselsuche, Hash-Preimage | Quadratisch (halbiert Schluessel-Bits) | 256-bit → 128-bit (immer noch sicher) |

## Kryptographische Primitive von GroveDB

GroveDB und das auf Orchard basierende geschuetzte Protokoll verwenden eine Mischung
aus Primitiven fuer elliptische Kurven und symmetrischen/hash-basierten Primitiven.
Die folgende Tabelle klassifiziert jede Primitive nach ihrer Quantenverwundbarkeit:

### Quantenverwundbar (Shors Algorithmus — 0 Bits Post-Quanten)

| Primitive | Verwendung | Was gebrochen wird |
|-----------|-----------|-------------------|
| **Pallas ECDLP** | Note-Commitments (cmx), ephemere Schluessel (epk/esk), Betrachtungsschluessel (ivk), Zahlungsschluessel (pk_d), Nullifier-Ableitung | Jeden privaten Schluessel aus seinem oeffentlichen Gegenstueck wiederherstellen |
| **ECDH-Schluesselvereinbarung** (Pallas) | Ableitung symmetrischer Verschluesselungsschluessel fuer Note-Chiffretexte | Gemeinsames Geheimnis wiederherstellen → alle Notes entschluesseln |
| **Sinsemilla-Hash** | Merkle-Pfade des CommitmentTree, Hashing innerhalb des Schaltkreises | Kollisionsresistenz haengt von ECDLP ab; verschlechtert sich wenn Pallas gebrochen wird |
| **Halo 2 IPA** | ZK-Beweissystem (polynomiales Commitment ueber Pasta-Kurven) | Beweise fuer falsche Aussagen faelschen (Faelschung, unautorisierte Ausgaben) |
| **Pedersen-Commitments** | Wert-Commitments (cv_net), die Transaktionsbetraege verbergen | Verborgene Betraege wiederherstellen; Balance-Beweise faelschen |

### Quantensicher (Grovers Algorithmus — 128+ Bits Post-Quanten)

| Primitive | Verwendung | Post-Quanten-Sicherheit |
|-----------|-----------|------------------------|
| **Blake3** | Knotenhashes von Merk-Baeumen, MMR-Knoten, BulkAppendTree-Zustandswurzeln, Unterbaum-Pfadpraefixe | 128-bit Preimage, 128-bit zweite Preimage |
| **BLAKE2b-256** | KDF fuer symmetrische Schluesselableitung, ausgehender Chiffrierschluessel, PRF^expand | 128-bit Preimage |
| **ChaCha20-Poly1305** | Verschluesselt enc_ciphertext und out_ciphertext (256-Bit-Schluessel) | 128-bit Schluesselsuche (sicher, aber der Schluesselableitungspfad ueber ECDH ist es nicht) |
| **PRF^expand** (BLAKE2b-512) | Leitet esk, rcm, psi von rseed ab | 128-bit PRF-Sicherheit |

### GroveDB-Infrastruktur: Vollstaendig Quantensicher

Alle eigenen Datenstrukturen von GroveDB basieren ausschliesslich auf Blake3-Hashing:

- **Merk-AVL-Baeume** — Knotenhashes, combined_value_hash, Kind-Hash-Propagation
- **MMR-Baeume** — interne Knotenhashes, Peak-Berechnung, Wurzelableitung
- **BulkAppendTree** — Puffer-Hash-Ketten, dichte Merkle-Wurzeln, Epochen-MMR
- **CommitmentTree-Zustandswurzel** — `blake3("ct_state" || sinsemilla_root || bulk_state_root)`
- **Unterbaum-Pfadpraefixe** — Blake3-Hashing von Pfadsegmenten
- **V1-Beweise** — Authentifizierungsketten durch die Merk-Hierarchie

**Keine Aenderungen erforderlich.** GroveDBs Merk-Baum-Beweise, MMR-Konsistenzpruefungen,
BulkAppendTree-Epochenwurzeln und alle V1-Beweis-Authentifizierungsketten bleiben
sicher gegen Quantencomputer. Die hash-basierte Infrastruktur ist der staerkste Teil
des Systems im Post-Quanten-Bereich.

## Retroaktive vs. Echtzeit-Bedrohungen

Diese Unterscheidung ist entscheidend fuer die Priorisierung, was wann behoben werden muss.

**Retroaktive Bedrohungen** kompromittieren bereits gespeicherte Daten. Ein Angreifer
zeichnet heute Daten auf und entschluesselt sie, wenn Quantencomputer verfuegbar werden.
Diese Bedrohungen **koennen nachtraeglich nicht gemildert werden** — sobald die Daten
auf der Blockchain sind, koennen sie nicht erneut verschluesselt oder zurueckgerufen
werden.

**Echtzeit-Bedrohungen** betreffen nur zukuenftig erstellte Transaktionen. Ein
Angreifer mit einem Quantencomputer koennte Signaturen oder Beweise faelschen, aber
nur fuer neue Transaktionen. Alte Transaktionen wurden bereits vom Netzwerk validiert
und bestaetigt.

| Bedrohung | Typ | Was exponiert wird | Dringlichkeit |
|-----------|-----|-------------------|---------------|
| **Note-Entschluesselung** (enc_ciphertext) | **Retroaktiv** | Note-Inhalte: Empfaenger, Betrag, Memo, rseed | **Hoch** — dauerhaft gespeichert |
| **Wert-Commitment-Oeffnung** (cv_net) | **Retroaktiv** | Transaktionsbetraege (aber nicht Sender/Empfaenger) | **Mittel** — nur Betraege |
| **Sender-Wiederherstellungsdaten** (out_ciphertext) | **Retroaktiv** | Wiederherstellungsschluessel des Senders fuer gesendete Notes | **Hoch** — dauerhaft gespeichert |
| Faelschung der Ausgabenautorisierung | Echtzeit | Koennte neue Ausgabensignaturen faelschen | Niedrig — vor Ankunft des QC aktualisieren |
| Halo 2-Beweisfaelschung | Echtzeit | Koennte neue Beweise faelschen (Faelschung) | Niedrig — vor Ankunft des QC aktualisieren |
| Sinsemilla-Kollision | Echtzeit | Koennte neue Merkle-Pfade faelschen | Niedrig — durch Beweisfaelschung subsumiert |
| Faelschung der Bindungssignatur | Echtzeit | Koennte neue Balance-Beweise faelschen | Niedrig — vor Ankunft des QC aktualisieren |

### Was Genau Wird Offengelegt?

**Wenn die Note-Verschluesselung gebrochen wird** (die primaere HNDL-Bedrohung):

Ein Quantenangreifer stellt `esk` aus dem gespeicherten `epk` mittels Shors Algorithmus
wieder her, berechnet das gemeinsame ECDH-Geheimnis, leitet den symmetrischen Schluessel
ab und entschluesselt `enc_ciphertext`. Dies offenbart den vollstaendigen Note-Klartext:

| Feld | Groesse | Was es offenbart |
|------|---------|-----------------|
| version | 1 byte | Protokollversion (nicht sensibel) |
| diversifier | 11 bytes | Adresskomponente des Empfaengers |
| value | 8 bytes | Genauer Transaktionsbetrag |
| rseed | 32 bytes | Ermoeglicht Nullifier-Verkettung (deanonymisiert den Transaktionsgraphen) |
| memo | 36 bytes (DashMemo) | Anwendungsdaten, potenziell identifizierend |

Mit `rseed` und `rho` (neben dem Chiffretext gespeichert) kann der Angreifer
`esk = PRF(rseed, rho)` berechnen und die Bindung des ephemeren Schluessels
verifizieren. In Kombination mit dem diversifier verknuepft dies Eingaben mit Ausgaben
ueber die gesamte Transaktionshistorie — **vollstaendige Deanonymisierung des
geschuetzten Pools**.

**Wenn nur Wert-Commitments gebrochen werden** (sekundaere HNDL-Bedrohung):

Der Angreifer stellt `v` aus `cv_net = [v]*V + [rcv]*R` durch Loesung von ECDLP
wieder her. Dies offenbart **Transaktionsbetraege, aber nicht die Identitaeten von
Sender oder Empfaenger**. Der Angreifer sieht "jemand hat 5.0 Dash an jemanden
gesendet", kann aber den Betrag keiner Adresse oder Identitaet zuordnen, ohne auch
die Note-Verschluesselung zu brechen.

Fuer sich genommen haben Betraege ohne Verknuepfung begrenzten Nutzen. Aber in
Kombination mit externen Daten (Timing, bekannte Rechnungen, Betraege die zu
oeffentlichen Anfragen passen) werden Korrelationsangriffe moeglich.

## Der "Jetzt Ernten, Spaeter Entschluesseln"-Angriff

Dies ist die dringendste und praktischste Quantenbedrohung.

**Angriffsmodell:** Ein staatlicher Angreifer (oder jede Partei mit ausreichend
Speicherplatz) zeichnet heute alle geschuetzten On-Chain-Transaktionsdaten auf.
Diese Daten sind oeffentlich auf der Blockchain verfuegbar und unveraenderlich.
Der Angreifer wartet auf einen kryptographisch relevanten Quantencomputer (CRQC)
und dann:

```text
Schritt 1: Gespeicherten Datensatz aus dem BulkAppendTree des CommitmentTree lesen:
           cmx (32) || rho (32) || epk (32) || enc_ciphertext (104) || out_ciphertext (80)

Schritt 2: ECDLP auf Pallas via Shors Algorithmus loesen:
           epk = [esk] * g_d  →  esk wiederherstellen

Schritt 3: Gemeinsames Geheimnis berechnen:
           shared_secret = [esk] * pk_d

Schritt 4: Symmetrischen Schluessel ableiten (BLAKE2b ist quantensicher, aber die Eingabe ist kompromittiert):
           K_enc = BLAKE2b-256("Zcash_OrchardKDF", shared_secret || epk)

Schritt 5: enc_ciphertext mit ChaCha20-Poly1305 entschluesseln:
           → version || diversifier || value || rseed || memo

Schritt 6: Mit rseed + rho, Nullifier mit Note-Commitments verknuepfen:
           esk = PRF(rseed, rho)
           → vollstaendige Rekonstruktion des Transaktionsgraphen
```

**Zentrale Erkenntnis:** Die symmetrische Verschluesselung (ChaCha20-Poly1305) ist
perfekt quantensicher. Die Verwundbarkeit liegt vollstaendig im
**Schluesselableitungspfad** — der symmetrische Schluessel wird aus einem gemeinsamen
ECDH-Geheimnis abgeleitet, und ECDH wird durch Shors Algorithmus gebrochen. Der
Angreifer bricht nicht die Verschluesselung; er stellt den Schluessel wieder her.

**Retroaktivitaet:** Dieser Angriff ist **vollstaendig retroaktiv**. Jede jemals auf
der Blockchain gespeicherte verschluesselte Note kann entschluesselt werden, sobald
ein CRQC existiert. Die Daten koennen nachtraeglich nicht erneut verschluesselt oder
geschuetzt werden. Deshalb muss dies adressiert werden, bevor Daten gespeichert werden,
nicht danach.

## Mitigation: Hybrides KEM (ML-KEM + ECDH)

Die Verteidigung gegen HNDL besteht darin, den symmetrischen Verschluesselungsschluessel
aus **zwei unabhaengigen Schluesselvereinbarungsmechanismen** abzuleiten, sodass das
Brechen von nur einem unzureichend ist. Dies wird als hybrides KEM bezeichnet.

### ML-KEM-768 (CRYSTALS-Kyber)

ML-KEM ist der vom NIST standardisierte (FIPS 203, August 2024) Post-Quanten-
Schluesselkapselungsmechanismus, basierend auf dem Module Learning With Errors
(MLWE)-Problem.

| Parameter | ML-KEM-512 | ML-KEM-768 | ML-KEM-1024 |
|-----------|-----------|-----------|------------|
| Oeffentlicher Schluessel (ek) | 800 bytes | **1,184 bytes** | 1,568 bytes |
| Chiffretext (ct) | 768 bytes | **1,088 bytes** | 1,568 bytes |
| Gemeinsames Geheimnis | 32 bytes | 32 bytes | 32 bytes |
| NIST-Kategorie | 1 (128-bit) | **3 (192-bit)** | 5 (256-bit) |

**ML-KEM-768** ist die empfohlene Wahl — es ist der Parametersatz, der von X-Wing,
Signals PQXDH und dem hybriden TLS-Schluesselaustausch von Chrome/Firefox verwendet
wird. Kategorie 3 bietet einen komfortablen Spielraum gegen zukuenftige Fortschritte
in der Gitter-Kryptoanalyse.

### Wie das Hybride Schema Funktioniert

**Aktueller Ablauf (verwundbar):**

```text
Sender:
  esk = PRF(rseed, rho)                    // deterministisch aus der Note
  epk = [esk] * g_d                         // Pallas-Kurvenpunkt
  shared_secret = [esk] * pk_d              // ECDH (durch Shor gebrochen)
  K_enc = BLAKE2b("Zcash_OrchardKDF", shared_secret || epk)
  enc_ciphertext = ChaCha20(K_enc, note_plaintext)
```

**Hybrider Ablauf (quantenresistent):**

```text
Sender:
  esk = PRF(rseed, rho)                    // unveraendert
  epk = [esk] * g_d                         // unveraendert
  ss_ecdh = [esk] * pk_d                    // gleiches ECDH

  (ct_pq, ss_pq) = ML-KEM-768.Encaps(ek_pq)  // NEU: gitterbasiertes KEM
                                                // ek_pq aus der Empfaengeradresse

  K_enc = BLAKE2b(                          // GEAENDERT: kombiniert beide Geheimnisse
      "DashPlatform_HybridKDF",
      ss_ecdh || ss_pq || ct_pq || epk
  )

  enc_ciphertext = ChaCha20(K_enc, note_plaintext)  // unveraendert
```

**Empfaengerentschluesselung:**

```text
Empfaenger:
  ss_ecdh = [ivk] * epk                    // gleiches ECDH (mit eingehendem Betrachtungsschluessel)
  ss_pq = ML-KEM-768.Decaps(dk_pq, ct_pq)  // NEU: KEM-Dekapselung
  K_enc = BLAKE2b("DashPlatform_HybridKDF", ss_ecdh || ss_pq || ct_pq || epk)
  note_plaintext = ChaCha20.Decrypt(K_enc, enc_ciphertext)
```

### Sicherheitsgarantie

Das kombinierte KEM ist IND-CCA2-sicher, wenn **eines der beiden** Komponenten-KEMs
sicher ist. Dies ist formal bewiesen von [Giacon, Heuer und Poettering (2018)](https://eprint.iacr.org/2018/024)
fuer KEM-Kombinierer unter Verwendung eines PRF (BLAKE2b qualifiziert sich), und
unabhaengig durch den [X-Wing-Sicherheitsbeweis](https://eprint.iacr.org/2024/039).

| Szenario | ECDH | ML-KEM | Kombinierter Schluessel | Status |
|----------|------|--------|------------------------|--------|
| Klassische Welt | Sicher | Sicher | **Sicher** | Beide intakt |
| Quanten brechen ECC | **Gebrochen** | Sicher | **Sicher** | ML-KEM schuetzt |
| Gitter-Fortschritte brechen ML-KEM | Sicher | **Gebrochen** | **Sicher** | ECDH schuetzt (wie heute) |
| Beide gebrochen | Gebrochen | Gebrochen | **Gebrochen** | Erfordert zwei gleichzeitige Durchbrueche |

### Groessenauswirkung

Das hybride KEM fuegt den ML-KEM-768-Chiffretext (1,088 bytes) zu jeder gespeicherten
Note hinzu und erweitert den ausgehenden Chiffretext um das ML-KEM-Shared-Secret fuer
die Sender-Wiederherstellung:

**Gespeicherter Datensatz pro Note:**

```text
┌──────────────────────────────────────────────────────────────────┐
│  Aktuell (280 bytes)           Hybrid (1,400 bytes)              │
│                                                                  │
│  cmx:             32         cmx:              32               │
│  rho:             32         rho:              32               │
│  epk:             32         epk:              32               │
│  enc_ciphertext: 104         ct_pq:         1,088  ← NEU       │
│  out_ciphertext:  80         enc_ciphertext:  104               │
│                              out_ciphertext:  112  ← +32        │
│  ─────────────────           ──────────────────────             │
│  Total:          280         Total:          1,400  (5.0x)      │
└──────────────────────────────────────────────────────────────────┘
```

**Speicher im grossen Massstab:**

| Notes | Aktuell (280 B) | Hybrid (1,400 B) | Delta |
|-------|----------------|------------------|-------|
| 100,000 | 26.7 MB | 133 MB | +106 MB |
| 1,000,000 | 267 MB | 1.33 GB | +1.07 GB |
| 10,000,000 | 2.67 GB | 13.3 GB | +10.7 GB |

**Adressgroesse:**

```text
Aktuell: diversifier (11) + pk_d (32) = 43 bytes
Hybrid:  diversifier (11) + pk_d (32) + ek_pq (1,184) = 1,227 bytes
```

Der 1,184-Byte-ML-KEM-oeffentliche Schluessel muss in der Adresse enthalten sein,
damit Sender die Kapselung durchfuehren koennen. Mit etwa 1,960 Bech32m-Zeichen ist
dies gross, passt aber immer noch in einen QR-Code (maximal ~2,953 alphanumerische
Zeichen).

### Schluesselverwaltung

Das ML-KEM-Schluesselpaar wird deterministisch aus dem Ausgabenschluessel abgeleitet:

```text
SpendingKey (sk) [32 bytes]
  |
  +-> ... (gesamte bestehende Orchard-Schluesselableitung unveraendert)
  |
  +-> ml_kem_d = PRF^expand(sk, [0x09])[0..32]
  +-> ml_kem_z = PRF^expand(sk, [0x0A])[0..32]
        |
        +-> (ek_pq, dk_pq) = ML-KEM-768.KeyGen(d=ml_kem_d, z=ml_kem_z)
              ek_pq: 1,184 bytes (oeffentlich, in der Adresse enthalten)
              dk_pq: 2,400 bytes (privat, Teil des Betrachtungsschluessels)
```

**Keine Backup-Aenderungen erforderlich.** Die bestehende 24-Woerter-Seed-Phrase
deckt den ML-KEM-Schluessel ab, da er deterministisch aus dem Ausgabenschluessel
abgeleitet wird. Die Wallet-Wiederherstellung funktioniert wie zuvor.

**Diversifizierte Adressen** teilen alle denselben `ek_pq`, da ML-KEM keinen
natuerlichen Diversifizierungsmechanismus wie die Pallas-Skalarmultiplikation hat.
Dies bedeutet, dass ein Beobachter mit zwei Adressen eines Benutzers diese durch
Vergleich von `ek_pq` verknuepfen kann.

### Leistung der Probeentschluesselung

| Schritt | Aktuell | Hybrid | Delta |
|---------|---------|--------|-------|
| Pallas ECDH | ~100 us | ~100 us | — |
| ML-KEM-768 Decaps | — | ~40 us | +40 us |
| BLAKE2b KDF | ~0.5 us | ~1 us | — |
| ChaCha20 (52 bytes) | ~0.1 us | ~0.1 us | — |
| **Gesamt pro Note** | **~101 us** | **~141 us** | **+40% Overhead** |

Scannen von 100,000 Notes: ~10.1 Sek. → ~14.1 Sek. Der Overhead ist bedeutsam, aber
nicht prohibitiv. Die ML-KEM-Dekapselung laeuft in konstanter Zeit ohne
Stapelverarbeitungsvorteil (im Gegensatz zu Operationen auf elliptischen Kurven) und
skaliert daher linear.

### Auswirkung auf ZK-Schaltkreise

**Keine.** Das hybride KEM befindet sich vollstaendig in der Transport-/
Verschluesselungsschicht. Der Halo 2-Schaltkreis beweist die Existenz von Notes,
die Korrektheit von Nullifiern und die Wertbalance — er beweist nichts ueber die
Verschluesselung. Keine Aenderungen an Beweisschluesseln, Verifikationsschluesseln
oder Schaltkreis-Constraints.

### Vergleich mit der Industrie

| System | Ansatz | Status |
|--------|--------|--------|
| **Signal** (PQXDH) | X25519 + ML-KEM-768, obligatorisch fuer alle Benutzer | **Bereitgestellt** (2023) |
| **Chrome/Firefox TLS** | X25519 + ML-KEM-768 hybrider Schluesselaustausch | **Bereitgestellt** (2024) |
| **X-Wing** (IETF-Entwurf) | X25519 + ML-KEM-768, zweckgebundener Kombinierer | Standardentwurf |
| **Zcash** | Entwurf-ZIP fuer Quantenwiederherstellbarkeit (Fondswiederherstellung, nicht Verschluesselung) | Nur Diskussion |
| **Dash Platform** | Pallas ECDH + ML-KEM-768 (vorgeschlagen) | Designphase |

## Wann Bereitstellen

### Die Zeitplanfrage

- **Aktueller Stand (2026):** Kein Quantencomputer kann 255-Bit-ECC brechen. Groesste
  demonstrierte Quantenfaktorisierung: ~50 Bits. Luecke: Groessenordnungen.
- **Kurzfristig (2030-2035):** Hardware-Roadmaps von IBM, Google, Quantinuum zielen auf
  Millionen von Qubits ab. ML-KEM-Implementierungen und Parametersaetze werden
  ausgereift sein.
- **Mittelfristig (2035-2050):** Die meisten Schaetzungen platzieren die Ankunft von
  CRQC in diesem Fenster. Heute gesammelte HNDL-Daten sind gefaehrdet.
- **Langfristig (2050+):** Konsens unter Kryptographen: Grossskalige Quantencomputer
  sind eine Frage des "wann", nicht des "ob".

### Empfohlene Strategie

**1. Jetzt fuer Aktualisierbarkeit entwerfen.** Sicherstellen, dass das Format des
gespeicherten Datensatzes, die `TransmittedNoteCiphertext`-Struktur und das
BulkAppendTree-Eingabelayout versioniert und erweiterbar sind. Dies ist kostenguenstig
und bewahrt die Option, spaeter ein hybrides KEM hinzuzufuegen.

**2. Hybrides KEM bereitstellen, wenn bereit, und obligatorisch machen.** Keine zwei
Pools anbieten (klassisch und hybrid). Die Aufteilung der Anonymitaetsmenge
untergraebt den Zweck geschuetzter Transaktionen — Benutzer, die sich in einer
kleineren Gruppe verstecken, haben weniger Privatsphaere, nicht mehr. Bei der
Bereitstellung nutzt jede Note das hybride Schema.

**3. Das Fenster 2028-2030 anpeilen.** Dies liegt weit vor jeder realistischen
Quantenbedrohung, aber nach der Stabilisierung von ML-KEM-Implementierungen und
Parametergroessen. Es ermoeglicht auch, aus den Bereitstellungserfahrungen von Zcash
und Signal zu lernen.

**4. Ausloeseereignisse ueberwachen:**
- NIST oder NSA, die Post-Quanten-Migrationsfristen vorschreiben
- Bedeutende Fortschritte in der Quantenhardware (>100,000 physische Qubits mit
  Fehlerkorrektur)
- Kryptoanalytische Fortschritte gegen Gitterprobleme (wuerden die ML-KEM-Wahl
  beeinflussen)

### Was Keine Dringende Aktion Erfordert

| Komponente | Warum es warten kann |
|------------|---------------------|
| Ausgabenautorisierungssignaturen | Faelschung ist in Echtzeit, nicht retroaktiv. Auf ML-DSA/SLH-DSA aktualisieren, bevor CRQC ankommt. |
| Halo 2-Beweissystem | Beweisfaelschung ist in Echtzeit. Bei Bedarf auf STARK-basiertes System migrieren. |
| Sinsemilla-Kollisionsresistenz | Nur fuer neue Angriffe nuetzlich, nicht retroaktiv. Durch Migration des Beweissystems subsumiert. |
| GroveDB Merk/MMR/Blake3-Infrastruktur | **Unter aktuellen kryptografischen Annahmen bereits quantensicher.** Keine Aktion basierend auf bekannten Angriffen erforderlich. |

## Referenz der Post-Quanten-Alternativen

### Fuer Verschluesselung (Ersatz von ECDH)

| Schema | Typ | Oeffentlicher Schluessel | Chiffretext | NIST-Kategorie | Anmerkungen |
|--------|-----|------------------------|------------|----------------|-------------|
| ML-KEM-768 | Lattice (MLWE) | 1,184 B | 1,088 B | 3 (192-bit) | FIPS 203, Industriestandard |
| ML-KEM-512 | Lattice (MLWE) | 800 B | 768 B | 1 (128-bit) | Kleiner, geringerer Spielraum |
| ML-KEM-1024 | Lattice (MLWE) | 1,568 B | 1,568 B | 5 (256-bit) | Uebertrieben fuer hybrid |

### Fuer Signaturen (Ersatz von RedPallas/Schnorr)

| Schema | Typ | Oeffentlicher Schluessel | Signatur | NIST-Kategorie | Anmerkungen |
|--------|-----|------------------------|---------|----------------|-------------|
| ML-DSA-65 (Dilithium) | Lattice | 1,952 B | 3,293 B | 3 | FIPS 204, schnell |
| SLH-DSA (SPHINCS+) | Hash-basiert | 32-64 B | 7,856-49,856 B | 1-5 | FIPS 205, konservativ |
| XMSS/LMS | Hash-basiert (zustandsbehaftet) | 60 B | 2,500 B | variiert | Zustandsbehaftet — Wiederverwendung = Bruch |

### Fuer ZK-Beweise (Ersatz von Halo 2)

| System | Annahme | Beweisgroesse | Post-Quanten | Anmerkungen |
|--------|---------|--------------|-------------|-------------|
| STARKs | Hashfunktionen (Kollisionsresistenz) | ~100-400 KB | **Ja** | Verwendet von StarkNet |
| Plonky3 | FRI (hash-basiertes polynomiales Commitment) | ~50-200 KB | **Ja** | Aktive Entwicklung |
| Halo 2 (aktuell) | ECDLP auf Pasta-Kurven | ~5 KB | **Nein** | Aktuelles Orchard-System |
| Lattice SNARKs | MLWE | Forschung | **Ja** | Nicht produktionsreif |

### Rust-Crate-Oekosystem

| Crate | Quelle | FIPS 203 | Verifiziert | Anmerkungen |
|-------|--------|----------|-------------|-------------|
| `libcrux-ml-kem` | Cryspen | Ja | Formal verifiziert (hax/F*) | Hoechste Sicherheitsgarantie |
| `ml-kem` | RustCrypto | Ja | Konstante Zeit, nicht auditiert | Oekosystem-Kompatibilitaet |
| `fips203` | integritychain | Ja | Konstante Zeit | Reines Rust, no_std |

## Zusammenfassung

```text
┌─────────────────────────────────────────────────────────────────────┐
│  ZUSAMMENFASSUNG DER QUANTENBEDROHUNGEN FUER GROVEDB + ORCHARD     │
│                                                                     │
│  SICHER UNTER AKTUELLEN ANNAHMEN (hash-basiert):                   │
│    ✓ Blake3 Merk-Baeume, MMR, BulkAppendTree                       │
│    ✓ BLAKE2b KDF, PRF^expand                                       │
│    ✓ Symmetrische Verschluesselung ChaCha20-Poly1305               │
│    ✓ Alle GroveDB-Beweis-Authentifizierungsketten                  │
│                                                                     │
│  VOR DER DATENSPEICHERUNG BEHEBEN (retroaktives HNDL):             │
│    ✗ Note-Verschluesselung (ECDH-Schluesselvereinbarung) → Hybrid  │
│    ✗ Wert-Commitments (Pedersen) → Betraege offengelegt            │
│                                                                     │
│  VOR ANKUNFT DER QUANTENCOMPUTER BEHEBEN (nur Echtzeit):           │
│    ~ Ausgabenautorisierung → ML-DSA / SLH-DSA                     │
│    ~ ZK-Beweise → STARKs / Plonky3                                │
│    ~ Sinsemilla → hash-basierter Merkle-Baum                       │
│                                                                     │
│  EMPFOHLENER ZEITPLAN:                                              │
│    2026-2028: Fuer Aktualisierbarkeit entwerfen, Formate version. │
│    2028-2030: Obligatorisches hybrides KEM fuer Verschluesselung   │
│    2035+: Signaturen und Beweissystem bei Bedarf migrieren         │
└─────────────────────────────────────────────────────────────────────┘
```

---
