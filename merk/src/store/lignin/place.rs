//! Where a cursor of the store stands, on its subtree's keys and beside
//! them (`flat-store.md`, rule 6).
//!
//! RocksDB keeps the records of every subtree of a column in one column
//! family, in the order of prefix and key, and a cursor of a subtree is an
//! iterator of the column family that shows a record of another subtree
//! as no record. A move that leaves the subtree therefore leaves the
//! iterator on a record beside it, if there is one that way, and a step
//! goes on from there: back into the subtree, or further out. With no
//! record that way the iterator stands nowhere, and a step from nowhere
//! does nothing.
//!
//! GroveDB's queries rely on the first: a descending read of a range seeks
//! to the range's end and steps back. So a cursor of the store keeps the
//! same place. LigninDB keeps the nodes of a Merk in the Merk's own tree,
//! and the engine is asked how many records lie beside the subtree
//! ([`Line::beside`]) when a step is made from no key. No other move asks.

use super::{
    access::{Seek, Walk},
    flat::FlatWalk,
    left::Left,
};
use crate::Error;

/// The two calls of a cursor of the engine that a place is kept by.
pub(super) trait Line {
    /// Moves the cursor among the keys of its subtree. `key` takes the key
    /// it then stands on, and the answer is whether it stands on one.
    fn go(&mut self, to: Seek<'_>, key: &mut Vec<u8>) -> Result<bool, Error>;

    /// How many records the cursor's column holds in the subtrees after
    /// the cursor's own in the order of their prefixes, or with `back` in
    /// those before it, counted up to `limit`.
    fn beside(&mut self, back: bool, limit: usize) -> Result<usize, Error>;
}

/// A cursor of the engine as a line, with what its own cursor knows of
/// the marks of flat drops ([`Left`]): each mark of the cursor's keyspace
/// under a prefix on the side that is asked counts as one record beside
/// the subtree.
pub(super) struct Marked<'a, W: ?Sized> {
    pub(super) walk: &'a mut W,
    pub(super) left: Option<Left>,
}

impl Line for Marked<'_, dyn Walk + '_> {
    fn go(&mut self, to: Seek<'_>, key: &mut Vec<u8>) -> Result<bool, Error> {
        self.walk.go(to, key)
    }

    fn beside(&mut self, back: bool, limit: usize) -> Result<usize, Error> {
        match self.left {
            Some(left) => left.beside(back, |marks| self.walk.beside(back, limit, Some(marks))),
            None => self.walk.beside(back, limit, None),
        }
    }
}

impl Line for Marked<'_, dyn FlatWalk + '_> {
    fn go(&mut self, to: Seek<'_>, key: &mut Vec<u8>) -> Result<bool, Error> {
        self.walk.go(to, key)
    }

