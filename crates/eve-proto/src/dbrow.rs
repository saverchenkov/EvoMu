//! `dbrow` — Canonical `DBRowDescriptor`, `PyPackedRow` (`blue.DBRow`) RLE + bit-field codec,
//! and typed rowset container builders (`dbutil.CRowset`, `dbutil.CIndexedRowset`,
//! `util.Rowset`, `util.IndexRowset`, `PackedRowList`, `PackedRowListTuple`).
//!
//! Reference: Crucible `EVEUnmarshal.cpp:506-680`, `EVEMarshal.cpp:320-599`,
//! `EVEDBUtils.cpp:316-475`, `PyDatabase.cpp:112-220`.

#![forbid(unsafe_code)]

use crate::{PyRep, MAX_COLLECTION_LEN};
use serde::{Deserialize, Serialize};

/// Canonical EVE `DBTYPE` column type codes (`EVEDB.h` / `blue.DBRowDescriptor`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DbColumnType {
    Empty = 0,
    I2 = 2,
    I4 = 3,
    R4 = 4,
    R8 = 5,
    Cy = 6,
    Bool = 11,
    I1 = 16,
    Ui1 = 17,
    Ui2 = 18,
    Ui4 = 19,
    I8 = 20,
    Ui8 = 21,
    FileTime = 64,
    Bytes = 128,
    Str = 129,
    WStr = 130,
}

impl DbColumnType {
    pub fn from_i64(val: i64) -> Option<Self> {
        match val {
            0 => Some(Self::Empty),
            2 => Some(Self::I2),
            3 => Some(Self::I4),
            4 => Some(Self::R4),
            5 => Some(Self::R8),
            6 => Some(Self::Cy),
            11 => Some(Self::Bool),
            16 => Some(Self::I1),
            17 => Some(Self::Ui1),
            18 => Some(Self::Ui2),
            19 => Some(Self::Ui4),
            20 => Some(Self::I8),
            21 => Some(Self::Ui8),
            64 => Some(Self::FileTime),
            128 => Some(Self::Bytes),
            129 => Some(Self::Str),
            130 => Some(Self::WStr),
            _ => None,
        }
    }

    pub const fn as_i64(self) -> i64 {
        self as u8 as i64
    }

    pub const fn bit_width(self) -> usize {
        dbtype_bit_width(self as u8 as i64)
    }

    /// Normalizes a `PyRep` value to the decoded representation produced by `unpack_packed_row_columns`
    /// and trailing stream unmarshaling so `encode -> decode` is a strict fixed point.
    pub fn normalize_value(self, val: PyRep) -> PyRep {
        if matches!(val, PyRep::None) {
            return PyRep::None;
        }
        match self {
            Self::I8 | Self::Ui8 | Self::FileTime => {
                PyRep::Integer(val.as_i64().unwrap_or(0))
            }
            Self::Cy => match val {
                PyRep::Integer(v) => PyRep::Integer(v),
                PyRep::Real(f) => PyRep::Integer((f * 10_000.0).round() as i64),
                _ => PyRep::Integer(0),
            },
            Self::I4 => PyRep::Integer((val.as_i64().unwrap_or(0) as i32) as i64),
            Self::Ui4 => PyRep::Integer((val.as_i64().unwrap_or(0) as u32) as i64),
            Self::I2 => PyRep::Integer((val.as_i64().unwrap_or(0) as i16) as i64),
            Self::Ui2 => PyRep::Integer((val.as_i64().unwrap_or(0) as u16) as i64),
            Self::I1 => PyRep::Integer((val.as_i64().unwrap_or(0) as i8) as i64),
            Self::Ui1 => PyRep::Integer((val.as_i64().unwrap_or(0) as u8) as i64),
            Self::R8 => PyRep::Real(val.as_f64().unwrap_or(0.0)),
            Self::R4 => PyRep::Real((val.as_f64().unwrap_or(0.0) as f32) as f64),
            Self::Bool => PyRep::Bool(val.as_bool().unwrap_or(false)),
            Self::Bytes => match val {
                PyRep::Bytes(b) | PyRep::Buffer(b) => PyRep::Bytes(b),
                PyRep::String(s) | PyRep::Token(s) => PyRep::Bytes(s.into_bytes()),
                _ => PyRep::Bytes(Vec::new()),
            },
            Self::Str => match val {
                PyRep::Bytes(b) => PyRep::Bytes(b),
                PyRep::String(s) => PyRep::String(s),
                PyRep::Token(s) => PyRep::String(s),
                _ => PyRep::String(String::new()),
            },
            Self::WStr => match val {
                PyRep::String(s) | PyRep::Token(s) => PyRep::String(s),
                PyRep::Bytes(b) => PyRep::String(String::from_utf8_lossy(&b).into_owned()),
                _ => PyRep::String(String::new()),
            },
            Self::Empty => PyRep::None,
        }
    }
}

