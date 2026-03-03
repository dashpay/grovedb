# O Grove Hierarquico — Arvore de Arvores

## Como Subarvores se Aninham Dentro de Arvores Pai

A caracteristica definidora do GroveDB e que uma arvore Merk pode conter elementos que
sao eles mesmos arvores Merk. Isso cria um **namespace hierarquico**:

```mermaid
graph TD
    subgraph root["ARVORE MERK RAIZ — caminho: []"]
        contracts["&quot;contracts&quot;<br/>Tree"]
        identities["&quot;identities&quot;<br/>Tree"]
        balances["&quot;balances&quot;<br/>SumTree, sum=0"]
        contracts --> identities
        contracts --> balances
    end

    subgraph ident["MERK IDENTITIES — caminho: [&quot;identities&quot;]"]
        bob456["&quot;bob456&quot;<br/>Tree"]
        alice123["&quot;alice123&quot;<br/>Tree"]
        eve["&quot;eve&quot;<br/>Item"]
        bob456 --> alice123
        bob456 --> eve
    end

    subgraph bal["MERK BALANCES (SumTree) — caminho: [&quot;balances&quot;]"]
        bob_bal["&quot;bob456&quot;<br/>SumItem(800)"]
    end

    subgraph alice_tree["MERK ALICE123 — caminho: [&quot;identities&quot;, &quot;alice123&quot;]"]
        name["&quot;name&quot;<br/>Item(&quot;Al&quot;)"]
        balance_item["&quot;balance&quot;<br/>SumItem(1000)"]
        docs["&quot;docs&quot;<br/>Tree"]
        name --> balance_item
        name --> docs
    end

    identities -.-> bob456
    balances -.-> bob_bal
    alice123 -.-> name
    docs -.->|"... mais subarvores"| more["..."]

    style root fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style ident fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style bal fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style alice_tree fill:#e8daef,stroke:#8e44ad,stroke-width:2px
    style more fill:none,stroke:none
```

> Cada caixa colorida e uma arvore Merk separada. As setas tracejadas representam os links de portal dos elementos Tree para suas arvores Merk filhas. O caminho para cada Merk e mostrado no seu rotulo.

## Sistema de Enderecamento por Caminho

Cada elemento no GroveDB e enderecado por um **caminho** (path) — uma sequencia de
strings de bytes que navegam da raiz atraves de subarvores ate a chave alvo:

```text
    Caminho: ["identities", "alice123", "name"]

    Passo 1: Na arvore raiz, buscar "identities" → elemento Tree
    Passo 2: Abrir subarvore identities, buscar "alice123" → elemento Tree
    Passo 3: Abrir subarvore alice123, buscar "name" → Item("Alice")
```

Os caminhos sao representados como `Vec<Vec<u8>>` ou usando o tipo `SubtreePath` para
manipulacao eficiente sem alocacao:

```rust
// O caminho para o elemento (todos os segmentos exceto o ultimo)
let path: &[&[u8]] = &[b"identities", b"alice123"];
// A chave dentro da subarvore final
let key: &[u8] = b"name";
```

## Geracao de Prefixo Blake3 para Isolamento de Armazenamento

Cada subarvore no GroveDB recebe seu proprio **namespace de armazenamento isolado** no
RocksDB. O namespace e determinado pelo hashing do caminho com Blake3:

```rust
pub type SubtreePrefix = [u8; 32];

// O prefixo e computado pelo hashing dos segmentos do caminho
// storage/src/rocksdb_storage/storage.rs
```

Por exemplo:

```text
    Caminho: ["identities", "alice123"]
    Prefixo: Blake3(["identities", "alice123"]) = [0xab, 0x3f, ...]  (32 bytes)

    No RocksDB, as chaves para esta subarvore sao armazenadas como:
    [prefixo: 32 bytes][chave_original]

    Entao "name" nesta subarvore se torna:
    [0xab, 0x3f, ...][0x6e, 0x61, 0x6d, 0x65]  ("name")
```

Isso garante:
- Sem colisoes de chave entre subarvores (prefixo de 32 bytes = isolamento de 256 bits)
- Computacao eficiente do prefixo (um unico hash Blake3 sobre os bytes do caminho)
- Dados da subarvore sao colocalizados no RocksDB para eficiencia de cache

## Propagacao do Hash Raiz Atraves da Hierarquia

Quando um valor muda no fundo do grove, a mudanca deve **propagar para cima** para
atualizar o hash raiz:

```text
    Alteracao: Atualizar "name" para "ALICE" em identities/alice123/

    Passo 1: Atualizar valor na arvore Merk de alice123
            → arvore alice123 recebe novo hash raiz: H_alice_new

    Passo 2: Atualizar elemento "alice123" na arvore identities
            → value_hash da arvore identities para "alice123" =
              combine_hash(H(tree_element_bytes), H_alice_new)
            → arvore identities recebe novo hash raiz: H_ident_new

    Passo 3: Atualizar elemento "identities" na arvore raiz
            → value_hash da arvore raiz para "identities" =
              combine_hash(H(tree_element_bytes), H_ident_new)
            → HASH RAIZ muda
```

