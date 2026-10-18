//! Strongly-typed V24.01 Quasar Public/Internal Gateway Envelopes and Core Protobuf Types.
//!
//! Every struct in this module serializes and deserializes on the Protobuf binary wire
//! identically to the V24.01 client's `eveProto/generated/*_pb2.pyj` definitions.

use serde::{Deserialize, Serialize};

use super::{
    decode_zigzag32, encode_bytes_field, encode_fixed32_field, encode_fixed64_field,
    encode_string_field, encode_varint_field, encode_zigzag32, QuasarError, WireMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};

pub trait QuasarMessage: Sized + Clone {
    const FULL_NAME: &'static str;

    fn type_url() -> String {
        format!("{TYPE_GOOGLEAPIS_PREFIX}{}", Self::FULL_NAME)
    }

    fn encode_proto(&self) -> Vec<u8>;
    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError>;

    fn to_any(&self) -> GoogleAny {
        GoogleAny {
            type_url: Self::type_url(),
            value: self.encode_proto(),
        }
    }

    fn from_any(any: &GoogleAny) -> Result<Self, QuasarError> {
        let expected = Self::type_url();
        let norm_actual = any
            .type_url
            .strip_prefix(TYPE_GOOGLEAPIS_PREFIX)
            .unwrap_or(&any.type_url)
            .trim_start_matches('.');
        if norm_actual != Self::FULL_NAME {
            return Err(QuasarError::TypeUrlMismatch {
                expected,
                actual: any.type_url.clone(),
            });
        }
        Self::decode_proto(&any.value)
    }
}

// ============================================================================
// Well-Known Google Protobuf Types (Timestamp, Duration, Any, Empty)
// ============================================================================

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleTimestamp {
    pub seconds: i64,
    pub nanos: i32,
}

impl QuasarMessage for GoogleTimestamp {
    const FULL_NAME: &'static str = "google.protobuf.Timestamp";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.seconds != 0 {
            encode_varint_field(1, self.seconds as u64, &mut out);
        }
        if self.nanos != 0 {
            encode_varint_field(2, self.nanos as i64 as u64, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.seconds = v as i64;
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res.nanos = v as i32;
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleDuration {
    pub seconds: i64,
    pub nanos: i32,
}

impl QuasarMessage for GoogleDuration {
    const FULL_NAME: &'static str = "google.protobuf.Duration";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.seconds != 0 {
            encode_varint_field(1, self.seconds as u64, &mut out);
        }
        if self.nanos != 0 {
            encode_varint_field(2, self.nanos as i64 as u64, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.seconds = v as i64;
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res.nanos = v as i32;
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleAny {
    pub type_url: String,
    pub value: Vec<u8>,
}

impl GoogleAny {
    pub fn normalized_type_name(&self) -> &str {
        self.type_url
            .strip_prefix(TYPE_GOOGLEAPIS_PREFIX)
            .unwrap_or(&self.type_url)
            .trim_start_matches('.')
    }
}

impl QuasarMessage for GoogleAny {
    const FULL_NAME: &'static str = "google.protobuf.Any";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.type_url.is_empty() {
            encode_string_field(1, &self.type_url, &mut out);
        }
        if !self.value.is_empty() {
            encode_bytes_field(2, &self.value, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(s) = f.value.as_str()? {
                        res.type_url = s.to_string();
                    }
                }
                2 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.value = b.to_vec();
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleEmpty;

impl QuasarMessage for GoogleEmpty {
    const FULL_NAME: &'static str = "google.protobuf.Empty";

    fn encode_proto(&self) -> Vec<u8> {
        Vec::new()
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let _ = WireMessage::decode(buf)?;
        Ok(Self)
    }
}

// ============================================================================
// Core Identity, TraceContext, Idempotency & Shared Primitives (`feat:quasar.core-types`)
// ============================================================================

/// `.eve_public.user.Identifier` — field 1 `sequential` is `sint32` (ZigZag varint).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvePublicUserId {
    pub sequential: i32,
}

impl QuasarMessage for EvePublicUserId {
    const FULL_NAME: &'static str = "eve_public.user.Identifier";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.sequential != 0 {
            encode_varint_field(1, encode_zigzag32(self.sequential) as u64, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            if f.field_number == 1 {
                if let Some(v) = f.value.as_varint() {
                    res.sequential = decode_zigzag32(v as u32);
                }
            }
        }
        Ok(res)
    }
}

/// `.eve_public.character.Identifier` — field 1 `sequential` is `uint32` (varint).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvePublicCharacterId {
    pub sequential: u32,
}

impl QuasarMessage for EvePublicCharacterId {
    const FULL_NAME: &'static str = "eve_public.character.Identifier";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.sequential != 0 {
            encode_varint_field(1, self.sequential as u64, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            if f.field_number == 1 {
                if let Some(v) = f.value.as_varint() {
                    res.sequential = v as u32;
                }
            }
        }
        Ok(res)
    }
}

/// `.eve_public.corporation.Identifier` — field 1 `sequential` is `uint32` (varint).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvePublicCorporationId {
    pub sequential: u32,
}

