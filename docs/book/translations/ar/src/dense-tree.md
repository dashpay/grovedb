# شجرة DenseAppendOnlyFixedSizeTree — تخزين ميركل كثيف ثابت السعة

DenseAppendOnlyFixedSizeTree هي شجرة ثنائية كاملة بارتفاع ثابت حيث
**كل عقدة** — داخلية وورقية — تُخزّن قيمة بيانات. تُملأ المواقع
تتابعياً بترتيب المستويات (BFS): الجذر أولاً (الموقع 0)، ثم من اليسار لليمين في كل
مستوى. لا تُحفظ تجزئات وسيطة؛ يُعاد حساب تجزئة الجذر أثناء التشغيل بالتجزئة
التكرارية من الأوراق إلى الجذر.

هذا التصميم مثالي لبنى بيانات صغيرة محدودة حيث السعة القصوى
معروفة مسبقاً وتحتاج إلحاق O(1) واسترجاع O(1) بالموقع والتزام
تجزئة جذر مدمج من 32 بايت يتغير بعد كل إدراج.

## بنية الشجرة

شجرة بارتفاع *h* سعتها `2^h - 1` موقعاً. المواقع تستخدم فهرسة ترتيب المستويات
من 0:

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

تُلحق القيم تتابعياً: القيمة الأولى تذهب للموقع 0 (الجذر)، ثم
الموقع 1، 2، 3، وهكذا.

## حساب التجزئة

تجزئة الجذر لا تُخزَّن منفصلة — يُعاد حسابها من الصفر عند الحاجة:

```text
hash(position, store):
  value = store.get(position)

  if position is unfilled (>= count):
    return [0; 32]

  value_hash = blake3(value)
  left_hash  = hash(2 * position + 1, store)
  right_hash = hash(2 * position + 2, store)
  return blake3(value_hash || left_hash || right_hash)
```

**الخصائص الأساسية:**
- جميع العقد (ورقية وداخلية): `blake3(blake3(value) || H(left) || H(right))`
- العقد الورقية: left_hash وright_hash كلاهما `[0; 32]` (أبناء غير ممتلئين)
- المواقع غير الممتلئة: `[0u8; 32]` (تجزئة صفرية)
- الشجرة الفارغة (count = 0): `[0u8; 32]`

## متغير العنصر

```rust
Element::DenseAppendOnlyFixedSizeTree(
    u16,                   // count — number of values stored (max 65,535)
    u8,                    // height — immutable after creation (1..=16)
    Option<ElementFlags>,  // flags — storage flags
)
```

تجزئة الجذر لا تُخزَّن في العنصر — تتدفق كتجزئة Merk الابن
عبر معامل `subtree_root_hash` في `insert_subtree`.

**المُميِّز:** 14 (ElementType)، TreeType = 10

## تخطيط التخزين

مثل MmrTree وBulkAppendTree، تُخزّن DenseAppendOnlyFixedSizeTree البيانات في
فضاء اسم **البيانات**. القيم مُفتَّحة بموقعها كـ `u64` بترتيب الطرف الأكبر:

```text
Storage keys:
  [0, 0, 0, 0, 0, 0, 0, 0] → value at position 0 (root)
  [0, 0, 0, 0, 0, 0, 0, 1] → value at position 1
  [0, 0, 0, 0, 0, 0, 0, 2] → value at position 2
  ...
```

## العمليات

### `dense_tree_insert(path, key, value, tx, grove_version)`

تُلحق قيمة بالموقع المتاح التالي. تُرجع `(root_hash, position)`.

### `dense_tree_get(path, key, position, tx, grove_version)`

تسترجع القيمة في موقع معين. تُرجع `None` إذا كان الموقع >= العدد.

### `dense_tree_root_hash(path, key, tx, grove_version)`

تُرجع تجزئة الجذر المُخزَّنة في العنصر.

### `dense_tree_count(path, key, tx, grove_version)`

تُرجع عدد القيم المُخزَّنة (حقل `count` من العنصر).

## العمليات الدفعية

متغير `GroveOp::DenseTreeInsert` يدعم الإدراج الدفعي عبر خط أنابيب
الدفعة القياسي في GroveDB. **المعالجة المسبقة** تعمل كجميع أنواع الأشجار غير-Merk.

