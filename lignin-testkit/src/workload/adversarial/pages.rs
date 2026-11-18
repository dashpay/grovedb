//! Pages, budgets and rankings: every way a query stops early or starts
//! late.

use grovedb::{
    element::reference_path::ReferencePathType, AxisQuery, Element, PathQuery, Query,
    QueryItem as Item,
};
use grovedb_merk::proofs::query::IndexAxis;

use super::DOMAIN_PAGES;
use crate::workload::{
    build::{self, batch, child, path, sum, value},
    op::{Op, Path},
    rng::Rng,
    Features, Prepared, Stream,
};

const ROOT: &[u8] = b"pages";
const GROUPS: u8 = 4;
const BRANCHES: u8 = 3;
const RANKED: u8 = 5;

/// Offsets, limits, sum budgets and axis reads.
///
/// The trees are small and of every kind a query pages through differently:
///
/// - a normal tree and a tree of trees, where an offset reads past the rows
///   it skips, and where conditional subqueries pick a range per group;
/// - three trees that commit their count, where a page is proven and the
///   rows before it are skipped by count, without being read;
/// - trees that commit their count under a tree of groups, where each group
///   gives a page of its own;
/// - a sum tree, walked until a running total is reached, through the
///   reader of its own and as a read mode of a path query;
/// - indexed trees under several branches, read in the order of their
///   counts, sums and averages, one branch at a time and several at once.
///
/// Each round changes some rows, commits and runs every read the target
/// grove version serves, with a proof where it proves one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pagination {
    /// How many rows the paged trees start with.
    pub rows: u32,
    /// How many rounds of writes and reads to run.
    pub rounds: u32,
    /// What the target grove version can store.
    pub features: Features,
}

impl Default for Pagination {
    fn default() -> Self {
        Self {
            rows: 40,
            rounds: 4,
            features: Features::ALL,
        }
    }
}

/// The trees a page is skipped to by count.
const COUNTED: [&[u8]; 3] = [b"counted", b"counted-sums", b"both"];

fn row_key(row: u32) -> Vec<u8> {
    (row as u16 * 3).to_be_bytes().to_vec()
}

fn group_key(prefix: u8, group: u8) -> Vec<u8> {
    vec![prefix, b'0' + group]
}

struct Generator {
    config: Pagination,
    rng: Rng,
    root: Path,
    next_row: u32,
    ops: Vec<Op>,
}

impl Pagination {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(self.rows >= 8, "a page needs rows around it");
        assert!(self.rows < 10_000, "row keys are two bytes");
        assert!(
            self.rounds.saturating_mul(5) < self.rows,
            "every round removes a row of its own"
        );
        let mut generator = Generator {
            config: *self,
            rng: Rng::derive(seed, DOMAIN_PAGES, 0),
            root: path(&[ROOT]),
            next_row: self.rows,
            ops: Vec::new(),
        };
        generator.setup();
        for round in 0..self.rounds {
            generator.change(round);
            generator.ops.push(Op::Commit);
            generator.plain_pages();
            generator.counted_pages();
            generator.group_pages();
            generator.budgets();
            generator.rankings();
        }
        Prepared::stream(generator.ops)
    }
}

impl Generator {
    fn tree(&self, key: &[u8]) -> Path {
        child(&self.root, key)
    }

    /// The element a row of the tree at `key` holds.
    fn row(&mut self, key: &[u8]) -> Element {
        match key {
            b"counted-sums" | b"both" => Element::new_sum_item(sum(&mut self.rng)),
            _ => Element::new_item(value(&mut self.rng, 1, 40)),
        }
    }

