//! A run of changes to one flat page: its bytes, and what it makes of a
//! page.

use std::collections::BTreeMap;

use super::*;
use crate::{
    flat_page::{FlatPage, FlatPageError},
    value_log::ValueLogError,
};

fn run_of(raw_page: u32, changes: &[FlatChange<'_>]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut run = FlatRunWriter::new(page(raw_page), &mut bytes);
    for change in changes {
        run.push(change).expect("a record");
    }
    run.finish().expect("a run");
    bytes
}

fn read(bytes: &[u8]) -> FlatRun<'_> {
    FlatRun::decode(bytes, V1).expect("a run")
}

/// The page of the images these tests build, and of the runs they apply.
const PAGE: u32 = 5;

/// The image of a leaf of page 5 with no entry and no prefix.
const EMPTY_LEAF: [u8; 12] = [1, 0, 12, 0, 5, 0, 0, 0, 0, 0, 0, 0];

fn leaf_of(prefix: &[u8], entries: &BTreeMap<Vec<u8>, (bool, Vec<u8>)>) -> Vec<u8> {
    let mut leaf = FlatLeafBuilder::new(page(PAGE), prefix, CAPACITY).expect("a leaf");
    for (key, (out_of_line, bytes)) in entries {
        let value = if *out_of_line {
            FlatValue::OutOfLine(bytes)
        } else {
            FlatValue::Inline(bytes)
        };
        leaf.push(key, value).expect("fits");
    }
    leaf.finish()
}

fn branch_of(prefix: &[u8], first: u32, separators: &BTreeMap<Vec<u8>, u32>) -> Vec<u8> {
    let mut branch =
        FlatBranchBuilder::new(page(PAGE), prefix, CAPACITY, 2, page(first)).expect("a branch");
    for (key, child) in separators {
        branch.push(key, page(*child)).expect("fits");
    }
    branch.finish()
}

/// One change to a leaf, owning what it carries.
#[derive(Debug, Clone)]
enum Op {
    Insert(Vec<u8>, bool, Vec<u8>),
    Replace(Vec<u8>, bool, Vec<u8>),
    Remove(Vec<u8>),
    RemoveRange(Vec<u8>, u16),
    SetPrefix(Vec<u8>),
}

impl Op {
    const KINDS: usize = 5;

    const fn kind(&self) -> usize {
        match self {
            Self::Insert(..) => 0,
            Self::Replace(..) => 1,
            Self::Remove(..) => 2,
            Self::RemoveRange(..) => 3,
            Self::SetPrefix(..) => 4,
        }
    }

    fn change(&self) -> FlatChange<'_> {
        let value = |out_of_line: &bool, bytes| {
            if *out_of_line {
                FlatValue::OutOfLine(bytes)
            } else {
                FlatValue::Inline(bytes)
            }
        };
        match self {
            Self::Insert(key, out_of_line, bytes) => FlatChange::Insert {
                key,
                value: value(out_of_line, bytes),
            },
            Self::Replace(key, out_of_line, bytes) => FlatChange::Replace {
                key,
                value: value(out_of_line, bytes),
            },
            Self::Remove(key) => FlatChange::Remove { key },
            Self::RemoveRange(key, count) => FlatChange::RemoveRange { key, count: *count },
            Self::SetPrefix(prefix) => FlatChange::SetPrefix { prefix },
        }
    }
}

fn applied(run: &[u8], base: Option<&[u8]>) -> Result<Vec<u8>, FormatError> {
    let mut out = b"kept".to_vec();
    let result = read(run).apply(base, V1, CAPACITY, &mut out);
    if result.is_err() {
        assert_eq!(out, b"kept");
    }
    result.map(|()| out[4..].to_vec())
}

