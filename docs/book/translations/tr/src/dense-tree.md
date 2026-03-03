# DenseAppendOnlyFixedSizeTree — Yogun Sabit Kapasiteli Merkle Depolamasi

DenseAppendOnlyFixedSizeTree, sabit bir yukseklikte tam bir ikili agactir ve **her dugum** — hem dahili hem de yaprak — bir veri degeri depolar. Konumlar seviye sirasinda (BFS) sirali olarak doldurulur: once kok (konum 0), ardindan her seviyede soldan saga. Ara hash'ler kalici olarak depolanmaz; kok hash'i yapraklardan koke dogru ozyinelemeli olarak hash'lenerek anlik olarak yeniden hesaplanir.

Bu tasarim, maksimum kapasitenin onceden bilindigi ve O(1) ekleme, O(1) konuma gore erisim ve her eklemeden sonra degisen kompakt 32 baytlik kok hash taahhudunun gerektigi kucuk, sinirli veri yapilari icin idealdir.

## Agac Yapisi

Yuksekligi *h* olan bir agacin kapasitesi `2^h - 1` konumdur. Konumlar 0 tabanli seviye sirasinda indeksleme kullanir:

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

Degerler sirali olarak eklenir: ilk deger konum 0'a (kok), ardindan konum 1, 2, 3 ve bu sekilde devam eder. Bu, kokun her zaman veri icerdigi ve agacin seviye sirasinda — tam bir ikili agac icin en dogal gezinme sirasi — doldugu anlamina gelir.

## Hash Hesaplamasi

Kok hash'i ayri olarak depolanmaz — ihtiyac duyuldugunda sifirdan yeniden hesaplanir. Ozyinelemeli algoritma yalnizca dolu konumlari ziyaret eder:

```text
hash(position, store):
  value = store.get(position)

  if position is unfilled (>= count):
    return [0; 32]                                    ← empty sentinel

  value_hash = blake3(value)
  left_hash  = hash(2 * position + 1, store)
  right_hash = hash(2 * position + 2, store)
  return blake3(value_hash || left_hash || right_hash)
```

**Temel ozellikler:**
- Tum dugumlerde (yaprak ve dahili): `blake3(blake3(value) || H(left) || H(right))`
- Yaprak dugumler: left_hash ve right_hash her ikisi de `[0; 32]`'dir (bos alt dugumleri)
- Doldurulmamis konumlar: `[0u8; 32]` (sifir hash)
- Bos agac (count = 0): `[0u8; 32]`

**Yaprak/dahili alan ayirma etiketleri KULLANILMAZ.** Agac yapisi (`height` ve `count`), Merk hiyerarsisi uzerinden akan ust `Element::DenseAppendOnlyFixedSizeTree` icinde harici olarak dogrulanir. Dogrulayici her zaman yukseklik ve sayidan hangi konumlarin yaprak hangi konumlarin dahili dugum oldugunu tam olarak bilir, dolayisiyla bir saldirgan ust dogrulama zincirini bozmadan birini digerinin yerine koyamaz.

Bu, kok hash'inin depolanan her degere ve agactaki tam konumuna bir taahhut kodladigi anlamina gelir. Herhangi bir degerin degistirilmesi (degistirilebilir olsaydi) tum ata hash'lerinde koke kadar kademeli olarak yayilirdi.

**Hash maliyeti:** Kok hash'inin hesaplanmasi tum dolu konumlari ve doldurulmamis alt dugumlerini ziyaret eder. *n* degerli bir agac icin en kotu durum O(*n*) blake3 cagrisidir. Bu kabul edilebilir cunku agac kucuk, sinirli kapasiteler icin tasarlanmistir (maks yukseklik 16, maks 65.535 konum).

## Element Varyanti

```rust
Element::DenseAppendOnlyFixedSizeTree(
    u16,                   // count — number of values stored (max 65,535)
    u8,                    // height — immutable after creation (1..=16)
    Option<ElementFlags>,  // flags — storage flags
)
```

| Alan | Tip | Aciklama |
|---|---|---|
| `count` | `u16` | Simdiye kadar eklenmis deger sayisi (maks 65.535) |
| `height` | `u8` | Agac yuksekligi (1..=16), olusturulduktan sonra degismez |
| `flags` | `Option<ElementFlags>` | Istege bagli depolama bayraklari |

