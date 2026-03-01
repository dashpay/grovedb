# Haszowanie -- Integralnosc kryptograficzna

Kazdy wezel w drzewie Merk jest haszowany, aby wytworzyc **hasz korzenia** (root hash) --
pojedyncza 32-bajtowa wartosc uwierzytelniajaca calego drzewo. Jakakolwiek zmiana
w dowolnym kluczu, wartosci lub relacji strukturalnej spowoduje inny hasz korzenia.

## Trzystopniowa hierarchia haszy

Merk uzywa trzystopniowego schematu haszowania, od najwewnetrznego do zewnetrznego:

Przyklad: klucz = `"bob"` (3 bajty), wartosc = `"hello"` (5 bajtow):

```mermaid
graph LR
    subgraph level1["Poziom 1: value_hash"]
        V_IN["varint(5) ‖ &quot;hello&quot;"]
        V_BLAKE["Blake3"]
        V_OUT(["value_hash<br/><small>32 bajty</small>"])
        V_IN --> V_BLAKE --> V_OUT
    end

    subgraph level2["Poziom 2: kv_hash"]
        K_IN["varint(3) ‖ &quot;bob&quot; ‖ value_hash"]
        K_BLAKE["Blake3"]
        K_OUT(["kv_hash<br/><small>32 bajty</small>"])
        K_IN --> K_BLAKE --> K_OUT
    end

    subgraph level3["Poziom 3: node_hash"]
        N_LEFT(["left_child_hash<br/><small>32B (lub NULL_HASH)</small>"])
        N_KV(["kv_hash<br/><small>32B</small>"])
        N_RIGHT(["right_child_hash<br/><small>32B (lub NULL_HASH)</small>"])
        N_BLAKE["Blake3<br/><small>96B wejscia = 2 bloki</small>"]
        N_OUT(["node_hash<br/><small>32 bajty</small>"])
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

> KORZEN drzewa = `node_hash` wezla korzeniowego -- uwierzytelnia **kazdy** klucz, wartosc i relacje strukturalna. Brakujace potomki uzywaja `NULL_HASH = [0x00; 32]`.

### Poziom 1: value_hash

```rust
// merk/src/tree/hash.rs
pub fn value_hash(value: &[u8]) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    let val_length = value.len().encode_var_vec();  // Kodowanie varint
    hasher.update(val_length.as_slice());
    hasher.update(value);
    // ...
}
```

Dlugosc wartosci jest **kodowana jako varint** i dopisywana na poczatku. Jest to
kluczowe dla odpornosci na kolizje -- bez tego `H("AB" || "C")` byloby rowne
`H("A" || "BC")`.

### Poziom 2: kv_hash

```rust
pub fn kv_hash(key: &[u8], value: &[u8]) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    let key_length = key.len().encode_var_vec();
    hasher.update(key_length.as_slice());
    hasher.update(key);
    let vh = value_hash(value);
    hasher.update(vh.as_slice());  // Zagniezdzone haszowanie
    // ...
}
```

To wiaze klucz z wartoscia. Do weryfikacji dowodow istnieje tez wariant, ktory
przyjmuje wstepnie obliczony value_hash:

```rust
pub fn kv_digest_to_kv_hash(key: &[u8], value_hash: &CryptoHash) -> CostContext<CryptoHash>
```

Jest to uzywane, gdy weryfikator juz posiada value_hash (np. dla poddrzew, gdzie
value_hash jest polaczonym haszem).

### Poziom 3: node_hash

```rust
pub fn node_hash(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);       // 32 bajty
    hasher.update(left);     // 32 bajty
    hasher.update(right);    // 32 bajty — lacznie 96 bajtow
    // Zawsze dokladnie 2 operacje haszowania (96 bajtow / 64-bajtowy blok = 2)
}
```

Jezeli potomek jest nieobecny, jego hasz to **NULL_HASH** -- 32 bajty zer:

```rust
pub const NULL_HASH: CryptoHash = [0; HASH_LENGTH];  // [0u8; 32]
```

## Blake3 jako funkcja haszujaca

GroveDB uzywa **Blake3** do calego haszowania. Kluczowe wlasciwosci:

- **256-bitowe wyjscie** (32 bajty)
- **Rozmiar bloku**: 64 bajty
- **Szybkosc**: ok. 3 razy szybszy niz SHA-256 na nowoczesnym sprzecie
- **Strumieniowy**: Moze przyrostowo dostarczac dane

Koszt operacji haszowania jest obliczany na podstawie liczby przetworzonych
64-bajtowych blokow:

```rust
let hashes = 1 + (hasher.count() - 1) / 64;  // Liczba operacji haszowania
```

## Kodowanie prefiksu dlugosci dla odpornosci na kolizje

Kazde wejscie o zmiennej dlugosci jest poprzedzone swoim rozmiarem za pomoca **kodowania varint**:

```mermaid
graph LR
    subgraph bad["Bez prefiksu dlugosci — PODATNE"]
        BAD1["H(&quot;AB&quot; + &quot;C&quot;) = H(0x41 0x42 0x43)"]
        BAD2["H(&quot;A&quot; + &quot;BC&quot;) = H(0x41 0x42 0x43)"]
        BAD1 --- SAME["TEN SAM HASZ!"]
        BAD2 --- SAME
    end

    subgraph good["Z prefiksem dlugosci — odporne na kolizje"]
        GOOD1["H([02] 0x41 0x42)<br/><small>varint(2) ‖ &quot;AB&quot;</small>"]
        GOOD2["H([03] 0x41 0x42 0x43)<br/><small>varint(3) ‖ &quot;ABC&quot;</small>"]
        GOOD1 --- DIFF["ROZNE"]
        GOOD2 --- DIFF
    end

    style bad fill:#fadbd8,stroke:#e74c3c
    style good fill:#d5f5e3,stroke:#27ae60
    style SAME fill:#e74c3c,color:#fff,stroke:#c0392b
    style DIFF fill:#27ae60,color:#fff,stroke:#229954
