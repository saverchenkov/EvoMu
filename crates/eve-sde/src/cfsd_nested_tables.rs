//! Recursive layout-driven loaders for V24.01 `.fsdbinary` (`cfsd`) resources whose records hold
//! nested lists, dicts and inline objects (`T-2`, `D-2`).
//!
//! A [`CfsdNestedLayout`] describes one resource's record object (byte offsets, encodings and
//! optional-field mask bits of every client attribute, recursively), and
//! [`SdeRegistry::load_cfsd_nested`] decodes every record of the client file registered by the
//! SDE importer into a JSON value keyed by the record key. All reads are bounds-checked: a
//! truncated or corrupt local file yields [`SdeError::InvalidSchema`], never a panic.

#![forbid(unsafe_code)]

use super::*;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Bounds-checked reader over a `cfsd` file (relative offsets are relative to byte 32).
pub(crate) struct CfsdBuf<'a>(pub(crate) &'a [u8]);

fn corrupt(what: &str, pos: usize) -> SdeError {
    SdeError::InvalidSchema(format!("cfsd {what} out of bounds at byte {pos}"))
}

impl CfsdBuf<'_> {
    pub(crate) fn bytes(&self, pos: usize, n: usize) -> Result<&[u8], SdeError> {
        self.0
            .get(pos..pos.checked_add(n).ok_or_else(|| corrupt("range", pos))?)
            .ok_or_else(|| corrupt("range", pos))
    }
    pub(crate) fn u8(&self, pos: usize) -> Result<u8, SdeError> {
        self.0.get(pos).copied().ok_or_else(|| corrupt("u8", pos))
    }
    pub(crate) fn u32(&self, pos: usize) -> Result<u32, SdeError> {
        Ok(u32::from_le_bytes(
            self.bytes(pos, 4)?.try_into().expect("4 bytes"),
        ))
    }
    pub(crate) fn u64(&self, pos: usize) -> Result<u64, SdeError> {
        Ok(u64::from_le_bytes(
            self.bytes(pos, 8)?.try_into().expect("8 bytes"),
        ))
    }
    /// Absolute position of a non-zero relative offset, with its preceding u64 length/count.
    pub(crate) fn counted(&self, rel: u64) -> Result<(usize, usize), SdeError> {
        let abs = usize::try_from(rel)
            .ok()
            .and_then(|r| r.checked_add(32))
            .ok_or_else(|| corrupt("offset", 0))?;
        let n = self.u64(abs.checked_sub(8).ok_or_else(|| corrupt("offset", abs))?)?;
        Ok((abs, usize::try_from(n).map_err(|_| corrupt("count", abs))?))
    }
    pub(crate) fn str_at(&self, rel: u64) -> Result<String, SdeError> {
        if rel == 0 {
            return Ok(String::new());
        }
        let (abs, len) = self.counted(rel)?;
        std::str::from_utf8(self.bytes(abs, len)?)
            .map(str::to_string)
            .map_err(|e| SdeError::InvalidSchema(format!("cfsd string: {e}")))
    }
    /// Positions of every entry of a `cfsd` hash dict whose bucket table starts at `abs`
    /// (bucket count stored at `abs - 8`), with entries `stride` bytes apart.
    pub(crate) fn dict_entries(&self, abs: usize, stride: usize) -> Result<Vec<usize>, SdeError> {
        let buckets = self.u64(abs.checked_sub(8).ok_or_else(|| corrupt("dict", abs))?)? as usize;
        let mut out = Vec::new();
        for b in 0..buckets {
            let off = self.u64(abs + b * 8)?;
            if off == 0 {
                continue;
            }
            let (at, n) = self.counted(off)?;
            if n > self.0.len() {
                return Err(corrupt("bucket count", at));
            }
            out.extend((0..n).map(|i| at + i * stride));
        }
        Ok(out)
    }
}

