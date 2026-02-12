//! Binary serialization for commitment tree structures.
//!
//! Provides encode/decode functions for `PrunableTree`, `LocatedPrunableTree`,
//! and `Checkpoint` types from the `shardtree` crate.
//!
//! # Wire Format
//!
//! ## PrunableTree
//! - `NIL`:    `0x00`
//! - `LEAF`:   `0x01` + hash_bytes(32) + retention_flags(1 byte)
//! - `PARENT`: `0x02` + optional_ann(1 byte presence + optional 32 bytes hash)
//!   + left_tree + right_tree
//!
//! ## LocatedPrunableTree
//! - level(1 byte) + index(8 bytes BE) + prunable_tree
//!
//! ## Checkpoint
//! - tree_state tag (0x00 = Empty, 0x01 = AtPosition) + optional position(8
//!   bytes BE)
//! - marks_removed count(4 bytes BE) + \[position(8 bytes BE)\]...

use std::{
    io::{self, Read, Write},
    ops::Deref,
    sync::Arc,
};

use incrementalmerkletree::{Address, Level, Position};
use shardtree::{
    store::{Checkpoint, TreeState},
    LocatedPrunableTree, LocatedTree, Node, PrunableTree, RetentionFlags, Tree,
};

/// Trait for types that can be serialized to and from a fixed 32-byte
/// representation.
///
/// This is used to abstract hash serialization so that the serialization
/// functions are generic over the hash type.
pub trait HashSer: Sized {
    /// Read a hash from 32 bytes.
    fn hash_read<R: Read>(reader: &mut R) -> io::Result<Self>;
    /// Write a hash as 32 bytes.
    fn hash_write<W: Write>(&self, writer: &mut W) -> io::Result<()>;
}

impl HashSer for orchard::tree::MerkleHashOrchard {
    fn hash_read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut buf = [0u8; 32];
        reader.read_exact(&mut buf)?;
        // MerkleHashOrchard::from_bytes returns a CtOption; convert to Option.
        let opt: Option<Self> = Self::from_bytes(&buf).into();
        opt.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid hash bytes"))
    }

    fn hash_write<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&self.to_bytes())
    }
}

// Tag bytes for PrunableTree node types.
const TAG_NIL: u8 = 0x00;
const TAG_LEAF: u8 = 0x01;
const TAG_PARENT: u8 = 0x02;

// Tag bytes for TreeState variants.
const TREE_STATE_EMPTY: u8 = 0x00;
const TREE_STATE_AT_POSITION: u8 = 0x01;

/// Serialize a `PrunableTree<H>` to a writer.
///
/// The tree is encoded recursively:
/// - `Nil` -> `0x00`
/// - `Leaf { value: (hash, flags) }` -> `0x01` + 32-byte hash + 1-byte flags
/// - `Parent { ann, left, right }` -> `0x02` + annotation + left + right
///
/// The annotation `Option<Arc<H>>` is encoded as:
/// - `None` -> `0x00`
/// - `Some(h)` -> `0x01` + 32-byte hash
pub fn write_prunable_tree<H: HashSer, W: Write>(
    tree: &PrunableTree<H>,
    writer: &mut W,
) -> io::Result<()> {
    // Tree implements Deref to Node, so we dereference to access the Node variants.
    match tree.deref() {
        Node::Nil => {
            writer.write_all(&[TAG_NIL])?;
        }
        Node::Leaf {
            value: (hash, flags),
        } => {
            writer.write_all(&[TAG_LEAF])?;
            hash.hash_write(writer)?;
            writer.write_all(&[flags.bits()])?;
        }
        Node::Parent { ann, left, right } => {
            writer.write_all(&[TAG_PARENT])?;
            // Write optional annotation
            match ann {
                None => {
                    writer.write_all(&[0x00])?;
                }
                Some(h) => {
                    writer.write_all(&[0x01])?;
                    h.hash_write(writer)?;
                }
            }
            // Recursively write children
            write_prunable_tree(left.as_ref(), writer)?;
            write_prunable_tree(right.as_ref(), writer)?;
        }
    }
    Ok(())
}

