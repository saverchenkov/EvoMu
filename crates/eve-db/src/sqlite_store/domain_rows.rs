//! Generic row-per-key `domain_rows` persistence with checked JSON serialization (`#916`, RFC-2 §3.5 / §6 P2-4).

use crate::cow::{CowCell, CowDiffItem, CowInlineDiffItem, CowMap, CowMapInline};
use crate::tx_engine::TxError;
use rusqlite::{params, Connection, Transaction};
use serde::de::{
    self, DeserializeOwned, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess,
    Visitor,
};
use serde::ser::{
    self, Serialize, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant,
    SerializeTuple, SerializeTupleStruct, SerializeTupleVariant, Serializer,
};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::Write;

/// Family 1 (`ledger`) canonical `domain_rows.domain` names.
pub const DOMAIN_WALLET_JOURNAL_META: &str = "ledger.wallet_journal_meta";
pub const DOMAIN_MARKET_TRANSACTIONS: &str = "ledger.market_transactions";
pub const DOMAIN_ACCOUNT_PLEX_VAULT: &str = "ledger.account_plex_vault";

/// Family 2 (`notes_lp`) canonical `domain_rows.domain` names.
pub const DOMAIN_OWNER_NOTES: &str = "notes_lp.owner_notes";
pub const DOMAIN_CORP_LP_BALANCES: &str = "notes_lp.corp_lp_balances";
pub const DOMAIN_CHAR_LP_LOGS: &str = "notes_lp.char_lp_logs";

/// Family 3 (`social`) canonical `domain_rows.domain` names (`#916`).
pub const DOMAIN_SOCIAL_MAIL: &str = "social.mail";
pub const DOMAIN_SOCIAL_BOOKMARKS: &str = "social.bookmarks";
pub const DOMAIN_SOCIAL_CHAT_CALENDAR: &str = "social.chat_calendar";
pub const DOMAIN_SOCIAL_CONTACTS_NOTES: &str = "social.contacts_notes";

/// Family 4 (`corp_crimewatch`) canonical `domain_rows.domain` names (`#916`).
pub const DOMAIN_CORP_GOVERNANCE: &str = "corp_crimewatch.corp_governance";
pub const DOMAIN_CRIMEWATCH: &str = "corp_crimewatch.crimewatch";
pub const DOMAIN_WARFARE: &str = "corp_crimewatch.warfare";

/// Family 6 (`service_domains`) canonical `domain_rows.domain` names (`#916`).
pub const DOMAIN_PVE_SPECIAL_SITES: &str = "service_domains.pve_special_sites";
pub const DOMAIN_CRAB_BEACONS: &str = "service_domains.crab_beacons";
pub const DOMAIN_ABYSSAL_STATE: &str = "service_domains.abyssal_state";
pub const DOMAIN_DUNGEON_RUNTIME: &str = "service_domains.dungeon_runtime";
pub const DOMAIN_DUNGEON_EDITOR: &str = "service_domains.dungeon_editor";
pub const DOMAIN_FOB_OBSERVATORY_STATE: &str = "service_domains.fob_observatory_state";
pub const DOMAIN_MINING_AND_ESS_STATE: &str = "service_domains.mining_and_ess_state";
pub const DOMAIN_PVP_FILAMENT_STATE: &str = "service_domains.pvp_filament_state";
pub const DOMAIN_TOURNEY_STATE: &str = "service_domains.tourney_state";
pub const DOMAIN_DEPLOYABLE_STATE: &str = "service_domains.deployable_state";
pub const DOMAIN_SUPERWEAPON_RUNTIME: &str = "service_domains.superweapon_runtime";
pub const DOMAIN_INSURGENCY_COMPLEX_OPS: &str = "service_domains.insurgency_complex_ops";
pub const DOMAIN_NODE_GRAPH_STATE: &str = "service_domains.node_graph_state";
pub const DOMAIN_SEASONS_AND_CAMPAIGNS: &str = "service_domains.seasons_and_campaigns";
pub const DOMAIN_QUASAR_COMMERCE: &str = "service_domains.quasar_commerce";
pub const DOMAIN_FLEET_AND_SOCIAL_OPS: &str = "service_domains.fleet_and_social_ops";
pub const DOMAIN_CLUSTER_AND_RUNTIME: &str = "service_domains.cluster_and_runtime";

/// Idempotent schema DDL for the generic `domain_rows` key/value table.
pub fn apply_domain_rows_migration(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS domain_rows (
            domain TEXT NOT NULL,
            key TEXT NOT NULL,
            value TEXT NOT NULL,
            PRIMARY KEY (domain, key)
        );",
    )
    .map_err(|e| format!("domain_rows migration error: {e}"))
}

// ============================================================================
// Canonical Key Codec
// ============================================================================

/// Bijective, collision-free string encoding for map/tuple keys persisted in `domain_rows.key`.
pub trait KeyCodec: Sized + Clone {
    fn encode_key(&self) -> String;
    fn decode_key(raw: &str) -> Result<Self, String>;
}

/// Helper trait for unambiguous encoding of tuple components separated by `|`.
pub trait TupleKeyPart: Sized {
    fn push_part(&self, out: &mut String);
    fn take_part(input: &str) -> Result<(Self, &str), String>;
}

macro_rules! impl_int_key_codec {
    ($($ty:ty),* $(,)?) => {
        $(
            impl KeyCodec for $ty {
                #[inline]
                fn encode_key(&self) -> String {
                    self.to_string()
                }

                #[inline]
                fn decode_key(raw: &str) -> Result<Self, String> {
                    raw.parse::<$ty>()
                        .map_err(|e| format!("invalid {} key '{raw}': {e}", stringify!($ty)))
                }
            }

            impl TupleKeyPart for $ty {
                #[inline]
                fn push_part(&self, out: &mut String) {
                    use std::fmt::Write as _;
                    let _ = write!(out, "{self}");
                }

                fn take_part(input: &str) -> Result<(Self, &str), String> {
                    let (head, rest) = match input.split_once('|') {
                        Some((h, r)) => (h, r),
                        None => (input, ""),
                    };
                    let parsed = head
                        .parse::<$ty>()
                        .map_err(|e| format!("invalid {} key part '{head}': {e}", stringify!($ty)))?;
                    Ok((parsed, rest))
                }
            }
        )*
    };
}

impl_int_key_codec!(u8, i32, u32, i64, u64, i128, u128, usize);

impl KeyCodec for () {
    #[inline]
    fn encode_key(&self) -> String {
        String::new()
    }

    #[inline]
    fn decode_key(raw: &str) -> Result<Self, String> {
        if raw.is_empty() {
            Ok(())
        } else {
            Err(format!("invalid unit key '{raw}'"))
        }
    }
}

impl TupleKeyPart for () {
    #[inline]
    fn push_part(&self, out: &mut String) {
        out.push('_');
    }

    fn take_part(input: &str) -> Result<(Self, &str), String> {
        let (head, rest) = match input.split_once('|') {
            Some((h, r)) => (h, r),
            None => (input, ""),
        };
        if head == "_" {
            Ok(((), rest))
        } else {
            Err(format!("invalid unit key part '{head}'"))
        }
    }
}

impl KeyCodec for String {
    #[inline]
    fn encode_key(&self) -> String {
        self.clone()
    }

    #[inline]
    fn decode_key(raw: &str) -> Result<Self, String> {
        Ok(raw.to_string())
    }
}

impl TupleKeyPart for String {
    fn push_part(&self, out: &mut String) {
        let encoded = serde_json::to_string(self).expect("String JSON encoding is infallible");
        out.push_str(&encoded);
    }

    fn take_part(input: &str) -> Result<(Self, &str), String> {
        if !input.starts_with('"') {
            return Err(format!("expected quoted string key part in '{input}'"));
        }
        let bytes = input.as_bytes();
        let mut i = 1;
        let mut escaped = false;
        while i < bytes.len() {
            let b = bytes[i];
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                let slice = &input[..=i];
                let decoded = serde_json::from_str::<String>(slice)
                    .map_err(|e| format!("invalid string key part {slice}: {e}"))?;
                let rest = &input[i + 1..];
                let rest = if let Some(after_sep) = rest.strip_prefix('|') {
                    after_sep
                } else if rest.is_empty() {
                    ""
                } else {
                    return Err(format!("unexpected trailing bytes in key part '{input}'"));
                };
                return Ok((decoded, rest));
            }
            i += 1;
        }
        Err(format!("unterminated string key part in '{input}'"))
    }
}

impl<A: TupleKeyPart + Clone, B: TupleKeyPart + Clone> KeyCodec for (A, B) {
    fn encode_key(&self) -> String {
        let mut out = String::new();
        self.0.push_part(&mut out);
        out.push('|');
        self.1.push_part(&mut out);
        out
    }

    fn decode_key(raw: &str) -> Result<Self, String> {
        let (a, rest) = A::take_part(raw)?;
        if rest.is_empty() && !raw.ends_with('|') {
            return Err(format!("missing second component in composite key '{raw}'"));
        }
        let (b, tail) = B::take_part(rest)?;
        if !tail.is_empty() {
            return Err(format!("unexpected trailing component '{tail}' in key '{raw}'"));
        }
        Ok((a, b))
    }
}

impl<A: TupleKeyPart + Clone, B: TupleKeyPart + Clone, C: TupleKeyPart + Clone> KeyCodec
    for (A, B, C)
{
    fn encode_key(&self) -> String {
        let mut out = String::new();
        self.0.push_part(&mut out);
        out.push('|');
        self.1.push_part(&mut out);
        out.push('|');
        self.2.push_part(&mut out);
        out
    }

    fn decode_key(raw: &str) -> Result<Self, String> {
        let (a, rest1) = A::take_part(raw)?;
        let (b, rest2) = B::take_part(rest1)?;
        let (c, tail) = C::take_part(rest2)?;
        if !tail.is_empty() {
            return Err(format!("unexpected trailing component '{tail}' in key '{raw}'"));
        }
        Ok((a, b, c))
    }
}

// ============================================================================
// Checked JSON Serializer & Deserializer
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckedSerError {
    NonFiniteFloat,
    Message(String),
}

impl fmt::Display for CheckedSerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteFloat => f.write_str("non-finite floating-point value (NaN/Infinity)"),
            Self::Message(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for CheckedSerError {}

impl ser::Error for CheckedSerError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Self::Message(msg.to_string())
    }
}

fn json_ser_err(err: serde_json::Error) -> CheckedSerError {
    CheckedSerError::Message(err.to_string())
}

fn io_ser_err(err: std::io::Error) -> CheckedSerError {
    CheckedSerError::Message(err.to_string())
}

/// Serializes any `Serialize` value to compact JSON while:
/// - rejecting any non-finite `f32`/`f64` (`NaN` / `+-Infinity`), and
/// - encoding `u128`/`i128` values and map keys as quoted decimal strings.
pub fn to_checked_domain_json<T: Serialize + ?Sized>(
    domain: &'static str,
    key: &str,
    value: &T,
) -> Result<String, TxError> {
    let mut ser = CheckedJsonSerializer {
        out: Vec::with_capacity(128),
    };
    match value.serialize(&mut ser) {
        Ok(()) => String::from_utf8(ser.out).map_err(|e| {
            TxError::Persist(format!(
                "non-UTF-8 JSON in domain '{domain}' key '{key}': {e}"
            ))
        }),
        Err(CheckedSerError::NonFiniteFloat) => Err(TxError::NonFinite {
            table: domain,
            key: key.to_string(),
        }),
        Err(CheckedSerError::Message(msg)) => Err(TxError::Persist(format!(
            "checked serialize error in domain '{domain}' key '{key}': {msg}"
        ))),
    }
}

/// Deserializes compact JSON written by [`to_checked_domain_json`] (or legacy `serde_json`),
/// accepting both quoted decimal strings and JSON integers for `u128`/`i128`.
pub fn from_checked_domain_json<T: DeserializeOwned>(json: &str) -> Result<T, String> {
    let mut inner = serde_json::Deserializer::from_str(json);
    let val = T::deserialize(CheckedDeserializer(&mut inner)).map_err(|e| e.to_string())?;
    inner.end().map_err(|e| e.to_string())?;
    Ok(val)
}

struct CheckedJsonSerializer {
    out: Vec<u8>,
}

impl CheckedJsonSerializer {
    #[inline]
    fn write_comma_if_needed(&mut self, first: &mut bool) {
        if *first {
            *first = false;
        } else {
            self.out.push(b',');
        }
    }

