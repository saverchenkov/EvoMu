//! Live end-to-end integration tests for cluster load management, season schedule,
//! Zarzakh gravity hazard zones, progression/dungeon/incursion/insurgency notifications,
//! corporation/structure ESI endpoints, and Quasar payment schedules, tokens, and plans.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{corp_roles::{ROLE_DIRECTOR, ROLE_STATION_MANAGER}, service_roles::ROLE_GM_DEFAULT, CorpProjectRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderMeta, MarketOrderRecord, StarbaseDetailSettings, TrainedSkillRecord, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field, DynamicValue, EvePublicNotice, EvePublicRequest, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry, WireMessage, TYPE_GOOGLEAPIS_PREFIX};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::quasar_gateway::{STATUS_NOT_FOUND, STATUS_OK, STATUS_UNAUTHORIZED};
use eve_server::router::ServerContext;
use eve_server::services::quasar_payment_store_types::{register_user_payment_token, PaymentTokenRecord};
use eve_server::system_actor::SystemHandle;
use glam::DVec3;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;
const SYS_ZARZAKH: u32 = 30_100_000;
const ZARZAKH_FULCRUM_STATION: i64 = 60_015_187;

struct TempSqlitePath(PathBuf);
impl TempSqlitePath {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("evomu_live_{tag}_{}.sqlite", std::process::id()));
        Self::cleanup(&p);
        Self(p)
    }
    fn cleanup(p: &PathBuf) { for ext in ["", "-wal", "-shm"] { let _ = std::fs::remove_file(format!("{}{ext}", p.display())); } }
}
impl Drop for TempSqlitePath { fn drop(&mut self) { Self::cleanup(&self.0); } }
async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> { ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await }

fn quasar_call(ctx: &ServerContext, sess: &SessionState, type_name: &str, value: Vec<u8>) -> eve_proto::quasar::EvePublicResponse {
    let now = ctx.now_unix();
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: now as i64, nanos: 0 }),
        correlation_uuid: b"corr-live-suite-01".to_vec(),
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-live-suite-01".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    ctx.quasar_gateway.handle_public_request(sess, &req, None, false, now)
}

fn id_msg(field_num: u32, id: u64) -> Vec<u8> {
    let (mut inner, mut out) = (Vec::new(), Vec::new());
    encode_varint_field(1, id, &mut inner);
    encode_bytes_field(field_num, &inner, &mut out);
    out
}

fn drain_sub(rx: &mut mpsc::Receiver<EvePublicNotice>) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    while let Ok(env) = rx.try_recv() {
        if let Some(p) = env.payload { out.push((p.normalized_type_name().to_string(), p.value)); }
    }
    out
}

fn decode_nested_u64(raw: &[u8], outer_field: u32, inner_field: u32) -> Option<u64> {
    let wm = WireMessage::decode(raw).ok()?;
    let inner = wm.fields.iter().find(|f| f.field_number == outer_field)?.value.as_bytes()?;
    WireMessage::decode(inner).ok()?.fields.iter().find(|f| f.field_number == inner_field)?.value.as_varint()
}

fn fit_turret_and_skills(ctx: &ServerContext, char_id: i64, ship_id: i64, gun_id: i64, turret_tid: u32, ammo_tid: u32, gun_count: usize) {
    let hi = [InventoryFlag::HiSlot0, InventoryFlag::HiSlot1, InventoryFlag::HiSlot2, InventoryFlag::HiSlot3, InventoryFlag::HiSlot4, InventoryFlag::HiSlot5, InventoryFlag::HiSlot6, InventoryFlag::HiSlot7];
    let mods = [InventoryFlag::LoSlot1, InventoryFlag::LoSlot2, InventoryFlag::LoSlot3, InventoryFlag::LoSlot4, InventoryFlag::LoSlot5, InventoryFlag::LoSlot6, InventoryFlag::MedSlot1, InventoryFlag::MedSlot2, InventoryFlag::MedSlot3];
    ctx.db.with_mut_snapshot(|snap| {
        for (i, flg) in hi.into_iter().take(gun_count).enumerate() {
            let gid = gun_id + (i as i64) * 2;
            snap.items.insert(gid, InventoryItemRecord { item_id: gid, type_id: turret_tid, owner_id: char_id, location_id: ship_id, flag: flg, quantity: 1, singleton: true, dynamic_attributes: HashMap::from([(64, 500_000_000.0)]), ..Default::default() });
            snap.items.insert(gid + 1, InventoryItemRecord { item_id: gid + 1, type_id: ammo_tid, owner_id: char_id, location_id: ship_id, flag: flg, quantity: 25_000, singleton: false, ..Default::default() });
            snap.module_online_states.insert(gid, true);
        }
        if gun_count > 1 {
            for (i, flg) in mods.into_iter().enumerate() {
                let id = gun_id + 111 + i as i64;
                snap.items.insert(id, InventoryItemRecord { item_id: id, type_id: if i < 6 { 22_291 } else { 2032 }, owner_id: char_id, location_id: ship_id, flag: flg, quantity: 1, singleton: true, ..Default::default() });
                snap.module_online_states.insert(id, true);
            }
        }
        for tid in [3300_u32, 3301, 3302, 3303, 3307, 3308, 3310, 3311, 3312, 3317, 3319, 3324, 3325, 3331, 3336, 3337, 3413, 3418, 3419, 11_082, 12_209, 12_212, 20_312, 20_327, 20_533, 21_071, 21_668, 25_719] {
            snap.skills.entry(char_id).or_default().insert(tid, TrainedSkillRecord { type_id: tid, level: 5, skill_points: 256_000.0 });
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
}

async fn login_space_pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64, ship_id: i64, ship_type_id: u32, sys_id: u32, pos: DVec3) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("SelectCharacterID");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ship_id, InventoryItemRecord { item_id: ship_id, type_id: ship_type_id, owner_id: char_id, location_id: i64::from(sys_id), flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default() });
        snap.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            (ch.station_id, ch.structure_id, ch.solar_system_id, ch.active_ship_id) = (None, None, sys_id, ship_id);
        }
    });
    (sess.role, sess.solar_system_id, sess.station_id, sess.structure_id, sess.ship_id) = (sess.role | eve_machonet::service_roles::ROLE_GM_DEFAULT, Some(sys_id), None, None, Some(ship_id));
    ctx.invalidate_ship_fitting(ship_id);
    let beyonce = ctx.bind_object(&sess, "beyonce", i64::from(sys_id)).expect("bind beyonce");
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce), "UpdateStateRequest", &[], &HashMap::new(), None).await.expect("UpdateStateRequest");
    let sys = ctx.systems.get_or_spawn(sys_id);
    if let Ok(Some(mut cs)) = sys.get_combat_state(ship_id).await {
        (cs.undock_invulnerable_remaining_ticks, cs.capacitor) = (0, cs.max_capacitor);
        let _ = sys.set_combat_state(ship_id, cs).await;
    }
    ctx.sync_session(&mut sess);
    sess
}

