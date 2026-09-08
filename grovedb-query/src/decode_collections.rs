//! Allocation policies for the manual Query wire schema.
//! Chosen explicitly by the Decode or DecodeUntrusted parser, never by decoder state.
use crate::{QueryItem, SubqueryBranch};
use bincode::{
    de::{Decoder, UntrustedDecoder},
    error::DecodeError,
};
use indexmap::IndexMap;

pub(crate) struct Ordinary;
pub(crate) struct Untrusted;

fn check_length(len: usize, max: usize, error: &'static str) -> Result<usize, DecodeError> {
    if len > max {
        Err(DecodeError::Other(error))
    } else {
        Ok(len)
    }
}

impl Ordinary {
    pub(crate) fn length(len: u64, max: usize, error: &'static str) -> Result<usize, DecodeError> {
        check_length(len as usize, max, error)
    }
    pub(crate) fn items<D: Decoder>(
        decoder: &mut D,
        len: usize,
        mut read: impl FnMut(&mut D) -> Result<QueryItem, DecodeError>,
    ) -> Result<Vec<QueryItem>, DecodeError> {
        let mut items = Vec::with_capacity(len);
        for _ in 0..len {
            items.push(read(decoder)?);
        }
        Ok(items)
    }
    pub(crate) fn branches<D: Decoder>(
        decoder: &mut D,
        len: usize,
        mut read: impl FnMut(&mut D) -> Result<(QueryItem, SubqueryBranch), DecodeError>,
    ) -> Result<IndexMap<QueryItem, SubqueryBranch>, DecodeError> {
        let mut map = IndexMap::with_capacity(len);
        for _ in 0..len {
            let (key, value) = read(decoder)?;
            map.insert(key, value);
        }
        Ok(map)
    }
}

impl Untrusted {
    pub(crate) fn length(len: u64, max: usize, error: &'static str) -> Result<usize, DecodeError> {
        let len = usize::try_from(len).map_err(|_| DecodeError::LimitExceeded)?;
        check_length(len, max, error)
    }
    pub(crate) fn items<D: UntrustedDecoder>(
        decoder: &mut D,
        len: usize,
        mut read: impl FnMut(&mut D) -> Result<QueryItem, DecodeError>,
    ) -> Result<Vec<QueryItem>, DecodeError> {
        decoder.claim_container_read::<QueryItem>(len)?;
        let mut items = Vec::new();
        for _ in 0..len {
            decoder.unclaim_bytes_read(std::mem::size_of::<QueryItem>());
            let item = read(decoder)?;
            items
                .try_reserve(1)
                .map_err(|_| DecodeError::LimitExceeded)?;
            items.push(item);
        }
        Ok(items)
    }
    pub(crate) fn branches<D: UntrustedDecoder>(
        decoder: &mut D,
        len: usize,
        mut read: impl FnMut(&mut D) -> Result<(QueryItem, SubqueryBranch), DecodeError>,
    ) -> Result<IndexMap<QueryItem, SubqueryBranch>, DecodeError> {
        decoder.claim_container_read::<(QueryItem, SubqueryBranch)>(len)?;
        let mut map = IndexMap::new();
        for _ in 0..len {
            decoder.unclaim_bytes_read(std::mem::size_of::<(QueryItem, SubqueryBranch)>());
            let (key, value) = read(decoder)?;
            map.try_reserve(1).map_err(|_| DecodeError::LimitExceeded)?;
            map.insert(key, value);
        }
        Ok(map)
    }
}
