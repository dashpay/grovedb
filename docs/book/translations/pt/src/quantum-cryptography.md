# Criptografia Quantica — Analise de Ameacas Pos-Quanticas

Este capitulo analisa como computadores quanticos afetariam as primitivas
criptograficas usadas no GroveDB e nos protocolos de transacoes blindadas
construidos sobre ele (Orchard, Dash Platform). Abrange quais componentes sao
vulneraveis, quais sao seguros, o que significa "colher agora, decriptar depois"
para dados armazenados e quais estrategias de mitigacao existem — incluindo
designs hibridos de KEM.

## Dois Algoritmos Quanticos Relevantes

Apenas dois algoritmos quanticos sao relevantes para a criptografia na pratica:

**O algoritmo de Shor** resolve o problema do logaritmo discreto (e a
fatoracao de inteiros) em tempo polinomial. Para uma curva eliptica de 255 bits
como Pallas, isso requer aproximadamente 510 qubits logicos — mas com a
sobrecarga de correcao de erros, o requisito real e de aproximadamente 4 milhoes
de qubits fisicos. O algoritmo de Shor **quebra completamente** toda a
criptografia de curvas elipticas, independentemente do tamanho da chave.

**O algoritmo de Grover** fornece uma aceleracao quadratica para busca por
forca bruta. Uma chave simetrica de 256 bits efetivamente se torna de 128 bits.
No entanto, a profundidade do circuito para Grover num espaco de chaves de
128 bits ainda e de 2^64 operacoes quanticas — muitos criptografos acreditam que
isso nunca sera pratico em hardware real devido aos limites de decoerencia.
Grover reduz as margens de seguranca, mas nao quebra criptografia simetrica
bem parametrizada.

| Algoritmo | Alvos | Aceleracao | Impacto pratico |
|-----------|-------|------------|-----------------|
| **Shor** | Logaritmo discreto ECC, fatoracao RSA | Exponencial (tempo polinomial) | **Quebra total** de ECC |
| **Grover** | Busca de chave simetrica, pre-imagem de hash | Quadratica (reduz bits da chave pela metade) | 256 bits → 128 bits (ainda seguro) |

## Primitivas Criptograficas do GroveDB

O GroveDB e o protocolo blindado baseado em Orchard usam uma combinacao de
primitivas de curvas elipticas e primitivas simetricas/baseadas em hash.
A tabela abaixo classifica cada primitiva pela sua vulnerabilidade quantica:

### Vulneravel ao Quantico (algoritmo de Shor — 0 bits pos-quanticos)

| Primitiva | Onde e usada | O que quebra |
|-----------|-------------|-------------|
| **Pallas ECDLP** | Compromissos de notas (cmx), chaves efemeras (epk/esk), chaves de visualizacao (ivk), chaves de pagamento (pk_d), derivacao de anuladores | Recuperar qualquer chave privada a partir da sua contraparte publica |
| **Acordo de chaves ECDH** (Pallas) | Derivacao de chaves simetricas de encriptacao para textos cifrados de notas | Recuperar segredo compartilhado → decriptar todas as notas |
| **Hash Sinsemilla** | Caminhos Merkle da CommitmentTree, hashing em circuito | Resistencia a colisoes depende do ECDLP; degrada quando Pallas e quebrada |
| **Halo 2 IPA** | Sistema de provas ZK (compromisso polinomial sobre curvas Pasta) | Forjar provas para declaracoes falsas (falsificacao, gastos nao autorizados) |
| **Compromissos de Pedersen** | Compromissos de valor (cv_net) ocultando montantes de transacoes | Recuperar montantes ocultos; forjar provas de balanco |

### Seguro contra Quantico (algoritmo de Grover — 128+ bits pos-quanticos)