#[test]
fn golden_bytes() {
    let place = ValuePlace::Placed(pointer(3, 20, 70_000)).encode();
    let image = [1, 0, 12, 0, 12, 0, 0, 0, 0, 0, 0, 0];
    let changes = [
        FlatChange::Image(&image),
        FlatChange::Insert {
            key: b"user/alice",
            value: FlatValue::Inline(b"A1"),
        },
        FlatChange::Replace {
            key: b"user/bob",
            value: FlatValue::OutOfLine(&place),
        },
        FlatChange::Remove { key: b"user/carol" },
        FlatChange::RemoveRange {
            key: b"user/d",
            count: 3,
        },
        FlatChange::InsertChild {
            key: b"user/m",
            child: page(9),
        },
        FlatChange::SetPrefix { prefix: b"user/" },
        FlatChange::Insert {
            key: b"",
            value: FlatValue::Inline(b""),
        },
    ];
    #[rustfmt::skip]
    let golden: Vec<u8> = [
        &[12, 0, 0, 0][..],                                   // page 12
        &[1, 12, 0, 1, 0, 12, 0, 12, 0, 0, 0, 0, 0, 0, 0],    // image: page 12, an empty leaf
        &[2, 10, 0], b"user/alice", &[0, 2, 0], b"A1",        // insert, inline
        &[3, 8, 0], b"user/bob", &[1, 16, 0],                 // replace, out of line
        &[3, 0, 0, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0x70, 0x11, 1, 0],
        &[4, 10, 0], b"user/carol",                           // remove
        &[5, 6, 0], b"user/d", &[3, 0],                       // remove 3 from a key
        &[6, 6, 0], b"user/m", &[9, 0, 0, 0],                 // insert a child
        &[7, 5, 0], b"user/",                                 // set the prefix
        &[2, 0, 0, 0, 0, 0],                                  // the empty key and value
    ]
    .concat();
    let bytes = run_of(12, &changes);
    assert_eq!(bytes, golden);

    let run = read(&golden);
    assert_eq!(run.page(), page(12));
    assert_eq!(run.as_bytes(), golden);
    assert_eq!(run.encoded_len(), golden.len());
    assert_eq!(run.version(), V1);
    assert_eq!(run.image(), Some(&image[..]));
    assert_eq!(run.changes().len(), changes.len());
    assert_eq!(run.changes().collect::<Vec<_>>(), changes);
    assert_eq!(FlatRun::HEADER_LEN, 4);
    assert_eq!(read(&run_of(1, &changes[1..2])).image(), None);
}

#[test]
fn a_page_and_a_run_give_the_page_a_model_gives() {
    let mut rng = SplitMix64::new(0xF1A7);
    let prefix = b"sub/".to_vec();
    let mut model: BTreeMap<Vec<u8>, (bool, Vec<u8>)> = BTreeMap::new();
    let mut stored = prefix.clone();
    let mut image = leaf_of(&stored, &model);
    let mut counts = [0usize; Op::KINDS];
    for round in 0..600 {
        // A run of drawn changes that fit the page, made to the model as
        // they are written.
        let mut ops: Vec<Op> = Vec::new();
        let mut restart = None;
        if round % 37 == 36 {
            restart = Some(leaf_of(&stored, &model));
        }
        for _ in 0..1 + rng.next_u64() % 5 {
            let present: Vec<Vec<u8>> = model.keys().cloned().collect();
            let pick = |rng: &mut SplitMix64| {
                present[(rng.next_u64() % present.len() as u64) as usize].clone()
            };
            let out_of_line = rng.next_u64().is_multiple_of(4);
            let bytes = if out_of_line {
                random_place(&mut rng, 900).encode().to_vec()
            } else {
                let len = (rng.next_u64() % 20) as usize;
                rng.bytes(len)
            };
            match rng.next_u64() % 10 {
                0..=3 if model.len() < 60 => {
                    let len = (rng.next_u64() % 6) as usize;
                    let key = [&prefix[..], &rng.bytes(len)].concat();
                    if model.contains_key(&key) {
                        continue;
                    }
                    model.insert(key.clone(), (out_of_line, bytes.clone()));
                    ops.push(Op::Insert(key, out_of_line, bytes));
                }
                4 | 5 if !present.is_empty() => {
                    let key = pick(&mut rng);
                    model.insert(key.clone(), (out_of_line, bytes.clone()));
                    ops.push(Op::Replace(key, out_of_line, bytes));
                }
                6 | 7 if !present.is_empty() => {
                    let key = pick(&mut rng);
                    model.remove(&key);
                    ops.push(Op::Remove(key));
                }
                8 if present.len() >= 2 => {
                    let from = (rng.next_u64() % (present.len() as u64 - 1)) as usize;
                    let count = 2 + (rng.next_u64() % (present.len() - from - 1) as u64) as usize;
                    for key in &present[from..from + count] {
                        model.remove(key);
                    }
                    ops.push(Op::RemoveRange(present[from].clone(), count as u16));
                }
                9 => {
                    // Any prefix that every key starts with is one the
                    // page may store, not only the longest.
                    let len = (rng.next_u64() % (prefix.len() as u64 + 1)) as usize;
                    stored = prefix[..len].to_vec();
                    ops.push(Op::SetPrefix(stored.clone()));
                }
                _ => {}
            }
        }
        if ops.is_empty() && restart.is_none() {
            continue;
        }
        let mut changes: Vec<FlatChange<'_>> = restart
            .iter()
            .map(|image| FlatChange::Image(image))
            .collect();
        for op in &ops {
            counts[op.kind()] += 1;
            changes.push(op.change());
        }
        let run = run_of(5, &changes);
        assert_eq!(read(&run).changes().collect::<Vec<_>>(), changes);

        // A run that starts from an image needs no page, and is not given
        // the page it replaces.
        let base = restart.is_none().then_some(&image[..]);
        let next = applied(&run, base).expect("applies");
        assert_eq!(next, leaf_of(&stored, &model), "round {round}");
        if restart.is_some() {
            assert_eq!(applied(&run, Some(&image)), Ok(next.clone()));
        }
        image = next;
    }
    println!("{counts:?} inserts, replaces, removes, ranges and prefixes");
    assert!(counts.iter().all(|count| *count > 30), "{counts:?}");
}

