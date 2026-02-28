# DenseAppendOnlyFixedSizeTree — Penyimpanan Merkle Padat Berkapasitas Tetap

DenseAppendOnlyFixedSizeTree adalah pohon biner lengkap dengan tinggi tetap di mana
**setiap node** — baik internal maupun daun — menyimpan nilai data. Posisi diisi
secara berurutan dalam urutan level-order (BFS): root terlebih dahulu (posisi 0), kemudian
kiri-ke-kanan di setiap level. Tidak ada hash perantara yang disimpan secara persisten;
root hash dihitung ulang secara langsung (on the fly) dengan melakukan hashing rekursif
dari daun ke root.

Desain ini ideal untuk struktur data kecil dan terbatas di mana kapasitas maksimum
diketahui sebelumnya dan Anda membutuhkan O(1) append, O(1) pengambilan berdasarkan posisi,
dan komitmen root hash 32 byte yang ringkas yang berubah setelah setiap penyisipan.

## Struktur Pohon

Pohon dengan tinggi *h* memiliki kapasitas `2^h - 1` posisi. Posisi menggunakan
pengindeksan level-order berbasis 0:

```text
Height 3 tree (capacity = 7):

              pos 0          ← root (level 0)
             /     \
          pos 1    pos 2     ← level 1
         /   \    /   \
       pos 3 pos 4 pos 5 pos 6  ← level 2 (leaves)

Navigation:
  left_child(i)  = 2i + 1
  right_child(i) = 2i + 2
  parent(i)      = (i - 1) / 2
  is_leaf(i)     = 2i + 1 >= capacity
```

Nilai ditambahkan secara berurutan: nilai pertama masuk ke posisi 0 (root), kemudian
posisi 1, 2, 3, dan seterusnya. Ini berarti root selalu memiliki data, dan pohon
terisi dalam urutan level-order — urutan traversal paling natural untuk pohon biner
lengkap.

## Komputasi Hash

Root hash tidak disimpan secara terpisah — ia dihitung ulang dari awal kapan pun
diperlukan. Algoritma rekursif hanya mengunjungi posisi yang terisi:

```text
hash(position, store):
  value = store.get(position)

  if position is unfilled (>= count):
    return [0; 32]                                    ← sentinel kosong

  value_hash = blake3(value)
  left_hash  = hash(2 * position + 1, store)
  right_hash = hash(2 * position + 2, store)
  return blake3(value_hash || left_hash || right_hash)
```

**Properti kunci:**
- Semua node (daun dan internal): `blake3(blake3(value) || H(left) || H(right))`
- Node daun: left_hash dan right_hash keduanya `[0; 32]` (anak tidak terisi)
- Posisi tidak terisi: `[0u8; 32]` (hash nol)
- Pohon kosong (count = 0): `[0u8; 32]`

**Tidak ada tag pemisah domain daun/internal yang digunakan.** Struktur pohon (`height`
dan `count`) diautentikasi secara eksternal di `Element::DenseAppendOnlyFixedSizeTree`
induk, yang mengalir melalui hierarki Merk. Verifikator selalu tahu persis posisi mana
yang merupakan daun vs node internal dari height dan count, sehingga penyerang
tidak dapat menggantikan satu dengan yang lain tanpa merusak rantai autentikasi induk.

Ini berarti root hash mengkodekan komitmen terhadap setiap nilai yang disimpan dan
posisi tepatnya dalam pohon. Mengubah nilai apa pun (jika bisa dimutasi) akan
mengalir melalui semua hash ancestor hingga ke root.

**Biaya hash:** Menghitung root hash mengunjungi semua posisi yang terisi ditambah
anak-anak yang tidak terisi. Untuk pohon dengan *n* nilai, kasus terburuk adalah
O(*n*) panggilan blake3. Ini dapat diterima karena pohon dirancang untuk kapasitas
kecil dan terbatas (tinggi maksimum 16, maksimum 65.535 posisi).

## Varian Element

```rust
Element::DenseAppendOnlyFixedSizeTree(
    u16,                   // count — jumlah nilai yang disimpan (maks 65.535)
    u8,                    // height — tidak dapat diubah setelah pembuatan (1..=16)
    Option<ElementFlags>,  // flags — flag penyimpanan
)
```

| Field | Tipe | Deskripsi |
|---|---|---|
| `count` | `u16` | Jumlah nilai yang telah disisipkan (maks 65.535) |
| `height` | `u8` | Tinggi pohon (1..=16), tidak dapat diubah setelah pembuatan |
| `flags` | `Option<ElementFlags>` | Flag penyimpanan opsional |

