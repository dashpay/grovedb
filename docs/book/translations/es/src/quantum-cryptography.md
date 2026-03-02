# Criptografia Cuantica — Analisis de Amenazas Post-Cuanticas

Este capitulo analiza como las computadoras cuanticas afectarian las primitivas
criptograficas utilizadas en GroveDB y los protocolos de transacciones blindadas
construidos sobre el (Orchard, Dash Platform). Cubre que componentes son vulnerables,
cuales son seguros, que significa "cosechar ahora, descifrar despues" para los datos
almacenados, y que estrategias de mitigacion existen, incluidos los disenos de KEM
hibrido.

## Dos Algoritmos Cuanticos Relevantes

Solo dos algoritmos cuanticos son relevantes para la criptografia en la practica:

**El algoritmo de Shor** resuelve el problema del logaritmo discreto (y la
factorizacion de enteros) en tiempo polinomial. Para una curva eliptica de 255 bits
como Pallas, esto requiere aproximadamente 510 qubits logicos, pero con la
sobrecarga de correccion de errores, el requisito real es aproximadamente 4 millones
de qubits fisicos. El algoritmo de Shor **rompe completamente** toda la criptografia
de curvas elipticas independientemente del tamano de la clave.

**El algoritmo de Grover** proporciona una aceleracion cuadratica para la busqueda
por fuerza bruta. Una clave simetrica de 256 bits efectivamente se convierte en
128 bits. Sin embargo, la profundidad del circuito para el algoritmo de Grover en
un espacio de claves de 128 bits sigue siendo 2^64 operaciones cuanticas — muchos
criptografos creen que esto nunca sera practico en hardware real debido a los limites
de decoherencia. El algoritmo de Grover reduce los margenes de seguridad pero no
rompe la criptografia simetrica bien parametrizada.

| Algoritmo | Objetivos | Aceleracion | Impacto practico |
|-----------|-----------|-------------|------------------|
| **Shor** | Logaritmo discreto ECC, factorizacion RSA | Exponencial (tiempo polinomial) | **Ruptura total** de ECC |
| **Grover** | Busqueda de claves simetricas, preimagen de hash | Cuadratica (reduce bits de clave a la mitad) | 256-bit → 128-bit (aun seguro) |

## Primitivas Criptograficas de GroveDB

GroveDB y el protocolo blindado basado en Orchard utilizan una combinacion de
primitivas de curvas elipticas y primitivas simetricas/basadas en hash. La tabla
siguiente clasifica cada primitiva segun su vulnerabilidad cuantica:

### Vulnerable a Computacion Cuantica (algoritmo de Shor — 0 bits post-cuanticos)

| Primitiva | Donde se usa | Que se rompe |
|-----------|-------------|-------------|
| **Pallas ECDLP** | Compromisos de notas (cmx), claves efimeras (epk/esk), claves de visualizacion (ivk), claves de pago (pk_d), derivacion de anuladores | Recuperar cualquier clave privada de su contraparte publica |
| **Acuerdo de claves ECDH** (Pallas) | Derivacion de claves de cifrado simetricas para textos cifrados de notas | Recuperar secreto compartido → descifrar todas las notas |
| **Hash Sinsemilla** | Rutas Merkle del CommitmentTree, hashing dentro del circuito | La resistencia a colisiones depende de ECDLP; se degrada cuando Pallas se rompe |
| **Halo 2 IPA** | Sistema de pruebas ZK (compromiso polinomial sobre curvas Pasta) | Falsificar pruebas para declaraciones falsas (falsificacion, gastos no autorizados) |
| **Compromisos de Pedersen** | Compromisos de valor (cv_net) que ocultan montos de transacciones | Recuperar montos ocultos; falsificar pruebas de balance |

### Seguro ante Computacion Cuantica (algoritmo de Grover — 128+ bits post-cuanticos)