| Primitiva | Onde e usada | Seguranca pos-quantica |
|-----------|-------------|----------------------|
| **Blake3** | Hashes de nos da arvore Merk, nos MMR, raizes de estado da BulkAppendTree, prefixos de caminho de subarvores | Pre-imagem de 128 bits, segunda pre-imagem de 128 bits |
| **BLAKE2b-256** | KDF para derivacao de chave simetrica, chave de cifra de saida, PRF^expand | Pre-imagem de 128 bits |
| **ChaCha20-Poly1305** | Encripta enc_ciphertext e out_ciphertext (chaves de 256 bits) | Busca de chave de 128 bits (seguro, mas o caminho de derivacao de chave via ECDH nao e) |
| **PRF^expand** (BLAKE2b-512) | Deriva esk, rcm, psi a partir de rseed | Seguranca PRF de 128 bits |

### Infraestrutura do GroveDB: Totalmente Segura contra Quantico

Todas as estruturas de dados proprias do GroveDB dependem exclusivamente de hashing Blake3:

- **Arvores AVL Merk** — hashes de nos, combined_value_hash, propagacao de hash filho
- **Arvores MMR** — hashes de nos internos, computacao de picos, derivacao de raiz
- **BulkAppendTree** — cadeias de hash de buffer, raizes Merkle densas, MMR de epocas
- **Raiz de estado da CommitmentTree** — `blake3("ct_state" || sinsemilla_root || bulk_state_root)`
- **Prefixos de caminho de subarvores** — hashing Blake3 de segmentos de caminho
- **Provas V1** — cadeias de autenticacao atraves da hierarquia Merk

**Nenhuma alteracao necessaria.** As provas de arvore Merk do GroveDB,
verificacoes de consistencia MMR, raizes de epoca da BulkAppendTree e todas
as cadeias de autenticacao de provas V1 permanecem seguras contra computadores
quanticos. A infraestrutura baseada em hash e a parte mais forte do sistema
pos-quantico.

## Ameacas Retroativas vs em Tempo Real

Esta distincao e critica para priorizar o que corrigir e quando.

**Ameacas retroativas** comprometem dados que ja estao armazenados. Um
adversario grava dados hoje e os decripta quando computadores quanticos
estiverem disponiveis. Essas ameacas **nao podem ser mitigadas apos o facto** —
uma vez que os dados estao na blockchain, nao podem ser re-encriptados ou
recuperados.

**Ameacas em tempo real** afetam apenas transacoes criadas no futuro. Um
adversario com um computador quantico poderia forjar assinaturas ou provas,
mas apenas para novas transacoes. Transacoes antigas ja foram validadas e
confirmadas pela rede.

| Ameaca | Tipo | O que e exposto | Urgencia |
|--------|------|----------------|----------|
| **Decriptacao de notas** (enc_ciphertext) | **Retroativa** | Conteudo das notas: destinatario, montante, memo, rseed | **Alta** — armazenado para sempre |
| **Abertura de compromisso de valor** (cv_net) | **Retroativa** | Montantes de transacoes (mas nao remetente/destinatario) | **Media** — apenas montantes |
| **Dados de recuperacao do remetente** (out_ciphertext) | **Retroativa** | Chaves de recuperacao do remetente para notas enviadas | **Alta** — armazenado para sempre |
| Falsificacao de autorizacao de gasto | Tempo real | Poderia forjar novas assinaturas de gasto | Baixa — atualizar antes do QC chegar |
| Falsificacao de prova Halo 2 | Tempo real | Poderia forjar novas provas (falsificacao) | Baixa — atualizar antes do QC chegar |
| Colisao Sinsemilla | Tempo real | Poderia forjar novos caminhos Merkle | Baixa — subsumida pela falsificacao de provas |
| Falsificacao de assinatura de vinculacao | Tempo real | Poderia forjar novas provas de balanco | Baixa — atualizar antes do QC chegar |

### O Que Exatamente e Revelado?

**Se a encriptacao de notas for quebrada** (a principal ameaca HNDL):

Um adversario quantico recupera `esk` a partir do `epk` armazenado via
algoritmo de Shor, calcula o segredo compartilhado ECDH, deriva a chave
simetrica e decripta `enc_ciphertext`. Isso revela o texto plano completo
da nota:

| Campo | Tamanho | O que revela |
|-------|---------|-------------|
| version | 1 byte | Versao do protocolo (nao sensivel) |
| diversifier | 11 bytes | Componente do endereco do destinatario |
| value | 8 bytes | Montante exato da transacao |
| rseed | 32 bytes | Permite ligacao de anuladores (desanonimiza o grafo de transacoes) |
| memo | 36 bytes (DashMemo) | Dados da aplicacao, potencialmente identificadores |