#[test]
fn a_branch_takes_separators_and_gives_them_up() {
    let mut rng = SplitMix64::new(0xB4A9C);
    let mut model: BTreeMap<Vec<u8>, u32> = BTreeMap::new();
    let mut image = branch_of(b"", 7, &model);
    for round in 0..300 {
        let present: Vec<Vec<u8>> = model.keys().cloned().collect();
        let key = rng.bytes(1 + (round % 3));
        let child = 1 + (rng.next_u64() % 900) as u32;
        let change = match rng.next_u64() % 4 {
            0 if present.len() >= 2 => {
                let from = (rng.next_u64() % (present.len() as u64 - 1)) as usize;
                let count = 2.min(present.len() - from);
                present[from..from + count].iter().for_each(|key| {
                    model.remove(key);
                });
                FlatChange::RemoveRange {
                    key: &present[from],
                    count: count as u16,
                }
            }
            1 if !present.is_empty() => {
                let key = &present[(rng.next_u64() % present.len() as u64) as usize];
                model.remove(key);
                FlatChange::Remove { key }
            }
            _ if !model.contains_key(&key) && model.len() < 80 => {
                model.insert(key.clone(), child);
                FlatChange::InsertChild {
                    key: &key,
                    child: page(child),
                }
            }
            _ => continue,
        };
        image = applied(&run_of(5, &[change]), Some(&image)).expect("applies");
        assert_eq!(image, branch_of(b"", 7, &model), "round {round}");
    }
    let page = FlatPage::decode(&image, V1).unwrap();
    assert_eq!(page.branch().map(|branch| branch.level()), Some(2));
}

