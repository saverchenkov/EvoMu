//! `sov_warzone_dynamic` — Dynamic Resource / ESS Cache, Sovereignty Hub Ops, and FW Warzone Query Handlers.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::with_combat_runtime;
use crate::services::fw_services::{
    compute_all_frontline_statuses, STATUS_REARGUARD,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

fn build_warzone_occupation_state(
    _ctx: &ServerContext,
    sys_id: u32,
    contested: f64,
    owner_fac: i64,
    occupier_fac: i64,
    frontline: i64,
) -> PyRep {
    PyRep::keyval([
        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
        ("ownerFactionID", PyRep::Integer(owner_fac)),
        ("occupierFactionID", PyRep::Integer(occupier_fac)),
        ("frontlineStatus", PyRep::Integer(frontline)),
        ("contestedPercentage", PyRep::Real(contested)),
    ])
}

pub(crate) fn collect_all_warzone_systems(ctx: &ServerContext) -> Vec<(u32, f64, i64, i64, i64)> {
    ctx.db.with_snapshot(|snap| {
        let frontlines = compute_all_frontline_statuses(ctx, snap);
        let mut list = Vec::new();
        for (&sys_id, occ) in &snap.fw_system_occupations {
            let contested = snap
                .system_states
                .get(&sys_id)
                .map(|st| st.fw_contested_pct)
                .unwrap_or_else(|| {
                    let threshold = (occ.victory_points_threshold as f64).max(1.0);
                    let vp = snap
                        .fw_runtime
                        .system_victory_points
                        .get(&sys_id)
                        .copied()
                        .unwrap_or(occ.victory_points);
                    ((vp as f64 / threshold) * 100.0).clamp(0.0, 100.0)
                });
            let frontline = frontlines.get(&sys_id).copied().unwrap_or(STATUS_REARGUARD);
            list.push((sys_id, contested, occ.owner_faction_id, occ.occupier_faction_id, frontline));
        }
        list
    })
}

fn dynamic_resource_settings() -> PyRep {
    PyRep::Dict(vec![
        (PyRep::String("bountyRiskModifierMax".into()), PyRep::Real(2.0)),
        (PyRep::String("bountyRiskModifierMin".into()), PyRep::Real(0.5)),
        (PyRep::String("bountyRiskModifierEquilibrium".into()), PyRep::Real(1.15)),
        (PyRep::String("mainBankShare".into()), PyRep::Real(0.05)),
        (PyRep::String("reserveBankShare".into()), PyRep::Real(0.05)),
        (PyRep::String("linkDurationSeconds".into()), PyRep::Integer(300)),
        (PyRep::String("linkDistanceMeters".into()), PyRep::Real(2500.0)),
        (PyRep::String("autoPayoutTimeSeconds".into()), PyRep::Integer(10800)),
        (PyRep::String("reserveBankUnlockDurationSeconds".into()), PyRep::Integer(2700)),
        (PyRep::String("reserveBankKeyGroupIDs".into()), PyRep::List(vec![PyRep::Integer(4102)])),
        (PyRep::String("autopaymentIntervalSeconds".into()), PyRep::Integer(10800)),
        (PyRep::String("bountiesCut".into()), PyRep::Real(0.10)),
        (PyRep::String("bufferTimeSeconds".into()), PyRep::Integer(300)),
        (PyRep::String("equilibriumValue".into()), PyRep::Real(1.15)),
        (PyRep::String("mainBankPayRatio".into()), PyRep::Real(0.50)),
        (PyRep::String("maxOutput".into()), PyRep::Real(2.0)),
        (PyRep::String("minOutput".into()), PyRep::Real(0.5)),
        (PyRep::String("reserveBankMaxPayout".into()), PyRep::Real(100_000_000.0)),
        (PyRep::String("reserveBankTimeAccessFactor".into()), PyRep::Real(1.0)),
        (PyRep::String("timeIntervalSeconds".into()), PyRep::Integer(60)),
    ])
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("dynamicResourceCacheMgr", "GetDBSMapData") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let map = ctx.db.with_snapshot(|snap| {
                let mut entries = Vec::new();
                for (&sys_id, st) in &snap.system_states {
                    let sec = ctx
                        .sde
                        .solar_systems
                        .get(&sys_id)
                        .map(|s| s.security_status)
                        .unwrap_or(1.0);
                    if sec <= 0.0 && sys_id < 31_000_000 {
                        entries.push((
                            PyRep::Integer(i64::from(sys_id)),
                            PyRep::Real(st.dbs_multiplier),
                        ));
                    }
                }
                if entries.is_empty() {
                    if let Some(sys_id) = session.solar_system_id {
                        let sec = ctx
                            .sde
                            .solar_systems
                            .get(&sys_id)
                            .map(|s| s.security_status)
                            .unwrap_or(1.0);
                        if sec <= 0.0 && sys_id < 31_000_000 {
                            entries.push((PyRep::Integer(i64::from(sys_id)), PyRep::Real(1.35)));
                        }
                    }
                }
                entries
            });
            Some(Ok(PyRep::Dict(map)))
        }

        ("dynamicResourceCacheMgr", "GetDynamicResourceSettings") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(dynamic_resource_settings()))
        }

        ("dynamicResourceCacheMgr", "GetESSAgencyData") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now_u = ctx.now_unix();
            let unlocked_map =
                with_combat_runtime(ctx, |rt| rt.ess_reserve_unlocked_until.clone());
            let entries = ctx.db.with_snapshot(|snap| {
                let mut dict = Vec::new();
                for (&sys_id, st) in &snap.system_states {
                    let sec = ctx
                        .sde
                        .solar_systems
                        .get(&sys_id)
                        .map(|s| s.security_status)
                        .unwrap_or(1.0);
                    if sec <= 0.0 && sys_id < 31_000_000 {
                        let pulse_st = crate::services::space_destiny::get_ess_reserve_pulses(snap, sys_id);
                        let unlocked_until = pulse_st.unlocked_until_unix.max(
                            unlocked_map.get(&sys_id).copied().unwrap_or(0),
                        );
                        let is_unlocked = unlocked_until > now_u;
                        let main_val = st.ess_main_bank.to_wire_f64();
                        let res_val = st.ess_reserve_bank.to_wire_f64();
                        dict.push((
                            PyRep::Integer(i64::from(sys_id)),
                            PyRep::Dict(vec![
                                (PyRep::String("solarSystemID".into()), PyRep::Integer(i64::from(sys_id))),
                                (PyRep::String("vaultOpen".into()), PyRep::Bool(is_unlocked)),
                                (PyRep::String("mainValue".into()), PyRep::Real(main_val)),
                                (PyRep::String("reserveValue".into()), PyRep::Real(res_val)),
                                (PyRep::String("currentOutput".into()), PyRep::Real(st.dbs_multiplier)),
                                (PyRep::String("mainBankValue".into()), PyRep::Real(main_val)),
                                (PyRep::String("reserveBankValue".into()), PyRep::Real(res_val)),
                                (PyRep::String("dbsMultiplier".into()), PyRep::Real(st.dbs_multiplier)),
                                (
                                    PyRep::String("linkedCharacterID".into()),
                                    st.ess_linked_character_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                                ),
                                (PyRep::String("isLinked".into()), PyRep::Bool(st.ess_linked_character_id.is_some())),
                                (PyRep::String("unlockedUntil".into()), PyRep::Integer(unlocked_until as i64)),
                                (PyRep::String("isUnlocked".into()), PyRep::Bool(is_unlocked)),
                            ]),
                        ));
                    }
                }
                dict
            });
            Some(Ok(PyRep::Dict(entries)))
        }

        ("dynamicResourceCacheMgr", "GetESSReserveBankKeys") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut keys = Vec::new();
            for tid in 57714..=57721 {
                let name = ctx
                    .sde
                    .types
                    .get(&tid)
                    .map(|t| t.name.as_str())
                    .unwrap_or("ESS Reserve Security Key");
                keys.push(PyRep::keyval([
                    ("typeID", PyRep::Integer(i64::from(tid))),
                    ("typeName", PyRep::String(name.into())),
                    ("groupID", PyRep::Integer(4102)),
                    ("unlockDurationSeconds", PyRep::Integer(2700)),
                    ("pulseCount", PyRep::Integer(45)),
                ]));
            }
            Some(Ok(PyRep::List(keys)))
        }

        ("dynamicResourceCacheMgr", "GetESSSystemDetails") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .and_then(|v| u32::try_from(v).ok())
                .or(session.solar_system_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "GetESSSystemDetails requires solarSystemID".into(),
                }));
            };
            let now_u = ctx.now_unix();
            let rt_unlocked_until = with_combat_runtime(ctx, |rt| {
                rt.ess_reserve_unlocked_until
                    .get(&sys_id)
                    .copied()
                    .unwrap_or(0)
            });
            let rep = ctx.db.with_snapshot(|snap| {
                let st = snap.system_states.get(&sys_id);
                let pulse_st = crate::services::space_destiny::get_ess_reserve_pulses(snap, sys_id);
                let unlocked_until = pulse_st.unlocked_until_unix.max(rt_unlocked_until);
                let is_unlocked = unlocked_until > now_u;
                let rem_secs = if is_unlocked { (unlocked_until - now_u) as i64 } else { 0 };
                let total_pulses = if is_unlocked { (unlocked_until - now_u).div_ceil(60) as i64 } else { 0 };
                let active_links = if st.and_then(|s| s.ess_linked_character_id).is_some() { 1 } else { 0 };
                PyRep::Dict(vec![
                    (PyRep::String("solarSystemID".into()), PyRep::Integer(i64::from(sys_id))),
                    (PyRep::String("activeLinks".into()), PyRep::Integer(active_links)),
                    (PyRep::String("autopaymentWindowState".into()), PyRep::Integer(1)),
                    (PyRep::String("reserveTimeChosen".into()), PyRep::Integer(rem_secs)),
                    (PyRep::String("reserveTimeWindowState".into()), PyRep::Integer(if is_unlocked { 2 } else { 0 })),
                    (PyRep::String("totalPulses".into()), PyRep::Integer(total_pulses)),
                    (PyRep::String("mainBankValue".into()), PyRep::Real(st.map(|s| s.ess_main_bank.to_wire_f64()).unwrap_or(0.0))),
                    (PyRep::String("reserveBankValue".into()), PyRep::Real(st.map(|s| s.ess_reserve_bank.to_wire_f64()).unwrap_or(0.0))),
                    (PyRep::String("dbsMultiplier".into()), PyRep::Real(st.map(|s| s.dbs_multiplier).unwrap_or(1.0))),
                    (
                        PyRep::String("linkedCharacterID".into()),
                        st.and_then(|s| s.ess_linked_character_id).map(PyRep::Integer).unwrap_or(PyRep::None),
                    ),
                    (
                        PyRep::String("linkStartedTick".into()),
                        st.and_then(|s| s.ess_link_started_tick).map(|t| PyRep::Integer(t as i64)).unwrap_or(PyRep::None),
                    ),
                    (PyRep::String("unlockedUntil".into()), PyRep::Integer(unlocked_until as i64)),
                    (PyRep::String("isUnlocked".into()), PyRep::Bool(is_unlocked)),
                ])
            });
            Some(Ok(rep))
        }

        ("dynamicResourceCacheMgr", "get") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let default_val = args.get(1).cloned().unwrap_or(PyRep::None);
            let Some(first_arg) = args.first() else {
                return Some(Ok(default_val));
            };
            if let Some(sys_id) = first_arg.as_i64().and_then(|v| u32::try_from(v).ok()) {
                let mult = ctx.db.with_snapshot(|snap| {
                    snap.system_states
                        .get(&sys_id)
                        .map(|s| PyRep::Real(s.dbs_multiplier))
                });
                Some(Ok(mult.unwrap_or(default_val)))
            } else if let Some(key_str) = first_arg.as_str() {
                if key_str == "settings" {
                    Some(Ok(dynamic_resource_settings()))
                } else {
                    Some(Ok(default_val))
                }
            } else {
                Some(Ok(default_val))
            }
        }

        ("dynamicResourceCacheMgr", "iteritems") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let items = ctx.db.with_snapshot(|snap| {
                snap.system_states
                    .iter()
                    .map(|(&sys_id, st)| {
                        PyRep::Tuple(vec![
                            PyRep::Integer(i64::from(sys_id)),
                            PyRep::Real(st.dbs_multiplier),
                        ])
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(items)))
        }

        ("sovMgr", "remove_from_cache") => {
            // const-ok: cache invalidation notification returns None
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // const-ok: client-side memoized cache invalidation notification returns None
            Some(Ok(PyRep::None))
        }

        ("fwWarzoneSolarsystem", "iteritems") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let systems = collect_all_warzone_systems(ctx);
            let items: Vec<PyRep> = systems
                .into_iter()
                .map(|(sys_id, contested, owner_fac, occupier_fac, frontline)| {
                    let st = build_warzone_occupation_state(
                        ctx,
                        sys_id,
                        contested,
                        owner_fac,
                        occupier_fac,
                        frontline,
                    );
                    PyRep::Tuple(vec![PyRep::Integer(i64::from(sys_id)), st])
                })
                .collect();
            Some(Ok(PyRep::List(items)))
        }

        ("fwWarzoneSolarsystem", "itervalues") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let systems = collect_all_warzone_systems(ctx);
            let values: Vec<PyRep> = systems
                .into_iter()
                .map(|(sys_id, contested, owner_fac, occupier_fac, frontline)| {
                    build_warzone_occupation_state(
                        ctx,
                        sys_id,
                        contested,
                        owner_fac,
                        occupier_fac,
                        frontline,
                    )
                })
                .collect();
            Some(Ok(PyRep::List(values)))
        }

        ("fwWarzoneSolarsystem", "keys") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let systems = collect_all_warzone_systems(ctx);
            let keys: Vec<PyRep> = systems
                .into_iter()
                .map(|(sys_id, _, _, _, _)| PyRep::Integer(i64::from(sys_id)))
                .collect();
            Some(Ok(PyRep::List(keys)))
        }

        _ => None,
    }
}

