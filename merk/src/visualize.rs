use std::io::{Result, Write};

use storage::StorageContext;
use visualize::{Drawer, Visualize};

use crate::{tree::Tree, Merk};

pub struct VisualizeableMerk<'a, S, F> {
    merk: &'a Merk<S>,
    deserialize_fn: F,
}

impl<'a, S, F> VisualizeableMerk<'a, S, F> {
    pub fn new(merk: &'a Merk<S>, deserialize_fn: F) -> Self {
        Self {
            merk,
            deserialize_fn,
        }
    }
}

struct VisualizableTree<'a, F> {
    tree: &'a Tree,
    deserialize_fn: F,
}

impl<'a, F> VisualizableTree<'a, F> {
    fn new(tree: &'a Tree, deserialize_fn: F) -> Self {
        Self {
            tree,
            deserialize_fn,
        }
    }
}

impl<'a, 'db, S: StorageContext<'db>, T: Visualize, F: Fn(&[u8]) -> T + Copy> Visualize
    for VisualizeableMerk<'a, S, F>
{
    fn visualize<W: Write>(&self, mut drawer: Drawer<W>) -> Result<Drawer<W>> {
        drawer.write(b"Merk root: ")?;
        drawer = self.merk.use_tree(|tree| {
            if let Some(t) = tree {
                VisualizableTree::new(t, self.deserialize_fn).visualize(drawer)
            } else {
                drawer.write(b"empty")?;
                Ok(drawer)
            }
        })?;
        drawer.flush()?;

        Ok(drawer)
    }
}

impl<'a, T: Visualize, F: Fn(&[u8]) -> T + Copy> Visualize for VisualizableTree<'a, F> {
    fn visualize<W: Write>(&self, mut drawer: Drawer<W>) -> Result<Drawer<W>> {
        drawer.write(b"[key: ")?;
        drawer = self.tree.inner.key_as_slice().visualize(drawer)?;
        drawer.write(b", value: ")?;
        drawer = (self.deserialize_fn)(self.tree.inner.value_as_slice()).visualize(drawer)?;

        drawer.down();
        drawer.write(b"\n")?;

        drawer.write(b"left: ")?;
        drawer = self
            .tree
            .child(true)
            .map(|tree| Self::new(tree, self.deserialize_fn))
            .visualize(drawer)?;
        drawer.write(b"\n")?;

        drawer.write(b"right: ")?;
        drawer = self
            .tree
            .child(false)
            .map(|tree| Self::new(tree, self.deserialize_fn))
            .visualize(drawer)?;

        drawer.up();
        Ok(drawer)
    }
}
