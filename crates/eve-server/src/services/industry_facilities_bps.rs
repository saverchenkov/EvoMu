//! `industry_facilities_bps` — Blueprint data, facility registry, taxes, structure rigs, and system cost indices (`E-4`).

// parity: plan:E-4
// parity: feat:industry.blueprint-data, feat:industry.blueprint-limits, feat:industry.facilities
// parity: feat:industry.facility-tax, feat:industry.job-cost, feat:industry.cost-index
// parity: feat:industry.structure-rigs, feat:structures.service-industry, feat:structures.service-reactions
// parity: rpc:blueprintManager.GetBlueprintDataByOwner

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

pub const ATTR_BLUEPRINT_ME: u16 = 1112;
pub const ATTR_BLUEPRINT_TE: u16 = 1113;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct IndustryJobCostMeta {
    pub eiv_isk: f64,
    pub system_cost_index: f64,
    pub structure_cost_modifier: f64,
    pub base_job_cost_isk: f64,
    pub facility_tax_rate: f64,
    pub facility_tax_isk: f64,
    pub scc_surcharge_isk: f64,
    #[serde(default)]
    pub alpha_clone_tax_isk: f64,
    pub total_cost_isk: f64,
    #[serde(default)]
    pub decryptor_type_id: Option<TypeId>,
    #[serde(default)]
    pub product_type_id: TypeId,
    #[serde(default)]
    pub invented_runs: i32,
    #[serde(default)]
    pub invented_me: f64,
    #[serde(default)]
    pub invented_te: f64,
    #[serde(default)]
    pub inv_prob: f64,
    #[serde(default)]
    pub use_corp: bool,
    #[serde(default)]
    pub owner_id: i64,
    #[serde(default)]
    pub paused_at_unix: Option<u64>,
    #[serde(default)]
    pub blueprint_original_flag: Option<eve_db::InventoryFlag>,
    /// Consumed BOM quantities recorded in `i64` so multi-run jobs above `i32::MAX` never truncate.
    #[serde(default)]
    pub consumed_materials: Vec<(TypeId, i64)>,
    #[serde(default)]
    pub blueprint_location_id: i64,
    #[serde(default)]
    pub output_location_id: i64,
    #[serde(default)]
    pub location_id: i64,
    #[serde(default)] pub output_flag: Option<eve_db::InventoryFlag>,
    #[serde(default)] pub ready_before_offline: bool,
    #[serde(default)]
    pub completed_unix: Option<u64>,
    #[serde(default)]
    pub completed_character_id: Option<i64>,
}

#[path = "industry_facilities_bps_helpers.rs"]
mod industry_facilities_bps_helpers;
#[allow(unused_imports)]
pub use industry_facilities_bps_helpers::{
    facility_tax_note_id, is_fulcrum_pirate_ship_bonus_applicable,
    is_structure_industry_service_online, is_structure_industry_service_online_for_bp,
    resolve_facility_offline_moment_opt, sync_industry_job_pause_states,
    sync_industry_job_pause_states_on_service_change,
};

/// Resolves a requested facility ID or falls back to caller's active/home station or structure from session/DB.
pub(crate) fn resolve_caller_facility_id(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    requested: Option<i64>,
) -> i64 {
    requested
        .or(session.station_id)
        .or(session.structure_id)
        .or_else(|| {
            ctx.db.with_snapshot(|snap| {
                snap.characters.get(&char_id).and_then(|c| {
                    c.station_id
                        .or(c.structure_id)
                        .or_else(|| (c.home_station_id > 0).then_some(c.home_station_id))
                })
            })
        })
        .unwrap_or(0)
}

/// Fixed NPC station industry facility tax rate (0.25% per `wiki:Manufacturing`), shared with ESI `/industry/facilities` and `InstallJob`.
pub(crate) const NPC_STATION_INDUSTRY_TAX_RATE: f64 = 0.0025;

pub fn load_facility_taxes(snap: &eve_db::WorldStateSnapshot, facility_id: i64) -> HashMap<u8, f64> {
    load_facility_taxes_with_sde(None, snap, facility_id)
}

pub fn load_facility_taxes_with_sde(
    _sde: Option<&eve_sde::SdeRegistry>,
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
) -> HashMap<u8, f64> {
    let nid = facility_tax_note_id(facility_id);
    if let Some(note) = snap.owner_notes.get(&nid) {
        if let Ok(map) = serde_json::from_str::<HashMap<u8, f64>>(&note.note) {
            return map;
        }
    }
    let is_upwell = snap.structures.contains_key(&facility_id)
        || snap
            .items
            .get(&facility_id)
            .is_some_and(|it| matches!(it.type_id, 35825 | 35826 | 35827 | 35835 | 35836));
    let default_rate = if is_upwell {
        0.05
    } else {
        NPC_STATION_INDUSTRY_TAX_RATE
    };
    [1_u8, 3, 4, 5, 8, 9]
        .into_iter()
        .map(|act| (act, default_rate))
        .collect()
}

pub fn has_custom_facility_taxes(snap: &eve_db::WorldStateSnapshot, facility_id: i64) -> bool {
    snap.owner_notes.contains_key(&facility_tax_note_id(facility_id))
        || snap.structures.contains_key(&facility_id)
        || snap
            .items
            .get(&facility_id)
            .map(|it| matches!(it.type_id, 35825 | 35826 | 35827 | 35835 | 35836))
            .unwrap_or(false)
}

pub fn save_facility_taxes(
    snap: &mut eve_db::WorldStateSnapshot,
    facility_id: i64,
    taxes: &HashMap<u8, f64>,
) {
    let nid = facility_tax_note_id(facility_id);
    if let Ok(js) = serde_json::to_string(taxes) {
        snap.owner_notes.insert(
            nid,
            eve_db::OwnerNoteRecord {
                note_id: nid,
                character_id: -2,
                item_id: facility_id,
                label: format!("facility_tax:{facility_id}"),
                note: js,
                timestamp_unix: 0,
            },
        );
    }
}

pub fn job_cost_note_id(job_id: i64) -> i64 {
    -(2_000_000_000_000_i64.saturating_add(job_id.abs()))
}

pub fn save_job_cost_meta(
    snap: &mut eve_db::WorldStateSnapshot,
    job_id: i64,
    meta: &IndustryJobCostMeta,
) {
    let nid = job_cost_note_id(job_id);
    if let Ok(js) = serde_json::to_string(meta) {
        snap.owner_notes.insert(
            nid,
            eve_db::OwnerNoteRecord {
                note_id: nid,
                character_id: -3,
                item_id: job_id,
                label: format!("ind_job_cost:{job_id}"),
                note: js,
                timestamp_unix: 0,
            },
        );
    }
}

pub fn load_job_cost_meta(
    snap: &eve_db::WorldStateSnapshot,
    job_id: i64,
) -> Option<IndustryJobCostMeta> {
    let nid = job_cost_note_id(job_id);
    snap.owner_notes
        .get(&nid)
        .and_then(|n| serde_json::from_str::<IndustryJobCostMeta>(&n.note).ok())
}

pub use crate::services::industry_pi::industry_rigs::*;

/// Computes blueprint Estimated Item Value (EIV) per run from SDE manufacturing or reaction BOM (`feat:industry.eiv-adjusted-prices`).
pub fn compute_blueprint_eiv_per_run(ctx: &ServerContext, bp: &eve_sde::BlueprintInfo, activity_id: u8) -> f64 {
    compute_blueprint_eiv_per_run_with_snap(ctx, None, bp, activity_id)
}

pub fn compute_blueprint_eiv_per_run_with_snap(
    ctx: &ServerContext,
    snap: Option<&eve_db::WorldStateSnapshot>,
    bp: &eve_sde::BlueprintInfo,
    activity_id: u8,
) -> f64 {
    let mats: Vec<(TypeId, u64)> = if activity_id == 9 {
        bp.activities
            .get("reaction")
            .or_else(|| bp.activities.get("9"))
            .map(|a| a.materials.iter().map(|m| (m.type_id, m.quantity)).collect())
            .unwrap_or_default()
    } else if !bp.materials.is_empty() {
        bp.materials.iter().map(|m| (m.type_id, m.quantity)).collect()
    } else {
        bp.activities
            .get("manufacturing")
            .or_else(|| bp.activities.get("1"))
            .map(|a| a.materials.iter().map(|m| (m.type_id, m.quantity)).collect())
            .unwrap_or_default()
    };
    let sum = mats.into_iter()
        .map(|(tid, qty)| {
            let unit_price = crate::services::industry_pi::industry_rigs::resolve_material_adjusted_price(ctx, snap, tid);
            (qty as f64) * unit_price
        })
        .sum::<f64>();
    if sum > 0.0 {
        sum
    } else {
        ctx.sde.types.get(&bp.product_type_id).map(|t| t.base_price.max(0.0)).unwrap_or(0.0)
    }
}

pub fn compute_industry_job_costs(ctx: &ServerContext, snap: &eve_db::WorldStateSnapshot, bp_type_id: TypeId, activity_id: u8, runs: u32, facility_id: i64) -> Result<IndustryJobCostMeta, BridgeRpcError> {
    compute_industry_job_costs_for_char(ctx, snap, None, bp_type_id, activity_id, runs, facility_id, false)
}

