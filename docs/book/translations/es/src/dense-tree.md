# El DenseAppendOnlyFixedSizeTree — Almacenamiento Denso de Merkle con Capacidad Fija

El DenseAppendOnlyFixedSizeTree es un arbol binario completo de altura fija donde
**cada nodo** — tanto interno como hoja — almacena un valor de datos. Las posiciones se llenan
secuencialmente en orden por niveles (BFS): primero la raiz (posicion 0), luego de izquierda a derecha en cada
nivel. No se persisten hashes intermedios; el hash raiz se recalcula sobre la marcha
hasheando recursivamente desde las hojas hasta la raiz.

Este diseno es ideal para estructuras de datos pequenas y acotadas donde la capacidad maxima se
conoce de antemano y necesitas insercion O(1), recuperacion por posicion O(1), y un compromiso
compacto de hash raiz de 32 bytes que cambia despues de cada insercion.

## Estructura del Arbol

Un arbol de altura *h* tiene capacidad `2^h - 1` posiciones. Las posiciones usan indexacion
en orden por niveles basada en 0:

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

Los valores se anaden secuencialmente: el primer valor va a la posicion 0 (raiz), luego
posicion 1, 2, 3, y asi sucesivamente. Esto significa que la raiz siempre tiene datos, y el arbol se llena
en orden por niveles — el orden de recorrido mas natural para un arbol binario completo.

## Calculo del Hash

El hash raiz no se almacena por separado — se recalcula desde cero cada vez que se necesita.
El algoritmo recursivo visita solo las posiciones llenas:

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

**Propiedades clave:**
- Todos los nodos (hoja e interno): `blake3(blake3(value) || H(left) || H(right))`
- Nodos hoja: left_hash y right_hash son ambos `[0; 32]` (hijos no llenados)
- Posiciones no llenadas: `[0u8; 32]` (hash cero)
- Arbol vacio (count = 0): `[0u8; 32]`

**No se usan etiquetas de separacion de dominio hoja/interno.** La estructura del arbol (`height`
y `count`) esta autenticada externamente en el `Element::DenseAppendOnlyFixedSizeTree` padre,
que fluye a traves de la jerarquia Merk. El verificador siempre sabe exactamente cuales
posiciones son hojas vs nodos internos a partir de la altura y el conteo, por lo que un atacante
no puede sustituir uno por otro sin romper la cadena de autenticacion del padre.

Esto significa que el hash raiz codifica un compromiso con cada valor almacenado y su posicion
exacta en el arbol. Cambiar cualquier valor (si fuera mutable) se propagaria en cascada a traves
de todos los hashes ancestrales hasta la raiz.

**Costo de hash:** Calcular el hash raiz visita todas las posiciones llenas mas cualquier
hijo no llenado. Para un arbol con *n* valores, el peor caso es O(*n*) llamadas blake3. Esto es
aceptable porque el arbol esta disenado para capacidades pequenas y acotadas (altura maxima 16,
maximo 65,535 posiciones).

## La Variante de Element

```rust
Element::DenseAppendOnlyFixedSizeTree(
    u16,                   // count — number of values stored (max 65,535)
    u8,                    // height — immutable after creation (1..=16)
    Option<ElementFlags>,  // flags — storage flags
)
```

| Campo | Tipo | Descripcion |
|---|---|---|
| `count` | `u16` | Numero de valores insertados hasta ahora (max 65,535) |
| `height` | `u8` | Altura del arbol (1..=16), inmutable despues de la creacion |
| `flags` | `Option<ElementFlags>` | Flags de almacenamiento opcionales |

El hash raiz NO se almacena en el Element — fluye como el hash hijo del Merk
a traves del parametro `subtree_root_hash` de `insert_subtree`.

**Discriminante:** 14 (ElementType), TreeType = 10

**Tamano de costo:** `DENSE_TREE_COST_SIZE = 6` bytes (2 count + 1 height + 1 discriminante
+ 2 overhead)

## Disposicion del Almacenamiento

Al igual que MmrTree y BulkAppendTree, el DenseAppendOnlyFixedSizeTree almacena datos en el
espacio de nombres **data** (no en un Merk hijo). Los valores se identifican por su posicion como un `u64` big-endian:

```text
Subtree path: blake3(parent_path || key)

Storage keys:
  [0, 0, 0, 0, 0, 0, 0, 0] → value at position 0 (root)
  [0, 0, 0, 0, 0, 0, 0, 1] → value at position 1
  [0, 0, 0, 0, 0, 0, 0, 2] → value at position 2
  ...
```

El Element en si (almacenado en el Merk padre) lleva el `count` y `height`.
El hash raiz fluye como el hash hijo del Merk. Esto significa:
- **Leer el hash raiz** requiere recalculo desde el almacenamiento (O(n) hashing)
- **Leer un valor por posicion es O(1)** — una sola busqueda en almacenamiento
- **Insertar es O(n) hashing** — una escritura en almacenamiento + recalculo completo del hash raiz

