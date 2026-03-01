# A BulkAppendTree — Armazenamento Append-Only de Alto Desempenho

A BulkAppendTree e a resposta do GroveDB para um desafio especifico de engenharia: como
construir um log append-only de alto desempenho que suporte provas de faixa eficientes,
minimize o hashing por escrita e produza snapshots de chunks imutaveis adequados para
distribuicao via CDN?

Enquanto uma MmrTree (Capitulo 13) e ideal para provas de folhas individuais, a
BulkAppendTree e projetada para cargas de trabalho onde milhares de valores chegam por
bloco e os clientes precisam sincronizar buscando faixas de dados. Ela alcanca isso com
uma **arquitetura de dois niveis**: um buffer de arvore densa de Merkle que absorve os
appends recebidos, e um MMR no nivel de chunk que registra as raizes dos chunks finalizados.

## A Arquitetura de Dois Niveis

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

**Nivel 1 — O Buffer.** Os valores recebidos sao escritos em uma `DenseFixedSizedMerkleTree`
(veja Capitulo 16). A capacidade do buffer e `2^height - 1` posicoes. O hash raiz da arvore
densa (`dense_tree_root`) e atualizado apos cada insercao.

**Nivel 2 — O Chunk MMR.** Quando o buffer enche (atinge `chunk_size` entradas), todas
as entradas sao serializadas em um **blob de chunk** imutavel, uma raiz densa de Merkle e
computada sobre essas entradas, e essa raiz e adicionada como folha ao MMR de chunks.
O buffer e entao limpo.

A **raiz de estado** (state root) combina ambos os niveis em um unico compromisso de 32 bytes
que muda a cada append, garantindo que a arvore Merk pai sempre reflita o estado mais recente.

## Como os Valores Preenchem o Buffer

Cada chamada a `append()` segue esta sequencia:

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

O **buffer E uma DenseFixedSizedMerkleTree** (veja Capitulo 16). Seu hash raiz muda apos
cada insercao, fornecendo um compromisso com todas as entradas atuais do buffer. Esse
hash raiz e o que flui para o calculo da raiz de estado.

## Compactacao de Chunks

Quando o buffer enche (atinge `chunk_size` entradas), a compactacao e acionada automaticamente:

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

Apos a compactacao, o blob de chunk e **permanentemente imutavel** — ele nunca muda
novamente. Isso torna os blobs de chunk ideais para cache em CDN, sincronizacao de
clientes e armazenamento de arquivo.

**Exemplo: 4 appends com chunk_power=2 (chunk_size=4)**

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

## A Raiz de Estado

A raiz de estado vincula ambos os niveis em um unico hash:

```rust
fn compute_state_root(
    mmr_root: &[u8; 32],         // Chunk MMR root (or [0;32] if empty)
    dense_tree_root: &[u8; 32],  // Root hash of current buffer (dense tree)
) -> [u8; 32] {
    blake3("bulk_state" || mmr_root || dense_tree_root)
}
```

O `total_count` e o `chunk_power` **nao** sao incluidos na raiz de estado porque ja sao
autenticados pelo hash de valor da Merk — eles sao campos do `Element` serializado
armazenado no no pai da Merk. A raiz de estado captura apenas os compromissos no nivel
de dados (`mmr_root` e `dense_tree_root`). Este e o hash que flui como o hash filho da
Merk e se propaga ate o hash raiz do GroveDB.

## A Raiz Densa de Merkle

Quando um chunk compacta, as entradas precisam de um unico compromisso de 32 bytes. A
BulkAppendTree usa uma **arvore binaria densa (completa) de Merkle**:

```text
Given entries [e_0, e_1, e_2, e_3]:

Level 0 (leaves):  blake3(e_0)  blake3(e_1)  blake3(e_2)  blake3(e_3)
                      \__________/              \__________/
Level 1:           blake3(h_0 || h_1)       blake3(h_2 || h_3)
                          \____________________/
Level 2 (root):    blake3(h_01 || h_23)  ← this is the dense Merkle root
```

Como `chunk_size` e sempre uma potencia de 2 (por construcao: `1u32 << chunk_power`),
a arvore e sempre completa (sem necessidade de preenchimento ou folhas ficticioas). A
contagem de hashes e exatamente `2 * chunk_size - 1`:
- `chunk_size` hashes de folha (um por entrada)
- `chunk_size - 1` hashes de nos internos

