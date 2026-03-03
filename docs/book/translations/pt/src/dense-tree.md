# A DenseAppendOnlyFixedSizeTree — Armazenamento Denso de Merkle de Capacidade Fixa

A DenseAppendOnlyFixedSizeTree e uma arvore binaria completa de altura fixa onde
**cada no** — tanto interno quanto folha — armazena um valor de dados. As posicoes sao
preenchidas sequencialmente em ordem de nivel (BFS): raiz primeiro (posicao 0), depois
da esquerda para a direita em cada nivel. Nenhum hash intermediario e persistido; o hash
raiz e recomputado em tempo real por hashing recursivo das folhas ate a raiz.

Este design e ideal para estruturas de dados pequenas e limitadas onde a capacidade maxima
e conhecida antecipadamente e voce precisa de O(1) append, O(1) recuperacao por posicao
e um compromisso compacto de hash raiz de 32 bytes que muda apos cada insercao.

## Estrutura da Arvore

Uma arvore de altura *h* tem capacidade `2^h - 1` posicoes. As posicoes usam indexacao
em ordem de nivel baseada em 0:

```text
Height 3 tree (capacity = 7):

              pos 0          ← root (level 0)
             /     \
          pos 1    pos 2     ← level 1
         /   \    /   \
       pos 3 pos 4 pos 5 pos 6  ← level 2 (leaves)

Navigation:
  left_child(i)  = 2i + 1
  right_child(i) = 2i + 2
  parent(i)      = (i - 1) / 2
  is_leaf(i)     = 2i + 1 >= capacity
```

Os valores sao adicionados sequencialmente: o primeiro valor vai para a posicao 0 (raiz),
depois posicao 1, 2, 3, e assim por diante. Isso significa que a raiz sempre tem dados,
e a arvore preenche em ordem de nivel — a ordem de travessia mais natural para uma arvore
binaria completa.

## Computacao de Hash

O hash raiz nao e armazenado separadamente — ele e recomputado do zero sempre que
necessario. O algoritmo recursivo visita apenas as posicoes preenchidas:

```text
hash(position, store):
  value = store.get(position)

  if position is unfilled (>= count):
    return [0; 32]                                    ← empty sentinel

  value_hash = blake3(value)
  left_hash  = hash(2 * position + 1, store)
  right_hash = hash(2 * position + 2, store)
  return blake3(value_hash || left_hash || right_hash)
```

**Propriedades-chave:**
- Todos os nos (folha e interno): `blake3(blake3(value) || H(left) || H(right))`
- Nos folha: left_hash e right_hash sao ambos `[0; 32]` (filhos nao preenchidos)
- Posicoes nao preenchidas: `[0u8; 32]` (hash zero)
- Arvore vazia (count = 0): `[0u8; 32]`

**Nenhuma tag de separacao de dominio folha/interno e usada.** A estrutura da arvore
(`height` e `count`) e autenticada externamente no `Element::DenseAppendOnlyFixedSizeTree`
pai, que flui pela hierarquia Merk. O verificador sempre sabe exatamente quais posicoes
sao folhas vs nos internos a partir da altura e contagem, entao um atacante nao pode
substituir um pelo outro sem quebrar a cadeia de autenticacao pai.

Isso significa que o hash raiz codifica um compromisso com cada valor armazenado e sua
posicao exata na arvore. Alterar qualquer valor (se fosse mutavel) cascatearia atraves de
todos os hashes ancestrais ate a raiz.

**Custo de hash:** Computar o hash raiz visita todas as posicoes preenchidas mais quaisquer
filhos nao preenchidos. Para uma arvore com *n* valores, o pior caso e O(*n*) chamadas
blake3. Isso e aceitavel porque a arvore e projetada para capacidades pequenas e limitadas
(altura maxima 16, maximo 65.535 posicoes).

## A Variante do Elemento

```rust
Element::DenseAppendOnlyFixedSizeTree(
    u16,                   // count — number of values stored (max 65,535)
    u8,                    // height — immutable after creation (1..=16)
    Option<ElementFlags>,  // flags — storage flags
)
```

| Campo | Tipo | Descricao |
|---|---|---|
| `count` | `u16` | Numero de valores inseridos ate agora (max 65.535) |
| `height` | `u8` | Altura da arvore (1..=16), imutavel apos criacao |
| `flags` | `Option<ElementFlags>` | Flags de armazenamento opcionais |

