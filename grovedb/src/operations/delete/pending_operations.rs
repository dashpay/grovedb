//! The operations already pending in the batch that delete operations are
//! built into.

use crate::batch::QualifiedGroveDbOp;

/// The operations already pending in the batch a delete is built into, as
/// [`delete_operation_for_delete_internal`](crate::GroveDb::delete_operation_for_delete_internal)
/// and
/// [`delete_operations_for_delete_up_tree_while_empty`](crate::GroveDb::delete_operations_for_delete_up_tree_while_empty)
/// read them.
///
/// Building a delete asks for the pending operations at one path, the deleted
/// tree's own path, and only when the deleted element is a tree: deletes there
/// count as removing the tree's children, and anything else there makes the
/// tree non-empty. An up-tree chain asks once per tree it climbs through.
///
/// Every `IntoIterator<Item = &QualifiedGroveDbOp> + Clone` is a
/// `PendingOperations`: a slice or `&Vec` (`&ops`), an iterator
/// (`ops.iter()`), or an adapter over a caller's own operation type. It
/// answers each question by cloning itself and scanning all of its
/// operations, so a reference or an iterator is the cheap form to pass; an
/// owned `Vec` of references is copied on every question.
///
/// A caller that builds many deletes into one large batch can instead keep an
/// index of its pending operations by path and implement this trait over a
/// reference to it, so that each question costs the operations at that path
/// alone rather than a scan of the whole batch.
pub trait PendingOperations<'a> {
    /// The pending operations whose path is `path`. Their order does not
    /// matter.
    fn at_path(&self, path: &[Vec<u8>]) -> impl Iterator<Item = &'a QualifiedGroveDbOp>;
}

impl<'a, I> PendingOperations<'a> for I
where
    I: IntoIterator<Item = &'a QualifiedGroveDbOp> + Clone,
{
    fn at_path(&self, path: &[Vec<u8>]) -> impl Iterator<Item = &'a QualifiedGroveDbOp> {
        self.clone()
            .into_iter()
            .filter(move |op| op.path.eq_path_vec(path))
    }
}
