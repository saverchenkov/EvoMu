//! `eve-proto` — Dual-protocol wire representation for `EvoMu`.
//!
//! Supports both:
//! 1. **Bridge Mode (`BridgeEnvelope`)**: Clean MessagePack/JSON RPC & Event framing for
//!    clients running the `evomu_bridge.py` transport shim.
//! 2. **Legacy `eveMarshal` Mode (`PyRep`)**: Conformant binary encoder/decoder for the
//!    marshal wire format (`0x01..0x2F` opcodes, `zlib` streams,
//!    trailing `save_count` storage-index map, and node-budget decompression bomb protection).

#![forbid(unsafe_code)]

pub mod client_error;
pub mod dbrow;
pub mod marshal;
pub mod pb;
pub mod quasar;
pub mod quasar_wire;

pub use client_error::{
    round_user_error_real_6dp, ClientError, IntoUserErrorArg, IskCents, ItemQty, ItemQtyError,
    RejectionFacts, UserErrorArg, UserErrorKey, UserErrorPresentation, WarpPhaseKind,
    WarpPhasePolicy, MACHONETERR_WRAPPEDEXCEPTION, UI_LABEL_GATE_CLOSED, UI_LABEL_SHIP_TOO_LARGE,
    V24_PROGRAMMATIC_USER_ERROR_KEYS, V24_USER_ERROR_LABELS, WARP_PHASE_REJECTION_TABLE,
};

pub use dbrow::{
    dbtype_bit_width, decode_rle, encode_rle, extract_dbrow_col_defs,
    extract_dbrow_header_col_defs, pack_packed_row_columns, pack_packed_row_columns_from_defs,
    try_unpack_packed_row_columns, unpack_packed_row_columns, DBRowDescriptor, DbColumnType,
};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use thiserror::Error;