## Operaciones

### `dense_tree_insert(path, key, value, tx, grove_version)`

Anade un valor a la siguiente posicion disponible. Retorna `(root_hash, position)`.

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

Recupera el valor en una posicion dada. Retorna `None` si posicion >= count.

### `dense_tree_root_hash(path, key, tx, grove_version)`

Retorna el hash raiz almacenado en el elemento. Este es el hash calculado durante la
insercion mas reciente — no se necesita recalculo.

### `dense_tree_count(path, key, tx, grove_version)`

Retorna el numero de valores almacenados (el campo `count` del elemento).

## Operaciones por Lotes

La variante `GroveOp::DenseTreeInsert` soporta insercion por lotes a traves de la tuberia
estandar de lotes de GroveDB:

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

**Preprocesamiento:** Como todos los tipos de arbol no-Merk, las operaciones `DenseTreeInsert` se preprocesan
antes de que el cuerpo principal del lote se ejecute. El metodo `preprocess_dense_tree_ops`:

1. Agrupa todas las operaciones `DenseTreeInsert` por `(path, key)`
2. Para cada grupo, ejecuta las inserciones secuencialmente (leyendo el elemento, insertando
   cada valor, actualizando el hash raiz)
3. Convierte cada grupo en una operacion `ReplaceNonMerkTreeRoot` que lleva el
   `root_hash` final y `count` a traves de la maquinaria estandar de propagacion

Multiples inserciones al mismo arbol denso dentro de un solo lote estan soportadas — se
procesan en orden y la verificacion de consistencia permite claves duplicadas para este tipo de operacion.

**Propagacion:** El hash raiz y el conteo fluyen a traves de la variante `NonMerkTreeMeta::DenseTree`
en `ReplaceNonMerkTreeRoot`, siguiendo el mismo patron que MmrTree y
BulkAppendTree.

## Pruebas

DenseAppendOnlyFixedSizeTree soporta **pruebas de subconsulta V1** a traves de la
variante `ProofBytes::DenseTree`. Las posiciones individuales pueden probarse contra el hash raiz del arbol usando
pruebas de inclusion que llevan valores ancestrales y hashes de subarboles hermanos.

### Estructura del Camino de Autenticacion

Dado que los nodos internos hashean su **propio valor** (no solo los hashes hijos), el
camino de autenticacion difiere de un arbol de Merkle estandar. Para verificar una hoja en posicion
`p`, el verificador necesita:

1. **El valor de la hoja** (la entrada probada)
2. **Hashes de valores ancestrales** para cada nodo interno en el camino de `p` a la raiz (solo el hash de 32 bytes, no el valor completo)
3. **Hashes de subarboles hermanos** para cada hijo que NO esta en el camino

Dado que todos los nodos usan `blake3(H(value) || H(left) || H(right))` (sin etiquetas de dominio),
la prueba solo lleva hashes de valores de 32 bytes para los ancestros — no valores completos. Esto
mantiene las pruebas compactas independientemente de cuan grandes sean los valores individuales.

```rust
pub struct DenseTreeProof {
    pub entries: Vec<(u16, Vec<u8>)>,            // proved (position, value) pairs
    pub node_value_hashes: Vec<(u16, [u8; 32])>, // ancestor value hashes on the auth path
    pub node_hashes: Vec<(u16, [u8; 32])>,       // precomputed sibling subtree hashes
}
```

> **Nota:** `height` y `count` no estan en la estructura de la prueba — el verificador los obtiene del Element padre, que esta autenticado por la jerarquia Merk.

### Ejemplo Detallado

Arbol con height=3, capacity=7, count=5, probando la posicion 4:

```text
        0
       / \
      1   2
     / \ / \
    3  4 5  6
```

Camino de 4 a la raiz: `4 -> 1 -> 0`. Conjunto expandido: `{0, 1, 4}`.

La prueba contiene:
- **entries**: `[(4, value[4])]` — la posicion probada
- **node_value_hashes**: `[(0, H(value[0])), (1, H(value[1]))]` — hashes de valores ancestrales (32 bytes cada uno, no valores completos)
- **node_hashes**: `[(2, H(subtree_2)), (3, H(node_3))]` — hermanos que no estan en el camino

La verificacion recalcula el hash raiz de abajo hacia arriba:
1. `H(4) = blake3(blake3(value[4]) || [0;32] || [0;32])` — hoja (los hijos no estan llenados)
2. `H(3)` — de `node_hashes`
3. `H(1) = blake3(H(value[1]) || H(3) || H(4))` — interno usa hash de valor de `node_value_hashes`
4. `H(2)` — de `node_hashes`
5. `H(0) = blake3(H(value[0]) || H(1) || H(2))` — raiz usa hash de valor de `node_value_hashes`
6. Comparar `H(0)` contra el hash raiz esperado

