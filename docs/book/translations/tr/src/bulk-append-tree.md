# BulkAppendTree — Yuksek Verimli Yalnizca Ekleme Depolamasi

BulkAppendTree, GroveDB'nin belirli bir muhendislik sorununa cevaptir: verimli aralik ispatlarini (range proof) destekleyen, yazma basina hash maliyetini en aza indiren ve CDN dagitimina uygun degismez parca goruntulerini (chunk snapshot) ureten yuksek verimli bir yalnizca ekleme gunlugu (append-only log) nasil insa edilir?

Bir MmrTree (Bolum 13) bireysel yaprak ispatları icin ideal olsa da, BulkAppendTree her blokta binlerce degerin geldigini ve istemcilerin veri araliklari getirerek senkronize olması gerektigi is yukleri icin tasarlanmistir. Bunu **iki seviyeli bir mimariyle** basarir: gelen eklemeleri emen yogun bir Merkle agac tamponu (dense Merkle tree buffer) ve tamamlanmis parca koklerini (chunk root) kaydeden bir parca seviyesinde MMR.

## Iki Seviyeli Mimari

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

**Seviye 1 — Tampon.** Gelen degerler bir `DenseFixedSizedMerkleTree`'ye (bkz. Bolum 16) yazilir. Tampon kapasitesi `2^height - 1` konumdur. Yogun agacin kok hash'i (`dense_tree_root`) her eklemeden sonra guncellenir.

**Seviye 2 — Parca MMR'si (Chunk MMR).** Tampon dolduğunda (`chunk_size` girdiye ulastiginda), tum girdiler degismez bir **parca blobu'na** (chunk blob) seriestirilir, bu girdiler uzerinde yogun bir Merkle koku hesaplanir ve bu kok, parca MMR'sine yaprak olarak eklenir. Ardindan tampon temizlenir.

**Durum koku** (state root) her iki seviyeyi tek bir 32 baytlik taahhude (commitment) birlestirir ve her eklemede degisir, boylece ust Merk agaci her zaman en son durumu yansitir.

## Degerler Tamponu Nasil Doldurur

Her `append()` cagrisi su siralamayi izler:

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

**Tampon bir DenseFixedSizedMerkleTree'DİR** (bkz. Bolum 16). Kok hash'i her eklemeden sonra degisir ve tum mevcut tampon girdilerine bir taahhut saglar. Bu kok hash, durum koku hesaplamasina akan degerdir.

## Parca Sikistirmasi (Chunk Compaction)

Tampon dolduğunda (`chunk_size` girdiye ulastiginda), sikistirma otomatik olarak tetiklenir:

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

Sikistirmadan sonra, parca blobu **kalici olarak degismezdir** — bir daha asla degismez. Bu, parca bloblarini CDN onbellekleme, istemci senkronizasyonu ve arsiv depolama icin ideal kilar.

**Ornek: chunk_power=2 (chunk_size=4) ile 4 ekleme**

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

## Durum Koku (State Root)

Durum koku her iki seviyeyi tek bir hash'e baglar:

```rust
fn compute_state_root(
    mmr_root: &[u8; 32],         // Chunk MMR root (or [0;32] if empty)
    dense_tree_root: &[u8; 32],  // Root hash of current buffer (dense tree)
) -> [u8; 32] {
    blake3("bulk_state" || mmr_root || dense_tree_root)
}
```

`total_count` ve `chunk_power` durum kokune dahil **edilmez** cunku bunlar zaten Merk deger hash'i (value hash) tarafindan dogrulanir — bunlar ust Merk dugumunde depolanan serilestilmis `Element`'in alanlaridir. Durum koku yalnizca veri seviyesindeki taahhutleri (`mmr_root` ve `dense_tree_root`) yakalar. Bu, Merk alt hash'i (child hash) olarak akan ve GroveDB kok hash'ine kadar yukari yayilan hash'tir.

