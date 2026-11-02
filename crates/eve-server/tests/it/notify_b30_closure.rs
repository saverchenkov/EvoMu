//! Live integration tests for Batch 30 notification emitters (`notify_emitters_b30.rs`).
//!
//! Covers 12 `notify:` parity rows:
//! - `notify:OnOperationReplayed`
//! - `notify:OnUiRevealForced`
//! - `notify:OnPasswordChanged`
//! - `notify:OnInvalidateCacheForLocations`
//! - `notify:OnMultiEvent`
//! - `notify:OnSolarSystemLPChange`
//! - `notify:OnVictoryPointStateUpdated`
//! - `notify:OnWeaponsTimerUpdate`
//! - `notify:OnPvpTimerUpdate`
//! - `notify:OnCriminalTimerUpdate`
//! - `notify:OnSystemCriminalFlagUpdates`
//! - `notify:OnShipDeath`

#![forbid(unsafe_code)]

use crate::common;
use crate::notify_real_emitters_batch7::{login_in_space, make_item};
use eve_db::{
    corp_roles::{ROLE_DIRECTOR, ROLE_EQUIPMENT_CONFIG},
    InventoryFlag, TrainedSkillRecord,
};
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;
const STATION_JITA: i64 = 60_003_760;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn kv_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.iter().find_map(|(k, v)| if k == key { Some(v) } else { None }),
        PyRep::Object { args, .. } => kv_get(args, key),
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| match k {
            PyRep::String(s) if s == key => Some(v),
            _ => None,
        }),
        _ => None,
    }
}

fn expected_ft(unix_secs: u64) -> i64 {
    i64::try_from(unix_secs).unwrap_or(0) * 10_000_000 + 116_444_736_000_000_000
}

