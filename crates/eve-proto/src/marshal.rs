//! `marshal` — Conformant, resource-bounded `eveMarshal` binary encoder and decoder.

#![forbid(unsafe_code)]

use crate::dbrow::{
    dbtype_bit_width, extract_dbrow_col_defs, extract_dbrow_header_col_defs,
    pack_packed_row_columns,
};
use crate::{
    MarshalOpcode, ProtoError, PyRep, MARSHAL_HEADER_BYTE, MARSHAL_STRING_TABLE,
    MAX_COLLECTION_LEN, MAX_DECODED_NODES, MAX_RECURSION_DEPTH, SAVE_FLAG,
    ZLIB_HEADER_BYTE,
};
use flate2::read::ZlibDecoder;
use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;

pub use crate::dbrow::{decode_rle, encode_rle};
pub use crate::MAX_FRAME_SIZE;

/// Maximum container/substream depth accepted by infallible [`encode`]; deeper values return an empty frame.
pub(crate) const MAX_ENCODE_DEPTH: usize = MAX_RECURSION_DEPTH * 2;
/// Maximum serialized byte size emitted by infallible [`encode`] (exceeded payloads return an empty frame).
pub(crate) const MAX_ENCODE_FRAME_BYTES: usize = MAX_FRAME_SIZE * 2;

#[derive(Clone, Copy)]
struct EncodeLimits {
    max_depth: usize,
    max_collection_len: usize,
    max_frame_bytes: usize,
}

impl EncodeLimits {
    const fn strict(max_frame_bytes: usize) -> Self {
        Self {
            max_depth: MAX_RECURSION_DEPTH,
            max_collection_len: MAX_COLLECTION_LEN,
            max_frame_bytes,
        }
    }

    const fn permissive_infallible() -> Self {
        Self {
            max_depth: MAX_ENCODE_DEPTH,
            max_collection_len: MAX_COLLECTION_LEN,
            max_frame_bytes: MAX_ENCODE_FRAME_BYTES,
        }
    }
}

/// Encodes `rep` as a legacy `eveMarshal` binary frame.
///
/// Enforces bounded iterative traversal (`MAX_ENCODE_DEPTH`, `MAX_COLLECTION_LEN`, `MAX_ENCODE_FRAME_BYTES`);
/// callers handling untrusted or strict-wire payloads should use [`try_encode`].
pub fn encode(rep: &PyRep) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);
    buf.push(MARSHAL_HEADER_BYTE);
    buf.extend_from_slice(&0u32.to_le_bytes()); // Save count = 0
    if encode_rep_into_iter(rep, &mut buf, EncodeLimits::permissive_infallible()).is_err() {
        return Vec::new();
    }
    buf
}

/// Resource-bounded [`encode`] (`#1086`): enforces `MAX_RECURSION_DEPTH`, `MAX_COLLECTION_LEN`,
/// and `MAX_FRAME_SIZE` before allocating unbounded marshal buffers.
pub fn try_encode(rep: &PyRep) -> Result<Vec<u8>, ProtoError> {
    try_encode_with_limits(rep, MAX_FRAME_SIZE)
}

/// Resource-bounded [`encode`] with a caller-supplied maximum serialized frame size in bytes.
pub fn try_encode_with_limits(rep: &PyRep, max_frame_bytes: usize) -> Result<Vec<u8>, ProtoError> {
    if max_frame_bytes < 5 {
        return Err(ProtoError::FrameTooLarge(5));
    }
    let mut buf = Vec::with_capacity(64.min(max_frame_bytes));
    buf.push(MARSHAL_HEADER_BYTE);
    buf.extend_from_slice(&0u32.to_le_bytes()); // Save count = 0
    encode_rep_into_iter(rep, &mut buf, EncodeLimits::strict(max_frame_bytes))?;
    if buf.len() > max_frame_bytes {
        return Err(ProtoError::FrameTooLarge(buf.len()));
    }
    Ok(buf)
}

pub fn encode_zlib(rep: &PyRep) -> Result<Vec<u8>, ProtoError> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    let raw = try_encode(rep)?;
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&raw)
        .map_err(|e| ProtoError::ZlibError(e.to_string()))?;
    encoder
        .finish()
        .map_err(|e| ProtoError::ZlibError(e.to_string()))
}

/// Module paths blue imports when unpickling these classes in the V24.01 client (there is no
/// `util` or `objectCaching` top-level module there): `utillib.KeyVal`,
/// `eve/common/script/sys/rowset.pyj`, `carbon/common/script/sys/row.pyj`,
/// `carbon/common/script/net/objectCaching.pyj`, `carbon/common/script/net/cachedObject.pyj`.
pub const V24_KEYVAL_CLASS: &str = "utillib.KeyVal";
pub const V24_ROWSET_CLASS: &str = "eve.common.script.sys.rowset.Rowset";
pub const V24_INDEX_ROWSET_CLASS: &str = "eve.common.script.sys.rowset.IndexRowset";
pub const V24_ROW_CLASS: &str = "carbon.common.script.sys.row.Row";
/// V24.01 classic old-style class paths (`Op_PyObject` `0x17`, whitelisted in `carbon/common/lib/whitelist.pyj`).
pub const V24_CACHED_OBJECT_CLASS: &str = "carbon.common.script.net.objectCaching.CachedObject";
pub const V24_CACHED_METHOD_CALL_RESULT_CLASS: &str =
    "carbon.common.script.net.objectCaching.CachedMethodCallResult";
pub const V24_CACHE_HINT_CLASS: &str = "carbon.common.script.net.cachedObject.CachedObject";

#[cfg(test)]
fn encode_inner(rep: &PyRep, out: &mut Vec<u8>) {
    let _ = encode_rep_into_iter(rep, out, EncodeLimits::permissive_infallible());
}

struct EncoderState {
    frames: Vec<Vec<u8>>,
    total_bytes: usize,
    limits: EncodeLimits,
}

impl EncoderState {
    fn new(initial: Vec<u8>, limits: EncodeLimits) -> Result<Self, ProtoError> {
        let total_bytes = initial.len();
        if total_bytes > limits.max_frame_bytes {
            return Err(ProtoError::FrameTooLarge(total_bytes));
        }
        Ok(Self {
            frames: vec![initial],
            total_bytes,
            limits,
        })
    }

    fn check_collection(&self, len: usize) -> Result<(), ProtoError> {
        if len > self.limits.max_collection_len {
            return Err(ProtoError::CollectionTooLarge(len));
        }
        Ok(())
    }

    fn check_depth(&self, depth: usize) -> Result<(), ProtoError> {
        if depth > self.limits.max_depth {
            return Err(ProtoError::RecursionLimitExceeded);
        }
        Ok(())
    }

    fn reserve_room(&self, additional: usize) -> Result<(), ProtoError> {
        let pending_prefixes = self.frames.len().saturating_sub(1);
        let projected = self
            .total_bytes
            .saturating_add(additional)
            .saturating_add(pending_prefixes);
        if projected > self.limits.max_frame_bytes {
            return Err(ProtoError::FrameTooLarge(projected));
        }
        Ok(())
    }

    fn cur_mut(&mut self) -> &mut Vec<u8> {
        self.frames
            .last_mut()
            .expect("encoder always has an active frame buffer")
    }

    fn push_u8(&mut self, byte: u8) -> Result<(), ProtoError> {
        self.reserve_room(1)?;
        self.cur_mut().push(byte);
        self.total_bytes += 1;
        Ok(())
    }

    fn push_slice(&mut self, slice: &[u8]) -> Result<(), ProtoError> {
        self.reserve_room(slice.len())?;
        self.cur_mut().extend_from_slice(slice);
        self.total_bytes += slice.len();
        Ok(())
    }

    fn push_size_ex(&mut self, len: usize) -> Result<(), ProtoError> {
        let encoded_len = if len < 0xFF { 1 } else { 5 };
        self.reserve_room(encoded_len)?;
        write_size_ex(self.cur_mut(), len);
        self.total_bytes += encoded_len;
        Ok(())
    }

    fn emit_len_prefixed_bytes(
        &mut self,
        opcode: MarshalOpcode,
        bytes: &[u8],
    ) -> Result<(), ProtoError> {
        let size_len = if bytes.len() < 0xFF { 1 } else { 5 };
        self.reserve_room(1usize.saturating_add(size_len).saturating_add(bytes.len()))?;
        let out = self.cur_mut();
        out.push(opcode as u8);
        write_size_ex(out, bytes.len());
        out.extend_from_slice(bytes);
        self.total_bytes += 1 + size_len + bytes.len();
        Ok(())
    }

    fn emit_bytes_atom(&mut self, bytes: &[u8]) -> Result<(), ProtoError> {
        match bytes.len() {
            0 => self.push_u8(MarshalOpcode::PyEmptyString as u8),
            1 => self.push_slice(&[MarshalOpcode::PyCharString as u8, bytes[0]]),
            len if len <= 0xFF => {
                self.reserve_room(2 + len)?;
                let out = self.cur_mut();
                out.push(MarshalOpcode::PyShortString as u8);
                out.push(len as u8);
                out.extend_from_slice(bytes);
                self.total_bytes += 2 + len;
                Ok(())
            }
            _ => self.emit_len_prefixed_bytes(MarshalOpcode::PyLongString, bytes),
        }
    }

    fn emit_wstring(&mut self, s: &str) -> Result<(), ProtoError> {
        if s.is_empty() {
            self.push_u8(MarshalOpcode::PyEmptyWString as u8)
        } else {
            self.emit_len_prefixed_bytes(MarshalOpcode::PyWStringUTF8, s.as_bytes())
        }
    }

