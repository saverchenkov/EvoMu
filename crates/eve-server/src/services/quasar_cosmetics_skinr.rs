//! `quasar_cosmetics_skinr` — State-backed V24.01 Quasar handlers for 3rd-party SKINR component
//! licenses (`finite`, `infinite`, `item` consumption, and `GrantedNotice`) and character title
//! definitions & semantic version checkpoints (`GetLatestRequest`, `CheckVersionNotice`).
//!
//! Static catalogue rules (components, categories, rarities, associated item types, point values,
//! tier thresholds, and ship cosmetic slot configurations) are loaded directly from the SDE
//! `.fsdbinary` resources via [`SdeRegistry`]. Mutable state persists in
//! `WorldStateSnapshot::quasar_state_blobs["quasar_cosmetics_skinr"]`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST,
    STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_PRECONDITION_FAILED,
};
use eve_db::WorldRepository;
use eve_proto::pb::{self, ProtoBuilder};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, GoogleAny, GoogleTimestamp, WireMessage,
};
use eve_sde::SdeRegistry;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

#[cfg(test)]
use eve_proto::quasar::TYPE_GOOGLEAPIS_PREFIX;

#[path = "quasar_cosmetics_skinr_wire.rs"]
mod skinr_wire;
pub use skinr_wire::*;

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
    /// Maps `tier_level (1..=19) -> max_points` (representative hull threshold table from SDE).
    pub tier_thresholds: BTreeMap<u32, u32>,
    /// Maps `isis_group_id -> (tier_level 1..=19 -> max_points)` across all 49 hull classes in
    /// `ship_skin_design_tier_thresholds.fsdbinary`.
    pub tier_thresholds_by_isis_group: BTreeMap<u32, BTreeMap<u32, u32>>,
    /// Maps `ship_type_id -> isis_group_id` from `sde_types.isis_group_id`.
    pub ship_isis_groups: HashMap<u32, u32>,
    /// Maps `slot_id (1..=8) -> allowed_design_component_categories` from `ship_cosmetic_slots.fsdbinary`.
    pub slot_allowed_categories: BTreeMap<u32, Vec<u32>>,
    /// Default allowed slot IDs (`1..=8`) from `ship_cosmetic_slot_configurations.fsdbinary`.
    pub default_ship_slots: Vec<u32>,
    /// Ship-specific slot configuration overrides (`ship_type_id -> allowed_slot_ids`).
    pub ship_slot_overrides: HashMap<u32, Vec<u32>>,
}

