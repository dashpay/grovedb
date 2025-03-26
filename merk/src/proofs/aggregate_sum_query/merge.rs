use crate::Error;
use crate::proofs::aggregate_sum_query::AggregateSumQuery;

impl AggregateSumQuery {
    pub fn merge_multiple(mut queries: Vec<AggregateSumQuery>) -> Result<Self, Error> {
        if queries.is_empty() {
            // We put sum 0 and limit 0 to represent a no-op query
            return Ok(AggregateSumQuery::new(0, Some(0)));
        }

        // Slight performance improvement via swap_remove
        let mut merged_query = queries.swap_remove(0);
        let mut aggregate_sum_limit = merged_query.sum_limit;
        let expected_left_to_right = merged_query.left_to_right;
        let mut merged_limit: Option<u16> = merged_query.limit_of_items_to_check;

        for query in queries {
            if query.left_to_right != expected_left_to_right {
                return Err(Error::NotSupported(
                    "Cannot merge queries with differing left_to_right values".to_string(),
                ));
            }

            aggregate_sum_limit = aggregate_sum_limit
                .checked_add(query.sum_limit)
                .ok_or(Error::Overflow("Overflow when merging sum limits"))?;

            merged_limit = match (merged_limit, query.limit_of_items_to_check) {
                (Some(a), Some(b)) => Some(a.checked_add(b).ok_or(Error::Overflow(
                    "Overflow when merging item check limits"
                ))?),
                _ => None, // if either is None, result is None
            };

            merged_query.insert_items(query.items);
        }

        merged_query.sum_limit = aggregate_sum_limit;
        merged_query.limit_of_items_to_check = merged_limit;

        Ok(merged_query)
    }

    pub fn merge_with(&mut self, other: AggregateSumQuery) -> Result<(), Error> {
        if self.left_to_right != other.left_to_right {
            return Err(Error::NotSupported(
                "Cannot merge queries with differing left_to_right values".to_string(),
            ));
        }

        self.sum_limit = self.sum_limit
            .checked_add(other.sum_limit)
            .ok_or(Error::Overflow("Overflow when merging sum limits"))?;

        self.limit_of_items_to_check = match (self.limit_of_items_to_check, other.limit_of_items_to_check) {
            (Some(a), Some(b)) => Some(a.checked_add(b).ok_or(Error::Overflow(
                "Overflow when merging item check limits"
            ))?),
            _ => None,
        };

        self.insert_items(other.items);

        Ok(())
    }
}
