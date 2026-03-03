# L'albero Merk — Un albero Merkle AVL

L'albero Merk e il mattone fondamentale di GroveDB. Ogni sotto-albero nel bosco e un albero Merk — un albero binario di ricerca autobilanciante dove ogni nodo e sottoposto a hash crittografico, producendo un singolo hash radice che autentica l'intero contenuto dell'albero.

## Cos'e un nodo Merk?

A differenza di molte implementazioni di alberi di Merkle dove i dati risiedono solo nelle foglie, in un albero Merk **ogni nodo memorizza una coppia chiave-valore**. Cio significa che non esistono nodi interni "vuoti" — l'albero e contemporaneamente sia una struttura di ricerca che un archivio dati.

```mermaid
graph TD
    subgraph TreeNode
        subgraph inner["inner: Box&lt;TreeNodeInner&gt;"]
            subgraph kv["kv: KV"]
                KEY["<b>key:</b> Vec&lt;u8&gt;<br/><i>es. b&quot;alice&quot;</i>"]
                VAL["<b>value:</b> Vec&lt;u8&gt;<br/><i>byte dell'Element serializzato</i>"]
                FT["<b>feature_type:</b> TreeFeatureType<br/><i>BasicMerkNode | SummedMerkNode(n) | ...</i>"]
                VH["<b>value_hash:</b> [u8; 32]<br/><i>H(varint(value.len) ‖ value)</i>"]
                KVH["<b>hash:</b> [u8; 32] — il kv_hash<br/><i>H(varint(key.len) ‖ key ‖ value_hash)</i>"]
            end
            LEFT["<b>left:</b> Option&lt;Link&gt;"]
            RIGHT["<b>right:</b> Option&lt;Link&gt;"]
        end
        OLD["<b>old_value:</b> Option&lt;Vec&lt;u8&gt;&gt; — valore precedente per i delta di costo"]
        KNOWN["<b>known_storage_cost:</b> Option&lt;KeyValueStorageCost&gt;"]
    end

    LEFT -->|"chiavi minori"| LC["Figlio sinistro<br/><i>Link::Reference | Modified<br/>Uncommitted | Loaded</i>"]
    RIGHT -->|"chiavi maggiori"| RC["Figlio destro<br/><i>Link::Reference | Modified<br/>Uncommitted | Loaded</i>"]

    style kv fill:#eaf2f8,stroke:#2980b9
    style inner fill:#fef9e7,stroke:#f39c12
    style TreeNode fill:#f9f9f9,stroke:#333
    style LC fill:#d5f5e3,stroke:#27ae60
    style RC fill:#d5f5e3,stroke:#27ae60
```

Nel codice (`merk/src/tree/mod.rs`):

```rust
pub struct TreeNode {
    pub(crate) inner: Box<TreeNodeInner>,
    pub(crate) old_value: Option<Vec<u8>>,        // Valore precedente per il tracciamento dei costi
    pub(crate) known_storage_cost: Option<KeyValueStorageCost>,
}

pub struct TreeNodeInner {
    pub(crate) left: Option<Link>,    // Figlio sinistro (chiavi minori)
    pub(crate) right: Option<Link>,   // Figlio destro (chiavi maggiori)
    pub(crate) kv: KV,               // Il payload chiave-valore
}
```

Il `Box<TreeNodeInner>` mantiene il nodo nell'heap, il che e essenziale poiche i link figli possono contenere ricorsivamente intere istanze di `TreeNode`.

## La struttura KV

La struttura `KV` contiene sia i dati grezzi che i loro digest crittografici (`merk/src/tree/kv.rs`):

```rust
pub struct KV {
    pub(super) key: Vec<u8>,                        // La chiave di ricerca
    pub(super) value: Vec<u8>,                      // Il valore memorizzato
    pub(super) feature_type: TreeFeatureType,       // Comportamento di aggregazione
    pub(crate) value_defined_cost: Option<ValueDefinedCostType>,
    pub(super) hash: CryptoHash,                    // kv_hash
    pub(super) value_hash: CryptoHash,              // H(value)
}
```

Due punti importanti:

1. **Le chiavi non vengono memorizzate su disco come parte del nodo codificato.** Vengono memorizzate come chiave RocksDB. Quando un nodo viene decodificato dall'archiviazione, la chiave viene iniettata dall'esterno. Cio evita la duplicazione dei byte della chiave.

2. **Vengono mantenuti due campi hash.** Il `value_hash` e `H(value)` e l'`hash` (kv_hash) e `H(key, value_hash)`. Mantenere entrambi permette al sistema di prove di scegliere quanta informazione rivelare.

