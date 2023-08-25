#[derive(Debug, thiserror::Error)]
/// Chunk related errors
pub enum ChunkError {
    /// Limit too small for first chunk, cannot make progress
    #[error("overflow error {0}")]
    LimitTooSmall(&'static str),

    /// Chunk index out of bounds
    #[error("chunk index out of bounds: {0}")]
    OutOfBounds(&'static str),

    /// Empty tree contains no chunks
    #[error("chunk from empty tree: {0}")]
    EmptyTree(&'static str),

    /// Invalid traversal instruction (points to no element)
    #[error("traversal instruction invalid {0}")]
    BadTraversalInstruction(&'static str),

    /// Expected ChunkId when parsing chunk ops
    #[error("expected chunk id when parsing chunk op")]
    ExpectedChunkId,

    /// Expected Chunk when parsing chunk ops
    #[error("expected chunk when parsing chunk op")]
    ExpectedChunk,

    /// Internal error, this should never surface
    /// if it does, it means wrong assumption in code
    #[error("internal error {0}")]
    InternalError(&'static str),
}
