//! Frozen offset-free Query wire and serde schema. Recursive query fields use
//! these types so a nested new query cannot accidentally use the new decoder.

use bincode::{
    de::Decoder,
    enc::write::Writer,
    error::{DecodeError, EncodeError},
    Decode, Encode,
};
use grovedb_query::{QueryItem, ReadMode};
use indexmap::IndexMap;

#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub items: Vec<QueryItem>,
    pub default_subquery_branch: SubqueryBranch,
    pub conditional_subquery_branches: Option<IndexMap<QueryItem, SubqueryBranch>>,
    pub left_to_right: bool,
    pub add_parent_tree_on_subquery: bool,
    pub read_mode: Option<Box<ReadMode>>,
    pub limit: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Encode)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SubqueryBranch {
    pub subquery_path: Option<Vec<Vec<u8>>>,
    pub subquery: Option<Box<Query>>,
}

const QUERY_V3_FLAG_READ_MODE: u8 = 1;
const QUERY_V3_FLAG_INSTANCE_LIMIT: u8 = 2;

impl<Context> Decode<Context> for SubqueryBranch {
    fn decode<D: Decoder<Context = Context>>(decoder: &mut D) -> Result<Self, DecodeError> {
        Ok(Self {
            subquery_path: Option::<Vec<Vec<u8>>>::decode(decoder)?,
            subquery: Option::<Box<Query>>::decode(decoder)?,
        })
    }
}

impl<Context> Decode<Context> for Query {
    fn decode<D: Decoder<Context = Context>>(decoder: &mut D) -> Result<Self, DecodeError> {
        let version = u8::decode(decoder)?;
        if !matches!(version, 1..=3) {
            return Err(DecodeError::Other("unsupported Query encoding version"));
        }
        let flags = if version == 3 {
            let flags = u8::decode(decoder)?;
            if flags & !3 != 0 {
                return Err(DecodeError::Other("unknown Query version 3 flags"));
            }
            if flags & QUERY_V3_FLAG_INSTANCE_LIMIT == 0 {
                return Err(DecodeError::Other("non-canonical Query version 3 encoding"));
            }
            flags
        } else {
            0
        };
        let items = Vec::<QueryItem>::decode(decoder)?;
        let default_subquery_branch = SubqueryBranch::decode(decoder)?;
        let conditional_subquery_branches = if u8::decode(decoder)? == 1 {
            let length = u64::decode(decoder)?;
            let mut branches = IndexMap::new();
            for _ in 0..length {
                branches.insert(
                    QueryItem::decode(decoder)?,
                    SubqueryBranch::decode(decoder)?,
                );
            }
            Some(branches)
        } else {
            None
        };
        let left_to_right = bool::decode(decoder)?;
        let add_parent_tree_on_subquery = bool::decode(decoder)?;
        let read_mode = if version == 2 || flags & QUERY_V3_FLAG_READ_MODE != 0 {
            Some(Box::new(ReadMode::decode(decoder)?))
        } else {
            None
        };
        let limit = if version == 3 {
            Some(u16::decode(decoder)?)
        } else {
            None
        };
        Ok(Self {
            items,
            default_subquery_branch,
            conditional_subquery_branches,
            left_to_right,
            add_parent_tree_on_subquery,
            read_mode,
            limit,
        })
    }
}

