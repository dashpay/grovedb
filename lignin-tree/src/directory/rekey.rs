//! Moving a Merk to another prefix, with every Merk below it.
//!
//! [`Directory::rekey`] moves one Merk to any host, and is told the
//! prefixes below it by a function of the steps down. A write transaction
//! moves by pairs ([`Directory::plan_rekey`], [`Directory::rekey_pairs`]):
//! its caller has a prefix for each Merk and no path, and the moves of one
//! write are made together.

use std::collections::HashSet;

use super::{
    error::DirectoryError,
    read::damaged,
    read::Children,
    record::{child_key, decode_child, entry_key, Dropped, Entry, Fields, Host, MerkId, Prefix},
    write::Pending,
    Directory,
};
use crate::flat::{FlatPages, FlatPagesMut};

/// One step down from a moved Merk towards a Merk below it: the key of the
/// element that hosts the next Merk, and the position of that Merk among
/// the element's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step<'a> {
    /// The host's key.
    pub key: &'a [u8],
    /// The position among the host's Merks.
    pub position: u8,
}

/// A Merk whose children are being moved.
struct Level {
    children: Children,
    id: MerkId,
    /// The prefix the Merk has now.
    prefix: Prefix,
}

impl Directory {
    /// Moves the Merk under `from` to the prefix `to`, where `host` hosts
    /// it, and every Merk below it to the prefix `derive` gives: the
    /// re-keying of a move that keeps the cells (`tree-layout.md` D5).
    ///
    /// `derive` is given the steps from the moved Merk down to a Merk
    /// below it, and returns the prefix that Merk has under the new path.
    /// The directory knows no path and no hash, so it cannot make one.
    ///
    /// Every Merk keeps its id, its root and its tree type, and no cell is
    /// read. One entry is written for each Merk, with the records that
    /// lead to it. Returns how many Merks were moved.
    ///
    /// The move is refused, and nothing is written, if `to` has a live
    /// Merk, if `host` is not in a live Merk, has a Merk at the position,
    /// the moved one or another, or is below the moved Merk.
    ///
    /// A Merk below the moved one is refused only when the walk comes to
    /// it: with [`DirectoryError::Exists`] if `derive` gives a prefix that
    /// has a live Merk, and with [`DirectoryError::TooDeep`] if the move
    /// takes it more than 65,535 Merks deep. The move is then half made,
    /// and the writer's pages are given up, as after any other error.
    pub fn rekey<P, F>(
        &self,
        pages: &mut P,
        from: &Prefix,
        to: &Prefix,
        host: Host<'_>,
        mut derive: F,
    ) -> Result<u64, DirectoryError>
    where
        P: FlatPagesMut + ?Sized,
        F: FnMut(&[Step<'_>]) -> Prefix,
    {
        // The records of each Merk are written when they are made: the
        // walk below reads the pages as the Merks before it left them.
        let mut pending = Pending::new();
        let entry = self.live(pages, from)?;
        let left = self.free(&pending, pages, to)?;
        let place = self.below(&pending, pages, &host, Some((from, entry.id())))?;
        self.host_is_free(&pending, pages, &host, &place)?;
        let fields = Fields {
            host: Some(host),
            lineage: &place.lineage,
            ..entry.fields()
        };
        let moved = fields.encode()?;

        if let Some(was) = entry.child_key() {
            self.tree.delete(pages, &was)?;
        }
        self.tree.delete(pages, &entry_key(from))?;
        self.displace(&mut pending, pages, to, left.as_ref())?;
        Self::enter(&mut pending, to, &moved, Some(&place.child));
        self.settle(pages, pending)?;

        let mut count = 1;
        let mut lineage = place.lineage;
        lineage.extend_from_slice(&entry.id().encode());
        let mut keys: Vec<(Vec<u8>, u8)> = Vec::new();
        let mut levels = vec![Level {
            children: self.children(entry.id()),
            id: entry.id(),
            prefix: *to,
        }];
        while let Some(level) = levels.last_mut() {
            let Some(child) = level.children.next(pages)? else {
                levels.pop();
                keys.pop();
                lineage.truncate(lineage.len().saturating_sub(MerkId::ENCODED_LEN));
                continue;
            };
            let (parent, parent_prefix) = (level.id, level.prefix);
            keys.push((child.key.clone(), child.position));
            let steps: Vec<Step<'_>> = keys
                .iter()
                .map(|(key, position)| Step {
                    key,
                    position: *position,
                })
                .collect();
            let prefix = derive(&steps);
            let (key, position) = (child.key.as_slice(), child.position);

            let old = entry_key(&child.prefix);
            let nested = self.stored(pages, &child.prefix)?;
            let nested = nested.filter(|nested| nested.id() == child.id);
            let what = "a Merk in use hosts a Merk that has no entry";
            let nested = nested.ok_or_else(|| damaged(&old, what))?;
            self.tree.delete(pages, &old)?;
            let mut pending = Pending::new();
            let left = self.free(&pending, pages, &prefix)?;
            let host = Host {
                parent: &parent_prefix,
                key,
                position,
            };
            let fields = Fields {
                host: Some(host),
                lineage: &lineage,
                ..nested.fields()
            };
            let moved = fields.encode()?;
            self.displace(&mut pending, pages, &prefix, left.as_ref())?;
            let record = child_key(parent, position, key);
            Self::enter(&mut pending, &prefix, &moved, Some(&record));
            self.settle(pages, pending)?;

            count += 1;
            lineage.extend_from_slice(&child.id.encode());
            levels.push(Level {
                children: self.children(child.id),
                id: child.id,
                prefix,
            });
        }
        Ok(count)
    }
}

/// One Merk that a re-keying by pairs moves, with the Merks below it
/// ([`Directory::plan_rekey`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rekey<'a> {
    /// The prefix the Merk is entered under.
    pub from: &'a Prefix,
    /// The prefix it goes to.
    pub to: &'a Prefix,
    /// The key of the element that hosts it afterwards, in the Merk its
    /// host is in now. Its position among the host's Merks stays.
    pub key: &'a [u8],
    /// The prefix that each Merk below it goes to, by the prefix it has.
    pub below: &'a [(Prefix, Prefix)],
}

/// One Merk of a plan: where it is, where it goes, and its records there.
#[derive(Debug, Clone)]
struct Planned {
    from: Prefix,
    to: Prefix,
    was: Entry,
    now: Entry,
    /// The key of the record of its host afterwards.
    child: Vec<u8>,
}

/// A re-keying by pairs that was checked against a directory, with nothing
/// changed ([`Directory::plan_rekey`]).
#[derive(Debug, Clone, Default)]
pub struct RekeyPlan {
    moved: Vec<Planned>,
    left: Vec<Prefix>,
}

impl RekeyPlan {
    /// Each Merk that gets another prefix: the prefix it has, the one it
    /// gets, and its entry as it is before. A Merk comes before those
    /// below it.
    pub fn moved(&self) -> impl Iterator<Item = (&Prefix, &Prefix, &Entry)> {
        (self.moved.iter()).map(|planned| (&planned.from, &planned.to, &planned.was))
    }

