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

/// Enforces exact Standup reactor service modules for Composite (`1888`), Hybrid (`1889`), and
/// Biochemical (`1890`) reactions via SDE `industry_installation_types.static` and
/// `industry_assembly_lines.static` (with formula-group fallback for synthetic unit-test blueprints).
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
    let req_tid_opt = match sde.types.get(&bp_type_id).map(|t| t.group_id) {
        Some(1888) => Some(45537u32),
        Some(1889) => Some(45538u32),
        Some(1890) => Some(45539u32),
        _ => None,
    };
    if let Some(req_tid) = req_tid_opt {
        if snap.items.values().any(|it| {
            it.location_id == facility_id
                && (164..=171).contains(&(it.flag as u16))
                && it.type_id == req_tid
                && snap.module_online_states.get(&it.item_id).copied().unwrap_or(true)
                && it.dynamic_attributes.get(&2).copied().unwrap_or(1.0) > 0.0
        }) {
            return true;
        }
    }
    if !sde.industry_installation_types.is_empty() {
        if let Some(prod_type) = sde
            .blueprints
            .get(&bp_type_id)
            .and_then(|bp| {
                bp.activities
                    .get("reaction")
                    .or_else(|| bp.activities.get("9"))
                    .and_then(|a| a.products.first())
                    .map(|p| p.type_id)
            })
            .and_then(|ptid| sde.types.get(&ptid))
        {
            return snap.items.values().any(|it| {
                it.location_id == facility_id
                    && (164..=171).contains(&(it.flag as u16))
                    && snap.module_online_states.get(&it.item_id).copied().unwrap_or(true)
                    && it.dynamic_attributes.get(&2).copied().unwrap_or(1.0) > 0.0
                    && sde
                        .installation_assembly_lines_for_activity(it.type_id, 9)
                        .iter()
                        .any(|line| line.accepts_product(prod_type.category_id, prod_type.group_id))
            });
        }
    }
    req_tid_opt.is_none()
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
                meta.ready_before_offline = true;
                super::save_job_cost_meta(snap, jid, &meta);
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
            .is_some_and(|fac| fac.type_id == 78_334 || fac.solar_system_id == 30_100_000);
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

#[cfg(test)]
#[path = "industry_facilities_bps_helpers_cov_unit_tests.rs"]
mod industry_facilities_bps_helpers_cov_unit_tests;

#[cfg(test)]
mod w3_mutants_tests {
    use super::*;
    use eve_db::{IndustryJobRecord, UpwellStructureRecord, WorldRepository};
    use eve_sde::{InvType, SdeRegistry};
    use std::sync::Arc;