/// Bit width of a raw `DBTYPE` integer code per `DBTYPE_GetSizeBits` (`EVEDB.cpp`).
pub const fn dbtype_bit_width(dbtype: i64) -> usize {
    match dbtype {
        20 | 21 | 64 | 5 | 6 => 64, // I8, UI8, FILETIME, R8, CY
        3 | 19 | 4 => 32,           // I4, UI4, R4
        2 | 18 => 16,               // I2, UI2
        16 | 17 => 8,               // I1, UI1
        11 => 1,                    // BOOL
        _ => 0,                     // Variable-length / object column (128 BYTES, 129 STR, 130 WSTR)
    }
}

/// Typed `blue.DBRowDescriptor` builder and parser (`PyDatabase.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DBRowDescriptor {
    pub columns: Vec<(String, DbColumnType)>,
    pub keywords: Vec<(String, String)>,
}

impl DBRowDescriptor {
    pub fn new(columns: impl IntoIterator<Item = (impl Into<String>, DbColumnType)>) -> Self {
        Self {
            columns: columns
                .into_iter()
                .map(|(name, ty)| (name.into(), ty))
                .collect(),
            keywords: Vec::new(),
        }
    }

    pub fn with_keywords(
        mut self,
        keywords: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    ) -> Self {
        self.keywords = keywords
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        self
    }

    pub fn column_names(&self) -> Vec<String> {
        self.columns.iter().map(|(n, _)| n.clone()).collect()
    }

    pub fn raw_defs(&self) -> Vec<(String, i64)> {
        self.columns
            .iter()
            .map(|(n, t)| (n.clone(), t.as_i64()))
            .collect()
    }

    /// Converts this descriptor into its wire `PyObjectEx1("blue.DBRowDescriptor", ...)` representation.
    pub fn to_pyrep(&self) -> PyRep {
        let col_pairs: Vec<PyRep> = self
            .columns
            .iter()
            .map(|(name, col_type)| {
                PyRep::Tuple(vec![
                    PyRep::String(name.clone()),
                    PyRep::Integer(col_type.as_i64()),
                ])
            })
            .collect();
        let mut header_items = vec![
            PyRep::Token("blue.DBRowDescriptor".into()),
            PyRep::Tuple(vec![PyRep::Tuple(col_pairs)]),
        ];
        if !self.keywords.is_empty() {
            let kw_list = self
                .keywords
                .iter()
                .map(|(k, tok)| {
                    PyRep::Tuple(vec![PyRep::String(k.clone()), PyRep::Token(tok.clone())])
                })
                .collect();
            header_items.push(PyRep::List(kw_list));
        }
        PyRep::ObjectEx {
            is_type_2: false,
            header: Box::new(PyRep::Tuple(header_items)),
            list: Vec::new(),
            dict: Vec::new(),
        }
    }

    /// Parses a `DBRowDescriptor` from a decoded `PyRep` (`ObjectEx` or `Tuple`).
    pub fn from_pyrep(rep: &PyRep) -> Option<Self> {
        let raw_defs = extract_dbrow_header_col_defs(rep)?;
        let mut columns = Vec::with_capacity(raw_defs.len());
        for (name, code) in raw_defs {
            columns.push((name, DbColumnType::from_i64(code)?));
        }
        let mut keywords = Vec::new();
        let tuple_items = match rep {
            PyRep::ObjectEx { header, .. } => match header.as_ref() {
                PyRep::Tuple(items) => Some(items),
                _ => None,
            },
            PyRep::Tuple(items) => Some(items),
            _ => None,
        };
        if let Some(items) = tuple_items {
            if let Some(PyRep::List(kw_items)) = items.get(2) {
                for kw in kw_items {
                    if let PyRep::Tuple(pair) = kw {
                        if pair.len() >= 2 {
                            if let (Some(k), Some(v)) = (pair[0].as_str(), pair[1].as_str()) {
                                keywords.push((k.to_string(), v.to_string()));
                            }
                        }
                    }
                }
            }
        }
        Some(Self { columns, keywords })
    }