Com `rseed` e `rho` (armazenados junto ao texto cifrado), o adversario pode
calcular `esk = PRF(rseed, rho)` e verificar a vinculacao da chave efemera.
Combinado com o diversificador, isso liga entradas a saidas em todo o historico
de transacoes — **desanonimizacao completa do pool blindado**.

**Se apenas os compromissos de valor forem quebrados** (ameaca HNDL secundaria):

O adversario recupera `v` de `cv_net = [v]*V + [rcv]*R` resolvendo o ECDLP.
Isso revela **montantes de transacoes, mas nao identidades de remetente ou
destinatario**. O adversario ve "alguem enviou 5.0 Dash para alguem" mas
nao consegue vincular o montante a nenhum endereco ou identidade sem tambem
quebrar a encriptacao de notas.

Por si so, montantes sem vinculacao tem utilidade limitada. Mas combinados
com dados externos (timing, faturas conhecidas, montantes correspondentes a
pedidos publicos), ataques de correlacao tornam-se possiveis.

## O Ataque "Colher Agora, Decriptar Depois"

Esta e a ameaca quantica mais urgente e pratica.

**Modelo de ataque:** Um adversario a nivel estatal (ou qualquer parte com
armazenamento suficiente) grava todos os dados de transacoes blindadas na
blockchain hoje. Estes dados sao publicamente disponiveis na blockchain e
imutaveis. O adversario espera por um computador quantico criptograficamente
relevante (CRQC), e entao:

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

**Ponto chave:** A encriptacao simetrica (ChaCha20-Poly1305) e perfeitamente
segura contra quantico. A vulnerabilidade esta inteiramente no **caminho de
derivacao de chave** — a chave simetrica e derivada de um segredo compartilhado
ECDH, e o ECDH e quebrado pelo algoritmo de Shor. O atacante nao quebra a
encriptacao; ele recupera a chave.

**Retroatividade:** Este ataque e **totalmente retroativo**. Cada nota
encriptada ja armazenada na blockchain pode ser decriptada assim que um CRQC
existir. Os dados nao podem ser re-encriptados ou protegidos apos o facto.
E por isso que deve ser abordado antes de os dados serem armazenados, nao depois.

## Mitigacao: KEM Hibrido (ML-KEM + ECDH)

A defesa contra HNDL e derivar a chave de encriptacao simetrica a partir de
**dois mecanismos de acordo de chaves independentes**, de modo que quebrar
apenas um seja insuficiente. Isto chama-se KEM hibrido.

### ML-KEM-768 (CRYSTALS-Kyber)

ML-KEM e o mecanismo de encapsulamento de chaves pos-quantico padronizado pelo
NIST (FIPS 203, agosto de 2024), baseado no problema Module Learning With
Errors (MLWE).

| Parametro | ML-KEM-512 | ML-KEM-768 | ML-KEM-1024 |
|-----------|-----------|-----------|------------|
| Chave publica (ek) | 800 bytes | **1.184 bytes** | 1.568 bytes |
| Texto cifrado (ct) | 768 bytes | **1.088 bytes** | 1.568 bytes |
| Segredo compartilhado | 32 bytes | 32 bytes | 32 bytes |
| Categoria NIST | 1 (128-bit) | **3 (192-bit)** | 5 (256-bit) |

**ML-KEM-768** e a escolha recomendada — e o conjunto de parametros usado pelo
X-Wing, PQXDH do Signal e troca hibrida de chaves TLS do Chrome/Firefox.
A Categoria 3 fornece uma margem confortavel contra avancos futuros na
criptoanalise de reticulados.

### Como o Esquema Hibrido Funciona

**Fluxo atual (vulneravel):**

```text
Sender:
  esk = PRF(rseed, rho)                    // deterministic from note
  epk = [esk] * g_d                         // Pallas curve point
  shared_secret = [esk] * pk_d              // ECDH (broken by Shor's)
  K_enc = BLAKE2b("Zcash_OrchardKDF", shared_secret || epk)
  enc_ciphertext = ChaCha20(K_enc, note_plaintext)
```

