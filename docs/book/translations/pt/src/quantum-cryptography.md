# Criptografia Quântica — Análise de Ameaças Pós-Quânticas

Este capítulo analisa como computadores quânticos afetariam as primitivas
criptográficas usadas no GroveDB e nos protocolos de transações blindadas
construídos sobre ele (Orchard, Dash Platform). Abrange quais componentes são
vulneráveis, quais são seguros, o que significa "colher agora, decriptar depois"
para dados armazenados e quais estratégias de mitigação existem — incluindo
designs híbridos de KEM.

## Dois Algoritmos Quânticos Relevantes

Apenas dois algoritmos quânticos são relevantes para a criptografia na prática:

**O algoritmo de Shor** resolve o problema do logaritmo discreto (e a
fatoração de inteiros) em tempo polinomial. Para uma curva elíptica de 255 bits
como Pallas, isso requer aproximadamente 510 qubits lógicos — mas com a
sobrecarga de correção de erros, o requisito real é de aproximadamente 4 milhões
de qubits físicos. O algoritmo de Shor **quebra completamente** toda a
criptografia de curvas elípticas, independentemente do tamanho da chave.

**O algoritmo de Grover** fornece uma aceleração quadrática para busca por
força bruta. Uma chave simétrica de 256 bits efetivamente se torna de 128 bits.
No entanto, a profundidade do circuito para Grover num espaço de chaves de
128 bits ainda é de 2^64 operações quânticas — muitos criptógrafos acreditam que
isso nunca será prático em hardware real devido aos limites de decoerência.
Grover reduz as margens de segurança, mas não quebra criptografia simétrica
bem parametrizada.

| Algoritmo | Alvos | Aceleração | Impacto prático |
|-----------|-------|------------|-----------------|
| **Shor** | Logaritmo discreto ECC, fatoração RSA | Tempo polinomial (aceleração exponencial sobre clássico) | **Quebra total** de ECC |
| **Grover** | Busca de chave simétrica, pré-imagem de hash | Quadrática (reduz bits da chave pela metade) | 256 bits → 128 bits (ainda seguro) |

## Primitivas Criptográficas do GroveDB

O GroveDB e o protocolo blindado baseado em Orchard usam uma combinação de
primitivas de curvas elípticas e primitivas simétricas/baseadas em hash.
A tabela abaixo classifica cada primitiva pela sua vulnerabilidade quântica:

### Vulnerável ao Quântico (algoritmo de Shor — 0 bits pós-quânticos)

| Primitiva | Onde é usada | O que quebra |
|-----------|-------------|-------------|
| **Pallas ECDLP** | Compromissos de notas (cmx), chaves efémeras (epk/esk), chaves de visualização (ivk), chaves de pagamento (pk_d), derivação de anuladores | Recuperar qualquer chave privada a partir da sua contraparte pública |
| **Acordo de chaves ECDH** (Pallas) | Derivação de chaves simétricas de encriptação para textos cifrados de notas | Recuperar segredo compartilhado → decriptar todas as notas |
| **Hash Sinsemilla** | Caminhos Merkle da CommitmentTree, hashing em circuito | Resistência a colisões depende do ECDLP; degrada quando Pallas é quebrada |
| **Halo 2 IPA** | Sistema de provas ZK (compromisso polinomial sobre curvas Pasta) | Forjar provas para declarações falsas (falsificação, gastos não autorizados) |
| **Compromissos de Pedersen** | Compromissos de valor (cv_net) ocultando montantes de transações | Recuperar montantes ocultos; forjar provas de balanço |

### Seguro contra Quântico (algoritmo de Grover — 128+ bits pós-quânticos)

