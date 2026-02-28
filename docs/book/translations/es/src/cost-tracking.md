# Seguimiento de Costos

## La Estructura OperationCost

Cada operación en GroveDB acumula costos, medidos en recursos computacionales:

```rust
// costs/src/lib.rs
pub struct OperationCost {
    pub seek_count: u32,              // Number of storage seeks
    pub storage_cost: StorageCost,    // Bytes added/replaced/removed
    pub storage_loaded_bytes: u64,    // Bytes read from disk
    pub hash_node_calls: u32,         // Number of Blake3 hash operations
    pub sinsemilla_hash_calls: u32,   // Number of Sinsemilla hash operations (EC ops)
}
```

> Las **llamadas de hash Sinsemilla** rastrean las operaciones de hash de curva elíptica para anclas del
> CommitmentTree. Son significativamente más costosas que los hashes de nodos Blake3.

Los costos de almacenamiento se desglosan más:

```rust
// costs/src/storage_cost/mod.rs
pub struct StorageCost {
    pub added_bytes: u32,                   // New data written
    pub replaced_bytes: u32,                // Existing data overwritten
    pub removed_bytes: StorageRemovedBytes, // Data freed
}
```

## El Patrón CostContext

Todas las operaciones retornan su resultado envuelto en un `CostContext`:

```rust
pub struct CostContext<T> {
    pub value: T,               // The operation result
    pub cost: OperationCost,    // Resources consumed
}

pub type CostResult<T, E> = CostContext<Result<T, E>>;
```

Esto crea un patrón **monádico** de seguimiento de costos — los costos fluyen a través de cadenas de
operaciones automáticamente:

```rust
// Unwrap a result, adding its cost to an accumulator
let result = expensive_operation().unwrap_add_cost(&mut total_cost);

// Chain operations, accumulating costs
let final_result = op1()
    .flat_map(|x| op2(x))      // Costs from op1 + op2
    .flat_map(|y| op3(y));      // + costs from op3
```

## La Macro cost_return_on_error!

El patrón más común en el código de GroveDB es la macro `cost_return_on_error!`,
que actúa como `?` pero preserva los costos en el retorno anticipado:

```rust
macro_rules! cost_return_on_error {
    ( &mut $cost:ident, $($body:tt)+ ) => {
        {
            let result_with_cost = { $($body)+ };
            let result = result_with_cost.unwrap_add_cost(&mut $cost);
            match result {
                Ok(x) => x,
                Err(e) => return Err(e).wrap_with_cost($cost),
            }
        }
    };
}
```

En la práctica:

```rust
fn insert_element(&self, path: &[&[u8]], key: &[u8], element: Element) -> CostResult<(), Error> {
    let mut cost = OperationCost::default();

    // Each macro call adds the operation's cost to `cost`
    // and returns the Ok value (or early-returns with accumulated cost on Err)
    let merk = cost_return_on_error!(&mut cost, self.open_merk(path));
    cost_return_on_error!(&mut cost, merk.insert(key, element));
    cost_return_on_error!(&mut cost, self.propagate_changes(path));

    Ok(()).wrap_with_cost(cost)
    // `cost` now contains the sum of all three operations' costs
}
```

## Desglose del Costo de Almacenamiento

Cuando un valor se actualiza, el costo depende de si el nuevo valor es más grande,
más pequeño o del mismo tamaño:

```mermaid
graph LR
    subgraph case1["CASO 1: Mismo Tamaño (old=100, new=100)"]
        c1_old["old: 100B"]
        c1_new["new: 100B"]
        c1_cost["replaced_bytes += 100"]
    end

    subgraph case2["CASO 2: Crecimiento (old=100, new=120)"]
        c2_old["old: 100B"]
        c2_new["new: 120B"]
        c2_replaced["replaced: 100B"]
        c2_added["added: +20B"]
        c2_cost["replaced_bytes += 100<br/>added_bytes += 20"]
    end

    subgraph case3["CASO 3: Reducción (old=100, new=70)"]
        c3_old["old: 100B"]
        c3_new["new: 70B"]
        c3_replaced["replaced: 70B"]
        c3_removed["removed: 30B"]
        c3_cost["replaced_bytes += 70<br/>removed_bytes += 30"]
    end

    style case1 fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style case2 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style c2_added fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style case3 fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
    style c3_removed fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

## Costos de Operaciones de Hash

Los costos de hash se miden en "llamadas de hash de nodo" — el número de compresiones de bloques
Blake3:

| Operación | Tamaño de Entrada | Llamadas de Hash |
|-----------|-----------|------------|
| `value_hash(small)` | < 64 bytes | 1 |
| `value_hash(medium)` | 64-127 bytes | 2 |
| `kv_hash` | key + value_hash | varía |
| `node_hash` | 96 bytes (3 × 32) | 2 (siempre) |
| `combine_hash` | 64 bytes (2 × 32) | 1 (siempre) |
| `node_hash_with_count` | 104 bytes (3 × 32 + 8) | 2 (siempre) |
| Sinsemilla (CommitmentTree) | Op EC en curva Pallas | rastreado por separado vía `sinsemilla_hash_calls` |

La fórmula general para Blake3:

```text
hash_calls = 1 + (input_bytes - 1) / 64
```

## Estimación de Peor Caso y Caso Promedio

GroveDB proporciona funciones para **estimar** los costos de operación antes de ejecutarlas.
Esto es crucial para el cálculo de comisiones en blockchain — necesitas conocer el costo antes
de comprometerte a pagarlo.

```rust
// Worst-case cost for reading a node
pub fn add_worst_case_get_merk_node(
    cost: &mut OperationCost,
    not_prefixed_key_len: u32,
    max_element_size: u32,
    node_type: NodeType,
) {
    cost.seek_count += 1;  // One disk seek
    cost.storage_loaded_bytes +=
        TreeNode::worst_case_encoded_tree_size(
            not_prefixed_key_len, max_element_size, node_type
        ) as u64;
}

// Worst-case propagation cost
pub fn add_worst_case_merk_propagate(
    cost: &mut OperationCost,
    input: &WorstCaseLayerInformation,
) {
    let levels = match input {
        MaxElementsNumber(n) => ((*n + 1) as f32).log2().ceil() as u32,
        NumberOfLevels(n) => *n,
    };
    let mut nodes_updated = levels;

    // AVL rotations may update additional nodes
    if levels > 2 {
        nodes_updated += 2;  // At most 2 extra nodes for rotations
    }

    cost.storage_cost.replaced_bytes += nodes_updated * MERK_BIGGEST_VALUE_SIZE;
    cost.storage_loaded_bytes +=
        nodes_updated as u64 * (MERK_BIGGEST_VALUE_SIZE + MERK_BIGGEST_KEY_SIZE) as u64;
    cost.seek_count += nodes_updated;
    cost.hash_node_calls += nodes_updated * 2;
}
```

Constantes utilizadas:

```rust
pub const MERK_BIGGEST_VALUE_SIZE: u32 = u16::MAX as u32;  // 65535
pub const MERK_BIGGEST_KEY_SIZE: u32 = 256;
```

---
