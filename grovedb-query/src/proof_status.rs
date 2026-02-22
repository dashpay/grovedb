/// Tracks proof generation status, primarily the remaining result limit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ProofStatus {
    /// The remaining number of results allowed, or `None` for unlimited.
    pub limit: Option<u16>,
}

impl ProofStatus {
    /// Returns `true` if the limit has been reached (equals zero).
    pub fn hit_limit(&self) -> bool {
        self.limit.is_some() && self.limit.unwrap() == 0
    }
}

impl ProofStatus {
    /// Creates a new `ProofStatus` with the given limit.
    pub fn new_with_limit(limit: Option<u16>) -> Self {
        Self { limit }
    }

    /// Returns a new `ProofStatus` with the limit updated to `new_limit` if
    /// provided.
    pub fn update_limit(mut self, new_limit: Option<u16>) -> Self {
        if let Some(new_limit) = new_limit {
            self.limit = Some(new_limit)
        }
        self
    }
}