/// Maximum allowed wire frame size (4 MiB) to prevent memory exhaustion on untrusted input.
pub const MAX_FRAME_SIZE: usize = 4 * 1024 * 1024;
/// Maximum collection element count decoded from a single container opcode.
pub const MAX_COLLECTION_LEN: usize = 131_072;
/// Maximum recursion depth for nested `PyRep` containers and sub-streams.
pub const MAX_RECURSION_DEPTH: usize = 64;
/// Maximum total AST nodes constructed during a single `marshal::decode` call (stops reference bombs).
pub const MAX_DECODED_NODES: usize = 1_048_576;

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
#[derive(Serialize, Deserialize)]
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
    PySet(Vec<PyRep>),
    Set(Vec<PyRep>),
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
                    return matches!(
                        args.first(),
                        Some(Self::String(s) | Self::Token(s))
                            if matches!(
                                s.as_str(),
                                "dbutil.CRowset" | "carbon.common.script.sys.crowset.CRowset"
                            )
                    );
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
                    return matches!(
                        args.first(),
                        Some(Self::String(s) | Self::Token(s))
                            if matches!(
                                s.as_str(),
                                "dbutil.CIndexedRowset"
                                    | "carbon.common.script.sys.crowset.CIndexedRowset"
                            )
                    );
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
            Self::List(items) | Self::PySet(items) | Self::Set(items) => Some(items.len()),
            _ => None,
        }
    }

    /// Convenience accessor for dictionary-like containers (`Dict`, `KeyVal`, `Rowset`, `DBRow`, `util.Row`).
    ///
    /// Uses an explicit traversal stack so deeply nested containers cannot overflow the call stack.
    pub fn dict_get(&self, key: &str) -> Option<&PyRep> {
        let mut stack = vec![self];
        while let Some(node) = stack.pop() {
            match node {
                Self::KeyVal(map) => {
                    if let Some(v) = map.get(key) {
                        return Some(v);
                    }
                }
                Self::Dict(entries) => {
                    if let Some(v) = entries
                        .iter()
                        .find(|(k, _)| k.as_str() == Some(key))
                        .map(|(_, v)| v)
                    {
                        return Some(v);
                    }
                }
                Self::DBRow { columns, .. } => {
                    if let Some(v) = columns
                        .iter()
                        .find(|(n, _)| n == key)
                        .map(|(_, v)| v)
                    {
                        return Some(v);
                    }
                }
                Self::Rowset { columns, rows } | Self::TupleSet { columns, rows } => {
                    if let Some(v) = columns
                        .iter()
                        .position(|c| c == key)
                        .and_then(|idx| rows.first().and_then(|r| r.get(idx)))
                    {
                        return Some(v);
                    }
                }
                Self::Object { type_name, args } => {
                    if type_name.ends_with(".Row") || type_name == "Row" {
                        if let Self::Tuple(parts) = args.as_ref() {
                            if let (Some(Self::List(cols)), Some(Self::List(vals))) =
                                (parts.first(), parts.get(1))
                            {
                                for (i, c) in cols.iter().enumerate() {
                                    if c.as_str() == Some(key) {
                                        return vals.get(i);
                                    }
                                }
                            }
                        }
                    }
                    stack.push(args.as_ref());
                }
                Self::ObjectEx {
                    header, dict, list, ..
                } => {
                    if let Some(v) = dict
                        .iter()
                        .find(|(k, _)| k.as_str() == Some(key))
                        .map(|(_, v)| v)
                    {
                        return Some(v);
                    }
                    // Preserve `list.first()` precedence over `header` on LIFO stack.
                    stack.push(header.as_ref());
                    if let Some(first) = list.first() {
                        stack.push(first);
                    }
                }
                Self::Tuple(items) | Self::List(items) => {
                    stack.extend(items.iter().rev());
                }
                _ => {}
            }
        }
        None
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
    ///
    /// Implemented iteratively so arbitrarily deep synthetic trees never overflow the call stack.
    pub fn node_count(&self) -> usize {
        if let Some(leaf_weight) = Self::leaf_node_weight(self) {
            return leaf_weight;
        }
        let mut total = 0usize;
        let mut stack = vec![self];
        while let Some(node) = stack.pop() {
            match node {
                Self::None | Self::Bool(_) | Self::Integer(_) | Self::Real(_) => {
                    total = total.saturating_add(1);
                }
                Self::Buffer(b) | Self::Bytes(b) | Self::Picked(b) => {
                    total = total.saturating_add(1 + (b.len() / 64));
                }
                Self::Token(s) | Self::String(s) => {
                    total = total.saturating_add(1 + (s.len() / 64));
                }
                Self::Tuple(v) | Self::List(v) | Self::PySet(v) | Self::Set(v) => {
                    total = total.saturating_add(1);
                    stack.extend(v.iter());
                }
                Self::Dict(entries) => {
                    total = total.saturating_add(1);
                    for (k, v) in entries {
                        stack.push(k);
                        stack.push(v);
                    }
                }
                Self::KeyVal(map) => {
                    total = total.saturating_add(1);
                    for (k, v) in map {
                        total = total.saturating_add(1 + (k.len() / 64));
                        stack.push(v);
                    }
                }
                Self::Rowset { columns, rows } | Self::TupleSet { columns, rows } => {
                    total = total.saturating_add(1);
                    for c in columns {
                        total = total.saturating_add(1 + (c.len() / 64));
                    }
                    for r in rows {
                        if r.is_empty() {
                            total = total.saturating_add(1);
                        } else {
                            stack.extend(r.iter());
                        }
                    }
                }
                Self::IndexRowset {
                    key_column,
                    columns,
                    items,
                } => {
                    total = total.saturating_add(1 + (1 + (key_column.len() / 64)));
                    for c in columns {
                        total = total.saturating_add(1 + (c.len() / 64));
                    }
                    for (k, r) in items {
                        stack.push(k);
                        if r.is_empty() {
                            total = total.saturating_add(1);
                        } else {
                            stack.extend(r.iter());
                        }
                    }
                }
                Self::DBRow {
                    header,
                    columns,
                    raw_rle,
                } => {
                    total = total.saturating_add(1 + (raw_rle.len() / 64));
                    stack.push(header.as_ref());
                    for (c, v) in columns {
                        total = total.saturating_add(1 + (c.len() / 64));
                        stack.push(v);
                    }
                }
                Self::Object { type_name, args } => {
                    total = total.saturating_add(1 + (type_name.len() / 64));
                    stack.push(args.as_ref());
                }
                Self::SubStruct(args)
                | Self::SubStream(args)
                | Self::ChecksumedStream { body: args, .. } => {
                    total = total.saturating_add(1);
                    stack.push(args.as_ref());
                }
                Self::ObjectEx {
                    header, list, dict, ..
                } => {
                    total = total.saturating_add(1);
                    stack.push(header.as_ref());
                    stack.extend(list.iter());
                    for (k, v) in dict {
                        stack.push(k);
                        stack.push(v);
                    }
                }
            }
        }
        total
    }

    fn leaf_node_weight(&self) -> Option<usize> {
        match self {
            Self::None | Self::Bool(_) | Self::Integer(_) | Self::Real(_) => Some(1),
            Self::Buffer(b) | Self::Bytes(b) | Self::Picked(b) => Some(1 + (b.len() / 64)),
            Self::Token(s) | Self::String(s) => Some(1 + (s.len() / 64)),
            _ => None,
        }
    }

    fn has_pyrep_children(&self) -> bool {
        match self {
            Self::None
            | Self::Bool(_)
            | Self::Integer(_)
            | Self::Real(_)
            | Self::Buffer(_)
            | Self::Bytes(_)
            | Self::Token(_)
            | Self::String(_)
            | Self::Picked(_) => false,
            Self::Tuple(v) | Self::List(v) | Self::PySet(v) | Self::Set(v) => !v.is_empty(),
            Self::Dict(v) => !v.is_empty(),
            Self::KeyVal(m) => !m.is_empty(),
            Self::Rowset { rows, .. } | Self::TupleSet { rows, .. } => !rows.is_empty(),
            Self::IndexRowset { items, .. } => !items.is_empty(),
            Self::DBRow {
                header, columns, ..
            } => !matches!(header.as_ref(), Self::None) || !columns.is_empty(),
            Self::Object { args, .. }
            | Self::SubStruct(args)
            | Self::SubStream(args)
            | Self::ChecksumedStream { body: args, .. } => !matches!(args.as_ref(), Self::None),
            Self::ObjectEx {
                header, list, dict, ..
            } => !matches!(header.as_ref(), Self::None) || !list.is_empty() || !dict.is_empty(),
        }
    }

    fn drain_children_into(&mut self, out: &mut Vec<PyRep>) {
        match self {
            Self::None
            | Self::Bool(_)
            | Self::Integer(_)
            | Self::Real(_)
            | Self::Buffer(_)
            | Self::Bytes(_)
            | Self::Token(_)
            | Self::String(_)
            | Self::Picked(_) => {}
            Self::Tuple(v) | Self::List(v) | Self::PySet(v) | Self::Set(v) => out.append(v),
            Self::Dict(entries) => {
                for (k, v) in entries.drain(..) {
                    out.push(k);
                    out.push(v);
                }
            }
            Self::KeyVal(map) => {
                for (_, v) in map.drain() {
                    out.push(v);
                }
            }
            Self::Rowset { rows, .. } | Self::TupleSet { rows, .. } => {
                for mut row in rows.drain(..) {
                    out.append(&mut row);
                }
            }
            Self::IndexRowset { items, .. } => {
                for (k, mut row) in items.drain(..) {
                    out.push(k);
                    out.append(&mut row);
                }
            }
            Self::DBRow {
                header, columns, ..
            } => {
                if !matches!(header.as_ref(), Self::None) {
                    out.push(std::mem::replace(header.as_mut(), PyRep::None));
                }
                for (_, v) in columns.drain(..) {
                    out.push(v);
                }
            }
            Self::Object { args, .. }
            | Self::SubStruct(args)
            | Self::SubStream(args)
            | Self::ChecksumedStream { body: args, .. } => {
                if !matches!(args.as_ref(), Self::None) {
                    out.push(std::mem::replace(args.as_mut(), PyRep::None));
                }
            }
            Self::ObjectEx {
                header, list, dict, ..
            } => {
                if !matches!(header.as_ref(), Self::None) {
                    out.push(std::mem::replace(header.as_mut(), PyRep::None));
                }
                out.append(list);
                for (k, v) in dict.drain(..) {
                    out.push(k);
                    out.push(v);
                }
            }
        }
    }

    fn try_clone_shallow(&self) -> Option<Self> {
        match self {
            Self::None => Some(Self::None),
            Self::Bool(b) => Some(Self::Bool(*b)),
            Self::Integer(i) => Some(Self::Integer(*i)),
            Self::Real(r) => Some(Self::Real(*r)),
            Self::Buffer(b) => Some(Self::Buffer(b.clone())),
            Self::Bytes(b) => Some(Self::Bytes(b.clone())),
            Self::Token(s) => Some(Self::Token(s.clone())),
            Self::String(s) => Some(Self::String(s.clone())),
            Self::Picked(b) => Some(Self::Picked(b.clone())),
            Self::Tuple(v) if v.is_empty() => Some(Self::Tuple(Vec::new())),
            Self::List(v) if v.is_empty() => Some(Self::List(Vec::new())),
            Self::PySet(v) if v.is_empty() => Some(Self::PySet(Vec::new())),
            Self::Set(v) if v.is_empty() => Some(Self::Set(Vec::new())),
            Self::Dict(v) if v.is_empty() => Some(Self::Dict(Vec::new())),
            Self::KeyVal(m) if m.is_empty() => Some(Self::KeyVal(HashMap::new())),
            Self::Rowset { columns, rows } if rows.is_empty() => Some(Self::Rowset {
                columns: columns.clone(),
                rows: Vec::new(),
            }),
            Self::TupleSet { columns, rows } if rows.is_empty() => Some(Self::TupleSet {
                columns: columns.clone(),
                rows: Vec::new(),
            }),
            Self::IndexRowset {
                key_column,
                columns,
                items,
            } if items.is_empty() => Some(Self::IndexRowset {
                key_column: key_column.clone(),
                columns: columns.clone(),
                items: Vec::new(),
            }),
            _ => None,
        }
    }
    /// Drops this `PyRep` subtree using an explicit heap stack so arbitrarily deep synthetic trees
    /// can be freed in `O(1)` stack space without requiring `impl Drop for PyRep` (which would break
    /// owned pattern destructuring such as `let PyRep::List(items) = rep` across workspace callers).
    pub fn drop_iterative(mut self) {
        if !self.has_pyrep_children() {
            return;
        }
        let mut stack = Vec::new();
        self.drain_children_into(&mut stack);
        while let Some(mut node) = stack.pop() {
            node.drain_children_into(&mut stack);
        }
    }
}