impl SkinrStaticCatalog {
    pub fn load(sde: &SdeRegistry) -> Self {
        let mut cat = Self {
            ship_isis_groups: sde.type_isis_groups.clone(),
            ..Self::default()
        };

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
                (3, 1, 100), (3, 2, 200), (3, 3, 300), (3, 4, 400), (3, 5, 600), (3, 6, 900),
            ] {
                cat.point_values.insert((c, r), p);
            }
        }

        if let Ok(raw_tiers) = sde.load_cfsd_nested("res:/staticdata/ship_skin_design_tier_thresholds.fsdbinary") {
            for (ig_str, tbl_val) in raw_tiers {
                let Ok(isis_group_id) = ig_str.parse::<u32>() else { continue };
                let Some(obj) = tbl_val.as_object() else { continue };
                let mut tbl = BTreeMap::new();
                for (tier_str, thresh_val) in obj {
                    if let (Ok(tier), Some(thresh)) = (tier_str.parse::<u32>(), thresh_val.as_u64()) {
                        tbl.insert(tier, thresh as u32);
                    }
                }
                if !tbl.is_empty() {
                    cat.tier_thresholds_by_isis_group.insert(isis_group_id, tbl);
                }
            }
        }
        if cat.tier_thresholds_by_isis_group.is_empty() {
            let default_tbl: BTreeMap<u32, u32> = [
                (1, 125), (2, 250), (3, 400), (4, 625), (5, 850),
                (6, 1100), (7, 1375), (8, 1650), (9, 1950), (10, 2250),
                (11, 2575), (12, 2900), (13, 3225), (14, 3550), (15, 3900),
                (16, 4250), (17, 4600), (18, 4950), (19, 5300),
            ]
            .into_iter()
            .collect();
            cat.tier_thresholds_by_isis_group.insert(8, default_tbl);
        }
        if let Some(default_tbl) = cat.tier_thresholds_by_isis_group.get(&8).cloned() {
            cat.tier_thresholds = default_tbl;
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

    /// Resolves the `1..=19` upper-bound point threshold map for `ship_type_id` via its
    /// `sde_types.isis_group_id` (falling back to frigate group `8` when absent).
    pub fn thresholds_for_ship(&self, ship_type_id: u32) -> &BTreeMap<u32, u32> {
        let isis_group_id = self.ship_isis_groups.get(&ship_type_id).copied().unwrap_or(8);
        self.tier_thresholds_by_isis_group
            .get(&isis_group_id)
            .unwrap_or(&self.tier_thresholds)
    }

    /// Resolves total design points and tier level (`1..=20`) using V24 `BaseSkinDesign.compute_current_points_value`
    /// (`NO_SECONDARY_COLOR_BLEND_MODES` `SUBTRACT=2` / `EXCLUSION=3` ignore `SlotID::SECONDARY_PATTERN_MATERIAL=8`).
    pub fn compute_points_and_tier_for_design(
        &self,
        ship_type_id: u32,
        pattern_blend_mode: u32,
        fitted_slots: &[(u32, u32)],
    ) -> (u32, u32) {
        const SLOT_SECONDARY_PATTERN_MATERIAL: u32 = 8;
        const BLEND_SUBTRACT: u32 = 2;
        const BLEND_EXCLUSION: u32 = 3;
        let skip_secondary_pat_mat = matches!(pattern_blend_mode, BLEND_SUBTRACT | BLEND_EXCLUSION);
        let mut points = 0u32;
        for &(slot_id, cid) in fitted_slots {
            if skip_secondary_pat_mat && slot_id == SLOT_SECONDARY_PATTERN_MATERIAL {
                continue;
            }
            if let Some(comp) = self.components.get(&cid) {
                let pt = self
                    .point_values
                    .get(&(comp.category, comp.rarity))
                    .copied()
                    .unwrap_or(50);
                points = points.saturating_add(pt);
            }
        }
        let thresholds = self.thresholds_for_ship(ship_type_id);
        for (&tier, &max_pts) in thresholds {
            if points <= max_pts {
                return (points, tier);
            }
        }
        let max_tier = thresholds.keys().next_back().copied().unwrap_or(19);
        (points, (max_tier + 1).min(20))
    }

    /// Computes total design points and resolves the tier level (`1..=20`) for `ship_type_id`
    /// using the hull's `isisGroupID` threshold table (`points <= threshold[tier]` for `1..=19`,
    /// or `20` when exceeding tier 19).
    pub fn compute_points_and_tier_for_ship(
        &self,
        ship_type_id: u32,
        fitted_components: &[u32],
    ) -> (u32, u32) {
        let slots: Vec<(u32, u32)> = fitted_components.iter().map(|&cid| (0, cid)).collect();
        self.compute_points_and_tier_for_design(ship_type_id, 1, &slots)
    }

    /// Computes total design points and resolves the tier level (`1..=20`) for a standard frigate hull (`587`).
    pub fn compute_points_and_tier(&self, fitted_components: &[u32]) -> (u32, u32) {
        self.compute_points_and_tier_for_ship(587, fitted_components)
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
            title_definitions: Vec::new(),
        }
    }
}

pub struct SkinrCosmeticsState {
    pub db: Arc<WorldRepository>,
    pub sde: Arc<SdeRegistry>,
    pub catalog: SkinrStaticCatalog,
    pub data: RwLock<SkinrCosmeticsData>,
    pub last_redeemed_gen: std::sync::atomic::AtomicU64,
}

const SKINR_RUNTIME_SLOT_KEY: &str = "quasar_cosmetics_skinr_runtime";

#[derive(Default)]
struct SkinrRepoRuntime {
    live_states: std::sync::Mutex<Vec<std::sync::Weak<SkinrCosmeticsState>>>,
}

fn skinr_runtime(db: &WorldRepository) -> Arc<SkinrRepoRuntime> {
    db.get_or_init_transient_runtime_slot(SKINR_RUNTIME_SLOT_KEY, SkinrRepoRuntime::default)
}

pub(super) fn register_live_skinr_state(state: &Arc<SkinrCosmeticsState>) {
    let weak = Arc::downgrade(state);
    let runtime = skinr_runtime(&state.db);
    let mut entry = runtime
        .live_states
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    entry.retain(|w| w.strong_count() > 0);
    if !entry.iter().any(|w| std::sync::Weak::ptr_eq(w, &weak)) {
        entry.push(weak);
    }
}

