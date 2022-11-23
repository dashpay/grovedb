#[derive(Debug, thiserror::Error)]
pub enum Error {
    // Input data errors
    #[error("overflow error {0}")]
    Overflow(&'static str),
}
