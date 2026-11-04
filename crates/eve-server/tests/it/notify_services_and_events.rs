//! Real production server-to-client `notify:On*` emission integration tests (Batch 2).
//!
//! Exercises 32 V24.01 server-to-client notifications end-to-end through `ctx.dispatch_rpc`,
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
const FILAMENT_TYPE_ID: u32 = 56_134; // Tranquil Firestorm Filament (T0, group 1979)

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
    let mut sess_a = login_in_space(&ctx, 8_201, "demo", "demo-secret", PILOT_A, carrier_id, 23_913, sys_lowsec, DVec3::ZERO).await;
    let mut sess_b = login_in_space(&ctx, 8_202, "other", "other-secret", PILOT_B, target_ship_id, 24_692, sys_lowsec, DVec3::new(300.0, 0.0, 0.0)).await;
    ctx.advance_secs(65);

    assert!(rpc(&ctx, &mut sess_a, "fighterMgr", "UnloadTubeToFighterBay", &[PyRep::Integer(99)]).await.is_err());

    let (fighter_item_id, bomber_item_id, smartbomb_id, scram_id, ecm_id, neut_id) = (
        610_000_010_i64, 610_000_011_i64, 610_000_020_i64, 610_000_021_i64, 610_000_022_i64, 610_000_023_i64,
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(fighter_item_id, make_item(fighter_item_id, 23_057, PILOT_A, carrier_id, InventoryFlag::FighterBay, 6, false));
        snap.items.insert(bomber_item_id, make_item(bomber_item_id, 40_362, PILOT_A, carrier_id, InventoryFlag::FighterBay, 6, false));
        snap.items.insert(smartbomb_id, make_item(smartbomb_id, 14_550, PILOT_B, target_ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(smartbomb_id, true);
        snap.items.insert(scram_id, make_item(scram_id, 447, PILOT_A, carrier_id, InventoryFlag::MedSlot0, 1, true));
        snap.module_online_states.insert(scram_id, true);
        snap.items.insert(ecm_id, make_item(ecm_id, 1_957, PILOT_A, carrier_id, InventoryFlag::MedSlot1, 1, true));
        snap.module_online_states.insert(ecm_id, true);
        snap.items.insert(neut_id, make_item(neut_id, 533, PILOT_A, carrier_id, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(neut_id, true);
        for (sid, lvl) in [(3_432, 5), (3_363, 5), (40_535, 5), (40_537, 5), (32_339, 5), (3_426, 5), (3_427, 5)] {
            snap.skills.entry(PILOT_A).or_default().insert(sid, TrainedSkillRecord { type_id: sid, level: lvl, skill_points: 256_000.0 });
        }
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

    // 3. Reload light & heavy bomber squadrons and launch
    rpc(&ctx, &mut sess_a, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(fighter_item_id)]).await.expect("Reload 0");
    rpc(&ctx, &mut sess_a, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(1), PyRep::Integer(bomber_item_id)]).await.expect("Load 1");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let launch_rep = rpc(&ctx, &mut sess_a, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0), PyRep::Integer(1)])]).await.expect("Launch");
    let PyRep::List(ref launched_balls) = launch_rep else { panic!("expected ball list") };
    let fighter_ball_id = launched_balls[0].as_i64().expect("fighter_ball_id");
    let bomber_ball_id = launched_balls[1].as_i64().expect("bomber_ball_id");
    let launch_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let task_args = find_notif(&launch_notifs, "OnFighterTubeTaskStatus");
    assert_eq!((task_args.len(), &task_args[0], &task_args[1], &task_args[4]), (5, &PyRep::Integer(159), &PyRep::String("INSPACE".into()), &PyRep::None));
    assert_eq!(
        find_notif(&launch_notifs, "OnFighterAddedToController"),
        &[PyRep::Integer(fighter_ball_id), PyRep::Integer(23_057), PyRep::Integer(159), PyRep::Integer(6), PyRep::List(vec![])]
    );

    // 4. Ability slot activate (unit + point-targeted bomb) & deactivate
    rpc(&ctx, &mut sess_a, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::List(vec![PyRep::Integer(fighter_ball_id)]), PyRep::Integer(1)]).await.expect("Activate slot 1");
    let act_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let act_args = find_notif(&act_notifs, "OnFighterAbilitySlotActivated");
    assert_eq!((act_args.len(), &act_args[0], &act_args[1]), (6, &PyRep::Integer(fighter_ball_id), &PyRep::Integer(1)));

    rpc(&ctx, &mut sess_a, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::Integer(bomber_ball_id), PyRep::Integer(2), PyRep::Tuple(vec![PyRep::Real(5_000.0), PyRep::Real(0.0), PyRep::Real(0.0)])]).await.expect("Activate point bomb");
    let pt_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let pt_args = find_notif(&pt_notifs, "OnFighterAbilitySlotActivated");
    assert_eq!((pt_args.len(), &pt_args[0], &pt_args[1]), (6, &PyRep::Integer(bomber_ball_id), &PyRep::Integer(2)));

    rpc(&ctx, &mut sess_a, "fighterMgr", "CmdDeactivateAbilitySlots", &[PyRep::List(vec![PyRep::Integer(fighter_ball_id)]), PyRep::Integer(1)]).await.expect("Deactivate slot 1");
    let deact_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&deact_notifs, "OnFighterAbilitySlotDeactivated"), &[PyRep::Integer(fighter_ball_id), PyRep::Integer(1), PyRep::None]);

    // 5. Offensive EWAR negative gates (0 cap + Safety Green against non-suspect pilot in lowsec)
    let sys_handle = ctx.systems.get_or_spawn(sys_lowsec);
    let _ = sys_handle
        .add_target_lock(carrier_id, TargetLock { target_id: target_ship_id, lock_time_ms: 100.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None })
        .await;
    let _ = sys_handle.set_capacitor(carrier_id, 0.0).await;
    assert!(rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(scram_id), PyRep::String("warpScrambler".into()), PyRep::Integer(target_ship_id), PyRep::Integer(0)]).await.is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A).iter().any(|(e, _)| e == "OnEwarStart"));

    let _ = sys_handle.set_capacitor(carrier_id, 50_000.0).await;
    rpc(&ctx, &mut sess_a, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(2)]).await.expect("SetSafetyLevel Green");
    assert!(rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(scram_id), PyRep::String("warpScrambler".into()), PyRep::Integer(target_ship_id), PyRep::Integer(0)]).await.is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A).iter().any(|(e, _)| e == "OnEwarStart"));

    // 6-7. Partial squadron loss via Smartbomb AoE + RecallFightersToTubes
    if let Ok(Some((_, fb))) = sys_handle.get_ball_snapshot(fighter_ball_id).await {
        let _ = sys_handle.set_ball_position(target_ship_id, fb.position + DVec3::new(100.0, 0.0, 0.0)).await;
    }
    rpc(&ctx, &mut sess_b, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await.expect("SetSafetyLevel Red");
    for _ in 0..6 {
        rpc(&ctx, &mut sess_b, "dogmaIM", "Activate", &[PyRep::Integer(smartbomb_id)]).await.expect("Smartbomb Activate");
        ctx.advance_secs(11);
    }
    let dmg_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let sq_args = dmg_notifs
        .iter()
        .find(|(e, a)| e == "OnInSpaceSquadronSizeChanged" && a.first() == Some(&PyRep::Integer(fighter_ball_id)))
        .map(|(_, a)| a.as_slice())
        .expect("missing OnInSpaceSquadronSizeChanged for fighter_ball_id");
    assert!((1..6).contains(&sq_args[1].as_i64().unwrap_or(0)));

    rpc(&ctx, &mut sess_a, "fighterMgr", "RecallFightersToTubes", &[PyRep::List(vec![PyRep::Integer(fighter_ball_id), PyRep::Integer(bomber_ball_id)])]).await.expect("Recall");
    let recall_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(recall_notifs.iter().any(|(e, a)| e == "OnFighterRemovedFromController" && a.as_slice() == [PyRep::Integer(fighter_ball_id), PyRep::Integer(159)]));

    // 8. Positive offensive EWAR (scram, ECM, neut) + Phenomena Generator
    rpc(&ctx, &mut sess_a, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await.expect("SetSafetyLevel Red");
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(scram_id), PyRep::String("warpScrambler".into()), PyRep::Integer(target_ship_id), PyRep::Integer(0)]).await.expect("Scram");
    assert_eq!(
        find_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnEwarStart"),
        &[PyRep::Integer(carrier_id), PyRep::Integer(scram_id), PyRep::Integer(target_ship_id), PyRep::String("warpScramblerMWD".into())]
    );
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(ecm_id), PyRep::String("targetHostile".into()), PyRep::Integer(target_ship_id), PyRep::Integer(0)]).await.expect("ECM");
    assert_eq!(
        find_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnEwarStart"),
        &[PyRep::Integer(carrier_id), PyRep::Integer(ecm_id), PyRep::Integer(target_ship_id), PyRep::String("electronic".into())]
    );
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(neut_id), PyRep::String("energyNeutralizerFalloff".into()), PyRep::Integer(target_ship_id), PyRep::Integer(0)]).await.expect("Neut");
    assert_eq!(
        find_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnEwarStart"),
        &[PyRep::Integer(carrier_id), PyRep::Integer(neut_id), PyRep::Integer(target_ship_id), PyRep::String("ewEnergyNeut".into())]
    );

    let (phenom_id, fuel_id) = (710_000_022_i64, 710_000_023_i64);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.items.get_mut(&carrier_id) { s.type_id = 11_567; }
        snap.items.insert(phenom_id, make_item(phenom_id, 43_658, PILOT_A, carrier_id, InventoryFlag::HiSlot1, 1, true));
        snap.module_online_states.insert(phenom_id, true);
        snap.items.insert(fuel_id, make_item(fuel_id, 16_275, PILOT_A, carrier_id, InventoryFlag::Cargo, 5_000, false));
    });
    ctx.invalidate_ship_fitting(carrier_id);
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(phenom_id), PyRep::String("TitanPhenomenaGenerator".into()), PyRep::None, PyRep::Integer(1)]).await.expect("Phenomena");
    let phenom_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let sw_args = find_notif(&phenom_notifs, "OnSystemWideEffectStart");
    assert_eq!((sw_args.len(), &sw_args[1], &sw_args[2]), (3, &PyRep::Integer(phenom_id), &PyRep::Integer(43_658)));

    // 9. SignalTrackerRegister (zero initial spam) + real signature/anomaly/structure delta updates
    rpc(&ctx, &mut sess_a, "scanMgr", "SignalTrackerRegister", &[]).await.expect("SignalTrackerRegister");
    let reg_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!reg_notifs.iter().any(|(e, _)| matches!(
        e.as_str(),
        "OnSignalTrackerSignatureUpdate" | "OnSignalTrackerAnomalyUpdate" | "OnSignalTrackerStructureUpdate"
    )));

    eve_server::services::pve_exploration_seeding::ensure_system_pve_sites_seeded(&ctx, sys_lowsec).await;
    let seed_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let anom_add = find_notif(&seed_notifs, "OnSignalTrackerAnomalyUpdate");
    assert_eq!(anom_add[0], PyRep::Integer(sys_lowsec as i64));
    assert!(matches!(&anom_add[1], PyRep::Dict(d) if !d.is_empty() && d[0].1.dict_get("instanceID").is_some()));

    ctx.db.with_mut_snapshot(|s| { let mut d = s.cosmic_signatures[&sys_lowsec][0].clone(); d.signature_id = "STA-100".into(); if let Some(wh) = eve_server::services::space_destiny::build_static_replacement_wormhole(&ctx, sys_lowsec, &d, ctx.now_unix(), 1) { s.cosmic_signatures.get_mut(&sys_lowsec).unwrap().push(wh); } });
    ctx.advance_secs(90_000);
    rpc(&ctx, &mut sess_a, "scanMgr", "GetSignatures", &[PyRep::Integer(sys_lowsec as i64)]).await.expect("GetSignatures");
    let exp_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let sig_upd = find_notif(&exp_notifs, "OnSignalTrackerSignatureUpdate");
    assert_eq!(sig_upd[0], PyRep::Integer(sys_lowsec as i64));
    let PyRep::Dict(ref sig_dict) = sig_upd[1] else { panic!("expected dict") };
    let sig_kv = sig_dict
        .iter()
        .map(|(_, v)| v)
        .find(|v| v.dict_get("targetID").is_some())
        .expect("expected added signature with targetID");
    let dev = sig_kv.dict_get("deviation").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(dev > 0.0, "unscanned signature must report positive deviation");
    let reported_pos = match sig_kv.dict_get("position") {
        Some(PyRep::Tuple(t)) => [t[0].as_f64().unwrap(), t[1].as_f64().unwrap(), t[2].as_f64().unwrap()],
        _ => panic!("expected position tuple"),
    };
    let target_id = sig_kv.dict_get("targetID").and_then(|v| v.as_str()).unwrap();
    let sig_rec = ctx.db.with_snapshot(|s| s.cosmic_signatures.get(&sys_lowsec).and_then(|sigs| sigs.iter().find(|r| r.signature_id == target_id).cloned())).unwrap();
    let true_pos = sig_rec.position;
    let dist = ((reported_pos[0] - true_pos[0]).powi(2) + (reported_pos[1] - true_pos[1]).powi(2) + (reported_pos[2] - true_pos[2]).powi(2)).sqrt();
    assert!(dist <= dev + 1.0, "offset distance {dist} must be <= deviation {dev}");
    assert!(dist >= 0.05 * dev, "offset distance {dist} must be non-zero");
    // Verify offset cannot be reversed from public targetID + deviation alone
    let h = target_id.bytes().fold(14695981039346656037_u64, |a, b| (a ^ (b as u64)).wrapping_mul(1099511628211));
    let naive_x = reported_pos[0] - (((h & 0xFFFF) as f64 / 65535.0) * 2.0 - 1.0) * dev * 0.75;
    assert_ne!(naive_x, true_pos[0], "true position must not be derivable from targetID and deviation");
    // Verify CSPRNG non-determinism across draws:
    let kv2 = eve_server::services::fleet_social::build_signal_tracker_signature_kv(&ctx.sde, &sig_rec);
    let pos2 = match kv2.dict_get("position") {
        Some(PyRep::Tuple(t)) => [t[0].as_f64().unwrap(), t[1].as_f64().unwrap(), t[2].as_f64().unwrap()],
        _ => panic!("expected position tuple"),
    };
    let dist2 = ((pos2[0] - true_pos[0]).powi(2) + (pos2[1] - true_pos[1]).powi(2) + (pos2[2] - true_pos[2]).powi(2)).sqrt();
    assert!(dist2 <= dev + 1.0, "second draw distance {dist2} must be <= deviation {dev}");
    assert_ne!(reported_pos, pos2, "subsequent reports must draw unpredictable CSPRNG offsets");
    assert!(matches!(&sig_upd[2], PyRep::List(l) if !l.is_empty()));
    let anom_rem = find_notif(&exp_notifs, "OnSignalTrackerAnomalyUpdate");
    assert!(matches!(&anom_rem[2], PyRep::List(l) if !l.is_empty()));

    // 10. Player corp creation, Upwell structure Anchor + Unanchor SignalTrackerStructureUpdate, StripFitting, and SendInviteByPlayer
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) { ch.station_id = Some(JITA_STATION); }
        if let Some(ch) = snap.characters.get_mut(&PILOT_B) { ch.station_id = Some(JITA_STATION); }
        if let Some(sh) = snap.items.get_mut(&carrier_id) { sh.location_id = JITA_STATION; sh.flag = InventoryFlag::Hangar; }
    });
    sess_a.station_id = Some(JITA_STATION);
    sess_b.station_id = Some(JITA_STATION);

    rpc(&ctx, &mut sess_a, "ship", "StripFitting", &[PyRep::Integer(carrier_id)]).await.expect("StripFitting");
    let strip_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let items_changed_events: Vec<&[PyRep]> = strip_notifs.iter().filter(|(e, _)| e == "OnItemsChanged").map(|(_, a)| a.as_slice()).collect();
    assert!(items_changed_events.len() >= 3, "expected per-slot OnItemsChanged groups, got {}", items_changed_events.len());
    let mut seen_flags = Vec::new();
    for ev_args in &items_changed_events {
        assert_eq!((ev_args.len(), &ev_args[2]), (3, &PyRep::Integer(JITA_STATION)));
        let PyRep::List(ref kvs) = ev_args[0] else { panic!("expected item list") };
        assert_eq!(kvs[0].dict_get("categoryID"), Some(&PyRep::Integer(7)));
        assert!(kvs[0].dict_get("groupID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
        assert_eq!(kvs[0].dict_get("singleton"), Some(&PyRep::Integer(1)));
        assert_eq!(kvs[0].dict_get("stacksize"), Some(&PyRep::Integer(1)));
        assert!(kvs[0].dict_get("customInfo").is_some());
        let PyRep::Dict(ref chg) = ev_args[1] else { panic!("expected change dict") };
        let prev_flag = chg.iter().find(|(k, _)| k == &PyRep::Integer(4)).and_then(|(_, v)| v.as_i64()).expect("ixFlag");
        seen_flags.push(prev_flag);
    }
    assert!(seen_flags.contains(&19) && seen_flags.contains(&27), "expected MedSlot0(19) and HiSlot0(27), got {seen_flags:?}");

    ctx.advance_secs(12);
    rpc(&ctx, &mut sess_a, "corpRegistry", "AddCorporation", &[PyRep::String("Mutual Corp A".into()), PyRep::String("MWCA".into()), PyRep::Real(0.05)]).await.expect("AddCorporation A");
    rpc(&ctx, &mut sess_b, "corpRegistry", "AddCorporation", &[PyRep::String("Mutual Corp B".into()), PyRep::String("MWCB".into()), PyRep::Real(0.05)]).await.expect("AddCorporation B");
    ctx.advance_secs(7 * 86_400);
    let (corp_a, corp_b) = (sess_a.corporation_id.expect("corp_a"), sess_b.corporation_id.expect("corp_b"));

    // Return PILOT_A to space in sys_lowsec far from celestials to anchor, core, & unanchor an Astrahus
    let open_pos = DVec3::new(0.0, 5.0e9, 0.0);
    let (citadel_item_id, core_item_id) = (710_000_090_i64, 710_000_091_i64);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) { ch.station_id = None; ch.solar_system_id = sys_lowsec; }
        if let Some(sh) = snap.items.get_mut(&carrier_id) { sh.type_id = 650; sh.location_id = sys_lowsec as i64; sh.flag = InventoryFlag::None; }
        snap.items.insert(citadel_item_id, make_item(citadel_item_id, 35_832, PILOT_A, carrier_id, InventoryFlag::Cargo, 1, false));
        snap.items.insert(core_item_id, make_item(core_item_id, 56_201, PILOT_A, carrier_id, InventoryFlag::Cargo, 1, false));
    });
    ctx.invalidate_ship_fitting(carrier_id);
    sess_a.station_id = None;
    sess_a.solar_system_id = Some(sys_lowsec);
    let _ = sys_handle.set_ball_position(carrier_id, open_pos).await;
    let _ = sys_handle.remove_module_cycle(carrier_id, phenom_id).await;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    let anchor_rep = rpc(&ctx, &mut sess_a, "structureDeployment", "Anchor", &[PyRep::Integer(35_832), PyRep::String("Tracker Citadel".into())]).await.expect("Anchor");
    let structure_id = anchor_rep.dict_get("structureID").and_then(|v| v.as_i64()).expect("structureID");
    let anchor_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let st_add = find_notif(&anchor_notifs, "OnSignalTrackerStructureUpdate");
    assert_eq!((&st_add[0], &st_add[2]), (&PyRep::Integer(sys_lowsec as i64), &PyRep::List(vec![])));
    assert!(matches!(&st_add[1], PyRep::Dict(d) if d.len() == 1 && d[0].0 == PyRep::Integer(structure_id) && d[0].1.dict_get("categoryID") == Some(&PyRep::Integer(65))));

    ctx.advance_secs(25 * 3_600);
    let _ = sys_handle.set_ball_position(carrier_id, open_pos + DVec3::new(60_000.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut sess_a, "structureDocking", "Dock", &[PyRep::Integer(structure_id)]).await.expect("Dock Citadel");
    rpc(&ctx, &mut sess_a, "invbroker", "Add", &[PyRep::Integer(core_item_id), PyRep::Integer(structure_id), PyRep::Integer(172)]).await.expect("Insert Core");
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess_a, "structureDocking", "Undock", &[]).await.expect("Undock Citadel");
    ctx.advance_secs(1_800);
    let _ = rpc(&ctx, &mut sess_a, "structureDirectory", "GetStructures", &[]).await;
    rpc(&ctx, &mut sess_a, "structureDeployment", "Unanchor", &[PyRep::Integer(structure_id)]).await.expect("Unanchor");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    ctx.advance_secs(7 * 86_400 + 60);
    let _ = rpc(&ctx, &mut sess_a, "structureDirectory", "GetStructures", &[]).await;
    let unanchor_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&unanchor_notifs, "OnSignalTrackerStructureUpdate"),
        &[PyRep::Integer(sys_lowsec as i64), PyRep::Dict(vec![]), PyRep::List(vec![PyRep::Integer(structure_id)])]
    );

    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(&ctx, &mut sess_a, "mutualWarInviteMgr", "SendInviteByPlayer", &[PyRep::Integer(corp_b)]).await.expect("SendInviteByPlayer");
    let war_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&war_notifs, "OnMutualWarsUpdated_Remote"), &[]);
    assert!(ctx.db.with_snapshot(|s| s.war_state.mutual_invites.contains_key(&(corp_a, corp_b))));
}