Root hash TIDAK disimpan dalam Element — ia mengalir sebagai Merk child hash
melalui parameter `subtree_root_hash` dari `insert_subtree`.

**Diskriminan:** 14 (ElementType), TreeType = 10

**Ukuran biaya:** `DENSE_TREE_COST_SIZE = 6` byte (2 count + 1 height + 1 diskriminan
+ 2 overhead)

## Tata Letak Penyimpanan

Seperti MmrTree dan BulkAppendTree, DenseAppendOnlyFixedSizeTree menyimpan data di
namespace **data** (bukan Merk anak). Nilai di-key berdasarkan posisinya sebagai
big-endian `u64`:

```text
Subtree path: blake3(parent_path || key)

Storage keys:
  [0, 0, 0, 0, 0, 0, 0, 0] → value at position 0 (root)
  [0, 0, 0, 0, 0, 0, 0, 1] → value at position 1
  [0, 0, 0, 0, 0, 0, 0, 2] → value at position 2
  ...
```

Element itu sendiri (disimpan di Merk induk) membawa `count` dan `height`.
Root hash mengalir sebagai Merk child hash. Ini berarti:
- **Membaca root hash** memerlukan penghitungan ulang dari penyimpanan (O(n) hashing)
- **Membaca nilai berdasarkan posisi adalah O(1)** — satu pencarian penyimpanan
- **Menyisipkan adalah O(n) hashing** — satu penulisan penyimpanan + penghitungan ulang root hash penuh

## Operasi

### `dense_tree_insert(path, key, value, tx, grove_version)`

Menambahkan nilai ke posisi berikutnya yang tersedia. Mengembalikan `(root_hash, position)`.

```text
Step 1: Read element, extract (count, height)
Step 2: Check capacity: if count >= 2^height - 1 → error
Step 3: Build subtree path, open storage context
Step 4: Write value to position = count
Step 5: Reconstruct DenseFixedSizedMerkleTree from state
Step 6: Call tree.insert(value, store) → (root_hash, position, hash_calls)
Step 7: Update element with new root_hash and count + 1
Step 8: Propagate changes up through Merk hierarchy
Step 9: Commit transaction
```

### `dense_tree_get(path, key, position, tx, grove_version)`

Mengambil nilai pada posisi tertentu. Mengembalikan `None` jika posisi >= count.

### `dense_tree_root_hash(path, key, tx, grove_version)`

Mengembalikan root hash yang disimpan dalam element. Ini adalah hash yang dihitung
selama penyisipan terbaru — tidak perlu penghitungan ulang.

### `dense_tree_count(path, key, tx, grove_version)`

Mengembalikan jumlah nilai yang disimpan (field `count` dari element).

## Operasi Batch

Varian `GroveOp::DenseTreeInsert` mendukung penyisipan batch melalui pipeline
batch GroveDB standar:

```rust
let ops = vec![
    QualifiedGroveDbOp::dense_tree_insert_op(
        vec![b"parent".to_vec()],
        b"my_dense_tree".to_vec(),
        b"value_data".to_vec(),
    ),
];
db.apply_batch(ops, None, None, grove_version)?;
```

**Preprocessing:** Seperti semua tipe pohon non-Merk, operasi `DenseTreeInsert` diproses
terlebih dahulu sebelum badan batch utama dieksekusi. Metode `preprocess_dense_tree_ops`:

1. Mengelompokkan semua operasi `DenseTreeInsert` berdasarkan `(path, key)`
2. Untuk setiap grup, mengeksekusi penyisipan secara berurutan (membaca element, menyisipkan
   setiap nilai, memperbarui root hash)
3. Mengonversi setiap grup menjadi operasi `ReplaceNonMerkTreeRoot` yang membawa `root_hash`
   dan `count` akhir melalui mesin propagasi standar

Beberapa penyisipan ke dense tree yang sama dalam satu batch didukung — mereka diproses
secara berurutan dan pemeriksaan konsistensi mengizinkan key duplikat untuk tipe operasi ini.

**Propagasi:** Root hash dan count mengalir melalui varian `NonMerkTreeMeta::DenseTree`
dalam `ReplaceNonMerkTreeRoot`, mengikuti pola yang sama seperti MmrTree dan
BulkAppendTree.

## Proof