    fn emit_list_header(&mut self, len: usize) -> Result<(), ProtoError> {
        self.check_collection(len)?;
        match len {
            0 => self.push_u8(MarshalOpcode::PyEmptyList as u8),
            1 => self.push_u8(MarshalOpcode::PyOneList as u8),
            n => {
                self.push_u8(MarshalOpcode::PyList as u8)?;
                self.push_size_ex(n)
            }
        }
    }

    fn emit_string_list(&mut self, cols: &[String], item_depth: usize) -> Result<(), ProtoError> {
        self.emit_list_header(cols.len())?;
        if !cols.is_empty() {
            self.check_depth(item_depth)?;
            for c in cols {
                self.emit_wstring(c)?;
            }
        }
        Ok(())
    }

    fn open_substream(&mut self) -> Result<(), ProtoError> {
        self.push_u8(MarshalOpcode::PySubStream as u8)?;
        // Reserve 5 bytes for nested stream header plus at least 1 byte for this substream's length prefix.
        let pending_prefixes = self.frames.len();
        let projected = self.total_bytes.saturating_add(5).saturating_add(pending_prefixes);
        if projected > self.limits.max_frame_bytes {
            return Err(ProtoError::FrameTooLarge(projected));
        }
        let mut sub_buf = Vec::with_capacity(64.min(self.limits.max_frame_bytes));
        sub_buf.push(MARSHAL_HEADER_BYTE);
        sub_buf.extend_from_slice(&0u32.to_le_bytes());
        self.frames.push(sub_buf);
        self.total_bytes += 5;
        Ok(())
    }

    fn close_substream(&mut self) -> Result<(), ProtoError> {
        let sub = self
            .frames
            .pop()
            .expect("substream frame must match open_substream");
        let prefix_len = if sub.len() < 0xFF { 1 } else { 5 };
        let pending_prefixes = self.frames.len().saturating_sub(1);
        let projected = self
            .total_bytes
            .saturating_add(prefix_len)
            .saturating_add(pending_prefixes);
        if projected > self.limits.max_frame_bytes {
            return Err(ProtoError::FrameTooLarge(projected));
        }
        let parent = self.cur_mut();
        write_size_ex(parent, sub.len());
        parent.extend_from_slice(&sub);
        self.total_bytes += prefix_len;
        Ok(())
    }
}

enum EncodeTask<'a> {
    Rep {
        rep: &'a PyRep,
        depth: usize,
    },
    Byte(u8),
    Str {
        s: &'a str,
        depth: usize,
    },
    ListSlice {
        items: &'a [PyRep],
        depth: usize,
    },
    PackedRowTail {
        header: &'a PyRep,
        columns: &'a [(String, PyRep)],
        raw_rle: &'a [u8],
        depth: usize,
    },
    FinishSubStream,
}

static ENCODE_NONE_FALLBACK: PyRep = PyRep::None;

fn push_rep_seq<'a>(work: &mut Vec<EncodeTask<'a>>, items: &'a [PyRep], depth: usize) {
    for item in items.iter().rev() {
        work.push(EncodeTask::Rep { rep: item, depth });
    }
}

fn push_encode_scalar(state: &mut EncoderState, rep: &PyRep) -> Result<bool, ProtoError> {
    match rep {
        PyRep::None => state.push_u8(MarshalOpcode::PyNone as u8)?,
        PyRep::Bool(true) => state.push_u8(MarshalOpcode::PyTrue as u8)?,
        PyRep::Bool(false) => state.push_u8(MarshalOpcode::PyFalse as u8)?,
        PyRep::Token(t) => state.emit_len_prefixed_bytes(MarshalOpcode::PyToken, t.as_bytes())?,
        PyRep::Integer(-1) => state.push_u8(MarshalOpcode::PyMinusOne as u8)?,
        PyRep::Integer(0) => state.push_u8(MarshalOpcode::PyZeroInteger as u8)?,
        PyRep::Integer(1) => state.push_u8(MarshalOpcode::PyOneInteger as u8)?,
        PyRep::Integer(v) if (-128..=127).contains(v) => {
            state.push_slice(&[MarshalOpcode::PyByte as u8, *v as i8 as u8])?;
        }
        PyRep::Integer(v) if (-32768..=32767).contains(v) => {
            let b = (*v as i16).to_le_bytes();
            state.push_slice(&[MarshalOpcode::PySignedShort as u8, b[0], b[1]])?;
        }
        PyRep::Integer(v) if (i32::MIN as i64..=i32::MAX as i64).contains(v) => {
            state.push_u8(MarshalOpcode::PyLong as u8)?;
            state.push_slice(&(*v as i32).to_le_bytes())?;
        }
        PyRep::Integer(v) => {
            state.push_u8(MarshalOpcode::PyLongLong as u8)?;
            state.push_slice(&v.to_le_bytes())?;
        }
        PyRep::Real(v) if *v == 0.0 && v.is_sign_positive() => {
            state.push_u8(MarshalOpcode::PyZeroReal as u8)?;
        }
        PyRep::Real(v) => {
            state.push_u8(MarshalOpcode::PyReal as u8)?;
            state.push_slice(&v.to_le_bytes())?;
        }
        PyRep::Bytes(b) => state.emit_bytes_atom(b)?,
        PyRep::String(s) => state.emit_wstring(s)?,
        PyRep::Buffer(b) => state.emit_len_prefixed_bytes(MarshalOpcode::PyBuffer, b)?,
        PyRep::Picked(b) => state.emit_len_prefixed_bytes(MarshalOpcode::PyPicked, b)?,
        _ => return Ok(false),
    }
    Ok(true)
}

