//! Integration tests for Pirate Insurgency Corruption (Stages 1–5) system effects and
//! effect removal upon stage drop (`feat:fw.corruption-suppression-effects`).

#![forbid(unsafe_code)]

use crate::sovereignty_equinox_gameplay::{
    make_ball, place_ship_ball, register_online_pilot, rpc_call, set_pilot_location,
};
use eve_db::snapshot_ext::FwSystemOccupationRecord;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime};
use eve_server::services::insurgency_services::{
    ACTIVE_DURATION_SECS, FORECASTING_DURATION_SECS, WINDDOWN_DURATION_SECS,
};
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub(crate) fn in_memory_ctx() -> ServerContext {
    static FAST_SDE: std::sync::OnceLock<Arc<eve_sde::SdeRegistry>> = std::sync::OnceLock::new();
    let sde = Arc::clone(FAST_SDE.get_or_init(|| Arc::new(eve_sde::SdeRegistry::with_canonical_seed())));
    let repo = eve_db::WorldRepository::with_demo_seed();
    repo.with_mut_snapshot(|snap| {
        snap.markets_seeded = true;
    });
    let ctx = ServerContext::with_sde_and_repository(sde, Arc::new(repo));
    crate::common::pin_clock(&ctx);
    ctx
}

pub(crate) fn setup_warzone_chain_with_hs_station(ctx: &ServerContext) -> (u32, u32, u32, u32, u32, i64) {
    let mut lowsec: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status > 0.0 && s.security_status < 0.45 && s.solar_system_id < 30_100_000)
        .map(|s| s.solar_system_id)
        .collect();
    lowsec.sort_unstable();
    let lowsec_set: HashSet<u32> = lowsec.iter().copied().collect();
    let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
    for j in ctx.sde.jumps.values() {
        if lowsec_set.contains(&j.from_solar_system_id) && lowsec_set.contains(&j.to_solar_system_id) {
            adj.entry(j.from_solar_system_id).or_default().push(j.to_solar_system_id);
        }
    }
    let neighbors = |sys: u32| -> &[u32] { adj.get(&sys).map_or(&[], Vec::as_slice) };
    let first_hs_with_station = |sys: u32| -> Option<(u32, i64)> {
        let mut hs_cands: Vec<u32> = ctx
            .sde
            .jumps
            .values()
            .filter(|j| j.from_solar_system_id == sys)
            .map(|j| j.to_solar_system_id)
            .filter(|&hs| {
                ctx.sde.solar_systems.get(&hs).is_some_and(|s| {
                    s.solar_system_id < 30_100_000
                        && matches!(s.wormhole_class_id, None | Some(7..=9))
                        && s.security_status >= 0.5
                        && s.security_status < 0.85
                        && ctx.sde.regions.get(&s.region_id).is_some_and(|r| {
                            matches!(r.faction_id, Some(500_001..=500_004 | 500_007 | 500_008))
                        })
                })
            })
            .collect();
        hs_cands.sort_unstable();
        hs_cands.dedup();
        let hs = *hs_cands.first()?;
        let mut hs_stations: Vec<i64> = ctx
            .sde
            .stations
            .values()
            .filter(|st| st.solar_system_id == hs)
            .map(|st| st.station_id)
            .collect();
        hs_stations.sort_unstable();
        let st_id = *hs_stations.first()?;
        let has_ls_gate = ctx.sde.jumps.values().any(|j| j.from_solar_system_id == sys && j.position.length() > 5_000_000.0);
        has_ls_gate.then_some((hs, st_id))
    };

    let mut chosen = None;
    'outer: for &front in &lowsec {
        let Some((hs_sys, hs_station_id)) = first_hs_with_station(front) else { continue; };
        let fnbs = neighbors(front);
        if fnbs.len() < 3 {
            continue;
        }
        for &enemy in fnbs {
            for &cmd in fnbs {
                if cmd == enemy || neighbors(cmd).contains(&enemy) { continue; }
                for &rear in neighbors(cmd) {
                    if rear != front && rear != enemy && !neighbors(rear).contains(&enemy) && !neighbors(rear).contains(&front) {
                        chosen = Some((enemy, front, cmd, rear, hs_sys, hs_station_id));
                        break 'outer;
                    }
                }
            }
        }
    }
    let (enemy, front, cmd, rear, hs_sys, hs_station_id) = chosen.expect("warzone chain with HS station");
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.clear();
        let mut caldari_set = vec![front, cmd, rear];
        for base in [front, cmd, rear] {
            for &nb in neighbors(base) {
                if nb != enemy && !caldari_set.contains(&nb) {
                    caldari_set.push(nb);
                }
            }
        }
        snap.fw_system_occupations.insert(enemy, FwSystemOccupationRecord {
            solar_system_id: enemy, owner_faction_id: 500_004, occupier_faction_id: 500_004, victory_points: 0, victory_points_threshold: 3_000,
        });
        for sys in caldari_set {
            snap.fw_system_occupations.insert(sys, FwSystemOccupationRecord {
                solar_system_id: sys, owner_faction_id: 500_001, occupier_faction_id: 500_001, victory_points: 0, victory_points_threshold: 3_000,
            });
        }
    });
    (enemy, front, cmd, rear, hs_sys, hs_station_id)
}

pub(crate) fn fast_forward_secs(ctx: &ServerContext, secs: u64) {
    ctx.clock().advance_secs(secs);
    let cur = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert("__server_now_unix".into(), cur.to_string());
    });
}

pub(crate) fn lookup_modified_sec(rep: &PyRep, sys_id: u32) -> Option<f64> {
    let PyRep::Dict(entries) = rep else { panic!("expected Dict from securityMgr.get_modified_systems, got {rep:?}") };
    entries.iter().find_map(|(k, v)| match (k, v) {
        (PyRep::Integer(id), PyRep::Real(sec)) if *id == i64::from(sys_id) => Some(*sec),
        _ => None,
    })
}

pub(crate) async fn local_campaign_stages(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32) -> Option<(u8, i64, u8, i64)> {
    set_pilot_location(ctx, sess, sys_id, None);
    let PyRep::Tuple(tup) = rpc_call(ctx, sess, "insurgencySolarsystem", "GetLocalCampaignClientSnapshot", &[]).await.unwrap() else {
        panic!("expected Tuple");
    };
    assert_eq!(tup[0].as_i64(), Some(i64::from(sys_id)));
    if matches!(tup[1], PyRep::None) {
        return None;
    }
    let snap = &tup[1];
    Some((
        snap.dict_get("corruptionStage").and_then(PyRep::as_i64).unwrap() as u8,
        snap.dict_get("corruptionPoints").and_then(PyRep::as_i64).unwrap(),
        snap.dict_get("suppressionStage").and_then(PyRep::as_i64).unwrap() as u8,
        snap.dict_get("suppressionPoints").and_then(PyRep::as_i64).unwrap(),
    ))
}

pub(crate) async fn fetch_insurgency_sites(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32) -> Vec<(String, String)> {
    let PyRep::List(list) = rpc_call(ctx, sess, "dungeonInstanceCacheMgr", "GetPirateInsurgencyInstances", &[PyRep::Integer(i64::from(sys_id))]).await.unwrap() else {
        panic!("expected List");
    };
    list.into_iter()
        .filter(|e| e.dict_get("completed").and_then(PyRep::as_bool) == Some(false))
        .map(|e| (e.dict_get("siteType").and_then(PyRep::as_str).unwrap().to_string(), e.dict_get("name").and_then(PyRep::as_str).unwrap().to_string()))
        .collect()
}

pub(crate) async fn fetch_ice_heist_count(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32) -> usize {
    let PyRep::List(list) = rpc_call(ctx, sess, "dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances", &[PyRep::Integer(i64::from(sys_id))]).await.unwrap() else {
        panic!("expected List");
    };
    list.into_iter().filter(|e| e.dict_get("completed").and_then(PyRep::as_bool) == Some(false)).count()
}