## La natura semi-bilanciata — Come l'AVL "oscilla"

Un albero Merk e un **albero AVL** — il classico albero binario di ricerca autobilanciante inventato da Adelson-Velsky e Landis. L'invariante chiave e:

> Per ogni nodo, la differenza di altezza tra i sotto-alberi sinistro e destro e al massimo 1.

Questa e espressa come il **fattore di bilanciamento** (balance factor):

```text
balance_factor = altezza_destra - altezza_sinistra
```

Valori validi: **{-1, 0, 1}**

```rust
// merk/src/tree/mod.rs
pub const fn balance_factor(&self) -> i8 {
    let left_height = self.child_height(true) as i8;
    let right_height = self.child_height(false) as i8;
    right_height - left_height
}
```

Ma ecco il punto sottile: mentre ogni singolo nodo puo inclinarsi solo di un livello, queste inclinazioni possono **accumularsi** attraverso l'albero. Ecco perche lo chiamiamo "semi-bilanciato" — l'albero non e perfettamente bilanciato come un albero binario completo.

Consideriamo un albero di 10 nodi. Un albero perfettamente bilanciato avrebbe altezza 4 (ceil(log2(10+1))). Ma un albero AVL potrebbe avere altezza 5:

**Perfettamente bilanciato (altezza 4)** — ogni livello completamente pieno:

```mermaid
graph TD
    N5["5<br/><small>bf=0</small>"]
    N3["3<br/><small>bf=0</small>"]
    N8["8<br/><small>bf=0</small>"]
    N2["2<br/><small>bf=0</small>"]
    N4["4<br/><small>bf=0</small>"]
    N6["6<br/><small>bf=0</small>"]
    N9["9<br/><small>bf=+1</small>"]
    N10["10<br/><small>bf=0</small>"]

    N5 --- N3
    N5 --- N8
    N3 --- N2
    N3 --- N4
    N8 --- N6
    N8 --- N9
    N9 --- N10

    style N5 fill:#d4e6f1,stroke:#2980b9
```

**"Oscillazione" valida per AVL (altezza 5)** — ogni nodo si inclina al massimo di 1, ma si accumula:

```mermaid
graph TD
    N4["4<br/><small>bf=+1</small>"]
    N2["2<br/><small>bf=-1</small>"]
    N7["7<br/><small>bf=+1</small>"]
    N1["1<br/><small>bf=-1</small>"]
    N3["3<br/><small>bf=0</small>"]
    N5["5<br/><small>bf=0</small>"]
    N9["9<br/><small>bf=-1</small>"]
    N0["0<br/><small>bf=0</small>"]
    N8["8<br/><small>bf=0</small>"]
    N10["10<br/><small>bf=0</small>"]

    N4 --- N2
    N4 --- N7
    N2 --- N1
    N2 --- N3
    N7 --- N5
    N7 --- N9
    N1 --- N0
    N9 --- N8
    N9 --- N10

    style N4 fill:#fadbd8,stroke:#e74c3c
```

> Altezza 5 contro l'ideale 4 — questa e l'"oscillazione". Caso peggiore: h <= 1.44 x log2(n+2).

Entrambi gli alberi sono alberi AVL validi! L'altezza nel caso peggiore di un albero AVL e:

```text
h <= 1.4404 x log2(n + 2) - 0.3277
```

Quindi per **n = 1.000.000** nodi:
- Bilanciamento perfetto: altezza 20
- Caso peggiore AVL: altezza approssimativa 29

Questo sovraccarico di circa il 44% e il prezzo delle semplici regole di rotazione dell'AVL. In pratica, inserimenti casuali producono alberi molto piu vicini al bilanciamento perfetto.

Ecco come appaiono gli alberi validi e invalidi:

**VALIDO** — tutti i fattori di bilanciamento in {-1, 0, +1}:

```mermaid
graph TD
    subgraph balanced["Bilanciato (bf=0)"]
        D1["D<br/>bf=0"] --- B1["B<br/>bf=0"]
        D1 --- F1["F<br/>bf=0"]
        B1 --- A1["A"] & C1["C"]
        F1 --- E1["E"] & G1["G"]
    end
    subgraph rightlean["Inclinato a destra (bf=+1)"]
        D2["D<br/>bf=+1"] --- B2["B<br/>bf=0"]
        D2 --- F2["F<br/>bf=0"]
        B2 --- A2["A"] & C2["C"]
        F2 --- E2["E"] & G2["G"]
    end
    subgraph leftlean["Inclinato a sinistra (bf=-1)"]
        D3["D<br/>bf=-1"] --- B3["B<br/>bf=-1"]
        D3 --- E3["E"]
        B3 --- A3["A"]
    end

    style balanced fill:#d5f5e3,stroke:#27ae60
    style rightlean fill:#d5f5e3,stroke:#27ae60
    style leftlean fill:#d5f5e3,stroke:#27ae60
```

