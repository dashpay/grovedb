//! Whether a subtree leaves with a move of the transaction that reads it
//! (`Opened::leaves_with_a_move`, `transactions.md` MV2), whether the
//! transaction took it with a subtree above it that it dropped or
//! cleared (`Opened::seen_entry`, CV9), and whether a move decides what
//! is read under it (`Opened::moved`), over transactions at random.
//! Each answer is made from the subtree's own hosts, and at every call
//! it is held to the answer it was made with before, which opens every
//! prefix that a move of the transaction took a subtree from, every
//! subtree that the transaction dropped or cleared, or every prefix
//! that a move names (`opened::held`). Here the transactions are made
//! that the answers are compared over: subtrees entered under each
//! other, some with nodes and some with none; moves, of a subtree that
//! was moved, of one under a host that was moved, and to a prefix that
//! a move left; drops and clears between them, of one subtree and of
//! several at once; marks and rollbacks to them; scopes that merge and
//! scopes that are rolled back; reads by a scope whose writes are kept
//! in the write set, which a move refuses; and commits, after which the
//! next transaction reads another version of the directory.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    model::{self, handle_of, OwnedBatch, OwnedMerk, OwnedSubtree},
    store::{Db, Outcome, Rng, ENTERED},
    subtrees::{aux, Held, Items},
    *,
};
use crate::txn::opened::held::{compared, Compared};

/// The prefixes of a run: none is entered where the run begins.
const POOL: [u8; 16] = {
    let mut pool = [0; 16];
    let mut at = 0;
    while at < pool.len() {
        pool[at] = ENTERED + 10 + at as u8;
        at += 1;
    }
    pool
};

fn sub(name: u8) -> Subtree {
    model::subtree(name)
}

/// What a run knows of its transaction: the subtrees it holds, the
/// prefixes that its moves took a subtree from, and those they brought
/// one to.
#[derive(Debug, Clone, Default)]
struct Account {
    held: Held,
    left: BTreeSet<u8>,
    brought: BTreeSet<u8>,
}

/// A change of the items of one Merk.
type Edit = Box<dyn FnOnce(&mut Items)>;

/// What a batch does to the account of its run, once it is applied.
type Noted = Box<dyn FnOnce(&mut Account)>;

/// What one batch changes in the Merks of a run, by Merk.
#[derive(Default)]
struct Changes {
    edits: BTreeMap<u8, Vec<Edit>>,
    /// The subtree that the node under a key of a Merk hosts once the
    /// batch is applied, where the batch says so.
    hosted: BTreeMap<(u8, Vec<u8>), u8>,
    /// The Merks that the batch changes with no change set: one that it
    /// clears. The nodes that host them are written again as well.
    cleared: Vec<u8>,
}

impl Changes {
    fn edit(&mut self, name: u8, edit: impl FnOnce(&mut Items) + 'static) {
        self.edits.entry(name).or_default().push(Box::new(edit));
    }

    /// Writes again the node that hosts `name`, and the nodes that host
    /// the Merks above it.
    fn write_hosts_of(&mut self, held: &Held, name: u8, stamp: u8) {
        let mut at = name;
        while let Some((host, key)) = held.live.get(&at).and_then(|(host, _)| host.clone()) {
            // A node that the batch says more of is written as that says.
            self.hosted.entry((host, key.clone())).or_insert(at);
            let written = self.edits.contains_key(&host);
            self.edit(host, move |items| {
                if let Some(value) = items.get_mut(&key) {
                    value.push(stamp);
                }
            });
            if written {
                break;
            }
            at = host;
        }
    }

