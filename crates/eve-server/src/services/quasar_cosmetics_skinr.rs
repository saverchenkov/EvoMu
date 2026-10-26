//! `quasar_cosmetics_skinr` — State-backed V24.01 Quasar handlers for 3rd-party SKINR component
//! licenses (`finite`, `infinite`, `item` consumption, and `GrantedNotice`) and character title
//! definitions & semantic version checkpoints (`GetLatestRequest`, `CheckVersionNotice`).
//!
//! Static catalogue rules (components, categories, rarities, associated item types, point values,
//! tier thresholds, and ship cosmetic slot configurations) are loaded directly from the SDE
//! `.fsdbinary` resources via [`SdeRegistry`]. Mutable state persists in
//! `WorldStateSnapshot::quasar_state_blobs["quasar_cosmetics_skinr"]`.

#![forbid(unsafe_code)]

use super::quasar_social_chat::ctx_queue_notice;
use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN,
    STATUS_NOT_FOUND, STATUS_PRECONDITION_FAILED,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicQuasarMessage, DynamicValue, GoogleAny, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

pub const SKINR_BLOB_KEY: &str = "quasar_cosmetics_skinr";
pub const CATEGORY_SHIP: u32 = 6;
/// SDE component category IDs (`ship_skin_design_component_categories.fsdbinary`).
pub const COMP_CAT_MATERIAL: u32 = 1;
pub const COMP_CAT_PATTERN: u32 = 2;
pub const COMP_CAT_METALLIC: u32 = 3;

pub type HandlerResult = Result<GoogleAny, (u32, String)>;

/// One SKINR design component entry loaded from `ship_skin_design_components.fsdbinary`.
#[derive(Debug, Clone)]
pub struct SdeSkinComponent {
    pub component_id: u32,
    /// `1` = Material (coating), `2` = Pattern (pattern), `3` = Metallic (coating).
    pub category: u32,
    pub rarity: u32,
    pub disallowed_slots: Vec<u32>,
    /// `(item_type_id, license_uses_granted)` where `-1` means infinite uses.
    pub associated_types: Vec<(u32, i32)>,
    pub binder_type_id: u32,
    pub binder_count: u32,
}

impl SdeSkinComponent {
    pub fn is_pattern(&self) -> bool {
        self.category == COMP_CAT_PATTERN
    }
}

/// Static SKINR catalogue loaded from SDE `.fsdbinary` tables.
#[derive(Debug, Clone, Default)]
pub struct SkinrStaticCatalog {
    pub components: BTreeMap<u32, SdeSkinComponent>,
    /// Maps inventory item `type_id -> (component_id, license_uses_granted)`.
    pub item_type_to_component: HashMap<u32, (u32, i32)>,
    /// Maps `(category, rarity) -> point_value` from `ship_skin_design_component_point_values.fsdbinary`.
    pub point_values: HashMap<(u32, u32), u32>,
    /// Maps `tier_level (1..=19) -> min_points` (representative hull threshold table from SDE).
    pub tier_thresholds: BTreeMap<u32, u32>,
    /// Maps `slot_id (1..=8) -> allowed_design_component_categories` from `ship_cosmetic_slots.fsdbinary`.
    pub slot_allowed_categories: BTreeMap<u32, Vec<u32>>,
    /// Default allowed slot IDs (`1..=8`) from `ship_cosmetic_slot_configurations.fsdbinary`.
    pub default_ship_slots: Vec<u32>,
    /// Ship-specific slot configuration overrides (`ship_type_id -> allowed_slot_ids`).
    pub ship_slot_overrides: HashMap<u32, Vec<u32>>,
}