pub(crate) fn strip_applied_thirdparty_skins_in_memory(db: &WorldRepository, ship_ids: &[u64]) {
    if ship_ids.is_empty() {
        return;
    }
    let live_states: Vec<Arc<SkinrCosmeticsState>> = {
        let runtime = skinr_runtime(db);
        let mut list = runtime
            .live_states
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        list.retain(|w| w.strong_count() > 0);
        list.iter().filter_map(|w| w.upgrade()).collect()
    };
    for st in live_states {
        let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
        for &sid in ship_ids {
            data.applied_thirdparty_skins.remove(&sid);
        }
    }
}

fn settle_completed_skinr_jobs_inner(
    db: &Arc<WorldRepository>,
    queued_notices: Option<&RwLock<Vec<(NoticeScope, GoogleAny, u64)>>>,
    now: u64,
) {
    let state_opt: Option<Arc<SkinrCosmeticsState>> = {
        let runtime = skinr_runtime(db);
        let mut list = runtime
            .live_states
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        list.retain(|w| w.strong_count() > 0);
        list.iter().find_map(|w| w.upgrade())
    };
    if let Some(state) = state_opt {
        state.settle_completed_jobs(queued_notices, now);
    }
}

/// Periodically settles completed SKINR sequencing jobs for the given world repository (`#545`).
pub fn settle_completed_skinr_jobs_for_db(db: &Arc<WorldRepository>, now: u64) {
    settle_completed_skinr_jobs_inner(db, None, now);
}

/// Periodically settles completed SKINR sequencing jobs and queues notices for the gateway router (`#545`).
pub fn settle_completed_skinr_jobs_with_notices(
    db: &Arc<WorldRepository>,
    queued_notices: &RwLock<Vec<(NoticeScope, GoogleAny, u64)>>,
    now: u64,
) {
    settle_completed_skinr_jobs_inner(db, Some(queued_notices), now);
}

impl SkinrCosmeticsState {
    pub fn load(db: Arc<WorldRepository>, sde: Arc<SdeRegistry>) -> Arc<Self> {
        let st = Self::load_unregistered(db, sde);
        register_live_skinr_state(&st);
        st
    }

    pub(crate) fn load_unregistered(db: Arc<WorldRepository>, sde: Arc<SdeRegistry>) -> Arc<Self> {
        let data = db.with_snapshot(|s| {
            s.quasar_state_blobs
                .get(SKINR_BLOB_KEY)
                .and_then(|j| serde_json::from_str::<SkinrCosmeticsData>(j).ok())
                .unwrap_or_default()
        });
        let catalog = SkinrStaticCatalog::load(&sde);
        let initial_redeemed_gen = super::quasar_entitlement_ledger::redeemed_skin_generation(&db);
        let st = Arc::new(Self {
            db,
            sde,
            catalog,
            data: RwLock::new(data),
            last_redeemed_gen: std::sync::atomic::AtomicU64::new(initial_redeemed_gen),
        });
        st
    }

    pub(crate) fn settle_completed_jobs(
        &self,
        queued_notices: Option<&RwLock<Vec<(NoticeScope, GoogleAny, u64)>>>,
        now: u64,
    ) {
        super::quasar_cosmetics_skinr_seq::settle_completed_jobs(self, queued_notices, now);
    }

    pub fn sync_redeemed_skins_if_needed(&self) {
        let cur_gen = super::quasar_entitlement_ledger::redeemed_skin_generation(&self.db);
        let prev = self
            .last_redeemed_gen
            .swap(cur_gen, std::sync::atomic::Ordering::AcqRel);
        if prev != cur_gen {
            if let Some((blob_skins, blob_lics)) = self.db.with_snapshot(|s| {
                s.quasar_state_blobs
                    .get(SKINR_BLOB_KEY)
                    .and_then(|j| serde_json::from_str::<SkinrCosmeticsData>(j).ok())
                    .map(|d| (d.skins, d.skin_licenses))
            }) {
                let mut data = self.data.write().unwrap_or_else(|e| e.into_inner());
                for (k, v) in blob_skins {
                    data.skins.entry(k).or_insert(v);
                }
                for (k, v) in blob_lics {
                    data.skin_licenses.insert(k, v);
                }
            }
        }
    }