A implementacao da raiz densa de Merkle esta em `grovedb-mmr/src/dense_merkle.rs` e
fornece duas funcoes:
- `compute_dense_merkle_root(hashes)` — a partir de folhas pre-hashadas
- `compute_dense_merkle_root_from_values(values)` — faz hash dos valores primeiro, depois
  constroi a arvore

## Serializacao de Blob de Chunk

Blobs de chunk sao os arquivos imutaveis produzidos pela compactacao. O serializador
seleciona automaticamente o formato mais compacto com base nos tamanhos das entradas:

**Formato de tamanho fixo** (flag `0x01`) — quando todas as entradas tem o mesmo comprimento:

```text
┌──────┬──────────┬─────────────┬─────────┬─────────┬─────────┐
│ 0x01 │ count    │ entry_size  │ entry_0 │ entry_1 │ ...     │
│ 1B   │ 4B (BE)  │ 4B (BE)     │ N bytes │ N bytes │         │
└──────┴──────────┴─────────────┴─────────┴─────────┴─────────┘
Total: 1 + 4 + 4 + (count × entry_size) bytes
```

**Formato de tamanho variavel** (flag `0x00`) — quando as entradas tem comprimentos diferentes:

```text
┌──────┬──────────┬─────────┬──────────┬─────────┬─────────────┐
│ 0x00 │ len_0    │ entry_0 │ len_1    │ entry_1 │ ...         │
│ 1B   │ 4B (BE)  │ N bytes │ 4B (BE)  │ M bytes │             │
└──────┴──────────┴─────────┴──────────┴─────────┴─────────────┘
Total: 1 + Σ(4 + len_i) bytes
```

O formato de tamanho fixo economiza 4 bytes por entrada comparado ao de tamanho variavel,
o que se acumula significativamente para grandes chunks de dados de tamanho uniforme
(como compromissos de hash de 32 bytes). Para 1024 entradas de 32 bytes cada:
- Fixo: `1 + 4 + 4 + 32768 = 32.777 bytes`
- Variavel: `1 + 1024 × (4 + 32) = 36.865 bytes`
- Economia: ~11%

## Layout de Chaves de Armazenamento

Todos os dados da BulkAppendTree residem no namespace de **dados**, com chaves com
prefixos de caractere unico:

| Padrao de chave | Formato | Tamanho | Proposito |
|---|---|---|---|
| `M` | 1 byte | 1B | Chave de metadados |
| `b` + `{index}` | `b` + u32 BE | 5B | Entrada do buffer no indice |
| `e` + `{index}` | `e` + u64 BE | 9B | Blob de chunk no indice |
| `m` + `{pos}` | `m` + u64 BE | 9B | No MMR na posicao |

**Metadados** armazenam `mmr_size` (8 bytes BE). O `total_count` e o `chunk_power` sao
armazenados no proprio Element (na Merk pai), nao nos metadados do namespace de dados.
Essa divisao significa que ler a contagem e uma simples consulta de elemento sem abrir
o contexto de armazenamento de dados.

Chaves de buffer usam indices u32 (0 a `chunk_size - 1`) porque a capacidade do buffer
e limitada pelo `chunk_size` (um u32, calculado como `1u32 << chunk_power`). Chaves de
chunk usam indices u64 porque o numero de chunks completados pode crescer indefinidamente.

## A Struct BulkAppendTree

```rust
pub struct BulkAppendTree<S> {
    pub total_count: u64,                        // Total values ever appended
    pub dense_tree: DenseFixedSizedMerkleTree<S>, // The buffer (dense tree)
}
```

O buffer E uma `DenseFixedSizedMerkleTree` — seu hash raiz e `dense_tree_root`.

**Acessores:**
- `capacity() -> u16`: `dense_tree.capacity()` (= `2^height - 1`)
- `epoch_size() -> u64`: `capacity + 1` (= `2^height`, o numero de entradas por chunk)
- `height() -> u8`: `dense_tree.height()`

**Valores derivados** (nao armazenados):

| Valor | Formula |
|---|---|
| `chunk_count` | `total_count / epoch_size` |
| `buffer_count` | `dense_tree.count()` |
| `mmr_size` | `leaf_count_to_mmr_size(chunk_count)` |

## Operacoes do GroveDB

A BulkAppendTree se integra com o GroveDB atraves de seis operacoes definidas em
`grovedb/src/operations/bulk_append_tree.rs`:

### bulk_append

A operacao mutavel primaria. Segue o padrao de armazenamento nao-Merk padrao do GroveDB:

```text
1. Validate element is BulkAppendTree
2. Open data storage context
3. Load tree from store
4. Append value (may trigger compaction)
5. Update element in parent Merk with new state_root + total_count
6. Propagate changes up through Merk hierarchy
7. Commit transaction
```

O adaptador `AuxBulkStore` envolve as chamadas `get_aux`/`put_aux`/`delete_aux` do GroveDB
e acumula `OperationCost` em um `RefCell` para rastreamento de custos. Custos de hash da
operacao de append sao adicionados a `cost.hash_node_calls`.

### Operacoes de leitura

| Operacao | O que retorna | Armazenamento aux? |
|---|---|---|
| `bulk_get_value(path, key, position)` | Valor na posicao global | Sim — le do blob de chunk ou buffer |
| `bulk_get_chunk(path, key, chunk_index)` | Blob de chunk bruto | Sim — le chave do chunk |
| `bulk_get_buffer(path, key)` | Todas as entradas atuais do buffer | Sim — le chaves do buffer |
| `bulk_count(path, key)` | Contagem total (u64) | Nao — le do elemento |
| `bulk_chunk_count(path, key)` | Chunks completados (u64) | Nao — calculado do elemento |

A operacao `get_value` roteia transparentemente por posicao:

```text
if position < completed_chunks × chunk_size:
    chunk_idx = position / chunk_size
    intra_idx = position % chunk_size
    → read chunk blob, deserialize, return entry[intra_idx]
else:
    buffer_idx = position % chunk_size
    → read buffer_key(buffer_idx)
```

## Operacoes em Lote e Preprocessamento

A BulkAppendTree suporta operacoes em lote atraves da variante `GroveOp::BulkAppend`.
Como `execute_ops_on_path` nao tem acesso ao contexto de armazenamento de dados, todas
as ops BulkAppend devem ser preprocessadas antes de `apply_body`.

O pipeline de preprocessamento:

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

A variante `append_with_mem_buffer` evita problemas de leitura-apos-escrita: entradas do
buffer sao rastreadas em um `Vec<Vec<u8>>` na memoria, para que a compactacao possa
le-las mesmo que o armazenamento transacional nao tenha sido commitado ainda.

## A Trait BulkStore

```rust
pub trait BulkStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String>;
    fn delete(&self, key: &[u8]) -> Result<(), String>;
}
```

Os metodos recebem `&self` (nao `&mut self`) para corresponder ao padrao de mutabilidade
interior do GroveDB onde escritas passam por um lote. A integracao com o GroveDB implementa
isso via `AuxBulkStore` que envolve um `StorageContext` e acumula `OperationCost`.

O `MmrAdapter` faz ponte entre `BulkStore` e as traits `MMRStoreReadOps`/`MMRStoreWriteOps`
do ckb MMR, adicionando um cache de escrita para corretude de leitura-apos-escrita.

## Geracao de Provas

As provas da BulkAppendTree suportam **consultas de faixa** sobre posicoes. A estrutura
de prova captura tudo necessario para um verificador sem estado confirmar que dados
especificos existem na arvore:

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

**Passos de geracao** para uma faixa `[start, end)` (com `chunk_size = 1u32 << chunk_power`):

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

**Por que incluir TODAS as entradas do buffer?** O buffer e uma arvore densa de Merkle
cujo hash raiz compromete cada entrada. O verificador deve reconstruir a arvore a partir
de todas as entradas para verificar o `dense_tree_root`. Como o buffer e limitado pela
`capacity` (no maximo 65.535 entradas), este e um custo razoavel.

## Verificacao de Provas

A verificacao e uma funcao pura — nenhum acesso ao banco de dados e necessario. Ela
realiza cinco verificacoes:

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

Apos a verificacao bem-sucedida, o `BulkAppendTreeProofResult` fornece um metodo
`values_in_range(start, end)` que extrai valores especificos dos blobs de chunk e
entradas de buffer verificados.

## Como se Conecta ao Hash Raiz do GroveDB

A BulkAppendTree e uma **arvore nao-Merk** — ela armazena dados no namespace de dados,
nao em uma subarvore Merk filha. Na Merk pai, o elemento e armazenado como:

```text
Element::BulkAppendTree(total_count, chunk_power, flags)
```

A raiz de estado flui como o hash filho da Merk. O hash do no Merk pai e:

```text
combine_hash(value_hash(element_bytes), state_root)
```