| Primitiva | Onde é usada | Segurança pós-quântica |
|-----------|-------------|----------------------|
| **Blake3** | Hashes de nós da árvore Merk, nós MMR, raízes de estado da BulkAppendTree, prefixos de caminho de subárvores | Pré-imagem de 128 bits, segunda pré-imagem de 128 bits |
| **BLAKE2b-256** | KDF para derivação de chave simétrica, chave de cifra de saída, PRF^expand | Pré-imagem de 128 bits |
| **ChaCha20-Poly1305** | Encripta enc_ciphertext e out_ciphertext (chaves de 256 bits) | Busca de chave de 128 bits (seguro, mas o caminho de derivação de chave via ECDH não é) |
| **PRF^expand** (BLAKE2b-512) | Deriva esk, rcm, psi a partir de rseed | Segurança PRF de 128 bits |

### Infraestrutura do GroveDB: Considerada Segura contra Quântico sob as Suposições Atuais

Todas as estruturas de dados próprias do GroveDB dependem exclusivamente de hashing
Blake3, que é considerado resistente ao quântico sob as suposições criptográficas atuais:

- **Árvores AVL Merk** — hashes de nós, combined_value_hash, propagação de hash filho
- **Árvores MMR** — hashes de nós internos, computação de picos, derivação de raiz
- **BulkAppendTree** — cadeias de hash de buffer, raízes Merkle densas, MMR de épocas
- **Raiz de estado da CommitmentTree** — `blake3("ct_state" || sinsemilla_root || bulk_state_root)`
- **Prefixos de caminho de subárvores** — hashing Blake3 de segmentos de caminho
- **Provas V1** — cadeias de autenticação através da hierarquia Merk

**Nenhuma alteração necessária com base nos ataques conhecidos.** As provas de árvore
Merk do GroveDB, verificações de consistência MMR, raízes de época da BulkAppendTree
e todas as cadeias de autenticação de provas V1 são consideradas seguras contra
computadores quânticos. A infraestrutura baseada em hash é a parte mais forte do
sistema pós-quântico, embora as avaliações possam evoluir com novas técnicas
criptoanalíticas.

## Ameaças Retroativas vs em Tempo Real

Esta distinção é crítica para priorizar o que corrigir e quando.

**Ameaças retroativas** comprometem dados que já estão armazenados. Um
adversário grava dados hoje e os decripta quando computadores quânticos
estiverem disponíveis. Essas ameaças **não podem ser mitigadas após o facto** —
uma vez que os dados estão na blockchain, não podem ser re-encriptados ou
recuperados.

**Ameaças em tempo real** afetam apenas transações criadas no futuro. Um
adversário com um computador quântico poderia forjar assinaturas ou provas,
mas apenas para novas transações. Transações antigas já foram validadas e
confirmadas pela rede.

| Ameaça | Tipo | O que é exposto | Urgência |
|--------|------|----------------|----------|
| **Decriptação de notas** (enc_ciphertext) | **Retroativa** | Conteúdo das notas: destinatário, montante, memo, rseed | **Alta** — armazenado para sempre |
| **Abertura de compromisso de valor** (cv_net) | **Retroativa** | Montantes de transações (mas não remetente/destinatário) | **Média** — apenas montantes |
| **Dados de recuperação do remetente** (out_ciphertext) | **Retroativa** | Chaves de recuperação do remetente para notas enviadas | **Alta** — armazenado para sempre |
| Falsificação de autorização de gasto | Tempo real | Poderia forjar novas assinaturas de gasto | Baixa — atualizar antes do QC chegar |
| Falsificação de prova Halo 2 | Tempo real | Poderia forjar novas provas (falsificação) | Baixa — atualizar antes do QC chegar |
| Colisão Sinsemilla | Tempo real | Poderia forjar novos caminhos Merkle | Baixa — subsumida pela falsificação de provas |
| Falsificação de assinatura de vinculação | Tempo real | Poderia forjar novas provas de balanço | Baixa — atualizar antes do QC chegar |

### O Que Exatamente é Revelado?

**Se a encriptação de notas for quebrada** (a principal ameaça HNDL):