O hash raiz NAO e armazenado no Element — ele flui como o hash filho da Merk
via o parametro `subtree_root_hash` de `insert_subtree`.

**Discriminante:** 14 (ElementType), TreeType = 10

**Tamanho do custo:** `DENSE_TREE_COST_SIZE = 6` bytes (2 count + 1 height + 1
discriminante + 2 overhead)

## Layout de Armazenamento

Assim como MmrTree e BulkAppendTree, a DenseAppendOnlyFixedSizeTree armazena dados no
namespace de **dados** (nao em uma Merk filha). Os valores sao chaveados pela sua posicao
como `u64` big-endian:

```text
Subtree path: blake3(parent_path || key)

Storage keys:
  [0, 0, 0, 0, 0, 0, 0, 0] → value at position 0 (root)
  [0, 0, 0, 0, 0, 0, 0, 1] → value at position 1
  [0, 0, 0, 0, 0, 0, 0, 2] → value at position 2
  ...
```

O proprio Element (armazenado na Merk pai) carrega o `count` e `height`. O hash raiz
flui como o hash filho da Merk. Isso significa:
- **Ler o hash raiz** requer recomputacao a partir do armazenamento (O(n) hashing)
- **Ler um valor por posicao e O(1)** — consulta unica ao armazenamento
- **Inserir e O(n) hashing** — uma escrita no armazenamento + recomputacao completa do hash raiz

## Operacoes

### `dense_tree_insert(path, key, value, tx, grove_version)`

Adiciona um valor na proxima posicao disponivel. Retorna `(root_hash, position)`.

```text
Step 1: Read element, extract (count, height)
Step 2: Check capacity: if count >= 2^height - 1 → error
Step 3: Build subtree path, open storage context
Step 4: Write value to position = count
Step 5: Reconstruct DenseFixedSizedMerkleTree from state
Step 6: Call tree.insert(value, store) → (root_hash, position, hash_calls)
Step 7: Update element with new root_hash and count + 1
Step 8: Propagate changes up through Merk hierarchy
Step 9: Commit transaction
```

### `dense_tree_get(path, key, position, tx, grove_version)`

Recupera o valor em uma posicao dada. Retorna `None` se posicao >= count.

### `dense_tree_root_hash(path, key, tx, grove_version)`

Retorna o hash raiz armazenado no elemento. Este e o hash computado durante a insercao
mais recente — nenhuma recomputacao necessaria.

### `dense_tree_count(path, key, tx, grove_version)`

Retorna o numero de valores armazenados (o campo `count` do elemento).

## Operacoes em Lote

A variante `GroveOp::DenseTreeInsert` suporta insercao em lote atraves do pipeline padrao
de lote do GroveDB:

```rust
let ops = vec![
    QualifiedGroveDbOp::dense_tree_insert_op(
        vec![b"parent".to_vec()],
        b"my_dense_tree".to_vec(),
        b"value_data".to_vec(),
    ),
];
db.apply_batch(ops, None, None, grove_version)?;
```

**Preprocessamento:** Assim como todos os tipos de arvore nao-Merk, ops `DenseTreeInsert`
sao preprocessadas antes da execucao do corpo principal do lote. O metodo
`preprocess_dense_tree_ops`:

1. Agrupa todas as ops `DenseTreeInsert` por `(path, key)`
2. Para cada grupo, executa as insercoes sequencialmente (lendo o elemento, inserindo
   cada valor, atualizando o hash raiz)
3. Converte cada grupo em uma op `ReplaceNonMerkTreeRoot` que carrega o `root_hash`
   final e `count` atraves da maquinaria padrao de propagacao

Multiplas insercoes na mesma arvore densa dentro de um unico lote sao suportadas — elas
sao processadas em ordem e a verificacao de consistencia permite chaves duplicadas para
este tipo de op.

**Propagacao:** O hash raiz e a contagem fluem atraves da variante
`NonMerkTreeMeta::DenseTree` em `ReplaceNonMerkTreeRoot`, seguindo o mesmo padrao de
MmrTree e BulkAppendTree.

## Provas

