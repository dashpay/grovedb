# Hashing — Integrita crittografica

Ogni nodo in un albero Merk viene sottoposto a hash per produrre un **hash radice** — un singolo valore di 32 byte che autentica l'intero albero. Qualsiasi modifica a qualsiasi chiave, valore o relazione strutturale produrra un hash radice diverso.

## Gerarchia di hash a tre livelli

Merk utilizza uno schema di hashing a tre livelli, dal piu interno al piu esterno:

Esempio: chiave = `"bob"` (3 byte), valore = `"hello"` (5 byte):

```mermaid
graph LR
    subgraph level1["Livello 1: value_hash"]
        V_IN["varint(5) ‖ &quot;hello&quot;"]
        V_BLAKE["Blake3"]
        V_OUT(["value_hash<br/><small>32 byte</small>"])
        V_IN --> V_BLAKE --> V_OUT
    end

    subgraph level2["Livello 2: kv_hash"]
        K_IN["varint(3) ‖ &quot;bob&quot; ‖ value_hash"]
        K_BLAKE["Blake3"]
        K_OUT(["kv_hash<br/><small>32 byte</small>"])
        K_IN --> K_BLAKE --> K_OUT
    end

    subgraph level3["Livello 3: node_hash"]
        N_LEFT(["left_child_hash<br/><small>32B (o NULL_HASH)</small>"])
        N_KV(["kv_hash<br/><small>32B</small>"])
        N_RIGHT(["right_child_hash<br/><small>32B (o NULL_HASH)</small>"])
        N_BLAKE["Blake3<br/><small>96B input = 2 blocchi</small>"]
        N_OUT(["node_hash<br/><small>32 byte</small>"])
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

> La RADICE dell'albero = `node_hash` del nodo radice — autentica **ogni** chiave, valore e relazione strutturale. I figli assenti usano `NULL_HASH = [0x00; 32]`.

### Livello 1: value_hash

```rust
// merk/src/tree/hash.rs
pub fn value_hash(value: &[u8]) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    let val_length = value.len().encode_var_vec();  // Codifica varint
    hasher.update(val_length.as_slice());
    hasher.update(value);
    // ...
}
```

La lunghezza del valore e **codificata in varint** e anteposta. Questo e critico per la resistenza alle collisioni — senza di essa, `H("AB" || "C")` sarebbe uguale a `H("A" || "BC")`.

### Livello 2: kv_hash

```rust
pub fn kv_hash(key: &[u8], value: &[u8]) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    let key_length = key.len().encode_var_vec();
    hasher.update(key_length.as_slice());
    hasher.update(key);
    let vh = value_hash(value);
    hasher.update(vh.as_slice());  // Hash annidato
    // ...
}
```

Questo lega la chiave al valore. Per la verifica delle prove, esiste anche una variante che prende un value_hash pre-calcolato:

```rust
pub fn kv_digest_to_kv_hash(key: &[u8], value_hash: &CryptoHash) -> CostContext<CryptoHash>
```

Questa viene usata quando il verificatore ha gia il value_hash (ad esempio per sotto-alberi dove il value_hash e un hash combinato).

### Livello 3: node_hash

```rust
pub fn node_hash(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);       // 32 byte
    hasher.update(left);     // 32 byte
    hasher.update(right);    // 32 byte — totale 96 byte
    // Sempre esattamente 2 operazioni di hash (96 byte / blocco da 64 byte = 2)
}
```

Se un figlio e assente, il suo hash e il **NULL_HASH** — 32 byte a zero:

```rust
pub const NULL_HASH: CryptoHash = [0; HASH_LENGTH];  // [0u8; 32]
```

## Blake3 come funzione di hash

GroveDB utilizza **Blake3** per tutto l'hashing. Proprieta chiave:

- **Output a 256 bit** (32 byte)
- **Dimensione blocco**: 64 byte
- **Velocita**: circa 3 volte piu veloce di SHA-256 sull'hardware moderno
- **Streaming**: puo alimentare i dati in modo incrementale

Il costo dell'operazione di hash e calcolato in base a quanti blocchi da 64 byte vengono elaborati:

```rust
let hashes = 1 + (hasher.count() - 1) / 64;  // Numero di operazioni di hash
```

## Codifica con prefisso di lunghezza per la resistenza alle collisioni

Ogni input a lunghezza variabile e prefissato con la sua lunghezza usando la **codifica varint**:

```mermaid
graph LR
    subgraph bad["Senza prefisso di lunghezza — VULNERABILE"]
        BAD1["H(&quot;AB&quot; + &quot;C&quot;) = H(0x41 0x42 0x43)"]
        BAD2["H(&quot;A&quot; + &quot;BC&quot;) = H(0x41 0x42 0x43)"]
        BAD1 --- SAME["STESSO HASH!"]
        BAD2 --- SAME
    end

    subgraph good["Con prefisso di lunghezza — resistente alle collisioni"]
        GOOD1["H([02] 0x41 0x42)<br/><small>varint(2) ‖ &quot;AB&quot;</small>"]
        GOOD2["H([03] 0x41 0x42 0x43)<br/><small>varint(3) ‖ &quot;ABC&quot;</small>"]
        GOOD1 --- DIFF["DIVERSI"]
        GOOD2 --- DIFF
    end

    style bad fill:#fadbd8,stroke:#e74c3c
    style good fill:#d5f5e3,stroke:#27ae60
    style SAME fill:#e74c3c,color:#fff,stroke:#c0392b
    style DIFF fill:#27ae60,color:#fff,stroke:#229954