Um adversário quântico recupera `esk` a partir do `epk` armazenado via
algoritmo de Shor, calcula o segredo compartilhado ECDH, deriva a chave
simétrica e decripta `enc_ciphertext`. Isso revela o texto plano completo
da nota:

| Campo | Tamanho | O que revela |
|-------|---------|-------------|
| version | 1 byte | Versão do protocolo (não sensível) |
| diversifier | 11 bytes | Componente do endereço do destinatário |
| value | 8 bytes | Montante exato da transação |
| rseed | 32 bytes | Permite ligação de anuladores (desanonimiza o grafo de transações) |
| memo | 36 bytes (DashMemo) | Dados da aplicação, potencialmente identificadores |

Com `rseed` e `rho` (armazenados junto ao texto cifrado), o adversário pode
calcular `esk = PRF(rseed, rho)` e verificar a vinculação da chave efémera.
Combinado com o diversificador, isso liga entradas a saídas em todo o histórico
de transações — **desanonimização completa do pool blindado**.

**Se apenas os compromissos de valor forem quebrados** (ameaça HNDL secundária):

O adversário recupera `v` de `cv_net = [v]*V + [rcv]*R` resolvendo o ECDLP.
Isso revela **montantes de transações, mas não identidades de remetente ou
destinatário**. O adversário vê "alguém enviou 5.0 Dash para alguém" mas
não consegue vincular o montante a nenhum endereço ou identidade sem também
quebrar a encriptação de notas.

Por si só, montantes sem vinculação têm utilidade limitada. Mas combinados
com dados externos (timing, faturas conhecidas, montantes correspondentes a
pedidos públicos), ataques de correlação tornam-se possíveis.

## O Ataque "Colher Agora, Decriptar Depois"

Esta é a ameaça quântica mais urgente e prática.

**Modelo de ataque:** Um adversário a nível estatal (ou qualquer parte com
armazenamento suficiente) grava todos os dados de transações blindadas na
blockchain hoje. Estes dados são publicamente disponíveis na blockchain e
imutáveis. O adversário espera por um computador quântico criptograficamente
relevante (CRQC), e então:

```text
Step 1: Read stored record from CommitmentTree BulkAppendTree:
        cmx (32) || rho (32) || epk (32) || enc_ciphertext (104) || out_ciphertext (80)

Step 2: Solve ECDLP on Pallas via Shor's algorithm:
        epk = [esk] * g_d  →  recover esk

Step 3: Compute shared secret:
        shared_secret = [esk] * pk_d

Step 4: Derive symmetric key (BLAKE2b is quantum-safe, but input is compromised):
        K_enc = BLAKE2b-256("Zcash_OrchardKDF", shared_secret || epk)

Step 5: Decrypt enc_ciphertext using ChaCha20-Poly1305:
        → version || diversifier || value || rseed || memo

Step 6: With rseed + rho, link nullifiers to note commitments:
        esk = PRF(rseed, rho)
        → full transaction graph reconstruction
```

**Ponto chave:** A encriptação simétrica (ChaCha20-Poly1305) é perfeitamente
segura contra quântico. A vulnerabilidade está inteiramente no **caminho de
derivação de chave** — a chave simétrica é derivada de um segredo compartilhado
ECDH, e o ECDH é quebrado pelo algoritmo de Shor. O atacante não quebra a
encriptação; ele recupera a chave.

**Retroatividade:** Este ataque é **totalmente retroativo**. Cada nota
encriptada já armazenada na blockchain pode ser decriptada assim que um CRQC
existir. Os dados não podem ser re-encriptados ou protegidos após o facto.
É por isso que deve ser abordado antes de os dados serem armazenados, não depois.

## Mitigação: KEM Híbrido (ML-KEM + ECDH)

A defesa contra HNDL é derivar a chave de encriptação simétrica a partir de
**dois mecanismos de acordo de chaves independentes**, de modo que quebrar
apenas um seja insuficiente. Isto chama-se KEM híbrido.

