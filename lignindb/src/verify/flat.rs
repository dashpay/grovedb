//! The walk of the flat keyspaces, and the directory's own check.

use std::{collections::HashMap, rc::Rc};

use lignin_format::{
    flat_page::{FlatPage, FlatValue},
    value_log::ValuePlace,
    CommitVersion, PageId,
};
use lignin_store::load::FRAME_FORMAT;
use lignin_tree::{
    directory::{record::PREFIX_LEN, Directory, Prefix},
    flat::{Committed, FlatTree},
    OperationGuard,
};

use super::{
    grove::{Entries, Merk},
    pages::{FlatReach, Kind, Seen},
    read::ValueFault,
    Grove, Place, Rule, Run,
};

/// What the records of a keyspace are checked against.
#[derive(Clone, Copy)]
enum Records<'a> {
    /// Nothing: the keyspace holds no record of a subtree.
    Any,
    /// The records are of subtrees: each stands under one of `leads`,
    /// then a prefix that the directory has an entry for. The leads are
    /// those of every column that is kept in the keyspace.
    Columns {
        leads: &'a [&'a [u8]],
        prefixes: &'a HashMap<Prefix, usize>,
    },
}

/// How a walk came to its pages.
#[derive(Clone, Copy, PartialEq, Eq)]
enum From {
    /// From the root of a keyspace.
    Tree,
    /// From the record of a lazy range delete. A page that is gone, or
    /// that was born after this version, was freed and is passed.
    Left { newest_birth: CommitVersion },
}

/// A page that a walk has still to enter.
struct Visit {
    page: PageId,
    /// The page that names this one, or `None` for a root.
    parent: Option<PageId>,
    /// The level of the branch that names the page.
    above: Option<u8>,
    /// The keys of the page are at or after `low` and before `high`.
    low: Option<Rc<[u8]>>,
    high: Option<Rc<[u8]>>,
    /// What stands before the keys of this tree in the keys of its
    /// keyspace: the heads of the buckets of long keys it is in.
    outer: Rc<[u8]>,
}