/// Encoding of a nested `cfsd` value.
#[derive(Debug, Clone, Copy)]
pub enum CfsdType {
    /// `u8` flag (decoded as `0`/`1`).
    Bool,
    I32,
    U32,
    U64,
    F32,
    F64,
    /// Inline fixed-length array of `f32` (e.g. RGBA colours).
    F32Arr(usize),
    /// Inline fixed-length array of `f64` (e.g. 3-vectors).
    F64Arr(usize),
    /// Tagged union: a `value_size`-byte value area followed by a `u64` variant tag; each known
    /// tag maps to the variant's encoding. An unknown tag is a decode error.
    Union(usize, &'static [(u64, CfsdType)]),
    /// UTF-8 string through a `u64` relative offset.
    Str,
    /// List through a `u64` relative offset; elements are inline, `elem_stride` bytes apart.
    List(&'static CfsdType, usize),
    /// Hash dict through `(u64 relative offset, u64 length)`; entries are inline, `entry_stride`
    /// bytes apart, with the key at entry offset 0 and the value at `value_offset`.
    Dict(&'static CfsdType, &'static CfsdType, usize, usize),
    /// Inline object.
    Obj(&'static CfsdObject),
}

/// One attribute of a [`CfsdObject`]: client name, byte offset, encoding and optional-mask bit
/// (`0` when the attribute is always present).
#[derive(Debug, Clone, Copy)]
pub struct CfsdAttr {
    pub name: &'static str,
    pub offset: usize,
    pub ty: CfsdType,
    pub bit: u32,
}

/// Inline object layout: total size, optional-field `u32` mask offset and attributes.
#[derive(Debug, Clone, Copy)]
pub struct CfsdObject {
    pub stride: usize,
    pub mask_offset: Option<usize>,
    pub attrs: &'static [CfsdAttr],
}

/// One nested `cfsd` resource: record key encoding (`U64` or `Str` at record offset 0), record
/// object, and whether the client record is the bare value of its single attribute.
#[derive(Debug, Clone, Copy)]
pub struct CfsdNestedLayout {
    pub res_path: &'static str,
    pub key: CfsdType,
    pub record: CfsdObject,
    pub bare: bool,
    /// The file holds one top-level object (at relative offset 0) instead of a dict of records;
    /// [`SdeRegistry::load_cfsd_nested`] then returns that object's attributes by name.
    pub single: bool,
}

fn decode(buf: &CfsdBuf<'_>, pos: usize, ty: &CfsdType) -> Result<Value, SdeError> {
    Ok(match *ty {
        CfsdType::Bool => Value::from(u8::from(buf.u8(pos)? != 0)),
        CfsdType::I32 => Value::from(buf.u32(pos)? as i32),
        CfsdType::U32 => Value::from(buf.u32(pos)?),
        CfsdType::F32Arr(n) => Value::Array(
            (0..n)
                .map(|i| {
                    Ok(Value::from(f64::from(f32::from_bits(
                        buf.u32(pos + 4 * i)?,
                    ))))
                })
                .collect::<Result<_, SdeError>>()?,
        ),
        CfsdType::F64Arr(n) => Value::Array(
            (0..n)
                .map(|i| Ok(Value::from(f64::from_bits(buf.u64(pos + 8 * i)?))))
                .collect::<Result<_, SdeError>>()?,
        ),
        CfsdType::Union(size, variants) => {
            let tag = buf.u64(pos + size)?;
            let (_, ty) = variants.iter().find(|(t, _)| *t == tag).ok_or_else(|| {
                SdeError::InvalidSchema(format!("cfsd union tag {tag} at byte {pos} not in layout"))
            })?;
            return decode(buf, pos, ty);
        }
        CfsdType::U64 => Value::from(buf.u64(pos)?),
        CfsdType::F32 => Value::from(f64::from(f32::from_bits(buf.u32(pos)?))),
        CfsdType::F64 => Value::from(f64::from_bits(buf.u64(pos)?)),
        CfsdType::Str => Value::from(buf.str_at(buf.u64(pos)?)?),
        CfsdType::List(elem, stride) => {
            let rel = buf.u64(pos)?;
            if rel == 0 {
                return Ok(Value::Array(Vec::new()));
            }
            let (at, n) = buf.counted(rel)?;
            if n > buf.0.len() {
                return Err(corrupt("list count", at));
            }
            Value::Array(
                (0..n)
                    .map(|i| decode(buf, at + i * stride, elem))
                    .collect::<Result<_, _>>()?,
            )
        }
        CfsdType::Dict(key, val, stride, val_off) => {
            let (rel, len) = (buf.u64(pos)?, buf.u64(pos + 8)? as usize);
            let mut map = Map::new();
            if rel != 0 {
                let abs = usize::try_from(rel)
                    .ok()
                    .and_then(|r| r.checked_add(32))
                    .ok_or_else(|| corrupt("dict offset", pos))?;
                for e in buf.dict_entries(abs, stride)? {
                    let k = match decode(buf, e, key)? {
                        Value::String(s) => s,
                        other => other.to_string(),
                    };
                    map.insert(k, decode(buf, e + val_off, val)?);
                }
            }
            if map.len() != len {
                return Err(SdeError::InvalidSchema(format!(
                    "cfsd dict at {pos}: {} of {len} entries",
                    map.len()
                )));
            }
            Value::Object(map)
        }
        CfsdType::Obj(obj) => Value::Object(decode_obj(buf, pos, obj)?),
    })
}

fn decode_obj(
    buf: &CfsdBuf<'_>,
    pos: usize,
    obj: &CfsdObject,
) -> Result<Map<String, Value>, SdeError> {
    let mask = match obj.mask_offset {
        Some(m) => buf.u32(pos + m)?,
        None => 0,
    };
    let mut map = Map::new();
    for a in obj.attrs {
        if a.bit != 0 && mask & a.bit == 0 {
            continue;
        }
        map.insert(a.name.to_string(), decode(buf, pos + a.offset, &a.ty)?);
    }
    Ok(map)
}

impl SdeRegistry {
    /// Layout of a nested `cfsd` resource decoded by [`Self::load_cfsd_nested`].
    pub fn cfsd_nested_layout(res_path: &str) -> Option<&'static CfsdNestedLayout> {
        super::cfsd_nested_layouts::CFSD_NESTED_LAYOUTS
            .iter()
            .chain(super::cfsd_nested_layouts_ops::CFSD_NESTED_LAYOUTS_OPS)
            .find(|l| l.res_path == res_path)
    }

    /// Decodes every record of the nested `cfsd` resource `res_path` from the client file
    /// registered by the SDE importer, keyed by the record key (integer keys in decimal). Fails
    /// if the file size differs from the importer's pinned size, any read is out of bounds, or
    /// the decoded record count differs from the file header.
    pub fn load_cfsd_nested(&self, res_path: &str) -> Result<BTreeMap<String, Value>, SdeError> {
        let lay = Self::cfsd_nested_layout(res_path).ok_or_else(|| {
            SdeError::InvalidSchema(format!("no nested cfsd layout for {res_path}"))
        })?;
        let path = self.cdn_resource_path(res_path)?;
        let raw = std::fs::read(&path)
            .map_err(|_| SdeError::MissingDatabase(path.display().to_string()))?;
        if raw.len() as u64 != self.resources[res_path].byte_size {
            return Err(SdeError::InvalidSchema(format!(
                "{res_path}: size differs from importer pin"
            )));
        }
        let buf = CfsdBuf(&raw);
        if lay.single {
            return Ok(decode_obj(&buf, 32, &lay.record)?.into_iter().collect());
        }
        let count = buf.u64(40)? as usize;
        let mut out = BTreeMap::new();
        for rec in buf.dict_entries(56, lay.record.stride)? {
            let key = match decode(&buf, rec, &lay.key)? {
                Value::String(s) => s,
                other => other.to_string(),
            };
            let obj = decode_obj(&buf, rec, &lay.record)?;
            let value = if lay.bare {
                obj.into_iter()
                    .next()
                    .map(|(_, v)| v)
                    .unwrap_or(Value::Null)
            } else {
                Value::Object(obj)
            };
            out.insert(key, value);
        }
        if out.len() != count {
            return Err(SdeError::InvalidSchema(format!(
                "{res_path}: decoded {} records, header says {count}",
                out.len()
            )));
        }
        Ok(out)
    }

    /// Reads the `itemTrader` component (`recipes: Vec<u32>`, `interactionRange: f64`) for
    /// `type_id` from `res:/staticdata/spacecomponentsbytype.fsdbinary` (`feat:wallet.item-trader`).
    pub fn item_trader_component_for_type(
        &self,
        type_id: u32,
    ) -> Result<Option<(Vec<u32>, f64)>, SdeError> {
        const RES_PATH: &str = "res:/staticdata/spacecomponentsbytype.fsdbinary";
        if !self
            .space_components_by_type
            .get(&type_id)
            .is_some_and(|c| c.iter().any(|s| s == "itemTrader"))
        {
            return Ok(None);
        }
        let path = self.cdn_resource_path(RES_PATH)?;
        let raw = std::fs::read(&path)
            .map_err(|_| SdeError::MissingDatabase(path.display().to_string()))?;
        let buf = CfsdBuf(&raw);
        for rec in buf.dict_entries(56, 1832)? {
            if buf.u32(rec)? == type_id {
                let mut recipes = Vec::new();
                let rel = buf.u64(rec + 464)?;
                if rel != 0 {
                    let (at, n) = buf.counted(rel)?;
                    for i in 0..n {
                        recipes.push(buf.u32(at + i * 4)?);
                    }
                }
                let range_m = f64::from(buf.u32(rec + 472)?).max(2_500.0);
                return Ok(Some((recipes, range_m)));
            }
        }
        Ok(None)
    }

    // parity: sde:res:/staticdata/contributionmethods.fsdbinary, sde:res:/staticdata/dungeonobjectives.fsdbinary, sde:res:/staticdata/surveys.fsdbinary
    // parity: sde:res:/staticdata/operations.fsdbinary, sde:res:/staticdata/operationtasks.fsdbinary
    /// Decodes `res:/staticdata/contributionmethods.fsdbinary`.
    pub fn contribution_methods(&self) -> Result<BTreeMap<String, Value>, SdeError> {
        self.load_cfsd_nested("res:/staticdata/contributionmethods.fsdbinary")
    }

    /// Decodes `res:/staticdata/dungeonobjectives.fsdbinary` (`T-2`).
    pub fn dungeon_objectives(&self) -> Result<BTreeMap<String, Value>, SdeError> {
        self.load_cfsd_nested("res:/staticdata/dungeonobjectives.fsdbinary")
    }

    /// Decodes `res:/staticdata/surveys.fsdbinary` (`T-2`).
    pub fn surveys(&self) -> Result<BTreeMap<String, Value>, SdeError> {
        self.load_cfsd_nested("res:/staticdata/surveys.fsdbinary")
    }

    /// Decodes `res:/staticdata/operations.fsdbinary`.
    pub fn operations(&self) -> Result<BTreeMap<String, Value>, SdeError> {
        self.load_cfsd_nested("res:/staticdata/operations.fsdbinary")
    }

    /// Decodes `res:/staticdata/operationtasks.fsdbinary`.
    pub fn operation_tasks(&self) -> Result<BTreeMap<String, Value>, SdeError> {
        self.load_cfsd_nested("res:/staticdata/operationtasks.fsdbinary")
    }

    /// Decodes `res:/staticdata/challengeconditions.fsdbinary` (`T-2`).
    pub fn challenge_conditions(&self) -> Result<BTreeMap<String, Value>, SdeError> {
        self.load_cfsd_nested("res:/staticdata/challengeconditions.fsdbinary")
    }

    /// Decodes `res:/staticdata/challengeevents.fsdbinary` (`T-2`).
    pub fn challenge_events(&self) -> Result<BTreeMap<String, Value>, SdeError> {
        self.load_cfsd_nested("res:/staticdata/challengeevents.fsdbinary")
    }

    /// Decodes `res:/staticdata/challenges.fsdbinary` (`T-2`).
    pub fn challenges(&self) -> Result<BTreeMap<String, Value>, SdeError> {
        self.load_cfsd_nested("res:/staticdata/challenges.fsdbinary")
    }

    /// Loads the 46 activity IDs from `res:/staticdata/newactivities.fsdbinary` (`stride = 88`).
    pub fn load_new_activity_ids(&self) -> Result<std::collections::BTreeSet<i64>, SdeError> {
        const RES_PATH: &str = "res:/staticdata/newactivities.fsdbinary";
        let path = self.cdn_resource_path(RES_PATH)?;
        let raw = std::fs::read(&path)
            .map_err(|_| SdeError::MissingDatabase(path.display().to_string()))?;
        if let Some(info) = self.resources.get(RES_PATH) {
            if raw.len() as u64 != info.byte_size {
                return Err(SdeError::InvalidSchema(format!(
                    "{RES_PATH}: size differs from importer pin"
                )));
            }
        }
        let buf = CfsdBuf(&raw);
        let count = buf.u64(40)? as usize;
        let mut out = std::collections::BTreeSet::new();
        for rec in buf.dict_entries(56, 88)? {
            out.insert(buf.u64(rec)? as i64);
        }
        if out.len() != count {
            return Err(SdeError::InvalidSchema(format!(
                "{RES_PATH}: decoded {} records, header says {count}",
                out.len()
            )));
        }
        Ok(out)
    }
}