#[test]
fn a_change_that_does_not_fit_its_page_is_refused() {
    let leaf = {
        let mut model = BTreeMap::new();
        for key in [&b"pa"[..], b"pb", b"pc"] {
            model.insert(key.to_vec(), (false, b"v".to_vec()));
        }
        leaf_of(b"p", &model)
    };
    let branch = branch_of(b"", 7, &BTreeMap::from([(b"m".to_vec(), 8)]));
    let inline = FlatValue::Inline(b"w");
    let refused = |change: FlatChange<'_>, base: &[u8]| {
        applied(&run_of(5, &[change]), Some(base)).expect_err("refused")
    };

    assert_eq!(
        refused(
            FlatChange::Insert {
                key: b"pb",
                value: inline
            },
            &leaf
        ),
        wal_error(WalError::KeyPresent)
    );
    assert_eq!(
        refused(
            FlatChange::InsertChild {
                key: b"m",
                child: page(9)
            },
            &branch
        ),
        wal_error(WalError::KeyPresent)
    );
    let absent = wal_error(WalError::KeyAbsent);
    assert_eq!(
        refused(
            FlatChange::Replace {
                key: b"pd",
                value: inline
            },
            &leaf
        ),
        absent
    );
    assert_eq!(refused(FlatChange::Remove { key: b"p" }, &leaf), absent);
    assert_eq!(refused(FlatChange::Remove { key: b"n" }, &branch), absent);
    assert_eq!(
        refused(
            FlatChange::RemoveRange {
                key: b"pbb",
                count: 2
            },
            &leaf
        ),
        absent
    );
    let too_long = wal_error(WalError::RangeTooLong);
    assert_eq!(
        refused(
            FlatChange::RemoveRange {
                key: b"pb",
                count: 3
            },
            &leaf
        ),
        too_long
    );
    assert_eq!(
        refused(
            FlatChange::RemoveRange {
                key: b"m",
                count: 2
            },
            &branch
        ),
        too_long
    );
    assert_eq!(
        applied(
            &run_of(
                5,
                &[FlatChange::RemoveRange {
                    key: b"pa",
                    count: 3
                }]
            ),
            Some(&leaf)
        ),
        Ok(leaf_of(b"p", &BTreeMap::new()))
    );

    let other_kind = wal_error(WalError::WrongPageKind);
    assert_eq!(
        refused(
            FlatChange::Insert {
                key: b"k",
                value: inline
            },
            &branch
        ),
        other_kind
    );
    assert_eq!(
        refused(
            FlatChange::Replace {
                key: b"m",
                value: inline
            },
            &branch
        ),
        other_kind
    );
    assert_eq!(
        refused(
            FlatChange::InsertChild {
                key: b"pd",
                child: page(9)
            },
            &leaf
        ),
        other_kind
    );

    // What the page itself refuses: a prefix that a key does not start
    // with, and an entry it has no room for.
    assert_eq!(
        refused(FlatChange::SetPrefix { prefix: b"pa" }, &leaf),
        FormatError::FlatPage(FlatPageError::KeyOutsidePrefix)
    );
    assert_eq!(
        refused(
            FlatChange::Insert {
                key: b"q",
                value: inline
            },
            &leaf
        ),
        FormatError::FlatPage(FlatPageError::KeyOutsidePrefix)
    );
    let big = vec![7; CAPACITY];
    assert!(matches!(
        refused(
            FlatChange::Insert {
                key: b"pz",
                value: FlatValue::Inline(&big)
            },
            &leaf
        ),
        FormatError::FlatPage(FlatPageError::PageFull { .. })
    ));
    assert!(matches!(
        refused(
            FlatChange::InsertChild {
                key: &big,
                child: page(9)
            },
            &branch
        ),
        FormatError::FlatPage(FlatPageError::PageFull { .. })
    ));

    // The prefix and the room are asked of the image the run leaves, not
    // of what the page would be between two of its records.
    let between = run_of(
        5,
        &[
            FlatChange::SetPrefix { prefix: b"z" },
            FlatChange::Insert {
                key: b"pz",
                value: FlatValue::Inline(&big),
            },
            FlatChange::Remove { key: b"pz" },
            FlatChange::SetPrefix { prefix: b"p" },
        ],
    );
    assert_eq!(applied(&between, Some(&leaf)), Ok(leaf.clone()));
    for cut in 1..=3 {
        let changes: Vec<_> = read(&between).changes().take(cut).collect();
        assert!(applied(&run_of(5, &changes), Some(&leaf)).is_err(), "{cut}");
    }

    // A run that starts from no image needs a page, the page is one, and
    // it is the page the run names.
    let run = run_of(5, &[FlatChange::Remove { key: b"pa" }]);
    assert_eq!(applied(&run, None), Err(wal_error(WalError::NoPage)));
    assert_eq!(
        applied(
            &run_of(6, &[FlatChange::Remove { key: b"pa" }]),
            Some(&leaf)
        ),
        Err(wal_error(WalError::WrongPage {
            found: page(5),
            expected: page(6),
        }))
    );
    assert!(applied(&run, Some(&leaf[..7])).is_err());
    assert!(applied(&run, Some(&[0; 64])).is_err());

    // The changes before a refused one are not kept either: nothing is
    // written.
    let run = run_of(
        5,
        &[
            FlatChange::Remove { key: b"pa" },
            FlatChange::Remove { key: b"pa" },
        ],
    );
    assert_eq!(applied(&run, Some(&leaf)), Err(absent));
}