async fn destroy_npc_via_turrets(ctx: &ServerContext, sess: &mut SessionState, space: &SystemHandle, target_id: i64, gun_base_id: i64, gun_count: usize) {
    let Some((_, b)) = space.get_ball_snapshot(target_id).await.ok().flatten() else { return; };
    let (ship_id, eff) = (sess.ship_id.expect("ship_id"), "projectileFired");
    let _ = space.set_ball_position(ship_id, b.position - b.velocity.try_normalize().unwrap_or(DVec3::X) * (b.radius + 2_500.0)).await;
    rpc(ctx, sess, "dogmaIM", "AddTarget", &[PyRep::Integer(target_id)]).await.expect("AddTarget");
    for _ in 0..20 {
        if space.get_target_locks(ship_id).await.is_ok_and(|ls| ls.iter().any(|l| l.target_id == target_id && l.locked)) { break; }
        let _ = space.step_manual(5.0).await;
    }
    let mut last_res = PyRep::None;
    for _ in 0..80 {
        let Some((_, nb)) = space.get_ball_snapshot(target_id).await.ok().flatten() else { return; };
        let _ = space.set_ball_position(ship_id, nb.position - nb.velocity.try_normalize().unwrap_or(DVec3::X) * (nb.radius + 2_500.0)).await;
        for i in 0..gun_count {
            let gid = gun_base_id + (i as i64) * 2;
            let res = match rpc(ctx, sess, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::String(eff.into()), PyRep::Integer(target_id), PyRep::Integer(1)]).await {
                Ok(v) => Some(v),
                Err(e) if e.code == "ModuleAlreadyActive" => {
                    let _ = rpc(ctx, sess, "dogmaIM", "Deactivate", &[PyRep::Integer(gid), PyRep::String(eff.into())]).await;
                    rpc(ctx, sess, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::String(eff.into()), PyRep::Integer(target_id), PyRep::Integer(1)]).await.ok()
                }
                Err(e) if e.code == "NotEnoughCapacitor" => { let _ = rpc(ctx, sess, "slash", "SlashCmd", &[PyRep::String("/heal".into())]).await; None }
                Err(e) => panic!("Activate gun {i} on {target_id}: {e:?}"),
            };
            if let Some(v) = res {
                let dead = v.dict_get("destroyed") == Some(&PyRep::Bool(true));
                last_res = v;
                if dead { return; }
            }
        }
        let _ = space.step_manual(7.0).await;
        ctx.clock().advance_secs(7);
    }
    let cs = space.get_combat_state(target_id).await.ok().flatten();
    assert!(space.get_ball_snapshot(target_id).await.ok().flatten().is_none(), "NPC {target_id} destroyed (last={last_res:?}, cs={cs:?})");
}

