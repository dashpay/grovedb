//! The memory of nodes a thread no longer holds, kept for the nodes it
//! loads next.
//!
//! A Merk that is opened for one operation loads the nodes on the paths it
//! walks, and lets go of them when it is dropped. A node is a box, a key,
//! a value and the key of each child that is not loaded, so the next Merk
//! of the thread would ask the allocator for the same memory again. A
//! thread keeps what it lets go of instead, on a list of its own.
//!
//! - **What is given to the list:** the tree a Merk holds when it is
//!   dropped ([`give_tree`]), and the key by which a link named a child
//!   ([`give`]): where a walk takes the link out of its node to fetch the
//!   child, and where a fetched child is put in the link's place.
//! - **What is taken from it:** a box, by the constructors of [`TreeNode`]
//!   and by its decoders ([`boxed`]); the key of a link, by the decoder of
//!   [`Link`]; the key of a node, by `get_node` of the key-value store and
//!   by `get_node` and `fetch_node_without_value` of the LigninDB store;
//!   and the value of a node and the keys of its links, by the reads of
//!   the LigninDB store ([`bytes`], [`buffer`]).
//! - **It changes no answer, and no charge that is counted from bytes.**
//!   A buffer is empty when it is taken and a box is overwritten whole, so
//!   a node that is made of spare memory has in every field what one made
//!   of new memory has. What can differ is the room of its buffers
//!   (`Vec::capacity`). No cost of this workspace reads it; a cost
//!   function of a caller that read the room of a key or a value it is
//!   shown would be given another room where that buffer is a spare one
//!   with more room than was asked for.
//! - **It is bounded.** A thread keeps at most [`BOXES`] node boxes, and of
//!   the byte buffers of each size class at most the number [`KEPT`] gives:
//!   no more than [`CEILING`] bytes of room in buffers (479,232), whatever
//!   is given to it. A buffer with more than [`LARGEST`] bytes of room is
//!   never kept. What does not fit goes back to the allocator as before.
//! - **It is the thread's own.** Nothing is shared and nothing is locked.
//!   When a thread ends, its list is dropped and the memory goes back to
//!   the allocator. A thread whose list is gone, or is in use further up
//!   the stack, asks the allocator as it did before.
//!
//! A buffer that is made new has exactly the room that is asked for. A
//! spare one that is taken has at least that room, and less than twice
//! that room unless it has no more than [`SMALLEST`] bytes of it: the
//! buffers are kept by size class, and a take looks at the [`LOOKED_AT`]
//! buffers at the end of its class's list for one with room.

use std::{cell::RefCell, mem};

use super::{Baseline, Link, TreeNode, TreeNodeInner};

/// The room of the buffers of the first size class.
const SMALLEST: usize = 16;

/// How many buffers a thread keeps of each size class. The class `c` holds
/// the buffers whose room is at most `SMALLEST << c` bytes and more than
/// that of the class before it.
///
/// The classes of 17 to 64 bytes are kept in the largest number: a loaded
/// node holds its own key and one for each child that is not loaded.
const KEPT: [usize; 9] = [256, 1024, 1024, 128, 128, 128, 64, 32, 32];

/// The longest buffer a thread keeps, in bytes of room.
pub(crate) const LARGEST: usize = SMALLEST << (KEPT.len() - 1);

/// How many node boxes a thread keeps.
pub(crate) const BOXES: usize = 128;

/// How many spare buffers of a size class a take looks at: those at the
/// end of the class's list. They are not always the ones that were given
/// last, since the buffer a take takes leaves its place to the last one.
const LOOKED_AT: usize = 8;

/// The most bytes of room the buffers hold that a thread keeps.
pub(crate) const CEILING: usize = {
    let (mut bytes, mut class) = (0, 0);
    while class < KEPT.len() {
        bytes += KEPT[class] * (SMALLEST << class);
        class += 1;
    }
    bytes
};

const _: () = assert!(CEILING == 479_232);

struct Spare {
    // The boxes are the memory that is kept.
    #[allow(clippy::vec_box)]
    boxes: Vec<Box<TreeNodeInner>>,
    bytes: [Vec<Vec<u8>>; KEPT.len()],
}