| Primitiva | Donde se usa | Seguridad post-cuantica |
|-----------|-------------|------------------------|
| **Blake3** | Hashes de nodos de arboles Merk, nodos MMR, raices de estado de BulkAppendTree, prefijos de rutas de subarboles | 128-bit preimagen, 128-bit segunda preimagen |
| **BLAKE2b-256** | KDF para derivacion de claves simetricas, clave de cifrado saliente, PRF^expand | 128-bit preimagen |
| **ChaCha20-Poly1305** | Cifra enc_ciphertext y out_ciphertext (claves de 256 bits) | 128-bit busqueda de clave (seguro, pero la ruta de derivacion de clave a traves de ECDH no lo es) |
| **PRF^expand** (BLAKE2b-512) | Deriva esk, rcm, psi de rseed | 128-bit seguridad PRF |

### Infraestructura de GroveDB: Completamente Segura ante Computacion Cuantica

Todas las estructuras de datos propias de GroveDB dependen exclusivamente de hashing Blake3:

- **Arboles AVL Merk** — hashes de nodos, combined_value_hash, propagacion de hash hijo
- **Arboles MMR** — hashes de nodos internos, calculo de picos, derivacion de raiz
- **BulkAppendTree** — cadenas de hash de buffer, raices Merkle densas, MMR de epocas
- **Raiz de estado del CommitmentTree** — `blake3("ct_state" || sinsemilla_root || bulk_state_root)`
- **Prefijos de rutas de subarboles** — hashing Blake3 de segmentos de ruta
- **Pruebas V1** — cadenas de autenticacion a traves de la jerarquia Merk

**No se necesitan cambios.** Las pruebas de arboles Merk de GroveDB, las verificaciones
de consistencia de MMR, las raices de epocas de BulkAppendTree y todas las cadenas de
autenticacion de pruebas V1 permanecen seguras contra computadoras cuanticas. La
infraestructura basada en hash es la parte mas solida del sistema post-cuantico.

## Amenazas Retroactivas vs. en Tiempo Real

Esta distincion es critica para priorizar que arreglar y cuando.

**Las amenazas retroactivas** comprometen datos que ya estan almacenados. Un adversario
registra datos hoy y los descifra cuando las computadoras cuanticas esten disponibles.
Estas amenazas **no pueden mitigarse despues del hecho** — una vez que los datos estan
en la cadena de bloques, no pueden re-cifrarse ni retirarse.

**Las amenazas en tiempo real** solo afectan transacciones creadas en el futuro. Un
adversario con una computadora cuantica podria falsificar firmas o pruebas, pero solo
para nuevas transacciones. Las transacciones antiguas ya fueron validadas y confirmadas
por la red.

| Amenaza | Tipo | Que se expone | Urgencia |
|---------|------|--------------|----------|
| **Descifrado de notas** (enc_ciphertext) | **Retroactiva** | Contenido de notas: destinatario, monto, memo, rseed | **Alta** — almacenado permanentemente |
| **Apertura de compromiso de valor** (cv_net) | **Retroactiva** | Montos de transacciones (pero no remitente/destinatario) | **Media** — solo montos |
| **Datos de recuperacion del remitente** (out_ciphertext) | **Retroactiva** | Claves de recuperacion del remitente para notas enviadas | **Alta** — almacenado permanentemente |
| Falsificacion de autorizacion de gasto | En tiempo real | Podria falsificar nuevas firmas de gasto | Baja — actualizar antes de que llegue la CC |
| Falsificacion de pruebas Halo 2 | En tiempo real | Podria falsificar nuevas pruebas (falsificacion) | Baja — actualizar antes de que llegue la CC |
| Colision de Sinsemilla | En tiempo real | Podria falsificar nuevas rutas Merkle | Baja — subsumida por la falsificacion de pruebas |
| Falsificacion de firma de vinculacion | En tiempo real | Podria falsificar nuevas pruebas de balance | Baja — actualizar antes de que llegue la CC |

### Que se Revela Exactamente?

