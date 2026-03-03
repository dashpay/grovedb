# Hashovani -- Kryptograficka integrita

Kazdy uzel ve stromu Merk je zahasovan, aby se vyprodukovalo **korenove hashe** (root hash)
-- jedina 32-bajtova hodnota, ktera autentizuje cely strom. Jakakoliv zmena
jakehokoli klice, hodnoty nebo strukturalniho vztahu vyprodukovuje odlisny
korenovy hash.

## Triurovnova hierarchie hashu

Merk pouziva triurovnove hashovaci schema, od nejvnitrnejsiho k nejvnejsimu:

Priklad: klic = `"bob"` (3 bajty), hodnota = `"hello"` (5 bajtu):

```mermaid
graph LR
    subgraph level1["Uroven 1: value_hash"]
        V_IN["varint(5) ‖ &quot;hello&quot;"]
        V_BLAKE["Blake3"]
        V_OUT(["value_hash<br/><small>32 bajtu</small>"])
        V_IN --> V_BLAKE --> V_OUT
    end

    subgraph level2["Uroven 2: kv_hash"]
        K_IN["varint(3) ‖ &quot;bob&quot; ‖ value_hash"]
        K_BLAKE["Blake3"]
        K_OUT(["kv_hash<br/><small>32 bajtu</small>"])
        K_IN --> K_BLAKE --> K_OUT
    end

    subgraph level3["Uroven 3: node_hash"]
        N_LEFT(["left_child_hash<br/><small>32B (nebo NULL_HASH)</small>"])
        N_KV(["kv_hash<br/><small>32B</small>"])
        N_RIGHT(["right_child_hash<br/><small>32B (nebo NULL_HASH)</small>"])
        N_BLAKE["Blake3<br/><small>vstup 96B = 2 bloky</small>"]
        N_OUT(["node_hash<br/><small>32 bajtu</small>"])
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

> KOREN stromu = `node_hash` korenoveho uzlu -- autentizuje **kazdy** klic, hodnotu a strukturalni vztah. Chybejici potomci pouzivaji `NULL_HASH = [0x00; 32]`.

### Uroven 1: value_hash

```rust
// merk/src/tree/hash.rs
pub fn value_hash(value: &[u8]) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    let val_length = value.len().encode_var_vec();  // Kodovani varint
    hasher.update(val_length.as_slice());
    hasher.update(value);
    // ...
}
```

Delka hodnoty je **kodovana jako varint** a predrazena. To je klicove pro
odolnost proti kolizim -- bez toho by `H("AB" || "C")` bylo rovno `H("A" || "BC")`.

### Uroven 2: kv_hash

```rust
pub fn kv_hash(key: &[u8], value: &[u8]) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    let key_length = key.len().encode_var_vec();
    hasher.update(key_length.as_slice());
    hasher.update(key);
    let vh = value_hash(value);
    hasher.update(vh.as_slice());  // Vnoreny hash
    // ...
}
```

To vaze klic k hodnote. Pro overovani dukazu existuje take varianta,
ktera prijima predvypocteny value_hash:

```rust
pub fn kv_digest_to_kv_hash(key: &[u8], value_hash: &CryptoHash) -> CostContext<CryptoHash>
```

To se pouziva, kdyz overovatel jiz ma value_hash (napr. pro podstromy,
kde je value_hash kombinovany hash).

### Uroven 3: node_hash

```rust
pub fn node_hash(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);       // 32 bajtu
    hasher.update(left);     // 32 bajtu
    hasher.update(right);    // 32 bajtu — celkem 96 bajtu
    // Vzdy presne 2 hashovaci operace (96 bajtu / 64-bajtovy blok = 2)
}
```

Pokud potomek chybi, jeho hash je **NULL_HASH** -- 32 nulovych bajtu:

```rust
pub const NULL_HASH: CryptoHash = [0; HASH_LENGTH];  // [0u8; 32]
```

## Blake3 jako hashovaci funkce

GroveDB pouziva **Blake3** pro veskere hashovani. Klicove vlastnosti:

- **256-bitovy vystup** (32 bajtu)
- **Velikost bloku**: 64 bajtu
- **Rychlost**: priblizne 3x rychlejsi nez SHA-256 na modernim hardwaru
- **Streamovani**: Muze inkrementalne prijimat data

Naklady hashovaci operace se vypocitaji na zaklade poctu zpracovanych
64-bajtovych bloku:

```rust
let hashes = 1 + (hasher.count() - 1) / 64;  // Pocet hashovacich operaci
```

## Kodovani predpon delky pro odolnost proti kolizim

Kazdy vstup s promenlivou delkou je opatren prefixem jeho delky pomoci
**kodovani varint**:

```mermaid
graph LR
    subgraph bad["Bez prefixu delky — ZRANITELNE"]
        BAD1["H(&quot;AB&quot; + &quot;C&quot;) = H(0x41 0x42 0x43)"]
        BAD2["H(&quot;A&quot; + &quot;BC&quot;) = H(0x41 0x42 0x43)"]
        BAD1 --- SAME["STEJNY HASH!"]
        BAD2 --- SAME
    end

    subgraph good["S prefixem delky — odolne proti kolizim"]
        GOOD1["H([02] 0x41 0x42)<br/><small>varint(2) ‖ &quot;AB&quot;</small>"]
        GOOD2["H([03] 0x41 0x42 0x43)<br/><small>varint(3) ‖ &quot;ABC&quot;</small>"]
        GOOD1 --- DIFF["ODLISNE"]
        GOOD2 --- DIFF
    end

    style bad fill:#fadbd8,stroke:#e74c3c
    style good fill:#d5f5e3,stroke:#27ae60
    style SAME fill:#e74c3c,color:#fff,stroke:#c0392b
    style DIFF fill:#27ae60,color:#fff,stroke:#229954