thread_local! {
    static SPARE: RefCell<Spare> = const {
        RefCell::new(Spare {
            boxes: Vec::new(),
            bytes: [const { Vec::new() }; KEPT.len()],
        })
    };
}

/// The size class of `len` bytes of room: the first whose room,
/// `SMALLEST << class`, is at least `len`. `None` for no bytes, and for
/// more than a thread keeps.
const fn class_of(len: usize) -> Option<usize> {
    if len == 0 || len > LARGEST {
        return None;
    }
    let units = (len - 1) / SMALLEST;
    Some((usize::BITS - units.leading_zeros()) as usize)
}

impl Spare {
    fn take(&mut self, len: usize) -> Option<Vec<u8>> {
        let kept = &mut self.bytes[class_of(len)?];
        let from = kept.len().saturating_sub(LOOKED_AT);
        let with_room = kept[from..]
            .iter()
            .rposition(|spare| spare.capacity() >= len)?;
        Some(kept.swap_remove(from + with_room))
    }

    fn give(&mut self, mut bytes: Vec<u8>) {
        let Some(class) = class_of(bytes.capacity()) else {
            return;
        };
        let kept = &mut self.bytes[class];
        if kept.len() < KEPT[class] {
            bytes.clear();
            kept.push(bytes);
        }
    }

    fn give_tree(&mut self, tree: TreeNode) {
        let TreeNode {
            mut inner,
            baseline,
            known_storage_cost: _,
        } = tree;
        if let Baseline::Was(value) = baseline {
            self.give(value);
        }
        for link in [inner.left.take(), inner.right.take()] {
            match link {
                None => {}
                Some(Link::Reference { key, .. }) => self.give(key),
                Some(
                    Link::Modified { tree, .. }
                    | Link::Uncommitted { tree, .. }
                    | Link::Loaded { tree, .. },
                ) => self.give_tree(tree),
            }
        }
        self.give(mem::take(&mut inner.kv.key));
        self.give(mem::take(&mut inner.kv.value));
        // What is left in the box holds no memory of its own: the box is
        // overwritten whole when it is taken.
        if self.boxes.len() < BOXES {
            self.boxes.push(inner);
        }
    }
}

/// Runs `run` on the list of the thread. `None` where the thread has none
/// to give: it is ending, or the list is in use further up the stack.
fn with<R>(run: impl FnOnce(&mut Spare) -> R) -> Option<R> {
    let run = |spare: &RefCell<Spare>| spare.try_borrow_mut().ok().map(|mut spare| run(&mut spare));
    SPARE.try_with(run).ok().flatten()
}

/// An empty buffer with room for `len` bytes: a spare one of the thread
/// where it has one, and a new one of exactly that room otherwise.
pub(crate) fn buffer(len: usize) -> Vec<u8> {
    with(|spare| spare.take(len))
        .flatten()
        .unwrap_or_else(|| Vec::with_capacity(len))
}

/// A copy of `bytes`, in a spare buffer of the thread where it has one.
pub(crate) fn bytes(bytes: &[u8]) -> Vec<u8> {
    let mut copy = buffer(bytes.len());
    copy.extend_from_slice(bytes);
    copy
}

/// Gives the memory of `bytes` to the thread, which keeps it if it has
/// room for it.
pub(crate) fn give(bytes: Vec<u8>) {
    if bytes.capacity() != 0 {
        with(|spare| spare.give(bytes));
    }
}

/// `inner` in a box: a spare one of the thread where it has one.
pub(crate) fn boxed(inner: TreeNodeInner) -> Box<TreeNodeInner> {
    match with(|spare| spare.boxes.pop()).flatten() {
        Some(mut spare) => {
            *spare = inner;
            spare
        }
        None => Box::new(inner),
    }
}

/// Gives the memory of `tree` to the thread: the box of each node that
/// the tree holds, its key, its value, and the keys of its links.
pub(crate) fn give_tree(tree: TreeNode) {
    with(|spare| spare.give_tree(tree));
}

