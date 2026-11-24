//! What a settled batch is told by whoever applies it is found by an
//! index, and is what a list of the statements that is looked through
//! has: the hosts of elements, the statements that are made once, and
//! the order of the enters that a batch is applied with.

use lignindb::txn::{HostPut, Subtree, SubtreeWrite};
use rand::{rngs::StdRng, RngExt, SeedableRng};

use super::super::{
    access::with_enters,
    change_set::{Hosts, Once},
};

fn subtree(number: usize) -> Subtree {
    Subtree::new([number as u8; 32])
}

type Said = (Subtree, Vec<u8>, Vec<Subtree>);

/// The statements of elements as a list: a statement takes out the one
/// that was made of its element and goes to the end, and each is found
/// by looking through the list from its start.
#[derive(Default)]
struct Listed(Vec<Said>);

impl Listed {
    fn say(&mut self, subtree: Subtree, key: &[u8], hosted: Vec<Subtree>) {
        self.0.retain(|(of, at, _)| !(*of == subtree && at == key));
        self.0.push((subtree, key.to_vec(), hosted));
    }

    fn hosted(&self, subtree: Subtree, key: &[u8]) -> Option<&[Subtree]> {
        let mut said = self.0.iter();
        let found = said.find(|(of, at, _)| *of == subtree && at == key);
        found.map(|(.., hosted)| hosted.as_slice())
    }

    fn of_merk(&self, merk: &Subtree) -> Option<&Said> {
        self.0.iter().find(|(.., hosted)| hosted.contains(merk))
    }
}

/// Elements are said to host Merks at random: an element is said again
/// with other Merks, a Merk is named by several elements and twice by
/// one, and everything is taken back in between. After each statement
/// every element and every Merk is asked for.
#[test]
fn what_is_said_of_elements_is_found_as_a_list_of_the_statements_has_it() {
    let mut rng = StdRng::seed_from_u64(0x686f_7374);
    let (mut again, mut shared, mut cleared, mut moved_on) = (0, 0, 0, 0);
    for round in 0..600 {
        let (mut hosts, mut listed) = (Hosts::default(), Listed::default());
        for _ in 0..rng.random_range(0..40) {
            if rng.random_range(0..30) == 0 {
                hosts.clear();
                listed.0.clear();
                cleared += 1;
            }
            let of = subtree(rng.random_range(0..3));
            let key = vec![rng.random_range(0..5u8); rng.random_range(0..3)];
            let hosted: Vec<Subtree> = (0..rng.random_range(0..4))
                .map(|_| subtree(10 + rng.random_range(0..8)))
                .collect();
            again += usize::from(listed.hosted(of, &key).is_some());
            // A Merk whose first statement is of this element, and that
            // this statement names no more: the next statement that
            // names it is found for it from here on.
            let first_of = |merk: &Subtree| {
                let found = listed.of_merk(merk);
                found.is_some_and(|(subtree, at, _)| (*subtree, at) == (of, &key))
            };
            let named_on = |merk: &Subtree| {
                let others = listed
                    .0
                    .iter()
                    .filter(|(subtree, at, _)| (*subtree, at) != (of, &key));
                others.filter(|(.., hosted)| hosted.contains(merk)).count() > 0
            };
            let leaves = |merk: &usize| {
                let merk = subtree(*merk);
                first_of(&merk) && !hosted.contains(&merk) && named_on(&merk)
            };
            moved_on += (10..18).filter(leaves).count();
            hosts.say(of, &key, hosted.clone());
            listed.say(of, &key, hosted);
            assert_eq!(hosts.is_empty(), listed.0.is_empty(), "round {round}");
            for of in (0..4).map(subtree) {
                for key in (0..5u8).flat_map(|byte| (0..3).map(move |len| vec![byte; len])) {
                    assert_eq!(
                        hosts.hosted(of, &key),
                        listed.hosted(of, &key),
                        "round {round}"
                    );
                }
            }
            for merk in (10..19).map(subtree) {
                assert_eq!(hosts.of_merk(&merk), listed.of_merk(&merk), "round {round}");
                let naming = listed
                    .0
                    .iter()
                    .filter(|(.., hosted)| hosted.contains(&merk));
                shared += usize::from(naming.count() > 1);
            }
        }
    }
    eprintln!(
        "{again} statements of an element that was said before, {shared} times a Merk was named \
         by more than one element, {moved_on} times the first statement of a Merk named it no \
         more, {cleared} times everything was taken back"
    );
    assert!(again > 3_000 && shared > 10_000 && moved_on > 500 && cleared > 200);
}