impl SkinrStaticCatalog {
    pub fn load(sde: &SdeRegistry) -> Self {
        let mut cat = Self::default();

        if let Ok(raw_comps) = sde.load_cfsd_nested("res:/staticdata/ship_skin_design_components.fsdbinary") {
            for (k, v) in raw_comps {
                let Ok(comp_id) = k.parse::<u32>() else { continue };
                let category = v.get("category").and_then(Value::as_u64).unwrap_or(COMP_CAT_MATERIAL as u64) as u32;
                let rarity = v.get("rarity").and_then(Value::as_u64).unwrap_or(1) as u32;
                let disallowed_slots = v
                    .get("disallowed_slots")
                    .and_then(Value::as_array)
                    .map(|arr| arr.iter().filter_map(|x| x.as_u64().map(|u| u as u32)).collect())
                    .unwrap_or_default();
                let mut associated_types = Vec::new();
                if let Some(arr) = v.get("associated_type_ids").and_then(Value::as_array) {
                    for entry in arr {
                        let tid = entry.get("type_id").and_then(Value::as_u64).unwrap_or(0) as u32;
                        let uses = entry.get("license_uses_granted").and_then(Value::as_i64).unwrap_or(1) as i32;
                        if tid > 0 {
                            associated_types.push((tid, uses));
                            cat.item_type_to_component.insert(tid, (comp_id, uses));
                        }
                    }
                }
                let (binder_type_id, binder_count) = v
                    .get("sequence_binder")
                    .map(|b| {
                        (
                            b.get("item_type_id").and_then(Value::as_u64).unwrap_or(0) as u32,
                            b.get("count").and_then(Value::as_u64).unwrap_or(0) as u32,
                        )
                    })
                    .unwrap_or((0, 0));
                cat.components.insert(
                    comp_id,
                    SdeSkinComponent {
                        component_id: comp_id,
                        category,
                        rarity,
                        disallowed_slots,
                        associated_types,
                        binder_type_id,
                        binder_count,
                    },
                );
            }
        }

        if let Ok(raw_pts) = sde.load_cfsd_nested("res:/staticdata/ship_skin_design_component_point_values.fsdbinary") {
            for (cat_str, map_val) in raw_pts {
                let Ok(cat_id) = cat_str.parse::<u32>() else { continue };
                if let Some(obj) = map_val.as_object() {
                    for (rar_str, pt_val) in obj {
                        if let (Ok(rar_id), Some(pts)) = (rar_str.parse::<u32>(), pt_val.as_u64()) {
                            cat.point_values.insert((cat_id, rar_id), pts as u32);
                        }
                    }
                }
            }
        }
        if cat.point_values.is_empty() {
            for (c, r, p) in [
                (1, 1, 25), (1, 2, 50), (1, 3, 100), (1, 4, 200), (1, 5, 400), (1, 6, 600),
                (2, 1, 75), (2, 2, 150), (2, 3, 250), (2, 4, 350), (2, 5, 500), (2, 6, 750),
                (3, 1, 25), (3, 2, 50), (3, 3, 100), (3, 4, 200), (3, 5, 400), (3, 6, 600),
            ] {
                cat.point_values.insert((c, r), p);
            }
        }

        if let Ok(raw_tiers) = sde.load_cfsd_nested("res:/staticdata/ship_skin_design_tier_thresholds.fsdbinary") {
            if let Some(first_table) = raw_tiers.values().next().and_then(Value::as_object) {
                for (tier_str, thresh_val) in first_table {
                    if let (Ok(tier), Some(thresh)) = (tier_str.parse::<u32>(), thresh_val.as_u64()) {
                        cat.tier_thresholds.insert(tier, thresh as u32);
                    }
                }
            }
        }
        if cat.tier_thresholds.is_empty() {
            for (t, pts) in [
                (1, 0), (2, 350), (3, 700), (4, 1050), (5, 1400),
                (6, 0), (7, 350), (8, 700), (9, 1050), (10, 1400),
                (11, 0), (12, 350), (13, 700), (14, 1050), (15, 1400),
                (16, 0), (17, 500), (18, 1000), (19, 1500),
            ] {
                cat.tier_thresholds.insert(t, pts);
            }
        }

        if let Ok(raw_slots) = sde.load_cfsd_table("res:/staticdata/ship_cosmetic_slots.fsdbinary") {
            for (slot_id, rec) in raw_slots {
                if slot_id > 0 {
                    let allowed: Vec<u32> = rec
                        .list("allowed_design_component_categories")
                        .map(|s| s.iter().filter_map(|&x| u32::try_from(x).ok()).collect())
                        .unwrap_or_default();
                    cat.slot_allowed_categories.insert(slot_id as u32, allowed);
                }
            }
        }
        if cat.slot_allowed_categories.is_empty() {
            for slot_id in 1..=4 {
                cat.slot_allowed_categories.insert(slot_id, vec![COMP_CAT_MATERIAL, COMP_CAT_METALLIC]);
            }
            cat.slot_allowed_categories.insert(5, vec![COMP_CAT_PATTERN]);
            for slot_id in 6..=8 {
                cat.slot_allowed_categories.insert(slot_id, vec![COMP_CAT_MATERIAL, COMP_CAT_METALLIC]);
            }
        }

        if let Ok(raw_cfgs) = sde.load_cfsd_nested("res:/staticdata/ship_cosmetic_slot_configurations.fsdbinary") {
            for v in raw_cfgs.values() {
                let slots: Vec<u32> = v
                    .get("config")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|x| x.as_u64().map(|u| u as u32)).collect())
                    .unwrap_or_default();
                let allow_all = v.get("allow_all_ships").and_then(Value::as_bool).unwrap_or(false);
                if allow_all && !slots.is_empty() {
                    cat.default_ship_slots = slots.clone();
                }
                if let Some(ships) = v.get("ships").and_then(Value::as_array) {
                    for sh in ships {
                        if let Some(ship_tid) = sh.as_u64() {
                            cat.ship_slot_overrides.insert(ship_tid as u32, slots.clone());
                        }
                    }
                }
            }
        }
        if cat.default_ship_slots.is_empty() {
            cat.default_ship_slots = (1..=8).collect();
        }
        cat
    }

    pub fn allowed_slots_for_ship(&self, ship_type_id: u32) -> &[u32] {
        self.ship_slot_overrides
            .get(&ship_type_id)
            .map(Vec::as_slice)
            .unwrap_or(&self.default_ship_slots)
    }

    /// Computes total design points and resolves the tier level (1..=5 within standard hull group).
    pub fn compute_points_and_tier(&self, fitted_components: &[u32]) -> (u32, u32) {
        let mut points = 0u32;
        for &cid in fitted_components {
            if let Some(comp) = self.components.get(&cid) {
                let pt = self
                    .point_values
                    .get(&(comp.category, comp.rarity))
                    .copied()
                    .unwrap_or(50);
                points = points.saturating_add(pt);
            }
        }
        let mut tier = 1u32;
        for t in 1..=5u32 {
            let min_pts = self.tier_thresholds.get(&t).copied().unwrap_or((t - 1) * 350);
            if points >= min_pts {
                tier = t;
            }
        }
        (points, tier)
    }
}

/// Finite component license record (`eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.Attributes`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FiniteComponentLicense {
    pub character_id: u32,
    pub component_id: u32,
    pub is_pattern: bool,
    pub capacity: u64,
    pub first_acquired_unix: u64,
}

/// Infinite component license record (`eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.Attributes`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InfiniteComponentLicense {
    pub character_id: u32,
    pub component_id: u32,
    pub is_pattern: bool,
    pub first_acquired_unix: u64,
    pub is_free: bool,
}

/// One fitted slot in a 3rd-party SKIN layout (`eve_public.cosmetic.ship.skin.thirdparty.layout.Slot`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkinLayoutSlot {
    pub slot_id: u32,
    pub component_id: u32,
    pub is_pattern: bool,
    pub primary_coating_id: Option<u32>,
    pub secondary_coating_id: Option<u32>,
}

/// Custom 3rd-party SKIN design (`eve_public.cosmetic.ship.skin.thirdparty.Attributes`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkinDesignRecord {
    pub skin_hex: String,
    pub name: String,
    pub line_name: String,
    pub creator_id: u32,
    pub ship_type_id: u32,
    pub pattern_blend_mode: u32,
    pub slots: Vec<SkinLayoutSlot>,
    pub tier_level: u32,
    pub first_sequenced_unix: Option<u64>,
}

