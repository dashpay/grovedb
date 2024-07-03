use std::fmt;
#[cfg(any(feature = "full", feature = "verify"))]
use std::io::Read;
#[cfg(feature = "full")]
use std::io::Write;

use grovedb_merk::{
    proofs::query::{Key, Path, ProvedKeyValue},
    CryptoHash,
};
#[cfg(any(feature = "full", feature = "verify"))]
use integer_encoding::{VarInt, VarIntReader};

#[cfg(any(feature = "full", feature = "verify"))]
pub type ProvedKeyValues = Vec<ProvedKeyValue>;

/// Proved path-key-values
pub type ProvedPathKeyValues = Vec<ProvedPathKeyValue>;

/// Proved path-key-value
#[cfg(any(feature = "full", feature = "verify"))]
#[derive(Debug, PartialEq, Eq)]
pub struct ProvedPathKeyValue {
    /// Path
    pub path: Path,
    /// Key
    pub key: Key,
    /// Value
    pub value: Vec<u8>,
    /// Proof
    pub proof: CryptoHash,
}

impl ProvedPathKeyValue {
    // TODO: make path a reference
    /// Consumes the ProvedKeyValue and returns a ProvedPathKeyValue given a
    /// Path
    pub fn from_proved_key_value(path: Path, proved_key_value: ProvedKeyValue) -> Self {
        Self {
            path,
            key: proved_key_value.key,
            value: proved_key_value.value,
            proof: proved_key_value.proof,
        }
    }

    /// Transforms multiple ProvedKeyValues to their equivalent
    /// ProvedPathKeyValue given a Path
    pub fn from_proved_key_values(path: Path, proved_key_values: ProvedKeyValues) -> Vec<Self> {
        proved_key_values
            .into_iter()
            .map(|pkv| Self::from_proved_key_value(path.clone(), pkv))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::query::ProvedKeyValue;

    use crate::operations::proof::util::ProvedPathKeyValue;

    #[test]
    fn test_proved_path_from_single_proved_key_value() {
        let path = vec![b"1".to_vec(), b"2".to_vec()];
        let proved_key_value = ProvedKeyValue {
            key: b"a".to_vec(),
            value: vec![5, 6],
            proof: [0; 32],
        };
        let proved_path_key_value =
            ProvedPathKeyValue::from_proved_key_value(path.clone(), proved_key_value);
        assert_eq!(
            proved_path_key_value,
            ProvedPathKeyValue {
                path,
                key: b"a".to_vec(),
                value: vec![5, 6],
                proof: [0; 32]
            }
        );
    }

    #[test]
    fn test_many_proved_path_from_many_proved_key_value() {
        let path = vec![b"1".to_vec(), b"2".to_vec()];
        let proved_key_value_a = ProvedKeyValue {
            key: b"a".to_vec(),
            value: vec![5, 6],
            proof: [0; 32],
        };
        let proved_key_value_b = ProvedKeyValue {
            key: b"b".to_vec(),
            value: vec![5, 7],
            proof: [1; 32],
        };
        let proved_key_value_c = ProvedKeyValue {
            key: b"c".to_vec(),
            value: vec![6, 7],
            proof: [2; 32],
        };
        let proved_key_values = vec![proved_key_value_a, proved_key_value_b, proved_key_value_c];
        let proved_path_key_values =
            ProvedPathKeyValue::from_proved_key_values(path.clone(), proved_key_values);
        assert_eq!(proved_path_key_values.len(), 3);
        assert_eq!(
            proved_path_key_values[0],
            ProvedPathKeyValue {
                path: path.clone(),
                key: b"a".to_vec(),
                value: vec![5, 6],
                proof: [0; 32]
            }
        );
        assert_eq!(
            proved_path_key_values[1],
            ProvedPathKeyValue {
                path: path.clone(),
                key: b"b".to_vec(),
                value: vec![5, 7],
                proof: [1; 32]
            }
        );
        assert_eq!(
            proved_path_key_values[2],
            ProvedPathKeyValue {
                path,
                key: b"c".to_vec(),
                value: vec![6, 7],
                proof: [2; 32]
            }
        );
    }
}
