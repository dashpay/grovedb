use grovedb_costs::storage_cost::removal::{
    StorageRemovedBytes, StorageRemovedBytes::BasicStorageRemoval,
};

use crate::{error::StorageFlagsError, ElementFlags, StorageFlags};

impl StorageFlags {
    pub fn split_removal_bytes(
        flags: &mut ElementFlags,
        removed_key_bytes: u32,
        removed_value_bytes: u32,
    ) -> Result<(StorageRemovedBytes, StorageRemovedBytes), StorageFlagsError> {
        let maybe_storage_flags =
            StorageFlags::from_element_flags_ref(flags).map_err(|mut e| {
                e.add_info("drive did not understand flags of item being updated");
                e
            })?;
        // if we removed key bytes then we removed the entire value
        match maybe_storage_flags {
            None => Ok((
                BasicStorageRemoval(removed_key_bytes),
                BasicStorageRemoval(removed_value_bytes),
            )),
            Some(storage_flags) => {
                Ok(storage_flags
                    .split_storage_removed_bytes(removed_key_bytes, removed_value_bytes))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use grovedb_costs::storage_cost::removal::StorageRemovedBytes;

    use crate::StorageFlags;

    #[test]
    fn split_removal_bytes_uses_basic_removal_without_flags() {
        let mut flags = vec![];
        let (key_removal, value_removal) =
            StorageFlags::split_removal_bytes(&mut flags, 10, 15).expect("expected split");

        assert_eq!(key_removal, StorageRemovedBytes::BasicStorageRemoval(10));
        assert_eq!(value_removal, StorageRemovedBytes::BasicStorageRemoval(15));
    }

    #[test]
    fn split_removal_bytes_uses_storage_flags_when_present() {
        let mut flags = StorageFlags::SingleEpoch(7).to_element_flags();
        let (key_removal, value_removal) =
            StorageFlags::split_removal_bytes(&mut flags, 2, 3).expect("expected split");
        let (expected_key, expected_value) =
            StorageFlags::SingleEpoch(7).split_storage_removed_bytes(2, 3);

        assert_eq!(key_removal, expected_key);
        assert_eq!(value_removal, expected_value);
    }

    #[test]
    fn split_removal_bytes_propagates_parse_error_with_context() {
        let mut flags = vec![255, 1, 2];
        let error = StorageFlags::split_removal_bytes(&mut flags, 1, 1)
            .expect_err("expected invalid flags to fail");

        let message = error.to_string();
        assert!(message.contains("unknown storage flags serialization"));
        assert!(message.contains("drive did not understand flags of item being updated"));
    }
}
