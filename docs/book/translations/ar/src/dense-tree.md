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
الموقع 1، 2، 3، وهكذا. هذا يعني أن الجذر يحتوي دائماً على بيانات، والشجرة تُملأ
بترتيب المستويات — وهو أكثر ترتيب عبور طبيعي لشجرة ثنائية كاملة.

## حساب التجزئة

تجزئة الجذر لا تُخزَّن منفصلة — يُعاد حسابها من الصفر عند الحاجة.
الخوارزمية التكرارية تزور فقط المواقع الممتلئة:

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

**لا تُستخدم علامات فصل نطاق بين الأوراق والعقد الداخلية.** بنية الشجرة (`height`
و`count`) مُوثّقة خارجياً في العنصر الأب `Element::DenseAppendOnlyFixedSizeTree`،
الذي يتدفق عبر تسلسل Merk الهرمي. المُحقِّق يعرف دائماً بالضبط أي
المواقع أوراق وأيها عقد داخلية من الارتفاع والعدد، لذا لا يستطيع المهاجم
استبدال أحدها بالآخر دون كسر سلسلة التوثيق الأصلية.

هذا يعني أن تجزئة الجذر تُشفّر التزاماً بكل قيمة مُخزَّنة وموقعها الدقيق
في الشجرة. تغيير أي قيمة (لو كانت قابلة للتعديل) سيتتالى عبر
جميع تجزئات الأسلاف صعوداً إلى الجذر.

**تكلفة التجزئة:** حساب تجزئة الجذر يزور جميع المواقع الممتلئة بالإضافة إلى أي أبناء
غير ممتلئين. لشجرة بها *n* قيمة، أسوأ حالة هي O(*n*) استدعاءات blake3. هذا
مقبول لأن الشجرة مُصمّمة لسعات صغيرة محدودة (ارتفاع أقصى 16،
أقصى 65,535 موقعاً).

## متغير العنصر

```rust
Element::DenseAppendOnlyFixedSizeTree(
    u16,                   // count — number of values stored (max 65,535)
    u8,                    // height — immutable after creation (1..=16)
    Option<ElementFlags>,  // flags — storage flags
)
```

| Field | Type | Description |
|---|---|---|
| `count` | `u16` | Number of values inserted so far (max 65,535) |
| `height` | `u8` | Tree height (1..=16), immutable after creation |
| `flags` | `Option<ElementFlags>` | Optional storage flags |

تجزئة الجذر لا تُخزَّن في العنصر — تتدفق كتجزئة Merk الابن
عبر معامل `subtree_root_hash` في `insert_subtree`.

**المُميِّز:** 14 (ElementType)، TreeType = 10

**حجم التكلفة:** `DENSE_TREE_COST_SIZE = 6` بايت (2 عدد + 1 ارتفاع + 1 مُميِّز
+ 2 حِمل إضافي)

## تخطيط التخزين

مثل MmrTree وBulkAppendTree، تُخزّن DenseAppendOnlyFixedSizeTree البيانات في
فضاء اسم **البيانات**. القيم مُفتَّحة بموقعها كـ `u64` بترتيب الطرف الأكبر:

```text
Subtree path: blake3(parent_path || key)

Storage keys:
  [0, 0, 0, 0, 0, 0, 0, 0] → value at position 0 (root)
  [0, 0, 0, 0, 0, 0, 0, 1] → value at position 1
  [0, 0, 0, 0, 0, 0, 0, 2] → value at position 2
  ...
```

العنصر نفسه (المُخزَّن في Merk الأب) يحمل `count` و`height`.
تجزئة الجذر تتدفق كتجزئة Merk الابن. هذا يعني:
- **قراءة تجزئة الجذر** تتطلب إعادة حساب من التخزين (تجزئة O(n))
- **قراءة قيمة بالموقع هي O(1)** — بحث تخزين واحد
- **الإدراج هو تجزئة O(n)** — كتابة تخزين واحدة + إعادة حساب تجزئة الجذر بالكامل

## العمليات

### `dense_tree_insert(path, key, value, tx, grove_version)`

تُلحق قيمة بالموقع المتاح التالي. تُرجع `(root_hash, position)`.

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

تسترجع القيمة في موقع معين. تُرجع `None` إذا كان الموقع >= العدد.

### `dense_tree_root_hash(path, key, tx, grove_version)`

تُرجع تجزئة الجذر المُخزَّنة في العنصر. هذه هي التجزئة المحسوبة أثناء
آخر إدراج — لا حاجة لإعادة الحساب.