#[cfg(all(test, merk_test_engine))]
mod tests {
    use grovedb_costs::OperationCost;
    use grovedb_version::version::GroveVersion;

    use super::*;
    use crate::{
        merk::TreeCell,
        test_utils::TempMerk,
        tree::{kv::KV, AggregateData, BatchEntry, Op, TreeFeatureType::BasicMerkNode},
        CryptoHash, TreeFeatureType,
    };

    /// What the thread keeps: its boxes, and its buffers by size class.
    fn kept() -> (usize, [usize; KEPT.len()]) {
        with(|spare| {
            let mut bytes = [0; KEPT.len()];
            for (count, kept) in bytes.iter_mut().zip(&spare.bytes) {
                *count = kept.len();
            }
            (spare.boxes.len(), bytes)
        })
        .expect("the list of a thread that runs a test")
    }

    /// Drops what the thread keeps: a test starts from an empty list.
    fn clear() {
        with(|spare| {
            spare.boxes.clear();
            spare.bytes.iter_mut().for_each(Vec::clear);
        });
    }

    fn inner(key: &[u8], value: &[u8], feature: TreeFeatureType) -> TreeNodeInner {
        TreeNodeInner {
            marks: Default::default(),
            left: None,
            right: None,
            kv: KV::from_fields(key.to_vec(), value.to_vec(), [1; 32], [2; 32], feature),
        }
    }

    fn reference(key: &[u8]) -> Link {
        Link::Reference {
            hash: [3; 32],
            child_heights: (0, 0),
            key: key.to_vec(),
            aggregate_data: AggregateData::NoAggregateData,
        }
    }

    #[test]
    fn a_size_class_holds_the_lengths_up_to_its_room() {
        assert_eq!(class_of(0), None);
        for (class, room) in (0..KEPT.len()).map(|class| (class, SMALLEST << class)) {
            assert_eq!(class_of(room), Some(class), "{room} bytes");
            assert_eq!(
                class_of(room / 2 + 1),
                Some(class),
                "{} bytes",
                room / 2 + 1
            );
            let next = (class + 1 < KEPT.len()).then_some(class + 1);
            assert_eq!(class_of(room + 1), next, "{} bytes", room + 1);
        }
        assert_eq!(class_of(1), Some(0));
        assert_eq!(LARGEST, 4096);
        assert_eq!(class_of(usize::MAX), None);
    }

    #[test]
    fn a_buffer_that_was_given_is_taken_again_empty() {
        clear();
        let first = bytes(&[7; 32]);
        let (at, room) = (first.as_ptr(), first.capacity());
        assert_eq!((first.as_slice(), room), (&[7; 32][..], 32));
        give(first);
        assert_eq!(kept().1[1], 1);

        // A shorter length of the same class takes it, and reads nothing
        // of what it held.
        let again = bytes(&[9; 20]);
        assert_eq!(again, [9; 20]);
        assert_eq!((again.as_ptr(), again.capacity()), (at, room));
        assert_eq!(kept().1[1], 0);
        give(again);

        // A take passes over a buffer of its class that has less room.
        let mut small = Vec::with_capacity(20);
        small.push(1);
        give(small);
        assert_eq!(kept().1[1], 2);
        let longer = buffer(32);
        assert_eq!((longer.as_ptr(), longer.capacity()), (at, room));
        assert_eq!(kept().1[1], 1);

        // Where no buffer of the class has the room, and for a length of
        // a class that holds none, the buffer is new and has exactly the
        // room that was asked for.
        assert_eq!(buffer(32).capacity(), 32);
        assert_eq!(buffer(33).capacity(), 33);
        assert_eq!(buffer(0).capacity(), 0);
        assert_eq!(kept().1[1], 1);
        assert_eq!(buffer(17).capacity(), 20);
        assert_eq!(kept().1[1], 0);
        clear();
    }

