//! On-demand domain loaders for V24.01 `.static` (SQLite3 and FSD binaryLoader), `.schema`, and
//! `.pickle` resources (`T-2`, `D-2`).
//!
//! Exposes typed domain tables through [`SdeRegistry`] for industry activities, modifier sources,
//! target filters, assembly lines, installation types, info bubbles, racial wreck tables,
//! asteroid belt counts, per-system asteroid types, overview brackets, certificates and ship
//! masteries, achievements, universe landmarks, empire selection/technologies, event/trigger
//! types, pod-guide entries, selection groups, dust icons, skill tags, and client dialogs.

#![forbid(unsafe_code)]

use super::*;
use crate::sqlite_loader_skins::parse_json_col;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndustryActivityInfo {
    #[serde(rename = "activityID")]
    pub activity_id: u32,
    #[serde(rename = "activityName")]
    pub activity_name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndustryModifierSourceEntry {
    #[serde(rename = "dogmaAttributeID")]
    pub dogma_attribute_id: AttributeId,
    #[serde(rename = "filterID", default)]
    pub filter_id: Option<u32>,
}

pub type IndustryModifierSources =
    HashMap<String, HashMap<String, Vec<IndustryModifierSourceEntry>>>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndustryTargetFilterInfo {
    pub filter_id: u32,
    pub name: String,
    pub category_ids: Vec<CategoryId>,
    pub group_ids: Vec<GroupId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndustryAssemblyLineInfo {
    pub id: u32,
    pub name: String,
    pub activity: u32,
    #[serde(default)]
    pub base_time_multiplier: f64,
    #[serde(default)]
    pub base_material_multiplier: f64,
    #[serde(default)]
    pub base_cost_multiplier: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BracketInfo {
    pub bracket_id: u32,
    pub name: String,
    pub texture_path: String,
    pub texture_path_empty: Option<String>,
    pub min_visible_distance: Option<f32>,
    pub max_visible_distance: Option<f32>,
    pub icon_offset: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificateEntry {
    pub certificate_id: u32,
    pub name_id: u32,
    pub description_id: u32,
    pub group_id: GroupId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificatesBundle {
    pub certificates: HashMap<u32, CertificateEntry>,
    pub masteries_by_ship_type: HashMap<TypeId, Vec<Vec<u32>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AchievementInfo {
    pub achievement_id: u32,
    pub is_client_achievement: bool,
    pub is_enabled: bool,
    pub name_id: u32,
    pub description_id: u32,
    pub notification_text_id: u32,
    pub target_value: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LandmarkInfo {
    pub landmark_id: u32,
    pub landmark_name_id: u32,
    pub description_id: u32,
    pub position: [f64; 3],
    pub landmark_type: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmpireSelectionInfo {
    pub race_id: u32,
    pub quote_id: u32,
    pub logo_path: String,
    pub named_logo_path: String,
    pub seal_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmpireTechnologiesInfo {
    pub race_id: u32,
    pub title_id: u32,
    pub order: u32,
    pub is_displayed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodGuideEntry {
    pub term_id: u32,
    pub term_title_id: u32,
    pub short_text_id: u32,
    pub long_text_id: u32,
    pub lore_text_id: u32,
    pub group_id: u32,
    pub image_size: u8,
    pub texture_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodGuideGroup {
    pub group_id: u32,
    pub group_name_id: u32,
    pub group_type: u8,
    pub belongs_to_group: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectionGroupInfo {
    pub selection_group_id: u32,
    pub room_id: u32,
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
    pub selection_group_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerTypeInfo {
    pub trigger_type_id: u32,
    pub allow_on_guard_object: bool,
    pub allow_on_guards: bool,
    pub trigger_type_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogEntry {
    pub key: String,
    pub dialog_id: u32,
    pub dialog_type: u8,
}

fn u32_le(buf: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("u32 slice"))
}

fn i32_le(buf: &[u8], pos: usize) -> i32 {
    i32::from_le_bytes(buf[pos..pos + 4].try_into().expect("i32 slice"))
}

fn u64_le(buf: &[u8], pos: usize) -> u64 {
    u64::from_le_bytes(buf[pos..pos + 8].try_into().expect("u64 slice"))
}

fn f32_le(buf: &[u8], pos: usize) -> f32 {
    f32::from_le_bytes(buf[pos..pos + 4].try_into().expect("f32 slice"))
}

fn f64_le(buf: &[u8], pos: usize) -> f64 {
    f64::from_le_bytes(buf[pos..pos + 8].try_into().expect("f64 slice"))
}

fn fsd_str(buf: &[u8], pos: usize) -> String {
    let len = u32_le(buf, pos) as usize;
    std::str::from_utf8(&buf[pos + 4..pos + 4 + len])
        .expect("FSD UTF-8 string")
        .to_string()
}

fn fsd_dict_entries(buf: &[u8], pos: usize, item_size: usize) -> Vec<(u32, usize)> {
    let body_start = pos + 4;
    let body_end = body_start + u32_le(buf, pos) as usize;
    let footer_size = u32_le(buf, body_end - 4) as usize;
    let footer = body_end - 4 - footer_size;
    let cnt = u32_le(buf, footer) as usize;
    (0..cnt)
        .map(|i| {
            let at = footer + 4 + i * item_size;
            (u32_le(buf, at), body_start + u32_le(buf, at + 4) as usize)
        })
        .collect()
}

impl SdeRegistry {
    fn open_sqlite_static(&self, res_path: &str) -> Result<Connection, SdeError> {
        let path = self.cdn_resource_path(res_path)?;
        // Strip Windows extended path prefix `\\?\` for SQLite URI compatibility (SUP-001)
        let path_str = path.display().to_string();
        let clean_path = if cfg!(windows) && path_str.starts_with(r"\\?\") {
            &path_str[4..]
        } else {
            &path_str
        };
        let uri = format!("file:{}?immutable=1", clean_path);
        Ok(Connection::open_with_flags(
            &uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )?)
    }

    fn read_fsd_static_bytes(&self, res_path: &str) -> Result<Vec<u8>, SdeError> {
        let path = self.cdn_resource_path(res_path)?;
        std::fs::read(&path).map_err(|_| SdeError::MissingDatabase(path.display().to_string()))
    }

    /// Loads `res:/staticdata/industry_activities.static` (`6` industry activities).
    pub fn load_industry_activities(&self) -> Result<HashMap<u32, IndustryActivityInfo>, SdeError> {
        let conn = self.open_sqlite_static("res:/staticdata/industry_activities.static")?;
        let mut stmt = conn.prepare("SELECT key, value FROM cache")?;
        let rows = stmt.query_map([], |r| {
            let k: String = r.get(0)?;
            let v: String = r.get(1)?;
            let id: u32 = k.parse().unwrap_or(0);
            let info: IndustryActivityInfo = parse_json_col(1, &v)?;
            Ok((id, info))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (id, info) = row?;
            out.insert(id, info);
        }
        Ok(out)
    }

    /// Loads `res:/staticdata/industry_activity_modifier_sources.static` (`220` modifier-source
    /// types: rigs, structure service modules, and implants).
    pub fn load_industry_activity_modifier_sources(
        &self,
    ) -> Result<HashMap<TypeId, IndustryModifierSources>, SdeError> {
        let conn =
            self.open_sqlite_static("res:/staticdata/industry_activity_modifier_sources.static")?;
        let mut stmt = conn.prepare("SELECT key, value FROM cache")?;
        let rows = stmt.query_map([], |r| {
            let k: String = r.get(0)?;
            let v: String = r.get(1)?;
            let tid: TypeId = k.parse().unwrap_or(0);
            let info: IndustryModifierSources = parse_json_col(1, &v)?;
            Ok((tid, info))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (tid, info) = row?;
            out.insert(tid, info);
        }
        Ok(out)
    }

    /// Loads `res:/staticdata/industry_activity_target_filters.static` (`18` target filters).
    pub fn load_industry_activity_target_filters(
        &self,
    ) -> Result<HashMap<u32, IndustryTargetFilterInfo>, SdeError> {
        #[derive(Deserialize)]
        struct RawFilter {
            name: String,
            #[serde(rename = "categoryIDs")]
            category_ids: Vec<CategoryId>,
            #[serde(rename = "groupIDs")]
            group_ids: Vec<GroupId>,
        }
        let conn =
            self.open_sqlite_static("res:/staticdata/industry_activity_target_filters.static")?;
        let mut stmt = conn.prepare("SELECT key, value FROM cache")?;
        let rows = stmt.query_map([], |r| {
            let k: String = r.get(0)?;
            let v: String = r.get(1)?;
            let fid: u32 = k.parse().unwrap_or(0);
            let rf: RawFilter = parse_json_col(1, &v)?;
            Ok((
                fid,
                IndustryTargetFilterInfo {
                    filter_id: fid,
                    name: rf.name,
                    category_ids: rf.category_ids,
                    group_ids: rf.group_ids,
                },
            ))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (fid, info) = row?;
            out.insert(fid, info);
        }
        Ok(out)
    }

    /// Loads `res:/staticdata/industry_assembly_lines.static` (`146` assembly lines).
    pub fn load_industry_assembly_lines(
        &self,
    ) -> Result<HashMap<u32, IndustryAssemblyLineInfo>, SdeError> {
        let conn = self.open_sqlite_static("res:/staticdata/industry_assembly_lines.static")?;
        let mut stmt = conn.prepare("SELECT key, value FROM cache")?;
        let rows = stmt.query_map([], |r| {
            let k: String = r.get(0)?;
            let v: String = r.get(1)?;
            let id: u32 = k.parse().unwrap_or(0);
            let info: IndustryAssemblyLineInfo = parse_json_col(1, &v)?;
            Ok((id, info))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (id, info) = row?;
            out.insert(id, info);
        }
        Ok(out)
    }

    /// Loads `res:/staticdata/industry_installation_types.static` (`102` installation types
    /// mapping `type_id -> Vec<assembly_line_id>`).
    pub fn load_industry_installation_types(&self) -> Result<HashMap<TypeId, Vec<u32>>, SdeError> {
        #[derive(Deserialize)]
        struct RawLine {
            #[serde(rename = "assemblyLine")]
            assembly_line: u32,
        }
        #[derive(Deserialize)]
        struct RawInst {
            assembly_lines: Vec<RawLine>,
        }
        let conn =
            self.open_sqlite_static("res:/staticdata/industry_installation_types.static")?;
        let mut stmt = conn.prepare("SELECT key, value FROM cache")?;
        let rows = stmt.query_map([], |r| {
            let k: String = r.get(0)?;
            let v: String = r.get(1)?;
            let tid: TypeId = k.parse().unwrap_or(0);
            let ri: RawInst = parse_json_col(1, &v)?;
            Ok((
                tid,
                ri.assembly_lines
                    .into_iter()
                    .map(|l| l.assembly_line)
                    .collect::<Vec<u32>>(),
            ))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (tid, lines) = row?;
            out.insert(tid, lines);
        }
        Ok(out)
    }

    /// Loads `res:/staticdata/infobubbles.static` (`5` top-level info-bubble tables).
    pub fn load_info_bubbles(&self) -> Result<HashMap<String, serde_json::Value>, SdeError> {
        let conn = self.open_sqlite_static("res:/staticdata/infobubbles.static")?;
        let mut stmt = conn.prepare("SELECT key, value FROM cache")?;
        let rows = stmt.query_map([], |r| {
            let k: String = r.get(0)?;
            let v: String = r.get(1)?;
            let val: serde_json::Value = parse_json_col(1, &v)?;
            Ok((k, val))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (k, val) = row?;
            out.insert(k, val);
        }
        Ok(out)
    }

    /// Loads `res:/staticdata/wrecks.static` (`294` ship/structure groups mapped to
    /// `race_id -> wreck_type_id`).
    pub fn load_wrecks_by_group_and_race(
        &self,
    ) -> Result<HashMap<GroupId, HashMap<u32, TypeId>>, SdeError> {
        let raw = self.read_fsd_static_bytes("res:/staticdata/wrecks.static")?;
        let sch_len = u32_le(&raw, 0) as usize;
        let entries = fsd_dict_entries(&raw, 4 + sch_len, 8);
        let mut out = HashMap::with_capacity(entries.len());
        for (gid, pos) in entries {
            let dict_pos = pos + 12 + u32_le(&raw, pos + 8) as usize;
            let inner = fsd_dict_entries(&raw, dict_pos, 8);
            let mut race_map = HashMap::with_capacity(inner.len());
            for (race_id, vpos) in inner {
                race_map.insert(race_id, u32_le(&raw, vpos));
            }
            out.insert(gid, race_map);
        }
        Ok(out)
    }

    /// Loads `res:/staticdata/asteroidcountbytypeidbeltid.static` (`40,928` asteroid belts mapped
    /// to `asteroid_type_id -> count`).
    pub fn load_asteroid_counts_by_belt(
        &self,
    ) -> Result<HashMap<i64, HashMap<TypeId, u32>>, SdeError> {
        let raw =
            self.read_fsd_static_bytes("res:/staticdata/asteroidcountbytypeidbeltid.static")?;
        let sch_len = u32_le(&raw, 0) as usize;
        let entries = fsd_dict_entries(&raw, 4 + sch_len, 12);
        let mut out = HashMap::with_capacity(entries.len());
        for (belt_id, pos) in entries {
            let inner = fsd_dict_entries(&raw, pos, 8);
            let mut type_counts = HashMap::with_capacity(inner.len());
            for (tid, vpos) in inner {
                type_counts.insert(tid, u32_le(&raw, vpos));
            }
            out.insert(i64::from(belt_id), type_counts);
        }
        Ok(out)
    }

    /// Loads `res:/staticdata/asteroidtypesbysolarsystemid.pickle` (`3,974` solar systems mapped
    /// to sorted `Vec<TypeId>`).
    pub fn load_asteroid_types_by_solar_system(
        &self,
    ) -> Result<HashMap<SolarSystemId, Vec<TypeId>>, SdeError> {
        let raw =
            self.read_fsd_static_bytes("res:/staticdata/asteroidtypesbysolarsystemid.pickle")?;
        #[derive(Clone)]
        enum PItem {
            Mark,
            Int(u32),
            Tuple(Vec<u32>),
            Dict,
        }
        let mut stack: Vec<PItem> = Vec::new();
        let mut out: HashMap<SolarSystemId, Vec<TypeId>> = HashMap::new();
        let mut pos = 0usize;
        while pos < raw.len() {
            let op = raw[pos];
            pos += 1;
            match op {
                0x80 | b'q' => pos += 1,
                b'r' => pos += 4,
                b'}' => stack.push(PItem::Dict),
                b'(' => stack.push(PItem::Mark),
                b'K' => {
                    stack.push(PItem::Int(u32::from(raw[pos])));
                    pos += 1;
                }
                b'M' => {
                    let v = u16::from_le_bytes([raw[pos], raw[pos + 1]]);
                    stack.push(PItem::Int(u32::from(v)));
                    pos += 2;
                }
                b'J' => {
                    let v = u32_le(&raw, pos);
                    stack.push(PItem::Int(v));
                    pos += 4;
                }
                0x85 => {
                    let Some(PItem::Int(a)) = stack.pop() else {
                        return Err(SdeError::InvalidSchema("TUPLE1".into()));
                    };
                    stack.push(PItem::Tuple(vec![a]));
                }
                0x86 => {
                    let (Some(PItem::Int(b)), Some(PItem::Int(a))) = (stack.pop(), stack.pop())
                    else {
                        return Err(SdeError::InvalidSchema("TUPLE2".into()));
                    };
                    stack.push(PItem::Tuple(vec![a, b]));
                }
                0x87 => {
                    let (Some(PItem::Int(c)), Some(PItem::Int(b)), Some(PItem::Int(a))) =
                        (stack.pop(), stack.pop(), stack.pop())
                    else {
                        return Err(SdeError::InvalidSchema("TUPLE3".into()));
                    };
                    stack.push(PItem::Tuple(vec![a, b, c]));
                }
                b't' => {
                    let mut items = Vec::new();
                    while let Some(it) = stack.pop() {
                        match it {
                            PItem::Mark => break,
                            PItem::Int(v) => items.push(v),
                            _ => return Err(SdeError::InvalidSchema("TUPLE item".into())),
                        }
                    }
                    items.reverse();
                    stack.push(PItem::Tuple(items));
                }
                b'u' => {
                    let mut pairs = Vec::new();
                    while let Some(val) = stack.pop() {
                        if matches!(val, PItem::Mark) {
                            break;
                        }
                        let Some(PItem::Int(k)) = stack.pop() else {
                            return Err(SdeError::InvalidSchema("SETITEMS key".into()));
                        };
                        let PItem::Tuple(mut tids) = val else {
                            return Err(SdeError::InvalidSchema("SETITEMS val".into()));
                        };
                        tids.sort_unstable();
                        pairs.push((k, tids));
                    }
                    for (k, tids) in pairs {
                        out.insert(k, tids);
                    }
                }
                b'.' => break,
                other => {
                    return Err(SdeError::InvalidSchema(format!(
                        "unsupported pickle opcode 0x{other:02x}"
                    )));
                }
            }
        }
        Ok(out)
    }

    /// Loads `res:/staticdata/bracketsbycategory.static` (`7` category bracket assignments).
    pub fn load_brackets_by_category(&self) -> Result<HashMap<CategoryId, u32>, SdeError> {
        let raw = self.read_fsd_static_bytes("res:/staticdata/bracketsbycategory.static")?;
        let sch_len = u32_le(&raw, 0) as usize;
        Ok(fsd_dict_entries(&raw, 4 + sch_len, 12)
            .into_iter()
            .map(|(k, vpos)| (k, u32_le(&raw, vpos)))
            .collect())
    }

    /// Loads `res:/staticdata/bracketsbygroup.static` (`547` group bracket assignments).
    pub fn load_brackets_by_group(&self) -> Result<HashMap<GroupId, u32>, SdeError> {
        let raw = self.read_fsd_static_bytes("res:/staticdata/bracketsbygroup.static")?;
        let sch_len = u32_le(&raw, 0) as usize;
        Ok(fsd_dict_entries(&raw, 4 + sch_len, 12)
            .into_iter()
            .map(|(k, vpos)| (k, u32_le(&raw, vpos)))
            .collect())
    }

    /// Loads `res:/staticdata/bracketsbytype.static` (`1,221` type bracket assignments).
    pub fn load_brackets_by_type(&self) -> Result<HashMap<TypeId, u32>, SdeError> {
        let raw = self.read_fsd_static_bytes("res:/staticdata/bracketsbytype.static")?;
        let sch_len = u32_le(&raw, 0) as usize;
        Ok(fsd_dict_entries(&raw, 4 + sch_len, 12)
            .into_iter()
            .map(|(k, vpos)| (k, u32_le(&raw, vpos)))
            .collect())
    }

    /// Loads `res:/staticdata/brackets.static` (`166` bracket definitions).
    pub fn load_brackets(&self) -> Result<HashMap<u32, BracketInfo>, SdeError> {
        let raw = self.read_fsd_static_bytes("res:/staticdata/brackets.static")?;
        let sch_len = u32_le(&raw, 0) as usize;
        let entries = fsd_dict_entries(&raw, 4 + sch_len, 12);
        let mut out = HashMap::with_capacity(entries.len());
        const ATTRS: [(&str, u64); 6] = [
            ("name", 0),
            ("texturePath", 0),
            ("texturePathEmpty", 1),
            ("minVisibleDistance", 2),
            ("maxVisibleDistance", 4),
            ("iconOffset", 8),
        ];
        for (bid, pos) in entries {
            let mask = u64_le(&raw, pos);
            let mut present = Vec::with_capacity(6);
            for (name, bit) in ATTRS {
                if bit == 0 || (mask & bit) != 0 {
                    present.push(name);
                }
            }
            let data_start = pos + 8 + 4 * present.len();
            let mut name = String::new();
            let mut texture_path = String::new();
            let mut texture_path_empty = None;
            let mut min_visible_distance = None;
            let mut max_visible_distance = None;
            let mut icon_offset = None;
            for (idx, &attr) in present.iter().enumerate() {
                let apos = data_start + u32_le(&raw, pos + 8 + idx * 4) as usize;
                match attr {
                    "name" => name = fsd_str(&raw, apos),
                    "texturePath" => texture_path = fsd_str(&raw, apos),
                    "texturePathEmpty" => texture_path_empty = Some(fsd_str(&raw, apos)),
                    "minVisibleDistance" => min_visible_distance = Some(f32_le(&raw, apos)),
                    "maxVisibleDistance" => max_visible_distance = Some(f32_le(&raw, apos)),
                    "iconOffset" => icon_offset = Some(i32_le(&raw, apos)),
                    _ => {}
                }
            }
            out.insert(
                bid,
                BracketInfo {
                    bracket_id: bid,
                    name,
                    texture_path,
                    texture_path_empty,
                    min_visible_distance,
                    max_visible_distance,
                    icon_offset,
                },
            );
        }
        Ok(out)
    }

    /// Loads `res:/staticdata/certificates.static` (`139` certificates and `477` ship mastery
    /// tables).
    pub fn load_certificates_and_masteries(&self) -> Result<CertificatesBundle, SdeError> {
        let raw = self.read_fsd_static_bytes("res:/staticdata/certificates.static")?;
        let sch_len = u32_le(&raw, 0) as usize;
        let body = 4 + sch_len;
        let c_off = u32_le(&raw, body + 8) as usize;
        let m_off = u32_le(&raw, body + 12) as usize;
        let c_pos = body + 16 + c_off;
        let m_pos = body + 16 + m_off;

        let cert_entries = fsd_dict_entries(&raw, c_pos, 12);
        let mut certificates = HashMap::with_capacity(cert_entries.len());
        for (cid, pos) in cert_entries {
            certificates.insert(
                cid,
                CertificateEntry {
                    certificate_id: cid,
                    name_id: u32_le(&raw, pos),
                    description_id: u32_le(&raw, pos + 4),
                    group_id: u32_le(&raw, pos + 8),
                },
            );
        }

        let ship_entries = fsd_dict_entries(&raw, m_pos, 8);
        let mut masteries_by_ship_type = HashMap::with_capacity(ship_entries.len());
        for (ship_tid, pos) in ship_entries {
            let lvl_entries = fsd_dict_entries(&raw, pos, 8);
            let mut levels = Vec::with_capacity(lvl_entries.len());
            for (_lvl, lpos) in lvl_entries {
                let n = u32_le(&raw, lpos) as usize;
                let certs: Vec<u32> = (0..n).map(|j| u32_le(&raw, lpos + 4 + j * 4)).collect();
                levels.push(certs);
            }
            masteries_by_ship_type.insert(ship_tid, levels);
        }

        Ok(CertificatesBundle {
            certificates,
            masteries_by_ship_type,
        })
    }

    /// Loads `res:/staticdata/achievements.static` (`68` achievements).
    pub fn load_achievements(&self) -> Result<HashMap<u32, AchievementInfo>, SdeError> {
        let raw = self.read_fsd_static_bytes("res:/staticdata/achievements.static")?;
        let sch_len = u32_le(&raw, 0) as usize;
        Ok(fsd_dict_entries(&raw, 4 + sch_len, 8)
            .into_iter()
            .map(|(k, pos)| {
                let name_id = if raw.len() >= pos + 34 { u32_le(&raw, pos + 30) } else { 0 };
                let description_id = if raw.len() >= pos + 38 { u32_le(&raw, pos + 34) } else { 0 };
                let notification_text_id = if raw.len() >= pos + 42 { u32_le(&raw, pos + 38) } else { 0 };
                let target_value = if raw.len() >= pos + 50 { u32_le(&raw, pos + 46) } else { 1 };
                (
                    k,
                    AchievementInfo {
                        achievement_id: u32_le(&raw, pos),
                        is_client_achievement: raw[pos + 4] != 0,
                        is_enabled: raw[pos + 5] != 0,
                        name_id,
                        description_id,
                        notification_text_id,
                        target_value,
                    },
                )
            })
            .collect())
    }

    /// Loads `res:/staticdata/landmarks.static` (`45` universe landmarks).
    pub fn load_landmarks(&self) -> Result<HashMap<u32, LandmarkInfo>, SdeError> {
        let raw = self.read_fsd_static_bytes("res:/staticdata/landmarks.static")?;
        let sch_len = u32_le(&raw, 0) as usize;
        Ok(fsd_dict_entries(&raw, 4 + sch_len, 8)
            .into_iter()
            .map(|(k, pos)| {
                (
                    k,
                    LandmarkInfo {
                        landmark_id: k,
                        landmark_name_id: u32_le(&raw, pos),
                        description_id: u32_le(&raw, pos + 4),
                        position: [
                            f64_le(&raw, pos + 8),
                            f64_le(&raw, pos + 16),
                            f64_le(&raw, pos + 24),
                        ],
                        landmark_type: raw[pos + 32],
                    },
                )
            })
            .collect())
    }

    /// Loads `res:/staticdata/empireselection.static` (`4` empire selection records keyed by
    /// `race_id`: `1, 2, 4, 8`).
    pub fn load_empire_selection(&self) -> Result<HashMap<u32, EmpireSelectionInfo>, SdeError> {
        let raw = self.read_fsd_static_bytes("res:/staticdata/empireselection.static")?;
        let sch_len = u32_le(&raw, 0) as usize;
        let body = 4 + sch_len;
        let body_end = body + 4 + u32_le(&raw, body) as usize;
        let footer_size = u32_le(&raw, body_end - 4) as usize;
        let footer = body_end - 4 - footer_size;
        let cnt = u32_le(&raw, footer) as usize;
        let mut out = HashMap::with_capacity(cnt);
        for i in 0..cnt {
            let at = footer + 4 + i * 5;
            let race_id = match raw[at] {
                1 => 4,
                2 => 1,
                3 => 8,
                4 => 2,
                other => u32::from(other),
            };
            let pos = body + 4 + u32_le(&raw, at + 1) as usize;
            let quote_id = u32_le(&raw, pos);
            let var_base = pos + 32;
            let logo_off = u32_le(&raw, pos + 20) as usize;
            let named_off = u32_le(&raw, pos + 24) as usize;
            let seal_off = u32_le(&raw, pos + 28) as usize;
            out.insert(
                race_id,
                EmpireSelectionInfo {
                    race_id,
                    quote_id,
                    logo_path: fsd_str(&raw, var_base + logo_off),
                    named_logo_path: fsd_str(&raw, var_base + named_off),
                    seal_path: fsd_str(&raw, var_base + seal_off),
                },
            );
        }
        Ok(out)
    }

    /// Loads `res:/staticdata/empiretechnologies.static` (`4` empire technology records).
    pub fn load_empire_technologies(
        &self,
    ) -> Result<HashMap<u32, EmpireTechnologiesInfo>, SdeError> {
        let raw = self.read_fsd_static_bytes("res:/staticdata/empiretechnologies.static")?;
        let sch_len = u32_le(&raw, 0) as usize;
        Ok(fsd_dict_entries(&raw, 4 + sch_len, 8)
            .into_iter()
            .map(|(race_id, pos)| {
                (
                    race_id,
                    EmpireTechnologiesInfo {
                        race_id,
                        title_id: u32_le(&raw, pos),
                        order: u32_le(&raw, pos + 4),
                        is_displayed: raw[pos + 8] != 0,
                    },
                )
            })
            .collect())
    }

    /// Loads `res:/staticdata/eventmessagetypes.static` (`8` event message types).
    pub fn load_event_message_types(&self) -> Result<HashMap<u32, String>, SdeError> {
        let raw = self.read_fsd_static_bytes("res:/staticdata/eventmessagetypes.static")?;
        let sch_len = u32_le(&raw, 0) as usize;
        Ok(fsd_dict_entries(&raw, 4 + sch_len, 8)
            .into_iter()
            .map(|(k, pos)| (k, fsd_str(&raw, pos + 12)))
            .collect())
    }

    /// Loads `res:/staticdata/eventtypes.static` (`72` event types).
    pub fn load_event_types(&self) -> Result<HashMap<u32, String>, SdeError> {
        let raw = self.read_fsd_static_bytes("res:/staticdata/eventtypes.static")?;
        let sch_len = u32_le(&raw, 0) as usize;
        Ok(fsd_dict_entries(&raw, 4 + sch_len, 8)
            .into_iter()
            .map(|(k, pos)| (k, fsd_str(&raw, pos + 12)))
            .collect())
    }

    /// Loads `res:/staticdata/podguide.static` (`44` pod-guide terms).
    pub fn load_pod_guide(&self) -> Result<HashMap<u32, PodGuideEntry>, SdeError> {
        let raw = self.read_fsd_static_bytes("res:/staticdata/podguide.static")?;
        let sch_len = u32_le(&raw, 0) as usize;
        Ok(fsd_dict_entries(&raw, 4 + sch_len, 8)
            .into_iter()
            .map(|(k, pos)| {
                (
                    k,
                    PodGuideEntry {
                        term_title_id: u32_le(&raw, pos),
                        short_text_id: u32_le(&raw, pos + 4),
                        long_text_id: u32_le(&raw, pos + 8),
                        lore_text_id: u32_le(&raw, pos + 12),
                        group_id: u32_le(&raw, pos + 16),
                        term_id: u32_le(&raw, pos + 20),
                        image_size: raw[pos + 24],
                        texture_path: fsd_str(&raw, pos + 37),
                    },
                )
            })
            .collect())
    }

    /// Loads `res:/staticdata/podguidegroups.static` (`14` pod-guide groups).
    pub fn load_pod_guide_groups(&self) -> Result<HashMap<u32, PodGuideGroup>, SdeError> {
        let raw = self.read_fsd_static_bytes("res:/staticdata/podguidegroups.static")?;
        let sch_len = u32_le(&raw, 0) as usize;
        Ok(fsd_dict_entries(&raw, 4 + sch_len, 8)
            .into_iter()
            .map(|(k, pos)| {
                (
                    k,
                    PodGuideGroup {
                        group_name_id: u32_le(&raw, pos),
                        group_id: u32_le(&raw, pos + 4),
                        group_type: raw[pos + 8],
                        belongs_to_group: u32_le(&raw, pos + 9),
                    },
                )
            })
            .collect())
    }

    /// Loads `res:/staticdata/selectiongroups.static` (`66` hangar/character selection groups).
    pub fn load_selection_groups(&self) -> Result<HashMap<u32, SelectionGroupInfo>, SdeError> {
        let raw = self.read_fsd_static_bytes("res:/staticdata/selectiongroups.static")?;
        let sch_len = u32_le(&raw, 0) as usize;
        Ok(fsd_dict_entries(&raw, 4 + sch_len, 8)
            .into_iter()
            .map(|(k, pos)| {
                (
                    k,
                    SelectionGroupInfo {
                        selection_group_id: k,
                        room_id: u32_le(&raw, pos),
                        yaw: f32_le(&raw, pos + 4),
                        pitch: f32_le(&raw, pos + 8),
                        roll: f32_le(&raw, pos + 12),
                        selection_group_name: fsd_str(&raw, pos + 28),
                    },
                )
            })
            .collect())
    }

    /// Loads `res:/staticdata/triggertypes.static` (`51` dungeon trigger types).
    pub fn load_trigger_types(&self) -> Result<HashMap<u32, TriggerTypeInfo>, SdeError> {
        let raw = self.read_fsd_static_bytes("res:/staticdata/triggertypes.static")?;
        let sch_len = u32_le(&raw, 0) as usize;
        Ok(fsd_dict_entries(&raw, 4 + sch_len, 8)
            .into_iter()
            .map(|(k, pos)| {
                (
                    k,
                    TriggerTypeInfo {
                        trigger_type_id: k,
                        allow_on_guard_object: raw[pos] != 0,
                        allow_on_guards: raw[pos + 1] != 0,
                        trigger_type_name: fsd_str(&raw, pos + 14),
                    },
                )
            })
            .collect())
    }

    /// Loads `res:/staticdata/dusticons.static` (`2,036` DUST icon path entries).
    pub fn load_dust_icons(&self) -> Result<HashMap<u32, String>, SdeError> {
        let raw = self.read_fsd_static_bytes("res:/staticdata/dusticons.static")?;
        let sch_len = u32_le(&raw, 0) as usize;
        Ok(fsd_dict_entries(&raw, 4 + sch_len, 12)
            .into_iter()
            .map(|(k, pos)| (k, fsd_str(&raw, pos + 12)))
            .collect())
    }

    /// Loads `res:/staticdata/skilltags.static` (`0` entries in V24.01).
    pub fn load_skill_tags(&self) -> Result<HashMap<u32, u32>, SdeError> {
        let raw = self.read_fsd_static_bytes("res:/staticdata/skilltags.static")?;
        let sch_len = u32_le(&raw, 0) as usize;
        Ok(fsd_dict_entries(&raw, 4 + sch_len, 8)
            .into_iter()
            .map(|(k, pos)| (k, u32_le(&raw, pos)))
            .collect())
    }

    /// Loads `res:/staticdata/dialogs.static` using its external schema
    /// `res:/staticdata/dialogs.schema` (`4,223` client dialog definitions).
    pub fn load_dialogs(&self) -> Result<HashMap<String, DialogEntry>, SdeError> {
        let _schema_bytes = self.read_fsd_static_bytes("res:/staticdata/dialogs.schema")?;
        let raw = self.read_fsd_static_bytes("res:/staticdata/dialogs.static")?;
        let end = raw.len();
        let footer_size = u32_le(&raw, end - 4) as usize;
        let footer = end - 4 - footer_size;
        let cnt = u32_le(&raw, footer) as usize;
        let mut out = HashMap::with_capacity(cnt);
        for i in 0..cnt {
            let item = footer + u32_le(&raw, footer + 4 + i * 4) as usize;
            let val_off = u32_le(&raw, item) as usize;
            let key = fsd_str(&raw, item + 16);
            let vpos = 4 + val_off;
            let dialog_id = u32_le(&raw, vpos);
            let dialog_type = raw[vpos + 4];
            out.insert(
                key.clone(),
                DialogEntry {
                    key,
                    dialog_id,
                    dialog_type,
                },
            );
        }
        Ok(out)
    }

    /// Directly opens and parses `res:/staticdata/fighterabilities.static` (`36` SQLite `cache`
    /// rows, `32,768` bytes) from the raw V24.01 client file.
    pub fn load_raw_fighter_abilities_static(
        &self,
    ) -> Result<(usize, HashMap<u32, FighterAbilityInfo>), SdeError> {
        let path = self.cdn_resource_path("res:/staticdata/fighterabilities.static")?;
        let byte_size = std::fs::metadata(&path)
            .map_err(|_| SdeError::MissingDatabase(path.display().to_string()))?
            .len() as usize;
        let uri = format!("file:{}?immutable=1", path.display());
        let conn = Connection::open_with_flags(
            &uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )?;
        let mut stmt = conn.prepare("SELECT key, value FROM cache")?;
        let rows = stmt.query_map([], |r| {
            let id = r
                .get::<_, i64>(0)
                .or_else(|_| r.get::<_, String>(0).map(|s| s.parse::<i64>().unwrap_or(0)))?
                as u32;
            let val_str: String = r.get(1)?;
            let val: serde_json::Value = crate::sqlite_loader_skins::parse_json_col(1, &val_str)?;
            let target_mode = val
                .get("targetMode")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let disallow_in_high_sec = val
                .get("disallowInHighSec")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let disallow_in_low_sec = val
                .get("disallowInLowSec")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let icon_id = val.get("iconID").and_then(|v| v.as_u64()).map(|v| v as u32);
            let display_name_id = val.get("displayNameID").and_then(|v| v.as_u64()).map(|v| v as u32);
            let ability_name_id = val
                .get("abilityNameID")
                .or_else(|| val.get("displayNameID"))
                .and_then(|v| v.as_u64())
                .map(|v| v as u32);
            let tooltip_text_id = val.get("tooltipTextID").and_then(|v| v.as_u64()).map(|v| v as u32);
            let turret_graphic_id = val.get("turretGraphicID").and_then(|v| v.as_u64()).map(|v| v as u32);
            Ok(FighterAbilityInfo {
                ability_id: id,
                target_mode,
                disallow_in_high_sec,
                disallow_in_low_sec,
                icon_id,
                display_name_id,
                ability_name_id,
                tooltip_text_id,
                turret_graphic_id,
                raw_json: val,
            })
        })?;
        let mut out = HashMap::new();
        for r in rows {
            let fa = r?;
            out.insert(fa.ability_id, fa);
        }
        Ok((byte_size, out))
    }
}
