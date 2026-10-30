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
    pub skill_types: HashMap<TypeId, [u8; 5]>,
    pub recommended_for: Vec<TypeId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificatesBundle {
    pub certificates: HashMap<u32, CertificateEntry>,
    pub masteries_by_ship_type: HashMap<TypeId, Vec<Vec<u32>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AchievementConditionInfo {
    pub condition_id: u32,
    pub target_value: u32,
    pub statistic_event_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AchievementInfo {
    pub achievement_id: u32,
    pub is_client_achievement: bool,
    pub is_enabled: bool,
    pub name_id: u32,
    pub description_id: u32,
    pub notification_text_id: u32,
    pub name: String,
    pub description: String,
    pub notification_text: String,
    pub target_value: u32,
    pub conditions: Vec<AchievementConditionInfo>,
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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SecondarySunInfo {
    pub item_id: u32,
    pub type_id: TypeId,
    pub effect_beacon_type_id: TypeId,
    pub position: [f64; 3],
}

static SECONDARY_SUNS_CACHE: OnceLock<HashMap<SolarSystemId, SecondarySunInfo>> = OnceLock::new();

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

fn decode_pickle_unicode_escape(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut raw_buf: Vec<u8> = Vec::new();
    let flush_raw = |buf: &mut Vec<u8>, dst: &mut String| {
        if !buf.is_empty() {
            dst.push_str(&String::from_utf8_lossy(buf));
            buf.clear();
        }
    };
    let read_hex = |it: &mut std::iter::Copied<std::slice::Iter<'_, u8>>, n: usize| -> Option<u32> {
        let mut val = 0u32;
        for _ in 0..n {
            let d = (it.next()? as char).to_digit(16)?;
            val = (val << 4) | d;
        }
        Some(val)
    };
    let mut iter = bytes.iter().copied();
    for _ in 0..bytes.len() {
        let Some(b) = iter.next() else { break };
        if b != b'\\' {
            raw_buf.push(b);
            continue;
        }
        flush_raw(&mut raw_buf, &mut out);
        match iter.next() {
            Some(b'\\') => out.push('\\'),
            Some(b'\'') => out.push('\''),
            Some(b'"') => out.push('"'),
            Some(b'n') => out.push('\n'),
            Some(b'r') => out.push('\r'),
            Some(b't') => out.push('\t'),
            Some(b'x') => {
                if let Some(ch) = read_hex(&mut iter, 2).and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some(b'u') => {
                if let Some(ch) = read_hex(&mut iter, 4).and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some(b'U') => {
                if let Some(ch) = read_hex(&mut iter, 8).and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other as char);
            }
            None => out.push('\\'),
        }
    }
    flush_raw(&mut raw_buf, &mut out);
    out
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
            let mask = u64_le(&raw, pos + 12);
            let has_recommended = (mask & 1) != 0;
            let var_cnt = if has_recommended { 2 } else { 1 };
            let var_base = pos + 20 + 4 * var_cnt;
            let st_pos = var_base + u32_le(&raw, pos + 20) as usize;
            let st_entries = fsd_dict_entries(&raw, st_pos, 8);
            let mut skill_types = HashMap::with_capacity(st_entries.len());
            for (skill_type_id, sk_pos) in st_entries {
                skill_types.insert(
                    skill_type_id,
                    [
                        u32_le(&raw, sk_pos) as u8,
                        u32_le(&raw, sk_pos + 4) as u8,
                        u32_le(&raw, sk_pos + 8) as u8,
                        u32_le(&raw, sk_pos + 12) as u8,
                        u32_le(&raw, sk_pos + 16) as u8,
                    ],
                );
            }
            let recommended_for = if has_recommended {
                let rec_pos = var_base + u32_le(&raw, pos + 24) as usize;
                let rec_n = u32_le(&raw, rec_pos) as usize;
                (0..rec_n)
                    .map(|j| u32_le(&raw, rec_pos + 4 + 4 * j))
                    .collect()
            } else {
                Vec::new()
            };
            certificates.insert(
                cid,
                CertificateEntry {
                    certificate_id: cid,
                    name_id: u32_le(&raw, pos),
                    description_id: u32_le(&raw, pos + 4),
                    group_id: u32_le(&raw, pos + 8),
                    skill_types,
                    recommended_for,
                },
            );
        }

        let ship_entries = fsd_dict_entries(&raw, m_pos, 8);
        let mut masteries_by_ship_type = HashMap::with_capacity(ship_entries.len());
        for (ship_tid, pos) in ship_entries {
            let mut lvl_entries = fsd_dict_entries(&raw, pos, 8);
            lvl_entries.sort_by_key(|(lvl, _)| *lvl);
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

    /// Loads `res:/staticdata/achievements.static` (`68` achievements) with FSD optional-field
    /// bitfield decoding, `simpleConditions` sub-dictionaries, and localized English strings
    /// from `res:/localizationfsd/localization_fsd_en-us.pickle`.
    pub fn load_achievements(&self) -> Result<HashMap<u32, AchievementInfo>, SdeError> {
        let raw = self.read_fsd_static_bytes("res:/staticdata/achievements.static")?;
        let sch_len = u32_le(&raw, 0) as usize;
        let entries = fsd_dict_entries(&raw, 4 + sch_len, 8);

        let mut parsed_rows = Vec::with_capacity(entries.len());
        let mut needed_msg_ids = std::collections::HashSet::new();

        for (k, pos) in entries {
            if raw.len() < pos + 14 {
                continue;
            }
            let achievement_id = u32_le(&raw, pos);
            let is_client_achievement = raw[pos + 4] != 0;
            let is_enabled = raw[pos + 5] != 0;
            let bitfield = u32_le(&raw, pos + 6);
            let n_opt = bitfield.count_ones() as usize;
            let offsets_start = pos + 14;
            let var_base = offsets_start + n_opt * 4;
            if raw.len() < var_base {
                continue;
            }

            let mut opt_idx = 0usize;
            let mut read_opt_pos = |bit: u32| -> Option<usize> {
                if (bitfield & bit) == 0 {
                    return None;
                }
                let rel_off = u32_le(&raw, offsets_start + opt_idx * 4) as usize;
                opt_idx += 1;
                Some(var_base + rel_off)
            };

            let name_id = read_opt_pos(0x01)
                .filter(|&p| raw.len() >= p + 4)
                .map(|p| u32_le(&raw, p))
                .unwrap_or(0);
            let description_id = read_opt_pos(0x02)
                .filter(|&p| raw.len() >= p + 4)
                .map(|p| u32_le(&raw, p))
                .unwrap_or(0);
            let notification_text_id = read_opt_pos(0x04)
                .filter(|&p| raw.len() >= p + 4)
                .map(|p| u32_le(&raw, p))
                .unwrap_or(0);

            if name_id > 0 {
                needed_msg_ids.insert(name_id);
            }
            if description_id > 0 {
                needed_msg_ids.insert(description_id);
            }
            if notification_text_id > 0 {
                needed_msg_ids.insert(notification_text_id);
            }

            let mut conditions = Vec::new();
            if let Some(sc_pos) = read_opt_pos(0x08).filter(|&p| raw.len() >= p + 8) {
                let mut cond_entries = fsd_dict_entries(&raw, sc_pos, 8);
                cond_entries.sort_by_key(|(cid, _)| *cid);
                for (condition_id, cond_pos) in cond_entries {
                    if raw.len() >= cond_pos + 20 {
                        conditions.push(AchievementConditionInfo {
                            condition_id,
                            target_value: u32_le(&raw, cond_pos),
                            statistic_event_id: u32_le(&raw, cond_pos + 16),
                        });
                    }
                }
            }
            let target_value = conditions
                .iter()
                .map(|c| c.target_value)
                .sum::<u32>()
                .max(1);

            parsed_rows.push((
                k,
                achievement_id,
                is_client_achievement,
                is_enabled,
                name_id,
                description_id,
                notification_text_id,
                target_value,
                conditions,
            ));
        }

        let localized = self
            .load_localized_messages_en_us(&needed_msg_ids)
            .unwrap_or_default();

        Ok(parsed_rows
            .into_iter()
            .map(
                |(
                    k,
                    achievement_id,
                    is_client_achievement,
                    is_enabled,
                    name_id,
                    description_id,
                    notification_text_id,
                    target_value,
                    conditions,
                )| {
                    let name = localized
                        .get(&name_id)
                        .cloned()
                        .unwrap_or_else(|| format!("Achievement {achievement_id}"));
                    let description = localized.get(&description_id).cloned().unwrap_or_default();
                    let notification_text = localized
                        .get(&notification_text_id)
                        .cloned()
                        .unwrap_or_default();
                    (
                        k,
                        AchievementInfo {
                            achievement_id,
                            is_client_achievement,
                            is_enabled,
                            name_id,
                            description_id,
                            notification_text_id,
                            name,
                            description,
                            notification_text,
                            target_value,
                            conditions,
                        },
                    )
                },
            )
            .collect())
    }

    fn load_localized_messages_en_us(
        &self,
        needed_ids: &std::collections::HashSet<u32>,
    ) -> Result<HashMap<u32, String>, SdeError> {
        if needed_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let raw_loc =
            self.read_fsd_static_bytes("res:/localizationfsd/localization_fsd_en-us.pickle")?;
        let mut out = HashMap::with_capacity(needed_ids.len());
        let lines: Vec<&[u8]> = raw_loc.split(|&b| b == b'\n').collect();
        for pair in lines.windows(2) {
            let Some(id_bytes) = pair[0].strip_prefix(b"sI") else {
                continue;
            };
            let Ok(id_str) = std::str::from_utf8(id_bytes) else {
                continue;
            };
            let Ok(msg_id) = id_str.parse::<u32>() else {
                continue;
            };
            if !needed_ids.contains(&msg_id) {
                continue;
            }
            let Some(val_bytes) = pair[1].strip_prefix(b"(V") else {
                continue;
            };
            out.insert(msg_id, decode_pickle_unicode_escape(val_bytes));
            if out.len() == needed_ids.len() {
                break;
            }
        }
        Ok(out)
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

    /// Loads `secondarySun` records (`1,038` W-space system phenomena celestials and their
    /// `effectBeaconTypeID`) from `res:/staticdata/solarsystemcontent.static`.
    pub fn load_secondary_suns(&self) -> Result<HashMap<SolarSystemId, SecondarySunInfo>, SdeError> {
        let raw = self.read_fsd_static_bytes("res:/staticdata/solarsystemcontent.static")?;
        let sch_len = u32_le(&raw, 0) as usize;
        let entries = fsd_dict_entries(&raw, 4 + sch_len, 12);
        let mut out = HashMap::new();
        for (sys_id, pos) in entries {
            let fixed_end = pos + 98;
            if fixed_end + 8 > raw.len() {
                continue;
            }
            let mask = u64_le(&raw, fixed_end);
            if (mask & 4) == 0 {
                continue;
            }
            let n_present = 2 + (mask & 0x1ffff).count_ones() as usize;
            let sec_idx = 1 + (mask & 0x03).count_ones() as usize;
            let offsets_start = fixed_end + 8;
            let data_start = offsets_start + 4 * n_present;
            let spos = data_start + u32_le(&raw, offsets_start + 4 * sec_idx) as usize;
            if spos + 36 > raw.len() {
                continue;
            }
            out.insert(
                sys_id,
                SecondarySunInfo {
                    item_id: u32_le(&raw, spos),
                    type_id: u32_le(&raw, spos + 4),
                    effect_beacon_type_id: u32_le(&raw, spos + 8),
                    position: [
                        f64_le(&raw, spos + 12),
                        f64_le(&raw, spos + 20),
                        f64_le(&raw, spos + 28),
                    ],
                },
            );
        }
        Ok(out)
    }

    /// Returns the cached `SecondarySunInfo` (`type_id` and `effect_beacon_type_id`) for a W-space
    /// solar system from `res:/staticdata/solarsystemcontent.static`, if the system has a secondary sun.
    pub fn secondary_sun_for_system(&self, solar_system_id: SolarSystemId) -> Option<SecondarySunInfo> {
        let map = SECONDARY_SUNS_CACHE.get_or_init(|| self.load_secondary_suns().unwrap_or_default());
        map.get(&solar_system_id).copied()
    }

    /// Loads localized English descriptions from `res:/staticdata/marketgroups.fsdbinary`
    /// (`descriptionID`) resolved via `res:/localizationfsd/localization_fsd_en-us.pickle`.
    pub fn load_market_group_descriptions(&self) -> Result<HashMap<u32, String>, SdeError> {
        let raw = self.read_fsd_static_bytes("res:/staticdata/marketgroups.fsdbinary")?;
        if raw.len() < 56 {
            return Ok(HashMap::new());
        }
        let buckets = u64_le(&raw, 48) as usize;
        if 56 + buckets * 8 > raw.len() {
            return Ok(HashMap::new());
        }
        let mut desc_ids_by_mg = Vec::new();
        let mut needed_ids = std::collections::HashSet::new();
        for b_idx in 0..buckets {
            let b_off = u64_le(&raw, 56 + b_idx * 8) as usize;
            if b_off == 0 {
                continue;
            }
            let abs_off = 32 + b_off;
            if abs_off < 8 || abs_off > raw.len() {
                continue;
            }
            let n_in_bucket = u64_le(&raw, abs_off - 8) as usize;
            for i in 0..n_in_bucket {
                let rec = abs_off + i * 28;
                if rec + 28 > raw.len() {
                    break;
                }
                let mgid = u32_le(&raw, rec);
                let desc_id = u32_le(&raw, rec + 4);
                let mask = u32_le(&raw, rec + 24);
                if (mask & 1) != 0 && desc_id > 0 {
                    desc_ids_by_mg.push((mgid, desc_id));
                    needed_ids.insert(desc_id);
                }
            }
        }
        let localized = self
            .load_localized_messages_en_us(&needed_ids)
            .unwrap_or_default();
        let mut out = HashMap::with_capacity(desc_ids_by_mg.len());
        for (mgid, desc_id) in desc_ids_by_mg {
            if let Some(text) = localized.get(&desc_id) {
                if !text.is_empty() {
                    out.insert(mgid, text.clone());
                }
            }
        }
        Ok(out)
    }

    /// Returns the cached localized English description for a market group ID, if present.
    pub fn market_group_description(&self, market_group_id: u32) -> Option<String> {
        static MG_DESCS: std::sync::OnceLock<HashMap<u32, String>> = std::sync::OnceLock::new();
        let map = MG_DESCS.get_or_init(|| self.load_market_group_descriptions().unwrap_or_default());
        map.get(&market_group_id).cloned()
    }
}