**Si se rompe el cifrado de notas** (la amenaza HNDL principal):

Un adversario cuantico recupera `esk` del `epk` almacenado mediante el algoritmo de
Shor, calcula el secreto compartido ECDH, deriva la clave simetrica y descifra
`enc_ciphertext`. Esto revela el texto plano completo de la nota:

| Campo | Tamano | Que revela |
|-------|--------|-----------|
| version | 1 byte | Version del protocolo (no sensible) |
| diversifier | 11 bytes | Componente de la direccion del destinatario |
| value | 8 bytes | Monto exacto de la transaccion |
| rseed | 32 bytes | Permite vincular anuladores (desanonimiza el grafo de transacciones) |
| memo | 36 bytes (DashMemo) | Datos de la aplicacion, potencialmente identificadores |

Con `rseed` y `rho` (almacenados junto al texto cifrado), el adversario puede
calcular `esk = PRF(rseed, rho)` y verificar la vinculacion de la clave efimera.
Combinado con el diversifier, esto vincula entradas con salidas a traves de todo
el historial de transacciones — **desanonimizacion completa del pool blindado**.

**Si solo se rompen los compromisos de valor** (amenaza HNDL secundaria):

El adversario recupera `v` de `cv_net = [v]*V + [rcv]*R` resolviendo ECDLP.
Esto revela **montos de transacciones pero no las identidades del remitente ni del
destinatario**. El adversario ve "alguien envio 5.0 Dash a alguien" pero no puede
vincular el monto a ninguna direccion o identidad sin romper tambien el cifrado
de notas.

Por si solos, los montos sin vinculacion tienen utilidad limitada. Pero combinados
con datos externos (temporalidad, facturas conocidas, montos que coinciden con
solicitudes publicas), los ataques de correlacion se vuelven posibles.

## El Ataque "Cosechar Ahora, Descifrar Despues"

Esta es la amenaza cuantica mas urgente y practica.

**Modelo de ataque:** Un adversario a nivel estatal (o cualquier parte con
almacenamiento suficiente) registra todos los datos de transacciones blindadas
en cadena hoy. Estos datos estan disponibles publicamente en la blockchain y son
inmutables. El adversario espera una computadora cuantica criptograficamente
relevante (CRQC), y entonces:

```text
Paso 1: Leer registro almacenado del BulkAppendTree del CommitmentTree:
        cmx (32) || rho (32) || epk (32) || enc_ciphertext (104) || out_ciphertext (80)

Paso 2: Resolver ECDLP en Pallas via algoritmo de Shor:
        epk = [esk] * g_d  →  recuperar esk

Paso 3: Calcular secreto compartido:
        shared_secret = [esk] * pk_d

Paso 4: Derivar clave simetrica (BLAKE2b es seguro cuanticamente, pero la entrada esta comprometida):
        K_enc = BLAKE2b-256("Zcash_OrchardKDF", shared_secret || epk)

Paso 5: Descifrar enc_ciphertext usando ChaCha20-Poly1305:
        → version || diversifier || value || rseed || memo

Paso 6: Con rseed + rho, vincular anuladores a compromisos de notas:
        esk = PRF(rseed, rho)
        → reconstruccion completa del grafo de transacciones
```

**Hallazgo clave:** El cifrado simetrico (ChaCha20-Poly1305) es perfectamente
seguro ante computacion cuantica. La vulnerabilidad esta enteramente en la **ruta
de derivacion de la clave** — la clave simetrica se deriva de un secreto compartido
ECDH, y ECDH es roto por el algoritmo de Shor. El atacante no rompe el cifrado;
recupera la clave.

**Retroactividad:** Este ataque es **completamente retroactivo**. Cada nota cifrada
almacenada en la cadena de bloques puede ser descifrada una vez que exista una CRQC.
Los datos no pueden re-cifrarse ni protegerse despues del hecho. Por eso debe
abordarse antes de que los datos sean almacenados, no despues.

