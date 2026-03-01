# شجرة BulkAppendTree — تخزين إلحاق فقط عالي الإنتاجية

BulkAppendTree هي إجابة GroveDB على تحدٍّ هندسي محدد: كيف تبني
سجل إلحاق فقط (append-only log) عالي الإنتاجية يدعم براهين نطاق (range proofs) فعّالة، ويُقلِّل
التجزئة لكل عملية كتابة، ويُنتج لقطات شرائح (chunk snapshots) ثابتة مناسبة لتوزيع CDN؟

بينما MmrTree (الفصل 13) مثالية لبراهين الأوراق الفردية، صُمِّمت
BulkAppendTree لأعباء العمل حيث تصل آلاف القيم لكل كتلة (block) ويحتاج العملاء
للمزامنة بجلب نطاقات من البيانات. تُحقِّق هذا بـ **بنية ذات مستويين**:
مخزن مؤقت (buffer) لشجرة ميركل كثيفة (dense Merkle tree) يمتص عمليات الإلحاق الواردة، وMMR على مستوى الشرائح
يُسجِّل جذور الشرائح المُنجَزة.

## البنية ذات المستويين

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

**المستوى 1 — المخزن المؤقت.** القيم الواردة تُكتب في `DenseFixedSizedMerkleTree`
(انظر الفصل 16). سعة المخزن المؤقت هي `2^height - 1` موقعاً. تجزئة جذر الشجرة الكثيفة
(`dense_tree_root`) تُحدَّث بعد كل إدراج.

**المستوى 2 — MMR الشرائح.** عندما يمتلئ المخزن المؤقت (يصل إلى `chunk_size` مدخل)،
تُرمَّز جميع المدخلات تسلسلياً في **كتلة شريحة** (chunk blob) ثابتة، ويُحسَب جذر ميركل كثيف
على تلك المدخلات، ويُلحق ذلك الجذر كورقة في MMR الشرائح.
ثم يُفرَّغ المخزن المؤقت.

**جذر الحالة** (state root) يجمع كلا المستويين في التزام واحد من 32 بايت يتغير
مع كل إلحاق، مما يضمن أن شجرة Merk الأب تعكس دائماً أحدث حالة.

## كيف تملأ القيم المخزن المؤقت

كل استدعاء لـ `append()` يتبع هذا التسلسل:

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

**المخزن المؤقت هو `DenseFixedSizedMerkleTree`** (انظر الفصل 16). تجزئة جذره
تتغير بعد كل إدراج، مما يوفر التزاماً بجميع مدخلات المخزن المؤقت الحالية.
تجزئة الجذر هذه هي ما يتدفق إلى حساب جذر الحالة.

## ضغط الشرائح

عندما يمتلئ المخزن المؤقت (يصل إلى `chunk_size` مدخل)، يُفعَّل الضغط (compaction) تلقائياً:

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

بعد الضغط، تصبح كتلة الشريحة **ثابتة نهائياً** — لا تتغير
أبداً مرة أخرى. هذا يجعل كتل الشرائح مثالية للتخزين المؤقت في CDN، ومزامنة العملاء،
والتخزين الأرشيفي.

**مثال: 4 إلحاقات مع chunk_power=2 (حجم الشريحة chunk_size=4)**

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

## جذر الحالة

جذر الحالة يربط كلا المستويين في تجزئة واحدة:

```rust
fn compute_state_root(
    mmr_root: &[u8; 32],         // Chunk MMR root (or [0;32] if empty)
    dense_tree_root: &[u8; 32],  // Root hash of current buffer (dense tree)
) -> [u8; 32] {
    blake3("bulk_state" || mmr_root || dense_tree_root)
}
```

`total_count` و`chunk_power` **ليسا** مُضمَّنين في جذر الحالة لأنهما
مُصادَق عليهما بالفعل بواسطة تجزئة قيمة Merk — فهما حقلا عنصر
`Element` المُرمَّز تسلسلياً المُخزَّن في عقدة Merk الأب. يلتقط جذر الحالة فقط
الالتزامات على مستوى البيانات (`mmr_root` و`dense_tree_root`). هذه هي التجزئة التي
تتدفق كتجزئة Merk الفرعية (child hash) وتنتشر صعوداً إلى تجزئة جذر GroveDB.