// parity: rpc:loadService.GetTotalStats
// parity: rpc:loadService.SetConfig
// parity: rpc:loadService.StartLoad
// parity: rpc:loadService.StopLoad
#[tokio::test]
async fn test_load_service_config_stepping_stats_and_season_end_time_live() {
    eve_testkit::attest(&[
        "rpc:loadService.GetTotalStats",
        "rpc:loadService.SetConfig",
        "rpc:loadService.StartLoad",
        "rpc:loadService.StopLoad",
    ]);

    let db_tmp = TempSqlitePath::new("load_season");
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    common::pin_clock(&ctx);
    ctx.db.provision_gm_account("gm_load", "gm-pass", 90_000_010, "GM Load");
    let mut player = SessionState::new(10_001);
    common::login_pilot(&ctx, &mut player, "demo", "demo-secret").await;
    rpc(&ctx, &mut player, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)]).await.expect("SelectCharacterID");
    let mut gm = SessionState::new(10_002);
    common::login_pilot(&ctx, &mut gm, "gm_load", "gm-pass").await;
    rpc(&ctx, &mut gm, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_010)]).await.expect("SelectCharacterID GM");
    let unauth_err = rpc(&ctx, &mut player, "loadService", "StartLoad", &[PyRep::Integer(3)]).await.expect_err("player StartLoad rejected");
    assert_eq!((unauth_err.code.as_str(), unauth_err.message.as_str()), ("UserError", "NotAuthorized"));
    let bad_cfg = rpc(&ctx, &mut gm, "loadService", "SetConfig", &[PyRep::keyval([("ticks", PyRep::Integer(0))])]).await.expect_err("zero ticks rejected");
    assert_eq!((bad_cfg.code.as_str(), bad_cfg.message.as_str()), ("UserError", "InvalidConfig"));
    assert_eq!(rpc(&ctx, &mut gm, "loadService", "SetConfig", &[PyRep::keyval([("ticks", PyRep::Integer(4)), ("solar_system_id", PyRep::Integer(i64::from(SYS_JITA)))])]).await.expect("SetConfig"), PyRep::None);
    let pre = rpc(&ctx, &mut gm, "loadService", "GetTotalStats", &[]).await.expect("GetTotalStats initial");
    assert_eq!((pre.dict_get("active"), pre.dict_get("runs_started").and_then(PyRep::as_i64)), (Some(&PyRep::Bool(false)), Some(0)));
    rpc(&ctx, &mut gm, "loadService", "StartLoad", &[]).await.expect("StartLoad defaults");
    rpc(&ctx, &mut gm, "loadService", "StartLoad", &[PyRep::Dict(vec![(PyRep::String("ticks".into()), PyRep::Integer(6)), (PyRep::String("solar_system_id".into()), PyRep::Integer(i64::from(SYS_JITA)))])]).await.expect("StartLoad Dict");
    let mid = rpc(&ctx, &mut gm, "loadService", "GetTotalStats", &[]).await.expect("GetTotalStats mid");
    assert_eq!((mid.dict_get("active"), mid.dict_get("runs_started").and_then(PyRep::as_i64), mid.dict_get("systems_stepped").and_then(PyRep::as_i64), mid.dict_get("ticks_executed").and_then(PyRep::as_i64)), (Some(&PyRep::Bool(true)), Some(2), Some(2), Some(10)));
    rpc(&ctx, &mut gm, "loadService", "StopLoad", &[]).await.expect("StopLoad");
    ctx.db.save_to_sqlite(&db_tmp.0).expect("save_to_sqlite");
    drop(ctx);
    let reloaded = ServerContext::from_sqlite(&db_tmp.0).expect("from_sqlite");
    common::pin_clock(&reloaded);
    reloaded.clock().advance_secs(3600);
    let mut gm2 = SessionState::new(10_003);
    common::login_pilot(&reloaded, &mut gm2, "gm_load", "gm-pass").await;
    rpc(&reloaded, &mut gm2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_010)]).await.expect("SelectCharacterID GM2");
    let post = rpc(&reloaded, &mut gm2, "loadService", "GetTotalStats", &[]).await.expect("GetTotalStats persisted");
    assert_eq!((post.dict_get("active"), post.dict_get("runs_started").and_then(PyRep::as_i64), post.dict_get("ticks_executed").and_then(PyRep::as_i64)), (Some(&PyRep::Bool(false)), Some(2), Some(10)));
    let to_ft = |unix_s: u64| ((unix_s as i64) + 11_644_473_600) * 10_000_000;
    let expected_ft = rpc(&reloaded, &mut gm2, "seasonManager", "get_season_end_time", &[]).await.expect("get_season_end_time").as_i64().expect("season end FILETIME");
    assert!(expected_ft > to_ft(reloaded.now_unix()), "active FSD season end must be in the future");
    reloaded.clock().advance_secs(3_600);
    assert_eq!(rpc(&reloaded, &mut gm2, "seasonManager", "get_season_end_time", &[]).await.expect("stable season end"), PyRep::Integer(expected_ft));
    reloaded.clock().advance_secs(30 * 86_400);
    let next_ft = expected_ft + 30 * 86_400 * 10_000_000;
    assert!(next_ft > expected_ft);
    assert_eq!(rpc(&reloaded, &mut gm2, "seasonManager", "get_season_end_time", &[]).await.expect("next season cadence"), PyRep::Integer(next_ft));
}

// parity: feat:navigation.gravity-hazard-safe-zones
#[tokio::test]
async fn test_zarzakh_gravity_hazard_safe_zones_and_killmail_pipeline_live() {
    eve_testkit::attest(&["feat:navigation.gravity-hazard-safe-zones"]);

    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    common::pin_clock(&ctx);

    let fulcrum_pos = ctx.sde.stations.get(&ZARZAKH_FULCRUM_STATION).map(|s| s.position).unwrap_or(DVec3::ZERO);
    let ship_id = 810_100_001_i64;
    let _sess = login_space_pilot(&ctx, 11_001, "demo", "demo-secret", PILOT_A, ship_id, 587, SYS_ZARZAKH, fulcrum_pos + DVec3::new(5_000.0, 0.0, 0.0)).await;
    let zarzakh_space = ctx.systems.get_or_spawn(SYS_ZARZAKH);
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    let safe_before = zarzakh_space.get_combat_state(ship_id).await.expect("safe before").expect("combat state");
    for _ in 0..20 { let _ = zarzakh_space.try_step_full_tick(1.0); tokio::task::yield_now().await; }
    let safe_after = zarzakh_space.get_combat_state(ship_id).await.expect("safe after").expect("combat state");
    assert_eq!(safe_after.structure_hp, safe_before.structure_hp, "zero hull damage inside Zarzakh safe zone");

    assert!(zarzakh_space.set_ball_position(ship_id, DVec3::new(500_000_000.0, 0.0, 0.0)).await);
    let haz_before = zarzakh_space.get_combat_state(ship_id).await.expect("haz before").expect("combat state");
    for _ in 0..3 { let _ = zarzakh_space.try_step_full_tick(1.0); tokio::task::yield_now().await; }
    let haz_after = zarzakh_space.get_combat_state(ship_id).await.expect("haz after").expect("combat state");
    assert_eq!(haz_after.shield_hp, haz_before.shield_hp);
    assert_eq!(haz_after.armor_hp, haz_before.armor_hp);
    assert!((haz_before.structure_hp - haz_after.structure_hp - 150.0).abs() < 1e-2);

    for _ in 0..40 {
        if ctx.db.with_snapshot(|s| !s.items.contains_key(&ship_id)) { break; }
        let _ = zarzakh_space.try_step_full_tick(1.0);
        tokio::task::yield_now().await;
    }
    let (ship_gone, has_capsule, has_killmail) = ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&PILOT_A];
        (!snap.items.contains_key(&ship_id), snap.items.get(&ch.active_ship_id).is_some_and(|it| it.type_id == 670),
         snap.killmails.values().any(|k| k.victim_character_id == PILOT_A && k.solar_system_id == SYS_ZARZAKH && k.ship_type_id == 587))
    });
    assert!(ship_gone && has_capsule && has_killmail);
}