## Yogun Merkle Koku (Dense Merkle Root)

Bir parca sikistirildiginda, girdiler tek bir 32 baytlik taahhude ihtiyac duyar. BulkAppendTree, bir **yogun (tam) ikili Merkle agaci** kullanir:

```text
Given entries [e_0, e_1, e_2, e_3]:

Level 0 (leaves):  blake3(e_0)  blake3(e_1)  blake3(e_2)  blake3(e_3)
                      \__________/              \__________/
Level 1:           blake3(h_0 || h_1)       blake3(h_2 || h_3)
                          \____________________/
Level 2 (root):    blake3(h_01 || h_23)  ← this is the dense Merkle root
```

`chunk_size` her zaman 2'nin kuvveti oldugu icin (yapim geregi: `1u32 << chunk_power`), agac her zaman tamdır (dolgulama veya sahte yapraklara gerek yoktur). Hash sayisi tam olarak `2 * chunk_size - 1`'dir:
- `chunk_size` yaprak hash'i (girdi basina bir tane)
- `chunk_size - 1` dahili dugum hash'i

Yogun Merkle koku implementasyonu `grovedb-mmr/src/dense_merkle.rs` icinde bulunur ve iki fonksiyon saglar:
- `compute_dense_merkle_root(hashes)` — onceden hash'lenmis yapraklardan
- `compute_dense_merkle_root_from_values(values)` — once degerleri hash'ler, sonra agaci olusturur

## Parca Blobu Serilestirmesi (Chunk Blob Serialization)

Parca bloblari, sikistirma tarafindan uretilen degismez arsivlerdir. Serilestirici, girdi boyutlarina gore en kompakt tel formatini otomatik olarak secer:

**Sabit boyutlu format** (bayrak `0x01`) — tum girdiler ayni uzunluktayken:

```text
┌──────┬──────────┬─────────────┬─────────┬─────────┬─────────┐
│ 0x01 │ count    │ entry_size  │ entry_0 │ entry_1 │ ...     │
│ 1B   │ 4B (BE)  │ 4B (BE)     │ N bytes │ N bytes │         │
└──────┴──────────┴─────────────┴─────────┴─────────┴─────────┘
Total: 1 + 4 + 4 + (count × entry_size) bytes
```

**Degisken boyutlu format** (bayrak `0x00`) — girdiler farkli uzunluklardayken:

```text
┌──────┬──────────┬─────────┬──────────┬─────────┬─────────────┐
│ 0x00 │ len_0    │ entry_0 │ len_1    │ entry_1 │ ...         │
│ 1B   │ 4B (BE)  │ N bytes │ 4B (BE)  │ M bytes │             │
└──────┴──────────┴─────────┴──────────┴─────────┴─────────────┘
Total: 1 + Σ(4 + len_i) bytes
```

Sabit boyutlu format, degisken boyutluya kiyasla girdi basina 4 bayt tasarruf saglar ve bu, buyuk parcalardaki tek tip boyutlu veriler (ornegin 32 baytlik hash taahhutleri) icin onemli bir fark yaratir.
32 baytlik 1024 girdi icin:
- Sabit: `1 + 4 + 4 + 32768 = 32.777 bayt`
- Degisken: `1 + 1024 × (4 + 32) = 36.865 bayt`
- Tasarruf: ~%11

## Depolama Anahtar Duzenlemesi (Storage Key Layout)

Tum BulkAppendTree verileri **data** ad alaninda bulunur ve tek karakterli oneklerle anahtarlanir:

| Anahtar deseni | Format | Boyut | Amac |
|---|---|---|---|
| `M` | 1 bayt | 1B | Meta veri anahtari |
| `b` + `{index}` | `b` + u32 BE | 5B | Indeks konumundaki tampon girdisi |
| `e` + `{index}` | `e` + u64 BE | 9B | Indeks konumundaki parca blobu |
| `m` + `{pos}` | `m` + u64 BE | 9B | Konumdaki MMR dugumu |