## Mitigacion: KEM Hibrido (ML-KEM + ECDH)

La defensa contra HNDL es derivar la clave de cifrado simetrica de
**dos mecanismos independientes de acuerdo de claves**, de manera que romper solo
uno sea insuficiente. Esto se llama KEM hibrido.

### ML-KEM-768 (CRYSTALS-Kyber)

ML-KEM es el mecanismo de encapsulacion de claves post-cuantico estandarizado por
NIST (FIPS 203, agosto 2024) basado en el problema Module Learning With Errors (MLWE).

| Parametro | ML-KEM-512 | ML-KEM-768 | ML-KEM-1024 |
|-----------|-----------|-----------|------------|
| Clave publica (ek) | 800 bytes | **1,184 bytes** | 1,568 bytes |
| Texto cifrado (ct) | 768 bytes | **1,088 bytes** | 1,568 bytes |
| Secreto compartido | 32 bytes | 32 bytes | 32 bytes |
| Categoria NIST | 1 (128-bit) | **3 (192-bit)** | 5 (256-bit) |

**ML-KEM-768** es la opcion recomendada — es el conjunto de parametros utilizado por
X-Wing, PQXDH de Signal y el intercambio de claves hibrido TLS de Chrome/Firefox.
La Categoria 3 proporciona un margen comodo contra futuros avances en criptoanalisis
de redes.

### Como Funciona el Esquema Hibrido

**Flujo actual (vulnerable):**

```text
Remitente:
  esk = PRF(rseed, rho)                    // deterministico desde la nota
  epk = [esk] * g_d                         // punto de curva Pallas
  shared_secret = [esk] * pk_d              // ECDH (roto por Shor)
  K_enc = BLAKE2b("Zcash_OrchardKDF", shared_secret || epk)
  enc_ciphertext = ChaCha20(K_enc, note_plaintext)
```

**Flujo hibrido (resistente a computacion cuantica):**

```text
Remitente:
  esk = PRF(rseed, rho)                    // sin cambios
  epk = [esk] * g_d                         // sin cambios
  ss_ecdh = [esk] * pk_d                    // mismo ECDH

  (ct_pq, ss_pq) = ML-KEM-768.Encaps(ek_pq)  // NUEVO: KEM basado en redes
                                                // ek_pq de la direccion del destinatario

  K_enc = BLAKE2b(                          // MODIFICADO: combina ambos secretos
      "DashPlatform_HybridKDF",
      ss_ecdh || ss_pq || ct_pq || epk
  )

  enc_ciphertext = ChaCha20(K_enc, note_plaintext)  // sin cambios
```

**Descifrado del destinatario:**

```text
Destinatario:
  ss_ecdh = [ivk] * epk                    // mismo ECDH (usando clave de visualizacion entrante)
  ss_pq = ML-KEM-768.Decaps(dk_pq, ct_pq)  // NUEVO: desencapsulacion KEM
  K_enc = BLAKE2b("DashPlatform_HybridKDF", ss_ecdh || ss_pq || ct_pq || epk)
  note_plaintext = ChaCha20.Decrypt(K_enc, enc_ciphertext)
```

### Garantia de Seguridad

