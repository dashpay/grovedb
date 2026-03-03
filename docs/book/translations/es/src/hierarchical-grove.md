# El Grove Jerárquico — Árbol de Árboles

## Cómo los Subárboles se Anidan Dentro de los Árboles Padre

La característica definitoria de GroveDB es que un árbol Merk puede contener elementos que son
a su vez árboles Merk. Esto crea un **espacio de nombres jerárquico**:

```mermaid
graph TD
    subgraph root["ROOT MERK TREE — path: []"]
        contracts["&quot;contracts&quot;<br/>Tree"]
        identities["&quot;identities&quot;<br/>Tree"]
        balances["&quot;balances&quot;<br/>SumTree, sum=0"]
        contracts --> identities
        contracts --> balances
    end

    subgraph ident["IDENTITIES MERK — path: [&quot;identities&quot;]"]
        bob456["&quot;bob456&quot;<br/>Tree"]
        alice123["&quot;alice123&quot;<br/>Tree"]
        eve["&quot;eve&quot;<br/>Item"]
        bob456 --> alice123
        bob456 --> eve
    end

    subgraph bal["BALANCES MERK (SumTree) — path: [&quot;balances&quot;]"]
        bob_bal["&quot;bob456&quot;<br/>SumItem(800)"]
    end

    subgraph alice_tree["ALICE123 MERK — path: [&quot;identities&quot;, &quot;alice123&quot;]"]
        name["&quot;name&quot;<br/>Item(&quot;Al&quot;)"]
        balance_item["&quot;balance&quot;<br/>SumItem(1000)"]
        docs["&quot;docs&quot;<br/>Tree"]
        name --> balance_item
        name --> docs
    end

    identities -.-> bob456
    balances -.-> bob_bal
    alice123 -.-> name
    docs -.->|"... more subtrees"| more["..."]

    style root fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style ident fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style bal fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style alice_tree fill:#e8daef,stroke:#8e44ad,stroke-width:2px
    style more fill:none,stroke:none
```

> Cada caja coloreada es un árbol Merk separado. Las flechas discontinuas representan los enlaces portal desde los elementos Tree hacia sus árboles Merk hijos. La ruta hacia cada Merk se muestra en su etiqueta.

## Sistema de Direccionamiento por Rutas

Cada elemento en GroveDB se direcciona mediante una **ruta** (path) — una secuencia de cadenas de bytes
que navega desde la raíz a través de subárboles hasta la clave objetivo:

```text
    Path: ["identities", "alice123", "name"]

    Paso 1: En el árbol raíz, buscar "identities" → elemento Tree
    Paso 2: Abrir subárbol identities, buscar "alice123" → elemento Tree
    Paso 3: Abrir subárbol alice123, buscar "name" → Item("Alice")
```

Las rutas se representan como `Vec<Vec<u8>>` o usando el tipo `SubtreePath` para
manipulación eficiente sin asignación:

```rust
// The path to the element (all segments except the last)
let path: &[&[u8]] = &[b"identities", b"alice123"];
// The key within the final subtree
let key: &[u8] = b"name";
```

## Generación de Prefijos Blake3 para Aislamiento de Almacenamiento

Cada subárbol en GroveDB obtiene su propio **espacio de nombres de almacenamiento aislado** en RocksDB.
El espacio de nombres se determina hasheando la ruta con Blake3:

```rust
pub type SubtreePrefix = [u8; 32];

// The prefix is computed by hashing the path segments
// storage/src/rocksdb_storage/storage.rs
```

Por ejemplo:

```text
    Path: ["identities", "alice123"]
    Prefix: Blake3(["identities", "alice123"]) = [0xab, 0x3f, ...]  (32 bytes)

    En RocksDB, las claves para este subárbol se almacenan como:
    [prefix: 32 bytes][original_key]

    Entonces "name" en este subárbol se convierte en:
    [0xab, 0x3f, ...][0x6e, 0x61, 0x6d, 0x65]  ("name")
```

Esto asegura:
- Sin colisiones de claves entre subárboles (prefijo de 32 bytes = aislamiento de 256 bits)
- Cálculo eficiente del prefijo (un solo hash Blake3 sobre los bytes de la ruta)
- Los datos del subárbol están co-ubicados en RocksDB para eficiencia de caché

## Propagación del Hash Raíz a Través de la Jerarquía

Cuando un valor cambia profundamente en el grove, el cambio debe **propagarse hacia arriba** para
actualizar el hash raíz:

```text
    Cambio: Actualizar "name" a "ALICE" en identities/alice123/

    Paso 1: Actualizar valor en el árbol Merk de alice123
            → el árbol alice123 obtiene nuevo hash raíz: H_alice_new

    Paso 2: Actualizar elemento "alice123" en el árbol identities
            → el value_hash del árbol identities para "alice123" =
              combine_hash(H(tree_element_bytes), H_alice_new)
            → el árbol identities obtiene nuevo hash raíz: H_ident_new

    Paso 3: Actualizar elemento "identities" en el árbol raíz
            → el value_hash del árbol raíz para "identities" =
              combine_hash(H(tree_element_bytes), H_ident_new)
            → EL HASH RAÍZ cambia
```

```mermaid
graph TD
    subgraph step3["PASO 3: Actualizar árbol raíz"]
        R3["El árbol raíz recalcula:<br/>value_hash para &quot;identities&quot; =<br/>combine_hash(H(tree_bytes), H_ident_NEW)<br/>→ nuevo HASH RAÍZ"]
    end
    subgraph step2["PASO 2: Actualizar árbol identities"]
        R2["El árbol identities recalcula:<br/>value_hash para &quot;alice123&quot; =<br/>combine_hash(H(tree_bytes), H_alice_NEW)<br/>→ nuevo hash raíz: H_ident_NEW"]
    end
    subgraph step1["PASO 1: Actualizar Merk alice123"]
        R1["El árbol alice123 recalcula:<br/>value_hash(&quot;ALICE&quot;) → nuevo kv_hash<br/>→ nuevo hash raíz: H_alice_NEW"]
    end

    R1 -->|"H_alice_NEW flows up"| R2
    R2 -->|"H_ident_NEW flows up"| R3

    style step1 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style step2 fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style step3 fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

**Antes vs Después** — nodos cambiados marcados en rojo:

```mermaid
graph TD
    subgraph before["ANTES"]
        B_root["Root: aabb1122"]
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

    subgraph after["DESPUÉS"]
        A_root["Root: ff990033"]
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

> Solo los nodos en la ruta desde el valor cambiado hasta la raíz se recalculan. Los hermanos y otras ramas permanecen sin cambios.

La propagación se implementa mediante `propagate_changes_with_transaction`, que recorre
la ruta hacia arriba desde el subárbol modificado hasta la raíz, actualizando el hash del elemento de cada padre
en el camino.

## Ejemplo de Estructura de Grove Multi-Nivel

Aquí hay un ejemplo completo mostrando cómo Dash Platform estructura su estado:

```mermaid
graph TD
    ROOT["GroveDB Root"]

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

Cada caja es un árbol Merk separado, autenticado completamente hasta un único hash
raíz en el que los validadores están de acuerdo.

---
