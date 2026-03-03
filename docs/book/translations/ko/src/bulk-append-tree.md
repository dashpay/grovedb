# BulkAppendTree -- 고처리량 추가 전용 스토리지

BulkAppendTree는 특정 엔지니어링 과제에 대한 GroveDB의 해답입니다: 효율적인 범위 증명을 지원하고, 쓰기당 해싱을 최소화하며, CDN 배포에 적합한 불변 청크 스냅샷을 생성하는 고처리량 추가 전용 로그를 어떻게 구축하는가?

MmrTree(13장)가 개별 리프 증명에 이상적인 반면, BulkAppendTree는 블록당 수천 개의 값이 도착하고 클라이언트가 데이터 범위를 가져와 동기화해야 하는 워크로드를 위해 설계되었습니다. 이것은 **2단계 아키텍처**로 달성합니다: 들어오는 추가를 흡수하는 조밀 머클 트리 버퍼와, 확정된 청크 루트를 기록하는 청크 레벨 MMR입니다.

## 2단계 아키텍처

```text
┌────────────────────────────────────────────────────────────────┐
│                      BulkAppendTree                            │
│                                                                │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  Chunk MMR                                               │  │
│  │  ┌────┐ ┌────┐ ┌────┐ ┌────┐                            │  │
│  │  │ R0 │ │ R1 │ │ R2 │ │ H  │ ← Dense Merkle roots      │  │
│  │  └────┘ └────┘ └────┘ └────┘   of each chunk blob       │  │
│  │                     peak hashes bagged together = MMR root│  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  Buffer (DenseFixedSizedMerkleTree, capacity = 2^h - 1) │  │
│  │  ┌───┐ ┌───┐ ┌───┐                                      │  │
│  │  │v_0│ │v_1│ │v_2│ ... (fills in level-order)           │  │
│  │  └───┘ └───┘ └───┘                                      │  │
│  │  dense_tree_root = recomputed root hash of dense tree     │  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                │
│  state_root = blake3("bulk_state" || mmr_root || dense_tree_root) │
│                                                                │
└────────────────────────────────────────────────────────────────┘
```

**레벨 1 -- 버퍼.** 들어오는 값은 `DenseFixedSizedMerkleTree`(16장 참조)에 기록됩니다. 버퍼 용량은 `2^height - 1` 위치입니다. 조밀 트리의 루트 해시(`dense_tree_root`)는 매 삽입 후 업데이트됩니다.

**레벨 2 -- 청크 MMR.** 버퍼가 가득 차면(`chunk_size` 항목에 도달), 모든 항목이 불변 **청크 블롭(chunk blob)**으로 직렬화되고, 해당 항목들에 대한 조밀 머클 루트가 계산되며, 그 루트가 청크 MMR에 리프로 추가됩니다. 그 후 버퍼가 비워집니다.

**상태 루트(state root)**는 두 레벨을 단일 32바이트 커밋먼트로 결합하며, 부모 Merk 트리가 항상 최신 상태를 반영하도록 매 추가마다 변경됩니다.

## 값이 버퍼를 채우는 방식

`append()` 호출은 다음 순서를 따릅니다:

```text
Step 1: Write value to dense tree buffer at next position
        dense_tree.insert(value, store)

Step 2: Increment total_count
        total_count += 1

Step 3: Check if buffer is full (dense tree at capacity)
        if dense_tree.count() == capacity:
            → trigger compaction (§14.3)

Step 4: Compute new state root (+1 blake3 call)
        dense_tree_root = dense_tree.root_hash(store)
        state_root = blake3("bulk_state" || mmr_root || dense_tree_root)
```

**버퍼는 DenseFixedSizedMerkleTree입니다**(16장 참조). 그 루트 해시는 매 삽입 후 변경되어, 현재 모든 버퍼 항목에 대한 커밋먼트를 제공합니다. 이 루트 해시가 상태 루트 계산에 흘러들어갑니다.

## 청크 압축(Compaction)

버퍼가 가득 차면(`chunk_size` 항목에 도달), 압축이 자동으로 발동합니다:

```text
Compaction Steps:
─────────────────
1. Read all chunk_size buffer entries

2. Compute dense Merkle root
   - Hash each entry: leaf[i] = blake3(entry[i])
   - Build complete binary tree bottom-up
   - Extract root hash
   Hash cost: chunk_size + (chunk_size - 1) = 2 * chunk_size - 1

3. Serialize entries into chunk blob
   - Auto-selects fixed-size or variable-size format (§14.6)
   - Store as: store.put(chunk_key(chunk_index), blob)

4. Append dense Merkle root to chunk MMR
   - MMR push with merge cascade (see Chapter 13)
   Hash cost: ~2 amortized (trailing_ones pattern)

5. Reset the dense tree (clear all buffer entries from storage)
   - Dense tree count reset to 0
```

압축 후, 청크 블롭은 **영구적으로 불변**합니다 -- 다시는 변경되지 않습니다. 이것은 청크 블롭을 CDN 캐싱, 클라이언트 동기화, 아카이브 스토리지에 이상적으로 만듭니다.

**예시: chunk_power=2(chunk_size=4)로 4회 추가**

```text
Append v_0: dense_tree=[v_0],       dense_root=H(v_0), total=1
Append v_1: dense_tree=[v_0,v_1],   dense_root=H(v_0,v_1), total=2
Append v_2: dense_tree=[v_0..v_2],  dense_root=H(v_0..v_2), total=3
Append v_3: dense_tree=[v_0..v_3],  dense_root=H(v_0..v_3), total=4
  → COMPACTION:
    chunk_blob_0 = serialize([v_0, v_1, v_2, v_3])
    dense_root_0 = dense_merkle_root(v_0..v_3)
    mmr.push(dense_root_0)
    dense tree cleared (count=0)

Append v_4: dense_tree=[v_4],       dense_root=H(v_4), total=5
  → state_root = blake3("bulk_state" || mmr_root || dense_root)
```

## 상태 루트

상태 루트는 두 레벨을 하나의 해시로 묶습니다:

```rust
fn compute_state_root(
    mmr_root: &[u8; 32],         // Chunk MMR root (or [0;32] if empty)
    dense_tree_root: &[u8; 32],  // Root hash of current buffer (dense tree)
) -> [u8; 32] {
    blake3("bulk_state" || mmr_root || dense_tree_root)
}
```

`total_count`와 `chunk_power`는 상태 루트에 **포함되지 않습니다**. 이미 Merk 값 해시에 의해 인증되기 때문입니다 -- 부모 Merk 노드에 저장된 직렬화된 `Element`의 필드입니다. 상태 루트는 데이터 레벨 커밋먼트(`mmr_root`와 `dense_tree_root`)만 캡처합니다. 이것이 Merk 자식 해시로 흐르고 GroveDB 루트 해시까지 전파되는 해시입니다.

## 조밀 머클 루트

청크가 압축되면, 항목들은 단일 32바이트 커밋먼트가 필요합니다. BulkAppendTree는 **조밀(완전) 이진 머클 트리**를 사용합니다:

```text
Given entries [e_0, e_1, e_2, e_3]:

Level 0 (leaves):  blake3(e_0)  blake3(e_1)  blake3(e_2)  blake3(e_3)
                      \__________/              \__________/
Level 1:           blake3(h_0 || h_1)       blake3(h_2 || h_3)
                          \____________________/
Level 2 (root):    blake3(h_01 || h_23)  ← this is the dense Merkle root
```

`chunk_size`는 항상 2의 거듭제곱이므로(설계상: `1u32 << chunk_power`), 트리는 항상 완전합니다(패딩이나 더미 리프가 필요 없음). 해시 수는 정확히 `2 * chunk_size - 1`입니다:
- `chunk_size`개 리프 해시(항목당 하나)
- `chunk_size - 1`개 내부 노드 해시

조밀 머클 루트 구현은 `grovedb-mmr/src/dense_merkle.rs`에 있으며 두 함수를 제공합니다:
- `compute_dense_merkle_root(hashes)` -- 사전 해싱된 리프에서
- `compute_dense_merkle_root_from_values(values)` -- 값을 먼저 해싱한 다음 트리 구축

## 청크 블롭 직렬화

청크 블롭은 압축에 의해 생성되는 불변 아카이브입니다. 직렬화기는 항목 크기에 따라 가장 컴팩트한 와이어 형식을 자동 선택합니다:

**고정 크기 형식** (플래그 `0x01`) -- 모든 항목이 같은 길이일 때:

```text
┌──────┬──────────┬─────────────┬─────────┬─────────┬─────────┐
│ 0x01 │ count    │ entry_size  │ entry_0 │ entry_1 │ ...     │
│ 1B   │ 4B (BE)  │ 4B (BE)     │ N bytes │ N bytes │         │
└──────┴──────────┴─────────────┴─────────┴─────────┴─────────┘
Total: 1 + 4 + 4 + (count × entry_size) bytes
```

**가변 크기 형식** (플래그 `0x00`) -- 항목의 길이가 다를 때:

```text
┌──────┬──────────┬─────────┬──────────┬─────────┬─────────────┐
│ 0x00 │ len_0    │ entry_0 │ len_1    │ entry_1 │ ...         │
│ 1B   │ 4B (BE)  │ N bytes │ 4B (BE)  │ M bytes │             │
└──────┴──────────┴─────────┴──────────┴─────────┴─────────────┘
Total: 1 + Σ(4 + len_i) bytes
```

고정 크기 형식은 가변 크기와 비교하여 항목당 4바이트를 절약하며, 균일한 크기 데이터(예: 32바이트 해시 커밋먼트)의 대량 청크에서 상당히 누적됩니다.
각 32바이트의 1024개 항목의 경우:
- 고정: `1 + 4 + 4 + 32768 = 32,777 bytes`
- 가변: `1 + 1024 × (4 + 32) = 36,865 bytes`
- 절약: ~11%

## 스토리지 키 레이아웃

모든 BulkAppendTree 데이터는 단일 문자 접두사로 키가 지정된 **데이터(data)** 네임스페이스에 있습니다:

| 키 패턴 | 형식 | 크기 | 목적 |
|---|---|---|---|
| `M` | 1 바이트 | 1B | 메타데이터 키 |
| `b` + `{index}` | `b` + u32 BE | 5B | 인덱스의 버퍼 항목 |
| `e` + `{index}` | `e` + u64 BE | 9B | 인덱스의 청크 블롭 |
| `m` + `{pos}` | `m` + u64 BE | 9B | 위치의 MMR 노드 |

**메타데이터**는 `mmr_size`(8바이트 BE)를 저장합니다. `total_count`와 `chunk_power`는 데이터 네임스페이스 메타데이터가 아닌 엘리먼트 자체(부모 Merk에)에 저장됩니다. 이러한 분리는 데이터 스토리지 컨텍스트를 열지 않고도 카운트를 간단한 엘리먼트 조회로 읽을 수 있게 합니다.

버퍼 키는 u32 인덱스(0~`chunk_size - 1`)를 사용합니다. 버퍼 용량이 `chunk_size`(u32, `1u32 << chunk_power`로 계산)에 의해 제한되기 때문입니다. 청크 키는 u64 인덱스를 사용합니다. 완료된 청크 수가 무한히 증가할 수 있기 때문입니다.

## BulkAppendTree 구조체

```rust
pub struct BulkAppendTree<S> {
    pub total_count: u64,                        // Total values ever appended
    pub dense_tree: DenseFixedSizedMerkleTree<S>, // The buffer (dense tree)
}
```

버퍼는 `DenseFixedSizedMerkleTree`입니다 -- 그 루트 해시가 `dense_tree_root`입니다.

**접근자:**
- `capacity() -> u16`: `dense_tree.capacity()` (= `2^height - 1`)
- `epoch_size() -> u64`: `capacity + 1` (= `2^height`, 청크당 항목 수)
- `height() -> u8`: `dense_tree.height()`

**도출 값** (저장되지 않음):

| 값 | 공식 |
|---|---|
| `chunk_count` | `total_count / epoch_size` |
| `buffer_count` | `dense_tree.count()` |
| `mmr_size` | `leaf_count_to_mmr_size(chunk_count)` |

## GroveDB 연산

BulkAppendTree는 `grovedb/src/operations/bulk_append_tree.rs`에 정의된 6개 연산을 통해 GroveDB와 통합됩니다:

### bulk_append

주요 변경 연산입니다. 표준 GroveDB 비-Merk 스토리지 패턴을 따릅니다:

```text
1. Validate element is BulkAppendTree
2. Open data storage context
3. Load tree from store
4. Append value (may trigger compaction)
5. Update element in parent Merk with new state_root + total_count
6. Propagate changes up through Merk hierarchy
7. Commit transaction
```