    fn beside(&mut self, back: bool, limit: usize) -> Result<usize, Error> {
        match self.left {
            Some(left) => left.beside(back, |marks| self.walk.beside(back, limit, Some(marks))),
            None => self.walk.beside(back, limit, None),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Stand {
    /// On no record of the column: a step does nothing.
    #[default]
    Nowhere,
    /// On a key of the subtree.
    On,
    /// Off the subtree's keys, behind the last of them or with `back`
    /// before the first: on the nearest record of another subtree that
    /// way, or nowhere. Which of the two is asked when a step needs it.
    Off { back: bool },
    /// On a record of another subtree: the `depth`th after the subtree's
    /// keys, or with `back` the `depth`th before them.
    Beside { back: bool, depth: usize },
}

/// The place of a cursor: the key of its subtree it stands on, or where
/// beside the subtree it stands.
#[derive(Debug, Default)]
pub(super) struct Place {
    key: Vec<u8>,
    stand: Stand,
}

impl Place {
    /// The key of its subtree the cursor stands on, if it stands on one.
    pub(super) fn key(&self) -> Option<&[u8]> {
        (self.stand == Stand::On).then_some(self.key.as_slice())
    }

    /// Takes the cursor off every record.
    pub(super) fn leave(&mut self) {
        self.stand = Stand::Nowhere;
    }

    /// Makes the move `to` on `line`. A move that fails leaves the cursor
    /// nowhere.
    ///
    /// The engine is asked what lies beside the subtree by a step from no
    /// key, and by no other move: a seek, and a step from a key, move
    /// among the subtree's keys and ask nothing, also where they leave
    /// them.
    pub(super) fn go<L: Line + ?Sized>(&mut self, line: &mut L, to: Seek<'_>) -> Result<(), Error> {
        let step_back = match to {
            Seek::Next => false,
            Seek::Prev => true,
            // A seek that finds no key of the subtree has gone past its
            // keys the way it looked.
            Seek::First | Seek::From(_) => return self.within(line, to, false),
            Seek::Last | Seek::Back(_) => return self.within(line, to, true),
        };
        let (back, depth) = match std::mem::take(&mut self.stand) {
            Stand::Nowhere => return Ok(()),
            Stand::On => return self.within(line, to, step_back),
            Stand::Off { back } => match line.beside(back, 1)? {
                1 => (back, 1),
                _ => return Ok(()),
            },
            Stand::Beside { back, depth } => (back, depth),
        };
        // A step into the subtree from the side the cursor left on: to the
        // last key from behind, to the first from before. A subtree with no
        // key is stepped over.
        if step_back != back && depth == 1 {
            let into = match back {
                false => Seek::Last,
                true => Seek::First,
            };
            return self.within(line, into, !back);
        }
        self.stand = match step_back == back {
            // A step further out, if a record is there.
            true => match line.beside(back, depth + 1)? == depth + 1 {
                true => Stand::Beside {
                    back,
                    depth: depth + 1,
                },
                false => Stand::Nowhere,
            },
            // A step back towards the subtree, over a record that was
            // counted when the cursor went out over it.
            false => Stand::Beside {
                back,
                depth: depth - 1,
            },
        };
        Ok(())
    }

    /// Moves among the keys of the subtree, and where that comes to no
    /// key, off them on the side `back` says.
    fn within<L: Line + ?Sized>(
        &mut self,
        line: &mut L,
        to: Seek<'_>,
        back: bool,
    ) -> Result<(), Error> {
        self.stand = Stand::Nowhere;
        self.stand = match line.go(to, &mut self.key)? {
            true => Stand::On,
            false => Stand::Off { back },
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A subtree with `keys`, between `before` records of other subtrees
    /// and `after` of them, as the engine answers for it.
    struct Fake {
        before: usize,
        keys: Vec<Vec<u8>>,
        after: usize,
        at: Option<usize>,
        /// How often the engine was asked what lies beside the subtree,
        /// and the most records out it was asked for.
        asked: usize,
        deepest: usize,
        /// Whether the question is refused, and whether a move is.
        refuses: bool,
        refuses_moves: bool,
    }

    impl Line for Fake {
        fn go(&mut self, to: Seek<'_>, key: &mut Vec<u8>) -> Result<bool, Error> {
            if self.refuses_moves {
                return Err(Error::CorruptedData("refused".to_owned()));
            }
            let keys = &self.keys;
            self.at = match to {
                Seek::First => (!keys.is_empty()).then_some(0),
                Seek::Last => keys.len().checked_sub(1),
                Seek::From(from) => keys.iter().position(|key| key[..] >= *from),
                Seek::Back(from) => keys.iter().rposition(|key| key[..] <= *from),
                Seek::Next => self.at.map(|at| at + 1).filter(|at| *at < keys.len()),
                Seek::Prev => self.at.and_then(|at| at.checked_sub(1)),
            };
            key.clear();
            key.extend(self.at.into_iter().flat_map(|at| &keys[at]));
            Ok(self.at.is_some())
        }

        fn beside(&mut self, back: bool, limit: usize) -> Result<usize, Error> {
            self.asked += 1;
            self.deepest = self.deepest.max(limit);
            if self.refuses {
                return Err(Error::CorruptedData("refused".to_owned()));
            }
            Ok(limit.min(match back {
                false => self.after,
                true => self.before,
            }))
        }
    }

    /// The iterator of RocksDB over the same records as one line: the
    /// index of the record it stands on, or none.
    struct Raw<'a> {
        fake: &'a Fake,
        at: Option<usize>,
    }

    impl Raw<'_> {
        fn len(&self) -> usize {
            self.fake.before + self.fake.keys.len() + self.fake.after
        }

        fn go(&mut self, to: Seek<'_>) {
            let (before, keys) = (self.fake.before, &self.fake.keys);
            let end = before + keys.len();
            // The first record at or after an index, and the last before
            // one.
            let from = |at: usize| (at < self.len()).then_some(at);
            let back = |at: usize| at.checked_sub(1);
            self.at = match to {
                Seek::First => from(before),
                Seek::Last => back(end),
                Seek::From(key) => {
                    let within = keys.iter().position(|own| own[..] >= *key);
                    from(within.map_or(end, |at| before + at))
                }
                Seek::Back(key) => {
                    let within = keys.iter().rposition(|own| own[..] <= *key);
                    back(within.map_or(before, |at| before + at + 1))
                }
                Seek::Next => self.at.and_then(|at| from(at + 1)),
                Seek::Prev => self.at.and_then(back),
            };
        }

        /// The key of the subtree the iterator stands on.
        fn key(&self) -> Option<&[u8]> {
            let within = self.at?.checked_sub(self.fake.before)?;
            self.fake.keys.get(within).map(Vec::as_slice)
        }
    }

    const MOVES: [Seek<'static>; 8] = [
        Seek::First,
        Seek::Last,
        Seek::From(&[2]),
        Seek::From(&[9]),
        Seek::Back(&[0]),
        Seek::Back(&[2]),
        Seek::Next,
        Seek::Prev,
    ];

    fn fake(before: usize, keys: usize, after: usize) -> Fake {
        Fake {
            before,
            keys: [vec![1], vec![3], vec![5]][..keys].to_vec(),
            after,
            at: None,
            asked: 0,
            deepest: 0,
            refuses: false,
            refuses_moves: false,
        }
    }

    /// Makes `moves` with a place and with the iterator of the line, and
    /// holds the place to the iterator after each. Returns how often the
    /// engine was asked.
    fn walk(before: usize, keys: usize, after: usize, moves: &[Seek<'_>]) -> usize {
        walked(before, keys, after, moves).0
    }

    /// As [`walk`], and returns also the most records out of the subtree
    /// the engine was asked for, and how often a step from no key came
    /// onto a key.
    fn walked(
        before: usize,
        keys: usize,
        after: usize,
        moves: &[Seek<'_>],
    ) -> (usize, usize, usize) {
        let mut came_back = 0;
        let (mut line, shape) = (fake(before, keys, after), fake(before, keys, after));
        let mut iterator = Raw {
            fake: &shape,
            at: None,
        };
        let mut place = Place::default();
        for (at, to) in moves.iter().enumerate() {
            let asked = line.asked;
            let on_key = iterator.key().is_some();
            place.go(&mut line, *to).expect("no move is refused");
            iterator.go(*to);
            assert_eq!(
                place.key(),
                iterator.key(),
                "{before} before, {keys} keys, {after} after: {:?}",
                &moves[..=at]
            );
            // Only a step from no key asks the engine.
            let steps = matches!(to, Seek::Next | Seek::Prev);
            if !steps || on_key {
                assert_eq!(line.asked, asked, "{:?}", &moves[..=at]);
            }
            came_back += usize::from(steps && !on_key && place.key().is_some());
        }
        (line.asked, line.deepest, came_back)
    }

    #[test]
    fn a_place_is_where_the_iterator_of_one_line_stands() {
        let sizes = [0, 1, 3];
        let (mut asked, mut deepest, mut came_back) = (0, 0, 0);
        let mut tell = |(asks, depth, back): (usize, usize, usize)| {
            asked += asks;
            deepest = deepest.max(depth);
            came_back += back;
        };
        let mut shapes = Vec::new();
        for before in sizes {
            for keys in 0..=3 {
                shapes.extend(sizes.map(|after| (before, keys, after)));
            }
        }
        for (before, keys, after) in shapes {
            // Every walk of five moves.
            for choice in 0..MOVES.len().pow(5) {
                let moves: Vec<_> = (0..5)
                    .map(|at| MOVES[choice / MOVES.len().pow(at) % MOVES.len()])
                    .collect();
                tell(walked(before, keys, after, &moves));
            }
            // Long walks, mostly of steps, that go far out and come back.
            let mut state = 0x9e37_79b9_7f4a_7c15_u64 ^ (before * 100 + keys * 10 + after) as u64;
            for _ in 0..400 {
                let moves: Vec<_> = (0..40)
                    .map(|_| {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        match state % 12 {
                            choice @ 0..=5 => MOVES[choice as usize],
                            6..=8 => Seek::Next,
                            _ => Seek::Prev,
                        }
                    })
                    .collect();
                tell(walked(before, keys, after, &moves));
            }
        }
        // The walks stepped from no key, went as far out as the lines have
        // records and one further, and came back onto keys.
        assert!(
            asked > 100_000 && came_back > 10_000,
            "{asked}, {came_back}"
        );
        assert_eq!(deepest, 4);
    }

    #[test]
    fn a_scan_and_a_seek_ask_the_engine_nothing() {
        use Seek::{Back, First, From, Last, Next, Prev};
        let forward = [First, Next, Next, Next];
        let backward = [Last, Prev, Prev, Prev];
        let seeks = [From(&[9]), Back(&[0]), From(&[2]), Back(&[2]), First, Last];
        for moves in [&forward[..], &backward[..], &seeks[..]] {
            assert_eq!(walk(2, 3, 2, moves), 0, "{moves:?}");
        }
        assert_eq!(walk(2, 0, 2, &seeks), 0);
        // The step back from past the end asks once, and once more for
        // each record further out.
        assert_eq!(walk(2, 3, 2, &[From(&[9]), Prev, Prev]), 1);
        assert_eq!(walk(2, 3, 2, &[From(&[9]), Next, Next, Next]), 3);
        // From nowhere no step asks again.
        assert_eq!(walk(2, 3, 0, &[From(&[9]), Prev, Prev, Next, Next]), 1);
    }

    #[test]
    fn a_question_that_is_refused_leaves_the_cursor_nowhere() {
        let mut line = fake(2, 3, 2);
        line.refuses = true;
        let mut place = Place::default();
        place.go(&mut line, Seek::Last).expect("a seek");
        assert_eq!(place.key(), Some(&[5][..]));
        place
            .go(&mut line, Seek::Next)
            .expect("a step off the keys");
        assert_eq!((place.key(), line.asked), (None, 0));
        place
            .go(&mut line, Seek::Prev)
            .expect_err("the question is refused");
        assert_eq!((place.key(), line.asked), (None, 1));
        // The cursor stands nowhere, and a step asks nothing more.
        line.refuses = false;
        place
            .go(&mut line, Seek::Prev)
            .expect("a step from nowhere");
        assert_eq!((place.key(), line.asked), (None, 1));
        place.go(&mut line, Seek::Last).expect("a seek");
        assert_eq!(place.key(), Some(&[5][..]));

        // A seek and a step from a key that are refused leave it nowhere
        // too, and not on the key it stood on.
        for refused in [Seek::First, Seek::Back(&[9]), Seek::Next, Seek::Prev] {
            place.go(&mut line, Seek::From(&[3])).expect("a seek");
            assert_eq!(place.key(), Some(&[3][..]));
            line.refuses_moves = true;
            place
                .go(&mut line, refused)
                .expect_err("the move is refused");
            assert_eq!(place.key(), None, "{refused:?}");
            line.refuses_moves = false;
            place
                .go(&mut line, Seek::Next)
                .expect("a step from nowhere");
            assert_eq!((place.key(), line.asked), (None, 1), "{refused:?}");
        }
    }
}
