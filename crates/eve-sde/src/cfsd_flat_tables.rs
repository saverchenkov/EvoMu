//! Generic layout-driven loaders for flat V24.01 `.fsdbinary` (`cfsd`) catalogue resources
//! (`T-2`, `D-2`).
//!
//! Each resource below is a `cfsd` hash dict of fixed-stride records. A [`CfsdLayout`] names every
//! field the client schema stores in that record (the client attribute name, its byte offset and
//! encoding, and its bit in the record's optional-field mask), and [`SdeRegistry::load_cfsd_table`]
//! decodes every record of the client file into a [`CfsdRecord`] keyed by the record key, so
//! server logic can read any field by its client name. Nothing here is synthesized: all values are
//! read from the client file registered (path, size, md5) by the SDE importer in `sde_resources`.

#![forbid(unsafe_code)]

use super::*;
use std::collections::BTreeMap;

/// Encoding of one record field inside a `cfsd` fixed-stride record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CfsdFieldKind {
    /// `u8` boolean flag.
    Bool,
    /// Little-endian `i32`.
    I32,
    /// Little-endian `u32`.
    U32,
    /// Little-endian `u64`.
    U64,
    /// UTF-8 string through a `u64` relative offset (length stored before the bytes).
    Str,
    /// List of little-endian `u32` through a `u64` relative offset (count stored before).
    U32List,
}

/// One field of a [`CfsdLayout`]: `(client attribute name, kind, byte offset, optional-mask bit)`.
pub type CfsdField = (&'static str, CfsdFieldKind, usize, Option<u32>);

/// Fixed-stride record layout of one flat `cfsd` resource.
#[derive(Debug, Clone, Copy)]
pub struct CfsdLayout {
    pub res_path: &'static str,
    /// Width of the record key at offset 0 (`4` or `8` bytes).
    pub key_width: usize,
    pub stride: usize,
    /// Offset of the `u32` optional-field presence mask, when the schema has optional fields.
    pub mask_offset: Option<usize>,
    pub fields: &'static [CfsdField],
}

/// A decoded field value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CfsdValue {
    Int(i64),
    Str(String),
    IntList(Vec<i64>),
}

/// One decoded record: the record key plus every field present in the client record.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CfsdRecord {
    pub key: i64,
    pub fields: BTreeMap<String, CfsdValue>,
}

impl CfsdRecord {
    pub fn int(&self, name: &str) -> Option<i64> {
        match self.fields.get(name) {
            Some(CfsdValue::Int(v)) => Some(*v),
            _ => None,
        }
    }
    pub fn str(&self, name: &str) -> Option<&str> {
        match self.fields.get(name) {
            Some(CfsdValue::Str(s)) => Some(s.as_str()),
            _ => None,
        }
    }
    pub fn list(&self, name: &str) -> Option<&[i64]> {
        match self.fields.get(name) {
            Some(CfsdValue::IntList(v)) => Some(v.as_slice()),
            _ => None,
        }
    }
}

use CfsdFieldKind::{Bool, Str, U32List, I32, U32, U64};

const fn layout(
    res_path: &'static str,
    key_width: usize,
    stride: usize,
    mask_offset: Option<usize>,
    fields: &'static [CfsdField],
) -> CfsdLayout {
    CfsdLayout {
        res_path,
        key_width,
        stride,
        mask_offset,
        fields,
    }
}