    #[inline]
    fn write_json_str(&mut self, s: &str) -> Result<(), CheckedSerError> {
        serde_json::to_writer(&mut self.out, s).map_err(json_ser_err)
    }
}

impl<'a> Serializer for &'a mut CheckedJsonSerializer {
    type Ok = ();
    type Error = CheckedSerError;

    type SerializeSeq = CheckedSeqSerializer<'a>;
    type SerializeTuple = CheckedSeqSerializer<'a>;
    type SerializeTupleStruct = CheckedSeqSerializer<'a>;
    type SerializeTupleVariant = CheckedTupleVariantSerializer<'a>;
    type SerializeMap = CheckedMapSerializer<'a>;
    type SerializeStruct = CheckedStructSerializer<'a>;
    type SerializeStructVariant = CheckedStructVariantSerializer<'a>;

    fn serialize_bool(self, v: bool) -> Result<(), Self::Error> {
        self.out
            .extend_from_slice(if v { b"true" } else { b"false" });
        Ok(())
    }

    fn serialize_i8(self, v: i8) -> Result<(), Self::Error> {
        write!(self.out, "{v}").map_err(io_ser_err)
    }

    fn serialize_i16(self, v: i16) -> Result<(), Self::Error> {
        write!(self.out, "{v}").map_err(io_ser_err)
    }

    fn serialize_i32(self, v: i32) -> Result<(), Self::Error> {
        write!(self.out, "{v}").map_err(io_ser_err)
    }

    fn serialize_i64(self, v: i64) -> Result<(), Self::Error> {
        write!(self.out, "{v}").map_err(io_ser_err)
    }

    fn serialize_i128(self, v: i128) -> Result<(), Self::Error> {
        write!(self.out, "\"{v}\"").map_err(io_ser_err)
    }

    fn serialize_u8(self, v: u8) -> Result<(), Self::Error> {
        write!(self.out, "{v}").map_err(io_ser_err)
    }

    fn serialize_u16(self, v: u16) -> Result<(), Self::Error> {
        write!(self.out, "{v}").map_err(io_ser_err)
    }

    fn serialize_u32(self, v: u32) -> Result<(), Self::Error> {
        write!(self.out, "{v}").map_err(io_ser_err)
    }

    fn serialize_u64(self, v: u64) -> Result<(), Self::Error> {
        write!(self.out, "{v}").map_err(io_ser_err)
    }

    fn serialize_u128(self, v: u128) -> Result<(), Self::Error> {
        write!(self.out, "\"{v}\"").map_err(io_ser_err)
    }

    fn serialize_f32(self, v: f32) -> Result<(), Self::Error> {
        if !v.is_finite() {
            return Err(CheckedSerError::NonFiniteFloat);
        }
        serde_json::to_writer(&mut self.out, &v).map_err(json_ser_err)
    }

    fn serialize_f64(self, v: f64) -> Result<(), Self::Error> {
        if !v.is_finite() {
            return Err(CheckedSerError::NonFiniteFloat);
        }
        serde_json::to_writer(&mut self.out, &v).map_err(json_ser_err)
    }

    fn serialize_char(self, v: char) -> Result<(), Self::Error> {
        serde_json::to_writer(&mut self.out, &v).map_err(json_ser_err)
    }

    fn serialize_str(self, v: &str) -> Result<(), Self::Error> {
        self.write_json_str(v)
    }

    fn serialize_bytes(self, v: &[u8]) -> Result<(), Self::Error> {
        let mut seq = self.serialize_seq(Some(v.len()))?;
        for b in v {
            SerializeSeq::serialize_element(&mut seq, b)?;
        }
        SerializeSeq::end(seq)
    }

    fn serialize_none(self) -> Result<(), Self::Error> {
        self.out.extend_from_slice(b"null");
        Ok(())
    }

    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<(), Self::Error> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<(), Self::Error> {
        self.out.extend_from_slice(b"null");
        Ok(())
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), Self::Error> {
        self.serialize_unit()
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<(), Self::Error> {
        self.write_json_str(variant)
    }

    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.out.push(b'{');
        self.write_json_str(variant)?;
        self.out.push(b':');
        value.serialize(&mut *self)?;
        self.out.push(b'}');
        Ok(())
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        self.out.push(b'[');
        Ok(CheckedSeqSerializer {
            ser: self,
            first: true,
        })
    }

    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        self.out.push(b'{');
        self.write_json_str(variant)?;
        self.out.extend_from_slice(b":[");
        Ok(CheckedTupleVariantSerializer {
            ser: self,
            first: true,
        })
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        self.out.push(b'{');
        Ok(CheckedMapSerializer {
            ser: self,
            first: true,
        })
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        self.out.push(b'{');
        Ok(CheckedStructSerializer {
            ser: self,
            first: true,
        })
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        self.out.push(b'{');
        self.write_json_str(variant)?;
        self.out.extend_from_slice(b":{");
        Ok(CheckedStructVariantSerializer {
            ser: self,
            first: true,
        })
    }
}

struct CheckedSeqSerializer<'a> {
    ser: &'a mut CheckedJsonSerializer,
    first: bool,
}

impl SerializeSeq for CheckedSeqSerializer<'_> {
    type Ok = ();
    type Error = CheckedSerError;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.ser.write_comma_if_needed(&mut self.first);
        value.serialize(&mut *self.ser)
    }

    fn end(self) -> Result<(), Self::Error> {
        self.ser.out.push(b']');
        Ok(())
    }
}

impl SerializeTuple for CheckedSeqSerializer<'_> {
    type Ok = ();
    type Error = CheckedSerError;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<(), Self::Error> {
        SerializeSeq::end(self)
    }
}

impl SerializeTupleStruct for CheckedSeqSerializer<'_> {
    type Ok = ();
    type Error = CheckedSerError;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<(), Self::Error> {
        SerializeSeq::end(self)
    }
}

struct CheckedTupleVariantSerializer<'a> {
    ser: &'a mut CheckedJsonSerializer,
    first: bool,
}

impl SerializeTupleVariant for CheckedTupleVariantSerializer<'_> {
    type Ok = ();
    type Error = CheckedSerError;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.ser.write_comma_if_needed(&mut self.first);
        value.serialize(&mut *self.ser)
    }

    fn end(self) -> Result<(), Self::Error> {
        self.ser.out.extend_from_slice(b"]}");
        Ok(())
    }
}

struct CheckedMapSerializer<'a> {
    ser: &'a mut CheckedJsonSerializer,
    first: bool,
}

impl SerializeMap for CheckedMapSerializer<'_> {
    type Ok = ();
    type Error = CheckedSerError;

    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<(), Self::Error> {
        self.ser.write_comma_if_needed(&mut self.first);
        key.serialize(JsonMapKeySerializer(&mut self.ser.out))
    }

    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.ser.out.push(b':');
        value.serialize(&mut *self.ser)
    }

    fn end(self) -> Result<(), Self::Error> {
        self.ser.out.push(b'}');
        Ok(())
    }
}

struct CheckedStructSerializer<'a> {
    ser: &'a mut CheckedJsonSerializer,
    first: bool,
}

impl SerializeStruct for CheckedStructSerializer<'_> {
    type Ok = ();
    type Error = CheckedSerError;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.ser.write_comma_if_needed(&mut self.first);
        self.ser.write_json_str(key)?;
        self.ser.out.push(b':');
        value.serialize(&mut *self.ser)
    }

    fn end(self) -> Result<(), Self::Error> {
        self.ser.out.push(b'}');
        Ok(())
    }
}

struct CheckedStructVariantSerializer<'a> {
    ser: &'a mut CheckedJsonSerializer,
    first: bool,
}

impl SerializeStructVariant for CheckedStructVariantSerializer<'_> {
    type Ok = ();
    type Error = CheckedSerError;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.ser.write_comma_if_needed(&mut self.first);
        self.ser.write_json_str(key)?;
        self.ser.out.push(b':');
        value.serialize(&mut *self.ser)
    }

    fn end(self) -> Result<(), Self::Error> {
        self.ser.out.extend_from_slice(b"}}");
        Ok(())
    }
}

struct JsonMapKeySerializer<'a>(&'a mut Vec<u8>);

impl Serializer for JsonMapKeySerializer<'_> {
    type Ok = ();
    type Error = CheckedSerError;

    type SerializeSeq = ser::Impossible<(), CheckedSerError>;
    type SerializeTuple = ser::Impossible<(), CheckedSerError>;
    type SerializeTupleStruct = ser::Impossible<(), CheckedSerError>;
    type SerializeTupleVariant = ser::Impossible<(), CheckedSerError>;
    type SerializeMap = ser::Impossible<(), CheckedSerError>;
    type SerializeStruct = ser::Impossible<(), CheckedSerError>;
    type SerializeStructVariant = ser::Impossible<(), CheckedSerError>;

    fn serialize_bool(self, v: bool) -> Result<(), Self::Error> {
        self.0
            .extend_from_slice(if v { b"\"true\"" } else { b"\"false\"" });
        Ok(())
    }

    fn serialize_i8(self, v: i8) -> Result<(), Self::Error> {
        write!(self.0, "\"{v}\"").map_err(io_ser_err)
    }

    fn serialize_i16(self, v: i16) -> Result<(), Self::Error> {
        write!(self.0, "\"{v}\"").map_err(io_ser_err)
    }

    fn serialize_i32(self, v: i32) -> Result<(), Self::Error> {
        write!(self.0, "\"{v}\"").map_err(io_ser_err)
    }

    fn serialize_i64(self, v: i64) -> Result<(), Self::Error> {
        write!(self.0, "\"{v}\"").map_err(io_ser_err)
    }

    fn serialize_i128(self, v: i128) -> Result<(), Self::Error> {
        write!(self.0, "\"{v}\"").map_err(io_ser_err)
    }

    fn serialize_u8(self, v: u8) -> Result<(), Self::Error> {
        write!(self.0, "\"{v}\"").map_err(io_ser_err)
    }

    fn serialize_u16(self, v: u16) -> Result<(), Self::Error> {
        write!(self.0, "\"{v}\"").map_err(io_ser_err)
    }

    fn serialize_u32(self, v: u32) -> Result<(), Self::Error> {
        write!(self.0, "\"{v}\"").map_err(io_ser_err)
    }

    fn serialize_u64(self, v: u64) -> Result<(), Self::Error> {
        write!(self.0, "\"{v}\"").map_err(io_ser_err)
    }

    fn serialize_u128(self, v: u128) -> Result<(), Self::Error> {
        write!(self.0, "\"{v}\"").map_err(io_ser_err)
    }

    fn serialize_f32(self, v: f32) -> Result<(), Self::Error> {
        if !v.is_finite() {
            return Err(CheckedSerError::NonFiniteFloat);
        }
        Err(CheckedSerError::Message("key must be a string or integer".into()))
    }

    fn serialize_f64(self, v: f64) -> Result<(), Self::Error> {
        if !v.is_finite() {
            return Err(CheckedSerError::NonFiniteFloat);
        }
        Err(CheckedSerError::Message("key must be a string or integer".into()))
    }

    fn serialize_char(self, v: char) -> Result<(), Self::Error> {
        serde_json::to_writer(self.0, &v).map_err(json_ser_err)
    }

    fn serialize_str(self, v: &str) -> Result<(), Self::Error> {
        serde_json::to_writer(self.0, v).map_err(json_ser_err)
    }

    fn serialize_bytes(self, _v: &[u8]) -> Result<(), Self::Error> {
        Err(CheckedSerError::Message("key must be a string or integer".into()))
    }

    fn serialize_none(self) -> Result<(), Self::Error> {
        Err(CheckedSerError::Message("key must be a string or integer".into()))
    }

    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<(), Self::Error> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<(), Self::Error> {
        Err(CheckedSerError::Message("key must be a string or integer".into()))
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), Self::Error> {
        Err(CheckedSerError::Message("key must be a string or integer".into()))
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<(), Self::Error> {
        serde_json::to_writer(self.0, variant).map_err(json_ser_err)
    }

    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<(), Self::Error> {
        Err(CheckedSerError::Message("key must be a string or integer".into()))
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        Err(CheckedSerError::Message("key must be a string or integer".into()))
    }

    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        Err(CheckedSerError::Message("key must be a string or integer".into()))
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        Err(CheckedSerError::Message("key must be a string or integer".into()))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        Err(CheckedSerError::Message("key must be a string or integer".into()))
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Err(CheckedSerError::Message("key must be a string or integer".into()))
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        Err(CheckedSerError::Message("key must be a string or integer".into()))
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        Err(CheckedSerError::Message("key must be a string or integer".into()))
    }
}