pub(crate) fn fit_tactical_sabre(ctx: &ServerContext, char_id: i64, ship_id: i64, base_id: i64) -> (i64, i64, i64, i64, i64, i64) {
    let (gun_id, ammo_id, bub_id, probe_id, rep_id, web_id, scram_id, md_id) =
        (base_id + 1, base_id + 2, base_id + 3, base_id + 4, base_id + 5, base_id + 6, base_id + 7, base_id + 8);
    let extra_guns = [
        (base_id + 11, base_id + 12, InventoryFlag::HiSlot3),
        (base_id + 13, base_id + 14, InventoryFlag::HiSlot4),
        (base_id + 15, base_id + 16, InventoryFlag::HiSlot5),
        (base_id + 17, base_id + 18, InventoryFlag::HiSlot6),
    ];
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_id);
        if let Some(sh) = snap.items.get_mut(&ship_id) { sh.type_id = 22_456; }
        let sk = snap.skills.entry(char_id).or_default();
        let mut req_skills = std::collections::BTreeSet::from([
            3300u32, 3301, 3303, 3311, 3312, 3327, 3328, 3330, 3392, 3413, 3416, 3424, 3426, 3435,
            3449, 3453, 3455, 12092, 12098, 22_043,
        ]);
        for tid in [22_456u32, 486, 185, 22_782, 22_778, 11_355, 526, 447, 12_198] {
            if let Some(tdef) = ctx.sde.types.get(&tid) {
                for aid in [182u16, 183, 184, 1285, 1289, 1290] {
                    if let Some(&sid) = tdef.default_attributes.get(&aid) {
                        req_skills.insert(sid as u32);
                    }
                }
            }
        }
        for tid in req_skills {
            if ctx.sde.types.contains_key(&tid) {
                sk.insert(tid, TrainedSkillRecord { type_id: tid, level: 5, skill_points: 256_000.0 });
            }
        }
        let mut items_to_fit = vec![
            (gun_id, 486u32, InventoryFlag::HiSlot0, 1, true),
            (ammo_id, 185u32, InventoryFlag::HiSlot0, 10_000, false),
            (bub_id, 22_782u32, InventoryFlag::HiSlot1, 1, true),
            (probe_id, 22_778u32, InventoryFlag::HiSlot1, 3, false),
            (rep_id, 11_355u32, InventoryFlag::HiSlot2, 1, true),
            (web_id, 526u32, InventoryFlag::MedSlot0, 1, true),
            (scram_id, 447u32, InventoryFlag::MedSlot1, 1, true),
            (md_id, 12_198u32, InventoryFlag::Cargo, 10, false),
        ];
        let mut bank = vec![gun_id];
        for (gid, aid, flg) in extra_guns {
            items_to_fit.push((gid, 486u32, flg, 1, true));
            items_to_fit.push((aid, 185u32, flg, 10_000, false));
            bank.push(gid);
        }
        snap.weapon_banks.entry(ship_id).or_default().insert(gun_id, bank);
        for (iid, tid, flg, qty, sing) in items_to_fit {
            let mut rec = InventoryItemRecord {
                item_id: iid, type_id: tid, owner_id: char_id, location_id: ship_id,
                flag: flg, quantity: qty, singleton: sing, version: 1, ..Default::default()
            };
            if sing { rec.dynamic_attributes.insert(2, 1.0); }
            snap.items.insert(iid, rec);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    (gun_id, bub_id, rep_id, web_id, scram_id, md_id)
}

pub(crate) async fn pulse_complex_60s(
    ctx: &ServerContext,
    sys_id: u32,
    active_sess: &mut SessionState,
    active_cid: i64,
    active_ship: i64,
    parked: &[(i64, u32)],
) {
    let beacon = ctx.db.with_snapshot(|s| {
        s.system_states
            .get(&sys_id)
            .and_then(|st| st.fw_complex_position)
            .map(DVec3::from_array)
            .unwrap_or(DVec3::ZERO)
    });
    let off_grid = beacon + DVec3::new(500_000.0, 500_000.0, 0.0);
    for &(pship, psys) in parked {
        let p_off = if psys == sys_id {
            off_grid
        } else {
            ctx.db
                .with_snapshot(|s| {
                    s.system_states
                        .get(&psys)
                        .and_then(|st| st.fw_complex_position)
                        .map(DVec3::from_array)
                        .unwrap_or(DVec3::ZERO)
                })
                + DVec3::new(500_000.0, 500_000.0, 0.0)
        };
        let _ = ctx.systems.get_or_spawn(psys).set_ball_position(pship, p_off).await;
    }
    set_pilot_location(ctx, active_sess, sys_id, None);
    ctx.db.with_mut_snapshot(|s| {
        s.sp6_lifecycle.ship_combat_hp.remove(&active_ship);
        for &(pship, _) in parked { s.sp6_lifecycle.ship_combat_hp.remove(&pship); }
        if let Some(it) = s.items.get_mut(&active_ship) { it.location_id = i64::from(sys_id); }
    });
    place_ship_ball(ctx, sys_id, active_cid, active_ship, 22_456, beacon + DVec3::new(5_000.0, 0.0, 0.0)).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h.step_manual(0.1).await.unwrap();
    fast_forward_secs(ctx, 60);
    eve_server::services::insurgency_services::step_system_insurgency_tick_delta(ctx, sys_id, Some(60)).await;
    let _ = sys_h.set_ball_position(active_ship, off_grid).await;
}