    /// Canonical V24 inventory item row descriptor (`StaticDataMgr::CreateHeader`, `InvBrokerService::GetItemDescriptor`).
    pub fn inv_item_header() -> Self {
        Self::new([
            ("itemID", DbColumnType::I8),
            ("typeID", DbColumnType::I4),
            ("ownerID", DbColumnType::I4),
            ("locationID", DbColumnType::I8),
            ("flagID", DbColumnType::I2),
            ("quantity", DbColumnType::I4),
            ("groupID", DbColumnType::I4),
            ("categoryID", DbColumnType::I4),
            ("customInfo", DbColumnType::Str),
        ])
        .with_keywords([
            ("stacksize", "util.StackSize"),
            ("singleton", "util.Singleton"),
        ])
    }

    /// Canonical character selection list row descriptor (`charUnboundMgr.GetCharactersToSelect`).
    pub fn character_select_list_header() -> Self {
        Self::new([
            ("characterID", DbColumnType::I8),
            ("characterName", DbColumnType::WStr),
            ("corporationID", DbColumnType::I8),
            ("allianceID", DbColumnType::I8),
            ("stationID", DbColumnType::I8),
            ("solarSystemID", DbColumnType::I4),
            ("constellationID", DbColumnType::I4),
            ("regionID", DbColumnType::I4),
            ("balance", DbColumnType::R8),
            ("skillPoints", DbColumnType::R8),
            ("shipTypeID", DbColumnType::I4),
            ("shipName", DbColumnType::WStr),
            ("securityRating", DbColumnType::R8),
        ])
    }

    /// Canonical regional market order book descriptor (`marketProxy.GetOrders`).
    pub fn market_order_header() -> Self {
        Self::new([
            ("price", DbColumnType::R8),
            ("volRemaining", DbColumnType::R8),
            ("typeID", DbColumnType::I4),
            ("orderID", DbColumnType::I8),
            ("volEntered", DbColumnType::I4),
            ("minVolume", DbColumnType::I4),
            ("bid", DbColumnType::Bool),
            ("stationID", DbColumnType::I8),
            ("regionID", DbColumnType::I4),
        ])
    }

    /// Canonical market ask summary descriptor (`typeID`, `price`, `volRemaining`, `stationID`).
    pub fn market_summary_header() -> Self {
        Self::new([
            ("typeID", DbColumnType::I4),
            ("price", DbColumnType::R8),
            ("volRemaining", DbColumnType::R8),
            ("stationID", DbColumnType::I8),
        ])
    }

    /// Builds a packed `blue.DBRow` (`PyRep::DBRow`) from positional column values.
    pub fn build_dbrow(&self, values: Vec<PyRep>) -> PyRep {
        let mut columns = Vec::with_capacity(self.columns.len());
        for (idx, (col_name, col_type)) in self.columns.iter().enumerate() {
            let raw_val = values.get(idx).cloned().unwrap_or(PyRep::None);
            columns.push((col_name.clone(), col_type.normalize_value(raw_val)));
        }
        let defs = self.raw_defs();
        let raw_rle = pack_packed_row_columns_from_defs(&defs, &columns);
        PyRep::DBRow {
            header: Box::new(self.to_pyrep()),
            columns,
            raw_rle,
        }
    }

    /// Builds a `dbutil.CRowset` (`PyObjectEx2`) with `((Token("dbutil.CRowset"),), {"header": desc})` header.
    pub fn build_crowset(&self, rows: Vec<Vec<PyRep>>) -> PyRep {
        let header_rep = self.to_pyrep();
        let packed_rows = rows.into_iter().map(|r| self.build_dbrow(r)).collect();
        PyRep::ObjectEx {
            is_type_2: true,
            header: Box::new(PyRep::Tuple(vec![
                PyRep::Tuple(vec![PyRep::Token("dbutil.CRowset".into())]),
                PyRep::Dict(vec![(PyRep::String("header".into()), header_rep)]),
            ])),
            list: packed_rows,
            dict: Vec::new(),
        }
    }