O `state_root` flui como o hash filho da Merk (via o parametro `subtree_root_hash` de
`insert_subtree`). Qualquer alteracao na raiz de estado se propaga para cima atraves da
hierarquia Merk do GroveDB ate o hash raiz.

Em provas V1 (secao 9.6), a prova da Merk pai prova os bytes do elemento e a vinculacao
do hash filho, e a `BulkAppendTreeProof` prova que os dados consultados sao consistentes
com o `state_root` usado como hash filho.

## Rastreamento de Custos

O custo de hash de cada operacao e rastreado explicitamente:

| Operacao | Chamadas Blake3 | Notas |
|---|---|---|
| Append unico (sem compactacao) | 3 | 2 para cadeia de hash do buffer + 1 para raiz de estado |
| Append unico (com compactacao) | 3 + 2C - 1 + ~2 | Cadeia + Merkle densa (C=chunk_size) + push MMR + raiz de estado |
| `get_value` do chunk | 0 | Desserializacao pura, sem hashing |
| `get_value` do buffer | 0 | Consulta direta por chave |
| Geracao de prova | Depende da contagem de chunks | Raiz densa de Merkle por chunk + prova MMR |
| Verificacao de prova | 2C·K - K + B·2 + 1 | K chunks, B entradas de buffer, C chunk_size |

**Custo amortizado por append**: Para chunk_size=1024 (chunk_power=10), a sobrecarga de
compactacao de ~2047 hashes (raiz densa de Merkle) e amortizada sobre 1024 appends,
adicionando ~2 hashes por append. Combinado com os 3 hashes por append, o total
amortizado e de **~5 chamadas blake3 por append** — muito eficiente para uma estrutura
autenticada criptograficamente.

## Comparacao com MmrTree

| | BulkAppendTree | MmrTree |
|---|---|---|
| **Arquitetura** | Dois niveis (buffer + chunk MMR) | MMR unico |
| **Custo de hash por append** | 3 (+ ~2 amortizados para compactacao) | ~2 |
| **Granularidade de prova** | Consultas de faixa sobre posicoes | Provas de folha individual |
| **Snapshots imutaveis** | Sim (blobs de chunk) | Nao |
| **Amigavel para CDN** | Sim (blobs de chunk cacheaveis) | Nao |
| **Entradas de buffer** | Sim (necessario todas para prova) | N/A |
| **Melhor para** | Logs de alto desempenho, sincronizacao em massa | Logs de eventos, consultas individuais |
| **Discriminante do elemento** | 13 | 12 |
| **TreeType** | 9 | 8 |

Escolha MmrTree quando precisar de provas de folha individual com sobrecarga minima.
Escolha BulkAppendTree quando precisar de consultas de faixa, sincronizacao em massa e
snapshots baseados em chunks.

## Arquivos de Implementacao

| Arquivo | Proposito |
|------|---------|
| `grovedb-bulk-append-tree/src/lib.rs` | Raiz do crate, re-exportacoes |
| `grovedb-bulk-append-tree/src/tree/mod.rs` | Struct `BulkAppendTree`, acessores de estado, persistencia de metadados |
| `grovedb-bulk-append-tree/src/tree/append.rs` | `append()`, `append_with_mem_buffer()`, `compact_entries()` |
| `grovedb-bulk-append-tree/src/tree/hash.rs` | `compute_state_root` |
| `grovedb-bulk-append-tree/src/tree/keys.rs` | `META_KEY`, `buffer_key`, `chunk_key`, `mmr_node_key` |
| `grovedb-bulk-append-tree/src/tree/query.rs` | `get_value`, `get_chunk`, `get_buffer` |
| `grovedb-bulk-append-tree/src/tree/mmr_adapter.rs` | `MmrAdapter` com cache de escrita |
| `grovedb-bulk-append-tree/src/chunk.rs` | Serializacao de blob de chunk (formatos fixo + variavel) |
| `grovedb-bulk-append-tree/src/proof.rs` | Geracao e verificacao de `BulkAppendTreeProof` |
| `grovedb-bulk-append-tree/src/store.rs` | Trait `BulkStore` |
| `grovedb-bulk-append-tree/src/error.rs` | Enum `BulkAppendError` |
| `grovedb/src/operations/bulk_append_tree.rs` | Operacoes do GroveDB, `AuxBulkStore`, preprocessamento em lote |
| `grovedb/src/tests/bulk_append_tree_tests.rs` | 27 testes de integracao |

---
