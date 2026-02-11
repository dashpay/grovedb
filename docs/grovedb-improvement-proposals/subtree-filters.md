# XX — Hierarchical Probabilistic Subtree Filters for Private Query Planning in GroveDB

**GroveDB Improvement Proposal #1**  
**Title:** Hierarchical Probabilistic Subtree Filters for Private Query Planning in GroveDB Merk‑AVL Subtrees  
**Author(s):** Samuel Westrich  
**Status:** Draft  
**Type:** Informational  
**Created:** 2025‑09‑22  
**License:** MIT

## Table of Contents
1. Abstract  
2. Motivation  
3. Prior Work  
4. Terminology  
5. Specification  
   5.1 Subtree type and cut heights  
   5.2 Filter construction and the union invariant  
   5.3 Hash commitments and a “cut‑tree certificate”  
   5.4 Client protocol (“filter ladder”)  
   5.5 Parameter selection and sizing (with worked example)  
   5.6 API additions  
   5.7 Updates, rotations, and caching  
6. Rationale  
7. Privacy considerations  
8. Backwards compatibility & deployment  
9. Security considerations  
10. Implementation notes  
11. Acknowledgements  
12. References

---

## 1. Abstract

This proposal introduces a **specialized Merk‑AVL subtree type** in GroveDB whose internal nodes at selected levels carry **union‑composable approximate‑membership filters**. By choosing a filter whose **parent equals the bitwise union of its children**, the structure remains correct across inserts, deletes, and **AVL rotations**. Clients can first download and verify a **whole “cut” layer** of filters, privately test their own keys locally, and then selectively descend only into subtrees that **might** contain matches. The technique provides **predictable bandwidth** and **reduced information leakage** compared to querying subtrees directly. GroveDB’s design already supports authenticated subtrees and **cross‑tree references**; this DIP adds a verifiable, privacy‑preserving **map‑before‑fetch** capability.  [oai_citation:0‡GitHub](https://github.com/dashpay/grovedb)

---

## 2. Motivation

GroveDB uses **Merk‑AVL (“Merk”) trees** to provide authenticated storage with secondary indices and references. Many applications want to locate relevant records **without revealing** which exact keys they seek. Shipping a **public, verifiable layer of probabilistic filters** lets a client **plan** its fetches locally and only request data from promising regions—similar in spirit to header‑first SPV and compact filter approaches, but applied to an **index subtree** instead of per‑block data. This pattern is consistent with the normative DIP format used in Dash Core documents (e.g., DIP‑0016) and leverages Bloom‑filter‑based wallet workflows that Dash already documents.  [oai_citation:1‡Dash Documentation](https://docs.dash.org/projects/core/en/20.1.0/docs/dips/dip-0016.html)

---

## 3. Prior Work

- **DIP‑0016 (Headers‑First SPV)** documents wallet synchronization phases and the construction of probabilistic (Bloom) filters in Dash wallets. We adapt the idea of **probabilistic precomputation** to GroveDB’s index level.  [oai_citation:2‡Dash Documentation](https://docs.dash.org/projects/core/en/20.1.0/docs/dips/dip-0016.html)  
- **Hierarchical Bloom indexes (“Bloofi”)** organize many Bloom filters so that **internal nodes are the bitwise OR of child filters**, enabling scalable search and updates—this is the key algebraic property we require.  [oai_citation:3‡arXiv](https://arxiv.org/abs/1501.01941?utm_source=chatgpt.com)  
- **Compact block filters (BIP157/158)** show the benefit of distributing **client‑verifiable filters** first, then fetching data. We adopt the model (filters → local test → selective fetch) for **Merk subtrees**.  [oai_citation:4‡BIPs](https://bips.dev/158/?utm_source=chatgpt.com)

---

## 4. Terminology

- **Merk‑AVL subtree:** A GroveDB subtree implemented as a Merkle‑committed AVL tree.  [oai_citation:5‡GitHub](https://github.com/dashpay/grovedb)  
- **Cut height `h`:** A selected level at which all node filters are materialized and can be fetched as a batch.  
- **Filter ladder:** Recursively fetching **only** the next filter layer under positives to refine candidates before downloading payloads.  
- **Reference:** GroveDB’s built‑in cross‑tree pointer; used to link index nodes to underlying records.  [oai_citation:6‡GitHub](https://github.com/dashpay/grovedb)

---

## 5. Specification

### 5.1 Subtree type and cut heights

Define a new GroveDB subtree **type** (e.g., `merk_with_filters`) that:

1. Stores normal `(key, value)` pairs (values may be **references** to data trees).  
2. At one or more **cut heights** `h` (and optionally `h+Δ`), **internal nodes** store **filter bytes**; other nodes store only a **filter digest** (commitment) inside the node hash.

Administrators MAY choose multiple cut heights for different **granularities** (e.g., `h` for coarse mapping, `h+Δ` for refinement).

### 5.2 Filter construction and the union invariant

**Filter choice:** classic **Bloom filter** with global parameters `m` (bits) and `k` (hashes), applied consistently across the special subtree.

**Invariant:** for any internal node `v`, BF(v) = BF(left(v)) OR BF(right(v)) Bitwise OR must produce exactly the same filter as inserting all child elements into an empty Bloom filter with the same (`m`,`k`,`hash family`). This makes the structure **rotation‑invariant** and cheap to maintain (see §5.7). The OR‑composable hierarchy matches **Bloofi**.  [oai_citation:7‡arXiv](https://arxiv.org/abs/1501.01941?utm_source=chatgpt.com)

**Bloom sizing:** with false‑positive rate `p` and `n` elements in a node, the optimal bits per element and number of hashes are

\[
\frac{m}{n} \approx \frac{-\ln p}{(\ln 2)^2}, \qquad k \approx \ln 2 \cdot \frac{m}{n}.
\]

Implementations SHOULD derive the `k` bit positions via **double hashing** (Kirsch–Mitzenmacher):  
`g_i(x) = h1(x) + i·h2(x) (mod m)` for `i=0..k-1`.  [oai_citation:8‡Harvard EECS](https://www.eecs.harvard.edu/~michaelm/postscripts/rsa2008.pdf?utm_source=chatgpt.com)

**Hash domain separation and salting:** use a **public, per‑epoch salt** that is committed in the subtree root. The salt MUST remain **constant** within the subtree so OR‑composition remains exact.

### 5.3 Hash commitments and a “cut‑tree certificate”

Every node hash MUST commit to the filter (or its absence):

nodeHash = H(meta || hash(left) || hash(right) || H(filter_bytes_or_empty))

To let clients verify a whole cut efficiently, servers MUST support a **cut‑tree certificate**: a pruned skeleton of internal hashes from the root to all nodes at height `h`. For a complete binary tree, this is `(2^{h+1}-1)` hashes (≈ **1.0 MiB** at `h=14` with 32‑byte hashes), amortizing verification across all `2^h` filters. (Real trees may be sparse; implementations SHOULD omit empty branches.)

### 5.4 Client protocol (“filter ladder”)

1. **Fetch & verify cut `h`:** Client downloads all `2^h` filters plus the cut‑tree certificate, verifies digests to the subtree root, and caches them.  
2. **Local match:** Test query keys locally; collect positive **parent buckets**.  
3. **Refine (optional):** For those parents, request filters at `h+Δ` (children), verify, and repeat.  
4. **Fetch data:** Request only the necessary subtrees or **reference‑linked** records, with standard GroveDB proofs.

This is the **filters‑first** model (cf. BIP157/158’s “client verifies then fetches”).  [oai_citation:9‡BIPs](https://bips.dev/158/?utm_source=chatgpt.com)

### 5.5 Parameter selection and sizing (with worked example)

Let the subtree index **N** elements in total. For a single cut at height `h`:

- Average elements per parent ≈ `N / 2^h`.  
- **Per‑parent filter** (Bloom) size ≈ `(N / 2^h) * (−ln p)/(ln 2)^2` **bits**.  
- **Total filter bytes for the whole cut** ≈ `N * (−ln p)/(ln 2)^2 / 8` — **independent of `h`**. (You pick `h` for **granularity**, not bytes.)

**Worked example (illustrative):**  
If `N = 10,000,000` and `p = 1%`:

- `bits/elem ≈ 4.60517 / 0.48045 ≈ 9.59`  
- **Total filter payload** = `N * 9.59 / 8 ≈ 12.0 MB` for the first cut (regardless of whether `h=12` or `h=16`).  
- At `p = 0.1%`, `bits/elem ≈ 14.38` → ≈ **18.0 MB**.

Subsequent **refinement layers** only scale with the **fraction of positive parents** and quickly become small; the initial cut dominates.

### 5.6 API additions

New read‑only endpoints for `merk_with_filters` subtrees:

- `getFilterCut(root_id, h)` → `[(node_id, filter_bytes)]` + **cut‑tree certificate**.  
- `getFilterChildren(root_id, parent_ids, Δ)` → child filters under the specified parents `Δ` levels below.  
- `getIndexNodes(root_id, keys|ranges)` → returns matched index nodes (optionally **references** to records).  
- `getRefs(node_id)` → follow reference(s) to target data tree(s).

(Exact wire formats are implementation‑defined; responses MUST include proofs necessary to recompute the subtree root commitment.)

### 5.7 Updates, rotations, and caching

- **Insert/delete:** Update leaf (or small leaf bucket), recompute `BF` along the path to the nearest filter‑bearing ancestor(s) using **bitwise OR**; cost `O(k * m_word_ops * log N)`.  
- **AVL rotations:** Only nodes whose child pointers change require re‑`OR`; the **union invariant** guarantees correctness. (This is exactly the hierarchical approach used by Bloofi.)  [oai_citation:10‡arXiv](https://arxiv.org/abs/1501.01941?utm_source=chatgpt.com)  
- **Caching & content addressing:** Serve filters as blobs content‑addressed by **node hash**, enabling CDN and client reuse.

---

## 6. Rationale

- **Why Bloom filters?** They are **exactly OR‑composable**, so parents remain valid across **AVL rotations** with a few cheap bitwise operations—unlike XOR/binary‑fuse or Golomb‑Rice coded sets which compress well for transmission but are **not** closed under union.  [oai_citation:11‡arXiv](https://arxiv.org/pdf/2201.01174?utm_source=chatgpt.com)  
- **Why a whole‑cut certificate?** It amortizes verification across an entire layer, avoiding `2^h` independent proofs while preserving full **Merkle verifiability**.  
- **Why global parameters (`m`,`k`,`salt`)?** Ensures that `parent = OR(children)` holds **exactly** and simplifies updates.

---

## 7. Privacy considerations

- **Initial cut download** is identical for all clients—reveals nothing.  
- Subsequent requests reveal only **sets of parent buckets**; clients SHOULD add **cover traffic** (fetch K extra) or perform extra refinement steps (smaller, more numerous filters) to widen the anonymity set.  
- Use a **public, per‑epoch salt** in the Bloom hash derivation to reduce offline enumeration risk, and rotate salts on schedule (new snapshots) with a deterministic, Merkle‑committed value.

---

## 8. Backwards compatibility & deployment

- This DIP would not modify consensus of clients using groveDB.  
- It adds a new subtree type and read‑only endpoints to GroveDB/Platform or DAPI surfaces.  
- Existing trees continue to operate unmodified; the feature can be considered an opt‑in per subtree.

---

## 9. Security considerations

- **Filter integrity:** Node hashes **commit** to filter bytes; clients verify against the subtree root. A malicious server cannot introduce false negatives without breaking the commitment.  
- **DoS:** Servers SHOULD rate‑limit large cut downloads and bound per‑request child expansion.  
- **Correctness:** With standard Bloom construction (e.g., **double hashing**) and fixed parameters, theoretical false‑negative probability is **zero**; false positives are bounded by `p`.  [oai_citation:12‡Harvard EECS](https://www.eecs.harvard.edu/~michaelm/postscripts/rsa2008.pdf?utm_source=chatgpt.com)

---

## 10. Implementation notes

- **Where to store filters:** Only at selected heights (`h`, optionally `h+Δ`). Other nodes store **digests only** to keep node payloads small.  
- **On‑the‑wire representation:** Bloom filters near their optimal density (~50%) compress modestly; implementers MAY use a **compressed Bloom** wire format while expanding to raw bitsets in memory.  
- **References:** Leverage GroveDB’s **reference system** so index nodes can point to underlying records (transactions, documents, etc.), enabling selective retrieval once a subtree is deemed promising.  [oai_citation:13‡GitHub](https://github.com/dashpay/grovedb)

---

## 11. Acknowledgements

We thank the authors and editors of the **Dash DIPs** for the structure and conventions (e.g., status, type, rationale), especially **DIP‑0016** for documenting probabilistic filter use in Dash wallet sync; and the **Bloofi** authors for the hierarchical OR‑composable approach that underpins this design.  [oai_citation:14‡Dash Documentation](https://docs.dash.org/projects/core/en/21.0.0/docs/dips/README.html?utm_source=chatgpt.com)

---

## 12. References

- **GroveDB README** (Merk‑AVL, references, proofs).  [oai_citation:15‡GitHub](https://github.com/dashpay/grovedb)  
- **DIP‑0016 — Headers First Synchronization on SPV Wallets** (Samuel Westrich).  [oai_citation:16‡Dash Documentation](https://docs.dash.org/projects/core/en/20.1.0/docs/dips/dip-0016.html)  
- **Dash DIPs overview and process/format.**  [oai_citation:17‡Dash Documentation](https://docs.dash.org/projects/core/en/21.0.0/docs/dips/README.html?utm_source=chatgpt.com)  
- **Bloofi — Hierarchical Bloom Filters.**  [oai_citation:18‡arXiv](https://arxiv.org/abs/1501.01941?utm_source=chatgpt.com)  
- **BIP157/158 — Compact Block Filters (Neutrino) overview.**  [oai_citation:19‡BIPs](https://bips.dev/158/?utm_source=chatgpt.com)  
- **Kirsch & Mitzenmacher (2006/2008)** — Two‑hash/“double hashing” construction for Bloom filters.  [oai_citation:20‡Harvard EECS](https://www.eecs.harvard.edu/~michaelm/postscripts/rsa2008.pdf?utm_source=chatgpt.com)