    #[test]
    fn a_take_looks_at_the_end_of_the_list_of_its_class() {
        clear();
        give(Vec::with_capacity(32));
        for _ in 0..LOOKED_AT {
            give(Vec::with_capacity(17));
        }
        // The buffer with room is further down than a take looks.
        assert_eq!(buffer(32).capacity(), 32);
        assert_eq!(kept().1[1], LOOKED_AT + 1);
        assert_eq!(buffer(17).capacity(), 17);
        assert_eq!(buffer(30).capacity(), 32);
        assert_eq!(kept().1[1], LOOKED_AT - 1);
        clear();
    }

    #[test]
    fn a_thread_keeps_no_more_than_its_bound() {
        clear();
        // Neither a buffer with no room nor one longer than the longest
        // that is kept.
        give(Vec::new());
        give(Vec::with_capacity(LARGEST + 1));
        assert_eq!(kept(), (0, [0; KEPT.len()]));
        give(Vec::with_capacity(LARGEST));
        assert_eq!(kept().1[KEPT.len() - 1], 1);

        // Of each class no more than its number, and of boxes no more
        // than theirs.
        for (class, most) in KEPT.iter().enumerate() {
            for _ in 0..most + 3 {
                give(Vec::with_capacity(SMALLEST << class));
            }
            assert_eq!(kept().1[class], *most, "class {class}");
        }
        let room = |spare: &mut Spare| -> usize {
            let kept = spare.bytes.iter().flatten();
            kept.map(Vec::capacity).sum()
        };
        assert_eq!(with(room), Some(CEILING));
        let node = || TreeNode::new_with_tree_inner(inner(b"", b"", BasicMerkNode));
        let nodes: Vec<TreeNode> = (0..BOXES + 3).map(|_| node()).collect();
        nodes.into_iter().for_each(give_tree);
        assert_eq!(kept().0, BOXES);
        clear();
    }

    #[test]
    fn a_node_in_a_spare_box_is_the_node_in_a_new_one() {
        clear();
        // A tree of three nodes: its root holds one child and names the
        // other by its key.
        let mut root = inner(&[1; 32], &[4; 400], BasicMerkNode);
        let mut child = inner(&[2; 32], &[5; 36], BasicMerkNode);
        child.left = Some(reference(&[6; 32]));
        root.left = Some(Link::Loaded {
            hash: [8; 32],
            child_heights: (1, 0),
            tree: TreeNode::new_with_tree_inner(child),
            aggregate_data: AggregateData::NoAggregateData,
        });
        root.right = Some(reference(&[3; 32]));
        let mut root = TreeNode::new_with_tree_inner(root);
        root.keep_old_value();
        root.inner.kv.value = vec![9; 100];
        // A Merk gives the tree it holds when it is dropped.
        let cell = TreeCell::default();
        cell.set(Some(root));
        assert_eq!(kept(), (0, [0; KEPT.len()]));
        drop(cell);

        // Two boxes; four keys of 32 bytes; the values of 36 and of 100
        // bytes; and the 400 bytes the root keeps as its value before the
        // change.
        let (boxes, bytes) = kept();
        assert_eq!(boxes, 2);
        assert_eq!(bytes, [0, 4, 1, 1, 0, 1, 0, 0, 0]);

        // A node that takes the box of another holds nothing of it.
        let fresh = inner(&[7; 5], &[], TreeFeatureType::SummedMerkNode(3));
        let node = boxed(fresh.clone());
        assert_eq!(*node, fresh);
        assert!(node.left.is_none() && node.right.is_none());
        assert_eq!(kept().0, 1);
        clear();
    }

    #[test]
    fn a_tree_gives_what_children_hold_that_were_not_written() {
        clear();
        // One child that was changed and is not hashed, and one that is
        // hashed and not written.
        let child = |key: &[u8]| TreeNode::new_with_tree_inner(inner(key, &[5; 40], BasicMerkNode));
        let mut root = inner(&[1; 32], &[4; 40], BasicMerkNode);
        root.left = Some(Link::Modified {
            pending_writes: 1,
            child_heights: (0, 0),
            tree: child(&[2; 32]),
        });
        root.right = Some(Link::Uncommitted {
            hash: [8; 32],
            child_heights: (0, 0),
            tree: child(&[3; 32]),
            aggregate_data: AggregateData::NoAggregateData,
        });
        give_tree(TreeNode::new_with_tree_inner(root));

        // Three boxes, three keys of 32 bytes and three values of 40.
        let (boxes, bytes) = kept();
        assert_eq!(boxes, 3);
        assert_eq!(bytes, [0, 3, 3, 0, 0, 0, 0, 0, 0]);
        clear();
    }