pub fn compute_industry_job_costs_ext(ctx: &ServerContext, snap: &eve_db::WorldStateSnapshot, bp_type_id: TypeId, activity_id: u8, runs: u32, facility_id: i64, is_alpha_clone: bool) -> Result<IndustryJobCostMeta, BridgeRpcError> {
    compute_industry_job_costs_for_char(ctx, snap, None, bp_type_id, activity_id, runs, facility_id, is_alpha_clone)
}

/// Canonical System Cost Index (SCI) formula from wiki:Manufacturing.md:70,88:
/// SCI = sqrt(system_work_hours / universe_work_hours over 28-day rolling window).
/// Returns standard base SCI (0.01) when universe/system work is zero.
pub fn compute_canonical_system_cost_index(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    sys_id: u32,
    activity_id: u8,
) -> f64 {
    let cutoff = ctx.now_unix().saturating_sub(28 * 86_400);
    let mut sys_work: u64 = 0;
    let mut univ_work: u64 = 0;
    for j in snap.industry_jobs.values() {
        if j.status.eq_ignore_ascii_case("Cancelled") || j.status.eq_ignore_ascii_case("Reverted") {
            continue;
        }
        let act_match = if activity_id == 9 { matches!(j.activity_id, 9 | 11) } else { j.activity_id == activity_id };
        if !act_match || (j.start_unix > 0 && j.start_unix < cutoff) {
            continue;
        }
        let job_sys = resolve_facility_modifiers_and_system(ctx, snap, j.facility_id).map(|m| m.solar_system_id);
        let work = j.duration_seconds;
        if job_sys == Some(sys_id) {
            sys_work = sys_work.saturating_add(work);
        }
        if job_sys.is_some() {
            univ_work = univ_work.saturating_add(work);
        }
    }
    if univ_work > 0 && sys_work > 0 {
        // DERIVED: floor/ceiling clamp (0.001 to 0.50) bounds System Cost Index against empty universe or extreme saturation.
        ((sys_work as f64) / (univ_work as f64)).sqrt().clamp(0.001, 0.50)
    } else {
        0.01
    }
}

pub fn compute_industry_job_costs_for_char(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    char_id: Option<i64>,
    bp_type_id: TypeId,
    activity_id: u8,
    runs: u32,
    facility_id: i64,
    is_alpha_clone: bool,
) -> Result<IndustryJobCostMeta, BridgeRpcError> {
    compute_industry_job_costs_with_step(ctx, snap, char_id, bp_type_id, activity_id, 0, runs, facility_id, is_alpha_clone)
}

#[allow(clippy::too_many_arguments)]
pub fn compute_industry_job_costs_with_step(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    char_id: Option<i64>,
    bp_type_id: TypeId,
    activity_id: u8,
    start_step: usize,
    runs: u32,
    facility_id: i64,
    is_alpha_clone: bool,
) -> Result<IndustryJobCostMeta, BridgeRpcError> {
    let Some(bp) = ctx.sde.blueprints.get(&bp_type_id) else {
        return Err(BridgeRpcError {
            code: "UnknownBlueprint".into(),
            message: format!("Blueprint type {bp_type_id} not found in SDE"),
            client_error: None,
        });
    };
    let eiv_per_run = compute_blueprint_eiv_per_run_with_snap(ctx, Some(snap), bp, activity_id);
    let total_eiv = eiv_per_run * crate::services::industry_pi::compute_activity_ptv_multiplier(activity_id, start_step, runs);
    let fallback_sys = char_id
        .and_then(|cid| snap.characters.get(&cid).map(|c| c.solar_system_id))
        .unwrap_or_else(|| ctx.sde.solar_systems.keys().copied().next().unwrap_or(0));
    let target_prod_tid = resolve_blueprint_activity_product(bp, activity_id);
    let (sys_id, structure_cost_modifier) =
        resolve_facility_modifiers_and_system_for_job(
            ctx,
            snap,
            facility_id,
            Some(activity_id),
            target_prod_tid,
        )
        .map_or((fallback_sys, 1.0), |f| (f.solar_system_id, f.cost_modifier));
    let system_cost_index = compute_canonical_system_cost_index(ctx, snap, sys_id, activity_id);
    let base_job_cost_isk =
        ((total_eiv * system_cost_index * structure_cost_modifier) * 100.0).round() / 100.0;
    let acl_tax_override = if snap.structures.contains_key(&facility_id) {
        if let Some(cid) = char_id {
            crate::services::structure_profile_services::check_industry_activity_acl_and_tax(
                snap,
                facility_id,
                cid,
                activity_id,
                ctx.now_unix(),
            )?
        } else {
            None
        }
    } else {
        None
    };
    let taxes = load_facility_taxes_with_sde(Some(&ctx.sde), snap, facility_id);
    let facility_tax_rate = acl_tax_override.unwrap_or_else(|| {
        taxes
            .get(&activity_id)
            .or_else(|| taxes.get(&1))
            .copied()
            .unwrap_or(NPC_STATION_INDUSTRY_TAX_RATE)
    });
    let raw_facility_tax_isk = ((total_eiv * facility_tax_rate) * 100.0).round() / 100.0;
    let is_enemy_militia = char_id
        .and_then(|cid| snap.characters.get(&cid))
        .and_then(|c| c.war_faction_id)
        .zip(snap.fw_system_occupations.get(&sys_id).map(|o| o.occupier_faction_id))
        .is_some_and(|(cf, of)| matches!((cf, of), (500_001, 500_004) | (500_004, 500_001) | (500_002, 500_003) | (500_003, 500_002)));
    let fw_tier = if is_enemy_militia {
        0
    } else {
        snap.fw_runtime.ihub_tiers.get(&sys_id).copied().unwrap_or(0)
    };
    let facility_tax_isk = if fw_tier > 0 {
        ((crate::services::industry_pi::compute_facility_tax_bonus_for_tier(raw_facility_tax_isk, fw_tier)) * 100.0).round() / 100.0
    } else {
        raw_facility_tax_isk
    };
    // Viridian/Equinox 4.0% SCC surcharge (reduced by 90% to 0.4% at Fulcrum for pirate subcaps)
    let fulcrum_bonus =
        is_fulcrum_pirate_ship_bonus_applicable(ctx, snap, char_id, bp_type_id, activity_id, facility_id);
    let scc_rate = if fulcrum_bonus { 0.004 } else { 0.04 };
    let scc_surcharge_isk = ((total_eiv * scc_rate) * 100.0).round() / 100.0;
    let alpha_clone_tax_isk = if is_alpha_clone {
        ((total_eiv * 0.0025) * 100.0).round() / 100.0
    } else {
        0.0
    };
    let total_cost_isk = ((base_job_cost_isk
        + facility_tax_isk
        + scc_surcharge_isk
        + alpha_clone_tax_isk)
        * 100.0)
        .round()
        / 100.0;
    Ok(IndustryJobCostMeta {
        eiv_isk: (eiv_per_run * 100.0).round() / 100.0,
        system_cost_index,
        structure_cost_modifier,
        base_job_cost_isk,
        facility_tax_rate,
        facility_tax_isk,
        scc_surcharge_isk,
        alpha_clone_tax_isk,
        total_cost_isk,
        ..Default::default()
    })
}

/// Conversion trait for strongly-typed V24 client wire replies (`RFC-3 C-9`, `#933`).
pub use crate::router::ToClient;

const INDUSTRY_CATEGORIES: &[i64] = &[
    4, 5, 6, 7, 8, 9, 16, 17, 18, 20, 22, 23, 24, 25, 32, 34, 35, 39, 40, 41, 42, 43, 46, 65, 66,
    87, 91,
];

/// Typed V24 reply payload for `facilityManager.GetFacilities`, `GetFacilitiesByID`, and `GetFacility`.
#[derive(Debug, Clone)]
pub struct FacilityRep {
    pub facility_id: i64,
    pub type_id: i64,
    pub owner_id: i64,
    pub solar_system_id: i64,
    pub region_id: i64,
    pub tax: f64,
    pub tax_by_activity: [(u8, f64); 6],
    pub service_access: Vec<(i64, f64)>,
    pub scc_tax_modifier: f64,
    pub system_cost_indices: Vec<(u8, f64)>,
    pub online: bool,
    pub material_modifier: f64,
    pub time_modifier: f64,
    pub cost_modifier: f64,
    pub rig_material_bonus: f64,
    pub rig_time_bonus: f64,
    pub rig_cost_bonus: f64,
    pub rig_security_multiplier: f64,
    pub system_cost_index: f64,
    pub activity_ids: Vec<i64>,
}