pub(crate) fn military_index_for_kills(kills: u64) -> u8 {
    match kills {
        0..=4 => 0,
        5..=14 => 1,
        15..=34 => 2,
        35..=69 => 3,
        70..=119 => 4,
        _ => 5,
    }
}

pub(crate) fn industrial_index_for_mined(mined: u64) -> u8 {
    match mined {
        0..=999 => 0,
        1_000..=4_999 => 1,
        5_000..=14_999 => 2,
        15_000..=34_999 => 3,
        35_000..=74_999 => 4,
        _ => 5,
    }
}

/// Decays Sovereignty Military and Industrial activity counters over inactive days (`feat:sovereignty.sov-dev-indices`).
pub(crate) fn decay_sov_dev_indices_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    now_unix: u64,
) -> Vec<u32> {
    let mut changed = Vec::new();
    let sys_ids: Vec<u32> = snap
        .system_states
        .iter()
        .filter(|(_, st)| st.sov_alliance_id.is_some())
        .map(|(&id, _)| id)
        .collect();
    for sys_id in sys_ids {
        let last = snap
            .sov_runtime
            .sov_dev_decay_last_unix
            .entry(sys_id)
            .or_insert(now_unix);
        let elapsed = now_unix.saturating_sub(*last);
        if elapsed < 86_400 {
            continue;
        }
        let days = (elapsed / 86_400) as i32;
        *last += (days as u64) * 86_400;
        let factor = 0.75_f64.powi(days);
        let kills = snap.sov_runtime.sov_npc_kills.entry(sys_id).or_insert(0);
        *kills = ((*kills as f64) * factor).floor() as u64;
        let k_val = *kills;
        let mined = snap.sov_runtime.sov_mined_units.entry(sys_id).or_insert(0);
        *mined = ((*mined as f64) * factor).floor() as u64;
        let m_val = *mined;
        let mil = military_index_for_kills(k_val);
        let ind = industrial_index_for_mined(m_val);
        if let Some(st) = snap.system_states.get_mut(&sys_id) {
            if st.military_index != mil || st.industrial_index != ind {
                st.military_index = mil;
                st.industrial_index = ind;
                changed.push(sys_id);
            }
        }
    }
    changed
}