enum PyRepCloneTask<'a> {
    Expand(&'a PyRep),
    BuildTuple(usize),
    BuildList(usize),
    BuildPySet(usize),
    BuildSet(usize),
    BuildDict(usize),
    BuildKeyVal(Vec<&'a str>),
    BuildRowset {
        columns: Vec<String>,
        row_lens: Vec<usize>,
        is_tuple_set: bool,
    },
    BuildIndexRowset {
        key_column: String,
        columns: Vec<String>,
        row_lens: Vec<usize>,
    },
    BuildDBRow {
        col_names: Vec<&'a str>,
        raw_rle: Vec<u8>,
    },
    BuildObject(String),
    BuildObjectEx {
        is_type_2: bool,
        list_len: usize,
        dict_len: usize,
    },
    BuildSubStruct,
    BuildChecksumedStream(u32),
    BuildSubStream,
}

fn push_clone_seq<'a>(
    work: &mut Vec<PyRepCloneTask<'a>>,
    items: &'a [PyRep],
    build: PyRepCloneTask<'a>,
) {
    work.push(build);
    for item in items.iter().rev() {
        work.push(PyRepCloneTask::Expand(item));
    }
}

fn clone_pairs_into_dict(results: &mut Vec<PyRep>, count: usize) -> Vec<(PyRep, PyRep)> {
    let start = results.len() - count * 2;
    let mut out = Vec::with_capacity(count);
    let mut iter = results.drain(start..);
    while let (Some(k), Some(v)) = (iter.next(), iter.next()) {
        out.push((k, v));
    }
    out
}

