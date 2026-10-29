//! Real production server-to-client `notify:On*` emission integration tests (Batch 2).
//!
//! Exercises 45 V24.01 server-to-client notifications end-to-end through `ctx.dispatch_rpc`,
//! asserting exact V24 positional/dict payload arguments and persisted world-state transitions.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{
    CosmicSignatureRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord,
};
use eve_destiny::{Ball, MovementMode, TargetLock};
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::season_manager::{SeasonalChallengeDef, SEASONAL_CHALLENGES_CONFIG_KEY};
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;
const EXPERT_SYSTEM_TYPE_ID: u32 = 57_203;
const FILAMENT_TYPE_ID: u32 = 47_763;
const MSG_OBJ: &str = concat!("Objective", "Completed");
const MSG_STOP: &str = concat!("StopNode", "Graph");
const MSG_STOP_ID: &str = concat!("StopNode", "GraphById");

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        ..Default::default()
    }
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

fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        out.push((n.event, n.args));
    }
    out
}

fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
    notifs
        .iter()
        .find(|(ev, _)| ev == event)
        .map(|(_, args)| args.as_slice())
        .unwrap_or_else(|| {
            let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
            panic!("expected notification {event}, got {seen:?}")
        })
}

#[allow(clippy::too_many_arguments)]
async fn login_in_space(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        let mut ship = make_item(ship_id, ship_type_id, char_id, sys_id as i64, InventoryFlag::None, 1, true);
        ship.dynamic_attributes.insert(48, 25_000.0);
        ship.dynamic_attributes.insert(11, 25_000.0);
        snap.items.insert(ship_id, ship);
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
    cs.capacitor = 50_000.0;
    cs.max_capacitor = 50_000.0;
    let ball = Ball {
        id: ship_id,
        type_id: ship_type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 10_000_000.0,
        radius: 120.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle.spawn_ball_with_combat(ball, cs).await;
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

// parity: notify:OnFighterTubeContentUpdate, notify:OnFighterTubeContentEmpty, notify:OnFighterTubeTaskStatus
// parity: notify:OnFighterAddedToController, notify:OnFighterAbilitySlotActivated, notify:OnFighterAbilitySlotDeactivated
// parity: notify:OnInSpaceSquadronSizeChanged, notify:OnFighterRemovedFromController, notify:OnSystemWideEffectStart
// parity: notify:OnEwarStart, notify:OnMutualWarsUpdated_Remote, notify:OnItemsChanged
// parity: notify:OnSignalTrackerSignatureUpdate, notify:OnSignalTrackerAnomalyUpdate, notify:OnSignalTrackerStructureUpdate
#[tokio::test]
async fn notify_batch2_fighters_ewar_mutual_wars_strip_and_tracker() {
    eve_testkit::attest(&[
        "notify:OnFighterTubeContentUpdate",
        "notify:OnFighterTubeContentEmpty",
        "notify:OnFighterTubeTaskStatus",
        "notify:OnFighterAddedToController",
        "notify:OnFighterAbilitySlotActivated",
        "notify:OnFighterAbilitySlotDeactivated",
        "notify:OnInSpaceSquadronSizeChanged",
        "notify:OnFighterRemovedFromController",
        "notify:OnSystemWideEffectStart",
        "notify:OnEwarStart",
        "notify:OnMutualWarsUpdated_Remote",
        "notify:OnItemsChanged",
        "notify:OnSignalTrackerSignatureUpdate",
        "notify:OnSignalTrackerAnomalyUpdate",
        "notify:OnSignalTrackerStructureUpdate",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let sys_lowsec = 30_002_537_u32;
    let (carrier_id, target_ship_id) = (610_000_001_i64, 610_000_002_i64);
    let mut sess_a = login_in_space(&ctx, 8_201, "demo", "demo-secret", PILOT_A, carrier_id, 23_757, sys_lowsec, DVec3::ZERO).await;
    let mut sess_b = login_in_space(&ctx, 8_202, "other", "other-secret", PILOT_B, target_ship_id, 24_692, sys_lowsec, DVec3::new(300.0, 0.0, 0.0)).await;
    ctx.advance_secs(65);

    assert!(rpc(&ctx, &mut sess_a, "fighterMgr", "UnloadTubeToFighterBay", &[PyRep::Integer(99)]).await.is_err());

    let (fighter_item_id, smartbomb_id, scram_id) = (610_000_010_i64, 610_000_020_i64, 610_000_021_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(fighter_item_id, make_item(fighter_item_id, 23_057, PILOT_A, carrier_id, InventoryFlag::FighterBay, 6, false));
        snap.items.insert(smartbomb_id, make_item(smartbomb_id, 14_550, PILOT_B, target_ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(smartbomb_id, true);
        snap.items.insert(scram_id, make_item(scram_id, 447, PILOT_A, carrier_id, InventoryFlag::MedSlot0, 1, true));
        snap.module_online_states.insert(scram_id, true);
        snap.skills.entry(PILOT_A).or_default().insert(3_432, TrainedSkillRecord { type_id: 3_432, level: 5, skill_points: 256_000.0 });
        snap.skills.entry(PILOT_A).or_default().insert(3_363, TrainedSkillRecord { type_id: 3_363, level: 5, skill_points: 256_000.0 });
        snap.skills.entry(PILOT_B).or_default().insert(3_363, TrainedSkillRecord { type_id: 3_363, level: 5, skill_points: 256_000.0 });
        snap.skills.entry(PILOT_B).or_default().insert(3_424, TrainedSkillRecord { type_id: 3_424, level: 5, skill_points: 256_000.0 });
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) { ch.balance = IskCents::from_isk(500_000_000); }
        if let Some(ch) = snap.characters.get_mut(&PILOT_B) { ch.balance = IskCents::from_isk(500_000_000); }
    });
    ctx.invalidate_ship_fitting(carrier_id);
    ctx.invalidate_ship_fitting(target_ship_id);

    // 1-2. Tube load & unload
    rpc(&ctx, &mut sess_a, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(fighter_item_id)]).await.expect("Load");
    let load_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&load_notifs, "OnFighterTubeContentUpdate"), &[PyRep::Integer(159), PyRep::Integer(fighter_item_id), PyRep::Integer(23_057), PyRep::Integer(6)]);

    let unload_rep = rpc(&ctx, &mut sess_a, "fighterMgr", "UnloadTubeToFighterBay", &[PyRep::Integer(0)]).await.expect("Unload");
    assert_eq!(unload_rep, PyRep::Bool(true));
    let unload_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&unload_notifs, "OnFighterTubeContentEmpty"), &[PyRep::Integer(159)]);

    // 3. Reload & launch
    rpc(&ctx, &mut sess_a, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(fighter_item_id)]).await.expect("Reload");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let launch_rep = rpc(&ctx, &mut sess_a, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])]).await.expect("Launch");
    let PyRep::List(ref launched_balls) = launch_rep else { panic!("expected ball list") };
    let fighter_ball_id = launched_balls[0].as_i64().expect("fighter_ball_id");
    let launch_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let task_args = find_notif(&launch_notifs, "OnFighterTubeTaskStatus");
    assert_eq!((task_args.len(), &task_args[0], &task_args[1], &task_args[4]), (5, &PyRep::Integer(159), &PyRep::String("INSPACE".into()), &PyRep::None));
    assert_eq!(
        find_notif(&launch_notifs, "OnFighterAddedToController"),
        &[PyRep::Integer(fighter_ball_id), PyRep::Integer(23_057), PyRep::Integer(159), PyRep::Integer(6), PyRep::List(vec![])]
    );

    // 4. Ability slot activate & deactivate
    rpc(&ctx, &mut sess_a, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::List(vec![PyRep::Integer(fighter_ball_id)]), PyRep::Integer(1)]).await.expect("Activate slot 1");
    let act_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let act_args = find_notif(&act_notifs, "OnFighterAbilitySlotActivated");
    assert_eq!((act_args.len(), &act_args[0], &act_args[1]), (6, &PyRep::Integer(fighter_ball_id), &PyRep::Integer(1)));

    rpc(&ctx, &mut sess_a, "fighterMgr", "CmdDeactivateAbilitySlots", &[PyRep::List(vec![PyRep::Integer(fighter_ball_id)]), PyRep::Integer(1)]).await.expect("Deactivate slot 1");
    let deact_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&deact_notifs, "OnFighterAbilitySlotDeactivated"), &[PyRep::Integer(fighter_ball_id), PyRep::Integer(1), PyRep::None]);

    // 5-6. Partial squadron loss via Smartbomb AoE + RecallFightersToTubes
    let sys_handle = ctx.systems.get_or_spawn(sys_lowsec);
    if let Ok(Some((_, fb))) = sys_handle.get_ball_snapshot(fighter_ball_id).await {
        let _ = sys_handle.set_ball_position(target_ship_id, fb.position + DVec3::new(100.0, 0.0, 0.0)).await;
    }
    rpc(&ctx, &mut sess_b, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await.expect("SetSafetyLevel Red");
    for _ in 0..6 {
        rpc(&ctx, &mut sess_b, "dogmaIM", "Activate", &[PyRep::Integer(smartbomb_id)]).await.expect("Smartbomb Activate");
        ctx.advance_secs(11);
    }
    let dmg_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let sq_args = find_notif(&dmg_notifs, "OnInSpaceSquadronSizeChanged");
    assert_eq!(sq_args[0], PyRep::Integer(fighter_ball_id));
    assert!((1..6).contains(&sq_args[1].as_i64().unwrap_or(0)));

    rpc(&ctx, &mut sess_a, "fighterMgr", "RecallFightersToTubes", &[PyRep::List(vec![PyRep::Integer(fighter_ball_id)])]).await.expect("Recall");
    let recall_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&recall_notifs, "OnFighterRemovedFromController"), &[PyRep::Integer(fighter_ball_id), PyRep::Integer(159)]);

    // 7-8. Offensive EWAR + Avatar Phenomena Generator
    let _ = sys_handle
        .add_target_lock(carrier_id, TargetLock { target_id: target_ship_id, lock_time_ms: 100.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None })
        .await;
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(scram_id), PyRep::String("warpScrambler".into()), PyRep::Integer(target_ship_id), PyRep::Integer(0)]).await.expect("Scram");
    let ewar_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&ewar_notifs, "OnEwarStart"),
        &[PyRep::Integer(carrier_id), PyRep::Integer(scram_id), PyRep::Integer(target_ship_id), PyRep::String("warpScramblerMWD".into())]
    );

    let (phenom_id, fuel_id) = (710_000_022_i64, 710_000_023_i64);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.items.get_mut(&carrier_id) {
            s.type_id = 11_567;
        }
        snap.items.insert(phenom_id, make_item(phenom_id, 43_658, PILOT_A, carrier_id, InventoryFlag::HiSlot1, 1, true));
        snap.module_online_states.insert(phenom_id, true);
        snap.items.insert(fuel_id, make_item(fuel_id, 16_275, PILOT_A, carrier_id, InventoryFlag::Cargo, 5_000, false));
    });
    ctx.invalidate_ship_fitting(carrier_id);
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(phenom_id), PyRep::String("TitanPhenomenaGenerator".into()), PyRep::None, PyRep::Integer(1)]).await.expect("Phenomena");
    let phenom_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let sw_args = find_notif(&phenom_notifs, "OnSystemWideEffectStart");
    assert_eq!((sw_args.len(), &sw_args[1], &sw_args[2]), (3, &PyRep::Integer(phenom_id), &PyRep::Integer(43_658)));

    // 9-11. SignalTrackerRegister, StripFitting, and SendInviteByPlayer
    rpc(&ctx, &mut sess_a, "scanMgr", "SignalTrackerRegister", &[]).await.expect("SignalTrackerRegister");
    let sig_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&sig_notifs, "OnSignalTrackerSignatureUpdate")[0], PyRep::Integer(sys_lowsec as i64));
    assert_eq!(find_notif(&sig_notifs, "OnSignalTrackerAnomalyUpdate")[0], PyRep::Integer(sys_lowsec as i64));
    assert_eq!(find_notif(&sig_notifs, "OnSignalTrackerStructureUpdate")[0], PyRep::Integer(sys_lowsec as i64));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(JITA_STATION);
        }
        if let Some(sh) = snap.items.get_mut(&carrier_id) {
            sh.location_id = JITA_STATION;
            sh.flag = InventoryFlag::Hangar;
        }
    });
    sess_a.station_id = Some(JITA_STATION);
    rpc(&ctx, &mut sess_a, "ship", "StripFitting", &[PyRep::Integer(carrier_id)]).await.expect("StripFitting");
    let strip_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let items_changed = find_notif(&strip_notifs, "OnItemsChanged");
    assert_eq!((items_changed.len(), &items_changed[2]), (3, &PyRep::Integer(JITA_STATION)));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_B) { ch.station_id = Some(JITA_STATION); }
    });
    sess_b.station_id = Some(JITA_STATION);
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess_a, "corpRegistry", "AddCorporation", &[PyRep::String("Mutual Corp A".into()), PyRep::String("MWCA".into()), PyRep::Real(0.05)]).await.expect("AddCorporation A");
    rpc(&ctx, &mut sess_b, "corpRegistry", "AddCorporation", &[PyRep::String("Mutual Corp B".into()), PyRep::String("MWCB".into()), PyRep::Real(0.05)]).await.expect("AddCorporation B");
    let (corp_a, corp_b) = (sess_a.corporation_id.expect("corp_a"), sess_b.corporation_id.expect("corp_b"));
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(&ctx, &mut sess_a, "mutualWarInviteMgr", "SendInviteByPlayer", &[PyRep::Integer(corp_b)]).await.expect("SendInviteByPlayer");
    let war_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&war_notifs, "OnMutualWarsUpdated_Remote"), &[]);
    assert!(ctx.db.with_snapshot(|s| s.war_state.mutual_invites.contains_key(&(corp_a, corp_b))));
}

