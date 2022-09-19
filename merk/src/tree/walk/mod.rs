mod fetch;
mod ref_walker;

use anyhow::Result;
use costs::{cost_return_on_error, CostContext, CostsExt, OperationCost};
pub use fetch::Fetch;
pub use ref_walker::RefWalker;

use super::{Link, Tree};
use crate::{owner::Owner, CryptoHash};

/// Allows traversal of a `Tree`, fetching from the given source when traversing
/// to a pruned node, detaching children as they are traversed.
pub struct Walker<S>
where
    S: Fetch + Sized + Clone,
{
    tree: Owner<Tree>,
    source: S,
}

impl<S> Walker<S>
where
    S: Fetch + Sized + Clone,
{
    /// Creates a `Walker` with the given tree and source.
    pub fn new(tree: Tree, source: S) -> Self {
        Self {
            tree: Owner::new(tree),
            source,
        }
    }

    /// Similar to `Tree#detach`, but yields a `Walker` which fetches from the
    /// same source as `self`. Returned tuple is `(updated_self,
    /// maybe_child_walker)`.
    pub fn detach(mut self, left: bool) -> CostContext<Result<(Self, Option<Self>)>> {
        let mut cost = OperationCost::default();

        let link = match self.tree.link(left) {
            None => return Ok((self, None)).wrap_with_cost(cost),
            Some(link) => link,
        };

        let child = if link.tree().is_some() {
            match self.tree.own_return(|t| t.detach(left)) {
                Some(child) => child,
                _ => unreachable!("Expected Some"),
            }
        } else {
            let link = self.tree.slot_mut(left).take();
            match link {
                Some(Link::Reference { .. }) => (),
                _ => unreachable!("Expected Some(Link::Reference)"),
            }
            cost_return_on_error!(&mut cost, self.source.fetch(&link.unwrap()))
        };

        let child = self.wrap(child);
        Ok((self, Some(child))).wrap_with_cost(cost)
    }

    /// Similar to `Tree#detach_expect`, but yields a `Walker` which fetches
    /// from the same source as `self`. Returned tuple is `(updated_self,
    /// child_walker)`.
    pub fn detach_expect(self, left: bool) -> CostContext<Result<(Self, Self)>> {
        self.detach(left).map_ok(|(walker, maybe_child)| {
            if let Some(child) = maybe_child {
                (walker, child)
            } else {
                panic!(
                    "Expected {} child, got None",
                    if left { "left" } else { "right" }
                );
            }
        })
    }

    /// Similar to `Tree#walk`, but yields a `Walker` which fetches from the
    /// same source as `self`.
    pub fn walk<F, T>(self, left: bool, f: F) -> CostContext<Result<Self>>
    where
        F: FnOnce(Option<Self>) -> CostContext<Result<Option<T>>>,
        T: Into<Tree>,
    {
        let mut cost = OperationCost::default();

        let (mut walker, maybe_child) = cost_return_on_error!(&mut cost, self.detach(left));
        let new_child = match f(maybe_child).unwrap_add_cost(&mut cost) {
            Ok(x) => x.map(|t| t.into()),
            Err(e) => return Err(e).wrap_with_cost(cost),
        };
        walker.tree.own(|t| t.attach(left, new_child));
        Ok(walker).wrap_with_cost(cost)
    }

    /// Similar to `Tree#walk_expect` but yields a `Walker` which fetches from
    /// the same source as `self`.
    pub fn walk_expect<F, T>(self, left: bool, f: F) -> CostContext<Result<Self>>
    where
        F: FnOnce(Self) -> CostContext<Result<Option<T>>>,
        T: Into<Tree>,
    {
        let mut cost = OperationCost::default();

        let (mut walker, child) = cost_return_on_error!(&mut cost, self.detach_expect(left));
        let new_child = match f(child).unwrap_add_cost(&mut cost) {
            Ok(x) => x.map(|t| t.into()),
            Err(e) => return Err(e).wrap_with_cost(cost),
        };
        walker.tree.own(|t| t.attach(left, new_child));
        Ok(walker).wrap_with_cost(cost)
    }

    /// Returns an immutable reference to the `Tree` wrapped by this walker.
    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    /// Consumes the `Walker` and returns the `Tree` it wraps.
    pub fn into_inner(self) -> Tree {
        self.tree.into_inner()
    }

    /// Takes a `Tree` and returns a `Walker` which fetches from the same source
    /// as `self`.
    fn wrap(&self, tree: Tree) -> Self {
        Self::new(tree, self.source.clone())
    }

    /// Returns a clone of this `Walker`'s source.
    pub fn clone_source(&self) -> S {
        self.source.clone()
    }

    /// Similar to `Tree#attach`, but can also take a `Walker` since it
    /// implements `Into<Tree>`.
    pub fn attach<T>(mut self, left: bool, maybe_child: Option<T>) -> Self
    where
        T: Into<Tree>,
    {
        self.tree
            .own(|t| t.attach(left, maybe_child.map(|t| t.into())));
        self
    }

    /// Similar to `Tree#with_value`.
    pub fn put_value(mut self, value: Vec<u8>) -> CostContext<Self> {
        let mut cost = OperationCost::default();
        self.tree
            .own(|t| t.put_value(value).unwrap_add_cost(&mut cost));
        self.wrap_with_cost(cost)
    }

    /// Similar to `Tree#with_value_and_value_hash`.
    pub fn put_value_and_value_hash(
        mut self,
        value: Vec<u8>,
        value_hash: CryptoHash,
    ) -> CostContext<Self> {
        let mut cost = OperationCost::default();
        self.tree.own(|t| {
            t.put_value_and_value_hash(value, value_hash)
                .unwrap_add_cost(&mut cost)
        });
        self.wrap_with_cost(cost)
    }
}