/// Layouts of the flat catalogue resources decoded by [`SdeRegistry::load_cfsd_table`]. Offsets
/// and mask bits were established against the client's own per-resource `*Loader.so` output.
pub const CFSD_FLAT_LAYOUTS: &[CfsdLayout] = &[
    layout(
        "res:/staticdata/agenttypes.fsdbinary",
        8,
        16,
        None,
        &[("value", Str, 8, None)],
    ),
    layout(
        "res:/staticdata/billtypes.fsdbinary",
        8,
        32,
        Some(28),
        &[
            ("billTypeName", Str, 8, None),
            ("description", Str, 16, Some(1)),
            ("nameID", U32, 24, None),
        ],
    ),
    layout(
        "res:/staticdata/careerpaths.fsdbinary",
        8,
        24,
        None,
        &[
            ("internalName", Str, 8, None),
            ("descriptionID", U32, 16, None),
            ("nameID", U32, 20, None),
        ],
    ),
    layout(
        "res:/staticdata/corporationroles.fsdbinary",
        8,
        32,
        Some(24),
        &[
            ("roleName", Str, 8, None),
            ("descriptionID", U32, 16, Some(1)),
            ("nameID", U32, 20, None),
        ],
    ),
    layout(
        "res:/staticdata/corporationrolegroups.fsdbinary",
        8,
        48,
        None,
        &[
            ("appliesTo", Str, 8, None),
            ("appliesToGrantable", Str, 16, None),
            ("roleGroupName", Str, 24, None),
            ("roleMask", U64, 32, None),
            ("roleGroupNameID", U32, 40, None),
            ("isDivisional", Bool, 44, None),
            ("isLocational", Bool, 45, None),
        ],
    ),
    layout(
        "res:/staticdata/currencybuckets.fsdbinary",
        8,
        24,
        None,
        &[
            ("currency", Str, 8, None),
            ("capacity", U32, 16, None),
            ("redeemTypeID", U32, 20, None),
        ],
    ),
    layout(
        "res:/staticdata/dogmaattributecategories.fsdbinary",
        8,
        32,
        Some(24),
        &[("description", Str, 8, Some(1)), ("name", Str, 16, None)],
    ),
    layout(
        "res:/staticdata/essreservekeys.fsdbinary",
        4,
        16,
        None,
        &[
            ("keyTypeID", U32, 4, None),
            ("locationListID", U32, 8, None),
            ("totalPulses", U32, 12, None),
        ],
    ),
    layout(
        "res:/staticdata/petitionevents.fsdbinary",
        8,
        32,
        Some(28),
        &[
            ("eventText", Str, 16, None),
            ("eventTextID", U32, 24, Some(2)),
        ],
    ),
    layout(
        "res:/staticdata/recruitmentgroups.fsdbinary",
        4,
        16,
        Some(12),
        &[("descriptionID", U32, 4, Some(1)), ("nameID", U32, 8, None)],
    ),
    layout(
        "res:/staticdata/recruitmenttypes.fsdbinary",
        4,
        20,
        Some(16),
        &[
            ("descriptionID", U32, 4, Some(1)),
            ("nameID", U32, 8, None),
            ("recruitmentGroupID", U32, 12, None),
        ],
    ),
    layout(
        "res:/staticdata/schoolmap.fsdbinary",
        4,
        12,
        None,
        &[("schoolID", U32, 4, None), ("solarSystemID", U32, 8, None)],
    ),
    layout(
        "res:/staticdata/ship_cosmetic_slot_categories.fsdbinary",
        8,
        16,
        None,
        &[("name", Str, 8, None)],
    ),
    layout(
        "res:/staticdata/ship_cosmetic_slot_names.fsdbinary",
        8,
        16,
        None,
        &[("name", Str, 8, None)],
    ),
    layout(
        "res:/staticdata/ship_cosmetic_slots.fsdbinary",
        8,
        32,
        None,
        &[
            ("allowed_design_component_categories", U32List, 8, None),
            ("category", U32, 16, None),
            ("description", U32, 20, None),
            ("internal_name", U32, 24, None),
            ("name", U32, 28, None),
        ],
    ),
    layout(
        "res:/staticdata/ship_skin_design_component_categories.fsdbinary",
        8,
        16,
        None,
        &[("name", Str, 8, None)],
    ),
    layout(
        "res:/staticdata/ship_skin_design_component_rarities.fsdbinary",
        4,
        12,
        None,
        &[("name", U32, 4, None), ("rank", U32, 8, None)],
    ),
    layout(
        "res:/staticdata/ship_skin_design_components_post_process.fsdbinary",
        8,
        16,
        None,
        &[("value", Str, 8, None)],
    ),
    layout(
        "res:/staticdata/shipcosmeticlicenses.fsdbinary",
        4,
        20,
        None,
        &[
            ("cosmeticType", U32, 4, None),
            ("shipCosmeticLicenseTypeID", U32, 8, None),
            ("shipTypeID", U32, 12, None),
            ("slotGroup", U32, 16, None),
        ],
    ),
    layout(
        "res:/staticdata/tags.fsdbinary",
        8,
        40,
        Some(32),
        &[
            ("internalName", Str, 8, None),
            ("texturePath", Str, 16, Some(2)),
            ("titleLabelID", U32, 28, Some(4)),
        ],
    ),
    layout(
        "res:/staticdata/pvpfilamenteventdates.fsdbinary",
        8,
        40,
        None,
        &[
            ("endDate", Str, 8, None),
            ("region", Str, 16, None),
            ("startDate", Str, 24, None),
            ("matchTypeID", U32, 32, None),
            ("QA", Bool, 36, None),
        ],
    ),
    layout(
        "res:/staticdata/checkpoints.fsdbinary",
        8,
        40,
        Some(32),
        &[
            ("description", Str, 8, Some(1)),
            ("featureID", Str, 16, None),
            ("name", Str, 24, None),
        ],
    ),
    layout(
        "res:/staticdata/conversationagents.fsdbinary",
        8,
        72,
        Some(68),
        &[
            ("imagePath", Str, 8, None),
            ("loopVideoPath", Str, 32, Some(4)),
            ("preloadCachePath", Str, 48, Some(16)),
            ("scenePath", Str, 56, Some(32)),
            ("name", U32, 64, None),
        ],
    ),
    layout(
        "res:/staticdata/paints.fsdbinary",
        8,
        48,
        None,
        &[
            ("finish", Str, 8, None),
            ("hueCategory", Str, 16, None),
            ("materialName", Str, 24, None),
            ("rarity", Str, 32, None),
            ("paintNameID", U32, 40, None),
        ],
    ),
    layout(
        "res:/staticdata/achievementevents.fsdbinary",
        8,
        40,
        Some(32),
        &[
            ("description", Str, 8, None),
            ("inputParameters", U32List, 16, Some(1)),
            ("name", Str, 24, None),
        ],
    ),
];