    pub fn save(&self) {
        if self.db.is_write_locked_by_current_thread() {
            return;
        }
        let mut snapshot_copy = self.data.read().unwrap_or_else(|e| e.into_inner()).clone();
        let mut recovered_skins = Vec::new();
        let _ = self.db.try_with_mut_snapshot(|s| {
            if let Some(existing) = s
                .quasar_state_blobs
                .get(SKINR_BLOB_KEY)
                .and_then(|j| serde_json::from_str::<SkinrCosmeticsData>(j).ok())
            {
                for (k, v) in existing.skins {
                    if !snapshot_copy.skins.contains_key(&k) {
                        recovered_skins.push((k.clone(), v.clone()));
                        snapshot_copy.skins.insert(k, v);
                    }
                }
            }
            if let Ok(json) = serde_json::to_string(&snapshot_copy) {
                s.quasar_state_blobs.insert(SKINR_BLOB_KEY.into(), json);
            }
            Ok::<(), String>(())
        });
        if !recovered_skins.is_empty() {
            let mut data = self.data.write().unwrap_or_else(|e| e.into_inner());
            for (k, v) in recovered_skins {
                data.skins.entry(k).or_insert(v);
            }
        }
    }

    pub fn rehydrate_from_db(&self) {
        let data = self.db.with_snapshot(|s| {
            s.quasar_state_blobs
                .get(SKINR_BLOB_KEY)
                .and_then(|j| serde_json::from_str::<SkinrCosmeticsData>(j).ok())
                .unwrap_or_default()
        });
        *self.data.write().unwrap_or_else(|e| e.into_inner()) = data;
        self.last_redeemed_gen.store(
            super::quasar_entitlement_ledger::redeemed_skin_generation(&self.db),
            std::sync::atomic::Ordering::Release,
        );
    }

    pub fn comp_key(character_id: u32, is_pattern: bool, component_id: u32) -> String {
        format!("{character_id}:{}:{component_id}", u8::from(is_pattern))
    }

    pub fn skin_lic_key(character_id: u32, skin_hex: &str) -> String {
        format!("{character_id}:{skin_hex}")
    }
}

// ============================================================================
// Component License Handlers
// ============================================================================

fn handle_comp_license_get_owned(state: &SkinrCosmeticsState, ctx: &AuthoritativeContext) -> HandlerResult {
    let cid = require_char(ctx)?;
    state.sync_redeemed_skins_if_needed();
    let data = state.data.read().unwrap_or_else(|e| e.into_inner());
    let mut rsp = pb::message(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GetOwnedResponse",
    );
    for lic in data.finite_licenses.values() {
        if lic.character_id == cid && lic.capacity > 0 {
            rsp = rsp.add(
                "finite",
                build_comp_license_id(
                    "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.Identifier",
                    cid,
                    lic.component_id,
                    lic.is_pattern,
                ),
            );
        }
    }
    for lic in data.infinite_licenses.values() {
        if lic.character_id == cid {
            rsp = rsp.add(
                "infinite",
                build_comp_license_id(
                    "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.Identifier",
                    cid,
                    lic.component_id,
                    lic.is_pattern,
                ),
            );
        }
    }
    rsp.to_any_or_internal_err()
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
    state.sync_redeemed_skins_if_needed();
    let data = state.data.read().unwrap_or_else(|e| e.into_inner());
    let Some(lic) = data.finite_licenses.get(&key) else {
        return Err((STATUS_NOT_FOUND, "Finite component license not found".into()));
    };
    pb::message(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.GetResponse",
    )
    .set(
        "license.first_acquired",
        GoogleTimestamp::from_unix(lic.first_acquired_unix as i64, 0),
    )
    .set("license.capacity", lic.capacity)
    .to_any_or_internal_err()
}

fn handle_finite_admin_grant(
    state: &SkinrCosmeticsState,
    queued_notices: &RwLock<Vec<(NoticeScope, GoogleAny, u64)>>,
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
    state.sync_redeemed_skins_if_needed();
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
    queue_component_license_granted_notice(queued_notices, cid, comp_id, is_pattern, Some(capacity), now);

    pb::message(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantResponse",
    )
    .set(
        "license",
        build_comp_license_id(
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.Identifier",
            cid,
            comp_id,
            is_pattern,
        ),
    )
    .to_any_or_internal_err()
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
    state.sync_redeemed_skins_if_needed();
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
    state.sync_redeemed_skins_if_needed();
    let data = state.data.read().unwrap_or_else(|e| e.into_inner());
    let Some(lic) = data.infinite_licenses.get(&key) else {
        return Err((STATUS_NOT_FOUND, "Infinite component license not found".into()));
    };
    pb::message(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.GetResponse",
    )
    .with(|b| {
        if lic.is_free {
            b.set("license.free", true)
        } else {
            b.set(
                "license.first_acquired",
                GoogleTimestamp::from_unix(lic.first_acquired_unix as i64, 0),
            )
        }
    })
    .to_any_or_internal_err()
}

