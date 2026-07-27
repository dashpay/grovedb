//! Insert operations

use std::{collections::HashMap, option::Option::None};

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{Merk, MerkOptions};
use grovedb_path::SubtreePath;
use grovedb_storage::{rocksdb_storage::PrefixedRocksDbTransactionContext, Storage, StorageBatch};
use grovedb_version::{check_grovedb_v0_with_cost, version::GroveVersion};

use crate::{util::TxRef, Element, Error, GroveDb, Transaction, TransactionArg};

/// Versioned dispatch for `add_element_on_transaction` (the non-batch insert
/// path). Consensus-critical — see the module docs.
mod add_element_on_transaction;

#[derive(Clone)]
/// Insert options
pub struct InsertOptions {
    /// Validate insertion does not override
    pub validate_insertion_does_not_override: bool,
    /// Validate insertion does not override tree
    pub validate_insertion_does_not_override_tree: bool,
    /// Base root storage is free
    pub base_root_storage_is_free: bool,
}

impl Default for InsertOptions {
    fn default() -> Self {
        InsertOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: true,
            base_root_storage_is_free: true,
        }
    }
}

impl InsertOptions {
    fn checks_for_override(&self) -> bool {
        self.validate_insertion_does_not_override_tree || self.validate_insertion_does_not_override
    }

    fn as_merk_options(&self) -> MerkOptions {
        MerkOptions {
            base_root_storage_is_free: self.base_root_storage_is_free,
        }
    }
}

/// Maximum key length in bytes. Merk link encoding stores the key length as a
/// single `u8`, so keys longer than 255 bytes would corrupt the encoding.
const MAX_KEY_LENGTH: usize = u8::MAX as usize;

fn validate_key_length(key: &[u8]) -> CostResult<(), Error> {
    if key.len() > MAX_KEY_LENGTH {
        return Err(Error::InvalidInput("key length must be at most 255 bytes"))
            .wrap_with_cost(OperationCost::default());
    }
    Ok(()).wrap_with_cost(OperationCost::default())
}

impl GroveDb {
    /// Insert a GroveDB element given a path to the subtree and the key to
    /// insert at
    pub fn insert<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        element: Element,
        options: Option<InsertOptions>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        check_grovedb_v0_with_cost!(
            "insert",
            grove_version.grovedb_versions.operations.insert.insert
        );

        let mut cost = OperationCost::default();
        cost_return_on_error!(&mut cost, validate_key_length(key));

        let subtree_path: SubtreePath<B> = path.into();
        let batch = StorageBatch::new();

        let tx = TxRef::new(&self.db, transaction);

        cost_return_on_error!(
            &mut cost,
            self.insert_on_transaction(
                subtree_path,
                key,
                element,
                options.unwrap_or_default(),
                tx.as_ref(),
                &batch,
                grove_version,
            )
        );

        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        tx.commit_local().wrap_with_cost(cost)
    }

    fn insert_on_transaction<'db, 'b, B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<'b, B>,
        key: &[u8],
        element: Element,
        options: InsertOptions,
        transaction: &'db Transaction,
        batch: &StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "insert_on_transaction",
            grove_version
                .grovedb_versions
                .operations
                .insert
                .insert_on_transaction
        );

        let mut cost = OperationCost::default();

        let mut merk_cache: HashMap<SubtreePath<'b, B>, Merk<PrefixedRocksDbTransactionContext>> =
            HashMap::default();

        let merk = cost_return_on_error!(
            &mut cost,
            self.add_element_on_transaction(
                path.clone(),
                key,
                element,
                options,
                transaction,
                batch,
                grove_version
            )
        );
        // A generic insert cannot mirror the new child's ordering value into
        // an indexed primary's secondary index. Reject before propagation, so
        // the `StorageBatch` is discarded and nothing is committed.
        cost_return_on_error_no_add!(
            cost,
            crate::operations::indexed_tree::reject_generic_write_into_indexed_primary(
                merk.tree_type,
                "insert",
            )
        );
        merk_cache.insert(path.clone(), merk);
        cost_return_on_error!(
            &mut cost,
            self.propagate_changes_with_transaction(
                merk_cache,
                path,
                transaction,
                batch,
                grove_version
            )
        );

        Ok(()).wrap_with_cost(cost)
    }

    /// Insert if not exists
    /// Insert if not exists
    ///
    /// Inserts an element at the specified path and key if it does not already
    /// exist.
    ///
    /// # Arguments
    ///
    /// * `path` - The path where the element should be inserted.
    /// * `key` - The key under which the element should be inserted.
    /// * `element` - The element to insert.
    /// * `transaction` - The transaction argument, if any.
    /// * `grove_version` - The GroveDB version.
    ///
    /// # Returns
    ///
    /// Returns a `CostResult<bool, Error>` indicating whether the element was
    /// inserted (`true`) or already existed (`false`).
    pub fn insert_if_not_exists<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        element: Element,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        check_grovedb_v0_with_cost!(
            "insert_if_not_exists",
            grove_version
                .grovedb_versions
                .operations
                .insert
                .insert_if_not_exists
        );

        let mut cost = OperationCost::default();
        cost_return_on_error!(&mut cost, validate_key_length(key));
        let subtree_path: SubtreePath<_> = path.into();

        if cost_return_on_error!(
            &mut cost,
            self.has_raw(subtree_path.clone(), key, transaction, grove_version)
        ) {
            Ok(false).wrap_with_cost(cost)
        } else {
            self.insert(subtree_path, key, element, None, transaction, grove_version)
                .map_ok(|_| true)
                .add_cost(cost)
        }
    }

    /// Insert if not exists
    /// If the item does exist return it
    ///
    /// Inserts an element at the given `path` and `key` if it does not exist.
    /// If the element already exists, returns the existing element.
    ///
    /// # Arguments
    ///
    /// * `path` - The path where the element should be inserted.
    /// * `key` - The key under which the element should be inserted.
    /// * `element` - The element to insert.
    /// * `transaction` - The transaction argument, if any.
    /// * `grove_version` - The GroveDB version.
    ///
    /// # Returns
    ///
    /// Returns a `CostResult<Option<Element>, Error>`, where
    /// `Ok(Some(element))` is the existing element if it was found, or
    /// `Ok(None)` if the new element was inserted.
    pub fn insert_if_not_exists_return_existing_element<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        element: Element,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<Element>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        check_grovedb_v0_with_cost!(
            "insert_if_not_exists_return_existing_element",
            grove_version
                .grovedb_versions
                .operations
                .insert
                .insert_if_not_exists_return_existing_element
        );

        let mut cost = OperationCost::default();
        cost_return_on_error!(&mut cost, validate_key_length(key));
        let subtree_path: SubtreePath<_> = path.into();

        let previous_element = cost_return_on_error!(
            &mut cost,
            self.get_raw_optional(subtree_path.clone(), key, transaction, grove_version)
        );
        if previous_element.is_some() {
            Ok(previous_element).wrap_with_cost(cost)
        } else {
            self.insert(subtree_path, key, element, None, transaction, grove_version)
                .map_ok(|_| None)
                .add_cost(cost)
        }
    }

    /// Insert if the value changed
    /// We return if the value was inserted
    /// If the value was changed then we return the previous element
    pub fn insert_if_changed_value<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        element: Element,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(bool, Option<Element>), Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        check_grovedb_v0_with_cost!(
            "insert_if_changed_value",
            grove_version
                .grovedb_versions
                .operations
                .insert
                .insert_if_changed_value
        );

        let mut cost = OperationCost::default();
        cost_return_on_error!(&mut cost, validate_key_length(key));
        let subtree_path: SubtreePath<B> = path.into();

        let previous_element = cost_return_on_error!(
            &mut cost,
            self.get_raw_optional(subtree_path.clone(), key, transaction, grove_version)
        );
        let needs_insert = match &previous_element {
            None => true,
            Some(previous_element) => previous_element != &element,
        };
        if !needs_insert {
            Ok((false, None)).wrap_with_cost(cost)
        } else {
            self.insert(subtree_path, key, element, None, transaction, grove_version)
                .map_ok(|_| (true, previous_element))
                .add_cost(cost)
        }
    }
}

#[cfg(test)]
mod tests {
    use grovedb_costs::{
        storage_cost::{removal::StorageRemovedBytes::NoStorageRemoval, StorageCost},
        OperationCost,
    };
    use grovedb_version::version::GroveVersion;
    use pretty_assertions::assert_eq;

    use crate::{
        operations::insert::InsertOptions,
        tests::{common::EMPTY_PATH, make_empty_grovedb, make_test_grovedb, TEST_LEAF},
        Element, Error,
    };

