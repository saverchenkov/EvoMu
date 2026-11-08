#![forbid(unsafe_code)]
#[allow(unused_imports)]
use super::*;

pub(crate) mod parity_notify_batch4 {
    #[allow(unused_imports)]
    use super::*;

// Integration tests verifying remediation of FLAGGED_FAKE `notify:*` rows (batch 4).

use crate::common;
use crate::notify_structures_clones_pi::login_in_space;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::TargetLock;
use eve_machonet::service_roles::{ROLE_GMH, ROLE_GM_DEFAULT};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::insurgency_services::{add_system_corruption, init_insurgency_campaign, FORECASTING_DURATION_SECS};
use eve_server::services::pve_special_sites::record_system_interference_activity;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;
const SYS_ZARZAKH: u32 = 30_100_000;
const JITA_STATION: i64 = 60_003_760;

fn new_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    ctx.set_clock_paused(true);
    eve_server::services::serverops_cluster::set_scheduled_downtime_enabled(&ctx, false);
    ctx
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

fn drain_all(ctx: &ServerContext, char_id: i64) -> Vec<(String, String, Vec<PyRep>)> {
    ctx.drain_notifications_for_char(char_id)
        .into_iter()
        .map(|n| (n.event, n.id_type, n.args))
        .collect()
}

fn has_notif(notifs: &[(String, String, Vec<PyRep>)], event: &str) -> bool {
    notifs.iter().any(|(e, _, _)| e == event)
}

fn find_notif<'a>(notifs: &'a [(String, String, Vec<PyRep>)], event: &str) -> Option<&'a (String, String, Vec<PyRep>)> {
    notifs.iter().find(|(e, _, _)| e == event)
}

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.get(key),
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        _ => None,
    }
}

fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id, type_id, owner_id, location_id, flag, quantity, singleton,
        version: 1, dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
        insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
    }
}