fn expand_encode_rep<'a>(
    rep: &'a PyRep,
    depth: usize,
    state: &mut EncoderState,
    work: &mut Vec<EncodeTask<'a>>,
) -> Result<(), ProtoError> {
    state.check_depth(depth)?;
    if push_encode_scalar(state, rep)? {
        return Ok(());
    }
    match rep {
        PyRep::Tuple(items) => {
            state.check_collection(items.len())?;
            match items.len() {
                0 => state.push_u8(MarshalOpcode::PyEmptyTuple as u8)?,
                1 => {
                    state.push_u8(MarshalOpcode::PyOneTuple as u8)?;
                    work.push(EncodeTask::Rep {
                        rep: &items[0],
                        depth: depth + 1,
                    });
                }
                2 => {
                    state.push_u8(MarshalOpcode::PyTwoTuple as u8)?;
                    push_rep_seq(work, items, depth + 1);
                }
                n => {
                    state.push_u8(MarshalOpcode::PyTuple as u8)?;
                    state.push_size_ex(n)?;
                    push_rep_seq(work, items, depth + 1);
                }
            }
        }
        PyRep::List(items) => {
            state.emit_list_header(items.len())?;
            push_rep_seq(work, items, depth + 1);
        }
        PyRep::Dict(entries) => {
            state.check_collection(entries.len())?;
            state.push_u8(MarshalOpcode::PyDict as u8)?;
            state.push_size_ex(entries.len())?;
            // EVE marshal writes VALUE first, then KEY.
            for (k, v) in entries.iter().rev() {
                work.push(EncodeTask::Rep {
                    rep: k,
                    depth: depth + 1,
                });
                work.push(EncodeTask::Rep {
                    rep: v,
                    depth: depth + 1,
                });
            }
        }
        PyRep::SubStruct(inner) => {
            state.push_u8(MarshalOpcode::PySubStruct as u8)?;
            work.push(EncodeTask::Rep {
                rep: inner,
                depth: depth + 1,
            });
        }
        PyRep::ChecksumedStream { checksum, body } => {
            state.push_u8(MarshalOpcode::PyChecksumedStream as u8)?;
            state.push_slice(&checksum.to_le_bytes())?;
            work.push(EncodeTask::Rep {
                rep: body,
                depth: depth + 1,
            });
        }
        PyRep::SubStream(inner) => {
            state.check_depth(depth + 1)?;
            state.open_substream()?;
            work.push(EncodeTask::FinishSubStream);
            work.push(EncodeTask::Rep {
                rep: inner,
                depth: depth + 1,
            });
        }
        PyRep::KeyVal(map) => {
            state.check_collection(map.len())?;
            state.check_depth(depth + 1)?;
            state.push_u8(MarshalOpcode::PyObject as u8)?;
            state.emit_bytes_atom(V24_KEYVAL_CLASS.as_bytes())?;
            state.push_u8(MarshalOpcode::PyDict as u8)?;
            state.push_size_ex(map.len())?;
            if !map.is_empty() {
                state.check_depth(depth + 2)?;
                let mut pairs: Vec<(&str, &PyRep)> =
                    map.iter().map(|(k, v)| (k.as_str(), v)).collect();
                pairs.sort_by_key(|(a, _)| *a);
                for (k, v) in pairs.into_iter().rev() {
                    work.push(EncodeTask::Str {
                        s: k,
                        depth: depth + 2,
                    });
                    work.push(EncodeTask::Rep {
                        rep: v,
                        depth: depth + 2,
                    });
                }
            }
        }
        PyRep::Rowset { columns, rows } => {
            state.check_collection(columns.len())?;
            state.check_collection(rows.len())?;
            state.check_depth(depth + 2)?;
            state.push_u8(MarshalOpcode::PyObject as u8)?;
            state.emit_bytes_atom(V24_ROWSET_CLASS.as_bytes())?;
            state.push_u8(MarshalOpcode::PyDict as u8)?;
            state.push_size_ex(3)?;
            state.emit_string_list(columns, depth + 3)?;
            state.emit_wstring("header")?;
            state.emit_len_prefixed_bytes(MarshalOpcode::PyToken, V24_ROW_CLASS.as_bytes())?;
            state.emit_wstring("RowClass")?;
            state.emit_list_header(rows.len())?;
            work.push(EncodeTask::Str {
                s: "lines",
                depth: depth + 2,
            });
            for row in rows.iter().rev() {
                work.push(EncodeTask::ListSlice {
                    items: row,
                    depth: depth + 3,
                });
            }
        }
        PyRep::IndexRowset {
            key_column,
            columns,
            items,
        } => {
            state.check_collection(columns.len())?;
            state.check_collection(items.len())?;
            state.check_depth(depth + 2)?;
            state.push_u8(MarshalOpcode::PyObject as u8)?;
            state.emit_bytes_atom(V24_INDEX_ROWSET_CLASS.as_bytes())?;
            state.push_u8(MarshalOpcode::PyDict as u8)?;
            state.push_size_ex(4)?;
            state.emit_string_list(columns, depth + 3)?;
            state.emit_wstring("header")?;
            state.emit_len_prefixed_bytes(MarshalOpcode::PyToken, V24_ROW_CLASS.as_bytes())?;
            state.emit_wstring("RowClass")?;
            state.emit_wstring(key_column)?;
            state.emit_wstring("idName")?;
            state.push_u8(MarshalOpcode::PyDict as u8)?;
            state.push_size_ex(items.len())?;
            work.push(EncodeTask::Str {
                s: "items",
                depth: depth + 2,
            });
            for (k, row) in items.iter().rev() {
                work.push(EncodeTask::Rep {
                    rep: k,
                    depth: depth + 3,
                });
                work.push(EncodeTask::ListSlice {
                    items: row,
                    depth: depth + 3,
                });
            }
        }
        PyRep::TupleSet { columns, rows } => {
            state.check_collection(columns.len())?;
            state.check_collection(rows.len())?;
            state.check_depth(depth + 1)?;
            state.push_u8(MarshalOpcode::PyTwoTuple as u8)?;
            state.emit_string_list(columns, depth + 2)?;
            state.emit_list_header(rows.len())?;
            for row in rows.iter().rev() {
                work.push(EncodeTask::ListSlice {
                    items: row,
                    depth: depth + 2,
                });
            }
        }
        PyRep::PySet(items) | PyRep::Set(items) => {
            // `set` is a builtin new-style type, so V24 `blue.marshal.Load` rejects `Op_PyObject`
            // (`0x17`, old-style classes only) and requires `Op_PyObjectEx1` (`0x22`) reduce form
            // `(Token("__builtin__.set"), (List(items),))` with empty list/dict slots.
            state.check_collection(items.len())?;
            state.check_depth(depth + 3)?;
            state.push_u8(MarshalOpcode::PyObjectEx1 as u8)?;
            state.push_u8(MarshalOpcode::PyTwoTuple as u8)?;
            state.emit_len_prefixed_bytes(MarshalOpcode::PyToken, b"__builtin__.set")?;
            state.push_u8(MarshalOpcode::PyOneTuple as u8)?;
            state.emit_list_header(items.len())?;
            work.push(EncodeTask::Byte(MarshalOpcode::PackedTerminator as u8));
            work.push(EncodeTask::Byte(MarshalOpcode::PackedTerminator as u8));
            push_rep_seq(work, items, depth + 4);
        }
        PyRep::DBRow {
            header,
            columns,
            raw_rle,
        } => {
            state.push_u8(MarshalOpcode::PyPackedRow as u8)?;
            work.push(EncodeTask::PackedRowTail {
                header,
                columns,
                raw_rle,
                depth: depth + 1,
            });
            work.push(EncodeTask::Rep {
                rep: header,
                depth: depth + 1,
            });
        }
        PyRep::Object { type_name, args } if type_name == "util.Row" => {
            // In V24.01 `carbon.common.script.sys.row.Row` is a new-style class (`class Row(object)`),
            // so `blue.marshal.Load` rejects `Op_PyObject` (`0x17`, old-style `PyClass_Type` only)
            // and requires `Op_PyObjectEx2` (`0x23`, `__newobj__`) with `((Row,), state)`.
            state.check_depth(depth + 3)?;
            state.push_u8(MarshalOpcode::PyObjectEx2 as u8)?;
            state.push_u8(MarshalOpcode::PyTwoTuple as u8)?;
            state.push_u8(MarshalOpcode::PyOneTuple as u8)?;
            state.emit_len_prefixed_bytes(MarshalOpcode::PyToken, V24_ROW_CLASS.as_bytes())?;
            work.push(EncodeTask::Byte(MarshalOpcode::PackedTerminator as u8));
            work.push(EncodeTask::Byte(MarshalOpcode::PackedTerminator as u8));
            work.push(EncodeTask::Rep {
                rep: args,
                depth: depth + 2,
            });
        }
        PyRep::Object { type_name, args } => {
            state.check_depth(depth + 1)?;
            let wire_type_name = match type_name.as_str() {
                "objectCaching.CachedObject" => V24_CACHED_OBJECT_CLASS,
                "objectCaching.CachedMethodCallResult" => V24_CACHED_METHOD_CALL_RESULT_CLASS,
                "util.CachedObject" => V24_CACHE_HINT_CLASS,
                other => other,
            };
            state.push_u8(MarshalOpcode::PyObject as u8)?;
            // blue reads the `PyObject` class name as a plain byte string (`0x17 0x13 ...`);
            // a `PyToken` here fails client unpickling with "expected string".
            state.emit_bytes_atom(wire_type_name.as_bytes())?;
            work.push(EncodeTask::Rep {
                rep: args,
                depth: depth + 1,
            });
        }
        PyRep::ObjectEx {
            is_type_2,
            header,
            list,
            dict,
        } => {
            state.check_collection(list.len())?;
            state.check_collection(dict.len())?;
            state.push_u8(if *is_type_2 {
                MarshalOpcode::PyObjectEx2 as u8
            } else {
                MarshalOpcode::PyObjectEx1 as u8
            })?;
            work.push(EncodeTask::Byte(MarshalOpcode::PackedTerminator as u8));
            for (k, v) in dict.iter().rev() {
                work.push(EncodeTask::Rep {
                    rep: v,
                    depth: depth + 1,
                });
                work.push(EncodeTask::Rep {
                    rep: k,
                    depth: depth + 1,
                });
            }
            work.push(EncodeTask::Byte(MarshalOpcode::PackedTerminator as u8));
            push_rep_seq(work, list, depth + 1);
            work.push(EncodeTask::Rep {
                rep: header,
                depth: depth + 1,
            });
        }
        PyRep::None
        | PyRep::Bool(_)
        | PyRep::Integer(_)
        | PyRep::Real(_)
        | PyRep::Buffer(_)
        | PyRep::Bytes(_)
        | PyRep::Token(_)
        | PyRep::String(_)
        | PyRep::Picked(_) => unreachable!("handled by push_encode_scalar"),
    }
    Ok(())
}

fn encode_rep_into_iter(
    rep: &PyRep,
    out: &mut Vec<u8>,
    limits: EncodeLimits,
) -> Result<(), ProtoError> {
    let mut state = EncoderState::new(std::mem::take(out), limits)?;
    let mut work = vec![EncodeTask::Rep { rep, depth: 0 }];
    while let Some(task) = work.pop() {
        match task {
            EncodeTask::Rep { rep, depth } => expand_encode_rep(rep, depth, &mut state, &mut work)?,
            EncodeTask::Byte(b) => state.push_u8(b)?,
            EncodeTask::Str { s, depth } => {
                state.check_depth(depth)?;
                state.emit_wstring(s)?;
            }
            EncodeTask::ListSlice { items, depth } => {
                state.check_depth(depth)?;
                state.emit_list_header(items.len())?;
                push_rep_seq(&mut work, items, depth + 1);
            }
            EncodeTask::PackedRowTail {
                header,
                columns,
                raw_rle,
                depth,
            } => {
                let rle_cow: std::borrow::Cow<'_, [u8]> = if !columns.is_empty() {
                    std::borrow::Cow::Owned(pack_packed_row_columns(header, columns))
                } else {
                    std::borrow::Cow::Borrowed(raw_rle)
                };
                state.push_size_ex(rle_cow.len())?;
                state.push_slice(&rle_cow)?;
                if let Some(defs) = extract_dbrow_header_col_defs(header) {
                    let zero_vals: Vec<&PyRep> = defs
                        .iter()
                        .filter(|(_, dbtype)| dbtype_bit_width(*dbtype) == 0)
                        .map(|(col_name, _)| {
                            columns
                                .iter()
                                .find(|(n, _)| n == col_name)
                                .map(|(_, v)| v)
                                .unwrap_or(&ENCODE_NONE_FALLBACK)
                        })
                        .collect();
                    for val in zero_vals.into_iter().rev() {
                        work.push(EncodeTask::Rep { rep: val, depth });
                    }
                }
            }
            EncodeTask::FinishSubStream => state.close_substream()?,
        }
    }
    debug_assert_eq!(state.frames.len(), 1);
    *out = state.frames.pop().expect("root frame remains");
    Ok(())
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
    decode_with_limits(input, MAX_FRAME_SIZE, MAX_DECODED_NODES)
}

#[derive(Clone)]
struct SavedSlot {
    rep: PyRep,
    height: usize,
}

/// Decodes a raw or `zlib`-compressed `eveMarshal` payload using caller-supplied outer/inflated
/// byte limits (`max_inflate_bytes`) and AST/byte-weight node budget (`max_nodes`).
///
/// `max_inflate_bytes` bounds both the raw outer slice length and the cumulative decompressed
/// bytes across the outer stream and all nested `PySubStream` (`0x2B`) `zlib` streams combined.
pub fn decode_with_limits(
    input: &[u8],
    max_inflate_bytes: usize,
    max_nodes: usize,
) -> Result<PyRep, ProtoError> {
    if input.is_empty() {
        return Err(ProtoError::UnexpectedEof);
    }
    if input.len() > max_inflate_bytes {
        return Err(ProtoError::FrameTooLarge(input.len()));
    }

    let mut remaining_budget = max_nodes;
    let mut remaining_inflate_budget = max_inflate_bytes;
    let (root, _height) = decode_stream_slice(
        input,
        0,
        &mut remaining_budget,
        &mut remaining_inflate_budget,
    )?;
    Ok(root)
}