    /// Builds a `dbutil.CIndexedRowset` (`PyObjectEx2`) with `((Token("dbutil.CIndexedRowset"),), {"header": desc, "columnName": key})` header.
    pub fn build_cindexed_rowset(&self, key_column: &str, rows: Vec<Vec<PyRep>>) -> PyRep {
        let key_idx = self
            .columns
            .iter()
            .position(|(name, _)| name == key_column)
            .unwrap_or(0);
        let key_type = self
            .columns
            .get(key_idx)
            .map(|(_, t)| *t)
            .unwrap_or(DbColumnType::I8);
        let header_rep = self.to_pyrep();
        let mut dict_entries = Vec::with_capacity(rows.len());
        for row in rows {
            let key_val = row
                .get(key_idx)
                .cloned()
                .map(|v| key_type.normalize_value(v))
                .unwrap_or(PyRep::None);
            let packed_row = self.build_dbrow(row);
            dict_entries.push((key_val, packed_row));
        }
        PyRep::ObjectEx {
            is_type_2: true,
            header: Box::new(PyRep::Tuple(vec![
                PyRep::Tuple(vec![PyRep::Token("dbutil.CIndexedRowset".into())]),
                PyRep::Dict(vec![
                    (PyRep::String("header".into()), header_rep),
                    (
                        PyRep::String("columnName".into()),
                        PyRep::String(key_column.into()),
                    ),
                ]),
            ])),
            list: Vec::new(),
            dict: dict_entries,
        }
    }

    /// Builds a `util.Rowset` (`PyRep::Rowset`) from this descriptor's column names.
    pub fn build_rowset(&self, rows: Vec<Vec<PyRep>>) -> PyRep {
        PyRep::Rowset {
            columns: self.column_names(),
            rows,
        }
    }

    /// Builds a `util.IndexRowset` (`PyRep::IndexRowset`) keyed by `key_column`.
    pub fn build_index_rowset(&self, key_column: &str, rows: Vec<Vec<PyRep>>) -> PyRep {
        let key_idx = self
            .columns
            .iter()
            .position(|(name, _)| name == key_column)
            .unwrap_or(0);
        let items = rows
            .into_iter()
            .map(|row| {
                let key = row.get(key_idx).cloned().unwrap_or(PyRep::None);
                (key, row)
            })
            .collect();
        PyRep::IndexRowset {
            key_column: key_column.to_string(),
            columns: self.column_names(),
            items,
        }
    }

    /// Builds a `PyList` of `PyPackedRow`.
    pub fn build_packed_row_list(&self, rows: Vec<Vec<PyRep>>) -> PyRep {
        PyRep::List(rows.into_iter().map(|r| self.build_dbrow(r)).collect())
    }

    /// Builds a `(header, [PyPackedRow, ...])` 2-tuple.
    pub fn build_packed_row_list_tuple(&self, rows: Vec<Vec<PyRep>>) -> PyRep {
        PyRep::Tuple(vec![self.to_pyrep(), self.build_packed_row_list(rows)])
    }
}

/// Compresses an unpacked row byte buffer into EVE's nibble-packed zero/non-zero run format.
/// Each nibble in `[1..=7]` encodes `(8 - nibble)` literal non-zero bytes (`1..=7` bytes),
/// and each nibble in `[8..=15]` encodes `(nibble - 7)` zero bytes (`1..=8` zeros).
/// Trailing all-zero nibble pairs are elided because the decoder zero-fills any remaining bytes.
pub fn encode_rle(input: &[u8]) -> Vec<u8> {
    let trimmed_len = input
        .iter()
        .rposition(|&b| b != 0)
        .map(|pos| pos + 1)
        .unwrap_or(0);
    let active = &input[..trimmed_len];
    if active.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(active.len() + 8);
    let mut cursor = 0usize;
    let mut pending_byte_pos: Option<usize> = None;

    while cursor < active.len() {
        let rest = &active[cursor..];
        let max_run = rest.len().min(8);
        let (nibble_val, literal_slice) = if rest[0] == 0 {
            let zero_len = rest[..max_run].iter().take_while(|&&b| b == 0).count();
            cursor += zero_len;
            ((zero_len + 7) as u8, &[][..])
        } else {
            let max_lit = rest.len().min(7);
            let lit_len = rest[..max_lit].iter().take_while(|&&b| b != 0).count();
            let slice = &rest[..lit_len];
            cursor += lit_len;
            ((8 - lit_len) as u8, slice)
        };

        if let Some(pos) = pending_byte_pos.take() {
            out[pos] |= nibble_val << 4;
        } else {
            let pos = out.len();
            out.push(nibble_val);
            pending_byte_pos = Some(pos);
        }
        out.extend_from_slice(literal_slice);
    }

    if let Some(pos) = pending_byte_pos {
        out[pos] |= 8u8 << 4;
    }
    out
}