    /// The change sets of the batch: one for each Merk that is changed,
    /// and one for each Merk above one of those, whose node that hosts
    /// the Merk below is written again and names it, as the element of a
    /// tree is written wherever its Merk changed. `held` takes them in.
    fn merks(mut self, held: &mut Held, stamp: u8) -> Vec<OwnedMerk> {
        let changed: Vec<u8> = self.edits.keys().copied().collect();
        for name in changed.into_iter().chain(std::mem::take(&mut self.cleared)) {
            self.write_hosts_of(held, name, stamp);
        }
        let mut hosted = self.hosted;
        let mut merks = Vec::new();
        for (name, edits) in self.edits {
            let mut merk = held.change(name, |items| {
                for edit in edits {
                    edit(items);
                }
            });
            // Its host is written with it.
            merk.anchored = false;
            let named: Vec<(u8, Vec<u8>)> = (hosted.keys())
                .filter(|(host, _)| *host == name)
                .cloned()
                .collect();
            for at in named {
                let below = hosted.remove(&at).expect("a named node");
                merk.hosted.insert(at.1, vec![sub(below)]);
            }
            merks.push(merk);
        }
        merks
    }
}

/// What the runs made, counted where it was applied.
#[derive(Debug, Default)]
struct Made {
    applied: usize,
    refused: usize,
    moves: usize,
    /// Moves of a subtree that a move of the transaction brought.
    moved_again: usize,
    /// Moves of a subtree whose host is in one that a move brought.
    under_a_moved_host: usize,
    /// Moves to a prefix that a move of the transaction took a subtree
    /// from.
    to_a_left_prefix: usize,
    /// Moves that named a subtree below, and moves that a subtree with no
    /// node left with.
    with_one_below: usize,
    left_behind: usize,
    /// Batches that took out of the transaction what a commit left under
    /// a hosted subtree, and moves of such a subtree right after.
    left_below: usize,
    moved_over_what_left: usize,
    drops: usize,
    clears: usize,
    /// Batches that dropped or cleared several subtrees, and how many
    /// subtrees those were.
    many_left: usize,
    left_at_once: usize,
    rewinds: usize,
    scopes_rolled_back: usize,
    commits: usize,
    /// Reads that were made while the transaction held a move.
    reads_beside_a_move: usize,
    /// Reads of a scope whose writes are kept in the write set, made
    /// while the transaction held a move, and those of them that the
    /// move refused.
    reads_of_a_scope: usize,
    refused_by_a_move: usize,
}

