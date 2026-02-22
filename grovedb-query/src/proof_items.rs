
#[derive(Clone, Debug, Default)]
pub struct ProofItems<'a> {
    key_query_items: BTreeSet<&'a Vec<u8>>,
    range_query_items: Vec<RangeSetBorrowed<'a>>,
}

impl fmt::Display for ProofItems<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "ProofItems:\n  Key Queries: {:?}\n  Range Queries: [{}]\n",
            self.key_query_items
                .iter()
                .map(|b| format!("{:X?}", b))
                .collect::<Vec<_>>(),
            self.range_query_items
                .iter()
                .map(|r| format!("{}", r))
                .collect::<Vec<_>>()
                .join(", "),
        )
    }
}

impl<'a> ProofItems<'a> {
    pub fn new_with_query_items(
        query_items: &[QueryItem],
        left_to_right: bool,
    ) -> (ProofItems<'_>, ProofParams) {
        let mut key_query_items = BTreeSet::new();
        let mut range_query_items = vec![];
        for query_item in query_items {
            match query_item {
                QueryItem::Key(key) => {
                    key_query_items.insert(key);
                }
                query_item => {
                    // These are all ranges
                    range_query_items.push(
                        query_item
                            .to_range_set_borrowed()
                            .expect("all query items at this point should be ranges"),
                    );
                }
            }
        }
        let status = ProofItems {
            key_query_items,
            range_query_items,
        };
        let params = ProofParams { left_to_right };
        (status, params)
    }

    /// The point of process key is to take the current proof items that we have
    /// and split them left and right
    fn process_key(&'a self, key: &'a Vec<u8>) -> (bool, bool, ProofItems<'a>, ProofItems<'a>) {
        // 1) Partition the user’s key-based queries
        let mut left_key_query_items = BTreeSet::new();
        let mut right_key_query_items = BTreeSet::new();
        let mut item_is_present = false;
        let mut item_on_boundary = false;

        for &query_item_key in self.key_query_items.iter() {
            match query_item_key.cmp(key) {
                std::cmp::Ordering::Less => left_key_query_items.insert(query_item_key),
                std::cmp::Ordering::Greater => right_key_query_items.insert(query_item_key),
                std::cmp::Ordering::Equal => {
                    item_is_present = true;
                    false // `insert` returns a bool, but we don't use it here
                }
            };
        }
        // 2) Partition the user’s range-based queries
        let mut left_range_query_items = vec![];
        let mut right_range_query_items = vec![];
        for &range_set in self.range_query_items.iter() {
            if range_set.could_have_items_in_direction(key, Direction::LeftOf) {
                left_range_query_items.push(range_set)
            }

            if range_set.could_have_items_in_direction(key, Direction::RightOf) {
                right_range_query_items.push(range_set)
            }

            if !item_is_present {
                let key_containment_result = range_set.could_contain_key(key);
                item_is_present = key_containment_result.included;
                item_on_boundary |= key_containment_result.on_bounds_not_included;
            }
        }

        let left = ProofItems {
            key_query_items: left_key_query_items,
            range_query_items: left_range_query_items,
        };

        let right = ProofItems {
            key_query_items: right_key_query_items,
            range_query_items: right_range_query_items,
        };

        (item_is_present, item_on_boundary, left, right)
    }

    pub fn has_no_query_items(&self) -> bool {
        self.key_query_items.is_empty() && self.range_query_items.is_empty()
    }
}
