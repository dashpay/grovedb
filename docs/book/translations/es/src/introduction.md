# Introducción — ¿Qué es GroveDB?

## La Idea Central

GroveDB es una **estructura de datos autenticada jerárquica** — esencialmente un *grove*
(bosque, o árbol de árboles) construido sobre árboles AVL de Merkle. Cada nodo en la
base de datos es parte de un árbol autenticado criptográficamente, y cada árbol puede
contener otros árboles como hijos, formando una jerarquía profunda de estado verificable.

```mermaid
graph TD
    subgraph root["Root Merk Tree"]
        R_contracts["&quot;contracts&quot;<br/><i>Tree</i>"]
        R_identities["&quot;identities&quot;<br/><i>Tree</i>"]
        R_balances["&quot;balances&quot;<br/><i>SumTree</i>"]
        R_contracts --- R_identities
        R_contracts --- R_balances
    end

    subgraph ident["Identities Merk"]
        I_bob["&quot;bob&quot;<br/><i>Tree</i>"]
        I_alice["&quot;alice&quot;<br/><i>Tree</i>"]
        I_carol["&quot;carol&quot;<br/><i>Item</i>"]
        I_bob --- I_alice
        I_bob --- I_carol
    end

    subgraph contracts["Contracts Merk"]
        C_c2["&quot;C2&quot;<br/><i>Item</i>"]
        C_c1["&quot;C1&quot;<br/><i>Item</i>"]
        C_c3["&quot;C3&quot;<br/><i>Item</i>"]
        C_c2 --- C_c1
        C_c2 --- C_c3
    end

    subgraph balances["Balances SumTree — sum=5300"]
        B_bob["&quot;bob&quot;<br/>SumItem(2500)"]
        B_al["&quot;alice&quot;<br/>SumItem(2000)"]
        B_eve["&quot;eve&quot;<br/>SumItem(800)"]
        B_bob --- B_al
        B_bob --- B_eve
    end

    subgraph alice_merk["Alice Merk"]
        A_name["&quot;name&quot; → Alice"]
        A_bal["&quot;balance&quot; → 1000"]
    end

    subgraph bob_merk["Bob Merk"]
        Bo_name["&quot;name&quot; → Bob"]
    end

    R_identities -.->|subtree| ident
    R_contracts -.->|subtree| contracts
    R_balances -.->|subtree| balances
    I_alice -.->|subtree| alice_merk
    I_bob -.->|subtree| bob_merk

    style root fill:#e8f4fd,stroke:#2980b9,stroke-width:2px
    style ident fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style contracts fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style balances fill:#fdedec,stroke:#e74c3c,stroke-width:2px
    style alice_merk fill:#eafaf1,stroke:#27ae60,stroke-width:2px
    style bob_merk fill:#eafaf1,stroke:#27ae60,stroke-width:2px
```

> Cada caja de color es un **árbol Merk separado**. Las flechas discontinuas muestran la relación de subárbol — un elemento Tree en el padre contiene la clave raíz del Merk hijo.

En una base de datos tradicional, podrías almacenar datos en un almacén clave-valor plano con
un único árbol de Merkle encima para autenticación. GroveDB toma un enfoque diferente:
anida árboles de Merkle dentro de árboles de Merkle. Esto te proporciona:

1. **Índices secundarios eficientes** — consulta por cualquier ruta, no solo por clave primaria
2. **Pruebas criptográficas compactas** — demuestra la existencia (o ausencia) de cualquier dato
3. **Datos agregados** — los árboles pueden sumar, contar o agregar automáticamente
   sus hijos
4. **Operaciones atómicas entre árboles** — las operaciones por lotes abarcan múltiples subárboles

## Por Qué Existe GroveDB

GroveDB fue diseñado para **Dash Platform**, una plataforma de aplicaciones descentralizada
donde cada pieza de estado debe ser:

- **Autenticada**: Cualquier nodo puede demostrar cualquier pieza de estado a un cliente ligero
- **Determinista**: Cada nodo calcula exactamente la misma raíz de estado
- **Eficiente**: Las operaciones deben completarse dentro de las restricciones de tiempo de bloque
- **Consultable**: Las aplicaciones necesitan consultas ricas, no solo búsquedas por clave

Los enfoques tradicionales se quedan cortos:

| Enfoque | Problema |
|----------|---------|
| Árbol de Merkle simple | Solo soporta búsquedas por clave, sin consultas por rango |
| MPT de Ethereum | Rebalanceo costoso, tamaños de prueba grandes |
| Clave-valor plano + árbol único | Sin consultas jerárquicas, una sola prueba cubre todo |
| Árbol B | No está naturalmente Merklizado, autenticación compleja |

GroveDB resuelve estos problemas combinando las **garantías probadas de balanceo de los árboles AVL**
con **anidamiento jerárquico** y un **sistema rico de tipos de elementos**.

## Visión General de la Arquitectura

GroveDB está organizado en capas distintas, cada una con una responsabilidad clara:

```mermaid
graph TD
    APP["<b>Capa de Aplicación</b><br/>Dash Platform, etc.<br/><code>insert() get() query() prove() apply_batch()</code>"]

    GROVE["<b>Núcleo de GroveDB</b> — <code>grovedb/src/</code><br/>Gestión jerárquica de subárboles · Sistema de tipos de elementos<br/>Resolución de referencias · Operaciones por lotes · Pruebas multi-capa"]

    MERK["<b>Capa Merk</b> — <code>merk/src/</code><br/>Árbol AVL de Merkle · Rotaciones de auto-balanceo<br/>Sistema de enlaces · Hashing Blake3 · Codificación de pruebas"]

    STORAGE["<b>Capa de Almacenamiento</b> — <code>storage/src/</code><br/>RocksDB OptimisticTransactionDB<br/>4 familias de columnas · Aislamiento por prefijo Blake3 · Escrituras por lotes"]

    COST["<b>Capa de Costos</b> — <code>costs/src/</code><br/>Seguimiento de OperationCost · Mónada CostContext<br/>Estimación de peor caso y caso promedio"]

    APP ==>|"escrituras ↓"| GROVE
    GROVE ==>|"ops de árbol"| MERK
    MERK ==>|"E/S de disco"| STORAGE
    STORAGE -.->|"acumulación de costos ↑"| COST
    COST -.-> MERK
    MERK -.-> GROVE
    GROVE ==>|"lecturas ↑"| APP

    style APP fill:#f5f5f5,stroke:#999,stroke-width:1px
    style GROVE fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style MERK fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style STORAGE fill:#fdebd0,stroke:#e67e22,stroke-width:2px
    style COST fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

Los datos fluyen **hacia abajo** a través de estas capas durante las escrituras y **hacia arriba** durante las lecturas.
Cada operación acumula costos mientras atraviesa la pila, permitiendo una contabilidad precisa
de recursos.

---
