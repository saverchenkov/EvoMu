//! `eve-proto` — Dual-protocol wire representation for `EvoMu`.
//!
//! Supports both:
//! 1. **Bridge Mode (`BridgeEnvelope`)**: Clean MessagePack/JSON RPC & Event framing for
//!    clients running the `evomu_bridge.py` transport shim.
//! 2. **Legacy `eveMarshal` Mode (`PyRep`)**: Conformant binary encoder/decoder for the
//!    marshal wire format (`0x01..0x2F` opcodes, `zlib` streams,
//!    trailing `save_count` storage-index map, and node-budget decompression bomb protection).

#![forbid(unsafe_code)]

pub mod dbrow;
pub mod marshal;
pub mod quasar;

pub use dbrow::{
    dbtype_bit_width, decode_rle, encode_rle, extract_dbrow_col_defs,
    extract_dbrow_header_col_defs, pack_packed_row_columns, pack_packed_row_columns_from_defs,
    try_unpack_packed_row_columns, unpack_packed_row_columns, DBRowDescriptor, DbColumnType,
};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

/// Maximum allowed wire frame size (4 MiB) to prevent memory exhaustion on untrusted input.
pub const MAX_FRAME_SIZE: usize = 4 * 1024 * 1024;
/// Maximum collection element count decoded from a single container opcode.
pub const MAX_COLLECTION_LEN: usize = 32_768;
/// Maximum recursion depth for nested `PyRep` containers and sub-streams.
pub const MAX_RECURSION_DEPTH: usize = 64;
/// Maximum total AST nodes constructed during a single `marshal::decode` call (stops reference bombs).
pub const MAX_DECODED_NODES: usize = 65_536;

#[derive(Debug, Error, PartialEq)]
pub enum ProtoError {
    #[error("Unexpected EOF while decoding marshal stream")]
    UnexpectedEof,
    #[error("Frame length {0} bytes exceeds maximum allowed ({MAX_FRAME_SIZE} bytes)")]
    FrameTooLarge(usize),
    #[error("Collection length {0} exceeds maximum allowed ({MAX_COLLECTION_LEN})")]
    CollectionTooLarge(usize),
    #[error("Maximum recursion depth ({MAX_RECURSION_DEPTH}) exceeded")]
    RecursionLimitExceeded,
    #[error("Maximum decoded AST node budget ({MAX_DECODED_NODES}) exceeded (reference bomb)")]
    NodeBudgetExceeded,
    #[error("Trailing {0} unconsumed byte(s) at end of marshal stream")]
    TrailingBytes(usize),
    #[error("Invalid marshal header byte: 0x{0:02x} (expected 0x7e '~' or 0x78 zlib)")]
    InvalidHeader(u8),
    #[error("Unsupported marshal opcode: 0x{0:02x}")]
    UnsupportedOpcode(u8),
    #[error("Invalid saved stream reference index: {0}")]
    InvalidSaveReference(usize),
    #[error("Invalid or corrupt PyPackedRow RLE buffer")]
    InvalidPackedRowRle,
    #[error("Zlib decompression failed: {0}")]
    ZlibError(String),
    #[error("Invalid UTF-8 string in stream: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("MessagePack encode error: {0}")]
    MsgPackEncode(String),
    #[error("MessagePack decode error: {0}")]
    MsgPackDecode(String),
    #[error("JSON serialization error: {0}")]
    Json(String),
}

impl From<rmp_serde::encode::Error> for ProtoError {
    fn from(e: rmp_serde::encode::Error) -> Self {
        Self::MsgPackEncode(e.to_string())
    }
}

impl From<rmp_serde::decode::Error> for ProtoError {
    fn from(e: rmp_serde::decode::Error) -> Self {
        Self::MsgPackDecode(e.to_string())
    }
}

impl From<serde_json::Error> for ProtoError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e.to_string())
    }
}