struct CheckedDeserializer<D>(D);

struct I128StrOrIntVisitor<V>(V);
impl<'de, V: Visitor<'de>> Visitor<'de> for I128StrOrIntVisitor<V> {
    type Value = V::Value;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("i128 as decimal string or integer")
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
        self.0.visit_i128(i128::from(v))
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
        self.0.visit_i128(i128::from(v))
    }
    fn visit_i128<E: de::Error>(self, v: i128) -> Result<Self::Value, E> {
        self.0.visit_i128(v)
    }
    fn visit_u128<E: de::Error>(self, v: u128) -> Result<Self::Value, E> {
        let n = i128::try_from(v).map_err(|_| E::custom("u128 overflows i128"))?;
        self.0.visit_i128(n)
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        let n = v
            .parse::<i128>()
            .map_err(|e| E::custom(format!("invalid i128 string '{v}': {e}")))?;
        self.0.visit_i128(n)
    }
}

struct U128StrOrIntVisitor<V>(V);
impl<'de, V: Visitor<'de>> Visitor<'de> for U128StrOrIntVisitor<V> {
    type Value = V::Value;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("u128 as decimal string or unsigned integer")
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
        let n = u128::try_from(v).map_err(|_| E::custom("negative integer for u128"))?;
        self.0.visit_u128(n)
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
        self.0.visit_u128(u128::from(v))
    }
    fn visit_i128<E: de::Error>(self, v: i128) -> Result<Self::Value, E> {
        let n = u128::try_from(v).map_err(|_| E::custom("negative i128 for u128"))?;
        self.0.visit_u128(n)
    }
    fn visit_u128<E: de::Error>(self, v: u128) -> Result<Self::Value, E> {
        self.0.visit_u128(v)
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        let n = v
            .parse::<u128>()
            .map_err(|e| E::custom(format!("invalid u128 string '{v}': {e}")))?;
        self.0.visit_u128(n)
    }
}

struct CheckedSeed<S>(S);
impl<'de, S: DeserializeSeed<'de>> DeserializeSeed<'de> for CheckedSeed<S> {
    type Value = S::Value;
    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        self.0.deserialize(CheckedDeserializer(deserializer))
    }
}

struct CheckedSeqAccess<A>(A);
impl<'de, A: SeqAccess<'de>> SeqAccess<'de> for CheckedSeqAccess<A> {
    type Error = A::Error;
    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Self::Error> {
        self.0.next_element_seed(CheckedSeed(seed))
    }
    fn size_hint(&self) -> Option<usize> {
        self.0.size_hint()
    }
}

struct CheckedMapAccess<A>(A);
impl<'de, A: MapAccess<'de>> MapAccess<'de> for CheckedMapAccess<A> {
    type Error = A::Error;
    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        self.0.next_key_seed(CheckedSeed(seed))
    }
    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Self::Error> {
        self.0.next_value_seed(CheckedSeed(seed))
    }
    fn next_entry_seed<K: DeserializeSeed<'de>, V: DeserializeSeed<'de>>(
        &mut self,
        kseed: K,
        vseed: V,
    ) -> Result<Option<(K::Value, V::Value)>, Self::Error> {
        self.0
            .next_entry_seed(CheckedSeed(kseed), CheckedSeed(vseed))
    }
    fn size_hint(&self) -> Option<usize> {
        self.0.size_hint()
    }
}

struct CheckedEnumAccess<A>(A);
impl<'de, A: EnumAccess<'de>> EnumAccess<'de> for CheckedEnumAccess<A> {
    type Error = A::Error;
    type Variant = CheckedVariantAccess<A::Variant>;
    fn variant_seed<V: DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, Self::Variant), Self::Error> {
        let (val, var) = self.0.variant_seed(CheckedSeed(seed))?;
        Ok((val, CheckedVariantAccess(var)))
    }
}

struct CheckedVariantAccess<A>(A);
impl<'de, A: VariantAccess<'de>> VariantAccess<'de> for CheckedVariantAccess<A> {
    type Error = A::Error;
    fn unit_variant(self) -> Result<(), Self::Error> {
        self.0.unit_variant()
    }
    fn newtype_variant_seed<T: DeserializeSeed<'de>>(
        self,
        seed: T,
    ) -> Result<T::Value, Self::Error> {
        self.0.newtype_variant_seed(CheckedSeed(seed))
    }
    fn tuple_variant<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0.tuple_variant(len, CheckedVisitor(visitor))
    }
    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0.struct_variant(fields, CheckedVisitor(visitor))
    }
}

struct CheckedVisitor<V>(V);
impl<'de, V: Visitor<'de>> Visitor<'de> for CheckedVisitor<V> {
    type Value = V::Value;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.expecting(f)
    }
    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
        self.0.visit_bool(v)
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
        self.0.visit_i64(v)
    }
    fn visit_i128<E: de::Error>(self, v: i128) -> Result<Self::Value, E> {
        self.0.visit_i128(v)
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
        self.0.visit_u64(v)
    }
    fn visit_u128<E: de::Error>(self, v: u128) -> Result<Self::Value, E> {
        self.0.visit_u128(v)
    }
    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
        self.0.visit_f64(v)
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        self.0.visit_str(v)
    }
    fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
        self.0.visit_borrowed_str(v)
    }
    fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
        self.0.visit_string(v)
    }
    fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
        self.0.visit_bytes(v)
    }
    fn visit_byte_buf<E: de::Error>(self, v: Vec<u8>) -> Result<Self::Value, E> {
        self.0.visit_byte_buf(v)
    }
    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        self.0.visit_none()
    }
    fn visit_some<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        self.0.visit_some(CheckedDeserializer(deserializer))
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        self.0.visit_unit()
    }
    fn visit_newtype_struct<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        self.0
            .visit_newtype_struct(CheckedDeserializer(deserializer))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
        self.0.visit_seq(CheckedSeqAccess(seq))
    }
    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        self.0.visit_map(CheckedMapAccess(map))
    }
    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> Result<Self::Value, A::Error> {
        self.0.visit_enum(CheckedEnumAccess(data))
    }
}

macro_rules! delegate_deserialize {
    ($($method:ident),* $(,)?) => {
        $(
            fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
                self.0.$method(CheckedVisitor(visitor))
            }
        )*
    };
}

impl<'de, D: de::Deserializer<'de>> de::Deserializer<'de> for CheckedDeserializer<D> {
    type Error = D::Error;

    delegate_deserialize!(
        deserialize_any,
        deserialize_bool,
        deserialize_i8,
        deserialize_i16,
        deserialize_i32,
        deserialize_i64,
        deserialize_u8,
        deserialize_u16,
        deserialize_u32,
        deserialize_u64,
        deserialize_f32,
        deserialize_f64,
        deserialize_char,
        deserialize_str,
        deserialize_string,
        deserialize_bytes,
        deserialize_byte_buf,
        deserialize_option,
        deserialize_unit,
        deserialize_seq,
        deserialize_map,
        deserialize_identifier,
        deserialize_ignored_any,
    );

    fn deserialize_i128<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.0.deserialize_any(I128StrOrIntVisitor(visitor))
    }

    fn deserialize_u128<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.0.deserialize_any(U128StrOrIntVisitor(visitor))
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0.deserialize_unit_struct(name, CheckedVisitor(visitor))
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0
            .deserialize_newtype_struct(name, CheckedVisitor(visitor))
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0.deserialize_tuple(len, CheckedVisitor(visitor))
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0
            .deserialize_tuple_struct(name, len, CheckedVisitor(visitor))
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0
            .deserialize_struct(name, fields, CheckedVisitor(visitor))
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0
            .deserialize_enum(name, variants, CheckedVisitor(visitor))
    }
}

// ============================================================================
// Generic Persist Trait + CowMap / CowMapInline / CowCell Impls
// ============================================================================

/// Single row change emitted by [`Persist::collect_diff_ops`] and executed in `domain_rows`.
#[derive(Debug, Clone)]
pub enum DomainRowChange {
    Upsert {
        domain: &'static str,
        key: String,
        value: Result<String, TxError>,
    },
    Delete {
        domain: &'static str,
        key: String,
    },
}

/// Generic persistence contract for Copy-on-Write domains backed by the `domain_rows` table.
pub trait Persist: Sized {
    /// Appends row-level upsert/delete operations representing `self -> after` (`O(k log N)` for maps,
    /// `O(1)` when `ptr_eq`).
    fn collect_diff_ops(
        &self,
        after: &Self,
        domain: &'static str,
        out: &mut Vec<DomainRowChange>,
    );

    /// Synchronizes an entire snapshot field against SQLite `domain_rows` (used by startup sync & tests).
    fn sync_full(&self, tx: &Transaction<'_>, domain: &'static str) -> Result<bool, String>;

    /// Loads all rows for `domain` from `domain_rows` respecting [`super::quarantine::LoadContext`].
    fn load_domain(
        conn: &Connection,
        domain: &'static str,
        ctx: &mut super::quarantine::LoadContext,
    ) -> Result<Self, String>;
}

fn sync_entries_map<'a, V: Serialize + 'a>(
    tx: &Transaction<'_>,
    domain: &'static str,
    entries: impl Iterator<Item = (String, &'a V)>,
) -> Result<bool, String> {
    let mut existing: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = tx
            .prepare("SELECT key, value FROM domain_rows WHERE domain = ?1")
            .map_err(|e| format!("domain_rows sync prepare({domain}): {e}"))?;
        let mut rows = stmt
            .query(params![domain])
            .map_err(|e| format!("domain_rows sync query({domain}): {e}"))?;
        while let Some(row) = rows
            .next()
            .map_err(|e| format!("domain_rows sync step({domain}): {e}"))?
        {
            let k: String = row.get(0).map_err(|e| e.to_string())?;
            let v: String = row.get(1).map_err(|e| e.to_string())?;
            existing.insert(k, v);
        }
    }

    let mut live_keys = HashSet::new();
    let mut dirty = false;
    let mut upsert_stmt = tx
        .prepare_cached(
            "INSERT OR REPLACE INTO domain_rows (domain, key, value) VALUES (?1, ?2, ?3)",
        )
        .map_err(|e| format!("domain_rows sync upsert prepare({domain}): {e}"))?;

    for (key_str, val) in entries {
        let json = to_checked_domain_json(domain, &key_str, val).map_err(|e| e.to_string())?;
        if existing.get(&key_str).map(String::as_str) != Some(json.as_str()) {
            upsert_stmt
                .execute(params![domain, &key_str, &json])
                .map_err(|e| format!("domain_rows sync upsert({domain}, {key_str}): {e}"))?;
            dirty = true;
        }
        live_keys.insert(key_str);
    }

    if existing.len() > live_keys.len() || existing.keys().any(|k| !live_keys.contains(k)) {
        let mut del_stmt = tx
            .prepare_cached("DELETE FROM domain_rows WHERE domain = ?1 AND key = ?2")
            .map_err(|e| format!("domain_rows sync delete prepare({domain}): {e}"))?;
        for old_key in existing.keys() {
            if !live_keys.contains(old_key) {
                del_stmt
                    .execute(params![domain, old_key])
                    .map_err(|e| format!("domain_rows sync delete({domain}, {old_key}): {e}"))?;
                dirty = true;
            }
        }
    }

    Ok(dirty)
}

fn load_domain_pairs<K, V>(
    conn: &Connection,
    domain: &'static str,
    ctx: &mut super::quarantine::LoadContext,
) -> Result<Vec<(K, V)>, String>
where
    K: KeyCodec,
    V: DeserializeOwned,
{
    let has_table: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='domain_rows')",
            [],
            |r| r.get(0),
        )
        .map_err(|e| format!("domain_rows table check({domain}): {e}"))?;
    if !has_table {
        return Ok(Vec::new());
    }

    let mut stmt = conn
        .prepare("SELECT rowid, key, value FROM domain_rows WHERE domain = ?1 ORDER BY key ASC")
        .map_err(|e| format!("domain_rows load prepare({domain}): {e}"))?;
    let mut rows = stmt
        .query(params![domain])
        .map_err(|e| format!("domain_rows load query({domain}): {e}"))?;
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|e| format!("domain_rows load step({domain}): {e}"))?
    {
        let rowid: i64 = row.get(0).map_err(|e| e.to_string())?;
        let raw_key: String = row.get(1).map_err(|e| e.to_string())?;
        let raw_val: String = row.get(2).map_err(|e| e.to_string())?;
        let parsed_key = match K::decode_key(&raw_key) {
            Ok(k) => k,
            Err(err) => {
                ctx.record_bad_row("domain_rows", domain, raw_key, raw_val, err, Some(rowid))?;
                continue;
            }
        };
        match from_checked_domain_json::<V>(&raw_val) {
            Ok(val) => out.push((parsed_key, val)),
            Err(err) => {
                ctx.record_bad_row("domain_rows", domain, raw_key, raw_val, err, Some(rowid))?;
            }
        }
    }
    Ok(out)
}