#[inline(never)]
fn inflate_partial_zlib(input: &[u8], capped_inflate: usize) -> Option<Vec<u8>> {
    let mut decoder = ZlibDecoder::new(input);
    let mut out = Vec::new();
    let mut chunk = [0u8; 4096];
    while out.len() < capped_inflate {
        let to_read = chunk.len().min(capped_inflate - out.len());
        match decoder.read(&mut chunk[..to_read]) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Best-effort bounded partial decoder for malformed or truncated `CallReq` frames (`#1086`).
///
/// Decodes only the outer packet header tuple elements `(packet_type, source)` without touching
/// `destination`, `user_id`, `payload`, or `named_payload`, allowing the listener to send an
/// internal error response on the same `call_id` rather than dropping or hanging an RPC.
pub fn decode_partial_call_header(
    input: &[u8],
    max_inflate_bytes: usize,
    max_nodes: usize,
) -> Option<PyRep> {
    if input.is_empty() {
        return None;
    }
    let capped_inflate = max_inflate_bytes.min(MAX_FRAME_SIZE);
    let inflated_buf: Vec<u8>;
    let stream: &[u8] = if is_zlib_stream_header(input) {
        inflated_buf = inflate_partial_zlib(input, capped_inflate)?;
        &inflated_buf
    } else {
        input
    };

    if stream.len() < 5 || stream[0] != MARSHAL_HEADER_BYTE {
        return None;
    }
    let save_count = u32::from_le_bytes(stream[1..5].try_into().ok()?) as usize;
    if save_count > MAX_COLLECTION_LEN {
        return None;
    }
    let map_byte_len = save_count.checked_mul(4)?;
    let (mut body_cursor, mut save_map_cursor, mut saved_slots): (
        &[u8],
        &[u8],
        Vec<Option<SavedSlot>>,
    ) = if stream.len() >= 5 + map_byte_len {
        let body_end = stream.len() - map_byte_len;
        (
            &stream[5..body_end],
            &stream[body_end..],
            vec![None; save_count],
        )
    } else {
        // Truncated before the trailing save map: only saved-slot-free prefixes can decode.
        (&stream[5..], &[][..], Vec::new())
    };

    let mut remaining_budget = max_nodes.clamp(1, 1024);
    let mut remaining_inflate_budget = capped_inflate;
    decode_partial_packet_pair(
        &mut body_cursor,
        &mut save_map_cursor,
        0,
        &mut saved_slots,
        &mut remaining_budget,
        &mut remaining_inflate_budget,
    )
}

fn peek_saved_and_opcode(
    input: &mut &[u8],
    save_map: &mut &[u8],
    saved_slots: &[Option<SavedSlot>],
) -> Option<u8> {
    if input.is_empty() {
        return None;
    }
    let raw = input[0];
    *input = &input[1..];
    if (raw & SAVE_FLAG) != 0 {
        if save_map.len() < 4 {
            return None;
        }
        let idx = u32::from_le_bytes(save_map[..4].try_into().ok()?) as usize;
        *save_map = &save_map[4..];
        if idx == 0 || idx > saved_slots.len() {
            return None;
        }
    }
    Some(raw & 0x3F)
}

fn read_tuple_len_for_opcode(opcode: u8, body: &mut &[u8]) -> Option<usize> {
    if opcode == MarshalOpcode::PyEmptyTuple as u8 {
        Some(0)
    } else if opcode == MarshalOpcode::PyOneTuple as u8 {
        Some(1)
    } else if opcode == MarshalOpcode::PyTwoTuple as u8 {
        Some(2)
    } else if opcode == MarshalOpcode::PyTuple as u8 {
        read_collection_len(body).ok()
    } else {
        None
    }
}

fn decode_partial_packet_pair(
    body: &mut &[u8],
    save_map: &mut &[u8],
    initial_depth: usize,
    saved_slots: &mut Vec<Option<SavedSlot>>,
    remaining_budget: &mut usize,
    remaining_inflate_budget: &mut usize,
) -> Option<PyRep> {
    let mut depth = initial_depth;
    loop {
        if depth > MAX_RECURSION_DEPTH {
            return None;
        }
        *remaining_budget = remaining_budget.checked_sub(1)?;
        let opcode = peek_saved_and_opcode(body, save_map, saved_slots)?;
        match opcode {
            x if x == MarshalOpcode::PyObject as u8 => {
                let (type_rep, _) = decode_inner(
                    body,
                    save_map,
                    depth + 1,
                    saved_slots,
                    remaining_budget,
                    remaining_inflate_budget,
                )
                .ok()?;
                let _ = type_rep.as_str()?;
                depth += 1;
            }
            x if x == MarshalOpcode::PyObjectEx1 as u8
                || x == MarshalOpcode::PyObjectEx2 as u8 =>
            {
                *remaining_budget = remaining_budget.checked_sub(1)?;
                let hdr_op = peek_saved_and_opcode(body, save_map, saved_slots)?;
                let hdr_len = read_tuple_len_for_opcode(hdr_op, body)?;
                if hdr_len < 2 {
                    return None;
                }
                let (_ctor, _) = decode_inner(
                    body,
                    save_map,
                    depth + 2,
                    saved_slots,
                    remaining_budget,
                    remaining_inflate_budget,
                )
                .ok()?;
                depth += 2;
            }
            _ => {
                let len = read_tuple_len_for_opcode(opcode, body)?;
                if len < 2 {
                    return None;
                }
                let (pkt_type, _) = decode_inner(
                    body,
                    save_map,
                    depth + 1,
                    saved_slots,
                    remaining_budget,
                    remaining_inflate_budget,
                )
                .ok()?;
                let (source, _) = decode_inner(
                    body,
                    save_map,
                    depth + 1,
                    saved_slots,
                    remaining_budget,
                    remaining_inflate_budget,
                )
                .ok()?;
                return Some(PyRep::Tuple(vec![pkt_type, source]));
            }
        }
    }
}

#[inline(never)]
fn inflate_zlib_bounded(
    stream: &[u8],
    remaining_inflate_budget: &mut usize,
) -> Result<Vec<u8>, ProtoError> {
    let max_take = (*remaining_inflate_budget as u64).saturating_add(1);
    let mut decoder = ZlibDecoder::new(stream).take(max_take);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| ProtoError::ZlibError(e.to_string()))?;
    if out.len() > *remaining_inflate_budget {
        return Err(ProtoError::FrameTooLarge(out.len()));
    }
    *remaining_inflate_budget -= out.len();
    Ok(out)
}

#[derive(Clone)]
enum StreamBacking<'a> {
    Borrowed(&'a [u8]),
    Shared(Arc<[u8]>),
}

impl<'a> StreamBacking<'a> {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Borrowed(s) => s,
            Self::Shared(buf) => buf,
        }
    }
}

#[derive(Clone)]
struct StreamCursor<'a> {
    backing: StreamBacking<'a>,
    pos: usize,
    end: usize,
}

impl<'a> StreamCursor<'a> {
    fn from_slice(s: &'a [u8]) -> Self {
        Self {
            backing: StreamBacking::Borrowed(s),
            pos: 0,
            end: s.len(),
        }
    }

    fn from_shared(buf: Arc<[u8]>, pos: usize, end: usize) -> Self {
        Self {
            backing: StreamBacking::Shared(buf),
            pos,
            end,
        }
    }

    fn as_slice(&self) -> &[u8] {
        &self.backing.as_slice()[self.pos..self.end]
    }

    fn len(&self) -> usize {
        self.end - self.pos
    }

    fn is_empty(&self) -> bool {
        self.pos >= self.end
    }

    fn peek_u8(&self) -> Option<u8> {
        self.as_slice().first().copied()
    }

    fn take_u8(&mut self) -> Result<u8, ProtoError> {
        let Some(&b) = self.as_slice().first() else {
            return Err(ProtoError::UnexpectedEof);
        };
        self.pos += 1;
        Ok(b)
    }

    fn slice_sub(&mut self, len: usize) -> Result<Self, ProtoError> {
        if self.len() < len {
            return Err(ProtoError::UnexpectedEof);
        }
        let sub = Self {
            backing: self.backing.clone(),
            pos: self.pos,
            end: self.pos + len,
        };
        self.pos += len;
        Ok(sub)
    }

    fn with_slice<R>(&mut self, f: impl FnOnce(&mut &[u8]) -> R) -> R {
        let start = self.pos;
        let mut slice = &self.backing.as_slice()[start..self.end];
        let before = slice.len();
        let out = f(&mut slice);
        self.pos = start + (before - slice.len());
        out
    }
}

struct StreamFrame<'a> {
    body: StreamCursor<'a>,
    save_map: StreamCursor<'a>,
    saved_slots: Vec<Option<SavedSlot>>,
}