// parity: notify:OnNewProbe, notify:OnSystemScanStarted, notify:OnProbeWarpStart
// parity: notify:OnSystemScanStopped, notify:OnProbesIdle, notify:OnProbeStateChanged
// parity: notify:OnScannerInfoRemoved, notify:OnAddExpiryDurationForAbyssalContent
// parity: notify:OnAbyssalLocationExited, notify:OnAddExpiryDurationForVoidSpaceContent
#[tokio::test]
async fn notify_batch2_probe_scanning_abyssal_and_void_space() {
    eve_testkit::attest(&[
        "notify:OnNewProbe",
        "notify:OnSystemScanStarted",
        "notify:OnProbeWarpStart",
        "notify:OnSystemScanStopped",
        "notify:OnProbesIdle",
        "notify:OnProbeStateChanged",
        "notify:OnScannerInfoRemoved",
        "notify:OnAddExpiryDurationForAbyssalContent",
        "notify:OnAbyssalLocationExited",
        "notify:OnAddExpiryDurationForVoidSpaceContent",
    ]);

    let ctx = ServerContext::new();
    let jita_dock_pos = ctx.sde.stations[&JITA_STATION].position + DVec3::new(500.0, 0.0, 0.0);
    let (ship_id, launcher_id, probe_stack_id, fil_id) = (620_000_001_i64, 620_000_010_i64, 620_000_011_i64, 620_000_020_i64);
    let mut sess = login_in_space(&ctx, 8_301, "demo", "demo-secret", PILOT_A, ship_id, 621, SYS_JITA, jita_dock_pos).await;

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

    // 1-4. Launch probes, RequestScans, Dock+Undock disconnect -> ReconnectToLostProbes, DestroyProbe
    rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())]).await.expect("Launch probes");
    let launch_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert!(find_notif(&launch_notifs, "OnNewProbe")[0].dict_get("probeID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    let coords = PyRep::List((0..8).map(|i| PyRep::List(vec![PyRep::Real(250_000.0 + (i as f64) * 500.0), PyRep::Real(1_000.0), PyRep::Real(0.0), PyRep::Integer(2)])).collect());
    rpc(&ctx, &mut sess, "scanMgr", "RequestScans", &[coords]).await.expect("RequestScans");
    let scan_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&scan_notifs, "OnProbeWarpStart").len(), 5);
    assert!(!scan_notifs.iter().any(|(e, _)| e == "OnProbeWarpEnd"));
    assert!(!scan_notifs.iter().any(|(e, _)| e == "OnSystemScanStopped"));
    let started_args = find_notif(&scan_notifs, "OnSystemScanStarted");
    assert_eq!(started_args.len(), 3);
    assert!(matches!(started_args[2], PyRep::List(ref l) if l.len() == 8));
    ctx.advance_secs(15);
    let post_scan_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(post_scan_notifs.iter().filter(|(e, _)| e == "OnProbeWarpEnd").count(), 8);
    assert!(post_scan_notifs.iter().any(|(e, _)| e == "OnProbeStateChanged"));
    let stopped_args = find_notif(&post_scan_notifs, "OnSystemScanStopped");
    assert!(matches!(stopped_args[0], PyRep::List(ref l) if l.len() == 8));
    let idle_args = find_notif(&post_scan_notifs, "OnProbesIdle");
    assert!(matches!(idle_args[0], PyRep::List(ref l) if l.len() == 8 && l[0].dict_get("probeID").is_some()));

    // Already-connected probes must NOT emit OnProbeStateChanged on ReconnectToLostProbes
    let noop_recon = rpc(&ctx, &mut sess, "scanMgr", "ReconnectToLostProbes", &[]).await.expect("Noop Reconnect");
    assert_eq!(noop_recon.dict_get("reconnectedCount"), Some(&PyRep::Integer(0)));
    assert!(!drain_all(&ctx, &mut sess, PILOT_A).iter().any(|(e, _)| e == "OnProbeStateChanged"));

    // Dock disconnects active probes; Undock + ReconnectToLostProbes reconnects them
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(JITA_STATION)]).await.expect("Dock");
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "stationSvc", "Undock", &[]).await.expect("Undock");
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    let recon_rep = rpc(&ctx, &mut sess, "scanMgr", "ReconnectToLostProbes", &[]).await.expect("ReconnectToLostProbes");
    assert_eq!(recon_rep.dict_get("reconnectedCount"), Some(&PyRep::Integer(8)));
    let recon_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let state_args = find_notif(&recon_notifs, "OnProbeStateChanged");
    assert_eq!((state_args.len(), &state_args[1]), (2, &PyRep::Integer(1)));

    let destroy_rep = rpc(&ctx, &mut sess, "scanMgr", "DestroyProbe", &[]).await.expect("DestroyProbe");
    assert_eq!(destroy_rep.dict_get("destroyedCount").and_then(|v| v.as_i64()), Some(8));
    let destroy_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&destroy_notifs, "OnScannerInfoRemoved"), &[]);

    // 5. Abyssal Deadspace entry + room clears + exit (away from station proximity)
    let _ = ctx.systems.get_or_spawn(SYS_JITA).set_ball_position(ship_id, DVec3::new(0.0, 2.0e9, 0.0)).await;
    ctx.advance_secs(12);
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

    // 6. Void Space jump + relog ClientIsReady preserves effective_content_id (fil_id)
    ctx.db.with_mut_snapshot(|s| s.items.get_mut(&fil_id).unwrap().type_id = 62_448); ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpacePlayerJump", &[PyRep::Integer(fil_id), PyRep::Integer(777_001)]).await.expect("VoidSpacePlayerJump");
    let void_in_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&void_in_notifs, "OnVoidSpaceEntered")[0].dict_get("content_id").and_then(|v| v.as_i64()), Some(fil_id));
    assert!(find_notif(&void_in_notifs, "OnAddExpiryDurationForVoidSpaceContent")[0].as_i64().unwrap_or(0) > 0);

    rpc(&ctx, &mut sess, "voidSpaceMgr", "ClientIsReady", &[]).await.expect("voidSpaceMgr.ClientIsReady");
    let relog_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let relog_args = find_notif(&relog_notifs, "OnVoidSpaceRelog");
    assert_eq!(relog_args[0].dict_get("content_id").and_then(|v| v.as_i64()), Some(fil_id));
    assert!(relog_args[0].dict_get("end_time").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
}

// parity: notify:OnSeasonSelected, notify:OnChallengeProgressUpdate, notify:OnChallengeCompleted
// parity: notify:OnChallengeRewardsGranted, notify:OnSeasonalPointsUpdated, notify:OnSeasonsFsdDataChanged
// parity: notify:OnExpertSystemsUpdated
#[tokio::test]
async fn notify_batch2_void_exit_seasons_expert_systems_and_node_graphs() {
    eve_testkit::attest(&[
        "notify:OnSeasonSelected",
        "notify:OnChallengeProgressUpdate",
        "notify:OnChallengeCompleted",
        "notify:OnChallengeRewardsGranted",
        "notify:OnSeasonalPointsUpdated",
        "notify:OnSeasonsFsdDataChanged",
        "notify:OnExpertSystemsUpdated",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let (ship_id, ship_b_id, fil_id, es_item_id) = (630_000_001_i64, 630_000_002_i64, 630_000_010_i64, 630_000_020_i64);
    let mut sess = login_in_space(&ctx, 8_401, "demo", "demo-secret", PILOT_A, ship_id, 621, SYS_JITA, DVec3::ZERO).await;
    let mut sess_b = login_in_space(&ctx, 8_402, "other", "other-secret", PILOT_B, ship_b_id, 621, SYS_JITA, DVec3::new(1_000.0, 0.0, 0.0)).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) { ch.clone_grade = 2; }
        if let Some(ch) = snap.characters.get_mut(&PILOT_B) { ch.clone_grade = 1; ch.station_id = Some(JITA_STATION); }
        snap.items.insert(fil_id, make_item(fil_id, 62_448, PILOT_A, ship_id, InventoryFlag::Cargo, 2, false));
        snap.items.insert(es_item_id, make_item(es_item_id, EXPERT_SYSTEM_TYPE_ID, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false));
        let mut defs = eve_server::services::season_manager::default_seasonal_challenge_definitions();
        defs.insert(0, SeasonalChallengeDef {
            challenge_id: 1_005, season_id: 1, event_type: "ui_onboarding_step".into(),
            title: "Onboarding".into(), description: "Complete two client onboarding steps".into(),
            points: 20, max_progress: 2, client_event_driven: true, reward_type_id: 0, reward_quantity: 0, is_dormant: false, ..Default::default()
        });
        snap.service_state_blobs.insert(SEASONAL_CHALLENGES_CONFIG_KEY.into(), serde_json::to_string(&defs).expect("defs"));
        snap.npe_progress.insert(PILOT_A, eve_db::NpeProgressRecord {
            character_id: PILOT_A, node_graph_id: 473, current_node_id: 1, completed: false,
            career_goal_progress: Default::default(), claimed_goals: Default::default(),
        });
    });
    sess_b.station_id = Some(JITA_STATION);

    // 1. Void Space jump (target fil_id) + exit conduit preserves effective_content_id (fil_id)
    rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpacePlayerJump", &[PyRep::Integer(fil_id), PyRep::Integer(777_002)]).await.expect("VoidSpacePlayerJump");
    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    ctx.advance_secs(12);
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

    // 2. Season selection + challenge progress/completion + Omega vs Alpha reward claim + GM FSD reload
    assert!(rpc(&ctx, &mut sess, "seasonManager", "process_client_event", &[PyRep::Integer(1_001), PyRep::Integer(1)]).await.is_err());

    rpc(&ctx, &mut sess, "seasonManager", "select_season", &[PyRep::Integer(1)]).await.expect("select_season");
    let sel_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert!(!sel_notifs.iter().any(|(e, _)| e == "OnSeasonSelectionUpdated"));
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

    // Alpha clone PILOT_B claims challenge -> does NOT receive OnSeasonOmegaRewardsClaimed
    rpc(&ctx, &mut sess_b, "seasonManager", "select_season", &[PyRep::Integer(1)]).await.expect("select_season B");
    rpc(&ctx, &mut sess_b, "seasonManager", "process_client_event", &[PyRep::Integer(1_005)]).await.expect("B step 1");
    rpc(&ctx, &mut sess_b, "seasonManager", "process_client_event", &[PyRep::Integer(1_005)]).await.expect("B step 2");
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    rpc(&ctx, &mut sess_b, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1_005)]).await.expect("B claim");
    let b_claim_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert!(b_claim_notifs.iter().any(|(e, _)| e == "OnSeasonalGoalCompleted"));
    assert!(!b_claim_notifs.iter().any(|(e, _)| e == "OnSeasonOmegaRewardsClaimed"));

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

    // 4. Node Graph lifecycle via operationsManager, node_graph, and air_npe
    // Under DATA GAP, client node graph instance protocol is Tranquility server-only;
    // RPCs complete without emitting notifications with guessed IDs.
    rpc(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(157), PyRep::Integer(SYS_JITA as i64), PyRep::Integer(31)]).await.expect("activate op");
    let ng_start_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert!(!ng_start_notifs.iter().any(|(e, _)| e == "OnStartClientNodeGraph"));

    rpc(&ctx, &mut sess, "node_graph", "process_client_graph_message", &[PyRep::Integer(1), PyRep::Integer(1), PyRep::String("client_action".into()), PyRep::Integer(42)]).await.expect("update");
    let ng_upd_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert!(!ng_upd_notifs.iter().any(|(e, _)| e == "OnClientNodeGraphUpdate"));

    rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(PILOT_A), PyRep::Integer(31)]).await.expect("complete category");
    let ng_stop_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert!(!ng_stop_notifs.iter().any(|(e, _)| e == "OnStopClientNodeGraph"));

    rpc(&ctx, &mut sess, "air_npe", "skip_air_npe", &[]).await.expect("skip_air_npe");
    let npe_stop_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert!(!npe_stop_notifs.iter().any(|(e, _)| e == "OnStopClientNodeGraphById"));
}

// Folded submodule: notify_real_emitters_batch3
#[allow(unused_imports, dead_code)]
mod notify_real_emitters_batch3 {
    use super::*;

// Real server-side emission tests for Batch 3:
// GM slash commands, RPC handlers, combat damage updates, session changes, and faction warfare.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

async fn pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    let args = [PyRep::Integer(char_id)];
    ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &args, &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    ctx.drain_notifications_for_char(char_id);
    s
}

fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
    let all = ctx.drain_notifications_for_char(char_id);
    let mut matched = Vec::new();
    let mut keep = Vec::new();
    for n in all {
        if n.event == event {
            matched.push(n.args);
        } else {
            keep.push(n);
        }
    }
    if !keep.is_empty() {
        if let Ok(mut guard) = ctx.pending_notifications.write() {
            guard.entry(char_id).or_default().extend(keep);
        }
    }
    matched
}

#[tokio::test]
async fn test_batch3_gm_and_admin_rpc_notifications() {
    // parity: notify:OnAchievementsReset, notify:OnCharacterPokedByGM, notify:OnClusterShutdownCancelled
    // parity: notify:OnGlobalConfigUpdated, notify:OnFastCheckoutEnabledChanged
    // parity: notify:OnModuleAttributeChange, notify:OnLocationAttributeChange
    eve_testkit::attest(&[
        "notify:OnAchievementsReset",
        "notify:OnCharacterPokedByGM",
        "notify:OnClusterShutdownCancelled",
        "notify:OnGlobalConfigUpdated",
        "notify:OnFastCheckoutEnabledChanged",
        "notify:OnModuleAttributeChange",
        "notify:OnLocationAttributeChange",
    ]);
    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, 801, "demo", "demo-secret", P1).await;
    let mut s2 = pilot(&ctx, 802, "other", "other-secret", P2).await;

    // Give s1 GM roles for GM slash and admin commands
    s1.role = eve_machonet::service_roles::ROLE_GMH | eve_machonet::service_roles::ROLE_ADMIN;

    // 1. OnAchievementsReset via slash.SlashCmd with state seeding and reset verification
    ctx.quasar_gateway.progression.achievements.unlock_achievement(P1 as u32, 2);
    ctx.quasar_gateway.progression.achievements.set_category_score(P1 as u32, 2, 50);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = eve_server::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap);
        ext.achievements_by_char.entry(P1).or_default().push(101);
        ext.achievement_counters_by_char.entry(P1).or_default().insert("jumps".into(), 5);
        eve_server::services::cosmetics_ui_ops::save_cosmetics_ui_state(snap, &ext);
    });
    assert!(ctx.quasar_gateway.progression.achievements.character_achievements.read().unwrap().contains_key(&(P1 as u32)));
    assert!(ctx.quasar_gateway.progression.achievements.category_scores.read().unwrap().contains_key(&(P1 as u32, 2)));
    assert!(ctx.quasar_gateway.progression.achievements.total_character_score(P1 as u32) > 0);

    let slash_ach = ctx
        .dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/achievements reset 90000001".into())], &HashMap::new(), None)
        .await
        .expect("SlashCmd /achievements reset");
    assert!(matches!(slash_ach, PyRep::KeyVal(_)));
    let ev = events(&ctx, P1, "OnAchievementsReset");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].len(), 0, "OnAchievementsReset payload expects 0 args");

    // Backing state is completely cleared
    assert!(ctx.quasar_gateway.progression.achievements.character_achievements.read().unwrap().get(&(P1 as u32)).is_none());
    assert!(ctx.quasar_gateway.progression.achievements.category_scores.read().unwrap().get(&(P1 as u32, 2)).is_none());
    assert_eq!(ctx.quasar_gateway.progression.achievements.total_character_score(P1 as u32), 0);
    let machonet_achs = ctx
        .dispatch_rpc(&mut s1, "monolithTaskTrackerMgr", None, "GetCompletedAchievementsAndClientEventCount", &[], &HashMap::new(), None)
        .await
        .expect("monolithTaskTrackerMgr.GetCompletedAchievementsAndClientEventCount");
    let completed_dict = machonet_achs.dict_get("completedDict").expect("completedDict");
    assert_eq!(completed_dict, &PyRep::Dict(vec![]));

    // 2. OnCharacterPermanentlyBanned via /chatban perm
    let ban_perm = ctx
        .dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/chatban 90000001 perm".into())], &HashMap::new(), None)
        .await
        .expect("/chatban perm");
    assert!(matches!(ban_perm, PyRep::KeyVal(_)));
    let ev = events(&ctx, P1, "OnCharacterPermanentlyBanned");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].len(), 0, "OnCharacterPermanentlyBanned payload expects 0 args");

    // 3. OnCharacterTemporarilyBanned via /chatban 3600
    let ban_temp_start = ctx.now_blueos_filetime_ticks();
    let ban_temp = ctx
        .dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/chatban 90000001 3600".into())], &HashMap::new(), None)
        .await
        .expect("/chatban temp");
    assert!(matches!(ban_temp, PyRep::KeyVal(_)));
    let ev = events(&ctx, P1, "OnCharacterTemporarilyBanned");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].len(), 1, "OnCharacterTemporarilyBanned payload expects 1 arg [expiry_ft]");
    assert_eq!(ev[0][0], PyRep::Integer(ban_temp_start + (3600 * 10_000_000)));

    // 4. OnCharacterUnbanned via /chatunban
    let unban = ctx
        .dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/chatunban 90000001".into())], &HashMap::new(), None)
        .await
        .expect("/chatunban");
    assert!(matches!(unban, PyRep::KeyVal(_)));
    let ev = events(&ctx, P1, "OnCharacterUnbanned");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].len(), 0, "OnCharacterUnbanned payload expects 0 args");

    // 5. OnCharacterPokedByGM via mailMgr.PokePlayerAboutChatMsgGm with non-GM negative test
    let neg_poke = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "PokePlayerAboutChatMsgGm", &[PyRep::Integer(P1), PyRep::String("help".into())], &HashMap::new(), None)
        .await
        .unwrap_err();
    assert_eq!(neg_poke.code, "UserError");

    ctx.dispatch_rpc(&mut s1, "mailMgr", None, "PokePlayerAboutChatMsgGm", &[PyRep::Integer(P2), PyRep::String("ChannelPolicy".into())], &HashMap::new(), None)
        .await
        .expect("PokePlayerAboutChatMsgGm");
    let ev = events(&ctx, P2, "OnCharacterPokedByGM");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].len(), 2, "OnCharacterPokedByGM expects 2 args [gm_id, channel_name]");
    assert_eq!(ev[0][0], PyRep::Integer(P1));
    assert_eq!(ev[0][1], PyRep::String("ChannelPolicy".into()));

    // 6. OnClusterShutdownCancelled via /shutdown cancel
    ctx.dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/shutdown cancel UI/Shared/Delayed".into())], &HashMap::new(), None)
        .await
        .expect("/shutdown cancel");
    let ev = events(&ctx, P1, "OnClusterShutdownCancelled");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].len(), 1);
    assert_eq!(ev[0][0], PyRep::String("UI/Shared/Delayed".into()));

    // 7. OnGlobalConfigUpdated via machoNet.SetGlobalConfigValue with non-GM negative test and V24 kwargs
    let mut neg_cfg_kw = HashMap::new();
    neg_cfg_kw.insert("key".into(), PyRep::String("fleet_broadcast_cap".into()));
    neg_cfg_kw.insert("value".into(), PyRep::String("50".into()));
    let neg_cfg = ctx
        .dispatch_rpc(&mut s2, "machoNet", None, "SetGlobalConfigValue", &[], &neg_cfg_kw, None)
        .await
        .unwrap_err();
    assert_eq!(neg_cfg.code, "InsufficientRole");

    let mut gm_cfg_kw = HashMap::new();
    gm_cfg_kw.insert("key".into(), PyRep::String("fleet_broadcast_cap".into()));
    gm_cfg_kw.insert("value".into(), PyRep::String("50".into()));
    ctx.dispatch_rpc(&mut s1, "machoNet", None, "SetGlobalConfigValue", &[], &gm_cfg_kw, None)
        .await
        .expect("SetGlobalConfigValue kwargs");
    let ev = events(&ctx, P1, "OnGlobalConfigUpdated");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].len(), 2, "OnGlobalConfigUpdated expects [key, val]");
    assert_eq!(ev[0][0], PyRep::String("fleet_broadcast_cap".into()));
    assert_eq!(ev[0][1], PyRep::String("50".into()));

    // Verify PyRep::None is preserved (not converted to empty string)
    let mut null_cfg_kw = HashMap::new();
    null_cfg_kw.insert("key".into(), PyRep::String("null_key".into()));
    null_cfg_kw.insert("value".into(), PyRep::None);
    ctx.dispatch_rpc(&mut s1, "machoNet", None, "SetGlobalConfigValue", &[], &null_cfg_kw, None)
        .await
        .expect("SetGlobalConfigValue None");
    let ev_null = events(&ctx, P1, "OnGlobalConfigUpdated");
    assert_eq!(ev_null.len(), 1);
    assert_eq!(ev_null[0].len(), 2);
    assert_eq!(ev_null[0][0], PyRep::String("null_key".into()));
    assert_eq!(ev_null[0][1], PyRep::None);

    // 8. OnFastCheckoutEnabledChanged via /fastcheckout with state verification
    let fc_init = ctx
        .dispatch_rpc(&mut s1, "FastCheckoutService", None, "IsFastCheckoutEnabledForUser", &[], &HashMap::new(), None)
        .await
        .expect("IsFastCheckoutEnabledForUser initial");
    assert_eq!(fc_init, PyRep::Bool(false));

    ctx.dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/fastcheckout 1".into())], &HashMap::new(), None)
        .await
        .expect("/fastcheckout 1");
    let ev = events(&ctx, P1, "OnFastCheckoutEnabledChanged");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].len(), 1);
    assert_eq!(ev[0][0], PyRep::Bool(true));
    let fc_after = ctx
        .dispatch_rpc(&mut s1, "FastCheckoutService", None, "IsFastCheckoutEnabledForUser", &[], &HashMap::new(), None)
        .await
        .expect("IsFastCheckoutEnabledForUser after enable");
    assert_eq!(fc_after, PyRep::Bool(true));

    ctx.dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/fastcheckout 0".into())], &HashMap::new(), None)
        .await
        .expect("/fastcheckout 0");
    let ev_off = events(&ctx, P1, "OnFastCheckoutEnabledChanged");
    assert_eq!(ev_off.len(), 1);
    assert_eq!(ev_off[0], vec![PyRep::Bool(false)]);
    let fc_disabled = ctx
        .dispatch_rpc(&mut s1, "FastCheckoutService", None, "IsFastCheckoutEnabledForUser", &[], &HashMap::new(), None)
        .await
        .expect("IsFastCheckoutEnabledForUser after disable");
    assert_eq!(fc_disabled, PyRep::Bool(false));

    // 9. OnCampaignEnrollmentChanged via /campaign enroll 2
    ctx.dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/campaign enroll 2".into())], &HashMap::new(), None)
        .await
        .expect("/campaign enroll");
    let ev = events(&ctx, P1, "OnCampaignEnrollmentChanged");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].len(), 0);

    // 10 & 11. OnModuleAttributeChange & OnLocationAttributeChange via /setattr with strict non-GM negative test
    let neg_setattr = ctx
        .dispatch_rpc(&mut s2, "slash", None, "SlashCmd", &[PyRep::String("/setattr 7001 9 175.0".into())], &HashMap::new(), None)
        .await
        .unwrap_err();
    assert_eq!(neg_setattr.code, "InsufficientRole");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            7001,
            eve_db::InventoryItemRecord {
                item_id: 7001,
                type_id: 587,
                owner_id: P1,
                location_id: 60003760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let res_setattr = ctx
        .dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/setattr 7001 9 175.0".into())], &HashMap::new(), None)
        .await
        .expect("/setattr");
    assert!(matches!(res_setattr, PyRep::KeyVal(_)));

    let now_ft = ctx.now_blueos_filetime_ticks();
    let ev_mod = events(&ctx, P1, "OnModuleAttributeChange");
    assert_eq!(ev_mod.len(), 1);
    assert_eq!(ev_mod[0].len(), 8, "OnModuleAttributeChange expects 8 args");
    assert_eq!(ev_mod[0][0], PyRep::Integer(P1));
    assert_eq!(ev_mod[0][1], PyRep::Integer(7001));
    assert_eq!(ev_mod[0][2], PyRep::Integer(9));
    assert_eq!(ev_mod[0][3], PyRep::Integer(now_ft));
    assert_eq!(ev_mod[0][4], PyRep::Real(175.0));
    assert_eq!(ev_mod[0][5], PyRep::Real(350.0)); // Rifter base hp is 350.0
    assert_eq!(ev_mod[0][6], PyRep::Integer(now_ft));
    assert_eq!(ev_mod[0][7], PyRep::Bool(true));

    let ev_loc = events(&ctx, P1, "OnLocationAttributeChange");
    assert_eq!(ev_loc.len(), 1);
    assert_eq!(ev_loc[0].len(), 7, "OnLocationAttributeChange expects 7 args");
    assert_eq!(ev_loc[0][0], PyRep::Integer(P1));
    assert_eq!(ev_loc[0][1], PyRep::Integer(60003760));
    assert_eq!(ev_loc[0][2], PyRep::Integer(7001));
    assert_eq!(ev_loc[0][3], PyRep::Integer(9));
    assert_eq!(ev_loc[0][4], PyRep::Integer(now_ft));
    assert_eq!(ev_loc[0][5], PyRep::Real(175.0));
    assert_eq!(ev_loc[0][6], PyRep::Real(350.0));
}


}

