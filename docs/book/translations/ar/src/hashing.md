# التجزئة — السلامة التشفيرية

كل عقدة في شجرة Merk تُجزَّأ لإنتاج **تجزئة جذر** (root hash) — قيمة واحدة من 32 بايت
توثّق الشجرة بأكملها. أي تغيير في أي مفتاح أو قيمة أو
علاقة هيكلية سينتج تجزئة جذر مختلفة.

## التسلسل الهرمي للتجزئة من ثلاث مستويات

يستخدم Merk مخططاً للتجزئة من ثلاث مستويات، من الأعمق إلى الأبعد:

مثال: key = `"bob"` (3 بايت)، value = `"hello"` (5 بايت):

```mermaid
graph LR
    subgraph level1["Level 1: value_hash"]
        V_IN["varint(5) ‖ &quot;hello&quot;"]
        V_BLAKE["Blake3"]
        V_OUT(["value_hash<br/><small>32 bytes</small>"])
        V_IN --> V_BLAKE --> V_OUT
    end

    subgraph level2["Level 2: kv_hash"]
        K_IN["varint(3) ‖ &quot;bob&quot; ‖ value_hash"]
        K_BLAKE["Blake3"]
        K_OUT(["kv_hash<br/><small>32 bytes</small>"])
        K_IN --> K_BLAKE --> K_OUT
    end

    subgraph level3["Level 3: node_hash"]
        N_LEFT(["left_child_hash<br/><small>32B (or NULL_HASH)</small>"])
        N_KV(["kv_hash<br/><small>32B</small>"])
        N_RIGHT(["right_child_hash<br/><small>32B (or NULL_HASH)</small>"])
        N_BLAKE["Blake3<br/><small>96B input = 2 blocks</small>"]
        N_OUT(["node_hash<br/><small>32 bytes</small>"])
        N_LEFT --> N_BLAKE
        N_KV --> N_BLAKE
        N_RIGHT --> N_BLAKE
        N_BLAKE --> N_OUT
    end

    V_OUT -.-> K_IN
    K_OUT -.-> N_KV

    style level1 fill:#eaf2f8,stroke:#2980b9
    style level2 fill:#fef9e7,stroke:#f39c12
    style level3 fill:#fdedec,stroke:#e74c3c
```

> جذر الشجرة = `node_hash` لعقدة الجذر — يوثّق **كل** مفتاح وقيمة وعلاقة هيكلية. الأبناء المفقودون يستخدمون `NULL_HASH = [0x00; 32]`.

### المستوى 1: value_hash

```rust
// merk/src/tree/hash.rs
pub fn value_hash(value: &[u8]) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    let val_length = value.len().encode_var_vec();  // Varint encoding
    hasher.update(val_length.as_slice());
    hasher.update(value);
    // ...
}
```

طول القيمة يتم **ترميزه كمتغير عدد صحيح** (varint) ويُلحق في المقدمة. هذا حاسم لمقاومة
التصادم — بدونه، `H("AB" ‖ "C")` سيُساوي `H("A" ‖ "BC")`.

### المستوى 2: kv_hash

```rust
pub fn kv_hash(key: &[u8], value: &[u8]) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    let key_length = key.len().encode_var_vec();
    hasher.update(key_length.as_slice());
    hasher.update(key);
    let vh = value_hash(value);
    hasher.update(vh.as_slice());  // Nested hash
    // ...
}
```

هذا يربط المفتاح بالقيمة. للتحقق من البراهين، هناك أيضاً متغير
يأخذ value_hash محسوبة مسبقاً:

```rust
pub fn kv_digest_to_kv_hash(key: &[u8], value_hash: &CryptoHash) -> CostContext<CryptoHash>
```

يُستخدم هذا عندما يمتلك المُحقّق value_hash بالفعل (مثلاً، للأشجار الفرعية
حيث value_hash هي تجزئة مُركَّبة).

### المستوى 3: node_hash

```rust
pub fn node_hash(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);       // 32 bytes
    hasher.update(left);     // 32 bytes
    hasher.update(right);    // 32 bytes — total 96 bytes
    // Always exactly 2 hash operations (96 bytes / 64-byte block = 2)
}
```

إذا كان أحد الأبناء غائباً، تجزئته هي **NULL_HASH** — 32 بايت صفرية:

```rust
pub const NULL_HASH: CryptoHash = [0; HASH_LENGTH];  // [0u8; 32]
```

## Blake3 كدالة التجزئة

يستخدم GroveDB **Blake3** لجميع عمليات التجزئة. الخصائص الرئيسية:

- **مخرجات 256 بت** (32 بايت)
- **حجم الكتلة**: 64 بايت
- **السرعة**: أسرع بنحو 3 مرات من SHA-256 على الأجهزة الحديثة
- **تدفقية**: يمكن تغذية البيانات بشكل تزايدي