DenseAppendOnlyFixedSizeTree suporta **provas de subconsulta V1** via a variante
`ProofBytes::DenseTree`. Posicoes individuais podem ser provadas contra o hash raiz da
arvore usando provas de inclusao que carregam valores ancestrais e hashes de subarvores
irmas.

### Estrutura do Caminho de Autenticacao

Como nos internos fazem hash do seu **proprio valor** (nao apenas hashes dos filhos),
o caminho de autenticacao difere de uma arvore de Merkle padrao. Para verificar uma folha
na posicao `p`, o verificador precisa de:

1. **O valor da folha** (a entrada provada)
2. **Hashes de valor dos ancestrais** para cada no interno no caminho de `p` ate a raiz
   (apenas o hash de 32 bytes, nao o valor completo)
3. **Hashes de subarvores irmas** para cada filho que NAO esta no caminho

Como todos os nos usam `blake3(H(value) || H(left) || H(right))` (sem tags de dominio),
a prova carrega apenas hashes de valor de 32 bytes para ancestrais — nao valores completos.
Isso mantem as provas compactas independente do tamanho dos valores individuais.

```rust
pub struct DenseTreeProof {
    pub entries: Vec<(u16, Vec<u8>)>,            // proved (position, value) pairs
    pub node_value_hashes: Vec<(u16, [u8; 32])>, // ancestor value hashes on the auth path
    pub node_hashes: Vec<(u16, [u8; 32])>,       // precomputed sibling subtree hashes
}
```

> **Nota:** `height` e `count` nao estao na struct de prova — o verificador os obtem do
> Element pai, que e autenticado pela hierarquia Merk.

### Exemplo Detalhado

Arvore com height=3, capacity=7, count=5, provando posicao 4:

```text
        0
       / \
      1   2
     / \ / \
    3  4 5  6
```

Caminho de 4 ate a raiz: `4 → 1 → 0`. Conjunto expandido: `{0, 1, 4}`.

A prova contem:
- **entries**: `[(4, value[4])]` — a posicao provada
- **node_value_hashes**: `[(0, H(value[0])), (1, H(value[1]))]` — hashes de valor dos
  ancestrais (32 bytes cada, nao valores completos)
- **node_hashes**: `[(2, H(subtree_2)), (3, H(node_3))]` — irmaos fora do caminho

A verificacao recomputa o hash raiz de baixo para cima:
1. `H(4) = blake3(blake3(value[4]) || [0;32] || [0;32])` — folha (filhos nao preenchidos)
2. `H(3)` — de `node_hashes`
3. `H(1) = blake3(H(value[1]) || H(3) || H(4))` — interno usa hash de valor de
   `node_value_hashes`
4. `H(2)` — de `node_hashes`
5. `H(0) = blake3(H(value[0]) || H(1) || H(2))` — raiz usa hash de valor de
   `node_value_hashes`
6. Compara `H(0)` com o hash raiz esperado

### Provas de Multiplas Posicoes

Ao provar multiplas posicoes, o conjunto expandido mescla caminhos de autenticacao
sobrepostos. Ancestrais compartilhados sao incluidos apenas uma vez, tornando provas
de multiplas posicoes mais compactas do que provas de posicao unica independentes.

### Limitacao V0

Provas V0 nao podem descer em arvores densas. Se uma consulta V0 corresponder a uma
`DenseAppendOnlyFixedSizeTree` com uma subconsulta, o sistema retorna
`Error::NotSupported` direcionando o chamador a usar `prove_query_v1`.

### Codificacao de Chave de Consulta

Posicoes de arvore densa sao codificadas como chaves de consulta **u16 big-endian**
(2 bytes), diferente de MmrTree e BulkAppendTree que usam u64. Todos os tipos padrao
de `QueryItem` de faixa sao suportados.

## Comparacao com Outras Arvores Nao-Merk