### ML-KEM-768 (CRYSTALS-Kyber)

ML-KEM é o mecanismo de encapsulamento de chaves pós-quântico padronizado pelo
NIST (FIPS 203, agosto de 2024), baseado no problema Module Learning With
Errors (MLWE).

| Parâmetro | ML-KEM-512 | ML-KEM-768 | ML-KEM-1024 |
|-----------|-----------|-----------|------------|
| Chave pública (ek) | 800 bytes | **1.184 bytes** | 1.568 bytes |
| Texto cifrado (ct) | 768 bytes | **1.088 bytes** | 1.568 bytes |
| Segredo compartilhado | 32 bytes | 32 bytes | 32 bytes |
| Categoria NIST | 1 (128-bit) | **3 (192-bit)** | 5 (256-bit) |

**ML-KEM-768** é a escolha recomendada — é o conjunto de parâmetros usado pelo
X-Wing, PQXDH do Signal e troca híbrida de chaves TLS do Chrome/Firefox.
A Categoria 3 fornece uma margem confortável contra avanços futuros na
criptoanálise de reticulados.

### Como o Esquema Híbrido Funciona

**Fluxo atual (vulnerável):**

```text
Sender:
  esk = PRF(rseed, rho)                    // deterministic from note
  epk = [esk] * g_d                         // Pallas curve point
  shared_secret = [esk] * pk_d              // ECDH (broken by Shor's)
  K_enc = BLAKE2b("Zcash_OrchardKDF", shared_secret || epk)
  enc_ciphertext = ChaCha20(K_enc, note_plaintext)
```

**Fluxo híbrido (resistente a quântico):**

```text
Sender:
  esk = PRF(rseed, rho)                    // unchanged
  epk = [esk] * g_d                         // unchanged
  ss_ecdh = [esk] * pk_d                    // same ECDH

  (ct_pq, ss_pq) = ML-KEM-768.Encaps(ek_pq)  // NEW: lattice-based KEM
                                                // ek_pq from recipient's address

  K_enc = BLAKE2b(                          // MODIFIED: combines both secrets
      "DashPlatform_HybridKDF",
      ss_ecdh || ss_pq || ct_pq || epk
  )

  enc_ciphertext = ChaCha20(K_enc, note_plaintext)  // unchanged
```

**Decriptação pelo destinatário:**

```text
Recipient:
  ss_ecdh = [ivk] * epk                    // same ECDH (using incoming viewing key)
  ss_pq = ML-KEM-768.Decaps(dk_pq, ct_pq)  // NEW: KEM decapsulation
  K_enc = BLAKE2b("DashPlatform_HybridKDF", ss_ecdh || ss_pq || ct_pq || epk)
  note_plaintext = ChaCha20.Decrypt(K_enc, enc_ciphertext)
```

### Garantia de Segurança

