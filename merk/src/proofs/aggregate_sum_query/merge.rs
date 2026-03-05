use crate::proofs::aggregate_sum_query::AggregateSumQuery;
use crate::Error;

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
                (Some(a), Some(b)) => Some(
                    a.checked_add(b)
                        .ok_or(Error::Overflow("Overflow when merging item check limits"))?,
                ),
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

        self.sum_limit = self
            .sum_limit
            .checked_add(other.sum_limit)
            .ok_or(Error::Overflow("Overflow when merging sum limits"))?;

        self.limit_of_items_to_check =
            match (self.limit_of_items_to_check, other.limit_of_items_to_check) {
                (Some(a), Some(b)) => Some(
                    a.checked_add(b)
                        .ok_or(Error::Overflow("Overflow when merging item check limits"))?,
                ),
                _ => None,
            };

        self.insert_items(other.items);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_multiple_empty_returns_noop() {
        let result = AggregateSumQuery::merge_multiple(vec![]).unwrap();
        assert_eq!(result.sum_limit, 0);
        assert_eq!(result.limit_of_items_to_check, Some(0));
    }

    #[test]
    fn merge_multiple_single_returns_same() {
        let q = AggregateSumQuery::new(42, Some(3));
        let result = AggregateSumQuery::merge_multiple(vec![q.clone()]).unwrap();
        assert_eq!(result.sum_limit, 42);
        assert_eq!(result.limit_of_items_to_check, Some(3));
    }

    #[test]
    fn merge_multiple_sums_limits() {
        let q1 = AggregateSumQuery::new(10, Some(2));
        let q2 = AggregateSumQuery::new(20, Some(3));
        let result = AggregateSumQuery::merge_multiple(vec![q1, q2]).unwrap();
        assert_eq!(result.sum_limit, 30);
        assert_eq!(result.limit_of_items_to_check, Some(5));
    }

    #[test]
    fn merge_multiple_direction_mismatch_errors() {
        let q1 = AggregateSumQuery::new(10, None);
        let q2 = AggregateSumQuery::new_descending(20, None);
        let err = AggregateSumQuery::merge_multiple(vec![q1, q2]).unwrap_err();
        assert!(err.to_string().contains("left_to_right"));
    }

    #[test]
    fn merge_multiple_sum_overflow_errors() {
        let q1 = AggregateSumQuery::new(u64::MAX, None);
        let q2 = AggregateSumQuery::new(1, None);
        assert!(AggregateSumQuery::merge_multiple(vec![q1, q2]).is_err());
    }

    #[test]
    fn merge_multiple_limit_overflow_errors() {
        let q1 = AggregateSumQuery::new(1, Some(u16::MAX));
        let q2 = AggregateSumQuery::new(1, Some(1));
        assert!(AggregateSumQuery::merge_multiple(vec![q1, q2]).is_err());
    }

    #[test]
    fn merge_multiple_none_limit_gives_none() {
        let q1 = AggregateSumQuery::new(10, None);
        let q2 = AggregateSumQuery::new(10, Some(5));
        let result = AggregateSumQuery::merge_multiple(vec![q1, q2]).unwrap();
        assert_eq!(result.limit_of_items_to_check, None);
    }

    #[test]
    fn merge_with_adds_limits() {
        let mut q1 = AggregateSumQuery::new(10, Some(2));
        let q2 = AggregateSumQuery::new(20, Some(3));
        q1.merge_with(q2).unwrap();
        assert_eq!(q1.sum_limit, 30);
        assert_eq!(q1.limit_of_items_to_check, Some(5));
    }

    #[test]
    fn merge_with_direction_mismatch_errors() {
        let mut q1 = AggregateSumQuery::new(10, None);
        let q2 = AggregateSumQuery::new_descending(20, None);
        assert!(q1.merge_with(q2).is_err());
    }

    #[test]
    fn merge_with_sum_overflow_errors() {
        let mut q1 = AggregateSumQuery::new(u64::MAX, None);
        let q2 = AggregateSumQuery::new(1, None);
        assert!(q1.merge_with(q2).is_err());
    }

    #[test]
    fn merge_with_limit_overflow_errors() {
        let mut q1 = AggregateSumQuery::new(1, Some(u16::MAX));
        let q2 = AggregateSumQuery::new(1, Some(1));
        assert!(q1.merge_with(q2).is_err());
    }

    #[test]
    fn merge_with_none_limit_gives_none() {
        let mut q1 = AggregateSumQuery::new(5, Some(3));
        let q2 = AggregateSumQuery::new(5, None);
        q1.merge_with(q2).unwrap();
        assert_eq!(q1.limit_of_items_to_check, None);
    }
}