#[tokio::test]
async fn test_analysis_kredits_and_expedition_dungeon_instance_notifications_live() {
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    common::pin_clock(&ctx);

    let (ship_a, gun_a) = (810_200_001_i64, 810_200_010_i64);
    fit_turret_and_skills(&ctx, PILOT_A, ship_a, gun_a, 3074, 12_625, 3);
    let mut sess_a = login_space_pilot(&ctx, 12_001, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, DVec3::ZERO).await;

    assert_eq!(rpc(&ctx, &mut sess_a, "ProjectDiscovery", "get_player_analysis_kredits", &[]).await.expect("init AK"), PyRep::Integer(0));
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    let bad_err = rpc(&ctx, &mut sess_a, "ProjectDiscovery", "post_classification", &[PyRep::Integer(1), PyRep::String("".into())]).await.unwrap_err();
    assert_eq!(bad_err.code.as_str(), ["Invalid", "Classification"].concat());

    let t1 = rpc(&ctx, &mut sess_a, "ProjectDiscovery", "get_new_task", &[]).await.expect("get_new_task");
    assert_eq!(t1.dict_get("task_id").and_then(PyRep::as_str), Some("task_1"));
    rpc(&ctx, &mut sess_a, "ProjectDiscovery", "post_classification", &[
        PyRep::String("task_1".into()),
        PyRep::List(vec![PyRep::Real(1.0)]),
    ]).await.expect("post_classification");
    let ak_ev = ctx.drain_notifications_for_char(PILOT_A).into_iter().find(|n| n.event == "OnAnalysisKreditsChange").expect("OnAnalysisKreditsChange");
    assert!(ak_ev.args.is_empty());
    assert_eq!(rpc(&ctx, &mut sess_a, "ProjectDiscovery", "get_player_analysis_kredits", &[]).await.expect("updated AK"), PyRep::Integer(20));

    let dest_sys = eve_server::services::pve_expeditions::find_escalation_destination_system(&ctx, SYS_JITA);
    let mut sess_obs = login_space_pilot(&ctx, 12_002, "other", "other-secret", PILOT_B, 810_200_002, 587, dest_sys, DVec3::ZERO).await;
    rpc(&ctx, &mut sess_obs, "scanSvc", "GetCombatAnomalyInstances", &[]).await.expect("obs scan");
    let _ = ctx.drain_notifications_for_char(PILOT_B);

    let anoms = rpc(&ctx, &mut sess_a, "scanSvc", "GetCombatAnomalyInstances", &[]).await.expect("jita anoms");
    let first_site_id = match anoms {
        PyRep::List(items) => items.into_iter().find_map(|it| it.dict_get("siteID").and_then(PyRep::as_str).map(str::to_string)).expect("siteID"),
        _ => panic!("expected anomaly list"),
    };
    rpc(&ctx, &mut sess_a, "scanSvc", "EnterCombatAnomaly", &[PyRep::String(first_site_id.clone())]).await.expect("EnterCombatAnomaly");

    ctx.set_mechanic_rng_seed(Some(3));
    let jita_space = ctx.systems.get_or_spawn(SYS_JITA);
    for _ in 0..6 {
        let npcs: Vec<i64> = match rpc(&ctx, &mut sess_a, "scanSvc", "GetCombatAnomalyInstances", &[]).await.expect("poll") {
            PyRep::List(items) => items.into_iter().find(|it| it.dict_get("siteID").and_then(PyRep::as_str) == Some(first_site_id.as_str()))
                .and_then(|it| match it.dict_get("activeNpcBallIDs") { Some(PyRep::List(ids)) => Some(ids.iter().filter_map(PyRep::as_i64).collect()), _ => None }).unwrap_or_default(),
            _ => Vec::new(),
        };
        if npcs.is_empty() { break; }
        for nid in npcs { destroy_npc_via_turrets(&ctx, &mut sess_a, &jita_space, nid, gun_a, 3).await; }
    }

    let added = ctx.drain_notifications_for_char(PILOT_B).into_iter().find(|n| n.event == "OnDungeonInstanceAdded").expect("OnDungeonInstanceAdded");
    assert_eq!((added.id_type.as_str(), &added.args[0]), ("charid", &PyRep::Integer(i64::from(dest_sys))));
    let inst_id = added.args[1].dict_get("instanceID").and_then(PyRep::as_i64).expect("instanceID");
    let exp_sig = added.args[1].dict_get("signatureID").and_then(PyRep::as_str).expect("signatureID").to_string();
    let exp_did = added.args[1].dict_get("dungeonID").and_then(PyRep::as_i64).expect("dungeonID");

    fit_turret_and_skills(&ctx, PILOT_A, 810_200_003, gun_a + 100, 3074, 12_625, 3);
    let mut sess_a_dest = login_space_pilot(&ctx, 12_003, "demo", "demo-secret", PILOT_A, 810_200_003, 587, dest_sys, DVec3::ZERO).await;
    let exp_site_id = format!("EXP-{inst_id}-1");
    let dest_space = ctx.systems.get_or_spawn(dest_sys);
    let _ = ctx.drain_notifications_for_char(PILOT_B);

    for _ in 0..6 {
        let npcs: Vec<i64> = match rpc(&ctx, &mut sess_a_dest, "scanSvc", "GetCombatAnomalyInstances", &[]).await.expect("poll exp") {
            PyRep::List(items) => items.into_iter().find(|it| it.dict_get("siteID").and_then(PyRep::as_str) == Some(exp_site_id.as_str()))
                .and_then(|it| match it.dict_get("activeNpcBallIDs") { Some(PyRep::List(ids)) => Some(ids.iter().filter_map(PyRep::as_i64).collect()), _ => None }).unwrap_or_default(),
            _ => Vec::new(),
        };
        if npcs.is_empty() { break; }
        for nid in npcs { destroy_npc_via_turrets(&ctx, &mut sess_a_dest, &dest_space, nid, gun_a + 100, 3).await; }
    }

    let removed = ctx.drain_notifications_for_char(PILOT_B).into_iter().find(|n| n.event == "OnDungeonInstanceRemoved").expect("OnDungeonInstanceRemoved");
    assert_eq!(removed.args, vec![
        PyRep::Integer(i64::from(dest_sys)), PyRep::Integer(inst_id), PyRep::String(exp_sig), PyRep::Integer(exp_did),
    ]);
}