Kok hash'i Element'te DEPOLANMAZ — `insert_subtree`'nin `subtree_root_hash` parametresi araciligiyla Merk alt hash'i (child hash) olarak akar.

**Ayirt edici:** 14 (ElementType), TreeType = 10

**Maliyet boyutu:** `DENSE_TREE_COST_SIZE = 6` bayt (2 count + 1 height + 1 ayirt edici + 2 ek yuk)

## Depolama Duzenlemesi

MmrTree ve BulkAppendTree gibi, DenseAppendOnlyFixedSizeTree verileri **data** ad alaninda depolar (bir alt Merk degil). Degerler, buyuk-endian `u64` olarak konumlariyla anahtarlanir:

```text
Subtree path: blake3(parent_path || key)

Storage keys:
  [0, 0, 0, 0, 0, 0, 0, 0] → value at position 0 (root)
  [0, 0, 0, 0, 0, 0, 0, 1] → value at position 1
  [0, 0, 0, 0, 0, 0, 0, 2] → value at position 2
  ...
```

Element'in kendisi (ust Merk'te depolanir) `count` ve `height` degerlerini tasir. Kok hash'i, Merk alt hash'i olarak akar. Bu su anlama gelir:
- **Kok hash'ini okumak** depolamadan yeniden hesaplama gerektirir (O(n) hash'leme)
- **Konuma gore deger okumak O(1)'dir** — tekli depolama arama
- **Ekleme O(n) hash'lemedir** — bir depolama yazimi + tam kok hash yeniden hesaplamasi

## Islemler

### `dense_tree_insert(path, key, value, tx, grove_version)`

Bir sonraki uygun konuma bir deger ekler. `(root_hash, position)` dondurur.

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

Verilen konumdaki degeri getirir. Konum >= count ise `None` dondurur.

### `dense_tree_root_hash(path, key, tx, grove_version)`

Element'te depolanan kok hash'ini dondurur. Bu, en son ekleme sirasinda hesaplanan hash'tir — yeniden hesaplama gerekmez.

### `dense_tree_count(path, key, tx, grove_version)`