impl ToClient for FacilityRep {
    fn into_client_rep(self) -> PyRep {
        let tax_for = |act: u8| -> f64 {
            self.tax_by_activity
                .iter()
                .find(|(a, _)| *a == act)
                .map(|(_, r)| *r)
                .unwrap_or(self.tax)
        };
        let tax_kv = PyRep::keyval([
            ("1", PyRep::Real(tax_for(1))),
            ("3", PyRep::Real(tax_for(3))),
            ("4", PyRep::Real(tax_for(4))),
            ("5", PyRep::Real(tax_for(5))),
            ("8", PyRep::Real(tax_for(8))),
            ("9", PyRep::Real(tax_for(9))),
            ("manufacturing", PyRep::Real(tax_for(1))),
            ("reaction", PyRep::Real(tax_for(9))),
        ]);
        let service_access = self
            .service_access
            .into_iter()
            .map(|(sid, rate)| (PyRep::Integer(sid), PyRep::Real(rate)))
            .collect();
        let sys_sci_entries = self
            .system_cost_indices
            .into_iter()
            .map(|(act, sci)| (PyRep::Integer(i64::from(act)), PyRep::Real(sci)))
            .collect();
        let global_modifiers = PyRep::Dict(vec![
            (PyRep::String("faction".into()), PyRep::Real(1.0)),
            (PyRep::String("system".into()), PyRep::Dict(sys_sci_entries)),
        ]);
        let cats: Vec<PyRep> = INDUSTRY_CATEGORIES
            .iter()
            .map(|&c| PyRep::Integer(c))
            .collect();
        let activities = self
            .activity_ids
            .into_iter()
            .map(|act_id| {
                let tuple = PyRep::Tuple(vec![
                    PyRep::List(Vec::new()),
                    PyRep::List(Vec::new()),
                    PyRep::List(Vec::new()),
                    PyRep::List(cats.clone()),
                    PyRep::List(Vec::new()),
                    PyRep::List(Vec::new()),
                ]);
                (PyRep::Integer(act_id), tuple)
            })
            .collect();
        PyRep::keyval([
            ("facilityID", PyRep::Integer(self.facility_id)),
            ("typeID", PyRep::Integer(self.type_id)),
            ("ownerID", PyRep::Integer(self.owner_id)),
            ("solarSystemID", PyRep::Integer(self.solar_system_id)),
            ("regionID", PyRep::Integer(self.region_id)),
            ("tax", PyRep::Real(self.tax)),
            ("taxRates", tax_kv),
            ("serviceAccess", PyRep::Dict(service_access)),
            ("sccTaxModifier", PyRep::Real(self.scc_tax_modifier)),
            ("rigModifiers", PyRep::Dict(Vec::new())),
            ("globalModifiers", global_modifiers),
            ("online", PyRep::Bool(self.online)),
            ("materialMultiplier", PyRep::Real(self.material_modifier)),
            ("timeMultiplier", PyRep::Real(self.time_modifier)),
            ("costMultiplier", PyRep::Real(self.cost_modifier)),
            ("materialModifier", PyRep::Real(self.material_modifier)),
            ("timeModifier", PyRep::Real(self.time_modifier)),
            ("costModifier", PyRep::Real(self.cost_modifier)),
            ("rigMaterialBonus", PyRep::Real(self.rig_material_bonus)),
            ("rigTimeBonus", PyRep::Real(self.rig_time_bonus)),
            ("rigCostBonus", PyRep::Real(self.rig_cost_bonus)),
            ("rigSecurityMultiplier", PyRep::Real(self.rig_security_multiplier)),
            ("systemCostIndex", PyRep::Real(self.system_cost_index)),
            ("activities", PyRep::Dict(activities)),
        ])
    }
}

/// Typed V24 `industry.job.Location` reply entry for `facilityManager.GetFacilityLocations` (`#934`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FacilityLocationRep {
    pub item_id: i64,
    pub facility_id: i64,
    pub type_id: i64,
    pub owner_id: i64,
    pub flag_id: i64,
    pub solar_system_id: i64,
    pub region_id: i64,
    pub can_view: bool,
    pub can_take: bool,
}

impl ToClient for FacilityLocationRep {
    fn into_client_rep(self) -> PyRep {
        let state_dict = PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(self.item_id)),
            (PyRep::String("facilityID".into()), PyRep::Integer(self.facility_id)),
            (PyRep::String("typeID".into()), PyRep::Integer(self.type_id)),
            (PyRep::String("ownerID".into()), PyRep::Integer(self.owner_id)),
            (PyRep::String("flagID".into()), PyRep::Integer(self.flag_id)),
            (PyRep::String("solarSystemID".into()), PyRep::Integer(self.solar_system_id)),
            (PyRep::String("regionID".into()), PyRep::Integer(self.region_id)),
            (PyRep::String("canView".into()), PyRep::Bool(self.can_view)),
            (PyRep::String("canTake".into()), PyRep::Bool(self.can_take)),
        ]);
        PyRep::ObjectEx {
            is_type_2: true,
            header: Box::new(PyRep::Tuple(vec![
                PyRep::Tuple(vec![PyRep::Token("industry.job.Location".into())]),
                state_dict,
            ])),
            list: Vec::new(),
            dict: Vec::new(),
        }
    }
}

/// Typed material requirement entry on a `BlueprintInstanceRep`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlueprintMaterialRep {
    pub type_id: i64,
    pub quantity: i64,
}

impl ToClient for BlueprintMaterialRep {
    fn into_client_rep(self) -> PyRep {
        PyRep::keyval([
            ("typeID", PyRep::Integer(self.type_id)),
            ("quantity", PyRep::Integer(self.quantity)),
        ])
    }
}

/// Typed skill requirement entry on a `BlueprintInstanceRep`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlueprintSkillRep {
    pub type_id: i64,
    pub level: i64,
}

impl ToClient for BlueprintSkillRep {
    fn into_client_rep(self) -> PyRep {
        PyRep::keyval([
            ("typeID", PyRep::Integer(self.type_id)),
            ("level", PyRep::Integer(self.level)),
        ])
    }
}

/// Extra timing and job-cost fields included on single-blueprint `blueprintManager.GetBlueprintData` replies.
#[derive(Debug, Clone)]
pub struct BlueprintJobDetailsRep {
    pub max_production_limit: i64,
    pub manufacturing_time: i64,
    pub research_material_time: i64,
    pub research_time_time: i64,
    pub copying_time: i64,
    pub invention_time: i64,
    pub reaction_time: i64,
    pub base_job_cost: f64,
    pub facility_tax_isk: f64,
    pub scc_surcharge_isk: f64,
    pub total_job_cost_isk: f64,
}

/// Typed V24 `BlueprintInstance` reply payload for `blueprintManager.GetBlueprintData` and `GetBlueprintDataByOwner`.
#[derive(Debug, Clone)]
pub struct BlueprintInstanceRep {
    pub item_id: i64,
    pub type_id: i64,
    pub product_type_id: i64,
    pub owner_id: i64,
    pub facility_id: Option<i64>,
    pub location_id: i64,
    pub location_type_id: i64,
    pub flag_id: i64,
    pub solar_system_id: i64,
    pub material_efficiency: i64,
    pub time_efficiency: i64,
    pub original: bool,
    pub runs: i64,
    pub quantity: i64,
    pub job_id: Option<i64>,
    pub is_impounded: bool,
    pub materials: Vec<BlueprintMaterialRep>,
    pub skills: Vec<BlueprintSkillRep>,
    pub estimated_item_value_isk: f64,
    pub details: Option<BlueprintJobDetailsRep>,
}

