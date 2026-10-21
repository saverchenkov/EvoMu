//! `industry_facilities_bps` — Blueprint data, facility registry, taxes, structure rigs, and system cost indices (`E-4`).

// parity: plan:E-4
// parity: feat:industry.blueprint-data, feat:industry.blueprint-limits, feat:industry.facilities
// parity: feat:industry.facility-tax, feat:industry.job-cost, feat:industry.cost-index
// parity: feat:industry.structure-rigs, feat:structures.service-industry, feat:structures.service-reactions

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::InventoryFlag;
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
}

/// Checks whether an Upwell structure's industry/reaction service is online and fueled (`feat:industry.job-pause-on-service-offline`).
pub fn is_structure_industry_service_online(
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
    activity_id: u8,
) -> bool {
    let Some(st) = snap.structures.get(&facility_id) else { return true };
    if st.state == "Destroyed" || !st.quantum_core_installed || st.fuel_blocks == 0 {
        return false;
    }
    let service_modules: Vec<&eve_db::InventoryItemRecord> = snap
        .items
        .values()
        .filter(|it| {
            it.location_id == facility_id
                && (164..=171).contains(&(it.flag as u16))
                && match activity_id {
                    1 => matches!(it.type_id, 35878 | 35881 | 35877),
                    9 => matches!(it.type_id, 35882 | 35883 | 35884),
                    _ => matches!(it.type_id, 35879 | 35880),
                }
        })
        .collect();
    service_modules.is_empty()
        || service_modules
            .iter()
            .any(|m| m.dynamic_attributes.get(&2).copied().unwrap_or(1.0) > 0.0)
}

/// Synchronizes active/paused industry jobs when a host structure loses or regains fuel/service (`feat:industry.job-pause-on-service-offline`).
pub fn sync_industry_job_pause_states(snap: &mut eve_db::WorldStateSnapshot, now_unix: u64) {
    let job_ids: Vec<i64> = snap.industry_jobs.keys().copied().collect();
    for jid in job_ids {
        let Some((facility_id, activity_id, status)) = snap
            .industry_jobs
            .get(&jid)
            .map(|j| (j.facility_id, j.activity_id, j.status.clone()))
        else {
            continue;
        };
        if status != "Active" && status != "Paused" {
            continue;
        }
        let online = is_structure_industry_service_online(snap, facility_id, activity_id);
        if !online && status == "Active" {
            if let Some(j_mut) = snap.industry_jobs.get_mut(&jid) {
                j_mut.status = "Paused".into();
            }
            let mut meta = load_job_cost_meta(snap, jid).unwrap_or_default();
            if meta.paused_at_unix.is_none() {
                meta.paused_at_unix = Some(now_unix);
                save_job_cost_meta(snap, jid, &meta);
            }
        } else if online && status == "Paused" {
            let mut meta = load_job_cost_meta(snap, jid).unwrap_or_default();
            let paused_secs = now_unix.saturating_sub(meta.paused_at_unix.unwrap_or(now_unix));
            if let Some(j_mut) = snap.industry_jobs.get_mut(&jid) {
                j_mut.duration_seconds = j_mut.duration_seconds.saturating_add(paused_secs);
                j_mut.status = "Active".into();
            }
            meta.paused_at_unix = None;
            save_job_cost_meta(snap, jid, &meta);
        }
    }
}

/// Checks whether the Havoc Fulcrum pirate ship bonus applies (`feat:industry.fulcrum-pirate-ship-bonus`).
pub fn is_fulcrum_pirate_ship_bonus_applicable(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    char_id: Option<i64>,
    bp_type_id: TypeId,
    activity_id: u8,
    facility_id: i64,
) -> bool {
    let Some(cid) = char_id.filter(|_| activity_id == 1) else { return false };
    let pirate_enlisted = snap
        .characters
        .get(&cid)
        .and_then(|c| c.war_faction_id)
        .is_some_and(|wf| matches!(wf, 500_010 | 500_011));
    let is_fulcrum = facility_id == 60_015_187
        || resolve_facility_modifiers_and_system(ctx, snap, facility_id)
            .is_some_and(|(ftype, _, sys_id, _, _, _, _)| ftype == 78_334 || sys_id == 30_100_000);
    if !pirate_enlisted || !is_fulcrum {
        return false;
    }
    let Some(prod_type) = ctx
        .sde
        .blueprints
        .get(&bp_type_id)
        .and_then(|bp| ctx.sde.types.get(&bp.product_type_id))
    else {
        return false;
    };
    prod_type.category_id == 6
        && matches!(prod_type.faction_id, Some(500_010 | 500_011))
        && !matches!(
            prod_type.group_id,
            30 | 485 | 513 | 547 | 659 | 883 | 902 | 1538 | 4594 | 5120
        )
}

pub fn facility_tax_note_id(facility_id: i64) -> i64 {
    -(1_000_000_000_000_i64.saturating_add(facility_id.abs()))
}