#[test]
fn each_check_of_a_run_has_its_error() {
    let empty_leaf = EMPTY_LEAF;
    let decode = |bytes: &[u8]| FlatRun::decode(bytes, V1).map(|_| ());
    let record = |record: &[u8]| decode(&[&[5, 0, 0, 0][..], record].concat());

    for len in 0..4 {
        assert_eq!(
            decode(&[5, 0, 0, 0][..len]),
            Err(FormatError::Truncated {
                needed: 4,
                available: len,
            })
        );
    }
    assert_eq!(decode(&[0, 0, 0, 0, 4, 0, 0]), Err(FormatError::MissingId));
    assert_eq!(
        decode(&[5, 0, 0, 0]),
        Err(wal_error(WalError::EmptyFlatRun))
    );
    for code in [0, 8, 0xFF] {
        assert_eq!(
            record(&[code]),
            Err(FormatError::UnknownRecordKind { code: code.into() })
        );
    }

    // Every shorter run is a shorter run or an error.
    let place = ValuePlace::Placed(pointer(3, 20, 9)).encode();
    let whole = run_of(
        5,
        &[
            FlatChange::Image(&empty_leaf),
            FlatChange::Insert {
                key: b"key",
                value: FlatValue::OutOfLine(&place),
            },
            FlatChange::RemoveRange {
                key: b"k",
                count: 2,
            },
            FlatChange::InsertChild {
                key: b"s",
                child: page(3),
            },
            FlatChange::SetPrefix { prefix: b"k" },
        ],
    );
    let mut runs = 0;
    for len in 0..whole.len() {
        match FlatRun::decode(&whole[..len], V1) {
            Ok(run) => {
                runs += 1;
                assert!(run.changes().len() < 5);
            }
            Err(error) => assert!(
                matches!(error, FormatError::Truncated { needed, available }
                    if available == len && needed > len)
                    || error == wal_error(WalError::EmptyFlatRun),
                "{len}: {error:?}"
            ),
        }
    }
    assert_eq!(runs, 4);

    // An image is the first record, is a page, and is as long as it says.
    let image = |bytes: &[u8]| [&[1][..], &(bytes.len() as u16).to_le_bytes(), bytes].concat();
    assert_eq!(record(&image(&empty_leaf)), Ok(()));
    assert_eq!(
        record(&[image(&empty_leaf), image(&empty_leaf)].concat()),
        Err(wal_error(WalError::MisplacedImage))
    );
    assert_eq!(
        record(&[&[4, 1, 0, b'k'][..], &image(&empty_leaf)].concat()),
        Err(wal_error(WalError::MisplacedImage))
    );
    assert_eq!(
        record(&image(&[&empty_leaf[..], &[0]].concat())),
        Err(wal_error(WalError::ImageLength {
            stated: 12,
            found: 13
        }))
    );
    assert!(matches!(
        record(&image(&empty_leaf[..11])),
        Err(FormatError::Truncated { .. })
    ));
    let mut no_kind = empty_leaf;
    no_kind[0] = 3;
    assert!(matches!(
        record(&image(&no_kind)),
        Err(FormatError::UnknownRecordKind { code: 3 })
    ));
    // It is an image of the page the run names.
    let mut of_another = empty_leaf;
    of_another[4] = 6;
    assert_eq!(
        record(&image(&of_another)),
        Err(wal_error(WalError::WrongPage {
            found: page(6),
            expected: page(5),
        }))
    );
    let mut too_long = of_another.to_vec();
    too_long.push(0);
    assert_eq!(
        record(&image(&too_long)),
        Err(wal_error(WalError::ImageLength {
            stated: 12,
            found: 13
        }))
    );

    // The value of an entry.
    let entry = |flags: u8, value: &[u8]| {
        [
            &[2, 1, 0, b'k', flags][..],
            &(value.len() as u16).to_le_bytes(),
            value,
        ]
        .concat()
    };
    assert_eq!(record(&entry(0, b"")), Ok(()));
    assert_eq!(record(&entry(1, &place)), Ok(()));
    assert_eq!(
        record(&entry(1, &ValuePlace::Pending(pending(45, 9)).encode())),
        Ok(())
    );
    for flags in [2, 3, 0x80] {
        assert_eq!(
            record(&entry(flags, b"")),
            Err(FormatError::NonZeroReserved)
        );
    }
    for len in [0, 15, 17] {
        assert_eq!(
            record(&entry(1, &[9; 17][..len])),
            Err(wal_error(WalError::PlaceLength { len }))
        );
    }
    assert_eq!(record(&entry(1, &[0; 16])), Err(FormatError::MissingId));
    let mut empty = place;
    empty[12..].fill(0);
    assert_eq!(
        record(&entry(1, &empty)),
        Err(FormatError::ValueLog(ValueLogError::EmptyValue))
    );

    for count in [0, 1] {
        assert_eq!(
            record(&[4 + 1, 1, 0, b'k', count, 0]),
            Err(wal_error(WalError::InvalidRange))
        );
    }
    assert_eq!(
        record(&[6, 1, 0, b's', 0, 0, 0, 0]),
        Err(FormatError::MissingId)
    );
}