#[tokio::test]
async fn test_batch4_crimewatch_timers_and_system_flag_updates() {
    let ctx = new_ctx();
    let jita_pos = ctx.sde.stations[&JITA_STATION].position + DVec3::new(10_000.0, 0.0, 0.0);
    let (ship_a, ship_b, gun_id) = (640_000_001_i64, 640_000_002_i64, 640_000_010_i64);
    let mut sess_a = login_in_space(&ctx, 92_001, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, jita_pos).await;
    let _sess_b = login_in_space(&ctx, 92_002, "other", "other-secret", PILOT_B, ship_b, 587, SYS_JITA, jita_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(gun_id, make_item(gun_id, 484, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(gun_id, true);
    });
    ctx.invalidate_ship_fitting(ship_a);
    rpc(&ctx, &mut sess_a, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();

    let sys_h = ctx.systems.get_or_spawn(SYS_JITA);
    sys_h.add_target_lock(ship_a, TargetLock { target_id: ship_b, lock_time_ms: 100.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await.unwrap();

    // Negative: no timer or flag notifications before aggression
    let pre_a = drain_all(&ctx, PILOT_A);
    let pre_b = drain_all(&ctx, PILOT_B);
    assert!(!has_notif(&pre_a, "OnWeaponsTimerUpdate"));
    assert!(!has_notif(&pre_a, "OnPvpTimerUpdate"));
    assert!(!has_notif(&pre_a, "OnSystemCriminalFlagUpdates"));
    assert!(!has_notif(&pre_b, "OnSystemCriminalFlagUpdates"));

    // Positive: highsec aggression emits OnWeaponsTimerUpdate, OnPvpTimerUpdate, and OnSystemCriminalFlagUpdates
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("projectileFired".into()), PyRep::Integer(ship_b)]).await.unwrap();

    let post_a = drain_all(&ctx, PILOT_A);
    let post_b = drain_all(&ctx, PILOT_B);

    let (_, weap_id_type, weap_args) = find_notif(&post_a, "OnWeaponsTimerUpdate").expect("OnWeaponsTimerUpdate emitted");
    assert_eq!(weap_id_type, "charid");
    assert_eq!(weap_args.len(), 2);
    assert_eq!(weap_args[0], PyRep::Integer(101));
    assert!(weap_args[1].as_i64().unwrap_or(0) > 130_000_000_000_000_000);

    let (_, pvp_id_type, pvp_args) = find_notif(&post_a, "OnPvpTimerUpdate").expect("OnPvpTimerUpdate emitted");
    assert_eq!(pvp_id_type, "charid");
    assert_eq!(pvp_args.len(), 2);
    assert_eq!(pvp_args[0], PyRep::Integer(201));
    assert!(pvp_args[1].as_i64().unwrap_or(0) > 130_000_000_000_000_000);

    let (_, crim_id_type, crim_args) = find_notif(&post_b, "OnSystemCriminalFlagUpdates").expect("bystander receives OnSystemCriminalFlagUpdates");
    assert_eq!(crim_id_type, "solarsystemid");
    assert_eq!(crim_args, &vec![PyRep::List(vec![]), PyRep::List(vec![]), PyRep::List(vec![PyRep::Integer(PILOT_A)])]);

    // Advance 65s -> weapons timer expires (state 100, None)
    ctx.advance_secs(65);
    let exp_a = drain_all(&ctx, PILOT_A);
    let (_, _, weap_exp_args) = find_notif(&exp_a, "OnWeaponsTimerUpdate").expect("OnWeaponsTimerUpdate expiry emitted");
    assert_eq!(weap_exp_args, &vec![PyRep::Integer(100), PyRep::None]);

    // Zarzakh aggression -> OnSystemDisapprovalFlagUpdates
    let (z_ship_a, z_ship_b, z_gun) = (640_000_101_i64, 640_000_102_i64, 640_000_110_i64);
    let mut z_sess_a = login_in_space(&ctx, 92_003, "demo", "demo-secret", PILOT_A, z_ship_a, 587, SYS_ZARZAKH, DVec3::ZERO).await;
    let _z_sess_b = login_in_space(&ctx, 92_004, "other", "other-secret", PILOT_B, z_ship_b, 587, SYS_ZARZAKH, DVec3::new(1_000.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(z_gun, make_item(z_gun, 484, PILOT_A, z_ship_a, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(z_gun, true);
    });
    ctx.invalidate_ship_fitting(z_ship_a);
    let z_sys_h = ctx.systems.get_or_spawn(SYS_ZARZAKH);
    z_sys_h.add_target_lock(z_ship_a, TargetLock { target_id: z_ship_b, lock_time_ms: 100.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await.unwrap();

    assert!(!has_notif(&drain_all(&ctx, PILOT_B), "OnSystemDisapprovalFlagUpdates"));
    rpc(&ctx, &mut z_sess_a, "dogmaIM", "Activate", &[PyRep::Integer(z_gun), PyRep::String("projectileFired".into()), PyRep::Integer(z_ship_b)]).await.unwrap();
    let z_post_b = drain_all(&ctx, PILOT_B);
    let (_, dis_id_type, dis_args) = find_notif(&z_post_b, "OnSystemDisapprovalFlagUpdates").expect("OnSystemDisapprovalFlagUpdates emitted");
    assert_eq!(dis_id_type, "solarsystemid");
    assert_eq!(dis_args, &vec![PyRep::List(vec![]), PyRep::List(vec![PyRep::Integer(PILOT_A)])]);
}

#[tokio::test]
async fn test_batch4_ship_death_and_probe_scan_lifecycle() {
    eve_testkit::attest(&[
    ]);

    let ctx = new_ctx();
    let jita_pos = ctx.sde.stations[&JITA_STATION].position + DVec3::new(20_000.0, 0.0, 0.0);
    let (ship_id, launcher_id, probe_stack_id) = (640_000_201_i64, 640_000_210_i64, 640_000_211_i64);
    let mut sess = login_in_space(&ctx, 92_010, "demo", "demo-secret", PILOT_A, ship_id, 621, SYS_JITA, jita_pos).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(launcher_id, make_item(launcher_id, 17_901, PILOT_A, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(launcher_id, true);
        snap.items.insert(probe_stack_id, make_item(probe_stack_id, 30_013, PILOT_A, ship_id, InventoryFlag::HiSlot0, 8, false));
    });
    ctx.invalidate_ship_fitting(ship_id);

    rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())]).await.unwrap();
    let _ = drain_all(&ctx, PILOT_A);

    let coords = PyRep::List((0..8).map(|i| PyRep::List(vec![PyRep::Real(500_000.0 + (i as f64) * 500.0), PyRep::Real(2_000.0), PyRep::Real(0.0), PyRep::Integer(2)])).collect());
    rpc(&ctx, &mut sess, "scanMgr", "RequestScans", &[coords]).await.unwrap();

    let immediate = drain_all(&ctx, PILOT_A);
    let (_, scan_start_id_type, scan_start_args) = find_notif(&immediate, "OnSystemScanStarted").expect("OnSystemScanStarted emitted");
    assert_eq!(scan_start_id_type, "charid");
    assert_eq!(scan_start_args.len(), 3);
    assert!(scan_start_args[1].as_i64().unwrap_or(0) > 4_000);
    assert!(!has_notif(&immediate, "OnSystemScanStopped"));

    ctx.advance_secs(15);
    let after_scan = drain_all(&ctx, PILOT_A);
    let (_, scan_stop_id_type, scan_stop_args) = find_notif(&after_scan, "OnSystemScanStopped").expect("OnSystemScanStopped emitted");
    assert_eq!(scan_stop_id_type, "charid");
    assert_eq!(scan_stop_args.len(), 3);

    // Negative: OnShipDeath not emitted before self-destruct
    assert!(!has_notif(&drain_all(&ctx, PILOT_A), "OnShipDeath"));

    // Positive: ship.SelfDestruct + AdvanceClock(125) destroys ship and emits OnShipDeath()
    sess.role |= ROLE_GM_DEFAULT;
    rpc(&ctx, &mut sess, "ship", "SelfDestruct", &[]).await.unwrap();
    rpc(&ctx, &mut sess, "timeControl", "AdvanceClock", &[PyRep::Integer(125)]).await.unwrap();
    let death_notifs = drain_all(&ctx, PILOT_A);
    let (_, death_id_type, death_args) = find_notif(&death_notifs, "OnShipDeath").expect("OnShipDeath emitted");
    assert_eq!(death_id_type, "charid");
    assert!(death_args.is_empty());
}

#[tokio::test]
async fn test_batch4_fw_lp_vp_and_insurgency_corruption_security() {
    let ctx = new_ctx();
    let fw_sys: u32 = 30_002_537; // Amamake
    let caldari_militia_corp: i64 = 1_000_180;
    let mut sess_a = SessionState::new(92_020);
    common::login_pilot(&ctx, &mut sess_a, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess_a, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)]).await.unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = fw_sys;
            ch.war_faction_id = Some(500_001);
            ch.loyalty_points.insert(caldari_militia_corp, 5_000);
        }
        if let Some(occ) = snap.fw_system_occupations.get_mut(&fw_sys) {
            occ.owner_faction_id = 500_001;
            occ.occupier_faction_id = 500_001;
            occ.victory_points = 1_000;
            occ.victory_points_threshold = 3_000;
        }
    });
    sess_a.war_faction_id = Some(500_001);
    sess_a.solar_system_id = Some(fw_sys);
    let _ = drain_all(&ctx, PILOT_A);

    // Negative: insufficient LP fails without emitting OnSolarSystemLPChange
    assert!(rpc(&ctx, &mut sess_a, "facWarMgr", "DonateLPsToSolarSystem", &[PyRep::Integer(i64::from(fw_sys)), PyRep::Integer(99_999)]).await.is_err());
    assert!(!has_notif(&drain_all(&ctx, PILOT_A), "OnSolarSystemLPChange"));

    // Positive: valid LP donation emits OnSolarSystemLPChange(0, 1500)
    rpc(&ctx, &mut sess_a, "facWarMgr", "DonateLPsToSolarSystem", &[PyRep::Integer(i64::from(fw_sys)), PyRep::Integer(1_500)]).await.unwrap();
    let lp_notifs = drain_all(&ctx, PILOT_A);
    let (_, lp_id_type, lp_args) = find_notif(&lp_notifs, "OnSolarSystemLPChange").expect("OnSolarSystemLPChange emitted");
    assert_eq!(lp_id_type, "solarsystemid");
    assert_eq!(lp_args, &vec![PyRep::Integer(0), PyRep::Integer(1_500)]);

    // OnVictoryPointStateUpdated via live FW complex capture + Insurgency Stage 5 corruption
    let fw_ctx = new_ctx();
    crate::common::pin_clock(&fw_ctx);
    let (_enemy_sys, warzone_sys, _cmd_sys, _rear_sys) = crate::fw_warzone_insurgency::setup_warzone_chain(&fw_ctx);
    let fw_ship = 640_000_309_i64;
    fw_ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.corporation_id = 1_000_181;
            ch.station_id = None;
            ch.solar_system_id = warzone_sys;
        }
        for (i, flg) in [InventoryFlag::HiSlot0, InventoryFlag::HiSlot1, InventoryFlag::HiSlot2].into_iter().enumerate() {
            let gid = 640_000_320 + (i as i64) * 2;
            snap.items.insert(gid, make_item(gid, 3074, PILOT_A, fw_ship, flg, 1, true));
            snap.items.insert(gid + 1, make_item(gid + 1, 12_625, PILOT_A, fw_ship, flg, 500, false));
        }
        for &(tid, lvl) in &[(3300_u32, 5), (3301_u32, 5), (3311_u32, 5), (11_082_u32, 5)] {
            snap.skills.entry(PILOT_A).or_default().insert(tid, TrainedSkillRecord { type_id: tid, level: lvl, skill_points: 256_000.0 });
        }
    });
    let mut sess_fw = login_in_space(&fw_ctx, 92_021, "demo", "demo-secret", PILOT_A, fw_ship, 587, warzone_sys, DVec3::ZERO).await;
    sess_fw.role |= ROLE_GM_DEFAULT;
    fw_ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&PILOT_A).unwrap().station_id = Some(60_003_760); });
    rpc(&fw_ctx, &mut sess_fw, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_004)]).await.unwrap();
    fw_ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&PILOT_A).unwrap().station_id = None; });
    sess_fw.station_id = None;
    let wz_beyonce = fw_ctx.bind_object(&sess_fw, "beyonce", i64::from(warzone_sys)).unwrap();
    let _ = rpc(&fw_ctx, &mut sess_fw, "fwWarzoneSolarsystem", "GetLocalOccupationState", &[]).await.unwrap();
    let _ = rpc(&fw_ctx, &mut sess_fw, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();

    let novice_gate = fw_ctx.db.with_snapshot(|s| {
        s.fw_runtime.fw_complexes.iter().find_map(|(&gid, &(csys, ref tier, _, _, done))| {
            (csys == warzone_sys && tier == "Novice" && !done).then_some(gid)
        }).expect("Novice complex")
    });
    let wz_space = fw_ctx.systems.get_or_spawn(warzone_sys);
    fw_ctx.dispatch_rpc(&mut sess_fw, "beyonce", Some(wz_beyonce), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(novice_gate), PyRep::Real(0.0)], &HashMap::new(), None).await.unwrap();
    for _ in 0..4 {
        wz_space.step_manual(15.0).await.unwrap();
    }
    let _ = rpc(&fw_ctx, &mut sess_fw, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(novice_gate)]).await.unwrap();
    let guard_id = 500_000_000 + novice_gate;
    rpc(&fw_ctx, &mut sess_fw, "dogmaIM", "AddTarget", &[PyRep::Integer(guard_id)]).await.unwrap();
    wz_space.step_manual(5.0).await.unwrap();
    for gid in [640_000_320_i64, 640_000_322, 640_000_324] {
        rpc(&fw_ctx, &mut sess_fw, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::String("projectileFired".into()), PyRep::Integer(guard_id), PyRep::Integer(1)]).await.unwrap();
    }
    for _ in 0..90 {
        if wz_space.get_ball_snapshot(guard_id).await.unwrap().is_none() { break; }
        wz_space.step_manual(1.0).await.unwrap();
    }
    let _ = drain_all(&fw_ctx, PILOT_A);
    eve_server::services::fw_gameplay::tick_fw_gameplay(&fw_ctx, fw_ctx.now_unix()).await;
    for _ in 0..6 {
        rpc(&fw_ctx, &mut sess_fw, "timeControl", "AdvanceClock", &[PyRep::Integer(150)]).await.unwrap();
        eve_server::services::fw_gameplay::tick_fw_gameplay(&fw_ctx, fw_ctx.now_unix()).await;
    }
    let vp_notifs = drain_all(&fw_ctx, PILOT_A);
    let (_, vp_id_type, vp_args) = find_notif(&vp_notifs, "OnVictoryPointStateUpdated").expect("OnVictoryPointStateUpdated emitted");
    assert_eq!(vp_id_type, "solarsystemid");
    assert_eq!(vp_args[0], PyRep::Integer(i64::from(warzone_sys)));
    assert!(dict_get(&vp_args[1], "victoryPoints").and_then(PyRep::as_i64).unwrap_or(0) >= 20);

    // Insurgency Stage 5 corruption -> OnSystemFullyCorruptedForFaction & OnSecurityModified
    let (camp_id, affected_sys, pirate_fac) = (1_i64, warzone_sys, 500_010_i64);
    init_insurgency_campaign(&fw_ctx, camp_id, pirate_fac, 500_001, affected_sys, vec![affected_sys]);
    fw_ctx.clock().advance_secs(FORECASTING_DURATION_SECS + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&fw_ctx).await;
    let base_sec = fw_ctx.sde.solar_systems.get(&affected_sys).map(|s| s.security_status).unwrap_or(0.0);
    let _ = drain_all(&fw_ctx, PILOT_A);

    let (_pts, stage) = add_system_corruption(&fw_ctx, camp_id, affected_sys, 1_000);
    assert_eq!(stage, 5);
    let corr_notifs = drain_all(&fw_ctx, PILOT_A);
    let (_, corr_id_type, corr_args) = find_notif(&corr_notifs, "OnSystemFullyCorruptedForFaction").expect("OnSystemFullyCorruptedForFaction emitted");
    assert_eq!(corr_id_type, "solarsystemid");
    assert_eq!(corr_args[0], PyRep::Integer(i64::from(affected_sys)));
    assert_eq!(corr_args[1], PyRep::Integer(pirate_fac));

    let (_, sec_id_type, sec_args) = find_notif(&corr_notifs, "OnSecurityModified").expect("OnSecurityModified emitted");
    assert_eq!(sec_id_type, "solarsystemid");
    assert_eq!(sec_args, &vec![PyRep::Integer(i64::from(affected_sys)), PyRep::Real(-base_sec), PyRep::Real(0.0)]);
}

