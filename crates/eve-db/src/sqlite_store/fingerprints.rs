//! Table change tracking and FNV-1a row fingerprinting.

use crate::types::*;
use eve_sde::{SolarSystemId, TypeId};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// Fast zero-allocation FNV-1a 64-bit hasher for row-level dirty detection.
pub(crate) struct FnvHasher(pub(crate) u64);

impl FnvHasher {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01b3;

    #[inline]
    pub(crate) fn new() -> Self {
        Self(Self::OFFSET_BASIS)
    }

    #[inline]
    pub(crate) fn write_bytes(&mut self, bytes: &[u8]) {
        if bytes.len() <= 64 {
            let mut i = 0;
            while i + 8 <= bytes.len() {
                let word = u64::from_le_bytes([
                    bytes[i],
                    bytes[i + 1],
                    bytes[i + 2],
                    bytes[i + 3],
                    bytes[i + 4],
                    bytes[i + 5],
                    bytes[i + 6],
                    bytes[i + 7],
                ]);
                self.mix_word(word);
                i += 8;
            }
            if i < bytes.len() {
                let mut buf = [0u8; 8];
                let rem = &bytes[i..];
                buf[..rem.len()].copy_from_slice(rem);
                self.mix_word(u64::from_le_bytes(buf) ^ ((rem.len() as u64) << 56));
            } else if bytes.is_empty() {
                self.mix_word(0);
            }
            return;
        }
        use std::hash::Hasher;
        let mut dh = std::collections::hash_map::DefaultHasher::new();
        dh.write(bytes);
        self.mix_word(dh.finish());
    }

    #[inline]
    pub(crate) fn write_u64(&mut self, v: u64) {
        self.mix_word(v);
    }

    #[inline]
    pub(crate) fn write_i64(&mut self, v: i64) {
        self.mix_word(v as u64);
    }

    #[inline]
    pub(crate) fn write_u128(&mut self, v: u128) {
        self.mix_word(v as u64);
        self.mix_word((v >> 64) as u64);
    }

    #[inline]
    pub(crate) fn write_i128(&mut self, v: i128) {
        self.write_u128(v as u128);
    }

    #[inline]
    pub(crate) fn write_u32(&mut self, v: u32) {
        self.mix_word(v as u64);
    }

    #[inline]
    pub(crate) fn write_u8(&mut self, v: u8) {
        self.0 ^= v as u64;
        self.0 = self.0.wrapping_mul(Self::PRIME);
    }

    #[inline]
    pub(crate) fn write_bool(&mut self, v: bool) {
        self.write_u8(u8::from(v));
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn write_f64(&mut self, v: f64) {
        self.write_u64(v.to_bits());
    }

    #[inline]
    pub(crate) fn write_str(&mut self, s: &str) {
        self.write_u64(s.len() as u64);
        self.write_bytes(s.as_bytes());
    }

    #[inline]
    pub(crate) fn mix_word(&mut self, v: u64) {
        self.0 = (self.0 ^ v)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .rotate_left(27)
            .wrapping_mul(Self::PRIME);
    }

    #[inline]
    pub(crate) fn finish(self) -> u64 {
        self.0
    }
}

impl std::hash::Hasher for FnvHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        self.write_bytes(bytes);
    }
}

#[cfg(test)]
#[inline]
pub(crate) fn fnv1a_hasher() -> FnvHasher {
    FnvHasher::new()
}

#[derive(Debug)]
pub(crate) struct FnvSerError(pub(crate) String);

impl std::fmt::Display for FnvSerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let _ = &self.0;
        write!(f, "fnv serialization error")
    }
}

impl std::error::Error for FnvSerError {}

impl serde::ser::Error for FnvSerError {
    fn custom<T: std::fmt::Display>(msg: T) -> Self {
        Self(msg.to_string())
    }
}

/// Zero-allocation deterministic FNV-1a serde serializer for persisted state structs.
/// Maps combine order-independently via `(len, xor, sum)`; fields are visited via `#[derive(Serialize)]`.
pub(crate) struct FnvSerializer<'a>(pub(crate) &'a mut FnvHasher);

pub(crate) struct FnvSeqSer<'a>(&'a mut FnvHasher);

impl serde::ser::SerializeSeq for FnvSeqSer<'_> {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), FnvSerError> {
        value.serialize(FnvSerializer(self.0))
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl serde::ser::SerializeTuple for FnvSeqSer<'_> {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), FnvSerError> {
        value.serialize(FnvSerializer(self.0))
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl serde::ser::SerializeTupleStruct for FnvSeqSer<'_> {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), FnvSerError> {
        value.serialize(FnvSerializer(self.0))
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl serde::ser::SerializeTupleVariant for FnvSeqSer<'_> {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), FnvSerError> {
        value.serialize(FnvSerializer(self.0))
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

pub(crate) struct FnvMapSer<'a> {
    parent: &'a mut FnvHasher,
    len: u64,
    xor: u64,
    sum: u64,
    pending_key_hash: u64,
}

impl serde::ser::SerializeMap for FnvMapSer<'_> {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<(), FnvSerError> {
        let mut kh = FnvHasher::new();
        key.serialize(FnvSerializer(&mut kh))?;
        self.pending_key_hash = kh.finish();
        Ok(())
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), FnvSerError> {
        let mut eh = FnvHasher::new();
        eh.write_u64(self.pending_key_hash);
        value.serialize(FnvSerializer(&mut eh))?;
        let entry_fp = eh.finish();
        self.xor ^= entry_fp;
        self.sum = self.sum.wrapping_add(entry_fp.rotate_left(13));
        self.len = self.len.wrapping_add(1);
        Ok(())
    }
    fn end(self) -> Result<(), FnvSerError> {
        self.parent.write_u64(self.len);
        self.parent.write_u64(self.xor);
        self.parent.write_u64(self.sum);
        Ok(())
    }
}