## جذر ميركل الكثيف

عندما تُضغط شريحة، تحتاج المدخلات إلى التزام واحد من 32 بايت.
تستخدم BulkAppendTree **شجرة ميركل ثنائية كثيفة (كاملة)**:

```text
Given entries [e_0, e_1, e_2, e_3]:

Level 0 (leaves):  blake3(e_0)  blake3(e_1)  blake3(e_2)  blake3(e_3)
                      \__________/              \__________/
Level 1:           blake3(h_0 || h_1)       blake3(h_2 || h_3)
                          \____________________/
Level 2 (root):    blake3(h_01 || h_23)  ← this is the dense Merkle root
```

لأن `chunk_size` دائماً قوة 2 (بالتصميم: `1u32 << chunk_power`)،
تكون الشجرة دائماً كاملة (لا حاجة لحشو أو أوراق وهمية). عدد التجزئات
هو بالضبط `2 * chunk_size - 1`:
- `chunk_size` تجزئة أوراق (واحدة لكل مدخل)
- `chunk_size - 1` تجزئة عُقد داخلية

تنفيذ جذر ميركل الكثيف يعيش في `grovedb-mmr/src/dense_merkle.rs` ويوفر
دالتين:
- `compute_dense_merkle_root(hashes)` — من أوراق مُجزَّأة مسبقاً
- `compute_dense_merkle_root_from_values(values)` — تُجزِّئ القيم أولاً، ثم تبني
  الشجرة

## ترميز كتل الشرائح التسلسلي

كتل الشرائح (chunk blobs) هي الأرشيفات الثابتة المُنتجة بالضغط. يختار المُرمِّز التسلسلي
تلقائياً أكثر صيغة سلكية (wire format) مُحكمة بناءً على أحجام المدخلات:

**صيغة الحجم الثابت** (علامة `0x01`) — عندما يكون لجميع المدخلات نفس الطول:

```text
┌──────┬──────────┬─────────────┬─────────┬─────────┬─────────┐
│ 0x01 │ count    │ entry_size  │ entry_0 │ entry_1 │ ...     │
│ 1B   │ 4B (BE)  │ 4B (BE)     │ N bytes │ N bytes │         │
└──────┴──────────┴─────────────┴─────────┴─────────┴─────────┘
Total: 1 + 4 + 4 + (count × entry_size) bytes
```

**صيغة الحجم المتغير** (علامة `0x00`) — عندما يكون للمدخلات أطوال مختلفة:

```text
┌──────┬──────────┬─────────┬──────────┬─────────┬─────────────┐
│ 0x00 │ len_0    │ entry_0 │ len_1    │ entry_1 │ ...         │
│ 1B   │ 4B (BE)  │ N bytes │ 4B (BE)  │ M bytes │             │
└──────┴──────────┴─────────┴──────────┴─────────┴─────────────┘
Total: 1 + Σ(4 + len_i) bytes
```

صيغة الحجم الثابت توفر 4 بايتات لكل مدخل مقارنة بالحجم المتغير، وهو ما يتراكم
بشكل ملحوظ للشرائح الكبيرة من البيانات موحدة الحجم (مثل التزامات تجزئة 32 بايت).
لـ 1024 مدخل بحجم 32 بايت لكل منها:
- ثابت: `1 + 4 + 4 + 32768 = 32,777 بايت`
- متغير: `1 + 1024 × (4 + 32) = 36,865 بايت`
- التوفير: ~11%

## تخطيط مفاتيح التخزين

جميع بيانات BulkAppendTree تعيش في فضاء اسم **البيانات** (data namespace)، مُفتَّحة ببادئات أحرف فردية:

| نمط المفتاح | الصيغة | الحجم | الغرض |
|---|---|---|---|
| `M` | 1 بايت | 1 ب | مفتاح البيانات الوصفية (metadata) |
| `b` + `{index}` | `b` + u32 BE | 5 ب | مدخل مخزن مؤقت عند الفهرس |
| `e` + `{index}` | `e` + u64 BE | 9 ب | كتلة شريحة عند الفهرس |
| `m` + `{pos}` | `m` + u64 BE | 9 ب | عقدة MMR عند الموقع |