/// Decodes a `PyPackedRow` zero-compressed RLE byte stream per `EVEUnmarshal.cpp:760-808` (`LoadRLE`).
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

/// Extracts `(column_name, dbtype_code)` pairs from a `DBRowDescriptor` `PyRep` (`ObjectEx` or `Tuple`).
pub fn extract_dbrow_header_col_defs(header: &PyRep) -> Option<Vec<(String, i64)>> {
    match header {
        PyRep::ObjectEx { header: h, .. } => match h.as_ref() {
            PyRep::Tuple(items) => items.get(1).and_then(extract_dbrow_col_defs),
            _ => None,
        },
        PyRep::Tuple(items) => items.get(1).and_then(extract_dbrow_col_defs),
        _ => None,
    }
}

/// Extracts column definitions from either a direct tuple of `(name, dbtype)` pairs
/// or a 1-tuple wrapping a tuple of `(name, dbtype)` pairs (`OF-21`).
pub fn extract_dbrow_col_defs(rep: &PyRep) -> Option<Vec<(String, i64)>> {
    let PyRep::Tuple(outer) = rep else {
        return None;
    };
    if outer.is_empty() {
        return Some(Vec::new());
    }
    let col_tuple: &[PyRep] = match outer.first() {
        Some(PyRep::Tuple(t))
            if matches!(t.first(), Some(PyRep::Tuple(_))) || (outer.len() == 1 && t.is_empty()) =>
        {
            t.as_slice()
        }
        _ => outer.as_slice(),
    };
    let mut defs = Vec::with_capacity(col_tuple.len());
    for entry in col_tuple {
        if let PyRep::Tuple(pair) = entry {
            if pair.len() >= 2 {
                let name = pair[0].as_str()?.to_string();
                let dbtype = pair[1].as_i64()?;
                defs.push((name, dbtype));
            }
        }
    }
    Some(defs)
}

/// Packs fixed-width columns (widest-first: 64, 32, 16, 8 bits), boolean bits, and per-column null bits
/// into an RLE-compressed buffer matching `MarshalStream::VisitPackedRow` (`EVEMarshal.cpp:320-455`).
pub fn pack_packed_row_columns(header: &PyRep, columns: &[(String, PyRep)]) -> Vec<u8> {
    let Some(defs) = extract_dbrow_header_col_defs(header) else {
        return Vec::new();
    };
    pack_packed_row_columns_from_defs(&defs, columns)
}