// parity: notify:OnSecurityModified
#[tokio::test]
async fn test_incursion_tale_influence_remove_and_insurgency_security_notifications_live() {
    eve_testkit::attest(&["notify:OnSecurityModified"]);

    let inc_ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    common::pin_clock(&inc_ctx);
    let (inc_ship, inc_gun) = (810_300_001_i64, 810_300_010_i64);
    fit_turret_and_skills(&inc_ctx, PILOT_A, inc_ship, inc_gun, 34_288, 20_783, 2);
    let mut sess_inc = login_space_pilot(&inc_ctx, 13_001, "demo", "demo-secret", PILOT_A, inc_ship, 26_842, SYS_JITA, DVec3::ZERO).await;
    sess_inc.role |= ROLE_GM_DEFAULT;
    rpc(&inc_ctx, &mut sess_inc, "taleMgr", "GetGlobalWorldEventTales", &[]).await.expect("GetGlobalWorldEventTales");

    let (inc_cid, tale_id, hq_sys) = eve_server::services::pve_special_sites::with_pve_special_reg(&inc_ctx, |reg| {
        let inc = reg.incursions.values().find(|i| i.state != "Ended").expect("seeded incursion");
        (inc.constellation_id, inc.tale_id, inc.hq_solar_system_id)
    });

    let mut influence_seen = false;
    let mut obs_holder = None;
    for step in 0..16 {
        let next_target = eve_server::services::pve_special_sites::with_pve_special_reg(&inc_ctx, |reg| {
            let inc = reg.incursions.get(&inc_cid)?;
            if inc.state == "Withdrawing" || inc.state == "Ended" { return None; }
            inc.sites.iter().find(|s| s.tier == "Mothership" && !inc.mothership_defeated && (!s.locked_until_low_influence || (inc.influence <= 0.0 && inc.mothership_spawned)))
                .or_else(|| inc.sites.iter().filter(|s| !s.locked_until_low_influence).min_by_key(|s| (s.completed_count, s.site_id.clone())))
                .map(|s| (s.solar_system_id, s.site_id.clone()))
        });
        let Some((site_sys, site_id)) = next_target else { break; };

        rpc(&inc_ctx, &mut sess_inc, "slash", "SlashCmd", &[PyRep::String(format!("/tr {site_sys}"))]).await.expect("/tr");
        if step == 0 {
            obs_holder = Some(login_space_pilot(&inc_ctx, 13_002, "other", "other-secret", PILOT_B, 810_300_999, 587, hq_sys, DVec3::ZERO).await);
        }
        let space = inc_ctx.systems.get_or_spawn(site_sys);
        let _ = (inc_ctx.drain_notifications_for_char(PILOT_A), inc_ctx.drain_notifications_for_char(PILOT_B));

        for _ in 0..6 {
            let _ = rpc(&inc_ctx, &mut sess_inc, "keeper", "EnterNextDungeonRoom", &[PyRep::String(site_id.clone())]).await;
            let (escorts, boss_opt) = eve_server::services::pve_special_sites::with_pve_special_reg(&inc_ctx, |reg| {
                let s = reg.incursions.get(&inc_cid).and_then(|i| i.sites.iter().find(|s| s.site_id == site_id)).expect("site");
                (s.escort_spawns.iter().filter(|e| !e.destroyed).map(|e| e.npc_ball_id).collect::<Vec<_>>(), (!s.boss_destroyed).then_some(s.boss_npc_ball_id))
            });
            for eid in escorts { destroy_npc_via_turrets(&inc_ctx, &mut sess_inc, &space, eid, inc_gun, 2).await; }
            if let Some(bid) = boss_opt { destroy_npc_via_turrets(&inc_ctx, &mut sess_inc, &space, bid, inc_gun, 2).await; }
            if eve_server::services::pve_special_sites::with_pve_special_reg(&inc_ctx, |reg| {
                reg.incursions.get(&inc_cid).is_none_or(|i| i.state == "Withdrawing" || i.state == "Ended"
                    || i.sites.iter().find(|s| s.site_id == site_id).is_some_and(|s| s.completed_count > 0 && s.tier != "Mothership"))
            }) { break; }
        }
        for ev in inc_ctx.drain_notifications_for_char(PILOT_A).into_iter().chain(inc_ctx.drain_notifications_for_char(PILOT_B)) {
            if ev.event == "OnInfluenceUpdate" && ev.args.len() == 2 && ev.args[0] == PyRep::Integer(tale_id)
                && ev.args[1].dict_get("influence").and_then(PyRep::as_f64).is_some_and(|v| (0.0..1.0).contains(&v))
            {
                influence_seen = true;
            }
        }
    }
    drop(obs_holder);
    assert!(influence_seen, "OnInfluenceUpdate emitted");

    let _ = (rpc(&inc_ctx, &mut sess_inc, "slash", "SlashCmd", &[PyRep::String(format!("/tr {hq_sys}"))]).await.expect("/tr hq"), inc_ctx.sync_session(&mut sess_inc));
    let _ = inc_ctx.drain_notifications_for_char(PILOT_A);
    let withdraw_until = eve_server::services::pve_special_sites::with_pve_special_reg(&inc_ctx, |reg| {
        reg.incursion_lifecycle.get(&inc_cid).and_then(|m| m.withdraw_until_sec).unwrap_or(0)
    });
    inc_ctx.clock().advance_secs(withdraw_until.saturating_sub(inc_ctx.now_unix()) + eve_server::services::pve_incursion_lifecycle::INCURSION_WITHDRAWAL_DURATION_SEC + 15);
    eve_server::sp6_lifecycle::tick_background_cluster(&inc_ctx).await;

    let tale_rem = inc_ctx.drain_notifications_for_char(PILOT_A).into_iter().find(|n| n.event == "OnTaleRemove").expect("OnTaleRemove");
    assert_eq!(tale_rem.args, vec![
        PyRep::Integer(tale_id), PyRep::Integer(1),
        PyRep::Integer(eve_server::services::pve_incursion_sites::INCURSION_TALE_TEMPLATE_ID as i64),
    ]);
    let _ = rpc(&inc_ctx, &mut sess_inc, "taleMgr", "GetGlobalWorldEventTales", &[]).await;

    // Part B: Pirate Insurgency campaign Stage 5 -> OnSecurityModified via live grid progression
    let fw_ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    common::pin_clock(&fw_ctx);
    let (_enemy_sys, warzone_sys, _cmd_sys, _rear_sys) = crate::fw_warzone_insurgency::setup_warzone_chain(&fw_ctx);
    let fw_ship = 810_400_001_i64;
    let mut sess_docked = SessionState::new(13_199);
    common::login_pilot(&fw_ctx, &mut sess_docked, "demo", "demo-secret").await;
    rpc(&fw_ctx, &mut sess_docked, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)]).await.expect("SelectCharacterID docked");
    rpc(&fw_ctx, &mut sess_docked, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.expect("enlist Guristas");
    let mut sess_fw = login_space_pilot(&fw_ctx, 13_200, "demo", "demo-secret", PILOT_A, fw_ship, 587, warzone_sys, DVec3::ZERO).await;
    sess_fw.role |= ROLE_GM_DEFAULT;

    fw_ctx.clock().advance_secs(eve_server::services::insurgency_services::FORECASTING_DURATION_SECS + 30);
    eve_server::sp6_lifecycle::tick_background_cluster(&fw_ctx).await;
    rpc(&fw_ctx, &mut sess_fw, "dungeonInstanceCacheMgr", "GetPirateInsurgencyInstances", &[]).await.expect("GetPirateInsurgencyInstances");

    let camp_sys = fw_ctx.db.with_snapshot(|s| {
        s.insurgency_runtime.campaigns.values().flat_map(|c| c.systems.iter().copied()).next().expect("campaign system")
    });
    let camp_base_sec = fw_ctx.sde.solar_systems.get(&camp_sys).map(|s| s.security_status).unwrap_or(0.5);
    if camp_sys != warzone_sys {
        sess_fw = login_space_pilot(&fw_ctx, 13_201, "demo", "demo-secret", PILOT_A, fw_ship + 1, 587, camp_sys, DVec3::ZERO).await;
    }
    let beacon_pos = fw_ctx.db.with_snapshot(|s| s.system_states.get(&camp_sys).and_then(|st| st.fw_complex_position).expect("fw_complex_position"));
    let active_fw_ship = sess_fw.ship_id.expect("ship_id");
    let camp_space = fw_ctx.systems.get_or_spawn(camp_sys);
    assert!(camp_space.set_ball_position(active_fw_ship, DVec3::from_array(beacon_pos)).await);

    let _ = fw_ctx.drain_notifications_for_char(PILOT_A);
    let mut sec_ev = None;
    for _ in 0..15 {
        camp_space.step_manual(1.0).await.expect("insurgency grid step");
        eve_server::services::insurgency_services::step_system_insurgency_tick_delta(&fw_ctx, camp_sys, Some(300)).await;
        if let Some(ev) = fw_ctx.drain_notifications_for_char(PILOT_A).into_iter().find(|n| n.event == "OnSecurityModified") {
            sec_ev = Some(ev);
            break;
        }
    }
    let sec_ev = sec_ev.expect("OnSecurityModified at Corruption Stage 5");
    assert_eq!(sec_ev.args[0], PyRep::Integer(i64::from(camp_sys)));
    assert_eq!(sec_ev.args[2], PyRep::Real(0.0));
    assert!((sec_ev.args[1].as_f64().unwrap_or(0.0) + camp_base_sec).abs() < 1e-4);
}

