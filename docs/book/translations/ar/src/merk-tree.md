# شجرة Merk — شجرة Merkle AVL

شجرة Merk هي الوحدة البنائية الأساسية في GroveDB. كل شجرة فرعية في
البستان هي شجرة Merk — شجرة بحث ثنائية ذاتية التوازن حيث يتم تجزئة كل عقدة
تشفيرياً، مما ينتج تجزئة جذر واحدة توثّق
محتويات الشجرة بالكامل.

## ما هي عقدة Merk؟

على عكس العديد من تطبيقات شجرة ميركل حيث تعيش البيانات فقط في الأوراق، في شجرة
Merk **كل عقدة تُخزّن زوج مفتاح-قيمة**. هذا يعني أنه لا توجد عقد داخلية "فارغة"
— الشجرة هي بنية بحث ومخزن بيانات في آن واحد.

```mermaid
graph TD
    subgraph TreeNode
        subgraph inner["inner: Box&lt;TreeNodeInner&gt;"]
            subgraph kv["kv: KV"]
                KEY["<b>key:</b> Vec&lt;u8&gt;<br/><i>e.g. b&quot;alice&quot;</i>"]
                VAL["<b>value:</b> Vec&lt;u8&gt;<br/><i>serialized Element bytes</i>"]
                FT["<b>feature_type:</b> TreeFeatureType<br/><i>BasicMerkNode | SummedMerkNode(n) | ...</i>"]
                VH["<b>value_hash:</b> [u8; 32]<br/><i>H(varint(value.len) ‖ value)</i>"]
                KVH["<b>hash:</b> [u8; 32] — the kv_hash<br/><i>H(varint(key.len) ‖ key ‖ value_hash)</i>"]
            end
            LEFT["<b>left:</b> Option&lt;Link&gt;"]
            RIGHT["<b>right:</b> Option&lt;Link&gt;"]
        end
        OLD["<b>old_value:</b> Option&lt;Vec&lt;u8&gt;&gt; — previous value for cost deltas"]
        KNOWN["<b>known_storage_cost:</b> Option&lt;KeyValueStorageCost&gt;"]
    end

    LEFT -->|"smaller keys"| LC["Left Child<br/><i>Link::Reference | Modified<br/>Uncommitted | Loaded</i>"]
    RIGHT -->|"larger keys"| RC["Right Child<br/><i>Link::Reference | Modified<br/>Uncommitted | Loaded</i>"]

    style kv fill:#eaf2f8,stroke:#2980b9
    style inner fill:#fef9e7,stroke:#f39c12
    style TreeNode fill:#f9f9f9,stroke:#333
    style LC fill:#d5f5e3,stroke:#27ae60
    style RC fill:#d5f5e3,stroke:#27ae60
```

في الشيفرة (`merk/src/tree/mod.rs`):

```rust
pub struct TreeNode {
    pub(crate) inner: Box<TreeNodeInner>,
    pub(crate) old_value: Option<Vec<u8>>,        // Previous value for cost tracking
    pub(crate) known_storage_cost: Option<KeyValueStorageCost>,
}

pub struct TreeNodeInner {
    pub(crate) left: Option<Link>,    // Left child (smaller keys)
    pub(crate) right: Option<Link>,   // Right child (larger keys)
    pub(crate) kv: KV,               // The key-value payload
}
```

`Box<TreeNodeInner>` يُبقي العقدة على الكومة (heap)، وهو أمر ضروري لأن روابط الأبناء
يمكن أن تحتوي بشكل تكراري على نسخ `TreeNode` كاملة.

## بنية KV

بنية `KV` تحمل كلاً من البيانات الخام وملخصاتها التشفيرية
(`merk/src/tree/kv.rs`):

```rust
pub struct KV {
    pub(super) key: Vec<u8>,                        // The lookup key
    pub(super) value: Vec<u8>,                      // The stored value
    pub(super) feature_type: TreeFeatureType,       // Aggregation behavior
    pub(crate) value_defined_cost: Option<ValueDefinedCostType>,
    pub(super) hash: CryptoHash,                    // kv_hash
    pub(super) value_hash: CryptoHash,              // H(value)
}
```

نقطتان مهمتان:

1. **المفاتيح لا تُخزَّن على القرص كجزء من العقدة المُرمَّزة.** بل تُخزَّن كمفتاح
   RocksDB. عند فك ترميز عقدة من التخزين، يتم حقن المفتاح من
   الخارج. هذا يتجنب تكرار بايتات المفتاح.