pub fn pack_packed_row_columns_from_defs(
    defs: &[(String, i64)],
    columns: &[(String, PyRep)],
) -> Vec<u8> {
    if defs.is_empty() {
        return Vec::new();
    }

    let mut byte_data_bit_len = 0usize;
    let mut bool_bit_len = 0usize;
    let nulls_bit_len = defs.len();
    let mut bool_ordinals = vec![0usize; defs.len()];

    for (i, (_, dbtype)) in defs.iter().enumerate() {
        let size = dbtype_bit_width(*dbtype);
        if *dbtype == 11 {
            bool_ordinals[i] = bool_bit_len;
            bool_bit_len += 1;
        }
        if size >= 8 {
            byte_data_bit_len += size;
        }
    }

    let bit_data_len = ((bool_bit_len + nulls_bit_len) >> 3) + 1;
    let mut row_data = Vec::with_capacity((byte_data_bit_len >> 3) + bit_data_len);
    let mut bit_data = vec![0u8; bit_data_len];

    // Sort columns widest-first (64, 32, 16, 8, 1, 0), preserving original index order on ties
    let mut indexed_defs: Vec<(usize, &(String, i64))> = defs.iter().enumerate().collect();
    indexed_defs.sort_by(|(idx_a, (_, dt_a)), (idx_b, (_, dt_b))| {
        dbtype_bit_width(*dt_b)
            .cmp(&dbtype_bit_width(*dt_a))
            .then_with(|| idx_a.cmp(idx_b))
    });

    let lookup_val = |orig_idx: usize, col_name: &str| -> &PyRep {
        columns
            .get(orig_idx)
            .filter(|(n, _)| n == col_name)
            .map(|(_, v)| v)
            .or_else(|| columns.iter().find(|(n, _)| n == col_name).map(|(_, v)| v))
            .unwrap_or(&PyRep::None)
    };

    for (orig_idx, (col_name, dbtype)) in indexed_defs {
        let val = lookup_val(orig_idx, col_name);
        let is_none = matches!(val, PyRep::None);
        if is_none {
            let null_bit = bool_bit_len + orig_idx;
            let null_byte = null_bit >> 3;
            if null_byte < bit_data.len() {
                bit_data[null_byte] |= 1u8 << (null_bit & 7);
            }
        }

        match *dbtype {
            20 | 21 | 64 => {
                let v = if is_none { 0i64 } else { val.as_i64().unwrap_or(0) };
                row_data.extend_from_slice(&v.to_le_bytes());
            }
            6 => {
                let v = if is_none {
                    0i64
                } else {
                    match val {
                        PyRep::Integer(i) => *i,
                        PyRep::Real(f) => (*f * 10_000.0).round() as i64,
                        _ => 0,
                    }
                };
                row_data.extend_from_slice(&v.to_le_bytes());
            }
            5 => {
                let v = if is_none { 0.0f64 } else { val.as_f64().unwrap_or(0.0) };
                row_data.extend_from_slice(&v.to_le_bytes());
            }
            3 => {
                let v = if is_none {
                    0i32
                } else {
                    val.as_i64().unwrap_or(0) as i32
                };
                row_data.extend_from_slice(&v.to_le_bytes());
            }
            19 => {
                let v = if is_none {
                    0u32
                } else {
                    val.as_i64().unwrap_or(0) as u32
                };
                row_data.extend_from_slice(&v.to_le_bytes());
            }
            4 => {
                let v = if is_none {
                    0.0f32
                } else {
                    val.as_f64().unwrap_or(0.0) as f32
                };
                row_data.extend_from_slice(&v.to_le_bytes());
            }
            2 => {
                let v = if is_none {
                    0i16
                } else {
                    val.as_i64().unwrap_or(0) as i16
                };
                row_data.extend_from_slice(&v.to_le_bytes());
            }
            18 => {
                let v = if is_none {
                    0u16
                } else {
                    val.as_i64().unwrap_or(0) as u16
                };
                row_data.extend_from_slice(&v.to_le_bytes());
            }
            16 => {
                let v = if is_none {
                    0i8
                } else {
                    val.as_i64().unwrap_or(0) as i8
                };
                row_data.push(v as u8);
            }
            17 => {
                let v = if is_none {
                    0u8
                } else {
                    val.as_i64().unwrap_or(0) as u8
                };
                row_data.push(v);
            }
            11 => {
                if !is_none && val.as_bool().unwrap_or(false) {
                    let bool_bit = bool_ordinals[orig_idx];
                    let bool_byte = bool_bit >> 3;
                    if bool_byte < bit_data.len() {
                        bit_data[bool_byte] |= 1u8 << (bool_bit & 7);
                    }
                }
            }
            _ => {
                // Zero-bit-width columns (128 BYTES, 129 STR, 130 WSTR) are appended after the RLE stream
            }
        }
    }

    row_data.extend_from_slice(&bit_data);
    encode_rle(&row_data)
}