**INVALIDO** — fattore di bilanciamento = +2 (necessita rotazione!):

```mermaid
graph TD
    B["B<br/><b>bf=+2 ✗</b>"]
    D["D<br/>bf=+1"]
    F["F<br/>bf=0"]
    B --- D
    D --- F

    style B fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
```

> Il sotto-albero destro e 2 livelli piu alto del sinistro (che e vuoto). Cio innesca una **rotazione a sinistra** per ripristinare l'invariante AVL.

## Rotazioni — Ripristinare il bilanciamento

Quando un inserimento o una cancellazione causa un fattore di bilanciamento di +/-2, l'albero deve essere **ruotato** per ripristinare l'invariante AVL. Ci sono quattro casi, riducibili a due operazioni fondamentali.

### Singola rotazione a sinistra

Usata quando un nodo e **pesante a destra** (bf = +2) e il suo figlio destro e **pesante a destra o bilanciato** (bf >= 0):

**Prima** (bf=+2):

```mermaid
graph TD
    A["A<br/><small>bf=+2</small>"]
    t1["t₁"]
    B["B<br/><small>bf≥0</small>"]
    X["X"]
    C["C"]
    A --- t1
    A --- B
    B --- X
    B --- C
    style A fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
```

**Dopo** la rotazione a sinistra — B promosso a radice:

```mermaid
graph TD
    B2["B<br/><small>bf=0</small>"]
    A2["A"]
    C2["C"]
    t12["t₁"]
    X2["X"]
    B2 --- A2
    B2 --- C2
    A2 --- t12
    A2 --- X2
    style B2 fill:#d5f5e3,stroke:#27ae60,stroke-width:3px
```

> **Passi:** (1) Staccare B da A. (2) Staccare X (figlio sinistro di B). (3) Collegare X come figlio destro di A. (4) Collegare A come figlio sinistro di B. Il sotto-albero con radice B e ora bilanciato.

Nel codice (`merk/src/tree/ops.rs`):

```rust
fn rotate<V>(self, left: bool, ...) -> CostResult<Self, Error> {
    // Stacca il figlio dal lato pesante
    let (tree, child) = self.detach_expect(left, ...);
    // Stacca il nipote dal lato opposto del figlio
    let (child, maybe_grandchild) = child.detach(!left, ...);

    // Collega il nipote alla radice originale
    tree.attach(left, maybe_grandchild)
        .maybe_balance(...)
        .flat_map_ok(|tree| {
            // Collega la radice originale come figlio del nodo promosso
            child.attach(!left, Some(tree))
                .maybe_balance(...)
        })
}
```

Nota come `maybe_balance` viene chiamato ricorsivamente — la rotazione stessa potrebbe creare nuovi squilibri che necessitano ulteriori correzioni.

### Doppia rotazione (sinistra-destra)

Usata quando un nodo e **pesante a sinistra** (bf = -2) ma il suo figlio sinistro e **pesante a destra** (bf > 0). Una singola rotazione non risolverebbe il problema:

**Passo 0: Prima** — C e pesante a sinistra (bf=-2) ma il suo figlio sinistro A si inclina a destra (bf=+1). Una singola rotazione non risolverebbe:

```mermaid
graph TD
    C0["C<br/><small>bf=-2</small>"]
    A0["A<br/><small>bf=+1</small>"]
    t4["t₄"]
    t1["t₁"]
    B0["B"]
    t2["t₂"]
    t3["t₃"]
    C0 --- A0
    C0 --- t4
    A0 --- t1
    A0 --- B0
    B0 --- t2
    B0 --- t3
    style C0 fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
```

**Passo 1: Rotazione a sinistra del figlio A** — ora sia C che B si inclinano a sinistra, risolvibile con una singola rotazione:

```mermaid
graph TD
    C1["C<br/><small>bf=-2</small>"]
    B1["B"]
    t41["t₄"]
    A1["A"]
    t31["t₃"]
    t11["t₁"]
    t21["t₂"]
    C1 --- B1
    C1 --- t41
    B1 --- A1
    B1 --- t31
    A1 --- t11
    A1 --- t21
    style C1 fill:#fdebd0,stroke:#e67e22,stroke-width:2px
```