O KEM combinado é IND-CCA2 seguro se **qualquer um** dos KEMs componentes for
seguro. Isto é formalmente provado por [Giacon, Heuer e Poettering (2018)](https://eprint.iacr.org/2018/024)
para combinadores de KEM usando um PRF (BLAKE2b qualifica-se), e independentemente
pela [prova de segurança do X-Wing](https://eprint.iacr.org/2024/039).

| Cenário | ECDH | ML-KEM | Chave combinada | Estado |
|---------|------|--------|----------------|--------|
| Mundo clássico | Seguro | Seguro | **Seguro** | Ambos intactos |
| Quântico quebra ECC | **Quebrado** | Seguro | **Seguro** | ML-KEM protege |
| Avanços em reticulados quebram ML-KEM | Seguro | **Quebrado** | **Seguro** | ECDH protege (igual a hoje) |
| Ambos quebrados | Quebrado | Quebrado | **Quebrado** | Requer dois avanços simultâneos |

### Impacto no Tamanho

O KEM híbrido adiciona o texto cifrado ML-KEM-768 (1.088 bytes) a cada nota
armazenada e expande o texto cifrado de saída para incluir o segredo
compartilhado ML-KEM para recuperação do remetente:

**Registro armazenado por nota:**

```text
┌──────────────────────────────────────────────────────────────────┐
│  Current (280 bytes)         Hybrid (1,400 bytes)               │
│                                                                  │
│  cmx:             32         cmx:              32               │
│  rho:             32         rho:              32               │
│  epk:             32         epk:              32               │
│  enc_ciphertext: 104         ct_pq:         1,088  ← NEW       │
│  out_ciphertext:  80         enc_ciphertext:  104               │
│                              out_ciphertext:  112  ← +32        │
│  ─────────────────           ──────────────────────             │
│  Total:          280         Total:          1,400  (5.0x)      │
└──────────────────────────────────────────────────────────────────┘
```

**Armazenamento em escala:**

| Notas | Atual (280 B) | Híbrido (1.400 B) | Delta |
|-------|--------------|-------------------|-------|
| 100.000 | 26,7 MB | 133 MB | +106 MB |
| 1.000.000 | 267 MB | 1,33 GB | +1,07 GB |
| 10.000.000 | 2,67 GB | 13,3 GB | +10,7 GB |

**Tamanho do endereço:**

```text
Current:  diversifier (11) + pk_d (32) = 43 bytes
Hybrid:   diversifier (11) + pk_d (32) + ek_pq (1,184) = 1,227 bytes
```

A chave pública ML-KEM de 1.184 bytes deve ser incluída no endereço para que
remetentes possam realizar o encapsulamento. Com ~1.960 caracteres Bech32m,
é grande mas ainda cabe num código QR (máximo ~2.953 caracteres alfanuméricos).

### Gestão de Chaves

O par de chaves ML-KEM é derivado deterministicamente a partir da chave de gasto:

```text
SpendingKey (sk) [32 bytes]
  |
  +-> ... (all existing Orchard key derivation unchanged)
  |
  +-> ml_kem_d = PRF^expand(sk, [0x09])[0..32]
  +-> ml_kem_z = PRF^expand(sk, [0x0A])[0..32]
        |
        +-> (ek_pq, dk_pq) = ML-KEM-768.KeyGen(d=ml_kem_d, z=ml_kem_z)
              ek_pq: 1,184 bytes (public, included in address)
              dk_pq: 2,400 bytes (private, part of viewing key)
```

**Nenhuma alteração de backup necessária.** A frase semente existente de 24
palavras cobre a chave ML-KEM porque é derivada deterministicamente da chave
de gasto. A recuperação de carteira funciona como antes.

**Endereços diversificados** compartilham todos o mesmo `ek_pq` porque ML-KEM
não possui um mecanismo natural de diversificação como a multiplicação escalar
de Pallas. Isto significa que um observador com dois endereços de um utilizador
pode ligá-los comparando `ek_pq`.

### Desempenho de Decriptação por Tentativa

| Etapa | Atual | Híbrido | Delta |
|-------|-------|---------|-------|
| Pallas ECDH | ~100 us | ~100 us | — |
| ML-KEM-768 Decaps | — | ~40 us | +40 us |
| BLAKE2b KDF | ~0,5 us | ~1 us | — |
| ChaCha20 (52 bytes) | ~0,1 us | ~0,1 us | — |
| **Total por nota** | **~101 us** | **~141 us** | **+40% sobrecarga** |

Varredura de 100.000 notas: ~10,1 seg → ~14,1 seg. A sobrecarga é significativa
mas não proibitiva. A desencapsulação ML-KEM é de tempo constante sem vantagem
de agrupamento (ao contrário de operações de curvas elípticas), portanto escala
linearmente.

### Impacto nos Circuitos ZK

**Nenhum.** O KEM híbrido está inteiramente na camada de transporte/encriptação.
O circuito Halo 2 prova existência de notas, correção de anuladores e balanço
de valores — não prova nada sobre encriptação. Nenhuma alteração em chaves de
prova, chaves de verificação ou restrições de circuito.

### Comparação com a Indústria

| Sistema | Abordagem | Estado |
|---------|-----------|--------|
| **Signal** (PQXDH) | X25519 + ML-KEM-768, obrigatório para todos os utilizadores | **Implantado** (2023) |
| **Chrome/Firefox TLS** | X25519 + ML-KEM-768 troca híbrida de chaves | **Implantado** (2024) |
| **X-Wing** (rascunho IETF) | X25519 + ML-KEM-768, combinador dedicado | Rascunho de padrão |
| **Zcash** | Rascunho de ZIP de recuperabilidade quântica (recuperação de fundos, não encriptação) | Apenas discussão |
| **Dash Platform** | Pallas ECDH + ML-KEM-768 (proposto) | Fase de design |

## Quando Implantar

### A Questão da Linha Temporal

- **Estado atual (2026):** Nenhum computador quântico consegue quebrar ECC de
  255 bits. Maior fatoração quântica demonstrada: ~50 bits. Diferença: ordens
  de grandeza.
- **Curto prazo (2030-2035):** Roteiros de hardware da IBM, Google, Quantinuum
  visam milhões de qubits. Implementações e conjuntos de parâmetros ML-KEM terão
  amadurecido.
- **Médio prazo (2035-2050):** A maioria das estimativas coloca a chegada do
  CRQC nesta janela. Dados HNDL recolhidos hoje estão em risco.
- **Longo prazo (2050+):** Consenso entre criptógrafos: computadores quânticos
  de grande escala são uma questão de "quando," não "se."

### Estratégia Recomendada

**1. Projetar para atualizabilidade agora.** Garantir que o formato de registro
armazenado, a struct `TransmittedNoteCiphertext` e o layout de entradas da
BulkAppendTree sejam versionados e extensíveis. Isto tem baixo custo e preserva
a opção de adicionar KEM híbrido posteriormente.

**2. Implantar KEM híbrido quando pronto, torná-lo obrigatório.** Não oferecer
dois pools (clássico e híbrido). Dividir o conjunto de anonimato anula o
propósito das transações blindadas — utilizadores escondendo-se num grupo menor
são menos privados, não mais. Quando implantado, cada nota usa o esquema
híbrido.

**3. Visar a janela 2028-2030.** Isto é bem antes de qualquer ameaça quântica
realista, mas após implementações ML-KEM e tamanhos de parâmetros terem
estabilizado. Também permite aprender com a experiência de implantação do Zcash
e do Signal.

**4. Monitorar eventos gatilho:**
- NIST ou NSA mandatando prazos de migração pós-quântica
- Avanços significativos em hardware quântico (>100.000 qubits físicos com
  correção de erros)
- Avanços criptoanalíticos contra problemas de reticulados (afetaria a escolha
  de ML-KEM)

### O Que Não Necessita de Ação Urgente

| Componente | Porque pode esperar |
|------------|-------------------|
| Assinaturas de autorização de gasto | Falsificação é em tempo real, não retroativa. Atualizar para ML-DSA/SLH-DSA antes do CRQC chegar. |
| Sistema de provas Halo 2 | Falsificação de provas é em tempo real. Migrar para sistema baseado em STARK quando necessário. |
| Resistência a colisões Sinsemilla | Útil apenas para novos ataques, não retroativos. Subsumida pela migração do sistema de provas. |
| Infraestrutura GroveDB Merk/MMR/Blake3 | Já é segura quanticamente sob as suposições criptográficas atuais. Nenhuma ação necessária com base nos ataques conhecidos. |

## Referência de Alternativas Pós-Quânticas

### Para Encriptação (substituindo ECDH)

| Esquema | Tipo | Chave pública | Texto cifrado | Categoria NIST | Notas |
|---------|------|--------------|--------------|----------------|-------|
| ML-KEM-768 | Reticulado (MLWE) | 1.184 B | 1.088 B | 3 (192-bit) | FIPS 203, padrão da indústria |
| ML-KEM-512 | Reticulado (MLWE) | 800 B | 768 B | 1 (128-bit) | Menor, margem inferior |
| ML-KEM-1024 | Reticulado (MLWE) | 1.568 B | 1.568 B | 5 (256-bit) | Excessivo para híbrido |

### Para Assinaturas (substituindo RedPallas/Schnorr)

| Esquema | Tipo | Chave pública | Assinatura | Categoria NIST | Notas |
|---------|------|--------------|-----------|----------------|-------|
| ML-DSA-65 (Dilithium) | Reticulado | 1.952 B | 3.293 B | 3 | FIPS 204, rápido |
| SLH-DSA (SPHINCS+) | Baseado em hash | 32-64 B | 7.856-49.856 B | 1-5 | FIPS 205, conservador |
| XMSS/LMS | Baseado em hash (com estado) | 60 B | 2.500 B | varia | Com estado — reutilização = quebra |

### Para Provas ZK (substituindo Halo 2)

| Sistema | Suposição | Tamanho da prova | Pós-quântico | Notas |
|---------|-----------|-----------------|-------------|-------|
| STARKs | Funções hash (resistência a colisões) | ~100-400 KB | **Sim** | Usado pelo StarkNet |
| Plonky3 | FRI (compromisso polinomial baseado em hash) | ~50-200 KB | **Sim** | Desenvolvimento ativo |
| Halo 2 (atual) | ECDLP sobre curvas Pasta | ~5 KB | **Não** | Sistema Orchard atual |
| Lattice SNARKs | MLWE | Pesquisa | **Sim** | Não pronto para produção |

### Ecossistema de Crates Rust

| Crate | Fonte | FIPS 203 | Verificada | Notas |
|-------|-------|----------|-----------|-------|
| `libcrux-ml-kem` | Cryspen | Sim | Formalmente verificada (hax/F*) | Maior garantia |
| `ml-kem` | RustCrypto | Sim | Tempo constante, não auditada | Compatibilidade com ecossistema |
| `fips203` | integritychain | Sim | Tempo constante | Rust puro, no_std |

## Resumo

```text
┌─────────────────────────────────────────────────────────────────────┐
│  RESUMO DE AMEAÇAS QUÂNTICAS PARA GROVEDB + ORCHARD                │
│                                                                     │
│  SEGURO SOB SUPOSIÇÕES ATUAIS (baseado em hash):                   │
│    ✓ Blake3 árvores Merk, MMR, BulkAppendTree                      │
│    ✓ BLAKE2b KDF, PRF^expand                                       │
│    ✓ ChaCha20-Poly1305 encriptação simétrica                       │
│    ✓ Todas as cadeias de autenticação de provas GroveDB             │
│                                                                     │
│  CORRIGIR ANTES DE ARMAZENAR DADOS (HNDL retroativo):              │
│    ✗ Encriptação de notas (acordo de chaves ECDH) → KEM híbrido    │
│    ✗ Compromissos de valor (Pedersen) → montantes revelados         │
│                                                                     │
│  CORRIGIR ANTES DOS COMPUTADORES QUÂNTICOS CHEGAREM (tempo real):  │
│    ~ Autorização de gasto → ML-DSA / SLH-DSA                       │
│    ~ Provas ZK → STARKs / Plonky3                                  │
│    ~ Sinsemilla → árvore Merkle baseada em hash                     │
│                                                                     │
│  LINHA TEMPORAL RECOMENDADA:                                        │
│    2026-2028: Projetar para atualizabilidade, versionar formatos    │
│    2028-2030: Implantar KEM híbrido obrigatório para encriptação    │
│    2035+: Migrar assinaturas e sistema de provas se necessário      │
└─────────────────────────────────────────────────────────────────────┘
```

---