/// Iterative clone prevents call-stack exhaustion when cloning deep `PyRep` structures.
impl Clone for PyRep {
    fn clone(&self) -> Self {
        if let Some(shallow) = self.try_clone_shallow() {
            return shallow;
        }
        let mut work = vec![PyRepCloneTask::Expand(self)];
        let mut results: Vec<PyRep> = Vec::new();
        while let Some(task) = work.pop() {
            match task {
                PyRepCloneTask::Expand(node) => {
                    if let Some(shallow) = node.try_clone_shallow() {
                        results.push(shallow);
                        continue;
                    }
                    match node {
                        Self::Tuple(v) => {
                            push_clone_seq(&mut work, v, PyRepCloneTask::BuildTuple(v.len()));
                        }
                        Self::List(v) => {
                            push_clone_seq(&mut work, v, PyRepCloneTask::BuildList(v.len()));
                        }
                        Self::PySet(v) => {
                            push_clone_seq(&mut work, v, PyRepCloneTask::BuildPySet(v.len()));
                        }
                        Self::Set(v) => {
                            push_clone_seq(&mut work, v, PyRepCloneTask::BuildSet(v.len()));
                        }
                        Self::Dict(entries) => {
                            work.push(PyRepCloneTask::BuildDict(entries.len()));
                            for (k, v) in entries.iter().rev() {
                                work.push(PyRepCloneTask::Expand(v));
                                work.push(PyRepCloneTask::Expand(k));
                            }
                        }
                        Self::KeyVal(map) => {
                            let mut col_names = Vec::with_capacity(map.len());
                            let mut vals = Vec::with_capacity(map.len());
                            for (k, v) in map {
                                col_names.push(k.as_str());
                                vals.push(v);
                            }
                            work.push(PyRepCloneTask::BuildKeyVal(col_names));
                            for v in vals.into_iter().rev() {
                                work.push(PyRepCloneTask::Expand(v));
                            }
                        }
                        Self::Rowset { columns, rows } | Self::TupleSet { columns, rows } => {
                            let is_tuple_set = matches!(node, Self::TupleSet { .. });
                            let row_lens = rows.iter().map(Vec::len).collect();
                            work.push(PyRepCloneTask::BuildRowset {
                                columns: columns.clone(),
                                row_lens,
                                is_tuple_set,
                            });
                            for row in rows.iter().rev() {
                                for cell in row.iter().rev() {
                                    work.push(PyRepCloneTask::Expand(cell));
                                }
                            }
                        }
                        Self::IndexRowset {
                            key_column,
                            columns,
                            items,
                        } => {
                            let row_lens = items.iter().map(|(_, r)| r.len()).collect();
                            work.push(PyRepCloneTask::BuildIndexRowset {
                                key_column: key_column.clone(),
                                columns: columns.clone(),
                                row_lens,
                            });
                            for (k, row) in items.iter().rev() {
                                for cell in row.iter().rev() {
                                    work.push(PyRepCloneTask::Expand(cell));
                                }
                                work.push(PyRepCloneTask::Expand(k));
                            }
                        }
                        Self::DBRow {
                            header,
                            columns,
                            raw_rle,
                        } => {
                            let col_names = columns.iter().map(|(c, _)| c.as_str()).collect();
                            work.push(PyRepCloneTask::BuildDBRow {
                                col_names,
                                raw_rle: raw_rle.clone(),
                            });
                            for (_, v) in columns.iter().rev() {
                                work.push(PyRepCloneTask::Expand(v));
                            }
                            work.push(PyRepCloneTask::Expand(header.as_ref()));
                        }
                        Self::Object { type_name, args } => {
                            work.push(PyRepCloneTask::BuildObject(type_name.clone()));
                            work.push(PyRepCloneTask::Expand(args.as_ref()));
                        }
                        Self::ObjectEx {
                            is_type_2,
                            header,
                            list,
                            dict,
                        } => {
                            work.push(PyRepCloneTask::BuildObjectEx {
                                is_type_2: *is_type_2,
                                list_len: list.len(),
                                dict_len: dict.len(),
                            });
                            for (k, v) in dict.iter().rev() {
                                work.push(PyRepCloneTask::Expand(v));
                                work.push(PyRepCloneTask::Expand(k));
                            }
                            for item in list.iter().rev() {
                                work.push(PyRepCloneTask::Expand(item));
                            }
                            work.push(PyRepCloneTask::Expand(header.as_ref()));
                        }
                        Self::SubStruct(inner) => {
                            work.push(PyRepCloneTask::BuildSubStruct);
                            work.push(PyRepCloneTask::Expand(inner.as_ref()));
                        }
                        Self::ChecksumedStream { checksum, body } => {
                            work.push(PyRepCloneTask::BuildChecksumedStream(*checksum));
                            work.push(PyRepCloneTask::Expand(body.as_ref()));
                        }
                        Self::SubStream(inner) => {
                            work.push(PyRepCloneTask::BuildSubStream);
                            work.push(PyRepCloneTask::Expand(inner.as_ref()));
                        }
                        Self::None
                        | Self::Bool(_)
                        | Self::Integer(_)
                        | Self::Real(_)
                        | Self::Buffer(_)
                        | Self::Bytes(_)
                        | Self::Token(_)
                        | Self::String(_)
                        | Self::Picked(_) => unreachable!("handled by try_clone_shallow"),
                    }
                }
                PyRepCloneTask::BuildTuple(n) => {
                    let v = results.split_off(results.len() - n);
                    results.push(Self::Tuple(v));
                }
                PyRepCloneTask::BuildList(n) => {
                    let v = results.split_off(results.len() - n);
                    results.push(Self::List(v));
                }
                PyRepCloneTask::BuildPySet(n) => {
                    let v = results.split_off(results.len() - n);
                    results.push(Self::PySet(v));
                }
                PyRepCloneTask::BuildSet(n) => {
                    let v = results.split_off(results.len() - n);
                    results.push(Self::Set(v));
                }
                PyRepCloneTask::BuildDict(n) => {
                    let dict = clone_pairs_into_dict(&mut results, n);
                    results.push(Self::Dict(dict));
                }
                PyRepCloneTask::BuildKeyVal(col_names) => {
                    let start = results.len() - col_names.len();
                    let mut map = HashMap::with_capacity(col_names.len());
                    for (k, v) in col_names.into_iter().zip(results.drain(start..)) {
                        map.insert(k.to_owned(), v);
                    }
                    results.push(Self::KeyVal(map));
                }
                PyRepCloneTask::BuildRowset {
                    columns,
                    row_lens,
                    is_tuple_set,
                } => {
                    let total: usize = row_lens.iter().sum();
                    let start = results.len() - total;
                    let mut iter = results.drain(start..);
                    let mut rows = Vec::with_capacity(row_lens.len());
                    for len in row_lens {
                        rows.push(iter.by_ref().take(len).collect());
                    }
                    drop(iter);
                    let rep = if is_tuple_set {
                        Self::TupleSet { columns, rows }
                    } else {
                        Self::Rowset { columns, rows }
                    };
                    results.push(rep);
                }
                PyRepCloneTask::BuildIndexRowset {
                    key_column,
                    columns,
                    row_lens,
                } => {
                    let total: usize = row_lens.iter().map(|len| 1 + len).sum();
                    let start = results.len() - total;
                    let mut iter = results.drain(start..);
                    let mut items = Vec::with_capacity(row_lens.len());
                    for len in row_lens {
                        let k = iter.next().expect("index rowset key present");
                        let row: Vec<PyRep> = iter.by_ref().take(len).collect();
                        items.push((k, row));
                    }
                    drop(iter);
                    results.push(Self::IndexRowset {
                        key_column,
                        columns,
                        items,
                    });
                }
                PyRepCloneTask::BuildDBRow { col_names, raw_rle } => {
                    let start = results.len() - 1 - col_names.len();
                    let mut iter = results.drain(start..);
                    let header = Box::new(iter.next().expect("dbrow header present"));
                    let columns = col_names
                        .into_iter()
                        .zip(iter)
                        .map(|(name, val)| (name.to_owned(), val))
                        .collect();
                    results.push(Self::DBRow {
                        header,
                        columns,
                        raw_rle,
                    });
                }
                PyRepCloneTask::BuildObject(type_name) => {
                    let args = Box::new(results.pop().expect("object args present"));
                    results.push(Self::Object { type_name, args });
                }
                PyRepCloneTask::BuildObjectEx {
                    is_type_2,
                    list_len,
                    dict_len,
                } => {
                    let start = results.len() - (1 + list_len + dict_len * 2);
                    let mut iter = results.drain(start..);
                    let header = Box::new(iter.next().expect("objectex header present"));
                    let list: Vec<PyRep> = iter.by_ref().take(list_len).collect();
                    let mut dict = Vec::with_capacity(dict_len);
                    while let (Some(k), Some(v)) = (iter.next(), iter.next()) {
                        dict.push((k, v));
                    }
                    drop(iter);
                    results.push(Self::ObjectEx {
                        is_type_2,
                        header,
                        list,
                        dict,
                    });
                }
                PyRepCloneTask::BuildSubStruct => {
                    let inner = Box::new(results.pop().expect("substruct inner present"));
                    results.push(Self::SubStruct(inner));
                }
                PyRepCloneTask::BuildChecksumedStream(checksum) => {
                    let body = Box::new(results.pop().expect("checksumed body present"));
                    results.push(Self::ChecksumedStream { checksum, body });
                }
                PyRepCloneTask::BuildSubStream => {
                    let inner = Box::new(results.pop().expect("substream inner present"));
                    results.push(Self::SubStream(inner));
                }
            }
        }
        debug_assert_eq!(results.len(), 1);
        results.pop().expect("root cloned")
    }
}

fn try_eq_shallow(lhs: &PyRep, rhs: &PyRep) -> Option<bool> {
    match (lhs, rhs) {
        (PyRep::None, PyRep::None) => Some(true),
        (PyRep::Bool(a), PyRep::Bool(b)) => Some(a == b),
        (PyRep::Integer(a), PyRep::Integer(b)) => Some(a == b),
        (PyRep::Real(a), PyRep::Real(b)) => Some(a == b),
        (PyRep::Buffer(a), PyRep::Buffer(b))
        | (PyRep::Bytes(a), PyRep::Bytes(b))
        | (PyRep::Picked(a), PyRep::Picked(b)) => Some(a == b),
        (PyRep::Token(a), PyRep::Token(b)) | (PyRep::String(a), PyRep::String(b)) => Some(a == b),
        _ if std::mem::discriminant(lhs) != std::mem::discriminant(rhs) => Some(false),
        _ => None,
    }
}