    fn setup(&mut self) {
        let root = self.root.clone();
        self.ops.extend([
            build::insert(&[], ROOT, Element::empty_tree()),
            build::insert(&root, b"plain", Element::empty_tree()),
            build::insert(&root, b"groups", Element::empty_tree()),
            build::insert(&root, b"counted", Element::empty_provable_count_tree()),
            build::insert(
                &root,
                b"counted-sums",
                Element::empty_provable_count_sum_tree(),
            ),
            build::insert(
                &root,
                b"both",
                Element::empty_provable_count_provable_sum_tree(),
            ),
            build::insert(&root, b"by-group", Element::empty_tree()),
            build::insert(&root, b"sums", Element::empty_sum_tree()),
            build::insert(&root, b"ranked", Element::empty_tree()),
        ]);
        for key in [b"plain".as_slice()].into_iter().chain(COUNTED) {
            let tree = self.tree(key);
            for row in 0..self.config.rows {
                let element = self.row(key);
                self.ops.push(build::insert(&tree, &row_key(row), element));
            }
        }
        for group in 0..GROUPS {
            let groups = self.tree(b"groups");
            let by_group = self.tree(b"by-group");
            let (plain, counted) = (group_key(b'g', group), group_key(b'h', group));
            self.ops
                .push(build::insert(&groups, &plain, Element::empty_tree()));
            self.ops.push(build::insert(
                &by_group,
                &counted,
                Element::empty_provable_count_tree(),
            ));
            for row in 0..8 + u32::from(group) {
                for tree in [child(&groups, &plain), child(&by_group, &counted)] {
                    let element = Element::new_item(value(&mut self.rng, 1, 24));
                    self.ops.push(build::insert(&tree, &row_key(row), element));
                }
            }
        }
        self.ops.push(Op::Commit);

        let sums = self.tree(b"sums");
        for row in 0..self.config.rows {
            let key = row_key(row);
            let element = match row % 7 {
                3 => Element::new_item(value(&mut self.rng, 1, 20)),
                5 => Element::new_item_with_sum_item(
                    value(&mut self.rng, 1, 20),
                    self.rng.between(1, 500) as i64,
                ),
                6 => Element::new_reference(ReferencePathType::SiblingReference(row_key(row - 6))),
                _ => Element::new_sum_item(self.rng.between(0, 1_200) as i64 - 200),
            };
            self.ops.push(build::insert(&sums, &key, element));
        }
        self.ops.push(Op::Commit);

        let ranked = self.tree(b"ranked");
        for branch in 0..BRANCHES {
            let branch_path = child(&ranked, &group_key(b'b', branch));
            self.ops.push(Op::Batch(vec![
                batch::insert_only(&ranked, &group_key(b'b', branch), Element::empty_tree()),
                batch::insert_only(
                    &branch_path,
                    b"by-count",
                    Element::empty_provable_count_indexed_tree(),
                ),
                batch::insert_only(
                    &branch_path,
                    b"by-both",
                    Element::ProvableCountProvableSumIndexedTree(
                        None,
                        0,
                        0,
                        vec![(0, None), (1, None), (2, None)],
                        None,
                    ),
                ),
            ]));
            self.ops.push(Op::Commit);
            for entry in 0..RANKED {
                let key = group_key(b'e', entry);
                let by_count = child(&branch_path, b"by-count");
                let by_both = child(&branch_path, b"by-both");
                self.ops.push(Op::Batch(vec![
                    batch::insert_only(&by_count, &key, Element::empty_count_tree()),
                    batch::insert_only(&by_both, &key, Element::empty_count_sum_tree()),
                ]));
                let rows = self.rng.between(1, 6) as u32;
                let mut fill = Vec::new();
                for row in 0..rows {
                    fill.push(batch::insert_only(
                        &child(&by_count, &key),
                        &row_key(row),
                        Element::new_item(value(&mut self.rng, 1, 16)),
                    ));
                    fill.push(batch::insert_only(
                        &child(&by_both, &key),
                        &row_key(row),
                        Element::new_sum_item(self.rng.between(1, 900) as i64),
                    ));
                }
                self.ops.push(Op::Batch(fill));
            }
            self.ops.push(Op::Commit);
        }
    }