تُحسب تكلفة عملية التجزئة بناءً على عدد كتل 64 بايت التي تمت
معالجتها:

```rust
let hashes = 1 + (hasher.count() - 1) / 64;  // Number of hash operations
```

## ترميز بادئة الطول لمقاومة التصادم

كل مُدخل متغير الطول يُسبَق بطوله باستخدام **ترميز varint**:

```mermaid
graph LR
    subgraph bad["Without length prefix — VULNERABLE"]
        BAD1["H(&quot;AB&quot; + &quot;C&quot;) = H(0x41 0x42 0x43)"]
        BAD2["H(&quot;A&quot; + &quot;BC&quot;) = H(0x41 0x42 0x43)"]
        BAD1 --- SAME["SAME HASH!"]
        BAD2 --- SAME
    end

    subgraph good["With length prefix — collision resistant"]
        GOOD1["H([02] 0x41 0x42)<br/><small>varint(2) ‖ &quot;AB&quot;</small>"]
        GOOD2["H([03] 0x41 0x42 0x43)<br/><small>varint(3) ‖ &quot;ABC&quot;</small>"]
        GOOD1 --- DIFF["DIFFERENT"]
        GOOD2 --- DIFF
    end

    style bad fill:#fadbd8,stroke:#e74c3c
    style good fill:#d5f5e3,stroke:#27ae60
    style SAME fill:#e74c3c,color:#fff,stroke:#c0392b
    style DIFF fill:#27ae60,color:#fff,stroke:#229954
```

> **مُدخل value_hash**: `[varint(value.len)] [value bytes]`
> **مُدخل kv_hash**: `[varint(key.len)] [key bytes] [value_hash: 32 bytes]`

بدون بادئات الطول، يمكن للمهاجم صياغة أزواج مفتاح-قيمة مختلفة تُنتج
نفس الملخص. بادئة الطول تجعل هذا مستحيلاً تشفيرياً.

## التجزئة المُركَّبة للعناصر الخاصة

للـ **أشجار الفرعية** و**المراجع**، `value_hash` ليست ببساطة `H(value)`.
بدلاً من ذلك، هي **تجزئة مُركَّبة** تربط العنصر بهدفه:

```mermaid
graph LR
    subgraph item["Regular Item"]
        I_val["value bytes"] --> I_hash["H(len ‖ bytes)"] --> I_vh(["value_hash"])
    end

    subgraph subtree["Subtree Element"]
        S_elem["tree element bytes"] --> S_hash1["H(len ‖ bytes)"]
        S_root(["child Merk<br/>root hash"])
        S_hash1 --> S_combine["combine_hash()<br/><small>Blake3(a ‖ b)</small>"]
        S_root --> S_combine
        S_combine --> S_vh(["value_hash"])
    end

    subgraph reference["Reference Element"]
        R_elem["ref element bytes"] --> R_hash1["H(len ‖ bytes)"]
        R_target["target value"] --> R_hash2["H(len ‖ bytes)"]
        R_hash1 --> R_combine["combine_hash()"]
        R_hash2 --> R_combine
        R_combine --> R_vh(["value_hash"])
    end

    style item fill:#eaf2f8,stroke:#2980b9
    style subtree fill:#fef9e7,stroke:#f39c12
    style reference fill:#fdedec,stroke:#e74c3c
```

> **الشجرة الفرعية:** تربط تجزئة جذر شجرة Merk الابن في الأب. **المرجع:** يربط كلاً من مسار المرجع والقيمة الهدف. تغيير أي منهما يُغيّر تجزئة الجذر.

دالة `combine_hash`:

```rust
pub fn combine_hash(hash_one: &CryptoHash, hash_two: &CryptoHash) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(hash_one);   // 32 bytes
    hasher.update(hash_two);   // 32 bytes — total 64 bytes, exactly 1 hash op
    // ...
}
```

هذا ما يسمح لـ GroveDB بتوثيق التسلسل الهرمي بالكامل من خلال تجزئة
جذر واحدة — كل value_hash لشجرة أب لعنصر شجرة فرعية تتضمن
تجزئة جذر الشجرة الابن.

## التجزئة التجميعية لـ ProvableCountTree

عقد `ProvableCountTree` تُضمّن العدد التجميعي في تجزئة العقدة:

```rust
pub fn node_hash_with_count(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
    count: u64,
) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);                        // 32 bytes
    hasher.update(left);                      // 32 bytes
    hasher.update(right);                     // 32 bytes
    hasher.update(&count.to_be_bytes());      // 8 bytes — total 104 bytes
    // Still exactly 2 hash ops (104 < 128 = 2 × 64)
}
```

هذا يعني أن برهان العدد لا يتطلب كشف البيانات الفعلية — العدد
مُدمج في الالتزام التشفيري.

---
