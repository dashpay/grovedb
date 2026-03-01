# El BulkAppendTree — Almacenamiento de Solo-Adicion de Alto Rendimiento

El BulkAppendTree es la respuesta de GroveDB a un desafio de ingenieria especifico: como construir
un registro de solo-adicion (append-only log) de alto rendimiento que soporte pruebas de rango
eficientes, minimice el hashing por escritura y produzca instantaneas de chunks inmutables adecuadas
para distribucion por CDN?

Mientras que un MmrTree (Capitulo 13) es ideal para pruebas de hojas individuales, el BulkAppendTree
esta disenado para cargas de trabajo donde miles de valores llegan por bloque y los clientes necesitan
sincronizar obteniendo rangos de datos. Lo logra con una **arquitectura de dos niveles**:
un buffer de arbol denso de Merkle que absorbe las adiciones entrantes, y un MMR a nivel de chunk
que registra las raices de chunks finalizados.

## La Arquitectura de Dos Niveles

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

**Nivel 1 — El Buffer.** Los valores entrantes se escriben en un `DenseFixedSizedMerkleTree`
(ver Capitulo 16). La capacidad del buffer es `2^height - 1` posiciones. El hash raiz del
arbol denso (`dense_tree_root`) se actualiza despues de cada insercion.

**Nivel 2 — El Chunk MMR.** Cuando el buffer se llena (alcanza `chunk_size` entradas),
todas las entradas se serializan en un **blob de chunk** inmutable, se calcula una raiz
densa de Merkle sobre esas entradas, y esa raiz se anade como hoja al chunk MMR.
El buffer se limpia entonces.

La **raiz de estado** (state root) combina ambos niveles en un unico compromiso de 32 bytes que cambia
con cada adicion, asegurando que el arbol Merk padre siempre refleje el estado mas reciente.

## Como los Valores Llenan el Buffer

Cada llamada a `append()` sigue esta secuencia:

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

El **buffer ES un DenseFixedSizedMerkleTree** (ver Capitulo 16). Su hash raiz
cambia despues de cada insercion, proporcionando un compromiso con todas las entradas actuales del buffer.
Este hash raiz es lo que fluye hacia el calculo de la raiz de estado.

## Compactacion de Chunks

Cuando el buffer se llena (alcanza `chunk_size` entradas), la compactacion se dispara automaticamente:

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

Despues de la compactacion, el blob de chunk es **permanentemente inmutable** — nunca cambia
de nuevo. Esto hace que los blobs de chunk sean ideales para cache en CDN, sincronizacion de clientes
y almacenamiento de archivo.

**Ejemplo: 4 adiciones con chunk_power=2 (chunk_size=4)**

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

## La Raiz de Estado

La raiz de estado une ambos niveles en un solo hash:

```rust
fn compute_state_root(
    mmr_root: &[u8; 32],         // Chunk MMR root (or [0;32] if empty)
    dense_tree_root: &[u8; 32],  // Root hash of current buffer (dense tree)
) -> [u8; 32] {
    blake3("bulk_state" || mmr_root || dense_tree_root)
}
```

El `total_count` y `chunk_power` **no** estan incluidos en la raiz de estado porque
ya estan autenticados por el value hash del Merk — son campos del
`Element` serializado almacenado en el nodo Merk padre. La raiz de estado captura solo los
compromisos a nivel de datos (`mmr_root` y `dense_tree_root`). Este es el hash que
fluye como el hash hijo del Merk y se propaga hasta el hash raiz de GroveDB.

## La Raiz Densa de Merkle

Cuando un chunk se compacta, las entradas necesitan un unico compromiso de 32 bytes. El
BulkAppendTree usa un **arbol binario denso (completo) de Merkle**:

```text
Given entries [e_0, e_1, e_2, e_3]:

Level 0 (leaves):  blake3(e_0)  blake3(e_1)  blake3(e_2)  blake3(e_3)
                      \__________/              \__________/
Level 1:           blake3(h_0 || h_1)       blake3(h_2 || h_3)
                          \____________________/
Level 2 (root):    blake3(h_01 || h_23)  ← this is the dense Merkle root
```

Dado que `chunk_size` siempre es una potencia de 2 (por construccion: `1u32 << chunk_power`),
el arbol siempre esta completo (no se necesitan hojas de relleno o ficticias). La cuenta de hashes es
exactamente `2 * chunk_size - 1`:
- `chunk_size` hashes de hojas (uno por entrada)
- `chunk_size - 1` hashes de nodos internos