2. **يتم الحفاظ على حقلَي تجزئة.** `value_hash` هو `H(value)` و
   `hash` (أي kv_hash) هو `H(key, value_hash)`. الاحتفاظ بكليهما يسمح لنظام البراهين
   باختيار مقدار المعلومات المكشوفة.

## الطبيعة شبه المتوازنة — كيف "تتذبذب" شجرة AVL

شجرة Merk هي **شجرة AVL** — شجرة البحث الثنائية الذاتية التوازن الكلاسيكية
التي اخترعها أديلسون-فيلسكي ولانديس. الثابت الأساسي هو:

> لكل عقدة، فرق الارتفاع بين شجرتيها الفرعيتين اليسرى واليمنى
> لا يتجاوز 1.

يُعبَّر عن هذا بـ **عامل التوازن** (balance factor):

```text
balance_factor = right_height - left_height
```

القيم الصالحة: **{-1, 0, 1}**

```rust
// merk/src/tree/mod.rs
pub const fn balance_factor(&self) -> i8 {
    let left_height = self.child_height(true) as i8;
    let right_height = self.child_height(false) as i8;
    right_height - left_height
}
```

لكن هنا النقطة الدقيقة: بينما كل عقدة فردية يمكن أن تميل بمستوى واحد فقط،
هذه الميلانات يمكن أن **تتراكم** عبر الشجرة. لهذا نسميها
"شبه متوازنة" — الشجرة ليست متوازنة تماماً مثل شجرة ثنائية كاملة.

خذ بعين الاعتبار شجرة من 10 عقد. شجرة متوازنة تماماً سيكون ارتفاعها 4
(⌈log₂(10+1)⌉). لكن شجرة AVL قد يكون ارتفاعها 5:

**متوازنة تماماً (ارتفاع 4)** — كل مستوى ممتلئ بالكامل:

```mermaid
graph TD
    N5["5<br/><small>bf=0</small>"]
    N3["3<br/><small>bf=0</small>"]
    N8["8<br/><small>bf=0</small>"]
    N2["2<br/><small>bf=0</small>"]
    N4["4<br/><small>bf=0</small>"]
    N6["6<br/><small>bf=0</small>"]
    N9["9<br/><small>bf=+1</small>"]
    N10["10<br/><small>bf=0</small>"]

    N5 --- N3
    N5 --- N8
    N3 --- N2
    N3 --- N4
    N8 --- N6
    N8 --- N9
    N9 --- N10

    style N5 fill:#d4e6f1,stroke:#2980b9
```

**"تذبذب" صالح لـ AVL (ارتفاع 5)** — كل عقدة تميل بواحد على الأكثر، لكن التراكم يحصل:

```mermaid
graph TD
    N4["4<br/><small>bf=+1</small>"]
    N2["2<br/><small>bf=-1</small>"]
    N7["7<br/><small>bf=+1</small>"]
    N1["1<br/><small>bf=-1</small>"]
    N3["3<br/><small>bf=0</small>"]
    N5["5<br/><small>bf=0</small>"]
    N9["9<br/><small>bf=-1</small>"]
    N0["0<br/><small>bf=0</small>"]
    N8["8<br/><small>bf=0</small>"]
    N10["10<br/><small>bf=0</small>"]

    N4 --- N2
    N4 --- N7
    N2 --- N1
    N2 --- N3
    N7 --- N5
    N7 --- N9
    N1 --- N0
    N9 --- N8
    N9 --- N10

    style N4 fill:#fadbd8,stroke:#e74c3c
```

> ارتفاع 5 مقابل 4 المثالي — هذا هو "التذبذب". أسوأ حالة: h ≤ 1.44 × log₂(n+2).

كلتا الشجرتين شجرتا AVL صالحتان! أسوأ ارتفاع لشجرة AVL هو:

```text
h ≤ 1.4404 × log₂(n + 2) − 0.3277
```

إذاً لـ **n = 1,000,000** عقدة:
- توازن مثالي: ارتفاع 20
- أسوأ حالة AVL: ارتفاع ≈ 29

هذا الزيادة بنسبة ~44% هو ثمن قواعد الدوران البسيطة لـ AVL. عملياً، الإدراجات
العشوائية تنتج أشجاراً أقرب بكثير للتوازن المثالي.

إليك كيف تبدو الأشجار الصالحة وغير الصالحة:

**صالحة** — جميع عوامل التوازن في {-1, 0, +1}:

```mermaid
graph TD
    subgraph balanced["Balanced (bf=0)"]
        D1["D<br/>bf=0"] --- B1["B<br/>bf=0"]
        D1 --- F1["F<br/>bf=0"]
        B1 --- A1["A"] & C1["C"]
        F1 --- E1["E"] & G1["G"]
    end
    subgraph rightlean["Right-leaning (bf=+1)"]
        D2["D<br/>bf=+1"] --- B2["B<br/>bf=0"]
        D2 --- F2["F<br/>bf=0"]
        B2 --- A2["A"] & C2["C"]
        F2 --- E2["E"] & G2["G"]
    end
    subgraph leftlean["Left-leaning (bf=-1)"]
        D3["D<br/>bf=-1"] --- B3["B<br/>bf=-1"]
        D3 --- E3["E"]
        B3 --- A3["A"]
    end

    style balanced fill:#d5f5e3,stroke:#27ae60
    style rightlean fill:#d5f5e3,stroke:#27ae60
    style leftlean fill:#d5f5e3,stroke:#27ae60
```

**غير صالحة** — عامل التوازن = +2 (يحتاج دوراناً!):

```mermaid
graph TD
    B["B<br/><b>bf=+2 ✗</b>"]
    D["D<br/>bf=+1"]
    F["F<br/>bf=0"]
    B --- D
    D --- F

    style B fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
```

> الشجرة الفرعية اليمنى أطول بمستويين من اليسرى (الفارغة). هذا يُفعّل **دوراناً يسارياً** لاستعادة ثابت AVL.

## الدورانات — استعادة التوازن

عندما يتسبب إدراج أو حذف في وصول عامل التوازن إلى ±2، يجب أن تُدوَّر الشجرة
لاستعادة ثابت AVL. هناك أربع حالات، قابلة للاختزال إلى
عمليتين أساسيتين.

### دوران يساري مفرد

يُستخدم عندما تكون العقدة **ثقيلة يميناً** (bf = +2) وابنها الأيمن
**ثقيل يميناً أو متوازن** (bf ≥ 0):

**قبل** (bf=+2):

```mermaid
graph TD
    A["A<br/><small>bf=+2</small>"]
    t1["t₁"]
    B["B<br/><small>bf≥0</small>"]
    X["X"]
    C["C"]
    A --- t1
    A --- B
    B --- X
    B --- C
    style A fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
```

**بعد** الدوران اليساري — B يُرقَّى إلى الجذر:

```mermaid
graph TD
    B2["B<br/><small>bf=0</small>"]
    A2["A"]
    C2["C"]
    t12["t₁"]
    X2["X"]
    B2 --- A2
    B2 --- C2
    A2 --- t12
    A2 --- X2
    style B2 fill:#d5f5e3,stroke:#27ae60,stroke-width:3px
```

> **الخطوات:** (1) فصل B عن A. (2) فصل X (ابن B الأيسر). (3) ربط X كابن أيمن لـ A. (4) ربط A كابن أيسر لـ B. الشجرة الفرعية ذات الجذر B أصبحت الآن متوازنة.

في الشيفرة (`merk/src/tree/ops.rs`):

```rust
fn rotate<V>(self, left: bool, ...) -> CostResult<Self, Error> {
    // Detach child on the heavy side
    let (tree, child) = self.detach_expect(left, ...);
    // Detach grandchild from opposite side of child
    let (child, maybe_grandchild) = child.detach(!left, ...);

    // Attach grandchild to original root
    tree.attach(left, maybe_grandchild)
        .maybe_balance(...)
        .flat_map_ok(|tree| {
            // Attach original root as child of promoted node
            child.attach(!left, Some(tree))
                .maybe_balance(...)
        })
}
```

لاحظ كيف يتم استدعاء `maybe_balance` بشكل تكراري — الدوران نفسه قد يُنشئ
اختلالات جديدة تحتاج تصحيحاً إضافياً.

### دوران مزدوج (يسار-يمين)

يُستخدم عندما تكون العقدة **ثقيلة يساراً** (bf = -2) لكن ابنها الأيسر
**ثقيل يميناً** (bf > 0). دوران مفرد لن يُصلح هذا:

**الخطوة 0: قبل** — C ثقيلة يساراً (bf=-2) لكن ابنها الأيسر A يميل يميناً (bf=+1). دوران مفرد لن يُصلح هذا:

```mermaid
graph TD
    C0["C<br/><small>bf=-2</small>"]
    A0["A<br/><small>bf=+1</small>"]
    t4["t₄"]
    t1["t₁"]
    B0["B"]
    t2["t₂"]
    t3["t₃"]
    C0 --- A0
    C0 --- t4
    A0 --- t1
    A0 --- B0
    B0 --- t2
    B0 --- t3
    style C0 fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
```

**الخطوة 1: دوران يساري للابن A** — الآن كلا C وB يميلان يساراً، قابل للإصلاح بدوران مفرد:

```mermaid
graph TD
    C1["C<br/><small>bf=-2</small>"]
    B1["B"]
    t41["t₄"]
    A1["A"]
    t31["t₃"]
    t11["t₁"]
    t21["t₂"]
    C1 --- B1
    C1 --- t41
    B1 --- A1
    B1 --- t31
    A1 --- t11
    A1 --- t21
    style C1 fill:#fdebd0,stroke:#e67e22,stroke-width:2px
```

**الخطوة 2: دوران يميني للجذر C** — متوازن!

```mermaid
graph TD
    B2["B<br/><small>bf=0</small>"]
    A2["A"]
    C2["C"]
    t12["t₁"]
    t22["t₂"]
    t32["t₃"]
    t42["t₄"]
    B2 --- A2
    B2 --- C2
    A2 --- t12
    A2 --- t22
    C2 --- t32
    C2 --- t42
    style B2 fill:#d5f5e3,stroke:#27ae60,stroke-width:3px
```

تكتشف الخوارزمية هذه الحالة بمقارنة اتجاه ميلان الأب مع
عامل توازن الابن:

```rust
fn maybe_balance<V>(self, ...) -> CostResult<Self, Error> {
    let balance_factor = self.balance_factor();
    if balance_factor.abs() <= 1 {
        return Ok(self);  // Already balanced
    }

    let left = balance_factor < 0;  // true if left-heavy

    // Double rotation needed when child leans opposite to parent
    let tree = if left == (self.tree().link(left).unwrap().balance_factor() > 0) {
        // First rotation: rotate child in opposite direction
        self.walk_expect(left, |child|
            child.rotate(!left, ...).map_ok(Some), ...
        )
    } else {
        self
    };

    // Second (or only) rotation
    tree.rotate(left, ...)
}
```

## العمليات الدفعية — البناء والتطبيق

بدلاً من إدراج العناصر واحداً تلو الآخر، يدعم Merk العمليات الدفعية التي
تُطبّق تغييرات متعددة في تمريرة واحدة. هذا حاسم للكفاءة: دفعة
من N عملية على شجرة من M عنصر تستغرق **O((M + N) log(M + N))** وقتاً،
مقابل O(N log M) للإدراجات المتتابعة.

### نوع MerkBatch

```rust
type MerkBatch<K> = [(K, Op)];

enum Op {
    Put(Vec<u8>, TreeFeatureType),  // Insert or update with value and feature type
    PutWithSpecializedCost(...),     // Insert with predefined cost
    PutCombinedReference(...),       // Insert reference with combined hash
    Replace(Vec<u8>, TreeFeatureType),
    Patch { .. },                    // Partial value update
    Delete,                          // Remove key
    DeleteLayered,                   // Remove with layered cost
    DeleteMaybeSpecialized,          // Remove with optional specialized cost
}
```

### الاستراتيجية 1: build() — البناء من الصفر

عندما تكون الشجرة فارغة، `build()` تُنشئ شجرة متوازنة مباشرة من
الدفعة المرتبة باستخدام خوارزمية **تقسيم الوسيط**:

الدفعة المُدخلة (مرتبة): `[A, B, C, D, E, F, G]` — اختر الوسط (D) كجذر، كرر لكل نصف:

```mermaid
graph TD
    D["<b>D</b><br/><small>root = mid(0..6)</small>"]
    B["<b>B</b><br/><small>mid(A,B,C)</small>"]
    F["<b>F</b><br/><small>mid(E,F,G)</small>"]
    A["A"]
    C["C"]
    E["E"]
    G["G"]

    D --- B
    D --- F
    B --- A
    B --- C
    F --- E
    F --- G

    style D fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style B fill:#d5f5e3,stroke:#27ae60
    style F fill:#d5f5e3,stroke:#27ae60
```

> النتيجة: شجرة متوازنة تماماً بارتفاع = 3 = ⌈log₂(7)⌉.

```rust
fn build(batch: &MerkBatch<K>, ...) -> CostResult<Option<TreeNode>, Error> {
    let mid_index = batch.len() / 2;
    let (mid_key, mid_op) = &batch[mid_index];

    // Create root node from middle element
    let mid_tree = TreeNode::new(mid_key.clone(), value.clone(), None, feature_type)?;

    // Recursively build left and right subtrees
    let left = Self::build(&batch[..mid_index], ...);
    let right = Self::build(&batch[mid_index + 1..], ...);

    // Attach children
    mid_tree.attach(true, left).attach(false, right)
}
```

ينتج هذا شجرة بارتفاع ⌈log₂(n)⌉ — متوازنة تماماً.

