# Quantum Cryptography — Post-Quantum Threat Analysis

This chapter analyzes how quantum computers would affect the cryptographic
primitives used in GroveDB and the shielded transaction protocols built on top
of it (Orchard, Dash Platform). It covers which components are vulnerable, which
are safe, what "harvest now, decrypt later" means for stored data, and what
mitigation strategies exist — including hybrid KEM designs.

## Two Quantum Algorithms That Matter

Only two quantum algorithms are relevant to cryptography in practice:

**Shor's algorithm** solves the discrete logarithm problem (and integer
factoring) in polynomial time. For a 255-bit elliptic curve like Pallas, this
requires roughly 510 logical qubits — but with error correction overhead, the
real requirement is approximately 4 million physical qubits. Shor's algorithm
**completely breaks** all elliptic curve cryptography regardless of key size.

**Grover's algorithm** provides a quadratic speedup for brute-force search.
A 256-bit symmetric key effectively becomes 128-bit. However, the circuit depth
for Grover's on a 128-bit key space is still 2^64 quantum operations — many
cryptographers believe this will never be practical on real hardware due to
decoherence limits. Grover's reduces security margins but does not break
well-parameterized symmetric cryptography.

| Algorithm | Targets | Speedup | Practical impact |
|-----------|---------|---------|------------------|
| **Shor's** | ECC discrete log, RSA factoring | Polynomial time (exponential speedup over classical) | **Total break** of ECC |
| **Grover's** | Symmetric key search, hash preimage | Quadratic (halves key bits) | 256-bit → 128-bit (still safe) |

## GroveDB's Cryptographic Primitives

GroveDB and the Orchard-based shielded protocol use a mix of elliptic curve
and symmetric/hash-based primitives. The table below classifies every primitive
by its quantum vulnerability:

### Quantum-Vulnerable (Shor's algorithm — 0 bits post-quantum)

| Primitive | Where used | What breaks |
|-----------|-----------|-------------|
| **Pallas ECDLP** | Note commitments (cmx), ephemeral keys (epk/esk), viewing keys (ivk), payment keys (pk_d), nullifier derivation | Recover any private key from its public counterpart |
| **ECDH key agreement** (Pallas) | Deriving symmetric encryption keys for note ciphertexts | Recover shared secret → decrypt all notes |
| **Sinsemilla hash** | Commitment tree Merkle paths, in-circuit hashing | Collision resistance depends on ECDLP; degrades when Pallas breaks |
| **Halo 2 IPA** | ZK proof system (polynomial commitment over Pasta curves) | Forge proofs for false statements (counterfeit, unauthorized spends) |
| **Pedersen commitments** | Value commitments (cv_net) hiding transaction amounts | Recover hidden amounts; forge balance proofs |

### Quantum-Safe (Grover's algorithm — 128+ bits post-quantum)

| Primitive | Where used | Post-quantum security |
|-----------|-----------|----------------------|
| **Blake3** | Merk tree node hashes, MMR nodes, BulkAppendTree state roots, subtree path prefixes | 128-bit preimage, 128-bit second-preimage |
| **BLAKE2b-256** | KDF for symmetric key derivation, outgoing cipher key, PRF^expand | 128-bit preimage |
| **ChaCha20-Poly1305** | Encrypts enc_ciphertext and out_ciphertext (256-bit keys) | 128-bit key search (safe, but key derivation path through ECDH is not) |
| **PRF^expand** (BLAKE2b-512) | Derives esk, rcm, psi from rseed | 128-bit PRF security |

### GroveDB Infrastructure: Believed Quantum-Safe Under Current Assumptions

All of GroveDB's own data structures rely exclusively on Blake3 hashing, which
is believed to be quantum-resistant under current cryptographic assumptions:

- **Merk AVL trees** — node hashes, combined_value_hash, child hash propagation
- **MMR trees** — internal node hashes, peak computation, root derivation
- **BulkAppendTree** — buffer hash chains, dense Merkle roots, epoch MMR
- **CommitmentTree state root** — `blake3("ct_state" || sinsemilla_root || bulk_state_root)`
- **Subtree path prefixes** — Blake3 hashing of path segments
- **V1 proofs** — authentication chains through Merk hierarchy

**No changes needed based on known attacks.** GroveDB's Merk tree proofs, MMR
consistency checks, BulkAppendTree epoch roots, and all V1 proof authentication
chains are believed to remain secure against quantum computers. Hash-based
infrastructure is the strongest part of the system post-quantum, though
assessments may evolve with new cryptanalytic techniques.

## Retroactive vs Real-Time Threats

This distinction is critical for prioritizing what to fix and when.

**Retroactive threats** compromise data that is already stored. An adversary
records data today and decrypts it when quantum computers become available. These
threats **cannot be mitigated after the fact** — once the data is on-chain, it
cannot be re-encrypted or recalled.