fn handle_infinite_admin_grant(
    state: &SkinrCosmeticsState,
    queued_notices: &RwLock<Vec<(NoticeScope, GoogleAny, u64)>>,
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
    state.sync_redeemed_skins_if_needed();
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
    queue_component_license_granted_notice(queued_notices, cid, comp_id, is_pattern, None, now);

    pb::message(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.GrantResponse",
    )
    .set(
        "license",
        build_comp_license_id(
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.Identifier",
            cid,
            comp_id,
            is_pattern,
        ),
    )
    .to_any_or_internal_err()
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
    state.sync_redeemed_skins_if_needed();
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
    queued_notices: &RwLock<Vec<(NoticeScope, GoogleAny, u64)>>,
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
                    client_error: None,
                });
            };
            if item.owner_id != cid as i64 {
                return Err(eve_proto::BridgeRpcError {
                    code: STATUS_FORBIDDEN.to_string(),
                    message: "Character does not own component license item".into(),
                    client_error: None,
                });
            }
            let (current_dock, current_ship) = match s.characters.get(&(cid as i64)) {
                Some(ch) => (
                    ch.station_id.or(ch.structure_id),
                    (ch.active_ship_id > 0).then_some(ch.active_ship_id),
                ),
                None => (None, None),
            };
            if !s.is_item_consumable_by(item, cid as i64, current_dock, current_ship) {
                return Err(eve_proto::BridgeRpcError {
                    code: STATUS_PRECONDITION_FAILED.to_string(),
                    message: "Component license item is not reachable in your hangar/cargo or is locked in a contract".into(),
                    client_error: None,
                });
            }
            if item.quantity <= 0 || (item.quantity as u64) < quantity {
                return Err(eve_proto::BridgeRpcError {
                    code: STATUS_BAD_REQUEST.to_string(),
                    message: "Insufficient component license item stack quantity".into(),
                    client_error: None,
                });
            }
            let type_id = item.type_id;
            let Some(&(comp_id, uses_granted)) = state.catalog.item_type_to_component.get(&type_id) else {
                return Err(eve_proto::BridgeRpcError {
                    code: STATUS_BAD_REQUEST.to_string(),
                    message: format!("Item type {type_id} is not a SKINR component license"),
                    client_error: None,
                });
            };
            let is_pattern = state
                .catalog
                .components
                .get(&comp_id)
                .is_some_and(SdeSkinComponent::is_pattern);
            s.consume_exact_item_quantity(
                item_id as i64,
                quantity as i32,
                eve_db::ConsumeExactOptions {
                    active_ship_id: current_ship,
                    ..eve_db::ConsumeExactOptions::for_owner_exact(
                        Some(cid as i64),
                        cid as i64,
                        eve_db::ItemClassPolicy::ExactBlueprintOrSingleton,
                    )
                },
            )
            .map_err(|e| eve_proto::BridgeRpcError {
                code: STATUS_PRECONDITION_FAILED.to_string(),
                message: e.to_string(),
                client_error: None,
            })?;
            Ok((comp_id, uses_granted, is_pattern))
        })
        .map_err(|e| (e.code.parse::<u32>().unwrap_or(500), e.message))?;

    let now = ctx.now_unix();
    grant_component_license_from_item(state, queued_notices, cid, comp_id, is_pattern, uses_granted, quantity, now);

    Ok(any_msg(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeResponse",
        Vec::new(),
    ))
}

/// Plans direct component activation vs hangar item delivery for `immediately_activate` purchases:
/// finite components (`uses_granted > 0`) activate all purchased stacks; unlimited components
/// (`uses_granted <= 0`) activate at most 1 (if not already owned) and deliver any surplus to hangar
/// so paid items are never silently lost.
pub(super) fn plan_component_activation(
    state: &SkinrCosmeticsState,
    cid: u32,
    type_id: u32,
    quantity: u64,
    immediately_activate: bool,
) -> (u64, u64) {
    if !immediately_activate {
        return (0, quantity);
    }
    let Some(&(comp_id, uses_granted)) = state.catalog.item_type_to_component.get(&type_id) else {
        return (0, quantity);
    };
    if uses_granted > 0 {
        return (quantity, 0);
    }
    state.sync_redeemed_skins_if_needed();
    let is_pattern = state
        .catalog
        .components
        .get(&comp_id)
        .is_some_and(SdeSkinComponent::is_pattern);
    let key = SkinrCosmeticsState::comp_key(cid, is_pattern, comp_id);
    let already_infinite = state
        .data
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .infinite_licenses
        .contains_key(&key);
    if already_infinite {
        (0, quantity)
    } else {
        (1.min(quantity), quantity.saturating_sub(1))
    }
}