impl Run<'_, '_, '_> {
    /// Walks every flat keyspace of the store, checks the directory
    /// against itself, and returns its entries.
    pub(super) fn walk_flat_trees<G: OperationGuard + ?Sized>(
        &mut self,
        guard: &G,
    ) -> Option<Entries> {
        let store = self.store;
        let mut entries = None;
        if let Grove::Directory { tree, columns } = store.grove {
            if self.walk_flat(&tree, Records::Any) {
                entries = self.read_directory(&tree, guard);
            } else {
                let what = || "its keyspace is no tree, so its records are not read".to_owned();
                self.add(Rule::Directory, Place::Page(tree.root()), what);
            }
            let none = HashMap::new();
            let prefixes = (entries.as_ref()).map_or(&none, |entries| &entries.by_prefix);
            // Several columns may share one keyspace, each under a lead of
            // its own (`lignin_tree::drop::Column`): a keyspace is walked
            // once, with the leads of all its columns.
            let mut walked: Vec<PageId> = Vec::new();
            for column in columns {
                let root = column.tree.root();
                if walked.contains(&root) {
                    continue;
                }
                walked.push(root);
                let shares = |other: &&lignin_tree::drop::Column<'_>| other.tree.root() == root;
                let leads: Vec<&[u8]> = (columns.iter().filter(shares))
                    .map(|column| column.lead)
                    .collect();
                let records = match &entries {
                    Some(_) => Records::Columns {
                        leads: &leads,
                        prefixes,
                    },
                    None => Records::Any,
                };
                self.walk_flat(&column.tree, records);
            }
        }
        if let Some(drops) = store.flat_drops
            && self.walk_flat(drops.tree(), Records::Any)
        {
            self.walk_left_pages(&drops, guard);
        }
        for tree in store.flat {
            self.walk_flat(tree, Records::Any);
        }
        let unreached: Vec<PageId> = (self.pages.iter())
            .filter(|(_, seen)| matches!(seen, Seen::Flat(None)))
            .map(|(page, _)| *page)
            .collect();
        for page in unreached {
            let what = || "no keyspace reaches the page, and no range delete left it".to_owned();
            self.add(Rule::FlatPageUnreached, Place::Page(page), what);
        }
        entries
    }

    /// Reads the directory in `tree`, whose pages are a tree: its own
    /// check, and every entry.
    fn read_directory<G: OperationGuard + ?Sized>(
        &mut self,
        tree: &FlatTree,
        guard: &G,
    ) -> Option<Entries> {
        let store = self.store;
        let place = Place::Page(tree.root());
        let pages = Committed::new(store.loader, store.values, guard, self.at);
        let directory = match Directory::load(&pages, tree.root(), tree.config()) {
            Ok(directory) => directory,
            Err(error) => {
                self.add(Rule::Directory, place, || error.to_string());
                return None;
            }
        };
        match directory.check(&pages) {
            Ok(census) => {
                self.census.merks = census.live;
                self.census.left_over_entries = census.left_over;
                self.census.dropped_merks = census.dropped;
            }
            Err(error) => self.add(Rule::Directory, place, || error.to_string()),
        }
        let mut entries = Entries::default();
        let mut walk = directory.entries();
        loop {
            match walk.next(&pages) {
                Ok(Some((prefix, entry))) => {
                    entries.by_prefix.insert(prefix, entries.merks.len());
                    entries.merks.push(Merk {
                        prefix,
                        live: directory.is_live(&entry),
                        root: entry.root().map(|root| (root.address, root.key.to_vec())),
                        host: (entry.host())
                            .map(|host| (*host.parent, host.key.to_vec(), host.position)),
                    });
                }
                Ok(None) => break,
                Err(error) => {
                    self.add(Rule::Directory, place, || error.to_string());
                    break;
                }
            }
        }
        let mut walk = directory.drops();
        loop {
            match walk.next(&pages) {
                Ok(Some(dropped)) => entries.drops.push(dropped),
                Ok(None) => break,
                Err(error) => {
                    self.add(Rule::Directory, place, || error.to_string());
                    break;
                }
            }
        }
        Some(entries)
    }

    /// Walks the pages that the lazy range deletes left and that are not
    /// freed yet.
    fn walk_left_pages<G: OperationGuard + ?Sized>(
        &mut self,
        drops: &lignin_tree::drop::FlatDrops,
        guard: &G,
    ) {
        let store = self.store;
        let pages = Committed::new(store.loader, store.values, guard, self.at);
        let records = match drops.pending(&pages) {
            Ok(records) => records,
            Err(error) => {
                let place = Place::Page(drops.tree().root());
                self.add(Rule::FlatDropRecord, place, || error.to_string());
                return;
            }
        };
        for record in records {
            let from = From::Left {
                newest_birth: record.at.next().unwrap_or(record.at),
            };
            for left in &record.pages {
                let visit = Visit {
                    page: left.page,
                    parent: None,
                    above: left.above,
                    low: None,
                    high: None,
                    outer: Rc::from(&[][..]),
                };
                self.walk_flat_from(visit, drops.tree(), Records::Any, from);
            }
        }
    }

    /// Walks the keyspace `tree` from its root, and returns whether
    /// nothing was found wrong with its pages.
    fn walk_flat(&mut self, tree: &FlatTree, records: Records<'_>) -> bool {
        let before = self.found.list.len() as u64 + self.found.dropped;
        let visit = Visit {
            page: tree.root(),
            parent: None,
            above: None,
            low: None,
            high: None,
            outer: Rc::from(&[][..]),
        };
        self.walk_flat_from(visit, tree, records, From::Tree);
        before == self.found.list.len() as u64 + self.found.dropped
    }

    fn walk_flat_from(&mut self, first: Visit, tree: &FlatTree, records: Records<'_>, from: From) {
        let key_limit = tree.config().key_limit();
        let mut stack = vec![first];
        // The prefix of the record before, which the directory has.
        let mut entered: Option<Prefix> = None;
        while let Some(visit) = stack.pop() {
            self.begin_step();
            let page = visit.page;
            let named_by = visit.parent.map_or(Place::Page(page), Place::Page);
            let left = matches!(from, From::Left { .. });
            let kind = match self.see(page) {
                Kind::None if left => continue,
                Kind::Bad => continue,
                kind => kind,
            };
            // A page that a range delete left and that was born after the
            // range delete can have committed is another page: the one
            // that was left was freed, and its id was given out again, to a
            // page of either kind. The reclaimer passes it
            // (`lignin_tree::drop`), and so does this walk, before it asks
            // what the page is.
            let frame = self.frame(page).ok();
            if let (From::Left { newest_birth }, Some(frame)) = (from, &frame)
                && frame.born() > newest_birth
            {
                continue;
            }
            if kind != Kind::Flat {
                let rule = match from {
                    From::Tree => Rule::FlatDanglingPage,
                    From::Left { .. } => Rule::FlatDropRecord,
                };
                self.add(rule, named_by, || match kind {
                    Kind::Tree => format!("page {page} is named as a flat page and is a tree page"),
                    _ => format!("page {page} is named as a flat page and is no page"),
                });
                continue;
            }
            let Some(frame) = frame else {
                continue;
            };
            let reach = match from {
                From::Tree => FlatReach::Tree,
                From::Left { .. } => FlatReach::Left,
            };
            match self.pages.get_mut(&page) {
                Some(Seen::Flat(reached @ None)) => *reached = Some(reach),
                Some(Seen::Flat(Some(before))) => {
                    let before = *before;
                    self.add(Rule::FlatPageNamedTwice, named_by, || {
                        let other = match before {
                            FlatReach::Tree => "a keyspace reaches",
                            FlatReach::Left => "a range delete left",
                        };
                        format!("page {page} is named again, which {other} already")
                    });
                    continue;
                }
                _ => continue,
            }
            if left {
                self.census.left_flat_pages += 1;
            }
            let Ok(flat) = FlatPage::from_validated(&frame, FRAME_FORMAT) else {
                continue;
            };
            if visit
                .above
                .is_some_and(|above| above.checked_sub(1) != Some(flat.level()))
            {
                let (above, level) = (visit.above.unwrap_or_default(), flat.level());
                self.add(Rule::FlatLevel, Place::Page(page), || {
                    format!("the page is of level {level} below a branch of level {above}")
                });
                continue;
            }
            match flat {
                FlatPage::Branch(branch) => {
                    let mut previous: Option<Rc<[u8]>> = None;
                    for index in 0..=branch.len() {
                        let separator = branch.separator(index).map(|key| Rc::from(key.to_vec()));
                        if let (Some(previous), Some(separator)) = (&previous, &separator)
                            && separator <= previous
                        {
                            self.add(Rule::FlatKeyOrder, Place::Page(page), || {
                                format!("separator {index} is not above the one before it")
                            });
                        }
                        let Some(child) = branch.child(index) else {
                            self.add(Rule::FlatPageFormat, Place::Page(page), || {
                                format!("the branch has no child {index}")
                            });
                            break;
                        };
                        stack.push(Visit {
                            page: child,
                            parent: Some(page),
                            above: Some(branch.level()),
                            low: higher(previous.clone(), visit.low.clone()),
                            high: lower(separator.clone(), visit.high.clone()),
                            outer: visit.outer.clone(),
                        });
                        previous = separator.or(previous);
                    }
                }
                FlatPage::Leaf(leaf) => {
                    let mut before: Option<Vec<u8>> = None;
                    for (slot, (key, value)) in leaf.iter().enumerate() {
                        let key = key.to_vec();
                        let place = Place::FlatEntry { page, slot };
                        let in_order = before.as_ref().is_none_or(|before| *before < key)
                            && visit.low.as_ref().is_none_or(|low| low[..] <= key[..])
                            && visit.high.as_ref().is_none_or(|high| key[..] < high[..]);
                        if !in_order {
                            self.add(Rule::FlatKeyOrder, place, || {
                                "the key is not in its place in the order of its tree".to_owned()
                            });
                        }
                        if key.len() > key_limit {
                            let root = bucket_root(&key, key_limit, value);
                            let Some(root) = root else {
                                self.add(Rule::FlatBucket, place, || {
                                    format!(
                                        "a key of {} bytes, above the limit of {key_limit}, is \
                                         no bucket",
                                        key.len()
                                    )
                                });
                                before = Some(key);
                                continue;
                            };
                            let outer = [&visit.outer[..], &key[..key_limit]].concat();
                            stack.push(Visit {
                                page: root,
                                parent: Some(page),
                                above: None,
                                low: None,
                                high: None,
                                outer: Rc::from(outer),
                            });
                            before = Some(key);
                            continue;
                        }
                        if !left {
                            self.census.flat_records += 1;
                            if let Records::Columns { leads, prefixes } = records {
                                let full = [&visit.outer[..], &key[..]].concat();
                                let prefix = record_prefix(&full, leads, prefixes);
                                let known = match prefix {
                                    Some(prefix) if entered == Some(prefix) => true,
                                    Some(prefix) => prefixes.contains_key(&prefix),
                                    None => false,
                                };
                                if known {
                                    entered = prefix;
                                } else {
                                    self.add(
                                        Rule::FlatRecordWithoutEntry,
                                        place,
                                        || match prefix {
                                            Some(prefix) => format!(
                                                "the record is under the prefix {}, which the \
                                             directory has no entry for",
                                                Place::Merk(prefix)
                                            ),
                                            None => "the record's key holds no prefix".to_owned(),
                                        },
                                    );
                                }
                            }
                            self.check_flat_value(value, page, place);
                        }
                        before = Some(key);
                    }
                }
            }
        }
    }

    /// Reads the value of a flat record that is out of line, for the page
    /// that holds its place.
    fn check_flat_value(&mut self, value: FlatValue<'_>, page: PageId, place: Place) {
        let FlatValue::OutOfLine(bytes) = value else {
            return;
        };
        let Some(value) = (bytes.try_into().ok()).and_then(|bytes| ValuePlace::decode(bytes).ok())
        else {
            return;
        };
        if let Err(fault) = self.values.fetch(value, page) {
            let rule = match fault {
                ValueFault::Owner(_) => Rule::ValueOwner,
                // The census of the pages reported it.
                ValueFault::NoBytes { .. } => return,
                ValueFault::Unreadable(_) | ValueFault::Page(_) => Rule::ValueUnreadable,
            };
            self.add(rule, place, || fault.to_string());
        }
    }
}