    /// The Merks below a moved one that leave the directory with the
    /// move, by the prefix each has: a Merk comes before those below it.
    pub fn left(&self) -> &[Prefix] {
        &self.left
    }

    /// Whether the plan moves no Merk.
    pub fn is_empty(&self) -> bool {
        self.moved.is_empty()
    }
}

/// What a re-keying by pairs did ([`Directory::rekey_pairs`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rekeyed {
    /// What each Merk that left with it left for the reclaimer, the
    /// innermost first.
    pub left: Vec<Dropped>,
    /// The prefixes that a Merk went to and under which a dropped Merk
    /// had left an entry: what else is kept by that prefix is of the
    /// entry that was displaced (`tree-layout.md` D6).
    pub displaced: Vec<Prefix>,
}

/// A Merk whose children a plan is walking.
struct Walked {
    children: Children,
    id: MerkId,
    /// The prefix the Merk has.
    prefix: Prefix,
    /// The prefix it goes to, or `None` for a Merk that leaves.
    to: Option<Prefix>,
}

impl Directory {
    /// Checks the moves of one write against the directory, and works out
    /// every record they change: the re-keying of a move that keeps the
    /// cells (`tree-layout.md` D5), for a caller that has a prefix for
    /// each Merk and no path. Nothing is written.
    ///
    /// Each move gives a Merk another prefix under another key of the
    /// Merk its host is in. Each Merk below it, at any depth:
    ///
    /// - goes to the prefix that the move names for it. The key and the
    ///   position of its host stay, and its host's Merk is the Merk above
    ///   it under that one's new prefix;
    /// - or, if the move names none for it and it has no node, leaves the
    ///   directory with the move, as [`Self::unlink`] takes it out, whether
    ///   the Merk above it goes or leaves;
    /// - or refuses the move ([`DirectoryError::Unnamed`]), in two cases:
    ///   the move names no prefix for it and it has a node, or the move
    ///   names a prefix for it and the Merk above it leaves.
    ///
    /// A pair of `below` whose first prefix is not that of a Merk below
    /// the moved one gives no Merk another prefix. Every Merk keeps its
    /// id, its root, its tree type and the Merks it is below, and no cell
    /// is read.
    ///
    /// The moves are made together: a prefix that one Merk leaves is free
    /// for another, so two Merks can change places.
    ///
    /// `leaves` says of an entry in use whether its Merk is out of the
    /// directory before the plan is applied: the same write drops it, or
    /// drops or clears a Merk it is below. Such a Merk stands in no
    /// other's way.
    ///
    /// The plan is refused if a moved Merk is not in use, leaves before,
    /// or has no host ([`DirectoryError::NoHost`]); if the moves name a
    /// prefix twice as one a Merk is taken from, or twice as one a Merk
    /// goes to, whether a Merk is there or not
    /// ([`DirectoryError::Twice`]); if a prefix that a Merk goes to has a
    /// Merk in use that neither leaves before nor is moved away
    /// ([`DirectoryError::Exists`]); or if the new host has a Merk at the
    /// position already, which neither leaves before nor is moved away,
    /// or two Merks of the plan go to one position of one host
    /// ([`DirectoryError::HostTaken`]).
    pub fn plan_rekey<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
        moves: &[Rekey<'_>],
        leaves: &dyn Fn(&Entry) -> bool,
    ) -> Result<RekeyPlan, DirectoryError> {
        // No prefix is named twice on one side, by all the moves together.
        let (mut froms, mut tos) = (HashSet::new(), HashSet::new());
        for rekey in moves {
            let below = rekey.below.iter().map(|(from, to)| (from, to));
            for (from, to) in std::iter::once((rekey.from, rekey.to)).chain(below) {
                if !froms.insert(from) {
                    return Err(DirectoryError::Twice { prefix: *from });
                }
                if !tos.insert(to) {
                    return Err(DirectoryError::Twice { prefix: *to });
                }
            }
        }
        let mut plan = RekeyPlan::default();
        for rekey in moves {
            let entry = self.live(pages, rekey.from)?;
            if leaves(&entry) {
                return Err(DirectoryError::NotFound {
                    prefix: *rekey.from,
                });
            }
            let hosted = entry.host().zip(entry.parent_id());
            let (host, parent) = hosted.ok_or(DirectoryError::NoHost {
                prefix: *rekey.from,
            })?;
            let fields = Fields {
                host: Some(Host {
                    key: rekey.key,
                    ..host
                }),
                ..entry.fields()
            };
            plan.moved.push(Planned {
                from: *rekey.from,
                to: *rekey.to,
                now: fields.encode()?,
                child: child_key(parent, host.position, rekey.key),
                was: entry.clone(),
            });
            // The Merks below a moved one form a tree: each has one host,
            // so the walk reaches each once. A directory made by hand can
            // give a Merk a child record that leads back to it or to one
            // already walked, which `Directory::check` refuses ("a Merk in
            // use hosts a Merk that has no entry" and the host rules); the
            // walk would otherwise not end. A Merk reached a second time is
            // refused here, so the walk is at most as long as the directory
            // has Merks.
            let mut walked_ids = HashSet::new();
            let twice = "a Merk is reached twice walking the Merks a move covers";
            if !walked_ids.insert(entry.id()) {
                return Err(damaged(&entry_key(rekey.from), twice));
            }
            let mut walked = vec![Walked {
                children: self.children(entry.id()),
                id: entry.id(),
                prefix: *rekey.from,
                to: Some(*rekey.to),
            }];
            while let Some(above) = walked.last_mut() {
                let Some(child) = above.children.next(pages)? else {
                    walked.pop();
                    continue;
                };
                let (parent, parent_prefix, parent_to) = (above.id, above.prefix, above.to);
                let old = entry_key(&child.prefix);
                let nested = self.stored(pages, &child.prefix)?;
                let nested = nested.filter(|nested| nested.id() == child.id);
                let what = "a Merk in use hosts a Merk that has no entry";
                let nested = nested.ok_or_else(|| damaged(&old, what))?;
                if !walked_ids.insert(nested.id()) {
                    return Err(damaged(&old, twice));
                }
                let named = (rekey.below.iter()).find(|(from, _)| *from == child.prefix);
                let to = match (parent_to, named) {
                    (Some(parent_to), Some((_, to))) => {
                        let host = Host {
                            parent: &parent_to,
                            key: &child.key,
                            position: child.position,
                        };
                        let fields = Fields {
                            host: Some(host),
                            ..nested.fields()
                        };
                        plan.moved.push(Planned {
                            from: child.prefix,
                            to: *to,
                            now: fields.encode()?,
                            child: child_key(parent, child.position, &child.key),
                            was: nested,
                        });
                        Some(*to)
                    }
                    // The Merk above leaves, and this one would stay.
                    (None, Some(_)) => {
                        return Err(DirectoryError::Unnamed {
                            prefix: parent_prefix,
                        })
                    }
                    (_, None) if nested.root().is_some() => {
                        return Err(DirectoryError::Unnamed {
                            prefix: child.prefix,
                        })
                    }
                    (_, None) => {
                        plan.left.push(child.prefix);
                        None
                    }
                };
                walked.push(Walked {
                    children: self.children(child.id),
                    id: child.id,
                    prefix: child.prefix,
                    to,
                });
            }
        }

        let mut taken = HashSet::new();
        let sources = plan.moved.iter().map(|planned| &planned.from);
        for prefix in sources.chain(&plan.left) {
            if !taken.insert(*prefix) {
                return Err(DirectoryError::Twice { prefix: *prefix });
            }
        }
        let (mut targets, mut hosts) = (HashSet::new(), HashSet::new());
        // A Merk in use stands in the way unless it is out before the
        // Merks are entered: by the write's own drops, or by the plan.
        let stays = |entry: &Entry| self.is_live(entry) && !leaves(entry);
        for planned in &plan.moved {
            let prefix = planned.to;
            if !targets.insert(prefix) {
                return Err(DirectoryError::Twice { prefix });
            }
            let there = self.stored(pages, &prefix)?;
            if there.is_some_and(|there| stays(&there)) && !taken.contains(&prefix) {
                return Err(DirectoryError::Exists { prefix });
            }
            // Two Merks of the plan would hang at one position of one
            // host: the record of the second would take the place of the
            // first's.
            if !hosts.insert(&planned.child) {
                let host = planned.now.host();
                return Err(DirectoryError::HostTaken {
                    parent: host.map_or(prefix, |host| *host.parent),
                    position: host.map_or(0, |host| host.position),
                });
            }
            let Some(record) = self.get(pages, &planned.child)? else {
                continue;
            };
            let (hosted, id) =
                decode_child(&record).map_err(|what| damaged(&planned.child, what.0))?;
            if taken.contains(&hosted) {
                continue;
            }
            let there = self.stored(pages, &hosted)?;
            if there.is_some_and(|there| there.id() == id && stays(&there)) {
                let host = planned.now.host();
                return Err(DirectoryError::HostTaken {
                    parent: host.map_or(hosted, |host| *host.parent),
                    position: host.map_or(0, |host| host.position),
                });
            }
        }
        Ok(plan)
    }