// parity: notify:OnOperationReplayed, notify:OnUiRevealForced
// parity: notify:OnPasswordChanged, notify:OnInvalidateCacheForLocations
#[tokio::test]
async fn notify_b30_operations_and_container_password() {
    eve_testkit::attest(&[
        "notify:OnOperationReplayed",
        "notify:OnUiRevealForced",
        "notify:OnPasswordChanged",
        "notify:OnInvalidateCacheForLocations",
    ]);

    let ctx = new_ctx();
    let mut unauth = SessionState::new(9_600);
    assert_eq!(
        rpc(&ctx, &mut unauth, "slash", "SlashCmd", &[PyRep::String("/operation reveal_ui".into())])
            .await
            .unwrap_err()
            .code,
        "Unauthorized"
    );

    let mut sess_a = SessionState::new(9_601);
    common::login_pilot(&ctx, &mut sess_a, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess_a, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)])
        .await
        .expect("SelectCharacterID A");
    sess_a.role |= ROLE_GM_DEFAULT;
    sess_a.station_id = Some(STATION_JITA);
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    // 1. OnOperationReplayed: negative invalid op rejected without emitting
    assert!(rpc(&ctx, &mut sess_a, "slash", "SlashCmd", &[PyRep::String("/operation replay 0 0".into())])
        .await
        .is_err());
    assert!(ctx.drain_notifications_for_char(PILOT_A).iter().all(|n| n.event != "OnOperationReplayed"));

    // Positive: /operation replay <cat_id> <op_id> mutates CharacterOperationsRecord and emits OnOperationReplayed
    let op_id: i64 = ctx
        .sde
        .operations()
        .expect("operations")
        .keys()
        .filter_map(|k| k.parse::<i64>().ok())
        .min()
        .expect("at least one operation in SDE");
    assert_eq!(
        rpc(&ctx, &mut sess_a, "slash", "SlashCmd", &[PyRep::String(format!("/operation replay 1 {op_id}"))])
            .await
            .expect("SlashCmd /operation replay"),
        PyRep::Bool(true)
    );
    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    let replayed = notifs.iter().find(|n| n.event == "OnOperationReplayed").expect("OnOperationReplayed");
    assert_eq!(replayed.id_type, "charid");
    assert_eq!(replayed.args, vec![PyRep::Integer(1), PyRep::Integer(op_id)]);

    // 2. OnUiRevealForced: /operation reveal_ui emits OnUiRevealForced() with 0 args
    assert_eq!(
        rpc(&ctx, &mut sess_a, "slash", "SlashCmd", &[PyRep::String("/operation reveal_ui".into())])
            .await
            .expect("SlashCmd /operation reveal_ui"),
        PyRep::Bool(true)
    );
    let revealed = ctx
        .drain_notifications_for_char(PILOT_A)
        .into_iter()
        .find(|n| n.event == "OnUiRevealForced")
        .expect("OnUiRevealForced");
    assert_eq!(revealed.id_type, "charid");
    assert!(revealed.args.is_empty());

    // 3 & 4. OnPasswordChanged & OnInvalidateCacheForLocations via bound invbroker.SetPassword
    let container_id: i64 = 99_300_001;
    let corp_container_id: i64 = 99_300_002;
    let container_type_id: u32 = 3465; // Small Secure Container
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            container_id,
            make_item(container_id, container_type_id, PILOT_A, STATION_JITA, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            corp_container_id,
            make_item(corp_container_id, container_type_id, 2001, STATION_JITA, InventoryFlag::Hangar, 1, true),
        );
    });
    let bind = rpc(
        &ctx,
        &mut sess_a,
        "invbroker",
        "MachoBindObject",
        &[PyRep::Tuple(vec![PyRep::Integer(STATION_JITA), PyRep::Integer(15)]), PyRep::None],
    )
    .await
    .expect("MachoBindObject");
    let mgr_oid = match &bind {
        PyRep::Tuple(parts) => parts[0].as_i64().expect("mgr bound id") as u64,
        _ => panic!("expected MachoBindObject tuple"),
    };
    let oid = ctx
        .dispatch_rpc(
            &mut sess_a,
            "invbroker",
            Some(mgr_oid),
            "GetInventoryFromId",
            &[PyRep::Integer(container_id), PyRep::Integer(0)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("GetInventoryFromId")
        .as_i64()
        .expect("container bound id") as u64;
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    // Set initial config password (which = 2) and general password (which = 1) independently
    ctx.dispatch_rpc(
        &mut sess_a,
        "invbroker",
        Some(oid),
        "SetPassword",
        &[PyRep::Integer(2), PyRep::String("alpha123".into()), PyRep::String(String::new())],
        &HashMap::new(),
        None,
    )
    .await
    .expect("bound SetPassword which=2");

    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    let pw_notif = notifs.iter().find(|n| n.event == "OnPasswordChanged").expect("OnPasswordChanged");
    assert_eq!(pw_notif.args.len(), 2);
    assert_eq!(pw_notif.args[0], PyRep::Integer(2));
    assert_eq!(kv_get(&pw_notif.args[1], "itemID"), Some(&PyRep::Integer(container_id)));
    assert_eq!(kv_get(&pw_notif.args[1], "typeID"), Some(&PyRep::Integer(i64::from(container_type_id))));
    let inv_notif = notifs
        .iter()
        .find(|n| n.event == "OnInvalidateCacheForLocations")
        .expect("OnInvalidateCacheForLocations");
    assert_eq!(inv_notif.args, vec![PyRep::List(vec![PyRep::Integer(container_id)])]);

    // Omitting oldPassword fails with InvalidPassword when existing password is set
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess_a,
            "invbroker",
            Some(oid),
            "SetPassword",
            &[PyRep::Integer(2), PyRep::String("beta456".into())],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap_err()
        .code,
        "InvalidPassword"
    );
    assert!(ctx.drain_notifications_for_char(PILOT_A).iter().all(|n| n.event != "OnPasswordChanged"));

    // Setting and clearing general password (which = 1) with None does not collide with which = 2
    ctx.dispatch_rpc(
        &mut sess_a,
        "invbroker",
        Some(oid),
        "SetPassword",
        &[PyRep::Integer(1), PyRep::String("gen111".into()), PyRep::String(String::new())],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SetPassword which=1");
    ctx.dispatch_rpc(
        &mut sess_a,
        "invbroker",
        Some(oid),
        "SetPassword",
        &[PyRep::Integer(1), PyRep::None, PyRep::String("gen111".into())],
        &HashMap::new(),
        None,
    )
    .await
    .expect("Clear SetPassword which=1 with None");
    // Config password (which = 2) still requires "alpha123"
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess_a,
            "invbroker",
            Some(oid),
            "SetPassword",
            &[PyRep::Integer(2), PyRep::String("beta456".into()), PyRep::String(String::new())],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap_err()
        .code,
        "InvalidPassword"
    );

    // Corp container password requires ROLE_DIRECTOR | ROLE_EQUIPMENT_CONFIG
    sess_a.corporation_id = Some(2001);
    sess_a.corp_role = 0;
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a,
            "invbroker",
            "SetPassword",
            &[PyRep::Integer(corp_container_id), PyRep::Integer(1), PyRep::String("corp_pw".into()), PyRep::String(String::new())],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleRequired"
    );
    sess_a.corp_role = ROLE_EQUIPMENT_CONFIG | ROLE_DIRECTOR;
    rpc(
        &ctx,
        &mut sess_a,
        "invbroker",
        "SetPassword",
        &[PyRep::Integer(corp_container_id), PyRep::Integer(1), PyRep::String("corp_pw".into()), PyRep::String(String::new())],
    )
    .await
    .expect("corp container SetPassword with equipment config role");

    // BF-04 negative checks: invalid which, non-container item, non-string password, and >2500m in space
    assert_eq!(rpc(&ctx, &mut sess_a, "invbroker", "SetPassword", &[PyRep::Integer(corp_container_id), PyRep::Integer(99), PyRep::String("pw".into())]).await.unwrap_err().code, "InvalidPasswordType");
    assert_eq!(rpc(&ctx, &mut sess_a, "invbroker", "SetPassword", &[PyRep::Integer(corp_container_id), PyRep::Integer(1), PyRep::Integer(123)]).await.unwrap_err().code, "InvalidPassword");
    let (mineral_id, ship_sp_id, far_cont_id) = (960_000_031_i64, 960_000_032_i64, 960_000_033_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mineral_id, make_item(mineral_id, 34, PILOT_A, STATION_JITA, InventoryFlag::Hangar, 100, false));
        snap.items.insert(far_cont_id, make_item(far_cont_id, 3465, PILOT_A, SYS_JITA as i64, InventoryFlag::None, 1, true));
    });
    assert_eq!(rpc(&ctx, &mut sess_a, "invbroker", "SetPassword", &[PyRep::Integer(mineral_id), PyRep::Integer(1), PyRep::String("pw".into())]).await.unwrap_err().code, "ContainerDoesNotSupportPassword");
    let mut sess_space = login_in_space(&ctx, 9_609, "demo", "demo-secret", PILOT_A, ship_sp_id, 587, SYS_JITA, DVec3::ZERO).await;
    let sys_sp = ctx.systems.get_or_spawn(SYS_JITA);
    sys_sp.spawn_ball_with_meta(eve_destiny::Ball { id: far_cont_id, type_id: 3465, position: DVec3::new(10_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1_000.0, radius: 50.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop }, ctx.build_ship_combat_state(PILOT_A, far_cont_id, 3465), ctx.build_ship_slim_meta(PILOT_A, 3465)).await.unwrap();
    assert_eq!(rpc(&ctx, &mut sess_space, "invbroker", "SetPassword", &[PyRep::Integer(far_cont_id), PyRep::Integer(1), PyRep::String("pw".into())]).await.unwrap_err().code, "OutsideOperationalRange");
}

