#[derive(Debug, thiserror::Error)]
pub enum Error {
    // Input data errors
    #[error("overflow error {0}")]
    Overflow(&'static str),

    #[error("divide by zero error {0}")]
    DivideByZero(&'static str),

    #[error("wrong estimated costs element type for level error {0}")]
    WrongEstimatedCostsElementTypeForLevel(&'static str),
}