impl<K, V> Persist for CowMap<K, V>
where
    K: Ord + Clone + KeyCodec,
    V: Clone + PartialEq + Serialize + DeserializeOwned,
{
    fn collect_diff_ops(
        &self,
        after: &Self,
        domain: &'static str,
        out: &mut Vec<DomainRowChange>,
    ) {
        if self.ptr_eq(after) {
            return;
        }
        for item in self.diff(after) {
            match item {
                CowDiffItem::Add(k, v) | CowDiffItem::Update { new: (k, v), .. } => {
                    let key_str = k.encode_key();
                    let value = to_checked_domain_json(domain, &key_str, &**v);
                    out.push(DomainRowChange::Upsert {
                        domain,
                        key: key_str,
                        value,
                    });
                }
                CowDiffItem::Remove(k, _) => {
                    out.push(DomainRowChange::Delete {
                        domain,
                        key: k.encode_key(),
                    });
                }
            }
        }
    }

    fn sync_full(&self, tx: &Transaction<'_>, domain: &'static str) -> Result<bool, String> {
        sync_entries_map(tx, domain, self.iter().map(|(k, v)| (k.encode_key(), v)))
    }

    fn load_domain(
        conn: &Connection,
        domain: &'static str,
        ctx: &mut super::quarantine::LoadContext,
    ) -> Result<Self, String> {
        let pairs = load_domain_pairs::<K, V>(conn, domain, ctx)?;
        Ok(pairs.into_iter().collect())
    }
}

impl<K, V> Persist for CowMapInline<K, V>
where
    K: Ord + Clone + KeyCodec,
    V: Clone + PartialEq + Serialize + DeserializeOwned,
{
    fn collect_diff_ops(
        &self,
        after: &Self,
        domain: &'static str,
        out: &mut Vec<DomainRowChange>,
    ) {
        if self.ptr_eq(after) {
            return;
        }
        for item in self.diff(after) {
            match item {
                CowInlineDiffItem::Add(k, v) | CowInlineDiffItem::Update { new: (k, v), .. } => {
                    let key_str = k.encode_key();
                    let value = to_checked_domain_json(domain, &key_str, v);
                    out.push(DomainRowChange::Upsert {
                        domain,
                        key: key_str,
                        value,
                    });
                }
                CowInlineDiffItem::Remove(k, _) => {
                    out.push(DomainRowChange::Delete {
                        domain,
                        key: k.encode_key(),
                    });
                }
            }
        }
    }

    fn sync_full(&self, tx: &Transaction<'_>, domain: &'static str) -> Result<bool, String> {
        sync_entries_map(tx, domain, self.iter().map(|(k, v)| (k.encode_key(), v)))
    }

    fn load_domain(
        conn: &Connection,
        domain: &'static str,
        ctx: &mut super::quarantine::LoadContext,
    ) -> Result<Self, String> {
        let pairs = load_domain_pairs::<K, V>(conn, domain, ctx)?;
        Ok(pairs.into_iter().collect())
    }
}

impl<T> Persist for CowCell<T>
where
    T: Default + Clone + PartialEq + Serialize + DeserializeOwned,
{
    fn collect_diff_ops(
        &self,
        after: &Self,
        domain: &'static str,
        out: &mut Vec<DomainRowChange>,
    ) {
        if self.ptr_eq(after) {
            return;
        }
        if let Some((_, new_val)) = self.diff(after) {
            let key_str = ().encode_key();
            let value = to_checked_domain_json(domain, &key_str, new_val);
            out.push(DomainRowChange::Upsert {
                domain,
                key: key_str,
                value,
            });
        }
    }

    fn sync_full(&self, tx: &Transaction<'_>, domain: &'static str) -> Result<bool, String> {
        sync_entries_map(tx, domain, std::iter::once((().encode_key(), self.get())))
    }

    fn load_domain(
        conn: &Connection,
        domain: &'static str,
        ctx: &mut super::quarantine::LoadContext,
    ) -> Result<Self, String> {
        let mut pairs = load_domain_pairs::<(), T>(conn, domain, ctx)?;
        Ok(pairs
            .pop()
            .map(|(_, val)| CowCell::new(val))
            .unwrap_or_default())
    }
}

// ============================================================================
// Shared `domain_rows` Helpers for Decomposed Domain Stores (`#916`)
// ============================================================================

#[inline]
pub(crate) fn format_domain_field_key(prefix: &str, field: &str) -> String {
    if prefix.is_empty() {
        field.to_owned()
    } else {
        format!("{prefix}{field}")
    }
}

#[inline]
pub(crate) fn format_domain_entry_key<K: KeyCodec>(prefix: &str, field: &str, key: &K) -> String {
    if prefix.is_empty() {
        format!("{field}/{}", key.encode_key())
    } else {
        format!("{prefix}{field}/{}", key.encode_key())
    }
}