La implementacion de la raiz densa de Merkle se encuentra en `grovedb-mmr/src/dense_merkle.rs` y
proporciona dos funciones:
- `compute_dense_merkle_root(hashes)` — a partir de hojas pre-hasheadas
- `compute_dense_merkle_root_from_values(values)` — hashea los valores primero, luego construye
  el arbol

## Serializacion de Blobs de Chunk

Los blobs de chunk son los archivos inmutables producidos por la compactacion. El serializador
auto-selecciona el formato de cable mas compacto basandose en los tamanos de las entradas:

**Formato de tamano fijo** (flag `0x01`) — cuando todas las entradas tienen la misma longitud:

```text
┌──────┬──────────┬─────────────┬─────────┬─────────┬─────────┐
│ 0x01 │ count    │ entry_size  │ entry_0 │ entry_1 │ ...     │
│ 1B   │ 4B (BE)  │ 4B (BE)     │ N bytes │ N bytes │         │
└──────┴──────────┴─────────────┴─────────┴─────────┴─────────┘
Total: 1 + 4 + 4 + (count × entry_size) bytes
```

**Formato de tamano variable** (flag `0x00`) — cuando las entradas tienen diferentes longitudes:

```text
┌──────┬──────────┬─────────┬──────────┬─────────┬─────────────┐
│ 0x00 │ len_0    │ entry_0 │ len_1    │ entry_1 │ ...         │
│ 1B   │ 4B (BE)  │ N bytes │ 4B (BE)  │ M bytes │             │
└──────┴──────────┴─────────┴──────────┴─────────┴─────────────┘
Total: 1 + Σ(4 + len_i) bytes
```

El formato de tamano fijo ahorra 4 bytes por entrada comparado con el de tamano variable, lo cual suma
significativamente para grandes chunks de datos de tamano uniforme (como compromisos hash de 32 bytes).
Para 1024 entradas de 32 bytes cada una:
- Fijo: `1 + 4 + 4 + 32768 = 32,777 bytes`
- Variable: `1 + 1024 × (4 + 32) = 36,865 bytes`
- Ahorro: ~11%

## Disposicion de Claves de Almacenamiento

Todos los datos del BulkAppendTree residen en el espacio de nombres **data**, con claves de prefijo de un solo caracter:

| Patron de clave | Formato | Tamano | Proposito |
|---|---|---|---|
| `M` | 1 byte | 1B | Clave de metadatos |
| `b` + `{index}` | `b` + u32 BE | 5B | Entrada del buffer en el indice |
| `e` + `{index}` | `e` + u64 BE | 9B | Blob de chunk en el indice |
| `m` + `{pos}` | `m` + u64 BE | 9B | Nodo MMR en la posicion |

Los **metadatos** almacenan `mmr_size` (8 bytes BE). El `total_count` y `chunk_power` se
almacenan en el Element mismo (en el Merk padre), no en los metadatos del espacio de nombres data.
Esta division significa que leer el conteo es una simple consulta de elemento sin abrir el
contexto de almacenamiento de datos.

Las claves del buffer usan indices u32 (0 a `chunk_size - 1`) porque la capacidad del buffer esta
limitada por `chunk_size` (un u32, calculado como `1u32 << chunk_power`). Las claves de chunk usan indices u64
porque el numero de chunks completados puede crecer indefinidamente.

## La Estructura BulkAppendTree

```rust
pub struct BulkAppendTree<S> {
    pub total_count: u64,                        // Total values ever appended
    pub dense_tree: DenseFixedSizedMerkleTree<S>, // The buffer (dense tree)
}
```

El buffer ES un `DenseFixedSizedMerkleTree` — su hash raiz es `dense_tree_root`.

**Accesores:**
- `capacity() -> u16`: `dense_tree.capacity()` (= `2^height - 1`)
- `epoch_size() -> u64`: `capacity + 1` (= `2^height`, el numero de entradas por chunk)
- `height() -> u8`: `dense_tree.height()`

**Valores derivados** (no almacenados):

| Valor | Formula |
|---|---|
| `chunk_count` | `total_count / epoch_size` |
| `buffer_count` | `dense_tree.count()` |
| `mmr_size` | `leaf_count_to_mmr_size(chunk_count)` |

## Operaciones de GroveDB

El BulkAppendTree se integra con GroveDB a traves de seis operaciones definidas en
`grovedb/src/operations/bulk_append_tree.rs`:

### bulk_append

La operacion primaria de mutacion. Sigue el patron estandar de almacenamiento no-Merk de GroveDB:

```text
1. Validate element is BulkAppendTree
2. Open data storage context
3. Load tree from store
4. Append value (may trigger compaction)
5. Update element in parent Merk with new state_root + total_count
6. Propagate changes up through Merk hierarchy
7. Commit transaction
```

El adaptador `AuxBulkStore` envuelve las llamadas `get_aux`/`put_aux`/`delete_aux` de GroveDB y
acumula `OperationCost` en un `RefCell` para el seguimiento de costos. Los costos de hash de la
operacion de adicion se agregan a `cost.hash_node_calls`.

### Operaciones de lectura

| Operacion | Que retorna | Almacenamiento aux? |
|---|---|---|
| `bulk_get_value(path, key, position)` | Valor en posicion global | Si — lee del blob de chunk o buffer |
| `bulk_get_chunk(path, key, chunk_index)` | Blob de chunk crudo | Si — lee la clave del chunk |
| `bulk_get_buffer(path, key)` | Todas las entradas actuales del buffer | Si — lee las claves del buffer |
| `bulk_count(path, key)` | Conteo total (u64) | No — lee del elemento |
| `bulk_chunk_count(path, key)` | Chunks completados (u64) | No — calculado del elemento |

La operacion `get_value` enruta transparentemente por posicion:

```text
if position < completed_chunks × chunk_size:
    chunk_idx = position / chunk_size
    intra_idx = position % chunk_size
    → read chunk blob, deserialize, return entry[intra_idx]
else:
    buffer_idx = position % chunk_size
    → read buffer_key(buffer_idx)
```

## Operaciones por Lotes y Preprocesamiento

BulkAppendTree soporta operaciones por lotes a traves de la variante `GroveOp::BulkAppend`.
Dado que `execute_ops_on_path` no tiene acceso al contexto de almacenamiento de datos, todas las operaciones BulkAppend
deben ser preprocesadas antes de `apply_body`.

La tuberia de preprocesamiento:

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

La variante `append_with_mem_buffer` evita problemas de lectura-despues-de-escritura: las entradas del buffer
se rastrean en un `Vec<Vec<u8>>` en memoria, asi la compactacion puede leerlas incluso cuando
el almacenamiento transaccional aun no se ha confirmado.

## El Trait BulkStore

```rust
pub trait BulkStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String>;
    fn delete(&self, key: &[u8]) -> Result<(), String>;
}
```

Los metodos toman `&self` (no `&mut self`) para coincidir con el patron de mutabilidad interior de GroveDB
donde las escrituras pasan a traves de un lote. La integracion con GroveDB lo implementa mediante
`AuxBulkStore` que envuelve un `StorageContext` y acumula `OperationCost`.

El `MmrAdapter` hace de puente entre `BulkStore` y los traits `MMRStoreReadOps`/
`MMRStoreWriteOps` del MMR ckb, agregando una cache de escritura directa para
la correctitud de lectura-despues-de-escritura.

## Generacion de Pruebas

Las pruebas de BulkAppendTree soportan **consultas de rango** sobre posiciones. La estructura de prueba
captura todo lo necesario para que un verificador sin estado confirme que datos especificos
existen en el arbol:

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

**Pasos de generacion** para un rango `[start, end)` (con `chunk_size = 1u32 << chunk_power`):

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

**Por que incluir TODAS las entradas del buffer?** El buffer es un arbol denso de Merkle cuyo hash raiz
se compromete con cada entrada. El verificador debe reconstruir el arbol desde todas las entradas para verificar
el `dense_tree_root`. Dado que el buffer esta limitado por `capacity` (como maximo 65,535
entradas), este es un costo razonable.

## Verificacion de Pruebas

La verificacion es una funcion pura — no se necesita acceso a la base de datos. Realiza cinco verificaciones:

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

Despues de que la verificacion tiene exito, el `BulkAppendTreeProofResult` proporciona un
metodo `values_in_range(start, end)` que extrae valores especificos de los
blobs de chunk verificados y las entradas del buffer.

## Como se Conecta con el Hash Raiz de GroveDB

El BulkAppendTree es un **arbol no-Merk** — almacena datos en el espacio de nombres data,
no en un subarbol hijo Merk. En el Merk padre, el elemento se almacena como:

```text
Element::BulkAppendTree(total_count, chunk_power, flags)
```

La raiz de estado fluye como el hash hijo del Merk. El hash del nodo Merk padre es:

```text
combine_hash(value_hash(element_bytes), state_root)
```