impl serde::ser::SerializeStruct for FnvSeqSer<'_> {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), FnvSerError> {
        self.0.write_str(key);
        value.serialize(FnvSerializer(self.0))
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl serde::ser::SerializeStructVariant for FnvSeqSer<'_> {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), FnvSerError> {
        self.0.write_str(key);
        value.serialize(FnvSerializer(self.0))
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl<'a> serde::Serializer for FnvSerializer<'a> {
    type Ok = ();
    type Error = FnvSerError;
    type SerializeSeq = FnvSeqSer<'a>;
    type SerializeTuple = FnvSeqSer<'a>;
    type SerializeTupleStruct = FnvSeqSer<'a>;
    type SerializeTupleVariant = FnvSeqSer<'a>;
    type SerializeMap = FnvMapSer<'a>;
    type SerializeStruct = FnvSeqSer<'a>;
    type SerializeStructVariant = FnvSeqSer<'a>;

    fn serialize_bool(self, v: bool) -> Result<(), FnvSerError> {
        self.0.write_u8(1);
        self.0.write_bool(v);
        Ok(())
    }
    fn serialize_i8(self, v: i8) -> Result<(), FnvSerError> {
        self.serialize_i64(i64::from(v))
    }
    fn serialize_i16(self, v: i16) -> Result<(), FnvSerError> {
        self.serialize_i64(i64::from(v))
    }
    fn serialize_i32(self, v: i32) -> Result<(), FnvSerError> {
        self.serialize_i64(i64::from(v))
    }
    fn serialize_i64(self, v: i64) -> Result<(), FnvSerError> {
        self.0.write_u8(2);
        self.0.write_i64(v);
        Ok(())
    }
    fn serialize_i128(self, v: i128) -> Result<(), FnvSerError> {
        self.0.write_u8(20);
        self.0.write_i128(v);
        Ok(())
    }
    fn serialize_u8(self, v: u8) -> Result<(), FnvSerError> {
        self.serialize_u64(u64::from(v))
    }
    fn serialize_u16(self, v: u16) -> Result<(), FnvSerError> {
        self.serialize_u64(u64::from(v))
    }
    fn serialize_u32(self, v: u32) -> Result<(), FnvSerError> {
        self.serialize_u64(u64::from(v))
    }
    fn serialize_u64(self, v: u64) -> Result<(), FnvSerError> {
        self.0.write_u8(3);
        self.0.write_u64(v);
        Ok(())
    }
    fn serialize_u128(self, v: u128) -> Result<(), FnvSerError> {
        self.0.write_u8(21);
        self.0.write_u128(v);
        Ok(())
    }
    fn serialize_f32(self, v: f32) -> Result<(), FnvSerError> {
        self.serialize_f64(f64::from(v))
    }
    fn serialize_f64(self, v: f64) -> Result<(), FnvSerError> {
        self.0.write_u8(4);
        let bits = if v == 0.0 { 0u64 } else { v.to_bits() };
        self.0.write_u64(bits);
        Ok(())
    }
    fn serialize_char(self, v: char) -> Result<(), FnvSerError> {
        self.serialize_u32(v as u32)
    }
    fn serialize_str(self, v: &str) -> Result<(), FnvSerError> {
        self.0.write_u8(5);
        self.0.write_str(v);
        Ok(())
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<(), FnvSerError> {
        self.0.write_u8(6);
        self.0.write_u64(v.len() as u64);
        self.0.write_bytes(v);
        Ok(())
    }
    fn serialize_none(self) -> Result<(), FnvSerError> {
        self.0.write_u8(7);
        Ok(())
    }
    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<(), FnvSerError> {
        self.0.write_u8(8);
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), FnvSerError> {
        self.0.write_u8(9);
        Ok(())
    }
    fn serialize_unit_struct(self, name: &'static str) -> Result<(), FnvSerError> {
        self.0.write_u8(10);
        self.0.write_str(name);
        Ok(())
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        variant: &'static str,
    ) -> Result<(), FnvSerError> {
        self.0.write_u8(11);
        self.0.write_u32(variant_index);
        self.0.write_str(variant);
        Ok(())
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<(), FnvSerError> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<(), FnvSerError> {
        self.0.write_u8(12);
        self.0.write_u32(variant_index);
        self.0.write_str(variant);
        value.serialize(self)
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<FnvSeqSer<'a>, FnvSerError> {
        self.0.write_u8(13);
        if let Some(n) = len {
            self.0.write_u64(n as u64);
        }
        Ok(FnvSeqSer(self.0))
    }
    fn serialize_tuple(self, len: usize) -> Result<FnvSeqSer<'a>, FnvSerError> {
        self.0.write_u8(14);
        self.0.write_u64(len as u64);
        Ok(FnvSeqSer(self.0))
    }
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<FnvSeqSer<'a>, FnvSerError> {
        self.0.write_u8(15);
        self.0.write_u64(len as u64);
        Ok(FnvSeqSer(self.0))
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<FnvSeqSer<'a>, FnvSerError> {
        self.0.write_u8(16);
        self.0.write_u32(variant_index);
        self.0.write_str(variant);
        self.0.write_u64(len as u64);
        Ok(FnvSeqSer(self.0))
    }
    fn serialize_map(self, _len: Option<usize>) -> Result<FnvMapSer<'a>, FnvSerError> {
        self.0.write_u8(17);
        Ok(FnvMapSer {
            parent: self.0,
            len: 0,
            xor: 0,
            sum: 0,
            pending_key_hash: 0,
        })
    }
    fn serialize_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<FnvSeqSer<'a>, FnvSerError> {
        self.0.write_u8(18);
        self.0.write_u64(len as u64);
        Ok(FnvSeqSer(self.0))
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<FnvSeqSer<'a>, FnvSerError> {
        self.0.write_u8(19);
        self.0.write_u32(variant_index);
        self.0.write_str(variant);
        self.0.write_u64(len as u64);
        Ok(FnvSeqSer(self.0))
    }
}

#[inline]
pub(crate) fn canonical_fp<T: Serialize + ?Sized>(val: &T) -> u64 {
    let mut h = FnvHasher::new();
    if let Err(err) = val.serialize(FnvSerializer(&mut h)) {
        debug_assert!(false, "FnvSerializer failed in canonical_fp: {err}");
        h = FnvHasher::new();
        if let Ok(bytes) = serde_json::to_vec(val) {
            h.write_bytes(&bytes);
        }
    }
    h.finish()
}

pub(crate) struct FiniteFloatChecker;

