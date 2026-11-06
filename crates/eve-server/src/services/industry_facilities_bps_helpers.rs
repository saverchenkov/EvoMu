//! Structure industry/reaction service checks, job pause synchronization, and Fulcrum bonus helpers.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_sde::TypeId;

/// Checks whether an Upwell structure's industry/reaction service is online and fueled (`feat:industry.job-pause-on-service-offline`, `feat:structures.service-industry`).
pub fn is_structure_industry_service_online(
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
    activity_id: u8,
) -> bool {
    let Some(st) = snap.structures.get(&facility_id) else {
        return true;
    };
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
    sde: &eve_sde::SdeRegistry,
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
    activity_id: u8,
    bp_type_id: TypeId,
) -> bool {
    if !is_structure_industry_service_online(snap, facility_id, activity_id) {
        return false;
    }
    if !matches!(activity_id, 9 | 11) || !snap.structures.contains_key(&facility_id) {
        return true;
    }
    let Some(req_tid) = (match sde.types.get(&bp_type_id).map(|t| t.group_id) {
        Some(1888) => Some(45537u32),
        Some(1889) => Some(45538u32),
        Some(1890) => Some(45539u32),
        _ => None,
    }) else {
        return true;
    };
    snap.items.values().any(|it| {
        it.location_id == facility_id
            && (164..=171).contains(&(it.flag as u16))
            && it.type_id == req_tid
            && snap.module_online_states.get(&it.item_id).copied().unwrap_or(true)
            && it.dynamic_attributes.get(&2).copied().unwrap_or(1.0) > 0.0
    })
}

/// Synchronizes active/paused industry jobs when a host structure loses or regains fuel/service (`feat:industry.job-pause-on-service-offline`, `feat:lifecycle.industry-pause-and-many`).
pub fn sync_industry_job_pause_states(snap: &mut eve_db::WorldStateSnapshot, now_unix: u64) {
    sync_industry_job_pause_states_at(snap, now_unix, false);
}

/// Runs the sync at the exact moment a service module is offlined/onlined: jobs finished by then stay deliverable.
pub fn sync_industry_job_pause_states_on_service_change(
    snap: &mut eve_db::WorldStateSnapshot,
    now_unix: u64,
) {
    sync_industry_job_pause_states_at(snap, now_unix, true);
}

pub(crate) fn sync_industry_job_pause_states_at(
    snap: &mut eve_db::WorldStateSnapshot,
    now_unix: u64,
    offline_is_now: bool,
) {
    let job_ids: Vec<i64> = snap.industry_jobs.keys().copied().collect();
    for jid in job_ids {
        let Some((facility_id, activity_id, status, start_unix, duration_seconds)) = snap
            .industry_jobs
            .get(&jid)
            .map(|j| {
                (
                    j.facility_id,
                    j.activity_id,
                    j.status.clone(),
                    j.start_unix,
                    j.duration_seconds,
                )
            })
        else {
            continue;
        };
        if status != "Active" && status != "Paused" {
            continue;
        }
        let online = is_structure_industry_service_online(snap, facility_id, activity_id);
        if !online && status == "Active" {
            let ready_at = start_unix.saturating_add(duration_seconds);
            let offline_opt = resolve_facility_offline_moment_opt(snap, facility_id)
                .or(offline_is_now.then_some(now_unix));
            let mut meta = super::load_job_cost_meta(snap, jid).unwrap_or_default();
            if meta.ready_before_offline {
                continue;
            }
            if offline_opt.is_some_and(|m| ready_at <= m) {
                if offline_is_now {
                    meta.ready_before_offline = true;
                    super::save_job_cost_meta(snap, jid, &meta);
                }
                continue;
            }
            let pause_ts = offline_opt.unwrap_or(now_unix).max(start_unix);
            if let Some(j_mut) = snap.industry_jobs.get_mut(&jid) {
                j_mut.status = "Paused".into();
            }
            if meta.paused_at_unix.is_none() {
                meta.paused_at_unix = Some(pause_ts);
                super::save_job_cost_meta(snap, jid, &meta);
            }
        } else if online && status == "Paused" {
            let mut meta = super::load_job_cost_meta(snap, jid).unwrap_or_default();
            let pause_start = meta.paused_at_unix.unwrap_or(now_unix);
            let paused_secs = now_unix.saturating_sub(pause_start);
            if let Some(j_mut) = snap.industry_jobs.get_mut(&jid) {
                j_mut.duration_seconds = j_mut.duration_seconds.saturating_add(paused_secs);
                j_mut.status = "Active".into();
            }
            meta.paused_at_unix = None;
            super::save_job_cost_meta(snap, jid, &meta);
        }
    }
}

pub fn resolve_facility_offline_moment_opt(
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
) -> Option<u64> {
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
    let Some(cid) = char_id.filter(|_| activity_id == 1) else {
        return false;
    };
    let pirate_enlisted = snap
        .characters
        .get(&cid)
        .and_then(|c| c.war_faction_id)
        .is_some_and(|wf| matches!(wf, 500_010 | 500_011));
    let is_fulcrum = facility_id == 60_015_187
        || super::resolve_facility_modifiers_and_system(ctx, snap, facility_id)
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