**البيانات الوصفية** تُخزِّن `mmr_size` (8 بايت بترتيب البايت الأكبر BE). `total_count` و`chunk_power`
مُخزَّنان في العنصر نفسه (في Merk الأب)، وليس في بيانات وصفية فضاء اسم البيانات.
هذا التقسيم يعني أن قراءة العدد هي عملية بحث بسيطة عن العنصر دون فتح
سياق تخزين البيانات.

مفاتيح المخزن المؤقت تستخدم فهارس u32 (من 0 إلى `chunk_size - 1`) لأن سعة المخزن المؤقت
محدودة بـ `chunk_size` (وهو u32، يُحسب كـ `1u32 << chunk_power`). مفاتيح الشرائح تستخدم
فهارس u64 لأن عدد الشرائح المكتملة يمكن أن ينمو بلا حدود.

## بنية BulkAppendTree

```rust
pub struct BulkAppendTree<S> {
    pub total_count: u64,                        // Total values ever appended
    pub dense_tree: DenseFixedSizedMerkleTree<S>, // The buffer (dense tree)
}
```

المخزن المؤقت هو `DenseFixedSizedMerkleTree` — تجزئة جذره هي `dense_tree_root`.

**مُوصِّلات (accessors):**
- `capacity() -> u16`: `dense_tree.capacity()` (= `2^height - 1`)
- `epoch_size() -> u64`: `capacity + 1` (= `2^height`، عدد المدخلات لكل شريحة)
- `height() -> u8`: `dense_tree.height()`

**قيم مُشتقة** (غير مُخزَّنة):

| القيمة | الصيغة |
|---|---|
| `chunk_count` | `total_count / epoch_size` |
| `buffer_count` | `dense_tree.count()` |
| `mmr_size` | `leaf_count_to_mmr_size(chunk_count)` |

## عمليات GroveDB

تتكامل BulkAppendTree مع GroveDB من خلال ست عمليات مُعرَّفة في
`grovedb/src/operations/bulk_append_tree.rs`:

### bulk_append

العملية المُعدِّلة الأساسية. تتبع نمط تخزين GroveDB غير Merk القياسي:

```text
1. Validate element is BulkAppendTree
2. Open data storage context
3. Load tree from store
4. Append value (may trigger compaction)
5. Update element in parent Merk with new state_root + total_count
6. Propagate changes up through Merk hierarchy
7. Commit transaction
```

مُحوِّل `AuxBulkStore` يلتف حول استدعاءات `get_aux`/`put_aux`/`delete_aux` الخاصة بـ GroveDB
ويُجمِّع `OperationCost` في `RefCell` لتتبع التكاليف. تكاليف التجزئة من
عملية الإلحاق تُضاف إلى `cost.hash_node_calls`.

### عمليات القراءة

| العملية | ما تُرجعه | تخزين البيانات؟ |
|---|---|---|
| `bulk_get_value(path, key, position)` | القيمة في الموقع العام | نعم — تقرأ من كتلة الشريحة أو المخزن المؤقت |
| `bulk_get_chunk(path, key, chunk_index)` | كتلة الشريحة الخام | نعم — تقرأ مفتاح الشريحة |
| `bulk_get_buffer(path, key)` | جميع مدخلات المخزن المؤقت الحالية | نعم — تقرأ مفاتيح المخزن المؤقت |
| `bulk_count(path, key)` | العدد الإجمالي (u64) | لا — تقرأ من العنصر |
| `bulk_chunk_count(path, key)` | الشرائح المكتملة (u64) | لا — مُحسَب من العنصر |

عملية `get_value` توجِّه بشفافية حسب الموقع:

```text
if position < completed_chunks × chunk_size:
    chunk_idx = position / chunk_size
    intra_idx = position % chunk_size
    → read chunk blob, deserialize, return entry[intra_idx]
else:
    buffer_idx = position % chunk_size
    → read buffer_key(buffer_idx)
```

## العمليات الدفعية والمعالجة المسبقة

تدعم BulkAppendTree العمليات الدفعية عبر متغير `GroveOp::BulkAppend`.
بما أن `execute_ops_on_path` لا يملك وصولاً لسياق تخزين البيانات، يجب معالجة جميع عمليات BulkAppend
مسبقاً قبل `apply_body`.

