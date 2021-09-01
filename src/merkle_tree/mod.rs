extern crate blake3;

#[derive(Debug)]
pub struct LeafNode {
  key: Vec<u8>,
  value_hash: [u8; 32],
  value: Option<Data>,
}

#[derive(Debug)]
pub struct InnerNode {
  left: Box<TreeNode>,
  right: Option<Box<TreeNode>>
}

#[derive(Debug)]
pub enum TreeNode {
  LeafNode(LeafNode),
  InnerNode(InnerNode),
}

#[derive(Debug)]
pub enum Data {
  ValueData(Vec<u8>),
  TreeData(Box<MerkleTree>),
  SecondaryIndexData([u8; 32])
}

impl TreeNode {
  fn hash(&self) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();

    match self {
      TreeNode::LeafNode(LeafNode { key, value:_, value_hash }) => {
        hasher.update(&key);
        hasher.update(value_hash);
      },
      TreeNode::InnerNode(InnerNode { left, right }) => {
        match right {
          Some(right) => {
            hasher.update(&left.hash());
            hasher.update(&right.hash());
          },
          None => {
            hasher.update(&left.hash());
          }
        }
      },
    };

    let mut result = [0; 32];
    result.clone_from_slice(hasher.finalize().as_bytes());
    result
  }

  fn get_left(&self) -> Option<&Box<TreeNode>> {
    match self {
      TreeNode::LeafNode(_) => None,
      TreeNode::InnerNode(InnerNode { left, .. }) => Some(left),
    }
  }

  fn get_right(&mut self) -> Result<Option<&mut Box<TreeNode>>, &'static str> {
    match self {
      TreeNode::LeafNode(_) => Err("can not call `right` method on LeadNode"),
      TreeNode::InnerNode(inner) => match inner.right {
        Some(ref mut right) => Ok(Some(right)),
        None => Ok(None)
      }
    }
  }

  fn set_right(self, value: TreeNode) {
    match self {
      TreeNode::InnerNode(mut node) => {
        node.right = Some(Box::new(value))
      },
      TreeNode::LeafNode { .. } => ()
    }
  }

  fn get_value(&self) -> Option<&Data> {
    match self {
      TreeNode::LeafNode(LeafNode { key: _, value_hash: _, value }) => match value {
        Some(data) => Some(data),
        None => None,
      },
      TreeNode::InnerNode { .. } => None,
    }
  }
}

#[derive(Debug)]
pub struct MerkleTree {
  root_node: Box<TreeNode>,
}

impl MerkleTree {
  pub fn new(mut key_values: Vec<(Vec<u8>, Data)>) -> Self {
    if key_values.is_empty() {
      panic!("at least one key-value item should be submitted")
    }

    let leaf_nodes: Vec<TreeNode> = key_values.drain(0..).map(|(key, value)| {
      let value_hash = match &value {
        Data::ValueData(value_data) => {
          let mut result = [0; 32];
          result.clone_from_slice(blake3::hash(&value_data).as_bytes());
          result
        },
        Data::TreeData(tree) => tree.root_node.hash(),
        Data::SecondaryIndexData(index_data) => {
          let mut result = [0; 32];
          result.clone_from_slice(index_data);
          result
        },
      };

      TreeNode::LeafNode(
        LeafNode {
          key: key.to_vec(),
          value: Some(value),
          value_hash,
        }
      )
    }).collect();

    let root_node = MerkleTree::build_root_node(leaf_nodes);

    MerkleTree {
      root_node: Box::new(root_node),
    }
  }

  fn build_root_node(mut nodes: Vec<TreeNode>) -> TreeNode {
    nodes = nodes.into_iter().rev().collect();

    let mut result_nodes: Vec<TreeNode> = vec!();
    while nodes.len() > 0 {
      let left_node: TreeNode = nodes.pop().unwrap();

      let node = TreeNode::InnerNode(
        InnerNode {
          left: Box::new(left_node),
          right: nodes.pop().map(|leaf_node| Box::new(leaf_node)),
        }
      );

      result_nodes.push(node);
    }

    if result_nodes.len() == 1 {
      result_nodes.pop().unwrap()
    } else {
      MerkleTree::build_root_node(result_nodes)
    }
  }

  fn find_incomplete<'a>(&'a mut self) -> Option<&'a mut Box<TreeNode>> {
    if self.root_node.get_right().unwrap().is_none() {
      return Some(&mut self.root_node)
    }

    fn find(node_option: Option<&mut Box<TreeNode>>) -> Option<&mut Box<TreeNode>> {
      if node_option.is_none() {
        return None;
      }

      let node = node_option.unwrap();

      if node.get_right().is_err() {
        return None;
      }

      if node.get_right().unwrap().is_none() {
        return Some(node);
      }

      find(node.get_right().unwrap())
    }

    find(self.root_node.get_right().unwrap())
  }

  pub fn insert(&mut self, key: Vec<u8>, value: Data) -> Result<(), &'static str> {
    let result = self.find_incomplete();

    match result {
      Some(node) => match node.as_mut() {
        TreeNode::InnerNode(inner) => {
          let new_node = TreeNode::LeafNode(
            LeafNode {
              key,
              value_hash: [0; 32],
              value: Some(value),
            }
          );

          inner.right = Some(Box::new(new_node));

          Ok(())
        },
        _ => Ok(())
      }
      None => {
        // TODO: implement adding another layer here
        Ok(())
      },
    }
  }
}

#[test]
fn t1() {
  let a = MerkleTree::new(vec!(
    (vec!(1, 2, 3), Data::ValueData(vec!(4, 5, 6))),
    (vec!(10, 20, 30), Data::ValueData(vec!(40, 50, 60))),
    (vec!(100, 200, 201), Data::ValueData(vec!(202, 203, 204))),
  ));

  let mut b = MerkleTree::new(vec!(
    (vec!(1, 2, 3), Data::ValueData(vec!(4, 5, 6))),
    (vec!(10, 20, 30), Data::TreeData(Box::new(a))),
    (vec!(100, 200, 201), Data::SecondaryIndexData([1; 32])),
  ));

  println!("\nincomplete inner node : {:?}\n", b.find_incomplete());

  b.insert(vec!(0, 0, 0), Data::ValueData(vec!())).unwrap();

  println!("\nb after insert: {:?}\n", b);

  // let leaf = b.root_node.left().unwrap().right().unwrap();

  // match leaf.value().unwrap() {
  //   Data::TreeData(t) => {
  //     println!("testing inner: {:?}", t.root_node.left().unwrap().right().unwrap())
  //   }
  //   _ => ()
  // }
}