impl QuasarMessage for EvePublicCorporationId {
    const FULL_NAME: &'static str = "eve_public.corporation.Identifier";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.sequential != 0 {
            encode_varint_field(1, self.sequential as u64, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            if f.field_number == 1 {
                if let Some(v) = f.value.as_varint() {
                    res.sequential = v as u32;
                }
            }
        }
        Ok(res)
    }
}

/// `.eve_public.corporation.role.Collection` — field 1 `bitmask` is `uint64` (varint).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvePublicCorpRoleCollection {
    pub bitmask: u64,
}

impl QuasarMessage for EvePublicCorpRoleCollection {
    const FULL_NAME: &'static str = "eve_public.corporation.role.Collection";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.bitmask != 0 {
            encode_varint_field(1, self.bitmask, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            if f.field_number == 1 {
                if let Some(v) = f.value.as_varint() {
                    res.bitmask = v;
                }
            }
        }
        Ok(res)
    }
}

/// `.eve.idempotency.Token` — field 1 `uuid` is `bytes`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EveIdempotencyToken {
    pub uuid: Vec<u8>,
}

impl QuasarMessage for EveIdempotencyToken {
    const FULL_NAME: &'static str = "eve.idempotency.Token";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.uuid.is_empty() {
            encode_bytes_field(1, &self.uuid, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            if f.field_number == 1 {
                if let Some(b) = f.value.as_bytes() {
                    res.uuid = b.to_vec();
                }
            }
        }
        Ok(res)
    }
}

/// `.eve_public.IPAddress` / `.eve.IPAddress` — oneof `version` { `v4: fixed32` (1), `v6: bytes` (2) }.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EveIpAddress {
    V4(u32),
    V6(Vec<u8>),
}

impl Default for EveIpAddress {
    fn default() -> Self {
        Self::V4(0x7F00_0001)
    }
}