// Folded submodule: notify_real_emitters_batch5
#[allow(unused_imports, dead_code)]
mod notify_real_emitters_batch5 {
    use super::*;

// Real server-side emission tests for Batch 5:
// Station guest list events, Crimewatch timers, combat damage updates,
// shutdown announcements, scanners, mining, space destiny, and ESS bank links.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::notify_emitters_batch3_space::*;
use eve_server::services::notify_emitters_batch5::*;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const JITA_STATION: i64 = 60_003_760;
const JITA_SYSTEM: u32 = 30_000_142;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

async fn pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    let args = [PyRep::Integer(char_id)];
    ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &args, &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    ctx.drain_notifications_for_char(char_id);
    s
}

fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
    let all = ctx.drain_notifications_for_char(char_id);
    let mut matched = Vec::new();
    let mut keep = Vec::new();
    for n in all {
        if n.event == event {
            matched.push(n.args);
        } else {
            keep.push(n);
        }
    }
    if !keep.is_empty() {
        if let Ok(mut guard) = ctx.pending_notifications.write() {
            guard.entry(char_id).or_default().extend(keep);
        }
    }
    matched
}

#[tokio::test]
async fn test_batch5_station_damage_crimewatch_shutdown_scanners() {
    // DATA GAP: notify:OnCharNowInStation requires production dock RPC guest-list wiring
    // DATA GAP: notify:OnCharNoLongerInStation requires production undock RPC guest-list wiring
    // DATA GAP: notify:OnDamageStateChange requires bubble-scoped combat damage wiring
    // parity: notify:OnWeaponsTimerUpdate, notify:OnPvpTimerUpdate, notify:OnNpcTimerUpdate
    // parity: notify:OnDisapprovalTimerUpdate
    // DATA GAP: notify:OnCargoScanComplete requires ship scanner module cycle wiring
    // DATA GAP: notify:OnShipScanCompleted requires ship scanner module cycle wiring
    // DATA GAP: notify:OnOreMined requires mining cycle wiring (OF-28)
    eve_testkit::attest(&[
        "notify:OnWeaponsTimerUpdate",
        "notify:OnPvpTimerUpdate",
        "notify:OnNpcTimerUpdate",
        "notify:OnDisapprovalTimerUpdate",
    ]);

    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, 811, "demo", "demo-secret", P1).await;
    let _s2 = pilot(&ctx, 812, "other", "other-secret", P2).await;

    // Ensure P2 is docked at Jita 4-4 to receive station guest notifications
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&P2) {
            c.station_id = Some(JITA_STATION);
            c.solar_system_id = JITA_SYSTEM;
        }
    });

    // 1. OnCharNowInStation: Pilot 1 enters station -> Pilot 2 receives guest row
    emit_char_station_notification(&ctx, P1, JITA_STATION, true);
    let ev_now = events(&ctx, P2, "OnCharNowInStation");
    assert_eq!(ev_now.len(), 1, "P2 must receive OnCharNowInStation");
    assert_eq!(ev_now[0].len(), 1);
    if let PyRep::List(ref row) = ev_now[0][0] {
        assert_eq!(row[0], PyRep::Integer(P1));
    } else {
        panic!("expected PyRep::List for guest row");
    }

    // 2. OnCharNoLongerInStation: Pilot 1 leaves station -> Pilot 2 receives undock guest row
    emit_char_station_notification(&ctx, P1, JITA_STATION, false);
    let ev_gone = events(&ctx, P2, "OnCharNoLongerInStation");
    assert_eq!(ev_gone.len(), 1, "P2 must receive OnCharNoLongerInStation");
    assert_eq!(ev_gone[0].len(), 1);
    if let PyRep::List(ref row) = ev_gone[0][0] {
        assert_eq!(row[0], PyRep::Integer(P1));
    } else {
        panic!("expected PyRep::List for guest row");
    }

    // 3. OnDamageStateChange: Emit V24 damage tuple
    emit_damage_state_change(&ctx, P1, 7001, 0.65, 0.85, 1.0);
    let ev_dmg = events(&ctx, P1, "OnDamageStateChange");
    assert_eq!(ev_dmg.len(), 1, "P1 must receive OnDamageStateChange");
    assert_eq!(ev_dmg[0].len(), 2, "OnDamageStateChange expects [shipID, damageTuple]");
    assert_eq!(ev_dmg[0][0], PyRep::Integer(7001));
    if let PyRep::Tuple(ref dt) = ev_dmg[0][1] {
        assert_eq!(dt.len(), 3);
        assert_eq!(dt[1], PyRep::Real(0.85));
        assert_eq!(dt[2], PyRep::Real(1.0));
    } else {
        panic!("expected PyRep::Tuple for damage state");
    }

    // 4. Crimewatch timers: Weapons, PvP, NPC, Criminal, Disapproval
    let now = ctx.now_unix_secs();
    ctx.crimewatch.write().unwrap().states.insert(
        P1,
        eve_server::combat::CharacterCrimeWatchState {
            weapon_timer_until_unix: now + 60,
            pvp_timer_until_unix: now + 300,
            criminal_flag_until_unix: now + 900,
            ..Default::default()
        },
    );
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.npc_timers.insert(P1, now + 30);
        rt.disapproval_flags.insert(P1, now + 120);
    });
    eve_server::services::combat_crimewatch::crimewatch_timers::emit_crimewatch_timer_notifications(&ctx, P1, now);

    let ev_weap = events(&ctx, P1, "OnWeaponsTimerUpdate");
    assert_eq!(ev_weap.len(), 1);
    assert_eq!(ev_weap[0][0], PyRep::Integer(101));

    let ev_pvp = events(&ctx, P1, "OnPvpTimerUpdate");
    assert_eq!(ev_pvp.len(), 1);
    assert_eq!(ev_pvp[0][0], PyRep::Integer(201));

    let ev_npc = events(&ctx, P1, "OnNpcTimerUpdate");
    assert_eq!(ev_npc.len(), 1);
    assert_eq!(ev_npc[0][0], PyRep::Integer(401));

    let ev_crim = events(&ctx, P1, "OnCriminalTimerUpdate");
    assert_eq!(ev_crim.len(), 1);
    assert_eq!(ev_crim[0][0], PyRep::Integer(301));

    let ev_dis = events(&ctx, P1, "OnDisapprovalTimerUpdate");
    assert_eq!(ev_dis.len(), 1);
    assert_eq!(ev_dis[0][0], PyRep::Integer(501));

    // 5. OnClusterShutdownInitiated via GM slash /shutdown 120
    s1.role = eve_machonet::service_roles::ROLE_GMH | eve_machonet::service_roles::ROLE_ADMIN;
    ctx.dispatch_rpc(
        &mut s1,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String("/shutdown 120 ScheduledMaintenance".into())],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SlashCmd /shutdown");

    let ev_shut = events(&ctx, P1, "OnClusterShutdownInitiated");
    assert_eq!(ev_shut.len(), 1);
    assert_eq!(ev_shut[0].len(), 3);
    assert_eq!(ev_shut[0][0], PyRep::String("UI/Shared/ClusterShutdownInitiated".into()));
    assert!(ev_shut[0][1].as_i64().unwrap() > 0);
    assert_eq!(ev_shut[0][2], PyRep::Integer(120));

    // 6. OnCargoScanComplete
    emit_cargo_scan_complete(&ctx, P1, 8001, vec![(34, 1000), (35, 500)]);
    let ev_cscan = events(&ctx, P1, "OnCargoScanComplete");
    assert_eq!(ev_cscan.len(), 1);
    assert_eq!(ev_cscan[0].len(), 2);
    assert_eq!(ev_cscan[0][0], PyRep::Integer(8001));

    // 7. OnShipScanCompleted
    emit_ship_scan_completed(&ctx, P1, 8001, 1500.0, 2000.0, vec![(442, 1), (443, 1)]);
    let ev_sscan = events(&ctx, P1, "OnShipScanCompleted");
    assert_eq!(ev_sscan.len(), 1);
    assert_eq!(ev_sscan[0].len(), 4);
    assert_eq!(ev_sscan[0][0], PyRep::Integer(8001));
    assert_eq!(ev_sscan[0][1], PyRep::Real(1500.0));
    assert_eq!(ev_sscan[0][2], PyRep::Real(2000.0));

    // 8. OnOreMined
    emit_ore_mined(&ctx, P1, 1230, 390, 390, 0, 0, 9001, 7002);
    let ev_ore = events(&ctx, P1, "OnOreMined");
    assert_eq!(ev_ore.len(), 1);
    assert_eq!(ev_ore[0].len(), 1);
    let PyRep::Dict(ref d) = ev_ore[0][0] else {
        panic!("expected dataDict for OnOreMined, got {:?}", ev_ore[0][0]);
    };
    assert!(d.contains(&(PyRep::String("oreType".into()), PyRep::Integer(1230))));
    assert!(d.contains(&(PyRep::String("quantity_added".into()), PyRep::Integer(390))));
}

#[tokio::test]
async fn test_batch5_space_combat_arena_and_ess() {
    // DATA GAP: notify:OnCloakModuleForceDeactivated requires production wiring through combat decloak loop
    // DATA GAP: notify:OnKillNotification requires production wiring through combat death pipeline
    // DATA GAP: notify:OnLocalHackProgressUpdated requires production wiring through hacking loop
    // DATA GAP: notify:OnMatchFound requires production wiring through abyssal matchmaking loop
    // DATA GAP: notify:OnMatchMakingFailure requires production wiring through abyssal matchmaking loop
    // DATA GAP: notify:OnAddExpiryDurationForAbyssalPvpContent requires production wiring through abyssal arena loop
    // DATA GAP: notify:DoDestinyUpdates requires production wiring through destiny 1Hz loop
    // DATA GAP: notify:OnComponentRemovedSystemEnvironment requires production wiring through space environment loop
    // DATA GAP: notify:OnESSReserveBankLinkNotification requires production wiring through ESS link loop
    // DATA GAP: notify:OnESSMainBankLinkNotification requires production wiring through ESS link loop
    // DATA GAP: notify:OnMainBankPlayerLinkDisconnected requires production wiring through ESS disconnect loop
    // DATA GAP: notify:OnCosmicAnomalyAdded requires production wiring through exploration loop

    let ctx = new_ctx();
    let _s1 = pilot(&ctx, 821, "demo", "demo-secret", P1).await;
    let _s2 = pilot(&ctx, 822, "other", "other-secret", P2).await;

    // Put P1 in solar system 30000142
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&P1) {
            c.solar_system_id = JITA_SYSTEM;
        }
    });

    // 1. OnCloakModuleForceDeactivated: Proximity decloak
    emit_cloak_module_force_deactivated(&ctx, P1, "ProximityDecloak", 1950.0);
    let ev_cloak = events(&ctx, P1, "OnCloakModuleForceDeactivated");
    assert_eq!(ev_cloak.len(), 1);
    assert_eq!(ev_cloak[0].len(), 2);
    assert_eq!(ev_cloak[0][0], PyRep::String("ProximityDecloak".into()));
    assert_eq!(
        ev_cloak[0][1],
        PyRep::Dict(vec![(PyRep::String("distance".into()), PyRep::Real(1950.0))])
    );

    // 2. OnKillNotification: Sent to victim and attacker
    emit_kill_notifications(&ctx, P1, P2);
    let ev_kill1 = events(&ctx, P1, "OnKillNotification");
    let ev_kill2 = events(&ctx, P2, "OnKillNotification");
    assert_eq!(ev_kill1.len(), 1);
    assert_eq!(ev_kill2.len(), 1);
    assert_eq!(ev_kill1[0], Vec::<PyRep>::new());
    assert_eq!(ev_kill2[0], Vec::<PyRep>::new());

    // 3. OnLocalHackProgressUpdated: Container hacking progress in solar system
    emit_local_hack_progress_updated(&ctx, JITA_SYSTEM, 8501, 0.45);
    let ev_hack = events(&ctx, P1, "OnLocalHackProgressUpdated");
    assert_eq!(ev_hack.len(), 1);
    assert_eq!(ev_hack[0].len(), 2);
    assert_eq!(ev_hack[0][0], PyRep::Integer(8501));
    assert_eq!(ev_hack[0][1], PyRep::Real(0.45));

    // 4. OnMatchFound & OnAddExpiryDurationForAbyssalPvpContent: Arena match pop
    emit_pvp_match_found_and_expiry(&ctx, P1, 1_750_000_000);
    let ev_found = events(&ctx, P1, "OnMatchFound");
    let ev_exp = events(&ctx, P1, "OnAddExpiryDurationForAbyssalPvpContent");
    assert_eq!(ev_found.len(), 1);
    assert_eq!(ev_found[0], Vec::<PyRep>::new());
    assert_eq!(ev_exp.len(), 1);
    assert_eq!(ev_exp[0].len(), 1);
    let expected_ft = (1_750_000_000i64 + 11_644_473_600) * 10_000_000;
    assert_eq!(ev_exp[0][0], PyRep::Integer(expected_ft));

    // 5. OnMatchMakingFailure: Queue error notification
    emit_pvp_matchmaking_failure(&ctx, P1, 404);
    let ev_fail = events(&ctx, P1, "OnMatchMakingFailure");
    assert_eq!(ev_fail.len(), 1);
    assert_eq!(ev_fail[0].len(), 1);
    assert_eq!(ev_fail[0][0], PyRep::List(vec![PyRep::Integer(404)]));

    // 6. DoDestinyUpdates: Batch destiny movement updates
    emit_destiny_updates(&ctx, P1, vec![PyRep::Integer(101), PyRep::Integer(102)]);
    let ev_dest = events(&ctx, P1, "DoDestinyUpdates");
    assert_eq!(ev_dest.len(), 1);
    assert_eq!(ev_dest[0].len(), 1);
    assert_eq!(ev_dest[0][0], PyRep::List(vec![PyRep::Integer(101), PyRep::Integer(102)]));

    // 7. OnComponentRemovedSystemEnvironment: Clear environmental hazards
    emit_component_removed_system_environment(&ctx, JITA_SYSTEM);
    let ev_comp = events(&ctx, P1, "OnComponentRemovedSystemEnvironment");
    assert_eq!(ev_comp.len(), 1);
    assert_eq!(ev_comp[0].len(), 1);
    assert_eq!(ev_comp[0][0], PyRep::Integer(i64::from(JITA_SYSTEM)));

    // 8. OnESSReserveBankLinkNotification & OnESSMainBankLinkNotification
    emit_ess_bank_link_notifications(&ctx, JITA_SYSTEM, P1, "AttemptLinkToReserveBank", true, 0.0);
    let ev_res = events(&ctx, P1, "OnESSReserveBankLinkNotification");
    assert_eq!(ev_res.len(), 1);
    assert_eq!(ev_res[0].len(), 1);
    assert_eq!(ev_res[0][0], PyRep::Integer(i64::from(JITA_SYSTEM)));

    emit_ess_bank_link_notifications(&ctx, JITA_SYSTEM, P1, "AttemptLinkToMainBank", true, 0.0);
    let ev_main = events(&ctx, P1, "OnESSMainBankLinkNotification");
    assert_eq!(ev_main.len(), 1);
    assert_eq!(ev_main[0].len(), 1);
    assert_eq!(ev_main[0][0], PyRep::Integer(i64::from(JITA_SYSTEM)));

    // 9. OnMainBankPlayerLinkDisconnected
    emit_ess_main_bank_link_disconnected(&ctx, JITA_SYSTEM, P1, "WarpedAway");
    let ev_disc = events(&ctx, P1, "OnMainBankPlayerLinkDisconnected");
    assert_eq!(ev_disc.len(), 1);
    assert_eq!(ev_disc[0].len(), 1);
    if let PyRep::Dict(ref d) = ev_disc[0][0] {
        assert_eq!(d.len(), 4);
        assert!(d.iter().any(|(k, v)| k == &PyRep::String("reason".into()) && v == &PyRep::String("WarpedAway".into())));
    } else {
        panic!("expected PyRep::Dict for disconnect info");
    }

    // 10. OnCosmicAnomalyAdded
    spawn_and_emit_cosmic_anomaly(&ctx, JITA_SYSTEM, 1001);
    let ev_anom = events(&ctx, P1, "OnCosmicAnomalyAdded");
    assert_eq!(ev_anom.len(), 1);
    assert_eq!(ev_anom[0].len(), 1);
    if let PyRep::KeyVal(ref kv) = ev_anom[0][0] {
        assert!(kv.contains_key("targetID"));
        assert_eq!(kv.get("solarSystemID"), Some(&PyRep::Integer(i64::from(JITA_SYSTEM))));
    } else {
        panic!("expected PyRep::KeyVal for anomaly info");
    }
}

}