pub(crate) fn diff_scalar<T: PartialEq + Serialize>(
    domain: &'static str,
    field: &str,
    before: &T,
    after: &T,
    def: &T,
    out: &mut Vec<DomainRowChange>,
) {
    if before == after {
        return;
    }
    if after == def {
        out.push(DomainRowChange::Delete {
            domain,
            key: field.to_owned(),
        });
    } else {
        out.push(DomainRowChange::Upsert {
            domain,
            key: field.to_owned(),
            value: to_checked_domain_json(domain, field, after),
        });
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn diff_map_with_mapped<'a, K: KeyCodec + 'a, V: PartialEq + 'a, S: Serialize>(
    domain: &'static str,
    prefix: &str,
    field: &'static str,
    before_len: usize,
    after_iter: impl Iterator<Item = (&'a K, &'a V)>,
    before_get: impl Fn(&K) -> Option<&'a V>,
    before_keys: impl Iterator<Item = &'a K>,
    after_contains: impl Fn(&K) -> bool,
    mut to_serializable: impl FnMut(&'a V) -> S,
    out: &mut Vec<DomainRowChange>,
) {
    let mut kept = 0usize;
    for (k, v) in after_iter {
        let changed = match before_get(k) {
            Some(prev) => {
                kept += 1;
                prev != v
            }
            None => true,
        };
        if changed {
            let full_key = format_domain_entry_key(prefix, field, k);
            let serializable = to_serializable(v);
            let value = to_checked_domain_json(domain, &full_key, &serializable);
            out.push(DomainRowChange::Upsert {
                domain,
                key: full_key,
                value,
            });
        }
    }
    if kept < before_len {
        for k in before_keys {
            if !after_contains(k) {
                out.push(DomainRowChange::Delete {
                    domain,
                    key: format_domain_entry_key(prefix, field, k),
                });
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn diff_map_with<'a, K: KeyCodec + 'a, V: PartialEq + Serialize + 'a>(
    domain: &'static str,
    field: &'static str,
    before_len: usize,
    after_iter: impl Iterator<Item = (&'a K, &'a V)>,
    before_get: impl Fn(&K) -> Option<&'a V>,
    before_keys: impl Iterator<Item = &'a K>,
    after_contains: impl Fn(&K) -> bool,
    out: &mut Vec<DomainRowChange>,
) {
    diff_map_with_mapped(
        domain,
        "",
        field,
        before_len,
        after_iter,
        before_get,
        before_keys,
        after_contains,
        |v| v,
        out,
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn diff_set_with_prefix<'a, K: KeyCodec + 'a>(
    domain: &'static str,
    prefix: &str,
    field: &'static str,
    before_len: usize,
    after_iter: impl Iterator<Item = &'a K>,
    before_contains: impl Fn(&K) -> bool,
    before_iter: impl Iterator<Item = &'a K>,
    after_contains: impl Fn(&K) -> bool,
    out: &mut Vec<DomainRowChange>,
) {
    let mut kept = 0usize;
    for k in after_iter {
        if before_contains(k) {
            kept += 1;
        } else {
            out.push(DomainRowChange::Upsert {
                domain,
                key: format_domain_entry_key(prefix, field, k),
                value: Ok("true".to_owned()),
            });
        }
    }
    if kept < before_len {
        for k in before_iter {
            if !after_contains(k) {
                out.push(DomainRowChange::Delete {
                    domain,
                    key: format_domain_entry_key(prefix, field, k),
                });
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn diff_set_with<'a, K: KeyCodec + 'a>(
    domain: &'static str,
    field: &'static str,
    before_len: usize,
    after_iter: impl Iterator<Item = &'a K>,
    before_contains: impl Fn(&K) -> bool,
    before_iter: impl Iterator<Item = &'a K>,
    after_contains: impl Fn(&K) -> bool,
    out: &mut Vec<DomainRowChange>,
) {
    diff_set_with_prefix(
        domain,
        "",
        field,
        before_len,
        after_iter,
        before_contains,
        before_iter,
        after_contains,
        out,
    );
}

fn should_upsert<V: PartialEq + DeserializeOwned>(
    existing: &HashMap<String, String>,
    key: &str,
    new_val: &V,
    new_json: &str,
) -> bool {
    match existing.get(key) {
        None => true,
        Some(old_json) if old_json == new_json => false,
        Some(old_json) => match from_checked_domain_json::<V>(old_json) {
            Ok(old_val) => old_val != *new_val,
            Err(_) => true,
        },
    }
}

pub(crate) struct DomainSyncState {
    domain: &'static str,
    existing: HashMap<String, String>,
    live_keys: HashSet<String>,
    upserts: Vec<(String, String)>,
}

impl DomainSyncState {
    pub(crate) fn load(tx: &Transaction<'_>, domain: &'static str) -> Result<Self, String> {
        let mut existing = HashMap::new();
        {
            let mut stmt = tx
                .prepare("SELECT key, value FROM domain_rows WHERE domain = ?1")
                .map_err(|e| format!("domain_rows sync prepare({domain}): {e}"))?;
            let mut rows = stmt
                .query(params![domain])
                .map_err(|e| format!("domain_rows sync query({domain}): {e}"))?;
            while let Some(row) = rows
                .next()
                .map_err(|e| format!("domain_rows sync step({domain}): {e}"))?
            {
                let k: String = row.get(0).map_err(|e| e.to_string())?;
                let v: String = row.get(1).map_err(|e| e.to_string())?;
                existing.insert(k, v);
            }
        }
        Ok(Self {
            domain,
            existing,
            live_keys: HashSet::new(),
            upserts: Vec::new(),
        })
    }

    pub(crate) fn push_scalar<T: PartialEq + Serialize + DeserializeOwned>(
        &mut self,
        field: &str,
        val: &T,
        def: &T,
    ) -> Result<(), String> {
        if val != def {
            let json =
                to_checked_domain_json(self.domain, field, val).map_err(|e| e.to_string())?;
            if should_upsert(&self.existing, field, val, &json) {
                self.upserts.push((field.to_owned(), json));
            }
            self.live_keys.insert(field.to_owned());
        }
        Ok(())
    }

    pub(crate) fn push_map_mapped<
        'a,
        K: KeyCodec + 'a,
        V: PartialEq + DeserializeOwned + 'a,
        S: Serialize,
    >(
        &mut self,
        prefix: &str,
        field: &'static str,
        entries: impl Iterator<Item = (&'a K, &'a V)>,
        mut to_serializable: impl FnMut(&'a V) -> S,
    ) -> Result<(), String> {
        for (k, v) in entries {
            let full_key = format_domain_entry_key(prefix, field, k);
            let serializable = to_serializable(v);
            let json = to_checked_domain_json(self.domain, &full_key, &serializable)
                .map_err(|e| e.to_string())?;
            if should_upsert(&self.existing, &full_key, v, &json) {
                self.upserts.push((full_key.clone(), json));
            }
            self.live_keys.insert(full_key);
        }
        Ok(())
    }

    pub(crate) fn push_map<'a, K: KeyCodec + 'a, V: PartialEq + Serialize + DeserializeOwned + 'a>(
        &mut self,
        field: &'static str,
        entries: impl Iterator<Item = (&'a K, &'a V)>,
    ) -> Result<(), String> {
        self.push_map_mapped("", field, entries, |v| v)
    }

    pub(crate) fn push_set_with_prefix<'a, K: KeyCodec + 'a>(
        &mut self,
        prefix: &str,
        field: &'static str,
        entries: impl Iterator<Item = &'a K>,
    ) -> Result<(), String> {
        for k in entries {
            let full_key = format_domain_entry_key(prefix, field, k);
            if should_upsert(&self.existing, &full_key, &true, "true") {
                self.upserts.push((full_key.clone(), "true".to_owned()));
            }
            self.live_keys.insert(full_key);
        }
        Ok(())
    }

    pub(crate) fn push_set<'a, K: KeyCodec + 'a>(
        &mut self,
        field: &'static str,
        entries: impl Iterator<Item = &'a K>,
    ) -> Result<(), String> {
        self.push_set_with_prefix("", field, entries)
    }

    pub(crate) fn finish(self, tx: &Transaction<'_>) -> Result<bool, String> {
        let has_deletes = self.existing.len() > self.live_keys.len()
            || self.existing.keys().any(|k| !self.live_keys.contains(k));
        if self.upserts.is_empty() && !has_deletes {
            return Ok(false);
        }
        if !self.upserts.is_empty() {
            let mut upsert_stmt = tx
                .prepare_cached(
                    "INSERT OR REPLACE INTO domain_rows (domain, key, value) VALUES (?1, ?2, ?3)",
                )
                .map_err(|e| format!("domain_rows sync upsert prepare({}): {e}", self.domain))?;
            for (k, v) in &self.upserts {
                upsert_stmt
                    .execute(params![self.domain, k, v])
                    .map_err(|e| format!("domain_rows sync upsert({}, {k}): {e}", self.domain))?;
            }
        }
        if has_deletes {
            let mut del_stmt = tx
                .prepare_cached("DELETE FROM domain_rows WHERE domain = ?1 AND key = ?2")
                .map_err(|e| format!("domain_rows sync delete prepare({}): {e}", self.domain))?;
            for old_key in self.existing.keys() {
                if !self.live_keys.contains(old_key) {
                    del_stmt
                        .execute(params![self.domain, old_key])
                        .map_err(|e| {
                            format!("domain_rows sync delete({}, {old_key}): {e}", self.domain)
                        })?;
                }
            }
        }
        Ok(true)
    }
}

pub(crate) fn fetch_domain_row_entries(
    conn: &Connection,
    domain: &'static str,
) -> Result<Vec<(i64, String, String)>, String> {
    if !table_exists(conn, "domain_rows")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare("SELECT rowid, key, value FROM domain_rows WHERE domain = ?1 ORDER BY key ASC")
        .map_err(|e| format!("domain_rows load prepare({domain}): {e}"))?;
    let mut rows = stmt
        .query(params![domain])
        .map_err(|e| format!("domain_rows load query({domain}): {e}"))?;
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|e| format!("domain_rows load step({domain}): {e}"))?
    {
        out.push((
            row.get(0).map_err(|e| e.to_string())?,
            row.get(1).map_err(|e| e.to_string())?,
            row.get(2).map_err(|e| e.to_string())?,
        ));
    }
    Ok(out)
}

pub(crate) fn decode_map_entry<K: KeyCodec, V: DeserializeOwned>(
    subkey: &str,
    raw_val: &str,
) -> Result<(K, V), String> {
    let key = K::decode_key(subkey)?;
    let val = from_checked_domain_json::<V>(raw_val)?;
    Ok((key, val))
}

pub(crate) fn decode_set_entry<K: KeyCodec>(subkey: &str, raw_val: &str) -> Result<K, String> {
    let key = K::decode_key(subkey)?;
    if !from_checked_domain_json::<bool>(raw_val)? {
        return Err(format!("expected boolean true for set key '{subkey}'"));
    }
    Ok(key)
}

pub(crate) trait LegacyMerge: Sized {
    fn merge_legacy(&mut self, legacy: Self);
}

// ============================================================================
// Family 1 (`ledger`) Helpers & Legacy Blob-Table -> `domain_rows` Migration
// ============================================================================

pub(crate) fn table_exists(conn: &Connection, name: &str) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        params![name],
        |r| r.get(0),
    )
    .map_err(|e| format!("sqlite_master check({name}): {e}"))
}

/// Validates and applies `domain_rows` operations inside an active SQLite transaction.
pub(crate) fn apply_domain_row_changes(
    exec: &mut crate::tx_engine::TxSqlExecutor<'_, '_>,
    ops: &[DomainRowChange],
) -> Result<(), TxError> {
    for op in ops {
        match op {
            DomainRowChange::Upsert { domain, key, value } => {
                let json = value.as_ref().map_err(Clone::clone)?;
                exec.execute(
                    "domain_rows:upsert",
                    "INSERT OR REPLACE INTO domain_rows (domain, key, value) VALUES (?1, ?2, ?3)",
                    params![*domain, key, json],
                )?;
            }
            DomainRowChange::Delete { domain, key } => {
                exec.execute(
                    "domain_rows:delete",
                    "DELETE FROM domain_rows WHERE domain = ?1 AND key = ?2",
                    params![*domain, key],
                )?;
            }
        }
    }
    Ok(())
}

/// Validates all pending `domain_rows` operations before entering SQLite `BEGIN IMMEDIATE`.
pub(crate) fn validate_domain_row_changes(
    ops: &[DomainRowChange],
    max_row_bytes: usize,
) -> Result<(), TxError> {
    for op in ops {
        if let DomainRowChange::Upsert { domain, key, value } = op {
            let json = value.as_ref().map_err(Clone::clone)?;
            if json.len() > max_row_bytes {
                return Err(TxError::RowLimit {
                    table: *domain,
                    key: key.clone(),
                    bytes: json.len(),
                });
            }
        }
    }
    Ok(())
}

/// Loads Family 1 (`ledger`) from `domain_rows` and migrates any remaining legacy
/// `wallet_journal_meta` / `market_transactions` / `account_plex_vault` tables if present.
///
/// Returns `true` if any legacy Family 1 table was present and needs dropping once committed.
pub(crate) fn load_ledger_domain_rows(
    conn: &Connection,
    snap: &mut crate::WorldStateSnapshot,
    ctx: &mut super::quarantine::LoadContext,
) -> Result<bool, String> {
    let mut wallet_journal_meta: CowMap<i64, crate::ledger::JournalMeta> =
        CowMap::load_domain(conn, DOMAIN_WALLET_JOURNAL_META, ctx)?;
    let mut market_transactions: CowMap<i64, crate::ledger::MarketTransactionRecord> =
        CowMap::load_domain(conn, DOMAIN_MARKET_TRANSACTIONS, ctx)?;
    let mut account_plex_vault: CowMap<u64, i64> =
        CowMap::load_domain(conn, DOMAIN_ACCOUNT_PLEX_VAULT, ctx)?;

    let mut had_legacy = false;

    if table_exists(conn, "wallet_journal_meta")? {
        had_legacy = true;
        let mut stmt = conn
            .prepare("SELECT rowid, entry_id, meta_json FROM wallet_journal_meta")
            .map_err(|e| format!("legacy wallet_journal_meta prepare: {e}"))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| format!("legacy wallet_journal_meta query: {e}"))?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let rowid: i64 = row.get(0).map_err(|e| e.to_string())?;
            let id: i64 = row.get(1).map_err(|e| e.to_string())?;
            let payload: String = row.get(2).map_err(|e| e.to_string())?;
            match from_checked_domain_json::<crate::ledger::JournalMeta>(&payload) {
                Ok(meta) => {
                    if !wallet_journal_meta.contains_key(&id) {
                        wallet_journal_meta.insert(id, meta);
                    }
                }
                Err(err) => {
                    ctx.record_bad_row(
                        "wallet_journal_meta",
                        "wallet_journal_meta",
                        id.to_string(),
                        payload,
                        err,
                        Some(rowid),
                    )?;
                }
            }
        }
    }

    if table_exists(conn, "market_transactions")? {
        had_legacy = true;
        let mut stmt = conn
            .prepare("SELECT rowid, transaction_id, tx_json FROM market_transactions ORDER BY transaction_id ASC")
            .map_err(|e| format!("legacy market_transactions prepare: {e}"))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| format!("legacy market_transactions query: {e}"))?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let rowid: i64 = row.get(0).map_err(|e| e.to_string())?;
            let tx_id: i64 = row.get(1).map_err(|e| e.to_string())?;
            let payload: String = row.get(2).map_err(|e| e.to_string())?;
            match from_checked_domain_json::<crate::ledger::MarketTransactionRecord>(&payload) {
                Ok(rec) => {
                    if !market_transactions.contains_key(&rec.transaction_id) {
                        market_transactions.insert(rec.transaction_id, rec);
                    }
                }
                Err(err) => {
                    ctx.record_bad_row(
                        "market_transactions",
                        "market_transactions",
                        tx_id.to_string(),
                        payload,
                        err,
                        Some(rowid),
                    )?;
                }
            }
        }
    }

    if table_exists(conn, "account_plex_vault")? {
        had_legacy = true;
        let mut stmt = conn
            .prepare("SELECT rowid, user_id, plex FROM account_plex_vault")
            .map_err(|e| format!("legacy account_plex_vault prepare: {e}"))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| format!("legacy account_plex_vault query: {e}"))?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let rowid: i64 = row.get(0).map_err(|e| e.to_string())?;
            let uid: i64 = row.get(1).map_err(|e| e.to_string())?;
            let plex: i64 = row.get(2).map_err(|e| e.to_string())?;
            if uid < 0 {
                ctx.record_bad_row(
                    "account_plex_vault",
                    "account_plex_vault",
                    uid.to_string(),
                    plex.to_string(),
                    "negative user_id in account_plex_vault",
                    Some(rowid),
                )?;
                continue;
            }
            let uid = uid as u64;
            if !account_plex_vault.contains_key(&uid) {
                account_plex_vault.insert(uid, plex);
            }
        }
    }

    snap.wallet_journal_meta = wallet_journal_meta;
    snap.market_transactions = market_transactions;
    snap.account_plex_vault = account_plex_vault;

    Ok(had_legacy)
}

/// Drops the retired legacy Family 1 tables once migrated rows are flushed into `domain_rows`.
pub(crate) fn drop_legacy_ledger_tables(tx: &Transaction<'_>) -> Result<(), String> {
    tx.execute_batch(
        "DROP TABLE IF EXISTS wallet_journal_meta;
         DROP TABLE IF EXISTS market_transactions;
         DROP TABLE IF EXISTS account_plex_vault;",
    )
    .map_err(|e| format!("drop legacy ledger tables: {e}"))
}

/// Synchronizes all Family 1 (`ledger`) domains into `domain_rows`.
pub(crate) fn sync_ledger_domain_rows(
    tx: &Transaction<'_>,
    snap: &crate::WorldStateSnapshot,
) -> Result<bool, String> {
    let mut dirty = false;
    dirty |= snap
        .wallet_journal_meta
        .sync_full(tx, DOMAIN_WALLET_JOURNAL_META)?;
    dirty |= snap
        .market_transactions
        .sync_full(tx, DOMAIN_MARKET_TRANSACTIONS)?;
    dirty |= snap
        .account_plex_vault
        .sync_full(tx, DOMAIN_ACCOUNT_PLEX_VAULT)?;
    Ok(dirty)
}

// ============================================================================
// Family 2 (`notes_lp`) Helpers & Legacy Table -> `domain_rows` Migration
// ============================================================================

/// Loads Family 2 (`owner_notes`, `corp_lp_balances`, `char_lp_logs`) from `domain_rows` and
/// migrates any remaining legacy `owner_notes` / `corp_lp_balances` / `char_lp_logs` tables
/// if present.
///
/// Returns `true` if any legacy Family 2 table was present and needs dropping once committed.
pub(crate) fn load_notes_lp_domain_rows(
    conn: &Connection,
    snap: &mut crate::WorldStateSnapshot,
    ctx: &mut super::quarantine::LoadContext,
) -> Result<bool, String> {
    let mut owner_notes: CowMap<i64, crate::types::OwnerNoteRecord> =
        CowMap::load_domain(conn, DOMAIN_OWNER_NOTES, ctx)?;
    let mut corp_lp_balances: CowMap<(i64, i64), i64> =
        CowMap::load_domain(conn, DOMAIN_CORP_LP_BALANCES, ctx)?;
    let mut char_lp_logs: CowMap<i64, Vec<crate::snapshot_ext::LpRewardLogEntry>> =
        CowMap::load_domain(conn, DOMAIN_CHAR_LP_LOGS, ctx)?;

    let mut had_legacy = false;

    if table_exists(conn, "owner_notes")? {
        had_legacy = true;
        let mut stmt = conn
            .prepare(
                "SELECT rowid, note_id, character_id, item_id, label, note, timestamp_unix FROM owner_notes",
            )
            .map_err(|e| format!("legacy owner_notes prepare: {e}"))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| format!("legacy owner_notes query: {e}"))?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let rowid: i64 = row.get(0).map_err(|e| e.to_string())?;
            let key = super::quarantine::row_col_string(row, 1);
            let payload = super::quarantine::row_payload_summary(row, 1, 7);
            match (|| -> Result<crate::types::OwnerNoteRecord, String> {
                Ok(crate::types::OwnerNoteRecord {
                    note_id: row.get(1).map_err(|e| e.to_string())?,
                    character_id: row.get(2).map_err(|e| e.to_string())?,
                    item_id: row.get(3).map_err(|e| e.to_string())?,
                    label: row.get(4).map_err(|e| e.to_string())?,
                    note: row.get(5).map_err(|e| e.to_string())?,
                    timestamp_unix: row.get::<_, i64>(6).map_err(|e| e.to_string())? as u64,
                })
            })() {
                Ok(note) => {
                    if !owner_notes.contains_key(&note.note_id) {
                        owner_notes.insert(note.note_id, note);
                    }
                }
                Err(err) => {
                    ctx.record_bad_row(
                        "owner_notes",
                        "owner_notes",
                        key,
                        payload,
                        err,
                        Some(rowid),
                    )?;
                }
            }
        }
    }

    if table_exists(conn, "corp_lp_balances")? {
        had_legacy = true;
        let mut stmt = conn
            .prepare("SELECT rowid, corporation_id, issuer_corp_id, lp_balance FROM corp_lp_balances")
            .map_err(|e| format!("legacy corp_lp_balances prepare: {e}"))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| format!("legacy corp_lp_balances query: {e}"))?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let rowid: i64 = row.get(0).map_err(|e| e.to_string())?;
            let key = super::quarantine::row_composite_key_string(row, 1, 2);
            let payload = super::quarantine::row_payload_summary(row, 1, 4);
            match (|| -> Result<(i64, i64, i64), String> {
                Ok((
                    row.get(1).map_err(|e| e.to_string())?,
                    row.get(2).map_err(|e| e.to_string())?,
                    row.get(3).map_err(|e| e.to_string())?,
                ))
            })() {
                Ok((corp_id, issuer_corp_id, lp)) => {
                    if !corp_lp_balances.contains_key(&(corp_id, issuer_corp_id)) {
                        corp_lp_balances.insert((corp_id, issuer_corp_id), lp);
                    }
                }
                Err(err) => {
                    ctx.record_bad_row(
                        "corp_lp_balances",
                        "corp_lp_balances",
                        key,
                        payload,
                        err,
                        Some(rowid),
                    )?;
                }
            }
        }
    }

    if table_exists(conn, "char_lp_logs")? {
        had_legacy = true;
        let mut stmt = conn
            .prepare("SELECT rowid, character_id, logs_json FROM char_lp_logs")
            .map_err(|e| format!("legacy char_lp_logs prepare: {e}"))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| format!("legacy char_lp_logs query: {e}"))?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let rowid: i64 = row.get(0).map_err(|e| e.to_string())?;
            let key = super::quarantine::row_col_string(row, 1);
            let payload = super::quarantine::row_col_string(row, 2);
            match (|| -> Result<(i64, Vec<crate::snapshot_ext::LpRewardLogEntry>), String> {
                let cid: i64 = row.get(1).map_err(|e| e.to_string())?;
                let raw_json: String = row.get(2).map_err(|e| e.to_string())?;
                let entries =
                    from_checked_domain_json::<Vec<crate::snapshot_ext::LpRewardLogEntry>>(&raw_json)?;
                Ok((cid, entries))
            })() {
                Ok((cid, entries)) => {
                    if !char_lp_logs.contains_key(&cid) {
                        char_lp_logs.insert(cid, entries);
                    }
                }
                Err(err) => {
                    ctx.record_bad_row(
                        "char_lp_logs",
                        "char_lp_logs",
                        key,
                        payload,
                        err,
                        Some(rowid),
                    )?;
                }
            }
        }
    }

    snap.owner_notes = owner_notes;
    snap.corp_lp_balances = corp_lp_balances;
    snap.char_lp_logs = char_lp_logs;

    Ok(had_legacy)
}