/// Maximum recursion depth for deserializing a `PrunableTree`.
///
/// The commitment tree has depth 32 with shard height 16, so the deepest
/// subtree (either cap or shard) has at most 16 levels. We use 64 as a
/// generous limit to accommodate any reasonable tree structure while
/// preventing stack overflow from malicious input.
const MAX_TREE_DESERIALIZE_DEPTH: usize = 64;

/// Deserialize a `PrunableTree<H>` from a reader.
pub fn read_prunable_tree<H: HashSer + Clone, R: Read>(
    reader: &mut R,
) -> io::Result<PrunableTree<H>> {
    read_prunable_tree_inner(reader, 0)
}

/// Inner recursive deserializer with depth tracking.
fn read_prunable_tree_inner<H: HashSer + Clone, R: Read>(
    reader: &mut R,
    depth: usize,
) -> io::Result<PrunableTree<H>> {
    if depth > MAX_TREE_DESERIALIZE_DEPTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "tree exceeds maximum depth of {}",
                MAX_TREE_DESERIALIZE_DEPTH
            ),
        ));
    }

    let mut tag = [0u8; 1];
    reader.read_exact(&mut tag)?;

    match tag[0] {
        TAG_NIL => Ok(Tree::empty()),
        TAG_LEAF => {
            let hash = H::hash_read(reader)?;
            let mut flags_byte = [0u8; 1];
            reader.read_exact(&mut flags_byte)?;
            let flags = RetentionFlags::from_bits_retain(flags_byte[0]);
            Ok(Tree::leaf((hash, flags)))
        }
        TAG_PARENT => {
            // Read optional annotation
            let mut ann_tag = [0u8; 1];
            reader.read_exact(&mut ann_tag)?;
            let ann = match ann_tag[0] {
                0x00 => None,
                0x01 => {
                    let h = H::hash_read(reader)?;
                    Some(Arc::new(h))
                }
                other => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("invalid annotation tag: {:#04x}", other),
                    ));
                }
            };
            let left = read_prunable_tree_inner(reader, depth + 1)?;
            let right = read_prunable_tree_inner(reader, depth + 1)?;
            Ok(Tree::parent(ann, left, right))
        }
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid tree node tag: {:#04x}", other),
        )),
    }
}

/// Serialize a `LocatedPrunableTree<H>` to a writer.
///
/// Format: level(1 byte) + index(8 bytes BE) + prunable_tree
pub fn write_located_prunable_tree<H: HashSer, W: Write>(
    tree: &LocatedPrunableTree<H>,
    writer: &mut W,
) -> io::Result<()> {
    let addr = tree.root_addr();
    let level: u8 = addr.level().into();
    let index: u64 = addr.index();
    writer.write_all(&[level])?;
    writer.write_all(&index.to_be_bytes())?;
    write_prunable_tree(tree.root(), writer)
}

/// Deserialize a `LocatedPrunableTree<H>` from a reader.
pub fn read_located_prunable_tree<H: HashSer + Clone, R: Read>(
    reader: &mut R,
) -> io::Result<LocatedPrunableTree<H>> {
    let mut level_byte = [0u8; 1];
    reader.read_exact(&mut level_byte)?;

    // Validate level is within the commitment tree depth
    let tree_depth = orchard::NOTE_COMMITMENT_TREE_DEPTH as u8;
    if level_byte[0] > tree_depth {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "level {} exceeds maximum tree depth {}",
                level_byte[0], tree_depth
            ),
        ));
    }

    let level = Level::from(level_byte[0]);

    let mut index_bytes = [0u8; 8];
    reader.read_exact(&mut index_bytes)?;
    let index = u64::from_be_bytes(index_bytes);

    // Validate index is within range for this level.
    // At level L in a depth-D tree, valid indices are 0..2^(D-L)-1.
    let shift = tree_depth - level_byte[0];
    let max_index = (1u64 << shift) - 1;
    if index > max_index {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "index {} exceeds maximum {} for level {}",
                index, max_index, level_byte[0]
            ),
        ));
    }

    let addr = Address::from_parts(level, index);
    let tree = read_prunable_tree(reader)?;

    LocatedTree::from_parts(addr, tree).map_err(|bad_addr| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "tree structure exceeds address range at level={}, index={}",
                u8::from(bad_addr.level()),
                bad_addr.index()
            ),
        )
    })
}

