use super::*;
use crate::{
    segment::{self, BlockContent, Compression},
    tree_page::{
        tests::model::{ModelCell, ModelLink, ModelPage, ModelValue, Rng},
        CellField, Hash, HashField, Side, TreeCell, TreePage, TreePageError, TreePageMut,
    },
    value_log::{ValueLogError, ValuePointer},
    CellAddress, FileId, FormatVersion, SlotId,
};

mod applying;
mod chain;
mod reading;

const V1: FormatVersion = FormatVersion::V1;
const SIZES: [TreePageSize; 3] = [TreePageSize::MIN, TreePageSize::DEFAULT, TreePageSize::MAX];

fn page_id(raw: u32) -> PageId {
    PageId::new(raw).expect("non-zero")
}

fn slot(raw: u16) -> SlotId {
    SlotId::new(raw).expect("non-zero")
}

fn address(page: u32, raw_slot: u16) -> CellAddress {
    CellAddress::new(page_id(page), slot(raw_slot))
}

fn pointer(file: u32, offset: u64, value_len: u32) -> ValuePointer {
    ValuePointer::new(FileId::new(file).expect("non-zero"), offset, value_len).expect("valid")
}

fn delta_error(error: FormatError) -> DeltaError {
    match error {
        FormatError::Delta(error) => error,
        other => panic!("expected a delta error, got {other:?}"),
    }
}

/// A frame of `size` holding the page of `model`, built cell by cell.
fn frame_of(model: &ModelPage, size: TreePageSize) -> Vec<u8> {
    let mut frame = vec![0xEE; size.get()];
    let mut page = TreePageMut::init(&mut frame, size, model.id, model.departures).expect("fits");
    for (&raw, cell) in &model.cells {
        page.insert(slot(raw), &cell.content()).expect("fits");
    }
    frame
}

fn open(frame: &mut [u8], size: TreePageSize) -> TreePageMut<'_> {
    TreePageMut::open(frame, size, V1).expect("a page")
}

fn image_of(frame: &[u8]) -> Vec<u8> {
    TreePage::from_validated(frame, V1)
        .expect("a page")
        .to_image()
}

fn decode(bytes: &[u8]) -> DeltaRun<'_> {
    DeltaRun::decode(bytes, V1).expect("a run")
}

/// One change to a page, owning what it carries.
#[derive(Debug, Clone, PartialEq)]
enum Op {
    Insert(u16, ModelCell),
    Replace(u16, ModelCell),
    Remove(u16),
    Hash(u16, HashField, Hash),
    Child(u16, Side, CellAddress),
    Link(u16, Side, Option<ModelLink>),
    FragmentRoot(u16, bool),
    HostedRoot(u16, u8, Option<CellAddress>),
    HostedRoots(u16, Vec<Option<CellAddress>>),
    Feature(u16, Vec<u8>),
    Value(u16, ModelValue),
}

impl Op {
    /// The number of kinds of change.
    const KINDS: usize = 11;

    const fn kind(&self) -> usize {
        match self {
            Self::Insert(..) => 0,
            Self::Replace(..) => 1,
            Self::Remove(..) => 2,
            Self::Hash(..) => 3,
            Self::Child(..) => 4,
            Self::Link(..) => 5,
            Self::FragmentRoot(..) => 6,
            Self::HostedRoot(..) => 7,
            Self::HostedRoots(..) => 8,
            Self::Feature(..) => 9,
            Self::Value(..) => 10,
        }
    }