### `dense_tree_count(path, key, tx, grove_version)`

تُرجع عدد القيم المُخزَّنة (حقل `count` من العنصر).

## العمليات الدفعية

متغير `GroveOp::DenseTreeInsert` يدعم الإدراج الدفعي عبر خط أنابيب
الدفعة القياسي في GroveDB:

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

**المعالجة المسبقة:** مثل جميع أنواع الأشجار غير-Merk، تُعالَج عمليات `DenseTreeInsert` مسبقاً
قبل تنفيذ جسم الدفعة الرئيسي. طريقة `preprocess_dense_tree_ops`:

1. تُجمِّع جميع عمليات `DenseTreeInsert` حسب `(path, key)`
2. لكل مجموعة، تُنفّذ الإدراجات تتابعياً (قراءة العنصر، إدراج
   كل قيمة، تحديث تجزئة الجذر)
3. تُحوِّل كل مجموعة إلى عملية `ReplaceNonMerkTreeRoot` تحمل تجزئة الجذر
   النهائية والعدد عبر آلية النشر القياسية

الإدراجات المتعددة لنفس الشجرة الكثيفة ضمن دفعة واحدة مدعومة — تُعالَج
بالترتيب وفحص الاتساق يسمح بالمفاتيح المكررة لهذا النوع من العمليات.

**النشر:** تجزئة الجذر والعدد يتدفقان عبر متغير `NonMerkTreeMeta::DenseTree`
في `ReplaceNonMerkTreeRoot`، متبعين نفس نمط MmrTree وBulkAppendTree.

## البراهين

تدعم DenseAppendOnlyFixedSizeTree **براهين استعلامات فرعية V1** عبر متغير `ProofBytes::DenseTree`.
يمكن إثبات المواقع الفردية ضد تجزئة جذر الشجرة باستخدام براهين تضمين
تحمل قيم الأسلاف وتجزئات الأشجار الفرعية الشقيقة.

### بنية مسار التوثيق

لأن العقد الداخلية تُجزّئ **قيمتها الخاصة** (وليس فقط تجزئات الأبناء)، فإن
مسار التوثيق يختلف عن شجرة ميركل القياسية. للتحقق من ورقة في الموقع
`p`، يحتاج المُحقِّق:

1. **قيمة الورقة** (المدخل المُثبت)
2. **تجزئات قيم الأسلاف** لكل عقدة داخلية على المسار من `p` إلى الجذر (فقط التجزئة من 32 بايت، وليس القيمة الكاملة)
3. **تجزئات الأشجار الفرعية الشقيقة** لكل ابن ليس على المسار

لأن جميع العقد تستخدم `blake3(H(value) || H(left) || H(right))` (بدون علامات نطاق)،
فإن البرهان يحمل فقط تجزئات قيم من 32 بايت للأسلاف — وليس القيم الكاملة. هذا
يُبقي البراهين مدمجة بغض النظر عن حجم القيم الفردية.

```rust
pub struct DenseTreeProof {
    pub entries: Vec<(u16, Vec<u8>)>,            // proved (position, value) pairs
    pub node_value_hashes: Vec<(u16, [u8; 32])>, // ancestor value hashes on the auth path
    pub node_hashes: Vec<(u16, [u8; 32])>,       // precomputed sibling subtree hashes
}
```

> **ملاحظة:** `height` و`count` ليسا في بنية البرهان — يحصل عليهما المُحقِّق من العنصر الأب، المُوثّق بواسطة تسلسل Merk الهرمي.

### مثال تفصيلي

شجرة بارتفاع=3، سعة=7، عدد=5، إثبات الموقع 4:

```text
        0
       / \
      1   2
     / \ / \
    3  4 5  6
```

المسار من 4 إلى الجذر: `4 → 1 → 0`. المجموعة الموسّعة: `{0, 1, 4}`.

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

### براهين المواقع المتعددة

عند إثبات مواقع متعددة، تدمج المجموعة الموسّعة مسارات التوثيق المتداخلة. الأسلاف
المشتركون يُضمَّنون مرة واحدة فقط، مما يجعل براهين المواقع المتعددة أكثر إحكاماً من
البراهين المستقلة لموقع واحد.

### قيود V0

براهين V0 لا تستطيع النزول داخل الأشجار الكثيفة. إذا طابق استعلام V0
`DenseAppendOnlyFixedSizeTree` مع استعلام فرعي، يُرجع النظام
`Error::NotSupported` موجّهاً المُستدعي لاستخدام `prove_query_v1`.