/// Validates and opens a raw or `zlib`-compressed `eveMarshal` stream (outer or nested `PySubStream`).
fn open_stream_frame<'a>(
    raw: StreamCursor<'a>,
    depth: usize,
    remaining_inflate_budget: &mut usize,
) -> Result<StreamFrame<'a>, ProtoError> {
    if depth > MAX_RECURSION_DEPTH {
        return Err(ProtoError::RecursionLimitExceeded);
    }
    if raw.is_empty() {
        return Err(ProtoError::UnexpectedEof);
    }

    // Both outer frames and nested `PySubStream` (`0x2B`) payloads produced by V24.01
    // `blue.marshal.Save` may be independently `zlib`-compressed (`0x78 ...` or `0x58 0x09 ...`).
    // All inflations share `remaining_inflate_budget` so nested zip-bombs cannot amplify memory.
    let stream = if is_zlib_stream_header(raw.as_slice()) {
        let out = inflate_zlib_bounded(raw.as_slice(), remaining_inflate_budget)?;
        let out_len = out.len();
        StreamCursor::from_shared(Arc::from(out), 0, out_len)
    } else {
        raw
    };

    let bytes = stream.as_slice();
    if bytes.is_empty() {
        return Err(ProtoError::UnexpectedEof);
    }
    if bytes[0] != MARSHAL_HEADER_BYTE {
        return Err(ProtoError::InvalidHeader(bytes[0]));
    }
    if bytes.len() < 5 {
        return Err(ProtoError::UnexpectedEof);
    }

    let save_count = u32::from_le_bytes(bytes[1..5].try_into().unwrap()) as usize;
    if save_count > MAX_COLLECTION_LEN {
        return Err(ProtoError::CollectionTooLarge(save_count));
    }

    let map_byte_len = save_count.checked_mul(4).ok_or(ProtoError::UnexpectedEof)?;
    if bytes.len() < 5 + map_byte_len {
        return Err(ProtoError::UnexpectedEof);
    }

    let body_end = stream.pos + bytes.len() - map_byte_len;
    let body = StreamCursor {
        backing: stream.backing.clone(),
        pos: stream.pos + 5,
        end: body_end,
    };
    let save_map = StreamCursor {
        backing: stream.backing,
        pos: body_end,
        end: stream.end,
    };

    Ok(StreamFrame {
        body,
        save_map,
        saved_slots: vec![None; save_count],
    })
}

fn decode_stream_slice(
    stream: &[u8],
    depth: usize,
    remaining_budget: &mut usize,
    remaining_inflate_budget: &mut usize,
) -> Result<(PyRep, usize), ProtoError> {
    let frame = open_stream_frame(
        StreamCursor::from_slice(stream),
        depth,
        remaining_inflate_budget,
    )?;
    let (decoded, remaining_body) = decode_from_frame(
        frame,
        depth,
        remaining_budget,
        remaining_inflate_budget,
    )?;
    if remaining_body > 0 {
        return Err(ProtoError::TrailingBytes(remaining_body));
    }
    Ok(decoded)
}

#[inline(never)]
fn read_size_ex_bytes(
    input: &mut &[u8],
    remaining_budget: &mut usize,
) -> Result<Vec<u8>, ProtoError> {
    let len = read_size_ex(input)?;
    if input.len() < len {
        return Err(ProtoError::UnexpectedEof);
    }
    *remaining_budget = remaining_budget
        .checked_sub(len / 64)
        .ok_or(ProtoError::NodeBudgetExceeded)?;
    let bytes = input[..len].to_vec();
    *input = &input[len..];
    Ok(bytes)
}

#[inline(never)]
fn decode_var_integer(input: &mut &[u8]) -> Result<PyRep, ProtoError> {
    let len = read_size_ex(input)?;
    if input.len() < len || len > 9 {
        return Err(ProtoError::UnexpectedEof);
    }
    let mut bytes = [0u8; 8];
    if len == 9 {
        if input[8] != 0x00 && input[8] != 0xFF {
            return Err(ProtoError::UnexpectedEof);
        }
        bytes.copy_from_slice(&input[..8]);
    } else {
        bytes[..len].copy_from_slice(&input[..len]);
        let fill = if len > 0 && (input[len - 1] & 0x80) != 0 {
            0xFF
        } else {
            0x00
        };
        for b in &mut bytes[len..] {
            *b = fill;
        }
    }
    *input = &input[len..];
    Ok(PyRep::Integer(i64::from_le_bytes(bytes)))
}

#[inline(never)]
fn decode_wstring_ucs2(
    input: &mut &[u8],
    remaining_budget: &mut usize,
) -> Result<PyRep, ProtoError> {
    let char_len = read_collection_len(input)?;
    let byte_len = char_len.checked_mul(2).ok_or(ProtoError::UnexpectedEof)?;
    if input.len() < byte_len {
        return Err(ProtoError::UnexpectedEof);
    }
    *remaining_budget = remaining_budget
        .checked_sub(byte_len / 64)
        .ok_or(ProtoError::NodeBudgetExceeded)?;
    let mut units = Vec::with_capacity(char_len.min(1024));
    for chunk in input[..byte_len].chunks_exact(2) {
        units.push(u16::from_le_bytes([chunk[0], chunk[1]]));
    }
    *input = &input[byte_len..];
    let s = String::from_utf16_lossy(&units);
    let extra_utf8 = (s.len() / 64).saturating_sub(byte_len / 64);
    *remaining_budget = remaining_budget
        .checked_sub(extra_utf8)
        .ok_or(ProtoError::NodeBudgetExceeded)?;
    Ok(PyRep::String(s))
}

struct PackedRowPlan {
    header: PyRep,
    columns: Vec<(String, PyRep)>,
    raw_rle: Vec<u8>,
    pending_cols: Vec<(usize, String)>,
    max_child_h: usize,
}

fn setup_packed_row(
    header: PyRep,
    hdr_h: usize,
    body: &mut StreamCursor<'_>,
    remaining_budget: &mut usize,
) -> Result<PackedRowPlan, ProtoError> {
    let rle_len = body.with_slice(read_size_ex)?;
    if body.len() < rle_len {
        return Err(ProtoError::UnexpectedEof);
    }
    *remaining_budget = remaining_budget
        .checked_sub(rle_len / 64)
        .ok_or(ProtoError::NodeBudgetExceeded)?;
    let raw_rle = body.as_slice()[..rle_len].to_vec();
    body.pos += rle_len;

    // Charge column count, column-name byte weights, unpacked fixed-width cells, and
    // unpacked RLE byte-buffer weight BEFORE allocating inside `try_unpack_packed_row_columns`.
    let defs = extract_dbrow_header_col_defs(&header);
    if let Some(ref col_defs) = defs {
        if !col_defs.is_empty() {
            let expected_bytes = crate::dbrow::packed_row_expected_bytes(col_defs);
            if expected_bytes > MAX_COLLECTION_LEN {
                return Err(ProtoError::CollectionTooLarge(expected_bytes));
            }
            let col_weight: usize = col_defs
                .iter()
                .map(|(col_name, dbtype)| {
                    let fixed_cell = usize::from(dbtype_bit_width(*dbtype) > 0);
                    (1 + (col_name.len() / 64)) + fixed_cell
                })
                .sum();
            let unpack_cost = col_weight.saturating_add(expected_bytes / 64);
            *remaining_budget = remaining_budget
                .checked_sub(unpack_cost)
                .ok_or(ProtoError::NodeBudgetExceeded)?;
        }
    }

    let columns = crate::dbrow::try_unpack_packed_row_columns(&header, &raw_rle)?;
    let pending_cols = defs
        .map(|col_defs| {
            col_defs
                .into_iter()
                .enumerate()
                .filter(|(_, (_, dbtype))| dbtype_bit_width(*dbtype) == 0)
                .map(|(idx, (col_name, _))| (idx, col_name))
                .collect()
        })
        .unwrap_or_default();

    Ok(PackedRowPlan {
        header,
        columns,
        raw_rle,
        pending_cols,
        max_child_h: hdr_h,
    })
}

fn build_py_object(type_name: String, args: PyRep) -> PyRep {
    if type_name == "util.KeyVal" || type_name == V24_KEYVAL_CLASS {
        if let PyRep::Dict(entries) = &args {
            let mut map = HashMap::with_capacity(entries.len());
            for (k, v) in entries {
                if let Some(ks) = k.as_str() {
                    map.insert(ks.to_string(), v.clone());
                }
            }
            return PyRep::KeyVal(map);
        }
    } else if type_name == "util.Rowset" || type_name == V24_ROWSET_CLASS {
        if let Some((columns, rows)) = try_extract_rowset(&args) {
            return PyRep::Rowset { columns, rows };
        }
    } else if type_name == "util.IndexRowset" || type_name == V24_INDEX_ROWSET_CLASS {
        if let Some((key_column, columns, items)) = try_extract_index_rowset(&args) {
            return PyRep::IndexRowset {
                key_column,
                columns,
                items,
            };
        }
    } else if type_name == "__builtin__.set"
        || type_name == "builtins.set"
        || type_name == "set"
        || type_name == "builtin.set"
    {
        if let PyRep::Tuple(items) = &args {
            if let Some(PyRep::List(elems)) = items.first() {
                return PyRep::PySet(elems.clone());
            }
            return PyRep::PySet(items.clone());
        } else if let PyRep::List(elems) = &args {
            return PyRep::PySet(elems.clone());
        }
    }

    PyRep::Object {
        type_name,
        args: Box::new(args),
    }
}

fn build_py_object_ex(
    is_type_2: bool,
    header: PyRep,
    list: Vec<PyRep>,
    dict: Vec<(PyRep, PyRep)>,
) -> PyRep {
    if is_type_2 && list.is_empty() && dict.is_empty() {
        if let PyRep::Tuple(h_items) = &header {
            if h_items.len() == 2 {
                if let PyRep::Tuple(ctor_args) = &h_items[0] {
                    if ctor_args.len() == 1
                        && ctor_args[0] == PyRep::Token(V24_ROW_CLASS.into())
                    {
                        return PyRep::Object {
                            type_name: "util.Row".into(),
                            args: Box::new(h_items[1].clone()),
                        };
                    }
                }
            }
        }
    }

    if let Some(elems) = try_extract_object_ex_set(is_type_2, &header, &list, &dict) {
        PyRep::PySet(elems)
    } else {
        PyRep::ObjectEx {
            is_type_2,
            header: Box::new(header),
            list,
            dict,
        }
    }
}

