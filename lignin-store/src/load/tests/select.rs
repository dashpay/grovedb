//! Which links of a chain are the page version a reader is given
//! (`page-table.md` T5).

use super::*;

fn at(offset: u64) -> BlockLocation {
    BlockLocation {
        file: FileId::MIN,
        offset,
        len: 64,
    }
}

fn links_of(table: &PageTable, of: PageId) -> Vec<Link> {
    let mut links = Vec::new();
    table.chain(of, &mut links);
    links
}

#[test]
fn a_reader_is_given_what_the_page_table_gives_it() {
    let mut rng = rng(1);
    let (mut pages, mut none, mut freed_pages) = (0, 0, 0);
    for round in 0..3000 {
        let table = PageTable::new();
        let of = page(1 + round % 7);
        let (mut newest, mut offset) = (0, 20);
        let mut pushed = Vec::new();
        for _ in 0..rng.below(14) {
            newest += rng.below(3);
            offset += 64;
            let born = version(newest.max(1));
            let done = match rng.below(8) {
                0 | 1 => table.push(of, Link::base(born, at(offset))).is_ok(),
                2..=4 => table.push(of, Link::delta(born, at(offset))).is_ok(),
                5 => table.push(of, Link::free(born)).is_ok() && false,
                6 if !pushed.is_empty() => {
                    let through = pushed[rng.below(pushed.len() as u64) as usize];
                    table.consolidate(of, through, at(offset)).is_ok()
                }
                _ => {
                    table.trim(of, version(1 + rng.below(newest + 1)), &mut Vec::new());
                    false
                }
            };
            if done {
                pushed.push(at(offset));
            }
        }

        let links = links_of(&table, of);
        for asked in 1..=newest + 2 {
            let asked = version(asked);
            let mut blocks = vec![at(8)];
            let born = table.read(of, asked, &mut blocks);
            match Selected::of(of, asked, &links) {
                Ok(selected) => {
                    pages += 1;
                    assert_eq!(born, Some(selected.born));
                    let mut given = vec![selected.base];
                    given.extend(
                        links[selected.runs.clone()]
                            .iter()
                            .filter_map(Link::location),
                    );
                    assert_eq!(given, blocks);
                    let end = selected.runs.end;
                    assert!(links[..end].iter().all(|link| link.version <= asked));
                    assert!(links[end..].iter().all(|link| link.version > asked));
                    assert_eq!(selected.next, links.get(end).map(|link| link.version));
                    assert_eq!(links[end - 1].version, selected.born);
                }
                Err(LoadError::NoPage {
                    page: found,
                    at,
                    freed,
                }) => {
                    assert_eq!((found, at, born), (of, asked, None));
                    let newest = links.iter().rfind(|link| link.version <= asked);
                    match (freed, newest) {
                        (None, None) => none += 1,
                        (Some(freed), Some(link)) => {
                            freed_pages += 1;
                            assert_eq!((link.kind, link.version), (LinkKind::Free, freed));
                        }
                        other => panic!("{other:?} of {links:?} at {asked}"),
                    }
                }
                Err(other) => panic!("{other:?} of {links:?} at {asked}"),
            }
        }
    }
    assert!(pages > 3000 && none > 1000 && freed_pages > 300);
}

#[test]
fn among_links_of_one_version_the_later_one_is_the_newer() {
    let links = [
        Link::base(version(1), at(20)),
        Link::delta(version(2), at(84)),
        Link::base(version(2), at(148)),
        Link::delta(version(2), at(212)),
        Link::delta(version(4), at(276)),
        Link::free(version(6)),
        Link::base(version(6), at(340)),
    ];
    let given = |asked| Selected::of(page(3), asked, &links).expect("a page version");
    let selected = |asked| given(version(asked));
    assert_eq!(
        selected(1),
        Selected {
            base: at(20),
            runs: 1..1,
            born: version(1),
            next: Some(version(2)),
        }
    );
    for asked in [2, 3] {
        assert_eq!(
            selected(asked),
            Selected {
                base: at(148),
                runs: 3..4,
                born: version(2),
                next: Some(version(4)),
            }
        );
    }
    assert_eq!(
        selected(5),
        Selected {
            base: at(148),
            runs: 3..5,
            born: version(4),
            next: Some(version(6)),
        }
    );
    assert_eq!(
        given(CommitVersion::MAX),
        Selected {
            base: at(340),
            runs: 7..7,
            born: version(6),
            next: None,
        }
    );
}

#[test]
fn runs_with_no_base_image_beneath_them_are_no_page_version() {
    let alone = [Link::delta(version(2), at(20))];
    let after_free = [
        Link::base(version(1), at(20)),
        Link::free(version(2)),
        Link::delta(version(3), at(84)),
        Link::delta(version(4), at(148)),
    ];
    for (links, asked, born) in [
        (&alone[..], 2, 2),
        (&after_free[..], 3, 3),
        (&after_free[..], 9, 4),
    ] {
        match Selected::of(page(5), version(asked), links) {
            Err(LoadError::NoBase {
                page: of,
                born: found,
            }) => {
                assert_eq!((of, found), (page(5), version(born)));
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(matches!(
        Selected::of(page(5), version(1), &alone),
        Err(LoadError::NoPage { freed: None, .. })
    ));
    assert!(Selected::of(page(5), version(1), &after_free).is_ok());
}