Depolanan deger sayisini dondurur (element'teki `count` alani).

## Toplu Islemler (Batch Operations)

`GroveOp::DenseTreeInsert` varyanti, standart GroveDB toplu hatti araciligiyla toplu eklemeyi destekler:

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

**On islem (Preprocessing):** Tum Merk-disi agac tipleri gibi, `DenseTreeInsert` islemleri ana toplu govde (batch body) calistirilmadan once on islenir. `preprocess_dense_tree_ops` metodu:

1. Tum `DenseTreeInsert` islemlerini `(path, key)` bazinda gruplar
2. Her grup icin, eklemeleri sirali olarak calistirir (elementi okur, her degeri ekler, kok hash'ini gunceller)
3. Her grubu, son `root_hash` ve `count` degerlerini standart yayilim makinesine tasiyan bir `ReplaceNonMerkTreeRoot` islemine donusturur

Tek bir toplu islem icinde ayni yogun agaca birden fazla ekleme desteklenir — sirayla islenir ve tutarlilik kontrolu bu islem tipi icin yinelenen anahtarlara izin verir.

**Yayilim:** Kok hash'i ve sayac `ReplaceNonMerkTreeRoot` icindeki `NonMerkTreeMeta::DenseTree` varyanti uzerinden akar; MmrTree ve BulkAppendTree ile ayni deseni izler.

## Ispatlar (Proofs)

DenseAppendOnlyFixedSizeTree, `ProofBytes::DenseTree` varyanti araciligiyla **V1 alt sorgu ispatlarini** destekler. Bireysel konumlar, ata degerlerini ve kardes alt agac hash'lerini tasiyan dahil etme ispatları (inclusion proof) kullanilarak agacin kok hash'ine karsi ispatlanabilir.

### Dogrulama Yolu Yapisi (Auth Path Structure)

Dahili dugumler kendi **degerlerini** hash'ledigi icin (yalnizca alt hash'leri degil), dogrulama yolu standart bir Merkle agacindan farklidir. `p` konumundaki bir yapragi dogrulamak icin dogrulayicinin ihtiyaci olan:

1. **Yaprak degeri** (ispatlanan girdi)
2. `p`'den koke giden yoldaki her dahili dugum icin **ata deger hash'leri** (tam deger degil, yalnizca 32 baytlik hash)
3. Yolda OLMAYAN her alt dugum icin **kardes alt agac hash'leri**

Tum dugumler `blake3(H(value) || H(left) || H(right))` kullandigi icin (alan ayirma etiketleri yok), ispat yalnizca atalar icin 32 baytlik deger hash'lerini tasir — tam degerleri degil. Bu, bireysel degerlerin ne kadar buyuk olduguna bakilmaksizin ispatları kompakt tutar.

```rust
pub struct DenseTreeProof {
    pub entries: Vec<(u16, Vec<u8>)>,            // proved (position, value) pairs
    pub node_value_hashes: Vec<(u16, [u8; 32])>, // ancestor value hashes on the auth path
    pub node_hashes: Vec<(u16, [u8; 32])>,       // precomputed sibling subtree hashes
}
```

> **Not:** `height` ve `count` ispat yapisinda degildir — dogrulayici bunlari, Merk hiyerarsisi tarafindan dogrulanan ust Element'ten alir.

### Adim Adim Ornek

Yukseklik=3, kapasite=7, sayi=5, konum 4'u ispatlayan agac:

```text
        0
       / \
      1   2
     / \ / \
    3  4 5  6
```

4'ten koke giden yol: `4 → 1 → 0`. Genisletilmis kume: `{0, 1, 4}`.

Ispat icerir:
- **entries**: `[(4, value[4])]` — ispatlanan konum
- **node_value_hashes**: `[(0, H(value[0])), (1, H(value[1]))]` — ata deger hash'leri (her biri 32 bayt, tam deger degil)
- **node_hashes**: `[(2, H(subtree_2)), (3, H(node_3))]` — yolda olmayan kardesler

Dogrulama kok hash'ini asagidan yukariya yeniden hesaplar:
1. `H(4) = blake3(blake3(value[4]) || [0;32] || [0;32])` — yaprak (alt dugumler doldurulmamis)
2. `H(3)` — `node_hashes`'den
3. `H(1) = blake3(H(value[1]) || H(3) || H(4))` — dahili, `node_value_hashes`'den deger hash'i kullanir
4. `H(2)` — `node_hashes`'den
5. `H(0) = blake3(H(value[0]) || H(1) || H(2))` — kok, `node_value_hashes`'den deger hash'i kullanir
6. `H(0)`'i beklenen kok hash ile karsilastir

### Coklu Konum Ispatları

Birden fazla konumu ispatlarken, genisletilmis kume cakisan dogrulama yollarini birlestirir. Paylasilan atalar yalnizca bir kez dahil edilir, bu da coklu konum ispatlarini bagimsiz tekli konum ispatlarindan daha kompakt kilar.

### V0 Sinirlamasi

V0 ispatları yogun agaclara inemez. Bir V0 sorgusu alt sorguyla bir `DenseAppendOnlyFixedSizeTree` ile eslestirdiginde, sistem arayana `prove_query_v1` kullanmasini yonlendiren `Error::NotSupported` dondurur.

### Sorgu Anahtari Kodlamasi

Yogun agac konumlari, u64 kullanan MmrTree ve BulkAppendTree'nin aksine **buyuk-endian u16** (2 bayt) sorgu anahtarlari olarak kodlanir. Tum standart `QueryItem` aralik tipleri desteklenir.

## Diger Merk-Disi Agaclarla Karsilastirma

| | DenseTree | MmrTree | BulkAppendTree | CommitmentTree |
|---|---|---|---|---|
| **Element ayirt edicisi** | 14 | 12 | 13 | 11 |
| **TreeType** | 10 | 8 | 9 | 7 |
| **Kapasite** | Sabit (`2^h - 1`, maks 65.535) | Sinirsiz | Sinirsiz | Sinirsiz |
| **Veri modeli** | Her konum bir deger depolar | Yalnizca yaprak | Yogun agac tamponu + parcalar | Yalnizca yaprak |
| **Element'te hash?** | Hayir (alt hash olarak akar) | Hayir (alt hash olarak akar) | Hayir (alt hash olarak akar) | Hayir (alt hash olarak akar) |
| **Ekleme maliyeti (hash)** | O(n) blake3 | O(1) amortismanlı | O(1) amortismanlı | ~33 Sinsemilla |
| **Maliyet boyutu** | 6 bayt | 11 bayt | 12 bayt | 12 bayt |
| **Ispat destegi** | V1 (Dense) | V1 (MMR) | V1 (Bulk) | V1 (CommitmentTree) |
| **En uygun kullanim** | Kucuk sinirli yapilar | Olay gunlukleri | Yuksek verimli gunlukler | ZK taahhutleri |

**DenseAppendOnlyFixedSizeTree ne zaman secilmeli:**
- Maksimum girdi sayisi olusturma zamaninda bilinir
- Her konumun (dahili dugumler dahil) veri depolamasi gerekir
- Sinirsiz buyume olmadan mumkun olan en basit veri modelini istiyorsunuz
- O(n) kok hash yeniden hesaplamasi kabul edilebilir (kucuk agac yukseklikleri)

**Ne zaman secilmemeli:**
- Sinirsiz kapasiteye ihtiyaciniz var → MmrTree veya BulkAppendTree kullanin
- ZK uyumluluguna ihtiyaciniz var → CommitmentTree kullanin

## Kullanim Ornegi

```rust
use grovedb::Element;
use grovedb_version::version::GroveVersion;

let grove_version = GroveVersion::latest();

// Create a dense tree of height 4 (capacity = 15 values)
db.insert(
    &[b"state"],
    b"validator_slots",
    Element::empty_dense_tree(4),
    None,
    None,
    grove_version,
)?;

// Append values — positions filled 0, 1, 2, ...
let (root_hash, pos) = db.dense_tree_insert(
    &[b"state"],
    b"validator_slots",
    validator_pubkey.to_vec(),
    None,
    grove_version,
)?;
// pos == 0, root_hash = blake3(validator_pubkey)

// Read back by position
let value = db.dense_tree_get(
    &[b"state"],
    b"validator_slots",
    0,     // position
    None,
    grove_version,
)?;
assert_eq!(value, Some(validator_pubkey.to_vec()));

// Query metadata
let count = db.dense_tree_count(&[b"state"], b"validator_slots", None, grove_version)?;
let hash = db.dense_tree_root_hash(&[b"state"], b"validator_slots", None, grove_version)?;
```

## Implementasyon Dosyalari

| Dosya | Icerik |
|---|---|
| `grovedb-dense-fixed-sized-merkle-tree/src/lib.rs` | `DenseTreeStore` arayuzu, `DenseFixedSizedMerkleTree` yapisi, ozyinelemeli hash |
| `grovedb-dense-fixed-sized-merkle-tree/src/proof.rs` | `DenseTreeProof` yapisi, `generate()`, `encode_to_vec()`, `decode_from_slice()` |
| `grovedb-dense-fixed-sized-merkle-tree/src/verify.rs` | `DenseTreeProof::verify()` — saf fonksiyon, depolama gerekmez |
| `grovedb-element/src/element/mod.rs` | `Element::DenseAppendOnlyFixedSizeTree` (ayirt edici 14) |
| `grovedb-element/src/element/constructor.rs` | `empty_dense_tree()`, `new_dense_tree()` |
| `merk/src/tree_type/mod.rs` | `TreeType::DenseAppendOnlyFixedSizeTree = 10` |
| `merk/src/tree_type/costs.rs` | `DENSE_TREE_COST_SIZE = 6` |
| `grovedb/src/operations/dense_tree.rs` | GroveDB islemleri, `AuxDenseTreeStore`, toplu on islem |
| `grovedb/src/operations/proof/generate.rs` | `generate_dense_tree_layer_proof()`, `query_items_to_positions()` |
| `grovedb/src/operations/proof/verify.rs` | `verify_dense_tree_lower_layer()` |
| `grovedb/src/operations/proof/mod.rs` | `ProofBytes::DenseTree` varyanti |
| `grovedb/src/batch/estimated_costs/average_case_costs.rs` | Ortalama durum maliyet modeli |
| `grovedb/src/batch/estimated_costs/worst_case_costs.rs` | En kotu durum maliyet modeli |
| `grovedb/src/tests/dense_tree_tests.rs` | 22 entegrasyon testi |

---