// Folded submodule: notify_real_emitters_batch6
#[allow(unused_imports, dead_code)]
mod notify_real_emitters_batch6 {
    use super::*;

// Real production emitter tests for Notify Batch 6 (`P-5`, `T-1`).
// Covers Station, Ship/Pod destruction, ESS, Faction Warfare, Insurgency, and SafeLogoff/Crimewatch.


use std::collections::HashMap;
use glam::DVec3;
use eve_destiny::{Ball, MovementMode};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

use crate::common;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const JITA_SYSTEM: u32 = 30000142;
const JITA_STATION: i64 = 60003760;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

async fn pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    let args = [PyRep::Integer(char_id)];
    ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &args, &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    ctx.drain_notifications_for_char(char_id);
    s
}

fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
    let all = ctx.drain_notifications_for_char(char_id);
    let mut matched = Vec::new();
    let mut keep = Vec::new();
    for n in all {
        if n.event == event {
            matched.push(n.args);
        } else {
            keep.push(n);
        }
    }
    if !keep.is_empty() {
        if let Ok(mut guard) = ctx.pending_notifications.write() {
            guard.entry(char_id).or_default().extend(keep);
        }
    }
    matched
}

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
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

#[tokio::test]
async fn test_batch6_station_scanners_mining_cloak_and_hack() {
    // parity: notify:OnCharNowInStation, notify:OnCharNoLongerInStation
    // DATA GAP: notify:OnDamageStateChange requires bubble-scoped combat damage wiring
    // DATA GAP: notify:OnCargoScanComplete requires ship scanner module cycle wiring
    // DATA GAP: notify:OnShipScanCompleted requires ship scanner module cycle wiring
    // DATA GAP: notify:OnOreMined requires mining cycle wiring (OF-28)
    // DATA GAP: notify:OnCloakModuleForceDeactivated requires ship cloak proximity/deactivation wiring
    // DATA GAP: notify:OnLocalHackProgressUpdated requires hacking minigame live container state wiring
    eve_testkit::attest(&[
        "notify:OnCharNowInStation",
        "notify:OnCharNoLongerInStation",
    ]);

    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, 821, "demo", "demo-secret", P1).await;
    let _s2 = pilot(&ctx, 822, "other", "other-secret", P2).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&P2) {
            c.station_id = Some(JITA_STATION);
            c.solar_system_id = JITA_SYSTEM;
        }
    });

    // 1. Station Undock -> emits OnCharNoLongerInStation to station pilots
    ctx.dispatch_rpc(&mut s1, "stationSvc", None, "Undock", &[], &HashMap::new(), None)
        .await
        .expect("Undock");
    let ev_gone = events(&ctx, P2, "OnCharNoLongerInStation");
    assert_eq!(ev_gone.len(), 1, "P2 receives OnCharNoLongerInStation");
    assert_eq!(
        ev_gone[0],
        vec![PyRep::Tuple(vec![
            PyRep::Integer(P1),
            PyRep::Integer(1_000_170),
            PyRep::None,
            PyRep::None,
        ])]
    );

    // Station Dock -> emits OnCharNowInStation to station pilots
    ctx.dispatch_rpc(&mut s1, "stationSvc", None, "Dock", &[PyRep::Integer(JITA_STATION)], &HashMap::new(), None)
        .await
        .expect("Dock");
    let ev_now = events(&ctx, P2, "OnCharNowInStation");
    assert_eq!(ev_now.len(), 1, "P2 receives OnCharNowInStation");
    assert_eq!(
        ev_now[0],
        vec![PyRep::Tuple(vec![
            PyRep::Integer(P1),
            PyRep::Integer(1_000_170),
            PyRep::None,
            PyRep::None,
        ])]
    );
}

#[tokio::test]
async fn test_batch6_ship_and_pod_destruction_pipeline() {
    // parity: notify:OnKillNotification, notify:OnShipDeath, notify:OnPodDeath
    // parity: notify:OnPodKilled, notify:OnPlayerPodDeath, notify:OnPlayerPodRespawn
    eve_testkit::attest(&[
        "notify:OnKillNotification",
        "notify:OnShipDeath",
        "notify:OnPodDeath",
        "notify:OnPodKilled",
        "notify:OnPlayerPodDeath",
        "notify:OnPlayerPodRespawn",
    ]);

    let ctx = new_ctx();
    let _s1 = pilot(&ctx, 831, "demo", "demo-secret", P1).await;
    let _s2 = pilot(&ctx, 832, "other", "other-secret", P2).await;

    let ship1: i64 = 770_001;
    let pod1: i64 = 770_002;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ship1, make_item(ship1, 587, P1, JITA_SYSTEM as i64, InventoryFlag::Hangar, 1, false));
        snap.items.insert(pod1, make_item(pod1, 670, P1, JITA_SYSTEM as i64, InventoryFlag::Hangar, 1, false));
    });

    // 1. Ship destruction via execute_death_pipeline_sync
    let res = eve_server::combat::death_pipeline::execute_death_pipeline_sync(
        &ctx,
        JITA_SYSTEM,
        P1,
        ship1,
        P2,
        DVec3::ZERO,
        42,
    );
    assert!(res.is_ok(), "execute_death_pipeline_sync must succeed");

    let ev_sd = events(&ctx, P1, "OnShipDeath");
    assert_eq!(ev_sd.len(), 1, "P1 receives OnShipDeath");
    assert!(ev_sd[0].is_empty(), "OnShipDeath has arity 0");

    let ev_km_vic = events(&ctx, P1, "OnKillNotification");
    assert_eq!(ev_km_vic.len(), 1, "Victim P1 receives OnKillNotification");
    assert!(ev_km_vic[0].is_empty(), "OnKillNotification has arity 0");

    let ev_km_atk = events(&ctx, P2, "OnKillNotification");
    assert_eq!(ev_km_atk.len(), 1, "Attacker P2 receives OnKillNotification");
    assert!(ev_km_atk[0].is_empty(), "OnKillNotification has arity 0");

    // 2. Pod destruction via execute_death_pipeline_sync_inner with authentic combat kill (is_self_destruct = false)
    let res_pod = eve_server::combat::death_pipeline::execute_death_pipeline_sync_inner(
        &ctx,
        JITA_SYSTEM,
        P1,
        pod1,
        P2,
        DVec3::ZERO,
        43,
        false,
    );
    assert!(res_pod.is_ok(), "execute_death_pipeline_sync_inner for pod must succeed");

    let ev_pd = events(&ctx, P1, "OnPodDeath");
    assert_eq!(ev_pd.len(), 1, "P1 receives OnPodDeath");
    assert!(ev_pd[0].is_empty(), "OnPodDeath has arity 0");

    let ev_ppd = events(&ctx, P1, "OnPlayerPodDeath");
    assert_eq!(ev_ppd.len(), 1, "P1 receives OnPlayerPodDeath");
    assert!(ev_ppd[0].is_empty(), "OnPlayerPodDeath has arity 0");

    let ev_pk = events(&ctx, P1, "OnPodKilled");
    assert_eq!(ev_pk.len(), 1, "P1 receives OnPodKilled");
    assert_eq!(ev_pk[0], vec![PyRep::Integer(pod1)]);

    let ev_respawn = events(&ctx, P1, "OnPlayerPodRespawn");
    assert_eq!(ev_respawn.len(), 1, "P1 receives OnPlayerPodRespawn");
    assert!(ev_respawn[0].is_empty(), "OnPlayerPodRespawn has arity 0");

    let ev_km_atk2 = events(&ctx, P2, "OnKillNotification");
    assert_eq!(ev_km_atk2.len(), 1, "Attacker P2 receives OnKillNotification for pod");
    assert!(ev_km_atk2[0].is_empty(), "OnKillNotification for pod has arity 0");
}

#[tokio::test]
async fn test_batch6_ess_and_dynamic_bounty() {
    // parity: notify:OnSystemESSDataChanged
    // parity: notify:OnESSMainBankLinkNotification, notify:OnESSReserveBankLinkNotification
    // parity: notify:OnMainBankPlayerLinkDisconnected, notify:OnReserveBankPlayerLinkDisconnected
    // parity: notify:OnMainBankPlayerHackSuccess, notify:OnReserveBankUnlock
    // DATA GAP: notify:OnSystemDynamicBountyStateChanged requires dynamic bounty multiplier stepping from ratting bounty / pvp loss path
    // DATA GAP: notify:OnEssReserveBankInventoryFull requires ESS container inventory overflow wiring
    eve_testkit::attest(&[
        "notify:OnSystemESSDataChanged",
        "notify:OnESSMainBankLinkNotification",
        "notify:OnESSReserveBankLinkNotification",
        "notify:OnMainBankPlayerLinkDisconnected",
        "notify:OnReserveBankPlayerLinkDisconnected",
        "notify:OnMainBankPlayerHackSuccess",
        "notify:OnReserveBankUnlock",
    ]);

    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, 841, "demo", "demo-secret", P1).await;
    let nullsec_sys_id = 30_004_759; // 1DQ1-A (K-space null-sec in SW quadrant)
    let ess_ball_id = 899_001_i64;
    let key_item_id = 899_002_i64;
    let ship_id = 899_003_i64;

    s1.solar_system_id = Some(nullsec_sys_id);
    s1.ship_id = Some(ship_id);
    s1.station_id = None;
    s1.structure_id = None;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = nullsec_sys_id;
            ch.active_ship_id = ship_id;
        }
        let st = snap.system_states.entry(nullsec_sys_id).or_default();
        st.ess_main_bank = eve_db::IskCents(50_000_000_00);
        st.ess_reserve_bank = eve_db::IskCents(100_000_000_00);
        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, P1, nullsec_sys_id as i64, InventoryFlag::None, 1, true),
        );
        snap.items.insert(
            key_item_id,
            make_item(key_item_id, 57720, P1, ship_id, InventoryFlag::Cargo, 1, false),
        );
    });

    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(P1, eve_server::services::combat_crimewatch::SafetyLevel::Yellow);
    });

    let sys_handle = ctx.systems.get_or_spawn(nullsec_sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ess_ball_id,
            type_id: 32882,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 50_000_000.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(1000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 0.5,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // 1. ESS Main Bank Link start
    let kw = HashMap::new();
    let res_start = ctx
        .dispatch_rpc(&mut s1, "essMgr", None, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)], &kw, None)
        .await
        .expect("AttemptLinkToMainBank start");
    assert_eq!(res_start.dict_get("linkStartedOnly"), Some(&PyRep::Bool(true)));
    let ev_main_link = events(&ctx, P1, "OnESSMainBankLinkNotification");
    assert_eq!(ev_main_link.len(), 1, "P1 receives OnESSMainBankLinkNotification");
    assert_eq!(ev_main_link[0], vec![PyRep::Integer(nullsec_sys_id as i64)]);

    // 2. ESS Main Bank Hack complete (advance 5 ticks)
    for _ in 0..5 {
        let _ = sys_handle.step_manual(1.0).await;
    }
    let res_comp = ctx
        .dispatch_rpc(&mut s1, "essMgr", None, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)], &kw, None)
        .await
        .expect("AttemptLinkToMainBank complete");
    assert_eq!(res_comp.dict_get("stolenISK"), Some(&PyRep::Real(50_000_000.0)));

    let ev_hack_ok = events(&ctx, P1, "OnMainBankPlayerHackSuccess");
    assert_eq!(ev_hack_ok.len(), 1, "P1 receives OnMainBankPlayerHackSuccess");
    assert_eq!(ev_hack_ok[0][0].dict_get("characterID"), Some(&PyRep::Integer(P1)));
    assert_eq!(ev_hack_ok[0][0].dict_get("solarSystemID"), Some(&PyRep::Integer(nullsec_sys_id as i64)));
    assert_eq!(ev_hack_ok[0][0].dict_get("linkID"), Some(&PyRep::Integer(ess_ball_id)));

    let ev_disc = events(&ctx, P1, "OnMainBankPlayerLinkDisconnected");
    assert_eq!(ev_disc.len(), 1, "P1 receives OnMainBankPlayerLinkDisconnected");
    assert_eq!(ev_disc[0][0].dict_get("characterID"), Some(&PyRep::Integer(P1)));
    assert_eq!(ev_disc[0][0].dict_get("solarSystemID"), Some(&PyRep::Integer(nullsec_sys_id as i64)));
    assert_eq!(ev_disc[0][0].dict_get("reason"), Some(&PyRep::String("Completed".into())));
    assert_eq!(ev_disc[0][0].dict_get("linkID"), Some(&PyRep::Integer(ess_ball_id)));

    let ev_ess_data = events(&ctx, P1, "OnSystemESSDataChanged");
    assert_eq!(ev_ess_data.len(), 1, "P1 receives OnSystemESSDataChanged");
    assert_eq!(ev_ess_data[0][0], PyRep::Integer(nullsec_sys_id as i64));
    assert_eq!(ev_ess_data[0][1].dict_get("solarSystemID"), Some(&PyRep::Integer(nullsec_sys_id as i64)));
    assert_eq!(ev_ess_data[0][1].dict_get("mainBank"), Some(&PyRep::Real(0.0)));
    assert_eq!(ev_ess_data[0][1].dict_get("reserveBank"), Some(&PyRep::Real(100_000_000.0)));

    // 3. ESS Reserve Bank Link start (with key)
    let res_res_start = ctx
        .dispatch_rpc(&mut s1, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_ball_id)], &kw, None)
        .await
        .expect("AttemptLinkToReserveBank start");
    assert_eq!(res_res_start.dict_get("linkStartedOnly"), Some(&PyRep::Bool(true)));
    let ev_res_link = events(&ctx, P1, "OnESSReserveBankLinkNotification");
    assert_eq!(ev_res_link.len(), 1, "P1 receives OnESSReserveBankLinkNotification");
    assert_eq!(ev_res_link[0], vec![PyRep::Integer(nullsec_sys_id as i64)]);

    let ev_unlock = events(&ctx, P1, "OnReserveBankUnlock");
    assert_eq!(ev_unlock.len(), 1, "P1 receives OnReserveBankUnlock");
    assert_eq!(ev_unlock[0][0].dict_get("regionID"), Some(&PyRep::Integer(10000060)));

    // Advance 5 ticks and complete Reserve Bank link
    for _ in 0..5 {
        let _ = sys_handle.step_manual(1.0).await;
    }
    let res_res_comp = ctx
        .dispatch_rpc(&mut s1, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_ball_id)], &kw, None)
        .await
        .expect("AttemptLinkToReserveBank complete");
    let expected_p1 = (eve_server::services::pve_open_feats_b36::compute_ess_reserve_pulse_payout_cents(
        10_000_000_000,
        15,
        15,
    ) as f64)
        / 100.0;
    assert_eq!(res_res_comp.dict_get("stolenISK"), Some(&PyRep::Real(expected_p1)));

    let _ = ctx
        .dispatch_rpc(&mut s1, "essMgr", None, "RequestReserveBankUnlink", &[], &kw, None)
        .await
        .expect("RequestReserveBankUnlink");

    let ev_res_disc = events(&ctx, P1, "OnReserveBankPlayerLinkDisconnected");
    assert_eq!(ev_res_disc.len(), 1, "P1 receives OnReserveBankPlayerLinkDisconnected");
    assert_eq!(ev_res_disc[0][0].dict_get("characterID"), Some(&PyRep::Integer(P1)));
    assert_eq!(ev_res_disc[0][0].dict_get("solarSystemID"), Some(&PyRep::Integer(nullsec_sys_id as i64)));
    assert_eq!(ev_res_disc[0][0].dict_get("reason"), Some(&PyRep::Integer(0)));
}

#[tokio::test]
async fn test_batch6_faction_warfare_and_insurgency() {
    // parity: notify:OnFacWarBunkerVulnerable, notify:OnFacWarBunkerNonVulnerable
    // parity: notify:OnFwObjectiveCompletedInSolarSystem, notify:OnFwScoreboardWon, notify:OnSolarSystemLPChange
    // parity: notify:OnSolarsystemInterferenceChanged, notify:OnSystemFullyCorruptedForFaction
    // parity: notify:OnSystemFullySuppressedForFaction, notify:OnVictoryPointStateUpdated
    // DATA GAP: notify:OnCaptureChanged requires real-time FW complex capture timer progression wiring
    eve_testkit::attest(&[
        "notify:OnFacWarBunkerVulnerable",
        "notify:OnFacWarBunkerNonVulnerable",
        "notify:OnFwObjectiveCompletedInSolarSystem",
        "notify:OnFwScoreboardWon",
        "notify:OnSolarSystemLPChange",
        "notify:OnSolarsystemInterferenceChanged",
        "notify:OnSystemFullyCorruptedForFaction",
        "notify:OnSystemFullySuppressedForFaction",
        "notify:OnVictoryPointStateUpdated",
    ]);

    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, 851, "demo", "demo-secret", P1).await;

    ctx.db.with_mut_snapshot(|s| {
        s.fw_system_occupations.insert(
            JITA_SYSTEM,
            eve_db::FwSystemOccupationRecord {
                solar_system_id: JITA_SYSTEM,
                occupier_faction_id: 500_004,
                owner_faction_id: 500_001,
                victory_points: 1200,
                victory_points_threshold: 3000,
            },
        );
    });

    // 1. Bunker vulnerability
    eve_server::services::fw_services::set_facwar_bunker_vulnerable(&ctx, JITA_SYSTEM, 500001, true);
    let ev_v = events(&ctx, P1, "OnFacWarBunkerVulnerable");
    assert_eq!(ev_v.len(), 1);
    assert_eq!(ev_v[0], vec![PyRep::Integer(500001)]);

    eve_server::services::fw_services::set_facwar_bunker_vulnerable(&ctx, JITA_SYSTEM, 0, false);
    let ev_nv = events(&ctx, P1, "OnFacWarBunkerNonVulnerable");
    assert_eq!(ev_nv.len(), 1);
    assert_eq!(ev_nv[0], vec![PyRep::Integer(0)]);

    // 2. Objective & Scoreboard
    eve_server::services::fw_services::complete_fw_dungeon_and_objective(
        &ctx, JITA_SYSTEM, 500001, true, 1, 9905,
    );
    let ev_obj = events(&ctx, P1, "OnFwObjectiveCompletedInSolarSystem");
    assert_eq!(ev_obj.len(), 1);
    assert_eq!(ev_obj[0], vec![PyRep::Integer(i64::from(JITA_SYSTEM)), PyRep::Integer(500001), PyRep::Integer(1)]);

    let ev_sb = events(&ctx, P1, "OnFwScoreboardWon");
    assert_eq!(ev_sb.len(), 1);
    assert_eq!(ev_sb[0], vec![PyRep::Integer(i64::from(JITA_SYSTEM)), PyRep::Integer(9905), PyRep::Integer(500001)]);

    // 3. Solar system LP change & Victory Points
    eve_server::services::fw_services::donate_solar_system_lp(&ctx, JITA_SYSTEM, 1000, 2000);
    let ev_lp = events(&ctx, P1, "OnSolarSystemLPChange");
    assert_eq!(ev_lp.len(), 1);
    assert_eq!(ev_lp[0], vec![PyRep::Integer(1000), PyRep::Integer(2000)]);

    eve_server::services::fw_services::add_victory_points(&ctx, JITA_SYSTEM, 50);
    let ev_vp = events(&ctx, P1, "OnVictoryPointStateUpdated");
    assert_eq!(ev_vp.len(), 1);
    assert_eq!(ev_vp[0][0], PyRep::Integer(i64::from(JITA_SYSTEM)));
    assert_eq!(ev_vp[0][1].dict_get("solarSystemID"), Some(&PyRep::Integer(i64::from(JITA_SYSTEM))));
    assert_eq!(ev_vp[0][1].dict_get("victoryPoints"), Some(&PyRep::Integer(1250)));
    assert_eq!(ev_vp[0][1].dict_get("threshold"), Some(&PyRep::Integer(3000)));

    // 4. System corruption & suppression via real add_system_corruption / add_system_suppression
    let camp_id = 9999_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.insert(
            camp_id,
            eve_db::PersistedInsurgencyCampaign {
                campaign_id: camp_id,
                pirate_faction_id: 500010,
                target_faction_id: 500001,
                fob_solar_system_id: JITA_SYSTEM,
                systems: vec![JITA_SYSTEM],
                created_at_unix: 1000,
                forecasting_end_unix: 2000,
                active_end_unix: 2_000_000_000,
                winddown_end_unix: 2_000_000_000,
                corruption_points: Default::default(),
                suppression_points: Default::default(),
            },
        );
    });

    eve_server::services::insurgency_services::add_system_corruption(&ctx, camp_id, JITA_SYSTEM, 1000);
    let ev_corr = events(&ctx, P1, "OnSystemFullyCorruptedForFaction");
    assert_eq!(ev_corr.len(), 1, "P1 receives OnSystemFullyCorruptedForFaction");
    assert_eq!(ev_corr[0], vec![PyRep::Integer(i64::from(JITA_SYSTEM)), PyRep::Integer(500010), PyRep::Integer(1000), PyRep::Integer(1000)]);

    eve_server::services::insurgency_services::add_system_suppression(&ctx, camp_id, JITA_SYSTEM, 1000);
    let ev_supp = events(&ctx, P1, "OnSystemFullySuppressedForFaction");
    assert_eq!(ev_supp.len(), 1, "P1 receives OnSystemFullySuppressedForFaction");
    assert_eq!(ev_supp[0], vec![PyRep::Integer(i64::from(JITA_SYSTEM)), PyRep::Integer(1000), PyRep::Integer(1000)]);

    // 5. Stellar interference via record_system_interference_activity
    eve_server::services::pve_special_sites::record_system_interference_activity(&ctx, &mut s1, JITA_SYSTEM, 0.40);
    let ev_interf = events(&ctx, P1, "OnSolarsystemInterferenceChanged");
    assert_eq!(ev_interf.len(), 1, "P1 receives OnSolarsystemInterferenceChanged");
    assert_eq!(ev_interf[0], vec![PyRep::Integer(i64::from(JITA_SYSTEM)), PyRep::Real(0.40), PyRep::Integer(2)]);
}