// parity: esi:GET /corporations/{corporation_id}/blueprints
// parity: feat:esi.corporation.blueprints
// parity: feat:esi.corporation.projects
// parity: esi:GET /markets/structures/{structure_id}
// parity: plan:ESI-5
#[tokio::test]
async fn test_esi_corp_blueprints_projects_and_structure_markets_live() {
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/blueprints",
        "feat:esi.corporation.blueprints",
        "feat:esi.corporation.projects",
        "esi:GET /markets/structures/{structure_id}",
        "plan:ESI-5",
    ]);

    let ctx = Arc::new(crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx());
    common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let (corp_id, struct_id, starbase_id) = (98_700_055_i64, 1_037_000_055_001_i64, 99_700_055_i64);
    let (bp_item_id, market_hub_id) = (880_700_001_i64, 880_700_002_i64);
    let master_wallet = IskCents::from_isk(1_000_000);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ceo) = snap.characters.get_mut(&PILOT_A) { (ceo.corporation_id, ceo.station_id, ceo.structure_id) = (corp_id, None, Some(struct_id)); }
        snap.corporations.insert(corp_id, { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "Kalakiota Logistics Corp".into();
            __corp.ticker = "KLCL".into();
            __corp.ceo_id = PILOT_A;
            __corp.alliance_id = None;
            __corp.tax_rate = 0.05;
            __corp.members = HashSet::from([PILOT_A]);
            __corp.member_roles = HashMap::from([(PILOT_A, ROLE_DIRECTOR | ROLE_STATION_MANAGER)]);
            __corp.applications = HashMap::new();
            __corp.description = String::new();
            __corp.url = String::new();
            __corp.hq_station_id = None;
            __corp.wallet_divisions = CorporationRecord::default_wallet_divisions(master_wallet);
            let __mw = master_wallet;
            if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); }
            __corp });
        snap.structures.insert(struct_id, UpwellStructureRecord {
            structure_id: struct_id, type_id: 35_833, owner_corp_id: corp_id, solar_system_id: SYS_JITA,
            name: "Jita Trade Citadel".into(), state: "ShieldVulnerable".into(), fuel_blocks: 200, quantum_core_installed: true, ..Default::default()
        });
        snap.items.insert(market_hub_id, InventoryItemRecord { item_id: market_hub_id, type_id: 35_892, owner_id: corp_id, location_id: struct_id, flag: InventoryFlag::ServiceSlot0, quantity: 1, singleton: true, ..Default::default() });
        snap.items.insert(bp_item_id, InventoryItemRecord {
            item_id: bp_item_id, type_id: 687, owner_id: corp_id, location_id: struct_id, flag: InventoryFlag::CorpSAG1,
            quantity: 1, singleton: true, dynamic_attributes: HashMap::from([(1112, 10.0), (1113, 20.0)]), ..Default::default()
        });
        snap.market_orders.insert(770_700_001, MarketOrderRecord {
            order_id: 770_700_001, owner_id: PILOT_A, station_id: struct_id, region_id: 10_000_002, type_id: 587,
            is_bid: false, price: IskCents::from_isk(425_000), vol_entered: 10, vol_remaining: 7, min_volume: 1,
            is_npc: false, escrow: IskCents(0),
        });
        snap.market_order_meta.insert(770_700_001, MarketOrderMeta { issued_unix: 1_770_000_000, duration_days: 90, range: "region".into(), issued_by: Some(PILOT_A), wallet_division: Some(1000), is_corporation: false, state: Some("open".into()) });
        snap.items.insert(starbase_id, InventoryItemRecord { item_id: starbase_id, type_id: 16_213, owner_id: corp_id, location_id: i64::from(SYS_JITA), flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default() });
        snap.starbase_settings.insert(starbase_id, StarbaseDetailSettings::default());
        snap.corp_projects.insert(700_055, CorpProjectRecord {
            project_id: 700_055, corporation_id: corp_id, name: "Tritanium Hauling Drive".into(),
            target_quantity: 5_000, current_quantity: 250, isk_per_unit: IskCents::from_isk(5),
            escrow_remaining: IskCents::from_isk(23_750),
        });
    });

    let now = ctx.now_unix();
    let code = ctx.db.authorize_pkce(
        "demo", "demo-secret", PILOT_A, "evomu-live-esi-test", "https://localhost/cb",
        vec![
            "esi-corporations.read_blueprints.v1".into(), "esi-corporations.read_projects.v1".into(),
            "esi-markets.structure_markets.v1".into(), "esi-corporations.read_structures.v1".into(),
            "esi-corporations.read_starbases.v1".into(),
        ],
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM", "S256", now,
    ).expect("authorize_pkce");
    let token = ctx.db.exchange_pkce_code(&code, "evomu-live-esi-test", "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", now).expect("exchange").access_token;

    let esi_get = |path: String, auth: Option<&str>, expected: u16| -> Value {
        let mut req = EsiHttpRequest::new("GET", path.clone());
        if let Some(tok) = auth { req.headers.insert("Authorization".into(), format!("Bearer {tok}")); }
        let resp = router.handle_request(&req);
        assert_eq!(resp.status_code, expected, "{path} -> {}", resp.status_code);
        let raw_path = path.split('?').next().unwrap_or(&path);
        if expected != 403 {
            spec.validate_response(&req.method, raw_path, resp.status_code, &resp.headers, &resp.body).expect("OpenAPI valid");
        }
        if expected == 200 {
            let etag = resp.header("etag").expect("ETag");
            let mut req304 = req.clone();
            req304.headers.insert("If-None-Match".into(), etag.to_string());
            assert_eq!(router.handle_request(&req304).status_code, 304);
        }
        resp.json().expect("json")
    };

    esi_get(format!("/v2/corporations/{corp_id}/blueprints/"), None, 401);
    esi_get("/v2/corporations/98999999/projects/".into(), Some(&token), 403);
    esi_get("/v1/markets/structures/103999999999/".into(), Some(&token), 404);
    let bps = esi_get(format!("/v2/corporations/{corp_id}/blueprints/"), Some(&token), 200);
    assert_eq!((bps[0]["item_id"].clone(), bps[0]["type_id"].clone(), bps[0]["material_efficiency"].clone()), (json!(bp_item_id), json!(687), json!(10)));
    let projs = esi_get(format!("/v1/corporations/{corp_id}/projects/"), Some(&token), 200);
    assert_eq!((projs[0]["name"].clone(), projs[0]["target_quantity"].clone()), (json!("Tritanium Hauling Drive"), json!(5_000)));
    let orders = esi_get(format!("/v1/markets/structures/{struct_id}/"), Some(&token), 200);
    assert_eq!((orders[0]["order_id"].clone(), orders[0]["price"].clone(), orders[0]["volume_remain"].clone()), (json!(770_700_001_i64), json!(425_000.0), json!(7)));

    let structs = esi_get(format!("/v4/corporations/{corp_id}/structures/"), Some(&token), 200);
    assert_eq!(structs[0]["services"][0]["name"], json!("Standup Market Hub I"));
    let bases = esi_get(format!("/v4/corporations/{corp_id}/starbases/"), Some(&token), 200);
    assert_eq!(bases[0]["starbase_id"], json!(starbase_id));
}

