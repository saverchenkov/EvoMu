//! `quasar` — V24.01 Quasar Protobuf Wire Codec, `FileDescriptorProto` Registry,
//! Dynamic Message Codec, Deterministic Rust Codegen, and Gateway Core Types (`Q-1`, `Q-2`).
//!
//! Loads all 1,455 `google.protobuf.FileDescriptorProto` binary schemas extracted from the
//! V24.01 EVE client's `code.ccp` into local gitignored `data/quasar_descriptors.sqlite`,
//! verifies the tracked `data/quasar_descriptor_manifest.json` SHA-256 and MD5 checksums and
//! per-domain file counts (135 domains, 1,455 files, 6,741 messages), and provides both:
//! - Dynamic schema-driven encoding, decoding, validation, and sample instantiation for every
//!   message across all 1,455 files;
//! - Static strongly-typed Rust structs for the Quasar public/internal gateway envelopes and
//!   shared core types (`eve_public.Request`, `eve_public.Response`, `eve_public.Notice`,
//!   `eve_public.Event`, `eve_public.AuthoritativeContext`, `eve.Request`, `eve.Response`, etc.).

pub mod envelopes;
pub mod registry;
pub mod types;
pub use envelopes::*;
pub use registry::*;
pub use types::*;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{MAX_COLLECTION_LEN, MAX_FRAME_SIZE, MAX_RECURSION_DEPTH};

pub const EXPECTED_QUASAR_DOMAIN_COUNT: usize = 135;
pub const EXPECTED_QUASAR_FILE_COUNT: usize = 1_455;
pub const TYPE_GOOGLEAPIS_PREFIX: &str = "type.googleapis.com/";

#[derive(Debug, Error, Clone, PartialEq)]
pub enum QuasarError {
    #[error("Unexpected EOF while decoding protobuf wire stream")]
    UnexpectedEof,
    #[error("Protobuf varint exceeds 64-bit integer bounds")]
    VarintOverflow,
    #[error("Invalid protobuf wire type: {0}")]
    InvalidWireType(u8),
    #[error("Protobuf frame length {0} exceeds maximum allowed ({MAX_FRAME_SIZE} bytes)")]
    FrameTooLarge(usize),
    #[error("Protobuf recursion depth exceeds maximum allowed ({MAX_RECURSION_DEPTH})")]
    RecursionLimitExceeded,
    #[error("Protobuf collection length {0} exceeds maximum allowed ({MAX_COLLECTION_LEN})")]
    CollectionTooLarge(usize),
    #[error("Invalid UTF-8 string in protobuf field: {0}")]
    InvalidUtf8(String),
    #[error("Wire type mismatch on field '{field}' (#{number}): expected {expected:?}, got {actual:?}")]
    WireTypeMismatch {
        field: String,
        number: u32,
        expected: WireType,
        actual: WireType,
    },
    #[error("Unknown protobuf message type: '{0}'")]
    UnknownMessageType(String),
    #[error("Protobuf type URL mismatch: expected '{expected}', got '{actual}'")]
    TypeUrlMismatch { expected: String, actual: String },
    #[error("Missing Quasar descriptor database at '{0}': run `python3 tools/quasar-import/extract_quasar_descriptors.py`")]
    MissingDescriptorDatabase(String),
    #[error("Quasar descriptor database error: {0}")]
    DatabaseError(String),
    #[error("Quasar descriptor manifest mismatch: {0}")]
    ManifestMismatch(String),
}

// ============================================================================
// Deterministic SHA-256 and MD5 (RFC 6234 & RFC 1321) for manifest verification
// ============================================================================

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
    0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
    0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
    0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
    0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
    0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
    0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
    0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
    0xc67178f2,
];

pub fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity((data.len() + 9).div_ceil(64) * 64);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

pub fn sha256_hex(data: &[u8]) -> String {
    let digest = sha256_bytes(data);
    let mut s = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}

const MD5_S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

const MD5_K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
    0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
    0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
    0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
    0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
    0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
    0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
    0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
    0xeb86d391,
];