/// Regenerates unattended Sovereignty Command Nodes in the defender's favour up to 98 minutes (`5_880s`).
pub(crate) async fn regenerate_unattended_command_nodes(ctx: &ServerContext, now_unix: u64) {
    let db_key = crate::services::tactical_modules::db_ptr_key(&ctx.db);
    let active_targets: std::collections::HashSet<i64> = {
        let tst = ctx.tactical_state.read().unwrap_or_else(|e| e.into_inner());
        tst.entosis_active_links
            .iter()
            .filter(|(&(k, _), _)| k == db_key)
            .map(|(_, l)| l.target_id)
            .collect()
    };

    let mut defender_won_hubs: Vec<(i64, u32, Vec<(i64, u32)>)> = Vec::new();
    ctx.db.with_mut_snapshot(|snap| {
        let hub_ids: Vec<i64> = snap.sov_campaign_scores.keys().copied().collect();
        for hub_id in hub_ids {
            let nodes: Vec<(i64, u32)> = snap
                .sov_runtime
                .command_nodes
                .iter()
                .filter(|(_, &(hid, _, _))| hid == hub_id)
                .map(|(&nid, &(_, nsys, _))| (nid, nsys))
                .collect();
            if nodes.is_empty() {
                continue;
            }
            let any_active = nodes.iter().any(|(nid, _)| {
                active_targets.contains(nid)
                    || snap.sov_runtime.entosis_capture_progress.contains_key(nid)
            });
            let last_ts = snap
                .sov_runtime
                .command_node_regen_last_unix
                .entry(hub_id)
                .or_insert(now_unix);
            if any_active {
                *last_ts = now_unix;
                continue;
            }
            let dt = now_unix.saturating_sub(*last_ts) as f64;
            if dt <= 0.0 {
                continue;
            }
            *last_ts = now_unix;
            let scores = snap.sov_runtime.command_node_scores.entry(hub_id).or_default();
            if scores.is_empty() {
                continue;
            }
            let mut progress = (scores[0].1 + dt / 5_880.0).min(1.0);
            if progress >= 1.0 - 1e-6 {
                progress = 1.0;
            }
            scores[0].1 = progress;
            let def_rounded = (progress.clamp(0.0, 1.0) * 100.0).round() / 100.0;
            let att_rounded = ((1.0 - def_rounded).clamp(0.0, 1.0) * 100.0).round() / 100.0;
            snap.sov_campaign_scores.insert(
                hub_id,
                eve_db::SovCampaignScoreRecord {
                    structure_id: hub_id,
                    attackers_score: att_rounded,
                    defender_score: def_rounded,
                },
            );
            if progress >= 1.0 - 1e-6 {
                for (nid, _) in &nodes {
                    snap.sov_runtime.command_nodes.remove(nid);
                    snap.items.remove(nid);
                    snap.item_positions.remove(nid);
                }
                snap.sov_runtime.command_node_scores.remove(&hub_id);
                snap.sov_runtime.command_node_regen_last_unix.remove(&hub_id);
                snap.sov_campaign_scores.remove(&hub_id);
                let mut hub_sys = 0u32;
                if let Some(st) = snap.structures.get_mut(&hub_id) {
                    st.state = "ShieldVulnerable".into();
                    st.reinforced_until_unix = 0;
                    hub_sys = st.solar_system_id;
                }
                if hub_sys > 0 {
                    defender_won_hubs.push((hub_id, hub_sys, nodes));
                }
            }
        }
    });

    for (hub_id, hub_sys, nodes) in defender_won_hubs {
        for (nid, nsys) in nodes {
            let _ = ctx.systems.get_or_spawn(nsys).remove_ball(nid).await;
        }
        crate::services::sov_services::emit_sov_structures_updated(ctx, hub_sys, &[hub_id]);
    }
}