**Real-time threats** only affect transactions created in the future. An
adversary with a quantum computer could forge signatures or proofs, but only for
new transactions. Old transactions were already validated and confirmed by the
network.

| Threat | Type | What's exposed | Urgency |
|--------|------|---------------|---------|
| **Note decryption** (enc_ciphertext) | **Retroactive** | Note contents: recipient, amount, memo, rseed | **High** — stored forever |
| **Value commitment opening** (cv_net) | **Retroactive** | Transaction amounts (but not sender/receiver) | **Medium** — amounts only |
| **Sender recovery data** (out_ciphertext) | **Retroactive** | Sender's recovery keys for sent notes | **High** — stored forever |
| Spend authorization forgery | Real-time | Could forge new spend signatures | Low — upgrade before QC arrives |
| Halo 2 proof forgery | Real-time | Could forge new proofs (counterfeit) | Low — upgrade before QC arrives |
| Sinsemilla collision | Real-time | Could forge new Merkle paths | Low — subsumed by proof forgery |
| Binding signature forgery | Real-time | Could forge new balance proofs | Low — upgrade before QC arrives |

### What Exactly Gets Revealed?

**If note encryption is broken** (the primary HNDL threat):

A quantum adversary recovers `esk` from the stored `epk` via Shor's algorithm,
computes the ECDH shared secret, derives the symmetric key, and decrypts
`enc_ciphertext`. This reveals the full note plaintext:

| Field | Size | What it reveals |
|-------|------|----------------|
| version | 1 byte | Protocol version (not sensitive) |
| diversifier | 11 bytes | Recipient's address component |
| value | 8 bytes | Exact transaction amount |
| rseed | 32 bytes | Enables nullifier linkage (deanonymizes transaction graph) |
| memo | 36 bytes (DashMemo) | Application data, potentially identifying |

With `rseed` and `rho` (stored alongside the ciphertext), the adversary can
compute `esk = PRF(rseed, rho)` and verify the ephemeral key binding. Combined
with the diversifier, this links inputs to outputs across the entire transaction
history — **full deanonymization of the shielded pool**.

**If only value commitments are broken** (secondary HNDL threat):

The adversary recovers `v` from `cv_net = [v]*V + [rcv]*R` by solving ECDLP.
This reveals **transaction amounts but not sender or receiver identities**.
The adversary sees "someone sent 5.0 Dash to someone" but cannot link the
amount to any address or identity without also breaking note encryption.

On its own, amounts without linkage are limited in usefulness. But combined
with external data (timing, known invoices, amounts matching public requests),
correlation attacks become possible.

## The "Harvest Now, Decrypt Later" Attack

This is the most urgent and practical quantum threat.

**Attack model:** A state-level adversary (or any party with sufficient storage)
records all on-chain shielded transaction data today. This data is publicly
available on the blockchain and immutable. The adversary waits for a
cryptographically relevant quantum computer (CRQC), then:

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

**Key insight:** The symmetric encryption (ChaCha20-Poly1305) is perfectly
quantum-safe. The vulnerability is entirely in the **key derivation path** —
the symmetric key is derived from an ECDH shared secret, and ECDH is broken
by Shor's algorithm. The attacker doesn't break the encryption; they recover
the key.

**Retroactivity:** This attack is **fully retroactive**. Every encrypted note
ever stored on-chain can be decrypted once a CRQC exists. The data cannot be
re-encrypted or protected after the fact. This is why it must be addressed
before data is stored, not after.

## Mitigation: Hybrid KEM (ML-KEM + ECDH)

The defense against HNDL is to derive the symmetric encryption key from
**two independent key agreement mechanisms**, such that breaking only one is
insufficient. This is called a hybrid KEM.

### ML-KEM-768 (CRYSTALS-Kyber)

ML-KEM is the NIST-standardized (FIPS 203, August 2024) post-quantum key
encapsulation mechanism based on the Module Learning With Errors (MLWE) problem.

| Parameter | ML-KEM-512 | ML-KEM-768 | ML-KEM-1024 |
|-----------|-----------|-----------|------------|
| Public key (ek) | 800 bytes | **1,184 bytes** | 1,568 bytes |
| Ciphertext (ct) | 768 bytes | **1,088 bytes** | 1,568 bytes |
| Shared secret | 32 bytes | 32 bytes | 32 bytes |
| NIST Category | 1 (128-bit) | **3 (192-bit)** | 5 (256-bit) |

**ML-KEM-768** is the recommended choice — it is the parameter set used by
X-Wing, Signal's PQXDH, and Chrome/Firefox TLS hybrid key exchange. Category 3
provides a comfortable margin against future lattice cryptanalysis advances.

### How the Hybrid Scheme Works

**Current flow (vulnerable):**

