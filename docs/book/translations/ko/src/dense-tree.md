# DenseAppendOnlyFixedSizeTree -- 조밀 고정 용량 머클 스토리지

DenseAppendOnlyFixedSizeTree는 고정 높이의 완전 이진 트리로, **모든 노드** -- 내부 및 리프 모두 -- 가 데이터 값을 저장합니다. 위치는 레벨 순서(BFS)로 순차적으로 채워집니다: 루트가 먼저(위치 0), 그 다음 각 레벨에서 좌에서 우로. 중간 해시는 영속화되지 않습니다; 루트 해시는 리프에서 루트로 재귀적으로 해싱하여 즉석에서 재계산됩니다.

이 설계는 최대 용량이 미리 알려져 있고 O(1) 추가, O(1) 위치별 조회, 그리고 매 삽입 후 변경되는 컴팩트 32바이트 루트 해시 커밋먼트가 필요한 작고 제한된 데이터 구조에 이상적입니다.

## 트리 구조

높이 *h*의 트리는 `2^h - 1` 위치의 용량을 가집니다. 위치는 0 기반 레벨 순서 인덱싱을 사용합니다:

```text
Height 3 tree (capacity = 7):

              pos 0          ← root (level 0)
             /     \
          pos 1    pos 2     ← level 1
         /   \    /   \
       pos 3 pos 4 pos 5 pos 6  ← level 2 (leaves)

Navigation:
  left_child(i)  = 2i + 1
  right_child(i) = 2i + 2
  parent(i)      = (i - 1) / 2
  is_leaf(i)     = 2i + 1 >= capacity
```

값은 순차적으로 추가됩니다: 첫 번째 값은 위치 0(루트)에, 그 다음 위치 1, 2, 3 순서로 갑니다. 이것은 루트가 항상 데이터를 가지며, 트리가 레벨 순서로 채워짐을 의미합니다 -- 완전 이진 트리에서 가장 자연스러운 순회 순서입니다.

## 해시 계산

루트 해시는 별도로 저장되지 않습니다 -- 필요할 때마다 처음부터 재계산됩니다. 재귀 알고리즘은 채워진 위치만 방문합니다:

```text
hash(position, store):
  value = store.get(position)

  if position is unfilled (>= count):
    return [0; 32]                                    ← empty sentinel

  value_hash = blake3(value)
  left_hash  = hash(2 * position + 1, store)
  right_hash = hash(2 * position + 2, store)
  return blake3(value_hash || left_hash || right_hash)
```

**주요 속성:**
- 모든 노드 (리프 및 내부): `blake3(blake3(value) || H(left) || H(right))`
- 리프 노드: left_hash와 right_hash 모두 `[0; 32]` (채워지지 않은 자식)
- 채워지지 않은 위치: `[0u8; 32]` (영 해시)
- 빈 트리 (count = 0): `[0u8; 32]`

**리프/내부 도메인 분리 태그는 사용되지 않습니다.** 트리 구조(`height`와 `count`)는 부모 `Element::DenseAppendOnlyFixedSizeTree`에서 외부적으로 인증되며, Merk 계층 구조를 통해 흐릅니다. 검증자는 높이와 카운트로부터 어떤 위치가 리프이고 어떤 위치가 내부 노드인지 항상 정확히 알기 때문에, 공격자는 부모 인증 체인을 깨지 않고는 하나를 다른 것으로 대체할 수 없습니다.

이것은 루트 해시가 저장된 모든 값과 트리에서의 정확한 위치에 대한 커밋먼트를 인코딩함을 의미합니다. 어떤 값이 변경되면(변경 가능했다면) 루트까지의 모든 조상 해시를 통해 연쇄됩니다.

**해시 비용:** 루트 해시를 계산하면 채워진 모든 위치와 채워지지 않은 자식을 방문합니다. *n*개의 값을 가진 트리의 최악 경우는 O(*n*) blake3 호출입니다. 이것은 트리가 작고 제한된 용량(최대 높이 16, 최대 65,535 위치)을 위해 설계되었으므로 허용됩니다.

## 엘리먼트 변형

```rust
Element::DenseAppendOnlyFixedSizeTree(
    u16,                   // count — number of values stored (max 65,535)
    u8,                    // height — immutable after creation (1..=16)
    Option<ElementFlags>,  // flags — storage flags
)
```

| 필드 | 타입 | 설명 |
|---|---|---|
| `count` | `u16` | 지금까지 삽입된 값의 수 (최대 65,535) |
| `height` | `u8` | 트리 높이 (1..=16), 생성 후 불변 |
| `flags` | `Option<ElementFlags>` | 선택적 스토리지 플래그 |