impl ToClient for BlueprintInstanceRep {
    fn into_client_rep(self) -> PyRep {
        let mats = self
            .materials
            .into_iter()
            .map(ToClient::into_client_rep)
            .collect();
        let skills = self
            .skills
            .into_iter()
            .map(ToClient::into_client_rep)
            .collect();
        let mut entries: Vec<(&'static str, PyRep)> = vec![
            ("blueprintID", PyRep::Integer(self.item_id)),
            ("itemID", PyRep::Integer(self.item_id)),
            ("typeID", PyRep::Integer(self.type_id)),
            ("blueprintTypeID", PyRep::Integer(self.type_id)),
            ("productTypeID", PyRep::Integer(self.product_type_id)),
            ("ownerID", PyRep::Integer(self.owner_id)),
            (
                "facilityID",
                self.facility_id.map(PyRep::Integer).unwrap_or(PyRep::None),
            ),
            ("locationID", PyRep::Integer(self.location_id)),
            ("locationTypeID", PyRep::Integer(self.location_type_id)),
            ("locationFlagID", PyRep::Integer(self.flag_id)),
            ("flagID", PyRep::Integer(self.flag_id)),
            ("solarSystemID", PyRep::Integer(self.solar_system_id)),
            ("materialEfficiency", PyRep::Integer(self.material_efficiency)),
            ("timeEfficiency", PyRep::Integer(self.time_efficiency)),
            ("original", PyRep::Bool(self.original)),
            ("copy", PyRep::Bool(!self.original)),
            ("runs", PyRep::Integer(self.runs)),
            ("runsRemaining", PyRep::Integer(self.runs)),
            ("quantity", PyRep::Integer(self.quantity)),
            ("jobID", self.job_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
            ("isImpounded", PyRep::Bool(self.is_impounded)),
            ("materials", PyRep::List(mats)),
            ("skills", PyRep::List(skills)),
            ("estimatedItemValueISK", PyRep::Real(self.estimated_item_value_isk)),
        ];
        if let Some(d) = self.details {
            entries.extend([
                ("maxProductionLimit", PyRep::Integer(d.max_production_limit)),
                ("manufacturingTime", PyRep::Integer(d.manufacturing_time)),
                ("researchMaterialTime", PyRep::Integer(d.research_material_time)),
                ("researchTimeTime", PyRep::Integer(d.research_time_time)),
                ("copyingTime", PyRep::Integer(d.copying_time)),
                ("inventionTime", PyRep::Integer(d.invention_time)),
                ("reactionTime", PyRep::Integer(d.reaction_time)),
                ("baseJobCost", PyRep::Real(d.base_job_cost)),
                ("facilityTaxISK", PyRep::Real(d.facility_tax_isk)),
                ("sccSurchargeISK", PyRep::Real(d.scc_surcharge_isk)),
                ("totalJobCostISK", PyRep::Real(d.total_job_cost_isk)),
            ]);
        }
        PyRep::keyval(entries)
    }
}

/// Typed V24 `(blueprints, facility_counts)` tuple reply for `blueprintManager.GetBlueprintDataByOwner`.
#[derive(Debug, Clone)]
pub struct BlueprintDataByOwnerRep {
    pub blueprints: Vec<BlueprintInstanceRep>,
    pub facility_counts: Vec<(Option<i64>, i64)>,
}

impl ToClient for BlueprintDataByOwnerRep {
    fn into_client_rep(self) -> PyRep {
        let bps = self
            .blueprints
            .into_iter()
            .map(ToClient::into_client_rep)
            .collect();
        let counts = self
            .facility_counts
            .into_iter()
            .map(|(f, c)| (f.map(PyRep::Integer).unwrap_or(PyRep::None), PyRep::Integer(c)))
            .collect();
        PyRep::Tuple(vec![PyRep::List(bps), PyRep::Dict(counts)])
    }
}

fn build_blueprint_materials_skills_product(
    bp: &eve_sde::BlueprintInfo,
    is_reaction: bool,
) -> (Vec<BlueprintMaterialRep>, Vec<BlueprintSkillRep>, TypeId) {
    let materials = if is_reaction {
        bp.activities
            .get("reaction")
            .map(|a| {
                a.materials
                    .iter()
                    .map(|m| BlueprintMaterialRep {
                        type_id: i64::from(m.type_id),
                        quantity: m.quantity as i64,
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else {
        bp.materials
            .iter()
            .map(|m| BlueprintMaterialRep {
                type_id: i64::from(m.type_id),
                quantity: m.quantity as i64,
            })
            .collect()
    };
    let mut skills: Vec<BlueprintSkillRep> = bp
        .activities
        .get(if is_reaction { "reaction" } else { "manufacturing" })
        .or_else(|| bp.activities.get("1"))
        .map(|act| {
            act.skills
                .iter()
                .map(|sk| BlueprintSkillRep {
                    type_id: i64::from(sk.type_id),
                    level: i64::from(sk.level),
                })
                .collect()
        })
        .unwrap_or_default();
    if skills.is_empty() {
        skills.push(BlueprintSkillRep {
            type_id: 3380,
            level: 1,
        });
    }
    let product_type_id = if is_reaction {
        bp.activities
            .get("reaction")
            .and_then(|a| a.products.first())
            .map(|p| p.type_id)
            .unwrap_or(bp.product_type_id)
    } else {
        bp.product_type_id
    };
    (materials, skills, product_type_id)
}

pub fn build_facility_pyrep(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
) -> Option<PyRep> {
    let fac = resolve_facility_modifiers_and_system(ctx, snap, facility_id)?;
    let type_id = fac.type_id;
    let owner_id = fac.owner_corp_id;
    let sys_id = fac.solar_system_id;
    let region_id = fac.region_id;
    let mat_mod = fac.material_modifier;
    let time_mod = fac.time_modifier;
    let cost_mod = fac.cost_modifier;
    let rigs = resolve_structure_rig_bonuses_full(ctx, snap, facility_id, sys_id);
    let rig_mat = rigs.mat_bonus;
    let rig_time = rigs.time_bonus;
    let rig_cost = rigs.cost_bonus;
    let rig_sec_mult = rigs.sec_multiplier;
    let taxes = load_facility_taxes_with_sde(Some(&ctx.sde), snap, facility_id);
    let mfg_tax = taxes.get(&1).copied().unwrap_or(0.0025);
    let sys_cost_index = compute_canonical_system_cost_index(ctx, snap, sys_id, 1);
    let sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(1.0);
    let supports_reactions = matches!(type_id, 35835 | 35836) && sec < 0.45;
    let mut activity_ids = vec![1_i64, 3, 4, 5, 8];
    if supports_reactions {
        activity_ids.push(9);
    }
    let tax_for_act = |act: u8| taxes.get(&act).copied().unwrap_or(mfg_tax);
    let mut service_access = vec![
        (20_i64, tax_for_act(1)),
        (21, tax_for_act(1)),
        (22, tax_for_act(1)),
        (23, tax_for_act(4)),
        (24, tax_for_act(3)),
        (25, tax_for_act(5)),
        (26, tax_for_act(8)),
        (30, tax_for_act(3)),
        (31, tax_for_act(4)),
        (32, tax_for_act(5)),
        (33, tax_for_act(8)),
        (34, tax_for_act(8)),
    ];
    if supports_reactions {
        for sid in [27_i64, 28, 29, 40, 41, 42, 43] {
            service_access.push((sid, tax_for_act(9)));
        }
    }
    let system_cost_indices = [1_u8, 3, 4, 5, 8, 9, 11]
        .into_iter()
        .map(|act| (act, compute_canonical_system_cost_index(ctx, snap, sys_id, act)))
        .collect();
    let online = is_structure_industry_service_online(snap, facility_id, 1);
    Some(
        FacilityRep {
            facility_id,
            type_id: i64::from(type_id),
            owner_id,
            solar_system_id: i64::from(sys_id),
            region_id: i64::from(region_id),
            tax: mfg_tax,
            tax_by_activity: [
                (1, tax_for_act(1)),
                (3, tax_for_act(3)),
                (4, tax_for_act(4)),
                (5, tax_for_act(5)),
                (8, tax_for_act(8)),
                (9, tax_for_act(9)),
            ],
            service_access,
            scc_tax_modifier: 1.0,
            system_cost_indices,
            online,
            material_modifier: mat_mod * (1.0 - rig_mat),
            time_modifier: time_mod,
            cost_modifier: cost_mod,
            rig_material_bonus: rig_mat,
            rig_time_bonus: rig_time,
            rig_cost_bonus: rig_cost,
            rig_security_multiplier: rig_sec_mult,
            system_cost_index: sys_cost_index,
            activity_ids,
        }
        .into_client_rep(),
    )
}

fn resolve_bp_facility_loc(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    loc_id: i64,
) -> (Option<i64>, i64, Option<i64>) {
    let loc_type_id = ctx
        .sde
        .stations
        .get(&loc_id)
        .map(|s| s.station_type_id as i64)
        .or_else(|| snap.structures.get(&loc_id).map(|s| s.type_id as i64))
        .or_else(|| snap.items.get(&loc_id).map(|p| p.type_id as i64))
        .or_else(|| u32::try_from(loc_id).ok().filter(|s| ctx.sde.solar_systems.contains_key(s)).map(|_| 5))
        .unwrap_or(0);
    let mut cur = loc_id;
    for _ in 0..4 {
        if let Some(st) = ctx.sde.stations.get(&cur) {
            return (Some(cur), loc_type_id, Some(st.solar_system_id as i64));
        }
        if let Some(st) = snap.structures.get(&cur) {
            return (Some(cur), loc_type_id, Some(st.solar_system_id as i64));
        }
        if let Some(sys) = u32::try_from(cur).ok().filter(|s| ctx.sde.solar_systems.contains_key(s)) {
            return (None, loc_type_id, Some(sys as i64));
        }
        if let Some(p) = snap.items.get(&cur) {
            let cat = ctx.sde.types.get(&p.type_id).map(|t| t.category_id).unwrap_or(0);
            let sys_opt = ctx.sde.stations.get(&p.location_id).map(|s| s.solar_system_id as i64)
                .or_else(|| snap.structures.get(&p.location_id).map(|s| s.solar_system_id as i64))
                .or_else(|| ctx.sde.location_solar_system(p.location_id).map(|s| s as i64));
            if cat == 65 || matches!(p.type_id, 35825..=35836) {
                return (Some(cur), loc_type_id, sys_opt);
            }
            if cat == 6 || p.location_id == cur || p.location_id <= 0 {
                return (None, loc_type_id, sys_opt);
            }
            cur = p.location_id;
            continue;
        }
        if (60_000_000..64_000_000).contains(&cur) {
            return (Some(cur), loc_type_id, ctx.sde.location_solar_system(cur).map(|s| s as i64));
        }
        break;
    }
    (None, loc_type_id, None)
}

pub fn handle_industry_facilities_bps(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("blueprintManager", "GetBlueprintData")
        | ("blueprintManager", "GetBlueprintDataByOwner") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if method == "GetBlueprintDataByOwner" {
                let owner_id = args.first().and_then(|a| a.as_i64()).or_else(|| kwargs.get("ownerID").and_then(|v| v.as_i64())).unwrap_or(char_id);
                let facility_filter = args.get(1).and_then(|a| a.as_i64()).or_else(|| kwargs.get("facilityID").and_then(|v| v.as_i64())).filter(|&id| id > 0);
                let res = ctx.db.with_snapshot(|snap| -> Result<PyRep, BridgeRpcError> {
                    if owner_id != char_id && !session.has_gm_role() {
                        const CORP_BP_ROLES: u64 = eve_db::corp_roles::ROLE_DIRECTOR
                            | eve_db::corp_roles::ROLE_FACTORY_MANAGER
                            | eve_db::corp_roles::ROLE_RENT_FACTORY_FACILITY
                            | eve_db::corp_roles::ROLE_RENT_RESEARCH_FACILITY;
                        let caller_corp_id = session.corporation_id.or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id)).unwrap_or(0);
                        let (is_ceo, eff_roles) = snap.effective_corp_roles_at(
                            owner_id,
                            char_id,
                            facility_filter,
                            session.corp_role,
                            ctx.now_unix(),
                        );
                        let has_corp_access = owner_id == caller_corp_id
                            && !(1_000_000..2_000_000).contains(&owner_id)
                            && (is_ceo || (eff_roles & CORP_BP_ROLES) != 0);
                        if !has_corp_access {
                            return Err(BridgeRpcError { code: "OwnerAccessDenied".into(), message: format!("Character {char_id} is not authorized to query blueprints for owner {owner_id}"), client_error: None });
                        }
                    }
                    let mut active_jobs: HashMap<i64, i64> = HashMap::new();
                    for j in snap.industry_jobs.values() {
                        if !matches!(j.status.as_str(), "Delivered" | "Completed" | "Cancelled" | "Reverted" | "Failed") {
                            active_jobs.entry(j.blueprint_item_id).or_insert(j.job_id);
                        }
                    }
                    let mut owned: Vec<&eve_db::InventoryItemRecord> = snap.items.values()
                        .filter(|it| {
                            it.owner_id == owner_id
                                && it.flag != eve_db::InventoryFlag::Impounded
                                && ctx.sde.blueprints.contains_key(&it.type_id)
                        })
                        .collect();
                    owned.sort_by_key(|it| it.item_id);
                    let mut counts: std::collections::BTreeMap<Option<i64>, i64> = std::collections::BTreeMap::new();
                    let mut bps: Vec<BlueprintInstanceRep> = Vec::new();
                    for it in owned {
                        let Some(bp) = ctx.sde.blueprints.get(&it.type_id) else { continue };
                        let (fac_opt, loc_type_id, sys_opt) = resolve_bp_facility_loc(ctx, snap, it.location_id);
                        *counts.entry(fac_opt).or_insert(0) += 1;
                        if facility_filter.is_some_and(|fid| fac_opt != Some(fid)) || bps.len() >= 25_000 {
                            continue;
                        }
                        let me = it.dynamic_attributes.get(&ATTR_BLUEPRINT_ME).copied().unwrap_or(0.0) as i64;
                        let te = it.dynamic_attributes.get(&ATTR_BLUEPRINT_TE).copied().unwrap_or(0.0) as i64;
                        let is_reaction = bp.activities.contains_key("reaction") && !bp.activities.contains_key("manufacturing");
                        let act_id = if is_reaction { 9 } else { 1 };
                        let cost_meta = compute_industry_job_costs(ctx, snap, it.type_id, act_id, 1, fac_opt.unwrap_or(it.location_id)).unwrap_or_default();
                        let (materials, skills, prod_type_id) = build_blueprint_materials_skills_product(bp, is_reaction);
                        let runs = if it.is_blueprint_copy { i64::from(it.blueprint_runs_remaining.unwrap_or(1)) } else { -1 };
                        let qty_code = if it.is_blueprint_copy { -2 } else if it.singleton { -1 } else { i64::from(it.quantity.max(1)) };
                        let flag_i64 = it.flag as u16 as i64;
                        let resolved_sys_id = sys_opt
                            .or_else(|| session.solar_system_id.map(i64::from))
                            .or_else(|| {
                                snap.characters
                                    .get(&char_id)
                                    .map(|c| i64::from(c.solar_system_id))
                                    .filter(|&sid| sid > 0)
                            })
                            .unwrap_or(0);
                        bps.push(BlueprintInstanceRep {
                            item_id: it.item_id,
                            type_id: i64::from(bp.blueprint_type_id),
                            product_type_id: i64::from(prod_type_id),
                            owner_id: it.owner_id,
                            facility_id: fac_opt,
                            location_id: it.location_id,
                            location_type_id: loc_type_id,
                            flag_id: flag_i64,
                            solar_system_id: resolved_sys_id,
                            material_efficiency: me,
                            time_efficiency: te,
                            original: !it.is_blueprint_copy,
                            runs,
                            quantity: qty_code,
                            job_id: active_jobs.get(&it.item_id).copied(),
                            is_impounded: it.flag == eve_db::InventoryFlag::Impounded,
                            materials,
                            skills,
                            estimated_item_value_isk: cost_meta.eiv_isk,
                            details: None,
                        });
                    }
                    Ok(BlueprintDataByOwnerRep {
                        blueprints: bps,
                        facility_counts: counts.into_iter().collect(),
                    }
                    .into_client_rep())
                });
                return Some(res);
            }
            let Some(raw_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidBlueprintTypeID".into(),
                    message: "GetBlueprintData requires a blueprint typeID or itemID".into(),
                    client_error: None,
                }));
            };
            let (bp_type_id, bp_item_opt) = match ctx.db.with_snapshot(|snap| -> Result<(TypeId, Option<eve_db::InventoryItemRecord>), BridgeRpcError> {
                if let Some(it) = snap.items.get(&raw_id) {
                    if ctx.sde.blueprints.contains_key(&it.type_id) {
                        let caller_corp_id = session.corporation_id.or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id)).unwrap_or(0);
                        let is_owned = it.owner_id == char_id
                            || (caller_corp_id > 0 && it.owner_id == caller_corp_id);
                        let caller_alliance_id = if caller_corp_id > 0 {
                            snap.corporations.get(&caller_corp_id).and_then(|c| c.alliance_id).unwrap_or(0)
                        } else {
                            0
                        };
                        let in_visible_contract = snap.contracts.values().any(|c| {
                            c.status != "Deleted"
                                && c.item_ids.contains(&raw_id)
                                && (c.availability.eq_ignore_ascii_case("Public")
                                    || c.issuer_id == char_id
                                    || c.assignee_id == char_id
                                    || c.acceptor_id == Some(char_id)
                                    || (caller_corp_id > 0
                                        && ((c.for_corp && c.issuer_corp_id == caller_corp_id)
                                            || (c.availability.eq_ignore_ascii_case("Corporation")
                                                && c.issuer_corp_id == caller_corp_id)
                                            || c.assignee_id == caller_corp_id
                                            || c.acceptor_id == Some(caller_corp_id)))
                                    || (caller_alliance_id > 0 && c.assignee_id == caller_alliance_id))
                        });
                        if !is_owned && !in_visible_contract {
                            return Err(BridgeRpcError {
                                code: "CrpAccessDenied".into(),
                                message: format!("Access denied to blueprint item {raw_id}"),
                                client_error: None,
                            });
                        }
                        return Ok((it.type_id, Some(it.clone())));
                    }
                }
                Ok((raw_id as TypeId, None))
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(bp) = ctx.sde.blueprints.get(&bp_type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UnknownBlueprint".into(),
                    message: format!("Blueprint type {bp_type_id} not found in SDE"),
                    client_error: None,
                }));
            };
            let facility_id = resolve_caller_facility_id(
                ctx,
                session,
                char_id,
                bp_item_opt.as_ref().map(|i| i.location_id),
            );
            let is_reaction = bp.activities.contains_key("reaction")
                && !bp.activities.contains_key("manufacturing");
            let act_id = if is_reaction { 9 } else { 1 };
            let me_lvl = bp_item_opt
                .as_ref()
                .and_then(|i| i.dynamic_attributes.get(&ATTR_BLUEPRINT_ME).copied())
                .unwrap_or(0.0) as i64;
            let te_lvl = bp_item_opt
                .as_ref()
                .and_then(|i| i.dynamic_attributes.get(&ATTR_BLUEPRINT_TE).copied())
                .unwrap_or(0.0) as i64;
            let (materials, skills, prod_type_id) =
                build_blueprint_materials_skills_product(bp, is_reaction);
            let reaction_time = bp
                .activities
                .get("reaction")
                .map(|a| a.time as i64)
                .unwrap_or(0);
            let bp_item_id = bp_item_opt.as_ref().map(|i| i.item_id).unwrap_or(raw_id);
            let bp_owner_id = bp_item_opt.as_ref().map(|i| i.owner_id).unwrap_or(char_id);
            let bp_flag_id = bp_item_opt.as_ref().map(|i| i.flag as u16 as i64).unwrap_or(4);
            let is_copy = bp_item_opt
                .as_ref()
                .map(|i| i.is_blueprint_copy)
                .unwrap_or(false);
            let is_impounded = bp_item_opt
                .as_ref()
                .is_some_and(|i| i.flag == eve_db::InventoryFlag::Impounded);
            let runs_rem = bp_item_opt
                .as_ref()
                .and_then(|i| i.blueprint_runs_remaining)
                .map(i64::from)
                .unwrap_or(if is_copy { 1 } else { -1 });
            let qty_code = if is_copy { -2_i64 } else { -1_i64 };
            let (cost_meta, loc_type_id, solar_system_id, active_job_id) =
                ctx.db.with_snapshot(|snap| {
                    let cost_meta =
                        compute_industry_job_costs(ctx, snap, bp_type_id, act_id, 1, facility_id)
                            .unwrap_or_default();
                    let (_, raw_loc_type_id, resolved_sys) =
                        resolve_bp_facility_loc(ctx, snap, facility_id);
                    let loc_type_id = if raw_loc_type_id > 0 { raw_loc_type_id } else { 1529 };
                    let solar_system_id = ctx
                        .sde
                        .stations
                        .get(&facility_id)
                        .map(|st| i64::from(st.solar_system_id))
                        .or(resolved_sys)
                        .or_else(|| session.solar_system_id.map(i64::from))
                        .or_else(|| {
                            snap.characters
                                .get(&char_id)
                                .map(|c| i64::from(c.solar_system_id))
                                .filter(|&sid| sid > 0)
                        })
                        .unwrap_or(0);
                    let active_job_id = snap
                        .industry_jobs
                        .values()
                        .find(|j| {
                            j.blueprint_item_id == bp_item_id
                                && !matches!(
                                    j.status.as_str(),
                                    "Delivered"
                                        | "Completed"
                                        | "Cancelled"
                                        | "Reverted"
                                        | "Failed"
                                )
                        })
                        .map(|j| j.job_id);
                    (cost_meta, loc_type_id, solar_system_id, active_job_id)
                });
            let res = BlueprintInstanceRep {
                item_id: bp_item_id,
                type_id: i64::from(bp.blueprint_type_id),
                product_type_id: i64::from(prod_type_id),
                owner_id: bp_owner_id,
                facility_id: Some(facility_id),
                location_id: facility_id,
                location_type_id: loc_type_id,
                flag_id: bp_flag_id,
                solar_system_id,
                material_efficiency: me_lvl,
                time_efficiency: te_lvl,
                original: !is_copy,
                runs: runs_rem,
                quantity: qty_code,
                job_id: active_job_id,
                is_impounded,
                materials,
                skills,
                estimated_item_value_isk: cost_meta.eiv_isk,
                details: Some(BlueprintJobDetailsRep {
                    max_production_limit: bp.max_production_limit as i64,
                    manufacturing_time: bp.manufacturing_time_sec as i64,
                    research_material_time: bp.research_me_time_sec as i64,
                    research_time_time: bp.research_te_time_sec as i64,
                    copying_time: bp.copy_time_sec as i64,
                    invention_time: bp.invention_time_sec as i64,
                    reaction_time,
                    base_job_cost: cost_meta.base_job_cost_isk,
                    facility_tax_isk: cost_meta.facility_tax_isk,
                    scc_surcharge_isk: cost_meta.scc_surcharge_isk,
                    total_job_cost_isk: cost_meta.total_cost_isk,
                }),
            }
            .into_client_rep();
            Some(Ok(res))
        }

        ("blueprintManager", "GetLimits") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let limits = ctx.db.with_snapshot(|snap| {
                let char_skills = snap.skills.get(&char_id);
                let skill_lvl = |sid: TypeId| -> u8 {
                    char_skills
                        .and_then(|m| m.get(&sid))
                        .map(|s| s.level)
                        .unwrap_or(0)
                };
                let mfg_max = 1 + (skill_lvl(3387) as i64) + (skill_lvl(24625) as i64);
                let sci_max = 1 + (skill_lvl(3406) as i64) + (skill_lvl(24624) as i64);
                let react_max = 1 + (skill_lvl(45748) as i64) + (skill_lvl(45749) as i64);
                let mfg_range = (skill_lvl(24268) as i64) * 5;
                let sci_range = (skill_lvl(24270) as i64) * 5;
                let react_range = (skill_lvl(45750) as i64) * 5;
                PyRep::keyval([
                    ("maxBlueprintResults", PyRep::Integer(25_000)),
                    ("maxManufacturingJobs", PyRep::Integer(mfg_max)),
                    ("maxResearchJobs", PyRep::Integer(sci_max)),
                    ("maxReactionsJobs", PyRep::Integer(react_max)),
                    ("manufacturingRangeJumps", PyRep::Integer(mfg_range)),
                    ("researchRangeJumps", PyRep::Integer(sci_range)),
                    ("reactionsRangeJumps", PyRep::Integer(react_range)),
                ])
            });
            Some(Ok(limits))
        }

        ("facilityManager", "GetFacilities")
        | ("facilityManager", "GetFacilitiesByID") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let explicit_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(ids) | PyRep::Tuple(ids)) => {
                    ids.iter().filter_map(|v| v.as_i64()).collect()
                }
                _ => Vec::new(),
            };
            let facilities = ctx.db.with_snapshot(|snap| {
                let current_sys = session
                    .solar_system_id
                    .or_else(|| snap.characters.get(&char_id).map(|c| c.solar_system_id))
                    .unwrap_or(0);
                let mut list = Vec::new();
                if !explicit_ids.is_empty() {
                    for fid in explicit_ids {
                        if let Some(f_rep) = build_facility_pyrep(ctx, snap, fid) {
                            list.push(f_rep);
                        }
                    }
                    return list;
                }
                let mut seen = std::collections::HashSet::new();
                for s in snap.structures.values() {
                    if s.state != "Destroyed" && seen.insert(s.structure_id) {
                        if let Some(f_rep) = build_facility_pyrep(ctx, snap, s.structure_id) {
                            list.push(f_rep);
                        }
                    }
                }
                for it in snap.items.values() {
                    if matches!(it.type_id, 35825 | 35826 | 35827 | 35835 | 35836)
                        && seen.insert(it.item_id)
                    {
                        if let Some(f_rep) = build_facility_pyrep(ctx, snap, it.item_id) {
                            list.push(f_rep);
                        }
                    }
                }
                if let Some(sys) = ctx.sde.solar_systems.get(&current_sys) {
                    for st in &sys.stations {
                        if seen.insert(st.station_id) {
                            if let Some(f_rep) = build_facility_pyrep(ctx, snap, st.station_id) {
                                list.push(f_rep);
                            }
                        }
                    }
                }
                if list.is_empty() {
                    let fallback_id = session
                        .station_id
                        .or(session.structure_id)
                        .or_else(|| {
                            snap.characters.get(&char_id).and_then(|c| {
                                c.station_id
                                    .or(c.structure_id)
                                    .or_else(|| (c.home_station_id > 0).then_some(c.home_station_id))
                            })
                        })
                        .unwrap_or(0);
                    if fallback_id > 0 {
                        if let Some(f_rep) = build_facility_pyrep(ctx, snap, fallback_id) {
                            list.push(f_rep);
                        }
                    }
                }
                list
            });
            Some(Ok(PyRep::List(facilities)))
        }

        ("facilityManager", "GetFacilityLocations") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            if let Some(PyRep::List(ids) | PyRep::Tuple(ids)) = args.first() {
                let explicit_ids: Vec<i64> = ids.iter().filter_map(|v| v.as_i64()).collect();
                let locs = ctx.db.with_snapshot(|snap| {
                    explicit_ids.into_iter().filter_map(|fid| {
                        let sys_id = snap.structures.get(&fid).map(|s| s.solar_system_id)
                            .or_else(|| ctx.sde.stations.get(&fid).map(|st| st.solar_system_id as u32))?;
                        let type_id = snap.structures.get(&fid).map(|s| s.type_id as i64)
                            .or_else(|| ctx.sde.stations.get(&fid).map(|st| st.station_type_id as i64))
                            .unwrap_or(1529);
                        let reg = ctx.sde.solar_systems.get(&sys_id).map(|sys| sys.region_id as i64).unwrap_or(0);
                        Some(FacilityLocationRep {
                            item_id: fid,
                            facility_id: fid,
                            type_id,
                            owner_id: char_id,
                            flag_id: 4,
                            solar_system_id: i64::from(sys_id),
                            region_id: reg,
                            can_view: true,
                            can_take: true,
                        }
                        .into_client_rep())
                    }).collect::<Vec<_>>()
                });
                return Some(Ok(PyRep::List(locs)));
            }
            let fid = resolve_caller_facility_id(ctx, session, char_id, args.first().and_then(|a| a.as_i64()));
            let owner_filter = args.get(1).and_then(|a| a.as_i64());
            let locs = match ctx.db.with_snapshot(|snap| -> Result<Vec<PyRep>, BridgeRpcError> {
                let sys_id = snap.structures.get(&fid).map(|s| s.solar_system_id)
                    .or_else(|| ctx.sde.stations.get(&fid).map(|st| st.solar_system_id as u32))
                    .or(session.solar_system_id)
                    .or_else(|| snap.characters.get(&char_id).map(|c| c.solar_system_id))
                    .ok_or_else(|| BridgeRpcError {
                        code: "FacilityLocationNotFound".into(),
                        message: format!("Could not resolve solar system for facility {fid}"),
                        client_error: None,
                    })?;
                let type_id = snap.structures.get(&fid).map(|s| s.type_id as i64)
                    .or_else(|| ctx.sde.stations.get(&fid).map(|st| st.station_type_id as i64))
                    .unwrap_or(1529);
                let reg = ctx.sde.solar_systems.get(&sys_id).map(|sys| sys.region_id as i64).unwrap_or(0);
                let mut out = Vec::new();
                if owner_filter.is_none() || owner_filter == Some(char_id) {
                    out.push(
                        FacilityLocationRep {
                            item_id: fid,
                            facility_id: fid,
                            type_id,
                            owner_id: char_id,
                            flag_id: 4,
                            solar_system_id: i64::from(sys_id),
                            region_id: reg,
                            can_view: true,
                            can_take: true,
                        }
                        .into_client_rep(),
                    );
                }
                let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
                if corp_id > 0 && (owner_filter.is_none() || owner_filter == Some(corp_id)) {
                    if snap.corporations.contains_key(&corp_id) {
                        let (is_ceo, roles) = snap.effective_corp_roles_at(
                            corp_id,
                            char_id,
                            Some(fid),
                            session.corp_role,
                            ctx.now_unix(),
                        );
                        let full = is_ceo || (roles & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_FACTORY_MANAGER)) != 0;
                        for d in 0..7u8 {
                            let div_mask = (128u64 << d) | (1024u64 << d) | (1_048_576u64 << d) | (8_388_608u64 << d);
                            if full || (roles & div_mask) != 0 {
                                let flag = 115 + i64::from(d);
                                out.push(
                                    FacilityLocationRep {
                                        item_id: fid,
                                        facility_id: fid,
                                        type_id,
                                        owner_id: corp_id,
                                        flag_id: flag,
                                        solar_system_id: i64::from(sys_id),
                                        region_id: reg,
                                        can_view: true,
                                        can_take: true,
                                    }
                                    .into_client_rep(),
                                );
                            }
                        }
                    }
                }
                Ok(out)
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::List(locs)))
        }

        ("facilityManager", "GetFacility") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let facility_id = resolve_caller_facility_id(ctx, session, char_id, args.first().and_then(|a| a.as_i64()));
            let res = ctx.db.with_snapshot(|snap| {
                build_facility_pyrep(ctx, snap, facility_id).ok_or_else(|| BridgeRpcError {
                    code: "FacilityNotFound".into(),
                    message: format!("Industry facility {facility_id} not found"),
                    client_error: None,
                })
            });
            Some(res)
        }

        ("facilityManager", "GetFacilityTaxes") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let facility_id = resolve_caller_facility_id(ctx, session, char_id, args.first().and_then(|a| a.as_i64()));
            let now = ctx.now_unix();
            let res = ctx.db.with_snapshot(|snap| {
                if resolve_facility_modifiers_and_system(ctx, snap, facility_id).is_none() {
                    return Err(BridgeRpcError { code: "FacilityNotFound".into(), message: format!("Industry facility {facility_id} not found"), client_error: None });
                }
                let mut taxes = load_facility_taxes_with_sde(Some(&ctx.sde), snap, facility_id);
                if snap.structures.contains_key(&facility_id) {
                    for act in [1_u8, 3, 4, 5, 8, 9] {
                        if let Ok(Some(r)) = crate::services::structure_profile_services::check_industry_activity_acl_and_tax(snap, facility_id, char_id, act, now) {
                            taxes.insert(act, r);
                        }
                    }
                }
                let mfg_tax = taxes.get(&1).copied().unwrap_or(0.0025);
                Ok(PyRep::keyval([
                    ("facilityID", PyRep::Integer(facility_id)),
                    ("tax", PyRep::Real(mfg_tax)),
                    ("taxCorporation", PyRep::Real(mfg_tax)),
                    ("1", PyRep::Real(taxes.get(&1).copied().unwrap_or(mfg_tax))),
                    ("3", PyRep::Real(taxes.get(&3).copied().unwrap_or(mfg_tax))),
                    ("4", PyRep::Real(taxes.get(&4).copied().unwrap_or(mfg_tax))),
                    ("5", PyRep::Real(taxes.get(&5).copied().unwrap_or(mfg_tax))),
                    ("8", PyRep::Real(taxes.get(&8).copied().unwrap_or(mfg_tax))),
                    ("9", PyRep::Real(taxes.get(&9).copied().unwrap_or(mfg_tax))),
                    ("manufacturing", PyRep::Real(taxes.get(&1).copied().unwrap_or(mfg_tax))),
                    ("research_te", PyRep::Real(taxes.get(&3).copied().unwrap_or(mfg_tax))),
                    ("research_me", PyRep::Real(taxes.get(&4).copied().unwrap_or(mfg_tax))),
                    ("copying", PyRep::Real(taxes.get(&5).copied().unwrap_or(mfg_tax))),
                    ("invention", PyRep::Real(taxes.get(&8).copied().unwrap_or(mfg_tax))),
                    ("reaction", PyRep::Real(taxes.get(&9).copied().unwrap_or(mfg_tax))),
                ]))
            });
            Some(res)
        }

        ("facilityManager", "SetFacilityTaxes") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(facility_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidFacilityID".into(),
                    message: "SetFacilityTaxes requires an explicit integer facilityID".into(),
                    client_error: None,
                }));
            };
            let tax_arg = if args.len() >= 3 {
                args.get(2)
            } else {
                args.get(1)
            };
            let mut updates: Vec<(u8, f64)> = Vec::new();
            let normalize_rate = |raw: f64| -> Result<f64, BridgeRpcError> {
                if !raw.is_finite() || raw < 0.0 {
                    return Err(BridgeRpcError {
                        code: "InvalidFacilityTaxRate".into(),
                        message: format!("Facility tax rate {raw} must be non-negative and finite"),
                        client_error: None,
                    });
                }
                let rate = if raw > 1.0 && raw <= 100.0 {
                    raw / 100.0
                } else if raw > 100.0 {
                    raw / 1_000_000.0
                } else {
                    raw
                };
                if rate > 1.0 {
                    return Err(BridgeRpcError {
                        code: "InvalidFacilityTaxRate".into(),
                        message: format!("Facility tax rate {rate} exceeds 100%"),
                        client_error: None,
                    });
                }
                Ok(rate)
            };
            let parse_act_key = |k: &str| -> Vec<u8> {
                match k {
                    "0" | "market" | "broker" => vec![0],
                    "1" | "manufacturing" => vec![1],
                    "3" | "research_te" => vec![3],
                    "4" | "research_me" => vec![4],
                    "5" | "copying" => vec![5],
                    "8" | "invention" => vec![8],
                    "9" | "reaction" => vec![9],
                    "all" | "tax" => vec![1, 3, 4, 5, 8, 9],
                    _ => Vec::new(),
                }
            };
            match tax_arg {
                Some(PyRep::Real(r)) => {
                    let rate = match normalize_rate(*r) {
                        Ok(v) => v,
                        Err(e) => return Some(Err(e)),
                    };
                    for act in [1_u8, 3, 4, 5, 8, 9] {
                        updates.push((act, rate));
                    }
                }
                Some(PyRep::KeyVal(kv)) => {
                    for (k, v) in kv {
                        if let Some(raw) = v.as_f64() {
                            let rate = match normalize_rate(raw) {
                                Ok(r) => r,
                                Err(e) => return Some(Err(e)),
                            };
                            for act in parse_act_key(k) {
                                updates.push((act, rate));
                            }
                        }
                    }
                }
                Some(PyRep::Dict(entries)) => {
                    for (k, v) in entries {
                        let Some(raw) = v.as_f64() else { continue };
                        let rate = match normalize_rate(raw) {
                            Ok(r) => r,
                            Err(e) => return Some(Err(e)),
                        };
                        if let Some(act_id) = k.as_i64().and_then(|i| u8::try_from(i).ok()) {
                            updates.push((act_id, rate));
                        } else if let Some(ks) = k.as_str() {
                            for act in parse_act_key(ks) {
                                updates.push((act, rate));
                            }
                        }
                    }
                }
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidFacilityTaxRate".into(),
                        message: "SetFacilityTaxes requires a taxRateValues dict/KeyVal or float"
                            .into(),
                        client_error: None,
                    }));
                }
            }
            if updates.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidFacilityTaxRate".into(),
                    message: "SetFacilityTaxes requires at least one valid activity tax rate"
                        .into(),
                    client_error: None,
                }));
            }
            if updates.iter().any(|&(act, rate)| act != 0 && rate > 0.10) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidFacilityTaxRate".into(),
                    message: "Structure industry facility tax rate cannot exceed 10%".into(),
                    client_error: None,
                }));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fac) = resolve_facility_modifiers_and_system(ctx, snap, facility_id) else {
                    return Err(BridgeRpcError {
                        code: "FacilityNotFound".into(),
                        message: format!("Industry facility {facility_id} not found"),
                        client_error: None,
                    });
                };
                let sys_id = fac.solar_system_id;
                let owner_id = snap
                    .structures
                    .get(&facility_id)
                    .map(|st| st.owner_corp_id)
                    .or_else(|| {
                        snap.items.get(&facility_id).and_then(|it| {
                            if matches!(
                                it.type_id,
                                35825
                                    | 35826
                                    | 35827
                                    | 35832
                                    | 35833
                                    | 35834
                                    | 35835
                                    | 35836
                                    | 47512..=47516
                            ) {
                                Some(it.owner_id)
                            } else {
                                None
                            }
                        })
                    });
                if !session.has_gm_role() {
                    // NPC stations have no player-structure owner (`owner_id == None`), and
                    // unowned/NPC-corp facilities (`!is_player_corp`) must never be mutable by
                    // non-GM players regardless of session roles.
                    let Some(owner_corp_id) = owner_id else {
                        return Err(BridgeRpcError {
                            code: "FacilityPermissionDenied".into(),
                            message: format!(
                                "Character {char_id} cannot modify taxes on NPC or unowned facility {facility_id}"
                            ),
                            client_error: None,
                        });
                    };
                    let is_player_corp = (owner_corp_id >= 98_000_000
                        && !ctx.sde.npc_corporations.contains_key(&owner_corp_id))
                        || snap
                            .corporations
                            .get(&owner_corp_id)
                            .is_some_and(|c| c.ceo_id != 0 || !c.members.is_empty());
                    if !is_player_corp {
                        return Err(BridgeRpcError {
                            code: "FacilityPermissionDenied".into(),
                            message: format!(
                                "Facility {facility_id} is not owned by a player corporation (owner {owner_corp_id})"
                            ),
                            client_error: None,
                        });
                    }
                    let char_corp = snap
                        .characters
                        .get(&char_id)
                        .map(|c| c.corporation_id)
                        .or(session.corporation_id)
                        .unwrap_or(0);
                    if owner_corp_id != char_corp {
                        return Err(BridgeRpcError {
                            code: "FacilityPermissionDenied".into(),
                            message: format!(
                                "Character {char_id} (corp {char_corp}) does not own structure {facility_id} (owner {owner_corp_id})"
                            ),
                            client_error: None,
                        });
                    }
                    let req_roles = eve_db::corp_roles::ROLE_DIRECTOR
                        | eve_db::corp_roles::ROLE_STATION_MANAGER;
                    let (is_ceo, eff_roles) = snap.effective_corp_roles_at(
                        owner_corp_id,
                        char_id,
                        Some(facility_id),
                        session.corp_role,
                        ctx.now_unix(),
                    );
                    if !is_ceo && (eff_roles & req_roles) == 0 {
                        return Err(BridgeRpcError {
                            code: "InsufficientCorpRoles".into(),
                            message:
                                "Setting facility taxes requires Director or Station Manager role"
                                    .into(),
                            client_error: None,
                        });
                    }
                }
                let mut taxes = load_facility_taxes_with_sde(Some(&ctx.sde), snap, facility_id);
                for (act, rate) in updates {
                    taxes.insert(act, rate);
                }
                save_facility_taxes(snap, facility_id, &taxes);
                let mfg_tax = taxes.get(&1).copied().unwrap_or(0.0025);
                Ok((
                    sys_id,
                    PyRep::keyval([
                        ("facilityID", PyRep::Integer(facility_id)),
                        ("tax", PyRep::Real(mfg_tax)),
                        ("1", PyRep::Real(taxes.get(&1).copied().unwrap_or(mfg_tax))),
                        ("3", PyRep::Real(taxes.get(&3).copied().unwrap_or(mfg_tax))),
                        ("4", PyRep::Real(taxes.get(&4).copied().unwrap_or(mfg_tax))),
                        ("5", PyRep::Real(taxes.get(&5).copied().unwrap_or(mfg_tax))),
                        ("8", PyRep::Real(taxes.get(&8).copied().unwrap_or(mfg_tax))),
                        ("9", PyRep::Real(taxes.get(&9).copied().unwrap_or(mfg_tax))),
                        ("manufacturing", PyRep::Real(taxes.get(&1).copied().unwrap_or(mfg_tax))),
                    ]),
                ))
            });
            match res {
                Ok((sys_id, kv)) => {
                    crate::services::notify_emitters_character_services::emit_facilities_updated(
                        ctx,
                        Some(sys_id),
                        char_id,
                        facility_id,
                    );
                    Some(Ok(kv))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("facilityManager", "GetMaxActivityModifiers") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut best_mat = 1.0_f64;
            let mut best_time = 1.0_f64;
            let mut best_cost = 1.0_f64;
            for t in ctx.sde.types.values() {
                if let Some(&m) = t.default_attributes.get(&2600) {
                    if m > 0.0 {
                        best_mat = best_mat.min(m);
                    }
                }
                if let Some(&tm) = t
                    .default_attributes
                    .get(&2602)
                    .or_else(|| t.default_attributes.get(&2721))
                {
                    if tm > 0.0 {
                        best_time = best_time.min(tm);
                    }
                }
                if let Some(&c) = t.default_attributes.get(&2601) {
                    if c > 0.0 {
                        best_cost = best_cost.min(c);
                    }
                }
            }
            ctx.db.with_snapshot(|snap| {
                for (&sid, st) in &snap.structures {
                    if st.state == "Destroyed" {
                        continue;
                    }
                    if let Some(fac) = resolve_facility_modifiers_and_system(ctx, snap, sid) {
                        let rigs = resolve_structure_rig_bonuses_full(ctx, snap, sid, fac.solar_system_id);
                        best_mat = best_mat.min((fac.material_modifier * (1.0 - rigs.mat_bonus)).clamp(0.01, 1.0));
                        best_time = best_time.min(fac.time_modifier.clamp(0.01, 1.0));
                        best_cost = best_cost.min(fac.cost_modifier.clamp(0.01, 1.0));
                    }
                }
            });
            Some(Ok(PyRep::keyval([
                ("materialMultiplier", PyRep::Real(best_mat)),
                ("timeMultiplier", PyRep::Real(best_time)),
                ("costMultiplier", PyRep::Real(best_cost)),
            ])))
        }

        // System Cost Index & 28d Job Volume (`feat:industry.cost-index`)
        ("map", "GetIndustryCostModifier") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let req_sys = args.first().and_then(|a| a.as_i64()).map(|v| v as u32).or(session.solar_system_id);
            let res = ctx.db.with_snapshot(|snap| {
                let sys_id = req_sys.or_else(|| snap.characters.get(&char_id).map(|c| c.solar_system_id)).unwrap_or(0);
                let sci = |act| compute_canonical_system_cost_index(ctx, snap, sys_id, act);
                PyRep::keyval([
                    ("solarSystemID", PyRep::Integer(sys_id as i64)),
                    ("1", PyRep::Real(sci(1))), ("3", PyRep::Real(sci(3))),
                    ("4", PyRep::Real(sci(4))), ("5", PyRep::Real(sci(5))),
                    ("8", PyRep::Real(sci(8))), ("9", PyRep::Real(sci(9))),
                    ("manufacturing", PyRep::Real(sci(1))), ("research_te", PyRep::Real(sci(3))),
                    ("research_me", PyRep::Real(sci(4))), ("copying", PyRep::Real(sci(5))),
                    ("invention", PyRep::Real(sci(8))), ("reaction", PyRep::Real(sci(9))),
                ])
            });
            Some(Ok(res))
        }

        ("map", "GetIndustryJobsOverLast24Hours") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now = ctx.now_unix_secs();
            let cutoff = now.saturating_sub(86_400);
            let rows = ctx.db.with_snapshot(|snap| {
                let mut counts: HashMap<u32, i64> = HashMap::new();
                for j in snap.industry_jobs.values() {
                    if j.start_unix < cutoff {
                        continue;
                    }
                    let j_sys = ctx
                        .sde
                        .stations
                        .get(&j.facility_id)
                        .map(|s| s.solar_system_id)
                        .or_else(|| snap.structures.get(&j.facility_id).map(|s| s.solar_system_id));
                    if let Some(sys) = j_sys {
                        *counts.entry(sys).or_insert(0) += 1;
                    }
                }
                counts
                    .into_iter()
                    .map(|(sys, cnt)| vec![PyRep::Integer(sys as i64), PyRep::Integer(cnt)])
                    .collect::<Vec<Vec<PyRep>>>()
            });
            Some(Ok(PyRep::rowset(vec!["solarSystemID", "noOfJobs"], rows)))
        }

        _ => None,
    }
}

#[cfg(test)] #[path = "industry_facilities_bps_unit_tests.rs"] mod industry_facilities_bps_unit_tests;
#[cfg(test)] #[path = "industry_facilities_bps_edges2_tests.rs"] mod industry_facilities_bps_edges2_tests;