**Meta veri** `mmr_size` (8 bayt BE) depolar. `total_count` ve `chunk_power` verinin kendisinde degil, Element'in icinde (ust Merk'te) depolanir. Bu ayrim, sayiyi okumayi veri depolama baglamini acmadan basit bir element aramasi haline getirir.

Tampon anahtarlari u32 indeksleri kullanir (0'dan `chunk_size - 1`'e) cunku tampon kapasitesi `chunk_size` ile sinirlidir (bir u32, `1u32 << chunk_power` olarak hesaplanir). Parca anahtarlari u64 indeksleri kullanir cunku tamamlanmis parca sayisi sinirsiz buyuyebilir.

## BulkAppendTree Yapisi

```rust
pub struct BulkAppendTree<S> {
    pub total_count: u64,                        // Total values ever appended
    pub dense_tree: DenseFixedSizedMerkleTree<S>, // The buffer (dense tree)
}
```

Tampon bir `DenseFixedSizedMerkleTree`'DIR — kok hash'i `dense_tree_root`'tur.

**Erisimciler (Accessors):**
- `capacity() -> u16`: `dense_tree.capacity()` (= `2^height - 1`)
- `epoch_size() -> u64`: `capacity + 1` (= `2^height`, parca basina girdi sayisi)
- `height() -> u8`: `dense_tree.height()`

**Turetilmis degerler** (depolanmaz):

| Deger | Formul |
|---|---|
| `chunk_count` | `total_count / epoch_size` |
| `buffer_count` | `dense_tree.count()` |
| `mmr_size` | `leaf_count_to_mmr_size(chunk_count)` |

## GroveDB Islemleri

BulkAppendTree, `grovedb/src/operations/bulk_append_tree.rs` icinde tanimlanmis alti islem araciligiyla GroveDB ile entegre olur:

### bulk_append

Birincil degistirici islem. Standart GroveDB Merk-disi depolama desenini izler:

```text
1. Validate element is BulkAppendTree
2. Open data storage context
3. Load tree from store
4. Append value (may trigger compaction)
5. Update element in parent Merk with new state_root + total_count
6. Propagate changes up through Merk hierarchy
7. Commit transaction
```

`AuxBulkStore` adaptoru GroveDB'nin `get_aux`/`put_aux`/`delete_aux` cagrilarini sarar ve maliyet takibi icin bir `RefCell` icinde `OperationCost` biriktirir. Ekleme isleminden gelen hash maliyetleri `cost.hash_node_calls`'a eklenir.

### Okuma islemleri

| Islem | Ne dondurur | Aux depolama? |
|---|---|---|
| `bulk_get_value(path, key, position)` | Global konumdaki deger | Evet — parca blobundan veya tampondan okur |
| `bulk_get_chunk(path, key, chunk_index)` | Ham parca blobu | Evet — parca anahtarini okur |
| `bulk_get_buffer(path, key)` | Tum mevcut tampon girdileri | Evet — tampon anahtarlarini okur |
| `bulk_count(path, key)` | Toplam sayi (u64) | Hayir — elementten okur |
| `bulk_chunk_count(path, key)` | Tamamlanmis parcalar (u64) | Hayir — elementten hesaplanir |

`get_value` islemi konuma gore seffaf bir sekilde yonlendirir:

```text
if position < completed_chunks × chunk_size:
    chunk_idx = position / chunk_size
    intra_idx = position % chunk_size
    → read chunk blob, deserialize, return entry[intra_idx]
else:
    buffer_idx = position % chunk_size
    → read buffer_key(buffer_idx)
```

## Toplu Islemler ve On Islem (Batch Operations and Preprocessing)

BulkAppendTree, `GroveOp::BulkAppend` varyanti araciligiyla toplu islemleri destekler. `execute_ops_on_path` veri depolama baglamina erisemedigi icin, tum BulkAppend islemleri `apply_body`'den once on islenmeli (preprocess) dir.