### Pruebas de Multiples Posiciones

Cuando se prueban multiples posiciones, el conjunto expandido fusiona caminos de autenticacion superpuestos. Los
ancestros compartidos se incluyen solo una vez, haciendo las pruebas de multiples posiciones mas compactas que
pruebas independientes de posicion unica.

### Limitacion V0

Las pruebas V0 no pueden descender a arboles densos. Si una consulta V0 coincide con un
`DenseAppendOnlyFixedSizeTree` con una subconsulta, el sistema retorna
`Error::NotSupported` dirigiendo al llamador a usar `prove_query_v1`.

### Codificacion de Claves de Consulta

Las posiciones del arbol denso se codifican como claves de consulta **u16 big-endian** (2 bytes), a diferencia de
MmrTree y BulkAppendTree que usan u64. Todos los tipos estandar de `QueryItem` de rango
estan soportados.

## Comparacion con Otros Arboles No-Merk

| | DenseTree | MmrTree | BulkAppendTree | CommitmentTree |
|---|---|---|---|---|
| **Discriminante de Element** | 14 | 12 | 13 | 11 |
| **TreeType** | 10 | 8 | 9 | 7 |
| **Capacidad** | Fija (`2^h - 1`, max 65,535) | Ilimitada | Ilimitada | Ilimitada |
| **Modelo de datos** | Cada posicion almacena un valor | Solo hojas | Buffer de arbol denso + chunks | Solo hojas |
| **Hash en Element?** | No (fluye como hash hijo) | No (fluye como hash hijo) | No (fluye como hash hijo) | No (fluye como hash hijo) |
| **Costo de insercion (hashing)** | O(n) blake3 | O(1) amortizado | O(1) amortizado | ~33 Sinsemilla |
| **Tamano de costo** | 6 bytes | 11 bytes | 12 bytes | 12 bytes |
| **Soporte de pruebas** | V1 (Dense) | V1 (MMR) | V1 (Bulk) | V1 (CommitmentTree) |
| **Mejor para** | Estructuras pequenas acotadas | Registros de eventos | Registros de alto rendimiento | Compromisos ZK |

**Cuando elegir DenseAppendOnlyFixedSizeTree:**
- El numero maximo de entradas se conoce al momento de la creacion
- Necesitas que cada posicion (incluyendo nodos internos) almacene datos
- Quieres el modelo de datos mas simple posible sin crecimiento ilimitado
- El recalculo del hash raiz O(n) es aceptable (alturas de arbol pequenas)

**Cuando NO elegirlo:**
- Necesitas capacidad ilimitada -> usa MmrTree o BulkAppendTree
- Necesitas compatibilidad ZK -> usa CommitmentTree

## Ejemplo de Uso

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

## Archivos de Implementacion

| Archivo | Contenido |
|---|---|
| `grovedb-dense-fixed-sized-merkle-tree/src/lib.rs` | Trait `DenseTreeStore`, estructura `DenseFixedSizedMerkleTree`, hash recursivo |
| `grovedb-dense-fixed-sized-merkle-tree/src/proof.rs` | Estructura `DenseTreeProof`, `generate()`, `encode_to_vec()`, `decode_from_slice()` |
| `grovedb-dense-fixed-sized-merkle-tree/src/verify.rs` | `DenseTreeProof::verify()` — funcion pura, no necesita almacenamiento |
| `grovedb-element/src/element/mod.rs` | `Element::DenseAppendOnlyFixedSizeTree` (discriminante 14) |
| `grovedb-element/src/element/constructor.rs` | `empty_dense_tree()`, `new_dense_tree()` |
| `merk/src/tree_type/mod.rs` | `TreeType::DenseAppendOnlyFixedSizeTree = 10` |
| `merk/src/tree_type/costs.rs` | `DENSE_TREE_COST_SIZE = 6` |
| `grovedb/src/operations/dense_tree.rs` | Operaciones GroveDB, `AuxDenseTreeStore`, preprocesamiento por lotes |
| `grovedb/src/operations/proof/generate.rs` | `generate_dense_tree_layer_proof()`, `query_items_to_positions()` |
| `grovedb/src/operations/proof/verify.rs` | `verify_dense_tree_lower_layer()` |
| `grovedb/src/operations/proof/mod.rs` | Variante `ProofBytes::DenseTree` |
| `grovedb/src/batch/estimated_costs/average_case_costs.rs` | Modelo de costo caso promedio |
| `grovedb/src/batch/estimated_costs/worst_case_costs.rs` | Modelo de costo peor caso |
| `grovedb/src/tests/dense_tree_tests.rs` | 22 pruebas de integracion |

---
