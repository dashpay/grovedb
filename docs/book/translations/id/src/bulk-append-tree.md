# BulkAppendTree — Penyimpanan Append-Only Throughput Tinggi

BulkAppendTree adalah jawaban GroveDB untuk tantangan rekayasa spesifik: bagaimana Anda
membangun log append-only throughput tinggi yang mendukung proof rentang yang efisien,
meminimalkan hashing per-tulis, dan menghasilkan snapshot chunk yang tidak dapat diubah
yang cocok untuk distribusi CDN?

Sementara MmrTree (Bab 13) ideal untuk proof daun individual, BulkAppendTree
dirancang untuk beban kerja di mana ribuan nilai tiba per blok dan klien perlu
menyinkronkan dengan mengambil rentang data. Ia mencapai ini dengan **arsitektur dua
level**: buffer dense Merkle tree yang menyerap append yang masuk, dan chunk-level MMR
yang mencatat root chunk yang telah difinalisasi.

## Arsitektur Dua Level

```text
┌────────────────────────────────────────────────────────────────┐
│                      BulkAppendTree                            │
│                                                                │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  Chunk MMR                                               │  │
│  │  ┌────┐ ┌────┐ ┌────┐ ┌────┐                            │  │
│  │  │ R0 │ │ R1 │ │ R2 │ │ H  │ ← Dense Merkle roots      │  │
│  │  └────┘ └────┘ └────┘ └────┘   of each chunk blob       │  │
│  │                     peak hashes bagged together = MMR root│  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  Buffer (DenseFixedSizedMerkleTree, capacity = 2^h - 1) │  │
│  │  ┌───┐ ┌───┐ ┌───┐                                      │  │
│  │  │v_0│ │v_1│ │v_2│ ... (fills in level-order)           │  │
│  │  └───┘ └───┘ └───┘                                      │  │
│  │  dense_tree_root = recomputed root hash of dense tree     │  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                │
│  state_root = blake3("bulk_state" || mmr_root || dense_tree_root) │
│                                                                │
└────────────────────────────────────────────────────────────────┘
```

**Level 1 — Buffer.** Nilai yang masuk ditulis ke `DenseFixedSizedMerkleTree`
(lihat Bab 16). Kapasitas buffer adalah `2^height - 1` posisi. Root hash dense tree
(`dense_tree_root`) diperbarui setelah setiap penyisipan.

**Level 2 — Chunk MMR.** Ketika buffer penuh (mencapai `chunk_size` entri),
semua entri diserialisasi menjadi **chunk blob** yang tidak dapat diubah, dense Merkle root
dihitung atas entri-entri tersebut, dan root tersebut ditambahkan sebagai daun ke chunk MMR.
Buffer kemudian dikosongkan.

**State root** menggabungkan kedua level menjadi satu komitmen 32 byte yang berubah
pada setiap append, memastikan pohon Merk induk selalu mencerminkan state terbaru.

## Bagaimana Nilai Mengisi Buffer

Setiap pemanggilan `append()` mengikuti urutan ini:

```text
Step 1: Write value to dense tree buffer at next position
        dense_tree.insert(value, store)

Step 2: Increment total_count
        total_count += 1

Step 3: Check if buffer is full (dense tree at capacity)
        if dense_tree.count() == capacity:
            → trigger compaction (§14.3)

Step 4: Compute new state root (+1 blake3 call)
        dense_tree_root = dense_tree.root_hash(store)
        state_root = blake3("bulk_state" || mmr_root || dense_tree_root)
```

**Buffer ADALAH sebuah DenseFixedSizedMerkleTree** (lihat Bab 16). Root hash-nya
berubah setelah setiap penyisipan, memberikan komitmen terhadap semua entri buffer saat ini.
Root hash inilah yang mengalir ke komputasi state root.

## Kompaksi Chunk

Ketika buffer penuh (mencapai `chunk_size` entri), kompaksi terjadi secara otomatis:

```text
Compaction Steps:
─────────────────
1. Read all chunk_size buffer entries

2. Compute dense Merkle root
   - Hash each entry: leaf[i] = blake3(entry[i])
   - Build complete binary tree bottom-up
   - Extract root hash
   Hash cost: chunk_size + (chunk_size - 1) = 2 * chunk_size - 1

3. Serialize entries into chunk blob
   - Auto-selects fixed-size or variable-size format (§14.6)
   - Store as: store.put(chunk_key(chunk_index), blob)

4. Append dense Merkle root to chunk MMR
   - MMR push with merge cascade (see Chapter 13)
   Hash cost: ~2 amortized (trailing_ones pattern)

5. Reset the dense tree (clear all buffer entries from storage)
   - Dense tree count reset to 0
```

Setelah kompaksi, chunk blob **tidak dapat diubah secara permanen** — ia tidak pernah berubah
lagi. Ini membuat chunk blob ideal untuk caching CDN, sinkronisasi klien, dan penyimpanan
arsip.

**Contoh: 4 append dengan chunk_power=2 (chunk_size=4)**

```text
Append v_0: dense_tree=[v_0],       dense_root=H(v_0), total=1
Append v_1: dense_tree=[v_0,v_1],   dense_root=H(v_0,v_1), total=2
Append v_2: dense_tree=[v_0..v_2],  dense_root=H(v_0..v_2), total=3
Append v_3: dense_tree=[v_0..v_3],  dense_root=H(v_0..v_3), total=4
  → COMPACTION:
    chunk_blob_0 = serialize([v_0, v_1, v_2, v_3])
    dense_root_0 = dense_merkle_root(v_0..v_3)
    mmr.push(dense_root_0)
    dense tree cleared (count=0)

Append v_4: dense_tree=[v_4],       dense_root=H(v_4), total=5
  → state_root = blake3("bulk_state" || mmr_root || dense_root)
```

## State Root

State root mengikat kedua level menjadi satu hash:

```rust
fn compute_state_root(
    mmr_root: &[u8; 32],         // Chunk MMR root (or [0;32] if empty)
    dense_tree_root: &[u8; 32],  // Root hash of current buffer (dense tree)
) -> [u8; 32] {
    blake3("bulk_state" || mmr_root || dense_tree_root)
}
```

`total_count` dan `chunk_power` **tidak** disertakan dalam state root karena
mereka sudah diautentikasi oleh Merk value hash — mereka adalah field dari
`Element` yang diserialisasi dan disimpan di node Merk induk. State root hanya menangkap
komitmen level-data (`mmr_root` dan `dense_tree_root`). Ini adalah hash yang
mengalir sebagai Merk child hash dan merambat ke atas menuju root hash GroveDB.

## Dense Merkle Root

Ketika chunk dikompaksi, entri-entri memerlukan satu komitmen 32 byte. BulkAppendTree
menggunakan **dense (lengkap) binary Merkle tree**:

```text
Given entries [e_0, e_1, e_2, e_3]:

Level 0 (leaves):  blake3(e_0)  blake3(e_1)  blake3(e_2)  blake3(e_3)
                      \__________/              \__________/
Level 1:           blake3(h_0 || h_1)       blake3(h_2 || h_3)
                          \____________________/
Level 2 (root):    blake3(h_01 || h_23)  ← this is the dense Merkle root
```

Karena `chunk_size` selalu merupakan pangkat 2 (berdasarkan konstruksi: `1u32 << chunk_power`),
pohon selalu lengkap (tidak perlu padding atau daun dummy). Jumlah hash tepat
`2 * chunk_size - 1`:
- `chunk_size` hash daun (satu per entri)
- `chunk_size - 1` hash node internal

Implementasi dense Merkle root berada di `grovedb-mmr/src/dense_merkle.rs` dan
menyediakan dua fungsi:
- `compute_dense_merkle_root(hashes)` — dari daun yang sudah di-hash
- `compute_dense_merkle_root_from_values(values)` — melakukan hash nilai terlebih dahulu,
  lalu membangun pohon

## Serialisasi Chunk Blob

Chunk blob adalah arsip tidak dapat diubah yang dihasilkan oleh kompaksi. Serializer
secara otomatis memilih format wire paling ringkas berdasarkan ukuran entri:

**Format ukuran tetap** (flag `0x01`) — ketika semua entri memiliki panjang yang sama:

```text
┌──────┬──────────┬─────────────┬─────────┬─────────┬─────────┐
│ 0x01 │ count    │ entry_size  │ entry_0 │ entry_1 │ ...     │
│ 1B   │ 4B (BE)  │ 4B (BE)     │ N bytes │ N bytes │         │
└──────┴──────────┴─────────────┴─────────┴─────────┴─────────┘
Total: 1 + 4 + 4 + (count × entry_size) bytes
```

**Format ukuran variabel** (flag `0x00`) — ketika entri memiliki panjang berbeda:

```text
┌──────┬──────────┬─────────┬──────────┬─────────┬─────────────┐
│ 0x00 │ len_0    │ entry_0 │ len_1    │ entry_1 │ ...         │
│ 1B   │ 4B (BE)  │ N bytes │ 4B (BE)  │ M bytes │             │
└──────┴──────────┴─────────┴──────────┴─────────┴─────────────┘
Total: 1 + Σ(4 + len_i) bytes
```

Format ukuran tetap menghemat 4 byte per entri dibandingkan ukuran variabel, yang bertambah
signifikan untuk chunk besar data berukuran seragam (seperti komitmen hash 32 byte).
Untuk 1024 entri berukuran 32 byte masing-masing:
- Tetap: `1 + 4 + 4 + 32768 = 32.777 byte`
- Variabel: `1 + 1024 × (4 + 32) = 36.865 byte`
- Penghematan: ~11%

## Tata Letak Key Penyimpanan

Semua data BulkAppendTree berada di namespace **data**, di-key dengan prefiks karakter tunggal:

| Pola key | Format | Ukuran | Tujuan |
|---|---|---|---|
| `M` | 1 byte | 1B | Key metadata |
| `b` + `{index}` | `b` + u32 BE | 5B | Entri buffer di indeks |
| `e` + `{index}` | `e` + u64 BE | 9B | Chunk blob di indeks |
| `m` + `{pos}` | `m` + u64 BE | 9B | Node MMR di posisi |

**Metadata** menyimpan `mmr_size` (8 byte BE). `total_count` dan `chunk_power`
disimpan di Element itu sendiri (di Merk induk), bukan di metadata namespace data.
Pemisahan ini berarti membaca count adalah pencarian element sederhana tanpa membuka
konteks penyimpanan data.

Key buffer menggunakan indeks u32 (0 hingga `chunk_size - 1`) karena kapasitas buffer
dibatasi oleh `chunk_size` (sebuah u32, dihitung sebagai `1u32 << chunk_power`). Key chunk
menggunakan indeks u64 karena jumlah chunk yang telah selesai dapat tumbuh tanpa batas.

## Struct BulkAppendTree

```rust
pub struct BulkAppendTree<S> {
    pub total_count: u64,                        // Total values ever appended
    pub dense_tree: DenseFixedSizedMerkleTree<S>, // The buffer (dense tree)
}
```

Buffer ADALAH sebuah `DenseFixedSizedMerkleTree` — root hash-nya adalah `dense_tree_root`.

**Accessor:**
- `capacity() -> u16`: `dense_tree.capacity()` (= `2^height - 1`)
- `epoch_size() -> u64`: `capacity + 1` (= `2^height`, jumlah entri per chunk)
- `height() -> u8`: `dense_tree.height()`

**Nilai turunan** (tidak disimpan):

| Nilai | Rumus |
|---|---|
| `chunk_count` | `total_count / epoch_size` |
| `buffer_count` | `dense_tree.count()` |
| `mmr_size` | `leaf_count_to_mmr_size(chunk_count)` |

## Operasi GroveDB

BulkAppendTree terintegrasi dengan GroveDB melalui enam operasi yang didefinisikan di
`grovedb/src/operations/bulk_append_tree.rs`:

### bulk_append

Operasi mutasi utama. Mengikuti pola penyimpanan non-Merk GroveDB standar:

```text
1. Validate element is BulkAppendTree
2. Open data storage context
3. Load tree from store
4. Append value (may trigger compaction)
5. Update element in parent Merk with new state_root + total_count
6. Propagate changes up through Merk hierarchy
7. Commit transaction
```

Adapter `AuxBulkStore` membungkus panggilan `get_aux`/`put_aux`/`delete_aux` GroveDB dan
mengakumulasi `OperationCost` dalam `RefCell` untuk pelacakan biaya. Biaya hash dari
operasi append ditambahkan ke `cost.hash_node_calls`.

### Operasi baca

| Operasi | Apa yang dikembalikan | Penyimpanan aux? |
|---|---|---|
| `bulk_get_value(path, key, position)` | Nilai di posisi global | Ya — membaca dari chunk blob atau buffer |
| `bulk_get_chunk(path, key, chunk_index)` | Chunk blob mentah | Ya — membaca key chunk |
| `bulk_get_buffer(path, key)` | Semua entri buffer saat ini | Ya — membaca key buffer |
| `bulk_count(path, key)` | Total count (u64) | Tidak — membaca dari element |
| `bulk_chunk_count(path, key)` | Chunk yang telah selesai (u64) | Tidak — dihitung dari element |

Operasi `get_value` secara transparan merutekan berdasarkan posisi:

```text
if position < completed_chunks × chunk_size:
    chunk_idx = position / chunk_size
    intra_idx = position % chunk_size
    → read chunk blob, deserialize, return entry[intra_idx]
else:
    buffer_idx = position % chunk_size
    → read buffer_key(buffer_idx)
```

## Operasi Batch dan Preprocessing

BulkAppendTree mendukung operasi batch melalui varian `GroveOp::BulkAppend`.
Karena `execute_ops_on_path` tidak memiliki akses ke konteks penyimpanan data, semua
operasi BulkAppend harus diproses terlebih dahulu sebelum `apply_body`.

Pipeline preprocessing:

```text
Input: [BulkAppend{v1}, Insert{...}, BulkAppend{v2}, BulkAppend{v3}]
                                     ↑ same (path,key) as v1

Step 1: Group BulkAppend ops by (path, key)
        group_1: [v1, v2, v3]

Step 2: For each group:
        a. Read existing element → get (total_count, chunk_power)
        b. Open transactional storage context
        c. Load BulkAppendTree from store
        d. Load existing buffer into memory (Vec<Vec<u8>>)
        e. For each value:
           tree.append_with_mem_buffer(store, value, &mut mem_buffer)
        f. Save metadata
        g. Compute final state_root

Step 3: Replace all BulkAppend ops with one ReplaceNonMerkTreeRoot per group
        carrying: hash=state_root, meta=BulkAppendTree{total_count, chunk_power}

Output: [ReplaceNonMerkTreeRoot{...}, Insert{...}]
```

Varian `append_with_mem_buffer` menghindari masalah read-after-write: entri buffer
dilacak dalam `Vec<Vec<u8>>` di memori, sehingga kompaksi dapat membacanya meskipun
penyimpanan transaksional belum di-commit.

## Trait BulkStore

```rust
pub trait BulkStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String>;
    fn delete(&self, key: &[u8]) -> Result<(), String>;
}
```

Metode mengambil `&self` (bukan `&mut self`) untuk mencocokkan pola interior mutability
GroveDB di mana penulisan melewati batch. Integrasi GroveDB mengimplementasikan ini melalui
`AuxBulkStore` yang membungkus `StorageContext` dan mengakumulasi `OperationCost`.

`MmrAdapter` menjembatani `BulkStore` ke trait `MMRStoreReadOps`/
`MMRStoreWriteOps` dari ckb MMR, menambahkan cache write-through untuk
kebenaran read-after-write.

## Pembuatan Proof

Proof BulkAppendTree mendukung **query rentang** atas posisi. Struktur proof
menangkap semua yang diperlukan agar verifikator stateless dapat mengonfirmasi bahwa data
spesifik ada dalam pohon:

```rust
pub struct BulkAppendTreeProof {
    pub chunk_power: u8,
    pub total_count: u64,
    pub chunk_blobs: Vec<(u64, Vec<u8>)>,         // Full chunk blobs
    pub chunk_mmr_size: u64,
    pub chunk_mmr_proof_items: Vec<[u8; 32]>,      // MMR sibling hashes
    pub chunk_mmr_leaves: Vec<(u64, [u8; 32])>,    // (leaf_idx, dense_root)
    pub buffer_entries: Vec<Vec<u8>>,               // ALL buffer entries
    pub chunk_mmr_root: [u8; 32],
}
```

**Langkah pembuatan** untuk rentang `[start, end)` (dengan `chunk_size = 1u32 << chunk_power`):

```text
1. Determine overlapping chunks
   first_chunk = start / chunk_size
   last_chunk  = min((end-1) / chunk_size, completed_chunks - 1)

2. Read chunk blobs for overlapping chunks
   For each chunk_idx in [first_chunk, last_chunk]:
     chunk_blobs.push((chunk_idx, store.get(chunk_key(idx))))

3. Compute dense Merkle root for each chunk blob
   For each blob:
     deserialize → values
     dense_root = compute_dense_merkle_root_from_values(values)
     chunk_mmr_leaves.push((chunk_idx, dense_root))

4. Generate MMR proof for those chunk positions
   positions = chunk_indices.map(|idx| leaf_to_pos(idx))
   proof = mmr.gen_proof(positions)
   chunk_mmr_proof_items = proof.proof_items().map(|n| n.hash)

5. Get chunk MMR root

6. Read ALL buffer entries (bounded by chunk_size)
   for i in 0..buffer_count:
     buffer_entries.push(store.get(buffer_key(i)))
```

**Mengapa menyertakan SEMUA entri buffer?** Buffer adalah dense Merkle tree yang root
hash-nya berkomitmen terhadap setiap entri. Verifikator harus membangun ulang pohon dari
semua entri untuk memverifikasi `dense_tree_root`. Karena buffer dibatasi oleh `capacity`
(paling banyak 65.535 entri), ini adalah biaya yang wajar.

## Verifikasi Proof

Verifikasi adalah fungsi murni — tidak memerlukan akses database. Ia melakukan lima pemeriksaan:

```text
Step 0: Metadata consistency
        - chunk_power <= 31
        - buffer_entries.len() == total_count % chunk_size
        - MMR leaf count == completed_chunks

Step 1: Chunk blob integrity
        For each (chunk_idx, blob):
          values = deserialize(blob)
          computed_root = dense_merkle_root(values)
          assert computed_root == chunk_mmr_leaves[chunk_idx]

Step 2: Chunk MMR proof
        Reconstruct MmrNode leaves and proof items
        proof.verify(chunk_mmr_root, leaves) == true

Step 3: Buffer (dense tree) integrity
        Rebuild DenseFixedSizedMerkleTree from buffer_entries
        dense_tree_root = compute root hash of rebuilt tree

Step 4: State root
        computed = blake3("bulk_state" || chunk_mmr_root || dense_tree_root)
        assert computed == expected_state_root
```

Setelah verifikasi berhasil, `BulkAppendTreeProofResult` menyediakan metode
`values_in_range(start, end)` yang mengekstrak nilai spesifik dari chunk blob dan
entri buffer yang telah diverifikasi.

## Bagaimana Ia Terikat ke Root Hash GroveDB

BulkAppendTree adalah **pohon non-Merk** — ia menyimpan data di namespace data,
bukan di subtree Merk anak. Di Merk induk, element disimpan sebagai:

```text
Element::BulkAppendTree(total_count, chunk_power, flags)
```

State root mengalir sebagai Merk child hash. Hash node Merk induk adalah:

```text
combine_hash(value_hash(element_bytes), state_root)
```

`state_root` mengalir sebagai Merk child hash (melalui parameter
`subtree_root_hash` dari `insert_subtree`). Perubahan apa pun pada state root merambat
ke atas melalui hierarki Merk GroveDB menuju root hash.