/// Grants a finite or infinite SKINR component license directly from a component item type
/// (shared by `ConsumeRequest` and `component.listing.api.InitiatePurchaseListingRequest` when
/// `immediately_activate == true`). Returns `true` when `type_id` mapped to an SDE component.
pub(super) fn grant_component_license_from_item_type(
    state: &SkinrCosmeticsState,
    queued_notices: &RwLock<Vec<(NoticeScope, GoogleAny, u64)>>,
    cid: u32,
    type_id: u32,
    quantity: u64,
    now: u64,
) -> bool {
    let Some(&(comp_id, uses_granted)) = state.catalog.item_type_to_component.get(&type_id) else {
        return false;
    };
    let is_pattern = state
        .catalog
        .components
        .get(&comp_id)
        .is_some_and(SdeSkinComponent::is_pattern);
    grant_component_license_from_item(
        state,
        queued_notices,
        cid,
        comp_id,
        is_pattern,
        uses_granted,
        quantity,
        now,
    );
    true
}

fn grant_component_license_from_item(
    state: &SkinrCosmeticsState,
    queued_notices: &RwLock<Vec<(NoticeScope, GoogleAny, u64)>>,
    cid: u32,
    comp_id: u32,
    is_pattern: bool,
    uses_granted: i32,
    quantity: u64,
    now: u64,
) {
    state.sync_redeemed_skins_if_needed();
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
        queue_component_license_granted_notice(queued_notices, cid, comp_id, is_pattern, None, now);
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
        queue_component_license_granted_notice(
            queued_notices,
            cid,
            comp_id,
            is_pattern,
            Some(added_capacity),
            now,
        );
    }
}

// ============================================================================
// Character Title Definition & Version Handlers
// ============================================================================

fn build_semver(prefix: &str, ver: [u32; 3]) -> ProtoBuilder {
    pb::message(format!("{prefix}.semanticversion.Specification"))
        .set("major", ver[0])
        .set("minor", ver[1])
        .set("patch", ver[2])
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

/// Encode character title definitions as an authentic V24.02 `'TITL'` FlatBuffer matching
/// `cosmetics.client.character.title.model.flatbuffer.generated.{Titles,Title}` (`#1126`).
///
/// Layout:
/// - File identifier: `*b"TITL"` (`TitlesBufferHasIdentifier`).
/// - Root `Titles` (1 slot): `0 -> Titles ([Title])`.
/// - `Title` (5 slots): `0 -> Id (UUID string)`, `1 -> Name (string)`,
///   `2 -> InternalName (string)`, `3 -> Visibility (uint8: 0 = AlwaysVisible)`,
///   `4 -> ReleaseDateUnixMs (int64, omitted when immediately released)`.
pub(crate) fn encode_title_definitions_flatbuffer(defs: &[TitleDefinitionRecord]) -> Vec<u8> {
    use super::quasar_achievements_flatbuffer::{uuid_bytes_to_hyphenated_string, FlatBufferWriter};

    let mut w = FlatBufferWriter::with_capacity(256 + defs.len() * 128);
    let mut title_offsets = Vec::with_capacity(defs.len());
    for def in defs {
        let uuid_bytes =
            crate::services::quasar_cosmetics_drafts::id_to_uuid_bytes(def.title_id as u64);
        let uuid_str = uuid_bytes_to_hyphenated_string(uuid_bytes);
        let id_off = w.create_string(&uuid_str);
        let name_off = w.create_string(&def.name);
        let internal_off = w.create_string(&def.internal_name);

        let mut tbl = w.start_table(5);
        w.add_offset(&mut tbl, 0, id_off);
        w.add_offset(&mut tbl, 1, name_off);
        w.add_offset(&mut tbl, 2, internal_off);
        w.add_u8(&mut tbl, 3, 0, 0);
        title_offsets.push(w.end_table(tbl));
    }
    let titles_vec = w.create_offset_vector(&title_offsets);
    let mut root = w.start_table(1);
    w.add_offset(&mut root, 0, titles_vec);
    let root_off = w.end_table(root);
    w.finish(root_off, *b"TITL")
}

fn title_language_tag(req_type: &str, dyn_msg: &DynamicQuasarMessage) -> String {
    if req_type.starts_with("eve.") {
        return match dyn_msg.get(1).and_then(as_u64) {
            Some(1) => "zh",
            Some(2) => "de",
            Some(3) => "fr",
            Some(4) => "ja",
            Some(5) => "ru",
            Some(6) => "ko",
            Some(7) => "es",
            _ => "en-us",
        }
        .to_string();
    }
    sub(dyn_msg, 3)
        .and_then(|l| str_of(l, 1))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "en-us".to_string())
}

