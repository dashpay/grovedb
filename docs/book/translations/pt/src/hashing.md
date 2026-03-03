# Hashing — Integridade Criptografica

Cada no em uma arvore Merk e hasheado para produzir um **hash raiz** — um unico valor
de 32 bytes que autentica a arvore inteira. Qualquer alteracao em qualquer chave, valor
ou relacao estrutural produzira um hash raiz diferente.

## Hierarquia de Hash em Tres Niveis

A Merk usa um esquema de hashing em tres niveis, do mais interno ao mais externo:

Exemplo: chave = `"bob"` (3 bytes), valor = `"hello"` (5 bytes):

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
        N_LEFT(["left_child_hash<br/><small>32B (ou NULL_HASH)</small>"])
        N_KV(["kv_hash<br/><small>32B</small>"])
        N_RIGHT(["right_child_hash<br/><small>32B (ou NULL_HASH)</small>"])
        N_BLAKE["Blake3<br/><small>96B entrada = 2 blocos</small>"]
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

> A RAIZ da arvore = `node_hash` do no raiz — autentica **cada** chave, valor e relacao estrutural. Filhos ausentes usam `NULL_HASH = [0x00; 32]`.

### Nivel 1: value_hash

```rust
// merk/src/tree/hash.rs
pub fn value_hash(value: &[u8]) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    let val_length = value.len().encode_var_vec();  // Codificacao varint
    hasher.update(val_length.as_slice());
    hasher.update(value);
    // ...
}
```

O comprimento do valor e **codificado em varint** e prefixado. Isso e critico para
resistencia a colisao — sem isso, `H("AB" || "C")` seria igual a `H("A" || "BC")`.

### Nivel 2: kv_hash

```rust
pub fn kv_hash(key: &[u8], value: &[u8]) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    let key_length = key.len().encode_var_vec();
    hasher.update(key_length.as_slice());
    hasher.update(key);
    let vh = value_hash(value);
    hasher.update(vh.as_slice());  // Hash aninhado
    // ...
}
```

Isso vincula a chave ao valor. Para verificacao de provas, existe tambem uma variante
que recebe um value_hash pre-calculado:

```rust
pub fn kv_digest_to_kv_hash(key: &[u8], value_hash: &CryptoHash) -> CostContext<CryptoHash>
```

Isso e usado quando o verificador ja possui o value_hash (por exemplo, para subarvores
onde o value_hash e um hash combinado).

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
    // Sempre exatamente 2 operacoes de hash (96 bytes / bloco de 64 bytes = 2)
}
```

Se um filho estiver ausente, seu hash e o **NULL_HASH** — 32 bytes zero:

```rust
pub const NULL_HASH: CryptoHash = [0; HASH_LENGTH];  // [0u8; 32]
```

## Blake3 como Funcao de Hash

O GroveDB usa **Blake3** para todo o hashing. Propriedades-chave:

- **Saida de 256 bits** (32 bytes)
- **Tamanho do bloco**: 64 bytes
- **Velocidade**: ~3x mais rapido que SHA-256 em hardware moderno
- **Streaming**: Pode alimentar dados incrementalmente

O custo da operacao de hash e calculado com base em quantos blocos de 64 bytes sao
processados:

```rust
let hashes = 1 + (hasher.count() - 1) / 64;  // Numero de operacoes de hash
```

## Codificacao de Prefixo de Comprimento para Resistencia a Colisao

Toda entrada de comprimento variavel e prefixada com seu comprimento usando **codificacao varint**:

```mermaid
graph LR
    subgraph bad["Sem prefixo de comprimento — VULNERAVEL"]
        BAD1["H(&quot;AB&quot; + &quot;C&quot;) = H(0x41 0x42 0x43)"]
        BAD2["H(&quot;A&quot; + &quot;BC&quot;) = H(0x41 0x42 0x43)"]
        BAD1 --- SAME["MESMO HASH!"]
        BAD2 --- SAME
    end

    subgraph good["Com prefixo de comprimento — resistente a colisao"]
        GOOD1["H([02] 0x41 0x42)<br/><small>varint(2) ‖ &quot;AB&quot;</small>"]
        GOOD2["H([03] 0x41 0x42 0x43)<br/><small>varint(3) ‖ &quot;ABC&quot;</small>"]
        GOOD1 --- DIFF["DIFERENTE"]
        GOOD2 --- DIFF
    end

    style bad fill:#fadbd8,stroke:#e74c3c
    style good fill:#d5f5e3,stroke:#27ae60
    style SAME fill:#e74c3c,color:#fff,stroke:#c0392b
    style DIFF fill:#27ae60,color:#fff,stroke:#229954
```

> **Entrada do value_hash**: `[varint(value.len)] [bytes do valor]`
> **Entrada do kv_hash**: `[varint(key.len)] [bytes da chave] [value_hash: 32 bytes]`

Sem prefixos de comprimento, um atacante poderia criar diferentes pares chave-valor que
produzem o mesmo digest. O prefixo de comprimento torna isso criptograficamente
inviavel.

## Hashing Combinado para Elementos Especiais

Para **subarvores** e **referencias**, o `value_hash` nao e simplesmente `H(value)`.
Em vez disso, e um **hash combinado** que vincula o elemento ao seu alvo:

```mermaid
graph LR
    subgraph item["Item Regular"]
        I_val["bytes do valor"] --> I_hash["H(len ‖ bytes)"] --> I_vh(["value_hash"])
    end

    subgraph subtree["Elemento de Subarvore"]
        S_elem["bytes do elemento tree"] --> S_hash1["H(len ‖ bytes)"]
        S_root(["hash raiz<br/>da Merk filha"])
        S_hash1 --> S_combine["combine_hash()<br/><small>Blake3(a ‖ b)</small>"]
        S_root --> S_combine
        S_combine --> S_vh(["value_hash"])
    end

    subgraph reference["Elemento de Referencia"]
        R_elem["bytes do elemento ref"] --> R_hash1["H(len ‖ bytes)"]
        R_target["valor alvo"] --> R_hash2["H(len ‖ bytes)"]
        R_hash1 --> R_combine["combine_hash()"]
        R_hash2 --> R_combine
        R_combine --> R_vh(["value_hash"])
    end

    style item fill:#eaf2f8,stroke:#2980b9
    style subtree fill:#fef9e7,stroke:#f39c12
    style reference fill:#fdedec,stroke:#e74c3c
```

> **Subarvore:** vincula o hash raiz da Merk filha ao pai. **Referencia:** vincula tanto o caminho da referencia QUANTO o valor alvo. Alterar qualquer um muda o hash raiz.

A funcao `combine_hash`:

```rust
pub fn combine_hash(hash_one: &CryptoHash, hash_two: &CryptoHash) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(hash_one);   // 32 bytes
    hasher.update(hash_two);   // 32 bytes — total 64 bytes, exatamente 1 op de hash
    // ...
}
```

Isso e o que permite ao GroveDB autenticar toda a hierarquia atraves de um unico
hash raiz — o value_hash de cada arvore pai para um elemento de subarvore inclui o
hash raiz da arvore filha.

## Hashing Agregado para ProvableCountTree

Nos de `ProvableCountTree` incluem a contagem agregada no hash do no:

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
    // Ainda exatamente 2 ops de hash (104 < 128 = 2 x 64)
}
```

Isso significa que uma prova de contagem nao requer revelar os dados reais — a contagem
e incorporada no compromisso criptografico.

---
