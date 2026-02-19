use crate::{
    error::validate_height,
    hash::{INTERNAL_DOMAIN_TAG, LEAF_DOMAIN_TAG},
    DenseMerkleError, DenseTreeStore,
};

/// A dense fixed-sized Merkle tree.
///
/// Positions are indexed level-order (BFS): root=0, left child=2i+1, right
/// child=2i+2. The tree has height `h` and capacity `2^h - 1`.
///
/// Note: root hash computation is O(n) per insert where n = count, since no
/// intermediate hashes are cached. Suitable for small trees (epoch sizes
/// typically 16-256).
#[derive(Debug, Clone, Copy)]
pub struct DenseFixedSizedMerkleTree {
    height: u8,
    count: u64,
}

impl DenseFixedSizedMerkleTree {
    /// Create a new empty tree with the given height.
    ///
    /// Height must be between 1 and 63 inclusive.
    pub fn new(height: u8) -> Result<Self, DenseMerkleError> {
        validate_height(height)?;
        Ok(Self { height, count: 0 })
    }

    /// Reconstitute a tree from stored state.
    pub fn from_state(height: u8, count: u64) -> Result<Self, DenseMerkleError> {
        validate_height(height)?;
        let capacity = (1u64 << height) - 1;
        if count > capacity {
            return Err(DenseMerkleError::InvalidData(format!(
                "count {} exceeds capacity {} for height {}",
                count, capacity, height
            )));
        }
        Ok(Self { height, count })
    }

    /// Maximum number of values this tree can hold.
    pub fn capacity(&self) -> u64 {
        (1u64 << self.height) - 1
    }

    /// Current number of values stored.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Height of the tree.
    pub fn height(&self) -> u8 {
        self.height
    }

    /// Insert a value at the next available position.
    ///
    /// Returns `(root_hash, position)` where position is the 0-based index
    /// where the value was inserted.
    pub fn insert<S: DenseTreeStore>(
        &mut self,
        value: &[u8],
        store: &S,
    ) -> Result<([u8; 32], u64, u32), DenseMerkleError> {
        if self.count >= self.capacity() {
            return Err(DenseMerkleError::TreeFull {
                capacity: self.capacity(),
                count: self.count,
            });
        }

        let position = self.count;
        store.put_value(position, value)?;
        self.count += 1;

        match self.compute_root_hash(store) {
            Ok((root_hash, hash_calls)) => Ok((root_hash, position, hash_calls)),
            Err(e) => {
                // Roll back count so the tree state remains consistent.
                // Note: the value remains in the store; the caller is
                // responsible for store-level cleanup if needed.
                self.count -= 1;
                Err(e)
            }
        }
    }

    /// Try to insert a value at the next available position.
    ///
    /// Returns `None` if the tree is full, otherwise returns
    /// `Some((root_hash, position, hash_calls))`.
    pub fn try_insert<S: DenseTreeStore>(
        &mut self,
        value: &[u8],
        store: &S,
    ) -> Result<Option<([u8; 32], u64, u32)>, DenseMerkleError> {
        if self.count >= self.capacity() {
            return Ok(None);
        }

        let position = self.count;
        store.put_value(position, value)?;
        self.count += 1;

        match self.compute_root_hash(store) {
            Ok((root_hash, hash_calls)) => Ok(Some((root_hash, position, hash_calls))),
            Err(e) => {
                self.count -= 1;
                Err(e)
            }
        }
    }

    /// Get a value by position.
    ///
    /// Returns `None` if position >= count. Returns an error if position <
    /// count but the store has no value (store inconsistency).
    pub fn get<S: DenseTreeStore>(
        &self,
        position: u64,
        store: &S,
    ) -> Result<Option<Vec<u8>>, DenseMerkleError> {
        if position >= self.count {
            return Ok(None);
        }
        let value = store.get_value(position)?.ok_or_else(|| {
            DenseMerkleError::StoreError(format!(
                "expected value at position {} but found none (count={})",
                position, self.count
            ))
        })?;
        Ok(Some(value))
    }

    /// Compute the root hash of the tree.
    ///
    /// Returns `([0u8; 32], 0)` if the tree is empty.
    /// Returns `(hash, hash_call_count)` otherwise.
    pub fn root_hash<S: DenseTreeStore>(
        &self,
        store: &S,
    ) -> Result<([u8; 32], u32), DenseMerkleError> {
        self.compute_root_hash(store)
    }

    /// Compute the hash of a specific position in the tree.
    ///
    /// This is a public wrapper around the internal `hash_node` method,
    /// useful for proof generation where sibling subtree hashes are needed.
    ///
    /// Returns `([0u8; 32], 0)` for positions beyond count or capacity.
    /// Returns `(hash, hash_call_count)` otherwise.
    pub(crate) fn hash_position<S: DenseTreeStore>(
        &self,
        position: u64,
        store: &S,
    ) -> Result<([u8; 32], u32), DenseMerkleError> {
        self.hash_node(position, store)
    }

    /// Internal recursive hash computation.
    fn compute_root_hash<S: DenseTreeStore>(
        &self,
        store: &S,
    ) -> Result<([u8; 32], u32), DenseMerkleError> {
        if self.count == 0 {
            return Ok(([0u8; 32], 0));
        }
        self.hash_node(0, store)
    }

    /// Recursively compute the hash of a node.
    ///
    /// Returns `(hash, hash_call_count)`.
    fn hash_node<S: DenseTreeStore>(
        &self,
        position: u64,
        store: &S,
    ) -> Result<([u8; 32], u32), DenseMerkleError> {
        let capacity = self.capacity();

        // Position beyond capacity or unfilled → zero hash
        if position >= capacity || position >= self.count {
            return Ok(([0u8; 32], 0));
        }

        let value = store.get_value(position)?.ok_or_else(|| {
            DenseMerkleError::StoreError(format!(
                "expected value at position {} but found none",
                position
            ))
        })?;

        // Check if this is a leaf BEFORE computing child indices to avoid
        // u64 overflow for positions near capacity in height-62/63 trees.
        // A node is a leaf when its left child (2*pos+1) would be >= capacity.
        // Equivalently: position >= (capacity - 1) / 2, which is overflow-safe.
        let first_leaf = (capacity - 1) / 2;
        if position >= first_leaf {
            // Leaf node: hash = blake3(0x00 || value)
            let mut hasher = blake3::Hasher::new();
            hasher.update(&[LEAF_DOMAIN_TAG]);
            hasher.update(&value);
            let hash = *hasher.finalize().as_bytes();
            return Ok((hash, 1));
        }

        // Internal node: hash = blake3(0x01 || H(value) || H(left) || H(right))
        let left_child = 2 * position + 1;
        let right_child = 2 * position + 2;
        let (left_hash, left_calls) = self.hash_node(left_child, store)?;
        let (right_hash, right_calls) = self.hash_node(right_child, store)?;

        let value_hash = blake3::hash(&value);
        let mut hasher = blake3::Hasher::new();
        hasher.update(&[INTERNAL_DOMAIN_TAG]);
        hasher.update(value_hash.as_bytes());
        hasher.update(&left_hash);
        hasher.update(&right_hash);
        let hash = *hasher.finalize().as_bytes();

        Ok((hash, 2 + left_calls + right_calls))
    }
}