/// Owned 3rd-party SKIN license (`eve_public.cosmetic.ship.skin.thirdparty.license.Attributes`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ThirdPartySkinLicense {
    pub character_id: u32,
    pub skin_hex: String,
    pub activated: bool,
    pub unactivated: u64,
    pub escrowed: u64,
    pub first_acquired_unix: u64,
}

/// Sequencing job state (`eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.Attributes`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SequencingJobRecord {
    pub job_uuid: [u8; 16],
    pub skin_hex: String,
    pub quantity: u32,
    pub sequencer_id: u32,
    pub initiated_unix: u64,
    pub planned_duration_secs: u64,
    pub completed_unix: Option<u64>,
    pub failed_reason: Option<u32>,
}

impl SequencingJobRecord {
    pub fn is_active(&self) -> bool {
        self.completed_unix.is_none() && self.failed_reason.is_none()
    }
}

/// Character title definition entry (`eve.cosmetic.character.title.definition.Definition`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TitleDefinitionRecord {
    pub title_id: u32,
    pub name: String,
    pub internal_name: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkinrCosmeticsData {
    /// Key: `"{character_id}:{is_pattern as u8}:{component_id}"`
    pub finite_licenses: BTreeMap<String, FiniteComponentLicense>,
    /// Key: `"{character_id}:{is_pattern as u8}:{component_id}"`
    pub infinite_licenses: BTreeMap<String, InfiniteComponentLicense>,
    /// Key: `skin_hex`
    pub skins: BTreeMap<String, SkinDesignRecord>,
    /// Key: `"{character_id}:{skin_hex}"`
    pub skin_licenses: BTreeMap<String, ThirdPartySkinLicense>,
    /// Key: `ship_item_id -> skin_hex`
    pub applied_thirdparty_skins: BTreeMap<u64, String>,
    /// Key: `job_uuid_hex`
    pub sequencing_jobs: BTreeMap<String, SequencingJobRecord>,
    pub next_job_seq: u64,
    /// Current semantic version `(major, minor, patch)` for character title definitions.
    pub title_def_version: [u32; 3],
    /// Tenant-specific version overrides (`tenant_name -> [major, minor, patch]`).
    pub tenant_title_versions: BTreeMap<String, [u32; 3]>,
    pub title_definitions: Vec<TitleDefinitionRecord>,
}

impl Default for SkinrCosmeticsData {
    fn default() -> Self {
        Self {
            finite_licenses: BTreeMap::new(),
            infinite_licenses: BTreeMap::new(),
            skins: BTreeMap::new(),
            skin_licenses: BTreeMap::new(),
            applied_thirdparty_skins: BTreeMap::new(),
            sequencing_jobs: BTreeMap::new(),
            next_job_seq: 1,
            title_def_version: [24, 1, 0],
            tenant_title_versions: BTreeMap::new(),
            title_definitions: vec![
                TitleDefinitionRecord {
                    title_id: 101,
                    name: "Grand Admiral".into(),
                    internal_name: "title_grand_admiral".into(),
                },
                TitleDefinitionRecord {
                    title_id: 102,
                    name: "Fleet Commander".into(),
                    internal_name: "title_fleet_commander".into(),
                },
                TitleDefinitionRecord {
                    title_id: 103,
                    name: "Sovereign Vanguard".into(),
                    internal_name: "title_sovereign_vanguard".into(),
                },
            ],
        }
    }
}

pub struct SkinrCosmeticsState {
    pub db: Arc<WorldRepository>,
    pub sde: Arc<SdeRegistry>,
    pub catalog: SkinrStaticCatalog,
    pub data: RwLock<SkinrCosmeticsData>,
}

impl SkinrCosmeticsState {
    pub fn load(db: Arc<WorldRepository>, sde: Arc<SdeRegistry>) -> Arc<Self> {
        let data = db.with_snapshot(|s| {
            s.quasar_state_blobs
                .get(SKINR_BLOB_KEY)
                .and_then(|j| serde_json::from_str::<SkinrCosmeticsData>(j).ok())
                .unwrap_or_default()
        });
        let catalog = SkinrStaticCatalog::load(&sde);
        Arc::new(Self {
            db,
            sde,
            catalog,
            data: RwLock::new(data),
        })
    }

    pub fn save(&self) {
        let data = self.data.read().unwrap_or_else(|e| e.into_inner());
        if let Ok(json) = serde_json::to_string(&*data) {
            let _ = self.db.try_with_mut_snapshot(|s| {
                s.quasar_state_blobs.insert(SKINR_BLOB_KEY.into(), json);
                Ok::<(), String>(())
            });
        }
    }

    pub fn comp_key(character_id: u32, is_pattern: bool, component_id: u32) -> String {
        format!("{character_id}:{}:{component_id}", u8::from(is_pattern))
    }

    pub fn skin_lic_key(character_id: u32, skin_hex: &str) -> String {
        format!("{character_id}:{skin_hex}")
    }
}

// ============================================================================
// Wire Encoding & Parsing Helpers
// ============================================================================

pub fn any_msg(type_name: &str, value: Vec<u8>) -> GoogleAny {
    GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value,
    }
}

pub fn put_seq(field: u32, id: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, id, &mut b);
    encode_bytes_field(field, &b, out);
}

pub fn put_timestamp(field: u32, unix_secs: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, unix_secs, &mut b);
    encode_bytes_field(field, &b, out);
}

pub fn put_duration(field: u32, secs: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, secs, &mut b);
    encode_bytes_field(field, &b, out);
}

pub fn require_char(ctx: &AuthoritativeContext) -> Result<u32, (u32, String)> {
    ctx.effective_character_id()
        .ok_or_else(|| (STATUS_FORBIDDEN, "Active character required".to_string()))
}