루트 해시는 엘리먼트에 저장되지 않습니다 -- `insert_subtree`의 `subtree_root_hash` 매개변수를 통해 Merk 자식 해시로 흐릅니다.

**판별자:** 14 (ElementType), TreeType = 10

**비용 크기:** `DENSE_TREE_COST_SIZE = 6` 바이트 (2 count + 1 height + 1 판별자 + 2 오버헤드)

## 스토리지 레이아웃

MmrTree 및 BulkAppendTree와 마찬가지로, DenseAppendOnlyFixedSizeTree는 데이터를 **데이터(data)** 네임스페이스에 저장합니다(자식 Merk가 아님). 값은 빅엔디안 `u64`로 된 위치를 키로 사용합니다:

```text
Subtree path: blake3(parent_path || key)

Storage keys:
  [0, 0, 0, 0, 0, 0, 0, 0] → value at position 0 (root)
  [0, 0, 0, 0, 0, 0, 0, 1] → value at position 1
  [0, 0, 0, 0, 0, 0, 0, 2] → value at position 2
  ...
```

엘리먼트 자체(부모 Merk에 저장)는 `count`와 `height`를 담고 있습니다. 루트 해시는 Merk 자식 해시로 흐릅니다. 이것은 다음을 의미합니다:
- **루트 해시 읽기**에는 스토리지로부터의 재계산이 필요합니다 (O(n) 해싱)
- **위치로 값 읽기는 O(1)**입니다 -- 단일 스토리지 조회
- **삽입은 O(n) 해싱**입니다 -- 1회 스토리지 쓰기 + 전체 루트 해시 재계산

## 연산

### `dense_tree_insert(path, key, value, tx, grove_version)`

다음 사용 가능한 위치에 값을 추가합니다. `(root_hash, position)`을 반환합니다.

```text
Step 1: Read element, extract (count, height)
Step 2: Check capacity: if count >= 2^height - 1 → error
Step 3: Build subtree path, open storage context
Step 4: Write value to position = count
Step 5: Reconstruct DenseFixedSizedMerkleTree from state
Step 6: Call tree.insert(value, store) → (root_hash, position, hash_calls)
Step 7: Update element with new root_hash and count + 1
Step 8: Propagate changes up through Merk hierarchy
Step 9: Commit transaction
```

### `dense_tree_get(path, key, position, tx, grove_version)`

주어진 위치의 값을 조회합니다. position >= count이면 `None`을 반환합니다.

### `dense_tree_root_hash(path, key, tx, grove_version)`

엘리먼트에 저장된 루트 해시를 반환합니다. 이것은 가장 최근 삽입 중에 계산된 해시입니다 -- 재계산이 필요 없습니다.

### `dense_tree_count(path, key, tx, grove_version)`

저장된 값의 수(엘리먼트의 `count` 필드)를 반환합니다.

## 배치 연산

`GroveOp::DenseTreeInsert` 변형은 표준 GroveDB 배치 파이프라인을 통한 배치 삽입을 지원합니다:

```rust
let ops = vec![
    QualifiedGroveDbOp::dense_tree_insert_op(
        vec![b"parent".to_vec()],
        b"my_dense_tree".to_vec(),
        b"value_data".to_vec(),
    ),
];
db.apply_batch(ops, None, None, grove_version)?;
```

**전처리:** 모든 비-Merk 트리 타입과 마찬가지로, `DenseTreeInsert` 연산은 메인 배치 본문 실행 전에 전처리됩니다. `preprocess_dense_tree_ops` 메서드는:

1. 모든 `DenseTreeInsert` 연산을 `(path, key)`별로 그룹화
2. 각 그룹에 대해 순차적으로 삽입 실행 (엘리먼트 읽기, 각 값 삽입, 루트 해시 업데이트)
3. 각 그룹을 최종 `root_hash`와 `count`를 표준 전파 기계를 통해 전달하는 `ReplaceNonMerkTreeRoot` 연산으로 변환

단일 배치 내에서 같은 조밀 트리에 대한 다중 삽입이 지원됩니다 -- 순서대로 처리되며 일관성 검사가 이 연산 타입에 대해 중복 키를 허용합니다.

**전파:** 루트 해시와 카운트는 `ReplaceNonMerkTreeRoot`의 `NonMerkTreeMeta::DenseTree` 변형을 통해 흐르며, MmrTree 및 BulkAppendTree와 같은 패턴을 따릅니다.