/// Serialize a `Checkpoint` to a writer.
///
/// Format:
/// - tree_state tag (0x00 = Empty, 0x01 = AtPosition) + optional position(8
///   bytes BE)
/// - marks_removed count(4 bytes BE) + \[position(8 bytes BE)\]...
pub fn write_checkpoint<W: Write>(checkpoint: &Checkpoint, writer: &mut W) -> io::Result<()> {
    match checkpoint.tree_state() {
        TreeState::Empty => {
            writer.write_all(&[TREE_STATE_EMPTY])?;
        }
        TreeState::AtPosition(pos) => {
            writer.write_all(&[TREE_STATE_AT_POSITION])?;
            let pos_u64: u64 = pos.into();
            writer.write_all(&pos_u64.to_be_bytes())?;
        }
    }

    let marks = checkpoint.marks_removed();
    let count: u32 = marks.len().try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("marks_removed count {} exceeds u32::MAX", marks.len()),
        )
    })?;
    writer.write_all(&count.to_be_bytes())?;
    for pos in marks {
        let pos_u64: u64 = (*pos).into();
        writer.write_all(&pos_u64.to_be_bytes())?;
    }

    Ok(())
}

/// Maximum number of marks_removed entries in a checkpoint.
///
/// Each mark corresponds to a pruned leaf position. In practice this
/// should be much smaller, but we cap at 1M to prevent OOM.
const MAX_CHECKPOINT_MARKS: usize = 1_000_000;

/// Deserialize a `Checkpoint` from a reader.
pub fn read_checkpoint<R: Read>(reader: &mut R) -> io::Result<Checkpoint> {
    let mut state_tag = [0u8; 1];
    reader.read_exact(&mut state_tag)?;

    let tree_state = match state_tag[0] {
        TREE_STATE_EMPTY => TreeState::Empty,
        TREE_STATE_AT_POSITION => {
            let mut pos_bytes = [0u8; 8];
            reader.read_exact(&mut pos_bytes)?;
            let pos = Position::from(u64::from_be_bytes(pos_bytes));
            TreeState::AtPosition(pos)
        }
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid tree state tag: {:#04x}", other),
            ));
        }
    };

    let mut count_bytes = [0u8; 4];
    reader.read_exact(&mut count_bytes)?;
    let count = u32::from_be_bytes(count_bytes) as usize;

    if count > MAX_CHECKPOINT_MARKS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "marks_removed count {} exceeds maximum of {}",
                count, MAX_CHECKPOINT_MARKS,
            ),
        ));
    }

    let mut marks_removed = std::collections::BTreeSet::new();
    for _ in 0..count {
        let mut pos_bytes = [0u8; 8];
        reader.read_exact(&mut pos_bytes)?;
        marks_removed.insert(Position::from(u64::from_be_bytes(pos_bytes)));
    }

    Ok(Checkpoint::from_parts(tree_state, marks_removed))
}

#[cfg(test)]
mod tests {
    use incrementalmerkletree::Hashable;
    use orchard::tree::MerkleHashOrchard;

    use super::*;

    /// Helper: create a deterministic MerkleHashOrchard value from an index.
    fn test_hash(index: u64) -> MerkleHashOrchard {
        let empty = MerkleHashOrchard::empty_leaf();
        MerkleHashOrchard::combine(Level::from((index % 31) as u8 + 1), &empty, &empty)
    }