```

> **Wejscie value_hash**: `[varint(value.len)] [bajty wartosci]`
> **Wejscie kv_hash**: `[varint(key.len)] [bajty klucza] [value_hash: 32 bajty]`

Bez prefiksow dlugosci atakujacy moglby spreparowac rozne pary klucz-wartosc,
ktore haszuja sie do tego samego skrotu. Prefiks dlugosci sprawia, ze jest to
kryptograficznie niewykonalne.

## Haszowanie kombinowane dla elementow specjalnych

Dla **poddrzew** i **referencji**, `value_hash` nie jest po prostu `H(value)`.
Zamiast tego jest to **polaczony hasz** (combined hash), ktory wiaze element
z jego celem:

```mermaid
graph LR
    subgraph item["Zwykly Item"]
        I_val["bajty wartosci"] --> I_hash["H(len ‖ bytes)"] --> I_vh(["value_hash"])
    end

    subgraph subtree["Element poddrzewa"]
        S_elem["bajty elementu drzewa"] --> S_hash1["H(len ‖ bytes)"]
        S_root(["hasz korzenia<br/>potomnego Merk"])
        S_hash1 --> S_combine["combine_hash()<br/><small>Blake3(a ‖ b)</small>"]
        S_root --> S_combine
        S_combine --> S_vh(["value_hash"])
    end

    subgraph reference["Element referencji"]
        R_elem["bajty elementu ref"] --> R_hash1["H(len ‖ bytes)"]
        R_target["wartosc docelowa"] --> R_hash2["H(len ‖ bytes)"]
        R_hash1 --> R_combine["combine_hash()"]
        R_hash2 --> R_combine
        R_combine --> R_vh(["value_hash"])
    end

    style item fill:#eaf2f8,stroke:#2980b9
    style subtree fill:#fef9e7,stroke:#f39c12
    style reference fill:#fdedec,stroke:#e74c3c
```

> **Poddrzewo:** wiaze hasz korzenia potomnego Merk w rodzicu. **Referencja:** wiaze zarowno sciezke referencji, JAK I wartosc docelowa. Zmiana ktoregokolwiek zmienia hasz korzenia.

Funkcja `combine_hash`:

```rust
pub fn combine_hash(hash_one: &CryptoHash, hash_two: &CryptoHash) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(hash_one);   // 32 bajty
    hasher.update(hash_two);   // 32 bajty — lacznie 64 bajty, dokladnie 1 operacja haszowania
    // ...
}
```

To wlasnie pozwala GroveDB uwierzytelnic cala hierarchie przez pojedynczy hasz
korzenia -- value_hash kazdego drzewa nadrzednego dla elementu poddrzewa zawiera
hasz korzenia drzewa potomnego.

## Haszowanie agregacyjne dla ProvableCountTree

Wezly `ProvableCountTree` zawieraja zagregowany licznik w haszu wezla:

```rust
pub fn node_hash_with_count(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
    count: u64,
) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);                        // 32 bajty
    hasher.update(left);                      // 32 bajty
    hasher.update(right);                     // 32 bajty
    hasher.update(&count.to_be_bytes());      // 8 bajtow — lacznie 104 bajty
    // Nadal dokladnie 2 operacje haszowania (104 < 128 = 2 * 64)
}
```

Oznacza to, ze dowod (proof) licznika nie wymaga ujawniania rzeczywistych danych --
licznik jest wbudowany w kryptograficzne zobowiazanie.

---