/// One run: `steps` steps on a store of its own.
fn run(seed: u64, steps: usize, made: &mut Made) {
    let (db, versions) = Db::new(256);
    let world = World::new(&db, &versions);
    let mut rng = Rng::new(seed);
    let mut tx = world.begin();
    let mut account = Account::default();
    let mut marks: Vec<(Mark, Account)> = Vec::new();
    // The subtrees as the last commit left them, and the subtree that
    // the next step is to try to move.
    let mut committed = Held::default();
    let mut to_move: Option<u8> = None;
    for step in 0..steps {
        let live: Vec<u8> = account.held.live.keys().copied().collect();
        let free: Vec<u8> = (POOL.into_iter())
            .filter(|name| !account.held.live.contains_key(name))
            .collect();
        let pick = |rng: &mut Rng, from: &[u8]| from[rng.below(from.len())];
        // What the last commit left under a subtree that an element
        // hosts, where the transaction has moved neither: each with that
        // subtree. Once the transaction drops such a subtree, or clears
        // the one above it, it is no subtree of the transaction's, and a
        // move of the one above it neither names it nor takes it out: it
        // leaves with that move.
        let untouched = |name: &u8| !account.left.contains(name) && !account.brought.contains(name);
        let under_a_host: Vec<(u8, u8)> = (live.iter())
            .filter_map(|name| {
                let (above, _) = account.held.live[name].0.as_ref()?;
                account.held.live[above].0.as_ref()?;
                let (was_above, _) = committed.live.get(name)?.0.as_ref()?;
                let as_committed = was_above == above && committed.live.contains_key(above);
                (as_committed && untouched(name) && untouched(above)).then_some((*name, *above))
            })
            .collect();
        let before = account.clone();
        let mut batch = OwnedBatch::default();
        let mut changes = Changes::default();
        // What the account holds once the batch is applied, made after
        // the change sets are: they are of the Merks as they are before.
        let mut after: Option<Noted> = None;
        let mut counted = Made::default();
        // The subtree above what this step takes out of the
        // transaction, which the next step tries to move if this one
        // is kept: it moves it where enough prefixes are free.
        let mut left_under: Option<u8> = None;
        let stamp = step as u8;
        let mut what = match live.is_empty() {
            true => 0,
            false => rng.below(100),
        };
        // The subtree that the step before left something under is
        // to be moved now, if an element still hosts it.
        let hosted =
            |name: &u8| (account.held.live.get(name)).is_some_and(|(host, _)| host.is_some());
        let moved_now = to_move.take().filter(hosted);
        if moved_now.is_some() {
            what = 40;
        }
        match what {
            // A subtree is entered, under an element that the batch
            // writes or with no host. It has no node until one is
            // written into it.
            0..22 => {
                if free.is_empty() {
                    continue;
                }
                let name = pick(&mut rng, &free);
                let hosted = !live.is_empty() && !rng.chance(12);
                let host = hosted.then(|| pick(&mut rng, &live));
                let host = host.map(|host| (host, vec![b'h', name]));
                if let Some((host, key)) = host.clone() {
                    changes.hosted.insert((host, key.clone()), name);
                    changes.edit(host, move |items| drop(items.insert(key, b"tree".to_vec())));
                }
                let at = host.clone().map(|(host, key)| (sub(host), key, 0));
                batch.subtrees.push(OwnedSubtree::Enter(sub(name), at));
                after = Some(Box::new(move |account| {
                    account.held.live.insert(name, (host, BTreeMap::new()));
                }));
            }
            // Nodes that host nothing come and go.
            22..29 => {
                let name = pick(&mut rng, &live);
                let written: Vec<(Vec<u8>, Option<Vec<u8>>)> = (0..1 + rng.below(3))
                    .map(|_| {
                        let key = vec![b'n', rng.below(5) as u8];
                        let value = (!rng.chance(25)).then(|| vec![rng.below(250) as u8; 12]);
                        (key, value)
                    })
                    .collect();
                let items = &account.held.live[&name].1;
                let changes_it =
                    |(key, value): &(Vec<u8>, Option<Vec<u8>>)| items.get(key) != value.as_ref();
                if !written.iter().any(changes_it) {
                    continue;
                }
                changes.edit(name, move |items| {
                    for (key, value) in written {
                        match value {
                            Some(value) => drop(items.insert(key, value)),
                            None => drop(items.remove(&key)),
                        }
                    }
                });
            }
            // Several subtrees leave in one batch, none of them below
            // another: each is dropped, with the element that hosted
            // it, or cleared.
            29..34 => {
                let mut rest = live.clone();
                let mut chosen: Vec<(u8, bool)> = Vec::new();
                // Those that are chosen, and what is below them.
                let mut taken: BTreeSet<u8> = BTreeSet::new();
                for _ in 0..2 + rng.below(6) {
                    if rest.is_empty() {
                        break;
                    }
                    let name = rest.swap_remove(rng.below(rest.len()));
                    let below = account.held.below(name);
                    let above_one = |(other, _): &(u8, bool)| below.contains(other);
                    if taken.contains(&name) || chosen.iter().any(above_one) {
                        continue;
                    }
                    taken.insert(name);
                    taken.extend(below);
                    chosen.push((name, rng.chance(50)));
                }
                if chosen.len() < 2 {
                    continue;
                }
                for (name, dropped) in &chosen {
                    if !*dropped {
                        batch.subtrees.push(OwnedSubtree::Clear(sub(*name)));
                        changes.cleared.push(*name);
                        continue;
                    }
                    if let Some((host, key)) = account.held.live[name].0.clone() {
                        changes.edit(host, move |items| drop(items.remove(&key)));
                    }
                    batch.subtrees.push(OwnedSubtree::Drop(sub(*name)));
                }
                counted.many_left = 1;
                counted.left_at_once = chosen.len();
                after = Some(Box::new(move |account| {
                    for (name, dropped) in chosen {
                        account.held.empty(name);
                        if dropped {
                            account.held.live.remove(&name);
                        }
                    }
                }));
            }
            // What a commit left under a hosted subtree leaves the
            // transaction: it is dropped, or the subtree above it is
            // cleared. The next step tries to move that subtree.
            34..40 => {
                if under_a_host.is_empty() {
                    continue;
                }
                let (name, above) = under_a_host[rng.below(under_a_host.len())];
                left_under = Some(above);
                counted.left_below = 1;
                if rng.chance(50) {
                    batch.subtrees.push(OwnedSubtree::Clear(sub(above)));
                    changes.cleared.push(above);
                    counted.clears = 1;
                    after = Some(Box::new(move |account| account.held.empty(above)));
                } else {
                    let (host, key) = account.held.live[&name].0.clone().expect("a host");
                    changes.edit(host, move |items| drop(items.remove(&key)));
                    batch.subtrees.push(OwnedSubtree::Drop(sub(name)));
                    counted.drops = 1;
                    after = Some(Box::new(move |account| {
                        account.held.empty(name);
                        account.held.live.remove(&name);
                    }));
                }
            }
            // A subtree that an element hosts is moved to another key of
            // that element's Merk, with every subtree below it that has a
            // node, and with those that have none or without them.
            40..66 => {
                let movable: Vec<u8> = live.iter().filter(|name| hosted(name)).copied().collect();
                if movable.is_empty() {
                    continue;
                }
                let name = match moved_now {
                    Some(name) => name,
                    None => pick(&mut rng, &movable),
                };
                counted.moved_over_what_left = usize::from(moved_now.is_some());
                let below = account.held.below(name);
                let mut named = vec![name];
                for other in &below {
                    if !account.held.live[other].1.is_empty() || rng.chance(50) {
                        named.push(*other);
                    }
                }
                if free.len() < named.len() {
                    continue;
                }
                // Each goes to a prefix that is free, drawn at random.
                let mut free = free;
                let mut to = BTreeMap::new();
                for old in &named {
                    to.insert(*old, free.swap_remove(rng.below(free.len())));
                }
                let (parent, old_key) = account.held.live[&name].0.clone().expect("a host");
                let new_key = vec![b'm', stamp];
                let read = tx.txn.node(sub(parent), &old_key, handle_of);
                changes.hosted.insert((parent, new_key.clone()), to[&name]);
                let (from_key, to_key) = (old_key.clone(), new_key.clone());
                changes.edit(parent, move |items| {
                    let value = items.remove(&from_key).expect("the node of the host");
                    items.insert(to_key, value);
                });
                batch.subtrees.push(OwnedSubtree::Move {
                    from: sub(name),
                    to: sub(to[&name]),
                    host: (sub(parent), new_key.clone(), 0),
                    read: read.ok().flatten(),
                    below: (named[1..].iter())
                        .map(|old| (sub(*old), sub(to[old])))
                        .collect(),
                });
                counted.moves = 1;
                counted.moved_again = usize::from(account.brought.contains(&name));
                counted.under_a_moved_host = usize::from(account.brought.contains(&parent));
                let left = |new: &u8| account.left.contains(new);
                counted.to_a_left_prefix = usize::from(to.values().any(left));
                counted.with_one_below = usize::from(named.len() > 1);
                counted.left_behind = usize::from(named.len() <= below.len());
                // The subtrees are under their new prefixes, each under
                // the host it had, by that host's new prefix. One with no
                // node that was not named left with the move.
                after = Some(Box::new(move |account| {
                    let all = std::iter::once(name).chain(below);
                    let taken: Vec<_> = all
                        .map(|old| (old, account.held.live.remove(&old).expect("a live subtree")))
                        .collect();
                    for (old, (host, items)) in taken {
                        let Some(new) = to.get(&old) else {
                            continue;
                        };
                        let host = match old == name {
                            true => Some((parent, new_key.clone())),
                            false => host.map(|(above, key)| (to[&above], key)),
                        };
                        account.held.live.insert(*new, (host, items));
                    }
                    account.left.extend(named.iter().copied());
                    account.brought.extend(to.values().copied());
                }));
            }
            // A subtree is cleared.
            66..74 => {
                let name = pick(&mut rng, &live);
                batch.subtrees.push(OwnedSubtree::Clear(sub(name)));
                changes.cleared.push(name);
                counted.clears = 1;
                after = Some(Box::new(move |account| account.held.empty(name)));
            }
            // A subtree is dropped, with the element that hosted it.
            74..84 => {
                let name = pick(&mut rng, &live);
                if let Some((host, key)) = account.held.live[&name].0.clone() {
                    changes.edit(host, move |items| drop(items.remove(&key)));
                }
                batch.subtrees.push(OwnedSubtree::Drop(sub(name)));
                counted.drops = 1;
                after = Some(Box::new(move |account| {
                    account.held.empty(name);
                    account.held.live.remove(&name);
                }));
            }
            // A place to go back to.
            84..89 => {
                marks.push((tx.txn.mark(), account.clone()));
                continue;
            }
            // Back to one of them.
            89..94 => {
                if marks.is_empty() {
                    continue;
                }
                marks.truncate(1 + rng.below(marks.len()));
                let (mark, at_the_mark) = marks.pop().expect("a mark");
                made.rewinds += usize::from(tx.txn.rewind(mark));
                account = at_the_mark;
            }
            // The transaction commits, and the next one reads the
            // version it made.
            _ => {
                let outcome = db.commit(&versions, &tx.txn).expect("a commit");
                made.commits += usize::from(matches!(outcome, Outcome::Committed(_)));
                committed = account.held.clone();
                tx = world.begin();
                marks.clear();
                account.left.clear();
                account.brought.clear();
            }
        }
        batch.merks = changes.merks(&mut account.held, stamp);
        if let Some(after) = after {
            after(&mut account);
        }
        if !batch.merks.is_empty() || !batch.subtrees.is_empty() {
            let scope = tx.txn.scope().expect("a scope");
            let applied = batch.through(&scope).with(|batch| scope.apply(batch));
            // One scope in six is rolled back: its writes were never
            // made.
            let kept = applied.is_ok() && !rng.chance(16);
            match kept {
                true => scope.merge(),
                false => scope.rollback(),
            }
            match (applied.is_ok(), kept) {
                (true, true) => {
                    made.applied += 1;
                    made.moves += counted.moves;
                    made.moved_again += counted.moved_again;
                    made.under_a_moved_host += counted.under_a_moved_host;
                    made.to_a_left_prefix += counted.to_a_left_prefix;
                    made.with_one_below += counted.with_one_below;
                    made.left_behind += counted.left_behind;
                    made.left_below += counted.left_below;
                    made.moved_over_what_left += counted.moved_over_what_left;
                    made.drops += counted.drops;
                    made.clears += counted.clears;
                    made.many_left += counted.many_left;
                    made.left_at_once += counted.left_at_once;
                }
                (true, false) => made.scopes_rolled_back += 1,
                (false, _) => made.refused += 1,
            }
            match kept {
                true => to_move = left_under,
                false => account = before,
            }
        }
        // Every prefix is read, in two columns: what is under it on the
        // committed side is asked of the directory for each.
        let moving = !account.left.is_empty();
        for name in POOL {
            let mut value = Vec::new();
            let _ = tx.txn.get(aux(name), b"x", &mut value);
            let _ = tx.txn.node(sub(name), &[b'n', 1], handle_of);
            made.reads_beside_a_move += 2 * usize::from(moving);
        }
        // One step in three while the transaction holds a move, every
        // prefix is read once more by a scope whose writes are kept in
        // the write set: it is answered where no move decides what is
        // under the prefix, and refused where one does.
        if moving && rng.chance(34) {
            let laid_out = tx.txn.scope().expect("a scope");
            let by_key = tx.txn.scope().expect("a scope");
            assert!(!by_key.is_laid_out());
            for name in POOL {
                let mut value = Vec::new();
                let read = by_key.get(aux(name), b"x", &mut value);
                made.reads_of_a_scope += 1;
                match read {
                    Ok(_) => {}
                    Err(TxnError::Batch(BatchError::MoveBeside { .. })) => {
                        made.refused_by_a_move += 1;
                    }
                    Err(other) => panic!("a scope beside a move is refused otherwise: {other}"),
                }
            }
            by_key.rollback();
            laid_out.rollback();
        }
    }
    let _ = db.commit(&versions, &tx.txn).expect("a commit");
}