// parity: notify:OnMultiEvent, notify:OnSolarSystemLPChange, notify:OnVictoryPointStateUpdated
#[tokio::test]
async fn notify_b30_target_multi_event_and_fw_warzone() {
    eve_testkit::attest(&[
        "notify:OnMultiEvent",
        "notify:OnSolarSystemLPChange",
        "notify:OnVictoryPointStateUpdated",
    ]);

    let ctx = new_ctx();
    let (ship_a, ship_b) = (960_000_101_i64, 960_000_102_i64);
    let mut sess_a = login_in_space(&ctx, 9_611, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, DVec3::ZERO).await;
    let _sess_b = login_in_space(&ctx, 9_612, "other", "other-secret", PILOT_B, ship_b, 587, SYS_JITA, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let sys = ctx.systems.get_or_spawn(SYS_JITA);
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let _ = ctx.drain_notifications_for_char(PILOT_B);

    // 5. OnMultiEvent: RemoveTarget without an active lock must NOT emit otherlost
    rpc(&ctx, &mut sess_a, "dogmaIM", "RemoveTarget", &[PyRep::Integer(ship_b)])
        .await
        .expect("RemoveTarget without lock");
    assert!(ctx.drain_notifications_for_char(PILOT_B).iter().all(|n| n.event != "OnMultiEvent"));

    // AddTarget + step_manual -> PILOT_B receives otheradd; locker does NOT get duplicate standalone OnMultiEvent("add")
    assert!(rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(0)]).await.is_err());
    rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_b)]).await.expect("AddTarget");
    sys.step_manual(10.0).await.expect("step_manual");

    let notifs_b = ctx.drain_notifications_for_char(PILOT_B);
    let multi_add = notifs_b.iter().find(|n| n.event == "OnMultiEvent").expect("PILOT_B OnMultiEvent otheradd");
    assert_eq!(
        multi_add.args,
        vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Tuple(vec![
                PyRep::String("OnTarget".into()),
                PyRep::String("otheradd".into()),
                PyRep::Integer(ship_a),
                PyRep::String(String::new()),
            ]),
            PyRep::Integer(0),
        ])])]
    );
    assert!(ctx.drain_notifications_for_char(PILOT_A).iter().all(|n| n.event != "OnMultiEvent"));

    // RemoveTarget while locked -> PILOT_B receives otherlost; locker receives lost
    rpc(&ctx, &mut sess_a, "dogmaIM", "RemoveTarget", &[PyRep::Integer(ship_b)]).await.expect("RemoveTarget");
    let multi_lost = ctx
        .drain_notifications_for_char(PILOT_B)
        .into_iter()
        .find(|n| n.event == "OnMultiEvent")
        .expect("PILOT_B OnMultiEvent otherlost");
    assert_eq!(
        multi_lost.args,
        vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Tuple(vec![
                PyRep::String("OnTarget".into()),
                PyRep::String("otherlost".into()),
                PyRep::Integer(ship_a),
                PyRep::String(String::new()),
            ]),
            PyRep::Integer(0),
        ])])]
    );
    assert!(ctx.drain_notifications_for_char(PILOT_A).iter().any(|n| n.event == "OnMultiEvent"));

    // ClearTargets while locked -> PILOT_B receives otherlost, PILOT_A receives clear
    rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_b)]).await.expect("AddTarget 2");
    sys.step_manual(10.0).await.expect("step_manual 2");
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let _ = ctx.drain_notifications_for_char(PILOT_B);
    rpc(&ctx, &mut sess_a, "dogmaIM", "ClearTargets", &[]).await.expect("ClearTargets");
    assert!(ctx.drain_notifications_for_char(PILOT_B).iter().any(|n| n.event == "OnMultiEvent"));
    let clear_notif = ctx
        .drain_notifications_for_char(PILOT_A)
        .into_iter()
        .find(|n| n.event == "OnMultiEvent")
        .expect("PILOT_A OnMultiEvent clear");
    assert_eq!(
        clear_notif.args,
        vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Tuple(vec![
                PyRep::String("OnTarget".into()),
                PyRep::String("clear".into()),
                PyRep::Integer(0),
                PyRep::String(String::new()),
            ]),
            PyRep::Integer(0),
        ])])]
    );

    // 6. OnSolarSystemLPChange: facWarMgr.DonateLPsToSolarSystem
    let fw_sys: u32 = 30_002_537; // Amamake
    let caldari_militia_corp: i64 = 1_000_180;
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
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    assert_eq!(
        rpc(&ctx, &mut sess_a, "facWarMgr", "DonateLPsToSolarSystem", &[PyRep::Integer(i64::from(fw_sys)), PyRep::Integer(99_999)])
            .await
            .unwrap_err()
            .code,
        "InsufficientLoyaltyPoints"
    );
    assert!(ctx.drain_notifications_for_char(PILOT_A).iter().all(|n| n.event != "OnSolarSystemLPChange"));

    rpc(&ctx, &mut sess_a, "facWarMgr", "DonateLPsToSolarSystem", &[PyRep::Integer(i64::from(fw_sys)), PyRep::Integer(1_500)])
        .await
        .expect("DonateLPsToSolarSystem");
    let lp_ev = ctx
        .drain_notifications_for_char(PILOT_A)
        .into_iter()
        .find(|n| n.event == "OnSolarSystemLPChange")
        .expect("OnSolarSystemLPChange");
    assert_eq!(lp_ev.id_type, "solarsystemid");
    assert_eq!(lp_ev.args, vec![PyRep::Integer(0), PyRep::Integer(1_500)]);

    // 7. OnVictoryPointStateUpdated: live FW complex capture via RPCs + guard combat + clock advance
    let fw_ctx = new_ctx();
    let (_enemy_sys, warzone_sys, _cmd_sys, _rear_sys) =
        crate::fw_warzone_insurgency_closure::setup_warzone_chain(&fw_ctx);
    let fw_ship = 960_000_109_i64;
    fw_ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.corporation_id = 1_000_181;
            ch.station_id = None;
            ch.solar_system_id = warzone_sys;
        }
        for (i, flg) in [InventoryFlag::HiSlot0, InventoryFlag::HiSlot1, InventoryFlag::HiSlot2].into_iter().enumerate() {
            let gid = 960_000_120 + (i as i64) * 2;
            snap.items.insert(gid, make_item(gid, 3074, PILOT_A, fw_ship, flg, 1, true));
            snap.items.insert(gid + 1, make_item(gid + 1, 12_625, PILOT_A, fw_ship, flg, 500, false));
        }
        for &(tid, lvl) in &[(3300_u32, 5), (3301_u32, 5), (3311_u32, 5), (11_082_u32, 5)] {
            snap.skills.entry(PILOT_A).or_default().insert(tid, TrainedSkillRecord { type_id: tid, level: lvl, skill_points: 256_000.0 });
        }
    });
    let mut sess_fw = login_in_space(&fw_ctx, 9_619, "demo", "demo-secret", PILOT_A, fw_ship, 587, warzone_sys, DVec3::ZERO).await;
    sess_fw.role |= ROLE_GM_DEFAULT;
    let wz_beyonce = fw_ctx.bind_object(&sess_fw, "beyonce", i64::from(warzone_sys)).unwrap();
    rpc(&fw_ctx, &mut sess_fw, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_004)])
        .await
        .expect("CreateMyDirectEnlistment");
    let _ = rpc(&fw_ctx, &mut sess_fw, "fwWarzoneSolarsystem", "GetLocalOccupationState", &[]).await.expect("GetLocalOccupationState");
    let _ = rpc(&fw_ctx, &mut sess_fw, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.expect("GetFactionWarfareInstances");
    let novice_gate = fw_ctx.db.with_snapshot(|s| {
        s.fw_runtime
            .fw_complexes
            .iter()
            .find_map(|(&gid, &(csys, ref tier, _, _, done))| {
                (csys == warzone_sys && tier == "Novice" && !done).then_some(gid)
            })
            .expect("Novice complex in frontline system")
    });
    let wz_space = fw_ctx.systems.get_or_spawn(warzone_sys);
    fw_ctx.dispatch_rpc(&mut sess_fw, "beyonce", Some(wz_beyonce), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(novice_gate), PyRep::Real(0.0)], &HashMap::new(), None).await.expect("CmdWarpToStuff novice_gate");
    for _ in 0..4 {
        wz_space.step_manual(15.0).await.unwrap();
    }
    let _ = rpc(&fw_ctx, &mut sess_fw, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(novice_gate)]).await.expect("ActivateAccelerationGate");
    let guard_id = 500_000_000 + novice_gate;
    rpc(&fw_ctx, &mut sess_fw, "dogmaIM", "AddTarget", &[PyRep::Integer(guard_id)]).await.expect("AddTarget guard");
    wz_space.step_manual(5.0).await.unwrap();
    for gid in [960_000_120_i64, 960_000_122, 960_000_124] {
        rpc(&fw_ctx, &mut sess_fw, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::String("projectileFired".into()), PyRep::Integer(guard_id), PyRep::Integer(1)]).await.expect("Activate gun on guard");
    }
    for _ in 0..90 {
        if wz_space.get_ball_snapshot(guard_id).await.unwrap().is_none() { break; }
        wz_space.step_manual(1.0).await.unwrap();
    }
    assert!(wz_space.get_ball_snapshot(guard_id).await.unwrap().is_none(), "guard NPC destroyed via live combat");
    let _ = fw_ctx.drain_notifications_for_char(PILOT_A);
    eve_server::services::fw_gameplay::tick_fw_gameplay(&fw_ctx, fw_ctx.now_unix()).await;
    for _ in 0..6 {
        rpc(&fw_ctx, &mut sess_fw, "timeControl", "AdvanceClock", &[PyRep::Integer(150)]).await.expect("AdvanceClock 150");
        eve_server::services::fw_gameplay::tick_fw_gameplay(&fw_ctx, fw_ctx.now_unix()).await;
    }
    let vp_ev = fw_ctx
        .drain_notifications_for_char(PILOT_A)
        .into_iter()
        .find(|n| n.event == "OnVictoryPointStateUpdated")
        .expect("OnVictoryPointStateUpdated emitted from live complex capture");
    assert_eq!(vp_ev.id_type, "solarsystemid");
    assert_eq!(vp_ev.args[0], PyRep::Integer(i64::from(warzone_sys)));
    assert!(kv_get(&vp_ev.args[1], "victoryPoints").and_then(PyRep::as_i64).unwrap_or(0) >= 20);
}