#[tokio::test]
async fn test_batch6_safelogoff_and_crimewatch() {
    // parity: notify:OnSafeLogoffActivated, notify:OnSafeLogoffTimerStarted, notify:OnSafeLogoffAborted
    // parity: notify:OnSafeLogoffFailed, notify:OnSystemCriminalFlagUpdates, notify:OnSystemDisapprovalFlagUpdates
    eve_testkit::attest(&[
        "notify:OnSafeLogoffActivated",
        "notify:OnSafeLogoffTimerStarted",
        "notify:OnSafeLogoffAborted",
        "notify:OnSafeLogoffFailed",
        "notify:OnSystemCriminalFlagUpdates",
        "notify:OnSystemDisapprovalFlagUpdates",
    ]);

    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, 861, "demo", "demo-secret", P1).await;
    let empty = HashMap::new();

    // 1. SafeLogoff while docked -> fails with SAFE_LOGOFF_DOCKED and emits OnSafeLogoffFailed
    let fail_res = ctx
        .dispatch_rpc(&mut s1, "ship", None, "SafeLogoff", &[], &empty, None)
        .await
        .expect("SafeLogoff docked");
    assert_eq!(fail_res, PyRep::List(vec![PyRep::String("SAFE_LOGOFF_DOCKED".into())]));
    let ev_slf = events(&ctx, P1, "OnSafeLogoffFailed");
    assert_eq!(ev_slf.len(), 1, "P1 receives OnSafeLogoffFailed");
    assert_eq!(ev_slf[0], vec![PyRep::List(vec![PyRep::String("SAFE_LOGOFF_DOCKED".into())])]);

    // 2. Undock into space
    ctx.dispatch_rpc(&mut s1, "stationSvc", None, "Undock", &[], &empty, None)
        .await
        .expect("Undock");

    // 3. Valid SafeLogoff in space -> emits OnSafeLogoffTimerStarted
    let expected_ft = eve_server::sp6_lifecycle::unix_to_filetime(ctx.now_unix() + 30);
    let start_res = ctx
        .dispatch_rpc(&mut s1, "ship", None, "SafeLogoff", &[], &empty, None)
        .await
        .expect("SafeLogoff space");
    assert_eq!(start_res, PyRep::List(vec![]));
    let ev_slt = events(&ctx, P1, "OnSafeLogoffTimerStarted");
    assert_eq!(ev_slt.len(), 1, "P1 receives OnSafeLogoffTimerStarted");
    assert_eq!(ev_slt[0], vec![PyRep::Integer(expected_ft)]);

    // 4. Abort SafeLogoff -> emits OnSafeLogoffAborted
    let abort_res = ctx
        .dispatch_rpc(&mut s1, "ship", None, "AbortSafeLogoff", &[], &empty, None)
        .await
        .expect("AbortSafeLogoff");
    assert_eq!(abort_res, PyRep::Bool(true));
    let ev_sab = events(&ctx, P1, "OnSafeLogoffAborted");
    assert_eq!(ev_sab.len(), 1, "P1 receives OnSafeLogoffAborted");
    assert_eq!(ev_sab[0], vec![PyRep::String("SAFE_LOGOFF_ABORT_GENERIC".into())]);

    // 5. Restart SafeLogoff, advance 31 seconds -> on_advance_secs processes tick and emits OnSafeLogoffActivated
    ctx.dispatch_rpc(&mut s1, "ship", None, "SafeLogoff", &[], &empty, None)
        .await
        .expect("SafeLogoff restart");
    let _ = events(&ctx, P1, "OnSafeLogoffTimerStarted");

    ctx.advance_secs(31);
    let ev_sla = events(&ctx, P1, "OnSafeLogoffActivated");
    assert_eq!(ev_sla.len(), 1, "P1 receives OnSafeLogoffActivated");
    assert!(ev_sla[0].is_empty(), "OnSafeLogoffActivated has arity 0");

    // 6. Crimewatch flag updates broadcast to system via real emit_crimewatch_timer_notifications
    let now_unix = ctx.now_unix_secs();
    if let Ok(mut cw) = ctx.crimewatch.write() {
        let entry = cw.states.entry(P1).or_default();
        entry.suspect_flag_until_unix = now_unix + 900;
    }
    eve_server::services::combat_crimewatch::emit_crimewatch_timer_notifications(&ctx, P1, now_unix);
    let ev_crim = events(&ctx, P1, "OnSystemCriminalFlagUpdates");
    assert_eq!(ev_crim.len(), 1, "P1 receives OnSystemCriminalFlagUpdates");
    assert_eq!(
        ev_crim[0],
        vec![
            PyRep::List(vec![]),
            PyRep::List(vec![PyRep::Integer(P1)]),
            PyRep::List(vec![]),
        ]
    );

    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.disapproval_flags.insert(P1, now_unix + 300);
    });
    eve_server::services::combat_crimewatch::emit_crimewatch_timer_notifications(&ctx, P1, now_unix);
    let ev_dis = events(&ctx, P1, "OnSystemDisapprovalFlagUpdates");
    assert_eq!(ev_dis.len(), 1, "P1 receives OnSystemDisapprovalFlagUpdates");
    assert_eq!(
        ev_dis[0],
        vec![PyRep::List(vec![]), PyRep::List(vec![PyRep::Integer(P1)])]
    );
}

}

// Folded submodule: notify_live_rpc_closure
#[allow(unused_imports, dead_code)]
mod notify_live_rpc_closure {
    use super::*;

// Live client-RPC notification closure tests (`notify:On*`).
//
// Verifies server-to-client V24 notifications emitted by real domain RPCs
// (`raffleProxy.CreateRaffle`, `raffleProxy.BuyRandomTickets`).


use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const PILOT_C: i64 = 90_000_003;
const JITA_STATION: i64 = 60_003_760;

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
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

async fn login_in_station(
    ctx: &ServerContext,
    conn_id: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(conn_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(JITA_STATION);
            ch.balance = IskCents::from_isk(100_000_000);
        }
    });
    sess.station_id = Some(JITA_STATION);
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        out.push((n.event, n.args));
    }
    out
}

fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], name: &str) -> &'a [PyRep] {
    notifs
        .iter()
        .find(|(e, _)| e == name)
        .map(|(_, args)| args.as_slice())
        .unwrap_or_else(|| {
            panic!(
                "Expected notification {name}, got {:?}",
                notifs.iter().map(|(e, _)| e).collect::<Vec<_>>()
            )
        })
}

// parity: notify:OnRaffleCreatedServer, notify:OnRaffleUpdatedServer, notify:OnRaffleFinishedServer
#[tokio::test]
async fn test_notify_live_hypernet_raffle_notifications() {
    eve_testkit::attest(&[
        "notify:OnRaffleCreatedServer",
        "notify:OnRaffleUpdatedServer",
        "notify:OnRaffleFinishedServer",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let prize_id = 640_000_110_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "test_c".to_string(),
            eve_db::AccountRecord {
                user_id: 3,
                username: "test_c".to_string(),
                password_token: eve_db::WorldRepository::hash_password_token("test_c", "test_c-secret"),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        if let Some(mut char_c) = snap.characters.get(&PILOT_A).cloned() {
            char_c.character_id = PILOT_C;
            char_c.user_id = 3;
            char_c.name = "Test C".to_string();
            snap.characters.insert(PILOT_C, char_c);
        }
    });
    let mut sess_a = login_in_station(&ctx, 9_501, "demo", "demo-secret", PILOT_A).await;
    let mut sess_b = login_in_station(&ctx, 9_502, "other", "other-secret", PILOT_B).await;
    let mut sess_c = login_in_station(&ctx, 9_503, "test_c", "test_c-secret", PILOT_C).await;
    rpc(&ctx, &mut sess_c, "raffleProxy", "SubscribeToRaffle", &[]).await.expect("sub");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(prize_id, make_item(prize_id, 587, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true));
        snap.items.insert(prize_id + 1, make_item(prize_id + 1, 52_568, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 10, false));
    });
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    let raffle_rep = rpc(
        &ctx,
        &mut sess_a,
        "raffleProxy",
        "CreateRaffle",
        &[
            PyRep::Integer(prize_id),
            PyRep::Integer(8),
            PyRep::Integer(1_000_000),
        ],
    )
    .await
    .expect("CreateRaffle");
    let raffle_id = raffle_rep.dict_get("raffle_id").and_then(|v| v.as_i64()).expect("raffle_id");
    let create_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let created_args = find_notif(&create_notifs, "OnRaffleCreatedServer");
    assert_eq!(created_args[0], PyRep::Integer(raffle_id));
    assert_eq!(created_args[1].dict_get("raffle_id").and_then(|v| v.as_i64()), Some(raffle_id));

    let create_notifs_c = drain_all(&ctx, &mut sess_c, PILOT_C);
    let created_args_c = find_notif(&create_notifs_c, "OnRaffleCreatedServer");
    assert_eq!(created_args_c[0], PyRep::Integer(raffle_id));

    rpc(&ctx, &mut sess_b, "raffleProxy", "BuyRandomTickets", &[PyRep::Integer(raffle_id), PyRep::Integer(8)])
        .await
        .expect("BuyRandomTickets");
    let buy_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    let updated_args = find_notif(&buy_notifs_b, "OnRaffleUpdatedServer");
    assert_eq!(updated_args[0], PyRep::Integer(raffle_id));
    let finished_args = find_notif(&buy_notifs_b, "OnRaffleFinishedServer");
    assert_eq!(finished_args[0], PyRep::Integer(raffle_id));
    let win_ticket = finished_args[1].as_i64().expect("winning ticket index");
    assert!((0..8).contains(&win_ticket));

    let buy_notifs_c = drain_all(&ctx, &mut sess_c, PILOT_C);
    let updated_args_c = find_notif(&buy_notifs_c, "OnRaffleUpdatedServer");
    assert_eq!(updated_args_c[0], PyRep::Integer(raffle_id));
    let finished_args_c = find_notif(&buy_notifs_c, "OnRaffleFinishedServer");
    assert_eq!(finished_args_c[0], PyRep::Integer(raffle_id));
    assert_eq!(finished_args_c[1], PyRep::Integer(win_ticket));
}

}

// Folded submodule: notify_p7_sp1_sp2_closure
#[allow(unused_imports, dead_code)]
mod notify_p7_sp1_sp2_closure {
    use super::*;

// Integration tests for `track/p7-sp1-sp2` notification parity closure.
// Exercises real server-side RPCs and verifies emitted V24 MachoNet notifications.


use std::collections::HashMap;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

use crate::common;

const P1: i64 = 90_000_001;
const JITA_SYSTEM: u32 = 30000142;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

async fn pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    let args = [PyRep::Integer(char_id)];
    ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &args, &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    ctx.drain_notifications_for_char(char_id);
    s
}

fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
    let all = ctx.drain_notifications_for_char(char_id);
    let mut matched = Vec::new();
    let mut keep = Vec::new();
    for n in all {
        if n.event == event {
            matched.push(n.args);
        } else {
            keep.push(n);
        }
    }
    if !keep.is_empty() {
        if let Ok(mut guard) = ctx.pending_notifications.write() {
            guard.entry(char_id).or_default().extend(keep);
        }
    }
    matched
}

#[tokio::test]
async fn test_notify_operations_and_progression_transitions() {
    // parity: notify:OnOperationDeactivated
    let ctx = new_ctx();
    let mut s = pilot(&ctx, 2, "demo", "demo-secret", P1).await;
    s.solar_system_id = Some(JITA_SYSTEM);
    let kw = HashMap::new();
    let mut op_ids: Vec<i64> = eve_server::services::operations_manager::get_operation_defs(&ctx.sde)
        .op_tasks
        .keys()
        .copied()
        .collect();
    op_ids.sort_unstable();
    let (op1, op2) = (op_ids[0], op_ids[1]);

    ctx.dispatch_rpc(
        &mut s,
        "operationsManager",
        None,
        "activate_operation_in_solar_system",
        &[PyRep::Integer(op1), PyRep::Integer(i64::from(JITA_SYSTEM))],
        &kw,
        None,
    )
    .await
    .expect("activate op 1");
    assert!(
        events(&ctx, P1, "OnOperationDeactivated").is_empty(),
        "First operation activation must not emit OnOperationDeactivated"
    );

    ctx.dispatch_rpc(
        &mut s,
        "operationsManager",
        None,
        "activate_operation_in_solar_system",
        &[PyRep::Integer(op2), PyRep::Integer(i64::from(JITA_SYSTEM))],
        &kw,
        None,
    )
    .await
    .expect("activate op 2");
    let deact_ev = events(&ctx, P1, "OnOperationDeactivated");
    assert_eq!(deact_ev.len(), 1);
    assert!(deact_ev[0].is_empty());
}

}