impl QuasarMessage for EveIpAddress {
    const FULL_NAME: &'static str = "eve_public.IPAddress";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::V4(v4) => encode_fixed32_field(1, *v4, &mut out),
            Self::V6(v6) => encode_bytes_field(2, v6, &mut out),
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::V4(0);
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_fixed32() {
                        res = Self::V4(v);
                    }
                }
                2 => {
                    if let Some(b) = f.value.as_bytes() {
                        res = Self::V6(b.to_vec());
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceFlags {
    pub sampled: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceState {
    pub sample_rate: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceContext {
    pub trace_id: Vec<u8>,
    pub parent_id: Vec<u8>,
    pub flags: Option<TraceFlags>,
    pub state: Option<TraceState>,
}

impl QuasarMessage for TraceContext {
    const FULL_NAME: &'static str = "eve_public.tracecontext.TraceContext";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.trace_id.is_empty() {
            encode_bytes_field(1, &self.trace_id, &mut out);
        }
        if !self.parent_id.is_empty() {
            encode_bytes_field(2, &self.parent_id, &mut out);
        }
        if let Some(flags) = &self.flags {
            let mut fbuf = Vec::new();
            if flags.sampled {
                encode_varint_field(1, 1, &mut fbuf);
            }
            encode_bytes_field(3, &fbuf, &mut out);
        }
        if let Some(state) = &self.state {
            let mut sbuf = Vec::new();
            if state.sample_rate != 0 {
                encode_varint_field(1, state.sample_rate as u64, &mut sbuf);
            }
            encode_bytes_field(4, &sbuf, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.trace_id = b.to_vec();
                    }
                }
                2 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.parent_id = b.to_vec();
                    }
                }
                3 => {
                    if let Some(b) = f.value.as_bytes() {
                        let fwm = WireMessage::decode(b)?;
                        let mut sampled = false;
                        for ff in fwm.fields {
                            if ff.field_number == 1 {
                                sampled = ff.value.as_varint().unwrap_or(0) != 0;
                            }
                        }
                        res.flags = Some(TraceFlags { sampled });
                    }
                }
                4 => {
                    if let Some(b) = f.value.as_bytes() {
                        let swm = WireMessage::decode(b)?;
                        let mut sample_rate = 0u32;
                        for sf in swm.fields {
                            if sf.field_number == 1 {
                                sample_rate = sf.value.as_varint().unwrap_or(0) as u32;
                            }
                        }
                        res.state = Some(TraceState { sample_rate });
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticVersionSpec {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub prerelease_tags: Vec<String>,
    pub build_tags: Vec<String>,
}

impl QuasarMessage for SemanticVersionSpec {
    const FULL_NAME: &'static str = "eve_public.semanticversion.Specification";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.major != 0 {
            encode_varint_field(1, self.major as u64, &mut out);
        }
        if self.minor != 0 {
            encode_varint_field(2, self.minor as u64, &mut out);
        }
        if self.patch != 0 {
            encode_varint_field(3, self.patch as u64, &mut out);
        }
        for t in &self.prerelease_tags {
            encode_string_field(4, t, &mut out);
        }
        for t in &self.build_tags {
            encode_string_field(5, t, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => res.major = f.value.as_varint().unwrap_or(0) as u32,
                2 => res.minor = f.value.as_varint().unwrap_or(0) as u32,
                3 => res.patch = f.value.as_varint().unwrap_or(0) as u32,
                4 => {
                    if let Some(s) = f.value.as_str()? {
                        res.prerelease_tags.push(s.to_string());
                    }
                }
                5 => {
                    if let Some(s) = f.value.as_str()? {
                        res.build_tags.push(s.to_string());
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColorRgb {
    pub red: u32,
    pub green: u32,
    pub blue: u32,
}

impl QuasarMessage for ColorRgb {
    const FULL_NAME: &'static str = "eve_public.color.RGB";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.red != 0 {
            encode_varint_field(1, self.red as u64, &mut out);
        }
        if self.green != 0 {
            encode_varint_field(2, self.green as u64, &mut out);
        }
        if self.blue != 0 {
            encode_varint_field(3, self.blue as u64, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => res.red = f.value.as_varint().unwrap_or(0) as u32,
                2 => res.green = f.value.as_varint().unwrap_or(0) as u32,
                3 => res.blue = f.value.as_varint().unwrap_or(0) as u32,
                _ => {}
            }
        }
        Ok(res)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MathFraction {
    pub numerator: u64,
    pub denominator: u64,
}

impl QuasarMessage for MathFraction {
    const FULL_NAME: &'static str = "eve_public.math.Fraction";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.numerator != 0 {
            encode_varint_field(1, self.numerator, &mut out);
        }
        if self.denominator != 0 {
            encode_varint_field(2, self.denominator, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => res.numerator = f.value.as_varint().unwrap_or(0),
                2 => res.denominator = f.value.as_varint().unwrap_or(0),
                _ => {}
            }
        }
        Ok(res)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct MathVector3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl QuasarMessage for MathVector3 {
    const FULL_NAME: &'static str = "eve_public.math.Vector3";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.x.to_bits() != 0 {
            encode_fixed64_field(1, self.x.to_bits(), &mut out);
        }
        if self.y.to_bits() != 0 {
            encode_fixed64_field(2, self.y.to_bits(), &mut out);
        }
        if self.z.to_bits() != 0 {
            encode_fixed64_field(3, self.z.to_bits(), &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => res.x = f64::from_bits(f.value.as_fixed64().unwrap_or(0)),
                2 => res.y = f64::from_bits(f.value.as_fixed64().unwrap_or(0)),
                3 => res.z = f64::from_bits(f.value.as_fixed64().unwrap_or(0)),
                _ => {}
            }
        }
        Ok(res)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct MathVector4 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub w: f64,
}

impl QuasarMessage for MathVector4 {
    const FULL_NAME: &'static str = "eve_public.math.Vector4";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.x.to_bits() != 0 {
            encode_fixed64_field(1, self.x.to_bits(), &mut out);
        }
        if self.y.to_bits() != 0 {
            encode_fixed64_field(2, self.y.to_bits(), &mut out);
        }
        if self.z.to_bits() != 0 {
            encode_fixed64_field(3, self.z.to_bits(), &mut out);
        }
        if self.w.to_bits() != 0 {
            encode_fixed64_field(4, self.w.to_bits(), &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => res.x = f64::from_bits(f.value.as_fixed64().unwrap_or(0)),
                2 => res.y = f64::from_bits(f.value.as_fixed64().unwrap_or(0)),
                3 => res.z = f64::from_bits(f.value.as_fixed64().unwrap_or(0)),
                4 => res.w = f64::from_bits(f.value.as_fixed64().unwrap_or(0)),
                _ => {}
            }
        }
        Ok(res)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceUserIdentifier {
    pub device: Vec<u8>,
    pub user: Option<EvePublicUserId>,
}

impl QuasarMessage for DeviceUserIdentifier {
    const FULL_NAME: &'static str = "eve_public.device.user.Identifier";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.device.is_empty() {
            encode_bytes_field(1, &self.device, &mut out);
        }
        if let Some(u) = &self.user {
            encode_bytes_field(2, &u.encode_proto(), &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.device = b.to_vec();
                    }
                }
                2 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.user = Some(EvePublicUserId::decode_proto(b)?);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkRttSample {
    pub protocol: i32,
    pub rtt: Option<GoogleDuration>,
}

impl QuasarMessage for NetworkRttSample {
    const FULL_NAME: &'static str = "eve_public.network.rtt.Sample";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.protocol != 0 {
            encode_varint_field(1, self.protocol as i64 as u64, &mut out);
        }
        if let Some(rtt) = &self.rtt {
            encode_bytes_field(2, &rtt.encode_proto(), &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => res.protocol = f.value.as_varint().unwrap_or(0) as i32,
                2 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.rtt = Some(GoogleDuration::decode_proto(b)?);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

