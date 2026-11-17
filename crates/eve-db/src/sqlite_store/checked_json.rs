//! Deterministic FNV-1a canonical hashing and checked JSON serialization (rejecting non-finite floats).

use serde::Serialize;

/// Fast zero-allocation FNV-1a 64-bit hasher for canonical structural fingerprints.
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