On islem hatti (preprocessing pipeline):

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

`append_with_mem_buffer` varyanti yazdiktan sonra okuma (read-after-write) sorunlarini onler: tampon girdileri bellekte bir `Vec<Vec<u8>>` icinde izlenir, boylece sikistirma, islemsel depolama henuz commit edilmemis olsa bile bunlari okuyabilir.

## BulkStore Arayuzu (Trait)

```rust
pub trait BulkStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String>;
    fn delete(&self, key: &[u8]) -> Result<(), String>;
}
```

Metodlar `&self` alir (`&mut self` degil) cunku GroveDB'nin ic degistirilebilirlik (interior mutability) desenine uyar; burada yazmalar bir batch uzerinden gider. GroveDB entegrasyonu bunu, bir `StorageContext`'i saran ve `OperationCost` biriktiren `AuxBulkStore` araciligiyla uygular.

`MmrAdapter`, `BulkStore`'u ckb MMR'sinin `MMRStoreReadOps`/`MMRStoreWriteOps` arayuzlerine kopruler ve yazdiktan sonra okuma dogrulugu icin bir yazma yoluyla onbellek (write-through cache) ekler.

## Ispat Olusturma (Proof Generation)

BulkAppendTree ispatları konumlar uzerinde **aralik sorgularini** (range query) destekler. Ispat yapisi, belirli verilerin agacta oldugunu dogrulamak icin durumsuz bir dogrulayicinin (stateless verifier) ihtiyac duydugu her seyi yakalar:

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

**Olusturma adimlari** `[start, end)` araligi icin (`chunk_size = 1u32 << chunk_power` ile):

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

**Neden TUM tampon girdileri dahil ediliyor?** Tampon, kok hash'i her girdiye taahhut veren bir yogun Merkle agacidir. Dogrulayici, `dense_tree_root`'u dogrulamak icin tum girdilerden agaci yeniden olusturmalidir. Tampon `capacity` ile sinirli oldugu icin (en fazla 65.535 girdi), bu makul bir maliyettir.

## Ispat Dogrulama (Proof Verification)

Dogrulama saf bir fonksiyondur — veritabani erisimi gerekmez. Bes kontrol gerceklestirir:

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

Dogrulama basarili olduktan sonra, `BulkAppendTreeProofResult` dogrulanmis parca bloblarindan ve tampon girdilerinden belirli degerleri cikartan bir `values_in_range(start, end)` metodu saglar.

## GroveDB Kok Hash'ine Nasil Baglanir

BulkAppendTree bir **Merk-disi agactir** — verileri bir alt Merk alt agacinda degil, veri ad alaninda depolar. Ust Merk'te element su sekilde depolanir:

```text
Element::BulkAppendTree(total_count, chunk_power, flags)
```

Durum koku, Merk alt hash'i (child hash) olarak akar. Ust Merk dugum hash'i su sekildedir:

```text
combine_hash(value_hash(element_bytes), state_root)
```

`state_root`, Merk alt hash'i olarak akar (`insert_subtree`'nin `subtree_root_hash` parametresi araciligiyla). Durum kokundeki herhangi bir degisiklik, GroveDB Merk hiyerarsisi boyunca kok hash'e kadar yukari yayilir.

V1 ispatlarinda (§9.6), ust Merk ispati element baytlarini ve alt hash baglamini ispatlar ve `BulkAppendTreeProof` sorgulanan verilerin alt hash olarak kullanilan `state_root` ile tutarli oldugunu ispatlar.

## Maliyet Takibi (Cost Tracking)

Her islemin hash maliyeti acikca izlenir:

| Islem | Blake3 cagirilari | Notlar |
|---|---|---|
| Tek ekleme (sikistirmasiz) | 3 | Tampon hash zinciri icin 2 + durum koku icin 1 |
| Tek ekleme (sikistirmali) | 3 + 2C - 1 + ~2 | Zincir + yogun Merkle (C=chunk_size) + MMR push + durum koku |
| Parcadan `get_value` | 0 | Saf seriden cikartma, hash yok |
| Tampondan `get_value` | 0 | Dogrudan anahtar arama |
| Ispat olusturma | Parca sayisina baglidir | Parca basina yogun Merkle koku + MMR ispati |
| Ispat dogrulama | 2C·K - K + B·2 + 1 | K parca, B tampon girdisi, C chunk_size |

**Ekleme basina amortisman maliyeti**: chunk_size=1024 (chunk_power=10) icin, ~2047 hash'lik sikistirma yuku 1024 ekleme uzerine amortismana tabi tutulur ve ekleme basina ~2 hash ekler. Ekleme basina 3 hash ile birlestirildiginde, amortisman toplami **ekleme basina ~5 blake3 cagrisi**dir — kriptografik olarak dogrulanmis bir yapi icin oldukca verimlidir.

## MmrTree ile Karsilastirma

| | BulkAppendTree | MmrTree |
|---|---|---|
| **Mimari** | Iki seviyeli (tampon + parca MMR) | Tekli MMR |
| **Ekleme basina hash maliyeti** | 3 (+ amortismanlı ~2 sikistirma icin) | ~2 |
| **Ispat ayrintisi** | Konumlar uzerinde aralik sorgulari | Bireysel yaprak ispatları |
| **Degismez goruntuler** | Evet (parca bloblari) | Hayir |
| **CDN dostu** | Evet (parca bloblari onbeleklenebilir) | Hayir |
| **Tampon girdileri** | Evet (ispat icin tumu gerekli) | Uygulanmaz |
| **En uygun kullanim** | Yuksek verimli gunlukler, toplu senkronizasyon | Olay gunlukleri, bireysel aramalar |
| **Element ayirt edicisi** | 13 | 12 |
| **TreeType** | 9 | 8 |

Minimum yuk ile bireysel yaprak ispatlaarina ihtiyaciniz oldugunda MmrTree'yi secin. Aralik sorgulari, toplu senkronizasyon ve parca tabanli goruntuler ihtiyaciniz oldugunda BulkAppendTree'yi secin.

## Implementasyon Dosyalari

| Dosya | Amac |
|------|---------|
| `grovedb-bulk-append-tree/src/lib.rs` | Crate koku, yeniden ihraclar |
| `grovedb-bulk-append-tree/src/tree/mod.rs` | `BulkAppendTree` yapisi, durum erisimcileri, meta veri kaliciligi |
| `grovedb-bulk-append-tree/src/tree/append.rs` | `append()`, `append_with_mem_buffer()`, `compact_entries()` |
| `grovedb-bulk-append-tree/src/tree/hash.rs` | `compute_state_root` |
| `grovedb-bulk-append-tree/src/tree/keys.rs` | `META_KEY`, `buffer_key`, `chunk_key`, `mmr_node_key` |
| `grovedb-bulk-append-tree/src/tree/query.rs` | `get_value`, `get_chunk`, `get_buffer` |
| `grovedb-bulk-append-tree/src/tree/mmr_adapter.rs` | Yazma yoluyla onbellek ile `MmrAdapter` |
| `grovedb-bulk-append-tree/src/chunk.rs` | Parca blobu serilestirmesi (sabit + degisken formatlar) |
| `grovedb-bulk-append-tree/src/proof.rs` | `BulkAppendTreeProof` olusturma ve dogrulama |
| `grovedb-bulk-append-tree/src/store.rs` | `BulkStore` arayuzu |
| `grovedb-bulk-append-tree/src/error.rs` | `BulkAppendError` enum'u |
| `grovedb/src/operations/bulk_append_tree.rs` | GroveDB islemleri, `AuxBulkStore`, toplu on islem |
| `grovedb/src/tests/bulk_append_tree_tests.rs` | 27 entegrasyon testi |

---