**Fluxo hibrido (resistente a quantico):**

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

**Decriptacao pelo destinatario:**

```text
Recipient:
  ss_ecdh = [ivk] * epk                    // same ECDH (using incoming viewing key)
  ss_pq = ML-KEM-768.Decaps(dk_pq, ct_pq)  // NEW: KEM decapsulation
  K_enc = BLAKE2b("DashPlatform_HybridKDF", ss_ecdh || ss_pq || ct_pq || epk)
  note_plaintext = ChaCha20.Decrypt(K_enc, enc_ciphertext)
```

### Garantia de Seguranca

O KEM combinado e IND-CCA2 seguro se **qualquer um** dos KEMs componentes for
seguro. Isto e formalmente provado por [Giacon, Heuer e Poettering (2018)](https://eprint.iacr.org/2018/024)
para combinadores de KEM usando um PRF (BLAKE2b qualifica-se), e independentemente
pela [prova de seguranca do X-Wing](https://eprint.iacr.org/2024/039).

| Cenario | ECDH | ML-KEM | Chave combinada | Estado |
|---------|------|--------|----------------|--------|
| Mundo classico | Seguro | Seguro | **Seguro** | Ambos intactos |
| Quantico quebra ECC | **Quebrado** | Seguro | **Seguro** | ML-KEM protege |
| Avancos em reticulados quebram ML-KEM | Seguro | **Quebrado** | **Seguro** | ECDH protege (igual a hoje) |
| Ambos quebrados | Quebrado | Quebrado | **Quebrado** | Requer dois avancos simultaneos |

### Impacto no Tamanho

O KEM hibrido adiciona o texto cifrado ML-KEM-768 (1.088 bytes) a cada nota
armazenada e expande o texto cifrado de saida para incluir o segredo
compartilhado ML-KEM para recuperacao do remetente:

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

| Notas | Atual (280 B) | Hibrido (1.400 B) | Delta |
|-------|--------------|-------------------|-------|
| 100.000 | 26,7 MB | 133 MB | +106 MB |
| 1.000.000 | 267 MB | 1,33 GB | +1,07 GB |
| 10.000.000 | 2,67 GB | 13,3 GB | +10,7 GB |

**Tamanho do endereco:**

```text
Current:  diversifier (11) + pk_d (32) = 43 bytes
Hybrid:   diversifier (11) + pk_d (32) + ek_pq (1,184) = 1,227 bytes
```

A chave publica ML-KEM de 1.184 bytes deve ser incluida no endereco para que
remetentes possam realizar o encapsulamento. Com ~1.960 caracteres Bech32m,
e grande mas ainda cabe num codigo QR (maximo ~2.953 caracteres alfanumericos).

### Gestao de Chaves

O par de chaves ML-KEM e derivado deterministicamente a partir da chave de gasto:

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

**Nenhuma alteracao de backup necessaria.** A frase semente existente de 24
palavras cobre a chave ML-KEM porque e derivada deterministicamente da chave
de gasto. A recuperacao de carteira funciona como antes.

**Enderecos diversificados** compartilham todos o mesmo `ek_pq` porque ML-KEM
nao possui um mecanismo natural de diversificacao como a multiplicacao escalar
de Pallas. Isto significa que um observador com dois enderecos de um utilizador
pode liga-los comparando `ek_pq`.

### Desempenho de Decriptacao por Tentativa

| Etapa | Atual | Hibrido | Delta |
|-------|-------|---------|-------|
| Pallas ECDH | ~100 us | ~100 us | — |
| ML-KEM-768 Decaps | — | ~40 us | +40 us |
| BLAKE2b KDF | ~0,5 us | ~1 us | — |
| ChaCha20 (52 bytes) | ~0,1 us | ~0,1 us | — |
| **Total por nota** | **~101 us** | **~141 us** | **+40% sobrecarga** |

Varredura de 100.000 notas: ~10,1 seg → ~14,1 seg. A sobrecarga e significativa
mas nao proibitiva. A desencapsulacao ML-KEM e de tempo constante sem vantagem
de agrupamento (ao contrario de operacoes de curvas elipticas), portanto escala
linearmente.

### Impacto nos Circuitos ZK

**Nenhum.** O KEM hibrido esta inteiramente na camada de transporte/encriptacao.
O circuito Halo 2 prova existencia de notas, correcao de anuladores e balanco
de valores — nao prova nada sobre encriptacao. Nenhuma alteracao em chaves de
prova, chaves de verificacao ou restricoes de circuito.

### Comparacao com a Industria

| Sistema | Abordagem | Estado |
|---------|-----------|--------|
| **Signal** (PQXDH) | X25519 + ML-KEM-768, obrigatorio para todos os utilizadores | **Implantado** (2023) |
| **Chrome/Firefox TLS** | X25519 + ML-KEM-768 troca hibrida de chaves | **Implantado** (2024) |
| **X-Wing** (rascunho IETF) | X25519 + ML-KEM-768, combinador dedicado | Rascunho de padrao |
| **Zcash** | Rascunho de ZIP de recuperabilidade quantica (recuperacao de fundos, nao encriptacao) | Apenas discussao |
| **Dash Platform** | Pallas ECDH + ML-KEM-768 (proposto) | Fase de design |

## Quando Implantar

### A Questao da Linha Temporal

- **Estado atual (2026):** Nenhum computador quantico consegue quebrar ECC de
  255 bits. Maior fatoracao quantica demonstrada: ~50 bits. Diferenca: ordens
  de grandeza.
- **Curto prazo (2030-2035):** Roteiros de hardware da IBM, Google, Quantinuum
  visam milhoes de qubits. Implementacoes e conjuntos de parametros ML-KEM terao
  amadurecido.
- **Medio prazo (2035-2050):** A maioria das estimativas coloca a chegada do
  CRQC nesta janela. Dados HNDL recolhidos hoje estao em risco.
- **Longo prazo (2050+):** Consenso entre criptografos: computadores quanticos
  de grande escala sao uma questao de "quando," nao "se."

### Estrategia Recomendada

**1. Projetar para atualizabilidade agora.** Garantir que o formato de registro
armazenado, a struct `TransmittedNoteCiphertext` e o layout de entradas da
BulkAppendTree sejam versionados e extensiveis. Isto tem baixo custo e preserva
a opcao de adicionar KEM hibrido posteriormente.

**2. Implantar KEM hibrido quando pronto, torna-lo obrigatorio.** Nao oferecer
dois pools (classico e hibrido). Dividir o conjunto de anonimato anula o
proposito das transacoes blindadas — utilizadores escondendo-se num grupo menor
sao menos privados, nao mais. Quando implantado, cada nota usa o esquema
hibrido.

**3. Visar a janela 2028-2030.** Isto e bem antes de qualquer ameaca quantica
realista, mas apos implementacoes ML-KEM e tamanhos de parametros terem
estabilizado. Tambem permite aprender com a experiencia de implantacao do Zcash
e do Signal.

**4. Monitorar eventos gatilho:**
- NIST ou NSA mandatando prazos de migracao pos-quantica
- Avancos significativos em hardware quantico (>100.000 qubits fisicos com
  correcao de erros)
- Avancos criptoanaliticos contra problemas de reticulados (afetaria a escolha
  de ML-KEM)

### O Que Nao Necessita de Acao Urgente

| Componente | Porque pode esperar |
|------------|-------------------|
| Assinaturas de autorizacao de gasto | Falsificacao e em tempo real, nao retroativa. Atualizar para ML-DSA/SLH-DSA antes do CRQC chegar. |
| Sistema de provas Halo 2 | Falsificacao de provas e em tempo real. Migrar para sistema baseado em STARK quando necessario. |
| Resistencia a colisoes Sinsemilla | Util apenas para novos ataques, nao retroativos. Subsumida pela migracao do sistema de provas. |
| Infraestrutura GroveDB Merk/MMR/Blake3 | **Ja segura contra quantico.** Nenhuma acao necessaria, agora ou nunca. |

## Referencia de Alternativas Pos-Quanticas

### Para Encriptacao (substituindo ECDH)

| Esquema | Tipo | Chave publica | Texto cifrado | Categoria NIST | Notas |
|---------|------|--------------|--------------|----------------|-------|
| ML-KEM-768 | Reticulado (MLWE) | 1.184 B | 1.088 B | 3 (192-bit) | FIPS 203, padrao da industria |
| ML-KEM-512 | Reticulado (MLWE) | 800 B | 768 B | 1 (128-bit) | Menor, margem inferior |
| ML-KEM-1024 | Reticulado (MLWE) | 1.568 B | 1.568 B | 5 (256-bit) | Excessivo para hibrido |

### Para Assinaturas (substituindo RedPallas/Schnorr)

| Esquema | Tipo | Chave publica | Assinatura | Categoria NIST | Notas |
|---------|------|--------------|-----------|----------------|-------|
| ML-DSA-65 (Dilithium) | Reticulado | 1.952 B | 3.293 B | 3 | FIPS 204, rapido |
| SLH-DSA (SPHINCS+) | Baseado em hash | 32-64 B | 7.856-49.856 B | 1-5 | FIPS 205, conservador |
| XMSS/LMS | Baseado em hash (com estado) | 60 B | 2.500 B | varia | Com estado — reutilizacao = quebra |

### Para Provas ZK (substituindo Halo 2)

| Sistema | Suposicao | Tamanho da prova | Pos-quantico | Notas |
|---------|-----------|-----------------|-------------|-------|
| STARKs | Funcoes hash (resistencia a colisoes) | ~100-400 KB | **Sim** | Usado pelo StarkNet |
| Plonky3 | FRI (compromisso polinomial baseado em hash) | ~50-200 KB | **Sim** | Desenvolvimento ativo |
| Halo 2 (atual) | ECDLP sobre curvas Pasta | ~5 KB | **Nao** | Sistema Orchard atual |
| Lattice SNARKs | MLWE | Pesquisa | **Sim** | Nao pronto para producao |

### Ecossistema de Crates Rust

| Crate | Fonte | FIPS 203 | Verificada | Notas |
|-------|-------|----------|-----------|-------|
| `libcrux-ml-kem` | Cryspen | Sim | Formalmente verificada (hax/F*) | Maior garantia |
| `ml-kem` | RustCrypto | Sim | Tempo constante, nao auditada | Compatibilidade com ecossistema |
| `fips203` | integritychain | Sim | Tempo constante | Rust puro, no_std |

## Resumo

```text
┌─────────────────────────────────────────────────────────────────────┐
│  RESUMO DE AMEACAS QUANTICAS PARA GROVEDB + ORCHARD                │
│                                                                     │
│  SEGURO AGORA E PARA SEMPRE (baseado em hash):                      │
│    ✓ Blake3 arvores Merk, MMR, BulkAppendTree                      │
│    ✓ BLAKE2b KDF, PRF^expand                                       │
│    ✓ ChaCha20-Poly1305 encriptacao simetrica                       │
│    ✓ Todas as cadeias de autenticacao de provas GroveDB             │
│                                                                     │
│  CORRIGIR ANTES DE ARMAZENAR DADOS (HNDL retroativo):              │
│    ✗ Encriptacao de notas (acordo de chaves ECDH) → KEM hibrido    │
│    ✗ Compromissos de valor (Pedersen) → montantes revelados         │
│                                                                     │
│  CORRIGIR ANTES DOS COMPUTADORES QUANTICOS CHEGAREM (tempo real):   │
│    ~ Autorizacao de gasto → ML-DSA / SLH-DSA                       │
│    ~ Provas ZK → STARKs / Plonky3                                  │
│    ~ Sinsemilla → arvore Merkle baseada em hash                     │
│                                                                     │
│  LINHA TEMPORAL RECOMENDADA:                                        │
│    2026-2028: Projetar para atualizabilidade, versionar formatos    │
│    2028-2030: Implantar KEM hibrido obrigatorio para encriptacao    │
│    2035+: Migrar assinaturas e sistema de provas se necessario      │
└─────────────────────────────────────────────────────────────────────┘
```

---