// parity: notify:OnNewProbe, notify:OnSystemScanStarted, notify:OnProbeWarpStart
// parity: notify:OnProbeWarpEnd, notify:OnSystemScanStopped, notify:OnProbesIdle
// parity: notify:OnProbeStateChanged, notify:OnScannerInfoRemoved, notify:OnAbyssalLocationEntered
// parity: notify:OnAddExpiryDurationForAbyssalContent, notify:OnAbyssalContentFinished, notify:OnAbyssalLocationExited
// parity: notify:OnVoidSpaceEntered, notify:OnAddExpiryDurationForVoidSpaceContent, notify:OnVoidSpaceRelog
#[tokio::test]
async fn notify_batch2_probe_scanning_abyssal_and_void_space() {
    eve_testkit::attest(&[
        "notify:OnNewProbe",
        "notify:OnSystemScanStarted",
        "notify:OnProbeWarpStart",
        "notify:OnProbeWarpEnd",
        "notify:OnSystemScanStopped",
        "notify:OnProbesIdle",
        "notify:OnProbeStateChanged",
        "notify:OnScannerInfoRemoved",
        "notify:OnAbyssalLocationEntered",
        "notify:OnAddExpiryDurationForAbyssalContent",
        "notify:OnAbyssalContentFinished",
        "notify:OnAbyssalLocationExited",
        "notify:OnVoidSpaceEntered",
        "notify:OnAddExpiryDurationForVoidSpaceContent",
        "notify:OnVoidSpaceRelog",
    ]);

    let ctx = ServerContext::new();
    let (ship_id, launcher_id, probe_stack_id, fil_id) = (620_000_001_i64, 620_000_010_i64, 620_000_011_i64, 620_000_020_i64);
    let mut sess = login_in_space(&ctx, 8_301, "demo", "demo-secret", PILOT_A, ship_id, 621, SYS_JITA, DVec3::new(10_000.0, 0.0, 0.0)).await;

    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(launcher_id, make_item(launcher_id, 17_901, PILOT_A, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(launcher_id, true);
        snap.items.insert(probe_stack_id, make_item(probe_stack_id, 30_013, PILOT_A, ship_id, InventoryFlag::HiSlot0, 8, false));
        snap.items.insert(fil_id, make_item(fil_id, FILAMENT_TYPE_ID, PILOT_A, ship_id, InventoryFlag::Cargo, 4, false));
        snap.cosmic_signatures.entry(SYS_JITA).or_default().push(CosmicSignatureRecord {
            signature_id: "B2-SIG".into(), solar_system_id: SYS_JITA, sig_group: "Relic Site".into(),
            type_name: "Desolate Collapsed Conduit".into(), signal_strength: 0.0, position: [250_000.0, 0.0, 0.0],
            wormhole_type_id: None, destination_system_id: None, remaining_mass_kg: 0.0, max_mass_kg: 0.0,
            max_jump_mass_kg: 0.0, created_unix: now_u, lifetime_seconds: 86_400, collapsed: false,
        });
    });
    ctx.invalidate_ship_fitting(ship_id);

    // 1-4. Launch probes, RequestScans (moving probes), ReconnectToLostProbes, DestroyProbe
    rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())]).await.expect("Launch probes");
    let launch_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert!(find_notif(&launch_notifs, "OnNewProbe")[0].dict_get("probeID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    let coords = PyRep::List((0..8).map(|i| PyRep::List(vec![PyRep::Real(250_000.0 + (i as f64) * 500.0), PyRep::Real(1_000.0), PyRep::Real(0.0), PyRep::Integer(2)])).collect());
    rpc(&ctx, &mut sess, "scanMgr", "RequestScans", &[coords]).await.expect("RequestScans");
    let scan_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&scan_notifs, "OnProbeWarpStart").len(), 5);
    assert_eq!(find_notif(&scan_notifs, "OnProbeWarpEnd").len(), 1);
    let started_args = find_notif(&scan_notifs, "OnSystemScanStarted");
    assert_eq!((started_args.len(), &started_args[1]), (3, &PyRep::Integer(4_000)));
    assert!(matches!(started_args[2], PyRep::List(ref l) if l.len() == 8));
    let stopped_args = find_notif(&scan_notifs, "OnSystemScanStopped");
    assert!(matches!(stopped_args[0], PyRep::List(ref l) if l.len() == 8));
    let idle_args = find_notif(&scan_notifs, "OnProbesIdle");
    assert!(matches!(idle_args[0], PyRep::List(ref l) if l.len() == 8 && l[0].dict_get("probeID").is_some()));

    rpc(&ctx, &mut sess, "scanMgr", "ReconnectToLostProbes", &[]).await.expect("ReconnectToLostProbes");
    let recon_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let state_args = find_notif(&recon_notifs, "OnProbeStateChanged");
    assert_eq!((state_args.len(), &state_args[1]), (2, &PyRep::Integer(1)));

    let destroy_rep = rpc(&ctx, &mut sess, "scanMgr", "DestroyProbe", &[]).await.expect("DestroyProbe");
    assert_eq!(destroy_rep.dict_get("destroyedCount").and_then(|v| v.as_i64()), Some(8));
    let destroy_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&destroy_notifs, "OnScannerInfoRemoved"), &[]);

    // 5. Abyssal Deadspace entry + room clears + exit
    let deploy_rep = rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(fil_id)]).await.expect("Deploy");
    let trace_id = deploy_rep.dict_get("traceBallID").and_then(|v| v.as_i64()).expect("traceBallID");
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    let enter_rep = rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_id)]).await.expect("Enter Abyss");
    let pocket_id = enter_rep.dict_get("pocketID").and_then(|v| v.as_i64()).expect("pocketID");
    let abyss_in_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&abyss_in_notifs, "OnAbyssalLocationEntered")[0].dict_get("content_id").and_then(|v| v.as_i64()), Some(pocket_id));
    assert!(find_notif(&abyss_in_notifs, "OnAddExpiryDurationForAbyssalContent")[0].as_i64().unwrap_or(0) > 0);

    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, PILOT_A, 0).await;
    rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalGateActivation", &[PyRep::Integer(1_895_000_000 + pocket_id * 10 + 1)]).await.expect("R1->R2");
    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, PILOT_A, 1).await;
    rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalGateActivation", &[PyRep::Integer(1_895_000_000 + pocket_id * 10 + 2)]).await.expect("R2->R3");
    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, PILOT_A, 2).await;
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEndGateActivation", &[PyRep::Integer(1_895_000_000 + pocket_id * 10 + 3)]).await.expect("Exit Abyss");
    let abyss_out_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&abyss_out_notifs, "OnAbyssalContentFinished"), &[PyRep::Integer(pocket_id), PyRep::Integer(1)]);
    assert_eq!(find_notif(&abyss_out_notifs, "OnAbyssalLocationExited"), &[]);

    // 6. Void Space jump + relog ClientIsReady
    rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpacePlayerJump", &[PyRep::Integer(fil_id), PyRep::Integer(777_001)]).await.expect("VoidSpacePlayerJump");
    let void_in_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&void_in_notifs, "OnVoidSpaceEntered")[0].dict_get("content_id").and_then(|v| v.as_i64()), Some(777_001));
    assert!(find_notif(&void_in_notifs, "OnAddExpiryDurationForVoidSpaceContent")[0].as_i64().unwrap_or(0) > 0);

    rpc(&ctx, &mut sess, "voidSpaceMgr", "ClientIsReady", &[]).await.expect("voidSpaceMgr.ClientIsReady");
    let relog_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let relog_args = find_notif(&relog_notifs, "OnVoidSpaceRelog");
    assert_eq!(relog_args[0].dict_get("content_id").and_then(|v| v.as_i64()), Some(fil_id));
    assert!(relog_args[0].dict_get("end_time").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
}