`AuxBulkStore` 어댑터는 GroveDB의 `get_aux`/`put_aux`/`delete_aux` 호출을 래핑하고 비용 추적을 위해 `RefCell`에 `OperationCost`를 누적합니다. 추가 연산의 해시 비용은 `cost.hash_node_calls`에 추가됩니다.

### 읽기 연산

| 연산 | 반환하는 것 | Aux 스토리지? |
|---|---|---|
| `bulk_get_value(path, key, position)` | 전역 위치의 값 | 예 -- 청크 블롭 또는 버퍼에서 읽기 |
| `bulk_get_chunk(path, key, chunk_index)` | 원시 청크 블롭 | 예 -- 청크 키 읽기 |
| `bulk_get_buffer(path, key)` | 현재 모든 버퍼 항목 | 예 -- 버퍼 키 읽기 |
| `bulk_count(path, key)` | 총 카운트 (u64) | 아니오 -- 엘리먼트에서 읽기 |
| `bulk_chunk_count(path, key)` | 완료된 청크 수 (u64) | 아니오 -- 엘리먼트에서 계산 |

`get_value` 연산은 위치에 따라 투명하게 라우팅합니다:

```text
if position < completed_chunks × chunk_size:
    chunk_idx = position / chunk_size
    intra_idx = position % chunk_size
    → read chunk blob, deserialize, return entry[intra_idx]
else:
    buffer_idx = position % chunk_size
    → read buffer_key(buffer_idx)
```

## 배치 연산과 전처리

BulkAppendTree는 `GroveOp::BulkAppend` 변형을 통해 배치 연산을 지원합니다. `execute_ops_on_path`는 데이터 스토리지 컨텍스트에 접근할 수 없으므로, 모든 BulkAppend 연산은 `apply_body` 전에 전처리되어야 합니다.

전처리 파이프라인:

```text
Input: [BulkAppend{v1}, Insert{...}, BulkAppend{v2}, BulkAppend{v3}]
                                     ↑ same (path,key) as v1

Step 1: Group BulkAppend ops by (path, key)
        group_1: [v1, v2, v3]

Step 2: For each group:
        a. Read existing element → get (total_count, chunk_power)
        b. Open transactional storage context
        c. Load BulkAppendTree from store
        d. Load existing buffer into memory (Vec<Vec<u8>>)
        e. For each value:
           tree.append_with_mem_buffer(store, value, &mut mem_buffer)
        f. Save metadata
        g. Compute final state_root

Step 3: Replace all BulkAppend ops with one ReplaceNonMerkTreeRoot per group
        carrying: hash=state_root, meta=BulkAppendTree{total_count, chunk_power}

Output: [ReplaceNonMerkTreeRoot{...}, Insert{...}]
```

`append_with_mem_buffer` 변형은 쓰기 후 읽기 문제를 방지합니다: 버퍼 항목이 메모리의 `Vec<Vec<u8>>`에서 추적되므로, 트랜잭션 스토리지가 아직 커밋되지 않았더라도 압축이 항목을 읽을 수 있습니다.

## BulkStore 트레이트

```rust
pub trait BulkStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String>;
    fn delete(&self, key: &[u8]) -> Result<(), String>;
}
```

메서드는 `&self`(`&mut self`가 아님)를 받습니다. 쓰기가 배치를 통해 이루어지는 GroveDB의 내부 가변성 패턴과 일치시키기 위해서입니다. GroveDB 통합은 `StorageContext`를 래핑하고 `OperationCost`를 누적하는 `AuxBulkStore`를 통해 이를 구현합니다.

`MmrAdapter`는 `BulkStore`를 ckb MMR의 `MMRStoreReadOps`/`MMRStoreWriteOps` 트레이트에 브리지하며, 쓰기 후 읽기 정확성을 위한 쓰기 투과 캐시를 추가합니다.

## 증명 생성

BulkAppendTree 증명은 위치에 대한 **범위 쿼리**를 지원합니다. 증명 구조는 특정 데이터가 트리에 존재함을 상태 없는 검증자가 확인하는 데 필요한 모든 것을 캡처합니다:

```rust
pub struct BulkAppendTreeProof {
    pub chunk_power: u8,
    pub total_count: u64,
    pub chunk_blobs: Vec<(u64, Vec<u8>)>,         // Full chunk blobs
    pub chunk_mmr_size: u64,
    pub chunk_mmr_proof_items: Vec<[u8; 32]>,      // MMR sibling hashes
    pub chunk_mmr_leaves: Vec<(u64, [u8; 32])>,    // (leaf_idx, dense_root)
    pub buffer_entries: Vec<Vec<u8>>,               // ALL buffer entries
    pub chunk_mmr_root: [u8; 32],
}
```

범위 `[start, end)`에 대한 **생성 단계** (`chunk_size = 1u32 << chunk_power` 사용):

```text
1. Determine overlapping chunks
   first_chunk = start / chunk_size
   last_chunk  = min((end-1) / chunk_size, completed_chunks - 1)

2. Read chunk blobs for overlapping chunks
   For each chunk_idx in [first_chunk, last_chunk]:
     chunk_blobs.push((chunk_idx, store.get(chunk_key(idx))))

3. Compute dense Merkle root for each chunk blob
   For each blob:
     deserialize → values
     dense_root = compute_dense_merkle_root_from_values(values)
     chunk_mmr_leaves.push((chunk_idx, dense_root))

4. Generate MMR proof for those chunk positions
   positions = chunk_indices.map(|idx| leaf_to_pos(idx))
   proof = mmr.gen_proof(positions)
   chunk_mmr_proof_items = proof.proof_items().map(|n| n.hash)

5. Get chunk MMR root

6. Read ALL buffer entries (bounded by chunk_size)
   for i in 0..buffer_count:
     buffer_entries.push(store.get(buffer_key(i)))
```

**왜 모든 버퍼 항목을 포함하는가?** 버퍼는 모든 항목에 커밋하는 루트 해시를 가진 조밀 머클 트리입니다. 검증자는 `dense_tree_root`를 검증하기 위해 모든 항목에서 트리를 재구축해야 합니다. 버퍼가 `capacity`(최대 65,535개 항목)로 제한되므로, 이것은 합리적인 비용입니다.

## 증명 검증

검증은 순수 함수입니다 -- 데이터베이스 접근이 필요 없습니다. 다섯 가지 검사를 수행합니다:

```text
Step 0: Metadata consistency
        - chunk_power <= 31
        - buffer_entries.len() == total_count % chunk_size
        - MMR leaf count == completed_chunks

Step 1: Chunk blob integrity
        For each (chunk_idx, blob):
          values = deserialize(blob)
          computed_root = dense_merkle_root(values)
          assert computed_root == chunk_mmr_leaves[chunk_idx]

Step 2: Chunk MMR proof
        Reconstruct MmrNode leaves and proof items
        proof.verify(chunk_mmr_root, leaves) == true

Step 3: Buffer (dense tree) integrity
        Rebuild DenseFixedSizedMerkleTree from buffer_entries
        dense_tree_root = compute root hash of rebuilt tree

Step 4: State root
        computed = blake3("bulk_state" || chunk_mmr_root || dense_tree_root)
        assert computed == expected_state_root
```

검증이 성공하면, `BulkAppendTreeProofResult`는 검증된 청크 블롭과 버퍼 항목에서 특정 값을 추출하는 `values_in_range(start, end)` 메서드를 제공합니다.

## GroveDB 루트 해시와의 연결

BulkAppendTree는 **비-Merk 트리**입니다 -- 데이터를 자식 Merk 서브트리가 아닌 데이터 네임스페이스에 저장합니다. 부모 Merk에서 엘리먼트는 다음과 같이 저장됩니다:

```text
Element::BulkAppendTree(total_count, chunk_power, flags)
```

상태 루트는 Merk 자식 해시로 흐릅니다. 부모 Merk 노드 해시는:

```text
combine_hash(value_hash(element_bytes), state_root)
```

`state_root`는 Merk 자식 해시로 흐릅니다(`insert_subtree`의 `subtree_root_hash` 매개변수를 통해). 상태 루트의 어떤 변경이든 GroveDB Merk 계층 구조를 통해 루트 해시까지 전파됩니다.