/// Core algebraic representation of an EVE Online network value (`PyRep`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "$py", content = "val")]
pub enum PyRep {
    None,
    Bool(bool),
    Integer(i64),
    Real(f64),
    /// Raw buffer (`Op_PyBuffer` `0x0D`).
    Buffer(Vec<u8>),
    /// Arbitrary 8-bit byte string (`Op_PyEmptyString` `0x0E`, `Op_PyCharString` `0x0F`,
    /// `Op_PyShortString` `0x10`, `Op_PyLongString` `0x13`).
    Bytes(Vec<u8>),
    /// Python class/global token (`Op_PyToken` `0x02`, e.g., `"util.KeyVal"`).
    Token(String),
    /// Unicode UTF-8 string (`Op_PyEmptyWString` `0x28`, `Op_PyWStringUTF8` `0x2E`).
    String(String),
    Tuple(Vec<PyRep>),
    List(Vec<PyRep>),
    Dict(Vec<(PyRep, PyRep)>),
    /// High-level `util.KeyVal` projection.
    KeyVal(HashMap<String, PyRep>),
    /// High-level `util.Rowset` projection (`columns` + `rows`).
    Rowset {
        columns: Vec<String>,
        rows: Vec<Vec<PyRep>>,
    },
    /// High-level `util.IndexRowset` projection (`key_column`, `columns`, `items`).
    IndexRowset {
        key_column: String,
        columns: Vec<String>,
        items: Vec<(PyRep, Vec<PyRep>)>,
    },
    /// High-level `TupleSet` projection (`(columns, rows)`).
    TupleSet {
        columns: Vec<String>,
        rows: Vec<Vec<PyRep>>,
    },
    /// High-level `blue.DBRow` / `PyPackedRow` (`0x2A`) projection.
    DBRow {
        header: Box<PyRep>,
        columns: Vec<(String, PyRep)>,
        raw_rle: Vec<u8>,
    },
    /// Embedded Python `cPickle` payload (`Op_PyPicked` `0x21`).
    Picked(Vec<u8>),
    /// Legacy `PyObject` (`Op_PyObject` `0x17`: `type_name` token + `args`).
    Object {
        type_name: String,
        args: Box<PyRep>,
    },
    /// Legacy `PyObjectEx` (`Op_PyObjectEx1` `0x22` / `Op_PyObjectEx2` `0x23`).
    ObjectEx {
        is_type_2: bool,
        header: Box<PyRep>,
        list: Vec<PyRep>,
        dict: Vec<(PyRep, PyRep)>,
    },
    /// Nested sub-struct (`Op_PySubStruct` `0x19`).
    SubStruct(Box<PyRep>),
    /// Checksummed stream (`Op_PyChecksumedStream` `0x1C`).
    ChecksumedStream {
        checksum: u32,
        body: Box<PyRep>,
    },
    /// Nested marshaled sub-stream (`Op_PySubStream` `0x2B`).
    SubStream(Box<PyRep>),
}

impl PyRep {
    pub fn ascii_bytes(s: impl AsRef<[u8]>) -> Self {
        Self::Bytes(s.as_ref().to_vec())
    }