/// Drops the retired legacy Family 2 tables once migrated rows are flushed into `domain_rows`.
pub(crate) fn drop_legacy_notes_lp_tables(tx: &Transaction<'_>) -> Result<(), String> {
    tx.execute_batch(
        "DROP TABLE IF EXISTS owner_notes;
         DROP TABLE IF EXISTS corp_lp_balances;
         DROP TABLE IF EXISTS char_lp_logs;",
    )
    .map_err(|e| format!("drop legacy notes_lp tables: {e}"))
}

/// Synchronizes all Family 2 (`notes_lp`) domains into `domain_rows`.
pub(crate) fn sync_notes_lp_domain_rows(
    tx: &Transaction<'_>,
    snap: &crate::WorldStateSnapshot,
) -> Result<bool, String> {
    let mut dirty = false;
    dirty |= snap.owner_notes.sync_full(tx, DOMAIN_OWNER_NOTES)?;
    dirty |= snap
        .corp_lp_balances
        .sync_full(tx, DOMAIN_CORP_LP_BALANCES)?;
    dirty |= snap.char_lp_logs.sync_full(tx, DOMAIN_CHAR_LP_LOGS)?;
    Ok(dirty)
}

// Family 3 (`social`) persistence lives in `social_store.rs` and is re-exported here so
// `domain_rows` serves as the single entry point for migrated families.
pub(crate) use super::social_store::{
    collect_social_domain_row_diffs, drop_legacy_social_tables, sync_social_domain_rows,
};

// Family 4 (`corp_crimewatch`) persistence lives in `corp_crimewatch_store.rs` and is
// re-exported here so `domain_rows` serves as the single entry point for migrated families.
pub(crate) use super::corp_crimewatch_store::{
    collect_corp_crimewatch_domain_row_diffs, drop_legacy_corp_crimewatch_tables,
    sync_corp_crimewatch_domain_rows,
};