#[inline(never)]
fn decode_saved_element(
    input: &mut &[u8],
    depth: usize,
    saved_slots: &[Option<SavedSlot>],
    remaining_budget: &mut usize,
) -> Result<(PyRep, usize), ProtoError> {
    let idx = read_size_ex(input)?;
    let zero_idx = idx
        .checked_sub(1)
        .ok_or(ProtoError::InvalidSaveReference(idx))?;
    let target = saved_slots
        .get(zero_idx)
        .and_then(|opt| opt.as_ref())
        .ok_or(ProtoError::InvalidSaveReference(idx))?;

    // Reject depth amplification BEFORE paying `node_count()` traversal or `.clone()`.
    if depth.saturating_add(target.height) > MAX_RECURSION_DEPTH {
        return Err(ProtoError::RecursionLimitExceeded);
    }

    // Charge the full expanded node & byte-weight count BEFORE cloning to prevent DoS.
    let extra_nodes = target.rep.node_count().saturating_sub(1);
    *remaining_budget = remaining_budget
        .checked_sub(extra_nodes)
        .ok_or(ProtoError::NodeBudgetExceeded)?;
    Ok((target.rep.clone(), target.height))
}

#[inline(never)]
fn decode_scalar_opcode(
    opcode: u8,
    input: &mut &[u8],
    remaining_budget: &mut usize,
) -> Result<Option<PyRep>, ProtoError> {
    let rep = match opcode {
        x if x == MarshalOpcode::PyNone as u8 => PyRep::None,
        x if x == MarshalOpcode::PyTrue as u8 => PyRep::Bool(true),
        x if x == MarshalOpcode::PyFalse as u8 => PyRep::Bool(false),
        x if x == MarshalOpcode::PyToken as u8 => {
            let s = String::from_utf8(read_size_ex_bytes(input, remaining_budget)?)?;
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
        x if x == MarshalOpcode::PyVarInteger as u8 => decode_var_integer(input)?,
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
            PyRep::Buffer(read_size_ex_bytes(input, remaining_budget)?)
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
            *remaining_budget = remaining_budget
                .checked_sub(len / 64)
                .ok_or(ProtoError::NodeBudgetExceeded)?;
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
            *remaining_budget = remaining_budget
                .checked_sub(s.len() / 64)
                .ok_or(ProtoError::NodeBudgetExceeded)?;
            PyRep::String((*s).to_string())
        }
        x if x == MarshalOpcode::PyWStringUCS2Char as u8 => {
            if input.len() < 2 {
                return Err(ProtoError::UnexpectedEof);
            }
            let code_unit = u16::from_le_bytes([input[0], input[1]]);
            *input = &input[2..];
            PyRep::String(String::from_utf16_lossy(&[code_unit]))
        }
        x if x == MarshalOpcode::PyWStringUCS2 as u8 => {
            decode_wstring_ucs2(input, remaining_budget)?
        }
        x if x == MarshalOpcode::PyLongString as u8 => {
            PyRep::Bytes(read_size_ex_bytes(input, remaining_budget)?)
        }
        x if x == MarshalOpcode::PyPicked as u8 => {
            PyRep::Picked(read_size_ex_bytes(input, remaining_budget)?)
        }
        x if x == MarshalOpcode::PyEmptyWString as u8 => PyRep::String(String::new()),
        x if x == MarshalOpcode::PyWStringUTF8 as u8 => {
            let s = String::from_utf8(read_size_ex_bytes(input, remaining_budget)?)?;
            PyRep::String(s)
        }
        x if x == MarshalOpcode::PyEmptyTuple as u8 => PyRep::Tuple(vec![]),
        x if x == MarshalOpcode::PyEmptyList as u8 => PyRep::List(vec![]),
        _ => return Ok(None),
    };
    Ok(Some(rep))
}

#[inline(never)]
fn save_decoded_slot(
    slot_idx: usize,
    value: &PyRep,
    subtree_height: usize,
    saved_slots: &mut [Option<SavedSlot>],
    remaining_budget: &mut usize,
) -> Result<(), ProtoError> {
    let retained = value.node_count().saturating_sub(1);
    *remaining_budget = remaining_budget
        .checked_sub(retained)
        .ok_or(ProtoError::NodeBudgetExceeded)?;
    saved_slots[slot_idx] = Some(SavedSlot {
        rep: value.clone(),
        height: subtree_height,
    });
    Ok(())
}

struct ObjectExAcc {
    is_type_2: bool,
    child_depth: usize,
    max_child_h: usize,
    header: Option<PyRep>,
    list: Vec<PyRep>,
    dict: Vec<(PyRep, PyRep)>,
}

enum ObjectExPhase {
    Header,
    ListItem,
    DictKey,
    DictVal { key: PyRep },
}

enum DictPhase {
    ExpectVal,
    ExpectKey { val: PyRep },
}

struct PackedRowState {
    header: PyRep,
    columns: Vec<(String, PyRep)>,
    raw_rle: Vec<u8>,
    pending_cols: Vec<(usize, String)>,
    cursor: usize,
    max_child_h: usize,
    child_depth: usize,
}

enum DecodeTask {
    Item {
        depth: usize,
    },
    SaveSlot {
        slot_idx: usize,
    },
    Seq {
        is_tuple: bool,
        items: Vec<PyRep>,
        max_child_h: usize,
        remaining: usize,
        child_depth: usize,
        collect_last: bool,
    },
    Dict {
        entries: Vec<(PyRep, PyRep)>,
        max_child_h: usize,
        remaining: usize,
        child_depth: usize,
        phase: Option<DictPhase>,
    },
    PackedRowHeader {
        child_depth: usize,
    },
    PackedRowCol(Box<PackedRowState>),
    ObjectType {
        opcode: u8,
        child_depth: usize,
    },
    ObjectArgs {
        type_name: String,
        type_h: usize,
    },
    ObjectEx {
        acc: Box<ObjectExAcc>,
        phase: ObjectExPhase,
    },
    FinishSubStruct,
    FinishChecksumedStream {
        checksum: u32,
    },
    FinishSubStream,
}

fn push_leaf_and_optional_save(
    rep: PyRep,
    height: usize,
    storage_index: Option<usize>,
    frame: &mut StreamFrame<'_>,
    results: &mut Vec<(PyRep, usize)>,
    remaining_budget: &mut usize,
) -> Result<(), ProtoError> {
    if let Some(slot_idx) = storage_index {
        save_decoded_slot(
            slot_idx,
            &rep,
            height,
            &mut frame.saved_slots,
            remaining_budget,
        )?;
    }
    results.push((rep, height));
    Ok(())
}

fn advance_object_ex_list(
    acc: Box<ObjectExAcc>,
    body: &mut StreamCursor<'_>,
    work: &mut Vec<DecodeTask>,
    results: &mut Vec<(PyRep, usize)>,
) -> Result<(), ProtoError> {
    match body.peek_u8() {
        None => Err(ProtoError::UnexpectedEof),
        Some(b) if b == MarshalOpcode::PackedTerminator as u8 => {
            body.take_u8()?;
            advance_object_ex_dict(acc, body, work, results)
        }
        Some(_) => {
            if acc.list.len() >= MAX_COLLECTION_LEN {
                return Err(ProtoError::CollectionTooLarge(acc.list.len() + 1));
            }
            let child_depth = acc.child_depth;
            work.push(DecodeTask::ObjectEx {
                acc,
                phase: ObjectExPhase::ListItem,
            });
            work.push(DecodeTask::Item { depth: child_depth });
            Ok(())
        }
    }
}

fn advance_object_ex_dict(
    acc: Box<ObjectExAcc>,
    body: &mut StreamCursor<'_>,
    work: &mut Vec<DecodeTask>,
    results: &mut Vec<(PyRep, usize)>,
) -> Result<(), ProtoError> {
    match body.peek_u8() {
        None => Err(ProtoError::UnexpectedEof),
        Some(b) if b == MarshalOpcode::PackedTerminator as u8 => {
            body.take_u8()?;
            let header = acc.header.expect("object_ex header decoded");
            let rep = build_py_object_ex(acc.is_type_2, header, acc.list, acc.dict);
            results.push((rep, 1 + acc.max_child_h));
            Ok(())
        }
        Some(_) => {
            if acc.dict.len() >= MAX_COLLECTION_LEN {
                return Err(ProtoError::CollectionTooLarge(acc.dict.len() + 1));
            }
            let child_depth = acc.child_depth;
            work.push(DecodeTask::ObjectEx {
                acc,
                phase: ObjectExPhase::DictKey,
            });
            work.push(DecodeTask::Item { depth: child_depth });
            Ok(())
        }
    }
}

fn push_seq_or_empty(
    is_tuple: bool,
    len: usize,
    depth: usize,
    storage_index: Option<usize>,
    st: &mut DecodeState<'_, '_>,
) -> Result<(), ProtoError> {
    let frame = st.streams.last_mut().expect("active stream frame");
    if len == 0 {
        let rep = if is_tuple {
            PyRep::Tuple(Vec::new())
        } else {
            PyRep::List(Vec::new())
        };
        return push_leaf_and_optional_save(
            rep,
            0,
            storage_index,
            frame,
            &mut st.results,
            st.remaining_budget,
        );
    }
    let items = Vec::with_capacity(len.min(frame.body.len()).min(1024));
    if let Some(slot_idx) = storage_index {
        st.work.push(DecodeTask::SaveSlot { slot_idx });
    }
    st.work.push(DecodeTask::Seq {
        is_tuple,
        items,
        max_child_h: 0,
        remaining: len,
        child_depth: depth + 1,
        collect_last: false,
    });
    Ok(())
}

struct DecodeState<'a, 'b> {
    streams: &'b mut Vec<StreamFrame<'a>>,
    work: Vec<DecodeTask>,
    results: Vec<(PyRep, usize)>,
    remaining_budget: &'b mut usize,
    remaining_inflate_budget: &'b mut usize,
}

