use grovedb_costs::{CostResult, CostsExt, OperationCost};

use crate::{
    hash::{node_hash, validate_height},
    DenseMerkleError, DenseTreeStore,
};

/// Unwrap a `CostResult`, accumulate its cost into `$cost`, and return early
/// (with accumulated cost) on error.
macro_rules! cost_return_on_error {
    ($cost:ident, $expr:expr) => {
        match $expr.unwrap_add_cost(&mut $cost) {
            Ok(x) => x,
            Err(e) => return Err(e).wrap_with_cost($cost),
        }
    };
}

/// A dense fixed-sized Merkle tree.
///
/// Positions are indexed level-order (BFS): root=0, left child=2i+1, right
/// child=2i+2. The tree has height `h` (max 16) and capacity `2^h - 1`.
///
/// Note: root hash computation is O(n) per insert where n = count, since no
/// intermediate hashes are cached. Suitable for small trees (epoch sizes
/// typically 16-256).
#[derive(Debug, Clone, Copy)]
pub struct DenseFixedSizedMerkleTree {
    height: u8,
    count: u16,
}

impl DenseFixedSizedMerkleTree {
    /// Create a new empty tree with the given height.
    ///
    /// Height must be between 1 and 16 inclusive.
    pub fn new(height: u8) -> Result<Self, DenseMerkleError> {
        validate_height(height)?;
        Ok(Self { height, count: 0 })
    }

    /// Reconstitute a tree from stored state.
    pub fn from_state(height: u8, count: u16) -> Result<Self, DenseMerkleError> {
        validate_height(height)?;
        let capacity = Self::capacity_for_height(height);
        if count > capacity {
            return Err(DenseMerkleError::InvalidData(format!(
                "count {} exceeds capacity {} for height {}",
                count, capacity, height
            )));
        }
        Ok(Self { height, count })
    }

    /// Maximum number of values this tree can hold.
    pub fn capacity(&self) -> u16 {
        Self::capacity_for_height(self.height)
    }

    /// Compute capacity from height. Height must be 1..=16.
    /// Uses u32 internally to avoid overflow since 1u16 << 16 would overflow.
    fn capacity_for_height(height: u8) -> u16 {
        ((1u32 << height) - 1) as u16
    }

    /// Current number of values stored.
    pub fn count(&self) -> u16 {
        self.count
    }

    /// Height of the tree.
    pub fn height(&self) -> u8 {
        self.height
    }

    /// Insert a value at the next available position.
    ///
    /// Returns `(root_hash, position)` where position is the 0-based index
    /// where the value was inserted. Storage and hash costs are tracked in the
    /// returned `OperationCost`.
    pub fn insert<S: DenseTreeStore>(
        &mut self,
        value: &[u8],
        store: &S,
    ) -> CostResult<([u8; 32], u16), DenseMerkleError> {
        let mut cost = OperationCost::default();

        if self.count >= self.capacity() {
            return Err(DenseMerkleError::TreeFull {
                capacity: self.capacity(),
                count: self.count,
            })
            .wrap_with_cost(cost);
        }

        let position = self.count;
        cost_return_on_error!(cost, store.put_value(position, value));
        self.count += 1;

        match self.compute_root_hash(store).unwrap_add_cost(&mut cost) {
            Ok(root_hash) => Ok((root_hash, position)).wrap_with_cost(cost),
            Err(e) => {
                // Roll back count so the tree state remains consistent.
                // Note: the value remains in the store; the caller is
                // responsible for store-level cleanup if needed.
                self.count -= 1;
                Err(e).wrap_with_cost(cost)
            }
        }
    }

    /// Try to insert a value at the next available position.
    ///
    /// Returns `None` if the tree is full, otherwise returns
    /// `Some((root_hash, position))`.
    pub fn try_insert<S: DenseTreeStore>(
        &mut self,
        value: &[u8],
        store: &S,
    ) -> CostResult<Option<([u8; 32], u16)>, DenseMerkleError> {
        let mut cost = OperationCost::default();

        if self.count >= self.capacity() {
            return Ok(None).wrap_with_cost(cost);
        }

        let position = self.count;
        cost_return_on_error!(cost, store.put_value(position, value));
        self.count += 1;

        match self.compute_root_hash(store).unwrap_add_cost(&mut cost) {
            Ok(root_hash) => Ok(Some((root_hash, position))).wrap_with_cost(cost),
            Err(e) => {
                self.count -= 1;
                Err(e).wrap_with_cost(cost)
            }
        }
    }

