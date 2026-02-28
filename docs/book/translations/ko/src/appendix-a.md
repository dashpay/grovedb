# 부록 A: 완전한 엘리먼트 타입 참조

| 판별자 | 변형 | TreeType | 필드 | 비용 크기 | 목적 |
|---|---|---|---|---|---|
| 0 | `Item` | 해당 없음 | `(value, flags)` | 가변 | 기본 키-값 저장 |
| 1 | `Reference` | 해당 없음 | `(path, max_hop, flags)` | 가변 | 엘리먼트 간 링크 |
| 2 | `Tree` | 0 (NormalTree) | `(root_key, flags)` | TREE_COST_SIZE | 서브트리 컨테이너 |
| 3 | `SumItem` | 해당 없음 | `(value, flags)` | 가변 | 부모 합계에 기여 |
| 4 | `SumTree` | 1 (SumTree) | `(root_key, sum, flags)` | SUM_TREE_COST_SIZE | 하위 항목의 합계 유지 |
| 5 | `BigSumTree` | 4 (BigSumTree) | `(root_key, sum128, flags)` | BIG_SUM_TREE_COST_SIZE | 128비트 합계 트리 |
| 6 | `CountTree` | 2 (CountTree) | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | 엘리먼트 카운팅 트리 |
| 7 | `CountSumTree` | 3 (CountSumTree) | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | 카운트 + 합계 결합 |
| 8 | `ItemWithSumItem` | 해당 없음 | `(value, sum, flags)` | 가변 | 합계 기여가 있는 항목 |
| 9 | `ProvableCountTree` | 5 | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | 증명 가능한 카운트 트리 |
| 10 | `ProvableCountSumTree` | 6 | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | 증명 가능한 카운트 + 합계 |
| 11 | `CommitmentTree` | 7 | `(total_count: u64, chunk_power: u8, flags)` | 12 | ZK 호환 Sinsemilla + BulkAppendTree |
| 12 | `MmrTree` | 8 | `(mmr_size: u64, flags)` | 11 | 추가 전용 MMR 로그 |
| 13 | `BulkAppendTree` | 9 | `(total_count: u64, chunk_power: u8, flags)` | 12 | 고처리량 추가 전용 로그 |
| 14 | `DenseAppendOnlyFixedSizeTree` | 10 | `(count: u16, height: u8, flags)` | 6 | 조밀 고정 용량 머클 스토리지 |

**참고:**
- 판별자 11~14는 **비-Merk 트리**입니다: 데이터가 자식 Merk 서브트리 외부에 존재합니다
  - 네 가지 모두 비-Merk 데이터를 **데이터(data)** 컬럼에 저장합니다
  - `CommitmentTree`는 Sinsemilla 프론티어를 BulkAppendTree 항목과 같은 데이터 컬럼에 저장합니다 (키 `b"__ct_data__"`)
- 비-Merk 트리는 `root_key` 필드가 없습니다 -- 타입별 루트 해시가 `insert_subtree`를 통해 Merk 자식 해시로 흐릅니다
- `CommitmentTree`는 Sinsemilla 해싱(Pallas 곡선)을 사용합니다; 나머지는 모두 Blake3를 사용합니다
- 비-Merk 트리의 비용 동작은 `NormalTree`를 따릅니다 (BasicMerkNode, 집계 없음)
- `DenseAppendOnlyFixedSizeTree`의 count는 `u16`(최대 65,535)입니다; 높이는 1..=16으로 제한됩니다

---