#[test]
fn a_statement_that_is_made_once_keeps_the_first_of_each_and_their_order() {
    let mut once: Once<u8, (u8, &str)> = Once::default();
    assert!(once.is_empty());
    for (of, stated) in [(3, "a"), (1, "b"), (3, "c"), (2, "d"), (1, "e")] {
        once.state(of, (of, stated));
    }
    let kept: Vec<_> = once.iter().copied().collect();
    assert_eq!(kept, [(3, "a"), (1, "b"), (2, "d")]);
    once.clear();
    assert!(once.is_empty() && once.iter().next().is_none());
    once.state(3, (3, "f"));
    assert_eq!(once.iter().copied().collect::<Vec<_>>(), [(3, "f")]);
}

/// The order of the subtree writes of a batch with its enters, by the
/// words of [`with_enters`]: the subtree above each enter is looked for
/// among all the enters.
fn by_looking_through_all<'a>(
    own: &[SubtreeWrite<'a>],
    enters: &[SubtreeWrite<'a>],
) -> Vec<SubtreeWrite<'a>> {
    let (mut ordered, entered): (Vec<_>, Vec<_>) =
        (own.iter().chain(enters).copied()).partition(|write| !write.enters());
    let above = |subtree: &Subtree| {
        let host = entered.iter().find_map(|write| match write.entering() {
            Some(enter) if enter.subtree == *subtree => enter.host.map(|host| host.subtree),
            _ => None,
        })?;
        let enters_it = |write: &SubtreeWrite<'_>| write.subtree() == host;
        entered.iter().any(enters_it).then_some(host)
    };
    let depth = |write: &SubtreeWrite<'_>| match write.entering() {
        Some(enter) => {
            let hosts = std::iter::successors(Some(enter.subtree), above);
            hosts.take(entered.len()).count()
        }
        None => 0,
    };
    let mut by_depth: Vec<_> = entered.iter().map(|write| (depth(write), write)).collect();
    by_depth.sort_by_key(|(depth, _)| *depth);
    ordered.extend(by_depth.into_iter().map(|(_, write)| *write));
    ordered
}

/// Enters at random, of both kinds, among drops: a subtree is entered
/// twice, with a host the second time only or with two hosts; hosts are
/// in subtrees that are entered and in subtrees that are not; and
/// subtrees are entered under each other in a ring.
#[test]
fn the_enters_of_a_batch_come_in_the_order_that_looking_through_them_all_gives() {
    let mut rng = StdRng::seed_from_u64(0x656e_7465);
    let (mut twice, mut deep, mut hostless_first) = (0, 0, 0);
    for round in 0..3_000 {
        let subtrees = rng.random_range(1..14);
        let mut writes = Vec::new();
        for _ in 0..rng.random_range(0..2 * subtrees) {
            let of = rng.random_range(0..subtrees);
            let host = match (rng.random_range(0..4), round % 3) {
                (0, _) => None,
                // In one round of three a subtree hangs in any subtree.
                (_, 0) => Some(rng.random_range(0..subtrees + 2)),
                _ => (of > 0).then(|| rng.random_range(0..of)),
            };
            let host = host.map(|host| HostPut {
                subtree: subtree(host),
                key: b"k",
                position: 0,
            });
            let (subtree, tree_type) = (subtree(of), &[][..]);
            writes.push(match rng.random_range(0..5) {
                0 => SubtreeWrite::Drop { subtree },
                1 => SubtreeWrite::Enter {
                    subtree,
                    tree_type,
                    host,
                },
                _ => SubtreeWrite::EnterIfAbsent {
                    subtree,
                    tree_type,
                    host,
                },
            });
        }
        let cut = rng.random_range(0..=writes.len());
        let (own, enters) = writes.split_at(cut);
        let ordered = with_enters(own, enters);
        assert_eq!(
            ordered,
            by_looking_through_all(own, enters),
            "round {round}"
        );
        let entered: Vec<_> = writes.iter().filter_map(SubtreeWrite::entering).collect();
        let count_of = |one: Subtree| {
            let of_it = entered.iter().filter(|enter| enter.subtree == one);
            of_it.count()
        };
        twice += usize::from(entered.iter().any(|enter| count_of(enter.subtree) > 1));
        let first_has_none = |enter: &lignindb::txn::Entering<'_>| {
            let mut of_it = entered
                .iter()
                .filter(|other| other.subtree == enter.subtree);
            let first = of_it.next().is_some_and(|first| first.host.is_none());
            first && of_it.any(|later| later.host.is_some())
        };
        hostless_first += usize::from(entered.iter().any(first_has_none));
        // Three enters under each other.
        let hosted_in = |enter: &lignindb::txn::Entering<'_>| {
            let host = enter.host?.subtree;
            entered.iter().find(|above| above.subtree == host)
        };
        let chain =
            |enter: &lignindb::txn::Entering<'_>| hosted_in(enter).and_then(hosted_in).is_some();
        deep += usize::from(entered.iter().any(chain));
    }
    eprintln!(
        "{twice} rounds entered a subtree twice, {hostless_first} of them first with no host and \
         then with one, {deep} rounds had three enters under each other"
    );
    assert!(twice > 1_000 && hostless_first > 100 && deep > 500);
}
