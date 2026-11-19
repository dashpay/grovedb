//! The unit tests of `StorageBatch` (`storage/src/storage.rs`, `mod
//! tests`), run against the staged batch.
//!
//! The body of each test is the one the storage crate has, under the same
//! name. What stands in for the storage crate is at the top of this file:
//! the calls of a `StorageBatch`, the operation an outcome stands for, and
//! the three cost types the tests name, which the engine does not know.
//!
//! One test differs: `test_debug_output_batch_operation` expects the
//! subtree in what an operation shows, which a storage batch has inside
//! its key. `the_tests_are_those_of_the_storage_crate` holds the others to
//! the source of the storage crate.
//!
//! The other seven run twice: with the data column holding flat records,
//! and with the data column holding the nodes of a Merk.

use std::{fmt, ops::Add};

use lignin_tree::hash::Stale;

use crate::{
    batch::{Outcome, StagedBatch, Written},
    txn::{Column, Hosted, NodePut, Subtree, Unit, ValuePut},
};

type ChildrenSizesWithIsSumTree = Option<()>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum StorageRemovedBytes {
    #[default]
    NoStorageRemoval,
    BasicStorageRemoval(u32),
}

impl Add for StorageRemovedBytes {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        use StorageRemovedBytes::{BasicStorageRemoval, NoStorageRemoval};
        match (self, rhs) {
            (NoStorageRemoval, other) | (other, NoStorageRemoval) => other,
            (BasicStorageRemoval(a), BasicStorageRemoval(b)) => BasicStorageRemoval(a + b),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct StorageCost {
    added_bytes: u32,
    replaced_bytes: u32,
    removed_bytes: StorageRemovedBytes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KeyValueStorageCost {
    key_storage_cost: StorageCost,
    value_storage_cost: StorageCost,
    new_node: bool,
    needs_value_verification: bool,
    prepaid: bool,
}

impl KeyValueStorageCost {
    fn combined_removed_bytes(self) -> StorageRemovedBytes {
        self.key_storage_cost.removed_bytes + self.value_storage_cost.removed_bytes
    }
}

/// The operation of a storage batch that an outcome stands for.
#[allow(dead_code)]
enum AbstractBatchOperation {
    Put {
        key: Vec<u8>,
        value: Vec<u8>,
        children_sizes: ChildrenSizesWithIsSumTree,
        cost_info: Option<KeyValueStorageCost>,
    },
    PutAux {
        key: Vec<u8>,
        value: Vec<u8>,
        cost_info: Option<KeyValueStorageCost>,
    },
    PutRoot {
        key: Vec<u8>,
        value: Vec<u8>,
        cost_info: Option<KeyValueStorageCost>,
    },
    PutMeta {
        key: Vec<u8>,
        value: Vec<u8>,
        cost_info: Option<KeyValueStorageCost>,
    },
    Delete {
        key: Vec<u8>,
        cost_info: Option<KeyValueStorageCost>,
    },
    DeleteAux {
        key: Vec<u8>,
        cost_info: Option<KeyValueStorageCost>,
    },
    DeleteRoot {
        key: Vec<u8>,
        cost_info: Option<KeyValueStorageCost>,
    },
    DeleteMeta {
        key: Vec<u8>,
        cost_info: Option<KeyValueStorageCost>,
    },
}

impl AbstractBatchOperation {
    fn of(outcome: Outcome<'_, Option<KeyValueStorageCost>>) -> Self {
        let key = outcome.key.to_vec();
        let cost_info = outcome.tag.clone();
        let value = match outcome.write {
            Written::Record(value) => Some(value.to_vec()),
            Written::Node(node) => Some(node.value.as_deref().unwrap_or_default().to_vec()),
            Written::Removed | Written::NodeRemoved => None,
        };
        match (outcome.unit.column, value) {
            (Column::Data, Some(value)) => Self::Put {
                key,
                value,
                children_sizes: None,
                cost_info,
            },
            (Column::Aux, Some(value)) => Self::PutAux {
                key,
                value,
                cost_info,
            },
            (Column::Roots, Some(value)) => Self::PutRoot {
                key,
                value,
                cost_info,
            },
            (Column::Meta, Some(value)) => Self::PutMeta {
                key,
                value,
                cost_info,
            },
            (Column::Data, None) => Self::Delete { key, cost_info },
            (Column::Aux, None) => Self::DeleteAux { key, cost_info },
            (Column::Roots, None) => Self::DeleteRoot { key, cost_info },
            (Column::Meta, None) => Self::DeleteMeta { key, cost_info },
        }
    }

    /// Makes the call the operation is on `batch`.
    fn stage<const NODES: bool>(self, batch: &Shim<NODES>) {
        match self {
            Self::Put {
                key,
                value,
                children_sizes,
                cost_info,
            } => batch.put(key, value, children_sizes, cost_info),
            Self::PutAux {
                key,
                value,
                cost_info,
            } => batch.put_aux(key, value, cost_info),
            Self::PutRoot {
                key,
                value,
                cost_info,
            } => batch.put_root(key, value, cost_info),
            Self::PutMeta {
                key,
                value,
                cost_info,
            } => batch.put_meta(key, value, cost_info),
            Self::Delete { key, cost_info } => batch.delete(key, cost_info),
            Self::DeleteAux { key, cost_info } => batch.delete_aux(key, cost_info),
            Self::DeleteRoot { key, cost_info } => batch.delete_root(key, cost_info),
            Self::DeleteMeta { key, cost_info } => batch.delete_meta(key, cost_info),
        }
    }
}

/// An operation is shown as the batch shows the outcome it is.
impl fmt::Debug for AbstractBatchOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let batch = Shim::<false>::new();
        let again = match self {
            Self::Put { key, value, .. } => Self::Put {
                key: key.clone(),
                value: value.clone(),
                children_sizes: None,
                cost_info: None,
            },
            Self::PutAux { key, value, .. } => Self::PutAux {
                key: key.clone(),
                value: value.clone(),
                cost_info: None,
            },
            Self::PutRoot { key, value, .. } => Self::PutRoot {
                key: key.clone(),
                value: value.clone(),
                cost_info: None,
            },
            Self::PutMeta { key, value, .. } => Self::PutMeta {
                key: key.clone(),
                value: value.clone(),
                cost_info: None,
            },
            Self::Delete { key, .. } => Self::Delete {
                key: key.clone(),
                cost_info: None,
            },
            Self::DeleteAux { key, .. } => Self::DeleteAux {
                key: key.clone(),
                cost_info: None,
            },
            Self::DeleteRoot { key, .. } => Self::DeleteRoot {
                key: key.clone(),
                cost_info: None,
            },
            Self::DeleteMeta { key, .. } => Self::DeleteMeta {
                key: key.clone(),
                cost_info: None,
            },
        };
        again.stage(&batch);
        let resolved = batch.0.resolve().expect("one call");
        let outcome = resolved.outcomes().next().expect("one outcome");
        write!(f, "{outcome:?}")
    }
}

/// The subtree every key of these tests is in: a storage batch is handed
/// keys with their prefix.
const PREFIX: Subtree = Subtree::new([0; 32]);

/// The calls of a `StorageBatch` on a staged batch. With `NODES` the data
/// column holds the nodes of a Merk, and with none flat records.
struct Shim<const NODES: bool>(StagedBatch<Option<KeyValueStorageCost>>);

impl<const NODES: bool> Shim<NODES> {
    fn new() -> Self {
        Self(StagedBatch::new())
    }