El KEM combinado es seguro IND-CCA2 si **cualquiera** de los KEM componentes es
seguro. Esto esta formalmente demostrado por [Giacon, Heuer y Poettering (2018)](https://eprint.iacr.org/2018/024)
para combinadores de KEM que usan un PRF (BLAKE2b califica), e independientemente por
la [prueba de seguridad de X-Wing](https://eprint.iacr.org/2024/039).

| Escenario | ECDH | ML-KEM | Clave combinada | Estado |
|-----------|------|--------|----------------|--------|
| Mundo clasico | Seguro | Seguro | **Seguro** | Ambos intactos |
| Cuantica rompe ECC | **Roto** | Seguro | **Seguro** | ML-KEM protege |
| Avances en redes rompen ML-KEM | Seguro | **Roto** | **Seguro** | ECDH protege (igual que hoy) |
| Ambos rotos | Roto | Roto | **Roto** | Requiere dos avances simultaneos |

### Impacto en Tamano

El KEM hibrido agrega el texto cifrado de ML-KEM-768 (1,088 bytes) a cada nota
almacenada y expande el texto cifrado saliente para incluir el secreto compartido
de ML-KEM para la recuperacion del remitente:

**Registro almacenado por nota:**

```text
┌──────────────────────────────────────────────────────────────────┐
│  Actual (280 bytes)            Hibrido (1,400 bytes)             │
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

| Notas | Actual (280 B) | Hibrido (1,400 B) | Delta |
|-------|----------------|-------------------|-------|
| 100,000 | 26.7 MB | 133 MB | +106 MB |
| 1,000,000 | 267 MB | 1.33 GB | +1.07 GB |
| 10,000,000 | 2.67 GB | 13.3 GB | +10.7 GB |

**Tamano de direccion:**

```text
Actual:   diversifier (11) + pk_d (32) = 43 bytes
Hibrido:  diversifier (11) + pk_d (32) + ek_pq (1,184) = 1,227 bytes
```

La clave publica de ML-KEM de 1,184 bytes debe incluirse en la direccion para que
los remitentes puedan realizar la encapsulacion. Con aproximadamente 1,960 caracteres
Bech32m, es grande pero aun cabe en un codigo QR (maximo ~2,953 caracteres
alfanumericos).

### Gestion de Claves

El par de claves ML-KEM se deriva deterministicamente de la clave de gasto:

```text
SpendingKey (sk) [32 bytes]
  |
  +-> ... (toda la derivacion de claves Orchard existente sin cambios)
  |
  +-> ml_kem_d = PRF^expand(sk, [0x09])[0..32]
  +-> ml_kem_z = PRF^expand(sk, [0x0A])[0..32]
        |
        +-> (ek_pq, dk_pq) = ML-KEM-768.KeyGen(d=ml_kem_d, z=ml_kem_z)
              ek_pq: 1,184 bytes (publica, incluida en la direccion)
              dk_pq: 2,400 bytes (privada, parte de la clave de visualizacion)
```

**No se necesitan cambios en respaldos.** La frase semilla de 24 palabras existente
cubre la clave ML-KEM porque se deriva de la clave de gasto deterministicamente.
La recuperacion de billetera funciona como antes.

**Las direcciones diversificadas** comparten todas el mismo `ek_pq` porque ML-KEM
no tiene un mecanismo de diversificacion natural como la multiplicacion escalar de
Pallas. Esto significa que un observador con dos direcciones de un usuario puede
vincularlas comparando `ek_pq`.

### Rendimiento de Descifrado por Prueba

| Paso | Actual | Hibrido | Delta |
|------|--------|---------|-------|
| Pallas ECDH | ~100 us | ~100 us | — |
| ML-KEM-768 Decaps | — | ~40 us | +40 us |
| BLAKE2b KDF | ~0.5 us | ~1 us | — |
| ChaCha20 (52 bytes) | ~0.1 us | ~0.1 us | — |
| **Total por nota** | **~101 us** | **~141 us** | **+40% sobrecarga** |

Escanear 100,000 notas: ~10.1 seg → ~14.1 seg. La sobrecarga es significativa pero
no prohibitiva. La desencapsulacion de ML-KEM es en tiempo constante sin ventaja de
procesamiento por lotes (a diferencia de las operaciones de curvas elipticas), por lo
que escala linealmente.

### Impacto en Circuitos ZK

**Ninguno.** El KEM hibrido esta enteramente en la capa de transporte/cifrado. El
circuito Halo 2 demuestra la existencia de notas, la correccion de anuladores y el
balance de valor — no demuestra nada sobre el cifrado. Sin cambios en claves de
prueba, claves de verificacion ni restricciones de circuito.

### Comparacion con la Industria

| Sistema | Enfoque | Estado |
|---------|---------|--------|
| **Signal** (PQXDH) | X25519 + ML-KEM-768, obligatorio para todos los usuarios | **Desplegado** (2023) |
| **Chrome/Firefox TLS** | X25519 + ML-KEM-768 intercambio de claves hibrido | **Desplegado** (2024) |
| **X-Wing** (borrador IETF) | X25519 + ML-KEM-768, combinador disenado especificamente | Borrador de estandar |
| **Zcash** | Borrador ZIP de recuperabilidad cuantica (recuperacion de fondos, no cifrado) | Solo en discusion |
| **Dash Platform** | Pallas ECDH + ML-KEM-768 (propuesto) | Fase de diseno |

## Cuando Desplegar

### La Pregunta del Cronograma

- **Estado actual (2026):** Ninguna computadora cuantica puede romper ECC de 255 bits.
  La mayor factorizacion cuantica demostrada: ~50 bits. Brecha: ordenes de magnitud.
- **Corto plazo (2030-2035):** Las hojas de ruta de hardware de IBM, Google, Quantinuum
  apuntan a millones de qubits. Las implementaciones y conjuntos de parametros de
  ML-KEM habran madurado.
- **Mediano plazo (2035-2050):** La mayoria de las estimaciones situan la llegada de
  CRQC en esta ventana. Los datos HNDL recopilados hoy estan en riesgo.
- **Largo plazo (2050+):** Consenso entre criptografos: las computadoras cuanticas a
  gran escala son cuestion de "cuando", no de "si".

### Estrategia Recomendada

**1. Disenar para actualizable ahora.** Asegurar que el formato del registro
almacenado, la estructura `TransmittedNoteCiphertext` y el diseno de entradas del
BulkAppendTree esten versionados y sean extensibles. Esto tiene bajo costo y preserva
la opcion de agregar KEM hibrido despues.

**2. Desplegar KEM hibrido cuando este listo, hacerlo obligatorio.** No ofrecer dos
pools (clasico e hibrido). Dividir el conjunto de anonimato anula el proposito de las
transacciones blindadas — los usuarios que se ocultan entre un grupo mas pequeno tienen
menos privacidad, no mas. Cuando se despliegue, cada nota usa el esquema hibrido.

**3. Apuntar a la ventana 2028-2030.** Esto es mucho antes de cualquier amenaza
cuantica realista pero despues de que las implementaciones de ML-KEM y los tamanos de
parametros se hayan estabilizado. Tambien permite aprender de la experiencia de
despliegue de Zcash y Signal.

**4. Monitorear eventos desencadenantes:**
- NIST o NSA imponiendo plazos de migracion post-cuantica
- Avances significativos en hardware cuantico (>100,000 qubits fisicos con
  correccion de errores)
- Avances criptoanaliticos contra problemas de redes (afectarian la eleccion de ML-KEM)

### Que No Necesita Accion Urgente

| Componente | Por que puede esperar |
|------------|----------------------|
| Firmas de autorizacion de gasto | La falsificacion es en tiempo real, no retroactiva. Actualizar a ML-DSA/SLH-DSA antes de que llegue la CRQC. |
| Sistema de pruebas Halo 2 | La falsificacion de pruebas es en tiempo real. Migrar a un sistema basado en STARK cuando sea necesario. |
| Resistencia a colisiones de Sinsemilla | Solo util para nuevos ataques, no retroactivos. Subsumida por la migracion del sistema de pruebas. |
| Infraestructura GroveDB Merk/MMR/Blake3 | **Ya es segura ante computacion cuantica.** No se necesita accion, ni ahora ni nunca. |

## Referencia de Alternativas Post-Cuanticas

### Para Cifrado (reemplazando ECDH)

| Esquema | Tipo | Clave publica | Texto cifrado | Categoria NIST | Notas |
|---------|------|--------------|---------------|----------------|-------|
| ML-KEM-768 | Lattice (MLWE) | 1,184 B | 1,088 B | 3 (192-bit) | FIPS 203, estandar de la industria |
| ML-KEM-512 | Lattice (MLWE) | 800 B | 768 B | 1 (128-bit) | Mas pequeno, menor margen |
| ML-KEM-1024 | Lattice (MLWE) | 1,568 B | 1,568 B | 5 (256-bit) | Excesivo para hibrido |

### Para Firmas (reemplazando RedPallas/Schnorr)

| Esquema | Tipo | Clave publica | Firma | Categoria NIST | Notas |
|---------|------|--------------|-------|----------------|-------|
| ML-DSA-65 (Dilithium) | Lattice | 1,952 B | 3,293 B | 3 | FIPS 204, rapido |
| SLH-DSA (SPHINCS+) | Basado en hash | 32-64 B | 7,856-49,856 B | 1-5 | FIPS 205, conservador |
| XMSS/LMS | Basado en hash (con estado) | 60 B | 2,500 B | variable | Con estado — reutilizar = romper |

### Para Pruebas ZK (reemplazando Halo 2)

| Sistema | Supuesto | Tamano de prueba | Post-cuantico | Notas |
|---------|----------|-----------------|---------------|-------|
| STARKs | Funciones hash (resistencia a colisiones) | ~100-400 KB | **Si** | Usado por StarkNet |
| Plonky3 | FRI (compromiso polinomial basado en hash) | ~50-200 KB | **Si** | Desarrollo activo |
| Halo 2 (actual) | ECDLP en curvas Pasta | ~5 KB | **No** | Sistema actual de Orchard |
| Lattice SNARKs | MLWE | Investigacion | **Si** | No listo para produccion |

### Ecosistema de Crates de Rust

| Crate | Fuente | FIPS 203 | Verificado | Notas |
|-------|--------|----------|------------|-------|
| `libcrux-ml-kem` | Cryspen | Si | Formalmente verificado (hax/F*) | Mayor garantia |
| `ml-kem` | RustCrypto | Si | Tiempo constante, no auditado | Compatibilidad con ecosistema |
| `fips203` | integritychain | Si | Tiempo constante | Rust puro, no_std |

## Resumen

```text
┌─────────────────────────────────────────────────────────────────────┐
│  RESUMEN DE AMENAZAS CUANTICAS PARA GROVEDB + ORCHARD              │
│                                                                     │
│  SEGURO AHORA Y SIEMPRE (basado en hash):                          │
│    ✓ Arboles Merk Blake3, MMR, BulkAppendTree                      │
│    ✓ BLAKE2b KDF, PRF^expand                                       │
│    ✓ Cifrado simetrico ChaCha20-Poly1305                           │
│    ✓ Todas las cadenas de autenticacion de pruebas de GroveDB      │
│                                                                     │
│  ARREGLAR ANTES DE ALMACENAR DATOS (HNDL retroactivo):             │
│    ✗ Cifrado de notas (acuerdo de claves ECDH) → KEM Hibrido      │
│    ✗ Compromisos de valor (Pedersen) → montos revelados            │
│                                                                     │
│  ARREGLAR ANTES DE QUE LLEGUEN LAS COMPUTADORAS CUANTICAS          │
│  (solo tiempo real):                                                │
│    ~ Autorizacion de gasto → ML-DSA / SLH-DSA                     │
│    ~ Pruebas ZK → STARKs / Plonky3                                │
│    ~ Sinsemilla → arbol Merkle basado en hash                      │
│                                                                     │
│  CRONOGRAMA RECOMENDADO:                                            │
│    2026-2028: Disenar para actualizabilidad, versionar formatos    │
│    2028-2030: Desplegar KEM hibrido obligatorio para cifrado       │
│    2035+: Migrar firmas y sistema de pruebas si es necesario       │
└─────────────────────────────────────────────────────────────────────┘
```

---