```mermaid
graph TD
    subgraph step3["PASSO 3: Atualizar arvore raiz"]
        R3["Arvore raiz recalcula:<br/>value_hash para &quot;identities&quot; =<br/>combine_hash(H(tree_bytes), H_ident_NOVO)<br/>→ novo HASH RAIZ"]
    end
    subgraph step2["PASSO 2: Atualizar arvore identities"]
        R2["Arvore identities recalcula:<br/>value_hash para &quot;alice123&quot; =<br/>combine_hash(H(tree_bytes), H_alice_NOVO)<br/>→ novo hash raiz: H_ident_NOVO"]
    end
    subgraph step1["PASSO 1: Atualizar Merk de alice123"]
        R1["Arvore alice123 recalcula:<br/>value_hash(&quot;ALICE&quot;) → novo kv_hash<br/>→ novo hash raiz: H_alice_NOVO"]
    end

    R1 -->|"H_alice_NOVO flui para cima"| R2
    R2 -->|"H_ident_NOVO flui para cima"| R3

    style step1 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style step2 fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style step3 fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

**Antes vs Depois** — nos alterados marcados em vermelho:

```mermaid
graph TD
    subgraph before["ANTES"]
        B_root["Raiz: aabb1122"]
        B_ident["&quot;identities&quot;: cc44.."]
        B_contracts["&quot;contracts&quot;: 1234.."]
        B_balances["&quot;balances&quot;: 5678.."]
        B_alice["&quot;alice123&quot;: ee55.."]
        B_bob["&quot;bob456&quot;: bb22.."]
        B_name["&quot;name&quot;: 7f.."]
        B_docs["&quot;docs&quot;: a1.."]
        B_root --- B_ident
        B_root --- B_contracts
        B_root --- B_balances
        B_ident --- B_alice
        B_ident --- B_bob
        B_alice --- B_name
        B_alice --- B_docs
    end

    subgraph after["DEPOIS"]
        A_root["Raiz: ff990033"]
        A_ident["&quot;identities&quot;: dd88.."]
        A_contracts["&quot;contracts&quot;: 1234.."]
        A_balances["&quot;balances&quot;: 5678.."]
        A_alice["&quot;alice123&quot;: 1a2b.."]
        A_bob["&quot;bob456&quot;: bb22.."]
        A_name["&quot;name&quot;: 3c.."]
        A_docs["&quot;docs&quot;: a1.."]
        A_root --- A_ident
        A_root --- A_contracts
        A_root --- A_balances
        A_ident --- A_alice
        A_ident --- A_bob
        A_alice --- A_name
        A_alice --- A_docs
    end

    style A_root fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style A_ident fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style A_alice fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style A_name fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
```

> Apenas os nos no caminho do valor alterado ate a raiz sao recalculados. Irmaos e outras ramificacoes permanecem inalterados.

A propagacao e implementada por `propagate_changes_with_transaction`, que percorre
o caminho da subarvore modificada ate a raiz, atualizando o hash do elemento de cada pai
ao longo do caminho.

## Exemplo de Estrutura de Grove Multinivel

Aqui esta um exemplo completo mostrando como o Dash Platform estrutura seu estado:

```mermaid
graph TD
    ROOT["Raiz GroveDB"]

    ROOT --> contracts["[01] &quot;data_contracts&quot;<br/>Tree"]
    ROOT --> identities["[02] &quot;identities&quot;<br/>Tree"]
    ROOT --> balances["[03] &quot;balances&quot;<br/>SumTree"]
    ROOT --> pools["[04] &quot;pools&quot;<br/>Tree"]

    contracts --> c1["contract_id_1<br/>Tree"]
    contracts --> c2["contract_id_2<br/>Tree"]
    c1 --> docs["&quot;documents&quot;<br/>Tree"]
    docs --> profile["&quot;profile&quot;<br/>Tree"]
    docs --> note["&quot;note&quot;<br/>Tree"]
    profile --> d1["doc_id_1<br/>Item"]
    profile --> d2["doc_id_2<br/>Item"]
    note --> d3["doc_id_3<br/>Item"]

    identities --> id1["identity_id_1<br/>Tree"]
    identities --> id2["identity_id_2<br/>Tree"]
    id1 --> keys["&quot;keys&quot;<br/>Tree"]
    id1 --> rev["&quot;revision&quot;<br/>Item(u64)"]
    keys --> k1["key_id_1<br/>Item(pubkey)"]
    keys --> k2["key_id_2<br/>Item(pubkey)"]

    balances --> b1["identity_id_1<br/>SumItem(balance)"]
    balances --> b2["identity_id_2<br/>SumItem(balance)"]

    style ROOT fill:#2c3e50,stroke:#2c3e50,color:#fff
    style contracts fill:#d4e6f1,stroke:#2980b9
    style identities fill:#d5f5e3,stroke:#27ae60
    style balances fill:#fef9e7,stroke:#f39c12
    style pools fill:#e8daef,stroke:#8e44ad
```

Cada caixa e uma arvore Merk separada, autenticada ate um unico hash raiz com o qual
os validadores concordam.

---