DenseAppendOnlyFixedSizeTree mendukung **proof subquery V1** melalui varian
`ProofBytes::DenseTree`. Posisi individual dapat dibuktikan terhadap root hash pohon
menggunakan proof inklusi yang membawa nilai ancestor dan hash subtree sibling.

### Struktur Auth Path

Karena node internal melakukan hash terhadap **nilainya sendiri** (bukan hanya hash anak),
authentication path berbeda dari pohon Merkle standar. Untuk memverifikasi daun di posisi
`p`, verifikator memerlukan:

1. **Nilai daun** (entri yang dibuktikan)
2. **Hash nilai ancestor** untuk setiap node internal di jalur dari `p` ke root (hanya hash 32 byte, bukan nilai penuh)
3. **Hash subtree sibling** untuk setiap anak yang TIDAK berada di jalur

Karena semua node menggunakan `blake3(H(value) || H(left) || H(right))` (tanpa tag domain),
proof hanya membawa hash nilai 32 byte untuk ancestor — bukan nilai penuh. Ini menjaga
proof tetap ringkas terlepas dari seberapa besar nilai individual.

```rust
pub struct DenseTreeProof {
    pub entries: Vec<(u16, Vec<u8>)>,            // pasangan (posisi, nilai) yang dibuktikan
    pub node_value_hashes: Vec<(u16, [u8; 32])>, // hash nilai ancestor di auth path
    pub node_hashes: Vec<(u16, [u8; 32])>,       // hash subtree sibling yang sudah dihitung
}
```

> **Catatan:** `height` dan `count` tidak ada dalam struct proof — verifikator mendapatkannya dari Element induk, yang diautentikasi oleh hierarki Merk.

### Contoh Langkah demi Langkah

Pohon dengan height=3, kapasitas=7, count=5, membuktikan posisi 4:

```text
        0
       / \
      1   2
     / \ / \
    3  4 5  6
```

Jalur dari 4 ke root: `4 → 1 → 0`. Set yang diperluas: `{0, 1, 4}`.

Proof berisi:
- **entries**: `[(4, value[4])]` — posisi yang dibuktikan
- **node_value_hashes**: `[(0, H(value[0])), (1, H(value[1]))]` — hash nilai ancestor (masing-masing 32 byte, bukan nilai penuh)
- **node_hashes**: `[(2, H(subtree_2)), (3, H(node_3))]` — sibling yang tidak berada di jalur

Verifikasi menghitung ulang root hash dari bawah ke atas:
1. `H(4) = blake3(blake3(value[4]) || [0;32] || [0;32])` — daun (anak tidak terisi)
2. `H(3)` — dari `node_hashes`
3. `H(1) = blake3(H(value[1]) || H(3) || H(4))` — internal menggunakan hash nilai dari `node_value_hashes`
4. `H(2)` — dari `node_hashes`
5. `H(0) = blake3(H(value[0]) || H(1) || H(2))` — root menggunakan hash nilai dari `node_value_hashes`
6. Bandingkan `H(0)` dengan root hash yang diharapkan

### Proof Multi-Posisi

Ketika membuktikan beberapa posisi, set yang diperluas menggabungkan auth path yang
tumpang tindih. Ancestor yang sama hanya disertakan sekali, membuat proof multi-posisi
lebih ringkas dibandingkan proof posisi tunggal yang independen.

### Keterbatasan V0

Proof V0 tidak dapat turun ke dense tree. Jika query V0 mencocokkan
`DenseAppendOnlyFixedSizeTree` dengan subquery, sistem mengembalikan
`Error::NotSupported` yang mengarahkan pemanggil untuk menggunakan `prove_query_v1`.

### Pengkodean Key Query

Posisi dense tree dikodekan sebagai key query **big-endian u16** (2 byte), tidak seperti
MmrTree dan BulkAppendTree yang menggunakan u64. Semua tipe rentang `QueryItem` standar
didukung.

## Perbandingan dengan Pohon Non-Merk Lainnya