/// The runs of `seeds`, with what they made and what was compared in
/// them.
fn runs(seeds: std::ops::Range<u64>) -> (Made, Compared) {
    compared();
    let mut made = Made::default();
    for seed in seeds {
        run(seed, 60, &mut made);
    }
    let compared = compared();
    println!("compared: {compared:?}; {made:?}");
    (made, compared)
}

/// What each quarter of the runs has to have made and compared, so that
/// none of the kinds of transaction is missing from it.
fn assert_made(made: &Made, compared: &Compared) {
    assert_eq!(
        compared.unanswered, 0,
        "the hosts answer in every sound directory"
    );
    assert!(compared.leaves > 250 && compared.stays > 5_000 && compared.absent > 50_000);
    assert!(compared.unasked > 150_000);
    assert!(compared.deepest >= 3);
    // What the transaction took with a subtree above, by a drop or a
    // clear that says so and by a clear of which the version says, and
    // what it kept; with clears of each kind in its write set.
    assert!(compared.taken > 1_500 && compared.taken_as_the_version_says > 300);
    assert!(compared.kept > 8_000);
    assert!(compared.beside_a_clear_of_no_node > 1_000 && compared.beside_a_clear_unsaid > 1_000);
    // What a move decides for a scope beside it, by a prefix above.
    assert!(compared.below_a_name > 200 && compared.below_no_name > 5_000);
    assert!(made.reads_of_a_scope > 50_000 && made.refused_by_a_move > 15_000);
    assert!(made.many_left > 300 && made.left_at_once > 2 * made.many_left);
    assert!(made.moves > 2_000 && made.with_one_below > 200 && made.left_behind > 150);
    assert!(made.left_below > 40 && made.moved_over_what_left > 40);
    assert!(made.moved_again > 800 && made.under_a_moved_host > 150);
    assert!(made.to_a_left_prefix > 300);
    assert!(made.drops > 1_000 && made.clears > 1_000 && made.rewinds > 200);
    assert!(made.scopes_rolled_back > 1_000 && made.commits > 800);
    assert!(made.reads_beside_a_move > 100_000);
}

#[test]
fn the_hosts_of_a_subtree_answer_as_every_prefix_does_in_the_first_quarter_of_the_runs() {
    let (made, compared) = runs(0..750);
    assert_made(&made, &compared);
}

#[test]
fn the_hosts_of_a_subtree_answer_as_every_prefix_does_in_the_second_quarter_of_the_runs() {
    let (made, compared) = runs(750..1_500);
    assert_made(&made, &compared);
}

#[test]
fn the_hosts_of_a_subtree_answer_as_every_prefix_does_in_the_third_quarter_of_the_runs() {
    let (made, compared) = runs(1_500..2_250);
    assert_made(&made, &compared);
}

#[test]
fn the_hosts_of_a_subtree_answer_as_every_prefix_does_in_the_last_quarter_of_the_runs() {
    let (made, compared) = runs(2_250..3_000);
    assert_made(&made, &compared);
}
