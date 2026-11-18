//! Fixed-width field reading shared by the decoders.

/// Takes the next `N` bytes off the front of `bytes`, or `None` if fewer are
/// left.
pub(crate) fn take<const N: usize>(bytes: &mut &[u8]) -> Option<[u8; N]> {
    let (head, rest) = bytes.split_first_chunk::<N>()?;
    *bytes = rest;
    Some(*head)
}