    /// Hands `then` the change as a delta. A cell is laid out by the model.
    fn with_delta<R>(&self, then: impl FnOnce(Delta<'_>) -> R) -> R {
        match self {
            Self::Insert(raw, cell) | Self::Replace(raw, cell) => {
                let bytes = cell.assemble();
                let cell = TreeCell::decode(&bytes, V1).expect("a cell");
                let slot = slot(*raw);
                then(match self {
                    Self::Insert(..) => Delta::Insert { slot, cell },
                    _ => Delta::Replace { slot, cell },
                })
            }
            Self::Remove(raw) => then(Delta::Remove { slot: slot(*raw) }),
            Self::Hash(raw, field, hash) => then(Delta::Hash {
                slot: slot(*raw),
                field: *field,
                hash,
            }),
            Self::Child(raw, side, child) => then(Delta::Child {
                slot: slot(*raw),
                side: *side,
                child: *child,
            }),
            Self::Link(raw, side, link) => then(Delta::Link {
                slot: slot(*raw),
                side: *side,
                link: link.as_ref().map(ModelLink::link),
            }),
            Self::FragmentRoot(raw, fragment_root) => then(Delta::FragmentRoot {
                slot: slot(*raw),
                fragment_root: *fragment_root,
            }),
            Self::HostedRoot(raw, index, root) => then(Delta::HostedRoot {
                slot: slot(*raw),
                index: *index,
                root: *root,
            }),
            Self::HostedRoots(raw, roots) => then(Delta::HostedRoots {
                slot: slot(*raw),
                roots: roots.as_slice().into(),
            }),
            Self::Feature(raw, feature) => then(Delta::Feature {
                slot: slot(*raw),
                feature,
            }),
            Self::Value(raw, value) => then(Delta::Value {
                slot: slot(*raw),
                value: value.value(),
            }),
        }
    }

    /// The change as its writer makes it, with nothing of this module.
    fn write(&self, page: &mut TreePageMut<'_>) -> Result<(), FormatError> {
        match self {
            Self::Insert(raw, cell) => page.insert(slot(*raw), &cell.content()),
            Self::Replace(raw, cell) => page.replace(slot(*raw), &cell.content()),
            Self::Remove(raw) => page.remove(slot(*raw)),
            Self::Hash(raw, field, hash) => page.set_hash(slot(*raw), *field, hash),
            Self::Child(raw, side, child) => page.set_child(slot(*raw), *side, *child),
            Self::Link(raw, side, link) => page.set_link(
                slot(*raw),
                *side,
                link.as_ref().map(ModelLink::link).as_ref(),
            ),
            Self::FragmentRoot(raw, fragment_root) => {
                page.set_fragment_root(slot(*raw), *fragment_root)
            }
            Self::HostedRoot(raw, index, root) => {
                page.set_hosted_root(slot(*raw), usize::from(*index), *root)
            }
            Self::HostedRoots(raw, roots) => page.set_hosted_roots(slot(*raw), roots),
            Self::Feature(raw, feature) => page.set_feature(slot(*raw), feature),
            Self::Value(raw, value) => page.set_value(slot(*raw), value.value()),
        }
    }

    /// The change, made to the model. Only for a change that a page took.
    fn make(&self, model: &mut ModelPage) {
        let cell = |model: &mut ModelPage, raw| model.cells.remove(&raw).expect("a cell");
        let (raw, cell) = match self.clone() {
            Self::Insert(raw, cell) => (raw, cell),
            Self::Replace(raw, new) => {
                assert_eq!(cell(model, raw).key, new.key);
                (raw, new)
            }
            Self::Remove(raw) => {
                cell(model, raw);
                model.departures = model.departures.wrapping_add(1);
                return;
            }
            Self::Hash(raw, field, hash) => {
                let mut cell = cell(model, raw);
                *cell.hash_mut(field).expect("a stored hash") = hash;
                (raw, cell)
            }
            Self::Child(raw, side, child) => {
                let mut cell = cell(model, raw);
                cell.link_mut(side).as_mut().expect("a link").child = child;
                (raw, cell)
            }
            Self::Link(raw, side, link) => {
                let mut cell = cell(model, raw);
                *cell.link_mut(side) = link;
                (raw, cell)
            }
            Self::FragmentRoot(raw, fragment_root) => {
                let mut cell = cell(model, raw);
                cell.fragment_root = fragment_root;
                (raw, cell)
            }
            Self::HostedRoot(raw, index, root) => {
                let mut cell = cell(model, raw);
                cell.hosted[usize::from(index)] = root;
                (raw, cell)
            }
            Self::HostedRoots(raw, hosted) => {
                let mut cell = cell(model, raw);
                cell.hosted = hosted;
                (raw, cell)
            }
            Self::Feature(raw, feature) => {
                let mut cell = cell(model, raw);
                cell.feature = feature;
                (raw, cell)
            }
            Self::Value(raw, value) => {
                let mut cell = cell(model, raw);
                cell.value = value;
                (raw, cell)
            }
        };
        model.cells.insert(raw, cell);
    }

    /// A change to the page of `model`: most often one the page takes, and
    /// now and then one it must refuse.
    fn random(rng: &mut Rng, model: &ModelPage, size: TreePageSize) -> Self {
        let own = model.id;
        let occupied: Vec<u16> = model.cells.keys().copied().collect();
        let raw = match occupied.is_empty() || rng.chance(3) {
            true => 1 + rng.below(model.slot_count() + 3) as u16,
            false => occupied[rng.below(occupied.len())],
        };
        let old = model.cells.get(&raw);
        let max_inline = match rng.chance(3) {
            true => size.max_cell_len() + 600,
            false => 300,
        };
        let side = Side::BOTH[rng.below(2)];
        match rng.below(100) {
            0..24 => {
                let raw = match rng.chance(85) {
                    true => model.lowest_free_slot(),
                    false => 1 + rng.below(model.slot_count() + 5) as u16,
                };
                Self::Insert(raw, rng.cell(own, max_inline))
            }
            24..36 => Self::Remove(raw),
            36..42 => {
                let mut cell = rng.cell(own, max_inline);
                if let Some(old) = old.filter(|_| rng.chance(90)) {
                    cell.key = old.key.clone();
                }
                Self::Replace(raw, cell)
            }
            42..62 => Self::Hash(raw, HashField::ALL[rng.below(4)], rng.hash()),
            62..68 => Self::Child(raw, side, rng.address(own)),
            68..80 => {
                let mut link = rng.chance(80).then(|| rng.link(own));
                // Often a link like the one it replaces, as after a write
                // below the node.
                if let (Some(new), Some(Some(old))) = (&mut link, old.map(|cell| cell.link(side)))
                    && rng.chance(60)
                {
                    new.aggregate = rng.bytes(old.aggregate.len());
                }
                Self::Link(raw, side, link)
            }
            80..84 => Self::FragmentRoot(raw, rng.chance(50)),
            84..88 => {
                let count = old.map_or(0, |cell| cell.hosted.len());
                let index = match rng.chance(5) {
                    true => 255,
                    false => rng.below(count + 2) as u8,
                };
                Self::HostedRoot(raw, index, rng.chance(60).then(|| rng.address(own)))
            }
            88..91 => Self::HostedRoots(raw, rng.hosted(own)),
            91..94 => {
                let len = match (old, rng.chance(50)) {
                    (Some(old), true) => old.feature.len(),
                    _ => rng.below(30),
                };
                Self::Feature(raw, rng.bytes(len))
            }
            _ => Self::Value(raw, rng.value(max_inline)),
        }
    }
}

/// The bytes of a run of `ops` for `page`, which creates the page if
/// `created` gives the count of departures it starts at.
fn run_of(page: PageId, created: Option<u64>, ops: &[Op]) -> Vec<u8> {
    let mut bytes = vec![0xA5; 3];
    let mut writer = match created {
        Some(departures) => DeltaRunWriter::new_page(page, departures, &mut bytes),
        None => DeltaRunWriter::new(page, &mut bytes),
    };
    for op in ops {
        op.with_delta(|delta| writer.push(&delta)).expect("encodes");
    }
    let len = writer.finish().expect("a run");
    assert_eq!(bytes.len(), 3 + len);
    assert_eq!(bytes[..3], [0xA5; 3]);
    bytes.split_off(3)
}

/// The page of the example in the documentation of tree pages: page 7,
/// from which two cells have left, with a cell in slots 1 and 3.
fn example() -> ModelPage {
    let mut page = ModelPage::new(7, 2);
    page.cells.insert(
        1,
        ModelCell {
            key: b"ab".to_vec(),
            feature: vec![0x03, 0x2A],
            value_hash: [0x11; 32],
            kv_hash: [0x22; 32],
            left: Some(ModelLink {
                child: address(7, 3),
                hash: [0x33; 32],
                heights: (1, 0),
                child_key_len: 3,
                aggregate: vec![0x05],
            }),
            right: None,
            hosted: vec![Some(address(9, 1)), None],
            value: ModelValue::OutOfLine(pointer(12, 20, 300)),
            fragment_root: true,
        },
    );
    page.cells.insert(
        3,
        ModelCell {
            value_hash: [0x44; 32],
            kv_hash: [0x55; 32],
            ..ModelCell::leaf(b"abc", b"v")
        },
    );
    page
}

/// A run with a record of every kind that changes a cell, written out by
/// hand, and the changes it holds.
fn golden() -> (Vec<u8>, Vec<Op>) {
    let mut bytes = vec![0x07, 0x00, 0x00, 0x00]; // page 7
    let mut ops = Vec::new();

    bytes.extend([0x05, 0x01, 0x00]); // value hash of slot 1
    bytes.extend([0x66; 32]);
    ops.push(Op::Hash(1, HashField::Value, [0x66; 32]));

    bytes.extend([0x06, 0x01, 0x00]); // key-value hash of slot 1
    bytes.extend([0x77; 32]);
    ops.push(Op::Hash(1, HashField::KeyValue, [0x77; 32]));

    bytes.extend([0x07, 0x01, 0x00]); // hash of the left child of slot 1
    bytes.extend([0x88; 32]);
    ops.push(Op::Hash(1, HashField::Child(Side::Left), [0x88; 32]));

    bytes.extend([0x09, 0x01, 0x00]); // the left child of slot 1 is now
    bytes.extend([0x07, 0x00, 0x00, 0x00, 0x02, 0x00]); // in page 7, slot 2
    ops.push(Op::Child(1, Side::Left, address(7, 2)));

    bytes.extend([0x0C, 0x01, 0x00]); // a right link for slot 1
    bytes.extend([0x09, 0x00, 0x00, 0x00, 0x05, 0x00]); // child: page 9, slot 5
    bytes.extend([0x99; 32]); // its hash
    bytes.extend([0x02, 0x01]); // its heights: 2, 1
    bytes.extend([0x04]); // its key is 4 bytes long
    bytes.extend([0x01, 0x2A]); // aggregate: 1 byte, 2a
    let right = ModelLink {
        child: address(9, 5),
        hash: [0x99; 32],
        heights: (2, 1),
        child_key_len: 4,
        aggregate: vec![0x2A],
    };
    ops.push(Op::Link(1, Side::Right, Some(right)));

    bytes.extend([0x08, 0x01, 0x00]); // hash of the right child of slot 1
    bytes.extend([0xAA; 32]);
    ops.push(Op::Hash(1, HashField::Child(Side::Right), [0xAA; 32]));

    bytes.extend([0x0A, 0x01, 0x00]); // the right child of slot 1 is now
    bytes.extend([0x0A, 0x00, 0x00, 0x00, 0x01, 0x00]); // in page 10, slot 1
    ops.push(Op::Child(1, Side::Right, address(10, 1)));

    bytes.extend([0x0D, 0x01, 0x00]); // slot 1 has no left link
    ops.push(Op::Link(1, Side::Left, None));

    bytes.extend([0x0B, 0x03, 0x00]); // a left link for slot 3
    bytes.extend([0x07, 0x00, 0x00, 0x00, 0x02, 0x00]); // child: page 7, slot 2
    bytes.extend([0xBB; 32]); // its hash
    bytes.extend([0x00, 0x00]); // its heights: 0, 0
    bytes.extend([0x01]); // its key is 1 byte long
    bytes.extend([0x00]); // no aggregate
    let left = ModelLink {
        child: address(7, 2),
        hash: [0xBB; 32],
        heights: (0, 0),
        child_key_len: 1,
        aggregate: Vec::new(),
    };
    ops.push(Op::Link(3, Side::Left, Some(left)));

    bytes.extend([0x0E, 0x01, 0x00]); // slot 1 has no right link
    ops.push(Op::Link(1, Side::Right, None));

    bytes.extend([0x10, 0x01, 0x00]); // slot 1 is not a fragment root
    ops.push(Op::FragmentRoot(1, false));

    bytes.extend([0x0F, 0x03, 0x00]); // slot 3 is a fragment root
    ops.push(Op::FragmentRoot(3, true));

    bytes.extend([0x11, 0x01, 0x00, 0x01]); // hosted root 1 of slot 1:
    bytes.extend([0x08, 0x00, 0x00, 0x00, 0x02, 0x00]); // page 8, slot 2
    ops.push(Op::HostedRoot(1, 1, Some(address(8, 2))));

    bytes.extend([0x11, 0x01, 0x00, 0x00]); // hosted root 0 of slot 1:
    bytes.extend([0x00; 6]); // none
    ops.push(Op::HostedRoot(1, 0, None));

    bytes.extend([0x12, 0x03, 0x00, 0x02]); // slot 3 has 2 hosted roots:
    bytes.extend([0x00; 6]); // none
    bytes.extend([0x0B, 0x00, 0x00, 0x00, 0x03, 0x00]); // page 11, slot 3
    ops.push(Op::HostedRoots(3, vec![None, Some(address(11, 3))]));

    bytes.extend([0x13, 0x01, 0x00, 0x03]); // feature of slot 1, 3 bytes
    bytes.extend([0x01, 0x02, 0x03]);
    ops.push(Op::Feature(1, vec![0x01, 0x02, 0x03]));

    bytes.extend([0x14, 0x03, 0x00]); // value of slot 3,
    bytes.extend([0x02, 0x00, 0x00, 0x00]); // 2 bytes
    bytes.extend(b"vw");
    ops.push(Op::Value(3, ModelValue::Inline(b"vw".to_vec())));

    bytes.extend([0x15, 0x01, 0x00]); // the value of slot 1 is
    bytes.extend([0x0D, 0x00, 0x00, 0x00]); // in file 13
    bytes.extend([0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]); // at offset 64
    bytes.extend([0x2C, 0x01, 0x00, 0x00]); // and 300 bytes long
    ops.push(Op::Value(1, ModelValue::OutOfLine(pointer(13, 64, 300))));

    bytes.extend([0x02, 0x02, 0x00]); // a cell for slot 2,
    bytes.extend([0x4A, 0x00]); // 74 bytes:
    bytes.extend([0x00, 0x01, 0x00, 0x00]); // no flags. key 1, no feature, no hosted root
    bytes.extend([0x01, 0x00, 0x00, 0x00]); // value length 1
    bytes.extend([0xCC; 32]); // value hash
    bytes.extend([0xDD; 32]); // key-value hash
    bytes.extend(b"b"); // key
    bytes.extend(b"w"); // value
    let inserted = ModelCell {
        value_hash: [0xCC; 32],
        kv_hash: [0xDD; 32],
        ..ModelCell::leaf(b"b", b"w")
    };
    ops.push(Op::Insert(2, inserted.clone()));

    bytes.extend([0x03, 0x02, 0x00]); // another cell for the node in slot 2,
    bytes.extend([0x4B, 0x00]); // 75 bytes:
    bytes.extend([0x08, 0x01, 0x00, 0x00]); // fragment root. key 1
    bytes.extend([0x02, 0x00, 0x00, 0x00]); // value length 2
    bytes.extend([0xCC; 32]); // value hash
    bytes.extend([0xEE; 32]); // key-value hash
    bytes.extend(b"b"); // key
    bytes.extend(b"wx"); // value
    let replaced = ModelCell {
        kv_hash: [0xEE; 32],
        value: ModelValue::Inline(b"wx".to_vec()),
        fragment_root: true,
        ..inserted
    };
    ops.push(Op::Replace(2, replaced));

    bytes.extend([0x04, 0x03, 0x00]); // the cell of slot 3 leaves the page
    ops.push(Op::Remove(3));

    (bytes, ops)
}

#[test]
fn golden_bytes() {
    let (bytes, ops) = golden();
    assert_eq!(bytes.len(), 498);
    let kinds: Vec<usize> = ops.iter().map(Op::kind).collect();
    assert!((0..Op::KINDS).all(|kind| kinds.contains(&kind)));

    // What the writer writes for these changes.
    assert_eq!(run_of(page_id(7), None, &ops), bytes);

    // What the reader reads from these bytes.
    let run = decode(&bytes);
    assert_eq!(run.page(), page_id(7));
    assert_eq!(run.created(), None);
    assert_eq!(run.version(), V1);
    assert_eq!(run.as_bytes(), bytes);
    assert_eq!(run.encoded_len(), bytes.len());
    assert_eq!(run.deltas().len(), ops.len());
    assert_eq!(run.deltas().size_hint(), (ops.len(), Some(ops.len())));
    let mut deltas = run.deltas();
    for op in &ops {
        let delta = deltas.next().expect("a change");
        op.with_delta(|expected| {
            assert_eq!(delta, expected);
            assert_eq!(delta.slot(), expected.slot());
            assert_eq!(delta.encoded_len(), expected.encoded_len());
        });
    }
    assert_eq!(deltas.len(), 0);
    assert_eq!(deltas.next(), None);
    assert_eq!(deltas.next(), None);

    // What the page of the example becomes.
    let size = TreePageSize::DEFAULT;
    let mut model = example();
    let base = model.assemble();
    let mut frame = frame_of(&model, size);
    run.apply(&mut frame, size, V1).expect("applies");
    for op in &ops {
        op.make(&mut model);
    }
    assert_eq!(model.departures, 3);
    assert_eq!(model.cells.keys().copied().collect::<Vec<_>>(), [1, 2]);
    assert_eq!(image_of(&frame), model.assemble());
    model.assert_is(TreePage::from_validated(&frame, V1).expect("a page"), size);
    let mut image = Vec::new();
    consolidate(Some(&base), [&run], V1, size, &mut image).expect("consolidates");
    assert_eq!(image, model.assemble());
}

/// The run of the documentation's example: a right link, a new cell and a
/// cell that leaves.
#[test]
fn the_documented_run_is_132_bytes() {
    let (golden, ops) = golden();
    let picked: Vec<Op> = ops
        .into_iter()
        .filter(|op| {
            matches!(
                op,
                Op::Link(1, Side::Right, Some(_)) | Op::Insert(..) | Op::Remove(_)
            )
        })
        .collect();
    assert_eq!(picked.len(), 3);
    let bytes = run_of(page_id(7), None, &picked);
    assert_eq!(bytes.len(), 132);
    // Its records are the ones the golden run has, byte for byte.
    let mut at = DeltaRun::HEADER_LEN;
    for op in &picked {
        let len = op.with_delta(|delta| delta.encoded_len());
        let record = &bytes[at..at + len];
        assert!(golden.windows(len).any(|window| window == record));
        at += len;
    }
    assert_eq!(at, bytes.len());
    assert_eq!(bytes[4..7], [0x0C, 0x01, 0x00]);
    assert_eq!(bytes[50..55], [0x02, 0x02, 0x00, 0x4A, 0x00]);
    assert_eq!(bytes[129..], [0x04, 0x03, 0x00]);

    // The record that gives a value its place.
    let install = Op::Value(1, ModelValue::OutOfLine(pointer(13, 64, 300)));
    let bytes = run_of(page_id(7), None, std::slice::from_ref(&install));
    assert_eq!(
        bytes[4..],
        [
            0x15, 0x01, 0x00, 0x0D, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x2C, 0x01, 0x00, 0x00
        ]
    );
}

#[test]
fn a_run_that_creates_its_page_starts_with_the_create_record() {
    let cell = ModelCell::leaf(b"k", b"v");
    let insert = Op::Insert(1, cell.clone());
    let mut expected = vec![
        0x09, 0x00, 0x00, 0x00, // page 9
        0x01, // create the page
        0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // 5 departures
        0x02, 0x01, 0x00, // a cell for slot 1,
        0x4A, 0x00, // 74 bytes
    ];
    expected.extend(cell.assemble());
    let bytes = run_of(page_id(9), Some(5), std::slice::from_ref(&insert));
    assert_eq!(bytes, expected);
    assert_eq!(bytes.len(), 92);

    let run = decode(&bytes);
    assert_eq!(run.page(), page_id(9));
    assert_eq!(run.created(), Some(5));
    assert_eq!(run.deltas().len(), 1);
    insert.with_delta(|delta| assert_eq!(run.deltas().next(), Some(delta)));

    // The create record alone is a run: an empty page.
    let alone = run_of(page_id(9), Some(u64::MAX), &[]);
    assert_eq!(alone.len(), DeltaRun::HEADER_LEN + 9);
    let run = decode(&alone);
    assert_eq!(run.created(), Some(u64::MAX));
    assert_eq!(run.deltas().next(), None);
    let mut image = Vec::new();
    consolidate(None, [&run], V1, TreePageSize::MIN, &mut image).expect("consolidates");
    assert_eq!(image, ModelPage::new(9, u64::MAX).assemble());
}

/// A page of `size` filled with random cells until no more fit, with its
/// model.
fn full_page(seed: u64, size: TreePageSize) -> (Vec<u8>, ModelPage) {
    let mut rng = Rng::new(seed);
    let mut model = ModelPage::new(21, 0);
    let mut frame = frame_of(&model, size);
    loop {
        let raw = model.lowest_free_slot();
        let cell = rng.cell(model.id, 400);
        match open(&mut frame, size).insert(slot(raw), &cell.content()) {
            Ok(()) => drop(model.cells.insert(raw, cell)),
            Err(_) => return (frame, model),
        }
    }
}

/// The acceptance test of the task: a hash-only delta is at most 48 bytes.
///
/// The record of one changed hash is 35 bytes. A run that holds it is 39
/// with the page id, and 47 as the block a segment stores, with the block's
/// length, kind and checksum. Applying it writes those 32 bytes and no
/// other byte of the page.
#[test]
fn a_hash_only_delta_is_at_most_48_bytes() {
    assert_eq!(HASH_RECORD_LEN, 35);

    // The documented bytes, with the checksum of an independent CRC-32C.
    let documented = Op::Hash(3, HashField::KeyValue, [0x55; 32]);
    let run = run_of(page_id(7), None, std::slice::from_ref(&documented));
    let mut expected_run = vec![0x07, 0x00, 0x00, 0x00, 0x06, 0x03, 0x00];
    expected_run.extend([0x55; 32]);
    assert_eq!(run, expected_run);
    let mut expected_block = vec![0x27, 0x00, 0x00, 0x18, 0x9C, 0x0B, 0x7A, 0xC7];
    expected_block.extend(&expected_run);
    let mut block = Vec::new();
    segment::write_block(BlockContent::Delta, Compression::Raw, &run, &mut block).expect("a block");
    assert_eq!(block, expected_block);
    assert_eq!(block.len(), 47);

    let mut rng = Rng::new(0x4845);
    for size in SIZES {
        let (mut frame, model) = full_page(0x48A5 + size.get() as u64, size);
        assert!(model.cells.len() >= 20, "{} cells", model.cells.len());
        for (&raw, cell) in &model.cells {
            for field in HashField::ALL {
                let hash = rng.hash();
                let op = Op::Hash(raw, field, hash);
                op.with_delta(|delta| assert_eq!(delta.encoded_len(), HASH_RECORD_LEN));
                let run = run_of(model.id, None, std::slice::from_ref(&op));
                assert_eq!(run.len(), DeltaRun::HEADER_LEN + HASH_RECORD_LEN);
                assert_eq!(run.len(), 39);
                // The record holds the hash and nothing else of the cell.
                assert_eq!(run[run.len() - 32..], hash);

                // As a segment stores it, compressed or not.
                for compression in [Compression::Raw, Compression::Lz4] {
                    let mut block = Vec::new();
                    segment::write_block(BlockContent::Delta, compression, &run, &mut block)
                        .expect("a block");
                    assert!(block.len() <= 48, "a block of {} bytes", block.len());
                    if compression == Compression::Raw {
                        assert_eq!(block.len(), 47);
                    }
                }

                let before = frame.clone();
                let result = decode(&run).apply(&mut frame, size, V1);
                if cell.hash(field).is_none() {
                    let HashField::Child(side) = field else {
                        panic!("a node has its own hashes")
                    };
                    let refused = TreePageError::NoSuchLink {
                        slot: slot(raw),
                        side,
                    };
                    assert_eq!(result, Err(refused.into()));
                    assert!(frame == before);
                    continue;
                }
                result.expect("a stored hash");
                let changed: Vec<usize> = (0..frame.len())
                    .filter(|&at| frame[at] != before[at])
                    .collect();
                let page = TreePage::from_validated(&frame, V1).expect("a page");
                let stored = page.cell(slot(raw)).expect("a cell");
                assert_eq!(stored.hash(field), Some(&hash));
                let at = frame
                    .windows(32)
                    .position(|window| window == hash)
                    .expect("the hash is in the page");
                assert!(changed.iter().all(|&byte| (at..at + 32).contains(&byte)));
                assert!(!changed.is_empty());
                // The value is where it was, and so is the rest of the cell.
                let old = TreePage::from_validated(&before, V1).expect("a page");
                let old = old.cell(slot(raw)).expect("a cell");
                assert_eq!(stored.value(), old.value());
                assert_eq!(stored.key(), old.key());
                assert_eq!(stored.encoded_len(), old.encoded_len());
            }
        }
    }

    // Every further hash of the same page costs 35 bytes more.
    let hashes: Vec<Op> = (1..=20)
        .map(|raw| Op::Hash(raw, HashField::Value, rng.hash()))
        .collect();
    assert_eq!(run_of(page_id(1), None, &hashes).len(), 4 + 20 * 35);
}

#[test]
fn sizes_are_the_documented_ones() {
    assert_eq!(DeltaRun::HEADER_LEN, 4);
    assert_eq!(HASH_RECORD_LEN, 35);
    assert_eq!(MAX_CELL_LEN, 32_754);
    assert_eq!(MAX_CELL_LEN, TreePageSize::MAX.max_cell_len());

    let len = |op: &Op| {
        let len = op.with_delta(|delta| delta.encoded_len());
        let run = run_of(page_id(1), None, std::slice::from_ref(op));
        assert_eq!(run.len(), DeltaRun::HEADER_LEN + len);
        len
    };
    let cell = ModelCell::leaf(b"key", b"value");
    assert_eq!(cell.len(), 80);
    assert_eq!(len(&Op::Insert(1, cell.clone())), 85);
    assert_eq!(len(&Op::Replace(1, cell)), 85);
    assert_eq!(len(&Op::Remove(1)), 3);
    for field in HashField::ALL {
        assert_eq!(len(&Op::Hash(1, field, [7; 32])), 35);
    }
    for side in Side::BOTH {
        assert_eq!(len(&Op::Child(1, side, address(2, 3))), 9);
        assert_eq!(len(&Op::Link(1, side, None)), 3);
        // A link is as long as its tree's kind of aggregate makes it.
        for (aggregate_len, link_len) in [(0, 42), (1, 43), (9, 51), (17, 59), (25, 67), (255, 297)]
        {
            let link = ModelLink {
                child: address(2, 3),
                hash: [8; 32],
                heights: (1, 2),
                child_key_len: 3,
                aggregate: vec![4; aggregate_len],
            };
            assert_eq!(link.link().encoded_len(), link_len);
            assert_eq!(len(&Op::Link(1, side, Some(link))), 3 + link_len);
        }
    }
    assert_eq!(len(&Op::FragmentRoot(1, true)), 3);
    assert_eq!(len(&Op::FragmentRoot(1, false)), 3);
    assert_eq!(len(&Op::HostedRoot(1, 0, None)), 10);
    assert_eq!(len(&Op::HostedRoot(1, 255, Some(address(2, 3)))), 10);
    assert_eq!(len(&Op::HostedRoots(1, Vec::new())), 4);
    assert_eq!(len(&Op::HostedRoots(1, vec![None; 255])), 4 + 6 * 255);
    assert_eq!(len(&Op::Feature(1, Vec::new())), 4);
    assert_eq!(len(&Op::Feature(1, vec![1; 255])), 259);
    assert_eq!(len(&Op::Value(1, ModelValue::Inline(Vec::new()))), 7);
    assert_eq!(len(&Op::Value(1, ModelValue::Inline(vec![1; 1000]))), 1007);
    // The record that installs a value's place in the value log.
    let install = Op::Value(1, ModelValue::OutOfLine(pointer(1, 20, 1)));
    assert_eq!(len(&install), 19);
}

#[test]
fn every_error_has_its_message() {
    let cases = [
        (DeltaError::EmptyRun, "a run with no record"),
        (
            DeltaError::MisplacedCreate,
            "a create record that is not the first of its run",
        ),
        (
            DeltaError::WrongPage {
                run: page_id(7),
                page: page_id(9),
            },
            "a run of page 7 applied to page 9",
        ),
        (
            DeltaError::MissingBase,
            "a chain that does not create its page, and no base image",
        ),
    ];
    for (error, message) in cases {
        assert_eq!(error.to_string(), message);
        let wrapped = FormatError::from(error);
        assert_eq!(wrapped, FormatError::Delta(error));
        assert_eq!(wrapped.to_string(), format!("page delta: {message}"));
        assert_eq!(delta_error(wrapped), error);
        assert!(std::error::Error::source(&error).is_none());
    }
}
