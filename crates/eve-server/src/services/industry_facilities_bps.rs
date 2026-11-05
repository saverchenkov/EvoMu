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
    #[serde(default)]
    pub consumed_materials: Vec<(TypeId, i32)>,
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

/// Checks whether an Upwell structure's industry/reaction service is online and fueled (`feat:industry.job-pause-on-service-offline`, `feat:structures.service-industry`).
pub fn is_structure_industry_service_online(
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
    activity_id: u8,
) -> bool {
    let Some(st) = snap.structures.get(&facility_id) else { return true };
    if st.state == "Destroyed" || !st.quantum_core_installed || st.fuel_blocks == 0 {
        return false;
    }
    snap.items.values().any(|it| {
        it.location_id == facility_id
            && (164..=171).contains(&(it.flag as u16))
            && match activity_id {
                1 => matches!(it.type_id, 35878 | 35881 | 35877),
                3..=5 => matches!(it.type_id, 35891 | 45550 | 35879 | 35880),
                8 => matches!(it.type_id, 35886 | 35879 | 35880),
                9 | 11 => matches!(it.type_id, 45537..=45539 | 35882..=35884 | 35894..=35896),
                _ => matches!(it.type_id, 35879 | 35880 | 35891 | 35886),
            }
            && snap.module_online_states.get(&it.item_id).copied().unwrap_or(true)
            && it.dynamic_attributes.get(&2).copied().unwrap_or(1.0) > 0.0
    })
}

/// Enforces exact Standup reactor service modules for Composite (`1888`), Hybrid (`1889`), and Biochemical (`1890`) reactions.
pub fn is_structure_industry_service_online_for_bp(
    sde: &eve_sde::SdeRegistry, snap: &eve_db::WorldStateSnapshot, facility_id: i64, activity_id: u8, bp_type_id: TypeId,
) -> bool {
    if !is_structure_industry_service_online(snap, facility_id, activity_id) { return false; }
    if !matches!(activity_id, 9 | 11) || !snap.structures.contains_key(&facility_id) { return true; }
    let Some(req_tid) = (match sde.types.get(&bp_type_id).map(|t| t.group_id) {
        Some(1888) => Some(45537u32), Some(1889) => Some(45538u32), Some(1890) => Some(45539u32), _ => None,
    }) else { return true };
    snap.items.values().any(|it| {
        it.location_id == facility_id && (164..=171).contains(&(it.flag as u16)) && it.type_id == req_tid
            && snap.module_online_states.get(&it.item_id).copied().unwrap_or(true)
            && it.dynamic_attributes.get(&2).copied().unwrap_or(1.0) > 0.0
    })
}

/// Synchronizes active/paused industry jobs when a host structure loses or regains fuel/service (`feat:industry.job-pause-on-service-offline`, `feat:lifecycle.industry-pause-and-many`).
pub fn sync_industry_job_pause_states(snap: &mut eve_db::WorldStateSnapshot, now_unix: u64) {
    sync_industry_job_pause_states_at(snap, now_unix, false);
}

/// Runs the sync at the exact moment a service module is offlined/onlined: jobs finished by then stay deliverable.
pub fn sync_industry_job_pause_states_on_service_change(snap: &mut eve_db::WorldStateSnapshot, now_unix: u64) {
    sync_industry_job_pause_states_at(snap, now_unix, true);
}

fn sync_industry_job_pause_states_at(snap: &mut eve_db::WorldStateSnapshot, now_unix: u64, offline_is_now: bool) {
    let job_ids: Vec<i64> = snap.industry_jobs.keys().copied().collect();
    for jid in job_ids {
        let Some((facility_id, activity_id, status, start_unix, duration_seconds)) = snap
            .industry_jobs
            .get(&jid)
            .map(|j| (j.facility_id, j.activity_id, j.status.clone(), j.start_unix, j.duration_seconds))
        else {
            continue;
        };
        if status != "Active" && status != "Paused" {
            continue;
        }
        let online = is_structure_industry_service_online(snap, facility_id, activity_id);
        if !online && status == "Active" {
            let ready_at = start_unix.saturating_add(duration_seconds);
            let offline_opt = resolve_facility_offline_moment_opt(snap, facility_id).or(offline_is_now.then_some(now_unix));
            let mut meta = load_job_cost_meta(snap, jid).unwrap_or_default();
            if meta.ready_before_offline { continue; }
            if offline_opt.is_some_and(|m| ready_at <= m) {
                if offline_is_now {
                    meta.ready_before_offline = true;
                    save_job_cost_meta(snap, jid, &meta);
                }
                continue;
            }
            let pause_ts = offline_opt.unwrap_or(now_unix).max(start_unix);
            if let Some(j_mut) = snap.industry_jobs.get_mut(&jid) {
                j_mut.status = "Paused".into();
            }
            if meta.paused_at_unix.is_none() {
                meta.paused_at_unix = Some(pause_ts);
                save_job_cost_meta(snap, jid, &meta);
            }
        } else if online && status == "Paused" {
            let mut meta = load_job_cost_meta(snap, jid).unwrap_or_default();
            let pause_start = meta.paused_at_unix.unwrap_or(now_unix);
            let paused_secs = now_unix.saturating_sub(pause_start);
            if let Some(j_mut) = snap.industry_jobs.get_mut(&jid) {
                j_mut.duration_seconds = j_mut.duration_seconds.saturating_add(paused_secs);
                j_mut.status = "Active".into();
            }
            meta.paused_at_unix = None;
            save_job_cost_meta(snap, jid, &meta);
        }
    }
}

