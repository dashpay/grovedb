# Criptografía Cuántica — Análisis de Amenazas Post-Cuánticas

Este capítulo analiza cómo las computadoras cuánticas afectarían las primitivas
criptográficas utilizadas en GroveDB y los protocolos de transacciones blindadas
construidos sobre él (Orchard, Dash Platform). Cubre qué componentes son vulnerables,
cuáles son seguros, qué significa "cosechar ahora, descifrar después" para los datos
almacenados, y qué estrategias de mitigación existen, incluidos los diseños de KEM
híbrido.

## Dos Algoritmos Cuánticos Relevantes

Solo dos algoritmos cuánticos son relevantes para la criptografía en la práctica:

**El algoritmo de Shor** resuelve el problema del logaritmo discreto (y la
factorización de enteros) en tiempo polinomial. Para una curva elíptica de 255 bits
como Pallas, esto requiere aproximadamente 510 qubits lógicos, pero con la
sobrecarga de corrección de errores, el requisito real es aproximadamente 4 millones
de qubits físicos. El algoritmo de Shor **rompe completamente** toda la criptografía
de curvas elípticas independientemente del tamaño de la clave.

**El algoritmo de Grover** proporciona una aceleración cuadrática para la búsqueda
por fuerza bruta. Una clave simétrica de 256 bits efectivamente se convierte en
128 bits. Sin embargo, la profundidad del circuito para el algoritmo de Grover en
un espacio de claves de 128 bits sigue siendo 2^64 operaciones cuánticas — muchos
criptógrafos creen que esto nunca será práctico en hardware real debido a los límites
de decoherencia. El algoritmo de Grover reduce los márgenes de seguridad pero no
rompe la criptografía simétrica bien parametrizada.

| Algoritmo | Objetivos | Aceleración | Impacto práctico |
|-----------|-----------|-------------|------------------|
| **Shor** | Logaritmo discreto ECC, factorización RSA | Tiempo polinomial (aceleración exponencial sobre clásico) | **Ruptura total** de ECC |
| **Grover** | Búsqueda de claves simétricas, preimagen de hash | Cuadrática (reduce bits de clave a la mitad) | 256-bit → 128-bit (aún seguro) |

## Primitivas Criptográficas de GroveDB

GroveDB y el protocolo blindado basado en Orchard utilizan una combinación de
primitivas de curvas elípticas y primitivas simétricas/basadas en hash. La tabla
siguiente clasifica cada primitiva según su vulnerabilidad cuántica:

### Vulnerable a Computación Cuántica (algoritmo de Shor — 0 bits post-cuánticos)

| Primitiva | Dónde se usa | Qué se rompe |
|-----------|-------------|-------------|
| **Pallas ECDLP** | Compromisos de notas (cmx), claves efímeras (epk/esk), claves de visualización (ivk), claves de pago (pk_d), derivación de anuladores | Recuperar cualquier clave privada de su contraparte pública |
| **Acuerdo de claves ECDH** (Pallas) | Derivación de claves de cifrado simétricas para textos cifrados de notas | Recuperar secreto compartido → descifrar todas las notas |
| **Hash Sinsemilla** | Rutas Merkle del CommitmentTree, hashing dentro del circuito | La resistencia a colisiones depende de ECDLP; se degrada cuando Pallas se rompe |
| **Halo 2 IPA** | Sistema de pruebas ZK (compromiso polinomial sobre curvas Pasta) | Falsificar pruebas para declaraciones falsas (falsificación, gastos no autorizados) |
| **Compromisos de Pedersen** | Compromisos de valor (cv_net) que ocultan montos de transacciones | Recuperar montos ocultos; falsificar pruebas de balance |

### Seguro ante Computación Cuántica (algoritmo de Grover — 128+ bits post-cuánticos)