    /// Makes the re-keying that `plan` holds, which was made of this
    /// directory as it is now but for the Merks that the plan was told
    /// leave before ([`Self::plan_rekey`]).
    ///
    /// It is made in two steps. Every Merk that leaves is taken out, the
    /// innermost first, and every Merk that moves is taken from the
    /// prefix it has. Then each is entered under the prefix it goes to,
    /// where an entry that a dropped Merk left is displaced as a Merk
    /// that is added displaces it ([`Self::add`]).
    ///
    /// One entry is written for each Merk that moves, with the records
    /// that lead to it. A plan that was made of another state can be
    /// refused when it is half made ([`DirectoryError::Exists`],
    /// [`DirectoryError::NotFound`], [`DirectoryError::NotEmpty`]): after
    /// any error the writer's pages are given up.
    pub fn rekey_pairs<P: FlatPagesMut + ?Sized>(
        &mut self,
        pages: &mut P,
        plan: &RekeyPlan,
    ) -> Result<Rekeyed, DirectoryError> {
        let mut pending = Pending::new();
        let rekeyed = self.rekey_pairs_in(&mut pending, pages, plan)?;
        self.settle(pages, pending)?;
        Ok(rekeyed)
    }

    /// [`Self::rekey_pairs`] among the records of a batch ([`Pending`]):
    /// the plan was made of this directory as those records leave it, the
    /// records of the re-keying are put beside them, and no page is
    /// changed. After an error the records are dropped with the writer's
    /// pages: a plan that is refused when it is half made has put a part
    /// of its records.
    pub fn rekey_pairs_in<P: FlatPages + ?Sized>(
        &mut self,
        pending: &mut Pending,
        pages: &P,
        plan: &RekeyPlan,
    ) -> Result<Rekeyed, DirectoryError> {
        let mut rekeyed = Rekeyed::default();
        for prefix in plan.left.iter().rev() {
            rekeyed.left.push(self.unlink_in(pending, pages, prefix)?);
        }
        for planned in &plan.moved {
            if let Some(was) = planned.was.child_key() {
                pending.delete(&was);
            }
            pending.delete(&entry_key(&planned.from));
        }
        for planned in &plan.moved {
            let left = self.free(pending, pages, &planned.to)?;
            if left.is_some() {
                rekeyed.displaced.push(planned.to);
            }
            self.displace(pending, pages, &planned.to, left.as_ref())?;
            Self::enter(pending, &planned.to, &planned.now, Some(&planned.child));
        }
        Ok(rekeyed)
    }
}