    pub fn keyval(entries: impl IntoIterator<Item = (impl Into<String>, PyRep)>) -> Self {
        Self::KeyVal(entries.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    pub fn rowset(columns: Vec<&str>, rows: Vec<Vec<PyRep>>) -> Self {
        Self::Rowset {
            columns: columns.into_iter().map(String::from).collect(),
            rows,
        }
    }

    pub fn index_rowset(
        key_column: &str,
        columns: Vec<&str>,
        items: Vec<(PyRep, Vec<PyRep>)>,
    ) -> Self {
        Self::IndexRowset {
            key_column: key_column.to_string(),
            columns: columns.into_iter().map(String::from).collect(),
            items,
        }
    }

    pub fn tuple_set(columns: Vec<&str>, rows: Vec<Vec<PyRep>>) -> Self {
        Self::TupleSet {
            columns: columns.into_iter().map(String::from).collect(),
            rows,
        }
    }

    pub fn crowset(descriptor: &DBRowDescriptor, rows: Vec<Vec<PyRep>>) -> Self {
        descriptor.build_crowset(rows)
    }

    pub fn cindexed_rowset(
        descriptor: &DBRowDescriptor,
        key_column: &str,
        rows: Vec<Vec<PyRep>>,
    ) -> Self {
        descriptor.build_cindexed_rowset(key_column, rows)
    }

    pub fn is_crowset(&self) -> bool {
        if let Self::ObjectEx {
            is_type_2: true,
            header,
            ..
        } = self
        {
            if let Self::Tuple(items) = header.as_ref() {
                if let Some(Self::Tuple(args)) = items.first() {
                    return args.first().and_then(Self::as_str) == Some("dbutil.CRowset");
                }
            }
        }
        false
    }

    pub fn is_cindexed_rowset(&self) -> bool {
        if let Self::ObjectEx {
            is_type_2: true,
            header,
            ..
        } = self
        {
            if let Self::Tuple(items) = header.as_ref() {
                if let Some(Self::Tuple(args)) = items.first() {
                    return args.first().and_then(Self::as_str) == Some("dbutil.CIndexedRowset");
                }
            }
        }
        false
    }

    pub fn crowset_rows(&self) -> Option<&[PyRep]> {
        if self.is_crowset() {
            if let Self::ObjectEx { list, .. } = self {
                return Some(list.as_slice());
            }
        }
        None
    }

    pub fn cindexed_rowset_entries(&self) -> Option<&[(PyRep, PyRep)]> {
        if self.is_cindexed_rowset() {
            if let Self::ObjectEx { dict, .. } = self {
                return Some(dict.as_slice());
            }
        }
        None
    }

    pub fn dbrow_get(&self, col_name: &str) -> Option<&PyRep> {
        if let Self::DBRow { columns, .. } = self {
            columns
                .iter()
                .find(|(n, _)| n == col_name)
                .map(|(_, v)| v)
        } else {
            None
        }
    }

    pub fn dbrow_columns(&self) -> Option<&[(String, PyRep)]> {
        if let Self::DBRow { columns, .. } = self {
            Some(columns.as_slice())
        } else {
            None
        }
    }

    pub fn row_count(&self) -> Option<usize> {
        match self {
            Self::Rowset { rows, .. } | Self::TupleSet { rows, .. } => Some(rows.len()),
            Self::IndexRowset { items, .. } => Some(items.len()),
            Self::ObjectEx { list, dict, .. } if self.is_crowset() => Some(list.len()),
            Self::ObjectEx { dict, .. } if self.is_cindexed_rowset() => Some(dict.len()),
            Self::List(items) => Some(items.len()),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) | Self::Token(s) => Some(s.as_str()),
            Self::Bytes(b) => std::str::from_utf8(b).ok(),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(v) => Some(*v),
            Self::Bool(b) => Some(if *b { 1 } else { 0 }),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Real(v) => Some(*v),
            Self::Integer(v) => Some(*v as f64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            Self::Integer(v) => Some(*v != 0),
            _ => None,
        }
    }

    /// Counts total AST and byte-weight units in this `PyRep` subtree (used to enforce `MAX_DECODED_NODES`
    /// and prevent saved-stream reference memory amplification DoS).
    pub fn node_count(&self) -> usize {
        match self {
            Self::None | Self::Bool(_) | Self::Integer(_) | Self::Real(_) => 1,
            Self::Buffer(b) | Self::Bytes(b) | Self::Picked(b) => 1 + (b.len() / 64),
            Self::Token(s) | Self::String(s) => 1 + (s.len() / 64),
            Self::Tuple(v) | Self::List(v) => 1 + v.iter().map(Self::node_count).sum::<usize>(),
            Self::Dict(entries) => {
                1 + entries
                    .iter()
                    .map(|(k, v)| k.node_count() + v.node_count())
                    .sum::<usize>()
            }
            Self::KeyVal(map) => 1 + map.values().map(Self::node_count).sum::<usize>(),
            Self::Rowset { columns, rows } | Self::TupleSet { columns, rows } => {
                1 + columns.len()
                    + rows
                        .iter()
                        .flat_map(|r| r.iter())
                        .map(Self::node_count)
                        .sum::<usize>()
            }
            Self::IndexRowset {
                columns, items, ..
            } => {
                1 + columns.len()
                    + items
                        .iter()
                        .map(|(k, r)| k.node_count() + r.iter().map(Self::node_count).sum::<usize>())
                        .sum::<usize>()
            }
            Self::DBRow {
                header,
                columns,
                raw_rle,
            } => {
                1 + (raw_rle.len() / 64)
                    + header.node_count()
                    + columns
                        .iter()
                        .map(|(_, v)| v.node_count())
                        .sum::<usize>()
            }
            Self::Object { args, .. }
            | Self::SubStruct(args)
            | Self::SubStream(args)
            | Self::ChecksumedStream { body: args, .. } => 1 + args.node_count(),
            Self::ObjectEx {
                header, list, dict, ..
            } => {
                1 + header.node_count()
                    + list.iter().map(Self::node_count).sum::<usize>()
                    + dict
                        .iter()
                        .map(|(k, v)| k.node_count() + v.node_count())
                        .sum::<usize>()
            }
        }
    }
}

/// Authoritative 1-based string table from `EVEMarshalStringTable.cpp` (`s_mStringTable`).
pub const MARSHAL_STRING_TABLE: &[&str] = &[
    "*corpid",
    "*locationid",
    "age",
    "Asteroid",
    "authentication",
    "ballID",
    "beyonce",
    "bloodlineID",
    "capacity",
    "categoryID",
    "character",
    "characterID",
    "characterName",
    "characterType",
    "charID",
    "chatx",
    "clientID",
    "config",
    "contraband",
    "corporationDateTime",
    "corporationID",
    "createDateTime",
    "customInfo",
    "description",
    "divisionID",
    "DoDestinyUpdate",
    "dogmaIM",
    "EVE System",
    "flag",
    "foo.SlimItem",
    "gangID",
    "Gemini",
    "gender",
    "graphicID",
    "groupID",
    "header",
    "idName",
    "invbroker",
    "itemID",
    "items",
    "jumps",
    "line",
    "lines",
    "locationID",
    "locationName",
    "macho.CallReq",
    "macho.CallRsp",
    "macho.MachoAddress",
    "macho.Notification",
    "macho.SessionChangeNotification",
    "modules",
    "name",
    "objectCaching",
    "objectCaching.CachedObject",
    "OnChatJoin",
    "OnChatLeave",
    "OnChatSpeak",
    "OnGodmaShipEffect",
    "OnItemChange",
    "OnModuleAttributeChange",
    "OnMultiEvent",
    "orbitID",
    "ownerID",
    "ownerName",
    "quantity",
    "raceID",
    "RowClass",
    "securityStatus",
    "Sentry Gun",
    "sessionchange",
    "singleton",
    "skillEffect",
    "squadronID",
    "typeID",
    "used",
    "userID",
    "util.CachedObject",
    "util.IndexRowset",
    "util.Moniker",
    "util.Row",
    "util.Rowset",
    "*multicastID",
    "AddBalls",
    "AttackHit3",
    "AttackHit3R",
    "AttackHit4R",
    "DoDestinyUpdates",
    "GetLocationsEx",
    "InvalidateCachedObjects",
    "JoinChannel",
    "LSC",
    "LaunchMissile",
    "LeaveChannel",
    "OID+",
    "OID-",
    "OnAggressionChange",
    "OnCharGangChange",
    "OnCharNoLongerInStation",
    "OnCharNowInStation",
    "OnDamageMessage",
    "OnDamageStateChange",
    "OnEffectHit",
    "OnGangDamageStateChange",
    "OnLSC",
    "OnSpecialFX",
    "OnTarget",
    "RemoveBalls",
    "SendMessage",
    "SetMaxSpeed",
    "SetSpeedFraction",
    "TerminalExplosion",
    "address",
    "alert",
    "allianceID",
    "allianceid",
    "bid",
    "bookmark",
    "bounty",
    "channel",
    "charid",
    "constellationid",
    "corpID",
    "corpid",
    "corprole",
    "damage",
    "duration",
    "effects.Laser",
    "gangid",
    "gangrole",
    "hqID",
    "issued",
    "jit",
    "languageID",
    "locationid",
    "machoVersion",
    "marketProxy",
    "minVolume",
    "orderID",
    "price",
    "range",
    "regionID",
    "regionid",
    "role",
    "rolesAtAll",
    "rolesAtBase",
    "rolesAtHQ",
    "rolesAtOther",
    "shipid",
    "sn",
    "solarSystemID",
    "solarsystemid",
    "solarsystemid2",
    "source",
    "splash",
    "stationID",
    "stationid",
    "target",
    "userType",
    "userid",
    "volEntered",
    "volRemaining",
    "weapon",
    "agent.missionTemplatizedContent_BasicKillMission",
    "agent.missionTemplatizedContent_ResearchKillMission",
    "agent.missionTemplatizedContent_StorylineKillMission",
    "agent.missionTemplatizedContent_GenericStorylineKillMission",
    "agent.missionTemplatizedContent_BasicCourierMission",
    "agent.missionTemplatizedContent_ResearchCourierMission",
    "agent.missionTemplatizedContent_StorylineCourierMission",
    "agent.missionTemplatizedContent_GenericStorylineCourierMission",
    "agent.missionTemplatizedContent_BasicTradeMission",
    "agent.missionTemplatizedContent_ResearchTradeMission",
    "agent.missionTemplatizedContent_StorylineTradeMission",
    "agent.missionTemplatizedContent_GenericStorylineTradeMission",
    "agent.offerTemplatizedContent_BasicExchangeOffer",
    "agent.offerTemplatizedContent_BasicExchangeOffer_ContrabandDemand",
    "agent.offerTemplatizedContent_BasicExchangeOffer_Crafting",
    "agent.LoyaltyPoints",
    "agent.ResearchPoints",
    "agent.Credits",
    "agent.Item",
    "agent.Entity",
    "agent.Objective",
    "agent.FetchObjective",
    "agent.EncounterObjective",
    "agent.DungeonObjective",
    "agent.TransportObjective",
    "agent.Reward",
    "agent.TimeBonusReward",
    "agent.MissionReferral",
    "agent.Location",
    "agent.StandardMissionDetails",
    "agent.OfferDetails",
    "agent.ResearchMissionDetails",
    "agent.StorylineMissionDetails",
];

/// Exact marshal wire opcodes.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarshalOpcode {
    PyNone = 0x01,
    PyToken = 0x02,
    PyLongLong = 0x03,
    PyLong = 0x04,
    PySignedShort = 0x05,
    PyByte = 0x06,
    PyMinusOne = 0x07,
    PyZeroInteger = 0x08,
    PyOneInteger = 0x09,
    PyReal = 0x0A,
    PyZeroReal = 0x0B,
    PyBuffer = 0x0D,
    PyEmptyString = 0x0E,
    PyCharString = 0x0F,
    PyShortString = 0x10,
    PyStringTableItem = 0x11,
    PyWStringUCS2 = 0x12,
    PyLongString = 0x13,
    PyTuple = 0x14,
    PyList = 0x15,
    PyDict = 0x16,
    PyObject = 0x17,
    PySubStruct = 0x19,
    PySavedStreamElement = 0x1B,
    PyChecksumedStream = 0x1C,
    PyTrue = 0x1F,
    PyFalse = 0x20,
    PyPicked = 0x21,
    PyObjectEx1 = 0x22,
    PyObjectEx2 = 0x23,
    PyEmptyTuple = 0x24,
    PyOneTuple = 0x25,
    PyEmptyList = 0x26,
    PyOneList = 0x27,
    PyEmptyWString = 0x28,
    PyWStringUCS2Char = 0x29,
    PyPackedRow = 0x2A,
    PySubStream = 0x2B,
    PyTwoTuple = 0x2C,
    PackedTerminator = 0x2D,
    PyWStringUTF8 = 0x2E,
    PyVarInteger = 0x2F,
}

pub const MARSHAL_HEADER_BYTE: u8 = 0x7E; // '~'
pub const ZLIB_HEADER_BYTE: u8 = 0x78;
pub const SAVE_FLAG: u8 = 0x40;

/// High-level envelope used by the Python Client Bridge (`evomu_bridge.py`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum BridgeEnvelope {
    RpcRequest {
        req_id: u64,
        service: String,
        bound_id: Option<u64>,
        method: String,
        args: Vec<PyRep>,
        kwargs: HashMap<String, PyRep>,
        caller_trace: Option<String>,
    },
    RpcResponse {
        req_id: u64,
        result: Result<PyRep, BridgeRpcError>,
    },
    ServerEvent {
        tick: u64,
        event: String,
        args: Vec<PyRep>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BridgeRpcError {
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests;