/// The lower of two upper bounds.
fn lower(a: Option<Rc<[u8]>>, b: Option<Rc<[u8]>>) -> Option<Rc<[u8]>> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// The higher of two lower bounds.
fn higher(a: Option<Rc<[u8]>>, b: Option<Rc<[u8]>>) -> Option<Rc<[u8]>> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// The root of the tree of the bucket of long keys stored under `key`, if
/// the entry is one: the key is the head and one zero byte, and the value
/// is a page id (`lignin_tree::flat`).
fn bucket_root(key: &[u8], key_limit: usize, value: FlatValue<'_>) -> Option<PageId> {
    if key.len() != key_limit + 1 || key.last() != Some(&0) {
        return None;
    }
    match value {
        FlatValue::Inline(bytes) => bytes.try_into().ok().and_then(PageId::decode),
        FlatValue::OutOfLine(_) => None,
    }
}

/// The prefix of the subtree that the record under `key` is of, in a
/// keyspace whose columns begin their keys with `leads`: the one that
/// `prefixes` has, where one lead is the beginning of another.
fn record_prefix(key: &[u8], leads: &[&[u8]], prefixes: &HashMap<Prefix, usize>) -> Option<Prefix> {
    let under = |lead: &&[u8]| {
        let rest = key.strip_prefix(*lead)?;
        rest.first_chunk::<PREFIX_LEN>().copied()
    };
    let mut found = leads.iter().filter_map(under);
    let first = found.next()?;
    match prefixes.contains_key(&first) {
        true => Some(first),
        false => found
            .find(|prefix| prefixes.contains_key(prefix))
            .or(Some(first)),
    }
}