V1 증명(9.6절)에서, 부모 Merk 증명은 엘리먼트 바이트와 자식 해시 바인딩을 증명하고, `BulkAppendTreeProof`는 쿼리된 데이터가 자식 해시로 사용된 `state_root`와 일관성이 있음을 증명합니다.

## 비용 추적

각 연산의 해시 비용은 명시적으로 추적됩니다:

| 연산 | Blake3 호출 | 비고 |
|---|---|---|
| 단일 추가 (압축 없음) | 3 | 버퍼 해시 체인 2 + 상태 루트 1 |
| 단일 추가 (압축 포함) | 3 + 2C - 1 + ~2 | 체인 + 조밀 머클(C=chunk_size) + MMR push + 상태 루트 |
| 청크에서 `get_value` | 0 | 순수 역직렬화, 해싱 없음 |
| 버퍼에서 `get_value` | 0 | 직접 키 조회 |
| 증명 생성 | 청크 수에 따라 | 청크당 조밀 머클 루트 + MMR 증명 |
| 증명 검증 | 2C·K - K + B·2 + 1 | K개 청크, B개 버퍼 항목, C chunk_size |

**추가당 상각 비용**: chunk_size=1024(chunk_power=10)의 경우, ~2047 해시의 압축 오버헤드(조밀 머클 루트)는 1024회 추가에 걸쳐 상각되어, 추가당 ~2 해시를 추가합니다. 추가당 3회 해시와 결합하면, 상각 총계는 **추가당 ~5 blake3 호출** -- 암호학적으로 인증된 구조에 대해 매우 효율적입니다.

## MmrTree와의 비교

| | BulkAppendTree | MmrTree |
|---|---|---|
| **아키텍처** | 2단계 (버퍼 + 청크 MMR) | 단일 MMR |
| **추가당 해시 비용** | 3 (+ 압축 상각 ~2) | ~2 |
| **증명 세분화** | 위치에 대한 범위 쿼리 | 개별 리프 증명 |
| **불변 스냅샷** | 예 (청크 블롭) | 아니오 |
| **CDN 친화적** | 예 (청크 블롭 캐시 가능) | 아니오 |
| **버퍼 항목** | 예 (증명에 모두 필요) | 해당 없음 |
| **적합 용도** | 고처리량 로그, 대량 동기화 | 이벤트 로그, 개별 조회 |
| **엘리먼트 판별자** | 13 | 12 |
| **TreeType** | 9 | 8 |

최소한의 오버헤드로 개별 리프 증명이 필요한 경우 MmrTree를 선택하세요. 범위 쿼리, 대량 동기화, 청크 기반 스냅샷이 필요한 경우 BulkAppendTree를 선택하세요.

## 구현 파일

| 파일 | 목적 |
|------|---------|
| `grovedb-bulk-append-tree/src/lib.rs` | 크레이트 루트, 재내보내기 |
| `grovedb-bulk-append-tree/src/tree/mod.rs` | `BulkAppendTree` 구조체, 상태 접근자, 메타데이터 영속화 |
| `grovedb-bulk-append-tree/src/tree/append.rs` | `append()`, `append_with_mem_buffer()`, `compact_entries()` |
| `grovedb-bulk-append-tree/src/tree/hash.rs` | `compute_state_root` |
| `grovedb-bulk-append-tree/src/tree/keys.rs` | `META_KEY`, `buffer_key`, `chunk_key`, `mmr_node_key` |
| `grovedb-bulk-append-tree/src/tree/query.rs` | `get_value`, `get_chunk`, `get_buffer` |
| `grovedb-bulk-append-tree/src/tree/mmr_adapter.rs` | 쓰기 투과 캐시를 가진 `MmrAdapter` |
| `grovedb-bulk-append-tree/src/chunk.rs` | 청크 블롭 직렬화 (고정 + 가변 형식) |
| `grovedb-bulk-append-tree/src/proof.rs` | `BulkAppendTreeProof` 생성 및 검증 |
| `grovedb-bulk-append-tree/src/store.rs` | `BulkStore` 트레이트 |
| `grovedb-bulk-append-tree/src/error.rs` | `BulkAppendError` 열거형 |
| `grovedb/src/operations/bulk_append_tree.rs` | GroveDB 연산, `AuxBulkStore`, 배치 전처리 |
| `grovedb/src/tests/bulk_append_tree_tests.rs` | 27개 통합 테스트 |

---