    #[test]
    fn roundtrip_nil_tree() {
        let tree: PrunableTree<MerkleHashOrchard> = Tree::empty();
        let mut buf = Vec::new();
        write_prunable_tree(&tree, &mut buf).unwrap();

        let decoded = read_prunable_tree::<MerkleHashOrchard, _>(&mut buf.as_slice()).unwrap();
        assert_eq!(tree, decoded);
    }

    #[test]
    fn roundtrip_leaf_tree() {
        let hash = test_hash(42);
        let flags = RetentionFlags::MARKED | RetentionFlags::CHECKPOINT;
        let tree: PrunableTree<MerkleHashOrchard> = Tree::leaf((hash, flags));

        let mut buf = Vec::new();
        write_prunable_tree(&tree, &mut buf).unwrap();

        let decoded = read_prunable_tree::<MerkleHashOrchard, _>(&mut buf.as_slice()).unwrap();
        assert_eq!(tree, decoded);
    }

    #[test]
    fn roundtrip_parent_tree_no_annotation() {
        let h1 = test_hash(1);
        let h2 = test_hash(2);
        let tree: PrunableTree<MerkleHashOrchard> = Tree::parent(
            None,
            Tree::leaf((h1, RetentionFlags::EPHEMERAL)),
            Tree::leaf((h2, RetentionFlags::MARKED)),
        );

        let mut buf = Vec::new();
        write_prunable_tree(&tree, &mut buf).unwrap();

        let decoded = read_prunable_tree::<MerkleHashOrchard, _>(&mut buf.as_slice()).unwrap();
        assert_eq!(tree, decoded);
    }

    #[test]
    fn roundtrip_parent_tree_with_annotation() {
        let h1 = test_hash(1);
        let h2 = test_hash(2);
        let ann = test_hash(99);
        let tree: PrunableTree<MerkleHashOrchard> = Tree::parent(
            Some(Arc::new(ann)),
            Tree::leaf((h1, RetentionFlags::EPHEMERAL)),
            Tree::leaf((h2, RetentionFlags::CHECKPOINT)),
        );

        let mut buf = Vec::new();
        write_prunable_tree(&tree, &mut buf).unwrap();

        let decoded = read_prunable_tree::<MerkleHashOrchard, _>(&mut buf.as_slice()).unwrap();
        assert_eq!(tree, decoded);
    }

    #[test]
    fn roundtrip_nested_tree() {
        let h1 = test_hash(1);
        let h2 = test_hash(2);
        let h3 = test_hash(3);
        let tree: PrunableTree<MerkleHashOrchard> = Tree::parent(
            None,
            Tree::parent(
                None,
                Tree::leaf((h1, RetentionFlags::MARKED)),
                Tree::empty(),
            ),
            Tree::parent(
                Some(Arc::new(test_hash(77))),
                Tree::leaf((h2, RetentionFlags::EPHEMERAL)),
                Tree::leaf((h3, RetentionFlags::REFERENCE)),
            ),
        );

        let mut buf = Vec::new();
        write_prunable_tree(&tree, &mut buf).unwrap();

        let decoded = read_prunable_tree::<MerkleHashOrchard, _>(&mut buf.as_slice()).unwrap();
        assert_eq!(tree, decoded);
    }

    #[test]
    fn roundtrip_located_prunable_tree() {
        let h1 = test_hash(10);
        let h2 = test_hash(20);
        let addr = Address::from_parts(Level::from(16), 5);
        let tree = LocatedTree::from_parts(
            addr,
            Tree::parent(
                None,
                Tree::leaf((h1, RetentionFlags::MARKED)),
                Tree::leaf((h2, RetentionFlags::EPHEMERAL)),
            ),
        )
        .unwrap();

        let mut buf = Vec::new();
        write_located_prunable_tree(&tree, &mut buf).unwrap();

        let decoded =
            read_located_prunable_tree::<MerkleHashOrchard, _>(&mut buf.as_slice()).unwrap();
        assert_eq!(tree, decoded);
    }