### الاستراتيجية 2: apply_sorted() — الدمج في شجرة موجودة

عندما تحتوي الشجرة على بيانات بالفعل، `apply_sorted()` يستخدم **البحث الثنائي** لإيجاد
مكان كل عملية دفعية، ثم يُطبّق العمليات بشكل تكراري على الشجر الفرعية
اليسرى واليمنى:

شجرة موجودة مع الدفعة `[(B, Put), (F, Delete)]`:

بحث ثنائي: B < D (اذهب يساراً)، F > D (اذهب يميناً).

**قبل:**
```mermaid
graph TD
    D0["D"] --- C0["C"]
    D0 --- E0["E"]
    E0 --- F0["F"]
    style D0 fill:#d4e6f1,stroke:#2980b9
```

**بعد** تطبيق الدفعة وإعادة التوازن:
```mermaid
graph TD
    D1["D"] --- B1["B"]
    D1 --- E1["E"]
    B1 --- C1["C"]
    style D1 fill:#d5f5e3,stroke:#27ae60
```

> أُدرج B كشجرة فرعية يسرى، وحُذف F من الشجرة الفرعية اليمنى. `maybe_balance()` يُؤكد bf(D) = 0.

```rust
fn apply_sorted(self, batch: &MerkBatch<K>, ...) -> CostResult<...> {
    let search = batch.binary_search_by(|(key, _)| key.cmp(self.tree().key()));

    match search {
        Ok(index) => {
            // Key matches this node — apply operation directly
            // (Put replaces value, Delete removes node)
        }
        Err(mid) => {
            // Key not found — mid is the split point
            // Recurse on left_batch[..mid] and right_batch[mid..]
        }
    }

    self.recurse(batch, mid, exclusive, ...)
}
```

دالة `recurse` تُقسّم الدفعة وتمشي يساراً ويميناً:

```rust
fn recurse(self, batch: &MerkBatch<K>, mid: usize, ...) {
    let left_batch = &batch[..mid];
    let right_batch = &batch[mid..];  // or mid+1 if exclusive

    // Apply left batch to left subtree
    let tree = self.walk(true, |maybe_left| {
        Self::apply_to(maybe_left, left_batch, ...)
    });

    // Apply right batch to right subtree
    let tree = tree.walk(false, |maybe_right| {
        Self::apply_to(maybe_right, right_batch, ...)
    });

    // Re-balance after modifications
    tree.maybe_balance(...)
}
```

### حذف العقد

عند حذف عقدة ذات ابنين، يُرقّي Merk **عقدة الحافة** من
الشجرة الفرعية الأطول. هذا يُقلّل من احتمال الحاجة لدورانات إضافية:

**قبل** — حذف D (لديها ابنان، ارتفاع الشجرة الفرعية اليمنى ≥ اليسرى):

```mermaid
graph TD
    D["D ✗ delete"]
    B0["B"]
    F0["F"]
    A0["A"]
    C0["C"]
    E0["E ← successor"]
    G0["G"]
    D --- B0
    D --- F0
    B0 --- A0
    B0 --- C0
    F0 --- E0
    F0 --- G0
    style D fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style E0 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

**بعد** — E (الأقصى يساراً في الشجرة الفرعية اليمنى = الخلف بالترتيب) يُرقَّى لموقع D:

```mermaid
graph TD
    E1["E"]
    B1["B"]
    F1["F"]
    A1["A"]
    C1["C"]
    G1["G"]
    E1 --- B1
    E1 --- F1
    B1 --- A1
    B1 --- C1
    F1 --- G1
    style E1 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

> **القاعدة:** إذا كان ارتفاع اليسار > اليمين → رقّي حافة اليمين من الشجرة الفرعية اليسرى. إذا كان ارتفاع اليمين ≥ اليسار → رقّي حافة اليسار من الشجرة الفرعية اليمنى. هذا يُقلّل إعادة التوازن بعد الحذف.

```rust
pub fn remove(self, ...) -> CostResult<Option<Self>, Error> {
    let has_left = tree.link(true).is_some();
    let has_right = tree.link(false).is_some();
    let left = tree.child_height(true) > tree.child_height(false);

    if has_left && has_right {
        // Two children: promote edge of taller child
        let (tree, tall_child) = self.detach_expect(left, ...);
        let (_, short_child) = tree.detach_expect(!left, ...);
        tall_child.promote_edge(!left, short_child, ...)
    } else if has_left || has_right {
        // One child: promote it directly
        self.detach_expect(left, ...).1
    } else {
        // Leaf node: just remove
        None
    }
}
```

---