pub fn as_u64(v: &DynamicValue) -> Option<u64> {
    match v {
        DynamicValue::Uint64(u) => Some(*u),
        DynamicValue::Uint32(u) => Some(*u as u64),
        DynamicValue::Int64(i) => u64::try_from(*i).ok(),
        DynamicValue::Int32(i) => u64::try_from(*i).ok(),
        DynamicValue::Enum(e) => u64::try_from(*e).ok(),
        _ => None,
    }
}

pub fn sub(msg: &DynamicQuasarMessage, field: u32) -> Option<&DynamicQuasarMessage> {
    match msg.get(field)? {
        DynamicValue::Message(m) => Some(m),
        _ => None,
    }
}

pub fn seq_of(msg: &DynamicQuasarMessage, field: u32) -> Option<u64> {
    sub(msg, field).and_then(|m| m.get(1)).and_then(as_u64)
}

pub fn str_of(msg: &DynamicQuasarMessage, field: u32) -> Option<String> {
    match msg.get(field)? {
        DynamicValue::String(s) => Some(s.clone()),
        DynamicValue::Bytes(b) => String::from_utf8(b.clone()).ok(),
        _ => None,
    }
}

/// Encodes `eve_public.cosmetic.ship.skin.thirdparty.component.Identifier`.
pub fn encode_component_id(component_id: u32, is_pattern: bool) -> Vec<u8> {
    let mut inner = Vec::new();
    encode_varint_field(1, component_id as u64, &mut inner);
    let mut out = Vec::new();
    encode_bytes_field(if is_pattern { 2 } else { 1 }, &inner, &mut out);
    out
}

/// Parses `component.Identifier { 1: coating { 1: seq }, 2: pattern { 1: seq } }`.
pub fn parse_component_id(msg: &DynamicQuasarMessage) -> Option<(u32, bool)> {
    if let Some(id) = seq_of(msg, 1) {
        if id > 0 {
            return Some((id as u32, false));
        }
    }
    if let Some(id) = seq_of(msg, 2) {
        if id > 0 {
            return Some((id as u32, true));
        }
    }
    None
}

pub fn parse_component_id_wire(raw: &[u8]) -> Option<(u32, bool)> {
    let wm = WireMessage::decode(raw).ok()?;
    for f in &wm.fields {
        if f.field_number == 1 || f.field_number == 2 {
            if let Some(b) = f.value.as_bytes() {
                if let Ok(inner) = WireMessage::decode(b) {
                    if let Some(seq) = inner
                        .fields
                        .iter()
                        .find(|x| x.field_number == 1)
                        .and_then(|x| x.value.as_varint())
                    {
                        if seq > 0 {
                            return Some((seq as u32, f.field_number == 2));
                        }
                    }
                }
            }
        }
    }
    None
}

/// Encodes `finite.Identifier` or `infinite.Identifier` (`{ 1: character, 2: component }`).
pub fn encode_comp_license_id(character_id: u32, component_id: u32, is_pattern: bool) -> Vec<u8> {
    let mut out = Vec::new();
    put_seq(1, character_id as u64, &mut out);
    encode_bytes_field(2, &encode_component_id(component_id, is_pattern), &mut out);
    out
}

/// Encodes `eve_public.cosmetic.ship.skin.thirdparty.component.license.Kind`.
pub fn encode_license_kind(component_id: u32, is_pattern: bool, finite_qty: Option<u64>) -> Vec<u8> {
    let mut out = Vec::new();
    encode_bytes_field(1, &encode_component_id(component_id, is_pattern), &mut out);
    match finite_qty {
        Some(qty) => encode_varint_field(3, qty, &mut out),
        None => encode_varint_field(2, 1, &mut out),
    }
    out
}

/// Emits `eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GrantedNotice`.
pub fn queue_component_license_granted_notice(
    character_id: u32,
    component_id: u32,
    is_pattern: bool,
    finite_qty: Option<u64>,
    now_unix: u64,
) {
    let mut payload = Vec::new();
    encode_bytes_field(1, &encode_license_kind(component_id, is_pattern, finite_qty), &mut payload);
    ctx_queue_notice(
        NoticeScope::Character(character_id),
        any_msg(
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GrantedNotice",
            payload,
        ),
        now_unix,
    );
}

fn validate_component_in_sde(
    state: &SkinrCosmeticsState,
    comp_id: u32,
    is_pattern: bool,
) -> Result<&SdeSkinComponent, (u32, String)> {
    let Some(comp) = state.catalog.components.get(&comp_id) else {
        return Err((STATUS_BAD_REQUEST, format!("Unknown SKINR design component {comp_id}")));
    };
    if comp.is_pattern() != is_pattern {
        return Err((
            STATUS_BAD_REQUEST,
            format!("Component {comp_id} category mismatch for coating/pattern selector"),
        ));
    }
    Ok(comp)
}

fn parse_license_identifier(
    dyn_msg: &DynamicQuasarMessage,
    raw: &[u8],
) -> Option<(Option<u32>, u32, bool)> {
    if let Some(lic_msg) = sub(dyn_msg, 1) {
        let char_id = seq_of(lic_msg, 1).map(|u| u as u32);
        if let Some((cid, is_pat)) = sub(lic_msg, 2).and_then(parse_component_id) {
            return Some((char_id, cid, is_pat));
        }
    }
    let wm = WireMessage::decode(raw).ok()?;
    let lic_bytes = wm.fields.iter().find(|f| f.field_number == 1)?.value.as_bytes()?;
    let lic_wm = WireMessage::decode(lic_bytes).ok()?;
    let char_id = lic_wm
        .fields
        .iter()
        .find(|f| f.field_number == 1)
        .and_then(|f| f.value.as_bytes())
        .and_then(|cb| WireMessage::decode(cb).ok())
        .and_then(|cwm| {
            cwm.fields
                .iter()
                .find(|f| f.field_number == 1)
                .and_then(|f| f.value.as_varint())
                .map(|u| u as u32)
        });
    let comp_bytes = lic_wm.fields.iter().find(|f| f.field_number == 2)?.value.as_bytes()?;
    let (cid, is_pat) = parse_component_id_wire(comp_bytes)?;
    Some((char_id, cid, is_pat))
}