/// Unpacks fixed-width columns, boolean bits, and null bits from a `PyPackedRow` RLE byte slice
/// matching `UnmarshalStream::LoadPackedRow` (`EVEUnmarshal.cpp:506-680`).
pub fn unpack_packed_row_columns(header: &PyRep, raw_rle: &[u8]) -> Vec<(String, PyRep)> {
    let Some(defs) = extract_dbrow_header_col_defs(header) else {
        return Vec::new();
    };
    if defs.is_empty() {
        return Vec::new();
    }

    let mut byte_data_bit_len = 0usize;
    let mut bool_bit_len = 0usize;
    let nulls_bit_len = defs.len();
    let mut bool_ordinals = vec![0usize; defs.len()];

    for (i, (_, dbtype)) in defs.iter().enumerate() {
        let size = dbtype_bit_width(*dbtype);
        if *dbtype == 11 {
            bool_ordinals[i] = bool_bit_len;
            bool_bit_len += 1;
        }
        if size >= 8 {
            byte_data_bit_len += size;
        }
    }

    let expected_bytes = (byte_data_bit_len >> 3) + ((bool_bit_len + nulls_bit_len) >> 3) + 1;
    if expected_bytes > MAX_COLLECTION_LEN {
        return Vec::new();
    }
    let Some(unpacked) = decode_rle(raw_rle, expected_bytes) else {
        return Vec::new();
    };

    // Per EVEUnmarshal.cpp: columns are iterated in widest-first order (64, 32, 16, 8, 1, 0)
    let mut indexed_defs: Vec<(usize, &(String, i64))> = defs.iter().enumerate().collect();
    indexed_defs.sort_by(|(idx_a, (_, dt_a)), (idx_b, (_, dt_b))| {
        dbtype_bit_width(*dt_b)
            .cmp(&dbtype_bit_width(*dt_a))
            .then_with(|| idx_a.cmp(idx_b))
    });

    let mut offset = 0usize;
    let mut decoded_by_idx: Vec<(usize, String, PyRep)> = Vec::with_capacity(defs.len());

    for (orig_idx, (name, dbtype)) in indexed_defs {
        let size_bits = dbtype_bit_width(*dbtype);
        let null_bit = byte_data_bit_len + bool_bit_len + orig_idx;
        let null_byte = null_bit >> 3;
        let is_null = null_byte < unpacked.len()
            && (unpacked[null_byte] & (1u8 << (null_bit & 7))) != 0;

        if is_null && size_bits > 0 {
            offset += size_bits >> 3;
            decoded_by_idx.push((orig_idx, name.clone(), PyRep::None));
            continue;
        }

        let val = match *dbtype {
            20 | 21 | 64 | 6 if offset + 8 <= unpacked.len() => {
                let v = i64::from_le_bytes(unpacked[offset..offset + 8].try_into().unwrap());
                offset += 8;
                PyRep::Integer(v)
            }
            5 if offset + 8 <= unpacked.len() => {
                let v = f64::from_le_bytes(unpacked[offset..offset + 8].try_into().unwrap());
                offset += 8;
                PyRep::Real(v)
            }
            3 if offset + 4 <= unpacked.len() => {
                let v =
                    i32::from_le_bytes(unpacked[offset..offset + 4].try_into().unwrap()) as i64;
                offset += 4;
                PyRep::Integer(v)
            }
            19 if offset + 4 <= unpacked.len() => {
                let v =
                    u32::from_le_bytes(unpacked[offset..offset + 4].try_into().unwrap()) as i64;
                offset += 4;
                PyRep::Integer(v)
            }
            4 if offset + 4 <= unpacked.len() => {
                let v =
                    f32::from_le_bytes(unpacked[offset..offset + 4].try_into().unwrap()) as f64;
                offset += 4;
                PyRep::Real(v)
            }
            2 if offset + 2 <= unpacked.len() => {
                let v = i16::from_le_bytes([unpacked[offset], unpacked[offset + 1]]) as i64;
                offset += 2;
                PyRep::Integer(v)
            }
            18 if offset + 2 <= unpacked.len() => {
                let v = u16::from_le_bytes([unpacked[offset], unpacked[offset + 1]]) as i64;
                offset += 2;
                PyRep::Integer(v)
            }
            16 if offset < unpacked.len() => {
                let v = (unpacked[offset] as i8) as i64;
                offset += 1;
                PyRep::Integer(v)
            }
            17 if offset < unpacked.len() => {
                let v = unpacked[offset] as i64;
                offset += 1;
                PyRep::Integer(v)
            }
            11 => {
                let bool_bit = byte_data_bit_len + bool_ordinals[orig_idx];
                let bool_byte = bool_bit >> 3;
                if bool_byte < unpacked.len() {
                    PyRep::Bool((unpacked[bool_byte] & (1u8 << (bool_bit & 7))) != 0)
                } else {
                    PyRep::Bool(false)
                }
            }
            _ => PyRep::None,
        };
        decoded_by_idx.push((orig_idx, name.clone(), val));
    }

    decoded_by_idx.sort_by_key(|(idx, _, _)| *idx);
    decoded_by_idx
        .into_iter()
        .map(|(_, name, val)| (name, val))
        .collect()
}