```text
Sender:
  esk = PRF(rseed, rho)                    // deterministic from note
  epk = [esk] * g_d                         // Pallas curve point
  shared_secret = [esk] * pk_d              // ECDH (broken by Shor's)
  K_enc = BLAKE2b("Zcash_OrchardKDF", shared_secret || epk)
  enc_ciphertext = ChaCha20(K_enc, note_plaintext)
```

**Hybrid flow (quantum-resistant):**

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

**Recipient decryption:**

```text
Recipient:
  ss_ecdh = [ivk] * epk                    // same ECDH (using incoming viewing key)
  ss_pq = ML-KEM-768.Decaps(dk_pq, ct_pq)  // NEW: KEM decapsulation
  K_enc = BLAKE2b("DashPlatform_HybridKDF", ss_ecdh || ss_pq || ct_pq || epk)
  note_plaintext = ChaCha20.Decrypt(K_enc, enc_ciphertext)
```

### Security Guarantee

The combined KEM is IND-CCA2 secure if **either** component KEM is secure.
This is formally proven by [Giacon, Heuer, and Poettering (2018)](https://eprint.iacr.org/2018/024)
for KEM combiners using a PRF (BLAKE2b qualifies), and independently by the
[X-Wing security proof](https://eprint.iacr.org/2024/039).

| Scenario | ECDH | ML-KEM | Combined key | Status |
|----------|------|--------|-------------|--------|
| Classical world | Secure | Secure | **Secure** | Both intact |
| Quantum breaks ECC | **Broken** | Secure | **Secure** | ML-KEM protects |
| Lattice advances break ML-KEM | Secure | **Broken** | **Secure** | ECDH protects (same as today) |
| Both broken | Broken | Broken | **Broken** | Requires two simultaneous breakthroughs |

### Size Impact

The hybrid KEM adds the ML-KEM-768 ciphertext (1,088 bytes) to each stored
note and expands the outgoing ciphertext to include the ML-KEM shared secret
for sender recovery:

**Stored record per note:**

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

**Storage at scale:**

| Notes | Current (280 B) | Hybrid (1,400 B) | Delta |
|-------|----------------|------------------|-------|
| 100,000 | 26.7 MB | 133 MB | +106 MB |
| 1,000,000 | 267 MB | 1.33 GB | +1.07 GB |
| 10,000,000 | 2.67 GB | 13.3 GB | +10.7 GB |

**Address size:**

```text
Current:  diversifier (11) + pk_d (32) = 43 bytes
Hybrid:   diversifier (11) + pk_d (32) + ek_pq (1,184) = 1,227 bytes
```

The 1,184-byte ML-KEM public key must be included in the address so senders
can perform encapsulation. At ~1,960 Bech32m characters, this is large but
still fits in a QR code (max ~2,953 alphanumeric characters).

### Key Management

The ML-KEM keypair derives deterministically from the spending key:

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

**No backup changes needed.** The existing 24-word seed phrase covers the
ML-KEM key because it derives from the spending key deterministically. Wallet
recovery works as before.

**Diversified addresses** all share the same `ek_pq` because ML-KEM has no
natural diversification mechanism like Pallas scalar multiplication. This means
an observer with two of a user's addresses can link them by comparing `ek_pq`.

### Trial Decryption Performance

| Step | Current | Hybrid | Delta |
|------|---------|--------|-------|
| Pallas ECDH | ~100 us | ~100 us | — |
| ML-KEM-768 Decaps | — | ~40 us | +40 us |
| BLAKE2b KDF | ~0.5 us | ~1 us | — |
| ChaCha20 (52 bytes) | ~0.1 us | ~0.1 us | — |
| **Total per note** | **~101 us** | **~141 us** | **+40% overhead** |

Scanning 100,000 notes: ~10.1 sec → ~14.1 sec. The overhead is meaningful but
not prohibitive. ML-KEM decapsulation is constant-time with no batching
advantage (unlike elliptic curve operations), so it scales linearly.

### Impact on ZK Circuits

**None.** The hybrid KEM is entirely in the transport/encryption layer. The
Halo 2 circuit proves note existence, nullifier correctness, and value balance
— it does not prove anything about encryption. No changes to proving keys,
verifying keys, or circuit constraints.

### Comparison with Industry

| System | Approach | Status |
|--------|----------|--------|
| **Signal** (PQXDH) | X25519 + ML-KEM-768, mandatory for all users | **Deployed** (2023) |
| **Chrome/Firefox TLS** | X25519 + ML-KEM-768 hybrid key exchange | **Deployed** (2024) |
| **X-Wing** (IETF draft) | X25519 + ML-KEM-768, purpose-built combiner | Draft standard |
| **Zcash** | Quantum recoverability draft ZIP (fund recovery, not encryption) | Discussion only |
| **Dash Platform** | Pallas ECDH + ML-KEM-768 (proposed) | Design phase |

## When to Deploy

### The Timeline Question

- **Current state (2026):** No quantum computer can break 255-bit ECC. Largest
  demonstrated quantum factoring: ~50 bits. Gap: orders of magnitude.
- **Near-term (2030-2035):** Hardware roadmaps from IBM, Google, Quantinuum
  target millions of qubits. ML-KEM implementations and parameter sets will
  have matured.
- **Medium-term (2035-2050):** Most estimates place CRQC arrival in this
  window. HNDL data collected today is at risk.
- **Long-term (2050+):** Consensus among cryptographers: large-scale quantum
  computers are a matter of "when," not "if."

### Recommended Strategy

**1. Design for upgradability now.** Ensure the stored record format, the
`TransmittedNoteCiphertext` struct, and the BulkAppendTree entry layout are
versioned and extensible. This is low-cost and preserves the option to add
hybrid KEM later.

**2. Deploy hybrid KEM when ready, make it mandatory.** Do not offer two pools
(classical and hybrid). Splitting the anonymity set defeats the purpose of
shielded transactions — users hiding among a smaller group are less private,
not more. When deployed, every note uses the hybrid scheme.

**3. Target the 2028-2030 window.** This is well before any realistic quantum
threat but after ML-KEM implementations and parameter sizes have stabilized.
It also allows learning from Zcash's and Signal's deployment experience.

**4. Monitor trigger events:**
- NIST or NSA mandating post-quantum migration deadlines
- Significant advances in quantum hardware (>100,000 physical qubits with
  error correction)
- Cryptanalytic advances against lattice problems (would affect ML-KEM choice)

### What Does Not Need Urgent Action

| Component | Why it can wait |
|-----------|----------------|
| Spend authorization signatures | Forgery is real-time, not retroactive. Upgrade to ML-DSA/SLH-DSA before CRQC arrives. |
| Halo 2 proof system | Proof forgery is real-time. Migrate to STARK-based system when needed. |
| Sinsemilla collision resistance | Only useful for new attacks, not retroactive. Subsumed by proof system migration. |
| GroveDB Merk/MMR/Blake3 infrastructure | **Already quantum-safe** under current cryptographic assumptions. No action needed based on known attacks. |

## Post-Quantum Alternatives Reference

### For Encryption (replacing ECDH)

| Scheme | Type | Public key | Ciphertext | NIST Category | Notes |
|--------|------|-----------|-----------|---------------|-------|
| ML-KEM-768 | Lattice (MLWE) | 1,184 B | 1,088 B | 3 (192-bit) | FIPS 203, industry standard |
| ML-KEM-512 | Lattice (MLWE) | 800 B | 768 B | 1 (128-bit) | Smaller, lower margin |
| ML-KEM-1024 | Lattice (MLWE) | 1,568 B | 1,568 B | 5 (256-bit) | Overkill for hybrid |

### For Signatures (replacing RedPallas/Schnorr)

| Scheme | Type | Public key | Signature | NIST Category | Notes |
|--------|------|-----------|----------|---------------|-------|
| ML-DSA-65 (Dilithium) | Lattice | 1,952 B | 3,293 B | 3 | FIPS 204, fast |
| SLH-DSA (SPHINCS+) | Hash-based | 32-64 B | 7,856-49,856 B | 1-5 | FIPS 205, conservative |
| XMSS/LMS | Hash-based (stateful) | 60 B | 2,500 B | varies | Stateful — reuse = break |

### For ZK Proofs (replacing Halo 2)

| System | Assumption | Proof size | Post-quantum | Notes |
|--------|-----------|-----------|-------------|-------|
| STARKs | Hash functions (collision resistance) | ~100-400 KB | **Yes** | Used by StarkNet |
| Plonky3 | FRI (hash-based polynomial commitment) | ~50-200 KB | **Yes** | Active development |
| Halo 2 (current) | ECDLP on Pasta curves | ~5 KB | **No** | Current Orchard system |
| Lattice SNARKs | MLWE | Research | **Yes** | Not production-ready |

### Rust Crate Ecosystem

| Crate | Source | FIPS 203 | Verified | Notes |
|-------|--------|----------|----------|-------|
| `libcrux-ml-kem` | Cryspen | Yes | Formally verified (hax/F*) | Highest assurance |
| `ml-kem` | RustCrypto | Yes | Constant-time, not audited | Ecosystem compatibility |
| `fips203` | integritychain | Yes | Constant-time | Pure Rust, no_std |

## Summary

```text
┌─────────────────────────────────────────────────────────────────────┐
│  QUANTUM THREAT SUMMARY FOR GROVEDB + ORCHARD                      │
│                                                                     │
│  SAFE UNDER CURRENT ASSUMPTIONS (hash-based):                        │
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