| | DenseTree | MmrTree | BulkAppendTree | CommitmentTree |
|---|---|---|---|---|
| **Diskriminan Element** | 14 | 12 | 13 | 11 |
| **TreeType** | 10 | 8 | 9 | 7 |
| **Kapasitas** | Tetap (`2^h - 1`, maks 65.535) | Tidak terbatas | Tidak terbatas | Tidak terbatas |
| **Model data** | Setiap posisi menyimpan nilai | Hanya daun | Buffer dense tree + chunk | Hanya daun |
| **Hash di Element?** | Tidak (mengalir sebagai child hash) | Tidak (mengalir sebagai child hash) | Tidak (mengalir sebagai child hash) | Tidak (mengalir sebagai child hash) |
| **Biaya insert (hashing)** | O(n) blake3 | O(1) amortisasi | O(1) amortisasi | ~33 Sinsemilla |
| **Ukuran biaya** | 6 byte | 11 byte | 12 byte | 12 byte |
| **Dukungan proof** | V1 (Dense) | V1 (MMR) | V1 (Bulk) | V1 (CommitmentTree) |
| **Terbaik untuk** | Struktur terbatas kecil | Log event | Log throughput tinggi | Komitmen ZK |

**Kapan memilih DenseAppendOnlyFixedSizeTree:**
- Jumlah maksimum entri diketahui pada saat pembuatan
- Anda memerlukan setiap posisi (termasuk node internal) untuk menyimpan data
- Anda menginginkan model data sesederhana mungkin tanpa pertumbuhan tak terbatas
- Penghitungan ulang root hash O(n) dapat diterima (tinggi pohon kecil)

**Kapan TIDAK memilihnya:**
- Anda memerlukan kapasitas tidak terbatas → gunakan MmrTree atau BulkAppendTree
- Anda memerlukan kompatibilitas ZK → gunakan CommitmentTree

## Contoh Penggunaan

```rust
use grovedb::Element;
use grovedb_version::version::GroveVersion;

let grove_version = GroveVersion::latest();

// Buat dense tree dengan tinggi 4 (kapasitas = 15 nilai)
db.insert(
    &[b"state"],
    b"validator_slots",
    Element::empty_dense_tree(4),
    None,
    None,
    grove_version,
)?;

// Tambahkan nilai — posisi terisi 0, 1, 2, ...
let (root_hash, pos) = db.dense_tree_insert(
    &[b"state"],
    b"validator_slots",
    validator_pubkey.to_vec(),
    None,
    grove_version,
)?;
// pos == 0, root_hash = blake3(validator_pubkey)

// Baca kembali berdasarkan posisi
let value = db.dense_tree_get(
    &[b"state"],
    b"validator_slots",
    0,     // posisi
    None,
    grove_version,
)?;
assert_eq!(value, Some(validator_pubkey.to_vec()));

// Query metadata
let count = db.dense_tree_count(&[b"state"], b"validator_slots", None, grove_version)?;
let hash = db.dense_tree_root_hash(&[b"state"], b"validator_slots", None, grove_version)?;
```

## File Implementasi

| File | Isi |
|---|---|
| `grovedb-dense-fixed-sized-merkle-tree/src/lib.rs` | Trait `DenseTreeStore`, struct `DenseFixedSizedMerkleTree`, hash rekursif |
| `grovedb-dense-fixed-sized-merkle-tree/src/proof.rs` | Struct `DenseTreeProof`, `generate()`, `encode_to_vec()`, `decode_from_slice()` |
| `grovedb-dense-fixed-sized-merkle-tree/src/verify.rs` | `DenseTreeProof::verify()` — fungsi murni, tidak memerlukan penyimpanan |
| `grovedb-element/src/element/mod.rs` | `Element::DenseAppendOnlyFixedSizeTree` (diskriminan 14) |
| `grovedb-element/src/element/constructor.rs` | `empty_dense_tree()`, `new_dense_tree()` |
| `merk/src/tree_type/mod.rs` | `TreeType::DenseAppendOnlyFixedSizeTree = 10` |
| `merk/src/tree_type/costs.rs` | `DENSE_TREE_COST_SIZE = 6` |
| `grovedb/src/operations/dense_tree.rs` | Operasi GroveDB, `AuxDenseTreeStore`, preprocessing batch |
| `grovedb/src/operations/proof/generate.rs` | `generate_dense_tree_layer_proof()`, `query_items_to_positions()` |
| `grovedb/src/operations/proof/verify.rs` | `verify_dense_tree_lower_layer()` |
| `grovedb/src/operations/proof/mod.rs` | Varian `ProofBytes::DenseTree` |
| `grovedb/src/batch/estimated_costs/average_case_costs.rs` | Model biaya kasus rata-rata |
| `grovedb/src/batch/estimated_costs/worst_case_costs.rs` | Model biaya kasus terburuk |
| `grovedb/src/tests/dense_tree_tests.rs` | 22 tes integrasi |

---