impl SdeRegistry {
    /// Layout of a flat `cfsd` catalogue resource decoded by [`Self::load_cfsd_table`].
    pub fn cfsd_flat_layout(res_path: &str) -> Option<&'static CfsdLayout> {
        CFSD_FLAT_LAYOUTS.iter().find(|l| l.res_path == res_path)
    }

    /// Decodes every record of the flat `cfsd` resource `res_path` (one of
    /// [`CFSD_FLAT_LAYOUTS`]) from the client file registered by the SDE importer. Fails if the
    /// file size differs from the importer's pinned size or the walked record count differs
    /// from the file header's count.
    pub fn load_cfsd_table(&self, res_path: &str) -> Result<BTreeMap<i64, CfsdRecord>, SdeError> {
        let lay = Self::cfsd_flat_layout(res_path)
            .ok_or_else(|| SdeError::InvalidSchema(format!("no cfsd layout for {res_path}")))?;
        let path = self.cdn_resource_path(res_path)?;
        let raw = std::fs::read(&path)
            .map_err(|_| SdeError::MissingDatabase(path.display().to_string()))?;
        let info = &self.resources[res_path];
        if raw.len() as u64 != info.byte_size {
            return Err(SdeError::InvalidSchema(format!(
                "{res_path}: {} bytes, importer pinned {}",
                raw.len(),
                info.byte_size
            )));
        }
        let buf = super::cfsd_nested_tables::CfsdBuf(&raw);
        let count = buf.u64(40)? as usize;
        let mut out = BTreeMap::new();
        for rec in buf.dict_entries(56, lay.stride)? {
            let key = if lay.key_width == 8 {
                buf.u64(rec)? as i64
            } else {
                i64::from(buf.u32(rec)?)
            };
            let mask = match lay.mask_offset {
                Some(m) => buf.u32(rec + m)?,
                None => 0,
            };
            let mut fields = BTreeMap::new();
            for &(name, kind, off, bit) in lay.fields {
                if bit.is_some_and(|b| mask & b == 0) {
                    continue;
                }
                let p = rec + off;
                let v = match kind {
                    Bool => CfsdValue::Int(i64::from(buf.u8(p)? != 0)),
                    I32 => CfsdValue::Int(i64::from(buf.u32(p)? as i32)),
                    U32 => CfsdValue::Int(i64::from(buf.u32(p)?)),
                    U64 => CfsdValue::Int(buf.u64(p)? as i64),
                    Str => CfsdValue::Str(buf.str_at(buf.u64(p)?)?),
                    U32List => {
                        let rel = buf.u64(p)?;
                        let mut list = Vec::new();
                        if rel != 0 {
                            let (at, n) = buf.counted(rel)?;
                            for j in 0..n.min(raw.len()) {
                                list.push(i64::from(buf.u32(at + j * 4)?));
                            }
                        }
                        CfsdValue::IntList(list)
                    }
                };
                fields.insert(name.to_string(), v);
            }
            out.insert(key, CfsdRecord { key, fields });
        }
        if out.len() != count {
            return Err(SdeError::InvalidSchema(format!(
                "{res_path}: decoded {} records, header says {count}",
                out.len()
            )));
        }
        Ok(out)
    }
}