#[test]
fn a_change_that_would_not_read_back_is_not_written() {
    let mut bytes = b"before".to_vec();
    let mut run = FlatRunWriter::new(page(5), &mut bytes);
    assert_eq!(run.encoded_len(), 4);
    let long = vec![0; 65_536];
    let too_long = wal_error(WalError::FlatFieldTooLong { len: 65_536 });
    let place = ValuePlace::Placed(pointer(3, 20, 9)).encode();
    let mut long_image = EMPTY_LEAF.to_vec();
    long_image.push(0);
    let mut of_another = EMPTY_LEAF;
    of_another[4] = 6;
    let refused: [(FlatChange<'_>, FormatError); 10] = [
        (FlatChange::Remove { key: &long }, too_long),
        (FlatChange::SetPrefix { prefix: &long }, too_long),
        (
            FlatChange::InsertChild {
                key: &long,
                child: page(1),
            },
            too_long,
        ),
        (
            FlatChange::RemoveRange {
                key: &long,
                count: 2,
            },
            too_long,
        ),
        (
            FlatChange::Insert {
                key: b"k",
                value: FlatValue::Inline(&long),
            },
            too_long,
        ),
        (
            FlatChange::Replace {
                key: &long,
                value: FlatValue::Inline(b""),
            },
            too_long,
        ),
        (
            FlatChange::Insert {
                key: b"k",
                value: FlatValue::OutOfLine(&place[..15]),
            },
            wal_error(WalError::PlaceLength { len: 15 }),
        ),
        (
            FlatChange::RemoveRange {
                key: b"k",
                count: 1,
            },
            wal_error(WalError::InvalidRange),
        ),
        (
            FlatChange::Image(&long_image),
            wal_error(WalError::ImageLength {
                stated: 12,
                found: 13,
            }),
        ),
        (
            FlatChange::Image(&of_another),
            wal_error(WalError::WrongPage {
                found: page(6),
                expected: page(5),
            }),
        ),
    ];
    for (change, error) in refused {
        assert_eq!(run.push(&change), Err(error), "{change:?}");
        assert_eq!(run.encoded_len(), 4);
    }
    let mut no_page = EMPTY_LEAF;
    no_page[0] = 2;
    assert!(run.push(&FlatChange::Image(&no_page)).is_err());
    assert_eq!(run.encoded_len(), 4);
    assert_eq!(run.finish(), Err(wal_error(WalError::EmptyFlatRun)));
    assert_eq!(bytes, b"before");

    // A key of the longest length is written, and an image only first.
    let mut run = FlatRunWriter::new(page(5), &mut bytes);
    run.push(&FlatChange::Remove {
        key: &long[..65_535],
    })
    .unwrap();
    assert_eq!(
        run.push(&FlatChange::Image(&EMPTY_LEAF)),
        Err(wal_error(WalError::MisplacedImage))
    );
    assert_eq!(run.finish(), Ok(4 + 3 + 65_535));
    assert!(FlatRun::decode(&bytes[6..], V1).is_ok());
}