    #[test]
    fn roundtrip_located_empty_tree() {
        let addr = Address::from_parts(Level::from(16), 0);
        let tree: LocatedPrunableTree<MerkleHashOrchard> =
            LocatedTree::from_parts(addr, Tree::empty()).unwrap();

        let mut buf = Vec::new();
        write_located_prunable_tree(&tree, &mut buf).unwrap();

        let decoded =
            read_located_prunable_tree::<MerkleHashOrchard, _>(&mut buf.as_slice()).unwrap();
        assert_eq!(tree, decoded);
    }

    #[test]
    fn roundtrip_checkpoint_empty() {
        let cp = Checkpoint::tree_empty();
        let mut buf = Vec::new();
        write_checkpoint(&cp, &mut buf).unwrap();

        let decoded = read_checkpoint(&mut buf.as_slice()).unwrap();
        assert_eq!(cp.tree_state(), decoded.tree_state());
        assert_eq!(cp.marks_removed(), decoded.marks_removed());
    }

    #[test]
    fn roundtrip_checkpoint_at_position() {
        let cp = Checkpoint::at_position(Position::from(12345u64));
        let mut buf = Vec::new();
        write_checkpoint(&cp, &mut buf).unwrap();

        let decoded = read_checkpoint(&mut buf.as_slice()).unwrap();
        assert_eq!(cp.tree_state(), decoded.tree_state());
        assert_eq!(cp.marks_removed(), decoded.marks_removed());
    }

    #[test]
    fn roundtrip_checkpoint_with_marks_removed() {
        let mut marks = std::collections::BTreeSet::new();
        marks.insert(Position::from(10u64));
        marks.insert(Position::from(42u64));
        marks.insert(Position::from(9999u64));
        let cp = Checkpoint::from_parts(TreeState::AtPosition(Position::from(100u64)), marks);

        let mut buf = Vec::new();
        write_checkpoint(&cp, &mut buf).unwrap();

        let decoded = read_checkpoint(&mut buf.as_slice()).unwrap();
        assert_eq!(cp.tree_state(), decoded.tree_state());
        assert_eq!(cp.marks_removed(), decoded.marks_removed());
    }

    #[test]
    fn invalid_tag_produces_error() {
        let buf = [0xFF];
        let result = read_prunable_tree::<MerkleHashOrchard, _>(&mut buf.as_slice());
        assert!(result.is_err());
    }

    #[test]
    fn invalid_annotation_tag_produces_error() {
        // A parent tag with an invalid annotation tag
        let mut buf = Vec::new();
        buf.push(TAG_PARENT);
        buf.push(0xFF); // invalid annotation tag
        let result = read_prunable_tree::<MerkleHashOrchard, _>(&mut buf.as_slice());
        assert!(result.is_err());
    }

    #[test]
    fn invalid_checkpoint_state_produces_error() {
        let buf = [0xFF, 0, 0, 0, 0];
        let result = read_checkpoint(&mut buf.as_slice());
        assert!(result.is_err());
    }

    #[test]
    fn retention_flags_roundtrip_all_variants() {
        // Test each flag individually and combined
        let variants = [
            RetentionFlags::EPHEMERAL,
            RetentionFlags::CHECKPOINT,
            RetentionFlags::MARKED,
            RetentionFlags::REFERENCE,
            RetentionFlags::CHECKPOINT | RetentionFlags::MARKED,
            RetentionFlags::CHECKPOINT | RetentionFlags::REFERENCE,
        ];

        for flags in variants {
            let hash = test_hash(0);
            let tree: PrunableTree<MerkleHashOrchard> = Tree::leaf((hash, flags));

            let mut buf = Vec::new();
            write_prunable_tree(&tree, &mut buf).unwrap();

            let decoded = read_prunable_tree::<MerkleHashOrchard, _>(&mut buf.as_slice()).unwrap();
            assert_eq!(tree, decoded, "roundtrip failed for flags {:?}", flags);
        }
    }
}