**Passo 2: Rotazione a destra della radice C** — bilanciato!

```mermaid
graph TD
    B2["B<br/><small>bf=0</small>"]
    A2["A"]
    C2["C"]
    t12["t₁"]
    t22["t₂"]
    t32["t₃"]
    t42["t₄"]
    B2 --- A2
    B2 --- C2
    A2 --- t12
    A2 --- t22
    C2 --- t32
    C2 --- t42
    style B2 fill:#d5f5e3,stroke:#27ae60,stroke-width:3px
```

L'algoritmo rileva questo caso confrontando la direzione di inclinazione del genitore con il fattore di bilanciamento del figlio:

```rust
fn maybe_balance<V>(self, ...) -> CostResult<Self, Error> {
    let balance_factor = self.balance_factor();
    if balance_factor.abs() <= 1 {
        return Ok(self);  // Gia bilanciato
    }

    let left = balance_factor < 0;  // true se pesante a sinistra

    // Doppia rotazione necessaria quando il figlio si inclina opposto al genitore
    let tree = if left == (self.tree().link(left).unwrap().balance_factor() > 0) {
        // Prima rotazione: ruota il figlio nella direzione opposta
        self.walk_expect(left, |child|
            child.rotate(!left, ...).map_ok(Some), ...
        )
    } else {
        self
    };

    // Seconda (o unica) rotazione
    tree.rotate(left, ...)
}
```

## Operazioni batch — Costruzione e applicazione

Piuttosto che inserire elementi uno alla volta, Merk supporta operazioni batch che applicano piu modifiche in un singolo passaggio. Cio e critico per l'efficienza: un batch di N operazioni su un albero di M elementi richiede **O((M + N) log(M + N))** tempo, contro O(N log M) per inserimenti sequenziali.

### Il tipo MerkBatch

```rust
type MerkBatch<K> = [(K, Op)];

enum Op {
    Put(Vec<u8>, TreeFeatureType),  // Inserimento o aggiornamento con valore e tipo di feature
    PutWithSpecializedCost(...),     // Inserimento con costo predefinito
    PutCombinedReference(...),       // Inserimento di riferimento con hash combinato
    Replace(Vec<u8>, TreeFeatureType),
    Patch { .. },                    // Aggiornamento parziale del valore
    Delete,                          // Rimozione chiave
    DeleteLayered,                   // Rimozione con costo a livelli
    DeleteMaybeSpecialized,          // Rimozione con costo specializzato opzionale
}
```

### Strategia 1: build() — Costruzione da zero

Quando l'albero e vuoto, `build()` costruisce un albero bilanciato direttamente dal batch ordinato utilizzando un algoritmo di **divisione per mediana**:

Batch di input (ordinato): `[A, B, C, D, E, F, G]` — scegliere il medio (D) come radice, ricorsione su ogni meta:

```mermaid
graph TD
    D["<b>D</b><br/><small>radice = mid(0..6)</small>"]
    B["<b>B</b><br/><small>mid(A,B,C)</small>"]
    F["<b>F</b><br/><small>mid(E,F,G)</small>"]
    A["A"]
    C["C"]
    E["E"]
    G["G"]

    D --- B
    D --- F
    B --- A
    B --- C
    F --- E
    F --- G

    style D fill:#d4e6f1,stroke:#2980b9,stroke-width:2px
    style B fill:#d5f5e3,stroke:#27ae60
    style F fill:#d5f5e3,stroke:#27ae60
```

> Risultato: albero perfettamente bilanciato con altezza = 3 = ceil(log2(7)).

```rust
fn build(batch: &MerkBatch<K>, ...) -> CostResult<Option<TreeNode>, Error> {
    let mid_index = batch.len() / 2;
    let (mid_key, mid_op) = &batch[mid_index];

    // Crea il nodo radice dall'elemento centrale
    let mid_tree = TreeNode::new(mid_key.clone(), value.clone(), None, feature_type)?;

    // Costruisci ricorsivamente i sotto-alberi sinistro e destro
    let left = Self::build(&batch[..mid_index], ...);
    let right = Self::build(&batch[mid_index + 1..], ...);

    // Collega i figli
    mid_tree.attach(true, left).attach(false, right)
}
```

Questo produce un albero con altezza ceil(log2(n)) — perfettamente bilanciato.

### Strategia 2: apply_sorted() — Fusione in un albero esistente