```

> **Input del value_hash**: `[varint(value.len)] [byte del valore]`
> **Input del kv_hash**: `[varint(key.len)] [byte della chiave] [value_hash: 32 byte]`

Senza prefissi di lunghezza, un attaccante potrebbe creare diverse coppie chiave-valore che producono lo stesso digest. Il prefisso di lunghezza rende cio crittograficamente impraticabile.

## Hashing combinato per elementi speciali

Per **sotto-alberi** e **riferimenti**, il `value_hash` non e semplicemente `H(value)`. Invece, e un **hash combinato** che lega l'elemento al suo obiettivo:

```mermaid
graph LR
    subgraph item["Item regolare"]
        I_val["byte del valore"] --> I_hash["H(len ‖ byte)"] --> I_vh(["value_hash"])
    end

    subgraph subtree["Elemento sotto-albero"]
        S_elem["byte dell'elemento albero"] --> S_hash1["H(len ‖ byte)"]
        S_root(["hash radice<br/>del Merk figlio"])
        S_hash1 --> S_combine["combine_hash()<br/><small>Blake3(a ‖ b)</small>"]
        S_root --> S_combine
        S_combine --> S_vh(["value_hash"])
    end

    subgraph reference["Elemento riferimento"]
        R_elem["byte dell'elemento ref"] --> R_hash1["H(len ‖ byte)"]
        R_target["valore obiettivo"] --> R_hash2["H(len ‖ byte)"]
        R_hash1 --> R_combine["combine_hash()"]
        R_hash2 --> R_combine
        R_combine --> R_vh(["value_hash"])
    end

    style item fill:#eaf2f8,stroke:#2980b9
    style subtree fill:#fef9e7,stroke:#f39c12
    style reference fill:#fdedec,stroke:#e74c3c
```

> **Sotto-albero:** lega l'hash radice del Merk figlio nel genitore. **Riferimento:** lega sia il percorso del riferimento SIA il valore obiettivo. Modificando l'uno o l'altro cambia l'hash radice.

La funzione `combine_hash`:

```rust
pub fn combine_hash(hash_one: &CryptoHash, hash_two: &CryptoHash) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(hash_one);   // 32 byte
    hasher.update(hash_two);   // 32 byte — totale 64 byte, esattamente 1 operazione di hash
    // ...
}
```

Questo e cio che permette a GroveDB di autenticare l'intera gerarchia attraverso un singolo hash radice — il value_hash di ogni albero genitore per un elemento sotto-albero include l'hash radice dell'albero figlio.

## Hashing aggregato per ProvableCountTree

I nodi `ProvableCountTree` includono il conteggio aggregato nell'hash del nodo:

```rust
pub fn node_hash_with_count(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
    count: u64,
) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);                        // 32 byte
    hasher.update(left);                      // 32 byte
    hasher.update(right);                     // 32 byte
    hasher.update(&count.to_be_bytes());      // 8 byte — totale 104 byte
    // Sempre esattamente 2 operazioni di hash (104 < 128 = 2 x 64)
}
```

Cio significa che una prova del conteggio non richiede di rivelare i dati effettivi — il conteggio e incorporato nell'impegno crittografico.

---
