# Introducao — O que e o GroveDB?

## A Ideia Central

GroveDB e uma **estrutura de dados autenticada hierarquica** — essencialmente um *grove*
(bosque, ou arvore de arvores) construido sobre arvores AVL de Merkle. Cada no no banco
de dados faz parte de uma arvore criptograficamente autenticada, e cada arvore pode
conter outras arvores como filhas, formando uma hierarquia profunda de estado verificavel.

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

> Cada caixa colorida e uma **arvore Merk separada**. As setas tracejadas mostram o relacionamento de subarvore — um elemento Tree no pai contem a chave raiz da Merk filha.

Em um banco de dados tradicional, voce poderia armazenar dados em um armazem chave-valor
plano com uma unica arvore de Merkle no topo para autenticacao. O GroveDB adota uma
abordagem diferente: ele aninha arvores de Merkle dentro de arvores de Merkle. Isso
proporciona:

1. **Indices secundarios eficientes** — consulta por qualquer caminho, nao apenas por chave primaria
2. **Provas criptograficas compactas** — prova a existencia (ou ausencia) de qualquer dado
3. **Dados agregados** — arvores podem automaticamente somar, contar ou agregar de outras formas seus filhos
4. **Operacoes atomicas entre arvores** — operacoes em lote abrangem multiplas subarvores

## Por que o GroveDB Existe

O GroveDB foi projetado para o **Dash Platform**, uma plataforma de aplicacoes
descentralizada onde cada pedaco de estado deve ser:

- **Autenticado**: Qualquer no pode provar qualquer pedaco de estado para um cliente leve
- **Deterministico**: Todo no computa exatamente o mesmo hash raiz de estado
- **Eficiente**: As operacoes devem ser concluidas dentro das restricoes de tempo do bloco
- **Consultavel**: Aplicacoes precisam de consultas ricas, nao apenas buscas por chave

Abordagens tradicionais ficam aquem:

| Abordagem | Problema |
|-----------|----------|
| Arvore de Merkle simples | Suporta apenas buscas por chave, sem consultas por faixa |
| MPT do Ethereum | Rebalanceamento caro, provas grandes |
| Chave-valor plano + arvore unica | Sem consultas hierarquicas, uma unica prova cobre tudo |
| B-tree | Nao e naturalmente Merklizada, autenticacao complexa |

O GroveDB resolve isso combinando as **garantias comprovadas de balanceamento das arvores AVL**
com **aninhamento hierarquico** e um **sistema rico de tipos de elementos**.

## Visao Geral da Arquitetura

O GroveDB e organizado em camadas distintas, cada uma com uma responsabilidade clara:

```mermaid
graph TD
    APP["<b>Camada de Aplicacao</b><br/>Dash Platform, etc.<br/><code>insert() get() query() prove() apply_batch()</code>"]

    GROVE["<b>Nucleo do GroveDB</b> — <code>grovedb/src/</code><br/>Gerenciamento hierarquico de subarvores · Sistema de tipos de elementos<br/>Resolucao de referencias · Ops em lote · Provas multicamadas"]

    MERK["<b>Camada Merk</b> — <code>merk/src/</code><br/>Arvore AVL de Merkle · Rotacoes auto-balanceantes<br/>Sistema de links · Hashing Blake3 · Codificacao de provas"]

    STORAGE["<b>Camada de Armazenamento</b> — <code>storage/src/</code><br/>RocksDB OptimisticTransactionDB<br/>4 familias de colunas · Isolamento por prefixo Blake3 · Escritas em lote"]

    COST["<b>Camada de Custos</b> — <code>costs/src/</code><br/>Rastreamento de OperationCost · Monada CostContext<br/>Estimativa de pior caso &amp; caso medio"]

    APP ==>|"escritas ↓"| GROVE
    GROVE ==>|"ops de arvore"| MERK
    MERK ==>|"E/S de disco"| STORAGE
    STORAGE -.->|"acumulacao de custos ↑"| COST
    COST -.-> MERK
    MERK -.-> GROVE
    GROVE ==>|"leituras ↑"| APP

    style APP fill:#f5f5f5,stroke:#999,stroke-width:1px
    style GROVE fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style MERK fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style STORAGE fill:#fdebd0,stroke:#e67e22,stroke-width:2px
    style COST fill:#fadbd8,stroke:#e74c3c,stroke-width:2px
```

Os dados fluem **para baixo** atraves dessas camadas durante as escritas e **para cima**
durante as leituras. Cada operacao acumula custos conforme percorre a pilha, permitindo
uma contabilizacao precisa de recursos.

---