Dalam proof V1 (bagian 9.6), proof Merk induk membuktikan byte element dan pengikatan
child hash, dan `BulkAppendTreeProof` membuktikan bahwa data yang di-query konsisten
dengan `state_root` yang digunakan sebagai child hash.

## Pelacakan Biaya

Biaya hash setiap operasi dilacak secara eksplisit:

| Operasi | Panggilan Blake3 | Catatan |
|---|---|---|
| Satu append (tanpa kompaksi) | 3 | 2 untuk rantai hash buffer + 1 untuk state root |
| Satu append (dengan kompaksi) | 3 + 2C - 1 + ~2 | Rantai + dense Merkle (C=chunk_size) + MMR push + state root |
| `get_value` dari chunk | 0 | Deserialisasi murni, tanpa hashing |
| `get_value` dari buffer | 0 | Pencarian key langsung |
| Pembuatan proof | Tergantung jumlah chunk | Dense Merkle root per chunk + proof MMR |
| Verifikasi proof | 2C·K - K + B·2 + 1 | K chunk, B entri buffer, C chunk_size |

**Biaya amortisasi per append**: Untuk chunk_size=1024 (chunk_power=10), overhead kompaksi ~2047
hash (dense Merkle root) diamortisasi atas 1024 append, menambahkan ~2 hash per
append. Dikombinasikan dengan 3 hash per-append, total amortisasi adalah **~5 panggilan
blake3 per append** — sangat efisien untuk struktur yang diautentikasi secara kriptografis.

## Perbandingan dengan MmrTree

| | BulkAppendTree | MmrTree |
|---|---|---|
| **Arsitektur** | Dua level (buffer + chunk MMR) | MMR tunggal |
| **Biaya hash per-append** | 3 (+ amortisasi ~2 untuk kompaksi) | ~2 |
| **Granularitas proof** | Query rentang atas posisi | Proof daun individual |
| **Snapshot tidak dapat diubah** | Ya (chunk blob) | Tidak |
| **Ramah CDN** | Ya (chunk blob dapat di-cache) | Tidak |
| **Entri buffer** | Ya (diperlukan semua untuk proof) | N/A |
| **Terbaik untuk** | Log throughput tinggi, sinkronisasi massal | Log event, pencarian individual |
| **Diskriminan Element** | 13 | 12 |
| **TreeType** | 9 | 8 |

Pilih MmrTree ketika Anda memerlukan proof daun individual dengan overhead minimal. Pilih
BulkAppendTree ketika Anda memerlukan query rentang, sinkronisasi massal, dan snapshot
berbasis chunk.

## File Implementasi

| File | Tujuan |
|------|---------|
| `grovedb-bulk-append-tree/src/lib.rs` | Root crate, re-exports |
| `grovedb-bulk-append-tree/src/tree/mod.rs` | Struct `BulkAppendTree`, accessor state, persistensi metadata |
| `grovedb-bulk-append-tree/src/tree/append.rs` | `append()`, `append_with_mem_buffer()`, `compact_entries()` |
| `grovedb-bulk-append-tree/src/tree/hash.rs` | `compute_state_root` |
| `grovedb-bulk-append-tree/src/tree/keys.rs` | `META_KEY`, `buffer_key`, `chunk_key`, `mmr_node_key` |
| `grovedb-bulk-append-tree/src/tree/query.rs` | `get_value`, `get_chunk`, `get_buffer` |
| `grovedb-bulk-append-tree/src/tree/mmr_adapter.rs` | `MmrAdapter` dengan cache write-through |
| `grovedb-bulk-append-tree/src/chunk.rs` | Serialisasi chunk blob (format tetap + variabel) |
| `grovedb-bulk-append-tree/src/proof.rs` | Pembuatan dan verifikasi `BulkAppendTreeProof` |
| `grovedb-bulk-append-tree/src/store.rs` | Trait `BulkStore` |
| `grovedb-bulk-append-tree/src/error.rs` | Enum `BulkAppendError` |
| `grovedb/src/operations/bulk_append_tree.rs` | Operasi GroveDB, `AuxBulkStore`, preprocessing batch |
| `grovedb/src/tests/bulk_append_tree_tests.rs` | 27 tes integrasi |

---