impl serde::ser::SerializeSeq for FiniteFloatChecker {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), FnvSerError> {
        value.serialize(FiniteFloatChecker)
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl serde::ser::SerializeTuple for FiniteFloatChecker {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), FnvSerError> {
        value.serialize(FiniteFloatChecker)
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl serde::ser::SerializeTupleStruct for FiniteFloatChecker {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), FnvSerError> {
        value.serialize(FiniteFloatChecker)
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl serde::ser::SerializeTupleVariant for FiniteFloatChecker {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), FnvSerError> {
        value.serialize(FiniteFloatChecker)
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl serde::ser::SerializeMap for FiniteFloatChecker {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<(), FnvSerError> {
        key.serialize(FiniteFloatChecker)
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), FnvSerError> {
        value.serialize(FiniteFloatChecker)
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl serde::ser::SerializeStruct for FiniteFloatChecker {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        _key: &'static str,
        value: &T,
    ) -> Result<(), FnvSerError> {
        value.serialize(FiniteFloatChecker)
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl serde::ser::SerializeStructVariant for FiniteFloatChecker {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        _key: &'static str,
        value: &T,
    ) -> Result<(), FnvSerError> {
        value.serialize(FiniteFloatChecker)
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl serde::Serializer for FiniteFloatChecker {
    type Ok = ();
    type Error = FnvSerError;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    fn serialize_bool(self, _v: bool) -> Result<(), FnvSerError> {
        Ok(())
    }
    fn serialize_i8(self, _v: i8) -> Result<(), FnvSerError> {
        Ok(())
    }
    fn serialize_i16(self, _v: i16) -> Result<(), FnvSerError> {
        Ok(())
    }
    fn serialize_i32(self, _v: i32) -> Result<(), FnvSerError> {
        Ok(())
    }
    fn serialize_i64(self, _v: i64) -> Result<(), FnvSerError> {
        Ok(())
    }
    fn serialize_i128(self, _v: i128) -> Result<(), FnvSerError> {
        Ok(())
    }
    fn serialize_u8(self, _v: u8) -> Result<(), FnvSerError> {
        Ok(())
    }
    fn serialize_u16(self, _v: u16) -> Result<(), FnvSerError> {
        Ok(())
    }
    fn serialize_u32(self, _v: u32) -> Result<(), FnvSerError> {
        Ok(())
    }
    fn serialize_u64(self, _v: u64) -> Result<(), FnvSerError> {
        Ok(())
    }
    fn serialize_u128(self, _v: u128) -> Result<(), FnvSerError> {
        Ok(())
    }
    fn serialize_f32(self, v: f32) -> Result<(), FnvSerError> {
        if v.is_finite() {
            Ok(())
        } else {
            Err(FnvSerError(format!(
                "non-finite f32 ({v}) cannot be persisted to SQLite JSON"
            )))
        }
    }
    fn serialize_f64(self, v: f64) -> Result<(), FnvSerError> {
        if v.is_finite() {
            Ok(())
        } else {
            Err(FnvSerError(format!(
                "non-finite f64 ({v}) cannot be persisted to SQLite JSON"
            )))
        }
    }
    fn serialize_char(self, _v: char) -> Result<(), FnvSerError> {
        Ok(())
    }
    fn serialize_str(self, _v: &str) -> Result<(), FnvSerError> {
        Ok(())
    }
    fn serialize_bytes(self, _v: &[u8]) -> Result<(), FnvSerError> {
        Ok(())
    }
    fn serialize_none(self) -> Result<(), FnvSerError> {
        Ok(())
    }
    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<(), FnvSerError> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), FnvSerError> {
        Ok(())
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), FnvSerError> {
        Ok(())
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
    ) -> Result<(), FnvSerError> {
        Ok(())
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<(), FnvSerError> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        value: &T,
    ) -> Result<(), FnvSerError> {
        value.serialize(self)
    }
    fn serialize_seq(self, _len: Option<usize>) -> Result<Self, FnvSerError> {
        Ok(Self)
    }
    fn serialize_tuple(self, _len: usize) -> Result<Self, FnvSerError> {
        Ok(Self)
    }
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self, FnvSerError> {
        Ok(Self)
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self, FnvSerError> {
        Ok(Self)
    }
    fn serialize_map(self, _len: Option<usize>) -> Result<Self, FnvSerError> {
        Ok(Self)
    }
    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self, FnvSerError> {
        Ok(Self)
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self, FnvSerError> {
        Ok(Self)
    }
}

/// Validates that `val` contains no non-finite floats (`NaN` or `±Infinity`) before serializing to JSON,
/// preventing `serde_json` from silently emitting `null` for float fields (`#856`).
#[inline]
pub(crate) fn to_checked_json_string<T: Serialize + ?Sized>(val: &T) -> Result<String, String> {
    val.serialize(FiniteFloatChecker).map_err(|e| e.0)?;
    serde_json::to_string(val).map_err(|e| e.to_string())
}

#[inline]
pub(crate) fn fp_account(a: &AccountRecord) -> u64 {
    canonical_fp(a)
}

#[inline]
pub(crate) fn fp_character(c: &CharacterRecord) -> u64 {
    canonical_fp(c)
}

#[inline]
pub(crate) fn fp_item(i: &InventoryItemRecord) -> u64 {
    canonical_fp(i)
}

#[inline]
pub(crate) fn fp_market_order(o: &MarketOrderRecord) -> u64 {
    let mut h = FnvHasher::new();
    h.mix_word(o.order_id as u64);
    h.mix_word(o.owner_id as u64);
    h.mix_word(o.station_id as u64);
    h.mix_word(((o.region_id as u64) << 32) | (o.type_id as u64));
    h.mix_word(((o.vol_entered as u32 as u64) << 32) | (o.vol_remaining as u32 as u64));
    h.mix_word(
        ((o.min_volume as u32 as u64) << 2)
            | (u64::from(o.is_bid) << 1)
            | u64::from(o.is_npc),
    );
    h.mix_word(o.price.0 as u64);
    h.mix_word(o.escrow.0 as u64);
    h.finish()
}

#[inline]
pub(crate) fn fp_market_orders_digest_entry(oid: i64, order_fp: u64) -> u64 {
    let mut h = FnvHasher::new();
    h.write_i64(oid);
    h.write_u64(order_fp);
    h.finish()
}

#[inline]
pub(crate) fn fp_market_orders_digest(orders: &HashMap<i64, MarketOrderRecord>) -> u64 {
    let mut acc = 0u64;
    for (&oid, ord) in orders {
        acc ^= fp_market_orders_digest_entry(oid, fp_market_order(ord));
    }
    acc
}

#[inline]
pub(crate) fn fp_contract(c: &ContractRecord) -> u64 {
    canonical_fp(c)
}

#[inline]
pub(crate) fn fp_system_state(s: &SystemDynamicState) -> u64 {
    canonical_fp(s)
}

#[inline]
pub(crate) fn fp_structure(s: &UpwellStructureRecord) -> u64 {
    canonical_fp(s)
}

#[inline]
pub(crate) fn fp_wallet_journal_slice(entries: &[WalletJournalEntry]) -> u64 {
    canonical_fp(entries)
}

#[inline]
pub(crate) fn fp_mails_slice(entries: &[MailRecord]) -> u64 {
    canonical_fp(entries)
}

#[inline]
pub(crate) fn fp_skill_queue(q: &[SkillQueueEntry]) -> u64 {
    canonical_fp(q)
}

#[inline]
pub(crate) fn fp_sp6_lifecycle(st: &crate::Sp6PersistedState) -> u64 {
    canonical_fp(st)
}

#[inline]
pub(crate) fn fp_json<T: Serialize + ?Sized>(val: &T) -> u64 {
    canonical_fp(val)
}

#[inline]
pub fn fp_asteroid(a: &AsteroidRecord) -> u64 {
    canonical_fp(a)
}

#[derive(Debug, Clone, Default)]
pub struct TableFingerprints {
    pub next_entity_id: i64,
    pub wal_seq: u64,
    pub markets_seeded: bool,
    pub accounts: HashMap<String, u64>,
    pub characters: HashMap<i64, u64>,
    pub skills: HashMap<i64, HashMap<TypeId, u64>>,
    pub skill_queues: HashMap<i64, u64>,
    pub jump_clones: HashMap<i64, u64>,
    pub active_implants: HashMap<i64, u64>,
    pub items: HashMap<i64, u64>,
    pub saved_fittings: HashMap<i64, u64>,
    pub market_orders: std::sync::Arc<HashMap<i64, u64>>,
    pub market_orders_digest: u64,
    pub wallet_journal_len: usize,
    pub wallet_journal_hash: u64,
    pub contracts: HashMap<i64, u64>,
    pub industry_jobs: HashMap<i64, u64>,
    pub planetary_colonies: HashMap<(i64, i64), u64>,
    pub active_missions: HashMap<(i64, u32), u64>,
    pub mission_tally: HashMap<i64, u32>,
    pub standing_transactions: HashMap<i64, u64>,
    pub npe_progress: HashMap<i64, u64>,
    pub bookmarks: HashMap<i64, u64>,
    pub mails_len: usize,
    pub mails_hash: u64,
    pub fleets: HashMap<i64, u64>,
    pub corporations: HashMap<i64, u64>,
    pub alliances: HashMap<i64, u64>,
    pub wars: HashMap<i64, u64>,
    pub corp_projects: HashMap<i64, u64>,
    pub killmails: HashMap<i64, u64>,
    pub kill_rights: HashMap<i64, u64>,
    pub structures: HashMap<i64, u64>,
    pub system_states: HashMap<SolarSystemId, u64>,
    pub committed_idempotency_keys: HashSet<(i64, String)>,
    pub licensed_skins: HashMap<i64, u64>,
    pub applied_ship_skins: HashMap<i64, u32>,
    pub asteroids: HashMap<i64, u64>,
    pub character_last_jump_unix: HashMap<i64, u64>,
    pub character_cspa_charges: HashMap<i64, i64>,
    pub mail_timestamps: HashMap<i64, u64>,
    pub character_login_stats: HashMap<i64, u64>,
    pub item_custom_names: HashMap<i64, u64>,
    pub item_positions: HashMap<i64, u64>,
    pub structure_vulnerability_windows: HashMap<i64, (u64, u64)>,
    pub skill_history: HashMap<i64, u64>,
    pub character_doll_info: HashMap<i64, u64>,
    pub ledger: super::ledger_store::LedgerFingerprints,
    pub d6: super::notes_lp_sequence_store::D6Fingerprints,
    pub d6_social: super::social_store::D6SocialFingerprints,
    pub d6_corp_crimewatch: super::corp_crimewatch_store::D6CorpCrimewatchFingerprints,
    pub d6_batch4: super::industry_market_pve_store::D6Batch4Fingerprints,
    pub service_domains: super::service_domains_store::ServiceDomainsFingerprints,
}

/// Synchronizes only modified/inserted/deleted rows from `snap` into `tx` and updates `fp`,
/// returning a `WorldStateWalDelta` for optional legacy `.json.wal` mirroring.

pub fn compute_fingerprints(snap: &WorldStateSnapshot) -> TableFingerprints {
    let mut fp = TableFingerprints {
        next_entity_id: snap.next_entity_id,
        wal_seq: snap.wal_seq,
        markets_seeded: snap.markets_seeded,
        wallet_journal_len: snap.wallet_journal.len(),
        wallet_journal_hash: fp_wallet_journal_slice(&snap.wallet_journal),
        mails_len: snap.mails.len(),
        mails_hash: fp_mails_slice(&snap.mails),
        committed_idempotency_keys: snap.committed_idempotency_keys.clone(),
        mission_tally: snap.mission_tally.clone(),
        applied_ship_skins: snap.applied_ship_skins.clone(),
        ..Default::default()
    };
    for (k, v) in &snap.accounts {
        fp.accounts.insert(k.clone(), fp_account(v));
    }
    for (&k, v) in &snap.characters {
        fp.characters.insert(k, fp_character(v));
    }
    for (&cid, sk_map) in &snap.skills {
        let m = fp.skills.entry(cid).or_default();
        for (&tid, sk) in sk_map {
            m.insert(tid, canonical_fp(sk));
        }
    }
    for (&cid, q) in &snap.skill_queues {
        fp.skill_queues.insert(cid, fp_skill_queue(q));
    }
    for (&cid, jc) in &snap.jump_clones {
        fp.jump_clones.insert(cid, fp_json(jc));
    }
    for (&cid, imp) in &snap.active_implants {
        fp.active_implants.insert(cid, fp_json(imp));
    }
    for (&id, item) in &snap.items {
        fp.items.insert(id, fp_item(item));
    }
    for (&id, fit) in &snap.saved_fittings {
        fp.saved_fittings.insert(id, fp_json(fit));
    }
    let mut mkt_map = HashMap::with_capacity(snap.market_orders.len());
    for (&id, ord) in &snap.market_orders {
        mkt_map.insert(id, fp_market_order(ord));
    }
    fp.market_orders = std::sync::Arc::new(mkt_map);
    fp.market_orders_digest = fp_market_orders_digest(&snap.market_orders);
    for (&id, c) in &snap.contracts {
        fp.contracts.insert(id, fp_contract(c));
    }
    for (&id, j) in &snap.industry_jobs {
        fp.industry_jobs.insert(id, fp_json(j));
    }
    for (&k, col) in &snap.planetary_colonies {
        fp.planetary_colonies.insert(k, fp_json(col));
    }
    for (&k, m) in &snap.active_missions {
        fp.active_missions.insert(k, fp_json(m));
    }
    for (&cid, list) in &snap.standing_transactions {
        fp.standing_transactions.insert(cid, fp_json(list));
    }
    for (&cid, npe) in &snap.npe_progress {
        fp.npe_progress.insert(cid, fp_json(npe));
    }
    for (&bid, bm) in &snap.bookmarks {
        fp.bookmarks.insert(bid, fp_json(bm));
    }
    for (&fid, fl) in &snap.fleets {
        fp.fleets.insert(fid, fp_json(fl));
    }
    for (&cid, corp) in &snap.corporations {
        fp.corporations.insert(cid, fp_json(corp));
    }
    for (&aid, al) in &snap.alliances {
        fp.alliances.insert(aid, fp_json(al));
    }
    for (&wid, w) in &snap.wars {
        fp.wars.insert(wid, fp_json(w));
    }
    for (&pid, p) in &snap.corp_projects {
        fp.corp_projects.insert(pid, fp_json(p));
    }
    for (&kid, km) in &snap.killmails {
        fp.killmails.insert(kid, fp_json(km));
    }
    for (&krid, kr) in &snap.kill_rights {
        fp.kill_rights.insert(krid, fp_json(kr));
    }
    for (&stid, st) in &snap.structures {
        fp.structures.insert(stid, fp_structure(st));
    }
    for (&sys_id, st) in &snap.system_states {
        fp.system_states.insert(sys_id, fp_system_state(st));
    }
    for (&cid, skins) in &snap.licensed_skins {
        fp.licensed_skins.insert(cid, fp_json(skins));
    }
    for (&id, ast) in &snap.asteroids {
        fp.asteroids.insert(id, fp_asteroid(ast));
    }
    for (&cid, &v) in &snap.character_last_jump_unix {
        fp.character_last_jump_unix.insert(cid, v);
    }
    for (&cid, &cents) in &snap.character_cspa_charges {
        fp.character_cspa_charges.insert(cid, cents.0);
    }
    for (&mid, &v) in &snap.mail_timestamps {
        fp.mail_timestamps.insert(mid, v);
    }
    for (&cid, v) in &snap.character_login_stats {
        fp.character_login_stats.insert(cid, fp_json(v));
    }
    for (&iid, v) in &snap.item_custom_names {
        fp.item_custom_names.insert(iid, fp_json(v));
    }
    for (&iid, v) in &snap.item_positions {
        fp.item_positions.insert(iid, fp_json(v));
    }
    for (&sid, &v) in &snap.structure_vulnerability_windows {
        fp.structure_vulnerability_windows.insert(sid, v);
    }
    for (&cid, v) in &snap.skill_history {
        fp.skill_history.insert(cid, fp_json(v));
    }
    for (&cid, v) in &snap.character_doll_info {
        fp.character_doll_info.insert(cid, fp_json(v));
    }
    fp.ledger = super::ledger_store::compute_ledger_fingerprints(snap);
    fp.d6 = super::notes_lp_sequence_store::compute_d6_fingerprints(snap);
    fp.d6_social = super::social_store::compute_d6_social_fp(snap);
    fp.d6_corp_crimewatch =
        super::corp_crimewatch_store::compute_d6_corp_crimewatch_fingerprints(snap);
    fp.d6_batch4 = super::industry_market_pve_store::compute_d6_batch4_fingerprints(snap);
    fp.service_domains =
        super::service_domains_store::compute_service_domains_fingerprints(snap);
    fp
}

#[cfg(test)]
mod tests {
    use super::fp_sp6_lifecycle;
    use crate::{
        AbyssalPersistedSession, DisconnectedShipEntry, PersistedCombatHp,
        PersistedSpaceObjectMeta, SafeLogoffEntry, SelfDestructPersistedEntry, Sp6PersistedState,
    };
    use std::collections::HashMap;

    fn sample_sp6(keys: &[i64], cap: usize, scramble: bool) -> Sp6PersistedState {
        let mut st = Sp6PersistedState {
            safe_logoffs: HashMap::with_capacity(cap),
            disconnected_ships: HashMap::with_capacity(cap),
            self_destructs: HashMap::with_capacity(cap),
            abyssal_sessions: HashMap::with_capacity(cap),
            pvp_timers_until: HashMap::with_capacity(cap),
            npc_timers_until: HashMap::with_capacity(cap),
            weapon_timers_until: HashMap::with_capacity(cap),
            criminal_timers_until: HashMap::with_capacity(cap),
            last_downtime_day_idx: Some(19_850),
            ship_combat_hp: HashMap::with_capacity(cap),
            space_object_meta: HashMap::with_capacity(cap),
            missions_awaiting_downtime_respawn: vec![(90_000_001, 42), (90_000_002, 77)],
            mission_initial_npc_counts: HashMap::with_capacity(cap),
            mission_initial_npc_types: HashMap::with_capacity(cap),
            active_scanner_probes: HashMap::with_capacity(cap),
            server_runtime_json: Some("{}".into()),
        };
        if scramble {
            for dummy in 9_000..9_032 {
                st.pvp_timers_until.insert(dummy, 1);
                st.safe_logoffs.insert(dummy, SafeLogoffEntry::default());
            }
            for dummy in 9_000..9_032 {
                st.pvp_timers_until.remove(&dummy);
                st.safe_logoffs.remove(&dummy);
            }
        }
        for &k in keys {
            let u = k as u64;
            st.safe_logoffs.insert(
                k,
                SafeLogoffEntry {
                    char_id: k,
                    ship_id: k + 10_000,
                    solar_system_id: 30_000_142 + (k as u32),
                    started_unix: 1_700_000_000 + u,
                    complete_at_unix: 1_700_000_030 + u,
                    completed: false,
                },
            );
            st.disconnected_ships.insert(
                k,
                DisconnectedShipEntry {
                    char_id: k,
                    ship_id: k + 20_000,
                    ship_type_id: 587 + (k as u32),
                    solar_system_id: 30_002_187,
                    disconnected_at_unix: 1_700_001_000 + u,
                    remove_at_unix: 1_700_001_060 + u,
                    emergency_warp_dest: Some([10.0 + k as f64, 20.0, 30.0]),
                    last_position: [1.0, 2.0, 3.0 + k as f64],
                    removed_from_space: false,
                    pre_warp_position: Some([-5.0, 0.0, 5.0]),
                },
            );
            st.self_destructs.insert(
                k + 30_000,
                SelfDestructPersistedEntry {
                    ship_id: k + 30_000,
                    char_id: k,
                    solar_system_id: 30_002_537,
                    started_unix: 1_700_002_000 + u,
                    detonate_at_unix: 1_700_002_120 + u,
                },
            );
            st.abyssal_sessions.insert(
                k,
                AbyssalPersistedSession {
                    char_id: k,
                    ship_id: k + 40_000,
                    pocket_system_id: 32_000_001,
                    origin_system_id: 30_000_142,
                    entered_unix: 1_700_003_000 + u,
                    collapse_at_unix: 1_700_004_200 + u,
                },
            );
            st.pvp_timers_until.insert(k, 1_700_010_000 + u);
            st.npc_timers_until.insert(k, 1_700_020_000 + u);
            st.weapon_timers_until.insert(k, 1_700_030_000 + u);
            st.criminal_timers_until.insert(k, 1_700_040_000 + u);
            st.ship_combat_hp.insert(
                k + 50_000,
                PersistedCombatHp {
                    shield_hp: 250.0 + k as f64,
                    armor_hp: 500.0,
                    hull_hp: 650.0,
                },
            );
            st.space_object_meta.insert(
                k + 60_000,
                PersistedSpaceObjectMeta {
                    created_unix: Some(1_700_050_000 + u),
                    is_npc_wreck: false,
                    anchor_start_unix: Some(1_700_050_010 + u),
                    anchoring_delay_secs: 60.0,
                    anchor_completed: false,
                    bubble_radius_m: Some(20_000.0),
                    drone_ball_id: Some(k + 70_000),
                    planet_id: Some(40_000_001),
                },
            );
        }
        st
    }

    #[test]
    fn fp_sp6_lifecycle_order_and_capacity_independent() {
        let forward = sample_sp6(&[101, 202, 303, 404], 0, false);
        let reverse = sample_sp6(&[404, 303, 202, 101], 128, true);
        let perm = sample_sp6(&[303, 101, 404, 202], 16, false);
        assert_eq!(fp_sp6_lifecycle(&forward), fp_sp6_lifecycle(&reverse));
        assert_eq!(fp_sp6_lifecycle(&forward), fp_sp6_lifecycle(&perm));
    }

    #[test]
    fn fp_sp6_lifecycle_sensitive_to_every_persisted_field() {
        let base = sample_sp6(&[101, 202], 4, false);
        let base_fp = fp_sp6_lifecycle(&base);
        let check = |mutate: &dyn Fn(&mut Sp6PersistedState), label: &str| {
            let mut m = base.clone();
            mutate(&mut m);
            assert_ne!(fp_sp6_lifecycle(&m), base_fp, "fingerprint missed {label}");
        };

        check(&|s| s.safe_logoffs.get_mut(&101).unwrap().char_id += 1, "safe_logoffs.char_id");
        check(&|s| s.safe_logoffs.get_mut(&101).unwrap().ship_id += 1, "safe_logoffs.ship_id");
        check(&|s| s.safe_logoffs.get_mut(&101).unwrap().solar_system_id += 1, "safe_logoffs.solar_system_id");
        check(&|s| s.safe_logoffs.get_mut(&101).unwrap().started_unix += 1, "safe_logoffs.started_unix");
        check(&|s| s.safe_logoffs.get_mut(&101).unwrap().complete_at_unix += 1, "safe_logoffs.complete_at_unix");
        check(&|s| s.safe_logoffs.get_mut(&101).unwrap().completed = true, "safe_logoffs.completed");

        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().char_id += 1, "disconnected.char_id");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().ship_id += 1, "disconnected.ship_id");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().ship_type_id += 1, "disconnected.ship_type_id");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().solar_system_id += 1, "disconnected.solar_system_id");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().disconnected_at_unix += 1, "disconnected.disconnected_at_unix");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().remove_at_unix += 1, "disconnected.remove_at_unix");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().emergency_warp_dest = None, "disconnected.emergency_warp_dest=None");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().emergency_warp_dest = Some([0.0; 3]), "disconnected.emergency_warp_dest=zero");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().last_position[1] += 1.0, "disconnected.last_position");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().removed_from_space = true, "disconnected.removed_from_space");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().pre_warp_position = None, "disconnected.pre_warp_position=None");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().pre_warp_position = Some([9.0, 8.0, 7.0]), "disconnected.pre_warp_position=val");

        check(&|s| s.self_destructs.get_mut(&30_101).unwrap().ship_id += 1, "self_destructs.ship_id");
        check(&|s| s.self_destructs.get_mut(&30_101).unwrap().char_id += 1, "self_destructs.char_id");
        check(&|s| s.self_destructs.get_mut(&30_101).unwrap().solar_system_id += 1, "self_destructs.solar_system_id");
        check(&|s| s.self_destructs.get_mut(&30_101).unwrap().started_unix += 1, "self_destructs.started_unix");
        check(&|s| s.self_destructs.get_mut(&30_101).unwrap().detonate_at_unix += 1, "self_destructs.detonate_at_unix");

        check(&|s| s.abyssal_sessions.get_mut(&101).unwrap().char_id += 1, "abyssal.char_id");
        check(&|s| s.abyssal_sessions.get_mut(&101).unwrap().ship_id += 1, "abyssal.ship_id");
        check(&|s| s.abyssal_sessions.get_mut(&101).unwrap().pocket_system_id += 1, "abyssal.pocket_system_id");
        check(&|s| s.abyssal_sessions.get_mut(&101).unwrap().origin_system_id += 1, "abyssal.origin_system_id");
        check(&|s| s.abyssal_sessions.get_mut(&101).unwrap().entered_unix += 1, "abyssal.entered_unix");
        check(&|s| s.abyssal_sessions.get_mut(&101).unwrap().collapse_at_unix += 1, "abyssal.collapse_at_unix");

        check(&|s| *s.pvp_timers_until.get_mut(&101).unwrap() += 1, "pvp_timers_until");
        check(&|s| *s.npc_timers_until.get_mut(&101).unwrap() += 1, "npc_timers_until");
        check(&|s| *s.weapon_timers_until.get_mut(&101).unwrap() += 1, "weapon_timers_until");
        check(&|s| *s.criminal_timers_until.get_mut(&101).unwrap() += 1, "criminal_timers_until");
        check(&|s| s.last_downtime_day_idx = None, "last_downtime_day_idx=None");
        check(&|s| s.last_downtime_day_idx = Some(0), "last_downtime_day_idx=Some(0)");
        {
            let mut m_none = base.clone();
            m_none.last_downtime_day_idx = None;
            let mut m_zero = base.clone();
            m_zero.last_downtime_day_idx = Some(0);
            assert_ne!(fp_sp6_lifecycle(&m_none), fp_sp6_lifecycle(&m_zero));
        }

        check(&|s| s.ship_combat_hp.get_mut(&50_101).unwrap().shield_hp += 0.5, "ship_combat_hp.shield_hp");
        check(&|s| s.ship_combat_hp.get_mut(&50_101).unwrap().armor_hp += 0.5, "ship_combat_hp.armor_hp");
        check(&|s| s.ship_combat_hp.get_mut(&50_101).unwrap().hull_hp += 0.5, "ship_combat_hp.hull_hp");

        check(&|s| s.space_object_meta.get_mut(&60_101).unwrap().created_unix = None, "meta.created_unix");
        check(&|s| s.space_object_meta.get_mut(&60_101).unwrap().is_npc_wreck = true, "meta.is_npc_wreck");
        check(&|s| s.space_object_meta.get_mut(&60_101).unwrap().anchor_start_unix = None, "meta.anchor_start_unix");
        check(&|s| s.space_object_meta.get_mut(&60_101).unwrap().anchoring_delay_secs += 1.0, "meta.anchoring_delay_secs");
        check(&|s| s.space_object_meta.get_mut(&60_101).unwrap().anchor_completed = true, "meta.anchor_completed");
        check(&|s| s.space_object_meta.get_mut(&60_101).unwrap().bubble_radius_m = None, "meta.bubble_radius_m");
        check(&|s| s.space_object_meta.get_mut(&60_101).unwrap().drone_ball_id = None, "meta.drone_ball_id");
        check(&|s| s.space_object_meta.get_mut(&60_101).unwrap().planet_id = None, "meta.planet_id");

        check(&|s| s.missions_awaiting_downtime_respawn.reverse(), "missions_awaiting_downtime_respawn.order");
        check(&|s| s.missions_awaiting_downtime_respawn[0].1 += 1, "missions_awaiting_downtime_respawn.val");
    }

    fn build_unordered_collections_snapshot(order: &[i64], cap: usize, scramble: bool) -> crate::WorldStateSnapshot {
        use crate::service_domains::{CharacterSeasonState, PvpArenaMatchInstance};
        use crate::snapshot_ext::{CalendarEventRecord, ChatChannelAclRecord, MailingListRecord};
        use crate::types::{
            AllianceRecord, ChatChannelRecord, CorporationRecord, FleetMemberRecord, FleetRecord,
            FleetRole, FleetSquadRecord, FleetWingRecord, NpeProgressRecord, PlanetaryColonyRecord,
        };
        use std::collections::HashSet;

        let mut snap = crate::WorldStateSnapshot::default();
        if let Some(ch) = crate::persistence::WorldRepository::create_demo_snapshot()
            .characters
            .get(&90_000_001)
            .cloned()
        {
            snap.characters.insert(90_000_001, ch);
        }
        let make_i64_set = |vals: &[i64]| -> HashSet<i64> {
            let mut set = HashSet::with_capacity(cap);
            if scramble {
                for d in 70_000..70_040 {
                    set.insert(d);
                }
                for d in 70_000..70_040 {
                    set.remove(&d);
                }
            }
            for &idx in order {
                set.insert(vals[idx as usize % vals.len()]);
            }
            for &v in vals {
                set.insert(v);
            }
            set
        };
        let make_i64_map = |pairs: &[(i64, u64)]| -> HashMap<i64, u64> {
            let mut m = HashMap::with_capacity(cap);
            if scramble {
                for d in 80_000..80_040 {
                    m.insert(d, d as u64);
                }
                for d in 80_000..80_040 {
                    m.remove(&d);
                }
            }
            for &idx in order {
                let (k, v) = pairs[idx as usize % pairs.len()];
                m.insert(k, v);
            }
            for &(k, v) in pairs {
                m.insert(k, v);
            }
            m
        };

        let mut corp = CorporationRecord::new(98_000_001, "OrderIndep Corp", "ORD", 90_000_001);
        corp.members = make_i64_set(&[90_000_001, 90_000_002, 90_000_003, 90_000_004]);
        corp.member_roles = make_i64_map(&[(90_000_001, 1), (90_000_002, 128), (90_000_003, 512), (90_000_004, 1024)]);
        snap.corporations.insert(98_000_001, corp);

        snap.alliances.insert(
            99_000_001,
            AllianceRecord {
                alliance_id: 99_000_001,
                name: "OrderIndep Alliance".into(),
                short_name: "ORDA".into(),
                executor_corp_id: 98_000_001,
                member_corp_ids: make_i64_set(&[98_000_001, 98_000_002, 98_000_003, 98_000_004]),
            },
        );

        let mut fleet = FleetRecord {
            fleet_id: 640_001,
            boss_id: 90_000_001,
            members: vec![90_000_001, 90_000_002],
            wings: vec![1, 2],
            squads: vec![11, 22],
            free_move: true,
            is_registered: true,
            wing_details: HashMap::with_capacity(cap),
            squad_details: HashMap::with_capacity(cap),
            member_details: HashMap::with_capacity(cap),
            invites: make_i64_set(&[90_000_005, 90_000_006, 90_000_007]),
            motd: "Deterministic".into(),
        };
        for &idx in order {
            let wid = (idx % 2) + 1;
            fleet.wing_details.insert(
                wid,
                FleetWingRecord {
                    wing_id: wid,
                    name: format!("W{wid}"),
                    commander_id: None,
                    squad_ids: vec![wid * 11],
                },
            );
            fleet.squad_details.insert(
                wid * 11,
                FleetSquadRecord {
                    squad_id: wid * 11,
                    wing_id: wid,
                    name: format!("S{wid}"),
                    commander_id: None,
                },
            );
            let cid = 90_000_000 + wid;
            fleet.member_details.insert(
                cid,
                FleetMemberRecord {
                    character_id: cid,
                    role: FleetRole::SquadMember,
                    wing_id: Some(wid),
                    squad_id: Some(wid * 11),
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 0,
                    role_name: String::new(),
                    join_time_unix: 1_700_000_000,
                },
            );
        }
        snap.fleets.insert(640_001, fleet);

        let mut extracted = HashMap::with_capacity(cap);
        for &idx in order {
            let tid = 2393 + (idx as u32 % 4);
            extracted.insert(tid, 100 * u64::from(tid));
        }
        snap.planetary_colonies.insert(
            (90_000_001, 40_009_082),
            PlanetaryColonyRecord {
                planet_id: 40_009_082,
                owner_id: 90_000_001,
                solar_system_id: 30_000_142,
                command_center_level: 4,
                pins_count: 5,
                extracted_commodities: extracted,
                last_extracted_unix: 1_700_000_000,
            },
        );

        let mut claimed_goals = HashSet::with_capacity(cap);
        let mut goal_prog = HashMap::with_capacity(cap);
        for &idx in order {
            let gid = (idx as u32 % 4) + 1;
            claimed_goals.insert(gid);
            goal_prog.insert(gid, u64::from(gid) * 10);
        }
        snap.npe_progress.insert(
            90_000_001,
            NpeProgressRecord {
                character_id: 90_000_001,
                node_graph_id: 1,
                current_node_id: 2,
                completed: false,
                career_goal_progress: goal_prog,
                claimed_goals,
            },
        );

        let mut ml_members = HashMap::with_capacity(cap);
        let mut ml_access = HashMap::with_capacity(cap);
        for &idx in order {
            let id = 90_000_010 + (idx % 4);
            ml_members.insert(id, idx % 3);
            ml_access.insert(98_000_010 + (idx % 4), 1);
        }
        snap.mailing_lists.insert(
            501,
            MailingListRecord {
                list_id: 501,
                name: "FleetList".into(),
                owner_id: 90_000_001,
                default_access: 1,
                default_role: 0,
                cost: 0,
                members: ml_members,
                entity_access: ml_access,
                welcome_mail: None,
                mail_ids: make_i64_set(&[1001, 1002, 1003, 1004]),
            },
        );

        let mut cal_resp = HashMap::with_capacity(cap);
        for &idx in order {
            cal_resp.insert(90_000_010 + (idx % 4), (idx % 3) + 1);
        }
        snap.calendar_events.insert(
            601,
            CalendarEventRecord {
                event_id: 601,
                owner_id: 90_000_001,
                creator_id: 90_000_001,
                event_date_time: 1_700_050_000,
                duration: 60,
                importance: 1,
                title: "Op".into(),
                description: "Move".into(),
                scope: "Personal".into(),
                responses: cal_resp,
                is_deleted: false,
            },
        );

        snap.chat_channels.insert(
            "chan_det".into(),
            ChatChannelRecord {
                channel_id: "chan_det".into(),
                name: "Det".into(),
                motd: String::new(),
                owner_id: Some(90_000_001),
                members: make_i64_set(&[90_000_001, 90_000_002, 90_000_003]),
                is_system_local: false,
                solar_system_id: None,
                is_private: true,
                operators: make_i64_set(&[90_000_001, 90_000_003, 90_000_004]),
                password: None,
                is_delayed_local: false,
            },
        );
        let mut muted = HashMap::with_capacity(cap);
        for &idx in order {
            muted.insert(90_000_020 + (idx % 3), (1_800_000_000, "spam".into()));
        }
        snap.chat_channel_acl.insert(
            "chan_det".into(),
            ChatChannelAclRecord {
                allow_list: make_i64_set(&[90_000_001, 90_000_002, 90_000_005]),
                deny_list: make_i64_set(&[90_000_091, 90_000_092, 90_000_093]),
                muted_until: muted,
            },
        );

        snap.insurgency_runtime.active_state_notified = make_i64_set(&[90_000_001, 90_000_002, 90_000_003, 90_000_004]);
        snap.structure_docking_acl_denied
            .insert(1_000_000_000_001, make_i64_set(&[90_000_011, 90_000_012, 90_000_013]));

        snap.pvp_filament_state.active_matches.insert(
            1,
            PvpArenaMatchInstance {
                match_id: 1,
                event_id: 1,
                arena_solar_system_id: 32_000_001,
                team_a: vec![90_000_001],
                team_b: vec![90_000_002],
                origin_systems: HashMap::from([(90_000_001, 30_000_142), (90_000_002, 30_002_187)]),
                started_unix: 1_700_000_000,
                active: true,
                winner_char_id: None,
                exited_pilots: make_i64_set(&[90_000_002, 90_000_003, 90_000_004]),
                end_gate_id: 0,
            },
        );
        snap.pvp_filament_state
            .match_eliminated
            .insert(1, make_i64_set(&[90_000_002, 90_000_005, 90_000_006]));
        for &idx in order {
            snap.quasar_commerce
                .lp_asset_ops_completed
                .insert(format!("op_{}", idx % 4));
            snap.fleet_and_social_ops
                .agent_global_disabled_missions
                .insert(100 + (idx as u32 % 4));
        }
        snap.fleet_and_social_ops
            .fleet_join_requests
            .insert(640_001, make_i64_set(&[90_000_021, 90_000_022, 90_000_023]));
        snap.fleet_and_social_ops
            .fleet_watchlist
            .insert(640_001, make_i64_set(&[90_000_031, 90_000_032, 90_000_033]));
        snap.fleet_and_social_ops.mission_taxed_journal = make_i64_set(&[5001, 5002, 5003]);
        snap.fleet_and_social_ops
            .wh_local_witnesses
            .insert("J100001".into(), make_i64_set(&[90_000_041, 90_000_042, 90_000_043]));
        snap.cluster_and_runtime.resculpt_pending = make_i64_set(&[90_000_001, 90_000_002, 90_000_003]);
        for &idx in order {
            snap.cluster_and_runtime
                .omega_downgraded
                .insert(1_000 + (idx as u64 % 4));
        }
        snap.seasons_and_campaigns.season_states.insert(
            90_000_001,
            CharacterSeasonState {
                selected_season_id: 1,
                season_points: 100,
                challenges: HashMap::new(),
                claimed_challenges: HashMap::new(),
                completed_once_per_season_ids: make_i64_set(&[10, 20, 30, 40]),
            },
        );
        snap
    }

    #[test]
    fn multi_seed_unordered_collections_produce_identical_fingerprints_and_zero_write_sync() {
        use super::compute_fingerprints;
        use crate::sqlite_store::schema::apply_migrations;
        use crate::sqlite_store::sync::{replace_sqlite_from_snapshot, sync_dirty_to_sqlite};
        use rusqlite::Connection;

        let baseline = build_unordered_collections_snapshot(&[0, 1, 2, 3], 0, false);
        let base_fp = compute_fingerprints(&baseline);

        let mut conn = Connection::open_in_memory().unwrap();
        apply_migrations(&conn).unwrap();
        let mut sql_fp = replace_sqlite_from_snapshot(&mut conn, &baseline).unwrap();

        let permutations: &[(&[i64], usize, bool)] = &[
            (&[3, 2, 1, 0], 128, true),
            (&[2, 0, 3, 1], 16, false),
            (&[1, 3, 0, 2], 64, true),
            (&[0, 2, 1, 3], 4, true),
        ];
        for (round, &(perm, cap, scramble)) in permutations.iter().enumerate() {
            let candidate = build_unordered_collections_snapshot(perm, cap, scramble);
            let cand_fp = compute_fingerprints(&candidate);
            assert_eq!(cand_fp.corporations, base_fp.corporations, "round {round} corporations");
            assert_eq!(cand_fp.alliances, base_fp.alliances, "round {round} alliances");
            assert_eq!(cand_fp.fleets, base_fp.fleets, "round {round} fleets");
            assert_eq!(cand_fp.planetary_colonies, base_fp.planetary_colonies, "round {round} colonies");
            assert_eq!(cand_fp.npe_progress, base_fp.npe_progress, "round {round} npe_progress");
            assert_eq!(cand_fp.d6_social, base_fp.d6_social, "round {round} social");
            assert_eq!(cand_fp.d6_corp_crimewatch, base_fp.d6_corp_crimewatch, "round {round} corp_crimewatch");
            assert_eq!(cand_fp.service_domains, base_fp.service_domains, "round {round} service_domains");

            // Also roundtrip through JSON to allocate brand-new RandomState SipHash seeds on every HashMap/HashSet.
            let roundtripped: crate::WorldStateSnapshot =
                serde_json::from_str(&serde_json::to_string(&candidate).unwrap()).unwrap();
            let before = conn.total_changes();
            let tx = conn.transaction().unwrap();
            let delta = sync_dirty_to_sqlite(&tx, &roundtripped, &mut sql_fp, true).unwrap();
            tx.commit().unwrap();
            assert!(!delta.dirty, "round {round} resync must not mark delta dirty");
            assert!(delta.full_snapshot.is_none(), "round {round} resync must not emit full_snapshot");
            assert_eq!(conn.total_changes(), before, "round {round} resync must execute 0 SQL writes");
        }
    }
}