// ============================================================================
// Component License Handlers
// ============================================================================

fn handle_comp_license_get_owned(state: &SkinrCosmeticsState, ctx: &AuthoritativeContext) -> HandlerResult {
    let cid = require_char(ctx)?;
    let data = state.data.read().unwrap_or_else(|e| e.into_inner());
    let mut out = Vec::new();
    for lic in data.finite_licenses.values() {
        if lic.character_id == cid && lic.capacity > 0 {
            encode_bytes_field(
                1,
                &encode_comp_license_id(cid, lic.component_id, lic.is_pattern),
                &mut out,
            );
        }
    }
    for lic in data.infinite_licenses.values() {
        if lic.character_id == cid {
            encode_bytes_field(
                2,
                &encode_comp_license_id(cid, lic.component_id, lic.is_pattern),
                &mut out,
            );
        }
    }
    Ok(any_msg(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GetOwnedResponse",
        out,
    ))
}

fn handle_finite_get(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
    raw: &[u8],
    dyn_msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let caller = require_char(ctx)?;
    let Some((req_char, comp_id, is_pattern)) = parse_license_identifier(dyn_msg, raw) else {
        return Err((STATUS_BAD_REQUEST, "Missing finite component license identifier".into()));
    };
    let target_cid = req_char.unwrap_or(caller);
    if target_cid != caller && require_admin_role(ctx).is_err() {
        return Err((STATUS_FORBIDDEN, "Cannot query another character's component license".into()));
    }
    let key = SkinrCosmeticsState::comp_key(target_cid, is_pattern, comp_id);
    let data = state.data.read().unwrap_or_else(|e| e.into_inner());
    let Some(lic) = data.finite_licenses.get(&key) else {
        return Err((STATUS_NOT_FOUND, "Finite component license not found".into()));
    };
    let mut attrs = Vec::new();
    put_timestamp(1, lic.first_acquired_unix, &mut attrs);
    encode_varint_field(2, lic.capacity, &mut attrs);
    let mut out = Vec::new();
    encode_bytes_field(1, &attrs, &mut out);
    Ok(any_msg(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.GetResponse",
        out,
    ))
}

fn handle_finite_admin_grant(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
    raw: &[u8],
    dyn_msg: &DynamicQuasarMessage,
) -> HandlerResult {
    require_admin_role(ctx)?;
    let cid = require_char(ctx)?;
    let comp_parsed = sub(dyn_msg, 1).and_then(parse_component_id).or_else(|| {
        let wm = WireMessage::decode(raw).ok()?;
        let b = wm.fields.iter().find(|f| f.field_number == 1)?.value.as_bytes()?;
        parse_component_id_wire(b)
    });
    let Some((comp_id, is_pattern)) = comp_parsed else {
        return Err((STATUS_BAD_REQUEST, "Missing component identifier".into()));
    };
    let capacity = dyn_msg
        .get(2)
        .and_then(as_u64)
        .or_else(|| {
            WireMessage::decode(raw)
                .ok()?
                .fields
                .iter()
                .find(|f| f.field_number == 2)?
                .value
                .as_varint()
        })
        .unwrap_or(0);
    if capacity == 0 {
        return Err((STATUS_BAD_REQUEST, "Grant capacity must be greater than zero".into()));
    }
    validate_component_in_sde(state, comp_id, is_pattern)?;

    let now = ctx.now_unix();
    let key = SkinrCosmeticsState::comp_key(cid, is_pattern, comp_id);
    {
        let mut data = state.data.write().unwrap_or_else(|e| e.into_inner());
        let entry = data.finite_licenses.entry(key).or_insert(FiniteComponentLicense {
            character_id: cid,
            component_id: comp_id,
            is_pattern,
            capacity: 0,
            first_acquired_unix: now,
        });
        entry.capacity = entry.capacity.saturating_add(capacity);
    }
    state.save();
    queue_component_license_granted_notice(cid, comp_id, is_pattern, Some(capacity), now);

    let mut out = Vec::new();
    encode_bytes_field(1, &encode_comp_license_id(cid, comp_id, is_pattern), &mut out);
    Ok(any_msg(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantResponse",
        out,
    ))
}

fn handle_finite_admin_revoke(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
    raw: &[u8],
    dyn_msg: &DynamicQuasarMessage,
) -> HandlerResult {
    require_admin_role(ctx)?;
    let caller = require_char(ctx)?;
    let Some((req_char, comp_id, is_pattern)) = parse_license_identifier(dyn_msg, raw) else {
        return Err((STATUS_BAD_REQUEST, "Missing finite license identifier".into()));
    };
    let target_cid = req_char.unwrap_or(caller);
    let key = SkinrCosmeticsState::comp_key(target_cid, is_pattern, comp_id);
    let removed = state
        .data
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .finite_licenses
        .remove(&key);
    if removed.is_none() {
        return Err((STATUS_NOT_FOUND, "Finite component license not found".into()));
    }
    state.save();
    Ok(any_msg(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.RevokeResponse",
        Vec::new(),
    ))
}

fn handle_infinite_get(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
    raw: &[u8],
    dyn_msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let caller = require_char(ctx)?;
    let Some((req_char, comp_id, is_pattern)) = parse_license_identifier(dyn_msg, raw) else {
        return Err((STATUS_BAD_REQUEST, "Missing infinite component license identifier".into()));
    };
    let target_cid = req_char.unwrap_or(caller);
    if target_cid != caller && require_admin_role(ctx).is_err() {
        return Err((STATUS_FORBIDDEN, "Cannot query another character's component license".into()));
    }
    let key = SkinrCosmeticsState::comp_key(target_cid, is_pattern, comp_id);
    let data = state.data.read().unwrap_or_else(|e| e.into_inner());
    let Some(lic) = data.infinite_licenses.get(&key) else {
        return Err((STATUS_NOT_FOUND, "Infinite component license not found".into()));
    };
    let mut attrs = Vec::new();
    if lic.is_free {
        encode_varint_field(2, 1, &mut attrs);
    } else {
        put_timestamp(1, lic.first_acquired_unix, &mut attrs);
    }
    let mut out = Vec::new();
    encode_bytes_field(1, &attrs, &mut out);
    Ok(any_msg(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.GetResponse",
        out,
    ))
}