    /// Get a value by position.
    ///
    /// Returns `None` if position >= count. Returns an error if position <
    /// count but the store has no value (store inconsistency).
    pub fn get<S: DenseTreeStore>(
        &self,
        position: u16,
        store: &S,
    ) -> CostResult<Option<Vec<u8>>, DenseMerkleError> {
        let mut cost = OperationCost::default();

        if position >= self.count {
            return Ok(None).wrap_with_cost(cost);
        }

        let opt = cost_return_on_error!(cost, store.get_value(position));
        match opt {
            Some(v) => Ok(Some(v)).wrap_with_cost(cost),
            None => Err(DenseMerkleError::StoreError(format!(
                "expected value at position {} but found none (count={})",
                position, self.count
            )))
            .wrap_with_cost(cost),
        }
    }

    /// Compute the root hash of the tree.
    ///
    /// Returns `[0u8; 32]` if the tree is empty.
    pub fn root_hash<S: DenseTreeStore>(
        &self,
        store: &S,
    ) -> CostResult<[u8; 32], DenseMerkleError> {
        self.compute_root_hash(store)
    }

    /// Compute the hash of a specific position in the tree.
    ///
    /// This is a public wrapper around the internal `hash_node` method,
    /// useful for proof generation where sibling subtree hashes are needed.
    ///
    /// Returns `[0u8; 32]` for positions beyond count or capacity.
    pub(crate) fn hash_position<S: DenseTreeStore>(
        &self,
        position: u16,
        store: &S,
    ) -> CostResult<[u8; 32], DenseMerkleError> {
        self.hash_node(position, store)
    }

    /// Internal recursive hash computation.
    fn compute_root_hash<S: DenseTreeStore>(
        &self,
        store: &S,
    ) -> CostResult<[u8; 32], DenseMerkleError> {
        if self.count == 0 {
            return Ok([0u8; 32]).wrap_with_cost(OperationCost::default());
        }
        self.hash_node(0, store)
    }

    /// Recursively compute the hash of a node.
    ///
    /// All nodes use the same scheme: `blake3(H(value) || H(left) ||
    /// H(right))`. Leaf nodes simply have `[0; 32]` for both child hashes.
    fn hash_node<S: DenseTreeStore>(
        &self,
        position: u16,
        store: &S,
    ) -> CostResult<[u8; 32], DenseMerkleError> {
        let mut cost = OperationCost::default();
        let capacity = self.capacity();

        // Position beyond capacity or unfilled -> zero hash
        if position >= capacity || position >= self.count {
            return Ok([0u8; 32]).wrap_with_cost(cost);
        }

        let opt = cost_return_on_error!(cost, store.get_value(position));
        let value = match opt {
            Some(v) => v,
            None => {
                return Err(DenseMerkleError::StoreError(format!(
                    "expected value at position {} but found none",
                    position
                )))
                .wrap_with_cost(cost)
            }
        };

        let value_hash = *blake3::hash(&value).as_bytes();
        cost.hash_node_calls += 1; // value hash

        // Use u32 to avoid overflow for leaf positions near capacity.
        let left_child_u32 = 2 * position as u32 + 1;
        let right_child_u32 = 2 * position as u32 + 2;

        let left_hash = if left_child_u32 < capacity as u32 {
            cost_return_on_error!(cost, self.hash_node(left_child_u32 as u16, store))
        } else {
            [0u8; 32]
        };
        let right_hash = if right_child_u32 < capacity as u32 {
            cost_return_on_error!(cost, self.hash_node(right_child_u32 as u16, store))
        } else {
            [0u8; 32]
        };

        let hash = node_hash(&value_hash, &left_hash, &right_hash);
        cost.hash_node_calls += 1; // node_hash

        Ok(hash).wrap_with_cost(cost)
    }
}