fn dispatch_decode_item(depth: usize, st: &mut DecodeState<'_, '_>) -> Result<(), ProtoError> {
    if depth > MAX_RECURSION_DEPTH {
        return Err(ProtoError::RecursionLimitExceeded);
    }
    *st.remaining_budget = st
        .remaining_budget
        .checked_sub(1)
        .ok_or(ProtoError::NodeBudgetExceeded)?;

    let frame = st.streams.last_mut().expect("active stream frame");
    let raw_byte = frame.body.take_u8()?;
    let is_saved = (raw_byte & SAVE_FLAG) != 0;
    let opcode = raw_byte & 0x3F;

    // Per EVEUnmarshal.cpp: if `flagSave` is set, read the 1-based storage index from `save_map`
    // BEFORE decoding any children!
    let storage_index = if is_saved {
        if frame.save_map.len() < 4 {
            return Err(ProtoError::InvalidSaveReference(0));
        }
        let s = frame.save_map.as_slice();
        let idx = u32::from_le_bytes(s[..4].try_into().unwrap()) as usize;
        frame.save_map.pos += 4;
        if idx == 0 || idx > frame.saved_slots.len() {
            return Err(ProtoError::InvalidSaveReference(idx));
        }
        Some(idx - 1)
    } else {
        None
    };

    if let Some(scalar) = frame
        .body
        .with_slice(|s| decode_scalar_opcode(opcode, s, st.remaining_budget))?
    {
        return push_leaf_and_optional_save(
            scalar,
            0,
            storage_index,
            frame,
            &mut st.results,
            st.remaining_budget,
        );
    }

    match opcode {
        x if x == MarshalOpcode::PySavedStreamElement as u8 => {
            let saved_slots = &frame.saved_slots;
            let (rep, height) = frame
                .body
                .with_slice(|s| decode_saved_element(s, depth, saved_slots, st.remaining_budget))?;
            push_leaf_and_optional_save(
                rep,
                height,
                storage_index,
                frame,
                &mut st.results,
                st.remaining_budget,
            )
        }
        x if x == MarshalOpcode::PyOneTuple as u8 => {
            push_seq_or_empty(true, 1, depth, storage_index, st)
        }
        x if x == MarshalOpcode::PyTwoTuple as u8 => {
            push_seq_or_empty(true, 2, depth, storage_index, st)
        }
        x if x == MarshalOpcode::PyTuple as u8 => {
            let len = frame.body.with_slice(read_collection_len)?;
            push_seq_or_empty(true, len, depth, storage_index, st)
        }
        x if x == MarshalOpcode::PyOneList as u8 => {
            push_seq_or_empty(false, 1, depth, storage_index, st)
        }
        x if x == MarshalOpcode::PyList as u8 => {
            let len = frame.body.with_slice(read_collection_len)?;
            push_seq_or_empty(false, len, depth, storage_index, st)
        }
        x if x == MarshalOpcode::PyDict as u8 => {
            let len = frame.body.with_slice(read_collection_len)?;
            if len == 0 {
                return push_leaf_and_optional_save(
                    PyRep::Dict(Vec::new()),
                    0,
                    storage_index,
                    frame,
                    &mut st.results,
                    st.remaining_budget,
                );
            }
            let entries = Vec::with_capacity(len.min(frame.body.len() / 2).min(1024));
            if let Some(slot_idx) = storage_index {
                st.work.push(DecodeTask::SaveSlot { slot_idx });
            }
            st.work.push(DecodeTask::Dict {
                entries,
                max_child_h: 0,
                remaining: len,
                child_depth: depth + 1,
                phase: None,
            });
            Ok(())
        }
        x if x == MarshalOpcode::PyPackedRow as u8 => {
            if let Some(slot_idx) = storage_index {
                st.work.push(DecodeTask::SaveSlot { slot_idx });
            }
            st.work.push(DecodeTask::PackedRowHeader {
                child_depth: depth + 1,
            });
            st.work.push(DecodeTask::Item { depth: depth + 1 });
            Ok(())
        }
        x if x == MarshalOpcode::PyObject as u8 => {
            if let Some(slot_idx) = storage_index {
                st.work.push(DecodeTask::SaveSlot { slot_idx });
            }
            st.work.push(DecodeTask::ObjectType {
                opcode,
                child_depth: depth + 1,
            });
            st.work.push(DecodeTask::Item { depth: depth + 1 });
            Ok(())
        }
        x if x == MarshalOpcode::PyObjectEx1 as u8 || x == MarshalOpcode::PyObjectEx2 as u8 => {
            if let Some(slot_idx) = storage_index {
                st.work.push(DecodeTask::SaveSlot { slot_idx });
            }
            st.work.push(DecodeTask::ObjectEx {
                acc: Box::new(ObjectExAcc {
                    is_type_2: opcode == MarshalOpcode::PyObjectEx2 as u8,
                    child_depth: depth + 1,
                    max_child_h: 0,
                    header: None,
                    list: Vec::new(),
                    dict: Vec::new(),
                }),
                phase: ObjectExPhase::Header,
            });
            st.work.push(DecodeTask::Item { depth: depth + 1 });
            Ok(())
        }
        x if x == MarshalOpcode::PySubStruct as u8 => {
            if let Some(slot_idx) = storage_index {
                st.work.push(DecodeTask::SaveSlot { slot_idx });
            }
            st.work.push(DecodeTask::FinishSubStruct);
            st.work.push(DecodeTask::Item { depth: depth + 1 });
            Ok(())
        }
        x if x == MarshalOpcode::PyChecksumedStream as u8 => {
            if frame.body.len() < 4 {
                return Err(ProtoError::UnexpectedEof);
            }
            let checksum = u32::from_le_bytes(frame.body.as_slice()[..4].try_into().unwrap());
            frame.body.pos += 4;
            if let Some(slot_idx) = storage_index {
                st.work.push(DecodeTask::SaveSlot { slot_idx });
            }
            st.work.push(DecodeTask::FinishChecksumedStream { checksum });
            st.work.push(DecodeTask::Item { depth: depth + 1 });
            Ok(())
        }
        x if x == MarshalOpcode::PySubStream as u8 => {
            let len = frame.body.with_slice(read_size_ex)?;
            let sub_cursor = frame.body.slice_sub(len)?;
            let child_frame =
                open_stream_frame(sub_cursor, depth + 1, st.remaining_inflate_budget)?;
            if let Some(slot_idx) = storage_index {
                st.work.push(DecodeTask::SaveSlot { slot_idx });
            }
            st.streams.push(child_frame);
            st.work.push(DecodeTask::FinishSubStream);
            st.work.push(DecodeTask::Item { depth: depth + 1 });
            Ok(())
        }
        other => Err(ProtoError::UnsupportedOpcode(other)),
    }
}