pub fn resolve_facility_offline_moment_opt(snap: &eve_db::WorldStateSnapshot, facility_id: i64) -> Option<u64> {
    crate::services::station_structure::structure_upwell_ops::load_ext_state(snap)
        .lifecycle
        .get(&facility_id)
        .and_then(|l| l.low_power_since_unix)
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
        let job_sys = resolve_facility_modifiers_and_system(ctx, snap, j.facility_id).map(|m| m.2);
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
        });
    };
    let eiv_per_run = compute_blueprint_eiv_per_run_with_snap(ctx, Some(snap), bp, activity_id);
    let total_eiv = eiv_per_run * crate::services::industry_pi::compute_activity_ptv_multiplier(activity_id, start_step, runs);
    let fallback_sys = char_id
        .and_then(|cid| snap.characters.get(&cid).map(|c| c.solar_system_id))
        .unwrap_or_else(|| ctx.sde.solar_systems.keys().copied().next().unwrap_or(0));
    let fallback_reg = ctx.sde.solar_systems.get(&fallback_sys).map(|s| s.region_id).unwrap_or(0);
    let (_, _, sys_id, _, _, _, structure_cost_modifier) =
        resolve_facility_modifiers_and_system(ctx, snap, facility_id).unwrap_or((
            1529, 0, fallback_sys, fallback_reg, 1.0, 1.0, 1.0,
        ));
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