impl<S> From<Walker<S>> for Tree
where
    S: Fetch + Sized + Clone,
{
    fn from(walker: Walker<S>) -> Self {
        walker.into_inner()
    }
}

#[cfg(test)]
mod test {
    use costs::{
        storage_cost::removal::StorageRemovedBytes::NoStorageRemoval, CostContext, CostsExt,
    };

    use super::{super::NoopCommit, *};
    use crate::tree::Tree;

    #[derive(Clone)]
    struct MockSource {}

    impl Fetch for MockSource {
        fn fetch(&self, link: &Link) -> CostContext<Result<Tree>> {
            Tree::new(link.key().to_vec(), b"foo".to_vec()).map(Ok)
        }
    }

    #[test]
    fn walk_modified() {
        let tree = Tree::new(b"test".to_vec(), b"abc".to_vec())
            .unwrap()
            .attach(
                true,
                Some(Tree::new(b"foo".to_vec(), b"bar".to_vec()).unwrap()),
            );

        let source = MockSource {};
        let walker = Walker::new(tree, source);

        let walker = walker
            .walk(true, |child| -> CostContext<Result<Option<Tree>>> {
                assert_eq!(child.expect("should have child").tree().key(), b"foo");
                Ok(None).wrap_with_cost(Default::default())
            })
            .unwrap()
            .expect("walk failed");
        assert!(walker.into_inner().child(true).is_none());
    }

    #[test]
    fn walk_stored() {
        let mut tree = Tree::new(b"test".to_vec(), b"abc".to_vec())
            .unwrap()
            .attach(
                true,
                Some(Tree::new(b"foo".to_vec(), b"bar".to_vec()).unwrap()),
            );
        tree.commit(&mut NoopCommit {}, &mut |_, _, _| Ok(false), &mut |_, _| {
            Ok(NoStorageRemoval)
        })
        .unwrap()
        .expect("commit failed");

        let source = MockSource {};
        let walker = Walker::new(tree, source);

        let walker = walker
            .walk(true, |child| -> CostContext<Result<Option<Tree>>> {
                assert_eq!(child.expect("should have child").tree().key(), b"foo");
                Ok(None).wrap_with_cost(Default::default())
            })
            .unwrap()
            .expect("walk failed");
        assert!(walker.into_inner().child(true).is_none());
    }

    #[test]
    fn walk_pruned() {
        let tree = Tree::from_fields(
            b"test".to_vec(),
            b"abc".to_vec(),
            Default::default(),
            Some(Link::Reference {
                hash: Default::default(),
                key: b"foo".to_vec(),
                child_heights: (0, 0),
            }),
            None,
        )
        .unwrap();

        let source = MockSource {};
        let walker = Walker::new(tree, source);

        let walker = walker
            .walk_expect(true, |child| -> CostContext<Result<Option<Tree>>> {
                assert_eq!(child.tree().key(), b"foo");
                Ok(None).wrap_with_cost(Default::default())
            })
            .unwrap()
            .expect("walk failed");
        assert!(walker.into_inner().child(true).is_none());
    }

    #[test]
    fn walk_none() {
        let tree = Tree::new(b"test".to_vec(), b"abc".to_vec()).unwrap();

        let source = MockSource {};
        let walker = Walker::new(tree, source);

        walker
            .walk(true, |child| -> CostContext<Result<Option<Tree>>> {
                assert!(child.is_none());
                Ok(None).wrap_with_cost(Default::default())
            })
            .unwrap()
            .expect("walk failed");
    }
}