pub fn load_facility_taxes(snap: &eve_db::WorldStateSnapshot, facility_id: i64) -> HashMap<u8, f64> {
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
            .map(|it| matches!(it.type_id, 35825 | 35826 | 35827 | 35835 | 35836))
            .unwrap_or(false);
    let default_rate = if is_upwell { 0.05 } else { 0.10 };
    let mut map = HashMap::new();
    for act in [1_u8, 3, 4, 5, 8, 9] {
        map.insert(act, default_rate);
    }
    map
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

/// Computes Standup Structure Rig bonuses (`feat:industry.structure-rigs`, `feat:industry.structure-standup-rig-security-multipliers`):
/// - Manufacturing/Research/Invention rigs: High-Sec (`1.0x`), Low-Sec (`1.9x`), Null-Sec/WH (`2.1x`).
/// - Reaction rigs (`1933..=1939`, `2713`/`2714`, `1970` `disallowInHighSec`): High-Sec (`0.0x`), Low-Sec (`1.0x`), Null-Sec/WH (`1.1x`).
/// Returns `(rig_mat_bonus, rig_time_bonus, rig_cost_bonus, sec_multiplier)`.
pub fn resolve_structure_rig_bonuses_full(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
    solar_system_id: u32,
) -> (f64, f64, f64, f64) {
    let sec = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(1.0);
    let is_hs = sec >= 0.45 && solar_system_id < 31_000_000;
    let is_ls = sec > 0.0 && sec < 0.45 && solar_system_id < 31_000_000;
    let default_sec_mult = if is_hs {
        1.0
    } else if is_ls {
        1.9
    } else {
        2.1
    };

    let fitted_rigs: Vec<&eve_db::InventoryItemRecord> = snap
        .items
        .values()
        .filter(|it| {
            it.location_id == facility_id
                && matches!(
                    it.flag,
                    InventoryFlag::RigSlot0 | InventoryFlag::RigSlot1 | InventoryFlag::RigSlot2
                )
        })
        .collect();
    if fitted_rigs.is_empty() {
        return (0.0, 0.0, 0.0, default_sec_mult);
    }

    let norm_frac = |v: f64| -> f64 {
        let a = v.abs();
        if a > 1.0 { a / 100.0 } else { a }
    };

    let mut mat_bonus = 0.0_f64;
    let mut time_bonus = 0.0_f64;
    let mut cost_bonus = 0.0_f64;
    let mut active_sec_mult = default_sec_mult;

    for rig in fitted_rigs {
        let sde_type = ctx.sde.types.get(&rig.type_id);
        let is_t2 = sde_type
            .and_then(|t| t.default_attributes.get(&422).copied())
            .map(|tl| tl >= 2.0)
            .unwrap_or(false)
            || rig.dynamic_attributes.get(&422).copied().unwrap_or(1.0) >= 2.0;
        let is_reaction_rig = sde_type
            .map(|t| {
                (1933..=1939).contains(&t.group_id)
                    || t.default_attributes.get(&2713).copied().unwrap_or(0.0).abs() > 0.0
                    || t.default_attributes.get(&2714).copied().unwrap_or(0.0).abs() > 0.0
            })
            .unwrap_or(false);
        let disallow_hs = is_reaction_rig
            || sde_type
                .and_then(|t| t.default_attributes.get(&1970).copied())
                .unwrap_or(0.0)
                > 0.0;
        let rig_sec_mult = if is_hs {
            if disallow_hs {
                0.0
            } else {
                sde_type
                    .and_then(|t| t.default_attributes.get(&2355).copied())
                    .filter(|&v| v > 0.0)
                    .unwrap_or(1.0)
            }
        } else if is_ls {
            sde_type
                .and_then(|t| t.default_attributes.get(&2356).copied())
                .filter(|&v| v > 0.0)
                .unwrap_or(if is_reaction_rig { 1.0 } else { 1.9 })
        } else {
            sde_type
                .and_then(|t| t.default_attributes.get(&2357).copied())
                .filter(|&v| v > 0.0)
                .unwrap_or(if is_reaction_rig { 1.1 } else { 2.1 })
        };
        active_sec_mult = rig_sec_mult;

        let sde_mat = sde_type
            .and_then(|t| {
                t.default_attributes
                    .get(&2594)
                    .copied()
                    .filter(|v| v.abs() > 0.0)
                    .or_else(|| t.default_attributes.get(&2714).copied().filter(|v| v.abs() > 0.0))
            })
            .map(norm_frac);
        let sde_time = sde_type
            .and_then(|t| {
                t.default_attributes
                    .get(&2593)
                    .copied()
                    .filter(|v| v.abs() > 0.0)
                    .or_else(|| t.default_attributes.get(&2713).copied().filter(|v| v.abs() > 0.0))
            })
            .map(norm_frac);
        let sde_cost = sde_type
            .and_then(|t| t.default_attributes.get(&2595).copied().filter(|v| v.abs() > 0.0))
            .map(norm_frac);
        let has_any_sde_ind_attr = sde_mat.is_some() || sde_time.is_some() || sde_cost.is_some();

        let rig_mat = rig
            .dynamic_attributes
            .get(&2594)
            .or_else(|| rig.dynamic_attributes.get(&2714))
            .copied()
            .map(norm_frac)
            .or(sde_mat)
            .unwrap_or(if has_any_sde_ind_attr {
                0.0
            } else if is_t2 {
                0.024
            } else {
                0.02
            });
        let rig_time = rig
            .dynamic_attributes
            .get(&2593)
            .or_else(|| rig.dynamic_attributes.get(&2713))
            .copied()
            .map(norm_frac)
            .or(sde_time)
            .unwrap_or(if has_any_sde_ind_attr {
                0.0
            } else if is_t2 {
                0.24
            } else {
                0.20
            });
        let rig_cost = rig
            .dynamic_attributes
            .get(&2595)
            .copied()
            .map(norm_frac)
            .or(sde_cost)
            .unwrap_or(0.0);

        mat_bonus = mat_bonus.max((rig_mat * rig_sec_mult).clamp(0.0, 0.50));
        time_bonus = time_bonus.max((rig_time * rig_sec_mult).clamp(0.0, 0.80));
        cost_bonus = cost_bonus.max((rig_cost * rig_sec_mult).clamp(0.0, 0.50));
    }

    (mat_bonus, time_bonus, cost_bonus, active_sec_mult)
}

pub fn resolve_structure_rig_bonus(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
    solar_system_id: u32,
) -> (f64, f64, f64) {
    let (mat, time, _, sec_mult) =
        resolve_structure_rig_bonuses_full(ctx, snap, facility_id, solar_system_id);
    (mat, time, sec_mult)
}

pub fn resolve_facility_modifiers_and_system(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
) -> Option<(TypeId, i64, u32, u32, f64, f64, f64)> {
    if let Some(st) = snap.structures.get(&facility_id) {
        if st.state == "Destroyed" {
            return None;
        }
        let region_id = ctx
            .sde
            .solar_systems
            .get(&st.solar_system_id)
            .map(|sys| sys.region_id)
            .unwrap_or(10000002);
        let (base_mat_mod, base_time_mod, cost_mod) = match st.type_id {
            35825 => (0.99, 0.85, 0.97), // Raitaru
            35826 => (0.99, 0.80, 0.96), // Azbel
            35827 => (0.99, 0.70, 0.95), // Sotiyo
            35835 => (1.00, 0.80, 0.96), // Athanor (Refinery: -20% reaction time)
            35836 => (1.00, 0.75, 0.95), // Tatara (Refinery: -25% reaction time)
            _ => (1.0, 1.0, 1.0),
        };
        let (_, rig_time, rig_cost, _) =
            resolve_structure_rig_bonuses_full(ctx, snap, facility_id, st.solar_system_id);
        let eff_time_mod = base_time_mod * (1.0 - rig_time);
        let eff_cost_mod = cost_mod * (1.0 - rig_cost);
        return Some((
            st.type_id,
            st.owner_corp_id,
            st.solar_system_id,
            region_id,
            base_mat_mod,
            eff_time_mod,
            eff_cost_mod,
        ));
    }
    if let Some(it) = snap.items.get(&facility_id) {
        if matches!(it.type_id, 35825 | 35826 | 35827 | 35835 | 35836) {
            let sys_id = if (30_000_000..40_000_000).contains(&it.location_id) {
                it.location_id as u32
            } else {
                30_000_142
            };
            let region_id = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .map(|sys| sys.region_id)
                .unwrap_or(10000002);
            let (base_mat_mod, base_time_mod, cost_mod) = match it.type_id {
                35825 => (0.99, 0.85, 0.97),
                35826 => (0.99, 0.80, 0.96),
                35827 => (0.99, 0.70, 0.95),
                35835 => (1.00, 0.80, 0.96),
                35836 => (1.00, 0.75, 0.95),
                _ => (1.0, 1.0, 1.0),
            };
            let (_, rig_time, rig_cost, _) =
                resolve_structure_rig_bonuses_full(ctx, snap, facility_id, sys_id);
            let eff_time_mod = base_time_mod * (1.0 - rig_time);
            let eff_cost_mod = cost_mod * (1.0 - rig_cost);
            return Some((
                it.type_id,
                it.owner_id,
                sys_id,
                region_id,
                base_mat_mod,
                eff_time_mod,
                eff_cost_mod,
            ));
        }
    }
    for sys in ctx.sde.solar_systems.values() {
        if let Some(station) = sys.stations.iter().find(|s| s.station_id == facility_id) {
            return Some((
                station.station_type_id,
                station.owner_corporation_id,
                sys.solar_system_id,
                sys.region_id,
                1.0,
                1.0,
                1.0,
            ));
        }
    }
    None
}

/// Computes blueprint Estimated Item Value (EIV) per run from SDE manufacturing or reaction BOM.
pub fn compute_blueprint_eiv_per_run(
    ctx: &ServerContext,
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
    mats.into_iter()
        .map(|(tid, qty)| {
            let unit_price = ctx
                .sde
                .types
                .get(&tid)
                .map(|t| if t.base_price > 0.0 { t.base_price } else { 10.0 })
                .unwrap_or(10.0);
            (qty as f64) * unit_price
        })
        .sum::<f64>()
        .max(10_000.0)
}

pub fn compute_industry_job_costs(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    bp_type_id: TypeId,
    activity_id: u8,
    runs: u32,
    facility_id: i64,
) -> Result<IndustryJobCostMeta, BridgeRpcError> {
    compute_industry_job_costs_for_char(ctx, snap, None, bp_type_id, activity_id, runs, facility_id, false)
}

pub fn compute_industry_job_costs_ext(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    bp_type_id: TypeId,
    activity_id: u8,
    runs: u32,
    facility_id: i64,
    is_alpha_clone: bool,
) -> Result<IndustryJobCostMeta, BridgeRpcError> {
    compute_industry_job_costs_for_char(ctx, snap, None, bp_type_id, activity_id, runs, facility_id, is_alpha_clone)
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
    let Some(bp) = ctx.sde.blueprints.get(&bp_type_id) else {
        return Err(BridgeRpcError {
            code: "UnknownBlueprint".into(),
            message: format!("Blueprint type {bp_type_id} not found in SDE"),
        });
    };
    let eiv_per_run = compute_blueprint_eiv_per_run(ctx, bp, activity_id);
    let activity_eiv_factor = if matches!(activity_id, 3 | 4 | 5 | 8) { 0.02 } else { 1.0 };
    let total_eiv = eiv_per_run * (runs as f64) * activity_eiv_factor;
    let (_, _, sys_id, _, _, _, structure_cost_modifier) =
        resolve_facility_modifiers_and_system(ctx, snap, facility_id).unwrap_or((
            1529, 1000035, 30000142, 10000002, 1.0, 1.0, 1.0,
        ));
    let ind_idx = snap
        .system_states
        .get(&sys_id)
        .map(|s| s.industrial_index)
        .unwrap_or(4);
    let system_cost_index = (0.01 + 0.01 * (ind_idx as f64)).clamp(0.001, 0.50);
    let base_job_cost_isk =
        ((total_eiv * system_cost_index * structure_cost_modifier) * 100.0).round() / 100.0;
    let taxes = load_facility_taxes(snap, facility_id);
    let facility_tax_rate = taxes
        .get(&activity_id)
        .or_else(|| taxes.get(&1))
        .copied()
        .unwrap_or(0.10);
    let facility_tax_isk = ((base_job_cost_isk * facility_tax_rate) * 100.0).round() / 100.0;
    // Viridian/Equinox 4.0% SCC surcharge (reduced by 90% to 0.4% at Fulcrum for pirate subcaps)
    let fulcrum_bonus =
        is_fulcrum_pirate_ship_bonus_applicable(ctx, snap, char_id, bp_type_id, activity_id, facility_id);
    let scc_rate = if fulcrum_bonus { 0.004 } else { 0.04 };
    let scc_surcharge_isk = ((base_job_cost_isk * scc_rate) * 100.0).round() / 100.0;
    let alpha_clone_tax_isk = if is_alpha_clone {
        ((base_job_cost_isk * 0.0025) * 100.0).round() / 100.0
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

pub fn build_facility_pyrep(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
) -> Option<PyRep> {
    let (type_id, owner_id, sys_id, region_id, mat_mod, time_mod, cost_mod) =
        resolve_facility_modifiers_and_system(ctx, snap, facility_id)?;
    let (rig_mat, rig_time, rig_cost, rig_sec_mult) =
        resolve_structure_rig_bonuses_full(ctx, snap, facility_id, sys_id);
    let taxes = load_facility_taxes(snap, facility_id);
    let mfg_tax = taxes.get(&1).copied().unwrap_or(0.10);
    let ind_idx = snap
        .system_states
        .get(&sys_id)
        .map(|s| s.industrial_index)
        .unwrap_or(4);
    let sys_cost_index = (0.01 + 0.01 * (ind_idx as f64)).clamp(0.001, 0.50);
    let tax_kv = PyRep::keyval([
        ("1", PyRep::Real(taxes.get(&1).copied().unwrap_or(mfg_tax))),
        ("3", PyRep::Real(taxes.get(&3).copied().unwrap_or(mfg_tax))),
        ("4", PyRep::Real(taxes.get(&4).copied().unwrap_or(mfg_tax))),
        ("5", PyRep::Real(taxes.get(&5).copied().unwrap_or(mfg_tax))),
        ("8", PyRep::Real(taxes.get(&8).copied().unwrap_or(mfg_tax))),
        ("9", PyRep::Real(taxes.get(&9).copied().unwrap_or(mfg_tax))),
        ("manufacturing", PyRep::Real(taxes.get(&1).copied().unwrap_or(mfg_tax))),
        ("reaction", PyRep::Real(taxes.get(&9).copied().unwrap_or(mfg_tax))),
    ]);
    let sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(1.0);
    let mut acts = vec![
        PyRep::Integer(1),
        PyRep::Integer(3),
        PyRep::Integer(4),
        PyRep::Integer(5),
        PyRep::Integer(8),
    ];
    if matches!(type_id, 35835 | 35836) && sec < 0.45 {
        acts.push(PyRep::Integer(9));
    }
    let online = is_structure_industry_service_online(snap, facility_id, 1);
    Some(PyRep::keyval([
        ("facilityID", PyRep::Integer(facility_id)),
        ("typeID", PyRep::Integer(type_id as i64)),
        ("ownerID", PyRep::Integer(owner_id)),
        ("solarSystemID", PyRep::Integer(sys_id as i64)),
        ("regionID", PyRep::Integer(region_id as i64)),
        ("tax", PyRep::Real(mfg_tax)),
        ("taxRates", tax_kv),
        ("online", PyRep::Bool(online)),
        ("materialMultiplier", PyRep::Real(mat_mod * (1.0 - rig_mat))),
        ("timeMultiplier", PyRep::Real(time_mod)),
        ("costMultiplier", PyRep::Real(cost_mod)),
        ("materialModifier", PyRep::Real(mat_mod * (1.0 - rig_mat))),
        ("timeModifier", PyRep::Real(time_mod)),
        ("costModifier", PyRep::Real(cost_mod)),
        ("rigMaterialBonus", PyRep::Real(rig_mat)),
        ("rigTimeBonus", PyRep::Real(rig_time)),
        ("rigCostBonus", PyRep::Real(rig_cost)),
        ("rigSecurityMultiplier", PyRep::Real(rig_sec_mult)),
        ("systemCostIndex", PyRep::Real(sys_cost_index)),
        ("activities", PyRep::List(acts)),
    ]))
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
                let owner_id = args
                    .first()
                    .and_then(|a| a.as_i64())
                    .or_else(|| kwargs.get("ownerID").and_then(|v| v.as_i64()))
                    .unwrap_or(char_id);
                let facility_filter = args
                    .get(1)
                    .and_then(|a| a.as_i64())
                    .or_else(|| kwargs.get("facilityID").and_then(|v| v.as_i64()))
                    .filter(|&id| id > 0);
                let bps = ctx.db.with_snapshot(|snap| {
                    snap.items
                        .values()
                        .filter(|it| {
                            it.owner_id == owner_id
                                && facility_filter.map(|fid| it.location_id == fid).unwrap_or(true)
                                && ctx.sde.blueprints.contains_key(&it.type_id)
                        })
                        .filter_map(|it| {
                            let bp = ctx.sde.blueprints.get(&it.type_id)?;
                            let me = it
                                .dynamic_attributes
                                .get(&ATTR_BLUEPRINT_ME)
                                .copied()
                                .unwrap_or(0.0) as i64;
                            let te = it
                                .dynamic_attributes
                                .get(&ATTR_BLUEPRINT_TE)
                                .copied()
                                .unwrap_or(0.0) as i64;
                            let is_reaction = bp.activities.contains_key("reaction")
                                && !bp.activities.contains_key("manufacturing");
                            let act_id = if is_reaction { 9 } else { 1 };
                            let cost_meta = compute_industry_job_costs(
                                ctx,
                                snap,
                                it.type_id,
                                act_id,
                                1,
                                it.location_id,
                            )
                            .unwrap_or_default();
                            let mats_list: Vec<PyRep> = if is_reaction {
                                bp.activities
                                    .get("reaction")
                                    .map(|a| {
                                        a.materials
                                            .iter()
                                            .map(|m| {
                                                PyRep::keyval([
                                                    ("typeID", PyRep::Integer(m.type_id as i64)),
                                                    ("quantity", PyRep::Integer(m.quantity as i64)),
                                                ])
                                            })
                                            .collect()
                                    })
                                    .unwrap_or_default()
                            } else {
                                bp.materials
                                    .iter()
                                    .map(|m| {
                                        PyRep::keyval([
                                            ("typeID", PyRep::Integer(m.type_id as i64)),
                                            ("quantity", PyRep::Integer(m.quantity as i64)),
                                        ])
                                    })
                                    .collect()
                            };
                            let skills_list: Vec<PyRep> = bp
                                .activities
                                .get(if is_reaction { "reaction" } else { "manufacturing" })
                                .or_else(|| bp.activities.get("1"))
                                .map(|act| {
                                    act.skills
                                        .iter()
                                        .map(|sk| {
                                            PyRep::keyval([
                                                ("typeID", PyRep::Integer(sk.type_id as i64)),
                                                ("level", PyRep::Integer(sk.level as i64)),
                                            ])
                                        })
                                        .collect::<Vec<_>>()
                                })
                                .filter(|v| !v.is_empty())
                                .unwrap_or_else(|| {
                                    vec![PyRep::keyval([
                                        ("typeID", PyRep::Integer(3380)),
                                        ("level", PyRep::Integer(1)),
                                    ])]
                                });
                            Some(PyRep::keyval([
                                ("itemID", PyRep::Integer(it.item_id)),
                                ("blueprintID", PyRep::Integer(it.item_id)),
                                ("ownerID", PyRep::Integer(it.owner_id)),
                                ("blueprintTypeID", PyRep::Integer(bp.blueprint_type_id as i64)),
                                ("productTypeID", PyRep::Integer(bp.product_type_id as i64)),
                                ("facilityID", PyRep::Integer(it.location_id)),
                                ("materialEfficiency", PyRep::Integer(me)),
                                ("timeEfficiency", PyRep::Integer(te)),
                                ("original", PyRep::Bool(!it.is_blueprint_copy)),
                                (
                                    "runsRemaining",
                                    PyRep::Integer(it.blueprint_runs_remaining.unwrap_or(-1) as i64),
                                ),
                                ("materials", PyRep::List(mats_list)),
                                ("skills", PyRep::List(skills_list)),
                                ("estimatedItemValueISK", PyRep::Real(cost_meta.eiv_isk)),
                            ]))
                        })
                        .collect::<Vec<_>>()
                });
                return Some(Ok(PyRep::List(bps)));
            }
            let Some(raw_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidBlueprintTypeID".into(),
                    message: "GetBlueprintData requires a blueprint typeID or itemID".into(),
                }));
            };
            let (bp_type_id, bp_item_opt) = ctx.db.with_snapshot(|snap| {
                if let Some(it) = snap.items.get(&raw_id) {
                    if ctx.sde.blueprints.contains_key(&it.type_id) {
                        return (it.type_id, Some(it.clone()));
                    }
                }
                (raw_id as TypeId, None)
            });
            let Some(bp) = ctx.sde.blueprints.get(&bp_type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UnknownBlueprint".into(),
                    message: format!("Blueprint type {bp_type_id} not found in SDE"),
                }));
            };
            let facility_id = bp_item_opt
                .as_ref()
                .map(|i| i.location_id)
                .or(session.station_id)
                .or(session.structure_id)
                .unwrap_or(60_003_760);
            let is_reaction = bp.activities.contains_key("reaction")
                && !bp.activities.contains_key("manufacturing");
            let act_id = if is_reaction { 9 } else { 1 };
            let cost_meta = ctx.db.with_snapshot(|snap| {
                compute_industry_job_costs(ctx, snap, bp_type_id, act_id, 1, facility_id)
                    .unwrap_or_default()
            });
            let me_lvl = bp_item_opt
                .as_ref()
                .and_then(|i| i.dynamic_attributes.get(&ATTR_BLUEPRINT_ME).copied())
                .unwrap_or(0.0) as i64;
            let te_lvl = bp_item_opt
                .as_ref()
                .and_then(|i| i.dynamic_attributes.get(&ATTR_BLUEPRINT_TE).copied())
                .unwrap_or(0.0) as i64;
            let mats_rep: Vec<PyRep> = if is_reaction {
                bp.activities
                    .get("reaction")
                    .map(|a| {
                        a.materials
                            .iter()
                            .map(|m| {
                                PyRep::keyval([
                                    ("typeID", PyRep::Integer(m.type_id as i64)),
                                    ("quantity", PyRep::Integer(m.quantity as i64)),
                                ])
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                bp.materials
                    .iter()
                    .map(|m| {
                        PyRep::keyval([
                            ("typeID", PyRep::Integer(m.type_id as i64)),
                            ("quantity", PyRep::Integer(m.quantity as i64)),
                        ])
                    })
                    .collect()
            };
            let skills_rep: Vec<PyRep> = bp
                .activities
                .get(if is_reaction { "reaction" } else { "manufacturing" })
                .or_else(|| bp.activities.get("1"))
                .map(|act| {
                    act.skills
                        .iter()
                        .map(|sk| {
                            PyRep::keyval([
                                ("typeID", PyRep::Integer(sk.type_id as i64)),
                                ("level", PyRep::Integer(sk.level as i64)),
                            ])
                        })
                        .collect::<Vec<_>>()
                })
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| {
                    vec![PyRep::keyval([
                        ("typeID", PyRep::Integer(3380)),
                        ("level", PyRep::Integer(1)),
                    ])]
                });
            let reaction_time = bp
                .activities
                .get("reaction")
                .map(|a| a.time as i64)
                .unwrap_or(0);
            let prod_type_id = if is_reaction {
                bp.activities
                    .get("reaction")
                    .and_then(|a| a.products.first())
                    .map(|p| p.type_id)
                    .unwrap_or(bp.product_type_id)
            } else {
                bp.product_type_id
            };
            Some(Ok(PyRep::keyval([
                (
                    "blueprintID",
                    PyRep::Integer(bp_item_opt.as_ref().map(|i| i.item_id).unwrap_or(raw_id)),
                ),
                ("blueprintTypeID", PyRep::Integer(bp.blueprint_type_id as i64)),
                ("productTypeID", PyRep::Integer(prod_type_id as i64)),
                ("maxProductionLimit", PyRep::Integer(bp.max_production_limit as i64)),
                ("manufacturingTime", PyRep::Integer(bp.manufacturing_time_sec as i64)),
                ("researchMaterialTime", PyRep::Integer(bp.research_me_time_sec as i64)),
                ("researchTimeTime", PyRep::Integer(bp.research_te_time_sec as i64)),
                ("copyingTime", PyRep::Integer(bp.copy_time_sec as i64)),
                ("inventionTime", PyRep::Integer(bp.invention_time_sec as i64)),
                ("reactionTime", PyRep::Integer(reaction_time)),
                ("materialEfficiency", PyRep::Integer(me_lvl)),
                ("timeEfficiency", PyRep::Integer(te_lvl)),
                (
                    "original",
                    PyRep::Bool(!bp_item_opt.as_ref().map(|i| i.is_blueprint_copy).unwrap_or(false)),
                ),
                (
                    "runsRemaining",
                    PyRep::Integer(
                        bp_item_opt
                            .as_ref()
                            .and_then(|i| i.blueprint_runs_remaining)
                            .unwrap_or(-1) as i64,
                    ),
                ),
                ("materials", PyRep::List(mats_rep)),
                ("skills", PyRep::List(skills_rep)),
                ("estimatedItemValueISK", PyRep::Real(cost_meta.eiv_isk)),
                ("baseJobCost", PyRep::Real(cost_meta.base_job_cost_isk)),
                ("facilityTaxISK", PyRep::Real(cost_meta.facility_tax_isk)),
                ("sccSurchargeISK", PyRep::Real(cost_meta.scc_surcharge_isk)),
                ("totalJobCostISK", PyRep::Real(cost_meta.total_cost_isk)),
            ])))
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
                let mfg_range = match skill_lvl(24270) {
                    0 => 0,
                    l => (l as i64) * 5,
                };
                let sci_range = match skill_lvl(3409) {
                    0 => 0,
                    l => (l as i64) * 5,
                };
                let react_range = match skill_lvl(45750) {
                    0 => 0,
                    l => (l as i64) * 5,
                };
                PyRep::keyval([
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
        | ("facilityManager", "GetFacilitiesByID")
        | ("facilityManager", "GetFacilityLocations") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let explicit_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(ids) | PyRep::Tuple(ids)) => {
                    ids.iter().filter_map(|v| v.as_i64()).collect()
                }
                _ => Vec::new(),
            };
            let current_sys = session.solar_system_id.unwrap_or(30_000_142);
            let facilities = ctx.db.with_snapshot(|snap| {
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
                    if let Some(f_rep) = build_facility_pyrep(ctx, snap, 60_003_760) {
                        list.push(f_rep);
                    }
                }
                list
            });
            Some(Ok(PyRep::List(facilities)))
        }

        ("facilityManager", "GetFacility") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let facility_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.station_id)
                .or(session.structure_id)
                .unwrap_or(60_003_760);
            let res = ctx.db.with_snapshot(|snap| {
                build_facility_pyrep(ctx, snap, facility_id).ok_or_else(|| BridgeRpcError {
                    code: "FacilityNotFound".into(),
                    message: format!("Industry facility {facility_id} not found"),
                })
            });
            Some(res)
        }

        ("facilityManager", "GetFacilityTaxes") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let facility_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.station_id)
                .or(session.structure_id)
                .unwrap_or(60_003_760);
            let res = ctx.db.with_snapshot(|snap| {
                if resolve_facility_modifiers_and_system(ctx, snap, facility_id).is_none() {
                    return Err(BridgeRpcError {
                        code: "FacilityNotFound".into(),
                        message: format!("Industry facility {facility_id} not found"),
                    });
                }
                let taxes = load_facility_taxes(snap, facility_id);
                let mfg_tax = taxes.get(&1).copied().unwrap_or(0.10);
                Ok(PyRep::keyval([
                    ("facilityID", PyRep::Integer(facility_id)),
                    ("tax", PyRep::Real(mfg_tax)),
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
                    }));
                }
            }
            if updates.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidFacilityTaxRate".into(),
                    message: "SetFacilityTaxes requires at least one valid activity tax rate"
                        .into(),
                }));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if resolve_facility_modifiers_and_system(ctx, snap, facility_id).is_none() {
                    return Err(BridgeRpcError {
                        code: "FacilityNotFound".into(),
                        message: format!("Industry facility {facility_id} not found"),
                    });
                }
                if let Some(st) = snap.structures.get(&facility_id) {
                    let char_corp = snap
                        .characters
                        .get(&char_id)
                        .map(|c| c.corporation_id)
                        .unwrap_or(0);
                    if st.owner_corp_id != char_corp && st.owner_corp_id != char_id {
                        return Err(BridgeRpcError {
                            code: "FacilityPermissionDenied".into(),
                            message: format!(
                                "Character {char_id} (corp {char_corp}) does not own structure {facility_id} (owner corp {})",
                                st.owner_corp_id
                            ),
                        });
                    }
                }
                if let Some(it) = snap.items.get(&facility_id) {
                    if matches!(it.type_id, 35825 | 35826 | 35827 | 35835 | 35836) {
                        let char_corp = snap
                            .characters
                            .get(&char_id)
                            .map(|c| c.corporation_id)
                            .unwrap_or(0);
                        if it.owner_id != char_corp && it.owner_id != char_id {
                            return Err(BridgeRpcError {
                                code: "FacilityPermissionDenied".into(),
                                message: format!(
                                    "Character {char_id} (corp {char_corp}) does not own structure {facility_id} (owner {})",
                                    it.owner_id
                                ),
                            });
                        }
                    }
                }
                let mut taxes = load_facility_taxes(snap, facility_id);
                for (act, rate) in updates {
                    taxes.insert(act, rate);
                }
                save_facility_taxes(snap, facility_id, &taxes);
                let mfg_tax = taxes.get(&1).copied().unwrap_or(0.10);
                Ok(PyRep::keyval([
                    ("facilityID", PyRep::Integer(facility_id)),
                    ("tax", PyRep::Real(mfg_tax)),
                    ("1", PyRep::Real(taxes.get(&1).copied().unwrap_or(mfg_tax))),
                    ("3", PyRep::Real(taxes.get(&3).copied().unwrap_or(mfg_tax))),
                    ("4", PyRep::Real(taxes.get(&4).copied().unwrap_or(mfg_tax))),
                    ("5", PyRep::Real(taxes.get(&5).copied().unwrap_or(mfg_tax))),
                    ("8", PyRep::Real(taxes.get(&8).copied().unwrap_or(mfg_tax))),
                    ("9", PyRep::Real(taxes.get(&9).copied().unwrap_or(mfg_tax))),
                    ("manufacturing", PyRep::Real(taxes.get(&1).copied().unwrap_or(mfg_tax))),
                ]))
            });
            Some(res)
        }

        ("facilityManager", "GetMaxActivityModifiers") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::keyval([
                ("materialMultiplier", PyRep::Real(0.99)),
                ("timeMultiplier", PyRep::Real(0.70)),
                ("costMultiplier", PyRep::Real(0.95)),
            ])))
        }

        // System Cost Index & 24h Job Volume (`feat:industry.cost-index`)
        ("map", "GetIndustryCostModifier") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = args
                .first()
                .and_then(|a| a.as_i64())
                .map(|v| v as u32)
                .or(session.solar_system_id)
                .unwrap_or(30_000_142);
            let res = ctx.db.with_snapshot(|snap| {
                let ind_idx = snap
                    .system_states
                    .get(&sys_id)
                    .map(|s| s.industrial_index)
                    .unwrap_or(4);
                let base_idx = (0.01 + 0.01 * (ind_idx as f64)).clamp(0.001, 0.50);
                let mut act_runs: HashMap<u8, u64> = HashMap::new();
                for j in snap.industry_jobs.values() {
                    let j_sys = resolve_facility_modifiers_and_system(ctx, snap, j.facility_id)
                        .map(|(_, _, s, _, _, _, _)| s)
                        .unwrap_or(30_000_142);
                    if j_sys == sys_id {
                        *act_runs.entry(j.activity_id).or_insert(0) += u64::from(j.runs);
                    }
                }
                let idx_for = |act: u8, mult: f64| -> f64 {
                    let runs = act_runs.get(&act).copied().unwrap_or(0) as f64;
                    ((base_idx * mult) + 0.0005 * runs).clamp(0.001, 0.50)
                };
                PyRep::keyval([
                    ("solarSystemID", PyRep::Integer(sys_id as i64)),
                    ("1", PyRep::Real(idx_for(1, 1.0))),
                    ("3", PyRep::Real(idx_for(3, 0.6))),
                    ("4", PyRep::Real(idx_for(4, 0.6))),
                    ("5", PyRep::Real(idx_for(5, 0.5))),
                    ("8", PyRep::Real(idx_for(8, 0.8))),
                    ("9", PyRep::Real(idx_for(9, 0.9))),
                    ("manufacturing", PyRep::Real(idx_for(1, 1.0))),
                    ("research_te", PyRep::Real(idx_for(3, 0.6))),
                    ("research_me", PyRep::Real(idx_for(4, 0.6))),
                    ("copying", PyRep::Real(idx_for(5, 0.5))),
                    ("invention", PyRep::Real(idx_for(8, 0.8))),
                    ("reaction", PyRep::Real(idx_for(9, 0.9))),
                ])
            });
            Some(Ok(res))
        }

        ("map", "GetIndustryJobsOverLast24Hours") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let res = ctx.db.with_snapshot(|snap| {
                let mut counts: HashMap<u32, i64> = HashMap::new();
                for j in snap.industry_jobs.values() {
                    let j_sys = resolve_facility_modifiers_and_system(ctx, snap, j.facility_id)
                        .map(|(_, _, s, _, _, _, _)| s)
                        .unwrap_or(30_000_142);
                    *counts.entry(j_sys).or_insert(0) += i64::from(j.runs);
                }
                let entries: Vec<(PyRep, PyRep)> = counts
                    .into_iter()
                    .map(|(sys, cnt)| (PyRep::Integer(sys as i64), PyRep::Integer(cnt)))
                    .collect();
                PyRep::Dict(entries)
            });
            Some(Ok(res))
        }

        _ => None,
    }
}