fn handle_title_def_get_latest(
    state: &SkinrCosmeticsState,
    req_type: &str,
    dyn_msg: &DynamicQuasarMessage,
    esi_port: &std::sync::atomic::AtomicU16,
) -> HandlerResult {
    let lang = title_language_tag(req_type, dyn_msg);
    let data = state.data.read().unwrap_or_else(|e| e.into_inner());
    let ver = data.title_def_version;
    let fb = encode_title_definitions_flatbuffer(&data.title_definitions);
    drop(data);

    let crc = crate::router::cache_notifications::crc32_ieee(&fb) as u32;
    let cdn_base = crate::quasar_gateway::resolve_cdn_base_url(
        esi_port.load(std::sync::atomic::Ordering::Acquire),
    );
    let url = format!(
        "{cdn_base}/cosmetics/character/title/definitions/v{}.{}.{}/{}.fb",
        ver[0], ver[1], ver[2], lang
    );

    let prefix = if req_type.starts_with("eve.") { "eve" } else { "eve_public" };
    pb::message(format!(
        "{prefix}.cosmetic.character.title.definition.api.GetLatestResponse"
    ))
    .set("checkpoint.url", &url)
    .set("checkpoint.version", build_semver(prefix, ver))
    .set("checkpoint.crc.crc", crc)
    .to_any_or_internal_err()
}

fn handle_title_def_list(state: &SkinrCosmeticsState, _req_type: &str) -> HandlerResult {
    let data = state.data.read().unwrap_or_else(|e| e.into_inner());
    let mut rsp = pb::message("eve.cosmetic.character.title.definition.api.ListDefinitionsResponse")
        .set("version", build_semver("eve", data.title_def_version));
    for def in &data.title_definitions {
        let uuid = crate::services::quasar_cosmetics_drafts::id_to_uuid_bytes(def.title_id as u64);
        rsp = rsp.add_with("titles", |t| {
            t.set("id.uuid", uuid)
                .set("name", &def.name)
                .set("internal_name", &def.internal_name)
        });
    }
    rsp.to_any_or_internal_err()
}

fn broadcast_title_version_notice(
    queued_notices: &RwLock<Vec<(NoticeScope, GoogleAny, u64)>>,
    ver: [u32; 3],
    now_unix: u64,
) {
    crate::quasar_gateway::queue_pb_notice(
        queued_notices,
        NoticeScope::Broadcast,
        "quasar_cosmetics_skinr.broadcast_title_version_notice",
        pb::message(
            "eve_public.cosmetic.character.title.definition.version.api.CheckVersionNotice",
        )
        .set("version", build_semver("eve_public", ver)),
        now_unix,
    );
}

fn handle_title_version_set_default(
    state: &SkinrCosmeticsState,
    queued_notices: &RwLock<Vec<(NoticeScope, GoogleAny, u64)>>,
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
    state.sync_redeemed_skins_if_needed();
    state.data.write().unwrap_or_else(|e| e.into_inner()).title_def_version = ver;
    state.save();
    broadcast_title_version_notice(queued_notices, ver, ctx.now_unix());
    Ok(any_msg(
        "eve.cosmetic.character.title.definition.version.api.admin.SetDefaultVersionResponse",
        Vec::new(),
    ))
}

fn handle_title_version_override_tenant(
    state: &SkinrCosmeticsState,
    queued_notices: &RwLock<Vec<(NoticeScope, GoogleAny, u64)>>,
    ctx: &AuthoritativeContext,
    dyn_msg: &DynamicQuasarMessage,
) -> HandlerResult {
    require_admin_role(ctx)?;
    let tenant = str_of(dyn_msg, 1)
        .or_else(|| sub(dyn_msg, 1).and_then(|c| str_of(c, 1)))
        .unwrap_or_default();
    let Some(ver) = sub(dyn_msg, 2).and_then(parse_semver).filter(|v| v[0] > 0) else {
        return Err((STATUS_BAD_REQUEST, "Valid tenant and semantic version required".into()));
    };
    if tenant.is_empty() {
        return Err((STATUS_BAD_REQUEST, "Tenant name cannot be empty".into()));
    }
    state.sync_redeemed_skins_if_needed();
    {
        let mut data = state.data.write().unwrap_or_else(|e| e.into_inner());
        data.tenant_title_versions.insert(tenant, ver);
        data.title_def_version = ver;
    }
    state.save();
    broadcast_title_version_notice(queued_notices, ver, ctx.now_unix());
    Ok(any_msg(
        "eve.cosmetic.character.title.definition.version.api.admin.OverrideVersionForTenantResponse",
        Vec::new(),
    ))
}