fn handle_infinite_admin_grant(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
    raw: &[u8],
    dyn_msg: &DynamicQuasarMessage,
) -> HandlerResult {
    require_admin_role(ctx)?;
    let cid = require_char(ctx)?;
    let comp_parsed = sub(dyn_msg, 1).and_then(parse_component_id).or_else(|| {
        let wm = WireMessage::decode(raw).ok()?;
        let b = wm.fields.iter().find(|f| f.field_number == 1)?.value.as_bytes()?;
        parse_component_id_wire(b)
    });
    let Some((comp_id, is_pattern)) = comp_parsed else {
        return Err((STATUS_BAD_REQUEST, "Missing component identifier".into()));
    };
    validate_component_in_sde(state, comp_id, is_pattern)?;

    let now = ctx.now_unix();
    let key = SkinrCosmeticsState::comp_key(cid, is_pattern, comp_id);
    state.data.write().unwrap_or_else(|e| e.into_inner()).infinite_licenses.insert(
        key,
        InfiniteComponentLicense {
            character_id: cid,
            component_id: comp_id,
            is_pattern,
            first_acquired_unix: now,
            is_free: false,
        },
    );
    state.save();
    queue_component_license_granted_notice(cid, comp_id, is_pattern, None, now);

    let mut out = Vec::new();
    encode_bytes_field(1, &encode_comp_license_id(cid, comp_id, is_pattern), &mut out);
    Ok(any_msg(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.GrantResponse",
        out,
    ))
}

fn handle_infinite_admin_revoke(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
    raw: &[u8],
    dyn_msg: &DynamicQuasarMessage,
) -> HandlerResult {
    require_admin_role(ctx)?;
    let caller = require_char(ctx)?;
    let Some((req_char, comp_id, is_pattern)) = parse_license_identifier(dyn_msg, raw) else {
        return Err((STATUS_BAD_REQUEST, "Missing infinite license identifier".into()));
    };
    let target_cid = req_char.unwrap_or(caller);
    let key = SkinrCosmeticsState::comp_key(target_cid, is_pattern, comp_id);
    let removed = state
        .data
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .infinite_licenses
        .remove(&key);
    if removed.is_none() {
        return Err((STATUS_NOT_FOUND, "Infinite component license not found".into()));
    }
    state.save();
    Ok(any_msg(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.RevokeResponse",
        Vec::new(),
    ))
}

fn handle_item_consume(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
    raw: &[u8],
    dyn_msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let cid = require_char(ctx)?;
    let item_id = sub(dyn_msg, 1)
        .and_then(|m| sub(m, 1).and_then(|inner| inner.get(1)).or_else(|| m.get(1)))
        .and_then(as_u64)
        .or_else(|| {
            let wm = WireMessage::decode(raw).ok()?;
            let b1 = wm.fields.iter().find(|f| f.field_number == 1)?.value.as_bytes()?;
            let wm1 = WireMessage::decode(b1).ok()?;
            let f1 = wm1.fields.iter().find(|f| f.field_number == 1)?;
            if let Some(v) = f1.value.as_varint() {
                return Some(v);
            }
            let b2 = f1.value.as_bytes()?;
            let wm2 = WireMessage::decode(b2).ok()?;
            wm2.fields.iter().find(|f| f.field_number == 1)?.value.as_varint()
        })
        .unwrap_or(0);
    let quantity = dyn_msg
        .get(2)
        .and_then(as_u64)
        .or_else(|| {
            WireMessage::decode(raw)
                .ok()?
                .fields
                .iter()
                .find(|f| f.field_number == 2)?
                .value
                .as_varint()
        })
        .unwrap_or(0);
    if item_id == 0 || quantity == 0 {
        return Err((STATUS_BAD_REQUEST, "ConsumeRequest requires valid item ID and quantity > 0".into()));
    }

    let (comp_id, uses_granted, is_pattern) = state
        .db
        .try_with_mut_snapshot(|s| -> Result<(u32, i32, bool), eve_proto::BridgeRpcError> {
            let Some(item) = s.items.get(&(item_id as i64)) else {
                return Err(eve_proto::BridgeRpcError {
                    code: STATUS_NOT_FOUND.to_string(),
                    message: "Component license item not found in inventory".into(),
                });
            };
            if item.owner_id != cid as i64 {
                return Err(eve_proto::BridgeRpcError {
                    code: STATUS_FORBIDDEN.to_string(),
                    message: "Character does not own component license item".into(),
                });
            }
            let (current_dock, current_ship) = match s.characters.get(&(cid as i64)) {
                Some(ch) => (
                    ch.station_id.or(ch.structure_id),
                    (ch.active_ship_id > 0).then_some(ch.active_ship_id),
                ),
                None => (Some(60_003_760), None),
            };
            if !s.is_item_consumable_by(item, cid as i64, current_dock, current_ship) {
                return Err(eve_proto::BridgeRpcError {
                    code: STATUS_PRECONDITION_FAILED.to_string(),
                    message: "Component license item is not reachable in your hangar/cargo or is locked in a contract".into(),
                });
            }
            if item.quantity <= 0 || (item.quantity as u64) < quantity {
                return Err(eve_proto::BridgeRpcError {
                    code: STATUS_BAD_REQUEST.to_string(),
                    message: "Insufficient component license item stack quantity".into(),
                });
            }
            let type_id = item.type_id;
            let Some(&(comp_id, uses_granted)) = state.catalog.item_type_to_component.get(&type_id) else {
                return Err(eve_proto::BridgeRpcError {
                    code: STATUS_BAD_REQUEST.to_string(),
                    message: format!("Item type {type_id} is not a SKINR component license"),
                });
            };
            let is_pattern = state
                .catalog
                .components
                .get(&comp_id)
                .is_some_and(SdeSkinComponent::is_pattern);
            if let Some(it) = s.items.get_mut(&(item_id as i64)) {
                if (it.quantity as u64) <= quantity {
                    s.items.remove(&(item_id as i64));
                } else {
                    it.quantity -= quantity as i32;
                    it.version += 1;
                }
            }
            Ok((comp_id, uses_granted, is_pattern))
        })
        .map_err(|e| (e.code.parse::<u32>().unwrap_or(500), e.message))?;

    let now = ctx.now_unix();
    let key = SkinrCosmeticsState::comp_key(cid, is_pattern, comp_id);
    if uses_granted <= 0 {
        state.data.write().unwrap_or_else(|e| e.into_inner()).infinite_licenses.insert(
            key,
            InfiniteComponentLicense {
                character_id: cid,
                component_id: comp_id,
                is_pattern,
                first_acquired_unix: now,
                is_free: false,
            },
        );
        state.save();
        queue_component_license_granted_notice(cid, comp_id, is_pattern, None, now);
    } else {
        let added_capacity = quantity.saturating_mul(uses_granted as u64);
        {
            let mut data = state.data.write().unwrap_or_else(|e| e.into_inner());
            let entry = data.finite_licenses.entry(key).or_insert(FiniteComponentLicense {
                character_id: cid,
                component_id: comp_id,
                is_pattern,
                capacity: 0,
                first_acquired_unix: now,
            });
            entry.capacity = entry.capacity.saturating_add(added_capacity);
        }
        state.save();
        queue_component_license_granted_notice(cid, comp_id, is_pattern, Some(added_capacity), now);
    }

    Ok(any_msg(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeResponse",
        Vec::new(),
    ))
}

