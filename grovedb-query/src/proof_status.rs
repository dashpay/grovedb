
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ProofStatus {
    pub limit: Option<u16>,
}

impl ProofStatus {
    pub fn hit_limit(&self) -> bool {
        self.limit.is_some() && self.limit.unwrap() == 0
    }
}

impl ProofStatus {
    fn new_with_limit(limit: Option<u16>) -> Self {
        Self { limit }
    }

    pub fn update_limit(mut self, new_limit: Option<u16>) -> Self {
        if let Some(new_limit) = new_limit {
            self.limit = Some(new_limit)
        }
        self
    }
}