    /// Consensus version gate for `add_element_on_transaction` (the non-batch
    /// insert path). `CountSumTree` / `ProvableCountTree` / `ProvableCountSumTree`
    /// are written via the plain-value path (`Op::Put`) under **v0**
    /// (`GROVE_V1` / `GROVE_V2` — the behaviour frozen into the live protocol-v11
    /// activation chain, testnet block 245,344) and as **layered subtrees**
    /// under **v1** (`GROVE_V3`+, consistent with the batch insert path).
    ///
    /// The two ops compute a different parent `value_hash`
    /// (`value_hash(serialized)` vs `combine_hash(value_hash(serialized),
    /// NULL_HASH)`), hence a different grovedb root. This test pins both roots
    /// and asserts they differ, so the dispatch cannot silently collapse to a
    /// single behaviour — which would either break v11 replay (if v0 became
    /// layered) or revert the v3 change (if v1 became `Op::Put`).
    ///
    /// `empty_sum_tree` at `[56]` is the control: it is in the layered arm in
    /// both versions, so it never changed.
    #[test]
    fn add_element_on_transaction_version_gate_provable_count_sum_tree_root() {
        use grovedb_version::version::v1::GROVE_V1;

        // Replays the `transition_to_version_11` shape: an `empty_sum_tree`
        // (control) then an `empty_provable_count_sum_tree` (the regressed op).
        let root = |gv: &GroveVersion| {
            let db = make_empty_grovedb();
            db.insert(
                EMPTY_PATH,
                &[56u8],
                Element::empty_sum_tree(),
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("insert sum_tree at [56]");
            db.insert(
                [[56u8].as_slice()].as_ref(),
                b"c",
                Element::empty_provable_count_sum_tree(),
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("insert provable_count_sum_tree at [56,'c']");
            db.root_hash(None, gv).unwrap().unwrap()
        };

        let root_v0 = root(&GROVE_V1); // Op::Put — protocol-v11 consensus root
        let root_v1 = root(GroveVersion::latest()); // GROVE_V3 — layered

        eprintln!("root_v0 (Op::Put / protocol-v11) = {root_v0:?}");
        eprintln!("root_v1 (layered / GROVE_V3)     = {root_v1:?}");

        assert_ne!(
            root_v0, root_v1,
            "version gate must change the ProvableCountSumTree root: v0 (Op::Put) \
             vs v1 (layered)"
        );

        // v0 golden — the `Op::Put` root. Identical to PR #757's pinned root
        // (its `GOLDEN_2`), i.e. the grovedb v4.1.0 / protocol-v11 root that the
        // live activation chain (testnet block 245,344) committed. Locking it
        // here makes the GROVE_V1 / GROVE_V2 dispatch un-regressable.
        const GOLDEN_V0: [u8; 32] = [
            35, 99, 15, 178, 25, 57, 206, 47, 187, 195, 100, 28, 97, 85, 113, 230, 135, 22, 34,
            126, 72, 125, 158, 90, 116, 94, 214, 136, 96, 195, 235, 46,
        ];
        // v1 golden — the layered root produced under GROVE_V3.
        const GOLDEN_V1: [u8; 32] = [
            210, 14, 74, 67, 205, 240, 43, 174, 50, 154, 162, 90, 237, 45, 168, 42, 64, 155, 78,
            123, 102, 237, 213, 101, 63, 227, 24, 105, 16, 215, 194, 54,
        ];
        assert_eq!(
            root_v0, GOLDEN_V0,
            "v0 root drifted — the protocol-v11 (Op::Put) consensus root MUST NOT change"
        );
        assert_eq!(root_v1, GOLDEN_V1, "v1 (layered / GROVE_V3) root drifted");
    }

    /// Drives every match arm of `add_element_on_transaction` through the
    /// non-batch insert path for a given grove version: the layered-tree arm
    /// (all tree types), the commitment-tree arm, the append-tree arm
    /// (MMR / bulk-append / dense), the item arm, the reference arm, plus the
    /// override guards and the empty-tree-only (`value.is_some()`) guard. Run
    /// under both a v0 version (`GROVE_V1`) and a v1 version (`GROVE_V3`) so
    /// both frozen snapshots (`v0.rs` / `v1.rs`) are exercised.
    fn exercise_all_add_element_arms(gv: &GroveVersion) {
        use crate::reference_path::ReferencePathType;

        let db = make_empty_grovedb();
        let ins = |key: &[u8], el: Element| {
            db.insert(EMPTY_PATH, key, el, None, None, gv)
                .unwrap()
                .unwrap_or_else(|e| panic!("insert {}: {e:?}", String::from_utf8_lossy(key)));
        };

        // Layered-tree arm — every tree type round-trips on the non-batch path.
        ins(b"tree", Element::empty_tree());
        ins(b"sum", Element::empty_sum_tree());
        ins(b"bigsum", Element::empty_big_sum_tree());
        ins(b"count", Element::empty_count_tree());
        ins(b"countsum", Element::empty_count_sum_tree());
        ins(b"pcount", Element::empty_provable_count_tree());
        ins(b"pcountsum", Element::empty_provable_count_sum_tree());
        ins(b"psum", Element::empty_provable_sum_tree());
        ins(b"pcps", Element::empty_provable_count_provable_sum_tree());

        // Indexed-tree arms (PCIT / PSIT / PCPSIT). Each dispatcher (v0.rs /
        // v1.rs) has a dedicated branch that wires the layered subtree
        // (primary inline + dedicated indexed-child storage). PCPSIT requires
        // canonical axes — a single tag-0 entry with no item-key is valid.
        ins(b"pcit", Element::empty_provable_count_indexed_tree());
        ins(b"psit", Element::empty_provable_sum_indexed_tree());
        ins(
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(vec![(0, None)])
                .expect("canonical axes"),
        );

        // Append-tree arm.
        ins(b"mmr", Element::empty_mmr_tree());
        ins(
            b"bulk",
            Element::empty_bulk_append_tree(10).expect("valid chunk_power"),
        );
        ins(b"dense", Element::empty_dense_tree(4));

        // Commitment-tree arm (its own non-NULL initial child hash).
        ins(
            b"commit",
            Element::empty_commitment_tree(10).expect("valid chunk_power"),
        );

        // Item arm.
        ins(b"item", Element::new_item(b"v".to_vec()));
        // Sum item must live in a sum-bearing tree.
        db.insert(
            [b"sum".as_slice()].as_ref(),
            b"si",
            Element::new_sum_item(7),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("sum_item into sum tree");

        // Reference arm.
        ins(
            b"ref",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                b"item".to_vec()
            ])),
        );

        // Override guard (tree): default options forbid overriding a tree.
        let tree_override = db
            .insert(EMPTY_PATH, b"tree", Element::empty_tree(), None, None, gv)
            .unwrap();
        assert!(
            matches!(tree_override, Err(Error::OverrideNotAllowed(_))),
            "overriding a tree must be rejected, got {tree_override:?}"
        );

        // Override guard (plain value): explicit option forbids any override.
        let item_override = db
            .insert(
                EMPTY_PATH,
                b"item",
                Element::new_item(b"v2".to_vec()),
                Some(InsertOptions {
                    validate_insertion_does_not_override: true,
                    validate_insertion_does_not_override_tree: true,
                    base_root_storage_is_free: true,
                }),
                None,
                gv,
            )
            .unwrap();
        assert!(
            matches!(item_override, Err(Error::OverrideNotAllowed(_))),
            "overriding an item with validate_insertion_does_not_override must be rejected, got \
             {item_override:?}"
        );

        // Empty-tree-only guard: a tree element carrying a root key is rejected
        // on the non-batch path (trees must be empty at insertion time here).
        let with_root_key = db
            .insert(
                EMPTY_PATH,
                b"hasroot",
                Element::Tree(Some(vec![1u8]), None),
                None,
                None,
                gv,
            )
            .unwrap();
        assert!(
            matches!(with_root_key, Err(Error::InvalidCodeExecution(_))),
            "a non-empty tree must be rejected on the non-batch insert path, got {with_root_key:?}"
        );
    }

    /// Coverage for the v0 (`Op::Put`) snapshot — `GROVE_V1`.
    #[test]
    fn add_element_on_transaction_v0_covers_all_arms() {
        use grovedb_version::version::v1::GROVE_V1;
        exercise_all_add_element_arms(&GROVE_V1);
    }

    /// Coverage for the v1 (layered) snapshot — `GROVE_V3` (latest).
    #[test]
    fn add_element_on_transaction_v1_covers_all_arms() {
        exercise_all_add_element_arms(GroveVersion::latest());
    }

    /// The dispatcher rejects an unknown `add_element_on_transaction` version
    /// slot rather than silently picking a behaviour.
    #[test]
    fn add_element_on_transaction_rejects_unknown_version() {
        let mut bad = GroveVersion::latest().clone();
        bad.grovedb_versions
            .operations
            .insert
            .add_element_on_transaction = 2;
        let db = make_empty_grovedb();
        let err = db
            .insert(EMPTY_PATH, b"x", Element::empty_tree(), None, None, &bad)
            .unwrap();
        assert!(
            matches!(err, Err(Error::VersionError(_))),
            "unknown add_element_on_transaction version must error, got {err:?}"
        );
    }

