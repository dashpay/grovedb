use std::{
    cmp::Ordering,
    hash::{Hash, Hasher},
};

use storage::worst_case_costs::WorstKeyLength;
use visualize::{Drawer, Visualize};

use crate::batch::key_info::KeyInfo::{KnownKey, MaxKeySize};

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum KeyInfo {
    KnownKey(Vec<u8>),
    MaxKeySize { unique_id: Vec<u8>, max_size: u8 },
}

impl PartialOrd<Self> for KeyInfo {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match self.as_slice().partial_cmp(other.as_slice()) {
            None => None,
            Some(ord) => match ord {
                Ordering::Less => Some(Ordering::Less),
                Ordering::Equal => {
                    let other_len = other.len();
                    match self.len().partial_cmp(&other_len) {
                        None => Some(Ordering::Equal),
                        Some(ord) => Some(ord),
                    }
                }
                Ordering::Greater => Some(Ordering::Less),
            },
        }
    }
}

impl Ord for KeyInfo {
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}

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

impl WorstKeyLength for KeyInfo {
    fn len(&self) -> u8 {
        match self {
            Self::KnownKey(key) => key.len() as u8,
            Self::MaxKeySize { max_size, .. } => *max_size,
        }
    }
}

impl KeyInfo {
    pub fn as_slice(&self) -> &[u8] {
        match self {
            KnownKey(key) => key.as_slice(),
            MaxKeySize { unique_id, .. } => unique_id.as_slice(),
        }
    }

    pub fn get_key(self) -> Vec<u8> {
        match self {
            KnownKey(key) => key,
            MaxKeySize { unique_id, .. } => unique_id,
        }
    }

    pub fn get_key_clone(&self) -> Vec<u8> {
        match self {
            KnownKey(key) => key.clone(),
            MaxKeySize { unique_id, .. } => unique_id.clone(),
        }
    }
}

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