// ============================================================================
// Character Title Definition & Version Handlers
// ============================================================================

fn encode_semver(ver: [u32; 3]) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, ver[0] as u64, &mut b);
    encode_varint_field(2, ver[1] as u64, &mut b);
    encode_varint_field(3, ver[2] as u64, &mut b);
    b
}

fn parse_semver(msg: &DynamicQuasarMessage) -> Option<[u32; 3]> {
    let major = msg.get(1).and_then(as_u64)? as u32;
    let minor = msg.get(2).and_then(as_u64).unwrap_or(0) as u32;
    let patch = msg.get(3).and_then(as_u64).unwrap_or(0) as u32;
    Some([major, minor, patch])
}

fn parse_semver_wire(raw: &[u8], field_num: u32) -> Option<[u32; 3]> {
    let wm = WireMessage::decode(raw).ok()?;
    let b = wm.fields.iter().find(|f| f.field_number == field_num)?.value.as_bytes()?;
    let swm = WireMessage::decode(b).ok()?;
    let major = swm.fields.iter().find(|f| f.field_number == 1)?.value.as_varint()? as u32;
    let minor = swm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()).unwrap_or(0) as u32;
    let patch = swm.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_varint()).unwrap_or(0) as u32;
    Some([major, minor, patch])
}

fn compute_title_def_crc(defs: &[TitleDefinitionRecord], ver: [u32; 3], lang: &str) -> u32 {
    let mut hash: u32 = 0x811C_9DC5;
    for b in lang.as_bytes() {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    for v in ver {
        hash ^= v;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    for d in defs {
        hash ^= d.title_id;
        hash = hash.wrapping_mul(0x0100_0193);
        for b in d.name.as_bytes() {
            hash ^= *b as u32;
            hash = hash.wrapping_mul(0x0100_0193);
        }
    }
    hash
}

fn handle_title_def_get_latest(
    state: &SkinrCosmeticsState,
    req_type: &str,
    dyn_msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let lang = sub(dyn_msg, 3)
        .and_then(|l| str_of(l, 1))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "en-us".to_string());
    let data = state.data.read().unwrap_or_else(|e| e.into_inner());
    let ver = data.title_def_version;
    let crc = compute_title_def_crc(&data.title_definitions, ver, &lang);
    let url = format!(
        "https://resources.eveonline.com/cosmetics/character/title/definitions/v{}.{}.{}/{}.json",
        ver[0], ver[1], ver[2], lang
    );

    let mut crc_msg = Vec::new();
    encode_varint_field(1, crc as u64, &mut crc_msg);

    let mut checkpoint = Vec::new();
    encode_string_field(1, &url, &mut checkpoint);
    encode_bytes_field(2, &encode_semver(ver), &mut checkpoint);
    encode_bytes_field(3, &crc_msg, &mut checkpoint);

    let mut out = Vec::new();
    encode_bytes_field(1, &checkpoint, &mut out);
    let rsp_type = if req_type.starts_with("eve.") {
        "eve.cosmetic.character.title.definition.api.GetLatestResponse"
    } else {
        "eve_public.cosmetic.character.title.definition.api.GetLatestResponse"
    };
    Ok(any_msg(rsp_type, out))
}

fn handle_title_def_list(state: &SkinrCosmeticsState, req_type: &str) -> HandlerResult {
    let data = state.data.read().unwrap_or_else(|e| e.into_inner());
    let mut out = Vec::new();
    for def in &data.title_definitions {
        let mut id_bytes = Vec::new();
        encode_varint_field(1, def.title_id as u64, &mut id_bytes);
        let mut summary = Vec::new();
        encode_bytes_field(1, &id_bytes, &mut summary);
        encode_string_field(2, &def.name, &mut summary);
        encode_string_field(3, &def.internal_name, &mut summary);
        encode_bytes_field(1, &summary, &mut out);
        encode_bytes_field(2, &summary, &mut out);
    }
    let rsp_type = if req_type.ends_with("ListDefinitionsRequest") {
        "eve.cosmetic.character.title.definition.api.ListDefinitionsResponse"
    } else if req_type.starts_with("eve.") {
        "eve.cosmetic.character.title.definition.api.GetAllResponse"
    } else {
        "eve_public.cosmetic.character.title.definition.api.GetAllResponse"
    };
    Ok(any_msg(rsp_type, out))
}

fn broadcast_title_version_notice(ver: [u32; 3], now_unix: u64) {
    let mut payload = Vec::new();
    encode_bytes_field(1, &encode_semver(ver), &mut payload);
    ctx_queue_notice(
        NoticeScope::Broadcast,
        any_msg(
            "eve_public.cosmetic.character.title.definition.version.api.CheckVersionNotice",
            payload,
        ),
        now_unix,
    );
}

fn handle_title_version_set_default(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
    raw: &[u8],
    dyn_msg: &DynamicQuasarMessage,
) -> HandlerResult {
    require_admin_role(ctx)?;
    let ver = sub(dyn_msg, 2)
        .and_then(parse_semver)
        .or_else(|| sub(dyn_msg, 1).and_then(parse_semver))
        .or_else(|| parse_semver_wire(raw, 2))
        .or_else(|| parse_semver_wire(raw, 1))
        .filter(|v| v[0] > 0);
    let Some(ver) = ver else {
        return Err((STATUS_BAD_REQUEST, "Valid semantic version specification required".into()));
    };
    state.data.write().unwrap_or_else(|e| e.into_inner()).title_def_version = ver;
    state.save();
    broadcast_title_version_notice(ver, ctx.now_unix());
    Ok(any_msg(
        "eve.cosmetic.character.title.definition.version.api.admin.SetDefaultVersionResponse",
        Vec::new(),
    ))
}

fn handle_title_version_override_tenant(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
    dyn_msg: &DynamicQuasarMessage,
) -> HandlerResult {
    require_admin_role(ctx)?;
    let tenant = str_of(dyn_msg, 1).unwrap_or_default();
    let Some(ver) = sub(dyn_msg, 2).and_then(parse_semver).filter(|v| v[0] > 0) else {
        return Err((STATUS_BAD_REQUEST, "Valid tenant and semantic version required".into()));
    };
    if tenant.is_empty() {
        return Err((STATUS_BAD_REQUEST, "Tenant name cannot be empty".into()));
    }
    {
        let mut data = state.data.write().unwrap_or_else(|e| e.into_inner());
        data.tenant_title_versions.insert(tenant, ver);
        data.title_def_version = ver;
    }
    state.save();
    broadcast_title_version_notice(ver, ctx.now_unix());
    Ok(any_msg(
        "eve.cosmetic.character.title.definition.version.api.admin.OverrideVersionForTenantResponse",
        Vec::new(),
    ))
}

fn handle_title_version_get_default(state: &SkinrCosmeticsState) -> HandlerResult {
    let ver = state.data.read().unwrap_or_else(|e| e.into_inner()).title_def_version;
    let mut out = Vec::new();
    encode_bytes_field(1, &encode_semver(ver), &mut out);
    Ok(any_msg(
        "eve.cosmetic.character.title.definition.version.api.GetDefaultVersionResponse",
        out,
    ))
}

pub fn register(router: &PublicGatewayRouter, state: Arc<SkinrCosmeticsState>) {
    macro_rules! reg {
        ($name:expr, $closure:expr) => {{
            let s = Arc::clone(&state);
            router.register_handler($name, Arc::new(move |ctx, any, dyn_msg| ($closure)(&s, ctx, any, dyn_msg)));
        }};
    }

    // 1. 3rd-Party SKINR Component License endpoints
    reg!(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GetOwnedRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, _any: &GoogleAny, _d| handle_comp_license_get_owned(s, ctx)
    );
    reg!(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.GetRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, any: &GoogleAny, d| handle_finite_get(s, ctx, &any.value, d)
    );
    reg!(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, any: &GoogleAny, d| handle_finite_admin_grant(s, ctx, &any.value, d)
    );
    reg!(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.RevokeRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, any: &GoogleAny, d| handle_finite_admin_revoke(s, ctx, &any.value, d)
    );
    reg!(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.GetRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, any: &GoogleAny, d| handle_infinite_get(s, ctx, &any.value, d)
    );
    reg!(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.GrantRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, any: &GoogleAny, d| handle_infinite_admin_grant(s, ctx, &any.value, d)
    );
    reg!(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.RevokeRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, any: &GoogleAny, d| handle_infinite_admin_revoke(s, ctx, &any.value, d)
    );
    reg!(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, any: &GoogleAny, d| handle_item_consume(s, ctx, &any.value, d)
    );

    // 2. Character Title Definition & Version endpoints
    for req_name in [
        "eve_public.cosmetic.character.title.definition.api.GetLatestRequest",
        "eve.cosmetic.character.title.definition.api.GetLatestRequest",
    ] {
        reg!(req_name, move |s: &Arc<SkinrCosmeticsState>, _ctx, _any: &GoogleAny, d| {
            handle_title_def_get_latest(s, req_name, d)
        });
    }
    for req_name in [
        "eve_public.cosmetic.character.title.definition.api.GetAllRequest",
        "eve.cosmetic.character.title.definition.api.GetAllRequest",
        "eve.cosmetic.character.title.definition.api.ListDefinitionsRequest",
    ] {
        reg!(req_name, move |s: &Arc<SkinrCosmeticsState>, _ctx, _any: &GoogleAny, _d| {
            handle_title_def_list(s, req_name)
        });
    }
    reg!(
        "eve.cosmetic.character.title.definition.version.api.admin.SetDefaultVersionRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, any: &GoogleAny, d| handle_title_version_set_default(s, ctx, &any.value, d)
    );
    reg!(
        "eve.cosmetic.character.title.definition.version.api.admin.OverrideVersionForTenantRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, _any: &GoogleAny, d| handle_title_version_override_tenant(s, ctx, d)
    );
    reg!(
        "eve.cosmetic.character.title.definition.version.api.GetDefaultVersionRequest",
        |s: &Arc<SkinrCosmeticsState>, _ctx, _any: &GoogleAny, _d| handle_title_version_get_default(s)
    );
}
