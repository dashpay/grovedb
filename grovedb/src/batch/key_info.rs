//! Key info

#[cfg(feature = "full")]
use std::{
    cmp::Ordering,
    hash::{Hash, Hasher},
};

#[cfg(feature = "full")]
use grovedb_storage::worst_case_costs::WorstKeyLength;
#[cfg(feature = "full")]
use grovedb_visualize::{Drawer, Visualize};

#[cfg(feature = "full")]
use crate::batch::key_info::KeyInfo::{KnownKey, MaxKeySize};

/// Key info
#[cfg(feature = "full")]
#[derive(Clone, Eq, Debug)]
pub enum KeyInfo {
    /// Known key
    KnownKey(Vec<u8>),
    /// Max key size
    MaxKeySize {
        /// Unique ID
        unique_id: Vec<u8>,
        /// Max size
        max_size: u8,
    },
}

#[cfg(feature = "full")]
impl PartialEq for KeyInfo {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (KnownKey(..), MaxKeySize { .. }) | (MaxKeySize { .. }, KnownKey(..)) => false,
            (KnownKey(a), KnownKey(b)) => a == b,
            (
                MaxKeySize {
                    unique_id: unique_id_a,
                    max_size: max_size_a,
                },
                MaxKeySize {
                    unique_id: unique_id_b,
                    max_size: max_size_b,
                },
            ) => unique_id_a == unique_id_b && max_size_a == max_size_b,
        }
    }
}

impl PartialEq<Vec<u8>> for KeyInfo {
    fn eq(&self, other: &Vec<u8>) -> bool {
        if let KnownKey(key) = self {
            key == other
        } else {
            false
        }
    }
}

impl PartialEq<&[u8]> for KeyInfo {
    fn eq(&self, other: &&[u8]) -> bool {
        if let KnownKey(key) = self {
            key == other
        } else {
            false
        }
    }
}

#[cfg(feature = "full")]
impl PartialOrd<Self> for KeyInfo {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(feature = "full")]
impl Ord for KeyInfo {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.as_slice().cmp(other.as_slice()) {
            Ordering::Less => Ordering::Less,
            Ordering::Equal => {
                let other_len = other.max_length();
                self.max_length().cmp(&other_len)
            }
            Ordering::Greater => Ordering::Greater,
        }
    }
}

#[cfg(feature = "full")]
impl Hash for KeyInfo {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            KnownKey(k) => k.hash(state),
            MaxKeySize {
                unique_id,
                max_size,
            } => {
                unique_id.hash(state);
                max_size.hash(state);
            }
        }
    }
}

#[cfg(feature = "full")]
impl WorstKeyLength for KeyInfo {
    fn max_length(&self) -> u8 {
        match self {
            Self::KnownKey(key) => key.len() as u8,
            Self::MaxKeySize { max_size, .. } => *max_size,
        }
    }
}

#[cfg(feature = "full")]
impl KeyInfo {
    /// Return self as slice
    pub fn as_slice(&self) -> &[u8] {
        match self {
            KnownKey(key) => key.as_slice(),
            MaxKeySize { unique_id, .. } => unique_id.as_slice(),
        }
    }

    /// Return key
    pub fn get_key(self) -> Vec<u8> {
        match self {
            KnownKey(key) => key,
            MaxKeySize { unique_id, .. } => unique_id,
        }
    }

    /// Return clone of self
    pub fn get_key_clone(&self) -> Vec<u8> {
        match self {
            KnownKey(key) => key.clone(),
            MaxKeySize { unique_id, .. } => unique_id.clone(),
        }
    }
}

#[cfg(feature = "full")]
impl Visualize for KeyInfo {
    fn visualize<W: std::io::Write>(&self, mut drawer: Drawer<W>) -> std::io::Result<Drawer<W>> {
        match self {
            KnownKey(k) => {
                drawer.write(b"key: ")?;
                drawer = k.visualize(drawer)?;
            }
            MaxKeySize {
                unique_id,
                max_size,
            } => {
                drawer.write(b"max_size_key: ")?;
                drawer = unique_id.visualize(drawer)?;
                drawer.write(format!(", max_size: {max_size}").as_bytes())?;
            }
        }
        Ok(drawer)
    }
}
