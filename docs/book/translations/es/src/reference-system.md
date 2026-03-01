# El Sistema de Referencias

## Por Qué Existen las Referencias

En una base de datos jerárquica, frecuentemente necesitas que los mismos datos sean accesibles desde múltiples
rutas. Por ejemplo, los documentos podrían almacenarse bajo su contrato pero también ser
consultables por identidad del propietario. Las **referencias** (References) son la respuesta de GroveDB — son
punteros de una ubicación a otra, similares a los enlaces simbólicos en un sistema de archivos.

```mermaid
graph LR
    subgraph primary["Almacenamiento Primario"]
        item["contracts/C1/docs/D1<br/><b>Item</b>(data)"]
    end
    subgraph secondary["Índice Secundario"]
        ref["identities/alice/docs/D1<br/><b>Reference</b>"]
    end
    ref -->|"points to"| item

    style primary fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style secondary fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style ref fill:#fef9e7,stroke:#f39c12,stroke-width:2px
```

Propiedades clave:
- Las referencias son **autenticadas** — el value_hash de la referencia incluye tanto la
  referencia misma como el elemento referenciado
- Las referencias pueden estar **encadenadas** — una referencia puede apuntar a otra referencia
- La detección de ciclos previene bucles infinitos
- Un límite de saltos configurable previene el agotamiento de recursos

## Los Siete Tipos de Referencia

```rust
// grovedb-element/src/reference_path/mod.rs
pub enum ReferencePathType {
    AbsolutePathReference(Vec<Vec<u8>>),
    UpstreamRootHeightReference(u8, Vec<Vec<u8>>),
    UpstreamRootHeightWithParentPathAdditionReference(u8, Vec<Vec<u8>>),
    UpstreamFromElementHeightReference(u8, Vec<Vec<u8>>),
    CousinReference(Vec<u8>),
    RemovedCousinReference(Vec<Vec<u8>>),
    SiblingReference(Vec<u8>),
}
```

Recorramos cada uno con diagramas.

### AbsolutePathReference

El tipo más simple. Almacena la ruta completa al objetivo:

```mermaid
graph TD
    subgraph root["Root Merk — path: []"]
        A["A<br/>Tree"]
        P["P<br/>Tree"]
    end

    subgraph merkA["Merk [A]"]
        B["B<br/>Tree"]
    end

    subgraph merkP["Merk [P]"]
        Q["Q<br/>Tree"]
    end

    subgraph merkAB["Merk [A, B]"]
        X["X = Reference<br/>AbsolutePathRef([P, Q, R])"]
    end

    subgraph merkPQ["Merk [P, Q]"]
        R["R = Item<br/>&quot;target&quot;"]
    end

    A -.-> B
    P -.-> Q
    B -.-> X
    Q -.-> R
    X ==>|"resolves to [P, Q, R]"| R

    style merkAB fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style merkPQ fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style X fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style R fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

> X almacena la ruta absoluta completa `[P, Q, R]`. Sin importar dónde esté ubicado X, siempre se resuelve al mismo objetivo.

### UpstreamRootHeightReference

Conserva los primeros N segmentos de la ruta actual, luego añade una nueva ruta:

```mermaid
graph TD
    subgraph resolve["Resolución: conservar primeros 2 segmentos + añadir [P, Q]"]
        direction LR
        curr["current: [A, B, C, D]"] --> keep["keep first 2: [A, B]"] --> append["append: [A, B, <b>P, Q</b>]"]
    end

    subgraph grove["Jerarquía del Grove"]
        gA["A (height 0)"]
        gB["B (height 1)"]
        gC["C (height 2)"]
        gD["D (height 3)"]
        gX["X = Reference<br/>UpstreamRootHeight(2, [P,Q])"]
        gP["P (height 2)"]
        gQ["Q (height 3) — target"]

        gA --> gB
        gB --> gC
        gB -->|"keep first 2 → [A,B]<br/>then descend [P,Q]"| gP
        gC --> gD
        gD -.-> gX
        gP --> gQ
    end

    gX ==>|"resolves to"| gQ

    style resolve fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style gX fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style gQ fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

### UpstreamRootHeightWithParentPathAdditionReference

Como UpstreamRootHeight, pero re-añade el último segmento de la ruta actual:

```text
    Referencia en ruta [A, B, C, D, E] key=X
    UpstreamRootHeightWithParentPathAdditionReference(2, [P, Q])

    Ruta actual:       [A, B, C, D, E]
    Conservar primeros 2: [A, B]
    Añadir [P, Q]:     [A, B, P, Q]
    Re-añadir último:  [A, B, P, Q, E]   ← "E" de la ruta original añadido de vuelta

    Útil para: índices donde la clave padre debe preservarse
```

### UpstreamFromElementHeightReference

Descarta los últimos N segmentos, luego añade:

```text
    Referencia en ruta [A, B, C, D] key=X
    UpstreamFromElementHeightReference(1, [P, Q])

    Ruta actual:      [A, B, C, D]
    Descartar último 1: [A, B, C]
    Añadir [P, Q]:    [A, B, C, P, Q]
```

### CousinReference

Reemplaza solo el padre inmediato con una nueva clave:

```mermaid
graph TD
    subgraph resolve["Resolución: quitar últimos 2, añadir primo C, añadir clave X"]
        direction LR
        r1["path: [A, B, M, D]"] --> r2["pop last 2: [A, B]"] --> r3["push C: [A, B, C]"] --> r4["push key X: [A, B, C, X]"]
    end

    subgraph merkAB["Merk [A, B]"]
        M["M<br/>Tree"]
        C["C<br/>Tree<br/>(primo de M)"]
    end

    subgraph merkABM["Merk [A, B, M]"]
        D["D<br/>Tree"]
    end

    subgraph merkABMD["Merk [A, B, M, D]"]
        Xref["X = Reference<br/>CousinReference(C)"]
    end

    subgraph merkABC["Merk [A, B, C]"]
        Xtarget["X = Item<br/>(target)"]
    end

    M -.-> D
    D -.-> Xref
    C -.-> Xtarget
    Xref ==>|"resolves to [A, B, C, X]"| Xtarget

    style resolve fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style Xref fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style Xtarget fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style M fill:#fadbd8,stroke:#e74c3c
    style C fill:#d5f5e3,stroke:#27ae60
```

> El "primo" es un subárbol hermano del abuelo de la referencia. La referencia navega dos niveles hacia arriba, luego desciende al subárbol primo.

### RemovedCousinReference

Como CousinReference pero reemplaza el padre con una ruta de múltiples segmentos:

```text
    Referencia en ruta [A, B, C, D] key=X
    RemovedCousinReference([M, N])

    Ruta actual:   [A, B, C, D]
    Quitar padre C: [A, B]
    Añadir [M, N]: [A, B, M, N]
    Añadir clave X: [A, B, M, N, X]
```

### SiblingReference

La referencia relativa más simple — solo cambia la clave dentro del mismo padre:

```mermaid
graph TD
    subgraph merk["Merk [A, B, C] — mismo árbol, misma ruta"]
        M_sib["M"]
        X_sib["X = Reference<br/>SiblingRef(Y)"]
        Y_sib["Y = Item<br/>(target)"]
        Z_sib["Z = Item"]
        M_sib --> X_sib
        M_sib --> Y_sib
    end

    X_sib ==>|"resolves to [A, B, C, Y]"| Y_sib

    style merk fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style X_sib fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style Y_sib fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

> El tipo de referencia más simple. X e Y son hermanos en el mismo árbol Merk — la resolución solo cambia la clave manteniendo la misma ruta.

## Seguimiento de Referencias y el Límite de Saltos

Cuando GroveDB encuentra un elemento Reference, debe **seguirlo** para encontrar el
valor real. Dado que las referencias pueden apuntar a otras referencias, esto implica un bucle:

```rust
// grovedb/src/reference_path.rs
pub const MAX_REFERENCE_HOPS: usize = 10;

pub fn follow_reference(...) -> CostResult<ResolvedReference, Error> {
    let mut hops_left = MAX_REFERENCE_HOPS;
    let mut visited = HashSet::new();

    while hops_left > 0 {
        // Resolve reference path to absolute path
        let target_path = current_ref.absolute_qualified_path(...);

        // Check for cycles
        if !visited.insert(target_path.clone()) {
            return Err(Error::CyclicReference);
        }

        // Fetch element at target
        let element = Element::get(target_path);

        match element {
            Element::Reference(next_ref, ..) => {
                // Still a reference — keep following
                current_ref = next_ref;
                hops_left -= 1;
            }
            other => {
                // Found the actual element!
                return Ok(ResolvedReference { element: other, ... });
            }
        }
    }

    Err(Error::ReferenceLimit)  // Exceeded 10 hops
}
```

## Detección de Ciclos

El `visited` HashSet rastrea todas las rutas que hemos visto. Si encontramos una ruta que ya
hemos visitado, tenemos un ciclo:

```mermaid
graph LR
    A["A<br/>Reference"] -->|"step 1"| B["B<br/>Reference"]
    B -->|"step 2"| C["C<br/>Reference"]
    C -->|"step 3"| A

    style A fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style B fill:#fef9e7,stroke:#f39c12
    style C fill:#fef9e7,stroke:#f39c12
```

> **Traza de detección de ciclos:**
>
> | Paso | Seguir | conjunto visited | Resultado |
> |------|--------|-------------|--------|
> | 1 | Comenzar en A | { A } | A es Ref → seguir |
> | 2 | A → B | { A, B } | B es Ref → seguir |
> | 3 | B → C | { A, B, C } | C es Ref → seguir |
> | 4 | C → A | ¡A ya está en visited! | **Error::CyclicRef** |
>
> Sin detección de ciclos, esto se ejecutaría para siempre. `MAX_REFERENCE_HOPS = 10` también limita la profundidad de recorrido para cadenas largas.

## Referencias en Merk — Hashes de Valor Combinados

Cuando una Reference se almacena en un árbol Merk, su `value_hash` debe autenticar
tanto la estructura de la referencia como los datos referenciados:

```rust
// merk/src/tree/kv.rs
pub fn update_hashes_using_reference_value_hash(
    mut self,
    reference_value_hash: CryptoHash,
) -> CostContext<Self> {
    // Hash the reference element's own bytes
    let actual_value_hash = value_hash(self.value_as_slice());

    // Combine: H(reference_bytes) ⊕ H(referenced_data)
    let combined = combine_hash(&actual_value_hash, &reference_value_hash);

    self.value_hash = combined;
    self.hash = kv_digest_to_kv_hash(self.key(), self.value_hash());
    // ...
}
```

Esto significa que cambiar la referencia misma O los datos a los que apunta
cambiará el hash raíz — ambos están vinculados criptográficamente.

---