async fn measure_live_pvp_loot_drop_fraction(
    ctx: &ServerContext,
    killer_sess: &mut SessionState,
    killer_cid: i64,
    killer_ship: i64,
    gun_id: i64,
    sys_id: u32,
    victim_cid: i64,
    victim_ship: i64,
    item_base_id: i64,
) -> f64 {
    const N_ITEMS: i64 = 400;
    set_pilot_location(ctx, killer_sess, sys_id, None);
    let kill_pos = DVec3::new(250_000.0, 250_000.0, 0.0);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ks) = s.items.get_mut(&killer_ship) { ks.location_id = i64::from(sys_id); }
        let vch = s.characters.get_mut(&victim_cid).unwrap();
        vch.solar_system_id = sys_id;
        vch.active_ship_id = victim_ship;
        s.items.insert(victim_ship, InventoryItemRecord {
            item_id: victim_ship, type_id: 621, owner_id: victim_cid, location_id: i64::from(sys_id),
            flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        for k in 0..N_ITEMS {
            s.items.insert(item_base_id + k, InventoryItemRecord {
                item_id: item_base_id + k, type_id: 447, owner_id: victim_cid, location_id: victim_ship,
                flag: InventoryFlag::Cargo, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
        }
    });
    place_ship_ball(ctx, sys_id, killer_cid, killer_ship, 22_456, kill_pos).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let mut vcs = ctx.build_type_combat_state(621);
    vcs.shield_hp = 0.0;
    vcs.armor_hp = 0.0;
    vcs.structure_hp = 1.0;
    let _ = sys_h.remove_ball(victim_ship).await;
    sys_h.spawn_ball_with_combat(make_ball(victim_ship, 621, kill_pos + DVec3::new(250.0, 0.0, 0.0)), vcs).await.unwrap();
    sys_h.add_target_lock(killer_ship, TargetLock { target_id: victim_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
    rpc_call(ctx, killer_sess, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    rpc_call(ctx, killer_sess, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(victim_ship)]).await.unwrap();
    let _ = rpc_call(ctx, killer_sess, "dogmaIM", "Deactivate", &[PyRep::Integer(gun_id)]).await;
    fast_forward_secs(ctx, 12);
    sys_h.step_manual(12.0).await.unwrap();
    let dropped = ctx.db.with_mut_snapshot(|s| {
        let cnt = (0..N_ITEMS).filter(|&k| s.items.contains_key(&(item_base_id + k))).count();
        for k in 0..N_ITEMS { s.items.remove(&(item_base_id + k)); }
        s.combat_crimewatch_state.detailed_killmails.clear();
        s.killmails.clear();
        cnt
    });
    (dropped as f64) / (N_ITEMS as f64)
}

async fn measure_live_warp_speed_au_s(
    ctx: &ServerContext,
    sess: &mut SessionState,
    cid: i64,
    ship_id: i64,
    sys_id: u32,
    celestial_id: i64,
) -> f64 {
    set_pilot_location(ctx, sess, sys_id, None);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(it) = s.items.get_mut(&ship_id) { it.location_id = i64::from(sys_id); }
    });
    place_ship_ball(ctx, sys_id, cid, ship_id, 22_456, DVec3::new(200_000.0, 0.0, 0.0)).await;
    rpc_call(ctx, sess, "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(celestial_id), PyRep::Real(0.0)]).await.unwrap();
    let (_, ball) = ctx.systems.get_or_spawn(sys_id).get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let speed = match ball.mode {
        MovementMode::Warp { warp_speed_au_s, .. } => warp_speed_au_s,
        other => panic!("expected MovementMode::Warp after CmdWarpToStuff, got {other:?}"),
    };
    place_ship_ball(ctx, sys_id, cid, ship_id, 22_456, DVec3::new(900_000.0, 900_000.0, 0.0)).await;
    speed
}

#[tokio::test]
async fn test_corruption_all_stages_1_to_5_and_drop_removal() {
    // parity: feat:fw.corruption-suppression-effects
    eve_testkit::attest(&["feat:fw.corruption-suppression-effects"]);

    let ctx = in_memory_ctx();
    let (_enemy_sys, front_sys, cmd_sys, _rear_sys, hs_sys, hs_station_id) =
        setup_warzone_chain_with_hs_station(&ctx);

    let (emp_cid, emp_ship, pir_cid, pir_ship, neu_cid, neu_ship, emp_vic_cid, pir_vic_cid) =
        (90_400_001, 100_400_001, 90_400_002, 100_400_002, 90_400_003, 100_400_003, 90_400_004, 90_400_005);

    let mut s_emp = register_online_pilot(&ctx, 84_001, "b40_emp", "pw", emp_cid, 98_400_001, None, front_sys, emp_ship, 22_456, DVec3::new(900_000.0, 900_000.0, 0.0), 0).await;
    let mut s_pir = register_online_pilot(&ctx, 84_002, "b40_pir", "pw", pir_cid, 98_400_002, None, front_sys, pir_ship, 22_456, DVec3::new(900_000.0, 900_000.0, 0.0), 0).await;
    let mut s_neu = register_online_pilot(&ctx, 84_003, "b40_neu", "pw", neu_cid, 98_400_003, None, hs_sys, neu_ship, 22_456, DVec3::new(900_000.0, 900_000.0, 0.0), 0).await;
    let mut s_evic = register_online_pilot(&ctx, 84_004, "b40_evic", "pw", emp_vic_cid, 98_400_004, None, front_sys, 100_400_004, 621, DVec3::new(950_000.0, 950_000.0, 0.0), 0).await;
    let mut s_pvic = register_online_pilot(&ctx, 84_005, "b40_pvic", "pw", pir_vic_cid, 98_400_005, None, cmd_sys, 100_400_005, 621, DVec3::new(950_000.0, 950_000.0, 0.0), 0).await;

    rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)]).await.unwrap();
    rpc_call(&ctx, &mut s_pir, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
    rpc_call(&ctx, &mut s_pvic, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&emp_vic_cid).unwrap().war_faction_id = Some(500_001));

    let (emp_gun, _eb, emp_rep, _ew, _es, _emd) = fit_tactical_sabre(&ctx, emp_cid, emp_ship, 60_401_000);
    let (pir_gun, pir_bub, _pr, _pw, _ps, pir_md) = fit_tactical_sabre(&ctx, pir_cid, pir_ship, 60_402_000);
    let (neu_gun, _nb, _nr, _nw, _ns, _nmd) = fit_tactical_sabre(&ctx, neu_cid, neu_ship, 60_403_000);

    let mut front_gates: Vec<(i64, DVec3)> = ctx
        .sde
        .jumps
        .values()
        .filter(|j| j.from_solar_system_id == front_sys && j.position.length() > 5_000_000.0)
        .map(|j| (j.stargate_id, j.position))
        .collect();
    front_gates.sort_by_key(|(gid, _)| *gid);
    let (front_far_cel, front_gate_pos) = front_gates[0];

    let _ = rpc_call(&ctx, &mut s_pir, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let PyRep::List(vis0) = rpc_call(&ctx, &mut s_emp, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[]).await.unwrap() else { panic!() };
    let fob_sys = vis0[0].dict_get("fobSolarSystemID").and_then(PyRep::as_i64).unwrap() as u32;

    assert_eq!(local_campaign_stages(&ctx, &mut s_emp, front_sys).await, Some((0, 0, 0, 0)));
    assert_eq!(lookup_modified_sec(&rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap(), front_sys), None);
    let sites_c0 = fetch_insurgency_sites(&ctx, &mut s_emp, front_sys).await;
    assert!(sites_c0.iter().any(|(t, n)| t == "CorporateOutpostRaid" && n.contains("Outpost Raid"))); // rpc-lint: allow siteType value, not RPC method
    assert!(!sites_c0.iter().any(|(t, _)| t == "MiningOpAmbush" || t == "IceRefineryHeist"));
    assert_eq!(fetch_ice_heist_count(&ctx, &mut s_emp, front_sys).await, 0);
    assert!(fetch_insurgency_sites(&ctx, &mut s_emp, fob_sys).await.iter().any(|(t, n)| t == "PirateLaunderingFacility" && n.contains("Laundering Facility")));
    assert!(fetch_insurgency_sites(&ctx, &mut s_emp, 30_045_324).await.iter().any(|(t, n)| t == "CounterInsurgencyOutpost" && n.contains("Counter-Insurgency Outpost")));

    let drop_c0 = measure_live_pvp_loot_drop_fraction(&ctx, &mut s_pir, pir_cid, pir_ship, pir_gun, front_sys, emp_vic_cid, 100_410_000, 70_000_000).await;
    assert!((drop_c0 - 0.50).abs() < 0.06);
    let base_warp = measure_live_warp_speed_au_s(&ctx, &mut s_pir, pir_cid, pir_ship, front_sys, front_far_cel).await;

    // 1. Low-Sec Corruption Stages 1 -> 2 -> 3 -> 4 -> 5 via 5 live 60s complex pulses.
    let base_front_sec = ctx.sde.solar_systems[&front_sys].security_status;
    let mut prev_drop = drop_c0;
    let mut prev_roamer_hp = 0.0f64;
    for stage in 1u8..=5 {
        ctx.drain_notifications_for_char(emp_cid);
        pulse_complex_60s(&ctx, front_sys, &mut s_pir, pir_cid, pir_ship, &[(emp_ship, front_sys), (100_400_004, front_sys), (100_400_005, cmd_sys)]).await;
        assert_eq!(local_campaign_stages(&ctx, &mut s_emp, front_sys).await, Some((stage, i64::from(stage) * 200, 0, 0)));
        assert_eq!(lookup_modified_sec(&rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap(), front_sys), (stage == 5).then_some(0.0));
        if stage == 5 {
            let notes = ctx.drain_notifications_for_char(emp_cid);
            assert!(notes.iter().any(|n| n.event == "OnSecurityModified" && n.args == vec![PyRep::Integer(i64::from(front_sys)), PyRep::Real(-base_front_sec), PyRep::Real(0.0)]));
        }

        let sites = fetch_insurgency_sites(&ctx, &mut s_emp, front_sys).await;
        assert!(sites.iter().any(|(t, _)| t == "CorporateOutpostRaid") && sites.iter().any(|(t, n)| t == "MiningOpAmbush" && n.contains("Mining Op Ambush")));
        assert_eq!(sites.iter().any(|(t, n)| t == "IceRefineryHeist" && n.contains("Ice Refinery Heist")), (3..=4).contains(&stage));
        assert_eq!(fetch_ice_heist_count(&ctx, &mut s_emp, front_sys).await, usize::from((3..=4).contains(&stage)));

        let pir_drop = measure_live_pvp_loot_drop_fraction(&ctx, &mut s_pir, pir_cid, pir_ship, pir_gun, front_sys, emp_vic_cid, 100_410_000 + i64::from(stage), 70_100_000 + i64::from(stage) * 10_000).await;
        let expected_drop = 0.50 + 0.075 * f64::from(stage);
        assert!(pir_drop > prev_drop && (pir_drop - expected_drop).abs() < 0.06, "Stage {stage} drop {pir_drop} vs {expected_drop}");
        prev_drop = pir_drop;
        if stage == 5 {
            let emp_drop = measure_live_pvp_loot_drop_fraction(&ctx, &mut s_emp, emp_cid, emp_ship, emp_gun, front_sys, pir_vic_cid, 100_410_099, 70_900_000).await;
            assert!((emp_drop - 0.50).abs() < 0.06);
        }

        let roamer_tid = with_combat_runtime(&ctx, |rt| {
            rt.npc_entities.values().find_map(|n| (n.solar_system_id == front_sys && ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 4637)).then_some(n.type_id))
        });
        assert_eq!(roamer_tid.is_some(), stage >= 2);
        if let Some(tid) = roamer_tid {
            let hp = ctx.sde.types[&tid].default_attributes.get(&263).copied().unwrap_or(0.0)
                + ctx.sde.types[&tid].default_attributes.get(&265).copied().unwrap_or(0.0)
                + ctx.sde.types[&tid].default_attributes.get(&9).copied().unwrap_or(0.0);
            assert!(hp >= prev_roamer_hp);
            prev_roamer_hp = hp;
        }

        let PyRep::List(vis) = rpc_call(&ctx, &mut s_emp, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[]).await.unwrap() else { panic!() };
        let cov: Vec<u32> = match vis[0].dict_get("systems").unwrap() { PyRep::List(l) => l.iter().map(|x| x.as_i64().unwrap() as u32).collect(), _ => panic!() };
        assert_eq!(cov.contains(&hs_sys), stage >= 3);

        let pir_warp = measure_live_warp_speed_au_s(&ctx, &mut s_pir, pir_cid, pir_ship, front_sys, front_far_cel).await;
        let emp_warp = measure_live_warp_speed_au_s(&ctx, &mut s_emp, emp_cid, emp_ship, front_sys, front_far_cel).await;
        assert!((pir_warp - if stage >= 4 { base_warp + 2.0 } else { base_warp }).abs() < 1e-6);
        assert!((emp_warp - base_warp).abs() < 1e-6);

        set_pilot_location(&ctx, &mut s_pir, front_sys, None);
        ctx.db.with_mut_snapshot(|s| { s.sp6_lifecycle.ship_combat_hp.remove(&pir_ship); });
        place_ship_ball(&ctx, front_sys, pir_cid, pir_ship, 22_456, front_gate_pos + DVec3::new(2_500.0, 0.0, 0.0)).await;
        let (gvic_cid, gvic_ship) = (90_420_000 + i64::from(stage), 100_420_000 + i64::from(stage));
        let _s_gvic = register_online_pilot(&ctx, 84_100 + u64::from(stage), &format!("gvic_{stage}"), "pw", gvic_cid, 98_420_000 + i64::from(stage), None, front_sys, gvic_ship, 621, front_gate_pos + DVec3::new(2_750.0, 0.0, 0.0), 0).await;
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&gvic_cid).unwrap().security_status = 0.0);
        let sys_h = ctx.systems.get_or_spawn(front_sys);
        let hp_before = {
            let cs = sys_h.get_combat_state(pir_ship).await.unwrap().unwrap();
            cs.shield_hp + cs.armor_hp + cs.structure_hp
        };
        sys_h.add_target_lock(pir_ship, TargetLock { target_id: gvic_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
        rpc_call(&ctx, &mut s_pir, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
        rpc_call(&ctx, &mut s_pir, "dogmaIM", "Activate", &[PyRep::Integer(pir_gun), PyRep::Integer(gvic_ship)]).await.unwrap();
        let _ = rpc_call(&ctx, &mut s_pir, "dogmaIM", "Deactivate", &[PyRep::Integer(pir_gun)]).await;
        let hp_after = {
            let cs = sys_h.get_combat_state(pir_ship).await.unwrap().unwrap();
            cs.shield_hp + cs.armor_hp + cs.structure_hp
        };
        let _ = sys_h.remove_ball(gvic_ship).await;
        if stage < 5 {
            assert!(hp_after < hp_before, "Stage {stage} sentry: {hp_after} vs {hp_before}");
            assert_eq!(rpc_call(&ctx, &mut s_pir, "dogmaIM", "Activate", &[PyRep::Integer(pir_bub)]).await.unwrap_err().code, "CannotLaunchInterdictionProbeInEmpire");
            assert_eq!(rpc_call(&ctx, &mut s_pir, "posMgr", "AnchorStructure", &[PyRep::Integer(pir_md)]).await.unwrap_err().code, "CantAnchorInEmpire");
        } else {
            assert!((hp_after - hp_before).abs() < 1e-6);
            rpc_call(&ctx, &mut s_pir, "dogmaIM", "Activate", &[PyRep::Integer(pir_bub)]).await.expect("Stage 5 low-sec allows Interdiction Sphere Launcher");
            rpc_call(&ctx, &mut s_pir, "posMgr", "AnchorStructure", &[PyRep::Integer(pir_md)]).await.expect("Stage 5 low-sec allows Mobile Warp Disruptor");
        }
    }

    // 2. High-Sec `hs_sys` Corruption Stages 1 -> 2 -> 3 -> 4 -> 5 via 5 live pulses.
    let base_hs_sec = ctx.sde.solar_systems[&hs_sys].security_status;
    let (outlaw_cid, outlaw_ship) = (90_400_020i64, 100_400_020i64);
    let mut s_outlaw = register_online_pilot(&ctx, 84_020, "b40_outlaw", "pw", outlaw_cid, 98_400_020, None, hs_sys, outlaw_ship, 621, DVec3::new(300_000.0, 0.0, 0.0), 0).await;
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&outlaw_cid).unwrap().security_status = -6.0);
    let st_pos = ctx.sde.stations[&hs_station_id].position;

    for hs_stage in 1u8..=5 {
        pulse_complex_60s(&ctx, hs_sys, &mut s_pir, pir_cid, pir_ship, &[(emp_ship, front_sys), (neu_ship, hs_sys), (outlaw_ship, hs_sys)]).await;
        assert_eq!(local_campaign_stages(&ctx, &mut s_emp, hs_sys).await, Some((hs_stage, i64::from(hs_stage) * 200, 0, 0)));

        let prior_police: Vec<i64> = with_mut_combat_runtime(&ctx, |rt| {
            let ids: Vec<i64> = rt.npc_entities.iter().filter(|(_, n)| n.solar_system_id == hs_sys && matches!(n.type_id, 9970 | 9984 | 3768 | 10046)).map(|(&bid, _)| bid).collect();
            for id in &ids { rt.npc_entities.remove(id); }
            ids
        });
        let hs_h = ctx.systems.get_or_spawn(hs_sys);
        for id in prior_police { let _ = hs_h.remove_ball(id).await; }
        set_pilot_location(&ctx, &mut s_outlaw, hs_sys, None);
        place_ship_ball(&ctx, hs_sys, outlaw_cid, outlaw_ship, 621, DVec3::new(300_000.0, 0.0, 0.0)).await;
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
        let police_spawned = with_combat_runtime(&ctx, |rt| rt.npc_entities.values().any(|n| n.solar_system_id == hs_sys && matches!(n.type_id, 9970 | 9984 | 3768 | 10046)));
        assert_eq!(police_spawned, hs_stage < 2);

        ctx.db.with_mut_snapshot(|s| {
            let ch = s.characters.get_mut(&outlaw_cid).unwrap();
            ch.active_ship_id = outlaw_ship;
            ch.security_status = -6.0;
            s.sp6_lifecycle.ship_combat_hp.remove(&outlaw_ship);
            s.items.insert(outlaw_ship, InventoryItemRecord {
                item_id: outlaw_ship, type_id: 621, owner_id: outlaw_cid, location_id: i64::from(hs_sys),
                flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
        });
        s_outlaw.ship_id = Some(outlaw_ship);
        set_pilot_location(&ctx, &mut s_outlaw, hs_sys, None);
        place_ship_ball(&ctx, hs_sys, outlaw_cid, outlaw_ship, 621, st_pos + DVec3::new(500.0, 0.0, 0.0)).await;
        let dock_res = rpc_call(&ctx, &mut s_outlaw, "stationSvc", "Dock", &[PyRep::Integer(hs_station_id)]).await;
        if hs_stage < 2 {
            assert_eq!(dock_res.unwrap_err().code, "FactionPoliceRefusedDocking");
        } else {
            dock_res.expect("Outlaw docking allowed when Faction Police suppressed at Stage 2+");
            fast_forward_secs(&ctx, 12);
            let _ = rpc_call(&ctx, &mut s_outlaw, "stationSvc", "Undock", &[]).await.unwrap();
            fast_forward_secs(&ctx, 12);
        }

        set_pilot_location(&ctx, &mut s_emp, hs_sys, None);
        set_pilot_location(&ctx, &mut s_neu, hs_sys, None);
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&neu_cid).unwrap().security_status = 0.0);
        place_ship_ball(&ctx, hs_sys, emp_cid, emp_ship, 22_456, DVec3::new(400_000.0, 0.0, 0.0)).await;
        place_ship_ball(&ctx, hs_sys, neu_cid, neu_ship, 22_456, DVec3::new(400_250.0, 0.0, 0.0)).await;
        hs_h.add_target_lock(emp_ship, TargetLock { target_id: neu_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
        rpc_call(&ctx, &mut s_emp, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(1)]).await.unwrap();
        if hs_stage < 5 {
            assert_eq!(rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[PyRep::Integer(emp_gun), PyRep::Integer(neu_ship)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
        } else {
            assert_eq!(lookup_modified_sec(&rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap(), hs_sys), Some(0.0));
            rpc_call(&ctx, &mut s_emp, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
            assert_eq!(rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[PyRep::Integer(emp_gun), PyRep::Integer(neu_ship)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
            rpc_call(&ctx, &mut s_emp, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(1)]).await.unwrap();
            rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[PyRep::Integer(emp_gun), PyRep::Integer(neu_ship)]).await.expect("Stage 5 HS allows ship attack on Yellow");
            let _ = rpc_call(&ctx, &mut s_emp, "dogmaIM", "Deactivate", &[PyRep::Integer(emp_gun)]).await;
            {
                let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                let st = cw.get_or_create(emp_cid);
                assert!(st.is_suspect(ctx.now_unix()) && !st.is_criminal(ctx.now_unix()) && st.pending_concord_arrival.is_none());
            }
            let (pod_cid, pod_id) = (90_400_029i64, 100_400_029i64);
            let _s_pod = register_online_pilot(&ctx, 84_029, "b40_pod", "pw", pod_cid, 98_400_029, None, hs_sys, pod_id, 670, DVec3::new(400_400.0, 0.0, 0.0), 0).await;
            ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&pod_cid).unwrap().security_status = 0.0);
            hs_h.add_target_lock(neu_ship, TargetLock { target_id: pod_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
            rpc_call(&ctx, &mut s_neu, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(1)]).await.unwrap();
            assert_eq!(rpc_call(&ctx, &mut s_neu, "dogmaIM", "Activate", &[PyRep::Integer(neu_gun), PyRep::Integer(pod_id)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
            rpc_call(&ctx, &mut s_neu, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
            rpc_call(&ctx, &mut s_neu, "dogmaIM", "Activate", &[PyRep::Integer(neu_gun), PyRep::Integer(pod_id)]).await.unwrap();
            let _ = rpc_call(&ctx, &mut s_neu, "dogmaIM", "Deactivate", &[PyRep::Integer(neu_gun)]).await;
            rpc_call(&ctx, &mut s_emp, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
            rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[PyRep::Integer(emp_rep), PyRep::Integer(neu_ship)]).await.unwrap();
            let _ = rpc_call(&ctx, &mut s_emp, "dogmaIM", "Deactivate", &[PyRep::Integer(emp_rep)]).await;
            {
                let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                for cid in [neu_cid, emp_cid] {
                    let st = cw.get_or_create(cid);
                    assert!(st.is_criminal(ctx.now_unix()) && st.pending_concord_arrival.is_some());
                    st.criminal_flag_until_unix = 0;
                    st.suspect_flag_until_unix = 0;
                    st.pending_concord_arrival = None;
                }
            }
            set_pilot_location(&ctx, &mut s_pir, hs_sys, None);
            place_ship_ball(&ctx, hs_sys, pir_cid, pir_ship, 22_456, DVec3::new(405_000.0, 0.0, 0.0)).await;
            assert_eq!(rpc_call(&ctx, &mut s_pir, "dogmaIM", "Activate", &[PyRep::Integer(pir_bub)]).await.unwrap_err().code, "CannotLaunchInterdictionProbeInEmpire");
        }
    }

    // 3. Live Corruption Stage Drop (`5 -> 0`) via natural campaign expiry on `tick_background_cluster`:
    set_pilot_location(&ctx, &mut s_evic, front_sys, None);
    set_pilot_location(&ctx, &mut s_emp, hs_sys, None);
    ctx.drain_notifications_for_char(emp_vic_cid);
    ctx.drain_notifications_for_char(emp_cid);
    fast_forward_secs(&ctx, ACTIVE_DURATION_SECS + WINDDOWN_DURATION_SECS + 120);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    assert_eq!(local_campaign_stages(&ctx, &mut s_emp, front_sys).await, None);
    let mod_post = rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap();
    assert_eq!(lookup_modified_sec(&mod_post, front_sys), None);
    assert_eq!(lookup_modified_sec(&mod_post, hs_sys), None);
    let front_drop_notes = ctx.drain_notifications_for_char(emp_vic_cid);
    let hs_drop_notes = ctx.drain_notifications_for_char(emp_cid);
    assert!(front_drop_notes.iter().any(|n| n.event == "OnSecurityModified" && n.args == vec![PyRep::Integer(i64::from(front_sys)), PyRep::Real(0.0), PyRep::Real(base_front_sec)]));
    assert!(hs_drop_notes.iter().any(|n| n.event == "OnSecurityModified" && n.args == vec![PyRep::Integer(i64::from(hs_sys)), PyRep::Real(0.0), PyRep::Real(base_hs_sec)]));
    for sys in [front_sys, hs_sys, fob_sys, 30_045_324] {
        assert!(fetch_insurgency_sites(&ctx, &mut s_emp, sys).await.into_iter().all(|(t, _)| t == "PirateStronghold"));
        assert_eq!(fetch_ice_heist_count(&ctx, &mut s_emp, sys).await, 0);
    }

    let drop_post = measure_live_pvp_loot_drop_fraction(&ctx, &mut s_pir, pir_cid, pir_ship, pir_gun, front_sys, emp_vic_cid, 100_490_001, 71_000_000).await;
    assert!((drop_post - 0.50).abs() < 0.06);
    assert!((measure_live_warp_speed_au_s(&ctx, &mut s_pir, pir_cid, pir_ship, front_sys, front_far_cel).await - base_warp).abs() < 1e-6);
    assert!(!with_combat_runtime(&ctx, |rt| rt.npc_entities.values().any(|n| ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 4637))));

    set_pilot_location(&ctx, &mut s_pir, front_sys, None);
    let pir_md2 = 60_402_099i64;
    ctx.db.with_mut_snapshot(|s| {
        s.sp6_lifecycle.ship_combat_hp.remove(&pir_ship);
        s.items.insert(pir_md2, InventoryItemRecord {
            item_id: pir_md2, type_id: 12_198, owner_id: pir_cid, location_id: pir_ship,
            flag: InventoryFlag::Cargo, quantity: 1, singleton: false, version: 1, ..Default::default()
        });
    });
    place_ship_ball(&ctx, front_sys, pir_cid, pir_ship, 22_456, front_gate_pos + DVec3::new(2_500.0, 0.0, 0.0)).await;
    assert_eq!(rpc_call(&ctx, &mut s_pir, "dogmaIM", "Activate", &[PyRep::Integer(pir_bub)]).await.unwrap_err().code, "CannotLaunchInterdictionProbeInEmpire");
    assert_eq!(rpc_call(&ctx, &mut s_pir, "posMgr", "AnchorStructure", &[PyRep::Integer(pir_md2)]).await.unwrap_err().code, "CantAnchorInEmpire");
    let (post_gcid, post_gvic) = (90_490_010i64, 100_490_010i64);
    let _s_pgv = register_online_pilot(&ctx, 84_199, "pgvic", "pw", post_gcid, 98_490_010, None, front_sys, post_gvic, 621, front_gate_pos + DVec3::new(2_750.0, 0.0, 0.0), 0).await;
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&post_gcid).unwrap().security_status = 0.0);
    let front_h = ctx.systems.get_or_spawn(front_sys);
    let g_hp0 = {
        let cs = front_h.get_combat_state(pir_ship).await.unwrap().unwrap();
        cs.shield_hp + cs.armor_hp + cs.structure_hp
    };
    front_h.add_target_lock(pir_ship, TargetLock { target_id: post_gvic, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
    rpc_call(&ctx, &mut s_pir, "dogmaIM", "Activate", &[PyRep::Integer(pir_gun), PyRep::Integer(post_gvic)]).await.unwrap();
    let _ = rpc_call(&ctx, &mut s_pir, "dogmaIM", "Deactivate", &[PyRep::Integer(pir_gun)]).await;
    let g_hp1 = {
        let cs = front_h.get_combat_state(pir_ship).await.unwrap().unwrap();
        cs.shield_hp + cs.armor_hp + cs.structure_hp
    };
    assert!(g_hp1 < g_hp0);

    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&outlaw_cid).unwrap();
        ch.active_ship_id = outlaw_ship;
        ch.security_status = -6.0;
        s.sp6_lifecycle.ship_combat_hp.remove(&outlaw_ship);
        s.items.insert(outlaw_ship, InventoryItemRecord {
            item_id: outlaw_ship, type_id: 621, owner_id: outlaw_cid, location_id: i64::from(hs_sys),
            flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
    });
    s_outlaw.ship_id = Some(outlaw_ship);
    set_pilot_location(&ctx, &mut s_outlaw, hs_sys, None);
    place_ship_ball(&ctx, hs_sys, outlaw_cid, outlaw_ship, 621, st_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    assert_eq!(rpc_call(&ctx, &mut s_outlaw, "stationSvc", "Dock", &[PyRep::Integer(hs_station_id)]).await.unwrap_err().code, "FactionPoliceRefusedDocking");
    set_pilot_location(&ctx, &mut s_emp, hs_sys, None);
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&neu_cid).unwrap().security_status = 0.0);
    place_ship_ball(&ctx, hs_sys, emp_cid, emp_ship, 22_456, DVec3::new(400_000.0, 0.0, 0.0)).await;
    place_ship_ball(&ctx, hs_sys, neu_cid, neu_ship, 22_456, DVec3::new(400_250.0, 0.0, 0.0)).await;
    ctx.systems.get_or_spawn(hs_sys).add_target_lock(emp_ship, TargetLock { target_id: neu_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
    rpc_call(&ctx, &mut s_emp, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(1)]).await.unwrap();
    assert_eq!(rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[PyRep::Integer(emp_gun), PyRep::Integer(neu_ship)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
}

#[allow(unused_imports, dead_code)]
mod insurgency_suppression_effects_and_rpc_auth {
    use super::*;

// Integration tests for Pirate Insurgency Suppression (Stages 1–5) system effects,
// effect removal upon stage drop, and negative RPC auth/validation (`feat:fw.corruption-suppression-effects`).


use crate::features_freight_and_courier::{
    fast_forward_secs, fit_tactical_sabre, in_memory_ctx, local_campaign_stages,
    lookup_modified_sec, pulse_complex_60s, setup_warzone_chain_with_hs_station,
};
use crate::sovereignty_equinox_gameplay::{
    fit_siege_artillery, make_ball, place_ship_ball, register_online_pilot, rpc_call,
    set_pilot_location,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::TargetLock;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    spawn_specialized_npc_entity, with_combat_runtime, with_mut_combat_runtime,
};
use eve_server::services::insurgency_services::{
    ACTIVE_DURATION_SECS, FORECASTING_DURATION_SECS, WINDDOWN_DURATION_SECS,
};
use glam::DVec3;
use std::collections::HashMap;

async fn measure_live_npc_kill_bounty_and_sec_gain(
    ctx: &ServerContext,
    sess: &mut SessionState,
    cid: i64,
    ship_id: i64,
    gun_id: i64,
    sys_id: u32,
    rat_type_id: u32,
) -> (f64, f64) {
    set_pilot_location(ctx, sess, sys_id, None);
    let pos = DVec3::new(350_000.0, -250_000.0, 0.0);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().security_status = 0.0;
        s.sp6_lifecycle.ship_combat_hp.remove(&ship_id);
        if let Some(it) = s.items.get_mut(&ship_id) {
            it.location_id = i64::from(sys_id);
        }
    });
    with_mut_combat_runtime(ctx, |rt| {
        rt.pending_bounty_ticks.remove(&(sys_id, cid));
    });
    place_ship_ball(ctx, sys_id, cid, ship_id, 22_456, pos).await;
    let npc_ball_id =
        spawn_specialized_npc_entity(ctx, sys_id, rat_type_id, "Pirate", 1, pos + DVec3::new(250.0, 0.0, 0.0)).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id: npc_ball_id,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await
        .unwrap();
    rpc_call(ctx, sess, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(npc_ball_id)])
        .await
        .expect("live NPC rat kill via dogmaIM.Activate");
    for _ in 0..8 {
        sys_h.step_manual(15.0).await.unwrap();
        if sys_h.get_ball_snapshot(npc_ball_id).await.unwrap().is_none() {
            break;
        }
    }
    let _ = rpc_call(ctx, sess, "dogmaIM", "Deactivate", &[PyRep::Integer(gun_id)]).await;
    fast_forward_secs(ctx, 12);
    sys_h.step_manual(12.0).await.unwrap();
    assert!(sys_h.get_ball_snapshot(npc_ball_id).await.unwrap().is_none(), "NPC rat {npc_ball_id} must be destroyed by live turret cycles");
    let bounty = with_combat_runtime(ctx, |rt| rt.pending_bounty_ticks.get(&(sys_id, cid)).copied().unwrap_or(0.0));
    let sec_gain = ctx.db.with_snapshot(|s| s.characters[&cid].security_status);
    assert!(bounty > 0.0 && sec_gain > 0.0);
    (bounty, sec_gain)
}

async fn measure_live_militia_pvp_kill_lp(
    ctx: &ServerContext,
    killer_sess: &mut SessionState,
    killer_cid: i64,
    killer_ship: i64,
    gun_id: i64,
    militia_corp: i64,
    sys_id: u32,
    victim_cid: i64,
    victim_ship: i64,
) -> i64 {
    set_pilot_location(ctx, killer_sess, sys_id, None);
    let pos = DVec3::new(-350_000.0, 250_000.0, 0.0);
    let lp_before = ctx
        .db
        .with_snapshot(|s| s.characters[&killer_cid].loyalty_points.get(&militia_corp).copied().unwrap_or(0));
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ks) = s.items.get_mut(&killer_ship) {
            ks.location_id = i64::from(sys_id);
        }
        let vch = s.characters.get_mut(&victim_cid).unwrap();
        vch.solar_system_id = sys_id;
        vch.active_ship_id = victim_ship;
        s.items.insert(
            victim_ship,
            InventoryItemRecord {
                item_id: victim_ship,
                type_id: 621,
                owner_id: victim_cid,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    place_ship_ball(ctx, sys_id, killer_cid, killer_ship, 22_456, pos).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let mut vcs = ctx.build_type_combat_state(621);
    vcs.shield_hp = 0.0;
    vcs.armor_hp = 0.0;
    vcs.structure_hp = 1.0;
    let _ = sys_h.remove_ball(victim_ship).await;
    sys_h
        .spawn_ball_with_combat(make_ball(victim_ship, 621, pos + DVec3::new(250.0, 0.0, 0.0)), vcs)
        .await
        .unwrap();
    sys_h
        .add_target_lock(
            killer_ship,
            TargetLock {
                target_id: victim_ship,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await
        .unwrap();
    rpc_call(ctx, killer_sess, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)])
        .await
        .unwrap();
    rpc_call(ctx, killer_sess, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(victim_ship)])
        .await
        .expect("live militia PvP ship kill");
    let _ = rpc_call(ctx, killer_sess, "dogmaIM", "Deactivate", &[PyRep::Integer(gun_id)]).await;
    fast_forward_secs(ctx, 12);
    sys_h.step_manual(12.0).await.unwrap();
    let lp_after = ctx
        .db
        .with_snapshot(|s| s.characters[&killer_cid].loyalty_points.get(&militia_corp).copied().unwrap_or(0));
    lp_after - lp_before
}

#[tokio::test]
async fn test_suppression_all_stages_1_to_5_and_drop_removal() {
    // parity: feat:fw.corruption-suppression-effects
    eve_testkit::attest(&["feat:fw.corruption-suppression-effects"]);

    let ctx = in_memory_ctx();
    let (_enemy_sys, front_sys, cmd_sys, _rear_sys, _hs_sys, _hs_st) =
        setup_warzone_chain_with_hs_station(&ctx);

    let (emp_cid, emp_ship, pir_cid, pir_ship, emp_vic_cid, pir_vic_cid) =
        (90_450_001, 100_450_001, 90_450_002, 100_450_002, 90_450_004, 90_450_005);

    let mut s_emp = register_online_pilot(&ctx, 84_501, "b40s_emp", "pw", emp_cid, 98_450_001, None, cmd_sys, emp_ship, 22_456, DVec3::new(900_000.0, 900_000.0, 0.0), 0).await;
    let mut s_pir = register_online_pilot(&ctx, 84_502, "b40s_pir", "pw", pir_cid, 98_450_002, None, front_sys, pir_ship, 22_456, DVec3::new(900_000.0, 900_000.0, 0.0), 0).await;
    let _s_evic = register_online_pilot(&ctx, 84_504, "b40s_evic", "pw", emp_vic_cid, 98_450_004, None, cmd_sys, 100_450_004, 621, DVec3::new(950_000.0, 950_000.0, 0.0), 0).await;
    let mut s_pvic = register_online_pilot(&ctx, 84_505, "b40s_pvic", "pw", pir_vic_cid, 98_450_005, None, cmd_sys, 100_450_005, 621, DVec3::new(950_000.0, 950_000.0, 0.0), 0).await;

    rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)]).await.unwrap();
    rpc_call(&ctx, &mut s_pir, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
    rpc_call(&ctx, &mut s_pvic, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&emp_vic_cid).unwrap().war_faction_id = Some(500_001));

    let (emp_gun, _eb, _er, emp_web, emp_scram, _emd) = fit_tactical_sabre(&ctx, emp_cid, emp_ship, 60_451_000);
    let (pir_gun, _pb, _pr, pir_web, _ps, _pmd) = fit_tactical_sabre(&ctx, pir_cid, pir_ship, 60_452_000);

    let caldari_militia_corp = ctx.sde.factions[&500_001].militia_corporation_id.unwrap() as i64;
    let guristas_militia_corp = ctx.sde.factions[&500_010].militia_corporation_id.unwrap() as i64;
    let mut rat_cands: Vec<(u64, u32)> = ctx
        .sde
        .types
        .values()
        .filter_map(|t| {
            let bty = t.default_attributes.get(&481).copied().unwrap_or(0.0);
            if t.category_id != 11 || bty < 5_000.0 {
                return None;
            }
            let hp = t.default_attributes.get(&263).copied().unwrap_or(0.0)
                + t.default_attributes.get(&265).copied().unwrap_or(0.0)
                + t.default_attributes.get(&9).copied().unwrap_or(0.0);
            (hp > 0.0 && hp <= 1_200.0).then_some((hp as u64, t.type_id))
        })
        .collect();
    rat_cands.sort_unstable();
    let rat_type_id = rat_cands[0].1;

    let _ = rpc_call(&ctx, &mut s_pir, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let (base_bounty, base_sec_gain) =
        measure_live_npc_kill_bounty_and_sec_gain(&ctx, &mut s_emp, emp_cid, emp_ship, emp_gun, cmd_sys, rat_type_id).await;
    assert!(base_bounty > 0.0 && base_sec_gain > 0.0);
    let base_emp_lp =
        measure_live_militia_pvp_kill_lp(&ctx, &mut s_emp, emp_cid, emp_ship, emp_gun, caldari_militia_corp, cmd_sys, pir_vic_cid, 100_470_000).await;
    let base_pir_lp =
        measure_live_militia_pvp_kill_lp(&ctx, &mut s_pir, pir_cid, pir_ship, pir_gun, guristas_militia_corp, cmd_sys, emp_vic_cid, 100_470_100).await;
    assert!(base_emp_lp > 0 && base_pir_lp > 0);

    // 1. Step `cmd_sys` through Suppression Stages 1 -> 2 -> 3 -> 4 -> 5 via 5 live 60s Empire complex pulses.
    let mut expected_cmd_corr_pts = 0i64;
    let mut prev_enf_hp = 0.0f64;
    for s_stage in 1u8..=5 {
        pulse_complex_60s(&ctx, cmd_sys, &mut s_emp, emp_cid, emp_ship, &[(pir_ship, cmd_sys), (100_450_004, cmd_sys), (100_450_005, cmd_sys)]).await;
        let (_, cur_c_pts, cur_s_st, cur_s_pts) = local_campaign_stages(&ctx, &mut s_emp, cmd_sys).await.unwrap();
        assert_eq!((cur_s_st, cur_s_pts, cur_c_pts), (s_stage, i64::from(s_stage) * 200, expected_cmd_corr_pts));
        assert_eq!(lookup_modified_sec(&rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap(), cmd_sys), None);

        let (st_bounty, st_sec_gain) =
            measure_live_npc_kill_bounty_and_sec_gain(&ctx, &mut s_emp, emp_cid, emp_ship, emp_gun, cmd_sys, rat_type_id).await;
        let b_mult = 1.0 + 0.05 * f64::from(s_stage);
        assert!((st_bounty - base_bounty * b_mult).abs() < 1e-4);
        assert!((st_sec_gain - base_sec_gain * b_mult).abs() < 1e-9);

        let st_emp_lp =
            measure_live_militia_pvp_kill_lp(&ctx, &mut s_emp, emp_cid, emp_ship, emp_gun, caldari_militia_corp, cmd_sys, pir_vic_cid, 100_470_000 + i64::from(s_stage)).await;
        let lp_mult = if s_stage >= 2 { 1.0 + 0.05 * f64::from(s_stage - 1) } else { 1.0 };
        assert_eq!(st_emp_lp, ((base_emp_lp as f64) * lp_mult).round() as i64);
        if s_stage == 5 {
            let st_pir_lp =
                measure_live_militia_pvp_kill_lp(&ctx, &mut s_pir, pir_cid, pir_ship, pir_gun, guristas_militia_corp, cmd_sys, emp_vic_cid, 100_470_199).await;
            assert_eq!(st_pir_lp, base_pir_lp);
        }

        let enf_tid = with_combat_runtime(&ctx, |rt| {
            rt.npc_entities.values().find_map(|n| (n.solar_system_id == cmd_sys && ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 4638)).then_some(n.type_id))
        });
        assert_eq!(enf_tid.is_some(), s_stage >= 3);
        if let Some(tid) = enf_tid {
            let hp = ctx.sde.types[&tid].default_attributes.get(&263).copied().unwrap_or(0.0)
                + ctx.sde.types[&tid].default_attributes.get(&265).copied().unwrap_or(0.0)
                + ctx.sde.types[&tid].default_attributes.get(&9).copied().unwrap_or(0.0);
            assert!(hp >= prev_enf_hp);
            prev_enf_hp = hp;
        }
        if s_stage == 3 {
            pulse_complex_60s(&ctx, cmd_sys, &mut s_pir, pir_cid, pir_ship, &[(emp_ship, cmd_sys), (100_450_004, cmd_sys), (100_450_005, cmd_sys)]).await;
            expected_cmd_corr_pts = 140;
            let (c_st3, c_pts3, _, _) = local_campaign_stages(&ctx, &mut s_emp, cmd_sys).await.unwrap();
            assert_eq!((c_st3, c_pts3), (1, 140), "Suppression Stage 3 must apply -30% corruption gain (200 -> 140)");
        }

        let cmd_h = ctx.systems.get_or_spawn(cmd_sys);
        set_pilot_location(&ctx, &mut s_emp, cmd_sys, None);
        set_pilot_location(&ctx, &mut s_pir, cmd_sys, None);
        place_ship_ball(&ctx, cmd_sys, emp_cid, emp_ship, 22_456, DVec3::new(-500_000.0, 0.0, 0.0)).await;
        place_ship_ball(&ctx, cmd_sys, pir_cid, pir_ship, 22_456, DVec3::new(-489_500.0, 0.0, 0.0)).await;
        cmd_h.add_target_lock(emp_ship, TargetLock { target_id: pir_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
        cmd_h.add_target_lock(pir_ship, TargetLock { target_id: emp_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
        let web_res = rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[PyRep::Integer(emp_web), PyRep::Integer(pir_ship)]).await;
        if s_stage < 4 {
            assert_eq!(web_res.unwrap_err().code, "TargetOutOfRange");
        } else {
            web_res.expect("Empire Webifier succeeds at 10,500 m at Suppression Stage 4+");
            let _ = rpc_call(&ctx, &mut s_emp, "dogmaIM", "Deactivate", &[PyRep::Integer(emp_web)]).await;
            assert_eq!(rpc_call(&ctx, &mut s_pir, "dogmaIM", "Activate", &[PyRep::Integer(pir_web), PyRep::Integer(emp_ship)]).await.unwrap_err().code, "TargetOutOfRange");
        }
        place_ship_ball(&ctx, cmd_sys, pir_cid, pir_ship, 22_456, DVec3::new(-492_000.0, 0.0, 0.0)).await;
        cmd_h.add_target_lock(emp_ship, TargetLock { target_id: pir_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
        let scram_res = rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[PyRep::Integer(emp_scram), PyRep::Integer(pir_ship)]).await;
        if s_stage < 4 {
            assert_eq!(scram_res.unwrap_err().code, "TargetOutOfRange");
        } else {
            scram_res.expect("Empire Warp Scrambler succeeds at 8,000 m at Suppression Stage 4+");
            assert!(cmd_h.ballpark.read().unwrap().is_ball_scrambled(pir_ship));
            let _ = rpc_call(&ctx, &mut s_emp, "dogmaIM", "Deactivate", &[PyRep::Integer(emp_scram)]).await;
        }

        let ci_sentry = with_combat_runtime(&ctx, |rt| {
            rt.npc_entities.values().find(|n| n.solar_system_id == cmd_sys && n.type_id == 79_507).map(|n| (n.ball_id, n.spawn_position))
        });
        assert_eq!(ci_sentry.is_some(), s_stage == 5);
        if let Some((_ci_bid, ci_pos)) = ci_sentry {
            ctx.db.with_mut_snapshot(|s| { s.sp6_lifecycle.ship_combat_hp.remove(&pir_ship); });
            place_ship_ball(&ctx, cmd_sys, pir_cid, pir_ship, 22_456, ci_pos + DVec3::new(8_000.0, 0.0, 0.0)).await;
            let (sig_before, sh_before) = {
                let bp = cmd_h.ballpark.read().unwrap();
                let cs = &bp.combat_states[&pir_ship];
                (cs.signature_radius, cs.shield_hp)
            };
            eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
            {
                let bp = cmd_h.ballpark.read().unwrap();
                let cs_after = &bp.combat_states[&pir_ship];
                assert!(cs_after.shield_hp < sh_before && cs_after.signature_radius > sig_before * 1.4 && cs_after.ecm_jammed_remaining_s > 0.0);
            }

            let PyRep::List(vis_pre) = rpc_call(&ctx, &mut s_emp, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[]).await.unwrap() else { panic!() };
            let sys_count_pre = match vis_pre[0].dict_get("systems").unwrap() { PyRep::List(l) => l.len(), _ => 0 };
            for _ in 0..6 {
                pulse_complex_60s(&ctx, cmd_sys, &mut s_pir, pir_cid, pir_ship, &[(emp_ship, cmd_sys), (100_450_004, cmd_sys), (100_450_005, cmd_sys)]).await;
            }
            let (cap_c_st, cap_c_pts, _, _) = local_campaign_stages(&ctx, &mut s_emp, cmd_sys).await.unwrap();
            assert_eq!((cap_c_st, cap_c_pts), (4, 800));
            let PyRep::List(vis_post) = rpc_call(&ctx, &mut s_emp, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[]).await.unwrap() else { panic!() };
            let sys_count_post = match vis_post[0].dict_get("systems").unwrap() { PyRep::List(l) => l.len(), _ => 0 };
            assert_eq!(sys_count_post, sys_count_pre);
        }
    }

    // 2. Non-retroactive Stage 5 rule: `front_sys` at Corruption 5 -> Suppression 5 preserves LAWLESS Corruption 5!
    for _ in 0..5 {
        pulse_complex_60s(&ctx, front_sys, &mut s_pir, pir_cid, pir_ship, &[(emp_ship, cmd_sys)]).await;
    }
    for _ in 0..5 {
        pulse_complex_60s(&ctx, front_sys, &mut s_emp, emp_cid, emp_ship, &[(pir_ship, front_sys)]).await;
    }
    assert_eq!(local_campaign_stages(&ctx, &mut s_emp, front_sys).await, Some((5, 1_000, 5, 1_000)));
    assert_eq!(lookup_modified_sec(&rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap(), front_sys), Some(0.0));

    // 3. Live Suppression Stage Drop (`5 -> 0`) via natural campaign expiry on `tick_background_cluster`:
    fast_forward_secs(&ctx, ACTIVE_DURATION_SECS + WINDDOWN_DURATION_SECS + 120);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    assert_eq!(local_campaign_stages(&ctx, &mut s_emp, cmd_sys).await, None);
    let (post_bounty, post_sec) = measure_live_npc_kill_bounty_and_sec_gain(&ctx, &mut s_emp, emp_cid, emp_ship, emp_gun, cmd_sys, rat_type_id).await;
    assert!((post_bounty - base_bounty).abs() < 1e-4 && (post_sec - base_sec_gain).abs() < 1e-9);
    let post_lp = measure_live_militia_pvp_kill_lp(&ctx, &mut s_emp, emp_cid, emp_ship, emp_gun, caldari_militia_corp, cmd_sys, pir_vic_cid, 100_495_002).await;
    assert_eq!(post_lp, base_emp_lp);
    assert!(!with_combat_runtime(&ctx, |rt| rt.npc_entities.values().any(|n| ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 4638))));

    let cmd_h = ctx.systems.get_or_spawn(cmd_sys);
    set_pilot_location(&ctx, &mut s_emp, cmd_sys, None);
    place_ship_ball(&ctx, cmd_sys, emp_cid, emp_ship, 22_456, DVec3::new(-500_000.0, 0.0, 0.0)).await;
    place_ship_ball(&ctx, cmd_sys, pir_cid, pir_ship, 22_456, DVec3::new(-489_500.0, 0.0, 0.0)).await;
    cmd_h.add_target_lock(emp_ship, TargetLock { target_id: pir_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
    assert_eq!(rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[PyRep::Integer(emp_web), PyRep::Integer(pir_ship)]).await.unwrap_err().code, "TargetOutOfRange");
    place_ship_ball(&ctx, cmd_sys, pir_cid, pir_ship, 22_456, DVec3::new(-492_000.0, 0.0, 0.0)).await;
    cmd_h.add_target_lock(emp_ship, TargetLock { target_id: pir_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
    assert_eq!(rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[PyRep::Integer(emp_scram), PyRep::Integer(pir_ship)]).await.unwrap_err().code, "TargetOutOfRange");

    // Persistent `79507` Counter-Insurgency Sentry Gun survives campaign end until destroyed via live combat:
    let (ci_bid, ci_pos) = with_combat_runtime(&ctx, |rt| {
        rt.npc_entities.values().find(|n| n.solar_system_id == cmd_sys && n.type_id == 79_507).map(|n| (n.ball_id, n.spawn_position))
    }).expect("79507 sentry persists after campaign end");
    set_pilot_location(&ctx, &mut s_pir, cmd_sys, None);
    ctx.db.with_mut_snapshot(|s| {
        s.items.retain(|_, i| i.location_id != pir_ship);
        s.sp6_lifecycle.ship_combat_hp.remove(&pir_ship);
    });
    fit_siege_artillery(&ctx, pir_cid, pir_ship, 60_499_000);
    ctx.invalidate_ship_fitting(pir_ship);
    place_ship_ball(&ctx, cmd_sys, pir_cid, pir_ship, 19_722, ci_pos + DVec3::new(5_000.0, 0.0, 0.0)).await;
    cmd_h.add_target_lock(pir_ship, TargetLock { target_id: ci_bid, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
    for siege_gun in [60_499_000i64, 60_499_002, 60_499_004] {
        rpc_call(&ctx, &mut s_pir, "dogmaIM", "Activate", &[PyRep::Integer(siege_gun), PyRep::Integer(ci_bid)]).await
            .expect("Activate XL Siege Artillery on persistent 79507 sentry");
    }
    for _ in 0..80 {
        cmd_h.step_manual(12.0).await.unwrap();
        if cmd_h.get_ball_snapshot(ci_bid).await.unwrap().is_none() {
            break;
        }
    }
    assert!(cmd_h.get_ball_snapshot(ci_bid).await.unwrap().is_none());
}

#[tokio::test]
async fn test_security_mgr_and_insurgency_rpc_negative_auth_and_validation() {
    // parity: feat:fw.corruption-suppression-effects
    eve_testkit::attest(&["feat:fw.corruption-suppression-effects"]);
    let ctx = in_memory_ctx();
    let kw = HashMap::new();

    // 1. Unauthenticated (`Unauthorized`) and pre-character-selection (`CharacterNotSelected`) sessions rejected across all touched RPC handlers:
    let mut unauth = SessionState::new(84_901);
    ctx.db.provision_player_account("b40_nochar", "pw");
    let mut no_char = SessionState::new(84_903);
    crate::common::login_pilot(&ctx, &mut no_char, "b40_nochar", "pw").await;
    for (svc, method, args) in [
        ("securityMgr", "get_modified_systems", vec![]),
        ("insurgencySolarsystem", "GetAllVisibleCampaigns", vec![]),
        ("insurgencySolarsystem", "GetLocalCampaignClientSnapshot", vec![]),
        ("dungeonInstanceCacheMgr", "GetPirateInsurgencyInstances", vec![]),
        ("dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances", vec![]),
    ] {
        let u_err = ctx.dispatch_rpc(&mut unauth, svc, None, method, &args, &kw, None).await.unwrap_err();
        assert_eq!(u_err.code, "Unauthorized", "{svc}.{method} must reject unauthenticated session");
        let c_err = ctx.dispatch_rpc(&mut no_char, svc, None, method, &args, &kw, None).await.unwrap_err();
        assert_eq!(c_err.code, "CharacterNotSelected", "{svc}.{method} must require selected character");
    }

    // 2. Authenticated character negative validation (`NoSolarSystem`, `InvalidSolarSystem`, cross-service unknown method):
    let mut orphan = no_char.clone();
    orphan.character_id = Some(90_499_999);
    orphan.solar_system_id = None;
    let no_sys_err = ctx.dispatch_rpc(&mut orphan, "insurgencySolarsystem", None, "GetLocalCampaignClientSnapshot", &[], &kw, None).await.unwrap_err();
    assert_eq!(no_sys_err.code, "NoSolarSystem");

    let mut sess = register_online_pilot(&ctx, 84_902, "b40_neg", "pw", 90_499_001, 98_499_001, None, 30_000_142, 100_499_001, 587, DVec3::ZERO, 0).await;
    let wrong_svc = ctx.dispatch_rpc(&mut sess, "securityMgr", None, "GetAllVisibleCampaigns", &[], &kw, None).await.unwrap_err();
    assert_eq!(wrong_svc.code, "UnimplementedRPC");
    for bad_arg in [
        PyRep::Integer(-1),
        PyRep::Integer(0),
        PyRep::Integer(99_999_999),
        PyRep::String("not_a_system_id".into()),
    ] {
        for method in ["GetPirateInsurgencyInstances", "GetPirateInsurgencyIceHeistInstances"] {
            let err = ctx
                .dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, method, std::slice::from_ref(&bad_arg), &kw, None)
                .await
                .unwrap_err();
            assert_eq!(err.code, "InvalidSolarSystem", "{method} must reject {bad_arg:?}"); // rpc-lint: allow UserError code, not RPC method
        }
    }
}

#[tokio::test]
async fn test_specialized_npc_destroy_grants_sec_status_with_suppression_multiplier() {
    // parity: feat:fw.corruption-suppression-effects
    eve_testkit::attest(&["feat:fw.corruption-suppression-effects"]);
    let ctx = in_memory_ctx();
    let lowsec_sys = 30_002_537u32; // Tama
    let char_id = 90_499_111i64;
    let _sess = register_online_pilot(
        &ctx,
        84_911,
        "b40_spec_sec",
        "pw",
        char_id,
        98_499_111,
        None,
        lowsec_sys,
        100_499_111,
        587,
        DVec3::ZERO,
        0,
    )
    .await;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.security_status = 0.0;
        }
    });

    // 1. Baseline specialized NPC kill through destroy_specialized_npc_and_queue_bounty
    let npc1 = spawn_specialized_npc_entity(&ctx, lowsec_sys, 16_934, "Belt", 1, DVec3::new(5_000.0, 0.0, 0.0)).await;
    let (bounty1, _, _) = eve_server::services::combat_crimewatch::combat_npc_ai::destroy_specialized_npc_and_queue_bounty(
        &ctx,
        lowsec_sys,
        npc1,
        char_id,
    )
    .await;
    assert!(bounty1 > 0);
    let delta_base = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.security_status).unwrap_or(0.0));
    assert!(delta_base > 0.0, "Specialized NPC kill must grant positive security status (got {delta_base})");
    let tx_count = with_combat_runtime(&ctx, |rt| {
        rt.sec_status_transactions
            .get(&char_id)
            .map(|v| v.iter().filter(|t| t.reason == "NPC Pirate Bounty").count())
            .unwrap_or(0)
    });
    assert_eq!(tx_count, 1, "Must record 'NPC Pirate Bounty' SecStatusTransaction");

    // 2. Suppression Stage 5 (1.25x sec-status multiplier)
    fast_forward_secs(&ctx, 905);
    let now = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.security_status = 0.0;
        }
        s.insurgency_runtime.campaigns.insert(
            77_911,
            eve_db::PersistedInsurgencyCampaign {
                campaign_id: 77_911,
                pirate_faction_id: 500_011,
                target_faction_id: 500_001,
                fob_solar_system_id: lowsec_sys,
                systems: vec![lowsec_sys],
                created_at_unix: now.saturating_sub(20),
                forecasting_end_unix: now.saturating_sub(10),
                active_end_unix: now + 3_000,
                winddown_end_unix: now + 3_600,
                corruption_points: HashMap::new(),
                suppression_points: HashMap::from([(lowsec_sys, 1_000)]),
            },
        );
    });
    let npc2 = spawn_specialized_npc_entity(&ctx, lowsec_sys, 16_934, "Belt", 1, DVec3::new(6_000.0, 0.0, 0.0)).await;
    let _ = eve_server::services::combat_crimewatch::combat_npc_ai::destroy_specialized_npc_and_queue_bounty(
        &ctx,
        lowsec_sys,
        npc2,
        char_id,
    )
    .await;
    let delta_supp = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.security_status).unwrap_or(0.0));
    assert!(
        (delta_supp - delta_base * 1.25).abs() < 1e-9,
        "Suppression Stage 5 must apply 1.25x sec-status multiplier (base={delta_base}, supp={delta_supp})"
    );
}

}