fn step_decode_task(task: DecodeTask, st: &mut DecodeState<'_, '_>) -> Result<(), ProtoError> {
    match task {
        DecodeTask::Item { depth } => dispatch_decode_item(depth, st),
        DecodeTask::SaveSlot { slot_idx } => {
            let frame = st.streams.last_mut().expect("active stream frame");
            let (ref value, subtree_height) = *st.results.last().expect("saved item decoded");
            save_decoded_slot(
                slot_idx,
                value,
                subtree_height,
                &mut frame.saved_slots,
                st.remaining_budget,
            )
        }
        DecodeTask::Seq {
            is_tuple,
            mut items,
            mut max_child_h,
            remaining,
            child_depth,
            collect_last,
        } => {
            if collect_last {
                let (item, child_h) = st.results.pop().expect("seq child decoded");
                max_child_h = max_child_h.max(child_h);
                items.push(item);
            }
            if remaining == 0 {
                let rep = if is_tuple {
                    PyRep::Tuple(items)
                } else {
                    PyRep::List(items)
                };
                st.results.push((rep, 1 + max_child_h));
            } else {
                st.work.push(DecodeTask::Seq {
                    is_tuple,
                    items,
                    max_child_h,
                    remaining: remaining - 1,
                    child_depth,
                    collect_last: true,
                });
                st.work.push(DecodeTask::Item { depth: child_depth });
            }
            Ok(())
        }
        DecodeTask::Dict {
            mut entries,
            mut max_child_h,
            mut remaining,
            child_depth,
            phase,
        } => {
            match phase {
                None => {
                    st.work.push(DecodeTask::Dict {
                        entries,
                        max_child_h,
                        remaining,
                        child_depth,
                        phase: Some(DictPhase::ExpectVal),
                    });
                    st.work.push(DecodeTask::Item { depth: child_depth });
                }
                Some(DictPhase::ExpectVal) => {
                    let (val, vh) = st.results.pop().expect("dict value decoded");
                    max_child_h = max_child_h.max(vh);
                    st.work.push(DecodeTask::Dict {
                        entries,
                        max_child_h,
                        remaining,
                        child_depth,
                        phase: Some(DictPhase::ExpectKey { val }),
                    });
                    st.work.push(DecodeTask::Item { depth: child_depth });
                }
                Some(DictPhase::ExpectKey { val }) => {
                    let (key, kh) = st.results.pop().expect("dict key decoded");
                    max_child_h = max_child_h.max(kh);
                    entries.push((key, val));
                    remaining -= 1;
                    if remaining == 0 {
                        st.results.push((PyRep::Dict(entries), 1 + max_child_h));
                    } else {
                        st.work.push(DecodeTask::Dict {
                            entries,
                            max_child_h,
                            remaining,
                            child_depth,
                            phase: Some(DictPhase::ExpectVal),
                        });
                        st.work.push(DecodeTask::Item { depth: child_depth });
                    }
                }
            }
            Ok(())
        }
        DecodeTask::PackedRowHeader { child_depth } => {
            let (header, hdr_h) = st.results.pop().expect("packed row header decoded");
            let frame = st.streams.last_mut().expect("active stream frame");
            let plan = setup_packed_row(header, hdr_h, &mut frame.body, st.remaining_budget)?;
            if plan.pending_cols.is_empty() {
                st.results.push((
                    PyRep::DBRow {
                        header: Box::new(plan.header),
                        columns: plan.columns,
                        raw_rle: plan.raw_rle,
                    },
                    1 + plan.max_child_h,
                ));
            } else {
                st.work.push(DecodeTask::PackedRowCol(Box::new(PackedRowState {
                    header: plan.header,
                    columns: plan.columns,
                    raw_rle: plan.raw_rle,
                    pending_cols: plan.pending_cols,
                    cursor: 0,
                    max_child_h: plan.max_child_h,
                    child_depth,
                })));
                st.work.push(DecodeTask::Item { depth: child_depth });
            }
            Ok(())
        }
        DecodeTask::PackedRowCol(mut state) => {
            let (val, val_h) = st.results.pop().expect("packed row zero-bit col decoded");
            state.max_child_h = state.max_child_h.max(val_h);
            let (col_idx, ref col_name) = state.pending_cols[state.cursor];
            if let Some(slot) = state.columns.get_mut(col_idx) {
                *slot = (col_name.clone(), val);
            }
            state.cursor += 1;
            if state.cursor == state.pending_cols.len() {
                st.results.push((
                    PyRep::DBRow {
                        header: Box::new(state.header),
                        columns: state.columns,
                        raw_rle: state.raw_rle,
                    },
                    1 + state.max_child_h,
                ));
            } else {
                let child_depth = state.child_depth;
                st.work.push(DecodeTask::PackedRowCol(state));
                st.work.push(DecodeTask::Item { depth: child_depth });
            }
            Ok(())
        }
        DecodeTask::ObjectType {
            opcode,
            child_depth,
        } => {
            let (type_rep, type_h) = st.results.pop().expect("object type decoded");
            let type_name = type_rep
                .as_str()
                .ok_or(ProtoError::UnsupportedOpcode(opcode))?
                .to_string();
            st.work.push(DecodeTask::ObjectArgs { type_name, type_h });
            st.work.push(DecodeTask::Item { depth: child_depth });
            Ok(())
        }
        DecodeTask::ObjectArgs { type_name, type_h } => {
            let (args, args_h) = st.results.pop().expect("object args decoded");
            let height = 1 + type_h.max(args_h);
            st.results.push((build_py_object(type_name, args), height));
            Ok(())
        }
        DecodeTask::ObjectEx { mut acc, phase } => {
            let frame = st.streams.last_mut().expect("active stream frame");
            match phase {
                ObjectExPhase::Header => {
                    let (header, hdr_h) = st.results.pop().expect("object_ex header decoded");
                    acc.header = Some(header);
                    acc.max_child_h = hdr_h;
                    advance_object_ex_list(acc, &mut frame.body, &mut st.work, &mut st.results)
                }
                ObjectExPhase::ListItem => {
                    let (item, item_h) = st.results.pop().expect("object_ex list item decoded");
                    acc.max_child_h = acc.max_child_h.max(item_h);
                    acc.list.push(item);
                    advance_object_ex_list(acc, &mut frame.body, &mut st.work, &mut st.results)
                }
                ObjectExPhase::DictKey => {
                    let (key, kh) = st.results.pop().expect("object_ex dict key decoded");
                    acc.max_child_h = acc.max_child_h.max(kh);
                    let child_depth = acc.child_depth;
                    st.work.push(DecodeTask::ObjectEx {
                        acc,
                        phase: ObjectExPhase::DictVal { key },
                    });
                    st.work.push(DecodeTask::Item { depth: child_depth });
                    Ok(())
                }
                ObjectExPhase::DictVal { key } => {
                    let (val, vh) = st.results.pop().expect("object_ex dict val decoded");
                    acc.max_child_h = acc.max_child_h.max(vh);
                    acc.dict.push((key, val));
                    advance_object_ex_dict(acc, &mut frame.body, &mut st.work, &mut st.results)
                }
            }
        }
        DecodeTask::FinishSubStruct => {
            let (inner, h) = st.results.pop().expect("substruct child decoded");
            st.results.push((PyRep::SubStruct(Box::new(inner)), 1 + h));
            Ok(())
        }
        DecodeTask::FinishChecksumedStream { checksum } => {
            let (body, h) = st.results.pop().expect("checksumed body decoded");
            st.results.push((
                PyRep::ChecksumedStream {
                    checksum,
                    body: Box::new(body),
                },
                1 + h,
            ));
            Ok(())
        }
        DecodeTask::FinishSubStream => {
            let finished = st.streams.pop().expect("substream frame balanced");
            if !finished.body.is_empty() {
                return Err(ProtoError::TrailingBytes(finished.body.len()));
            }
            let (inner, inner_h) = st.results.pop().expect("substream item decoded");
            st.results.push((PyRep::SubStream(Box::new(inner)), 1 + inner_h));
            Ok(())
        }
    }
}

fn run_decode_loop<'a>(
    streams: &mut Vec<StreamFrame<'a>>,
    initial_depth: usize,
    remaining_budget: &mut usize,
    remaining_inflate_budget: &mut usize,
) -> Result<(PyRep, usize), ProtoError> {
    let mut st = DecodeState {
        streams,
        work: vec![DecodeTask::Item {
            depth: initial_depth,
        }],
        results: Vec::new(),
        remaining_budget,
        remaining_inflate_budget,
    };
    while let Some(task) = st.work.pop() {
        step_decode_task(task, &mut st)?;
    }
    Ok(st.results.pop().expect("root item decoded"))
}

fn decode_from_frame<'a>(
    initial_frame: StreamFrame<'a>,
    initial_depth: usize,
    remaining_budget: &mut usize,
    remaining_inflate_budget: &mut usize,
) -> Result<((PyRep, usize), usize), ProtoError> {
    let mut streams = vec![initial_frame];
    let decoded = run_decode_loop(
        &mut streams,
        initial_depth,
        remaining_budget,
        remaining_inflate_budget,
    )?;
    let root_frame = streams.pop().expect("root stream frame");
    Ok((decoded, root_frame.body.len()))
}

fn decode_inner(
    input: &mut &[u8],
    save_map: &mut &[u8],
    depth: usize,
    saved_slots: &mut Vec<Option<SavedSlot>>,
    remaining_budget: &mut usize,
    remaining_inflate_budget: &mut usize,
) -> Result<(PyRep, usize), ProtoError> {
    // Runs the iterative decoder for one item from `(input, save_map)` without requiring trailing
    // bytes to be consumed, while still handling nested `PySubStream` frames iteratively.
    let initial_body = *input;
    let initial_save_map = *save_map;
    let frame = StreamFrame {
        body: StreamCursor::from_slice(initial_body),
        save_map: StreamCursor::from_slice(initial_save_map),
        saved_slots: std::mem::take(saved_slots),
    };
    let mut streams = vec![frame];
    let res = run_decode_loop(
        &mut streams,
        depth,
        remaining_budget,
        remaining_inflate_budget,
    );
    let root = streams.into_iter().next().expect("root stream frame");
    *saved_slots = root.saved_slots;
    if res.is_ok() {
        *input = &initial_body[root.body.pos..];
        *save_map = &initial_save_map[root.save_map.pos..];
    }
    res
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

fn is_set_class_name(rep: &PyRep) -> bool {
    matches!(
        rep.as_str(),
        Some("__builtin__.set" | "builtins.set" | "set" | "builtin.set")
    )
}

fn try_extract_object_ex_set(
    is_type_2: bool,
    header: &PyRep,
    list: &[PyRep],
    dict: &[(PyRep, PyRep)],
) -> Option<Vec<PyRep>> {
    if is_type_2 || !dict.is_empty() {
        return None;
    }
    let PyRep::Tuple(h_items) = header else {
        return None;
    };
    if !h_items.first().is_some_and(is_set_class_name) {
        return None;
    }
    if h_items.len() == 1 {
        return Some(list.to_vec());
    }
    let third_is_empty_state = h_items.len() == 3
        && (matches!(h_items[2], PyRep::None)
            || matches!(&h_items[2], PyRep::Dict(d) if d.is_empty()));
    if h_items.len() == 2 || third_is_empty_state {
        let PyRep::Tuple(ctor_args) = &h_items[1] else {
            return None;
        };
        if ctor_args.is_empty() {
            return Some(list.to_vec());
        }
        if ctor_args.len() == 1 && list.is_empty() {
            return match &ctor_args[0] {
                PyRep::List(elems)
                | PyRep::Tuple(elems)
                | PyRep::PySet(elems)
                | PyRep::Set(elems) => Some(elems.clone()),
                _ => None,
            };
        }
    }
    None
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
/// delegating bit-packed field layout, boolean bits, and per-column null-indicator bitmaps to
/// [`crate::dbrow::pack_packed_row_columns_from_defs`].
pub fn pack_dbrow(cols: &[(&str, i64, PyRep)]) -> PyRep {
    let schema: Vec<(String, i64)> = cols
        .iter()
        .map(|(name, dt, _)| ((*name).to_string(), *dt))
        .collect();
    let descriptor_cols: Vec<PyRep> = schema
        .iter()
        .map(|(name, dt)| PyRep::Tuple(vec![PyRep::String(name.clone()), PyRep::Integer(*dt)]))
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
    let columns: Vec<(String, PyRep)> = cols
        .iter()
        .map(|(n, dt, v)| {
            let norm = crate::dbrow::DbColumnType::from_i64(*dt)
                .map_or_else(|| v.clone(), |ct| ct.normalize_value(v.clone()));
            ((*n).to_string(), norm)
        })
        .collect();
    let raw_rle = crate::dbrow::pack_packed_row_columns_from_defs(&schema, &columns);

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

#[cfg(test)]
#[path = "marshal_unit_tests.rs"]
mod marshal_unit_tests;
#[cfg(test)]
#[path = "marshal_substream_budget_tests.rs"]
mod marshal_substream_budget_tests;


