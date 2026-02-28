# การแฮช — ความสมบูรณ์ทางการเข้ารหัส

ทุกโหนดใน Merk tree จะถูกแฮชเพื่อสร้าง **root hash (แฮชราก)** — ค่าขนาด 32 ไบต์เพียงค่าเดียวที่รับรองต้นไม้ทั้งต้น การเปลี่ยนแปลงใด ๆ กับ key, ค่า หรือความสัมพันธ์เชิงโครงสร้าง จะทำให้ root hash แตกต่างออกไป

## ลำดับชั้นการแฮชสามระดับ

Merk ใช้ระบบการแฮชสามระดับ จากด้านในสู่ด้านนอก:

ตัวอย่าง: key = `"bob"` (3 ไบต์), value = `"hello"` (5 ไบต์):

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

> ROOT ของต้นไม้ = `node_hash` ของโหนดราก — รับรอง **ทุก** key, ค่า และความสัมพันธ์เชิงโครงสร้าง ลูกที่ไม่มีจะใช้ `NULL_HASH = [0x00; 32]`

### ระดับ 1: value_hash

```rust
// merk/src/tree/hash.rs
pub fn value_hash(value: &[u8]) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    let val_length = value.len().encode_var_vec();  // การเข้ารหัส Varint
    hasher.update(val_length.as_slice());
    hasher.update(value);
    // ...
}
```

ความยาวของค่าถูก **เข้ารหัสด้วย varint** และนำหน้า สิ่งนี้สำคัญสำหรับ collision resistance (ความต้านทานการชน) — หากไม่มีมัน `H("AB" || "C")` จะเท่ากับ `H("A" || "BC")`

### ระดับ 2: kv_hash

```rust
pub fn kv_hash(key: &[u8], value: &[u8]) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    let key_length = key.len().encode_var_vec();
    hasher.update(key_length.as_slice());
    hasher.update(key);
    let vh = value_hash(value);
    hasher.update(vh.as_slice());  // แฮชซ้อน
    // ...
}
```

สิ่งนี้ผูก key กับค่า สำหรับการตรวจสอบ proof ยังมี variant ที่รับ value_hash ที่คำนวณไว้แล้ว:

```rust
pub fn kv_digest_to_kv_hash(key: &[u8], value_hash: &CryptoHash) -> CostContext<CryptoHash>
```

ใช้เมื่อผู้ตรวจสอบ (verifier) มี value_hash อยู่แล้ว (เช่น สำหรับ subtree ที่ value_hash เป็น combined hash)

### ระดับ 3: node_hash

```rust
pub fn node_hash(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);       // 32 ไบต์
    hasher.update(left);     // 32 ไบต์
    hasher.update(right);    // 32 ไบต์ — รวม 96 ไบต์
    // แฮชเสมอ 2 ครั้ง (96 ไบต์ / 64-byte block = 2)
}
```

ถ้าลูกไม่มี แฮชของมันคือ **NULL_HASH** — 32 ไบต์ศูนย์:

```rust
pub const NULL_HASH: CryptoHash = [0; HASH_LENGTH];  // [0u8; 32]
```

## Blake3 เป็นฟังก์ชันแฮช

GroveDB ใช้ **Blake3** สำหรับการแฮชทั้งหมด คุณสมบัติหลัก:

- **เอาต์พุต 256 บิต** (32 ไบต์)
- **ขนาดบล็อก**: 64 ไบต์
- **ความเร็ว**: เร็วกว่า SHA-256 ประมาณ 3 เท่าบนฮาร์ดแวร์สมัยใหม่
- **สตรีมมิ่ง**: สามารถป้อนข้อมูลเข้าแบบทีละส่วนได้

ต้นทุนของการดำเนินการแฮชถูกคำนวณจากจำนวนบล็อก 64 ไบต์ที่ประมวลผล:

```rust
let hashes = 1 + (hasher.count() - 1) / 64;  // จำนวนรอบการแฮช
```

## การเข้ารหัส Length-Prefix เพื่อ Collision Resistance

ทุกอินพุตที่มีความยาวไม่คงที่จะถูกนำหน้าด้วยความยาวโดยใช้ **การเข้ารหัส varint**:

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

> **อินพุต value_hash**: `[varint(value.len)] [value bytes]`
> **อินพุต kv_hash**: `[varint(key.len)] [key bytes] [value_hash: 32 bytes]`

หากไม่มี length prefix ผู้โจมตีสามารถสร้างคู่ key-value ที่แตกต่างกันแต่ให้ digest เดียวกัน length prefix ทำให้สิ่งนี้เป็นไปไม่ได้ทางการเข้ารหัส

## การแฮชแบบรวม (Combined Hashing) สำหรับ Element พิเศษ

สำหรับ **subtree** และ **reference** ค่า `value_hash` ไม่ใช่แค่ `H(value)` เพียงอย่างเดียว แต่เป็น **combined hash (แฮชรวม)** ที่ผูก element กับเป้าหมาย:

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

> **Subtree:** ผูก root hash ของ Merk ลูกเข้ากับแม่ **Reference:** ผูกทั้งเส้นทาง reference และค่าเป้าหมาย การเปลี่ยนอย่างใดอย่างหนึ่งจะเปลี่ยน root hash

ฟังก์ชัน `combine_hash`:

```rust
pub fn combine_hash(hash_one: &CryptoHash, hash_two: &CryptoHash) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(hash_one);   // 32 ไบต์
    hasher.update(hash_two);   // 32 ไบต์ — รวม 64 ไบต์ = แฮชพอดี 1 ครั้ง
    // ...
}
```

สิ่งนี้คือสิ่งที่ช่วยให้ GroveDB รับรองลำดับชั้นทั้งหมดผ่าน root hash เดียว — value_hash ของต้นไม้แม่สำหรับ subtree element จะรวม root hash ของต้นไม้ลูกไว้ด้วย

## การแฮชแบบรวมค่าสำหรับ ProvableCountTree

โหนด `ProvableCountTree` จะรวมจำนวนรวม (aggregate count) ไว้ใน node hash:

```rust
pub fn node_hash_with_count(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
    count: u64,
) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);                        // 32 ไบต์
    hasher.update(left);                      // 32 ไบต์
    hasher.update(right);                     // 32 ไบต์
    hasher.update(&count.to_be_bytes());      // 8 ไบต์ — รวม 104 ไบต์
    // ยังคงแฮชพอดี 2 ครั้ง (104 < 128 = 2 x 64)
}
```

ซึ่งหมายความว่า proof ของจำนวนไม่ต้องเปิดเผยข้อมูลจริง — จำนวนถูกฝังอยู่ในการรับรองทางการเข้ารหัส (cryptographic commitment) แล้ว

---