| Primitiva | Dónde se usa | Seguridad post-cuántica |
|-----------|-------------|------------------------|
| **Blake3** | Hashes de nodos de árboles Merk, nodos MMR, raíces de estado de BulkAppendTree, prefijos de rutas de subárboles | 128-bit preimagen, 128-bit segunda preimagen |
| **BLAKE2b-256** | KDF para derivación de claves simétricas, clave de cifrado saliente, PRF^expand | 128-bit preimagen |
| **ChaCha20-Poly1305** | Cifra enc_ciphertext y out_ciphertext (claves de 256 bits) | 128-bit búsqueda de clave (seguro, pero la ruta de derivación de clave a través de ECDH no lo es) |
| **PRF^expand** (BLAKE2b-512) | Deriva esk, rcm, psi de rseed | 128-bit seguridad PRF |

### Infraestructura de GroveDB: Completamente Segura ante Computación Cuántica

Todas las estructuras de datos propias de GroveDB dependen exclusivamente de hashing Blake3:

- **Árboles AVL Merk** — hashes de nodos, combined_value_hash, propagación de hash hijo
- **Árboles MMR** — hashes de nodos internos, cálculo de picos, derivación de raíz
- **BulkAppendTree** — cadenas de hash de buffer, raíces Merkle densas, MMR de épocas
- **Raíz de estado del CommitmentTree** — `blake3("ct_state" || sinsemilla_root || bulk_state_root)`
- **Prefijos de rutas de subárboles** — hashing Blake3 de segmentos de ruta
- **Pruebas V1** — cadenas de autenticación a través de la jerarquía Merk

**No se necesitan cambios.** Las pruebas de árboles Merk de GroveDB, las verificaciones
de consistencia de MMR, las raíces de épocas de BulkAppendTree y todas las cadenas de
autenticación de pruebas V1 permanecen seguras contra computadoras cuánticas. La
infraestructura basada en hash es la parte más sólida del sistema post-cuántico.

## Amenazas Retroactivas vs. en Tiempo Real

Esta distinción es crítica para priorizar qué arreglar y cuándo.

**Las amenazas retroactivas** comprometen datos que ya están almacenados. Un adversario
registra datos hoy y los descifra cuando las computadoras cuánticas estén disponibles.
Estas amenazas **no pueden mitigarse después del hecho** — una vez que los datos están
en la cadena de bloques, no pueden re-cifrarse ni retirarse.

**Las amenazas en tiempo real** solo afectan transacciones creadas en el futuro. Un
adversario con una computadora cuántica podría falsificar firmas o pruebas, pero solo
para nuevas transacciones. Las transacciones antiguas ya fueron validadas y confirmadas
por la red.

| Amenaza | Tipo | Qué se expone | Urgencia |
|---------|------|--------------|----------|
| **Descifrado de notas** (enc_ciphertext) | **Retroactiva** | Contenido de notas: destinatario, monto, memo, rseed | **Alta** — almacenado permanentemente |
| **Apertura de compromiso de valor** (cv_net) | **Retroactiva** | Montos de transacciones (pero no remitente/destinatario) | **Media** — solo montos |
| **Datos de recuperación del remitente** (out_ciphertext) | **Retroactiva** | Claves de recuperación del remitente para notas enviadas | **Alta** — almacenado permanentemente |
| Falsificación de autorización de gasto | En tiempo real | Podría falsificar nuevas firmas de gasto | Baja — actualizar antes de que llegue la CC |
| Falsificación de pruebas Halo 2 | En tiempo real | Podría falsificar nuevas pruebas (falsificación) | Baja — actualizar antes de que llegue la CC |
| Colisión de Sinsemilla | En tiempo real | Podría falsificar nuevas rutas Merkle | Baja — subsumida por la falsificación de pruebas |
| Falsificación de firma de vinculación | En tiempo real | Podría falsificar nuevas pruebas de balance | Baja — actualizar antes de que llegue la CC |

### Qué se Revela Exactamente?

**Si se rompe el cifrado de notas** (la amenaza HNDL principal):

Un adversario cuántico recupera `esk` del `epk` almacenado mediante el algoritmo de
Shor, calcula el secreto compartido ECDH, deriva la clave simétrica y descifra
`enc_ciphertext`. Esto revela el texto plano completo de la nota:

| Campo | Tamaño | Qué revela |
|-------|--------|-----------|
| version | 1 byte | Versión del protocolo (no sensible) |
| diversifier | 11 bytes | Componente de la dirección del destinatario |
| value | 8 bytes | Monto exacto de la transacción |
| rseed | 32 bytes | Permite vincular anuladores (desanonimiza el grafo de transacciones) |
| memo | 36 bytes (DashMemo) | Datos de la aplicación, potencialmente identificadores |

Con `rseed` y `rho` (almacenados junto al texto cifrado), el adversario puede
calcular `esk = PRF(rseed, rho)` y verificar la vinculación de la clave efímera.
Combinado con el diversifier, esto vincula entradas con salidas a través de todo
el historial de transacciones — **desanonimización completa del pool blindado**.

**Si solo se rompen los compromisos de valor** (amenaza HNDL secundaria):

El adversario recupera `v` de `cv_net = [v]*V + [rcv]*R` resolviendo ECDLP.
Esto revela **montos de transacciones pero no las identidades del remitente ni del
destinatario**. El adversario ve "alguien envió 5.0 Dash a alguien" pero no puede
vincular el monto a ninguna dirección o identidad sin romper también el cifrado
de notas.

Por sí solos, los montos sin vinculación tienen utilidad limitada. Pero combinados
con datos externos (temporalidad, facturas conocidas, montos que coinciden con
solicitudes públicas), los ataques de correlación se vuelven posibles.

## El Ataque "Cosechar Ahora, Descifrar Después"

Esta es la amenaza cuántica más urgente y práctica.

**Modelo de ataque:** Un adversario a nivel estatal (o cualquier parte con
almacenamiento suficiente) registra todos los datos de transacciones blindadas
en cadena hoy. Estos datos están disponibles públicamente en la blockchain y son
inmutables. El adversario espera una computadora cuántica criptográficamente
relevante (CRQC), y entonces:

```text
Paso 1: Leer registro almacenado del BulkAppendTree del CommitmentTree:
        cmx (32) || rho (32) || epk (32) || enc_ciphertext (104) || out_ciphertext (80)

Paso 2: Resolver ECDLP en Pallas vía algoritmo de Shor:
        epk = [esk] * g_d  →  recuperar esk

Paso 3: Calcular secreto compartido:
        shared_secret = [esk] * pk_d

Paso 4: Derivar clave simétrica (BLAKE2b es seguro cuánticamente, pero la entrada está comprometida):
        K_enc = BLAKE2b-256("Zcash_OrchardKDF", shared_secret || epk)

Paso 5: Descifrar enc_ciphertext usando ChaCha20-Poly1305:
        → version || diversifier || value || rseed || memo

Paso 6: Con rseed + rho, vincular anuladores a compromisos de notas:
        esk = PRF(rseed, rho)
        → reconstrucción completa del grafo de transacciones
```

**Hallazgo clave:** El cifrado simétrico (ChaCha20-Poly1305) es perfectamente
seguro ante computación cuántica. La vulnerabilidad está enteramente en la **ruta
de derivación de la clave** — la clave simétrica se deriva de un secreto compartido
ECDH, y ECDH es roto por el algoritmo de Shor. El atacante no rompe el cifrado;
recupera la clave.

**Retroactividad:** Este ataque es **completamente retroactivo**. Cada nota cifrada
almacenada en la cadena de bloques puede ser descifrada una vez que exista una CRQC.
Los datos no pueden re-cifrarse ni protegerse después del hecho. Por eso debe
abordarse antes de que los datos sean almacenados, no después.

## Mitigación: KEM Híbrido (ML-KEM + ECDH)

La defensa contra HNDL es derivar la clave de cifrado simétrica de
**dos mecanismos independientes de acuerdo de claves**, de manera que romper solo
uno sea insuficiente. Esto se llama KEM híbrido.