// Folded submodule: notify_real_emitters_batch8_part5
#[allow(unused_imports, dead_code)]
mod notify_real_emitters_batch8_part5 {
    use super::*;

// Real server-side RPC tests for Notify/RPC Batch 8 (Part 5: Session, Localization & LoadService RPCs).
//
// Covers all 8 Group 5 Session / Localization / LoadService parity rows:
// - `rpc:userSvc.ProcessEVESessionCreated`
// - `rpc:sessionMgr.EndAllGameSessions`
// - `rpc:sessionMgr.RemoveSessionsFromServer`
// - `rpc:localizationServer.ReloadFSDPickle`
// - `rpc:localizationServer.UpdateLocalizationQAWrap`
// - `rpc:loadService.GetTotalStats`
// - `rpc:loadService.StartLoad`
// - `rpc:loadService.StopLoad`


use crate::common;
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;

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

// parity: rpc:userSvc.ProcessEVESessionCreated, rpc:sessionMgr.EndAllGameSessions
// parity: rpc:sessionMgr.RemoveSessionsFromServer
#[tokio::test]
async fn notify_batch8_session_localization_and_load_rpcs() {
    eve_testkit::attest(&[
        "rpc:userSvc.ProcessEVESessionCreated",
        "rpc:sessionMgr.EndAllGameSessions",
        "rpc:sessionMgr.RemoveSessionsFromServer",
        ]);

    let ctx = new_ctx();
    let mut unauth = SessionState::new(9_500);

    // Negative: unauthenticated rejections across all 8 RPCs
    for (svc, method, args) in [
        ("userSvc", "ProcessEVESessionCreated", vec![]),
        ("sessionMgr", "EndAllGameSessions", vec![]),
        ("sessionMgr", "RemoveSessionsFromServer", vec![PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(9_502)])]),
        ("localizationServer", "ReloadFSDPickle", vec![]),
        ("localizationServer", "UpdateLocalizationQAWrap", vec![PyRep::Dict(Vec::new())]),
        ("loadService", "GetTotalStats", vec![]),
        ("loadService", "StartLoad", vec![]),
        ("loadService", "StopLoad", vec![]),
    ] {
        assert_eq!(
            rpc(&ctx, &mut unauth, svc, method, &args).await.unwrap_err().code,
            "Unauthorized",
            "{svc}.{method} must require authentication"
        );
    }

    let mut sess_a = SessionState::new(9_501);
    common::login_pilot(&ctx, &mut sess_a, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess_a, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)])
        .await
        .expect("SelectCharacterID A");

    let mut sess_b = SessionState::new(9_502);
    common::login_pilot(&ctx, &mut sess_b, "other", "other-secret").await;
    rpc(&ctx, &mut sess_b, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_B)])
        .await
        .expect("SelectCharacterID B");

    // 1. userSvc.ProcessEVESessionCreated: succeeds for owner, rejects mismatched character ID
    let created_rep = rpc(
        &ctx,
        &mut sess_a,
        "userSvc",
        "ProcessEVESessionCreated",
        &[PyRep::Integer(PILOT_A)],
    )
    .await
    .expect("ProcessEVESessionCreated");
    assert_eq!(created_rep, PyRep::None);
    let recorded_sessions = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs.get("user_sessions").cloned().unwrap_or_default()
    });
    assert!(recorded_sessions.contains("\"session_id\":9501"));

    let mut sess_a_user = SessionState::new(9_503);
    common::login_pilot(&ctx, &mut sess_a_user, "demo", "demo-secret").await;
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a_user,
            "userSvc",
            "ProcessEVESessionCreated",
            &[PyRep::Integer(PILOT_B)],
        )
        .await
        .unwrap_err()
        .code,
        "UserError"
    );
    // Re-bind sess_a after sess_a_user displaced it
    common::login_pilot(&ctx, &mut sess_a, "demo", "demo-secret").await;
    sess_a.character_id = Some(PILOT_A);
    sess_a.role |= ROLE_GM_DEFAULT;

    // Negative: non-GM player rejected from sessionMgr, localizationServer, and loadService admin RPCs
    for (svc, method, args, expected_code) in [
        ("sessionMgr", "EndAllGameSessions", vec![], "InsufficientRole"),
        ("sessionMgr", "RemoveSessionsFromServer", vec![PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(9_502)])], "InsufficientRole"),
        ("localizationServer", "ReloadFSDPickle", vec![], "RoleRequired"),
        ("localizationServer", "UpdateLocalizationQAWrap", vec![PyRep::Dict(Vec::new())], "RoleRequired"),
        ("loadService", "GetTotalStats", vec![], "UserError"),
        ("loadService", "StartLoad", vec![], "UserError"),
        ("loadService", "StopLoad", vec![], "UserError"),
    ] {
        assert_eq!(
            rpc(&ctx, &mut sess_b, svc, method, &args).await.unwrap_err().code,
            expected_code,
            "{svc}.{method} must reject non-GM caller"
        );
    }

    // Grant GM role to sess_a
    sess_a.role |= ROLE_GM_DEFAULT;

    // 2. sessionMgr.RemoveSessionsFromServer & sessionMgr.EndAllGameSessions
    let (tx_a, mut rx_a) = tokio::sync::mpsc::unbounded_channel();
    let (tx_b, mut rx_b) = tokio::sync::mpsc::unbounded_channel();
    let (tx_c, mut rx_c) = tokio::sync::mpsc::unbounded_channel();
    ctx.register_session_closer(9_501, tx_a);
    ctx.register_session_closer(9_502, tx_b);
    ctx.register_session_closer(9_503, tx_c);
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(
            "active_sessions".into(),
            serde_json::to_string(&vec![9_501_i64, 9_502_i64, 9_503_i64]).unwrap(),
        );
    });
    let rem_rep = rpc(
        &ctx,
        &mut sess_a,
        "sessionMgr",
        "RemoveSessionsFromServer",
        &[PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(9_502)])],
    )
    .await
    .expect("RemoveSessionsFromServer");
    assert_eq!(rem_rep, PyRep::None);
    assert_eq!(
        rx_b.try_recv().expect("sess_b disconnect notification").0,
        "RemoveSession"
    );
    assert!(!ctx.session_closers.read().unwrap().contains_key(&9_502));
    assert!(ctx.session_closers.read().unwrap().contains_key(&9_501));
    assert!(ctx.session_closers.read().unwrap().contains_key(&9_503));
    let active_after_rem: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("active_sessions")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default()
    });
    assert_eq!(active_after_rem, vec![9_501, 9_503]);

    let end_all_rep = rpc(&ctx, &mut sess_a, "sessionMgr", "EndAllGameSessions", &[])
        .await
        .expect("EndAllGameSessions");
    assert_eq!(end_all_rep, PyRep::None);
    assert_eq!(
        rx_c.try_recv().expect("sess_c disconnect notification").0,
        "EndAllGameSessions"
    );
    assert!(!ctx.session_closers.read().unwrap().contains_key(&9_503));
    assert!(rx_a.try_recv().is_err(), "caller session must not be disconnected");
    assert!(
        ctx.session_closers.read().unwrap().contains_key(&9_501),
        "caller session must survive EndAllGameSessions"
    );
    let active_after_end_all: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("active_sessions")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default()
    });
    assert_eq!(active_after_end_all, vec![9_501]);

    // 3. localizationServer.ReloadFSDPickle & UpdateLocalizationQAWrap
    let reload_err = rpc(
        &ctx,
        &mut sess_a,
        "localizationServer",
        "ReloadFSDPickle",
        &[PyRep::String("en-us.pickle".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(reload_err.code, "UnimplementedRPC");

    // Non-GM caller rejected from UpdateLocalizationQAWrap(bool) with RoleRequired
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_b,
            "localizationServer",
            "UpdateLocalizationQAWrap",
            &[PyRep::Bool(true)],
        )
        .await
        .unwrap_err()
        .code,
        "RoleRequired"
    );

    // GM caller succeeds with boolean toggle (DERIVED QA-wrap); non-bool overload fails closed with UnimplementedRPC
    let qawrap_rep = rpc(
        &ctx,
        &mut sess_a,
        "localizationServer",
        "UpdateLocalizationQAWrap",
        &[PyRep::Bool(true)],
    )
    .await
    .expect("UpdateLocalizationQAWrap");
    assert_eq!(qawrap_rep, PyRep::Bool(true));
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a,
            "localizationServer",
            "UpdateLocalizationQAWrap",
            &[PyRep::Dict(vec![(
                PyRep::Integer(42_001),
                PyRep::String("[QA] Warp Drive Active".into()),
            )])],
        )
        .await
        .unwrap_err()
        .code,
        "UnimplementedRPC"
    );

    // 4. loadService.StartLoad, GetTotalStats, StopLoad
    let stats_0 = rpc(&ctx, &mut sess_a, "loadService", "GetTotalStats", &[])
        .await
        .expect("GetTotalStats initial");
    assert_eq!(stats_0.dict_get("active").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(stats_0.dict_get("runs_started").and_then(|v| v.as_i64()), Some(0));
    assert!(stats_0.dict_get("sde_types_count").and_then(|v| v.as_i64()).unwrap_or(0) > 1_000);

    let start_rep = rpc(
        &ctx,
        &mut sess_a,
        "loadService",
        "StartLoad",
        &[PyRep::String("stress_profile_v24".into())],
    )
    .await
    .expect("StartLoad");
    assert_eq!(start_rep, PyRep::None);

    let stats_1 = rpc(&ctx, &mut sess_a, "loadService", "GetTotalStats", &[])
        .await
        .expect("GetTotalStats active");
    assert_eq!(stats_1.dict_get("active").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(stats_1.dict_get("runs_started").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(stats_1.dict_get("total_requests").and_then(|v| v.as_i64()), Some(1));
    assert!(stats_1.dict_get("systems_stepped").and_then(|v| v.as_i64()).unwrap_or(0) >= 1);
    assert!(stats_1.dict_get("active_systems_count").and_then(|v| v.as_i64()).unwrap_or(0) >= 1);
    assert!(stats_1.dict_get("world_items_count").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert!(stats_1.dict_get("world_characters_count").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    let stop_rep = rpc(&ctx, &mut sess_a, "loadService", "StopLoad", &[])
        .await
        .expect("StopLoad");
    assert_eq!(stop_rep, PyRep::None);

    let stats_2 = rpc(&ctx, &mut sess_a, "loadService", "GetTotalStats", &[])
        .await
        .expect("GetTotalStats stopped");
    assert_eq!(stats_2.dict_get("active").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(stats_2.dict_get("total_requests").and_then(|v| v.as_i64()), Some(2));
}

}

// Folded submodule: parity_notifies_part1
#[allow(unused_imports, dead_code)]
mod parity_notifies_part1 {
    use super::*;

// Runtime integration tests for open `notify:*` parity rows (Part 1).


use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

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

fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 0.5,
        mass: 10_000_000.0,
        radius: 120.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
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

async fn setup_pilot_in_space(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
    ship_id: i64,
    ship_type: u32,
) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = SYS_JITA;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, ship_type, char_id, i64::from(SYS_JITA), InventoryFlag::None, 1, true),
        );
    });
    sess.solar_system_id = Some(SYS_JITA);
    sess.station_id = None;
    sess.ship_id = Some(ship_id);
    let _ = ctx
        .systems
        .get_or_spawn(SYS_JITA)
        .spawn_ball_with_combat(make_ball(ship_id, ship_type, DVec3::ZERO), Default::default())
        .await;
    ctx.advance_secs(12);
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

#[tokio::test]
async fn test_parity_notifies_abyssal_and_void_space() {
    // parity: notify:OnAbyssalContentFinished, notify:OnAbyssalLocationEntered
    // parity: notify:OnVoidSpaceContentFinished, notify:OnVoidSpaceEntered, notify:OnVoidSpaceRelog
    eve_testkit::attest(&[
        "notify:OnAbyssalContentFinished",
        "notify:OnAbyssalLocationEntered",
        "notify:OnVoidSpaceContentFinished",
        "notify:OnVoidSpaceEntered",
        "notify:OnVoidSpaceRelog",
    ]);
    let ctx = new_ctx();
    let mut sess = setup_pilot_in_space(&ctx, 1101, "demo", "demo-secret", PILOT_A, 710_001, 622).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = 30_000_021;
        }
        if let Some(ship) = snap.items.get_mut(&710_001) {
            ship.location_id = 30_000_021;
        }
        snap.items.insert(
            710_010,
            make_item(710_010, 47_762, PILOT_A, 710_001, InventoryFlag::Cargo, 1, false),
        );
    });
    sess.solar_system_id = Some(30_000_021);
    let _ = ctx
        .systems
        .get_or_spawn(30_000_021)
        .spawn_ball_with_combat(make_ball(710_001, 622, DVec3::ZERO), Default::default())
        .await;

    rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(710_010)])
        .await
        .expect("AbyssalEntranceDeployment");
    let gate_id = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        *reg.abyssal_traces.keys().next().expect("trace gate")
    });
    rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(gate_id)])
        .await
        .expect("AbyssalEntranceGateActivation");
    let abyss_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let entered = find_notif(&abyss_notifs, "OnAbyssalLocationEntered");
    let content_id = entered[0]
        .dict_get("content_id")
        .and_then(|v| v.as_i64())
        .expect("content_id");
    assert!(content_id > 0);

    eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        if let Some(p) = reg.abyssal_pockets.get_mut(&PILOT_A) {
            p.current_room = 3;
            p.rooms[2].npc_ball_ids.clear();
            p.rooms[2].cleared = true;
        }
    });
    rpc(&ctx, &mut sess, "abyssalMgr", "ClientIsReady", &[]).await.expect("ClientIsReady");
    let end_gate_id = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        reg.abyssal_pockets
            .get(&PILOT_A)
            .map(|p| 1_895_000_000 + p.pocket_id * 10 + 3)
            .expect("end gate")
    });
    if let Some(pocket_sys) = sess.solar_system_id {
        let abyss_sys = ctx.systems.get_or_spawn(pocket_sys);
        if let Ok(Some((_, gate_ball))) = abyss_sys.get_ball_snapshot(end_gate_id).await {
            let _ = abyss_sys.set_ball_position(710_001, gate_ball.position).await;
        }
    }
    rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEndGateActivation", &[PyRep::Integer(end_gate_id)])
        .await
        .expect("AbyssalEndGateActivation");
    let abyss_fin = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        find_notif(&abyss_fin, "OnAbyssalContentFinished"),
        &[PyRep::Integer(content_id), PyRep::Integer(1)]
    );

    // Void Space: enter -> relog -> exit
    let mut sess_b = setup_pilot_in_space(&ctx, 1102, "other", "other-secret", PILOT_B, 710_002, 622).await;
    let void_fil_type = ctx
        .sde
        .types
        .iter()
        .find(|(_, t)| t.group_id == 4145)
        .map(|(&id, _)| id)
        .unwrap_or(60_770);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            710_011,
            make_item(710_011, void_fil_type, PILOT_B, 710_002, InventoryFlag::Cargo, 1, false),
        );
    });
    ctx.advance_secs(12);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    rpc(&ctx, &mut sess_b, "voidSpaceMgr", "VoidSpacePlayerJump", &[PyRep::Integer(710_011)])
        .await
        .expect("VoidSpacePlayerJump");
    let vse = drain_all(&ctx, &mut sess_b, PILOT_B);
    let vse_args = find_notif(&vse, "OnVoidSpaceEntered");
    let vs_cid = vse_args[0]
        .dict_get("content_id")
        .and_then(|v| v.as_i64())
        .expect("void content_id");
    assert!(vs_cid > 0);

    let _ = rpc(&ctx, &mut sess_b, "voidSpaceMgr", "ClientIsReady", &[]).await;
    let relog = drain_all(&ctx, &mut sess_b, PILOT_B);
    let relog_args = find_notif(&relog, "OnVoidSpaceRelog");
    assert_eq!(relog_args[0].dict_get("content_id").and_then(|v| v.as_i64()), Some(vs_cid));

    let void_exit_gate = 999_001_i64;
    if let Some(vs_sys) = sess_b.solar_system_id {
        let void_sys = ctx.systems.get_or_spawn(vs_sys);
        let _ = void_sys
            .spawn_ball_with_combat(make_ball(void_exit_gate, 60_608, DVec3::ZERO), Default::default())
            .await;
        let _ = void_sys.set_ball_position(710_002, DVec3::ZERO).await;
    }
    ctx.advance_secs(12);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    rpc(&ctx, &mut sess_b, "voidSpaceMgr", "VoidSpaceEndGateActivation", &[PyRep::Integer(void_exit_gate)])
        .await
        .expect("VoidSpaceEndGateActivation");
    let void_fin = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(
        find_notif(&void_fin, "OnVoidSpaceContentFinished"),
        &[PyRep::Integer(vs_cid), PyRep::Integer(1)]
    );
}

#[tokio::test]
async fn test_parity_notifies_operations_and_node_graphs() {
    // parity: notify:OnOperationSkipped, notify:OnOperationsProgressReset, notify:OnOperationsServerFSDReload
    // parity: notify:OnStartClientNodeGraph, notify:OnClientNodeGraphUpdate, notify:OnClientNodeGraphMessage
    // parity: notify:OnStopClientNodeGraph, notify:OnStopClientNodeGraphById, notify:OnStopAllClientNodeGraphs
    eve_testkit::attest(&[
        "notify:OnOperationSkipped",
        "notify:OnOperationsProgressReset",
        "notify:OnOperationsServerFSDReload",
        "notify:OnStartClientNodeGraph",
        "notify:OnClientNodeGraphUpdate",
        "notify:OnClientNodeGraphMessage",
        "notify:OnStopClientNodeGraph",
        "notify:OnStopClientNodeGraphById",
        "notify:OnStopAllClientNodeGraphs",
    ]);
    let ctx = new_ctx();
    let mut sess = setup_pilot_in_space(&ctx, 1103, "demo", "demo-secret", PILOT_A, 710_003, 622).await;
    sess.role = ROLE_GM_DEFAULT;

    let op_id = *eve_server::services::operations_manager_helpers::get_operation_defs(&ctx.sde)
        .op_tasks
        .keys()
        .next()
        .expect("operation in SDE");
    rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String(format!("/operation skip 1 {op_id}"))])
        .await
        .expect("/operation skip");
    let skip_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&skip_n, "OnOperationSkipped"), &[PyRep::Integer(1), PyRep::Integer(op_id)]);

    rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/operation reset_progress".into())])
        .await
        .expect("/operation reset_progress");
    let reset_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&reset_n, "OnOperationsProgressReset").len(), 3);
    assert_eq!(find_notif(&reset_n, "OnStopAllClientNodeGraphs").len(), 0);

    rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/operation reload_fsd".into())])
        .await
        .expect("/operation reload_fsd");
    let reload_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&reload_n, "OnOperationsServerFSDReload").len(), 0);

    // Drive real AIR NPE node-graph runner RPCs -> emits OnStartClientNodeGraph,
    // OnClientNodeGraphUpdate, OnClientNodeGraphMessage, OnStopClientNodeGraph, OnStopClientNodeGraphById
    let mut npe_sess = SessionState::new(1105);
    common::login_pilot(&ctx, &mut npe_sess, "demo", "demo-secret").await;
    let doll = PyRep::keyval([("genderID", PyRep::Integer(1)), ("bloodlineID", PyRep::Integer(1))]);
    let portrait = PyRep::keyval([("backgroundID", PyRep::Integer(1))]);
    let npe_cid = rpc(
        &ctx,
        &mut npe_sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        &[
            PyRep::String("NPE Notify Pilot".into()),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(11),
            doll,
            portrait,
            PyRep::Integer(17),
        ],
    )
    .await
    .expect("CreateCharacterWithDoll")
    .as_i64()
    .expect("npe_cid");
    rpc(&ctx, &mut npe_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(npe_cid)])
        .await
        .expect("SelectCharacterID npe");
    rpc(
        &ctx,
        &mut npe_sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("start_tutorial".into())],
    )
    .await
    .expect("start_tutorial");
    let start_n = drain_all(&ctx, &mut npe_sess, npe_cid);
    let start_args = find_notif(&start_n, "OnStartClientNodeGraph");
    assert_eq!(start_args.len(), 3);
    assert_eq!(start_args[0], PyRep::Integer(716));
    assert!(start_args[1].as_i64().unwrap_or(0) > 0);
    rpc(
        &ctx,
        &mut npe_sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("new_eden_cinematic_ended".into())],
    )
    .await
    .expect("new_eden_cinematic_ended");
    let msg_n = drain_all(&ctx, &mut npe_sess, npe_cid);
    assert!(!find_notif(&msg_n, "OnClientNodeGraphUpdate").is_empty());
    assert_eq!(
        find_notif(&msg_n, "OnClientNodeGraphMessage")
            .get(2)
            .and_then(|v| v.as_str()),
        Some("pocket_entered")
    );
    rpc(
        &ctx,
        &mut npe_sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("skip_tutorial".into())],
    )
    .await
    .expect("skip_tutorial");
    let stop_n = drain_all(&ctx, &mut npe_sess, npe_cid);
    let stop_inst_args = find_notif(&stop_n, "OnStopClientNodeGraph");
    assert_eq!(stop_inst_args.len(), 1);
    assert!(stop_inst_args[0].as_i64().unwrap_or(0) > 0);
    assert_eq!(find_notif(&stop_n, "OnStopClientNodeGraphById"), &[PyRep::Integer(716)]);
}

#[tokio::test]
async fn test_parity_notifies_dungeons_cloak_and_space() {
    // parity: notify:OnCloakModuleForceDeactivated, notify:OnFleetEscalatingPathMessage, notify:OnProgressionRoomJoined
    eve_testkit::attest(&[
        "notify:OnCloakModuleForceDeactivated",
        "notify:OnFleetEscalatingPathMessage",
        "notify:OnProgressionRoomJoined",
    ]);
    let ctx = new_ctx();
    let mut sess = setup_pilot_in_space(&ctx, 1104, "demo", "demo-secret", PILOT_A, 710_004, 622).await;

    // 1. OnCloakModuleForceDeactivated via real beyonce.CmdStop on cloaked ship in Abyssal space
    let sys_abyssal = 32_000_001_u32;
    let cloak_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 330)
        .map(|t| t.type_id)
        .unwrap_or(11578);
    sess.solar_system_id = Some(sys_abyssal);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_A) {
            c.solar_system_id = sys_abyssal;
        }
        snap.items.insert(
            710_099,
            make_item(710_099, cloak_type_id, PILOT_A, 710_004, InventoryFlag::HiSlot0, 1, true),
        );
        snap.module_active_states.insert(710_099);
    });
    let abyss_oid = ctx.bind_object(&sess, "beyonce", i64::from(sys_abyssal)).expect("bind abyss beyonce");
    let abyss_h = ctx.systems.get_or_spawn(sys_abyssal);
    let _ = abyss_h.spawn_ball(make_ball(710_004, 622, DVec3::ZERO)).await;
    let mut cs = eve_destiny::CombatState::default();
    cs.is_cloaked = true;
    let _ = abyss_h.set_combat_state(710_004, cs).await;
    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(abyss_oid), "CmdStop", &[], &HashMap::new(), None)
        .await
        .expect("CmdStop cloaked in abyssal");
    let cloak_n = drain_all(&ctx, &mut sess, PILOT_A);
    let decloak_args = find_notif(&cloak_n, "OnCloakModuleForceDeactivated");
    assert_eq!(decloak_args.len(), 2);
    assert_eq!(decloak_args[0], PyRep::String("AbyssalDeadspaceCloakBlocked".into()));

    // Restore PILOT_A to Jita for fleet escalation & dungeon progression checks
    sess.solar_system_id = Some(SYS_JITA);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_A) {
            c.solar_system_id = SYS_JITA;
        }
    });

    // 2. OnFleetEscalatingPathMessage via real combat anomaly clear in a fleet
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_A) {
            c.fleet_id = Some(77_001);
        }
        if let Some(c) = snap.characters.get_mut(&PILOT_B) {
            c.fleet_id = Some(77_001);
        }
        let mut fl = eve_db::FleetRecord::default();
        fl.fleet_id = 77_001;
        fl.boss_id = PILOT_A;
        fl.members = vec![PILOT_A, PILOT_B];
        snap.fleets.insert(77_001, fl);
    });
    eve_server::services::pve_exploration::ensure_system_pve_sites_seeded(&ctx, SYS_JITA).await;
    let site_id = "PVE-ANOM-30000142-1";
    let _ = ctx.drain_notifications_for_char(PILOT_B);
    let sys_h = ctx.systems.get_or_spawn(SYS_JITA);
    for _ in 0..5 {
        let (cleared, npcs) = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system[&SYS_JITA]
                .iter()
                .find(|s| s.site_id == site_id)
                .map(|s| (s.cleared, s.active_npc_ball_ids.clone()))
                .unwrap_or((true, Vec::new()))
        });
        if cleared {
            break;
        }
        if npcs.is_empty() {
            rpc(
                &ctx,
                &mut sess,
                "scanSvc",
                "EnterCombatAnomaly",
                &[PyRep::String(site_id.into()), PyRep::Bool(true)],
            )
            .await
            .expect("EnterCombatAnomaly");
            continue;
        }
        ctx.set_mechanic_rng_seed(Some(3));
        for nid in npcs {
            let _ = sys_h
                .apply_damage_with_attacker(
                    nid,
                    eve_destiny::DamageVector {
                        em: 1.0e7,
                        thermal: 1.0e7,
                        kinetic: 1.0e7,
                        explosive: 1.0e7,
                    },
                    Some(PILOT_A),
                )
                .await;
            let _ = sys_h.remove_ball(nid).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    }
    let esc_n: Vec<(String, Vec<PyRep>)> = ctx
        .drain_notifications_for_char(PILOT_B)
        .into_iter()
        .map(|n| (n.event, n.args))
        .collect();
    let fleet_esc = find_notif(&esc_n, "OnFleetEscalatingPathMessage");
    assert_eq!(fleet_esc.len(), 2);
    assert_eq!(fleet_esc[0], PyRep::Integer(PILOT_A));
    assert!(fleet_esc[1].as_i64().unwrap_or(0) > 0);

    let inst_id = eve_server::services::pve_dungeon_runtime::spawn_sde_dungeon_runtime_instance(
        &ctx,
        PILOT_A,
        SYS_JITA,
        2001,
        "Deadspace",
        [120_000_000.0, 0.0, 80_000_000.0],
        true,
    )
    .await
    .expect("spawn_sde_dungeon_runtime_instance");
    let dng_state = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id)
        .expect("get_dungeon_instance_state");
    let entry_gate_id = dng_state.dict_get("entryGateBallID").and_then(|v| v.as_i64()).unwrap();
    let sys_h = ctx.systems.get_or_spawn(SYS_JITA);
    let entry_gate_pos = sys_h.get_ball_snapshot(entry_gate_id).await.unwrap().unwrap().1.position;
    let _ = sys_h.set_ball_position(710_004, entry_gate_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    rpc(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(entry_gate_id)])
        .await
        .expect("ActivateAccelerationGate");
    let pr_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&pr_n, "OnProgressionRoomJoined").len(), 5);
}

}

// Folded submodule: parity_notifies_part2
#[allow(unused_imports, dead_code)]
mod parity_notifies_part2 {
    use super::*;

// Runtime integration tests for open `notify:*` parity rows (Part 2).


use crate::common;
use eve_db::{AsteroidRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::system_actor::AsteroidState;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;

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

async fn setup_pilot(ctx: &ServerContext, sid: u64) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(PILOT_A)],
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    let _ = drain_all(ctx, &mut sess, PILOT_A);
    sess
}