    #[test]
    fn the_list_is_the_threads_own() {
        clear();
        give(Vec::with_capacity(32));
        assert_eq!(kept().1[1], 1);
        // Another thread starts with nothing, and what it keeps ends with
        // it.
        let other = std::thread::spawn(|| {
            let before = kept();
            give(Vec::with_capacity(32));
            give(Vec::with_capacity(32));
            (before, kept())
        });
        let (before, after) = other.join().expect("a thread that ends");
        assert_eq!(before, (0, [0; KEPT.len()]));
        assert_eq!(after.1[1], 2);
        assert_eq!(kept().1[1], 1);

        // A list that is in use gives nothing and takes nothing: the
        // allocator is asked as it was before.
        with(|_| {
            assert_eq!(buffer(32).capacity(), 32);
            give(Vec::with_capacity(32));
            assert_eq!(
                *boxed(inner(b"k", b"v", BasicMerkNode)),
                inner(b"k", b"v", BasicMerkNode)
            );
        });
        assert_eq!(kept().1[1], 1);
        clear();
    }

    /// A Merk that is built, changed and opened again over four commits:
    /// its root hash after each commit, and the cost of each batch.
    fn a_run_of_a_merk(fill: u8) -> Vec<(CryptoHash, OperationCost)> {
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new(grove_version);
        let key = |n: u64| n.to_be_bytes().to_vec();
        let put = |n: u64, len: usize| (key(n), Op::Put(vec![fill; len], BasicMerkNode));
        let batches: [Vec<BatchEntry<Vec<u8>>>; 4] = [
            (0..120).map(|n| put(n, 20 + n as usize % 90)).collect(),
            // Values that change their length, some to another size class.
            (0..120)
                .step_by(3)
                .map(|n| put(n, 300 + n as usize * 7))
                .collect(),
            (1..120).step_by(4).map(|n| (key(n), Op::Delete)).collect(),
            (100..160).map(|n| put(n, 5)).collect(),
        ];
        let mut seen = Vec::new();
        for batch in &batches {
            let applied = merk.apply::<_, Vec<_>>(batch, &[], None, grove_version);
            applied.value.expect("a batch that applies");
            // The commit drops the Merk and opens it again: the next batch
            // loads the nodes on its way from the store.
            merk.commit(grove_version);
            seen.push((merk.root_hash().unwrap(), applied.cost));
        }
        seen
    }

    #[test]
    fn a_merk_answers_and_is_charged_alike_with_spare_memory() {
        let with_none = std::thread::spawn(|| a_run_of_a_merk(7));
        let with_spare = std::thread::spawn(|| {
            // What the thread keeps of this run held other bytes, and so
            // did the nodes that are given after it: whatever the engine
            // of the suite took from the list since, the next run begins
            // with spare boxes, keys and values.
            a_run_of_a_merk(9);
            let node = |n: u64| inner(&n.to_be_bytes(), &[9; 60], BasicMerkNode);
            let nodes: Vec<TreeNode> = (0..40)
                .map(|n| TreeNode::new_with_tree_inner(node(n)))
                .collect();
            nodes.into_iter().for_each(give_tree);
            (kept(), a_run_of_a_merk(7))
        });
        let with_none = with_none.join().expect("a run that ends");
        let ((boxes, bytes), with_spare) = with_spare.join().expect("a run that ends");
        assert!(
            boxes >= 40 && bytes[0] >= 40 && bytes[2] >= 40,
            "{boxes} boxes, {bytes:?}"
        );
        assert_eq!(with_none.len(), 4);
        assert_eq!(with_none, with_spare);
    }
}