### ML-KEM-768 (CRYSTALS-Kyber)

ML-KEM es el mecanismo de encapsulación de claves post-cuántico estandarizado por
NIST (FIPS 203, agosto 2024) basado en el problema Module Learning With Errors (MLWE).

| Parámetro | ML-KEM-512 | ML-KEM-768 | ML-KEM-1024 |
|-----------|-----------|-----------|------------|
| Clave pública (ek) | 800 bytes | **1,184 bytes** | 1,568 bytes |
| Texto cifrado (ct) | 768 bytes | **1,088 bytes** | 1,568 bytes |
| Secreto compartido | 32 bytes | 32 bytes | 32 bytes |
| Categoría NIST | 1 (128-bit) | **3 (192-bit)** | 5 (256-bit) |

**ML-KEM-768** es la opción recomendada — es el conjunto de parámetros utilizado por
X-Wing, PQXDH de Signal y el intercambio de claves híbrido TLS de Chrome/Firefox.
La Categoría 3 proporciona un margen cómodo contra futuros avances en criptoanálisis
de redes.

### Cómo Funciona el Esquema Híbrido

**Flujo actual (vulnerable):**

```text
Remitente:
  esk = PRF(rseed, rho)                    // determinístico desde la nota
  epk = [esk] * g_d                         // punto de curva Pallas
  shared_secret = [esk] * pk_d              // ECDH (roto por Shor)
  K_enc = BLAKE2b("Zcash_OrchardKDF", shared_secret || epk)
  enc_ciphertext = ChaCha20(K_enc, note_plaintext)
```

**Flujo híbrido (resistente a computación cuántica):**

```text
Remitente:
  esk = PRF(rseed, rho)                    // sin cambios
  epk = [esk] * g_d                         // sin cambios
  ss_ecdh = [esk] * pk_d                    // mismo ECDH

  (ct_pq, ss_pq) = ML-KEM-768.Encaps(ek_pq)  // NUEVO: KEM basado en redes
                                                // ek_pq de la dirección del destinatario

  K_enc = BLAKE2b(                          // MODIFICADO: combina ambos secretos
      "DashPlatform_HybridKDF",
      ss_ecdh || ss_pq || ct_pq || epk
  )

  enc_ciphertext = ChaCha20(K_enc, note_plaintext)  // sin cambios
```

**Descifrado del destinatario:**

```text
Destinatario:
  ss_ecdh = [ivk] * epk                    // mismo ECDH (usando clave de visualización entrante)
  ss_pq = ML-KEM-768.Decaps(dk_pq, ct_pq)  // NUEVO: desencapsulación KEM
  K_enc = BLAKE2b("DashPlatform_HybridKDF", ss_ecdh || ss_pq || ct_pq || epk)
  note_plaintext = ChaCha20.Decrypt(K_enc, enc_ciphertext)
```

### Garantía de Seguridad