fn handle_title_version_get_default(state: &SkinrCosmeticsState) -> HandlerResult {
    let ver = state.data.read().unwrap_or_else(|e| e.into_inner()).title_def_version;
    pb::message(
        "eve.cosmetic.character.title.definition.version.api.GetDefaultVersionResponse",
    )
    .set("version", build_semver("eve", ver))
    .to_any_or_internal_err()
}

pub fn register(router: &PublicGatewayRouter, state: Arc<SkinrCosmeticsState>) {
    register_live_skinr_state(&state);
    let st_rehydrate = Arc::clone(&state);
    let st_sync = Arc::clone(&state);
    router.register_persistence_hooks(
        Arc::new(move || st_rehydrate.rehydrate_from_db()),
        Arc::new(move || st_sync.save()),
    );
    macro_rules! reg {
        ($name:expr, $closure:expr) => {{
            let s = Arc::clone(&state);
            router.register_handler($name, Arc::new(move |ctx, any, dyn_msg| ($closure)(&s, ctx, any, dyn_msg)));
        }};
    }
    macro_rules! reg_q {
        ($name:expr, $closure:expr) => {{
            let s = Arc::clone(&state);
            let q = Arc::clone(&router.queued_notices);
            router.register_handler($name, Arc::new(move |ctx, any, dyn_msg| ($closure)(&s, &q, ctx, any, dyn_msg)));
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
    reg_q!(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
        |s: &Arc<SkinrCosmeticsState>, q, ctx, any: &GoogleAny, d| handle_finite_admin_grant(s, q, ctx, &any.value, d)
    );
    reg!(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.RevokeRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, any: &GoogleAny, d| handle_finite_admin_revoke(s, ctx, &any.value, d)
    );
    reg!(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.GetRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, any: &GoogleAny, d| handle_infinite_get(s, ctx, &any.value, d)
    );
    reg_q!(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.GrantRequest",
        |s: &Arc<SkinrCosmeticsState>, q, ctx, any: &GoogleAny, d| handle_infinite_admin_grant(s, q, ctx, &any.value, d)
    );
    reg!(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.RevokeRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, any: &GoogleAny, d| handle_infinite_admin_revoke(s, ctx, &any.value, d)
    );
    reg_q!(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest",
        |s: &Arc<SkinrCosmeticsState>, q, ctx, any: &GoogleAny, d| handle_item_consume(s, q, ctx, &any.value, d)
    );

    // 2. Character Title Definition & Version endpoints
    for req_name in [
        "eve_public.cosmetic.character.title.definition.api.GetLatestRequest",
        "eve.cosmetic.character.title.definition.api.GetLatestRequest",
    ] {
        let s = Arc::clone(&state);
        let esi_port = Arc::clone(&router.esi_port);
        router.register_handler(
            req_name,
            Arc::new(move |_ctx, _any, d| handle_title_def_get_latest(&s, req_name, d, &esi_port)),
        );
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
    reg_q!(
        "eve.cosmetic.character.title.definition.version.api.admin.SetDefaultVersionRequest",
        |s: &Arc<SkinrCosmeticsState>, q, ctx, any: &GoogleAny, d| handle_title_version_set_default(s, q, ctx, &any.value, d)
    );
    reg_q!(
        "eve.cosmetic.character.title.definition.version.api.admin.OverrideVersionForTenantRequest",
        |s: &Arc<SkinrCosmeticsState>, q, ctx, _any: &GoogleAny, d| handle_title_version_override_tenant(s, q, ctx, d)
    );
    reg!(
        "eve.cosmetic.character.title.definition.version.api.GetDefaultVersionRequest",
        |s: &Arc<SkinrCosmeticsState>, _ctx, _any: &GoogleAny, _d| handle_title_version_get_default(s)
    );
}

#[cfg(test)]
#[path = "quasar_cosmetics_skinr_tests.rs"]
mod quasar_cosmetics_skinr_tests;

