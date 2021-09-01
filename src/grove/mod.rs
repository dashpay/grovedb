pub mod inner;

use std::collections::{ HashMap, HashSet };

use inner::{ GroveInnerTree, RootHash };

pub struct Grove {
  root_tree: Box<dyn GroveInnerTree>,
  leaves: Vec<Box<Grove>>,
  leaf_indices: HashMap<String, usize>,
}

impl Grove {
  pub fn new(root_tree: Box<dyn GroveInnerTree>, leaves: Vec<Box<Grove>>) -> Result<Grove, String> {
    let unique_indices: HashSet<&str> = leaves.iter().map(|leaf| leaf.root_tree.get_id()).collect();

    if unique_indices.len() < leaves.len() {
      return Err("all leaf ids should be unique".to_owned())
    }

    let leaf_indices: HashMap<String, usize> = leaves.iter()
      .enumerate()
      .map(|(index, grove)| (grove.root_tree.get_id().to_owned(), index))
      .collect();

    Ok(Grove {
      root_tree,
      leaves,
      leaf_indices,
    })
  }

  pub fn get_grove(&self, path: Vec<&str>) -> Option<&Box<Grove>> {
    if path.is_empty() {
      return None;
    }

    let leaf_name = path[0];

    return match self.leaf_indices.get(leaf_name) {
      Some(index) => {
        if path.len() == 1 {
          return Some(&self.leaves[*index])
        }

        self.leaves[*index].get_grove((&path[1..]).to_vec())
      },
      None => None
    }
  }

  pub fn get_root_hash(&self) -> RootHash {
    self.root_tree.get_root_hash()
  }
}