El KEM combinado es seguro IND-CCA2 si **cualquiera** de los KEM componentes es
seguro. Esto está formalmente demostrado por [Giacon, Heuer y Poettering (2018)](https://eprint.iacr.org/2018/024)
para combinadores de KEM que usan un PRF (BLAKE2b califica), e independientemente por
la [prueba de seguridad de X-Wing](https://eprint.iacr.org/2024/039).

| Escenario | ECDH | ML-KEM | Clave combinada | Estado |
|-----------|------|--------|----------------|--------|
| Mundo clásico | Seguro | Seguro | **Seguro** | Ambos intactos |
| Cuántica rompe ECC | **Roto** | Seguro | **Seguro** | ML-KEM protege |
| Avances en redes rompen ML-KEM | Seguro | **Roto** | **Seguro** | ECDH protege (igual que hoy) |
| Ambos rotos | Roto | Roto | **Roto** | Requiere dos avances simultáneos |

### Impacto en Tamaño

El KEM híbrido agrega el texto cifrado de ML-KEM-768 (1,088 bytes) a cada nota
almacenada y expande el texto cifrado saliente para incluir el secreto compartido
de ML-KEM para la recuperación del remitente:

**Registro almacenado por nota:**

```text
┌──────────────────────────────────────────────────────────────────┐
│  Actual (280 bytes)            Híbrido (1,400 bytes)             │
│                                                                  │
│  cmx:             32         cmx:              32               │
│  rho:             32         rho:              32               │
│  epk:             32         epk:              32               │
│  enc_ciphertext: 104         ct_pq:         1,088  ← NUEVO     │
│  out_ciphertext:  80         enc_ciphertext:  104               │
│                              out_ciphertext:  112  ← +32        │
│  ─────────────────           ──────────────────────             │
│  Total:          280         Total:          1,400  (5.0x)      │
└──────────────────────────────────────────────────────────────────┘
```

**Almacenamiento a escala:**

| Notas | Actual (280 B) | Híbrido (1,400 B) | Delta |
|-------|----------------|-------------------|-------|
| 100,000 | 26.7 MB | 133 MB | +106 MB |
| 1,000,000 | 267 MB | 1.33 GB | +1.07 GB |
| 10,000,000 | 2.67 GB | 13.3 GB | +10.7 GB |

**Tamaño de dirección:**

```text
Actual:   diversifier (11) + pk_d (32) = 43 bytes
Híbrido:  diversifier (11) + pk_d (32) + ek_pq (1,184) = 1,227 bytes
```

La clave pública de ML-KEM de 1,184 bytes debe incluirse en la dirección para que
los remitentes puedan realizar la encapsulación. Con aproximadamente 1,960 caracteres
Bech32m, es grande pero aún cabe en un código QR (máximo ~2,953 caracteres
alfanuméricos).

### Gestión de Claves

El par de claves ML-KEM se deriva determinísticamente de la clave de gasto:

```text
SpendingKey (sk) [32 bytes]
  |
  +-> ... (toda la derivación de claves Orchard existente sin cambios)
  |
  +-> ml_kem_d = PRF^expand(sk, [0x09])[0..32]
  +-> ml_kem_z = PRF^expand(sk, [0x0A])[0..32]
        |
        +-> (ek_pq, dk_pq) = ML-KEM-768.KeyGen(d=ml_kem_d, z=ml_kem_z)
              ek_pq: 1,184 bytes (pública, incluida en la dirección)
              dk_pq: 2,400 bytes (privada, parte de la clave de visualización)
```

**No se necesitan cambios en respaldos.** La frase semilla de 24 palabras existente
cubre la clave ML-KEM porque se deriva de la clave de gasto determinísticamente.
La recuperación de billetera funciona como antes.

**Las direcciones diversificadas** comparten todas el mismo `ek_pq` porque ML-KEM
no tiene un mecanismo de diversificación natural como la multiplicación escalar de
Pallas. Esto significa que un observador con dos direcciones de un usuario puede
vincularlas comparando `ek_pq`.

### Rendimiento de Descifrado por Prueba

| Paso | Actual | Híbrido | Delta |
|------|--------|---------|-------|
| Pallas ECDH | ~100 us | ~100 us | — |
| ML-KEM-768 Decaps | — | ~40 us | +40 us |
| BLAKE2b KDF | ~0.5 us | ~1 us | — |
| ChaCha20 (52 bytes) | ~0.1 us | ~0.1 us | — |
| **Total por nota** | **~101 us** | **~141 us** | **+40% sobrecarga** |

Escanear 100,000 notas: ~10.1 seg → ~14.1 seg. La sobrecarga es significativa pero
no prohibitiva. La desencapsulación de ML-KEM es en tiempo constante sin ventaja de
procesamiento por lotes (a diferencia de las operaciones de curvas elípticas), por lo
que escala linealmente.

### Impacto en Circuitos ZK

**Ninguno.** El KEM híbrido está enteramente en la capa de transporte/cifrado. El
circuito Halo 2 demuestra la existencia de notas, la corrección de anuladores y el
balance de valor — no demuestra nada sobre el cifrado. Sin cambios en claves de
prueba, claves de verificación ni restricciones de circuito.

### Comparación con la Industria

| Sistema | Enfoque | Estado |
|---------|---------|--------|
| **Signal** (PQXDH) | X25519 + ML-KEM-768, obligatorio para todos los usuarios | **Desplegado** (2023) |
| **Chrome/Firefox TLS** | X25519 + ML-KEM-768 intercambio de claves híbrido | **Desplegado** (2024) |
| **X-Wing** (borrador IETF) | X25519 + ML-KEM-768, combinador diseñado específicamente | Borrador de estándar |
| **Zcash** | Borrador ZIP de recuperabilidad cuántica (recuperación de fondos, no cifrado) | Solo en discusión |
| **Dash Platform** | Pallas ECDH + ML-KEM-768 (propuesto) | Fase de diseño |

## Cuándo Desplegar

### La Pregunta del Cronograma

- **Estado actual (2026):** Ninguna computadora cuántica puede romper ECC de 255 bits.
  La mayor factorización cuántica demostrada: ~50 bits. Brecha: órdenes de magnitud.
- **Corto plazo (2030-2035):** Las hojas de ruta de hardware de IBM, Google, Quantinuum
  apuntan a millones de qubits. Las implementaciones y conjuntos de parámetros de
  ML-KEM habrán madurado.
- **Mediano plazo (2035-2050):** La mayoría de las estimaciones sitúan la llegada de
  CRQC en esta ventana. Los datos HNDL recopilados hoy están en riesgo.
- **Largo plazo (2050+):** Consenso entre criptógrafos: las computadoras cuánticas a
  gran escala son cuestión de "cuándo", no de "si".

### Estrategia Recomendada

**1. Diseñar para actualizable ahora.** Asegurar que el formato del registro
almacenado, la estructura `TransmittedNoteCiphertext` y el diseño de entradas del
BulkAppendTree estén versionados y sean extensibles. Esto tiene bajo costo y preserva
la opción de agregar KEM híbrido después.

**2. Desplegar KEM híbrido cuando esté listo, hacerlo obligatorio.** No ofrecer dos
pools (clásico e híbrido). Dividir el conjunto de anonimato anula el propósito de las
transacciones blindadas — los usuarios que se ocultan entre un grupo más pequeño tienen
menos privacidad, no más. Cuando se despliegue, cada nota usa el esquema híbrido.

**3. Apuntar a la ventana 2028-2030.** Esto es mucho antes de cualquier amenaza
cuántica realista pero después de que las implementaciones de ML-KEM y los tamaños de
parámetros se hayan estabilizado. También permite aprender de la experiencia de
despliegue de Zcash y Signal.

**4. Monitorear eventos desencadenantes:**
- NIST o NSA imponiendo plazos de migración post-cuántica
- Avances significativos en hardware cuántico (>100,000 qubits físicos con
  corrección de errores)
- Avances criptoanalíticos contra problemas de redes (afectarían la elección de ML-KEM)

### Qué No Necesita Acción Urgente

| Componente | Por qué puede esperar |
|------------|----------------------|
| Firmas de autorización de gasto | La falsificación es en tiempo real, no retroactiva. Actualizar a ML-DSA/SLH-DSA antes de que llegue la CRQC. |
| Sistema de pruebas Halo 2 | La falsificación de pruebas es en tiempo real. Migrar a un sistema basado en STARK cuando sea necesario. |
| Resistencia a colisiones de Sinsemilla | Solo útil para nuevos ataques, no retroactivos. Subsumida por la migración del sistema de pruebas. |
| Infraestructura GroveDB Merk/MMR/Blake3 | Ya es segura cuánticamente bajo las suposiciones criptográficas actuales. No se necesita acción basándose en los ataques conocidos. |

## Referencia de Alternativas Post-Cuánticas

### Para Cifrado (reemplazando ECDH)

| Esquema | Tipo | Clave pública | Texto cifrado | Categoría NIST | Notas |
|---------|------|--------------|---------------|----------------|-------|
| ML-KEM-768 | Lattice (MLWE) | 1,184 B | 1,088 B | 3 (192-bit) | FIPS 203, estándar de la industria |
| ML-KEM-512 | Lattice (MLWE) | 800 B | 768 B | 1 (128-bit) | Más pequeño, menor margen |
| ML-KEM-1024 | Lattice (MLWE) | 1,568 B | 1,568 B | 5 (256-bit) | Excesivo para híbrido |

### Para Firmas (reemplazando RedPallas/Schnorr)

| Esquema | Tipo | Clave pública | Firma | Categoría NIST | Notas |
|---------|------|--------------|-------|----------------|-------|
| ML-DSA-65 (Dilithium) | Lattice | 1,952 B | 3,293 B | 3 | FIPS 204, rápido |
| SLH-DSA (SPHINCS+) | Basado en hash | 32-64 B | 7,856-49,856 B | 1-5 | FIPS 205, conservador |
| XMSS/LMS | Basado en hash (con estado) | 60 B | 2,500 B | variable | Con estado — reutilizar = romper |

### Para Pruebas ZK (reemplazando Halo 2)

| Sistema | Supuesto | Tamaño de prueba | Post-cuántico | Notas |
|---------|----------|-----------------|---------------|-------|
| STARKs | Funciones hash (resistencia a colisiones) | ~100-400 KB | **Sí** | Usado por StarkNet |
| Plonky3 | FRI (compromiso polinomial basado en hash) | ~50-200 KB | **Sí** | Desarrollo activo |
| Halo 2 (actual) | ECDLP en curvas Pasta | ~5 KB | **No** | Sistema actual de Orchard |
| Lattice SNARKs | MLWE | Investigación | **Sí** | No listo para producción |

### Ecosistema de Crates de Rust

| Crate | Fuente | FIPS 203 | Verificado | Notas |
|-------|--------|----------|------------|-------|
| `libcrux-ml-kem` | Cryspen | Sí | Formalmente verificado (hax/F*) | Mayor garantía |
| `ml-kem` | RustCrypto | Sí | Tiempo constante, no auditado | Compatibilidad con ecosistema |
| `fips203` | integritychain | Sí | Tiempo constante | Rust puro, no_std |

## Resumen

```text
┌─────────────────────────────────────────────────────────────────────┐
│  RESUMEN DE AMENAZAS CUÁNTICAS PARA GROVEDB + ORCHARD              │
│                                                                     │
│  SEGURO BAJO SUPOSICIONES ACTUALES (basado en hash):               │
│    ✓ Árboles Merk Blake3, MMR, BulkAppendTree                      │
│    ✓ BLAKE2b KDF, PRF^expand                                       │
│    ✓ Cifrado simétrico ChaCha20-Poly1305                           │
│    ✓ Todas las cadenas de autenticación de pruebas de GroveDB      │
│                                                                     │
│  ARREGLAR ANTES DE ALMACENAR DATOS (HNDL retroactivo):             │
│    ✗ Cifrado de notas (acuerdo de claves ECDH) → KEM Híbrido      │
│    ✗ Compromisos de valor (Pedersen) → montos revelados            │
│                                                                     │
│  ARREGLAR ANTES DE QUE LLEGUEN LAS COMPUTADORAS CUÁNTICAS          │
│  (solo tiempo real):                                                │
│    ~ Autorización de gasto → ML-DSA / SLH-DSA                     │
│    ~ Pruebas ZK → STARKs / Plonky3                                │
│    ~ Sinsemilla → árbol Merkle basado en hash                      │
│                                                                     │
│  CRONOGRAMA RECOMENDADO:                                            │
│    2026-2028: Diseñar para actualizabilidad, versionar formatos    │
│    2028-2030: Desplegar KEM híbrido obligatorio para cifrado       │
│    2035+: Migrar firmas y sistema de pruebas si es necesario       │
└─────────────────────────────────────────────────────────────────────┘
```

---