/// Returns per-tier and per-security-band site specs for Sovereignty Hub upgrades (`feat:sovereignty.sov-upgrade-sites`).
pub(crate) fn sov_upgrade_site_specs(
    type_name: &str,
    security_status: f64,
) -> Vec<(&'static str, String, [f64; 3], Vec<u32>, usize)> {
    let tier: usize = if type_name.contains("III") || type_name.contains("3") {
        3
    } else if type_name.contains("II") || type_name.contains("2") {
        2
    } else {
        1
    };
    if type_name.contains("Threat Detection") {
        let is_major = type_name.contains("Major");
        let count = if is_major { tier + 2 } else { tier };
        let names: [&str; 4] = if security_status <= -0.45 {
            ["Forsaken Hub", "Hidden Hub", "Haven", "Sanctum"]
        } else {
            ["Forsaken Refuge", "Drone Den", "Forsaken Hub", "Hidden Hub"]
        };
        let npc_count = 2 + tier;
        (0..count)
            .map(|i| {
                let pos = [165_000.0 + (i as f64) * 25_000.0, 25_000.0 + (i as f64) * 10_000.0, 0.0];
                ("Combat", names[i % names.len()].to_string(), pos, Vec::new(), npc_count)
            })
            .collect()
    } else if type_name.contains("Prospecting Array") {
        let ores = if type_name.contains("Tritanium") {
            vec![1230, 18]
        } else if type_name.contains("Pyerite") {
            vec![1228, 1224]
        } else if type_name.contains("Mexallon") {
            vec![18, 20]
        } else if type_name.contains("Isogen") {
            vec![1227, 1229]
        } else if type_name.contains("Nocxium") {
            vec![1232, 1225]
        } else if type_name.contains("Zydrine") {
            vec![1223, 22]
        } else if type_name.contains("Megacyte") {
            vec![19, 1223]
        } else {
            vec![1228, 1229]
        };
        let ast_count = 3 + tier;
        vec![(
            "Ore",
            format!("Sovereign {}", type_name),
            [160_000.0, 25_000.0, 0.0],
            ores,
            ast_count,
        )]
    } else if type_name.contains("Exploration Detector") {
        let count = 1 + tier;
        (0..count)
            .map(|i| {
                let arch = if i % 2 == 0 { "Relic" } else { "Data" };
                let sname = if arch == "Relic" {
                    format!("Ruined Sovereign Monument Site {}", i + 1)
                } else {
                    format!("Central Sovereign Data Mining Site {}", i + 1)
                };
                let pos = [170_000.0 + (i as f64) * 20_000.0, 25_000.0, (i as f64) * 5_000.0];
                (arch, sname, pos, Vec::new(), 1)
            })
            .collect()
    } else {
        Vec::new()
    }
}

