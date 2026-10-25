//! `marshal` — Conformant, resource-bounded `eveMarshal` binary encoder and decoder.

#![forbid(unsafe_code)]

use crate::dbrow::{
    dbtype_bit_width, extract_dbrow_col_defs, extract_dbrow_header_col_defs,
    pack_packed_row_columns,
};
use crate::{
    MarshalOpcode, ProtoError, PyRep, MARSHAL_HEADER_BYTE, MARSHAL_STRING_TABLE,
    MAX_COLLECTION_LEN, MAX_DECODED_NODES, MAX_FRAME_SIZE, MAX_RECURSION_DEPTH, SAVE_FLAG,
    ZLIB_HEADER_BYTE,
};
use flate2::read::ZlibDecoder;
use std::collections::HashMap;
use std::io::Read;

pub use crate::dbrow::{decode_rle, encode_rle};

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

/// Module paths blue imports when unpickling these classes in the V24.01 client (there is no
/// `util` module there): `utillib.KeyVal`, `eve/common/script/sys/rowset.pyj`, `carbon/common/script/sys/row.pyj`.
pub const V24_KEYVAL_CLASS: &str = "utillib.KeyVal";
pub const V24_ROWSET_CLASS: &str = "eve.common.script.sys.rowset.Rowset";
pub const V24_INDEX_ROWSET_CLASS: &str = "eve.common.script.sys.rowset.IndexRowset";
pub const V24_ROW_CLASS: &str = "carbon.common.script.sys.row.Row";

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
            encode_inner(&PyRep::ascii_bytes(V24_KEYVAL_CLASS), out);
            let mut entries: Vec<(PyRep, PyRep)> = map
                .iter()
                .map(|(k, v)| (PyRep::String(k.clone()), v.clone()))
                .collect();
            entries.sort_by(|(a, _), (b, _)| a.as_str().cmp(&b.as_str()));
            encode_inner(&PyRep::Dict(entries), out);
        }
        PyRep::Rowset { columns, rows } => {
            out.push(MarshalOpcode::PyObject as u8);
            encode_inner(&PyRep::ascii_bytes(V24_ROWSET_CLASS), out);
            let cols = PyRep::List(columns.iter().cloned().map(PyRep::String).collect());
            let row_list = PyRep::List(rows.iter().cloned().map(PyRep::List).collect());
            let dict = PyRep::Dict(vec![
                (PyRep::String("header".into()), cols),
                (
                    PyRep::String("RowClass".into()),
                    PyRep::Token(V24_ROW_CLASS.into()),
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
            encode_inner(&PyRep::ascii_bytes(V24_INDEX_ROWSET_CLASS), out);
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
                    PyRep::Token(V24_ROW_CLASS.into()),
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
        PyRep::PySet(items) | PyRep::Set(items) => {
            out.push(MarshalOpcode::PyObject as u8);
            encode_inner(&PyRep::ascii_bytes("__builtin__.set"), out);
            encode_inner(&PyRep::Tuple(vec![PyRep::List(items.clone())]), out);
        }
        PyRep::DBRow {
            header,
            columns,
            raw_rle,
        } => {
            out.push(MarshalOpcode::PyPackedRow as u8);
            encode_inner(header, out);
            let rle_cow: std::borrow::Cow<'_, [u8]> = if !columns.is_empty() {
                std::borrow::Cow::Owned(pack_packed_row_columns(header, columns))
            } else {
                std::borrow::Cow::Borrowed(raw_rle.as_slice())
            };
            write_size_ex(out, rle_cow.len());
            out.extend_from_slice(&rle_cow);
            if let Some(defs) = extract_dbrow_header_col_defs(header) {
                for (idx, (col_name, dbtype)) in defs.iter().enumerate() {
                    if dbtype_bit_width(*dbtype) == 0 {
                        let val = columns
                            .get(idx)
                            .filter(|(n, _)| n == col_name)
                            .map(|(_, v)| v)
                            .or_else(|| {
                                columns.iter().find(|(n, _)| n == col_name).map(|(_, v)| v)
                            })
                            .unwrap_or(&PyRep::None);
                        encode_inner(val, out);
                    }
                }
            }
        }
        PyRep::Picked(b) => {
            out.push(MarshalOpcode::PyPicked as u8);
            write_size_ex(out, b.len());
            out.extend_from_slice(b);
        }
        PyRep::Object { type_name, args } => {
            if type_name == "util.Row" {
                // In V24.01 `carbon.common.script.sys.row.Row` is a new-style class (`class Row(object)`),
                // so `blue.marshal.Load` rejects `Op_PyObject` (`0x17`, old-style `PyClass_Type` only)
                // and requires `Op_PyObjectEx2` (`0x23`, `__newobj__`) with `((Row,), state)`.
                encode_inner(
                    &PyRep::ObjectEx {
                        is_type_2: true,
                        header: Box::new(PyRep::Tuple(vec![
                            PyRep::Tuple(vec![PyRep::Token(V24_ROW_CLASS.into())]),
                            args.as_ref().clone(),
                        ])),
                        list: Vec::new(),
                        dict: Vec::new(),
                    },
                    out,
                );
            } else {
                out.push(MarshalOpcode::PyObject as u8);
                // blue reads the `PyObject` class name as a plain string (the V24 client writes
                // `0x17 0x13 <len> carbon.common...CallReq`); a `PyToken` here makes it fail with
                // "expected string" when unpickling.
                encode_inner(&PyRep::Bytes(type_name.clone().into_bytes()), out);
                encode_inner(args, out);
            }
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

/// RFC 1950 header check: deflate method (`CMF & 0x0F == 8`), window <= 32K and `(CMF*256+FLG) % 31 == 0`.
/// The V24 client's blue compresses with a small window, so its frames start `0x58 0x09`, not only
/// the common `0x78`. A raw marshal stream always starts with `0x7E`, which fails this check.
fn is_zlib_stream_header(input: &[u8]) -> bool {
    if input.first() == Some(&ZLIB_HEADER_BYTE) {
        return true;
    }
    match input {
        [cmf, flg, ..] => {
            cmf & 0x0F == 8 && (cmf >> 4) <= 7 && ((*cmf as u16) << 8 | *flg as u16) % 31 == 0
        }
        _ => false,
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
    let cursor: &[u8] = if is_zlib_stream_header(input) {
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
            let header = decode_inner(input, save_map, depth + 1, saved_slots, remaining_budget)?;
            let rle_len = read_size_ex(input)?;
            if input.len() < rle_len {
                return Err(ProtoError::UnexpectedEof);
            }
            let raw_rle = input[..rle_len].to_vec();
            *input = &input[rle_len..];
            let mut columns = crate::dbrow::try_unpack_packed_row_columns(&header, &raw_rle)?;
            if let Some(defs) = extract_dbrow_header_col_defs(&header) {
                for (idx, (col_name, dbtype)) in defs.iter().enumerate() {
                    if dbtype_bit_width(*dbtype) == 0 {
                        let val = decode_inner(
                            input,
                            save_map,
                            depth + 1,
                            saved_slots,
                            remaining_budget,
                        )?;
                        if let Some(slot) = columns.get_mut(idx) {
                            *slot = (col_name.clone(), val);
                        }
                    }
                }
            }
            PyRep::DBRow {
                header: Box::new(header),
                columns,
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
                let v = decode_inner(input, save_map, depth + 1, saved_slots, remaining_budget)?;
                let k = decode_inner(input, save_map, depth + 1, saved_slots, remaining_budget)?;
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

            if type_name == "util.KeyVal" || type_name == V24_KEYVAL_CLASS {
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
            } else if type_name == "util.Rowset" || type_name == V24_ROWSET_CLASS {
                if let Some((columns, rows)) = try_extract_rowset(&args) {
                    PyRep::Rowset { columns, rows }
                } else {
                    PyRep::Object {
                        type_name,
                        args: Box::new(args),
                    }
                }
            } else if type_name == "util.IndexRowset" || type_name == V24_INDEX_ROWSET_CLASS {
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
            } else if type_name == "__builtin__.set" || type_name == "builtins.set" || type_name == "set" {
                if let PyRep::Tuple(items) = &args {
                    if let Some(PyRep::List(elems)) = items.first() {
                        PyRep::PySet(elems.clone())
                    } else {
                        PyRep::PySet(items.clone())
                    }
                } else if let PyRep::List(elems) = &args {
                    PyRep::PySet(elems.clone())
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
            let header = decode_inner(input, save_map, depth + 1, saved_slots, remaining_budget)?;
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
                let k = decode_inner(input, save_map, depth + 1, saved_slots, remaining_budget)?;
                let v = decode_inner(input, save_map, depth + 1, saved_slots, remaining_budget)?;
                dict.push((k, v));
            }
            if input.is_empty() {
                return Err(ProtoError::UnexpectedEof);
            }
            *input = &input[1..]; // Consume second PackedTerminator (0x2D)

            if is_type_2 && list.is_empty() && dict.is_empty() {
                if let PyRep::Tuple(h_items) = &header {
                    if h_items.len() == 2 {
                        if let PyRep::Tuple(ctor_args) = &h_items[0] {
                            if ctor_args.len() == 1
                                && ctor_args[0] == PyRep::Token(V24_ROW_CLASS.into())
                            {
                                return Ok({
                                    let val = PyRep::Object {
                                        type_name: "util.Row".into(),
                                        args: Box::new(h_items[1].clone()),
                                    };
                                    if let Some(slot_idx) = storage_index {
                                        saved_slots[slot_idx] = Some(val.clone());
                                    }
                                    val
                                });
                            }
                        }
                    }
                }
            }

            PyRep::ObjectEx {
                is_type_2,
                header: Box::new(header),
                list,
                dict,
            }
        }
        x if x == MarshalOpcode::PySubStruct as u8 => {
            let inner = decode_inner(input, save_map, depth + 1, saved_slots, remaining_budget)?;
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
            let target_ref = saved_slots
                .get(zero_idx)
                .and_then(|opt| opt.as_ref())
                .ok_or(ProtoError::InvalidSaveReference(idx))?;

            // Charge the full expanded node & byte-weight count BEFORE cloning to prevent DoS!
            let extra_nodes = target_ref.node_count().saturating_sub(1);
            *remaining_budget = remaining_budget
                .checked_sub(extra_nodes)
                .ok_or(ProtoError::NodeBudgetExceeded)?;
            target_ref.clone()
        }
        other => return Err(ProtoError::UnsupportedOpcode(other)),
    };

    if let Some(slot_idx) = storage_index {
        saved_slots[slot_idx] = Some(value.clone());
    }

    Ok(value)
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

pub(crate) fn write_size_ex(out: &mut Vec<u8>, len: usize) {
    if len < 0xFF {
        out.push(len as u8);
    } else {
        out.push(0xFF);
        out.extend_from_slice(&(len as u32).to_le_bytes());
    }
}

pub(crate) fn read_collection_len(input: &mut &[u8]) -> Result<usize, ProtoError> {
    let len = read_size_ex(input)?;
    if len > MAX_COLLECTION_LEN {
        return Err(ProtoError::CollectionTooLarge(len));
    }
    Ok(len)
}

pub(crate) fn read_size_ex(input: &mut &[u8]) -> Result<usize, ProtoError> {
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

/// Constructs a `PyRep::DBRow` (`Op_PyPackedRow` `0x2A`) from `(column_name, dbtype, value)` definitions,
/// packing fixed-width columns widest-first (`64-bit, 32-bit, 16-bit, 8-bit, 1-bit`) into `raw_rle`
/// and preserving variable-length object columns (`dbtype == 129/130`) as trailing stream elements.
pub fn pack_dbrow(cols: &[(&str, i64, PyRep)]) -> PyRep {
    let descriptor_cols: Vec<PyRep> = cols
        .iter()
        .map(|(name, dt, _)| {
            PyRep::Tuple(vec![PyRep::String((*name).to_string()), PyRep::Integer(*dt)])
        })
        .collect();
    let header = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("blue.DBRowDescriptor".into()),
            PyRep::Tuple(vec![PyRep::Tuple(descriptor_cols)]),
        ])),
        list: vec![],
        dict: vec![],
    };

    let mut indexed: Vec<(usize, &str, i64, &PyRep)> = cols
        .iter()
        .enumerate()
        .map(|(idx, (n, dt, v))| (idx, *n, *dt, v))
        .collect();
    indexed.sort_by(|(ia, _, dta, _), (ib, _, dtb, _)| {
        dbtype_bit_width(*dtb)
            .cmp(&dbtype_bit_width(*dta))
            .then_with(|| ia.cmp(ib))
    });

    let mut raw_bytes = Vec::new();
    let mut bool_bits: Vec<bool> = Vec::new();
    for (_, _, dt, val) in &indexed {
        match *dt {
            20 | 21 | 64 | 6 => {
                raw_bytes.extend_from_slice(&val.as_i64().unwrap_or(0).to_le_bytes());
            }
            5 => {
                raw_bytes.extend_from_slice(&val.as_f64().unwrap_or(0.0).to_le_bytes());
            }
            3 => {
                raw_bytes
                    .extend_from_slice(&(val.as_i64().unwrap_or(0) as i32).to_le_bytes());
            }
            19 => {
                raw_bytes
                    .extend_from_slice(&(val.as_i64().unwrap_or(0) as u32).to_le_bytes());
            }
            4 => {
                raw_bytes
                    .extend_from_slice(&(val.as_f64().unwrap_or(0.0) as f32).to_le_bytes());
            }
            2 => {
                raw_bytes
                    .extend_from_slice(&(val.as_i64().unwrap_or(0) as i16).to_le_bytes());
            }
            18 => {
                raw_bytes
                    .extend_from_slice(&(val.as_i64().unwrap_or(0) as u16).to_le_bytes());
            }
            16 => {
                raw_bytes.push((val.as_i64().unwrap_or(0) as i8) as u8);
            }
            17 => {
                raw_bytes.push(val.as_i64().unwrap_or(0) as u8);
            }
            11 => {
                bool_bits.push(val.as_bool().unwrap_or(false));
            }
            _ => {}
        }
    }
    if !bool_bits.is_empty() {
        let mut bool_bytes = vec![0u8; bool_bits.len().div_ceil(8)];
        for (bit_idx, b) in bool_bits.into_iter().enumerate() {
            if b {
                bool_bytes[bit_idx / 8] |= 1u8 << (bit_idx % 8);
            }
        }
        raw_bytes.extend_from_slice(&bool_bytes);
    }

    let raw_rle = encode_rle(&raw_bytes);
    let columns = cols
        .iter()
        .map(|(n, _, v)| ((*n).to_string(), v.clone()))
        .collect();

    PyRep::DBRow {
        header: Box::new(header),
        columns,
        raw_rle,
    }
}

#[allow(dead_code)]
fn _keep_extract_col_defs_ref() {
    let _ = extract_dbrow_col_defs;
}