    #[test]
    fn fulcrum_pirate_ship_bonus_disjoint_facility_and_group_exclusions() {
        let mut sde = SdeRegistry::with_canonical_seed();
        let base_inv = sde.types.values().next().cloned().expect("seed type");
        let bp_tmpl = sde.blueprints.values().next().cloned().expect("seed bp");

        let valid_ship_tid = 998_500_u32;
        let valid_bp_tid = 998_501_u32;
        sde.types.insert(
            valid_ship_tid,
            InvType {
                type_id: valid_ship_tid,
                name: "Pirate Cruiser".into(),
                group_id: 26,
                category_id: 6,
                faction_id: Some(500_010),
                ..base_inv.clone()
            },
        );
        let mut valid_bp = bp_tmpl.clone();
        valid_bp.blueprint_type_id = valid_bp_tid;
        valid_bp.product_type_id = valid_ship_tid;
        sde.blueprints.insert(valid_bp_tid, valid_bp);

        let excluded_groups = [30_u32, 485, 513, 547, 659, 883, 902, 1538, 4594, 5120];
        for (idx, grp) in excluded_groups.iter().enumerate() {
            let stid = 998_600 + (idx as u32) * 2;
            let btid = stid + 1;
            sde.types.insert(
                stid,
                InvType {
                    type_id: stid,
                    name: format!("Excl {grp}"),
                    group_id: *grp,
                    category_id: 6,
                    faction_id: Some(500_011),
                    ..base_inv.clone()
                },
            );
            let mut bp = bp_tmpl.clone();
            bp.blueprint_type_id = btid;
            bp.product_type_id = stid;
            sde.blueprints.insert(btid, bp);
        }

        let ctx = ServerContext::with_sde_and_repository(
            Arc::new(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        );
        let fac_by_type = 1_000_899_001_i64;
        let fac_by_sys = 1_000_899_002_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&90_000_001).unwrap().war_faction_id = Some(500_010);
            // ftype == 78_334 in non-Zarzakh system (kills `|| -> &&`)
            snap.structures.insert(
                fac_by_type,
                UpwellStructureRecord {
                    structure_id: fac_by_type,
                    name: "Fulcrum Type Only".into(),
                    type_id: 78_334,
                    owner_corp_id: 98_000_001,
                    solar_system_id: 30_000_142,
                    position: [0.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    hull_hp: 1.0,
                    armor_hp: 1.0,
                    shield_hp: 1.0,
                    reinforced_until_unix: 0,
                    fuel_blocks: 10,
                    quantum_core_installed: true,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
            // ftype != 78_334 in Zarzakh 30_100_000 (kills `|| -> &&`)
            snap.structures.insert(
                fac_by_sys,
                UpwellStructureRecord {
                    structure_id: fac_by_sys,
                    name: "Zarzakh Sotiyo".into(),
                    type_id: 35_827,
                    owner_corp_id: 98_000_001,
                    solar_system_id: 30_100_000,
                    position: [0.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    hull_hp: 1.0,
                    armor_hp: 1.0,
                    shield_hp: 1.0,
                    reinforced_until_unix: 0,
                    fuel_blocks: 10,
                    quantum_core_installed: true,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );

            assert!(is_fulcrum_pirate_ship_bonus_applicable(
                &ctx,
                snap,
                Some(90_000_001),
                valid_bp_tid,
                1,
                fac_by_type
            ));
            assert!(is_fulcrum_pirate_ship_bonus_applicable(
                &ctx,
                snap,
                Some(90_000_001),
                valid_bp_tid,
                1,
                fac_by_sys
            ));
            assert!(!is_fulcrum_pirate_ship_bonus_applicable(
                &ctx,
                snap,
                Some(90_000_001),
                valid_bp_tid,
                8,
                fac_by_type
            ));

            for idx in 0..excluded_groups.len() {
                let btid = 998_601 + (idx as u32) * 2;
                assert!(!is_fulcrum_pirate_ship_bonus_applicable(
                    &ctx,
                    snap,
                    Some(90_000_001),
                    btid,
                    1,
                    60_015_187
                ));
            }

            // Also verify pause_ts clamps to start_unix when low_power_since_unix < start_unix
            let jid = 88_900_001_i64;
            snap.structures.get_mut(&fac_by_type).unwrap().fuel_blocks = 0;
            let mut ext =
                crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
            ext.lifecycle.insert(
                fac_by_type,
                crate::services::station_structure::structure_upwell_ops::StructureLifecycleState {
                    low_power_since_unix: Some(500),
                    ..Default::default()
                },
            );
            crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
            snap.industry_jobs.insert(
                jid,
                IndustryJobRecord {
                    job_id: jid,
                    installer_id: 90_000_001,
                    facility_id: fac_by_type,
                    blueprint_item_id: 1,
                    blueprint_type_id: valid_bp_tid,
                    activity_id: 1,
                    runs: 1,
                    product_type_id: valid_ship_tid,
                    status: "Active".into(),
                    duration_seconds: 1_000,
                    start_unix: 1_200,
                    ..Default::default()
                },
            );
            sync_industry_job_pause_states_at(snap, 1_500, false);
            assert_eq!(
                crate::services::industry_pi::load_job_cost_meta(snap, jid)
                    .unwrap()
                    .paused_at_unix,
                Some(1_200)
            );
        });
    }
}