خط أنابيب المعالجة المسبقة:

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

متغير `append_with_mem_buffer` يتجنب مشاكل القراءة بعد الكتابة (read-after-write):
مدخلات المخزن المؤقت تُتتبَّع في `Vec<Vec<u8>>` في الذاكرة، لذا يمكن للضغط قراءتها
حتى وإن لم يكن التخزين المعاملاتي (transactional storage) قد أُثبت بعد.

## سمة BulkStore

```rust
pub trait BulkStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String>;
    fn delete(&self, key: &[u8]) -> Result<(), String>;
}
```

الدوال تأخذ `&self` (وليس `&mut self`) لتتطابق مع نمط القابلية للتغيير الداخلية (interior mutability)
في GroveDB حيث تمر عمليات الكتابة عبر دفعة (batch). تكامل GroveDB يُنفِّذ هذا عبر
`AuxBulkStore` الذي يلتف حول `StorageContext` ويُجمِّع `OperationCost`.

`MmrAdapter` يربط `BulkStore` بسمتَي `MMRStoreReadOps`/
`MMRStoreWriteOps` الخاصتين بـ ckb MMR، مُضيفاً ذاكرة تخزين مؤقتة للكتابة المباشرة (write-through cache)
لصحة القراءة بعد الكتابة.

## توليد البراهين

براهين BulkAppendTree تدعم **استعلامات النطاق** (range queries) على المواقع. بنية البرهان
تلتقط كل ما يلزم لمُتحقِّق عديم الحالة (stateless verifier) لتأكيد أن بيانات محددة
موجودة في الشجرة:

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

**خطوات التوليد** لنطاق `[start, end)` (مع `chunk_size = 1u32 << chunk_power`):

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

**لماذا تضمين جميع مدخلات المخزن المؤقت؟** المخزن المؤقت هو شجرة ميركل كثيفة تجزئة جذرها
تلتزم بكل مدخل. يجب على المُتحقِّق إعادة بناء الشجرة من جميع المدخلات للتحقق
من `dense_tree_root`. بما أن المخزن المؤقت محدود بـ `capacity` (65,535 مدخل
على الأكثر)، فهذه تكلفة معقولة.

## التحقق من البراهين

التحقق هو دالة صافية (pure function) — لا حاجة للوصول إلى قاعدة البيانات. يُجري خمسة فحوصات:

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

بعد نجاح التحقق، يوفر `BulkAppendTreeProofResult` دالة
`values_in_range(start, end)` التي تستخلص قيماً محددة من كتل الشرائح
ومدخلات المخزن المؤقت المُتحقَّق منها.

## كيف ترتبط بتجزئة جذر GroveDB

BulkAppendTree هي **شجرة غير Merk** — تُخزِّن البيانات في فضاء اسم البيانات،
وليس في شجرة Merk فرعية. في Merk الأب، يُخزَّن العنصر كـ:

```text
Element::BulkAppendTree(total_count, chunk_power, flags)
```

جذر الحالة يتدفق كتجزئة Merk الفرعية. تجزئة عقدة Merk الأب هي:

```text
combine_hash(value_hash(element_bytes), state_root)
```

`state_root` يتدفق كتجزئة Merk الفرعية (عبر معامل `subtree_root_hash`
لـ `insert_subtree`). أي تغيير في جذر الحالة ينتشر صعوداً عبر
التسلسل الهرمي Merk في GroveDB إلى تجزئة الجذر.

في براهين V1 (§9.6)، برهان Merk الأب يُثبت بايتات العنصر وربط
التجزئة الفرعية، و`BulkAppendTreeProof` يُثبت أن البيانات المُستعلم عنها متسقة
مع `state_root` المُستخدم كتجزئة فرعية.

## تتبع التكاليف

تكلفة التجزئة لكل عملية تُتتبَّع صراحة:

| العملية | استدعاءات Blake3 | ملاحظات |
|---|---|---|
| إلحاق واحد (بدون ضغط) | 3 | 2 لسلسلة تجزئة المخزن المؤقت + 1 لجذر الحالة |
| إلحاق واحد (مع ضغط) | 3 + 2C - 1 + ~2 | السلسلة + ميركل كثيف (C=حجم الشريحة) + دفع MMR + جذر الحالة |
| `get_value` من شريحة | 0 | فك ترميز صافٍ، بدون تجزئة |
| `get_value` من المخزن المؤقت | 0 | بحث مباشر بالمفتاح |
| توليد البرهان | يعتمد على عدد الشرائح | جذر ميركل كثيف لكل شريحة + برهان MMR |
| التحقق من البرهان | 2C·K - K + B·2 + 1 | K شريحة، B مدخل مخزن مؤقت، C حجم الشريحة |

**التكلفة المُطفأة (amortized) لكل إلحاق**: لحجم شريحة 1024 (chunk_power=10)، الحمل الزائد للضغط من ~2047
تجزئة (جذر ميركل كثيف) يتوزع على 1024 إلحاقاً، مُضيفاً ~2 تجزئة لكل
إلحاق. مع الـ 3 تجزئات لكل إلحاق، المجموع المُطفأ هو **~5 استدعاءات Blake3
لكل إلحاق** — فعّال جداً لبنية مُصادَق عليها تشفيرياً.

## مقارنة مع MmrTree

| | BulkAppendTree | MmrTree |
|---|---|---|
| **البنية** | ذات مستويين (مخزن مؤقت + MMR شرائح) | MMR واحد |
| **تكلفة التجزئة لكل إلحاق** | 3 (+ ~2 مُطفأة للضغط) | ~2 |
| **دقة البراهين** | استعلامات نطاق على المواقع | براهين أوراق فردية |
| **لقطات ثابتة** | نعم (كتل الشرائح) | لا |
| **صديقة لـ CDN** | نعم (كتل الشرائح قابلة للتخزين المؤقت) | لا |
| **مدخلات المخزن المؤقت** | نعم (تُطلب جميعها للبرهان) | غير مطبَّق |
| **الأفضل لـ** | سجلات عالية الإنتاجية، مزامنة جماعية | سجلات أحداث، بحث فردي |
| **مُميِّز العنصر** | 13 | 12 |
| **TreeType** | 9 | 8 |

اختر MmrTree عندما تحتاج براهين أوراق فردية بحمل أدنى. اختر
BulkAppendTree عندما تحتاج استعلامات نطاق، ومزامنة جماعية، ولقطات مبنية
على الشرائح.

## ملفات التنفيذ

| الملف | الغرض |
|-------|-------|
| `grovedb-bulk-append-tree/src/lib.rs` | جذر الصندوق، إعادة التصدير |
| `grovedb-bulk-append-tree/src/tree/mod.rs` | بنية `BulkAppendTree`، مُوصِّلات الحالة، استمرارية البيانات الوصفية |
| `grovedb-bulk-append-tree/src/tree/append.rs` | `append()`، `append_with_mem_buffer()`، `compact_entries()` |
| `grovedb-bulk-append-tree/src/tree/hash.rs` | `compute_state_root` |
| `grovedb-bulk-append-tree/src/tree/keys.rs` | `META_KEY`، `buffer_key`، `chunk_key`، `mmr_node_key` |
| `grovedb-bulk-append-tree/src/tree/query.rs` | `get_value`، `get_chunk`، `get_buffer` |
| `grovedb-bulk-append-tree/src/tree/mmr_adapter.rs` | `MmrAdapter` مع ذاكرة تخزين مؤقتة للكتابة المباشرة |
| `grovedb-bulk-append-tree/src/chunk.rs` | ترميز كتل الشرائح تسلسلياً (صيغ ثابتة + متغيرة) |
| `grovedb-bulk-append-tree/src/proof.rs` | توليد والتحقق من `BulkAppendTreeProof` |
| `grovedb-bulk-append-tree/src/store.rs` | سمة `BulkStore` |
| `grovedb-bulk-append-tree/src/error.rs` | تعداد `BulkAppendError` |
| `grovedb/src/operations/bulk_append_tree.rs` | عمليات GroveDB، `AuxBulkStore`، معالجة الدفعات المسبقة |
| `grovedb/src/tests/bulk_append_tree_tests.rs` | 27 اختبار تكامل |

---
