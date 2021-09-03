extern crate blake3;

#[derive(Debug, Default)]
pub struct LeafNode {
  key: Vec<u8>,
  value_hash: [u8; 32],
  value: Option<Data>,
}

#[derive(Debug, Default)]
pub struct InnerNode {
  left: Option<Box<TreeNode>>,
  right: Option<Box<TreeNode>>
}

#[derive(Debug, Default)]
pub enum TreeNode {
  LeafNode(LeafNode),
  InnerNode(InnerNode),
  #[default]
  None,
}

#[derive(Debug)]
pub enum Data {
  ValueData(Vec<u8>),
  TreeData(Box<MerkleTree>),
  SecondaryIndexData([u8; 32])
}

impl Data {
  fn hash(&self) -> [u8; 32] {
    match self {
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
    }
  }
}

impl TreeNode {
  fn hash(&self) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();

    match self {
      TreeNode::LeafNode(LeafNode { key, value:_, value_hash }) => {
        hasher.update(&key);
        hasher.update(value_hash);
      },
      TreeNode::InnerNode(inner) => {
        match &inner.right {
          Some(right) => {
            hasher.update(&inner.left.as_ref().unwrap().hash());
            hasher.update(&right.hash());
          },
          None => {
            hasher.update(&inner.left.as_ref().unwrap().hash());
          }
        }
      },
      TreeNode::None => ()
    };

    let mut result = [0; 32];
    result.clone_from_slice(hasher.finalize().as_bytes());
    result
  }

  fn get_left(&self) -> Option<&Box<TreeNode>> {
    match self {
      TreeNode::LeafNode(_) => None,
      TreeNode::InnerNode(inner) => inner.left.as_ref(),
      TreeNode::None => None,
    }
  }

  fn get_left_mut(&mut self) -> Option<&mut Box<TreeNode>> {
    match self {
      TreeNode::LeafNode(_) => None,
      TreeNode::InnerNode(inner) => inner.left.as_mut(),
      TreeNode::None => None,
    }
  }

  fn set_left(&mut self, node: TreeNode) {
    match self {
      TreeNode::LeafNode(_) => (),
      TreeNode::InnerNode(inner) => inner.left = Some(Box::new(node)),
      TreeNode::None => (),
    }
  }

  fn get_right(&self) -> Option<&Box<TreeNode>> {
    match self {
      TreeNode::LeafNode(_) => None,
      TreeNode::InnerNode(inner) => inner.right.as_ref(),
      TreeNode::None => None,
    }
  }

  fn get_right_mut(&mut self) -> Option<&mut Box<TreeNode>> {
    match self {
      TreeNode::LeafNode(_) => None,
      TreeNode::InnerNode(inner) => inner.right.as_mut(),
      TreeNode::None =>  None,
    }
  }

  fn get_value(&self) -> Option<&Data> {
    match self {
      TreeNode::LeafNode(LeafNode { key: _, value_hash: _, value }) => value.as_ref(),
      TreeNode::InnerNode { .. } => None,
      TreeNode::None => None,
    }
  }

  fn is_leaf_node(&self) -> bool {
    match self {
      TreeNode::LeafNode(_) => true,
      TreeNode::InnerNode(_) => false,
      TreeNode::None => false,
    }
  }

  fn get_height(&self) -> u64 {
    if self.get_left().is_none() {
      return 0;
    }

    fn find_bottom(node_option: Option<&Box<TreeNode>>, counter: u64) -> (Option<&Box<TreeNode>>, u64) {
      if node_option.is_none() {
        return (None, counter)
      }

      let node = node_option.unwrap();

      if node.is_leaf_node() {
        return (node_option, counter);
      }

      find_bottom(node.get_left(), counter + 1)
    }

    find_bottom(self.get_left(), 1).1
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
      TreeNode::LeafNode(
        LeafNode {
          key: key.to_vec(),
          value_hash: value.hash(),
          value: Some(value),
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
          left: Some(Box::new(left_node)),
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
    if self.root_node.get_right().is_none() {
      return Some(&mut self.root_node)
    }

    fn find(node_option: Option<&mut Box<TreeNode>>) -> Option<&mut Box<TreeNode>> {
      if node_option.is_none() {
        return None;
      }

      let node = node_option.unwrap();

      if node.is_leaf_node() {
        return None;
      }

      if node.get_right().is_none() {
        return Some(node);
      }

      find(node.get_right_mut())
    }

    find(self.root_node.get_right_mut())
  }

  pub fn get_height(&self) -> u64 {
    fn find_bottom(node_option: Option<&Box<TreeNode>>, counter: u64) -> (Option<&Box<TreeNode>>, u64) {
      if node_option.is_none() {
        return (None, counter)
      }

      let node = node_option.unwrap();

      if node.is_leaf_node() {
        return (node_option, counter);
      }

      find_bottom(node.get_left(), counter + 1)
    }

    find_bottom(Some(&self.root_node), 0).1
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
        let tree_height = self.get_height();

        println!("the height is : {:?}", tree_height);

        let leaf_node = TreeNode::LeafNode(LeafNode {
          key,
          value_hash: value.hash(),
          value: Some(value),
        });

        let old_root = std::mem::take(&mut self.root_node);

        fn construct_new_chain(inner_node: TreeNode, counter: u64) -> TreeNode {
          if counter == 0 {
            return inner_node;
          }

          let top = TreeNode::InnerNode(InnerNode {
            left: Some(Box::new(inner_node)),
            right: None,
          });

          construct_new_chain(top, counter - 1)
        }

        let mut new_right = construct_new_chain(TreeNode::InnerNode(InnerNode { left: None, right: None }), tree_height);

        new_right.set_left(leaf_node);

        self.root_node = Box::new(TreeNode::InnerNode(InnerNode {
          left: Some(old_root),
          right: Some(Box::new(new_right)),
        }));

        Ok(())
      },
    }
  }
}

#[test]
fn t1() {
  let mut imbalanced_tree = MerkleTree::new(vec!(
    (vec!(1, 2, 3), Data::ValueData(vec!(4, 5, 6))),
    (vec!(10, 20, 30), Data::ValueData(vec!(40, 50, 60))),
    (vec!(100, 200, 201), Data::SecondaryIndexData([0; 32])),
  ));

  // checking we have nothing to the right
  assert_eq!(imbalanced_tree.root_node.get_right().unwrap().get_right().is_none(), true);

  imbalanced_tree.insert(vec!(0, 0, 0), Data::ValueData(vec!(0, 0, 0))).unwrap();

  assert_eq!(imbalanced_tree.root_node.get_right().unwrap().get_right().is_none(), false);

  let node = imbalanced_tree.root_node.get_right().unwrap().get_right().unwrap();

  assert!(matches!(node.get_value().unwrap(), Data::ValueData(_)));
}