// parity: quasar:eveProto/generated/eve_public/payment/schedule/notices_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/payment/token/api/notices_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/payment/token/api_pb2.pyj
// parity: plan:Q-5
// parity: plan:Q-6
#[tokio::test]
async fn test_quasar_payment_schedules_tokens_notices_and_domain_plans_live() {
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/payment/schedule/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/token/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/token/api_pb2.pyj",
        "plan:Q-5",
        "plan:Q-6",
    ]);

    let db_tmp = TempSqlitePath::new("quasar_payment_plans");
    let reg = QuasarDescriptorRegistry::open_default().expect("quasar descriptors");
    let seed_ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    seed_ctx.db.save_to_sqlite(&db_tmp.0).expect("save seed");
    drop(seed_ctx);
    let ctx = ServerContext::from_sqlite(&db_tmp.0).expect("from_sqlite");
    common::pin_clock(&ctx);
    let base_unix = ctx.now_unix();
    let unauth = SessionState::new(15_000);
    for (req_name, body) in [
        ("eve_public.payment.token.api.GetQuickPayRequest", Vec::new()),
        ("eve_public.payment.token.api.GetRequest", id_msg(1, 1)),
        ("eve_public.payment.token.api.DisableAllRequest", Vec::new()),
        ("eve_public.payment.schedule.api.GetRequest", id_msg(1, 1)),
        ("eve_public.payment.schedule.api.GetActiveByTypeRequest", Vec::new()),
    ] {
        assert_eq!(quasar_call(&ctx, &unauth, req_name, body).status_code, STATUS_UNAUTHORIZED);
    }

    let mut admin_sess = SessionState::new(15_001);
    (admin_sess.user_id, admin_sess.character_id, admin_sess.corporation_id, admin_sess.role) = (Some(1), Some(PILOT_A), Some(1_000_009), u64::MAX);
    let mut other_sess = SessionState::new(15_002);
    (other_sess.user_id, other_sess.character_id) = (Some(2), Some(PILOT_B));
    let mut sub_rx = ctx.quasar_gateway.subscribe_session(&admin_sess, Vec::new(), base_unix);

    let (mut cat_attr, mut cat_req, mut cp, mut off_attr, mut off_req) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_string_field(1, "Omega Subscriptions", &mut cat_attr);
    encode_bytes_field(1, &cat_attr, &mut cat_req);
    assert_eq!(quasar_call(&ctx, &admin_sess, "eve.store.admin.api.CreateCategoryRequest", cat_req).status_code, STATUS_OK);
    encode_string_field(1, "USD", &mut cp);
    encode_varint_field(2, 1_999, &mut cp);
    encode_string_field(1, "1 Month Omega", &mut off_attr);
    encode_varint_field(2, 500, &mut off_attr);
    encode_bytes_field(3, &cp, &mut off_attr);
    encode_bytes_field(1, &off_attr, &mut off_req);
    let off_rsp = quasar_call(&ctx, &admin_sess, "eve.store.admin.api.CreateOfferRequest", off_req);
    assert_eq!(off_rsp.status_code, STATUS_OK);
    let off_wm = WireMessage::decode(&off_rsp.payload.as_ref().unwrap().value).unwrap();
    let offer_id_str = String::from_utf8(WireMessage::decode(off_wm.fields[0].value.as_bytes().unwrap()).unwrap().fields[0].value.as_bytes().unwrap().to_vec()).unwrap();
    let offer_num: u64 = offer_id_str.parse().expect("offer id");

    register_user_payment_token(&ctx, 1, PaymentTokenRecord {
        id: 55, user_id: 1, token_str: "tok_visa_4242".into(), is_quick_pay: true, consumed: false,
    });
    let (_, added_raw) = drain_sub(&mut sub_rx).into_iter().find(|(t, _)| t == "eve_public.payment.token.api.QuickPayAddedNotice").expect("QuickPayAddedNotice");
    assert_eq!(decode_nested_u64(&added_raw, 1, 1), Some(55));

    let qp_rsp = quasar_call(&ctx, &admin_sess, "eve_public.payment.token.api.GetQuickPayRequest", Vec::new());
    assert_eq!(qp_rsp.status_code, STATUS_OK);
    assert_eq!(decode_nested_u64(&qp_rsp.payload.as_ref().unwrap().value, 1, 1), Some(55));

    let tok_rsp = quasar_call(&ctx, &admin_sess, "eve_public.payment.token.api.GetRequest", id_msg(1, 55));
    assert_eq!(tok_rsp.status_code, STATUS_OK);
    let tok_dyn = reg.decode_dynamic("eve_public.payment.token.api.GetResponse", &tok_rsp.payload.as_ref().unwrap().value).expect("decode token GetResponse");
    if let Some(DynamicValue::Message(attr)) = tok_dyn.get(1) {
        if let Some(DynamicValue::Message(cc)) = attr.get(4) {
            assert_eq!((cc.get(1), cc.get(2)), (Some(&DynamicValue::String("tok_visa_4242".into())), Some(&DynamicValue::String("12/2030".into()))));
        } else { panic!("missing credit_card"); }
    } else { panic!("missing token attr"); }
    assert_eq!(quasar_call(&ctx, &other_sess, "eve_public.payment.token.api.GetRequest", id_msg(1, 55)).status_code, STATUS_NOT_FOUND);

    let mut cost_req = Vec::new();
    encode_varint_field(1, offer_num, &mut cost_req);
    assert_eq!(quasar_call(&ctx, &admin_sess, "eve_public.payment.purchase.api.CostRequest", cost_req).status_code, STATUS_OK);

    let (mut off_ident, mut order, mut tok_ident, mut tok_req) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_string_field(1, &offer_id_str, &mut off_ident);
    encode_bytes_field(1, &off_ident, &mut order);
    encode_varint_field(2, 1, &mut order);
    encode_varint_field(1, 55, &mut tok_ident);
    encode_bytes_field(1, &order, &mut tok_req);
    encode_bytes_field(2, &tok_ident, &mut tok_req);
    assert_eq!(quasar_call(&ctx, &admin_sess, "eve_public.payment.purchase.api.TokenRequest", tok_req).status_code, STATUS_OK);

    let (_, set_raw) = drain_sub(&mut sub_rx).into_iter().find(|(t, _)| t == "eve_public.payment.schedule.api.SetNotice").expect("SetNotice");
    let sched_id = decode_nested_u64(&set_raw, 1, 1).expect("sched_id");

    let mut by_type = Vec::new();
    encode_varint_field(1, 1, &mut by_type);
    let active_rsp = quasar_call(&ctx, &admin_sess, "eve_public.payment.schedule.api.GetActiveByTypeRequest", by_type);
    assert_eq!(decode_nested_u64(&active_rsp.payload.as_ref().unwrap().value, 1, 1), Some(sched_id));

    assert_eq!(quasar_call(&ctx, &admin_sess, "eve_public.payment.token.api.DisableAllRequest", Vec::new()).status_code, STATUS_OK);
    let (_, rem_tok_raw) = drain_sub(&mut sub_rx).into_iter().find(|(t, _)| t == "eve_public.payment.token.api.QuickPayRemovedNotice").expect("QuickPayRemovedNotice");
    assert_eq!(decode_nested_u64(&rem_tok_raw, 1, 1), Some(55));

    ctx.clock().advance_secs(31 * 86_400);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let (_, cancel_raw) = drain_sub(&mut sub_rx).into_iter().find(|(t, _)| t == "eve_public.payment.schedule.api.CancelledNotice").expect("CancelledNotice");
    assert_eq!(decode_nested_u64(&cancel_raw, 1, 1), Some(sched_id));
    assert_eq!(quasar_call(&ctx, &admin_sess, "eve_public.payment.schedule.api.GetRequest", id_msg(1, sched_id)).status_code, STATUS_NOT_FOUND);

    // plan:Q-5 (Social/Org: alliance) & plan:Q-6 (Economy: plex vault transactions)
    assert_eq!(quasar_call(&ctx, &admin_sess, "eve.alliance.GetAllRequest", Vec::new()).status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &admin_sess, "eve_public.plex.vault.transaction.api.GetAllLoggedForUserRequest", Vec::new()).status_code, STATUS_OK);
}