El `state_root` fluye como el hash hijo del Merk (a traves del parametro
`subtree_root_hash` de `insert_subtree`). Cualquier cambio en la raiz de estado se propaga hacia arriba a traves
de la jerarquia Merk de GroveDB hasta el hash raiz.

En pruebas V1 (ver seccion 9.6), la prueba Merk del padre demuestra los bytes del elemento y el enlace del hash
hijo, y el `BulkAppendTreeProof` demuestra que los datos consultados son consistentes
con el `state_root` usado como hash hijo.

## Seguimiento de Costos

El costo de hash de cada operacion se rastrea explicitamente:

| Operacion | Llamadas Blake3 | Notas |
|---|---|---|
| Adicion simple (sin compactacion) | 3 | 2 para la cadena de hash del buffer + 1 para la raiz de estado |
| Adicion simple (con compactacion) | 3 + 2C - 1 + ~2 | Cadena + Merkle denso (C=chunk_size) + push MMR + raiz de estado |
| `get_value` desde chunk | 0 | Deserializacion pura, sin hashing |
| `get_value` desde buffer | 0 | Busqueda directa por clave |
| Generacion de prueba | Depende del conteo de chunks | Raiz densa de Merkle por chunk + prueba MMR |
| Verificacion de prueba | 2C*K - K + B*2 + 1 | K chunks, B entradas del buffer, C chunk_size |

**Costo amortizado por adicion**: Para chunk_size=1024 (chunk_power=10), el costo de compactacion de ~2047
hashes (raiz densa de Merkle) se amortiza sobre 1024 adiciones, agregando ~2 hashes por
adicion. Combinado con los 3 hashes por adicion, el total amortizado es **~5 llamadas blake3
por adicion** — muy eficiente para una estructura autenticada criptograficamente.

## Comparacion con MmrTree

| | BulkAppendTree | MmrTree |
|---|---|---|
| **Arquitectura** | Dos niveles (buffer + chunk MMR) | MMR unico |
| **Costo de hash por adicion** | 3 (+ amortizado ~2 por compactacion) | ~2 |
| **Granularidad de prueba** | Consultas de rango sobre posiciones | Pruebas de hojas individuales |
| **Instantaneas inmutables** | Si (blobs de chunk) | No |
| **Compatible con CDN** | Si (blobs de chunk cacheables) | No |
| **Entradas del buffer** | Si (necesita todas para la prueba) | N/A |
| **Mejor para** | Registros de alto rendimiento, sincronizacion masiva | Registros de eventos, busquedas individuales |
| **Discriminante de Element** | 13 | 12 |
| **TreeType** | 9 | 8 |

Elige MmrTree cuando necesites pruebas de hojas individuales con minima sobrecarga. Elige
BulkAppendTree cuando necesites consultas de rango, sincronizacion masiva e instantaneas
basadas en chunks.

## Archivos de Implementacion

| Archivo | Proposito |
|------|---------|
| `grovedb-bulk-append-tree/src/lib.rs` | Raiz del crate, re-exportaciones |
| `grovedb-bulk-append-tree/src/tree/mod.rs` | Estructura `BulkAppendTree`, accesores de estado, persistencia de metadatos |
| `grovedb-bulk-append-tree/src/tree/append.rs` | `append()`, `append_with_mem_buffer()`, `compact_entries()` |
| `grovedb-bulk-append-tree/src/tree/hash.rs` | `compute_state_root` |
| `grovedb-bulk-append-tree/src/tree/keys.rs` | `META_KEY`, `buffer_key`, `chunk_key`, `mmr_node_key` |
| `grovedb-bulk-append-tree/src/tree/query.rs` | `get_value`, `get_chunk`, `get_buffer` |
| `grovedb-bulk-append-tree/src/tree/mmr_adapter.rs` | `MmrAdapter` con cache de escritura directa |
| `grovedb-bulk-append-tree/src/chunk.rs` | Serializacion de blobs de chunk (formatos fijo + variable) |
| `grovedb-bulk-append-tree/src/proof.rs` | Generacion y verificacion de `BulkAppendTreeProof` |
| `grovedb-bulk-append-tree/src/store.rs` | Trait `BulkStore` |
| `grovedb-bulk-append-tree/src/error.rs` | Enum `BulkAppendError` |
| `grovedb/src/operations/bulk_append_tree.rs` | Operaciones GroveDB, `AuxBulkStore`, preprocesamiento por lotes |
| `grovedb/src/tests/bulk_append_tree_tests.rs` | 27 pruebas de integracion |

---