    /// Adds a row to and removes a row from the paged trees, and adds to
    /// one ranked entry so the rankings move.
    fn change(&mut self, round: u32) {
        let added = row_key(self.next_row);
        self.next_row += 1;
        let removed = row_key(round * 5 + 1);
        for key in [b"plain".as_slice()].into_iter().chain(COUNTED) {
            let tree = self.tree(key);
            let element = self.row(key);
            self.ops.push(Op::Batch(vec![
                batch::insert_only(&tree, &added, element),
                batch::delete(&tree, &removed),
            ]));
        }
        let sums = self.tree(b"sums");
        self.ops.push(build::insert(
            &sums,
            &added,
            Element::new_sum_item(self.rng.between(1, 2_000) as i64),
        ));
        let branch = group_key(b'b', (round % u32::from(BRANCHES)) as u8);
        let entry = group_key(b'e', self.rng.below(u64::from(RANKED)) as u8);
        let branch_path = child(&self.tree(b"ranked"), &branch);
        self.ops.push(Op::Batch(vec![
            batch::insert_only(
                &child(&child(&branch_path, b"by-count"), &entry),
                &added,
                Element::new_item(value(&mut self.rng, 1, 16)),
            ),
            batch::insert_only(
                &child(&child(&branch_path, b"by-both"), &entry),
                &added,
                Element::new_sum_item(self.rng.between(1, 900) as i64),
            ),
        ]));
    }

    /// One range over the rows of a paged tree.
    fn range(&mut self) -> Item {
        let low = row_key(self.rng.below(u64::from(self.config.rows) / 2) as u32);
        let high = row_key(self.rng.between(
            u64::from(self.config.rows) / 2,
            u64::from(self.config.rows) + 4,
        ) as u32);
        match self.rng.below(6) {
            0 => Item::RangeFull(..),
            1 => Item::Range(low..high),
            2 => Item::RangeInclusive(low..=high),
            3 => Item::RangeFrom(low..),
            4 => Item::RangeToInclusive(..=high),
            _ => Item::RangeAfterTo(low..high),
        }
    }

    /// A single-range query in a random direction.
    fn ranged(&mut self) -> Query {
        let item = self.range();
        Query::new_single_query_item_with_direction(item, self.rng.chance(2, 3))
    }

    /// Offsets that read past what they skip: never proven. With a subquery
    /// they go to the readers that return rows, as `run_path_query` refuses
    /// them.
    fn plain_pages(&mut self) {
        let plain = self.tree(b"plain");
        let groups = self.tree(b"groups");
        let rows = u64::from(self.config.rows);
        for offset in [1, self.rng.between(2, rows / 2), rows + 10] {
            let query = self.ranged();
            let limit = self.rng.chance(3, 4).then_some(5);
            self.ops.push(build::unproven(build::sized(
                &plain,
                query,
                limit,
                Some(offset as u16),
            )));
        }

        let mut all_groups = Query::new_range_full();
        all_groups.set_subquery(self.ranged());
        let offset = Some(self.rng.between(1, 20) as u16);
        self.ops.push(build::rows(
            build::sized(&groups, all_groups, Some(7), offset),
            self.rng.chance(1, 2),
        ));

        let mut by_group = Query::new_range_full();
        by_group.set_subquery(Query::new_range_full());
        by_group.add_conditional_subquery(
            Item::Key(group_key(b'g', 1)),
            None,
            Some(Query::new_single_query_item(Item::RangeTo(..row_key(3)))),
        );
        by_group.add_conditional_subquery(
            Item::RangeFrom(group_key(b'g', 2)..),
            None,
            Some(Query::new_single_query_item_with_direction(
                Item::RangeAfter(row_key(4)..),
                false,
            )),
        );
        self.ops.push(build::proven(build::sized(
            &groups,
            by_group.clone(),
            Some(12),
            None,
        )));
        self.ops.push(build::rows(
            build::sized(&groups, by_group, Some(6), Some(4)),
            true,
        ));
    }

