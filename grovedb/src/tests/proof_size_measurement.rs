//! Proof-size guard for the reference-backed indexed-axis proofs.
//!
//! The whole point of rows carrying their value is that a top-k proof
//! costs one value plus a hash per row instead of a per-row inclusion
//! proof. That is a property worth pinning: a future change that reaches
//! for per-row path proofs would still be correct, and would silently
//! multiply proof size several-fold.

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use crate::{
        tests::{make_test_grovedb, TEST_LEAF},
        Element, GroveDb,
    };

    /// Marginal proof cost per additional returned row must stay small and
    /// flat — the signature of "the value rides along with the row".
    ///
    /// Tree-shaped children on purpose: a count-indexed tree indexes its
    /// children's counts, so trees are the normal case, and they are the
    /// shape a per-row inclusion proof would be most expensive for.
    #[test]
    fn top_k_proof_cost_per_row_stays_flat() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("pcit");
        for i in 0..32usize {
            let key = format!("k{i:03}");
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                key.as_bytes(),
                Element::empty_tree(),
                None,
                v,
            )
            .unwrap()
            .expect("child");
            for j in 0..(i % 4) {
                db.insert(
                    [TEST_LEAF, b"cidx", key.as_bytes()].as_ref(),
                    format!("i{j}").as_bytes(),
                    Element::new_item(vec![b'x'; 8]),
                    None,
                    None,
                    v,
                )
                .unwrap()
                .expect("grandchild");
            }
        }

        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let size_for = |k: u16| -> usize {
            let p = db
                .prove_indexed_count_top_k(path, k, true, None, v)
                .unwrap()
                .expect("prove");
            let res = GroveDb::verify_indexed_count_top_k(&p, path, k, true, v).expect("verify");
            assert_eq!(res.entries.len(), k as usize, "must return k rows");
            assert_eq!(
                res.root_hash,
                db.root_hash(None, v).unwrap().unwrap(),
                "verified proof must reconstruct the grove root"
            );
            p.len()
        };

        let one = size_for(1);
        let sixteen = size_for(16);
        let per_row = (sixteen - one) / 15;

        // Generous ceiling: the real figure is well under this, and the
        // bound exists to catch an order-of-magnitude regression (a per-row
        // path proof costs hundreds of bytes), not to pin an exact size.
        assert!(
            per_row < 200,
            "marginal proof cost per returned row regressed to {per_row} bytes \
             (k=1: {one}, k=16: {sixteen}); a reference row should carry its value \
             for roughly the value's size plus a hash, not a per-row inclusion proof"
        );
        eprintln!("per-row marginal proof cost: {per_row} bytes (k=1 {one}, k=16 {sixteen})");
    }
}