## 증명

DenseAppendOnlyFixedSizeTree는 `ProofBytes::DenseTree` 변형을 통해 **V1 하위 쿼리 증명**을 지원합니다. 개별 위치는 조상 값과 형제 서브트리 해시를 전달하는 포함 증명을 사용하여 트리의 루트 해시에 대해 증명할 수 있습니다.

### 인증 경로 구조

내부 노드가 (자식 해시뿐만 아니라) **자신의 값**도 해싱하므로, 인증 경로는 표준 머클 트리와 다릅니다. 위치 `p`의 리프를 검증하려면 검증자가 다음을 필요로 합니다:

1. **리프 값** (증명된 항목)
2. **조상 값 해시** -- `p`에서 루트까지의 경로상 모든 내부 노드의 32바이트 해시(전체 값이 아닌 해시만)
3. **형제 서브트리 해시** -- 경로에 없는 모든 자식의 해시

모든 노드가 `blake3(H(value) || H(left) || H(right))`를 사용하므로(도메인 태그 없음), 증명은 조상에 대해 32바이트 값 해시만 전달합니다 -- 전체 값이 아닙니다. 이것은 개별 값이 얼마나 크든 증명을 컴팩트하게 유지합니다.

```rust
pub struct DenseTreeProof {
    pub entries: Vec<(u16, Vec<u8>)>,            // proved (position, value) pairs
    pub node_value_hashes: Vec<(u16, [u8; 32])>, // ancestor value hashes on the auth path
    pub node_hashes: Vec<(u16, [u8; 32])>,       // precomputed sibling subtree hashes
}
```

> **참고:** `height`와 `count`는 증명 구조체에 없습니다 -- 검증자는 Merk 계층 구조에 의해 인증된 부모 엘리먼트에서 가져옵니다.

### 구체적 예시

height=3, capacity=7, count=5인 트리에서 위치 4 증명:

```text
        0
       / \
      1   2
     / \ / \
    3  4 5  6
```

4에서 루트까지의 경로: `4 → 1 → 0`. 확장 집합: `{0, 1, 4}`.

증명에 포함되는 것:
- **entries**: `[(4, value[4])]` -- 증명된 위치
- **node_value_hashes**: `[(0, H(value[0])), (1, H(value[1]))]` -- 조상 값 해시 (각 32바이트, 전체 값이 아님)
- **node_hashes**: `[(2, H(subtree_2)), (3, H(node_3))]` -- 경로에 없는 형제

검증은 루트 해시를 상향으로 재계산합니다:
1. `H(4) = blake3(blake3(value[4]) || [0;32] || [0;32])` -- 리프 (자식이 채워지지 않음)
2. `H(3)` -- `node_hashes`에서
3. `H(1) = blake3(H(value[1]) || H(3) || H(4))` -- 내부 노드, `node_value_hashes`의 값 해시 사용
4. `H(2)` -- `node_hashes`에서
5. `H(0) = blake3(H(value[0]) || H(1) || H(2))` -- 루트, `node_value_hashes`의 값 해시 사용
6. `H(0)`을 예상 루트 해시와 비교

### 다중 위치 증명

여러 위치를 증명할 때, 확장 집합이 겹치는 인증 경로를 병합합니다. 공유 조상은 한 번만 포함되므로, 다중 위치 증명이 독립적인 단일 위치 증명보다 더 컴팩트합니다.

### V0 제한

V0 증명은 조밀 트리로 하강할 수 없습니다. V0 쿼리가 하위 쿼리를 가진 `DenseAppendOnlyFixedSizeTree`와 매칭되면, 시스템은 `prove_query_v1` 사용을 안내하는 `Error::NotSupported`를 반환합니다.

### 쿼리 키 인코딩

조밀 트리 위치는 u64을 사용하는 MmrTree 및 BulkAppendTree와 달리 **빅엔디안 u16** (2바이트) 쿼리 키로 인코딩됩니다. 모든 표준 `QueryItem` 범위 타입이 지원됩니다.

## 다른 비-Merk 트리와의 비교