## البراهين

تدعم DenseAppendOnlyFixedSizeTree **براهين استعلامات فرعية V1** عبر متغير `ProofBytes::DenseTree`.

### بنية مسار التوثيق

```rust
pub struct DenseTreeProof {
    pub entries: Vec<(u16, Vec<u8>)>,            // proved (position, value) pairs
    pub node_value_hashes: Vec<(u16, [u8; 32])>, // ancestor value hashes on the auth path
    pub node_hashes: Vec<(u16, [u8; 32])>,       // precomputed sibling subtree hashes
}
```

### مثال تفصيلي

شجرة بارتفاع=3، سعة=7، عدد=5، إثبات الموقع 4:

```text
        0
       / \
      1   2
     / \ / \
    3  4 5  6
```

المسار من 4 إلى الجذر: `4 → 1 → 0`.

البرهان يحتوي:
- **entries**: `[(4, value[4])]` — الموقع المُثبت
- **node_value_hashes**: `[(0, H(value[0])), (1, H(value[1]))]` — تجزئات قيم الأسلاف
- **node_hashes**: `[(2, H(subtree_2)), (3, H(node_3))]` — الأشقاء خارج المسار

التحقق يُعيد حساب تجزئة الجذر من الأسفل للأعلى:
1. `H(4) = blake3(blake3(value[4]) || [0;32] || [0;32])` — ورقة
2. `H(3)` — من `node_hashes`
3. `H(1) = blake3(H(value[1]) || H(3) || H(4))` — داخلي
4. `H(2)` — من `node_hashes`
5. `H(0) = blake3(H(value[0]) || H(1) || H(2))` — الجذر
6. مقارنة `H(0)` مع تجزئة الجذر المتوقعة

## مقارنة مع الأشجار الأخرى غير-Merk

| | DenseTree | MmrTree | BulkAppendTree | CommitmentTree |
|---|---|---|---|---|
| **السعة** | ثابتة (`2^h - 1`، حد أقصى 65,535) | غير محدودة | غير محدودة | غير محدودة |
| **نموذج البيانات** | كل موقع يُخزّن قيمة | أوراق فقط | مخزن مؤقت + شرائح | أوراق فقط |
| **تكلفة الإدراج (تجزئة)** | O(n) blake3 | O(1) مُطفأة | O(1) مُطفأة | ~33 Sinsemilla |
| **حجم التكلفة** | 6 بايت | 11 بايت | 12 بايت | 12 بايت |
| **الأفضل لـ** | بنى صغيرة محدودة | سجلات أحداث | سجلات عالية الإنتاجية | التزامات ZK |

**متى تختار DenseAppendOnlyFixedSizeTree:**
- العدد الأقصى للمدخلات معروف وقت الإنشاء
- تحتاج كل موقع (بما في ذلك العقد الداخلية) لتخزين بيانات
- تريد أبسط نموذج بيانات ممكن بدون نمو غير محدود
- إعادة حساب تجزئة الجذر بتعقيد O(n) مقبولة (ارتفاعات شجرة صغيرة)

## مثال استخدام

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

// Read back by position
let value = db.dense_tree_get(
    &[b"state"],
    b"validator_slots",
    0,
    None,
    grove_version,
)?;
assert_eq!(value, Some(validator_pubkey.to_vec()));
```

## ملفات التنفيذ

| الملف | المحتويات |
|-------|-----------|
| `grovedb-dense-fixed-sized-merkle-tree/src/lib.rs` | سمة `DenseTreeStore`، بنية `DenseFixedSizedMerkleTree`، التجزئة التكرارية |
| `grovedb-dense-fixed-sized-merkle-tree/src/proof.rs` | بنية `DenseTreeProof`، `generate()`، `encode_to_vec()` |
| `grovedb-dense-fixed-sized-merkle-tree/src/verify.rs` | `DenseTreeProof::verify()` — دالة صافية |
| `grovedb/src/operations/dense_tree.rs` | عمليات GroveDB، معالجة الدفعات المسبقة |
| `grovedb/src/tests/dense_tree_tests.rs` | 22 اختبار تكامل |

---