    fn len(&self) -> usize {
        self.0.len()
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn put_in(&self, column: Column, key: &[u8], value: &[u8], cost: Option<KeyValueStorageCost>) {
        self.0.put(Unit::new(column, PREFIX), key, value, cost);
    }

    fn delete_in(&self, column: Column, key: &[u8], cost: Option<KeyValueStorageCost>) {
        self.0.delete(Unit::new(column, PREFIX), key, cost);
    }

    fn put(
        &self,
        key: Vec<u8>,
        value: Vec<u8>,
        _children_sizes: ChildrenSizesWithIsSumTree,
        cost_info: Option<KeyValueStorageCost>,
    ) {
        if !NODES {
            return self.put_in(Column::Data, &key, &value, cost_info);
        }
        let node = NodePut {
            key: &key,
            feature: &[0],
            value_hash: &[1; 32],
            kv_hash: &[2; 32],
            left: None,
            right: None,
            hosted: Hosted::Keep,
            value: ValuePut::Value(&value),
            stale: Stale::NONE,
            at: None,
        };
        self.0.put_node(PREFIX, &node, cost_info);
    }

    fn put_aux(&self, key: Vec<u8>, value: Vec<u8>, cost_info: Option<KeyValueStorageCost>) {
        self.put_in(Column::Aux, &key, &value, cost_info);
    }

    fn put_root(&self, key: Vec<u8>, value: Vec<u8>, cost_info: Option<KeyValueStorageCost>) {
        self.put_in(Column::Roots, &key, &value, cost_info);
    }

    fn put_meta(&self, key: Vec<u8>, value: Vec<u8>, cost_info: Option<KeyValueStorageCost>) {
        self.put_in(Column::Meta, &key, &value, cost_info);
    }

    fn delete(&self, key: Vec<u8>, cost_info: Option<KeyValueStorageCost>) {
        match NODES {
            true => self.0.delete_node(PREFIX, &key, cost_info),
            false => self.delete_in(Column::Data, &key, cost_info),
        }
    }

    fn delete_aux(&self, key: Vec<u8>, cost_info: Option<KeyValueStorageCost>) {
        self.delete_in(Column::Aux, &key, cost_info);
    }

    fn delete_root(&self, key: Vec<u8>, cost_info: Option<KeyValueStorageCost>) {
        self.delete_in(Column::Roots, &key, cost_info);
    }

    fn delete_meta(&self, key: Vec<u8>, cost_info: Option<KeyValueStorageCost>) {
        self.delete_in(Column::Meta, &key, cost_info);
    }

    fn next_operation(&self) {
        self.0.next_operation();
    }

    fn merge(&self, other: Self) {
        self.0.merge(other.0);
    }

    fn merge_overwriting(&self, other: Self) {
        self.0.merge_overwriting(other.0);
    }

    fn into_iter(self) -> std::vec::IntoIter<AbstractBatchOperation> {
        let resolved = self.0.resolve().expect("no call is refused");
        let operations: Vec<_> = (resolved.outcomes())
            .map(AbstractBatchOperation::of)
            .collect();
        operations.into_iter()
    }
}

/// As in the storage crate, with the subtree in what an operation shows.
#[test]
fn test_debug_output_batch_operation() {
    let op1 = AbstractBatchOperation::PutMeta {
        key: b"key1".to_vec(),
        value: b"value1".to_vec(),
        cost_info: None,
    };
    let op2 = AbstractBatchOperation::DeleteRoot {
        key: b"key1".to_vec(),
        cost_info: None,
    };
    assert_eq!(
        format!("{op1:?}"),
        "PutMeta { subtree: Subtree(00000000..), key: \"[hex: 6b657931, str: key1]\", value: \"[hex: \
         76616c756531, str: value1]\" }"
    );
    assert_eq!(
        format!("{op2:?}"),
        "DeleteRoot { subtree: Subtree(00000000..), key: \"[hex: 6b657931, str: key1]\" }"
    );
}

/// The tests below are the tests of the storage crate: every test its
/// `mod tests` has is here under its name, and each body but the one above
/// is the same but for white space.
#[test]
fn the_tests_are_those_of_the_storage_crate() {
    let theirs = include_str!("../../../../storage/src/storage.rs");
    let theirs = &theirs[theirs
        .find("mod tests {")
        .expect("the tests of the storage crate")..];
    let ours = include_str!("storage_batch.rs");
    // A test is named where it is declared: `fn`, a space and the name.
    let declared = ["fn", " test_"].concat();
    let names = |source: &str| -> Vec<String> {
        let tests = source.split(declared.as_str()).skip(1);
        let mut names: Vec<String> = tests
            .filter_map(|test| Some(["test_", test.split_once("()")?.0].concat()))
            .collect();
        names.sort();
        names.dedup();
        names
    };
    assert_eq!(names(ours), names(theirs));
    assert_eq!(names(theirs).len(), 8);

    let body = |source: &str, name: &str| -> String {
        let from = source.find(&format!("fn {name}()")).expect("the test");
        let mut depth = 0;
        let mut body = String::new();
        for at in source[from..].chars() {
            match at {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            if !at.is_whitespace() {
                body.push(at);
            }
            if at == '}' && depth == 0 {
                break;
            }
        }
        body
    };
    for name in names(theirs) {
        if name != "test_debug_output_batch_operation" {
            assert_eq!(body(ours, &name), body(theirs, &name), "{name}");
        }
    }
}

macro_rules! storage_batch_tests {
    ($data:ident, $nodes:literal) => {
        mod $data {
            use super::{
                AbstractBatchOperation, ChildrenSizesWithIsSumTree, KeyValueStorageCost,
                StorageCost, StorageRemovedBytes::BasicStorageRemoval,
            };

            type StorageBatch = super::Shim<$nodes>;

            fn dummy_children_sizes() -> ChildrenSizesWithIsSumTree {
                None
            }

            fn removed_bytes_cost(bytes: u32) -> KeyValueStorageCost {
                KeyValueStorageCost {
                    key_storage_cost: StorageCost {
                        added_bytes: 0,
                        replaced_bytes: 0,
                        removed_bytes: BasicStorageRemoval(bytes),
                    },
                    value_storage_cost: StorageCost::default(),
                    new_node: false,
                    needs_value_verification: false,
                    prepaid: false,
                }
            }

            #[test]
            fn test_storage_batch_put_then_delete_keeps_put_operation() {
                let batch = StorageBatch::new();

                batch.put(
                    b"key".to_vec(),
                    b"value".to_vec(),
                    dummy_children_sizes(),
                    None,
                );
                batch.delete(b"key".to_vec(), None);

                assert_eq!(batch.len(), 1);
                let operations: Vec<_> = batch.into_iter().collect();
                assert_eq!(operations.len(), 1);
                assert!(matches!(operations[0], AbstractBatchOperation::Put { .. }));
            }

            #[test]
            fn test_storage_batch_delete_then_put_overwrites_delete() {
                let batch = StorageBatch::new();

                batch.delete(b"key".to_vec(), Some(removed_bytes_cost(5)));
                batch.put(
                    b"key".to_vec(),
                    b"value".to_vec(),
                    dummy_children_sizes(),
                    None,
                );

                assert_eq!(batch.len(), 1);
                let operations: Vec<_> = batch.into_iter().collect();
                assert_eq!(operations.len(), 1);
                assert!(matches!(operations[0], AbstractBatchOperation::Put { .. }));
            }

            #[test]
            fn test_storage_batch_duplicate_delete_does_not_replace_cost_info() {
                let batch = StorageBatch::new();

                batch.delete_aux(b"key".to_vec(), Some(removed_bytes_cost(3)));
                batch.delete_aux(b"key".to_vec(), Some(removed_bytes_cost(7)));

                let mut operations = batch.into_iter();
                match operations.next().expect("expected one operation") {
                    AbstractBatchOperation::DeleteAux { cost_info, .. } => {
                        let removed = cost_info.expect("cost info should be set");
                        assert_eq!(removed.combined_removed_bytes(), BasicStorageRemoval(3));
                    }
                    op => panic!("unexpected operation: {op:?}"),
                }
                assert!(operations.next().is_none());
            }

            #[test]
            fn test_storage_batch_later_operation_delete_supersedes_earlier_put() {
                let batch = StorageBatch::new();

                // Operation 1 (e.g. a Merk commit whose rebalancing rewrote a node).
                batch.put(
                    b"key".to_vec(),
                    b"value".to_vec(),
                    dummy_children_sizes(),
                    None,
                );
                batch.next_operation();
                // Operation 2 deletes that very node: it must win.
                batch.delete(b"key".to_vec(), Some(removed_bytes_cost(5)));

                let operations: Vec<_> = batch.into_iter().collect();
                assert_eq!(operations.len(), 1);
                assert!(matches!(
                    operations[0],
                    AbstractBatchOperation::Delete { .. }
                ));
            }

            #[test]
            fn test_storage_batch_put_wins_within_one_operation_after_boundary() {
                let batch = StorageBatch::new();
                batch.next_operation();

                // Within the new operation the put-wins rule is unchanged, in both
                // orders.
                batch.put(b"a".to_vec(), b"1".to_vec(), dummy_children_sizes(), None);
                batch.delete(b"a".to_vec(), None);
                batch.delete(b"b".to_vec(), None);
                batch.put(b"b".to_vec(), b"2".to_vec(), dummy_children_sizes(), None);

                let variants: Vec<_> = batch
                    .into_iter()
                    .map(|op| matches!(op, AbstractBatchOperation::Put { .. }))
                    .collect();
                assert_eq!(variants, vec![true, true]);
            }

            #[test]
            fn test_storage_batch_merge_overwriting_settles_generation() {
                let batch = StorageBatch::new();
                let other = StorageBatch::new();
                other.put(b"key".to_vec(), b"v".to_vec(), dummy_children_sizes(), None);

                batch.merge_overwriting(other);
                // The merged put is an earlier operation's outcome: a delete issued
                // afterwards removes the key.
                batch.delete(b"key".to_vec(), None);

                let operations: Vec<_> = batch.into_iter().collect();
                assert_eq!(operations.len(), 1);
                assert!(matches!(
                    operations[0],
                    AbstractBatchOperation::Delete { .. }
                ));
            }

            #[test]
            fn test_storage_batch_merge_and_iteration_order() {
                let batch = StorageBatch::new();
                let other = StorageBatch::new();

                other.put(b"d".to_vec(), b"1".to_vec(), dummy_children_sizes(), None);
                other.put_root(b"r".to_vec(), b"2".to_vec(), None);
                other.put_aux(b"a".to_vec(), b"3".to_vec(), None);
                other.put_meta(b"m".to_vec(), b"4".to_vec(), None);
                other.delete_meta(b"m_del".to_vec(), Some(removed_bytes_cost(9)));

                batch.merge(other);

                assert_eq!(batch.len(), 5);
                assert!(!batch.is_empty());

                let variants: Vec<_> = batch
                    .into_iter()
                    .map(|op| match op {
                        AbstractBatchOperation::Put { .. } => "Put",
                        AbstractBatchOperation::PutAux { .. } => "PutAux",
                        AbstractBatchOperation::PutRoot { .. } => "PutRoot",
                        AbstractBatchOperation::PutMeta { .. } => "PutMeta",
                        AbstractBatchOperation::Delete { .. } => "Delete",
                        AbstractBatchOperation::DeleteAux { .. } => "DeleteAux",
                        AbstractBatchOperation::DeleteRoot { .. } => "DeleteRoot",
                        AbstractBatchOperation::DeleteMeta { .. } => "DeleteMeta",
                    })
                    .collect();

                // Iterator traverses maps in this fixed order: meta, aux, roots, data.
                assert_eq!(
                    variants,
                    vec!["PutMeta", "DeleteMeta", "PutAux", "PutRoot", "Put"]
                );
            }
        }
    };
}

storage_batch_tests!(flat_records, false);
storage_batch_tests!(merk_nodes, true);