/// Iterative equality prevents call-stack exhaustion on deep `PyRep` trees.
impl PartialEq for PyRep {
    fn eq(&self, other: &Self) -> bool {
        if let Some(res) = try_eq_shallow(self, other) {
            return res;
        }
        let mut stack = vec![(self, other)];
        while let Some((lhs, rhs)) = stack.pop() {
            if let Some(res) = try_eq_shallow(lhs, rhs) {
                if !res {
                    return false;
                }
                continue;
            }
            match (lhs, rhs) {
                (Self::None, Self::None) => {}
                (Self::Bool(a), Self::Bool(b)) => {
                    if a != b {
                        return false;
                    }
                }
                (Self::Integer(a), Self::Integer(b)) => {
                    if a != b {
                        return false;
                    }
                }
                (Self::Real(a), Self::Real(b)) => {
                    if a != b {
                        return false;
                    }
                }
                (Self::Buffer(a), Self::Buffer(b))
                | (Self::Bytes(a), Self::Bytes(b))
                | (Self::Picked(a), Self::Picked(b)) => {
                    if a != b {
                        return false;
                    }
                }
                (Self::Token(a), Self::Token(b)) | (Self::String(a), Self::String(b)) => {
                    if a != b {
                        return false;
                    }
                }
                (Self::Tuple(a), Self::Tuple(b))
                | (Self::List(a), Self::List(b))
                | (Self::PySet(a), Self::PySet(b))
                | (Self::Set(a), Self::Set(b)) => {
                    if a.len() != b.len() {
                        return false;
                    }
                    stack.extend(a.iter().zip(b.iter()).rev());
                }
                (Self::Dict(a), Self::Dict(b)) => {
                    if a.len() != b.len() {
                        return false;
                    }
                    for ((ak, av), (bk, bv)) in a.iter().zip(b.iter()).rev() {
                        stack.push((av, bv));
                        stack.push((ak, bk));
                    }
                }
                (Self::KeyVal(a), Self::KeyVal(b)) => {
                    if a.len() != b.len() {
                        return false;
                    }
                    for (k, av) in a {
                        let Some(bv) = b.get(k) else {
                            return false;
                        };
                        stack.push((av, bv));
                    }
                }
                (
                    Self::Rowset {
                        columns: ac,
                        rows: ar,
                    },
                    Self::Rowset {
                        columns: bc,
                        rows: br,
                    },
                )
                | (
                    Self::TupleSet {
                        columns: ac,
                        rows: ar,
                    },
                    Self::TupleSet {
                        columns: bc,
                        rows: br,
                    },
                ) => {
                    if ac != bc || ar.len() != br.len() {
                        return false;
                    }
                    for (row_a, row_b) in ar.iter().zip(br.iter()).rev() {
                        if row_a.len() != row_b.len() {
                            return false;
                        }
                        stack.extend(row_a.iter().zip(row_b.iter()).rev());
                    }
                }
                (
                    Self::IndexRowset {
                        key_column: akc,
                        columns: ac,
                        items: ai,
                    },
                    Self::IndexRowset {
                        key_column: bkc,
                        columns: bc,
                        items: bi,
                    },
                ) => {
                    if akc != bkc || ac != bc || ai.len() != bi.len() {
                        return false;
                    }
                    for ((ak, ar), (bk, br)) in ai.iter().zip(bi.iter()).rev() {
                        if ar.len() != br.len() {
                            return false;
                        }
                        stack.extend(ar.iter().zip(br.iter()).rev());
                        stack.push((ak, bk));
                    }
                }
                (
                    Self::DBRow {
                        header: ah,
                        columns: ac,
                        raw_rle: arle,
                    },
                    Self::DBRow {
                        header: bh,
                        columns: bc,
                        raw_rle: brle,
                    },
                ) => {
                    if arle != brle || ac.len() != bc.len() {
                        return false;
                    }
                    for ((an, av), (bn, bv)) in ac.iter().zip(bc.iter()).rev() {
                        if an != bn {
                            return false;
                        }
                        stack.push((av, bv));
                    }
                    stack.push((ah.as_ref(), bh.as_ref()));
                }
                (
                    Self::Object {
                        type_name: at,
                        args: aa,
                    },
                    Self::Object {
                        type_name: bt,
                        args: ba,
                    },
                ) => {
                    if at != bt {
                        return false;
                    }
                    stack.push((aa.as_ref(), ba.as_ref()));
                }
                (
                    Self::ObjectEx {
                        is_type_2: a2,
                        header: ah,
                        list: al,
                        dict: ad,
                    },
                    Self::ObjectEx {
                        is_type_2: b2,
                        header: bh,
                        list: bl,
                        dict: bd,
                    },
                ) => {
                    if a2 != b2 || al.len() != bl.len() || ad.len() != bd.len() {
                        return false;
                    }
                    for ((ak, av), (bk, bv)) in ad.iter().zip(bd.iter()).rev() {
                        stack.push((av, bv));
                        stack.push((ak, bk));
                    }
                    stack.extend(al.iter().zip(bl.iter()).rev());
                    stack.push((ah.as_ref(), bh.as_ref()));
                }
                (Self::SubStruct(a), Self::SubStruct(b))
                | (Self::SubStream(a), Self::SubStream(b)) => {
                    stack.push((a.as_ref(), b.as_ref()));
                }
                (
                    Self::ChecksumedStream {
                        checksum: ac,
                        body: ab,
                    },
                    Self::ChecksumedStream {
                        checksum: bc,
                        body: bb,
                    },
                ) => {
                    if ac != bc {
                        return false;
                    }
                    stack.push((ab.as_ref(), bb.as_ref()));
                }
                _ => return false,
            }
        }
        true
    }
}

