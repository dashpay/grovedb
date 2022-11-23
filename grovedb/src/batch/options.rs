use merk::MerkOptions;
use crate::operations::delete::DeleteOptions;
use crate::operations::insert::InsertOptions;

#[derive(Debug, Clone)]
pub struct BatchApplyOptions {
    pub validate_insertion_does_not_override: bool,
    pub validate_insertion_does_not_override_tree: bool,
    pub allow_deleting_non_empty_trees: bool,
    pub deleting_non_empty_trees_returns_error: bool,
    pub disable_operation_consistency_check: bool,
    pub base_root_storage_is_free: bool,
}

impl Default for BatchApplyOptions {
    fn default() -> Self {
        BatchApplyOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: false,
            allow_deleting_non_empty_trees: false,
            deleting_non_empty_trees_returns_error: true,
            disable_operation_consistency_check: false,
            base_root_storage_is_free: true,
        }
    }
}

impl BatchApplyOptions {
    pub(crate) fn as_insert_options(&self) -> InsertOptions {
        InsertOptions {
            validate_insertion_does_not_override: self.validate_insertion_does_not_override,
            validate_insertion_does_not_override_tree: self
                .validate_insertion_does_not_override_tree,
            base_root_storage_is_free: self.base_root_storage_is_free,
        }
    }

    pub(crate) fn as_delete_options(&self) -> DeleteOptions where {
        DeleteOptions {
            allow_deleting_non_empty_trees: self.allow_deleting_non_empty_trees,
            deleting_non_empty_trees_returns_error: self.deleting_non_empty_trees_returns_error,
            base_root_storage_is_free: self.base_root_storage_is_free,
        }
    }

    pub(crate) fn as_merk_options(&self) -> MerkOptions {
        MerkOptions {
            base_root_storage_is_free: self.base_root_storage_is_free,
        }
    }
}