    /// `InsertOptions` that permit overriding an existing tree in place — the
    /// shape required to re-inject a populated indexed-tree element via the
    /// public `db.insert` path.
    fn override_tree_opts() -> InsertOptions {
        InsertOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: false,
            base_root_storage_is_free: true,
        }
    }

    /// BUG 1 — forged aggregate on non-empty indexed-tree direct insert.
    ///
    /// Builds a populated PCIT via the dedicated `insert_into_count_indexed_tree`
    /// API, reads the element back, then:
    ///   (a) re-inserts it with a FORGED `count_value` (correct root keys but a
    ///       bogus count) and asserts rejection with `InvalidInput` — before the
    ///       fix the forged count was hash-committed and propagated into
    ///       ancestor aggregates;
    ///   (b) re-inserts the UNMODIFIED element and asserts success.
    ///
    /// Runs the whole thing under a caller-supplied grove version so both the
    /// v0 (`GROVE_V1`, `Op::Put` snapshot) and v1 (latest, layered snapshot)
    /// dispatch arms are exercised.
    fn forged_pcit_count_is_rejected(gv: &GroveVersion) {
        let db = make_test_grovedb(gv);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create empty PCIT");
        for k in [b"a".as_slice(), b"b".as_slice(), b"c".as_slice()] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::new_provable_count_tree_with_flags_and_count_value(None, 1, None),
                None,
                gv,
            )
            .unwrap()
            .expect("populate PCIT entry");
        }

        let elem = db
            .get_raw([TEST_LEAF].as_ref().into(), b"cidx", None, gv)
            .unwrap()
            .expect("get PCIT");
        let (real_primary, real_secondary, real_count) = match elem.underlying() {
            Element::ProvableCountIndexedTree(p, s, c, _) => (p.clone(), s.clone(), *c),
            other => panic!("expected PCIT, got {other:?}"),
        };
        assert!(real_primary.is_some());
        assert!(real_secondary.is_some());
        assert_eq!(real_count, 3, "three entries were inserted");

        // (a) Forged count — correct root keys, bogus count. Must be rejected.
        let forged = Element::new_provable_count_indexed_tree_with_root_keys_and_count_value(
            real_primary.clone(),
            real_secondary.clone(),
            1_000_000_000,
            None,
        );
        let forged_result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"cidx",
                forged,
                Some(override_tree_opts()),
                None,
                gv,
            )
            .unwrap();
        assert!(
            matches!(forged_result, Err(Error::InvalidInput(_))),
            "forged PCIT count_value must be rejected, got {forged_result:?}"
        );

        // (b) Unmodified element still round-trips.
        let honest = Element::new_provable_count_indexed_tree_with_root_keys_and_count_value(
            real_primary,
            real_secondary,
            real_count,
            None,
        );
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            honest,
            Some(override_tree_opts()),
            None,
            gv,
        )
        .unwrap()
        .expect("honest re-insert of the unmodified PCIT succeeds");

        let issues = db.verify_grovedb(None, true, true, gv).expect("verify");
        assert!(issues.is_empty(), "issues: {issues:?}");
    }

    /// BUG 1 — same forgery test for PSIT: forge the `sum_value` while keeping
    /// the real root keys, assert rejection, then assert the unmodified element
    /// re-inserts cleanly.
    fn forged_psit_sum_is_rejected(gv: &GroveVersion) {
        let db = make_test_grovedb(gv);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create empty PSIT");
        for (k, v) in [
            (b"a".as_slice(), 5i64),
            (b"b".as_slice(), 7),
            (b"c".as_slice(), 11),
        ] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                k,
                Element::new_sum_item(v),
                None,
                gv,
            )
            .unwrap()
            .expect("populate PSIT entry");
        }

        let elem = db
            .get_raw([TEST_LEAF].as_ref().into(), b"psit", None, gv)
            .unwrap()
            .expect("get PSIT");
        let (real_primary, real_secondary, real_sum) = match elem.underlying() {
            Element::ProvableSumIndexedTree(p, s, sv, _) => (p.clone(), s.clone(), *sv),
            other => panic!("expected PSIT, got {other:?}"),
        };
        assert!(real_primary.is_some());
        assert!(real_secondary.is_some());
        assert_eq!(real_sum, 23, "5 + 7 + 11");

        // (a) Forged sum — must be rejected.
        let forged = Element::ProvableSumIndexedTree(
            real_primary.clone(),
            real_secondary.clone(),
            999_999_999,
            None,
        );
        let forged_result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"psit",
                forged,
                Some(override_tree_opts()),
                None,
                gv,
            )
            .unwrap();
        assert!(
            matches!(forged_result, Err(Error::InvalidInput(_))),
            "forged PSIT sum_value must be rejected, got {forged_result:?}"
        );

        // (b) Unmodified element still round-trips.
        let honest = Element::ProvableSumIndexedTree(real_primary, real_secondary, real_sum, None);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit",
            honest,
            Some(override_tree_opts()),
            None,
            gv,
        )
        .unwrap()
        .expect("honest re-insert of the unmodified PSIT succeeds");

        let issues = db.verify_grovedb(None, true, true, gv).expect("verify");
        assert!(issues.is_empty(), "issues: {issues:?}");
    }

    /// BUG 1 — PCPSIT forgery: independently forge the count and the sum while
    /// keeping the real primary/axis root keys, asserting each is rejected, then
    /// assert the unmodified element re-inserts cleanly.
    fn forged_pcpsit_aggregate_is_rejected(gv: &GroveVersion) {
        use grovedb_element::indexed::IndexAxis;

        let db = make_test_grovedb(gv);
        let axes = vec![(IndexAxis::Count.tag(), None), (IndexAxis::Sum.tag(), None)];
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(axes).unwrap(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create empty PCPSIT");
        for (k, v) in [
            (b"a".as_slice(), 5i64),
            (b"b".as_slice(), 7),
            (b"c".as_slice(), 11),
        ] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(k.to_vec(), v),
                None,
                gv,
            )
            .unwrap()
            .expect("populate PCPSIT entry");
        }

        let elem = db
            .get_raw([TEST_LEAF].as_ref().into(), b"pcpsit", None, gv)
            .unwrap()
            .expect("get PCPSIT");
        let (real_primary, real_count, real_sum, real_axes) = match elem.underlying() {
            Element::ProvableCountProvableSumIndexedTree(p, c, s, ax, _) => {
                (p.clone(), *c, *s, ax.clone())
            }
            other => panic!("expected PCPSIT, got {other:?}"),
        };
        assert!(real_primary.is_some());
        assert_eq!(real_count, 3);
        assert_eq!(real_sum, 23);

        // (a1) Forged count — must be rejected.
        let forged_count = Element::ProvableCountProvableSumIndexedTree(
            real_primary.clone(),
            1_000_000_000,
            real_sum,
            real_axes.clone(),
            None,
        );
        let r = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcpsit",
                forged_count,
                Some(override_tree_opts()),
                None,
                gv,
            )
            .unwrap();
        assert!(
            matches!(r, Err(Error::InvalidInput(_))),
            "forged PCPSIT count must be rejected, got {r:?}"
        );

        // (a2) Forged sum — must be rejected.
        let forged_sum = Element::ProvableCountProvableSumIndexedTree(
            real_primary.clone(),
            real_count,
            999_999_999,
            real_axes.clone(),
            None,
        );
        let r = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcpsit",
                forged_sum,
                Some(override_tree_opts()),
                None,
                gv,
            )
            .unwrap();
        assert!(
            matches!(r, Err(Error::InvalidInput(_))),
            "forged PCPSIT sum must be rejected, got {r:?}"
        );

        // (b) Unmodified element still round-trips.
        let honest = Element::ProvableCountProvableSumIndexedTree(
            real_primary,
            real_count,
            real_sum,
            real_axes,
            None,
        );
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            honest,
            Some(override_tree_opts()),
            None,
            gv,
        )
        .unwrap()
        .expect("honest re-insert of the unmodified PCPSIT succeeds");

        let issues = db.verify_grovedb(None, true, true, gv).expect("verify");
        assert!(issues.is_empty(), "issues: {issues:?}");
    }

    #[test]
    fn forged_indexed_aggregate_rejected_v0() {
        use grovedb_version::version::v1::GROVE_V1;
        forged_pcit_count_is_rejected(&GROVE_V1);
        forged_psit_sum_is_rejected(&GROVE_V1);
        forged_pcpsit_aggregate_is_rejected(&GROVE_V1);
    }

    #[test]
    fn forged_indexed_aggregate_rejected_v1() {
        let gv = GroveVersion::latest();
        forged_pcit_count_is_rejected(gv);
        forged_psit_sum_is_rejected(gv);
        forged_pcpsit_aggregate_is_rejected(gv);
    }

    // ------------------------------------------------------------------
    // Indexed-tree (PCIT / PSIT / PCPSIT) direct-insertion guards.
    //
    // `Element` is public, so `db.insert` can be handed a hand-built
    // indexed-tree element whose root keys / aggregates / axes disagree
    // with what is actually on disk. Those fields are hash-committed into
    // the parent and propagated into ancestor aggregates, so
    // `add_element_on_transaction` revalidates every one of them against
    // the on-disk state before writing. The scenarios below drive each
    // rejection in turn: half-initialized state, primary root-key
    // mismatch, element-variant vs stored-subtree mismatch, secondary
    // root-key mismatch, and PCPSIT axes-schema changes. Every scenario
    // ends with `verify_grovedb` to prove the rejected write left no
    // partial state behind.
    //
    // The guards are byte-identical in the v0 (`GROVE_V1` / `GROVE_V2`)
    // and v1 (`GROVE_V3`+) snapshots, so each scenario is parameterized by
    // grove version and run under both.
    // ------------------------------------------------------------------

    // Exact rejection messages emitted by the indexed-tree arms of
    // `add_element_on_transaction`.
    const PCIT_PARTIAL_STATE: &str = "CountIndexedTree direct insertion: non-empty cidx must have \
                                      BOTH primary_root_key and secondary_root_key set to \
                                      Some(_); partial state (one None, one Some, or count>0 with \
                                      no roots) is not permitted";
    const PCIT_PRIMARY_MISMATCH: &str = "CountIndexedTree direct insertion: provided \
                                         primary_root_key does not match the existing primary \
                                         Merk's root key";
    const PCIT_VARIANT_MISMATCH: &str = "CountIndexedTree direct insertion: the existing primary \
                                         Merk is not a provable-count tree; the element variant \
                                         does not match the stored subtree";
    const PCIT_SECONDARY_MISMATCH: &str = "CountIndexedTree direct insertion: provided \
                                           secondary_root_key does not match the existing \
                                           secondary Merk's root key";
    const PSIT_PARTIAL_STATE: &str = "ProvableSumIndexedTree direct insertion: non-empty PSIT \
                                      must have BOTH primary_root_key and secondary_root_key set \
                                      to Some(_); partial state is not permitted";
    const PSIT_PRIMARY_MISMATCH: &str = "ProvableSumIndexedTree direct insertion: provided \
                                         primary_root_key does not match the existing primary \
                                         Merk's root key";
    const PSIT_VARIANT_MISMATCH: &str = "ProvableSumIndexedTree direct insertion: the existing \
                                         primary Merk is not a provable-sum tree; the element \
                                         variant does not match the stored subtree";
    const PSIT_SECONDARY_MISMATCH: &str = "ProvableSumIndexedTree direct insertion: provided \
                                           secondary_root_key does not match the existing \
                                           secondary Merk's root key";
    const PCPSIT_PARTIAL_STATE: &str = "ProvableCountProvableSumIndexedTree direct insertion: \
                                        non-empty PCPSIT must have primary_root_key = Some(_); \
                                        partial state is not permitted";
    const PCPSIT_PRIMARY_MISMATCH: &str = "ProvableCountProvableSumIndexedTree direct insertion: \
                                           provided primary_root_key does not match the existing \
                                           primary Merk's root key";
    const PCPSIT_VARIANT_MISMATCH: &str = "ProvableCountProvableSumIndexedTree direct insertion: \
                                           the existing primary Merk is not a \
                                           provable-count-provable-sum tree; the element variant \
                                           does not match the stored subtree";
    const PCPSIT_AXES_SCHEMA_CHANGE: &str =
        "ProvableCountProvableSumIndexedTree direct insertion: the axes schema does not match the \
         stored element; axes cannot be added or removed on an existing indexed tree (no reindex \
         path exists)";
    const PCPSIT_AXIS_SECONDARY_MISMATCH: &str =
        "ProvableCountProvableSumIndexedTree direct insertion: provided axis secondary_root_key \
         does not match the existing secondary Merk's root key";

    /// A root key no Merk in these fixtures can have: nothing is ever
    /// stored under it, so `Merk::open_layered_with_root_key` loads an
    /// *empty* tree whose `root_hash_key_and_aggregate_data()` reports a
    /// root key of `None` — i.e. `!= Some(bogus)`, which is exactly the
    /// shape the secondary-root-key guards must reject.
    fn bogus_root_key() -> Option<Vec<u8>> {
        Some(b"\xff\xff\xff\xff\xff\xff\xff\xff".to_vec())
    }

    #[track_caller]
    fn assert_invalid_input(result: Result<(), Error>, expected: &str) {
        match result {
            Err(Error::InvalidInput(message)) => assert_eq!(message, expected),
            other => panic!("expected Err(Error::InvalidInput({expected:?})), got {other:?}"),
        }
    }

    /// Build a populated PCIT at `[TEST_LEAF, key]` through the dedicated
    /// indexed-tree API and return its real
    /// `(primary_root_key, secondary_root_key, count_value)`.
    fn populate_pcit(
        db: &crate::tests::TempGroveDb,
        gv: &GroveVersion,
        key: &[u8],
    ) -> (Option<Vec<u8>>, Option<Vec<u8>>, u64) {
        db.insert(
            [TEST_LEAF].as_ref(),
            key,
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create empty PCIT");
        for k in [b"a".as_slice(), b"b".as_slice(), b"c".as_slice()] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, key].as_ref(),
                k,
                Element::new_provable_count_tree_with_flags_and_count_value(None, 1, None),
                None,
                gv,
            )
            .unwrap()
            .expect("populate PCIT entry");
        }
        let elem = db
            .get_raw([TEST_LEAF].as_ref().into(), key, None, gv)
            .unwrap()
            .expect("get PCIT");
        let parts = match elem.underlying() {
            Element::ProvableCountIndexedTree(p, s, c, _) => (p.clone(), s.clone(), *c),
            other => panic!("expected PCIT, got {other:?}"),
        };
        assert!(parts.0.is_some() && parts.1.is_some(), "PCIT is populated");
        assert_eq!(parts.2, 3, "three entries were inserted");
        parts
    }

    /// Build a populated PSIT at `[TEST_LEAF, key]` and return its real
    /// `(primary_root_key, secondary_root_key, sum_value)`.
    fn populate_psit(
        db: &crate::tests::TempGroveDb,
        gv: &GroveVersion,
        key: &[u8],
    ) -> (Option<Vec<u8>>, Option<Vec<u8>>, i64) {
        db.insert(
            [TEST_LEAF].as_ref(),
            key,
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create empty PSIT");
        for (k, v) in [
            (b"a".as_slice(), 5i64),
            (b"b".as_slice(), 7),
            (b"c".as_slice(), 11),
        ] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, key].as_ref(),
                k,
                Element::new_sum_item(v),
                None,
                gv,
            )
            .unwrap()
            .expect("populate PSIT entry");
        }
        let elem = db
            .get_raw([TEST_LEAF].as_ref().into(), key, None, gv)
            .unwrap()
            .expect("get PSIT");
        let parts = match elem.underlying() {
            Element::ProvableSumIndexedTree(p, s, sv, _) => (p.clone(), s.clone(), *sv),
            other => panic!("expected PSIT, got {other:?}"),
        };
        assert!(parts.0.is_some() && parts.1.is_some(), "PSIT is populated");
        assert_eq!(parts.2, 23, "5 + 7 + 11");
        parts
    }

    /// Build a populated PCPSIT at `[TEST_LEAF, key]` carrying the count
    /// and sum axes, and return its real
    /// `(primary_root_key, count_value, sum_value, axes)`.
    #[allow(clippy::type_complexity)]
    fn populate_pcpsit(
        db: &crate::tests::TempGroveDb,
        gv: &GroveVersion,
        key: &[u8],
    ) -> (Option<Vec<u8>>, u64, i64, Vec<(u8, Option<Vec<u8>>)>) {
        use grovedb_element::indexed::IndexAxis;

        db.insert(
            [TEST_LEAF].as_ref(),
            key,
            Element::empty_provable_count_provable_sum_indexed_tree(vec![
                (IndexAxis::Count.tag(), None),
                (IndexAxis::Sum.tag(), None),
            ])
            .expect("canonical axes"),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create empty PCPSIT");
        for (k, v) in [
            (b"a".as_slice(), 5i64),
            (b"b".as_slice(), 7),
            (b"c".as_slice(), 11),
        ] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, key].as_ref(),
                k,
                Element::new_item_with_sum_item(k.to_vec(), v),
                None,
                gv,
            )
            .unwrap()
            .expect("populate PCPSIT entry");
        }
        let elem = db
            .get_raw([TEST_LEAF].as_ref().into(), key, None, gv)
            .unwrap()
            .expect("get PCPSIT");
        let parts = match elem.underlying() {
            Element::ProvableCountProvableSumIndexedTree(p, c, s, ax, _) => {
                (p.clone(), *c, *s, ax.clone())
            }
            other => panic!("expected PCPSIT, got {other:?}"),
        };
        assert!(parts.0.is_some(), "PCPSIT primary is populated");
        assert_eq!((parts.1, parts.2), (3, 23));
        assert_eq!(
            parts.3.iter().map(|(t, _)| *t).collect::<Vec<u8>>(),
            vec![IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
        );
        parts
    }

    /// Half-initialized indexed-tree elements are refused before any child
    /// Merk is opened: a claim that is not fully empty must carry every
    /// root key it needs, otherwise the element bytes would commit an
    /// aggregate that is disconnected from any real index content.
    fn indexed_partial_state_is_rejected(gv: &GroveVersion) {
        let db = make_test_grovedb(gv);

        // PCIT — (Some, None), (None, Some) and (None, None) with count > 0.
        for (primary, secondary, count) in [
            (bogus_root_key(), None, 0u64),
            (None, bogus_root_key(), 0),
            (None, None, 5),
        ] {
            let result = db
                .insert(
                    [TEST_LEAF].as_ref(),
                    b"pcit_partial",
                    Element::ProvableCountIndexedTree(primary, secondary, count, None),
                    None,
                    None,
                    gv,
                )
                .unwrap();
            assert_invalid_input(result, PCIT_PARTIAL_STATE);
        }

        // PSIT — same three shapes.
        for (primary, secondary, sum) in [
            (bogus_root_key(), None, 0i64),
            (None, bogus_root_key(), 0),
            (None, None, 9),
        ] {
            let result = db
                .insert(
                    [TEST_LEAF].as_ref(),
                    b"psit_partial",
                    Element::ProvableSumIndexedTree(primary, secondary, sum, None),
                    None,
                    None,
                    gv,
                )
                .unwrap();
            assert_invalid_input(result, PSIT_PARTIAL_STATE);
        }

        // PCPSIT — a missing primary with a non-zero count, with a non-zero
        // sum, and with an axis that already claims a secondary root key.
        for (count, sum, axes) in [
            (5u64, 0i64, vec![(0u8, None)]),
            (0, 9, vec![(0u8, None)]),
            (0, 0, vec![(0u8, bogus_root_key())]),
        ] {
            let result = db
                .insert(
                    [TEST_LEAF].as_ref(),
                    b"pcpsit_partial",
                    Element::ProvableCountProvableSumIndexedTree(None, count, sum, axes, None),
                    None,
                    None,
                    gv,
                )
                .unwrap();
            assert_invalid_input(result, PCPSIT_PARTIAL_STATE);
        }

        // Nothing was written at any of the three keys.
        for key in [
            b"pcit_partial".as_slice(),
            b"psit_partial".as_slice(),
            b"pcpsit_partial".as_slice(),
        ] {
            assert!(
                !db.has_raw([TEST_LEAF].as_ref(), key, None, gv)
                    .unwrap()
                    .expect("has_raw"),
                "rejected partial insert must not create {}",
                String::from_utf8_lossy(key)
            );
        }
        let issues = db.verify_grovedb(None, true, true, gv).expect("verify");
        assert!(issues.is_empty(), "issues: {issues:?}");
    }

    /// A claimed `primary_root_key` that disagrees with the primary Merk's
    /// actual root key is refused — persisting it would break the H1-A
    /// value-hash reconstruction for the whole subtree.
    fn indexed_primary_root_key_mismatch_is_rejected(gv: &GroveVersion) {
        let db = make_test_grovedb(gv);
        let (_, pcit_secondary, pcit_count) = populate_pcit(&db, gv, b"cidx");
        let (_, psit_secondary, psit_sum) = populate_psit(&db, gv, b"psit");
        let (_, pcpsit_count, pcpsit_sum, pcpsit_axes) = populate_pcpsit(&db, gv, b"pcpsit");

        // Everything except the primary root key is honest in each case.
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"cidx",
                Element::ProvableCountIndexedTree(
                    bogus_root_key(),
                    pcit_secondary,
                    pcit_count,
                    None,
                ),
                Some(override_tree_opts()),
                None,
                gv,
            )
            .unwrap();
        assert_invalid_input(result, PCIT_PRIMARY_MISMATCH);

        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"psit",
                Element::ProvableSumIndexedTree(bogus_root_key(), psit_secondary, psit_sum, None),
                Some(override_tree_opts()),
                None,
                gv,
            )
            .unwrap();
        assert_invalid_input(result, PSIT_PRIMARY_MISMATCH);

        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcpsit",
                Element::ProvableCountProvableSumIndexedTree(
                    bogus_root_key(),
                    pcpsit_count,
                    pcpsit_sum,
                    pcpsit_axes,
                    None,
                ),
                Some(override_tree_opts()),
                None,
                gv,
            )
            .unwrap();
        assert_invalid_input(result, PCPSIT_PRIMARY_MISMATCH);

        let issues = db.verify_grovedb(None, true, true, gv).expect("verify");
        assert!(issues.is_empty(), "issues: {issues:?}");
    }

    /// The element variant must agree with the aggregate *shape* of the
    /// primary Merk that is already stored under the key. A correct root
    /// key is not enough: writing a PCIT element over a provable-sum
    /// primary (or vice versa) makes the element and the subtree disagree
    /// on arity, which `verify_grovedb` cannot even report.
    fn indexed_variant_mismatch_is_rejected(gv: &GroveVersion) {
        let db = make_test_grovedb(gv);
        let (pcit_primary, _, pcit_count) = populate_pcit(&db, gv, b"cidx");
        let (psit_primary, _, _) = populate_psit(&db, gv, b"psit");

        // PCIT element over the populated PSIT: the primary root key
        // matches, but that primary is a provable-SUM tree.
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"psit",
                Element::ProvableCountIndexedTree(psit_primary, bogus_root_key(), 3, None),
                Some(override_tree_opts()),
                None,
                gv,
            )
            .unwrap();
        assert_invalid_input(result, PCIT_VARIANT_MISMATCH);

        // PSIT element over the populated PCIT (provable-COUNT primary).
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"cidx",
                Element::ProvableSumIndexedTree(pcit_primary.clone(), bogus_root_key(), 23, None),
                Some(override_tree_opts()),
                None,
                gv,
            )
            .unwrap();
        assert_invalid_input(result, PSIT_VARIANT_MISMATCH);

        // PCPSIT element over the same provable-count primary — the count
        // even matches, so only the aggregate shape rules it out.
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"cidx",
                Element::ProvableCountProvableSumIndexedTree(
                    pcit_primary,
                    pcit_count,
                    0,
                    vec![(0, None)],
                    None,
                ),
                Some(override_tree_opts()),
                None,
                gv,
            )
            .unwrap();
        assert_invalid_input(result, PCPSIT_VARIANT_MISMATCH);

        let issues = db.verify_grovedb(None, true, true, gv).expect("verify");
        assert!(issues.is_empty(), "issues: {issues:?}");
    }

    /// The secondary (index) Merk's root key is validated the same way the
    /// primary's is. A claimed key that no node lives under opens as an
    /// empty secondary, whose root key is `None` — the mismatch is caught
    /// instead of being committed into the element bytes.
    fn indexed_secondary_root_key_mismatch_is_rejected(gv: &GroveVersion) {
        let db = make_test_grovedb(gv);
        let (pcit_primary, _, pcit_count) = populate_pcit(&db, gv, b"cidx");
        let (psit_primary, _, psit_sum) = populate_psit(&db, gv, b"psit");
        let (pcpsit_primary, pcpsit_count, pcpsit_sum, pcpsit_axes) =
            populate_pcpsit(&db, gv, b"pcpsit");

        // PCIT — honest primary + count, bogus secondary.
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"cidx",
                Element::ProvableCountIndexedTree(pcit_primary, bogus_root_key(), pcit_count, None),
                Some(override_tree_opts()),
                None,
                gv,
            )
            .unwrap();
        assert_invalid_input(result, PCIT_SECONDARY_MISMATCH);

        // PSIT — honest primary + sum, bogus secondary.
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"psit",
                Element::ProvableSumIndexedTree(psit_primary, bogus_root_key(), psit_sum, None),
                Some(override_tree_opts()),
                None,
                gv,
            )
            .unwrap();
        assert_invalid_input(result, PSIT_SECONDARY_MISMATCH);

        // PCPSIT — the axis TAGS still match what is stored (so the
        // schema guard passes), but the count axis carries a bogus
        // secondary root key.
        let mut tampered_axes = pcpsit_axes;
        tampered_axes[0].1 = bogus_root_key();
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcpsit",
                Element::ProvableCountProvableSumIndexedTree(
                    pcpsit_primary,
                    pcpsit_count,
                    pcpsit_sum,
                    tampered_axes,
                    None,
                ),
                Some(override_tree_opts()),
                None,
                gv,
            )
            .unwrap();
        assert_invalid_input(result, PCPSIT_AXIS_SECONDARY_MISMATCH);

        let issues = db.verify_grovedb(None, true, true, gv).expect("verify");
        assert!(issues.is_empty(), "issues: {issues:?}");
    }

    /// Adding or removing a PCPSIT axis on an existing indexed tree is
    /// refused: `axes_digest` is recomputed from the element's own claimed
    /// axes, so a schema change would look self-consistent while the new
    /// axis indexed none of the existing rows (and a dropped axis orphaned
    /// a populated secondary Merk). There is no reindex path, so the only
    /// safe answer is to refuse.
    fn pcpsit_axes_schema_change_is_rejected(gv: &GroveVersion) {
        use grovedb_element::indexed::IndexAxis;

        let db = make_test_grovedb(gv);
        let (primary, count, sum, axes) = populate_pcpsit(&db, gv, b"pcpsit");

        // Drop the sum axis (stored tags [0, 1] vs incoming [0]).
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcpsit",
                Element::ProvableCountProvableSumIndexedTree(
                    primary.clone(),
                    count,
                    sum,
                    vec![axes[0].clone()],
                    None,
                ),
                Some(override_tree_opts()),
                None,
                gv,
            )
            .unwrap();
        assert_invalid_input(result, PCPSIT_AXES_SCHEMA_CHANGE);

        // Add the avg axis (stored tags [0, 1] vs incoming [0, 1, 2]).
        let mut widened = axes.clone();
        widened.push((IndexAxis::Avg.tag(), None));
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcpsit",
                Element::ProvableCountProvableSumIndexedTree(
                    primary.clone(),
                    count,
                    sum,
                    widened,
                    None,
                ),
                Some(override_tree_opts()),
                None,
                gv,
            )
            .unwrap();
        assert_invalid_input(result, PCPSIT_AXES_SCHEMA_CHANGE);

        // The stored element is untouched and the unchanged schema still
        // round-trips.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Element::ProvableCountProvableSumIndexedTree(primary, count, sum, axes, None),
            Some(override_tree_opts()),
            None,
            gv,
        )
        .unwrap()
        .expect("re-inserting the unchanged axes schema succeeds");

        let issues = db.verify_grovedb(None, true, true, gv).expect("verify");
        assert!(issues.is_empty(), "issues: {issues:?}");
    }

    /// The axes-schema guard only applies when the key already holds a
    /// PCPSIT. Over a *plain* `ProvableCountProvableSumTree` — whose
    /// primary has the very same aggregate shape, so the variant, count
    /// and sum checks all pass — there is no stored axes list to compare
    /// against, and validation falls through to the per-axis secondary
    /// check (which here rejects the bogus axis root key).
    fn pcpsit_over_plain_provable_count_sum_tree_has_no_axes_schema(gv: &GroveVersion) {
        let db = make_test_grovedb(gv);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"plain",
            Element::empty_provable_count_provable_sum_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create empty plain PCPS tree");
        for (k, v) in [
            (b"a".as_slice(), 5i64),
            (b"b".as_slice(), 7),
            (b"c".as_slice(), 11),
        ] {
            db.insert(
                [TEST_LEAF, b"plain"].as_ref(),
                k,
                Element::new_item_with_sum_item(k.to_vec(), v),
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("populate plain PCPS tree");
        }

        let elem = db
            .get_raw([TEST_LEAF].as_ref().into(), b"plain", None, gv)
            .unwrap()
            .expect("get plain PCPS tree");
        let (root_key, count, sum) = match elem.underlying() {
            Element::ProvableCountProvableSumTree(rk, c, s, _) => (rk.clone(), *c, *s),
            other => panic!("expected a plain ProvableCountProvableSumTree, got {other:?}"),
        };
        assert!(root_key.is_some());
        assert_eq!((count, sum), (3, 23));

        // Honest primary/count/sum, so every primary check passes — but
        // the stored element is a PLAIN tree of a different type, and
        // converting it in place would leave the axis secondaries empty
        // over its populated primary. The in-place conversion guard is
        // what rejects this (it did not always exist: before it, this
        // fell through the axes-schema comparison, which has nothing to
        // compare against a non-PCPSIT element, and was only stopped by
        // the axis secondary root-key check).
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"plain",
                Element::ProvableCountProvableSumIndexedTree(
                    root_key,
                    count,
                    sum,
                    vec![(0, bogus_root_key())],
                    None,
                ),
                Some(override_tree_opts()),
                None,
                gv,
            )
            .unwrap();
        assert_invalid_input(
            result,
            "ProvableCountProvableSumIndexedTree direct insertion: an existing tree of a \
             different type is stored at this key; converting it in place would leave the axis \
             secondaries empty over a populated primary (no reindex path)",
        );

        // The plain tree is still a plain tree.
        let elem = db
            .get_raw([TEST_LEAF].as_ref().into(), b"plain", None, gv)
            .unwrap()
            .expect("get plain PCPS tree");
        assert!(
            matches!(elem.underlying(), Element::ProvableCountProvableSumTree(..)),
            "the rejected insert must not have replaced the element, got {elem:?}"
        );
        let issues = db.verify_grovedb(None, true, true, gv).expect("verify");
        assert!(issues.is_empty(), "issues: {issues:?}");
    }

    /// v0 snapshot (`GROVE_V1`) — every indexed-tree direct-insert guard.
    #[test]
    fn indexed_tree_direct_insert_guards_v0() {
        use grovedb_version::version::v1::GROVE_V1;

        indexed_partial_state_is_rejected(&GROVE_V1);
        indexed_primary_root_key_mismatch_is_rejected(&GROVE_V1);
        indexed_variant_mismatch_is_rejected(&GROVE_V1);
        indexed_secondary_root_key_mismatch_is_rejected(&GROVE_V1);
        pcpsit_axes_schema_change_is_rejected(&GROVE_V1);
        pcpsit_over_plain_provable_count_sum_tree_has_no_axes_schema(&GROVE_V1);
    }

    /// v1 snapshot (`GROVE_V3`+, latest) — the indexed-tree arms are
    /// identical to v0's, so the same guards must hold.
    #[test]
    fn indexed_tree_direct_insert_guards_v1() {
        let gv = GroveVersion::latest();

        indexed_partial_state_is_rejected(gv);
        indexed_primary_root_key_mismatch_is_rejected(gv);
        indexed_variant_mismatch_is_rejected(gv);
        indexed_secondary_root_key_mismatch_is_rejected(gv);
        pcpsit_axes_schema_change_is_rejected(gv);
        pcpsit_over_plain_provable_count_sum_tree_has_no_axes_schema(gv);
    }

    #[test]
    fn test_non_root_insert_item_without_transaction() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let element = Element::new_item(b"ayy".to_vec());
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key",
            element.clone(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful insert");
        assert_eq!(
            db.get([TEST_LEAF].as_ref(), b"key", None, grove_version)
                .unwrap()
                .expect("successful get"),
            element
        );
    }

    #[test]
    fn test_non_root_insert_subtree_then_insert_item_without_transaction() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let element = Element::new_item(b"ayy".to_vec());

        // Insert a subtree first
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");
        // Insert an element into subtree
        db.insert(
            [TEST_LEAF, b"key1"].as_ref(),
            b"key2",
            element.clone(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful value insert");
        assert_eq!(
            db.get([TEST_LEAF, b"key1"].as_ref(), b"key2", None, grove_version)
                .unwrap()
                .expect("successful get"),
            element
        );
    }

    #[test]
    fn test_non_root_insert_item_with_transaction() {
        let grove_version = GroveVersion::latest();
        let item_key = b"key3";

        let db = make_test_grovedb(grove_version);
        let transaction = db.start_transaction();

        // Check that there's no such key in the DB
        let result = db
            .get([TEST_LEAF].as_ref(), item_key, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::PathKeyNotFound(_))));

        let element1 = Element::new_item(b"ayy".to_vec());

        db.insert(
            [TEST_LEAF].as_ref(),
            item_key,
            element1,
            None,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("cannot insert an item into GroveDB");

        // The key was inserted inside the transaction, so it shouldn't be
        // possible to get it back without committing or using transaction
        let result = db
            .get([TEST_LEAF].as_ref(), item_key, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::PathKeyNotFound(_))));
        // Check that the element can be retrieved when transaction is passed
        let result_with_transaction = db
            .get(
                [TEST_LEAF].as_ref(),
                item_key,
                Some(&transaction),
                grove_version,
            )
            .unwrap()
            .expect("Expected to work");
        assert_eq!(result_with_transaction, Element::new_item(b"ayy".to_vec()));

        // Test that commit works
        db.commit_transaction(transaction).unwrap().unwrap();

        // Check that the change was committed
        let result = db
            .get([TEST_LEAF].as_ref(), item_key, None, grove_version)
            .unwrap()
            .expect("Expected transaction to work");
        assert_eq!(result, Element::new_item(b"ayy".to_vec()));
    }

    #[test]
    fn test_non_root_insert_subtree_with_transaction() {
        let grove_version = GroveVersion::latest();
        let subtree_key = b"subtree_key";

        let db = make_test_grovedb(grove_version);
        let transaction = db.start_transaction();

        // Check that there's no such key in the DB
        let result = db
            .get([TEST_LEAF].as_ref(), subtree_key, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::PathKeyNotFound(_))));

        db.insert(
            [TEST_LEAF].as_ref(),
            subtree_key,
            Element::empty_tree(),
            None,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("cannot insert an item into GroveDB");

        let result = db
            .get([TEST_LEAF].as_ref(), subtree_key, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::PathKeyNotFound(_))));

        let result_with_transaction = db
            .get(
                [TEST_LEAF].as_ref(),
                subtree_key,
                Some(&transaction),
                grove_version,
            )
            .unwrap()
            .expect("Expected to work");
        assert_eq!(result_with_transaction, Element::empty_tree());

        db.commit_transaction(transaction).unwrap().unwrap();

        let result = db
            .get([TEST_LEAF].as_ref(), subtree_key, None, grove_version)
            .unwrap()
            .expect("Expected transaction to work");
        assert_eq!(result, Element::empty_tree());
    }

    #[test]
    fn test_insert_if_not_exists() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert twice at the same path
        assert!(db
            .insert_if_not_exists(
                [TEST_LEAF].as_ref(),
                b"key1",
                Element::empty_tree(),
                None,
                grove_version
            )
            .unwrap()
            .expect("Provided valid path"));
        assert!(!db
            .insert_if_not_exists(
                [TEST_LEAF].as_ref(),
                b"key1",
                Element::empty_tree(),
                None,
                grove_version
            )
            .unwrap()
            .expect("Provided valid path"));

        // Should propagate errors from insertion
        let result = db
            .insert_if_not_exists(
                [TEST_LEAF, b"unknown"].as_ref(),
                b"key1",
                Element::empty_tree(),
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidParentLayerPath(_))));
    }

    #[test]
    fn test_insert_if_not_exists_return_existing_element() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let element_key = b"key1";
        let new_element = Element::new_item(b"new_value".to_vec());

        // Insert a new element and check if it returns None
        let result = db
            .insert_if_not_exists_return_existing_element(
                [TEST_LEAF].as_ref(),
                element_key,
                new_element.clone(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("Expected insertion of new element");

        assert_eq!(result, None);

        // Try inserting the same element again and expect it to return the existing
        // element
        let result = db
            .insert_if_not_exists_return_existing_element(
                [TEST_LEAF].as_ref(),
                element_key,
                Element::new_item(b"another_value".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("Expected to return existing element");

        assert_eq!(result, Some(new_element.clone()));

        // Check if the existing element is still the original one and not replaced
        let fetched_element = db
            .get([TEST_LEAF].as_ref(), element_key, None, grove_version)
            .unwrap()
            .expect("Expected to retrieve the existing element");

        assert_eq!(fetched_element, new_element);
    }

    #[test]
    fn test_insert_if_not_exists_return_existing_element_with_transaction() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let element_key = b"key2";
        let new_element = Element::new_item(b"transaction_value".to_vec());
        let transaction = db.start_transaction();

        // Insert a new element within a transaction and check if it returns None
        let result = db
            .insert_if_not_exists_return_existing_element(
                [TEST_LEAF].as_ref(),
                element_key,
                new_element.clone(),
                Some(&transaction),
                grove_version,
            )
            .unwrap()
            .expect("Expected insertion of new element in transaction");

        assert_eq!(result, None);

        // Try inserting the same element again within the transaction
        // and expect it to return the existing element
        let result = db
            .insert_if_not_exists_return_existing_element(
                [TEST_LEAF].as_ref(),
                element_key,
                Element::new_item(b"another_transaction_value".to_vec()),
                Some(&transaction),
                grove_version,
            )
            .unwrap()
            .expect("Expected to return existing element in transaction");

        assert_eq!(result, Some(new_element.clone()));

        // Commit the transaction
        db.commit_transaction(transaction).unwrap().unwrap();

        // Check if the element is still the original one and not replaced
        let fetched_element = db
            .get([TEST_LEAF].as_ref(), element_key, None, grove_version)
            .unwrap()
            .expect("Expected to retrieve the existing element after transaction commit");

        assert_eq!(fetched_element, new_element);
    }

    #[test]
    fn test_insert_if_not_exists_return_existing_element_invalid_path() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Try inserting to an invalid path and expect an error
        let result = db.insert_if_not_exists_return_existing_element(
            [b"invalid_path"].as_ref(),
            b"key",
            Element::new_item(b"value".to_vec()),
            None,
            grove_version,
        );

        assert!(matches!(
            result.unwrap(),
            Err(Error::InvalidParentLayerPath(_))
        ));
    }

    #[test]
    fn test_one_insert_item_cost() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::new_item(b"cat".to_vec()),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("should insert");
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 72
        //   1 for the flag option (but no flags)
        //   1 for the enum type item
        //   3 for "cat"
        //   1 for cat length
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 1 for Basic merk
        // 1 byte for the value_size (required space for 70)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Basic Merk 1
        // Child Heights 2

        // Total 37 + 72 + 40 = 149

        // Hash node calls
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // 1 to get tree, 1 to insert, 1 to insert into root tree
                storage_cost: StorageCost {
                    added_bytes: 149,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 2,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_sum_item_in_sum_tree_cost() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"s",
            Element::empty_sum_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("expected to add upper tree");

        let cost = db
            .insert(
                [b"s".as_slice()].as_ref(),
                b"key1",
                Element::new_sum_item(5),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("should insert");
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 85
        //   1 for the enum type item
        //   9 for the value (encoded var vec)
        //   1 for the flag option (but no flags)
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 9 for Summed merk
        // 1 byte for the value_size (required space for 77)

        // Parent Hook -> 48
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Summed Merk 9
        // Child Heights 2

        // Total 37 + 85 + 48 = 170
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 5,
                storage_cost: StorageCost {
                    added_bytes: 170,
                    replaced_bytes: 84, // todo: verify
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 156,
                hash_node_calls: 8,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_sum_item_under_sum_item_cost() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"s",
            Element::empty_sum_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("expected to add upper tree");

        db.insert(
            [b"s".as_slice()].as_ref(),
            b"key1",
            Element::new_sum_item(5),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        let cost = db
            .insert(
                [b"s".as_slice()].as_ref(),
                b"key2",
                Element::new_sum_item(6),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("should insert");
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 85
        //   1 for the flag option (but no flags)
        //   1 for the enum type item
        //   9 for the value (encoded var vec)
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 9 for Summed merk
        // 1 byte for the value_size (required space for 77)

        // Parent Hook -> 48
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Summed Merk 9
        // Child Heights 2

        // Total 37 + 85 + 48 = 170

        // replaced bytes
        // 133 for key1 (higher node/same merk level)
        // ?

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 7,
                storage_cost: StorageCost {
                    added_bytes: 170,
                    replaced_bytes: 217,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 232,
                hash_node_calls: 10,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_bigger_sum_item_under_sum_item_cost() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"s",
            Element::empty_sum_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("expected to add upper tree");

        db.insert(
            [b"s".as_slice()].as_ref(),
            b"key1",
            Element::new_sum_item(126),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        // the cost of the varint goes up by 2 after 126 and another 2 at 32768
        let cost = db
            .insert(
                [b"s".as_slice()].as_ref(),
                b"key2",
                Element::new_sum_item(32768),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("should insert");
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 85
        //   1 for the flag option (but no flags)
        //   1 for the enum type item
        //   9 for the value (encoded var vec)
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 9 for Summed merk
        // 1 byte for the value_size (required space for 81)

        // Parent Hook -> 48
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Summed Merk 9
        // Child Heights 2

        // Total 37 + 85 + 48 = 170
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 7,
                storage_cost: StorageCost {
                    added_bytes: 170,
                    replaced_bytes: 217, // todo: verify
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 237,
                hash_node_calls: 10,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_item_cost_with_flags() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::new_item_with_flags(b"cat".to_vec(), Some(b"dog".to_vec())),
                None,
                Some(&tx),
                grove_version,
            )
            .cost;
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 76
        //   1 for the flag option
        //   3 for flags
        //   1 for flags length
        //   1 for the enum type item
        //   3 for "cat"
        //   1 for cat length
        //   1 for basic merk
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 1 byte for the value_size (required space for 70)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1
        // Total 37 + 76 + 40 = 153

        // Hash node calls
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // 1 to get tree, 1 to insert, 1 to insert into root tree
                storage_cost: StorageCost {
                    added_bytes: 153,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 2,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_empty_tree_cost() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::empty_tree(),
                None,
                Some(&tx),
                grove_version,
            )
            .cost;
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 38
        //   1 for the flag option (but no flags)
        //   1 for the enum type tree
        //   1 for empty option
        //   1 for no sum feature
        // 32 for node hash
        // 0 for value hash (trees have this for free)
        // 2 byte for the value_size (required space for 98 + x where x can be up to
        // 256)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total 37 + 38 + 40 = 115

        // Hash node calls
        // 1 for the node hash
        // 1 for the value hash
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // 1 to get tree, 1 to insert, 1 to insert into root tree
                storage_cost: StorageCost {
                    added_bytes: 115,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 3, // todo: verify this
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_empty_sum_tree_cost() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::empty_sum_tree(),
                None,
                Some(&tx),
                grove_version,
            )
            .cost;
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 47
        //   1 for the flag option (but no flags)
        //   1 for the enum type tree
        //   1 for empty option
        //   1 for no sum feature
        //   9 bytes for sum
        // 32 for node hash
        // 0 for value hash (trees have this for free)
        // 2 byte for the value_size (required space for 98 + x where x can be up to
        // 256)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total 37 + 47 + 40 = 124

        // Hash node calls
        // 1 for the node hash
        // 1 for the value hash
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // 1 to get tree, 1 to insert, 1 to insert into root tree
                storage_cost: StorageCost {
                    added_bytes: 124,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 3, // todo: verify this
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_empty_tree_cost_with_flags() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::empty_tree_with_flags(Some(b"cat".to_vec())),
                None,
                Some(&tx),
                grove_version,
            )
            .cost;
        // Explanation for 183 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 42
        //   1 for the flag option
        //   1 byte for flag size
        //   3 bytes for flags
        //   1 for the enum type tree
        //   1 for empty option
        //   1 for no sum feature
        // 32 for node hash
        // 0 for value hash (trees have this for free)
        // 2 byte for the value_size (required space for 98 + x where x can be up to
        // 256)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total 37 + 42 + 40 = 119

        // Hash node calls
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        // 1 for the combine hash
        // The node hash is not being called, as the root hash isn't cached
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // 1 to get tree, 1 to insert, 1 to insert into root tree
                storage_cost: StorageCost {
                    added_bytes: 119,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 3,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_item_cost_under_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                b"key1",
                Element::new_item(b"test".to_vec()),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .unwrap();

        // Explanation for 152 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 73
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for size of test bytes
        //   4 for test bytes
        //   1 for a basic merk
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 72)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total 37 + 73 + 40 = 150

        // Explanation for replaced bytes

        // Replaced parent Value -> 78
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for an empty option
        //   1 for a basic merk
        // 32 for node hash
        // 40 for the parent hook
        // 2 byte for the value_size
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 5, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 150,
                    replaced_bytes: 78,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 152, // todo: verify this
                hash_node_calls: 8,        // todo: verify this
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_item_cost_under_count_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_count_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                b"key1",
                Element::new_item(b"test".to_vec()),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .unwrap();

        // Explanation for 152 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 81
        //   1 for the enum type item
        //   1 for size of test bytes
        //   4 for test bytes
        //   1 for the flag option (but no flags)
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 9 for Count node
        // 1 byte for the value_size (required space for 1)

        // Parent Hook -> 48
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Count Merk 9
        // Child Heights 2

        // Total 37 + 81 + 48 = 166

        // Explanation for replaced bytes

        // Replaced parent Value -> 86
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for an empty option
        //   1 for the count merk
        //   9 for the count
        // 32 for node hash
        // 40 for the parent hook
        // 2 byte for the value_size
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 5, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 166,
                    replaced_bytes: 87,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 162, // todo: verify this
                hash_node_calls: 8,        // todo: verify this
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_item_with_apple_flags_cost() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::new_item_with_flags(b"test".to_vec(), Some(b"apple".to_vec())),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .unwrap();

        // Explanation for 152 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 79
        //   1 for the flag option
        //   1 for flags byte size
        //   5 for flags bytes
        //   1 for the enum type
        //   1 for size of test bytes
        //   4 for test bytes
        //   1 for a basic merk
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 77)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total 37 + 79 + 40 = 156

        // Hash node calls
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        // The node hash is not being called, as the root hash isn't cached
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 156,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 2,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_item_with_flags_cost_under_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                b"key1",
                Element::new_item_with_flags(b"test".to_vec(), Some(b"apple".to_vec())),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .unwrap();

        // Explanation for 152 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 79
        //   1 for the flag option
        //   1 for flags byte size
        //   5 for flags bytes
        //   1 for the enum type
        //   1 for size of test bytes
        //   4 for test bytes
        //   1 for the basic merk
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 78)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total 37 + 79 + 40 = 156

        // Explanation for replaced bytes

        // Replaced parent Value -> 78
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for an empty option
        //   1 for a basic merk
        // 32 for node hash
        // 40 for the parent hook
        // 2 byte for the value_size

        // Hash node calls
        // 1 for getting the merk
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        // 2 for the node hash

        // on the level above
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        // 1 for the combine hash

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 5, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 156,
                    replaced_bytes: 78,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 152, // todo: verify this
                hash_node_calls: 8,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_insert_item_with_flags_cost_under_tree_with_flags() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree_with_flags(Some(b"cat".to_vec())),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                b"key1",
                Element::new_item_with_flags(b"test".to_vec(), Some(b"apple".to_vec())),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .unwrap();

        // Explanation for 152 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 79
        //   1 for the flag option
        //   1 for flags byte size
        //   5 for flags bytes
        //   1 for the enum type
        //   1 for size of test bytes
        //   4 for test bytes
        //   1 for basic merk
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 78)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1
        // Total 37 + 79 + 40 = 156

        // Explanation for replaced bytes

        // Replaced parent Value -> 82
        //   1 for the flag option
        //   3 bytes for flags
        //   1 for flags size
        //   1 for the enum type
        //   1 for an empty option
        //   1 for basic merk
        // 32 for node hash
        // 0 for value hash (trees have this for free)
        // 40 for the child to parent hook
        // 2 byte for the value_size (required space)

        // Hash node calls
        // 1 for getting the merk
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        // 2 for the node hash

        // on the level above
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash
        // 1 for the combine hash

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 5, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 156,
                    replaced_bytes: 82,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 160, // todo: verify this
                hash_node_calls: 8,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_update_item_same_cost_at_root() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"key1",
            Element::new_item(b"cat".to_vec()),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::new_item(b"dog".to_vec()),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");

        // Explanation for 110 replaced bytes

        // Value -> 72
        //   1 for the flag option (but no flags)
        //   1 for the enum type item
        //   3 for "cat"
        //   1 for cat length
        //   1 for basic merk
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 1 byte for the value_size (required space for 71)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // 72 + 40 = 112

        // Hash node calls
        // 1 for the kv_digest_to_kv_hash hash
        // 1 for the value hash

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 112,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 77,
                hash_node_calls: 2,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_update_same_cost_in_underlying_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        db.insert(
            [b"tree".as_slice()].as_ref(),
            b"key1",
            Element::new_item(b"cat".to_vec()),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                b"key1",
                Element::new_item(b"dog".to_vec()),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 190,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 230, // todo verify this
                hash_node_calls: 8,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_update_same_cost_in_underlying_sum_tree_bigger_sum_item() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_sum_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        db.insert(
            [b"tree".as_slice()].as_ref(),
            [0; 32].as_slice(),
            Element::new_sum_item(15),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                [0; 32].as_slice(),
                Element::new_sum_item(1000000),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 248,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 266, // todo verify this
                hash_node_calls: 9,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_update_same_cost_in_underlying_sum_tree_bigger_sum_item_parent_sum_tree_already_big(
    ) {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_sum_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        db.insert(
            [b"tree".as_slice()].as_ref(),
            [1; 32].as_slice(),
            Element::new_sum_item(1000000),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        db.insert(
            [b"tree".as_slice()].as_ref(),
            [0; 32].as_slice(),
            Element::new_sum_item(15),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                [0; 32].as_slice(),
                Element::new_sum_item(1000000),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 9, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 409, // todo: verify this
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 487, // todo verify this
                hash_node_calls: 11,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_update_same_cost_in_underlying_sum_tree_smaller_sum_item() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_sum_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        db.insert(
            [b"tree".as_slice()].as_ref(),
            [0; 32].as_slice(),
            Element::new_sum_item(1000000),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                [0; 32].as_slice(),
                Element::new_sum_item(15),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 248,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 276, // todo verify this
                hash_node_calls: 9,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_update_bigger_cost() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        db.insert(
            [b"tree".as_slice()].as_ref(),
            b"key1",
            Element::new_item(b"test".to_vec()),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                b"key1",
                Element::new_item(b"test1".to_vec()),
                None,
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 1,
                    replaced_bytes: 191, // todo: verify this
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 231,
                hash_node_calls: 8,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_one_update_tree_bigger_cost_with_flags() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        db.insert(
            [b"tree".as_slice()].as_ref(),
            b"key1",
            Element::new_tree(None),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();

        let cost = db
            .insert(
                [b"tree".as_slice()].as_ref(),
                b"key1",
                Element::new_tree_with_flags(None, Some(b"cat".to_vec())),
                Some(InsertOptions {
                    validate_insertion_does_not_override: false,
                    validate_insertion_does_not_override_tree: false,
                    base_root_storage_is_free: true,
                }),
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to insert");

        // Explanation for 4 added bytes

        // 1 for size of "cat" flags
        // 3 for bytes

        // Explanation for replaced bytes

        // Replaced parent Value -> 78
        //   1 for the flag option (but no flags)
        //   1 for the enum type tree
        //   1 for empty option
        //   1 for Basic Merk
        // 32 for node hash
        // 0 for value hash (trees have this for free)
        // 40 for child to parent hook
        // 2 byte for the value_size (required space for 98 + x where x can be up to
        // 256)

        // Replaced current tree -> 78
        //   1 for the flag option (but no flags)
        //   1 for the enum type tree
        //   1 for empty option
        //   1 for Basic Merk
        // 32 for node hash
        // 0 for value hash (trees have this for free)
        // 40 for child to parent hook
        // 2 byte for the value_size (required space for 98 + x where x can be up to
        // 256)

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 4,
                    replaced_bytes: 156,
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 227,
                hash_node_calls: 9, // todo: verify this
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn insert_accepts_255_byte_key() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let key = vec![0xAA; 255];
        db.insert(
            [TEST_LEAF].as_ref(),
            &key,
            Element::new_item(b"val".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("255-byte key should be accepted");
    }

    #[test]
    fn insert_rejects_256_byte_key() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let key = vec![0xBB; 256];
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                &key,
                Element::new_item(b"val".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(..))));
    }

    #[test]
    fn insert_if_not_exists_rejects_256_byte_key() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let key = vec![0xBB; 256];
        let result = db
            .insert_if_not_exists(
                [TEST_LEAF].as_ref(),
                &key,
                Element::new_item(b"val".to_vec()),
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(..))));
    }

    #[test]
    fn insert_if_changed_value_rejects_256_byte_key() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let key = vec![0xBB; 256];
        let result = db
            .insert_if_changed_value(
                [TEST_LEAF].as_ref(),
                &key,
                Element::new_item(b"val".to_vec()),
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(..))));
    }

    /// In-place conversion guard: overriding a populated PLAIN tree with an
    /// indexed element of byte-compatible primary shape must be rejected.
    ///
    /// A plain `ProvableCountTree`'s subtree is exactly what a PCIT primary
    /// looks like on disk (same feature type, same aggregate), so a PCIT
    /// element carrying that tree's real root key and count passes the
    /// root-key, variant-shape and aggregate checks — but its Count secondary
    /// would start EMPTY over the populated primary, the same no-reindex
    /// hazard as an axes schema change. Same for PCPST -> PCPSIT.
    fn indexed_conversion_of_plain_tree_is_rejected(gv: &GroveVersion) {
        use grovedb_element::indexed::IndexAxis;

        let db = make_test_grovedb(gv);

        // PCT -> PCIT.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pct",
            Element::empty_provable_count_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create plain PCT");
        for k in [b"a".as_slice(), b"b".as_slice()] {
            db.insert(
                [TEST_LEAF, b"pct"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("populate PCT");
        }
        let (root_key, count) = match db
            .get([TEST_LEAF].as_ref(), b"pct", None, gv)
            .unwrap()
            .expect("get PCT")
        {
            Element::ProvableCountTree(rk, c, _) => (rk, c),
            other => panic!("expected PCT, got {other:?}"),
        };
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"pct",
                Element::ProvableCountIndexedTree(root_key, Some(b"sk".to_vec()), count, None),
                Some(override_tree_opts()),
                None,
                gv,
            )
            .unwrap();
        assert!(
            matches!(
                &result,
                Err(Error::InvalidInput(m)) if m.contains("existing tree of a different type")
            ),
            "PCT -> PCIT conversion must be rejected, got {result:?}"
        );

        // PCPST -> PCPSIT.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpst",
            Element::empty_provable_count_provable_sum_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create plain PCPST");
        db.insert(
            [TEST_LEAF, b"pcpst"].as_ref(),
            b"a",
            Element::new_item_with_sum_item(b"v".to_vec(), 7),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("populate PCPST");
        let (root_key, count, sum) = match db
            .get([TEST_LEAF].as_ref(), b"pcpst", None, gv)
            .unwrap()
            .expect("get PCPST")
        {
            Element::ProvableCountProvableSumTree(rk, c, s, _) => (rk, c, s),
            other => panic!("expected PCPST, got {other:?}"),
        };
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcpst",
                Element::ProvableCountProvableSumIndexedTree(
                    root_key,
                    count,
                    sum,
                    vec![(IndexAxis::Count.tag(), None)],
                    None,
                ),
                Some(override_tree_opts()),
                None,
                gv,
            )
            .unwrap();
        assert!(
            matches!(
                &result,
                Err(Error::InvalidInput(m)) if m.contains("existing tree of a different type")
            ),
            "PCPST -> PCPSIT conversion must be rejected, got {result:?}"
        );

        // Nothing was persisted by the rejected writes.
        let issues = db
            .verify_grovedb(None, true, true, gv)
            .expect("verify_grovedb");
        assert!(issues.is_empty(), "issues: {issues:?}");
    }

    #[test]
    fn indexed_conversion_of_plain_tree_rejected_v0() {
        use grovedb_version::version::v1::GROVE_V1;
        indexed_conversion_of_plain_tree_is_rejected(&GROVE_V1);
    }

    #[test]
    fn indexed_conversion_of_plain_tree_rejected_v1() {
        indexed_conversion_of_plain_tree_is_rejected(GroveVersion::latest());
    }
}
