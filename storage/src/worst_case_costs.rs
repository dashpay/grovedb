//! GroveDB storage_cost layer worst case costs traits.

/// Worst Key Length should be implemented for items being used
/// for get_storage_context_cost path items
pub trait WorstKeyLength {
    /// the max length of the key
    fn max_length(&self) -> u8;
}