Quando l'albero ha gia dei dati, `apply_sorted()` utilizza la **ricerca binaria** per trovare dove ogni operazione del batch appartiene, poi applica ricorsivamente le operazioni ai sotto-alberi sinistro e destro:

Albero esistente con batch `[(B, Put), (F, Delete)]`:

Ricerca binaria: B < D (vai a sinistra), F > D (vai a destra).

**Prima:**
```mermaid
graph TD
    D0["D"] --- C0["C"]
    D0 --- E0["E"]
    E0 --- F0["F"]
    style D0 fill:#d4e6f1,stroke:#2980b9
```

**Dopo** l'applicazione del batch e il ribilanciamento:
```mermaid
graph TD
    D1["D"] --- B1["B"]
    D1 --- E1["E"]
    B1 --- C1["C"]
    style D1 fill:#d5f5e3,stroke:#27ae60
```

> B inserito come sotto-albero sinistro, F eliminato dal sotto-albero destro. `maybe_balance()` conferma bf(D) = 0.

```rust
fn apply_sorted(self, batch: &MerkBatch<K>, ...) -> CostResult<...> {
    let search = batch.binary_search_by(|(key, _)| key.cmp(self.tree().key()));

    match search {
        Ok(index) => {
            // La chiave corrisponde a questo nodo — applica l'operazione direttamente
            // (Put sostituisce il valore, Delete rimuove il nodo)
        }
        Err(mid) => {
            // Chiave non trovata — mid e il punto di divisione
            // Ricorsione su left_batch[..mid] e right_batch[mid..]
        }
    }

    self.recurse(batch, mid, exclusive, ...)
}
```

Il metodo `recurse` divide il batch e percorre sinistra e destra:

```rust
fn recurse(self, batch: &MerkBatch<K>, mid: usize, ...) {
    let left_batch = &batch[..mid];
    let right_batch = &batch[mid..];  // o mid+1 se esclusivo

    // Applica il batch sinistro al sotto-albero sinistro
    let tree = self.walk(true, |maybe_left| {
        Self::apply_to(maybe_left, left_batch, ...)
    });

    // Applica il batch destro al sotto-albero destro
    let tree = tree.walk(false, |maybe_right| {
        Self::apply_to(maybe_right, right_batch, ...)
    });

    // Ribilancia dopo le modifiche
    tree.maybe_balance(...)
}
```

### Rimozione dei nodi

Quando si elimina un nodo con due figli, Merk promuove il **nodo di bordo** dal sotto-albero piu alto. Cio minimizza la probabilita di necessitare ulteriori rotazioni:

**Prima** — eliminazione di D (ha due figli, altezza sotto-albero destro >= sinistro):

```mermaid
graph TD
    D["D ✗ elimina"]
    B0["B"]
    F0["F"]
    A0["A"]
    C0["C"]
    E0["E ← successore"]
    G0["G"]
    D --- B0
    D --- F0
    B0 --- A0
    B0 --- C0
    F0 --- E0
    F0 --- G0
    style D fill:#fadbd8,stroke:#e74c3c,stroke-width:3px
    style E0 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

**Dopo** — E (il piu a sinistra nel sotto-albero destro = successore in-order) promosso alla posizione di D:

```mermaid
graph TD
    E1["E"]
    B1["B"]
    F1["F"]
    A1["A"]
    C1["C"]
    G1["G"]
    E1 --- B1
    E1 --- F1
    B1 --- A1
    B1 --- C1
    F1 --- G1
    style E1 fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
```

> **Regola:** Se altezza sinistra > destra → promuovi il bordo destro del sotto-albero sinistro. Se altezza destra >= sinistra → promuovi il bordo sinistro del sotto-albero destro. Cio minimizza il ribilanciamento post-eliminazione.

```rust
pub fn remove(self, ...) -> CostResult<Option<Self>, Error> {
    let has_left = tree.link(true).is_some();
    let has_right = tree.link(false).is_some();
    let left = tree.child_height(true) > tree.child_height(false);

    if has_left && has_right {
        // Due figli: promuovi il bordo del figlio piu alto
        let (tree, tall_child) = self.detach_expect(left, ...);
        let (_, short_child) = tree.detach_expect(!left, ...);
        tall_child.promote_edge(!left, short_child, ...)
    } else if has_left || has_right {
        // Un figlio: promuovilo direttamente
        self.detach_expect(left, ...).1
    } else {
        // Nodo foglia: rimuovi semplicemente
        None
    }
}
```

---