pub fn md5_hex(data: &[u8]) -> String {
    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity((data.len() + 9).div_ceil(64) * 64);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_le_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            m[i] = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | ((!b) & d), i),
                16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | (!d)), (7 * i) % 16),
            };
            let temp = d;
            d = c;
            c = b;
            let sum = a
                .wrapping_add(f)
                .wrapping_add(MD5_K[i])
                .wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(MD5_S[i]));
            a = temp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    let mut s = String::with_capacity(32);
    for b in out {
        use std::fmt::Write;
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}

// ============================================================================
// Protobuf Wire Codec
// ============================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireType {
    Varint = 0,
    Fixed64 = 1,
    LengthDelimited = 2,
    Fixed32 = 5,
}

impl WireType {
    pub fn from_u8(v: u8) -> Result<Self, QuasarError> {
        match v {
            0 => Ok(Self::Varint),
            1 => Ok(Self::Fixed64),
            2 => Ok(Self::LengthDelimited),
            5 => Ok(Self::Fixed32),
            other => Err(QuasarError::InvalidWireType(other)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireValue {
    Varint(u64),
    Fixed64(u64),
    LengthDelimited(Vec<u8>),
    Fixed32(u32),
}

impl WireValue {
    pub fn wire_type(&self) -> WireType {
        match self {
            Self::Varint(_) => WireType::Varint,
            Self::Fixed64(_) => WireType::Fixed64,
            Self::LengthDelimited(_) => WireType::LengthDelimited,
            Self::Fixed32(_) => WireType::Fixed32,
        }
    }

    pub fn as_varint(&self) -> Option<u64> {
        match self {
            Self::Varint(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::LengthDelimited(b) => Some(b.as_slice()),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Result<Option<&str>, QuasarError> {
        match self {
            Self::LengthDelimited(b) => std::str::from_utf8(b)
                .map(Some)
                .map_err(|e| QuasarError::InvalidUtf8(e.to_string())),
            _ => Ok(None),
        }
    }

    pub fn as_fixed32(&self) -> Option<u32> {
        match self {
            Self::Fixed32(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_fixed64(&self) -> Option<u64> {
        match self {
            Self::Fixed64(v) => Some(*v),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireField {
    pub field_number: u32,
    pub value: WireValue,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireMessage {
    pub fields: Vec<WireField>,
}

pub fn encode_varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
            out.push(byte);
        } else {
            out.push(byte);
            break;
        }
    }
}

pub fn decode_varint(buf: &[u8], pos: &mut usize) -> Result<u64, QuasarError> {
    let mut result = 0u64;
    let mut shift = 0u32;
    loop {
        if *pos >= buf.len() {
            return Err(QuasarError::UnexpectedEof);
        }
        let byte = buf[*pos];
        *pos += 1;
        if shift >= 64 && (byte > 1 || (byte == 1 && shift > 63)) {
            return Err(QuasarError::VarintOverflow);
        }
        result |= ((byte & 0x7F) as u64) << shift;
        if (byte & 0x80) == 0 {
            return Ok(result);
        }
        shift += 7;
        if shift >= 70 {
            return Err(QuasarError::VarintOverflow);
        }
    }
}

pub fn encode_zigzag32(v: i32) -> u32 {
    ((v << 1) ^ (v >> 31)) as u32
}

pub fn decode_zigzag32(v: u32) -> i32 {
    ((v >> 1) as i32) ^ (-((v & 1) as i32))
}

pub fn encode_zigzag64(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

pub fn decode_zigzag64(v: u64) -> i64 {
    ((v >> 1) as i64) ^ (-((v & 1) as i64))
}

pub fn encode_tag(field_number: u32, wire_type: WireType, out: &mut Vec<u8>) {
    let tag = ((field_number as u64) << 3) | (wire_type as u64);
    encode_varint(tag, out);
}

pub fn encode_varint_field(field_number: u32, value: u64, out: &mut Vec<u8>) {
    encode_tag(field_number, WireType::Varint, out);
    encode_varint(value, out);
}

pub fn encode_bytes_field(field_number: u32, bytes: &[u8], out: &mut Vec<u8>) {
    encode_tag(field_number, WireType::LengthDelimited, out);
    encode_varint(bytes.len() as u64, out);
    out.extend_from_slice(bytes);
}

pub fn encode_string_field(field_number: u32, s: &str, out: &mut Vec<u8>) {
    encode_bytes_field(field_number, s.as_bytes(), out);
}

pub fn encode_fixed32_field(field_number: u32, v: u32, out: &mut Vec<u8>) {
    encode_tag(field_number, WireType::Fixed32, out);
    out.extend_from_slice(&v.to_le_bytes());
}

pub fn encode_fixed64_field(field_number: u32, v: u64, out: &mut Vec<u8>) {
    encode_tag(field_number, WireType::Fixed64, out);
    out.extend_from_slice(&v.to_le_bytes());
}

impl WireMessage {
    pub fn decode(buf: &[u8]) -> Result<Self, QuasarError> {
        if buf.len() > MAX_FRAME_SIZE {
            return Err(QuasarError::FrameTooLarge(buf.len()));
        }
        let mut pos = 0usize;
        let mut fields = Vec::new();
        while pos < buf.len() {
            if fields.len() >= MAX_COLLECTION_LEN {
                return Err(QuasarError::CollectionTooLarge(fields.len()));
            }
            let tag = decode_varint(buf, &mut pos)?;
            let field_number = (tag >> 3) as u32;
            if field_number == 0 {
                return Err(QuasarError::InvalidWireType(0xFF));
            }
            let wire_type = WireType::from_u8((tag & 0x07) as u8)?;
            let value = match wire_type {
                WireType::Varint => WireValue::Varint(decode_varint(buf, &mut pos)?),
                WireType::Fixed64 => {
                    if pos + 8 > buf.len() {
                        return Err(QuasarError::UnexpectedEof);
                    }
                    let mut arr = [0u8; 8];
                    arr.copy_from_slice(&buf[pos..pos + 8]);
                    pos += 8;
                    WireValue::Fixed64(u64::from_le_bytes(arr))
                }
                WireType::LengthDelimited => {
                    let len = decode_varint(buf, &mut pos)? as usize;
                    if len > MAX_FRAME_SIZE || pos + len > buf.len() {
                        return Err(QuasarError::UnexpectedEof);
                    }
                    let slice = buf[pos..pos + len].to_vec();
                    pos += len;
                    WireValue::LengthDelimited(slice)
                }
                WireType::Fixed32 => {
                    if pos + 4 > buf.len() {
                        return Err(QuasarError::UnexpectedEof);
                    }
                    let mut arr = [0u8; 4];
                    arr.copy_from_slice(&buf[pos..pos + 4]);
                    pos += 4;
                    WireValue::Fixed32(u32::from_le_bytes(arr))
                }
            };
            fields.append(&mut vec![WireField {
                field_number,
                value,
            }]);
        }
        Ok(Self { fields })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for f in &self.fields {
            match &f.value {
                WireValue::Varint(v) => encode_varint_field(f.field_number, *v, &mut out),
                WireValue::Fixed64(v) => encode_fixed64_field(f.field_number, *v, &mut out),
                WireValue::LengthDelimited(b) => encode_bytes_field(f.field_number, b, &mut out),
                WireValue::Fixed32(v) => encode_fixed32_field(f.field_number, *v, &mut out),
            }
        }
        out
    }
}

// ============================================================================
// FileDescriptorProto Schema Model & Codec
// ============================================================================

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldType {
    Double = 1,
    Float = 2,
    Int64 = 3,
    Uint64 = 4,
    Int32 = 5,
    Fixed64 = 6,
    Fixed32 = 7,
    Bool = 8,
    String = 9,
    Group = 10,
    Message = 11,
    Bytes = 12,
    Uint32 = 13,
    Enum = 14,
    Sfixed32 = 15,
    Sfixed64 = 16,
    Sint32 = 17,
    Sint64 = 18,
}

impl FieldType {
    pub fn from_i32(v: i32) -> Self {
        match v {
            1 => Self::Double,
            2 => Self::Float,
            3 => Self::Int64,
            4 => Self::Uint64,
            5 => Self::Int32,
            6 => Self::Fixed64,
            7 => Self::Fixed32,
            8 => Self::Bool,
            9 => Self::String,
            10 => Self::Group,
            11 => Self::Message,
            12 => Self::Bytes,
            13 => Self::Uint32,
            14 => Self::Enum,
            15 => Self::Sfixed32,
            16 => Self::Sfixed64,
            17 => Self::Sint32,
            18 => Self::Sint64,
            _ => Self::Bytes,
        }
    }

    pub fn expected_wire_type(self) -> WireType {
        match self {
            Self::Double | Self::Fixed64 | Self::Sfixed64 => WireType::Fixed64,
            Self::Float | Self::Fixed32 | Self::Sfixed32 => WireType::Fixed32,
            Self::String | Self::Message | Self::Bytes | Self::Group => WireType::LengthDelimited,
            Self::Int64
            | Self::Uint64
            | Self::Int32
            | Self::Bool
            | Self::Uint32
            | Self::Enum
            | Self::Sint32
            | Self::Sint64 => WireType::Varint,
        }
    }

    pub fn is_packable_scalar(self) -> bool {
        !matches!(self, Self::String | Self::Message | Self::Bytes | Self::Group)
    }
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldLabel {
    Optional = 1,
    Required = 2,
    Repeated = 3,
}

impl FieldLabel {
    pub fn from_i32(v: i32) -> Self {
        match v {
            2 => Self::Required,
            3 => Self::Repeated,
            _ => Self::Optional,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDescriptorProto {
    pub name: String,
    pub number: u32,
    pub label: FieldLabel,
    pub field_type: FieldType,
    pub type_name: String,
    pub oneof_index: Option<u32>,
    pub json_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnumValueDescriptorProto {
    pub name: String,
    pub number: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnumDescriptorProto {
    pub name: String,
    pub full_name: String,
    pub values: Vec<EnumValueDescriptorProto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OneofDescriptorProto {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescriptorProto {
    pub name: String,
    pub full_name: String,
    pub fields: Vec<FieldDescriptorProto>,
    pub nested_types: Vec<DescriptorProto>,
    pub enum_types: Vec<EnumDescriptorProto>,
    pub oneofs: Vec<OneofDescriptorProto>,
    pub is_map_entry: bool,
}

impl DescriptorProto {
    pub fn type_url(&self) -> String {
        format!("{TYPE_GOOGLEAPIS_PREFIX}{}", self.full_name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MethodDescriptorProto {
    pub name: String,
    pub input_type: String,
    pub output_type: String,
    pub client_streaming: bool,
    pub server_streaming: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceDescriptorProto {
    pub name: String,
    pub full_name: String,
    pub methods: Vec<MethodDescriptorProto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDescriptorProto {
    pub pyj_path: String,
    pub domain: String,
    pub name: String,
    pub package: String,
    pub syntax: String,
    pub dependencies: Vec<String>,
    pub messages: Vec<DescriptorProto>,
    pub enums: Vec<EnumDescriptorProto>,
    pub services: Vec<ServiceDescriptorProto>,
    pub sha256: String,
    pub md5: String,
    pub raw_bytes: Vec<u8>,
}

fn qualify_name(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}.{name}")
    }
}

fn decode_field_descriptor(buf: &[u8]) -> Result<FieldDescriptorProto, QuasarError> {
    let wm = WireMessage::decode(buf)?;
    let mut name = String::new();
    let mut number = 0u32;
    let mut label = FieldLabel::Optional;
    let mut field_type = FieldType::Bytes;
    let mut type_name = String::new();
    let mut oneof_index = None;
    let mut json_name = String::new();

    for f in wm.fields {
        match f.field_number {
            1 => {
                if let Some(s) = f.value.as_str()? {
                    name = s.to_string();
                }
            }
            3 => {
                if let Some(v) = f.value.as_varint() {
                    number = v as u32;
                }
            }
            4 => {
                if let Some(v) = f.value.as_varint() {
                    label = FieldLabel::from_i32(v as i32);
                }
            }
            5 => {
                if let Some(v) = f.value.as_varint() {
                    field_type = FieldType::from_i32(v as i32);
                }
            }
            6 => {
                if let Some(s) = f.value.as_str()? {
                    type_name = s.trim_start_matches('.').to_string();
                }
            }
            9 => {
                if let Some(v) = f.value.as_varint() {
                    oneof_index = Some(v as u32);
                }
            }
            10 => {
                if let Some(s) = f.value.as_str()? {
                    json_name = s.to_string();
                }
            }
            _ => {}
        }
    }

    Ok(FieldDescriptorProto {
        name,
        number,
        label,
        field_type,
        type_name,
        oneof_index,
        json_name,
    })
}

fn decode_enum_descriptor(parent_prefix: &str, buf: &[u8]) -> Result<EnumDescriptorProto, QuasarError> {
    let wm = WireMessage::decode(buf)?;
    let mut name = String::new();
    let mut values = Vec::new();
    for f in wm.fields {
        match f.field_number {
            1 => {
                if let Some(s) = f.value.as_str()? {
                    name = s.to_string();
                }
            }
            2 => {
                if let Some(sub) = f.value.as_bytes() {
                    let vwm = WireMessage::decode(sub)?;
                    let mut vname = String::new();
                    let mut vnum = 0i32;
                    for vf in vwm.fields {
                        match vf.field_number {
                            1 => {
                                if let Some(s) = vf.value.as_str()? {
                                    vname = s.to_string();
                                }
                            }
                            2 => {
                                if let Some(v) = vf.value.as_varint() {
                                    vnum = v as i32;
                                }
                            }
                            _ => {}
                        }
                    }
                    values.push(EnumValueDescriptorProto {
                        name: vname,
                        number: vnum,
                    });
                }
            }
            _ => {}
        }
    }
    let full_name = qualify_name(parent_prefix, &name);
    Ok(EnumDescriptorProto {
        name,
        full_name,
        values,
    })
}

fn decode_message_descriptor(
    parent_prefix: &str,
    buf: &[u8],
    depth: usize,
) -> Result<DescriptorProto, QuasarError> {
    if depth > MAX_RECURSION_DEPTH {
        return Err(QuasarError::RecursionLimitExceeded);
    }
    let wm = WireMessage::decode(buf)?;
    let mut name = String::new();
    let mut raw_fields = Vec::new();
    let mut raw_nested = Vec::new();
    let mut raw_enums = Vec::new();
    let mut oneofs = Vec::new();
    let mut is_map_entry = false;

    for f in wm.fields {
        match f.field_number {
            1 => {
                if let Some(s) = f.value.as_str()? {
                    name = s.to_string();
                }
            }
            2 => {
                if let Some(sub) = f.value.as_bytes() {
                    raw_fields.push(decode_field_descriptor(sub)?);
                }
            }
            3 => {
                if let Some(sub) = f.value.as_bytes() {
                    raw_nested.push(sub.to_vec());
                }
            }
            4 => {
                if let Some(sub) = f.value.as_bytes() {
                    raw_enums.push(sub.to_vec());
                }
            }
            7 => {
                if let Some(sub) = f.value.as_bytes() {
                    let owm = WireMessage::decode(sub)?;
                    for of in owm.fields {
                        if of.field_number == 7 && of.value.as_varint().unwrap_or(0) != 0 {
                            is_map_entry = true;
                        }
                    }
                }
            }
            8 => {
                if let Some(sub) = f.value.as_bytes() {
                    let owm = WireMessage::decode(sub)?;
                    let mut oname = String::new();
                    for of in owm.fields {
                        if of.field_number == 1 {
                            if let Some(s) = of.value.as_str()? {
                                oname = s.to_string();
                            }
                        }
                    }
                    oneofs.push(OneofDescriptorProto { name: oname });
                }
            }
            _ => {}
        }
    }

    let full_name = qualify_name(parent_prefix, &name);
    let mut nested_types = Vec::with_capacity(raw_nested.len());
    for nb in raw_nested {
        nested_types.push(decode_message_descriptor(&full_name, &nb, depth + 1)?);
    }
    let mut enum_types = Vec::with_capacity(raw_enums.len());
    for eb in raw_enums {
        enum_types.push(decode_enum_descriptor(&full_name, &eb)?);
    }

    Ok(DescriptorProto {
        name,
        full_name,
        fields: raw_fields,
        nested_types,
        enum_types,
        oneofs,
        is_map_entry,
    })
}

fn decode_service_descriptor(
    parent_prefix: &str,
    buf: &[u8],
) -> Result<ServiceDescriptorProto, QuasarError> {
    let wm = WireMessage::decode(buf)?;
    let mut name = String::new();
    let mut methods = Vec::new();
    for f in wm.fields {
        match f.field_number {
            1 => {
                if let Some(s) = f.value.as_str()? {
                    name = s.to_string();
                }
            }
            2 => {
                if let Some(sub) = f.value.as_bytes() {
                    let mwm = WireMessage::decode(sub)?;
                    let mut mname = String::new();
                    let mut input_type = String::new();
                    let mut output_type = String::new();
                    let mut client_streaming = false;
                    let mut server_streaming = false;
                    for mf in mwm.fields {
                        match mf.field_number {
                            1 => {
                                if let Some(s) = mf.value.as_str()? {
                                    mname = s.to_string();
                                }
                            }
                            2 => {
                                if let Some(s) = mf.value.as_str()? {
                                    input_type = s.trim_start_matches('.').to_string();
                                }
                            }
                            3 => {
                                if let Some(s) = mf.value.as_str()? {
                                    output_type = s.trim_start_matches('.').to_string();
                                }
                            }
                            5 => {
                                client_streaming = mf.value.as_varint().unwrap_or(0) != 0;
                            }
                            6 => {
                                server_streaming = mf.value.as_varint().unwrap_or(0) != 0;
                            }
                            _ => {}
                        }
                    }
                    methods.push(MethodDescriptorProto {
                        name: mname,
                        input_type,
                        output_type,
                        client_streaming,
                        server_streaming,
                    });
                }
            }
            _ => {}
        }
    }
    let full_name = qualify_name(parent_prefix, &name);
    Ok(ServiceDescriptorProto {
        name,
        full_name,
        methods,
    })
}

impl FileDescriptorProto {
    pub fn decode_wire(
        pyj_path: impl Into<String>,
        domain: impl Into<String>,
        raw_bytes: Vec<u8>,
    ) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(&raw_bytes)?;
        let mut name = String::new();
        let mut package = String::new();
        let mut syntax = "proto2".to_string();
        let mut dependencies = Vec::new();
        let mut raw_messages = Vec::new();
        let mut raw_enums = Vec::new();
        let mut raw_services = Vec::new();

        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(s) = f.value.as_str()? {
                        name = s.to_string();
                    }
                }
                2 => {
                    if let Some(s) = f.value.as_str()? {
                        package = s.to_string();
                    }
                }
                3 => {
                    if let Some(s) = f.value.as_str()? {
                        dependencies.push(s.to_string());
                    }
                }
                4 => {
                    if let Some(sub) = f.value.as_bytes() {
                        raw_messages.push(sub.to_vec());
                    }
                }
                5 => {
                    if let Some(sub) = f.value.as_bytes() {
                        raw_enums.push(sub.to_vec());
                    }
                }
                6 => {
                    if let Some(sub) = f.value.as_bytes() {
                        raw_services.push(sub.to_vec());
                    }
                }
                12 => {
                    if let Some(s) = f.value.as_str()? {
                        syntax = s.to_string();
                    }
                }
                _ => {}
            }
        }

        let mut messages = Vec::with_capacity(raw_messages.len());
        for mb in raw_messages {
            messages.push(decode_message_descriptor(&package, &mb, 0)?);
        }
        let mut enums = Vec::with_capacity(raw_enums.len());
        for eb in raw_enums {
            enums.push(decode_enum_descriptor(&package, &eb)?);
        }
        let mut services = Vec::with_capacity(raw_services.len());
        for sb in raw_services {
            services.push(decode_service_descriptor(&package, &sb)?);
        }

        let sha256 = sha256_hex(&raw_bytes);
        let md5 = md5_hex(&raw_bytes);

        Ok(Self {
            pyj_path: pyj_path.into(),
            domain: domain.into(),
            name,
            package,
            syntax,
            dependencies,
            messages,
            enums,
            services,
            sha256,
            md5,
            raw_bytes,
        })
    }

    /// Re-encodes this `FileDescriptorProto` into canonical `google.protobuf.FileDescriptorProto` wire bytes.
    pub fn encode_wire(&self) -> Vec<u8> {
        fn encode_field_desc(f: &FieldDescriptorProto) -> Vec<u8> {
            let mut out = Vec::new();
            encode_string_field(1, &f.name, &mut out);
            encode_varint_field(3, f.number as u64, &mut out);
            encode_varint_field(4, f.label as i32 as u64, &mut out);
            encode_varint_field(5, f.field_type as i32 as u64, &mut out);
            if !f.type_name.is_empty() {
                encode_string_field(6, &format!(".{}", f.type_name), &mut out);
            }
            if let Some(idx) = f.oneof_index {
                encode_varint_field(9, idx as u64, &mut out);
            }
            if !f.json_name.is_empty() {
                encode_string_field(10, &f.json_name, &mut out);
            }
            out
        }

        fn encode_enum_desc(e: &EnumDescriptorProto) -> Vec<u8> {
            let mut out = Vec::new();
            encode_string_field(1, &e.name, &mut out);
            for v in &e.values {
                let mut vbuf = Vec::new();
                encode_string_field(1, &v.name, &mut vbuf);
                encode_varint_field(2, v.number as i64 as u64, &mut vbuf);
                encode_bytes_field(2, &vbuf, &mut out);
            }
            out
        }

        fn encode_msg_desc(m: &DescriptorProto) -> Vec<u8> {
            let mut out = Vec::new();
            encode_string_field(1, &m.name, &mut out);
            for f in &m.fields {
                encode_bytes_field(2, &encode_field_desc(f), &mut out);
            }
            for n in &m.nested_types {
                encode_bytes_field(3, &encode_msg_desc(n), &mut out);
            }
            for e in &m.enum_types {
                encode_bytes_field(4, &encode_enum_desc(e), &mut out);
            }
            if m.is_map_entry {
                let mut opt = Vec::new();
                encode_varint_field(7, 1, &mut opt);
                encode_bytes_field(7, &opt, &mut out);
            }
            for o in &m.oneofs {
                let mut obuf = Vec::new();
                encode_string_field(1, &o.name, &mut obuf);
                encode_bytes_field(8, &obuf, &mut out);
            }
            out
        }

        let mut out = Vec::new();
        encode_string_field(1, &self.name, &mut out);
        if !self.package.is_empty() {
            encode_string_field(2, &self.package, &mut out);
        }
        for dep in &self.dependencies {
            encode_string_field(3, dep, &mut out);
        }
        for m in &self.messages {
            encode_bytes_field(4, &encode_msg_desc(m), &mut out);
        }
        for e in &self.enums {
            encode_bytes_field(5, &encode_enum_desc(e), &mut out);
        }
        for s in &self.services {
            let mut sbuf = Vec::new();
            encode_string_field(1, &s.name, &mut sbuf);
            for m in &s.methods {
                let mut mbuf = Vec::new();
                encode_string_field(1, &m.name, &mut mbuf);
                encode_string_field(2, &format!(".{}", m.input_type), &mut mbuf);
                encode_string_field(3, &format!(".{}", m.output_type), &mut mbuf);
                if m.client_streaming {
                    encode_varint_field(5, 1, &mut mbuf);
                }
                if m.server_streaming {
                    encode_varint_field(6, 1, &mut mbuf);
                }
                encode_bytes_field(2, &mbuf, &mut sbuf);
            }
            encode_bytes_field(6, &sbuf, &mut out);
        }
        if !self.syntax.is_empty() {
            encode_string_field(12, &self.syntax, &mut out);
        }
        out
    }
}