### ترميز مفاتيح الاستعلام

مواقع الشجرة الكثيفة تُرمَّز كمفاتيح استعلام **u16 بترتيب الطرف الأكبر** (2 بايت)، على عكس
MmrTree وBulkAppendTree اللتين تستخدمان u64. جميع أنواع نطاقات `QueryItem` القياسية
مدعومة.

## مقارنة مع الأشجار الأخرى غير-Merk

| | DenseTree | MmrTree | BulkAppendTree | CommitmentTree |
|---|---|---|---|---|
| **Element discriminant** | 14 | 12 | 13 | 11 |
| **TreeType** | 10 | 8 | 9 | 7 |
| **Capacity** | Fixed (`2^h - 1`, max 65,535) | Unlimited | Unlimited | Unlimited |
| **Data model** | Every position stores a value | Leaf-only | Dense tree buffer + chunks | Leaf-only |
| **Hash in Element?** | No (flows as child hash) | No (flows as child hash) | No (flows as child hash) | No (flows as child hash) |
| **Insert cost (hashing)** | O(n) blake3 | O(1) amortized | O(1) amortized | ~33 Sinsemilla |
| **Cost size** | 6 bytes | 11 bytes | 12 bytes | 12 bytes |
| **Proof support** | V1 (Dense) | V1 (MMR) | V1 (Bulk) | V1 (CommitmentTree) |
| **Best for** | Small bounded structures | Event logs | High-throughput logs | ZK commitments |

**متى تختار DenseAppendOnlyFixedSizeTree:**
- العدد الأقصى للمدخلات معروف وقت الإنشاء
- تحتاج كل موقع (بما في ذلك العقد الداخلية) لتخزين بيانات
- تريد أبسط نموذج بيانات ممكن بدون نمو غير محدود
- إعادة حساب تجزئة الجذر بتعقيد O(n) مقبولة (ارتفاعات شجرة صغيرة)

**متى لا تختارها:**
- تحتاج سعة غير محدودة ← استخدم MmrTree أو BulkAppendTree
- تحتاج توافقية ZK ← استخدم CommitmentTree

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

// Query metadata
let count = db.dense_tree_count(&[b"state"], b"validator_slots", None, grove_version)?;
let hash = db.dense_tree_root_hash(&[b"state"], b"validator_slots", None, grove_version)?;
```

## ملفات التنفيذ

| الملف | المحتويات |
|-------|-----------|
| `grovedb-dense-fixed-sized-merkle-tree/src/lib.rs` | سمة `DenseTreeStore`، بنية `DenseFixedSizedMerkleTree`، التجزئة التكرارية |
| `grovedb-dense-fixed-sized-merkle-tree/src/proof.rs` | بنية `DenseTreeProof`، `generate()`، `encode_to_vec()`، `decode_from_slice()` |
| `grovedb-dense-fixed-sized-merkle-tree/src/verify.rs` | `DenseTreeProof::verify()` — دالة صافية، لا تحتاج تخزين |
| `grovedb-element/src/element/mod.rs` | `Element::DenseAppendOnlyFixedSizeTree` (المُميِّز 14) |
| `grovedb-element/src/element/constructor.rs` | `empty_dense_tree()`، `new_dense_tree()` |
| `merk/src/tree_type/mod.rs` | `TreeType::DenseAppendOnlyFixedSizeTree = 10` |
| `merk/src/tree_type/costs.rs` | `DENSE_TREE_COST_SIZE = 6` |
| `grovedb/src/operations/dense_tree.rs` | عمليات GroveDB، `AuxDenseTreeStore`، معالجة الدفعات المسبقة |
| `grovedb/src/operations/proof/generate.rs` | `generate_dense_tree_layer_proof()`، `query_items_to_positions()` |
| `grovedb/src/operations/proof/verify.rs` | `verify_dense_tree_lower_layer()` |
| `grovedb/src/operations/proof/mod.rs` | متغير `ProofBytes::DenseTree` |
| `grovedb/src/batch/estimated_costs/average_case_costs.rs` | نموذج تكلفة الحالة المتوسطة |
| `grovedb/src/batch/estimated_costs/worst_case_costs.rs` | نموذج تكلفة أسوأ حالة |
| `grovedb/src/tests/dense_tree_tests.rs` | 22 اختبار تكامل |

---
