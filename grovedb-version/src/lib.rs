use crate::version::GroveVersion;

pub mod version;
pub mod error;

#[macro_export]
macro_rules! check_v0 {
    ($method:expr, $version:expr) => {
        {
            const EXPECTED_VERSION: u16 = 0;
            if $version != EXPECTED_VERSION {
                return Err(GroveVersionError::UnknownVersionMismatch {
                    method: $method.to_string(),
                    known_versions: vec![EXPECTED_VERSION],
                    received: $version,
                }
                .into())
                .wrap_with_cost(OperationCost::default());
            }
        }
    };
}

pub trait TryFromVersioned<T>: Sized {
    /// The type returned in the event of a conversion error.
    type Error;

    /// Performs the conversion.
    fn try_from_versioned(value: T, grove_version: &GroveVersion) -> Result<Self, Self::Error>;
}