| | DenseTree | MmrTree | BulkAppendTree | CommitmentTree |
|---|---|---|---|---|
| **엘리먼트 판별자** | 14 | 12 | 13 | 11 |
| **TreeType** | 10 | 8 | 9 | 7 |
| **용량** | 고정 (`2^h - 1`, 최대 65,535) | 무제한 | 무제한 | 무제한 |
| **데이터 모델** | 모든 위치에 값 저장 | 리프만 | 조밀 트리 버퍼 + 청크 | 리프만 |
| **엘리먼트에 해시?** | 아니오 (자식 해시로 흐름) | 아니오 (자식 해시로 흐름) | 아니오 (자식 해시로 흐름) | 아니오 (자식 해시로 흐름) |
| **삽입 비용 (해싱)** | O(n) blake3 | O(1) 상각 | O(1) 상각 | ~33 Sinsemilla |
| **비용 크기** | 6 바이트 | 11 바이트 | 12 바이트 | 12 바이트 |
| **증명 지원** | V1 (Dense) | V1 (MMR) | V1 (Bulk) | V1 (CommitmentTree) |
| **적합 용도** | 작은 고정 구조 | 이벤트 로그 | 고처리량 로그 | ZK 커밋먼트 |

**DenseAppendOnlyFixedSizeTree를 선택할 때:**
- 최대 항목 수가 생성 시점에 알려져 있을 때
- 모든 위치(내부 노드 포함)에 데이터를 저장해야 할 때
- 무제한 성장 없이 가능한 가장 단순한 데이터 모델을 원할 때
- O(n) 루트 해시 재계산이 허용될 때 (작은 트리 높이)

**선택하지 않을 때:**
- 무제한 용량이 필요한 경우 -> MmrTree 또는 BulkAppendTree 사용
- ZK 호환성이 필요한 경우 -> CommitmentTree 사용

## 사용 예시

```rust
use grovedb::Element;
use grovedb_version::version::GroveVersion;

let grove_version = GroveVersion::latest();

// Create a dense tree of height 4 (capacity = 15 values)
db.insert(
    &[b"state"],
    b"validator_slots",
    Element::empty_dense_tree(4),
    None,
    None,
    grove_version,
)?;

// Append values — positions filled 0, 1, 2, ...
let (root_hash, pos) = db.dense_tree_insert(
    &[b"state"],
    b"validator_slots",
    validator_pubkey.to_vec(),
    None,
    grove_version,
)?;
// pos == 0, root_hash = blake3(validator_pubkey)

// Read back by position
let value = db.dense_tree_get(
    &[b"state"],
    b"validator_slots",
    0,     // position
    None,
    grove_version,
)?;
assert_eq!(value, Some(validator_pubkey.to_vec()));

// Query metadata
let count = db.dense_tree_count(&[b"state"], b"validator_slots", None, grove_version)?;
let hash = db.dense_tree_root_hash(&[b"state"], b"validator_slots", None, grove_version)?;
```

## 구현 파일

| 파일 | 내용 |
|---|---|
| `grovedb-dense-fixed-sized-merkle-tree/src/lib.rs` | `DenseTreeStore` 트레이트, `DenseFixedSizedMerkleTree` 구조체, 재귀 해시 |
| `grovedb-dense-fixed-sized-merkle-tree/src/proof.rs` | `DenseTreeProof` 구조체, `generate()`, `encode_to_vec()`, `decode_from_slice()` |
| `grovedb-dense-fixed-sized-merkle-tree/src/verify.rs` | `DenseTreeProof::verify()` -- 순수 함수, 스토리지 불필요 |
| `grovedb-element/src/element/mod.rs` | `Element::DenseAppendOnlyFixedSizeTree` (판별자 14) |
| `grovedb-element/src/element/constructor.rs` | `empty_dense_tree()`, `new_dense_tree()` |
| `merk/src/tree_type/mod.rs` | `TreeType::DenseAppendOnlyFixedSizeTree = 10` |
| `merk/src/tree_type/costs.rs` | `DENSE_TREE_COST_SIZE = 6` |
| `grovedb/src/operations/dense_tree.rs` | GroveDB 연산, `AuxDenseTreeStore`, 배치 전처리 |
| `grovedb/src/operations/proof/generate.rs` | `generate_dense_tree_layer_proof()`, `query_items_to_positions()` |
| `grovedb/src/operations/proof/verify.rs` | `verify_dense_tree_lower_layer()` |
| `grovedb/src/operations/proof/mod.rs` | `ProofBytes::DenseTree` 변형 |
| `grovedb/src/batch/estimated_costs/average_case_costs.rs` | 평균 케이스 비용 모델 |
| `grovedb/src/batch/estimated_costs/worst_case_costs.rs` | 최악 케이스 비용 모델 |
| `grovedb/src/tests/dense_tree_tests.rs` | 22개 통합 테스트 |

---