| | DenseTree | MmrTree | BulkAppendTree | CommitmentTree |
|---|---|---|---|---|
| **Discriminante do elemento** | 14 | 12 | 13 | 11 |
| **TreeType** | 10 | 8 | 9 | 7 |
| **Capacidade** | Fixa (`2^h - 1`, max 65.535) | Ilimitada | Ilimitada | Ilimitada |
| **Modelo de dados** | Cada posicao armazena um valor | Apenas folhas | Buffer de arvore densa + chunks | Apenas folhas |
| **Hash no Element?** | Nao (flui como hash filho) | Nao (flui como hash filho) | Nao (flui como hash filho) | Nao (flui como hash filho) |
| **Custo de insercao (hashing)** | O(n) blake3 | O(1) amortizado | O(1) amortizado | ~33 Sinsemilla |
| **Tamanho do custo** | 6 bytes | 11 bytes | 12 bytes | 12 bytes |
| **Suporte a provas** | V1 (Dense) | V1 (MMR) | V1 (Bulk) | V1 (CommitmentTree) |
| **Melhor para** | Estruturas pequenas limitadas | Logs de eventos | Logs de alto desempenho | Compromissos ZK |

**Quando escolher DenseAppendOnlyFixedSizeTree:**
- O numero maximo de entradas e conhecido no momento da criacao
- Voce precisa que cada posicao (incluindo nos internos) armazene dados
- Voce quer o modelo de dados mais simples possivel sem crescimento ilimitado
- Recomputacao de hash raiz O(n) e aceitavel (alturas pequenas de arvore)

**Quando NAO escolher:**
- Voce precisa de capacidade ilimitada → use MmrTree ou BulkAppendTree
- Voce precisa de compatibilidade ZK → use CommitmentTree

## Exemplo de Uso

```rust
use grovedb::Element;
use grovedb_version::version::GroveVersion;

let grove_version = GroveVersion::latest();

// Create a dense tree of height 4 (capacity = 15 values)
db.insert(
    &[b"state"],
    b"validator_slots",
    Element::empty_dense_tree(4),
    None,
    None,
    grove_version,
)?;

// Append values — positions filled 0, 1, 2, ...
let (root_hash, pos) = db.dense_tree_insert(
    &[b"state"],
    b"validator_slots",
    validator_pubkey.to_vec(),
    None,
    grove_version,
)?;
// pos == 0, root_hash = blake3(validator_pubkey)

// Read back by position
let value = db.dense_tree_get(
    &[b"state"],
    b"validator_slots",
    0,     // position
    None,
    grove_version,
)?;
assert_eq!(value, Some(validator_pubkey.to_vec()));

// Query metadata
let count = db.dense_tree_count(&[b"state"], b"validator_slots", None, grove_version)?;
let hash = db.dense_tree_root_hash(&[b"state"], b"validator_slots", None, grove_version)?;
```

## Arquivos de Implementacao

| Arquivo | Conteudo |
|---|---|
| `grovedb-dense-fixed-sized-merkle-tree/src/lib.rs` | Trait `DenseTreeStore`, struct `DenseFixedSizedMerkleTree`, hash recursivo |
| `grovedb-dense-fixed-sized-merkle-tree/src/proof.rs` | Struct `DenseTreeProof`, `generate()`, `encode_to_vec()`, `decode_from_slice()` |
| `grovedb-dense-fixed-sized-merkle-tree/src/verify.rs` | `DenseTreeProof::verify()` — funcao pura, sem armazenamento necessario |
| `grovedb-element/src/element/mod.rs` | `Element::DenseAppendOnlyFixedSizeTree` (discriminante 14) |
| `grovedb-element/src/element/constructor.rs` | `empty_dense_tree()`, `new_dense_tree()` |
| `merk/src/tree_type/mod.rs` | `TreeType::DenseAppendOnlyFixedSizeTree = 10` |
| `merk/src/tree_type/costs.rs` | `DENSE_TREE_COST_SIZE = 6` |
| `grovedb/src/operations/dense_tree.rs` | Operacoes do GroveDB, `AuxDenseTreeStore`, preprocessamento em lote |
| `grovedb/src/operations/proof/generate.rs` | `generate_dense_tree_layer_proof()`, `query_items_to_positions()` |
| `grovedb/src/operations/proof/verify.rs` | `verify_dense_tree_lower_layer()` |
| `grovedb/src/operations/proof/mod.rs` | Variante `ProofBytes::DenseTree` |
| `grovedb/src/batch/estimated_costs/average_case_costs.rs` | Modelo de custo de caso medio |
| `grovedb/src/batch/estimated_costs/worst_case_costs.rs` | Modelo de custo de pior caso |
| `grovedb/src/tests/dense_tree_tests.rs` | 22 testes de integracao |

---