impl Encode for Query {
    fn encode<E: bincode::enc::Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError> {
        // Version byte — always the lowest version that can represent
        // this node, so every already-expressible query keeps its exact
        // historical bytes and old decoders fail closed on exactly the
        // queries they cannot execute and on nothing else:
        // 1 = plain (pre-read-mode layout, byte-for-byte), 2 = carries
        // a read mode, 3 = carries a per-instance limit (flags byte
        // says whether a read mode rides along).
        if self.limit.is_some() {
            3u8.encode(encoder)?;
            let mut flags = QUERY_V3_FLAG_INSTANCE_LIMIT;
            if self.read_mode.is_some() {
                flags |= QUERY_V3_FLAG_READ_MODE;
            }
            flags.encode(encoder)?;
        } else if self.read_mode.is_some() {
            2u8.encode(encoder)?;
        } else {
            1u8.encode(encoder)?;
        }

        // Encode the items vector
        self.items.encode(encoder)?;

        // Encode the default subquery branch
        self.default_subquery_branch.encode(encoder)?;

        // Encode the conditional subquery branches
        match &self.conditional_subquery_branches {
            Some(conditional_subquery_branches) => {
                encoder.writer().write(&[1])?; // Write a flag indicating presence of data
                                               // Encode the length of the map
                (conditional_subquery_branches.len() as u64).encode(encoder)?;
                // Encode each key-value pair in the IndexMap
                for (key, value) in conditional_subquery_branches {
                    key.encode(encoder)?;
                    value.encode(encoder)?;
                }
            }
            None => {
                encoder.writer().write(&[0])?; // Write a flag indicating
                                               // absence of data
            }
        }

        // Encode the left_to_right boolean
        self.left_to_right.encode(encoder)?;

        self.add_parent_tree_on_subquery.encode(encoder)?;

        // Versions 2 and 3 append the read mode when present. No
        // per-field presence flag: the version byte (v2) or the flags
        // byte (v3) already says it's there.
        if let Some(read_mode) = &self.read_mode {
            read_mode.encode(encoder)?;
        }

        // Version 3 appends the per-instance limit last; its presence
        // is what selected version 3 in the first place.
        if let Some(limit) = self.limit {
            limit.encode(encoder)?;
        }

        Ok(())
    }
}

#[cfg(feature = "serde")]
mod query_serde {
    use std::fmt;

    use serde::{
        de::{self, MapAccess, Visitor},
        Deserialize, Deserializer, Serialize, Serializer,
    };

    use super::*;

    fn wire_version(read_mode: bool, limit: bool) -> u8 {
        match (read_mode, limit) {
            (_, true) => 3,
            (true, false) => 2,
            (false, false) => 1,
        }
    }

    fn validate_canonical<E: de::Error>(
        version: u8,
        read_mode: bool,
        limit: bool,
    ) -> Result<(), E> {
        let expected = wire_version(read_mode, limit);
        if version != expected {
            return Err(E::custom(format!(
                "non-canonical Query serde version {version} for its contents (expected \
                 {expected})"
            )));
        }
        Ok(())
    }

    /// The positional frame sentinel. Chosen so a legacy unframed
    /// reader, which decodes the leading bytes as its `items` vector
    /// length, sees an absurd length and errors instead of
    /// reinterpreting the payload.
    const POSITIONAL_MAGIC: u64 = u64::from_be_bytes(*b"grvquery");