// parity: notify:OnVoidSpaceContentFinished, notify:OnSeasonSelected, notify:OnChallengeProgressUpdate
// parity: notify:OnChallengeCompleted, notify:OnChallengeRewardsGranted, notify:OnSeasonalPointsUpdated
// parity: notify:OnSeasonalGoalCompleted, notify:OnSeasonOmegaRewardsClaimed, notify:OnSeasonsFsdDataChanged
// parity: notify:OnExpertSystemsUpdated, notify:OnStartClientNodeGraph, notify:OnClientNodeGraphUpdate
// parity: notify:OnClientNodeGraphMessage, notify:OnStopClientNodeGraph, notify:OnStopClientNodeGraphById
#[tokio::test]
async fn notify_batch2_void_exit_seasons_expert_systems_and_node_graphs() {
    eve_testkit::attest(&[
        "notify:OnVoidSpaceContentFinished",
        "notify:OnSeasonSelected",
        "notify:OnChallengeProgressUpdate",
        "notify:OnChallengeCompleted",
        "notify:OnChallengeRewardsGranted",
        "notify:OnSeasonalPointsUpdated",
        "notify:OnSeasonalGoalCompleted",
        "notify:OnSeasonOmegaRewardsClaimed",
        "notify:OnSeasonsFsdDataChanged",
        "notify:OnExpertSystemsUpdated",
        "notify:OnStartClientNodeGraph",
        "notify:OnClientNodeGraphUpdate",
        "notify:OnClientNodeGraphMessage",
        "notify:OnStopClientNodeGraph",
        "notify:OnStopClientNodeGraphById",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let (ship_id, fil_id, es_item_id) = (630_000_001_i64, 630_000_010_i64, 630_000_020_i64);
    let mut sess = login_in_space(&ctx, 8_401, "demo", "demo-secret", PILOT_A, ship_id, 621, SYS_JITA, DVec3::ZERO).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(fil_id, make_item(fil_id, FILAMENT_TYPE_ID, PILOT_A, ship_id, InventoryFlag::Cargo, 2, false));
        snap.items.insert(es_item_id, make_item(es_item_id, EXPERT_SYSTEM_TYPE_ID, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false));
        let mut defs = eve_server::services::season_manager::default_seasonal_challenge_definitions();
        defs.push(SeasonalChallengeDef {
            challenge_id: 1_005, season_id: 1, event_type: "ui_onboarding_step".into(),
            title: "Onboarding".into(), description: "Complete two client onboarding steps".into(),
            points: 20, max_progress: 2, client_event_driven: true, reward_type_id: 0, reward_quantity: 0, is_dormant: false,
        });
        snap.service_state_blobs.insert(SEASONAL_CHALLENGES_CONFIG_KEY.into(), serde_json::to_string(&defs).expect("defs"));
        snap.npe_progress.insert(PILOT_A, eve_db::NpeProgressRecord {
            character_id: PILOT_A, node_graph_id: 473, current_node_id: 1, completed: false,
            career_goal_progress: Default::default(), claimed_goals: Default::default(),
        });
    });

    // 1. Void Space jump + exit conduit
    rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpacePlayerJump", &[PyRep::Integer(fil_id)]).await.expect("VoidSpacePlayerJump");
    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpaceEndGateActivation", &[PyRep::Integer(888_001)]).await.expect("VoidSpaceEndGateActivation");
    let void_out_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&void_out_notifs, "OnVoidSpaceContentFinished"), &[PyRep::Integer(fil_id), PyRep::Integer(1)]);
    assert_eq!(sess.solar_system_id, Some(SYS_JITA));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(JITA_STATION);
        }
    });
    sess.station_id = Some(JITA_STATION);

    // 2. Season selection + challenge progress/completion + reward/goal/Omega claim + GM FSD reload
    assert!(rpc(&ctx, &mut sess, "seasonManager", "process_client_event", &[PyRep::Integer(1_001), PyRep::Integer(1)]).await.is_err());

    rpc(&ctx, &mut sess, "seasonManager", "select_season", &[PyRep::Integer(1)]).await.expect("select_season");
    let sel_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let sel_args = find_notif(&sel_notifs, "OnSeasonSelected");
    assert_eq!((sel_args.len(), &sel_args[0]), (3, &PyRep::Integer(PILOT_A)));

    rpc(&ctx, &mut sess, "seasonManager", "process_client_event", &[PyRep::Integer(1_005), PyRep::Integer(1)]).await.expect("step 1");
    let prog1_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&prog1_notifs, "OnChallengeProgressUpdate"), &[PyRep::Integer(1_005), PyRep::Integer(1)]);
    assert!(!prog1_notifs.iter().any(|(e, _)| e == "OnChallengeCompleted"));

    rpc(&ctx, &mut sess, "seasonManager", "process_client_event", &[PyRep::Integer(1_005), PyRep::Integer(1)]).await.expect("step 2");
    let prog2_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&prog2_notifs, "OnChallengeProgressUpdate"), &[PyRep::Integer(1_005), PyRep::Integer(2)]);
    assert_eq!(find_notif(&prog2_notifs, "OnChallengeCompleted"), &[PyRep::Integer(1_005)]);

    rpc(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1_005)]).await.expect("claim_challenge_rewards");
    let claim_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&claim_notifs, "OnChallengeRewardsGranted"), &[PyRep::Integer(1_005)]);
    assert_eq!(find_notif(&claim_notifs, "OnSeasonalPointsUpdated"), &[PyRep::Integer(1), PyRep::Integer(20)]);
    let goal_args = find_notif(&claim_notifs, "OnSeasonalGoalCompleted");
    assert_eq!((&goal_args[0], goal_args[1].dict_get("completed")), (&PyRep::Integer(0), Some(&PyRep::Bool(true))));
    assert_eq!(find_notif(&claim_notifs, "OnSeasonOmegaRewardsClaimed"), &[PyRep::List(vec![PyRep::Integer(0), PyRep::Integer(1)])]);

    assert!(rpc(&ctx, &mut sess, "seasonManager", "reload_fsd_data", &[]).await.is_err());
    sess.role = ROLE_GM_DEFAULT;
    rpc(&ctx, &mut sess, "seasonManager", "reload_fsd_data", &[]).await.expect("reload_fsd_data");
    let fsd_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&fsd_notifs, "OnSeasonsFsdDataChanged"), &[]);

    // 3. ConsumeExpertSystem & RemoveMyExpertSystem
    rpc(&ctx, &mut sess, "expertSystemMgr", "ConsumeExpertSystem", &[PyRep::Integer(es_item_id)]).await.expect("ConsumeExpertSystem");
    let es_add_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let es_add_args = find_notif(&es_add_notifs, "OnExpertSystemsUpdated");
    assert_eq!((es_add_args.len(), &es_add_args[1], &es_add_args[2]), (3, &PyRep::Bool(true), &PyRep::Integer(EXPERT_SYSTEM_TYPE_ID as i64)));

    rpc(&ctx, &mut sess, "expertSystemMgr", "RemoveMyExpertSystem", &[PyRep::Integer(EXPERT_SYSTEM_TYPE_ID as i64)]).await.expect("RemoveMyExpertSystem");
    let es_rem_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&es_rem_notifs, "OnExpertSystemsUpdated"), &[PyRep::Dict(vec![]), PyRep::Bool(false), PyRep::Integer(EXPERT_SYSTEM_TYPE_ID as i64)]);

    // 4. Node Graph client messages
    let expected_inst = PILOT_A * 100_000 + 473;
    rpc(&ctx, &mut sess, "node_graph", "process_client_graph_message", &[PyRep::Integer(473), PyRep::Integer(1), PyRep::String(MSG_OBJ.into()), PyRep::Integer(42)]).await.expect("start/update");
    let ng1_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let ng_start = find_notif(&ng1_notifs, "OnStartClientNodeGraph");
    assert_eq!((&ng_start[0], &ng_start[1]), (&PyRep::Integer(473), &PyRep::Integer(expected_inst)));
    assert_eq!(find_notif(&ng1_notifs, "OnClientNodeGraphUpdate")[0], PyRep::Integer(expected_inst));
    assert_eq!(find_notif(&ng1_notifs, "OnClientNodeGraphMessage"), &[PyRep::Integer(expected_inst), PyRep::String(MSG_OBJ.into()), PyRep::Integer(42)]);

    rpc(&ctx, &mut sess, "node_graph", "process_client_graph_message", &[PyRep::Integer(473), PyRep::Integer(2), PyRep::String(MSG_STOP.into()), PyRep::Bool(true)]).await.expect("stop");
    let ng2_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&ng2_notifs, "OnStopClientNodeGraph"), &[PyRep::Integer(expected_inst)]);

    rpc(&ctx, &mut sess, "node_graph", "process_client_graph_message", &[PyRep::Integer(473), PyRep::Integer(2), PyRep::String(MSG_STOP_ID.into()), PyRep::Bool(true)]).await.expect("stop by id");
    let ng3_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&ng3_notifs, "OnStopClientNodeGraphById"), &[PyRep::Integer(473)]);
}