// parity: notify:OnWeaponsTimerUpdate, notify:OnPvpTimerUpdate, notify:OnCriminalTimerUpdate
// parity: notify:OnSystemCriminalFlagUpdates, notify:OnShipDeath
#[tokio::test]
async fn notify_b30_crimewatch_timers_and_ship_death() {
    eve_testkit::attest(&[
        "notify:OnWeaponsTimerUpdate",
        "notify:OnPvpTimerUpdate",
        "notify:OnCriminalTimerUpdate",
        "notify:OnSystemCriminalFlagUpdates",
        "notify:OnShipDeath",
    ]);

    let ctx = new_ctx();
    let (ship_a, ship_b) = (960_000_201_i64, 960_000_202_i64);
    let gun_a: i64 = 960_000_211;
    let mut sess_a = login_in_space(&ctx, 9_621, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, DVec3::ZERO).await;
    let mut sess_b = login_in_space(&ctx, 9_622, "other", "other-secret", PILOT_B, ship_b, 587, SYS_JITA, DVec3::new(500.0, 0.0, 0.0)).await;
    sess_a.role |= ROLE_GM_DEFAULT;
    sess_b.role |= ROLE_GM_DEFAULT;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(gun_a, make_item(gun_a, 2873, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(960_000_212, make_item(960_000_212, 178, PILOT_A, ship_a, InventoryFlag::HiSlot0, 500, false));
        for &(tid, lvl) in &[(3300_u32, 5), (3301_u32, 5), (3311_u32, 5)] {
            snap.skills.entry(PILOT_A).or_default().insert(tid, TrainedSkillRecord { type_id: tid, level: lvl, skill_points: 256_000.0 });
        }
    });
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let _ = ctx.drain_notifications_for_char(PILOT_B);

    // 8-11. High-sec aggression via live client RPCs -> 101/201/301 + OnSystemCriminalFlagUpdates, then AdvanceClock -> 102/202/303
    rpc(&ctx, &mut sess_a, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await.expect("SetSafetyLevel Red");
    rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_b)]).await.expect("AddTarget B");
    ctx.systems.get_or_spawn(SYS_JITA).step_manual(10.0).await.expect("step_manual");
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(gun_a), PyRep::String("attack".into()), PyRep::Integer(ship_b)])
        .await
        .expect("Activate gun_a");

    let t0 = ctx.now_unix();
    let notifs_a = ctx.drain_notifications_for_char(PILOT_A);
    let weap = notifs_a.iter().find(|n| n.event == "OnWeaponsTimerUpdate").expect("OnWeaponsTimerUpdate 101");
    assert_eq!(weap.args[0], PyRep::Integer(101));
    let pvp = notifs_a.iter().find(|n| n.event == "OnPvpTimerUpdate").expect("OnPvpTimerUpdate 201");
    assert_eq!(pvp.args[0], PyRep::Integer(201));
    let crim = notifs_a.iter().find(|n| n.event == "OnCriminalTimerUpdate").expect("OnCriminalTimerUpdate 301");
    assert_eq!(crim.args[0], PyRep::Integer(301));
    let sys_crim = notifs_a.iter().find(|n| n.event == "OnSystemCriminalFlagUpdates").expect("OnSystemCriminalFlagUpdates");
    assert_eq!(sys_crim.id_type, "solarsystemid");
    assert_eq!(sys_crim.args.len(), 3);
    assert_eq!(sys_crim.args[2], PyRep::List(vec![PyRep::Integer(PILOT_A)]));

    // AdvanceClock past 1s trigger -> countdown states 102, 202, 303 with Windows FILETIME expiry
    rpc(&ctx, &mut sess_a, "timeControl", "AdvanceClock", &[PyRep::Integer(5)]).await.expect("AdvanceClock 5");
    let tick_notifs = ctx.drain_notifications_for_char(PILOT_A);
    let w_cd = tick_notifs.iter().find(|n| n.event == "OnWeaponsTimerUpdate").expect("OnWeaponsTimerUpdate 102");
    assert_eq!(w_cd.args, vec![PyRep::Integer(102), PyRep::Integer(expected_ft(t0 + 60))]);
    let p_cd = tick_notifs.iter().find(|n| n.event == "OnPvpTimerUpdate").expect("OnPvpTimerUpdate 202");
    assert_eq!(p_cd.args, vec![PyRep::Integer(202), PyRep::Integer(expected_ft(t0 + 900))]);
    let c_cd = tick_notifs.iter().find(|n| n.event == "OnCriminalTimerUpdate").expect("OnCriminalTimerUpdate 303");
    assert_eq!(c_cd.args, vec![PyRep::Integer(303), PyRep::Integer(expected_ft(t0 + 900))]);

    // Advance past t0 + 600 -> 900s PvP timer must stay 202 (no 202 -> 201 regression) in both notifications and GetClientStates
    rpc(&ctx, &mut sess_a, "timeControl", "AdvanceClock", &[PyRep::Integer(596)]).await.expect("AdvanceClock to t0+601");
    let mid_notifs = ctx.drain_notifications_for_char(PILOT_A);
    assert!(mid_notifs.iter().all(|n| n.event != "OnPvpTimerUpdate" || n.args[0] == PyRep::Integer(202)));
    let cw_states = rpc(&ctx, &mut sess_a, "crimewatch", "GetClientStates", &[]).await.expect("GetClientStates at t0+601");
    if let PyRep::Tuple(outer) = &cw_states {
        if let PyRep::Tuple(timers) = &outer[0] {
            assert_eq!(timers[1], PyRep::Tuple(vec![PyRep::Integer(202), PyRep::Integer(expected_ft(t0 + 900))]));
        }
    }

    // Low-sec suspect via SetSafetyLevel(1) + Activate -> 302 -> AdvanceClock -> 304 + suspects list
    let (low_sys, ship_c, gun_c) = (30_002_537_u32, 960_000_203_i64, 960_000_213_i64);
    let uid_c = ctx.db.provision_player_account("b30_crim_c", "b30_crim_pass");
    let pilot_c = ctx.db.create_character(uid_c, "B30 Crim C", 1, 1, 11, 17, &ctx.sde).unwrap().character_id;
    let ship_b_low = 960_000_204_i64;
    let mut sess_c = login_in_space(&ctx, 9_623, "b30_crim_c", "b30_crim_pass", pilot_c, ship_c, 587, low_sys, DVec3::ZERO).await;
    sess_c.role |= ROLE_GM_DEFAULT;
    let mut sess_b2 = login_in_space(&ctx, 9_624, "other", "other-secret", PILOT_B, ship_b_low, 587, low_sys, DVec3::new(500.0, 0.0, 0.0)).await;
    sess_b2.role |= ROLE_GM_DEFAULT;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(gun_c, make_item(gun_c, 2873, pilot_c, ship_c, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(960_000_214, make_item(960_000_214, 178, pilot_c, ship_c, InventoryFlag::HiSlot0, 500, false));
        for &(tid, lvl) in &[(3300_u32, 5), (3301_u32, 5), (3311_u32, 5)] {
            snap.skills.entry(pilot_c).or_default().insert(tid, TrainedSkillRecord { type_id: tid, level: lvl, skill_points: 256_000.0 });
        }
    });
    let _ = ctx.drain_notifications_for_char(pilot_c);
    rpc(&ctx, &mut sess_c, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(1)]).await.expect("SetSafetyLevel Partial");
    rpc(&ctx, &mut sess_c, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_b_low)]).await.expect("AddTarget B low");
    ctx.systems.get_or_spawn(low_sys).step_manual(10.0).await.expect("step_manual low");
    rpc(&ctx, &mut sess_c, "dogmaIM", "Activate", &[PyRep::Integer(gun_c), PyRep::String("attack".into()), PyRep::Integer(ship_b_low)])
        .await
        .expect("Activate gun_c");
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_c)
            .into_iter()
            .find(|n| n.event == "OnCriminalTimerUpdate")
            .expect("302 Suspect")
            .args[0],
        PyRep::Integer(302)
    );
    rpc(&ctx, &mut sess_c, "timeControl", "AdvanceClock", &[PyRep::Integer(5)]).await.expect("AdvanceClock low 5");
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_c)
            .into_iter()
            .find(|n| n.event == "OnCriminalTimerUpdate")
            .expect("304 Suspect countdown")
            .args[0],
        PyRep::Integer(304)
    );

    // 12. OnShipDeath: ship.SelfDestruct + timeControl.AdvanceClock(125) emits OnShipDeath() with 0 args
    let _ = ctx.drain_notifications_for_char(PILOT_B);
    rpc(&ctx, &mut sess_b2, "ship", "SelfDestruct", &[PyRep::Integer(ship_b_low)]).await.expect("SelfDestruct");
    rpc(&ctx, &mut sess_b2, "timeControl", "AdvanceClock", &[PyRep::Integer(125)]).await.expect("AdvanceClock 125");
    let death = ctx
        .drain_notifications_for_char(PILOT_B)
        .into_iter()
        .find(|n| n.event == "OnShipDeath")
        .expect("OnShipDeath emitted");
    assert_eq!(death.id_type, "charid");
    assert!(death.args.is_empty());
}