    /// Pages of the trees that commit their count: proven where the target
    /// proves by count, and a plain offset elsewhere.
    fn counted_pages(&mut self) {
        let proven = self.config.features.v1_proofs;
        let rows = u64::from(self.config.rows);
        for key in COUNTED {
            let tree = self.tree(key);
            for offset in [self.rng.between(1, rows / 2), rows + 10] {
                let query = self.ranged();
                let limit = Some(self.rng.between(1, 6) as u16);
                self.ops.push(build::path_query_read(
                    build::sized(&tree, query, limit, Some(offset as u16)),
                    proven,
                ));
            }
        }
    }

    /// A limit and a page of its own for each group.
    fn group_pages(&mut self) {
        let features = self.config.features;
        let by_group = self.tree(b"by-group");
        if features.instance_limits {
            let mut first_rows = Query::new_range_full();
            first_rows.limit = Some(self.rng.between(1, 3) as u16);
            let mut groups = Query::new_range_full();
            groups.set_subquery(first_rows);
            self.ops
                .push(build::proven(build::sized(&by_group, groups, None, None)));
        }
        if features.instance_offsets {
            let mut page = self.ranged();
            page.offset = Some(self.rng.between(1, 9) as u16);
            page.limit = Some(self.rng.between(1, 4) as u16);
            let mut groups = Query::new();
            groups.insert_key(group_key(b'h', 0));
            groups.insert_key(group_key(
                b'h',
                self.rng.between(1, u64::from(GROUPS) - 1) as u8,
            ));
            groups.set_subquery(page);
            self.ops
                .push(build::proven(build::sized(&by_group, groups, None, None)));
        }
    }

    /// Walks over sums that stop at a running total.
    fn budgets(&mut self) {
        let sums = self.tree(b"sums");
        for _ in 0..2 {
            let left_to_right = self.rng.chance(1, 2);
            let sum_limit = self.rng.between(200, 6_000);
            let limit = self.rng.chance(1, 2).then(|| self.rng.between(1, 8) as u16);
            let item = self.range();
            self.ops.push(build::sum_budget(
                &sums,
                vec![item.clone()],
                left_to_right,
                sum_limit,
                limit,
            ));
            if self.config.features.read_modes {
                self.ops.push(build::proven(PathQuery::new_sum_budget(
                    sums.clone(),
                    vec![item],
                    left_to_right,
                    sum_limit,
                    limit,
                )));
            }
        }
    }

    /// Reads of indexed trees in the order of an aggregate.
    fn rankings(&mut self) {
        if !self.config.features.read_modes {
            return;
        }
        let ranked = self.tree(b"ranked");
        let branch = group_key(b'b', self.rng.below(u64::from(BRANCHES)) as u8);
        let by_both = child(&child(&ranked, &branch), b"by-both");
        let descending = self.rng.chance(1, 2);
        let page = self.rng.between(1, 3) as u16;
        self.ops.push(build::proven(PathQuery::new_branched_axis(
            ranked,
            vec![
                group_key(b'b', 0),
                group_key(b'b', BRANCHES - 1),
                group_key(b'b', BRANCHES + 3),
            ],
            vec![b"by-count".to_vec()],
            AxisQuery::top_k(IndexAxis::Count, page, 0, descending),
        )));
        self.ops.push(build::proven(PathQuery::new_axis_top_k(
            by_both.clone(),
            IndexAxis::Sum,
            page,
            self.rng.below(3),
            descending,
        )));
        self.ops.push(build::proven(PathQuery::new_axis_bounded(
            by_both.clone(),
            IndexAxis::Avg,
            0,
            i128::from(self.rng.between(200, 900)),
            4,
            descending,
        )));
        self.ops.push(build::proven(PathQuery::new_axis(
            by_both,
            AxisQuery::bottom_k(IndexAxis::Count, page, 1).keys_only(),
        )));
    }
}