// Family 6 (`service_domains`) persistence lives in `service_domains_store.rs` and is
// re-exported here so `domain_rows` serves as the single entry point for migrated families.
pub(crate) use super::service_domains_store::{
    drop_legacy_service_domain_tables, sync_service_domains_rows,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::WorldRepository;
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;

    struct TempDirGuard(std::path::PathBuf);
    impl TempDirGuard {
        fn new(label: &str) -> Self {
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "evomu_domain_rows_{label}_{}_{n}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create tempdir");
            Self(dir)
        }
    }
    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
    struct SampleWith128AndFloat {
        id: u64,
        amount_u128: u128,
        delta_i128: i128,
        pair: (u64, u128),
        opt_u128: Option<u128>,
        ratio: f64,
        nested_map: BTreeMap<u128, String>,
    }

    #[test]
    fn key_codec_is_bijective_and_handles_composite_strings() {
        let a: (u64, String) = (12, "3|foo\"bar\\baz".to_string());
        let b: (u64, String) = (1, "23|foo\"bar\\baz".to_string());
        let ea = a.encode_key();
        let eb = b.encode_key();
        assert_ne!(ea, eb);
        assert_eq!(<(u64, String)>::decode_key(&ea).unwrap(), a);
        assert_eq!(<(u64, String)>::decode_key(&eb).unwrap(), b);

        let triple: (i64, String, u32) = (-42, "hello|world".to_string(), 99);
        assert_eq!(
            <(i64, String, u32)>::decode_key(&triple.encode_key()).unwrap(),
            triple
        );

        let u_pair: ((), u128) = ((), u128::MAX);
        assert_eq!(
            <((), u128)>::decode_key(&u_pair.encode_key()).unwrap(),
            u_pair
        );
    }

    #[test]
    fn checked_json_encodes_u128_i128_as_decimal_strings_and_rejects_non_finite() {
        let mut nested_map = BTreeMap::new();
        nested_map.insert(u128::MAX, "max".to_string());
        let val = SampleWith128AndFloat {
            id: 7,
            amount_u128: u128::MAX,
            delta_i128: i128::MIN,
            pair: (9, 123_456_789_012_345_678_901_234_567_890u128),
            opt_u128: Some(999_999_999_999_999_999_999u128),
            ratio: 1.25,
            nested_map,
        };
        let encoded = to_checked_domain_json("test.domain", "42", &val).expect("serialize");
        let max_str = format!("\"{}\"", u128::MAX);
        let min_str = format!("\"{}\"", i128::MIN);
        assert!(encoded.contains(&max_str), "expected decimal string {max_str} in {encoded}");
        assert!(encoded.contains(&min_str), "expected decimal string {min_str} in {encoded}");

        let decoded: SampleWith128AndFloat =
            from_checked_domain_json(&encoded).expect("round-trip decode");
        assert_eq!(decoded, val);

        // Legacy unquoted JSON number compatibility for u128/i128.
        let legacy_json = r#"{"id":1,"amount_u128":42,"delta_i128":-99,"pair":[3,55],"opt_u128":null,"ratio":0.5,"nested_map":{}}"#;
        let legacy_decoded: SampleWith128AndFloat =
            from_checked_domain_json(legacy_json).expect("legacy decode");
        assert_eq!(legacy_decoded.amount_u128, 42);
        assert_eq!(legacy_decoded.delta_i128, -99);
        assert_eq!(legacy_decoded.pair, (3, 55));

        for bad_float in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let bad = SampleWith128AndFloat {
                ratio: bad_float,
                ..val.clone()
            };
            let err = to_checked_domain_json("test.domain", "k99", &bad)
                .expect_err("non-finite float must fail");
            let msg = err.to_string();
            assert!(msg.contains("test.domain"), "error must name domain: {msg}");
            assert!(msg.contains("k99"), "error must name key: {msg}");
        }
    }

    #[test]
    fn cow_cell_and_cow_map_inline_persist_roundtrip_and_diff() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply_domain_rows_migration(&conn).expect("migrate");

        let cell_before = CowCell::new(SampleWith128AndFloat {
            id: 1,
            amount_u128: 123,
            delta_i128: -456,
            pair: (10, 20),
            opt_u128: None,
            ratio: 2.0,
            nested_map: BTreeMap::new(),
        });
        let mut ops = Vec::new();
        cell_before.collect_diff_ops(&cell_before, "test.cell", &mut ops);
        assert!(ops.is_empty());

        let mut cell_after = cell_before.clone();
        cell_after.get_mut().amount_u128 = u128::MAX;
        cell_before.collect_diff_ops(&cell_after, "test.cell", &mut ops);
        assert_eq!(ops.len(), 1);

        {
            let tx = conn.transaction().expect("tx");
            assert!(cell_after.sync_full(&tx, "test.cell").expect("sync cell"));
            assert!(!cell_after.sync_full(&tx, "test.cell").expect("noop cell"));
            tx.commit().expect("commit");
        }
        let mut ctx = super::super::quarantine::LoadContext::quarantine_in_memory();
        let reloaded_cell =
            CowCell::<SampleWith128AndFloat>::load_domain(&conn, "test.cell", &mut ctx)
                .expect("load cell");
        assert_eq!(reloaded_cell, cell_after);

        let mut inline: CowMapInline<u64, u128> = CowMapInline::new();
        inline.insert(7, u128::MAX - 1);
        {
            let tx = conn.transaction().expect("tx");
            assert!(inline.sync_full(&tx, "test.inline").expect("sync inline"));
            tx.commit().expect("commit");
        }
        let reloaded_inline =
            CowMapInline::<u64, u128>::load_domain(&conn, "test.inline", &mut ctx)
                .expect("load inline");
        assert_eq!(reloaded_inline.get(&7), Some(&(u128::MAX - 1)));
    }

    #[test]
    fn tx_nan_or_inf_in_persisted_domain_row_fails_with_domain_and_key() {
        let repo = WorldRepository::with_demo_seed();
        for bad_isk in [f64::NAN, f64::INFINITY] {
            let res: Result<(), TxError> = repo.tx("bad_float_ledger", |wtx| {
                wtx.snap.market_transactions.insert(
                    9_876_543,
                    crate::ledger::MarketTransactionRecord {
                        transaction_id: 9_876_543,
                        date_unix: 1_700_000_000,
                        type_id: 34,
                        quantity: 10,
                        price: crate::types::IskCents(1_000),
                        location_id: 60_003_760,
                        buyer_id: 90_000_001,
                        seller_id: 90_000_002,
                        buyer_account_key: 1000,
                        seller_account_key: 1000,
                        journal_ref_id: 1,
                    },
                );
                // Put a non-finite float via `JournalMeta` or `MarketTransactionRecord`:
                // Note `JournalMeta` and `MarketTransactionRecord` don't currently hold f64,
                // so validate `to_checked_domain_json` + `TxError::NonFinite` directly too!
                Err(to_checked_domain_json(
                    DOMAIN_MARKET_TRANSACTIONS,
                    "9876543",
                    &bad_isk,
                )
                .unwrap_err())
            });
            let err = res.expect_err("non-finite float must fail tx");
            let msg = err.to_string();
            assert!(msg.contains(DOMAIN_MARKET_TRANSACTIONS), "{msg}");
            assert!(msg.contains("9876543"), "{msg}");
        }

        // Also exercise CowMap<u64, SampleWith128AndFloat>::collect_diff_ops + validate_domain_row_changes
        // directly when a persisted map value contains NaN or Infinity.
        for bad_float in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let before_map: CowMap<u64, SampleWith128AndFloat> = CowMap::new();
            let mut after_map = before_map.clone();
            after_map.insert(
                4242,
                SampleWith128AndFloat {
                    id: 1,
                    amount_u128: 99,
                    delta_i128: -7,
                    pair: (1, 2),
                    opt_u128: Some(3),
                    ratio: bad_float,
                    nested_map: BTreeMap::new(),
                },
            );
            let mut ops = Vec::new();
            before_map.collect_diff_ops(&after_map, "custom.float_domain", &mut ops);
            let err = validate_domain_row_changes(&ops, 1024 * 1024)
                .expect_err("NaN/Inf in CowMap must fail validate_domain_row_changes");
            let msg = err.to_string();
            assert!(msg.contains("custom.float_domain"), "{msg}");
            assert!(msg.contains("4242"), "{msg}");
        }
    }

    #[test]
    fn legacy_ledger_tables_migrate_to_domain_rows_idempotently_and_preserve_pre() {
        let dir = TempDirGuard::new("legacy_migrate");
        let db_path = dir.0.join("legacy_ledger_migrate.db");

        // Build a DB with full modern schema PLUS legacy Family 1 tables populated as an old DB would have.
        {
            let conn = Connection::open(&db_path).expect("open");
            crate::sqlite_store::apply_migrations(&conn).expect("migrations");
            conn.execute_batch(
                "CREATE TABLE wallet_journal_meta (
                    entry_id INTEGER PRIMARY KEY,
                    meta_json TEXT NOT NULL
                );
                CREATE TABLE market_transactions (
                    transaction_id INTEGER PRIMARY KEY,
                    tx_json TEXT NOT NULL
                );
                CREATE TABLE account_plex_vault (
                    user_id INTEGER PRIMARY KEY,
                    plex INTEGER NOT NULL
                );",
            )
            .expect("create legacy ledger tables");

            let meta = crate::ledger::JournalMeta::new(37, 90_000_001, 90_000_002, 1_700_000_010)
                .with_context(crate::ledger::JournalContext::MarketTransaction, 8001)
                .with_account_key(1000);
            let tx_rec = crate::ledger::MarketTransactionRecord {
                transaction_id: 8001,
                date_unix: 1_700_000_010,
                type_id: 34,
                quantity: 50,
                price: crate::types::IskCents(1_250),
                location_id: 60_003_760,
                buyer_id: 90_000_001,
                seller_id: 90_000_002,
                buyer_account_key: 1000,
                seller_account_key: 1001,
                journal_ref_id: 901,
            };
            conn.execute(
                "INSERT INTO wallet_journal_meta (entry_id, meta_json) VALUES (?1, ?2)",
                params![901_i64, serde_json::to_string(&meta).unwrap()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO market_transactions (transaction_id, tx_json) VALUES (?1, ?2)",
                params![8001_i64, serde_json::to_string(&tx_rec).unwrap()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO account_plex_vault (user_id, plex) VALUES (101, 450), (102, 900)",
                [],
            )
            .unwrap();
        }

        // Open through WorldRepository: loads legacy rows, migrates to `domain_rows`, drops legacy tables.
        let repo = WorldRepository::open_sqlite(&db_path).expect("open migrated repo");
        let snap1 = repo.read_state();
        assert_eq!(snap1.wallet_journal_meta[&901].date_unix, 1_700_000_010);
        assert_eq!(snap1.market_transactions[&8001].quantity, 50);
        assert_eq!(snap1.account_plex_vault[&101], 450);
        assert_eq!(snap1.account_plex_vault[&102], 900);

        // Mutate and delete one migrated entry to verify deletes persist without legacy resurrection.
        repo.tx("delete_and_update_migrated_ledger", |wtx| -> Result<(), TxError> {
            wtx.snap.account_plex_vault.remove(&102);
            wtx.snap.account_plex_vault.insert(101, 555);
            Ok(())
        })
        .expect("mutate migrated ledger");
        drop(repo);

        // Verify legacy tables were dropped and `domain_rows` holds the canonical persisted state.
        {
            let conn = Connection::open(&db_path).expect("reopen raw sqlite");
            for old_tbl in [
                "wallet_journal_meta",
                "market_transactions",
                "account_plex_vault",
            ] {
                assert!(
                    !table_exists(&conn, old_tbl).expect("check table exists"),
                    "legacy table {old_tbl} should be dropped after migration"
                );
            }
            let domain_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM domain_rows WHERE domain IN (?1, ?2, ?3)",
                    params![
                        DOMAIN_WALLET_JOURNAL_META,
                        DOMAIN_MARKET_TRANSACTIONS,
                        DOMAIN_ACCOUNT_PLEX_VAULT
                    ],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(domain_count, 3);
            let unexpected_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM domain_rows
                     WHERE domain NOT IN (?1, ?2, ?3)
                       AND NOT (domain = ?4 AND key LIKE 'fw_system_occupations/%')",
                    params![
                        DOMAIN_WALLET_JOURNAL_META,
                        DOMAIN_MARKET_TRANSACTIONS,
                        DOMAIN_ACCOUNT_PLEX_VAULT,
                        DOMAIN_WARFARE
                    ],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                unexpected_count, 0,
                "only ledger rows and startup-backfilled fw_system_occupations should populate domain_rows"
            );
        }

        // Reopen again and check persist-reload equivalence.
        let reopened = WorldRepository::open_sqlite(&db_path).expect("reopen after delete");
        let snap2 = reopened.read_state();
        assert_eq!(snap2.wallet_journal_meta, snap1.wallet_journal_meta);
        assert_eq!(snap2.market_transactions, snap1.market_transactions);
        assert_eq!(snap2.account_plex_vault.get(&101), Some(&555));
        assert_eq!(snap2.account_plex_vault.get(&102), None);
    }

    #[test]
    fn single_entry_mutation_writes_strictly_one_sqlite_row() {
        let mut conn = Connection::open_in_memory().expect("open");
        crate::sqlite_store::apply_migrations(&conn).expect("migrations");

        let mut snap = crate::WorldStateSnapshot::default();
        for i in 1..=50_u64 {
            snap.account_plex_vault.insert(i, (i as i64) * 10);
            snap.wallet_journal_meta.insert(
                i as i64,
                crate::ledger::JournalMeta::new(10, 90_000_001, 90_000_002, 1_700_000_000 + i),
            );
            snap.market_transactions.insert(
                i as i64,
                crate::ledger::MarketTransactionRecord {
                    transaction_id: i as i64,
                    date_unix: 1_700_000_000 + i,
                    type_id: 34,
                    quantity: i as i64,
                    price: crate::types::IskCents(100),
                    location_id: 60_003_760,
                    buyer_id: 90_000_001,
                    seller_id: 90_000_002,
                    buyer_account_key: 1000,
                    seller_account_key: 1000,
                    journal_ref_id: i as i64,
                },
            );
        }
        {
            let tx = conn.transaction().expect("seed tx");
            assert!(sync_ledger_domain_rows(&tx, &snap).expect("seed sync"));
            tx.commit().expect("seed commit");
        }

        let base_fp = crate::sqlite_store::compute_fingerprints(&snap);

        // Mutate strictly ONE entry in `account_plex_vault`.
        let before_update = snap.clone();
        snap.account_plex_vault.insert(25, 999_999);
        let changes_update = crate::tx_engine::ChangeSet::diff(1, &before_update, &snap);
        assert_eq!(changes_update.domain_rows.len(), 1);
        assert!(!changes_update.cold_domains.any());

        let total_before_update = conn.total_changes();
        {
            let sql_tx = conn.transaction().expect("update tx");
            let mut exec = crate::tx_engine::TxSqlExecutor::new(
                &sql_tx,
                crate::tx_engine::TxFaultPlan::None,
            );
            let mut staged_fp = base_fp.clone();
            let (_delta, _hot) = crate::tx_engine::apply_changeset_to_sqlite(
                &mut exec,
                &snap,
                &changes_update,
                &base_fp,
                &mut staged_fp,
                false,
            )
            .expect("apply update");
            sql_tx.commit().expect("commit update");
        }
        assert_eq!(
            conn.total_changes() - total_before_update,
            1,
            "mutating one CowMap entry must write strictly 1 SQLite row"
        );

        // Delete strictly ONE entry in `market_transactions`.
        let before_delete = snap.clone();
        snap.market_transactions.remove(&25);
        let changes_delete = crate::tx_engine::ChangeSet::diff(2, &before_delete, &snap);
        assert_eq!(changes_delete.domain_rows.len(), 1);

        let total_before_delete = conn.total_changes();
        {
            let sql_tx = conn.transaction().expect("delete tx");
            let mut exec = crate::tx_engine::TxSqlExecutor::new(
                &sql_tx,
                crate::tx_engine::TxFaultPlan::None,
            );
            let mut staged_fp = base_fp.clone();
            let (_delta, _hot) = crate::tx_engine::apply_changeset_to_sqlite(
                &mut exec,
                &snap,
                &changes_delete,
                &base_fp,
                &mut staged_fp,
                false,
            )
            .expect("apply delete");
            sql_tx.commit().expect("commit delete");
        }
        assert_eq!(
            conn.total_changes() - total_before_delete,
            1,
            "deleting one CowMap entry must delete strictly 1 SQLite row"
        );
    }

    #[test]
    fn load_ledger_domain_rows_fails_closed_without_overwriting_existing_snapshot_maps() {
        let conn = Connection::open_in_memory().expect("open");
        crate::sqlite_store::apply_migrations(&conn).expect("migrations");

        // Seed valid `wallet_journal_meta` and invalid `market_transactions` directly in `domain_rows`.
        conn.execute(
            "INSERT INTO domain_rows (domain, key, value) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                DOMAIN_WALLET_JOURNAL_META,
                "10",
                serde_json::to_string(&crate::ledger::JournalMeta::new(
                    10,
                    90_000_001,
                    90_000_002,
                    1_700_000_010
                ))
                .expect("json")
            ],
        )
        .expect("insert valid wallet_journal_meta");
        conn.execute(
            "INSERT INTO domain_rows (domain, key, value) VALUES (?1, ?2, ?3)",
            rusqlite::params![DOMAIN_MARKET_TRANSACTIONS, "20", "{corrupt-json"],
        )
        .expect("insert corrupt market_transactions");

        // Pre-populate live maps on `snap` and verify a mid-load error leaves ALL 3 maps untouched.
        let mut snap = crate::WorldStateSnapshot::default();
        snap.wallet_journal_meta.insert(
            99,
            crate::ledger::JournalMeta::new(10, 1, 2, 1_700_000_099),
        );
        snap.market_transactions.insert(
            88,
            crate::ledger::MarketTransactionRecord {
                transaction_id: 88,
                date_unix: 1_700_000_088,
                type_id: 34,
                quantity: 5,
                price: crate::types::IskCents(200),
                location_id: 60_003_760,
                buyer_id: 90_000_001,
                seller_id: 90_000_002,
                buyer_account_key: 1000,
                seller_account_key: 1000,
                journal_ref_id: 99,
            },
        );
        snap.account_plex_vault.insert(77, 777);

        let mut load_ctx = crate::sqlite_store::LoadContext::new(false);
        let err = load_ledger_domain_rows(&conn, &mut snap, &mut load_ctx).unwrap_err();
        assert!(!err.is_empty());
        assert!(snap.wallet_journal_meta.contains_key(&99));
        assert!(!snap.wallet_journal_meta.contains_key(&10));
        assert!(snap.market_transactions.contains_key(&88));
        assert_eq!(snap.account_plex_vault.get(&77), Some(&777));
    }

    #[test]
    fn legacy_notes_lp_tables_migrate_to_domain_rows_and_drop_legacy_tables() {
        let dir = TempDirGuard::new("legacy_notes_lp");
        let db_path = dir.0.join("world.sqlite");

        {
            let conn = Connection::open(&db_path).expect("open sqlite");
            crate::sqlite_store::apply_migrations(&conn).expect("migrations");
            conn.execute_batch(
                "CREATE TABLE owner_notes (
                    note_id INTEGER PRIMARY KEY,
                    character_id INTEGER NOT NULL,
                    item_id INTEGER NOT NULL,
                    label TEXT NOT NULL,
                    note TEXT NOT NULL,
                    timestamp_unix INTEGER NOT NULL
                );
                CREATE TABLE corp_lp_balances (
                    corporation_id INTEGER NOT NULL,
                    issuer_corp_id INTEGER NOT NULL,
                    lp_balance INTEGER NOT NULL,
                    PRIMARY KEY (corporation_id, issuer_corp_id)
                );
                CREATE TABLE char_lp_logs (
                    character_id INTEGER PRIMARY KEY,
                    logs_json TEXT NOT NULL
                );",
            )
            .expect("create legacy tables");

            conn.execute(
                "INSERT INTO owner_notes (note_id, character_id, item_id, label, note, timestamp_unix)
                 VALUES (501, 90000011, 60003760, 'FitLabel', 'NoteText', 1700000123)",
                [],
            )
            .expect("insert legacy owner_notes");
            conn.execute(
                "INSERT INTO corp_lp_balances (corporation_id, issuer_corp_id, lp_balance)
                 VALUES (98000010, 1000125, 4200)",
                [],
            )
            .expect("insert legacy corp_lp_balances");
            let logs_json = serde_json::to_string(&vec![crate::snapshot_ext::LpRewardLogEntry {
                log_id: 7,
                corp_id: 1_000_125,
                amount: 850,
                reason: "plex_capture".into(),
                timestamp_unix: 1_700_000_200,
            }])
            .expect("serialize logs");
            conn.execute(
                "INSERT INTO char_lp_logs (character_id, logs_json) VALUES (90000011, ?1)",
                rusqlite::params![logs_json],
            )
            .expect("insert legacy char_lp_logs");
        }

        let repo = WorldRepository::open_sqlite(&db_path).expect("open migrated repo");
        repo.with_snapshot(|snap| {
            let note = snap.owner_notes.get(&501).expect("migrated owner_note");
            assert_eq!(note.character_id, 90_000_011);
            assert_eq!(note.item_id, 60_003_760);
            assert_eq!(note.label, "FitLabel");
            assert_eq!(note.note, "NoteText");
            assert_eq!(note.timestamp_unix, 1_700_000_123);
            assert_eq!(
                snap.corp_lp_balances.get(&(98_000_010, 1_000_125)),
                Some(&4_200)
            );
            let logs = snap.char_lp_logs.get(&90_000_011).expect("migrated logs");
            assert_eq!(logs.len(), 1);
            assert_eq!(logs[0].log_id, 7);
            assert_eq!(logs[0].amount, 850);
        });

        repo.with_sqlite_conn(|conn| {
            for legacy_tbl in ["owner_notes", "corp_lp_balances", "char_lp_logs"] {
                let exists: bool = conn
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                        rusqlite::params![legacy_tbl],
                        |r| r.get(0),
                    )
                    .expect("check legacy table dropped");
                assert!(!exists, "legacy table {legacy_tbl} should have been dropped");
            }
            let domain_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM domain_rows WHERE domain IN (?1, ?2, ?3)",
                    rusqlite::params![
                        DOMAIN_OWNER_NOTES,
                        DOMAIN_CORP_LP_BALANCES,
                        DOMAIN_CHAR_LP_LOGS
                    ],
                    |r| r.get(0),
                )
                .expect("count domain_rows");
            assert_eq!(domain_count, 3);
        });
        drop(repo);

        let reopened = WorldRepository::open_sqlite(&db_path).expect("reopen after migration");
        reopened.with_snapshot(|snap| {
            assert_eq!(snap.owner_notes[&501].note, "NoteText");
            assert_eq!(snap.corp_lp_balances[&(98_000_010, 1_000_125)], 4_200);
            assert_eq!(snap.char_lp_logs[&90_000_011][0].amount, 850);
        });
    }

    #[test]
    fn notes_lp_domain_rows_write_single_row_per_cowmap_delta() {
        let mut conn = Connection::open_in_memory().expect("open");
        crate::sqlite_store::apply_migrations(&conn).expect("migrations");

        let mut snap = crate::WorldStateSnapshot::default();
        for i in 1..=20i64 {
            snap.owner_notes.insert(
                i,
                crate::types::OwnerNoteRecord {
                    note_id: i,
                    character_id: 90_000_001,
                    item_id: 60_003_760,
                    label: format!("L{i}"),
                    note: format!("Body{i}"),
                    timestamp_unix: 1_700_000_000 + i as u64,
                },
            );
            snap.corp_lp_balances.insert((98_000_001, 1_000_000 + i), i * 100);
            snap.char_lp_logs.insert(
                90_000_000 + i,
                vec![crate::snapshot_ext::LpRewardLogEntry {
                    log_id: i,
                    corp_id: 1_000_035,
                    amount: i * 10,
                    reason: "mission".into(),
                    timestamp_unix: 1_700_000_000 + i as u64,
                }],
            );
        }
        {
            let tx = conn.transaction().expect("seed tx");
            assert!(sync_notes_lp_domain_rows(&tx, &snap).expect("seed sync"));
            assert!(!sync_notes_lp_domain_rows(&tx, &snap).expect("idempotent sync"));
            tx.commit().expect("seed commit");
        }

        let base_fp = crate::sqlite_store::compute_fingerprints(&snap);

        // 1. Mutate strictly ONE `corp_lp_balances` composite-key entry.
        let before_lp = snap.clone();
        snap.corp_lp_balances.insert((98_000_001, 1_000_010), 99_999);
        let changes_lp = crate::tx_engine::ChangeSet::diff(1, &before_lp, &snap);
        assert_eq!(changes_lp.domain_rows.len(), 1);
        assert!(!changes_lp.cold_domains.any());

        let total_before_lp = conn.total_changes();
        {
            let sql_tx = conn.transaction().expect("lp tx");
            let mut exec = crate::tx_engine::TxSqlExecutor::new(
                &sql_tx,
                crate::tx_engine::TxFaultPlan::None,
            );
            let mut staged_fp = base_fp.clone();
            let (_delta, _hot) = crate::tx_engine::apply_changeset_to_sqlite(
                &mut exec,
                &snap,
                &changes_lp,
                &base_fp,
                &mut staged_fp,
                false,
            )
            .expect("apply lp update");
            sql_tx.commit().expect("commit lp update");
        }
        assert_eq!(
            conn.total_changes() - total_before_lp,
            1,
            "mutating one corp_lp_balances CowMap entry must write strictly 1 SQLite row"
        );

        // 2. Mutate strictly ONE `owner_notes` entry.
        let before_note = snap.clone();
        snap.owner_notes.get_mut(&12).unwrap().note = "UpdatedNote12".into();
        let changes_note = crate::tx_engine::ChangeSet::diff(2, &before_note, &snap);
        assert_eq!(changes_note.domain_rows.len(), 1);

        let total_before_note = conn.total_changes();
        {
            let sql_tx = conn.transaction().expect("note tx");
            let mut exec = crate::tx_engine::TxSqlExecutor::new(
                &sql_tx,
                crate::tx_engine::TxFaultPlan::None,
            );
            let mut staged_fp = base_fp.clone();
            let (_delta, _hot) = crate::tx_engine::apply_changeset_to_sqlite(
                &mut exec,
                &snap,
                &changes_note,
                &base_fp,
                &mut staged_fp,
                false,
            )
            .expect("apply note update");
            sql_tx.commit().expect("commit note update");
        }
        assert_eq!(
            conn.total_changes() - total_before_note,
            1,
            "mutating one owner_notes CowMap entry must write strictly 1 SQLite row"
        );

        // 3. Delete strictly ONE `char_lp_logs` entry.
        let before_log = snap.clone();
        snap.char_lp_logs.remove(&90_000_005);
        let changes_log = crate::tx_engine::ChangeSet::diff(3, &before_log, &snap);
        assert_eq!(changes_log.domain_rows.len(), 1);

        let total_before_log = conn.total_changes();
        {
            let sql_tx = conn.transaction().expect("log del tx");
            let mut exec = crate::tx_engine::TxSqlExecutor::new(
                &sql_tx,
                crate::tx_engine::TxFaultPlan::None,
            );
            let mut staged_fp = base_fp.clone();
            let (_delta, _hot) = crate::tx_engine::apply_changeset_to_sqlite(
                &mut exec,
                &snap,
                &changes_log,
                &base_fp,
                &mut staged_fp,
                false,
            )
            .expect("apply log delete");
            sql_tx.commit().expect("commit log delete");
        }
        assert_eq!(
            conn.total_changes() - total_before_log,
            1,
            "deleting one char_lp_logs CowMap entry must delete strictly 1 SQLite row"
        );

        // Reload and verify persisted state matches.
        let mut reloaded = crate::WorldStateSnapshot::default();
        let had_legacy = load_notes_lp_domain_rows(
            &conn,
            &mut reloaded,
            &mut crate::sqlite_store::LoadContext::quarantine_in_memory(),
        )
        .expect("reload");
        assert!(!had_legacy);
        assert_eq!(reloaded.corp_lp_balances[&(98_000_001, 1_000_010)], 99_999);
        assert_eq!(reloaded.owner_notes[&12].note, "UpdatedNote12");
        assert!(!reloaded.char_lp_logs.contains_key(&90_000_005));
    }

    #[test]
    fn load_notes_lp_domain_rows_fails_closed_without_overwriting_existing_snapshot_maps() {
        let conn = Connection::open_in_memory().expect("open");
        crate::sqlite_store::apply_migrations(&conn).expect("migrations");

        conn.execute(
            "INSERT INTO domain_rows (domain, key, value) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                DOMAIN_OWNER_NOTES,
                "10",
                serde_json::to_string(&crate::types::OwnerNoteRecord {
                    note_id: 10,
                    character_id: 90_000_001,
                    item_id: 60_003_760,
                    label: "L".into(),
                    note: "N".into(),
                    timestamp_unix: 1_700_000_010,
                })
                .expect("json")
            ],
        )
        .expect("insert valid owner_notes");
        conn.execute(
            "INSERT INTO domain_rows (domain, key, value) VALUES (?1, ?2, ?3)",
            rusqlite::params![DOMAIN_CHAR_LP_LOGS, "90000020", "{corrupt-json"],
        )
        .expect("insert corrupt char_lp_logs");

        let mut snap = crate::WorldStateSnapshot::default();
        snap.owner_notes.insert(
            99,
            crate::types::OwnerNoteRecord {
                note_id: 99,
                character_id: 1,
                item_id: 2,
                label: "Live".into(),
                note: "Keep".into(),
                timestamp_unix: 100,
            },
        );
        snap.corp_lp_balances.insert((98_000_001, 1_000_035), 777);
        snap.char_lp_logs.insert(
            90_000_001,
            vec![crate::snapshot_ext::LpRewardLogEntry {
                log_id: 1,
                corp_id: 1_000_035,
                amount: 50,
                reason: "keep".into(),
                timestamp_unix: 100,
            }],
        );

        let mut load_ctx = crate::sqlite_store::LoadContext::new(false);
        let err = load_notes_lp_domain_rows(&conn, &mut snap, &mut load_ctx).unwrap_err();
        assert!(err.contains(DOMAIN_CHAR_LP_LOGS), "unexpected err: {err}");
        assert!(snap.owner_notes.contains_key(&99));
        assert!(!snap.owner_notes.contains_key(&10));
        assert_eq!(snap.corp_lp_balances.get(&(98_000_001, 1_000_035)), Some(&777));
        assert!(snap.char_lp_logs.contains_key(&90_000_001));
    }
}