#[tokio::test]
async fn test_batch4_interference_incursion_tale_remove_and_tourney_ban() {
    let ctx = new_ctx();
    let mut sess = SessionState::new(92_030);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)]).await.unwrap();
    let mut sess_b = SessionState::new(92_031);
    common::login_pilot(&ctx, &mut sess_b, "other", "other-secret").await;
    rpc(&ctx, &mut sess_b, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_B)]).await.unwrap();

    // 1. OnSolarsystemInterferenceChanged
    let _ = drain_all(&ctx, PILOT_A);
    let int_low = rpc(&ctx, &mut sess, "solarsystemInterferenceMgr", "GetLocalInterferenceState", &[PyRep::Integer(i64::from(SYS_JITA))]).await.unwrap();
    assert_eq!(int_low.dict_get("interferenceBand").and_then(PyRep::as_i64), Some(1));
    assert!(!has_notif(&drain_all(&ctx, PILOT_A), "OnSolarsystemInterferenceChanged"));

    record_system_interference_activity(&ctx, &mut sess, SYS_JITA, 0.75);
    let int_notifs = drain_all(&ctx, PILOT_A);
    let (_, int_id_type, int_args) = find_notif(&int_notifs, "OnSolarsystemInterferenceChanged").expect("OnSolarsystemInterferenceChanged emitted");
    assert_eq!(int_id_type, "solarsystemid");
    assert_eq!(int_args.len(), 2);
    assert_eq!(int_args[0], PyRep::Integer(i64::from(SYS_JITA)));
    assert_eq!(int_args[1].dict_get("interferenceBand").and_then(PyRep::as_i64), Some(3));

    // 2. OnTaleRemove when incursion withdrawal expires
    rpc(&ctx, &mut sess, "taleMgr", "GetGlobalWorldEventTales", &[]).await.unwrap();
    let (inc_cid, tale_id, staging_sys) = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        let inc = reg.incursions.values().find(|i| i.state != "Ended").expect("seeded incursion");
        (inc.constellation_id, inc.tale_id, inc.staging_solar_system_id)
    });
    let _ = ctx.db.set_character_solar_system(PILOT_A, staging_sys, &ctx.sde);
    sess.solar_system_id = Some(staging_sys);
    ctx.sync_session(&mut sess);
    let _ = drain_all(&ctx, PILOT_A);

    let now_u = ctx.now_unix();
    eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        if let Some(inc) = reg.incursions.get_mut(&inc_cid) { inc.state = "Withdrawing".into(); }
        if let Some(meta) = reg.incursion_lifecycle.get_mut(&inc_cid) { meta.withdraw_until_sec = Some(now_u + 30); }
    });
    ctx.advance_secs(5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(!has_notif(&drain_all(&ctx, PILOT_A), "OnTaleRemove"));

    ctx.advance_secs(35);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let tale_notifs = drain_all(&ctx, PILOT_A);
    let (_, tale_id_type, tale_args) = find_notif(&tale_notifs, "OnTaleRemove").expect("OnTaleRemove emitted");
    assert_eq!(tale_id_type, "solarsystemid");
    assert_eq!(tale_args, &vec![PyRep::Integer(tale_id), PyRep::Integer(1), PyRep::Integer(eve_server::services::pve_incursion_sites::INCURSION_TALE_TEMPLATE_ID as i64)]);

    // 3. OnTournamentPerformBan via tourneyMgr.StartAutobanning
    sess.role = ROLE_GMH;
    rpc(&ctx, &mut sess, "tourneyMgr", "CreateMatch", &[PyRep::Integer(1), PyRep::Integer(2001), PyRep::Integer(100), PyRep::Integer(600), PyRep::Integer(0), PyRep::Integer(0)]).await.unwrap();
    let _ = drain_all(&ctx, PILOT_A);
    let _ = drain_all(&ctx, PILOT_B);

    let mut autoban_kw = HashMap::new();
    autoban_kw.insert("totalNumBans".to_string(), PyRep::Integer(4));
    autoban_kw.insert("redCharID".to_string(), PyRep::Integer(PILOT_A));
    autoban_kw.insert("blueCharID".to_string(), PyRep::Integer(PILOT_B));
    ctx.dispatch_rpc(&mut sess, "tourneyMgr", None, "StartAutobanning", &[PyRep::Integer(2001)], &autoban_kw, None).await.unwrap();

    let ban_notifs_b = drain_all(&ctx, PILOT_B);
    let (_, ban_id_type, ban_args) = find_notif(&ban_notifs_b, "OnTournamentPerformBan").expect("OnTournamentPerformBan emitted");
    assert_eq!(ban_id_type, "charid");
    assert_eq!(ban_args.len(), 6);
    assert_eq!(ban_args[0], PyRep::Integer(2001));
    assert_eq!(ban_args[1], PyRep::Integer(4));
}
}