```

> **Vstup value_hash**: `[varint(value.len)] [bajty hodnoty]`
> **Vstup kv_hash**: `[varint(key.len)] [bajty klice] [value_hash: 32 bajtu]`

Bez prefixu delky by utocnik mohl vytvorit ruzne pary klicu a hodnot, ktere
se zahasuji na stejny digest. Prefix delky to cini kryptograficky
neproveditelnym.

## Kombinovane hashovani pro specialni elementy

Pro **podstromy** a **reference** neni `value_hash` jednodusse `H(value)`.
Misto toho je to **kombinovany hash**, ktery vaze element k jeho cili:

```mermaid
graph LR
    subgraph item["Bezny Item"]
        I_val["bajty hodnoty"] --> I_hash["H(len ‖ bajty)"] --> I_vh(["value_hash"])
    end

    subgraph subtree["Element Subtree"]
        S_elem["bajty elementu stromu"] --> S_hash1["H(len ‖ bajty)"]
        S_root(["korenovy hash<br/>podrizeneho Merk"])
        S_hash1 --> S_combine["combine_hash()<br/><small>Blake3(a ‖ b)</small>"]
        S_root --> S_combine
        S_combine --> S_vh(["value_hash"])
    end

    subgraph reference["Element Reference"]
        R_elem["bajty elementu reference"] --> R_hash1["H(len ‖ bajty)"]
        R_target["cilova hodnota"] --> R_hash2["H(len ‖ bajty)"]
        R_hash1 --> R_combine["combine_hash()"]
        R_hash2 --> R_combine
        R_combine --> R_vh(["value_hash"])
    end

    style item fill:#eaf2f8,stroke:#2980b9
    style subtree fill:#fef9e7,stroke:#f39c12
    style reference fill:#fdedec,stroke:#e74c3c
```

> **Podstrom:** vaze korenovy hash podrizeneho Merk do rodice. **Reference:** vaze jak cestu reference, TAK i cilovou hodnotu. Zmena kterehokoliv zmeni korenovy hash.

Funkce `combine_hash`:

```rust
pub fn combine_hash(hash_one: &CryptoHash, hash_two: &CryptoHash) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(hash_one);   // 32 bajtu
    hasher.update(hash_two);   // 32 bajtu — celkem 64 bajtu, presne 1 hashovaci operace
    // ...
}
```

Prave toto umoznuje GroveDB autentizovat celou hierarchii prostrednictvim
jedineho korenoveho hashe -- value_hash kazdeho rodicovskeho stromu pro element
podstromu zahrnuje korenovy hash podrizeneho stromu.

## Agregatni hashovani pro ProvableCountTree

Uzly `ProvableCountTree` zahrnovaji agregatni pocet do hashe uzlu:

```rust
pub fn node_hash_with_count(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
    count: u64,
) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);                        // 32 bajtu
    hasher.update(left);                      // 32 bajtu
    hasher.update(right);                     // 32 bajtu
    hasher.update(&count.to_be_bytes());      // 8 bajtu — celkem 104 bajtu
    // Stale presne 2 hashovaci operace (104 < 128 = 2 * 64)
}
```

To znamena, ze dukaz poctu nevyzaduje odhaleni skutecnych dat -- pocet je
zapecen do kryptografickeho zavazku.

---
