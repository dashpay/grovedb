use proptest::proptest;

use crate::{MMR, MmrNode, mem_store::MemStore};

/// Create an MmrNode leaf from an integer (for test convenience).
fn leaf_from_u32(i: u32) -> MmrNode {
    MmrNode::leaf(i.to_le_bytes().to_vec())
}

proptest! {
    #[test]
    fn test_incremental(start in 1u32..500, steps in 1usize..50, turns in 10usize..20) {
        test_incremental_with_params(start, steps, turns);
    }
}

fn test_incremental_with_params(start: u32, steps: usize, turns: usize) {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);

    let mut curr = 0;

    let _positions: Vec<u64> = (0u32..start)
        .map(|_| {
            let pos = mmr.push(leaf_from_u32(curr)).unwrap().expect("push");
            curr += 1;
            pos
        })
        .collect();
    mmr.commit().unwrap().expect("commit changes");

    for turn in 0..turns {
        let prev_root = mmr.get_root().unwrap().expect("get root");
        let (positions, leaves) = (0..steps).fold(
            (Vec::new(), Vec::new()),
            |(mut positions, mut leaves), _| {
                let leaf = leaf_from_u32(curr);
                let pos = mmr.push(leaf.clone()).unwrap().expect("push");
                curr += 1;
                positions.push(pos);
                leaves.push(leaf);
                (positions, leaves)
            },
        );
        mmr.commit().unwrap().expect("commit changes");
        let proof = mmr.gen_proof(positions).unwrap().expect("gen proof");
        let root = mmr.get_root().unwrap().expect("get root");
        let result = proof
            .verify_incremental(root, prev_root, leaves)
            .expect("verify_incremental");
        assert!(
            result,
            "start: {}, steps: {}, turn: {}, curr: {}",
            start, steps, turn, curr
        );
    }
}