pub(crate) fn constellation_systems_for_hub(ctx: &ServerContext, hub_sys_id: u32) -> Vec<u32> {
    if let Some(cs) = ctx.sde.solar_systems.get(&hub_sys_id) {
        let mut v: Vec<u32> = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| s.constellation_id == cs.constellation_id)
            .map(|s| s.solar_system_id)
            .collect();
        v.sort_unstable();
        if v.is_empty() { vec![hub_sys_id] } else { v }
    } else {
        vec![hub_sys_id]
    }
}

pub(crate) async fn spawn_command_node_balls(ctx: &ServerContext, spawned: Vec<(i64, u32, [f64; 3])>) {
    for (node_id, n_sys, pos) in spawned {
        let _ = ctx
            .systems
            .get_or_spawn(n_sys)
            .spawn_ball(eve_destiny::Ball {
                id: node_id,
                type_id: crate::services::sov_gameplay::TYPE_STRUCTURE_COMMAND_NODE,
                position: glam::DVec3::from_array(pos),
                velocity: glam::DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 5.0e7,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Rigid,
            })
            .await;
    }
}

pub(crate) async fn spawn_command_nodes_for_hub(ctx: &ServerContext, hub_id: i64, hub_sys_id: u32) {
    let const_systems = constellation_systems_for_hub(ctx, hub_sys_id);
    let now_unix = ctx.now_unix();
    let spawned: Vec<(i64, u32, [f64; 3])> = ctx.db.with_mut_snapshot(|snap| {
        let mut out = Vec::new();
        for idx in 0..4usize {
            let n_sys = const_systems[idx % const_systems.len()];
            snap.next_entity_id += 1;
            let node_id = snap.next_entity_id;
            let pos = crate::services::space_destiny::space_docking_cloaking::pick_system_celestial_position(ctx, n_sys, (idx as u32) + 10).0;
            snap.items.insert(
                node_id,
                eve_db::InventoryItemRecord {
                    item_id: node_id,
                    type_id: crate::services::sov_gameplay::TYPE_STRUCTURE_COMMAND_NODE,
                    owner_id: 1,
                    location_id: i64::from(n_sys),
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(node_id, pos);
            snap.sov_runtime.command_nodes.insert(node_id, (hub_id, n_sys, 0));
            out.push((node_id, n_sys, pos));
        }
        let def_key = snap
            .system_states
            .get(&hub_sys_id)
            .and_then(|s| s.sov_alliance_id)
            .or_else(|| snap.structures.get(&hub_id).map(|s| -s.owner_corp_id.max(1)))
            .unwrap_or(0);
        snap.sov_runtime
            .command_node_scores
            .insert(hub_id, vec![(def_key, crate::services::sov_gameplay::SOV_COMMAND_NODE_INITIAL_PROGRESS)]);
        snap.sov_runtime
            .command_node_regen_last_unix
            .insert(hub_id, now_unix);
        snap.sov_campaign_scores.insert(
            hub_id,
            eve_db::SovCampaignScoreRecord {
                structure_id: hub_id,
                attackers_score: 1.0 - crate::services::sov_gameplay::SOV_COMMAND_NODE_INITIAL_PROGRESS,
                defender_score: crate::services::sov_gameplay::SOV_COMMAND_NODE_INITIAL_PROGRESS,
            },
        );
        out
    });
    spawn_command_node_balls(ctx, spawned).await;
}
