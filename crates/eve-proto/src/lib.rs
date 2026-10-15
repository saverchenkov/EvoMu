//! `eve-proto` — Dual-protocol wire representation for `EvoMu`.
//!
//! Supports both:
//! 1. **Bridge Mode (`BridgeEnvelope`)**: Clean MessagePack/JSON RPC & Event framing for
//!    clients running the `evomu_bridge.py` transport shim.
//! 2. **Legacy `eveMarshal` Mode (`PyRep`)**: Conformant binary encoder/decoder matching
//!    `EVEMarshalOpcodes.h` and `EVEUnmarshal.cpp` (`0x01..0x2F` opcodes, `zlib` streams,
//!    trailing `save_count` storage-index map, and node-budget decompression bomb protection).

#![forbid(unsafe_code)]

use flate2::read::ZlibDecoder;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
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

    /// Counts total AST nodes in this `PyRep` subtree (used to enforce `MAX_DECODED_NODES`).
    pub fn node_count(&self) -> usize {
        match self {
            Self::None
            | Self::Bool(_)
            | Self::Integer(_)
            | Self::Real(_)
            | Self::Buffer(_)
            | Self::Bytes(_)
            | Self::Picked(_)
            | Self::Token(_)
            | Self::String(_) => 1,
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
                header, columns, ..
            } => {
                1 + header.node_count()
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

/// Exact wire opcodes from `evemu_Crucible/src/eve-common/marshal/EVEMarshalOpcodes.h`.
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

/// Conformant, resource-bounded `eveMarshal` binary encoder and decoder.
pub mod marshal {
    use super::*;

    pub fn encode(rep: &PyRep) -> Vec<u8> {
        let mut buf = Vec::with_capacity(64);
        buf.push(MARSHAL_HEADER_BYTE);
        buf.extend_from_slice(&0u32.to_le_bytes()); // Save count = 0
        encode_inner(rep, &mut buf);
        buf
    }

    pub fn encode_zlib(rep: &PyRep) -> Result<Vec<u8>, ProtoError> {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let raw = encode(rep);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(&raw)
            .map_err(|e| ProtoError::ZlibError(e.to_string()))?;
        encoder
            .finish()
            .map_err(|e| ProtoError::ZlibError(e.to_string()))
    }

    fn encode_inner(rep: &PyRep, out: &mut Vec<u8>) {
        match rep {
            PyRep::None => out.push(MarshalOpcode::PyNone as u8),
            PyRep::Bool(true) => out.push(MarshalOpcode::PyTrue as u8),
            PyRep::Bool(false) => out.push(MarshalOpcode::PyFalse as u8),
            PyRep::Token(t) => {
                out.push(MarshalOpcode::PyToken as u8);
                write_size_ex(out, t.len());
                out.extend_from_slice(t.as_bytes());
            }
            PyRep::Integer(-1) => out.push(MarshalOpcode::PyMinusOne as u8),
            PyRep::Integer(0) => out.push(MarshalOpcode::PyZeroInteger as u8),
            PyRep::Integer(1) => out.push(MarshalOpcode::PyOneInteger as u8),
            PyRep::Integer(v) if (-128..=127).contains(v) => {
                out.push(MarshalOpcode::PyByte as u8);
                out.push(*v as i8 as u8);
            }
            PyRep::Integer(v) if (-32768..=32767).contains(v) => {
                out.push(MarshalOpcode::PySignedShort as u8);
                out.extend_from_slice(&(*v as i16).to_le_bytes());
            }
            PyRep::Integer(v) if (i32::MIN as i64..=i32::MAX as i64).contains(v) => {
                out.push(MarshalOpcode::PyLong as u8);
                out.extend_from_slice(&(*v as i32).to_le_bytes());
            }
            PyRep::Integer(v) => {
                out.push(MarshalOpcode::PyLongLong as u8);
                out.extend_from_slice(&v.to_le_bytes());
            }
            PyRep::Real(v) if *v == 0.0 && v.is_sign_positive() => {
                out.push(MarshalOpcode::PyZeroReal as u8)
            }
            PyRep::Real(v) => {
                out.push(MarshalOpcode::PyReal as u8);
                out.extend_from_slice(&v.to_le_bytes());
            }
            PyRep::Bytes(b) => match b.len() {
                0 => out.push(MarshalOpcode::PyEmptyString as u8),
                1 => {
                    out.push(MarshalOpcode::PyCharString as u8);
                    out.push(b[0]);
                }
                len if len <= 0xFF => {
                    out.push(MarshalOpcode::PyShortString as u8);
                    out.push(len as u8);
                    out.extend_from_slice(b);
                }
                len => {
                    out.push(MarshalOpcode::PyLongString as u8);
                    write_size_ex(out, len);
                    out.extend_from_slice(b);
                }
            },
            PyRep::String(s) if s.is_empty() => out.push(MarshalOpcode::PyEmptyWString as u8),
            PyRep::String(s) => {
                out.push(MarshalOpcode::PyWStringUTF8 as u8);
                write_size_ex(out, s.len());
                out.extend_from_slice(s.as_bytes());
            }
            PyRep::Buffer(b) => {
                out.push(MarshalOpcode::PyBuffer as u8);
                write_size_ex(out, b.len());
                out.extend_from_slice(b);
            }
            PyRep::Tuple(items) => match items.len() {
                0 => out.push(MarshalOpcode::PyEmptyTuple as u8),
                1 => {
                    out.push(MarshalOpcode::PyOneTuple as u8);
                    encode_inner(&items[0], out);
                }
                2 => {
                    out.push(MarshalOpcode::PyTwoTuple as u8);
                    encode_inner(&items[0], out);
                    encode_inner(&items[1], out);
                }
                n => {
                    out.push(MarshalOpcode::PyTuple as u8);
                    write_size_ex(out, n);
                    for item in items {
                        encode_inner(item, out);
                    }
                }
            },
            PyRep::List(items) => match items.len() {
                0 => out.push(MarshalOpcode::PyEmptyList as u8),
                1 => {
                    out.push(MarshalOpcode::PyOneList as u8);
                    encode_inner(&items[0], out);
                }
                n => {
                    out.push(MarshalOpcode::PyList as u8);
                    write_size_ex(out, n);
                    for item in items {
                        encode_inner(item, out);
                    }
                }
            },
            PyRep::Dict(entries) => {
                out.push(MarshalOpcode::PyDict as u8);
                write_size_ex(out, entries.len());
                // EVE marshal writes VALUE first, then KEY
                for (k, v) in entries {
                    encode_inner(v, out);
                    encode_inner(k, out);
                }
            }
            PyRep::SubStruct(inner) => {
                out.push(MarshalOpcode::PySubStruct as u8);
                encode_inner(inner, out);
            }
            PyRep::ChecksumedStream { checksum, body } => {
                out.push(MarshalOpcode::PyChecksumedStream as u8);
                out.extend_from_slice(&checksum.to_le_bytes());
                encode_inner(body, out);
            }
            PyRep::SubStream(inner) => {
                out.push(MarshalOpcode::PySubStream as u8);
                let sub = encode(inner);
                write_size_ex(out, sub.len());
                out.extend_from_slice(&sub);
            }
            PyRep::KeyVal(map) => {
                out.push(MarshalOpcode::PyObject as u8);
                encode_inner(&PyRep::Token("util.KeyVal".to_string()), out);
                let mut entries: Vec<(PyRep, PyRep)> = map
                    .iter()
                    .map(|(k, v)| (PyRep::String(k.clone()), v.clone()))
                    .collect();
                entries.sort_by(|(a, _), (b, _)| a.as_str().cmp(&b.as_str()));
                encode_inner(&PyRep::Dict(entries), out);
            }
            PyRep::Rowset { columns, rows } => {
                out.push(MarshalOpcode::PyObject as u8);
                encode_inner(&PyRep::Token("util.Rowset".to_string()), out);
                let cols = PyRep::List(columns.iter().cloned().map(PyRep::String).collect());
                let row_list = PyRep::List(rows.iter().cloned().map(PyRep::List).collect());
                let dict = PyRep::Dict(vec![
                    (PyRep::String("header".into()), cols),
                    (
                        PyRep::String("RowClass".into()),
                        PyRep::Token("util.Row".into()),
                    ),
                    (PyRep::String("lines".into()), row_list),
                ]);
                encode_inner(&dict, out);
            }
            PyRep::IndexRowset {
                key_column,
                columns,
                items,
            } => {
                out.push(MarshalOpcode::PyObject as u8);
                encode_inner(&PyRep::Token("util.IndexRowset".to_string()), out);
                let cols = PyRep::List(columns.iter().cloned().map(PyRep::String).collect());
                let items_dict = PyRep::Dict(
                    items
                        .iter()
                        .map(|(k, r)| (k.clone(), PyRep::List(r.clone())))
                        .collect(),
                );
                let dict = PyRep::Dict(vec![
                    (PyRep::String("header".into()), cols),
                    (
                        PyRep::String("RowClass".into()),
                        PyRep::Token("util.Row".into()),
                    ),
                    (PyRep::String("idName".into()), PyRep::String(key_column.clone())),
                    (PyRep::String("items".into()), items_dict),
                ]);
                encode_inner(&dict, out);
            }
            PyRep::TupleSet { columns, rows } => {
                let cols = PyRep::List(columns.iter().cloned().map(PyRep::String).collect());
                let row_list = PyRep::List(rows.iter().cloned().map(PyRep::List).collect());
                encode_inner(&PyRep::Tuple(vec![cols, row_list]), out);
            }
            PyRep::DBRow {
                header, raw_rle, ..
            } => {
                out.push(MarshalOpcode::PyPackedRow as u8);
                encode_inner(header, out);
                write_size_ex(out, raw_rle.len());
                out.extend_from_slice(raw_rle);
            }
            PyRep::Picked(b) => {
                out.push(MarshalOpcode::PyPicked as u8);
                write_size_ex(out, b.len());
                out.extend_from_slice(b);
            }
            PyRep::Object { type_name, args } => {
                out.push(MarshalOpcode::PyObject as u8);
                encode_inner(&PyRep::Token(type_name.clone()), out);
                encode_inner(args, out);
            }
            PyRep::ObjectEx {
                is_type_2,
                header,
                list,
                dict,
            } => {
                out.push(if *is_type_2 {
                    MarshalOpcode::PyObjectEx2 as u8
                } else {
                    MarshalOpcode::PyObjectEx1 as u8
                });
                encode_inner(header, out);
                for item in list {
                    encode_inner(item, out);
                }
                out.push(MarshalOpcode::PackedTerminator as u8);
                for (k, v) in dict {
                    encode_inner(k, out);
                    encode_inner(v, out);
                }
                out.push(MarshalOpcode::PackedTerminator as u8);
            }
        }
    }

    /// Decodes a raw or `zlib`-compressed `eveMarshal` payload with strict size, collection,
    /// recursion, node-budget, and trailing `save_count` map handling.
    pub fn decode(input: &[u8]) -> Result<PyRep, ProtoError> {
        if input.is_empty() {
            return Err(ProtoError::UnexpectedEof);
        }
        if input.len() > MAX_FRAME_SIZE {
            return Err(ProtoError::FrameTooLarge(input.len()));
        }

        let decompressed_buf: Vec<u8>;
        let cursor: &[u8] = if input[0] == ZLIB_HEADER_BYTE {
            let mut decoder = ZlibDecoder::new(input).take((MAX_FRAME_SIZE + 1) as u64);
            let mut out = Vec::new();
            decoder
                .read_to_end(&mut out)
                .map_err(|e| ProtoError::ZlibError(e.to_string()))?;
            if out.len() > MAX_FRAME_SIZE {
                return Err(ProtoError::FrameTooLarge(out.len()));
            }
            decompressed_buf = out;
            &decompressed_buf
        } else {
            input
        };

        let mut remaining_budget = MAX_DECODED_NODES;
        decode_stream_slice(cursor, 0, &mut remaining_budget)
    }

    fn decode_stream_slice(
        stream: &[u8],
        depth: usize,
        remaining_budget: &mut usize,
    ) -> Result<PyRep, ProtoError> {
        if depth > MAX_RECURSION_DEPTH {
            return Err(ProtoError::RecursionLimitExceeded);
        }
        if stream.is_empty() {
            return Err(ProtoError::UnexpectedEof);
        }
        if stream[0] != MARSHAL_HEADER_BYTE {
            return Err(ProtoError::InvalidHeader(stream[0]));
        }
        if stream.len() < 5 {
            return Err(ProtoError::UnexpectedEof);
        }

        let save_count = u32::from_le_bytes(stream[1..5].try_into().unwrap()) as usize;
        if save_count > MAX_COLLECTION_LEN {
            return Err(ProtoError::CollectionTooLarge(save_count));
        }

        let map_byte_len = save_count.checked_mul(4).ok_or(ProtoError::UnexpectedEof)?;
        if stream.len() < 5 + map_byte_len {
            return Err(ProtoError::UnexpectedEof);
        }

        let body_end = stream.len() - map_byte_len;
        let mut body_cursor = &stream[5..body_end];
        let mut save_map_cursor = &stream[body_end..];
        let mut saved_slots: Vec<Option<PyRep>> = vec![None; save_count];

        let root = decode_inner(
            &mut body_cursor,
            &mut save_map_cursor,
            depth,
            &mut saved_slots,
            remaining_budget,
        )?;

        if !body_cursor.is_empty() {
            return Err(ProtoError::TrailingBytes(body_cursor.len()));
        }

        Ok(root)
    }

    fn decode_inner(
        input: &mut &[u8],
        save_map: &mut &[u8],
        depth: usize,
        saved_slots: &mut [Option<PyRep>],
        remaining_budget: &mut usize,
    ) -> Result<PyRep, ProtoError> {
        if depth > MAX_RECURSION_DEPTH {
            return Err(ProtoError::RecursionLimitExceeded);
        }
        *remaining_budget = remaining_budget
            .checked_sub(1)
            .ok_or(ProtoError::NodeBudgetExceeded)?;

        if input.is_empty() {
            return Err(ProtoError::UnexpectedEof);
        }

        let raw_byte = input[0];
        *input = &input[1..];
        let is_saved = (raw_byte & SAVE_FLAG) != 0;
        let opcode = raw_byte & 0x3F;

        // Per EVEUnmarshal.cpp: if `flagSave` is set, read the 1-based storage index from `save_map`
        // BEFORE decoding any children!
        let storage_index = if is_saved {
            if save_map.len() < 4 {
                return Err(ProtoError::InvalidSaveReference(0));
            }
            let idx = u32::from_le_bytes(save_map[..4].try_into().unwrap()) as usize;
            *save_map = &save_map[4..];
            if idx == 0 || idx > saved_slots.len() {
                return Err(ProtoError::InvalidSaveReference(idx));
            }
            Some(idx - 1)
        } else {
            None
        };

        let value = match opcode {
            x if x == MarshalOpcode::PyNone as u8 => PyRep::None,
            x if x == MarshalOpcode::PyTrue as u8 => PyRep::Bool(true),
            x if x == MarshalOpcode::PyFalse as u8 => PyRep::Bool(false),
            x if x == MarshalOpcode::PyToken as u8 => {
                let len = read_size_ex(input)?;
                if input.len() < len {
                    return Err(ProtoError::UnexpectedEof);
                }
                let s = String::from_utf8(input[..len].to_vec())?;
                *input = &input[len..];
                PyRep::Token(s)
            }
            x if x == MarshalOpcode::PyMinusOne as u8 => PyRep::Integer(-1),
            x if x == MarshalOpcode::PyZeroInteger as u8 => PyRep::Integer(0),
            x if x == MarshalOpcode::PyOneInteger as u8 => PyRep::Integer(1),
            x if x == MarshalOpcode::PyByte as u8 => {
                if input.is_empty() {
                    return Err(ProtoError::UnexpectedEof);
                }
                let val = input[0] as i8 as i64;
                *input = &input[1..];
                PyRep::Integer(val)
            }
            x if x == MarshalOpcode::PySignedShort as u8 => {
                if input.len() < 2 {
                    return Err(ProtoError::UnexpectedEof);
                }
                let val = i16::from_le_bytes([input[0], input[1]]) as i64;
                *input = &input[2..];
                PyRep::Integer(val)
            }
            x if x == MarshalOpcode::PyLong as u8 => {
                if input.len() < 4 {
                    return Err(ProtoError::UnexpectedEof);
                }
                let val = i32::from_le_bytes(input[..4].try_into().unwrap()) as i64;
                *input = &input[4..];
                PyRep::Integer(val)
            }
            x if x == MarshalOpcode::PyLongLong as u8 => {
                if input.len() < 8 {
                    return Err(ProtoError::UnexpectedEof);
                }
                let val = i64::from_le_bytes(input[..8].try_into().unwrap());
                *input = &input[8..];
                PyRep::Integer(val)
            }
            x if x == MarshalOpcode::PyVarInteger as u8 => {
                let len = read_size_ex(input)?;
                if input.len() < len || len > 8 {
                    return Err(ProtoError::UnexpectedEof);
                }
                let mut bytes = [0u8; 8];
                bytes[..len].copy_from_slice(&input[..len]);
                if len > 0 && (input[len - 1] & 0x80) != 0 {
                    for b in &mut bytes[len..] {
                        *b = 0xFF;
                    }
                }
                *input = &input[len..];
                PyRep::Integer(i64::from_le_bytes(bytes))
            }
            x if x == MarshalOpcode::PyZeroReal as u8 => PyRep::Real(0.0),
            x if x == MarshalOpcode::PyReal as u8 => {
                if input.len() < 8 {
                    return Err(ProtoError::UnexpectedEof);
                }
                let val = f64::from_le_bytes(input[..8].try_into().unwrap());
                *input = &input[8..];
                PyRep::Real(val)
            }
            x if x == MarshalOpcode::PyBuffer as u8 => {
                let len = read_size_ex(input)?;
                if input.len() < len {
                    return Err(ProtoError::UnexpectedEof);
                }
                let b = input[..len].to_vec();
                *input = &input[len..];
                PyRep::Buffer(b)
            }
            x if x == MarshalOpcode::PyEmptyString as u8 => PyRep::Bytes(vec![]),
            x if x == MarshalOpcode::PyCharString as u8 => {
                if input.is_empty() {
                    return Err(ProtoError::UnexpectedEof);
                }
                let b = input[0];
                *input = &input[1..];
                PyRep::Bytes(vec![b])
            }
            x if x == MarshalOpcode::PyShortString as u8 => {
                if input.is_empty() {
                    return Err(ProtoError::UnexpectedEof);
                }
                let len = input[0] as usize;
                *input = &input[1..];
                if input.len() < len {
                    return Err(ProtoError::UnexpectedEof);
                }
                let b = input[..len].to_vec();
                *input = &input[len..];
                PyRep::Bytes(b)
            }
            x if x == MarshalOpcode::PyStringTableItem as u8 => {
                if input.is_empty() {
                    return Err(ProtoError::UnexpectedEof);
                }
                let idx = input[0] as usize;
                *input = &input[1..];
                let s = idx
                    .checked_sub(1)
                    .and_then(|i| MARSHAL_STRING_TABLE.get(i))
                    .ok_or(ProtoError::UnsupportedOpcode(opcode))?;
                PyRep::String((*s).to_string())
            }
            x if x == MarshalOpcode::PyWStringUCS2Char as u8 => {
                if input.len() < 2 {
                    return Err(ProtoError::UnexpectedEof);
                }
                let code_unit = u16::from_le_bytes([input[0], input[1]]);
                *input = &input[2..];
                let s = String::from_utf16_lossy(&[code_unit]);
                PyRep::String(s)
            }
            x if x == MarshalOpcode::PyWStringUCS2 as u8 => {
                let char_len = read_collection_len(input)?;
                let byte_len = char_len.checked_mul(2).ok_or(ProtoError::UnexpectedEof)?;
                if input.len() < byte_len {
                    return Err(ProtoError::UnexpectedEof);
                }
                let mut units = Vec::with_capacity(char_len.min(1024));
                for chunk in input[..byte_len].chunks_exact(2) {
                    units.push(u16::from_le_bytes([chunk[0], chunk[1]]));
                }
                *input = &input[byte_len..];
                PyRep::String(String::from_utf16_lossy(&units))
            }
            x if x == MarshalOpcode::PyLongString as u8 => {
                let len = read_size_ex(input)?;
                if input.len() < len {
                    return Err(ProtoError::UnexpectedEof);
                }
                let b = input[..len].to_vec();
                *input = &input[len..];
                PyRep::Bytes(b)
            }
            x if x == MarshalOpcode::PyPicked as u8 => {
                let len = read_size_ex(input)?;
                if input.len() < len {
                    return Err(ProtoError::UnexpectedEof);
                }
                let b = input[..len].to_vec();
                *input = &input[len..];
                PyRep::Picked(b)
            }
            x if x == MarshalOpcode::PyPackedRow as u8 => {
                let header =
                    decode_inner(input, save_map, depth + 1, saved_slots, remaining_budget)?;
                let rle_len = read_size_ex(input)?;
                if input.len() < rle_len {
                    return Err(ProtoError::UnexpectedEof);
                }
                let raw_rle = input[..rle_len].to_vec();
                *input = &input[rle_len..];
                PyRep::DBRow {
                    header: Box::new(header),
                    columns: Vec::new(),
                    raw_rle,
                }
            }
            x if x == MarshalOpcode::PyEmptyWString as u8 => PyRep::String(String::new()),
            x if x == MarshalOpcode::PyWStringUTF8 as u8 => {
                let len = read_size_ex(input)?;
                if input.len() < len {
                    return Err(ProtoError::UnexpectedEof);
                }
                let s = String::from_utf8(input[..len].to_vec())?;
                *input = &input[len..];
                PyRep::String(s)
            }
            x if x == MarshalOpcode::PyEmptyTuple as u8 => PyRep::Tuple(vec![]),
            x if x == MarshalOpcode::PyOneTuple as u8 => PyRep::Tuple(vec![decode_inner(
                input,
                save_map,
                depth + 1,
                saved_slots,
                remaining_budget,
            )?]),
            x if x == MarshalOpcode::PyTwoTuple as u8 => {
                let a = decode_inner(input, save_map, depth + 1, saved_slots, remaining_budget)?;
                let b = decode_inner(input, save_map, depth + 1, saved_slots, remaining_budget)?;
                PyRep::Tuple(vec![a, b])
            }
            x if x == MarshalOpcode::PyTuple as u8 => {
                let len = read_collection_len(input)?;
                let mut items = Vec::with_capacity(len.min(input.len()).min(1024));
                for _ in 0..len {
                    items.push(decode_inner(
                        input,
                        save_map,
                        depth + 1,
                        saved_slots,
                        remaining_budget,
                    )?);
                }
                PyRep::Tuple(items)
            }
            x if x == MarshalOpcode::PyEmptyList as u8 => PyRep::List(vec![]),
            x if x == MarshalOpcode::PyOneList as u8 => PyRep::List(vec![decode_inner(
                input,
                save_map,
                depth + 1,
                saved_slots,
                remaining_budget,
            )?]),
            x if x == MarshalOpcode::PyList as u8 => {
                let len = read_collection_len(input)?;
                let mut items = Vec::with_capacity(len.min(input.len()).min(1024));
                for _ in 0..len {
                    items.push(decode_inner(
                        input,
                        save_map,
                        depth + 1,
                        saved_slots,
                        remaining_budget,
                    )?);
                }
                PyRep::List(items)
            }
            x if x == MarshalOpcode::PyDict as u8 => {
                let len = read_collection_len(input)?;
                let mut entries = Vec::with_capacity(len.min(input.len() / 2).min(1024));
                for _ in 0..len {
                    let v =
                        decode_inner(input, save_map, depth + 1, saved_slots, remaining_budget)?;
                    let k =
                        decode_inner(input, save_map, depth + 1, saved_slots, remaining_budget)?;
                    entries.push((k, v));
                }
                PyRep::Dict(entries)
            }
            x if x == MarshalOpcode::PyObject as u8 => {
                let type_rep =
                    decode_inner(input, save_map, depth + 1, saved_slots, remaining_budget)?;
                let type_name = type_rep
                    .as_str()
                    .ok_or(ProtoError::UnsupportedOpcode(opcode))?
                    .to_string();
                let args = decode_inner(input, save_map, depth + 1, saved_slots, remaining_budget)?;

                if type_name == "util.KeyVal" {
                    if let PyRep::Dict(entries) = &args {
                        let mut map = HashMap::with_capacity(entries.len());
                        for (k, v) in entries {
                            if let Some(ks) = k.as_str() {
                                map.insert(ks.to_string(), v.clone());
                            }
                        }
                        PyRep::KeyVal(map)
                    } else {
                        PyRep::Object {
                            type_name,
                            args: Box::new(args),
                        }
                    }
                } else if type_name == "util.Rowset" {
                    if let Some((columns, rows)) = try_extract_rowset(&args) {
                        PyRep::Rowset { columns, rows }
                    } else {
                        PyRep::Object {
                            type_name,
                            args: Box::new(args),
                        }
                    }
                } else if type_name == "util.IndexRowset" {
                    if let Some((key_column, columns, items)) = try_extract_index_rowset(&args) {
                        PyRep::IndexRowset {
                            key_column,
                            columns,
                            items,
                        }
                    } else {
                        PyRep::Object {
                            type_name,
                            args: Box::new(args),
                        }
                    }
                } else {
                    PyRep::Object {
                        type_name,
                        args: Box::new(args),
                    }
                }
            }
            x if x == MarshalOpcode::PyObjectEx1 as u8 || x == MarshalOpcode::PyObjectEx2 as u8 => {
                let is_type_2 = opcode == MarshalOpcode::PyObjectEx2 as u8;
                let header =
                    decode_inner(input, save_map, depth + 1, saved_slots, remaining_budget)?;
                let mut list = Vec::new();
                while !input.is_empty() && input[0] != MarshalOpcode::PackedTerminator as u8 {
                    if list.len() >= MAX_COLLECTION_LEN {
                        return Err(ProtoError::CollectionTooLarge(list.len() + 1));
                    }
                    list.push(decode_inner(
                        input,
                        save_map,
                        depth + 1,
                        saved_slots,
                        remaining_budget,
                    )?);
                }
                if input.is_empty() {
                    return Err(ProtoError::UnexpectedEof);
                }
                *input = &input[1..]; // Consume first PackedTerminator (0x2D)

                let mut dict = Vec::new();
                while !input.is_empty() && input[0] != MarshalOpcode::PackedTerminator as u8 {
                    if dict.len() >= MAX_COLLECTION_LEN {
                        return Err(ProtoError::CollectionTooLarge(dict.len() + 1));
                    }
                    let k =
                        decode_inner(input, save_map, depth + 1, saved_slots, remaining_budget)?;
                    let v =
                        decode_inner(input, save_map, depth + 1, saved_slots, remaining_budget)?;
                    dict.push((k, v));
                }
                if input.is_empty() {
                    return Err(ProtoError::UnexpectedEof);
                }
                *input = &input[1..]; // Consume second PackedTerminator (0x2D)

                PyRep::ObjectEx {
                    is_type_2,
                    header: Box::new(header),
                    list,
                    dict,
                }
            }
            x if x == MarshalOpcode::PySubStruct as u8 => {
                let inner =
                    decode_inner(input, save_map, depth + 1, saved_slots, remaining_budget)?;
                PyRep::SubStruct(Box::new(inner))
            }
            x if x == MarshalOpcode::PyChecksumedStream as u8 => {
                if input.len() < 4 {
                    return Err(ProtoError::UnexpectedEof);
                }
                let checksum = u32::from_le_bytes(input[..4].try_into().unwrap());
                *input = &input[4..];
                let body = decode_inner(input, save_map, depth + 1, saved_slots, remaining_budget)?;
                PyRep::ChecksumedStream {
                    checksum,
                    body: Box::new(body),
                }
            }
            x if x == MarshalOpcode::PySubStream as u8 => {
                let len = read_size_ex(input)?;
                if input.len() < len {
                    return Err(ProtoError::UnexpectedEof);
                }
                let sub_slice = &input[..len];
                *input = &input[len..];
                let inner = decode_stream_slice(sub_slice, depth + 1, remaining_budget)?;
                PyRep::SubStream(Box::new(inner))
            }
            x if x == MarshalOpcode::PySavedStreamElement as u8 => {
                let idx = read_size_ex(input)?;
                let zero_idx = idx
                    .checked_sub(1)
                    .ok_or(ProtoError::InvalidSaveReference(idx))?;
                let cloned = saved_slots
                    .get(zero_idx)
                    .and_then(|opt| opt.as_ref())
                    .cloned()
                    .ok_or(ProtoError::InvalidSaveReference(idx))?;

                // Charge the full expanded node count of the referenced subtree against budget!
                let extra_nodes = cloned.node_count().saturating_sub(1);
                *remaining_budget = remaining_budget
                    .checked_sub(extra_nodes)
                    .ok_or(ProtoError::NodeBudgetExceeded)?;
                cloned
            }
            other => return Err(ProtoError::UnsupportedOpcode(other)),
        };

        if let Some(slot_idx) = storage_index {
            saved_slots[slot_idx] = Some(value.clone());
        }

        Ok(value)
    }

    /// Decodes a `PyPackedRow` zero-compressed RLE byte stream per `EVEUnmarshal.cpp` `LoadRLE`.
    pub fn decode_rle(rle_bytes: &[u8], expected_len: usize) -> Option<Vec<u8>> {
        let mut out = vec![0u8; expected_len];
        let mut in_ix = 0usize;
        let mut out_ix = 0usize;
        let mut run = 0i32;
        let mut nibble = false;

        while in_ix < rle_bytes.len() {
            nibble = !nibble;
            let mut count = if nibble {
                run = rle_bytes[in_ix] as i32;
                in_ix += 1;
                (run & 0x0F) - 8
            } else {
                (run >> 4) - 8
            };

            if count >= 0 {
                let zeros = (count + 1) as usize;
                if out_ix.checked_add(zeros)? > out.len() {
                    return None;
                }
                for _ in 0..zeros {
                    out[out_ix] = 0;
                    out_ix += 1;
                }
            } else {
                while count < 0 && in_ix < rle_bytes.len() {
                    if out_ix >= out.len() {
                        return None;
                    }
                    out[out_ix] = rle_bytes[in_ix];
                    out_ix += 1;
                    in_ix += 1;
                    count += 1;
                }
            }
        }
        Some(out)
    }

    #[allow(clippy::type_complexity)]
    fn try_extract_index_rowset(
        args: &PyRep,
    ) -> Option<(String, Vec<String>, Vec<(PyRep, Vec<PyRep>)>)> {
        let PyRep::Dict(entries) = args else {
            return None;
        };
        let mut key_column = None;
        let mut columns = None;
        let mut items = None;
        for (k, v) in entries {
            match k.as_str() {
                Some("idName") => {
                    key_column = Some(v.as_str()?.to_string());
                }
                Some("header") => {
                    if let PyRep::List(col_reps) = v {
                        let mut cols = Vec::with_capacity(col_reps.len());
                        for c in col_reps {
                            cols.push(c.as_str()?.to_string());
                        }
                        columns = Some(cols);
                    }
                }
                Some("items") => {
                    if let PyRep::Dict(item_entries) = v {
                        let mut extracted = Vec::with_capacity(item_entries.len());
                        for (ik, iv) in item_entries {
                            if let PyRep::List(cells) = iv {
                                extracted.push((ik.clone(), cells.clone()));
                            } else {
                                return None;
                            }
                        }
                        items = Some(extracted);
                    }
                }
                _ => {}
            }
        }
        Some((key_column?, columns?, items?))
    }

    fn try_extract_rowset(args: &PyRep) -> Option<(Vec<String>, Vec<Vec<PyRep>>)> {
        let PyRep::Dict(entries) = args else {
            return None;
        };
        let mut columns = None;
        let mut rows = None;
        for (k, v) in entries {
            match k.as_str() {
                Some("header") => {
                    if let PyRep::List(col_reps) = v {
                        let mut cols = Vec::with_capacity(col_reps.len());
                        for c in col_reps {
                            cols.push(c.as_str()?.to_string());
                        }
                        columns = Some(cols);
                    }
                }
                Some("lines") => {
                    if let PyRep::List(line_reps) = v {
                        let mut r = Vec::with_capacity(line_reps.len());
                        for line in line_reps {
                            if let PyRep::List(cells) = line {
                                r.push(cells.clone());
                            } else {
                                return None;
                            }
                        }
                        rows = Some(r);
                    }
                }
                _ => {}
            }
        }
        Some((columns?, rows?))
    }

    fn write_size_ex(out: &mut Vec<u8>, len: usize) {
        if len < 0xFF {
            out.push(len as u8);
        } else {
            out.push(0xFF);
            out.extend_from_slice(&(len as u32).to_le_bytes());
        }
    }

    fn read_collection_len(input: &mut &[u8]) -> Result<usize, ProtoError> {
        let len = read_size_ex(input)?;
        if len > MAX_COLLECTION_LEN {
            return Err(ProtoError::CollectionTooLarge(len));
        }
        Ok(len)
    }

    fn read_size_ex(input: &mut &[u8]) -> Result<usize, ProtoError> {
        if input.is_empty() {
            return Err(ProtoError::UnexpectedEof);
        }
        let first = input[0];
        *input = &input[1..];
        if first < 0xFF {
            Ok(first as usize)
        } else {
            if input.len() < 4 {
                return Err(ProtoError::UnexpectedEof);
            }
            let len = u32::from_le_bytes(input[..4].try_into().unwrap()) as usize;
            *input = &input[4..];
            Ok(len)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_marshal_roundtrip_composite_and_bytes() {
        let original = PyRep::Tuple(vec![
            PyRep::None,
            PyRep::Bool(true),
            PyRep::Integer(30000142),
            PyRep::Real(149_597_870_700.0),
            PyRep::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF]), // Non-UTF8 binary byte string
            PyRep::Token("macho.CallReq".to_string()),
            PyRep::String("Jita IV - Moon 4".to_string()),
            PyRep::Dict(vec![(
                PyRep::String("solarSystemID".to_string()),
                PyRep::Integer(30000142),
            )]),
        ]);

        let bytes = marshal::encode(&original);
        let decoded = marshal::decode(&bytes).expect("marshal decode should succeed");
        assert_eq!(original, decoded);
        assert_eq!(bytes, marshal::encode(&decoded));
    }

    #[test]
    fn test_saved_stream_reference_map_and_bomb_protection() {
        // Construct a real eveMarshal stream with save_count = 1 and a trailing 4-byte storage index [1, 0, 0, 0]:
        // TupleTwo(Saved(Token("shared")), PySavedStreamElement(1))
        let mut raw = vec![
            MARSHAL_HEADER_BYTE,
            1,
            0,
            0,
            0, // save_count = 1
            MarshalOpcode::PyTwoTuple as u8,
            (MarshalOpcode::PyToken as u8) | SAVE_FLAG, // 0x02 | 0x40
            6,
            b's',
            b'h',
            b'a',
            b'r',
            b'e',
            b'd',
            MarshalOpcode::PySavedStreamElement as u8,
            1, // Reference slot 1
        ];
        raw.extend_from_slice(&1u32.to_le_bytes()); // Trailing save map: slot 1

        let decoded = marshal::decode(&raw).unwrap();
        assert_eq!(
            decoded,
            PyRep::Tuple(vec![
                PyRep::Token("shared".into()),
                PyRep::Token("shared".into()),
            ])
        );
    }

    #[test]
    fn test_keyval_rowset_and_objectex_roundtrip() {
        let kv = PyRep::keyval([
            ("solarSystemID", PyRep::Integer(30000142)),
            ("security", PyRep::Real(0.95)),
        ]);
        let kv_bytes = marshal::encode(&kv);
        assert_eq!(marshal::decode(&kv_bytes).unwrap(), kv);

        let rs = PyRep::rowset(
            vec!["characterID", "characterName"],
            vec![vec![
                PyRep::Integer(90_000_001),
                PyRep::String("Aura".into()),
            ]],
        );
        let rs_bytes = marshal::encode(&rs);
        assert_eq!(marshal::decode(&rs_bytes).unwrap(), rs);

        let obj_ex = PyRep::ObjectEx {
            is_type_2: false,
            header: Box::new(PyRep::Tuple(vec![PyRep::Token(
                "blue.DBRowDescriptor".into(),
            )])),
            list: vec![PyRep::Integer(1)],
            dict: vec![(PyRep::String("k".into()), PyRep::Integer(42))],
        };
        let ex_bytes = marshal::encode(&obj_ex);
        assert_eq!(marshal::decode(&ex_bytes).unwrap(), obj_ex);
    }

    #[test]
    fn test_zlib_roundtrip_and_trailing_garbage_rejection() {
        let val = PyRep::SubStream(Box::new(PyRep::Integer(123456)));
        let compressed = marshal::encode_zlib(&val).unwrap();
        assert_eq!(compressed[0], ZLIB_HEADER_BYTE);
        assert_eq!(marshal::decode(&compressed).unwrap(), val);

        let mut with_garbage = marshal::encode(&val);
        with_garbage.push(0x99);
        assert_eq!(
            marshal::decode(&with_garbage),
            Err(ProtoError::TrailingBytes(1))
        );
    }

    #[test]
    fn test_collection_and_recursion_bounds() {
        let mut forged = vec![
            MARSHAL_HEADER_BYTE,
            0,
            0,
            0,
            0,
            MarshalOpcode::PyTuple as u8,
            0xFF,
        ];
        forged.extend_from_slice(&1_000_000u32.to_le_bytes());
        assert_eq!(
            marshal::decode(&forged),
            Err(ProtoError::CollectionTooLarge(1_000_000))
        );

        let mut deep = vec![MARSHAL_HEADER_BYTE, 0, 0, 0, 0];
        deep.extend(std::iter::repeat_n(
            MarshalOpcode::PyOneTuple as u8,
            MAX_RECURSION_DEPTH + 5,
        ));
        deep.push(MarshalOpcode::PyNone as u8);
        assert_eq!(
            marshal::decode(&deep),
            Err(ProtoError::RecursionLimitExceeded)
        );
    }
}