pub fn build_facility_pyrep(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
) -> Option<PyRep> {
    let (type_id, owner_id, sys_id, region_id, mat_mod, time_mod, cost_mod) =
        resolve_facility_modifiers_and_system(ctx, snap, facility_id)?;
    let (rig_mat, rig_time, rig_cost, rig_sec_mult) =
        resolve_structure_rig_bonuses_full(ctx, snap, facility_id, sys_id);
    let taxes = load_facility_taxes_with_sde(Some(&ctx.sde), snap, facility_id);
    let mfg_tax = taxes.get(&1).copied().unwrap_or(0.0025);
    let sys_cost_index = compute_canonical_system_cost_index(ctx, snap, sys_id, 1);
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
                let v24_tuple_sig = args.len() >= 2 || kwargs.contains_key("facilityID");
                let res = ctx.db.with_snapshot(|snap| -> Result<PyRep, BridgeRpcError> {
                    if owner_id != char_id && !session.has_gm_role() {
                        const CORP_BP_ROLES: u64 = eve_db::corp_roles::ROLE_DIRECTOR
                            | eve_db::corp_roles::ROLE_FACTORY_MANAGER
                            | eve_db::corp_roles::ROLE_RENT_FACTORY_FACILITY
                            | eve_db::corp_roles::ROLE_RENT_RESEARCH_FACILITY;
                        let caller_corp_id = session.corporation_id.or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id)).unwrap_or(0);
                        let has_corp_access = owner_id == caller_corp_id
                            && !(1_000_000..2_000_000).contains(&owner_id)
                            && ((session.corp_role & CORP_BP_ROLES) != 0
                                || snap.corporations.get(&owner_id).is_some_and(|c| c.ceo_id == char_id || c.member_roles.get(&char_id).is_some_and(|&r| (r & CORP_BP_ROLES) != 0))
                                || snap.corp_role_assignments.get(&(owner_id, char_id)).is_some_and(|a| (a.roles & CORP_BP_ROLES) != 0));
                        if !has_corp_access {
                            return Err(BridgeRpcError { code: "OwnerAccessDenied".into(), message: format!("Character {char_id} is not authorized to query blueprints for owner {owner_id}") });
                        }
                    }
                    let mut active_jobs: HashMap<i64, i64> = HashMap::new();
                    for j in snap.industry_jobs.values() {
                        if !matches!(j.status.as_str(), "Delivered" | "Completed" | "Cancelled" | "Reverted" | "Failed") {
                            active_jobs.entry(j.blueprint_item_id).or_insert(j.job_id);
                        }
                    }
                    let mut owned: Vec<&eve_db::InventoryItemRecord> = snap.items.values()
                        .filter(|it| it.owner_id == owner_id && ctx.sde.blueprints.contains_key(&it.type_id))
                        .collect();
                    owned.sort_by_key(|it| it.item_id);
                    let mut counts: std::collections::BTreeMap<Option<i64>, i64> = std::collections::BTreeMap::new();
                    let mut bps = Vec::new();
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
                        let mats_list: Vec<PyRep> = if is_reaction {
                            bp.activities.get("reaction").map(|a| a.materials.iter().map(|m| PyRep::keyval([("typeID", PyRep::Integer(m.type_id as i64)), ("quantity", PyRep::Integer(m.quantity as i64))])).collect()).unwrap_or_default()
                        } else {
                            bp.materials.iter().map(|m| PyRep::keyval([("typeID", PyRep::Integer(m.type_id as i64)), ("quantity", PyRep::Integer(m.quantity as i64))])).collect()
                        };
                        let skills_list: Vec<PyRep> = bp.activities.get(if is_reaction { "reaction" } else { "manufacturing" }).or_else(|| bp.activities.get("1"))
                            .map(|act| act.skills.iter().map(|sk| PyRep::keyval([("typeID", PyRep::Integer(sk.type_id as i64)), ("level", PyRep::Integer(sk.level as i64))])).collect::<Vec<_>>())
                            .filter(|v| !v.is_empty())
                            .unwrap_or_else(|| vec![PyRep::keyval([("typeID", PyRep::Integer(3380)), ("level", PyRep::Integer(1))])]);
                        let prod_type_id = if is_reaction {
                            bp.activities.get("reaction").and_then(|a| a.products.first()).map(|p| p.type_id).unwrap_or(bp.product_type_id)
                        } else {
                            bp.product_type_id
                        };
                        let runs = if it.is_blueprint_copy { i64::from(it.blueprint_runs_remaining.unwrap_or(1)) } else { -1 };
                        let qty_code = if it.is_blueprint_copy { -2 } else if it.singleton { -1 } else { i64::from(it.quantity.max(1)) };
                        let flag_i64 = it.flag as u16 as i64;
                        bps.push(PyRep::keyval([
                            ("typeID", PyRep::Integer(bp.blueprint_type_id as i64)),
                            ("blueprintTypeID", PyRep::Integer(bp.blueprint_type_id as i64)),
                            ("itemID", PyRep::Integer(it.item_id)),
                            ("blueprintID", PyRep::Integer(it.item_id)),
                            ("ownerID", PyRep::Integer(it.owner_id)),
                            ("productTypeID", PyRep::Integer(prod_type_id as i64)),
                            ("facilityID", fac_opt.map(PyRep::Integer).unwrap_or(PyRep::None)),
                            ("locationID", PyRep::Integer(it.location_id)),
                            ("locationTypeID", PyRep::Integer(loc_type_id)),
                            ("locationFlagID", PyRep::Integer(flag_i64)),
                            ("flagID", PyRep::Integer(flag_i64)),
                            ("solarSystemID", sys_opt.map(PyRep::Integer).unwrap_or(PyRep::None)),
                            ("materialEfficiency", PyRep::Integer(me)),
                            ("timeEfficiency", PyRep::Integer(te)),
                            ("original", PyRep::Bool(!it.is_blueprint_copy)),
                            ("runs", PyRep::Integer(runs)),
                            ("runsRemaining", PyRep::Integer(runs)),
                            ("quantity", PyRep::Integer(qty_code)),
                            ("jobID", active_jobs.get(&it.item_id).copied().map(PyRep::Integer).unwrap_or(PyRep::None)),
                            ("isImpounded", PyRep::Bool(it.flag == eve_db::InventoryFlag::Impounded)),
                            ("materials", PyRep::List(mats_list)),
                            ("skills", PyRep::List(skills_list)),
                            ("estimatedItemValueISK", PyRep::Real(cost_meta.eiv_isk)),
                        ]));
                    }
                    if v24_tuple_sig {
                        let bp_counts = PyRep::Dict(counts.into_iter().map(|(f, c)| (f.map(PyRep::Integer).unwrap_or(PyRep::None), PyRep::Integer(c))).collect());
                        Ok(PyRep::Tuple(vec![PyRep::List(bps), bp_counts]))
                    } else {
                        Ok(PyRep::List(bps))
                    }
                });
                return Some(res);
            }
            let Some(raw_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidBlueprintTypeID".into(),
                    message: "GetBlueprintData requires a blueprint typeID or itemID".into(),
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
            let res = PyRep::keyval([
                ("blueprintID", PyRep::Integer(bp_item_opt.as_ref().map(|i| i.item_id).unwrap_or(raw_id))),
                ("blueprintTypeID", PyRep::Integer(bp.blueprint_type_id as i64)),
                ("productTypeID", PyRep::Integer(prod_type_id as i64)),
                ("facilityID", PyRep::Integer(facility_id)), ("locationID", PyRep::Integer(facility_id)),
                ("maxProductionLimit", PyRep::Integer(bp.max_production_limit as i64)),
                ("manufacturingTime", PyRep::Integer(bp.manufacturing_time_sec as i64)),
                ("researchMaterialTime", PyRep::Integer(bp.research_me_time_sec as i64)),
                ("researchTimeTime", PyRep::Integer(bp.research_te_time_sec as i64)),
                ("copyingTime", PyRep::Integer(bp.copy_time_sec as i64)),
                ("inventionTime", PyRep::Integer(bp.invention_time_sec as i64)),
                ("reactionTime", PyRep::Integer(reaction_time)),
                ("materialEfficiency", PyRep::Integer(me_lvl)), ("timeEfficiency", PyRep::Integer(te_lvl)),
                ("original", PyRep::Bool(!bp_item_opt.as_ref().map(|i| i.is_blueprint_copy).unwrap_or(false))),
                ("runsRemaining", PyRep::Integer(bp_item_opt.as_ref().and_then(|i| i.blueprint_runs_remaining).unwrap_or(-1) as i64)),
                ("materials", PyRep::List(mats_rep.clone())),
                ("skills", PyRep::List(skills_rep)),
                ("estimatedItemValueISK", PyRep::Real(cost_meta.eiv_isk)),
                ("baseJobCost", PyRep::Real(cost_meta.base_job_cost_isk)),
                ("facilityTaxISK", PyRep::Real(cost_meta.facility_tax_isk)),
                ("sccSurchargeISK", PyRep::Real(cost_meta.scc_surcharge_isk)),
                ("totalJobCostISK", PyRep::Real(cost_meta.total_cost_isk)),
            ]);
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
                        Some(PyRep::keyval([("facilityID", PyRep::Integer(fid)), ("itemID", PyRep::Integer(fid)), ("typeID", PyRep::Integer(type_id)), ("solarSystemID", PyRep::Integer(sys_id as i64)), ("regionID", PyRep::Integer(reg)), ("flagID", PyRep::Integer(4)), ("ownerID", PyRep::Integer(char_id))]))
                    }).collect::<Vec<_>>()
                });
                return Some(Ok(PyRep::List(locs)));
            }
            let fid = resolve_caller_facility_id(ctx, session, char_id, args.first().and_then(|a| a.as_i64()));
            let owner_filter = args.get(1).and_then(|a| a.as_i64());
            let locs = ctx.db.with_snapshot(|snap| {
                let sys_id = snap.structures.get(&fid).map(|s| s.solar_system_id)
                    .or_else(|| ctx.sde.stations.get(&fid).map(|st| st.solar_system_id as u32))
                    .or(session.solar_system_id)
                    .or_else(|| snap.characters.get(&char_id).map(|c| c.solar_system_id))
                    .unwrap_or(30_000_142);
                let type_id = snap.structures.get(&fid).map(|s| s.type_id as i64)
                    .or_else(|| ctx.sde.stations.get(&fid).map(|st| st.station_type_id as i64))
                    .unwrap_or(1529);
                let reg = ctx.sde.solar_systems.get(&sys_id).map(|sys| sys.region_id as i64).unwrap_or(0);
                let mut out = Vec::new();
                if owner_filter.is_none() || owner_filter == Some(char_id) {
                    out.push(PyRep::keyval([("facilityID", PyRep::Integer(fid)), ("itemID", PyRep::Integer(fid)), ("typeID", PyRep::Integer(type_id)), ("ownerID", PyRep::Integer(char_id)), ("flagID", PyRep::Integer(4)), ("solarSystemID", PyRep::Integer(sys_id as i64)), ("regionID", PyRep::Integer(reg))]));
                }
                let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
                if corp_id > 0 && (owner_filter.is_none() || owner_filter == Some(corp_id)) {
                    if let Some(corp) = snap.corporations.get(&corp_id) {
                        let roles = corp.member_roles.get(&char_id).copied().unwrap_or(session.corp_role);
                        let full = char_id == corp.ceo_id || (roles & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_FACTORY_MANAGER)) != 0;
                        for d in 0..7u8 {
                            let div_mask = (128u64 << d) | (1024u64 << d) | (1_048_576u64 << d) | (8_388_608u64 << d);
                            if full || (roles & div_mask) != 0 {
                                let flag = 115 + i64::from(d);
                                out.push(PyRep::keyval([("facilityID", PyRep::Integer(fid)), ("itemID", PyRep::Integer(fid)), ("typeID", PyRep::Integer(type_id)), ("ownerID", PyRep::Integer(corp_id)), ("flagID", PyRep::Integer(flag)), ("solarSystemID", PyRep::Integer(sys_id as i64)), ("regionID", PyRep::Integer(reg))]));
                            }
                        }
                    }
                }
                out
            });
            Some(Ok(PyRep::List(locs)))
        }

        ("facilityManager", "GetFacility") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let facility_id = resolve_caller_facility_id(ctx, session, char_id, args.first().and_then(|a| a.as_i64()));
            let res = ctx.db.with_snapshot(|snap| {
                build_facility_pyrep(ctx, snap, facility_id).ok_or_else(|| BridgeRpcError {
                    code: "FacilityNotFound".into(),
                    message: format!("Industry facility {facility_id} not found"),
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
                    return Err(BridgeRpcError { code: "FacilityNotFound".into(), message: format!("Industry facility {facility_id} not found") });
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
            if updates.iter().any(|&(act, rate)| act != 0 && rate > 0.10) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidFacilityTaxRate".into(),
                    message: "Structure industry facility tax rate cannot exceed 10%".into(),
                }));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some((_, _, sys_id, ..)) = resolve_facility_modifiers_and_system(ctx, snap, facility_id) else {
                    return Err(BridgeRpcError {
                        code: "FacilityNotFound".into(),
                        message: format!("Industry facility {facility_id} not found"),
                    });
                };
                let owner_id = snap
                    .structures
                    .get(&facility_id)
                    .map(|st| st.owner_corp_id)
                    .or_else(|| {
                        snap.items.get(&facility_id).and_then(|it| {
                            if matches!(
                                it.type_id,
                                35825 | 35826 | 35827 | 35832 | 35833 | 35834 | 35835 | 35836
                            ) {
                                Some(it.owner_id)
                            } else {
                                None
                            }
                        })
                    });
                if let Some(owner_corp_id) = owner_id {
                    let char_corp = snap
                        .characters
                        .get(&char_id)
                        .map(|c| c.corporation_id)
                        .or(session.corporation_id)
                        .unwrap_or(0);
                    if owner_corp_id != char_corp && owner_corp_id != char_id {
                        return Err(BridgeRpcError {
                            code: "FacilityPermissionDenied".into(),
                            message: format!(
                                "Character {char_id} (corp {char_corp}) does not own structure {facility_id} (owner {owner_corp_id})"
                            ),
                        });
                    }
                    if owner_corp_id != char_id {
                        let req_roles = eve_db::corp_roles::ROLE_DIRECTOR
                            | eve_db::corp_roles::ROLE_STATION_MANAGER;
                        let has_role = session.has_gm_role()
                            || (session.corp_role & req_roles) != 0
                            || snap.corporations.get(&owner_corp_id).is_some_and(|c| {
                                c.ceo_id == char_id
                                    || c.member_roles
                                        .get(&char_id)
                                        .is_some_and(|&r| (r & req_roles) != 0)
                            })
                            || snap
                                .corp_role_assignments
                                .get(&(owner_corp_id, char_id))
                                .is_some_and(|a| (a.roles & req_roles) != 0);
                        if !has_role {
                            return Err(BridgeRpcError {
                                code: "InsufficientCorpRoles".into(),
                                message:
                                    "Setting facility taxes requires Director or Station Manager role"
                                        .into(),
                            });
                        }
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
            Some(Ok(PyRep::keyval([
                ("materialMultiplier", PyRep::Real(0.99)),
                ("timeMultiplier", PyRep::Real(0.70)),
                ("costMultiplier", PyRep::Real(0.95)),
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