enum PyRepDebugOp<'a> {
    Rep(&'a PyRep),
    Lit(&'static str),
    StrDbg(&'a str),
    BytesDbg(&'a [u8]),
    StrSliceDbg(&'a [String]),
    BoolVal(bool),
    U32Val(u32),
}

fn push_debug_list<'a>(
    stack: &mut Vec<PyRepDebugOp<'a>>,
    prefix: &'static str,
    items: &'a [PyRep],
    suffix: &'static str,
) {
    stack.push(PyRepDebugOp::Lit(suffix));
    for (idx, item) in items.iter().enumerate().rev() {
        if idx + 1 < items.len() {
            stack.push(PyRepDebugOp::Lit(", "));
        }
        stack.push(PyRepDebugOp::Rep(item));
    }
    stack.push(PyRepDebugOp::Lit(prefix));
}

fn push_debug_pairs<'a>(
    stack: &mut Vec<PyRepDebugOp<'a>>,
    prefix: &'static str,
    pairs: &'a [(PyRep, PyRep)],
    suffix: &'static str,
) {
    stack.push(PyRepDebugOp::Lit(suffix));
    for (idx, (k, v)) in pairs.iter().enumerate().rev() {
        if idx + 1 < pairs.len() {
            stack.push(PyRepDebugOp::Lit(", "));
        }
        stack.push(PyRepDebugOp::Lit(")"));
        stack.push(PyRepDebugOp::Rep(v));
        stack.push(PyRepDebugOp::Lit(", "));
        stack.push(PyRepDebugOp::Rep(k));
        stack.push(PyRepDebugOp::Lit("("));
    }
    stack.push(PyRepDebugOp::Lit(prefix));
}

fn push_debug_rows<'a>(
    stack: &mut Vec<PyRepDebugOp<'a>>,
    rows: &'a [Vec<PyRep>],
) {
    stack.push(PyRepDebugOp::Lit("]"));
    for (r_idx, row) in rows.iter().enumerate().rev() {
        if r_idx + 1 < rows.len() {
            stack.push(PyRepDebugOp::Lit(", "));
        }
        push_debug_list(stack, "[", row, "]");
    }
    stack.push(PyRepDebugOp::Lit("["));
}

/// Iterative `Debug` formatter prevents panic/assertion stack overflow on deep `PyRep` trees.
impl fmt::Debug for PyRep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut stack = vec![PyRepDebugOp::Rep(self)];
        while let Some(op) = stack.pop() {
            match op {
                PyRepDebugOp::Lit(s) => f.write_str(s)?,
                PyRepDebugOp::StrDbg(s) => fmt::Debug::fmt(s, f)?,
                PyRepDebugOp::BytesDbg(b) => fmt::Debug::fmt(b, f)?,
                PyRepDebugOp::StrSliceDbg(ss) => fmt::Debug::fmt(ss, f)?,
                PyRepDebugOp::BoolVal(b) => fmt::Debug::fmt(&b, f)?,
                PyRepDebugOp::U32Val(u) => fmt::Debug::fmt(&u, f)?,
                PyRepDebugOp::Rep(node) => match node {
                    Self::None => f.write_str("None")?,
                    Self::Bool(b) => write!(f, "Bool({b:?})")?,
                    Self::Integer(i) => write!(f, "Integer({i:?})")?,
                    Self::Real(r) => write!(f, "Real({r:?})")?,
                    Self::Buffer(b) => write!(f, "Buffer({b:?})")?,
                    Self::Bytes(b) => write!(f, "Bytes({b:?})")?,
                    Self::Token(s) => write!(f, "Token({s:?})")?,
                    Self::String(s) => write!(f, "String({s:?})")?,
                    Self::Picked(b) => write!(f, "Picked({b:?})")?,
                    Self::Tuple(v) => push_debug_list(&mut stack, "Tuple([", v, "])"),
                    Self::List(v) => push_debug_list(&mut stack, "List([", v, "])"),
                    Self::PySet(v) => push_debug_list(&mut stack, "PySet([", v, "])"),
                    Self::Set(v) => push_debug_list(&mut stack, "Set([", v, "])"),
                    Self::Dict(pairs) => push_debug_pairs(&mut stack, "Dict([", pairs, "])"),
                    Self::KeyVal(map) => {
                        stack.push(PyRepDebugOp::Lit("})"));
                        let entries: Vec<(&str, &PyRep)> =
                            map.iter().map(|(k, v)| (k.as_str(), v)).collect();
                        for (idx, (k, v)) in entries.iter().enumerate().rev() {
                            if idx + 1 < entries.len() {
                                stack.push(PyRepDebugOp::Lit(", "));
                            }
                            stack.push(PyRepDebugOp::Rep(v));
                            stack.push(PyRepDebugOp::Lit(": "));
                            stack.push(PyRepDebugOp::StrDbg(k));
                        }
                        stack.push(PyRepDebugOp::Lit("KeyVal({"));
                    }
                    Self::Rowset { columns, rows } => {
                        stack.push(PyRepDebugOp::Lit(" }"));
                        push_debug_rows(&mut stack, rows);
                        stack.push(PyRepDebugOp::Lit(", rows: "));
                        stack.push(PyRepDebugOp::StrSliceDbg(columns));
                        stack.push(PyRepDebugOp::Lit("Rowset { columns: "));
                    }
                    Self::TupleSet { columns, rows } => {
                        stack.push(PyRepDebugOp::Lit(" }"));
                        push_debug_rows(&mut stack, rows);
                        stack.push(PyRepDebugOp::Lit(", rows: "));
                        stack.push(PyRepDebugOp::StrSliceDbg(columns));
                        stack.push(PyRepDebugOp::Lit("TupleSet { columns: "));
                    }
                    Self::IndexRowset {
                        key_column,
                        columns,
                        items,
                    } => {
                        stack.push(PyRepDebugOp::Lit("] }"));
                        for (idx, (k, row)) in items.iter().enumerate().rev() {
                            if idx + 1 < items.len() {
                                stack.push(PyRepDebugOp::Lit(", "));
                            }
                            stack.push(PyRepDebugOp::Lit(")"));
                            push_debug_list(&mut stack, "[", row, "]");
                            stack.push(PyRepDebugOp::Lit(", "));
                            stack.push(PyRepDebugOp::Rep(k));
                            stack.push(PyRepDebugOp::Lit("("));
                        }
                        stack.push(PyRepDebugOp::Lit(", items: ["));
                        stack.push(PyRepDebugOp::StrSliceDbg(columns));
                        stack.push(PyRepDebugOp::Lit(", columns: "));
                        stack.push(PyRepDebugOp::StrDbg(key_column));
                        stack.push(PyRepDebugOp::Lit("IndexRowset { key_column: "));
                    }
                    Self::DBRow {
                        header,
                        columns,
                        raw_rle,
                    } => {
                        stack.push(PyRepDebugOp::Lit(" }"));
                        stack.push(PyRepDebugOp::BytesDbg(raw_rle));
                        stack.push(PyRepDebugOp::Lit("], raw_rle: "));
                        for (idx, (c, v)) in columns.iter().enumerate().rev() {
                            if idx + 1 < columns.len() {
                                stack.push(PyRepDebugOp::Lit(", "));
                            }
                            stack.push(PyRepDebugOp::Lit(")"));
                            stack.push(PyRepDebugOp::Rep(v));
                            stack.push(PyRepDebugOp::Lit(", "));
                            stack.push(PyRepDebugOp::StrDbg(c));
                            stack.push(PyRepDebugOp::Lit("("));
                        }
                        stack.push(PyRepDebugOp::Lit(", columns: ["));
                        stack.push(PyRepDebugOp::Rep(header.as_ref()));
                        stack.push(PyRepDebugOp::Lit("DBRow { header: "));
                    }
                    Self::Object { type_name, args } => {
                        stack.push(PyRepDebugOp::Lit(" }"));
                        stack.push(PyRepDebugOp::Rep(args.as_ref()));
                        stack.push(PyRepDebugOp::Lit(", args: "));
                        stack.push(PyRepDebugOp::StrDbg(type_name));
                        stack.push(PyRepDebugOp::Lit("Object { type_name: "));
                    }
                    Self::ObjectEx {
                        is_type_2,
                        header,
                        list,
                        dict,
                    } => {
                        stack.push(PyRepDebugOp::Lit(" }"));
                        push_debug_pairs(&mut stack, ", dict: [", dict, "]");
                        push_debug_list(&mut stack, ", list: [", list, "]");
                        stack.push(PyRepDebugOp::Rep(header.as_ref()));
                        stack.push(PyRepDebugOp::Lit(", header: "));
                        stack.push(PyRepDebugOp::BoolVal(*is_type_2));
                        stack.push(PyRepDebugOp::Lit("ObjectEx { is_type_2: "));
                    }
                    Self::SubStruct(inner) => {
                        stack.push(PyRepDebugOp::Lit(")"));
                        stack.push(PyRepDebugOp::Rep(inner.as_ref()));
                        stack.push(PyRepDebugOp::Lit("SubStruct("));
                    }
                    Self::ChecksumedStream { checksum, body } => {
                        stack.push(PyRepDebugOp::Lit(" }"));
                        stack.push(PyRepDebugOp::Rep(body.as_ref()));
                        stack.push(PyRepDebugOp::Lit(", body: "));
                        stack.push(PyRepDebugOp::U32Val(*checksum));
                        stack.push(PyRepDebugOp::Lit("ChecksumedStream { checksum: "));
                    }
                    Self::SubStream(inner) => {
                        stack.push(PyRepDebugOp::Lit(")"));
                        stack.push(PyRepDebugOp::Rep(inner.as_ref()));
                        stack.push(PyRepDebugOp::Lit("SubStream("));
                    }
                },
            }
        }
        Ok(())
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
    "objectCaching", // rpc-lint: allow canonical eveMarshal 195-entry wire STRING_TABLE
    "objectCaching.CachedObject",
    "OnChatJoin", // rpc-lint: allow canonical eveMarshal 195-entry wire STRING_TABLE
    "OnChatLeave", // rpc-lint: allow canonical eveMarshal 195-entry wire STRING_TABLE
    "OnChatSpeak", // rpc-lint: allow canonical eveMarshal 195-entry wire STRING_TABLE
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
    "LSC", // rpc-lint: allow canonical eveMarshal 195-entry wire STRING_TABLE
    "LaunchMissile",
    "LeaveChannel",
    "OID+",
    "OID-",
    "OnAggressionChange", // rpc-lint: allow canonical eveMarshal 195-entry wire STRING_TABLE
    "OnCharGangChange", // rpc-lint: allow canonical eveMarshal 195-entry wire STRING_TABLE
    "OnCharNoLongerInStation",
    "OnCharNowInStation",
    "OnDamageMessage",
    "OnDamageStateChange",
    "OnEffectHit", // rpc-lint: allow canonical eveMarshal 195-entry wire STRING_TABLE
    "OnGangDamageStateChange", // rpc-lint: allow canonical eveMarshal 195-entry wire STRING_TABLE
    "OnLSC", // rpc-lint: allow canonical eveMarshal 195-entry wire STRING_TABLE
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// Validates a 4-byte little-endian frame length header (`1..=MAX_FRAME_SIZE`) before buffer allocation.
pub fn validate_frame_length(len_buf: [u8; 4]) -> Result<usize, ProtoError> {
    let frame_len = u32::from_le_bytes(len_buf) as usize;
    if frame_len == 0 {
        return Err(ProtoError::UnexpectedEof);
    }
    if frame_len > MAX_FRAME_SIZE {
        return Err(ProtoError::FrameTooLarge(frame_len));
    }
    Ok(frame_len)
}

impl BridgeEnvelope {
    /// Serializes a `BridgeEnvelope` as a length-prefixed (`u32` little-endian) JSON wire frame.
    pub fn to_wire_bytes(&self) -> Result<Vec<u8>, ProtoError> {
        let json_bytes = serde_json::to_vec(self)?;
        if json_bytes.len() > MAX_FRAME_SIZE {
            return Err(ProtoError::FrameTooLarge(json_bytes.len()));
        }
        let mut frame = Vec::with_capacity(4 + json_bytes.len());
        frame.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        frame.extend_from_slice(&json_bytes);
        Ok(frame)
    }

    /// Decodes a length-prefixed (`u32` little-endian) or raw JSON/MessagePack `BridgeEnvelope` wire frame.
    pub fn from_wire_bytes(input: &[u8]) -> Result<Self, ProtoError> {
        if input.is_empty() {
            return Err(ProtoError::UnexpectedEof);
        }
        if input.len() > MAX_FRAME_SIZE {
            return Err(ProtoError::FrameTooLarge(input.len()));
        }
        let payload = if input.len() >= 4 {
            let declared_len = u32::from_le_bytes([input[0], input[1], input[2], input[3]]) as usize;
            if declared_len > 0 && declared_len + 4 == input.len() {
                &input[4..]
            } else {
                input
            }
        } else {
            input
        };
        match serde_json::from_slice::<Self>(payload) {
            Ok(env) => Ok(env),
            Err(json_err) => rmp_serde::from_slice::<Self>(payload).map_err(|_| ProtoError::from(json_err)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BridgeRpcError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_error: Option<Box<ClientError>>,
}

impl BridgeRpcError {
    pub fn user_error(code: impl Into<String>) -> Self {
        let code = code.into();
        Self {
            message: code.clone(),
            code,
            client_error: None,
        }
    }
}
/// Synthetic protobuf test fixture frame direction (`Request`, `Response`, `Notice`).
/// Note: This is a synthetic wire-framing fixture for property/fuzz testing of protobuf primitives
/// (varints, zigzag `sint64`, length-delimited strings/bytes); real client `eveProto` gateway
/// schemas are tracked under `Q-1`/`Q-2`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GatewayFrameKind {
    Request = 1,
    Response = 2,
    Notice = 3,
}

impl GatewayFrameKind {
    pub fn from_u64(v: u64) -> Option<Self> {
        match v {
            1 => Some(Self::Request),
            2 => Some(Self::Response),
            3 => Some(Self::Notice),
            _ => None,
        }
    }
}

/// Synthetic protobuf wire test fixture exercising protobuf varint, zigzag `sint64`, and
/// length-delimited byte/string bounds (`MAX_FRAME_SIZE`, `MAX_COLLECTION_LEN`).
/// Does not represent the authentic client `publicGatewaySvc` protobuf descriptor (see `Q-1`/`Q-2`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayFrame {
    pub kind: GatewayFrameKind,
    pub correlation_id: u64,
    pub type_name: String,
    pub status_code: u32,
    pub actor_character_id: Option<i64>,
    pub idempotency_key: Option<String>,
    pub payload: Vec<u8>,
}

impl GatewayFrame {
    /// Encodes this synthetic `GatewayFrame` fixture using deterministic binary Protobuf wire framing
    /// (field 1: `kind` varint, field 2: `correlation_id` varint, field 3: `type_name` string,
    /// field 4: `status_code` varint, field 5: `actor_character_id` sint64 zigzag varint,
    /// field 6: `idempotency_key` string, field 7: `payload` bytes).
    pub fn encode_protobuf_wire(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + self.type_name.len() + self.payload.len());
        out.push(0x08);
        write_pb_varint(&mut out, self.kind as u64);

        out.push(0x10);
        write_pb_varint(&mut out, self.correlation_id);

        out.push(0x1A);
        write_pb_varint(&mut out, self.type_name.len() as u64);
        out.extend_from_slice(self.type_name.as_bytes());

        out.push(0x20);
        write_pb_varint(&mut out, self.status_code as u64);

        if let Some(char_id) = self.actor_character_id {
            out.push(0x28);
            let zigzag = ((char_id << 1) ^ (char_id >> 63)) as u64;
            write_pb_varint(&mut out, zigzag);
        }

        if let Some(ref key) = self.idempotency_key {
            out.push(0x32);
            write_pb_varint(&mut out, key.len() as u64);
            out.extend_from_slice(key.as_bytes());
        }

        out.push(0x3A);
        write_pb_varint(&mut out, self.payload.len() as u64);
        out.extend_from_slice(&self.payload);
        out
    }

    /// Decodes a binary Protobuf wire `GatewayFrame` with strict bounds checks.
    pub fn decode_protobuf_wire(mut input: &[u8]) -> Result<Self, ProtoError> {
        if input.is_empty() {
            return Err(ProtoError::UnexpectedEof);
        }
        if input.len() > MAX_FRAME_SIZE {
            return Err(ProtoError::FrameTooLarge(input.len()));
        }

        let mut kind = None;
        let mut correlation_id = 0u64;
        let mut type_name = String::new();
        let mut status_code = 0u32;
        let mut actor_character_id = None;
        let mut idempotency_key = None;
        let mut payload = Vec::new();

        while !input.is_empty() {
            let tag = read_pb_varint(&mut input)?;
            let field_num = tag >> 3;
            let wire_type = tag & 0x07;
            match (field_num, wire_type) {
                (1, 0) => {
                    let v = read_pb_varint(&mut input)?;
                    kind = Some(
                        GatewayFrameKind::from_u64(v)
                            .ok_or(ProtoError::UnsupportedOpcode(v as u8))?,
                    );
                }
                (2, 0) => {
                    correlation_id = read_pb_varint(&mut input)?;
                }
                (3, 2) => {
                    let bytes = read_pb_bytes(&mut input)?;
                    type_name = String::from_utf8(bytes)?;
                }
                (4, 0) => {
                    let v = read_pb_varint(&mut input)?;
                    status_code = u32::try_from(v).map_err(|_| ProtoError::UnexpectedEof)?;
                }
                (5, 0) => {
                    let z = read_pb_varint(&mut input)?;
                    let decoded = ((z >> 1) as i64) ^ (-((z & 1) as i64));
                    actor_character_id = Some(decoded);
                }
                (6, 2) => {
                    let bytes = read_pb_bytes(&mut input)?;
                    idempotency_key = Some(String::from_utf8(bytes)?);
                }
                (7, 2) => {
                    payload = read_pb_bytes(&mut input)?;
                }
                _ => return Err(ProtoError::UnsupportedOpcode(tag as u8)),
            }
        }

        Ok(Self {
            kind: kind.ok_or(ProtoError::UnexpectedEof)?,
            correlation_id,
            type_name,
            status_code,
            actor_character_id,
            idempotency_key,
            payload,
        })
    }

    /// Projects this `GatewayFrame` into a `PyRep` tuple for `publicGatewaySvc` over `machoNet`.
    pub fn to_pyrep(&self) -> PyRep {
        PyRep::Tuple(vec![
            PyRep::Integer(self.kind as i64),
            PyRep::Integer(self.correlation_id as i64),
            PyRep::String(self.type_name.clone()),
            PyRep::Integer(self.status_code as i64),
            self.actor_character_id
                .map(PyRep::Integer)
                .unwrap_or(PyRep::None),
            self.idempotency_key
                .as_ref()
                .cloned()
                .map(PyRep::String)
                .unwrap_or(PyRep::None),
            PyRep::Buffer(self.payload.clone()),
        ])
    }

    /// Reconstructs a `GatewayFrame` from its `PyRep` tuple representation.
    pub fn from_pyrep(rep: &PyRep) -> Result<Self, ProtoError> {
        let PyRep::Tuple(items) = rep else {
            return Err(ProtoError::UnexpectedEof);
        };
        if items.len() != 7 {
            return Err(ProtoError::UnexpectedEof);
        }
        let kind_u64 = items[0].as_i64().ok_or(ProtoError::UnexpectedEof)? as u64;
        let kind = GatewayFrameKind::from_u64(kind_u64)
            .ok_or(ProtoError::UnsupportedOpcode(kind_u64 as u8))?;
        let correlation_id = items[1].as_i64().ok_or(ProtoError::UnexpectedEof)? as u64;
        let type_name = items[2]
            .as_str()
            .ok_or(ProtoError::UnexpectedEof)?
            .to_string();
        let status_code =
            u32::try_from(items[3].as_i64().ok_or(ProtoError::UnexpectedEof)?)
                .map_err(|_| ProtoError::UnexpectedEof)?;
        let actor_character_id = match &items[4] {
            PyRep::None => None,
            v => Some(v.as_i64().ok_or(ProtoError::UnexpectedEof)?),
        };
        let idempotency_key = match &items[5] {
            PyRep::None => None,
            v => Some(v.as_str().ok_or(ProtoError::UnexpectedEof)?.to_string()),
        };
        let payload = match &items[6] {
            PyRep::Buffer(b) | PyRep::Bytes(b) => b.clone(),
            _ => return Err(ProtoError::UnexpectedEof),
        };
        Ok(Self {
            kind,
            correlation_id,
            type_name,
            status_code,
            actor_character_id,
            idempotency_key,
            payload,
        })
    }
}

fn write_pb_varint(out: &mut Vec<u8>, mut val: u64) {
    while val >= 0x80 {
        out.push((val as u8) | 0x80);
        val >>= 7;
    }
    out.push(val as u8);
}

fn read_pb_varint(input: &mut &[u8]) -> Result<u64, ProtoError> {
    let mut result = 0u64;
    let mut shift = 0u32;
    loop {
        if input.is_empty() || shift >= 64 {
            return Err(ProtoError::UnexpectedEof);
        }
        let b = input[0];
        *input = &input[1..];
        result |= ((b & 0x7F) as u64) << shift;
        if (b & 0x80) == 0 {
            return Ok(result);
        }
        shift += 7;
    }
}

fn read_pb_bytes(input: &mut &[u8]) -> Result<Vec<u8>, ProtoError> {
    let len = read_pb_varint(input)? as usize;
    if input.len() < len {
        return Err(ProtoError::UnexpectedEof);
    }
    let out = input[..len].to_vec();
    *input = &input[len..];
    Ok(out)
}

/// Conversion trait for strongly-typed V24 client wire replies (`RFC-3 C-9`, `#933`).
pub trait ToClient {
    fn into_client_rep(self) -> PyRep;
}

impl ToClient for PyRep {
    fn into_client_rep(self) -> PyRep {
        self
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)] mod proto_lib_extra_tests;
#[cfg(test)] mod quasar_extra_unit_tests;
#[cfg(test)] mod proto_deep_extra_unit_tests;
#[cfg(test)] mod proto_cov_unit_tests;
#[cfg(test)]
mod marshal_dbrow_and_quasar_edge_tests;

