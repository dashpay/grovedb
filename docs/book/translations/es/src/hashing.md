# Hashing — Integridad Criptográfica

Cada nodo en un árbol Merk se hashea para producir un **hash raíz** — un único valor de 32 bytes
que autentica todo el árbol. Cualquier cambio en cualquier clave, valor o
relación estructural producirá un hash raíz diferente.

## Jerarquía de Hash de Tres Niveles

Merk usa un esquema de hashing de tres niveles, del más interno al más externo:

Ejemplo: key = `"bob"` (3 bytes), value = `"hello"` (5 bytes):

```mermaid
graph LR
    subgraph level1["Nivel 1: value_hash"]
        V_IN["varint(5) ‖ &quot;hello&quot;"]
        V_BLAKE["Blake3"]
        V_OUT(["value_hash<br/><small>32 bytes</small>"])
        V_IN --> V_BLAKE --> V_OUT
    end

    subgraph level2["Nivel 2: kv_hash"]
        K_IN["varint(3) ‖ &quot;bob&quot; ‖ value_hash"]
        K_BLAKE["Blake3"]
        K_OUT(["kv_hash<br/><small>32 bytes</small>"])
        K_IN --> K_BLAKE --> K_OUT
    end

    subgraph level3["Nivel 3: node_hash"]
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

> La RAÍZ del árbol = `node_hash` del nodo raíz — autentica **cada** clave, valor y relación estructural. Los hijos faltantes usan `NULL_HASH = [0x00; 32]`.

### Nivel 1: value_hash

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

La longitud del valor se **codifica como varint** y se antepone. Esto es crítico para la
resistencia a colisiones — sin él, `H("AB" ‖ "C")` sería igual a `H("A" ‖ "BC")`.

### Nivel 2: kv_hash

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

Esto vincula la clave al valor. Para la verificación de pruebas, también existe una variante
que toma un value_hash precalculado:

```rust
pub fn kv_digest_to_kv_hash(key: &[u8], value_hash: &CryptoHash) -> CostContext<CryptoHash>
```

Esto se usa cuando el verificador ya tiene el value_hash (por ejemplo, para subárboles
donde value_hash es un hash combinado).

### Nivel 3: node_hash

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

Si un hijo está ausente, su hash es el **NULL_HASH** — 32 bytes cero:

```rust
pub const NULL_HASH: CryptoHash = [0; HASH_LENGTH];  // [0u8; 32]
```

## Blake3 como Función de Hash

GroveDB usa **Blake3** para todo el hashing. Propiedades clave:

- **Salida de 256 bits** (32 bytes)
- **Tamaño de bloque**: 64 bytes
- **Velocidad**: ~3x más rápido que SHA-256 en hardware moderno
- **Streaming**: Puede alimentar datos incrementalmente

El costo de la operación de hash se calcula según cuántos bloques de 64 bytes se
procesan:

```rust
let hashes = 1 + (hasher.count() - 1) / 64;  // Number of hash operations
```

## Codificación de Prefijo de Longitud para Resistencia a Colisiones

Cada entrada de longitud variable se prefija con su longitud usando **codificación varint**:

```mermaid
graph LR
    subgraph bad["Sin prefijo de longitud — VULNERABLE"]
        BAD1["H(&quot;AB&quot; + &quot;C&quot;) = H(0x41 0x42 0x43)"]
        BAD2["H(&quot;A&quot; + &quot;BC&quot;) = H(0x41 0x42 0x43)"]
        BAD1 --- SAME["SAME HASH!"]
        BAD2 --- SAME
    end

    subgraph good["Con prefijo de longitud — resistente a colisiones"]
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

> **Entrada de value_hash**: `[varint(value.len)] [bytes del valor]`
> **Entrada de kv_hash**: `[varint(key.len)] [bytes de la clave] [value_hash: 32 bytes]`

Sin prefijos de longitud, un atacante podría crear diferentes pares clave-valor que
produzcan el mismo digest. El prefijo de longitud hace esto criptográficamente
inviable.

## Hashing Combinado para Elementos Especiales

Para **subárboles** y **referencias**, el `value_hash` no es simplemente `H(value)`.
En su lugar, es un **hash combinado** que vincula el elemento a su objetivo:

```mermaid
graph LR
    subgraph item["Item Regular"]
        I_val["value bytes"] --> I_hash["H(len ‖ bytes)"] --> I_vh(["value_hash"])
    end

    subgraph subtree["Elemento Subtree"]
        S_elem["tree element bytes"] --> S_hash1["H(len ‖ bytes)"]
        S_root(["child Merk<br/>root hash"])
        S_hash1 --> S_combine["combine_hash()<br/><small>Blake3(a ‖ b)</small>"]
        S_root --> S_combine
        S_combine --> S_vh(["value_hash"])
    end

    subgraph reference["Elemento Reference"]
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

> **Subtree:** vincula el hash raíz del Merk hijo en el padre. **Reference:** vincula tanto la ruta de la referencia COMO el valor objetivo. Cambiar cualquiera cambia el hash raíz.

La función `combine_hash`:

```rust
pub fn combine_hash(hash_one: &CryptoHash, hash_two: &CryptoHash) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(hash_one);   // 32 bytes
    hasher.update(hash_two);   // 32 bytes — total 64 bytes, exactly 1 hash op
    // ...
}
```

Esto es lo que permite a GroveDB autenticar toda la jerarquía a través de un único
hash raíz — el value_hash de cada árbol padre para un elemento de subárbol incluye el
hash raíz del árbol hijo.

## Hashing Agregado para ProvableCountTree

Los nodos `ProvableCountTree` incluyen el conteo agregado en el hash del nodo:

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

Esto significa que una prueba del conteo no requiere revelar los datos reales — el conteo
está integrado en el compromiso criptográfico.

---