    /// Framed positional layout — magic, version, then every field
    /// always present.
    #[derive(Serialize)]
    struct PositionalRef<'a> {
        magic: u64,
        version: u8,
        items: &'a Vec<QueryItem>,
        default_subquery_branch: &'a SubqueryBranch,
        conditional_subquery_branches: &'a Option<IndexMap<QueryItem, SubqueryBranch>>,
        left_to_right: bool,
        add_parent_tree_on_subquery: bool,
        read_mode: &'a Option<Box<ReadMode>>,
        limit: &'a Option<u16>,
    }

    #[derive(Deserialize)]
    #[serde(rename = "Query")]
    struct PositionalOwned {
        magic: u64,
        version: u8,
        items: Vec<QueryItem>,
        default_subquery_branch: SubqueryBranch,
        conditional_subquery_branches: Option<IndexMap<QueryItem, SubqueryBranch>>,
        left_to_right: bool,
        add_parent_tree_on_subquery: bool,
        read_mode: Option<Box<ReadMode>>,
        limit: Option<u16>,
    }

    /// The nested body of the human-readable version-2 and version-3
    /// forms.
    #[derive(Serialize, Deserialize)]
    struct BodyOwned {
        items: Vec<QueryItem>,
        default_subquery_branch: SubqueryBranch,
        conditional_subquery_branches: Option<IndexMap<QueryItem, SubqueryBranch>>,
        left_to_right: bool,
        add_parent_tree_on_subquery: bool,
        read_mode: Option<Box<ReadMode>>,
        limit: Option<u16>,
    }

    #[derive(Serialize)]
    struct BodyRef<'a> {
        items: &'a Vec<QueryItem>,
        default_subquery_branch: &'a SubqueryBranch,
        conditional_subquery_branches: &'a Option<IndexMap<QueryItem, SubqueryBranch>>,
        left_to_right: bool,
        add_parent_tree_on_subquery: bool,
        read_mode: &'a Option<Box<ReadMode>>,
        limit: &'a Option<u16>,
    }

    impl Serialize for Query {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let version = wire_version(self.read_mode.is_some(), self.limit.is_some());
            if !serializer.is_human_readable() {
                return PositionalRef {
                    magic: POSITIONAL_MAGIC,
                    version,
                    items: &self.items,
                    default_subquery_branch: &self.default_subquery_branch,
                    conditional_subquery_branches: &self.conditional_subquery_branches,
                    left_to_right: self.left_to_right,
                    add_parent_tree_on_subquery: self.add_parent_tree_on_subquery,
                    read_mode: &self.read_mode,
                    limit: &self.limit,
                }
                .serialize(serializer);
            }
            use serde::ser::SerializeStruct;
            match version {
                2 | 3 => {
                    // Both non-released forms nest: the released
                    // derived reader ignores unknown flat keys, so
                    // either feature would silently vanish in a flat
                    // map.
                    let mut state = serializer.serialize_struct("Query", 2)?;
                    state.serialize_field("version", &version)?;
                    state.serialize_field(
                        "body",
                        &BodyRef {
                            items: &self.items,
                            default_subquery_branch: &self.default_subquery_branch,
                            conditional_subquery_branches: &self.conditional_subquery_branches,
                            left_to_right: self.left_to_right,
                            add_parent_tree_on_subquery: self.add_parent_tree_on_subquery,
                            read_mode: &self.read_mode,
                            limit: &self.limit,
                        },
                    )?;
                    state.end()
                }
                _ => {
                    let mut state = serializer.serialize_struct("Query", 6)?;
                    state.serialize_field("version", &version)?;
                    state.serialize_field("items", &self.items)?;
                    state.serialize_field(
                        "default_subquery_branch",
                        &self.default_subquery_branch,
                    )?;
                    state.serialize_field(
                        "conditional_subquery_branches",
                        &self.conditional_subquery_branches,
                    )?;
                    state.serialize_field("left_to_right", &self.left_to_right)?;
                    state.serialize_field(
                        "add_parent_tree_on_subquery",
                        &self.add_parent_tree_on_subquery,
                    )?;
                    state.end()
                }
            }
        }
    }

    struct QueryVisitor;

    impl<'de> Visitor<'de> for QueryVisitor {
        type Value = Query;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a versioned Query map")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Query, A::Error> {
            let mut version: Option<u8> = None;
            let mut body: Option<BodyOwned> = None;
            let mut items: Option<Vec<QueryItem>> = None;
            let mut default_subquery_branch: Option<SubqueryBranch> = None;
            let mut conditional_subquery_branches: Option<
                Option<IndexMap<QueryItem, SubqueryBranch>>,
            > = None;
            let mut left_to_right: Option<bool> = None;
            let mut add_parent_tree_on_subquery: Option<bool> = None;
            let mut read_mode: Option<Option<Box<ReadMode>>> = None;

            while let Some(key) = map.next_key::<String>()? {
                match key.as_str() {
                    "version" => version = Some(map.next_value()?),
                    "body" => body = Some(map.next_value()?),
                    "items" => items = Some(map.next_value()?),
                    "default_subquery_branch" => default_subquery_branch = Some(map.next_value()?),
                    "conditional_subquery_branches" => {
                        conditional_subquery_branches = Some(map.next_value()?)
                    }
                    "left_to_right" => left_to_right = Some(map.next_value()?),
                    "add_parent_tree_on_subquery" => {
                        add_parent_tree_on_subquery = Some(map.next_value()?)
                    }
                    "read_mode" => read_mode = Some(map.next_value()?),
                    other => {
                        // Unknown keys fail closed: silently ignoring a
                        // key is exactly how a limit was dropped by
                        // pre-versioning readers.
                        return Err(de::Error::unknown_field(
                            other,
                            &[
                                "version",
                                "body",
                                "items",
                                "default_subquery_branch",
                                "conditional_subquery_branches",
                                "left_to_right",
                                "add_parent_tree_on_subquery",
                                "read_mode",
                            ],
                        ));
                    }
                }
            }

            if let Some(body) = body {
                let version = version.ok_or_else(|| de::Error::missing_field("version"))?;
                if items.is_some()
                    || default_subquery_branch.is_some()
                    || conditional_subquery_branches.is_some()
                    || left_to_right.is_some()
                    || add_parent_tree_on_subquery.is_some()
                    || read_mode.is_some()
                {
                    return Err(de::Error::custom(
                        "a version-2 or version-3 Query nests every field under `body`; flat \
                         fields may not accompany it",
                    ));
                }
                validate_canonical::<A::Error>(
                    version,
                    body.read_mode.is_some(),
                    body.limit.is_some(),
                )?;
                if version < 2 {
                    return Err(de::Error::custom(
                        "a nested Query body requires version 2 or 3",
                    ));
                }
                return Ok(Query {
                    items: body.items,
                    default_subquery_branch: body.default_subquery_branch,
                    conditional_subquery_branches: body.conditional_subquery_branches,
                    left_to_right: body.left_to_right,
                    add_parent_tree_on_subquery: body.add_parent_tree_on_subquery,
                    read_mode: body.read_mode,
                    limit: body.limit,
                });
            }

            // The flat layout carries neither a read mode nor a limit
            // (both bump the query to the nested form); a `read_mode`
            // key in a flat map is refused rather than accepted, since
            // the released reader would drop it silently and the two
            // generations must not diverge on the same payload. A
            // missing `version` key is the released pre-versioning
            // layout — accepted for compatibility.
            if read_mode
                .as_ref()
                .is_some_and(|read_mode| read_mode.is_some())
            {
                return Err(de::Error::custom(
                    "a flat Query map may not carry a read mode; read-mode queries nest under \
                     `body` (version 2)",
                ));
            }
            let read_mode = read_mode.unwrap_or(None);
            if let Some(version) = version {
                validate_canonical::<A::Error>(version, false, false)?;
            }
            Ok(Query {
                items: items.ok_or_else(|| de::Error::missing_field("items"))?,
                default_subquery_branch: default_subquery_branch
                    .ok_or_else(|| de::Error::missing_field("default_subquery_branch"))?,
                conditional_subquery_branches: conditional_subquery_branches.unwrap_or(None),
                left_to_right: left_to_right
                    .ok_or_else(|| de::Error::missing_field("left_to_right"))?,
                add_parent_tree_on_subquery: add_parent_tree_on_subquery
                    .ok_or_else(|| de::Error::missing_field("add_parent_tree_on_subquery"))?,
                read_mode,
                limit: None,
            })
        }
    }

    impl<'de> Deserialize<'de> for Query {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Query, D::Error> {
            if !deserializer.is_human_readable() {
                let wire = PositionalOwned::deserialize(deserializer)?;
                if wire.magic != POSITIONAL_MAGIC {
                    return Err(de::Error::custom(
                        "positional Query payload does not carry the framed layout's magic \
                         sentinel",
                    ));
                }
                validate_canonical::<D::Error>(
                    wire.version,
                    wire.read_mode.is_some(),
                    wire.limit.is_some(),
                )?;
                return Ok(Query {
                    items: wire.items,
                    default_subquery_branch: wire.default_subquery_branch,
                    conditional_subquery_branches: wire.conditional_subquery_branches,
                    left_to_right: wire.left_to_right,
                    add_parent_tree_on_subquery: wire.add_parent_tree_on_subquery,
                    read_mode: wire.read_mode,
                    limit: wire.limit,
                });
            }
            deserializer.deserialize_map(QueryVisitor)
        }
    }
}