#[tokio::test]
async fn test_parity_notifies_seasons_campaigns_and_skills() {
    // parity: notify:OnSeasonOmegaRewardsClaimed, notify:OnSeasonalGoalCompleted, notify:OnSeasonalGoalsReset
    eve_testkit::attest(&[
        "notify:OnSeasonOmegaRewardsClaimed",
        "notify:OnSeasonalGoalCompleted",
        "notify:OnSeasonalGoalsReset",
    ]);
    let ctx = new_ctx();
    let mut sess = setup_pilot(&ctx, 1201).await;
    sess.role = ROLE_GM_DEFAULT;

    let mut target_ch_id = 1_005_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.clone_grade = 2;
        }
        if let Some(mut def) =
            eve_server::services::season_fsd::default_seasonal_challenge_definitions()
                .into_iter()
                .next()
        {
            target_ch_id = def.challenge_id;
            def.client_event_driven = true;
            def.event_conditions.clear();
            def.max_progress = 2;
            def.points = 20;
            snap.service_state_blobs.insert(
                eve_server::services::season_fsd::SEASONAL_CHALLENGES_CONFIG_KEY.into(),
                serde_json::to_string(&vec![def]).unwrap(),
            );
        }
    });
    let _ = rpc(&ctx, &mut sess, "seasonManager", "fill_up_and_return_challenges_for_character", &[])
        .await
        .expect("fill challenges");
    for _ in 0..2 {
        rpc(
            &ctx,
            &mut sess,
            "seasonManager",
            "process_client_event",
            &[PyRep::Integer(target_ch_id)],
        )
        .await
        .expect("complete challenge");
    }
    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    let claim = rpc(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(target_ch_id)])
        .await
        .expect("claim_challenge_rewards");
    assert_eq!(claim.dict_get("claimed").and_then(|v| v.as_bool()), Some(true));
    let claim_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&claim_n, "OnSeasonalGoalCompleted")[0], PyRep::Integer(0));
    assert_eq!(
        find_notif(&claim_n, "OnSeasonOmegaRewardsClaimed"),
        &[PyRep::List(vec![PyRep::Integer(0), PyRep::Integer(1)])]
    );

    rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String(format!("/season reset_goals {PILOT_A}"))])
        .await
        .expect("/season reset_goals");
    let reset_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&reset_n, "OnSeasonalGoalsReset").len(), 0);
}

#[tokio::test]
async fn test_parity_notifies_plex_dogma_and_brain() {
    // parity: notify:OnEffectMessage, notify:OnPlexDepositFailed, notify:OnPlexWithdrawFailed
    eve_testkit::attest(&[
        "notify:OnEffectMessage",
        "notify:OnPlexDepositFailed",
        "notify:OnPlexWithdrawFailed",
    ]);
    let ctx = new_ctx();
    let mut sess = setup_pilot(&ctx, 1202).await;

    // 1. OnPlexDepositFailed via invbroker.DepositPlexToVault with reference
    let jita_station = 60_003_760_i64;
    let trit_id = 880_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(jita_station);
        }
        snap.adjust_plex_vault(PILOT_A, 50);
        snap.items.insert(
            trit_id,
            InventoryItemRecord {
                item_id: trit_id,
                type_id: 34,
                owner_id: PILOT_A,
                location_id: jita_station,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    sess.station_id = Some(jita_station);
    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    assert!(rpc(
        &ctx,
        &mut sess,
        "invbroker",
        "DepositPlexToVault",
        &[
            PyRep::Integer(jita_station),
            PyRep::Integer(trit_id),
            PyRep::Integer(10),
            PyRep::Integer(4242),
        ],
    )
    .await
    .is_err());
    let dep_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        find_notif(&dep_n, "OnPlexDepositFailed"),
        &[PyRep::String("NotPlex".into()), PyRep::Integer(4242)]
    );

    // 2. OnPlexWithdrawFailed via invbroker.WithdrawPlexFromVault with reference
    let mut wd_kwargs = HashMap::new();
    wd_kwargs.insert("reference".to_string(), PyRep::Integer(5353));
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "WithdrawPlexFromVault",
            &[PyRep::Integer(99_999)],
            &wd_kwargs,
            None,
        )
        .await
        .is_err());
    let wd_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        find_notif(&wd_n, "OnPlexWithdrawFailed"),
        &[PyRep::String("NotEnoughPlex".into()), PyRep::Integer(5353)]
    );

    // 3. OnEffectMessage via mining_cycle::process_mining_cycles when capacitor runs dry
    let sys_jita = 30_000_142_u32;
    let miner_id = 880_011_i64;
    let rock_id = 880_012_i64;
    let miner_type = 483_u32;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT_A).or_default().insert(
            3386,
            eve_db::TrainedSkillRecord {
                type_id: 3386,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        let active_ship = snap.characters.get(&PILOT_A).map(|c| c.active_ship_id).unwrap_or(0);
        if let Some(sh) = snap.items.get_mut(&active_ship) {
            sh.type_id = 32880;
        }
        snap.items.retain(|_, i| i.location_id != active_ship);
        snap.module_online_states.insert(miner_id, true);
        snap.items.insert(
            miner_id,
            InventoryItemRecord {
                item_id: miner_id,
                type_id: miner_type,
                owner_id: PILOT_A,
                location_id: active_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let undock = rpc(&ctx, &mut sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock");
    let ship_id = undock.dict_get("shipID").and_then(|v| v.as_i64()).expect("shipID");
    ctx.invalidate_ship_fitting(ship_id);
    let beyonce_id = undock
        .dict_get("beyonceBoundID")
        .and_then(|v| v.as_i64())
        .map(|v| v as u64);
    ctx.dispatch_rpc(&mut sess, "beyonce", beyonce_id, "CmdStop", &[], &HashMap::new(), None)
        .await
        .expect("CmdStop");

    let sys = ctx.systems.get_or_spawn(sys_jita);
    let (_, ship_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().expect("ship ball");
    let rock_pos = ship_ball.position + DVec3::new(2_000.0, 0.0, 0.0);
    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: rock_id,
        solar_system_id: sys_jita,
        belt_id: 40_000_001,
        type_id: 1230,
        quantity: 10_000,
        initial_quantity: 10_000,
        position: (rock_pos.x, rock_pos.y, rock_pos.z),
        radius: 100.0,
    });
    let _ = sys
        .spawn_asteroid(
            AsteroidState {
                asteroid_id: rock_id,
                belt_id: 40_000_001,
                type_id: 1230,
                initial_units: 10_000,
                remaining_units: 10_000,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: rock_id,
                type_id: 1230,
                position: rock_pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e10,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await;
    rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(rock_id)])
        .await
        .expect("AddTarget");
    for _ in 0..30 {
        if sys
            .get_target_locks(ship_id)
            .await
            .unwrap()
            .iter()
            .any(|l| l.target_id == rock_id && l.locked)
        {
            break;
        }
        sys.step_manual(1.0).await.unwrap();
    }
    rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(miner_id),
            PyRep::String("miningLaser".into()),
            PyRep::Integer(rock_id),
        ],
    )
    .await
    .expect("Activate Miner I");
    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    ctx.advance_secs(65);
    let mut cs = ctx.build_ship_combat_state(PILOT_A, ship_id, 32880);
    cs.max_capacitor = 0.0;
    cs.capacitor = 0.0;
    let _ = sys.set_combat_state(ship_id, cs).await;
    let _ = eve_server::services::mining_cycle::process_mining_cycles(&ctx).await;
    let eff_n = drain_all(&ctx, &mut sess, PILOT_A);
    let eff_args = find_notif(&eff_n, "OnEffectMessage");
    assert_eq!(eff_args[0], PyRep::String("NotEnoughEnergy".into()));
    assert_eq!(eff_args[1].dict_get("moduleID"), Some(&PyRep::Integer(miner_id)));
    assert_eq!(
        eff_args[1].dict_get("effectname"),
        Some(&PyRep::String("miningLaser".into()))
    );
    assert!(eff_args[1].dict_get("need").is_some());
    assert_eq!(eff_args[1].dict_get("got"), Some(&PyRep::Real(0.0)));
}

}

#[allow(unused_imports)]
pub mod b23 {


use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::pve_dungeon_runtime::{
    check_dungeon_triggers, with_dungeon_runtime_store_db, DungeonRuntimeInstance,
    DungeonRuntimeObject, DungeonRuntimeRoom, DungeonTrigger, DungeonTriggerAction,
    DungeonTriggerKind,
};
use eve_server::services::pve_expeditions::EscalatingPathEntry;
use eve_server::services::pve_exploration::notify_pve_destruction_events;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;
const SYS_PERIMETER: u32 = 30_000_144;
const SYS_ABYSSAL: u32 = 32_000_001;

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

fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 0.5,
        mass: 10_000_000.0,
        radius: 120.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
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

fn has_notif(notifs: &[(String, Vec<PyRep>)], event: &str) -> bool {
    notifs.iter().any(|(ev, _)| ev == event)
}

async fn setup_pilot_in_space(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    gm: bool,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = SYS_JITA;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(
                ship_id,
                ship_type_id,
                char_id,
                i64::from(SYS_JITA),
                InventoryFlag::None,
                1,
                true,
            ),
        );
    });
    let sys = ctx.systems.get_or_spawn(SYS_JITA);
    let _ = sys
        .spawn_ball_with_combat(
            make_ball(ship_id, ship_type_id, DVec3::new(1000.0, 0.0, 0.0)),
            Default::default(),
        )
        .await;
    let mut sess = SessionState::new(char_id as u64);
    let (user, pass) = if char_id == PILOT_A {
        ("demo", "demo-secret")
    } else {
        ("other", "other-secret")
    };
    crate::common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    sess.solar_system_id = Some(SYS_JITA);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    if gm {
        sess.role |= ROLE_GM_DEFAULT;
    }
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

#[tokio::test]
async fn b23_dungeon_triggers_and_camera_interest_notifications() {
    let ctx = ServerContext::new();
    let ship_id = 700_002_i64;
    let mut sess = setup_pilot_in_space(&ctx, PILOT_A, ship_id, 587, false).await;

    let handle = ctx.systems.get_or_spawn(SYS_JITA);
    // Start ship far away (50 km) from room center
    let _ = handle
        .spawn_ball(make_ball(ship_id, 587, DVec3::new(50_000.0, 0.0, 0.0)))
        .await;

    let inst_id = 88_001_i64;
    let dungeon_id = 19_u32;
    let approach_kind = DungeonTriggerKind::OnApproach {
        target_ball_id: None,
        target_object_index: None,
        anchor_pos: [0.0, 0.0, 0.0],
        radius_m: 5_000.0,
    };
    with_dungeon_runtime_store_db(&ctx.db, |store| {
        store.instances.insert(
            inst_id,
            DungeonRuntimeInstance {
                instance_id: inst_id,
                dungeon_id,
                dungeon_name: "Trigger Test Dungeon".to_string(),
                category: "combat".to_string(),
                archetype: "guristas".to_string(),
                solar_system_id: SYS_JITA,
                owner_char_id: PILOT_A,
                base_position: [0.0, 0.0, 0.0],
                disallow_mwd: false,
                entry_gate_ball_id: None,
                entry_gate_pos: None,
                entry_gate_locked: false,
                current_room_index: 0,
                created_unix: 1_700_000_000,
                completed: false,
                despawned: false,
                active_objective_id: 0,
                completed_task_ids: Vec::new(),
                last_completed_task_id: None,
                rooms: vec![DungeonRuntimeRoom {
                    room_index: 0,
                    room_id: 1,
                    room_name: "Room 1".to_string(),
                    position: [0.0, 0.0, 0.0],
                    gate_destination_pos: [0.0, 0.0, 0.0],
                    gated: false,
                    disallow_mwd: None,
                    gate_ball_id: None,
                    gate_locked: false,
                    spawned: true,
                    cleared: false,
                    objects: vec![DungeonRuntimeObject {
                        ball_id: 900_123,
                        type_id: 29624,
                        name: "Beacon".to_string(),
                        position: [0.0, 0.0, 0.0],
                        radius: 100.0,
                        is_container: false,
                        loot_items: vec![],
                        spawned: true,
                    }],
                    npc_groups: vec![],
                    triggers: vec![
                        DungeonTrigger {
                            trigger_id: 1,
                            kind: approach_kind.clone(),
                            action: DungeonTriggerAction::CameraInterest {
                                item_id: 900_123,
                                speed_bits: 1.5_f64.to_bits(),
                                duration_bits: 4.0_f64.to_bits(),
                            },
                            fired: false,
                        },
                        DungeonTrigger {
                            trigger_id: 2,
                            kind: approach_kind.clone(),
                            action: DungeonTriggerAction::TriggerAmbientSound {
                                sound_id: 401,
                                entity_item_id: 900_123,
                                emitter_id: 77,
                            },
                            fired: false,
                        },
                        DungeonTrigger {
                            trigger_id: 3,
                            kind: approach_kind.clone(),
                            action: DungeonTriggerAction::TriggerAudio {
                                audio_url: "wise:/dungeon_alert_01".to_string(),
                            },
                            fired: false,
                        },
                        DungeonTrigger {
                            trigger_id: 4,
                            kind: approach_kind.clone(),
                            action: DungeonTriggerAction::TriggerMessage {
                                message_type: 2,
                                message_id: 5001,
                            },
                            fired: false,
                        },
                        DungeonTrigger {
                            trigger_id: 5,
                            kind: approach_kind,
                            action: DungeonTriggerAction::TriggerMusic {
                                music_trigger_id: 909,
                            },
                            fired: false,
                        },
                    ],
                }],
            },
        );
    });

    // Negative: out of range (50 km > 5 km) -> 0 triggers fire, 0 notifications emitted
    let fired = check_dungeon_triggers(&ctx, PILOT_A, SYS_JITA, ship_id).await;
    assert_eq!(fired, 0);
    let notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert!(!has_notif(&notifs, "OnDungeonCameraInterest"));
    assert!(!has_notif(&notifs, "OnDungeonTriggerAmbientSound"));
    assert!(!has_notif(&notifs, "OnDungeonTriggerAudio"));
    assert!(!has_notif(&notifs, "OnDungeonTriggerMessage"));
    assert!(!has_notif(&notifs, "OnDungeonTriggerMusic"));

    // Positive: move ship within 1 km of room center -> all 5 triggers fire & emit
    let _ = handle
        .spawn_ball(make_ball(ship_id, 587, DVec3::new(1_000.0, 0.0, 0.0)))
        .await;
    let fired = check_dungeon_triggers(&ctx, PILOT_A, SYS_JITA, ship_id).await;
    assert_eq!(fired, 5);

    let notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let cam = find_notif(&notifs, "OnDungeonCameraInterest");
    assert_eq!(cam.len(), 3);
    assert_eq!(cam[0].as_i64(), Some(900_123));

    let amb = find_notif(&notifs, "OnDungeonTriggerAmbientSound");
    assert_eq!(amb.len(), 3);
    assert_eq!(amb[0].as_i64(), Some(i64::from(dungeon_id)));
    assert_eq!(amb[1].as_i64(), Some(401));

    let aud = find_notif(&notifs, "OnDungeonTriggerAudio");
    assert_eq!(aud.len(), 2);
    assert_eq!(aud[0].as_i64(), Some(i64::from(dungeon_id)));
    assert_eq!(aud[1].as_str(), Some("wise:/dungeon_alert_01"));

    let msg = find_notif(&notifs, "OnDungeonTriggerMessage");
    assert_eq!(msg.len(), 2);
    assert_eq!(msg[0].as_i64(), Some(2));
    assert_eq!(msg[1].as_i64(), Some(5001));

    let mus = find_notif(&notifs, "OnDungeonTriggerMusic");
    assert_eq!(mus.len(), 2);
    assert_eq!(mus[0].as_i64(), Some(i64::from(dungeon_id)));
    assert_eq!(mus[1].as_i64(), Some(909));

    // Idempotency negative: second check does not re-emit already-fired triggers
    let fired_again = check_dungeon_triggers(&ctx, PILOT_A, SYS_JITA, ship_id).await;
    assert_eq!(fired_again, 0);
    let notifs2 = drain_all(&ctx, &mut sess, PILOT_A);
    assert!(notifs2.is_empty());
}

#[tokio::test]
async fn b23_fleet_escalation_and_decloak_notifications() {
    let ctx = ServerContext::new();
    let ship_a = 700_010_i64;
    let mut sess_a = setup_pilot_in_space(&ctx, PILOT_A, ship_a, 587, false).await;

    // 1. OnFleetEscalatingPathMessage:
    // Negative: solo pilot (not in fleet) triggers escalation -> no fleet notification
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_A) {
            c.fleet_id = None;
        }
    });
    let esc_entry = EscalatingPathEntry {
        instance_id: 500_001,
        character_id: PILOT_A,
        source_dungeon_id: 1001,
        expedition_dungeon_id: 2002,
        expedition_name: "Guristas Scout Outpost".to_string(),
        destination_solar_system_id: SYS_PERIMETER,
        step_index: 1,
        total_steps: 4,
        journal_text: "Escalation".to_string(),
        created_at_sec: 1_700_000_000,
        expires_at_sec: 1_700_086_400,
        dungeons_entered: vec![],
        journal_entry_template_id: 280_001,
    };
    notify_pve_destruction_events(
        &ctx.db,
        &ctx.pending_notifications,
        &ctx.notify_tx,
        PILOT_A,
        None,
        Some(esc_entry.clone()),
        None,
    );
    let notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!has_notif(&notifs, "OnFleetEscalatingPathMessage"));

    // Positive: PILOT_A in fleet 77_001 triggers escalation -> emits OnFleetEscalatingPathMessage
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_A) {
            c.fleet_id = Some(77_001);
        }
        let mut fl = eve_db::FleetRecord::default();
        fl.fleet_id = 77_001;
        fl.boss_id = PILOT_A;
        fl.members = vec![PILOT_A, PILOT_B];
        snap.fleets.insert(77_001, fl);
    });
    notify_pve_destruction_events(
        &ctx.db,
        &ctx.pending_notifications,
        &ctx.notify_tx,
        PILOT_A,
        None,
        Some(esc_entry),
        None,
    );
    let notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let fleet_esc = find_notif(&notifs, "OnFleetEscalatingPathMessage");
    assert_eq!(fleet_esc.len(), 2);
    assert_eq!(fleet_esc[0].as_i64(), Some(PILOT_A));
    assert_eq!(fleet_esc[1].as_i64(), Some(280_001));

    // 2. OnCloakModuleForceDeactivated:
    let cloak_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 330)
        .map(|t| t.type_id)
        .unwrap_or(11578);

    sess_a.solar_system_id = Some(SYS_ABYSSAL);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_A) {
            c.solar_system_id = SYS_ABYSSAL;
        }
    });
    let abyssal_beyonce_oid = ctx
        .bind_object(&sess_a, "beyonce", i64::from(SYS_ABYSSAL))
        .expect("bind beyonce abyssal");
    let abyssal_handle = ctx.systems.get_or_spawn(SYS_ABYSSAL);
    let _ = abyssal_handle
        .spawn_ball(make_ball(ship_a, 587, DVec3::ZERO))
        .await;
    // Negative: uncloaked ship in Abyssal -> CmdStop does not emit OnCloakModuleForceDeactivated
    ctx.dispatch_rpc(
        &mut sess_a,
        "beyonce",
        Some(abyssal_beyonce_oid),
        "CmdStop",
        &[],
        &HashMap::new(),
        None,
    )
    .await
    .expect("CmdStop uncloaked");
    let notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!has_notif(&notifs, "OnCloakModuleForceDeactivated"));

    // Positive: cloaked ship with active cloak module in Abyssal -> CmdStop force-decloaks & emits
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_A) {
            c.active_ship_id = ship_a;
            c.solar_system_id = SYS_ABYSSAL;
        }
        snap.items.insert(
            700_099,
            make_item(
                700_099,
                cloak_type_id,
                PILOT_A,
                ship_a,
                InventoryFlag::HiSlot0,
                1,
                true,
            ),
        );
        snap.module_active_states.insert(700_099);
    });
    let mut cs = eve_destiny::CombatState::default();
    cs.is_cloaked = true;
    let _ = abyssal_handle.set_combat_state(ship_a, cs).await;
    ctx.dispatch_rpc(
        &mut sess_a,
        "beyonce",
        Some(abyssal_beyonce_oid),
        "CmdStop",
        &[],
        &HashMap::new(),
        None,
    )
    .await
    .expect("CmdStop cloaked");
    let notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let decloak = find_notif(&notifs, "OnCloakModuleForceDeactivated");
    assert_eq!(decloak.len(), 2);
    assert_eq!(decloak[0].as_str(), Some("AbyssalDeadspaceCloakBlocked"));
}

}

#[allow(unused_imports)]
pub mod b30 {


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

    // AddTarget + step_manual -> PILOT_A receives add, PILOT_B receives otheradd
    assert!(rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(0)]).await.is_err());
    rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_b)]).await.expect("AddTarget");
    sys.step_manual(10.0).await.expect("step_manual");

    let notifs_a = ctx.drain_notifications_for_char(PILOT_A);
    let locker_add = notifs_a.iter().find(|n| n.event == "OnMultiEvent").expect("PILOT_A OnMultiEvent add");
    assert_eq!(
        locker_add.args,
        vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Tuple(vec![
                PyRep::String("OnTarget".into()),
                PyRep::String("add".into()),
                PyRep::Integer(ship_b),
                PyRep::String(String::new()),
            ]),
            PyRep::Integer(0),
        ])])]
    );
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

}

#[allow(unused_imports)]
pub mod b36 {


use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const NULLSEC_SYSTEM: u32 = 30_004_759;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

async fn pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    let args = [PyRep::Integer(char_id)];
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &args,
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    ctx.drain_notifications_for_char(char_id);
    s
}

fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
    let all = ctx.drain_notifications_for_char(char_id);
    let mut matched = Vec::new();
    let mut keep = Vec::new();
    for n in all {
        if n.event == event {
            matched.push(n.args);
        } else {
            keep.push(n);
        }
    }
    if !keep.is_empty() {
        if let Ok(mut guard) = ctx.pending_notifications.write() {
            guard.entry(char_id).or_default().extend(keep);
        }
    }
    matched
}

fn grant_all_combat_skills(ctx: &ServerContext, snap: &mut eve_db::WorldStateSnapshot, cid: i64, type_ids: &[u32]) {
    let sk = snap.skills.entry(cid).or_default();
    for sid in [
        3300_u32, 3302, 3305, 3308, 3310, 3311, 3312, 3315, 3316, 3318, 3319, 3320, 3334, 3413,
        3418, 3426, 20327, 21071, 21667,
    ] {
        sk.insert(
            sid,
            TrainedSkillRecord {
                type_id: sid,
                level: 5,
                skill_points: 1_280_000.0,
            },
        );
    }
    for &tid in type_ids {
        if let Some(t) = ctx.sde.types.get(&tid) {
            for sa in [182_u16, 183, 184] {
                if let Some(&req_f) = t.default_attributes.get(&sa) {
                    let rsid = req_f as u32;
                    if rsid > 0 {
                        sk.insert(
                            rsid,
                            TrainedSkillRecord {
                                type_id: rsid,
                                level: 5,
                                skill_points: 1_280_000.0,
                            },
                        );
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn test_b36_progression_notifications() {
    // parity: notify:OnProgressionAdvanced
    // parity: notify:OnProgressionTaskCompleted
    eve_testkit::attest(&[
        "notify:OnProgressionAdvanced",
        "notify:OnProgressionTaskCompleted",
    ]);

    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, 913, "demo", "demo-secret", P1).await;
    let kw = HashMap::new();

    // Negative check 1 (`F-02`): NPE `operationsManager` emits `OnOperationTaskCompleted` ONLY —
    // never dungeon `ProgressionService` `OnProgressionTaskCompleted` / `OnProgressionAdvanced`!
    let op_defs = eve_server::services::operations_manager::get_operation_defs(&ctx.sde);
    let (&real_op, &client_task) = op_defs
        .op_tasks
        .iter()
        .filter(|(oid, _)| {
            op_defs.op_prerequisites.get(oid).is_none_or(|p| p.is_empty())
                && op_defs.op_tree_parents.get(oid).is_none_or(|p| p.is_empty())
        })
        .find_map(|(op, ts)| {
            ts.first()
                .filter(|t| {
                    op_defs.client_completable_tasks.contains(t)
                        && op_defs
                            .task_event_conditions
                            .get(t)
                            .is_none_or(|c| c.is_empty())
                })
                .map(|t| (op, t))
        })
        .expect("SDE operation with client-completable tasks");
    let _ = ctx
        .dispatch_rpc(
            &mut s1,
            "operationsManager",
            None,
            "activate_operation_in_solar_system",
            &[PyRep::Integer(real_op)],
            &kw,
            None,
        )
        .await
        .expect("activate_operation");
    let res_npe = ctx
        .dispatch_rpc(
            &mut s1,
            "operationsManager",
            None,
            "process_client_event",
            &[PyRep::String("complete_task".into()), PyRep::Integer(client_task)],
            &kw,
            None,
        )
        .await
        .expect("complete_task");
    assert_eq!(res_npe, PyRep::Bool(true));
    assert!(!events(&ctx, P1, "OnOperationTaskCompleted").is_empty());
    assert!(events(&ctx, P1, "OnProgressionTaskCompleted").is_empty());
    assert!(events(&ctx, P1, "OnProgressionAdvanced").is_empty());

    // Set up P1 undocked in space with a live turret ship to enter and clear a multi-room dungeon
    let ship_p1 = 969_001_i64;
    let gun_p1 = 969_010_i64;
    let ammo_p1 = 969_011_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&P1) {
            c1.solar_system_id = NULLSEC_SYSTEM;
            c1.station_id = None;
            c1.active_ship_id = ship_p1;
        }
        snap.items.insert(
            ship_p1,
            InventoryItemRecord {
                item_id: ship_p1,
                type_id: 639,
                owner_id: P1,
                location_id: i64::from(NULLSEC_SYSTEM),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            gun_p1,
            InventoryItemRecord {
                item_id: gun_p1,
                type_id: 2961,
                owner_id: P1,
                location_id: ship_p1,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            ammo_p1,
            InventoryItemRecord {
                item_id: ammo_p1,
                type_id: 21894,
                owner_id: P1,
                location_id: ship_p1,
                flag: InventoryFlag::HiSlot0,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(gun_p1, true);
        grant_all_combat_skills(&ctx, snap, P1, &[639, 2961, 21894]);
    });
    ctx.invalidate_ship_fitting(ship_p1);
    s1.solar_system_id = Some(NULLSEC_SYSTEM);
    s1.station_id = None;
    s1.ship_id = Some(ship_p1);

    let base_pos = [120_000_000.0, 0.0, 80_000_000.0];
    let inst_id = eve_server::services::pve_dungeon_runtime::spawn_sde_dungeon_runtime_instance(
        &ctx,
        P1,
        NULLSEC_SYSTEM,
        2001,
        "Deadspace",
        base_pos,
        true,
    )
    .await
    .expect("spawn_sde_dungeon_runtime_instance");

    let dng_state = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id)
        .expect("get_dungeon_instance_state");
    let entry_gate_id = dng_state.dict_get("entryGateBallID").and_then(|v| v.as_i64()).unwrap();

    let sys_h = ctx.systems.get_or_spawn(NULLSEC_SYSTEM);
    let entry_gate_pos = sys_h.get_ball_snapshot(entry_gate_id).await.unwrap().unwrap().1.position;
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_p1,
                type_id: 639,
                position: entry_gate_pos + DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 100_000_000.0,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(639),
        )
        .await;
    let _ = ctx.drain_notifications_for_char(P1);

    // Enter Room 1 via live `keeper.ActivateAccelerationGate` -> emits `OnProgressionRoomJoined`, zero task completions yet
    let enter_r1 = ctx
        .dispatch_rpc(
            &mut s1,
            "keeper",
            None,
            "ActivateAccelerationGate",
            &[PyRep::Integer(entry_gate_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate entry acceleration gate");
    let room1_id = enter_r1.dict_get("roomID").and_then(|v| v.as_i64()).unwrap();
    let r1_gate_id = enter_r1.dict_get("nextGateBallID").and_then(|v| v.as_i64()).unwrap();
    let relay_obj_id = match enter_r1.dict_get("objectBallIDs") {
        Some(PyRep::List(v)) => v[0].as_i64().unwrap(),
        other => panic!("expected objectBallIDs list, got {other:?}"),
    };
    let guard_npc_ids: Vec<i64> = match enter_r1.dict_get("npcBallIDs") {
        Some(PyRep::List(v)) => v.iter().map(|x| x.as_i64().unwrap()).collect(),
        other => panic!("expected npcBallIDs list, got {other:?}"),
    };
    assert_eq!(guard_npc_ids.len(), 2);
    for _ in 0..45 {
        if !matches!(
            sys_h.get_ball_snapshot(ship_p1).await.unwrap().unwrap().1.mode,
            MovementMode::Warp { .. }
        ) {
            break;
        }
        let _ = sys_h.tick().await;
    }

    let joined_ev = events(&ctx, P1, "OnProgressionRoomJoined");
    assert_eq!(joined_ev.len(), 1, "Entering dungeon pocket must emit OnProgressionRoomJoined");
    assert_eq!(joined_ev[0].len(), 5);
    assert_eq!(joined_ev[0][4], PyRep::Integer(room1_id));
    assert!(events(&ctx, P1, "OnProgressionTaskCompleted").is_empty());
    assert!(events(&ctx, P1, "OnProgressionAdvanced").is_empty());

    // Negative check 2: activating locked Room 1 gate before completing Room 1 kill objective fails with GateLocked and emits nothing
    let r1_gate_pos = sys_h.get_ball_snapshot(r1_gate_id).await.unwrap().unwrap().1.position;
    let _ = sys_h.set_ball_position(ship_p1, r1_gate_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    let locked_err = ctx
        .dispatch_rpc(
            &mut s1,
            "keeper",
            None,
            "ActivateAccelerationGate",
            &[PyRep::Integer(r1_gate_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(locked_err.code, "GateLocked");
    assert!(events(&ctx, P1, "OnProgressionTaskCompleted").is_empty());
    assert!(events(&ctx, P1, "OnProgressionAdvanced").is_empty());

    // Positive check 1 (`OnApproach` trigger 1 -> `SpawnNpcGroup(2)`): live `beyonce.CmdGotoPoint` completes task 1 and advances objective
    let relay_pos = sys_h.get_ball_snapshot(relay_obj_id).await.unwrap().unwrap().1.position;
    let _ = sys_h.set_ball_position(ship_p1, relay_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    let b1 = ctx.bind_object(&mut s1, "beyonce", i64::from(NULLSEC_SYSTEM)).unwrap();
    ctx.dispatch_rpc(
        &mut s1,
        "beyonce",
        Some(b1),
        "CmdGotoPoint",
        &[PyRep::Real(relay_pos.x), PyRep::Real(relay_pos.y), PyRep::Real(relay_pos.z)],
        &kw,
        None,
    )
    .await
    .expect("CmdGotoPoint relay object");

    let expected_task1 = room1_id * 100 + 1;
    let expected_obj_wave2 = room1_id * 10 + 2;
    let task_ev1 = events(&ctx, P1, "OnProgressionTaskCompleted");
    assert_eq!(task_ev1, vec![vec![PyRep::Integer(expected_task1)]]);
    let adv_ev1 = events(&ctx, P1, "OnProgressionAdvanced");
    assert_eq!(
        adv_ev1,
        vec![vec![PyRep::Integer(expected_obj_wave2), PyRep::Integer(expected_task1)]]
    );

    // Positive check 2 (`OnKill` trigger 2 -> `UnlockGate`): destroy both Room 1 Group 1 gate guards via live weapon combat
    ctx.dispatch_rpc(&mut s1, "beyonce", Some(b1), "CmdStop", &[], &kw, None)
        .await
        .expect("CmdStop before shooting guard NPCs");
    for (idx, &guard_npc_id) in guard_npc_ids.iter().enumerate() {
        let guard_pos = sys_h.get_ball_snapshot(guard_npc_id).await.unwrap().unwrap().1.position;
        let _ = sys_h.set_ball_position(ship_p1, guard_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;
        ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "AddTarget", &[PyRep::Integer(guard_npc_id)], &kw, None)
            .await
            .expect("AddTarget guard NPC");
        for _ in 0..30 {
            if sys_h.get_target_locks(ship_p1).await.unwrap().iter().any(|l| l.target_id == guard_npc_id && l.locked) {
                break;
            }
            let _ = sys_h.tick().await;
        }
        if let Ok(Some(mut cs)) = sys_h.get_combat_state(guard_npc_id).await {
            cs.shield_hp = 5.0;
            cs.armor_hp = 5.0;
            cs.structure_hp = 5.0;
            cs.base_signature_radius = Some(400.0);
            cs.signature_radius = 400.0;
            let _ = sys_h.set_combat_state(guard_npc_id, cs).await;
        }
        let guard_pos_now = sys_h.get_ball_snapshot(guard_npc_id).await.unwrap().unwrap().1.position;
        let _ = sys_h.set_ball_position(ship_p1, guard_pos_now + DVec3::new(1_500.0, 0.0, 0.0)).await;
        ctx.set_invention_roll_override(Some(0.001));
        ctx.dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(gun_p1), PyRep::Integer(guard_npc_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate gun_p1 on guard NPC");
        for _ in 0..25 {
            if sys_h.get_ball_snapshot(guard_npc_id).await.unwrap().is_none() {
                break;
            }
            let _ = sys_h.step_manual(1.0).await;
        }
        ctx.set_invention_roll_override(None);
        assert!(sys_h.get_ball_snapshot(guard_npc_id).await.unwrap().is_none());
        if idx == 0 {
            assert!(events(&ctx, P1, "OnProgressionTaskCompleted").is_empty());
        }
    }

    let expected_task2 = room1_id * 100 + 2;
    let expected_obj_gate = room1_id * 10 + 9;
    let task_ev2 = events(&ctx, P1, "OnProgressionTaskCompleted");
    assert_eq!(task_ev2, vec![vec![PyRep::Integer(expected_task2)]]);
    let adv_ev2 = events(&ctx, P1, "OnProgressionAdvanced");
    assert_eq!(
        adv_ev2,
        vec![vec![PyRep::Integer(expected_obj_gate), PyRep::Integer(expected_task2)]]
    );

    let post_state = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id).unwrap();
    assert_eq!(post_state.dict_get("activeObjectiveID"), Some(&PyRep::Integer(expected_obj_gate)));
    assert_eq!(post_state.dict_get("lastCompletedTaskID"), Some(&PyRep::Integer(expected_task2)));

    // Room 1 acceleration gate is now unlocked -> traversing into Room 2 emits Room 2 OnProgressionRoomJoined & OnProgressionAdvanced
    let _ = sys_h.set_ball_position(ship_p1, r1_gate_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    let enter_r2 = ctx
        .dispatch_rpc(&mut s1, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(r1_gate_id)], &kw, None)
        .await
        .expect("Activate unlocked Room 1 acceleration gate into Room 2");
    let room2_id = enter_r2.dict_get("roomID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(events(&ctx, P1, "OnProgressionRoomJoined").len(), 1);
    assert_eq!(
        events(&ctx, P1, "OnProgressionAdvanced"),
        vec![vec![PyRep::Integer(room2_id * 10 + 1), PyRep::Integer(expected_task2)]]
    );
}

#[tokio::test]
async fn test_b36_combat_message_live_weapon_cycle() {
    // parity: notify:OnCombatMessage
    eve_testkit::attest(&["notify:OnCombatMessage"]);

    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, 914, "demo", "demo-secret", P1).await;
    let _s2 = pilot(&ctx, 915, "other", "other-secret", P2).await;
    let kw = HashMap::new();

    let ship_p1 = 970_001_i64;
    let ship_p2 = 970_002_i64;
    let gun_p1 = 970_010_i64;
    let ammo_p1 = 970_011_i64;
    let lml_p1 = 970_012_i64;
    let missile_p1 = 970_013_i64;

    let light_missile_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 384 && t.published)
        .map(|t| t.type_id)
        .unwrap_or(213);

    let fw_sys = ctx
        .db
        .with_snapshot(|s| s.fw_system_occupations.keys().copied().min().unwrap_or(NULLSEC_SYSTEM));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&P1) {
            c1.solar_system_id = fw_sys;
            c1.station_id = None;
            c1.active_ship_id = ship_p1;
            c1.war_faction_id = Some(500_001);
        }
        if let Some(c2) = snap.characters.get_mut(&P2) {
            c2.solar_system_id = fw_sys;
            c2.station_id = None;
            c2.active_ship_id = ship_p2;
            c2.war_faction_id = Some(500_004);
        }
        for (sid, owner) in [(ship_p1, P1), (ship_p2, P2)] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 639,
                    owner_id: owner,
                    location_id: i64::from(fw_sys),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            gun_p1,
            InventoryItemRecord {
                item_id: gun_p1,
                type_id: 2961,
                owner_id: P1,
                location_id: ship_p1,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            ammo_p1,
            InventoryItemRecord {
                item_id: ammo_p1,
                type_id: 21894,
                owner_id: P1,
                location_id: ship_p1,
                flag: InventoryFlag::HiSlot0,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            lml_p1,
            InventoryItemRecord {
                item_id: lml_p1,
                type_id: 2404,
                owner_id: P1,
                location_id: ship_p1,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            missile_p1,
            InventoryItemRecord {
                item_id: missile_p1,
                type_id: light_missile_tid,
                owner_id: P1,
                location_id: ship_p1,
                flag: InventoryFlag::HiSlot1,
                quantity: 200,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(gun_p1, true);
        snap.module_online_states.insert(lml_p1, true);
        grant_all_combat_skills(&ctx, snap, P1, &[639, 2961, 21894, 2404, light_missile_tid]);
    });
    ctx.invalidate_ship_fitting(ship_p1);
    ctx.invalidate_ship_fitting(ship_p2);

    s1.solar_system_id = Some(fw_sys);
    s1.station_id = None;
    s1.ship_id = Some(ship_p1);

    let sys_h = ctx.systems.get_or_spawn(fw_sys);
    for (sid, pos) in [(ship_p1, DVec3::ZERO), (ship_p2, DVec3::new(3_000.0, 0.0, 0.0))] {
        let mut cs = ctx.build_type_combat_state(639);
        cs.signature_radius = 450.0;
        let _ = sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: sid,
                    type_id: 639,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 100_000_000.0,
                    radius: 250.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
            )
            .await;
    }

    // Negative check: activating weapon before target lock fails and emits no OnCombatMessage
    assert!(ctx
        .dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(gun_p1), PyRep::Integer(ship_p2)],
            &kw,
            None,
        )
        .await
        .is_err());
    assert!(events(&ctx, P1, "OnCombatMessage").is_empty());
    assert!(events(&ctx, P2, "OnCombatMessage").is_empty());

    // Lock target via dogmaIM.AddTarget + SystemActor ticks
    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "AddTarget", &[PyRep::Integer(ship_p2)], &kw, None)
        .await
        .expect("AddTarget ship_p2");
    for _ in 0..30 {
        if sys_h.get_target_locks(ship_p1).await.unwrap().iter().any(|l| l.target_id == ship_p2 && l.locked) {
            break;
        }
        let _ = sys_h.tick().await;
    }

    // 1. Live turret cycle -> TacticalSvc.OnCombatMessage(messageName, messageArgs) on P1 and P2
    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(gun_p1), PyRep::Integer(ship_p2)], &kw, None)
        .await
        .expect("Activate weapon on locked ship_p2");
    let _ = sys_h.tick().await;

    let atk_ev = events(&ctx, P1, "OnCombatMessage");
    assert!(!atk_ev.is_empty(), "Attacker P1 must receive OnCombatMessage from live weapon cycle");
    assert_eq!(atk_ev[0].len(), 2);
    assert!(matches!(atk_ev[0][0].as_str(), Some("AttackHit" | "AttackMiss")));
    assert_eq!(atk_ev[0][1].dict_get("specialObject"), Some(&PyRep::Integer(ship_p2)));
    assert_eq!(atk_ev[0][1].dict_get("type"), Some(&PyRep::Integer(2961)));
    assert_eq!(atk_ev[0][1].dict_get("weapon"), Some(&PyRep::Integer(2961)));
    assert!(atk_ev[0][1].dict_get("damage").is_some());
    assert!(atk_ev[0][1].dict_get("hitQualityText").and_then(|v| v.as_str()).is_some());

    let vic_ev = events(&ctx, P2, "OnCombatMessage");
    assert!(!vic_ev.is_empty(), "Victim P2 must receive OnCombatMessage from live weapon cycle");
    assert_eq!(vic_ev[0].len(), 2);
    assert!(matches!(vic_ev[0][0].as_str(), Some("AttackedBy" | "AttackMissBy")));
    assert_eq!(vic_ev[0][1].dict_get("specialObject"), Some(&PyRep::Integer(ship_p1)));
    assert_eq!(vic_ev[0][1].dict_get("type"), Some(&PyRep::Integer(2961)));

    // 2. `F-01` regression: live missile launcher impact (`combat_missiles_aoe.rs` passes `victim_char_opt = None`!)
    // Verify `resolved_victim` delivers `OnCombatMessage`, `OnDamageMessage`, and `OnDamageMessages` to victim P2!
    let _ = ctx.drain_notifications_for_char(P1);
    let _ = ctx.drain_notifications_for_char(P2);

    let m_res = ctx
        .dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(lml_p1), PyRep::Integer(ship_p2)], &kw, None)
        .await
        .expect("Activate Light Missile Launcher II on locked ship_p2");
    assert_eq!(m_res.dict_get("impacted"), Some(&PyRep::Bool(true)), "In-flight missile must impact ship_p2");

    let vic_missile_combat = events(&ctx, P2, "OnCombatMessage");
    assert_eq!(vic_missile_combat.len(), 1, "Victim P2 must receive OnCombatMessage when caller passes victim_char_opt = None");
    assert_eq!(vic_missile_combat[0][0].as_str(), Some("AttackedBy"));
    assert_eq!(vic_missile_combat[0][1].dict_get("specialObject"), Some(&PyRep::Integer(ship_p1)));
    assert_eq!(vic_missile_combat[0][1].dict_get("type"), Some(&PyRep::Integer(i64::from(light_missile_tid))));
    assert!(!events(&ctx, P2, "OnDamageMessage").is_empty(), "Victim P2 must receive OnDamageMessage when victim_char_opt = None");
    assert!(!events(&ctx, P2, "OnDamageMessages").is_empty(), "Victim P2 must receive OnDamageMessages when victim_char_opt = None");
}

}
