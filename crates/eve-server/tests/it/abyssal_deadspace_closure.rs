//! End-to-end integration tests for Abyssal high-sec tier/suspect rules,
//! activation restrictions, hull capacity limits, co-op entry, and trace lifecycle.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

pub(crate) const SYS_10_MANIFEST: u32 = 30_100_032; // 1.0 high-sec
pub(crate) const SYS_09_JITA: u32 = 30_000_142; // 0.9 high-sec
pub(crate) const SYS_08_LASHESIH: u32 = 30_000_002; // 0.8 high-sec
pub(crate) const SYS_07_ORDIZE: u32 = 30_000_026; // 0.7 high-sec
pub(crate) const SYS_06_SOOMA: u32 = 30_000_009; // 0.6 high-sec
pub(crate) const SYS_05_CHIDAH: u32 = 30_000_010; // 0.5 high-sec
pub(crate) const SYS_LOW_TAMA: u32 = 30_002_813; // Low-sec

pub(crate) const FIL_T0_ELEC: u32 = 56_131; // Tranquil Electrical (T0)
pub(crate) const FIL_T1_ELEC: u32 = 47_765; // Calm Electrical (T1)
pub(crate) const FIL_T3_ELEC: u32 = 47_905; // Fierce Electrical (T3)
pub(crate) const FIL_T4_ELEC: u32 = 47_906; // Raging Electrical (T4)
pub(crate) const FIL_T5_ELEC: u32 = 47_907; // Chaotic Electrical (T5)
pub(crate) const FIL_T6_ELEC: u32 = 56_139; // Cataclysmic Electrical (T6)

pub(crate) async fn login_pilot_in_space(
    ctx: &ServerContext,
    sess_id: u64,
    user: &str,
    pass: &str,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("SelectCharacterID");
    move_pilot_to_system(ctx, &mut sess, char_id, ship_id, ship_type_id, sys_id, pos).await;
    sess
}

pub(crate) async fn move_pilot_to_system(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess.structure_id = None;
    let handle = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ship_id,
        type_id: ship_type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = handle.spawn_ball_with_combat(ball, ctx.build_ship_combat_state(char_id, ship_id, ship_type_id)).await;
}

pub(crate) fn seed_cargo_item(ctx: &ServerContext, item_id: i64, type_id: u32, char_id: i64, ship_id: i64, qty: i32) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: qty,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
}

// parity: feat:pve.abyssal-highsec-tier-suspect, feat:pve.abyssal-trace-suspect-flag
#[tokio::test]
async fn test_abyssal_highsec_tiers_suspect_and_activation_restrictions() {
    eve_testkit::attest(&[
        "feat:pve.abyssal-highsec-tier-suspect",
        "feat:pve.abyssal-trace-suspect-flag",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let ship_id = 610_000_001_i64;
    let mut sess = login_pilot_in_space(&ctx, 887_001, "demo", "demo-secret", char_id, ship_id, 621, SYS_09_JITA, DVec3::new(0.0, 2.0e9, 0.0)).await;

    // 1. High-sec security band tier & Suspect rules
    let cases = [
        (SYS_10_MANIFEST, FIL_T1_ELEC, Some("FilamentDisallowedInHighSec"), false),
        (SYS_10_MANIFEST, FIL_T0_ELEC, None, false),
        (SYS_09_JITA, FIL_T1_ELEC, Some("FilamentDisallowedInHighSec"), false),
        (SYS_09_JITA, FIL_T4_ELEC, Some("FilamentDisallowedInHighSec"), false),
        (SYS_09_JITA, FIL_T0_ELEC, None, false),
        (SYS_08_LASHESIH, FIL_T3_ELEC, None, false),
        (SYS_08_LASHESIH, FIL_T4_ELEC, None, true),
        (SYS_07_ORDIZE, FIL_T4_ELEC, None, false),
        (SYS_07_ORDIZE, FIL_T5_ELEC, None, true),
        (SYS_06_SOOMA, FIL_T5_ELEC, None, false),
        (SYS_06_SOOMA, FIL_T6_ELEC, None, true),
        (SYS_05_CHIDAH, FIL_T6_ELEC, None, false),
        (SYS_LOW_TAMA, FIL_T6_ELEC, None, false),
    ];

    for (idx, &(sys_id, fil_tid, expected_err, expected_suspect)) in cases.iter().enumerate() {
        let pos = DVec3::new((idx as f64 + 1.0) * 5_000_000.0, 2.0e9, 0.0);
        move_pilot_to_system(&ctx, &mut sess, char_id, ship_id, 621, sys_id, pos).await;
        ctx.crimewatch.write().unwrap().states.remove(&char_id);
        let fil_iid = 710_100_000 + idx as i64;
        seed_cargo_item(&ctx, fil_iid, fil_tid, char_id, ship_id, 1);

        let res = ctx
            .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fil_iid)], &kw, None)
            .await;
        match expected_err {
            Some(code) => {
                let err = res.expect_err("expected high-sec rejection");
                assert_eq!(err.code, code, "sys {sys_id} fil {fil_tid}");
                assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&fil_iid)), "rejected filament preserved");
            }
            None => {
                let rep = res.expect("allowed deployment");
                let _trace_id = rep.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
                let is_sus = ctx.crimewatch.read().unwrap().is_suspect(char_id, ctx.now_unix());
                assert_eq!(is_sus, expected_suspect, "suspect mismatch for sys {sys_id} fil {fil_tid}");
            }
        }
    }

    // Verify Suspect flag is refreshed on exit from high-sec Suspect pocket
    move_pilot_to_system(&ctx, &mut sess, char_id, ship_id, 621, SYS_08_LASHESIH, DVec3::new(90_000_000.0, 2.0e9, 0.0)).await;
    ctx.crimewatch.write().unwrap().states.remove(&char_id);
    seed_cargo_item(&ctx, 710_100_050, FIL_T4_ELEC, char_id, ship_id, 1);
    let dep_sus = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(710_100_050)], &kw, None).await.expect("dep T4");
    let tr_sus = dep_sus.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
    let ent_sus = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr_sus)], &kw, None).await.expect("enter T4");
    assert_eq!(ent_sus.dict_get("suspectFlagOnExit"), Some(&PyRep::Bool(true)));
    let pid_sus = ent_sus.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();
    ctx.crimewatch.write().unwrap().states.remove(&char_id);
    for r in 0..3 {
        crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, char_id, r).await;
        let gid = 1_895_000_000 + pid_sus * 10 + (r as i64 + 1);
        if r < 2 {
            ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalGateActivation", &[PyRep::Integer(gid)], &kw, None).await.expect("gate");
        } else {
            ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEndGateActivation", &[PyRep::Integer(gid)], &kw, None).await.expect("exit");
        }
    }
    assert!(ctx.crimewatch.read().unwrap().is_suspect(char_id, ctx.now_unix()), "Suspect flag re-applied on high-sec exit");

    // 2. Activation restrictions: cloak, tether, warp-scram, warp-exit invuln, safe-logoff & PvP timer, 1000km proximity (depot, gate, station, trace), deadspace
    let test_pos = DVec3::new(120_000_000.0, 2.0e9, 0.0);
    move_pilot_to_system(&ctx, &mut sess, char_id, ship_id, 621, SYS_08_LASHESIH, test_pos).await;
    seed_cargo_item(&ctx, 710_100_099, FIL_T1_ELEC, char_id, ship_id, 8);
    let sys_h = ctx.systems.get_or_spawn(SYS_08_LASHESIH);

    { let mut bp = sys_h.ballpark.write().unwrap(); bp.combat_states.get_mut(&ship_id).unwrap().is_cloaked = true; }
    let e_cloak = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(710_100_099)], &kw, None).await.unwrap_err();
    assert_eq!(e_cloak.code, "ShipIsCloaked");
    { let mut bp = sys_h.ballpark.write().unwrap(); bp.combat_states.get_mut(&ship_id).unwrap().is_cloaked = false; }

    { let mut bp = sys_h.ballpark.write().unwrap(); bp.combat_states.get_mut(&ship_id).unwrap().is_tethered = true; }
    let e_teth = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(710_100_099)], &kw, None).await.unwrap_err();
    assert_eq!(e_teth.code, "ShipIsTethered");
    { let mut bp = sys_h.ballpark.write().unwrap(); bp.combat_states.get_mut(&ship_id).unwrap().is_tethered = false; }

    { let mut bp = sys_h.ballpark.write().unwrap(); bp.combat_states.get_mut(&ship_id).unwrap().warp_scramble_strength = 2; }
    let e_scram = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(710_100_099)], &kw, None).await.unwrap_err();
    assert_eq!(e_scram.code, "WarpScrambled");
    { let mut bp = sys_h.ballpark.write().unwrap(); bp.combat_states.get_mut(&ship_id).unwrap().warp_scramble_strength = 0; }

    { let mut bp = sys_h.ballpark.write().unwrap(); bp.combat_states.get_mut(&ship_id).unwrap().warp_exit_invulnerable_remaining_ticks = 8; }
    let e_inv = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(710_100_099)], &kw, None).await.unwrap_err();
    assert_eq!(e_inv.code, "ShipInvulnerable");
    { let mut bp = sys_h.ballpark.write().unwrap(); bp.combat_states.get_mut(&ship_id).unwrap().warp_exit_invulnerable_remaining_ticks = 0; }

    ctx.crimewatch.write().unwrap().states.remove(&char_id);
    ctx.advance_secs(301);
    let safe_res = ctx.dispatch_rpc(&mut sess, "ship", None, "SafeLogoff", &[], &kw, None).await.expect("SafeLogoff");
    assert_eq!(safe_res, PyRep::List(vec![]), "SafeLogoff countdown started");
    let e_logoff = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(710_100_099)], &kw, None).await.unwrap_err();
    assert_eq!(e_logoff.code, "LogoffTimerActive");
    ctx.dispatch_rpc(&mut sess, "ship", None, "AbortSafeLogoff", &[], &kw, None).await.expect("AbortSafeLogoff");

    let depot_ball = Ball {
        id: 710_200_001, type_id: 33_474, position: test_pos + DVec3::new(500_000.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let _ = sys_h.spawn_ball(depot_ball).await;
    let e_prox = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(710_100_099)], &kw, None).await.unwrap_err();
    assert_eq!(e_prox.code, "ProximityRestriction");
    sys_h.try_remove_ball(710_200_001);

    // Within 1,000 km of an SDE stargate -> ProximityRestriction
    let gate_anchor = ctx.sde.stargates().values().find(|g| g.from_solar_system_id == SYS_08_LASHESIH).expect("Lashesih stargate").position;
    let _ = sys_h.set_ball_position(ship_id, gate_anchor + DVec3::new(500_000.0, 0.0, 0.0)).await;
    let e_gate_prox = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(710_100_099)], &kw, None).await.unwrap_err();
    assert_eq!(e_gate_prox.code, "ProximityRestriction", "Refused within 1,000 km of stargate");

    // Within 1,000 km of an SDE station -> ProximityRestriction (Jita has NPC stations in V24 SDE)
    let station_anchor = ctx.sde.solar_systems.get(&SYS_09_JITA).and_then(|s| s.stations.first()).expect("Jita station").position;
    move_pilot_to_system(&ctx, &mut sess, char_id, ship_id, 621, SYS_09_JITA, station_anchor + DVec3::new(500_000.0, 0.0, 0.0)).await;
    let e_station_prox = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(710_100_099)], &kw, None).await.unwrap_err();
    assert_eq!(e_station_prox.code, "ProximityRestriction", "Refused within 1,000 km of station");

    // Return to 0.8 Lashesih for trace proximity and PvP timer checks (T1 Calm filament allowed in 0.8)
    move_pilot_to_system(&ctx, &mut sess, char_id, ship_id, 621, SYS_08_LASHESIH, test_pos).await;
    let sys_h = ctx.systems.get_or_spawn(SYS_08_LASHESIH);
    let dep_trace = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(710_100_099)], &kw, None).await.expect("deploy trace");
    assert!(dep_trace.dict_get("traceBallID").is_some());
    let _ = sys_h.set_ball_position(ship_id, test_pos + DVec3::new(500_000.0, 0.0, 0.0)).await;
    let e_trace_prox = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(710_100_099)], &kw, None).await.unwrap_err();
    assert_eq!(e_trace_prox.code, "ProximityRestriction", "Refused within 1,000 km of another Abyssal Trace");

    // Active 15-minute Capsuleer Log-Off (PvP) timer -> PvpTimerActive
    let far_pos = test_pos + DVec3::new(2_500_000.0, 0.0, 0.0);
    let _ = sys_h.set_ball_position(ship_id, far_pos).await;
    eve_server::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(&ctx, char_id, 90_000_002, ctx.now_unix());
    let e_pvp = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(710_100_099)], &kw, None).await.unwrap_err();
    assert_eq!(e_pvp.code, "PvpTimerActive", "Refused with active 15m capsuleer log-off timer");
    ctx.crimewatch.write().unwrap().states.remove(&char_id);
    ctx.advance_secs(901);

    let accel_gate_tid = ctx.sde.types.values().filter(|t| t.group_id == 366).map(|t| t.type_id).min().expect("SDE group 366 Acceleration Gate");
    let ded_gate = Ball {
        id: 710_200_002, type_id: accel_gate_tid, position: far_pos + DVec3::new(10_000.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 10_000.0, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let _ = sys_h.spawn_ball(ded_gate).await;
    let e_ded = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(710_100_099)], &kw, None).await.unwrap_err();
    assert_eq!(e_ded.code, "ShipInDungeon");
    sys_h.try_remove_ball(710_200_002);
}

// parity: feat:pve.abyssal-entry-ship-limits, feat:pve.abyssal-coop-frigate-destroyer, feat:pve.abyssal-trace
#[tokio::test]
async fn test_abyssal_entry_ship_limits_coop_and_trace_lifecycle() {
    eve_testkit::attest(&[
        "feat:pve.abyssal-entry-ship-limits",
        "feat:pve.abyssal-coop-frigate-destroyer",
        "feat:pve.abyssal-trace",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = SYS_08_LASHESIH;

    let char1 = 90_000_001_i64;
    let char2 = 90_000_002_i64;
    let uid3 = ctx.db.provision_player_account("abyss_p3", "pw123");
    let char3 = ctx.db.create_character(uid3, "Abyss Pilot 3", 1, 1, 11, 17, &ctx.sde).unwrap().character_id;
    let uid4 = ctx.db.provision_player_account("abyss_p4", "pw123");
    let char4 = ctx.db.create_character(uid4, "Abyss Pilot 4", 1, 1, 11, 17, &ctx.sde).unwrap().character_id;

    let (ship1, ship2, ship3, ship4) = (620_000_001_i64, 620_000_002_i64, 620_000_003_i64, 620_000_004_i64);
    let base1 = DVec3::new(0.0, 2.0e9, 0.0);
    let mut sess1 = login_pilot_in_space(&ctx, 887_101, "demo", "demo-secret", char1, ship1, 641, sys_id, base1).await;
    let mut sess2 = login_pilot_in_space(&ctx, 887_102, "other", "other-secret", char2, ship2, 16_236, sys_id, base1 + DVec3::new(500.0, 0.0, 0.0)).await;
    let mut sess3 = login_pilot_in_space(&ctx, 887_103, "abyss_p3", "pw123", char3, ship3, 16_236, sys_id, base1 + DVec3::new(600.0, 0.0, 0.0)).await;
    let mut sess4 = login_pilot_in_space(&ctx, 887_104, "abyss_p4", "pw123", char4, ship4, 587, sys_id, base1 + DVec3::new(700.0, 0.0, 0.0)).await;

    // 1. Disallowed ship hulls rejected with DisallowedShipType; Expedition Frigate (group 1283) allowed (3-ship co-op)
    for (idx, bad_hull) in [641_u32, 16_227, 29_990, 19_720, 648, 670].into_iter().enumerate() {
        move_pilot_to_system(&ctx, &mut sess1, char1, ship1, bad_hull, sys_id, base1).await;
        let fiid = 720_100_000 + idx as i64;
        seed_cargo_item(&ctx, fiid, FIL_T1_ELEC, char1, ship1, 3);
        let err = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fiid)], &kw, None).await.unwrap_err();
        assert_eq!(err.code, "DisallowedShipType", "hull {bad_hull} must be disallowed");
    }
    let exp_frig_tid = ctx.sde.types.values().filter(|t| t.group_id == 1283).map(|t| t.type_id).min().expect("Expedition Frigate group 1283");
    move_pilot_to_system(&ctx, &mut sess1, char1, ship1, exp_frig_tid, sys_id, base1 + DVec3::new(2_000_000.0, 0.0, 0.0)).await;
    seed_cargo_item(&ctx, 720_100_010, FIL_T1_ELEC, char1, ship1, 3);
    let dep_exp = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(720_100_010)], &kw, None).await.expect("Expedition Frigate deploy");
    assert_eq!(dep_exp.dict_get("maxShips"), Some(&PyRep::Integer(3)), "Expedition Frigate (group 1283) allows 3-ship co-op");

    // 2. Destroyer co-op: requires 2 filaments (1 rejected), allows 2 fleetmates, rejects 3rd (TraceShipLimitReached) & non-fleetmate (AccessDenied)
    move_pilot_to_system(&ctx, &mut sess1, char1, ship1, 16_236, sys_id, base1).await;
    seed_cargo_item(&ctx, 720_100_020, FIL_T1_ELEC, char1, ship1, 1);
    let ins_dd = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(720_100_020)], &kw, None).await.unwrap_err();
    assert_eq!(ins_dd.code, "InsufficientFilamentQuantity");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&720_100_020).unwrap().quantity = 2;
        snap.fleets.insert(99_201, FleetRecord { fleet_id: 99_201, boss_id: char1, members: vec![char1, char2, char3], ..Default::default() });
    });
    let dep_dd = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(720_100_020)], &kw, None).await.expect("Destroyer deploy");
    assert_eq!(dep_dd.dict_get("maxShips"), Some(&PyRep::Integer(2)));
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&720_100_020).map(|i| i.quantity).unwrap_or(0)), 0, "2 filaments consumed");
    let dd_trace = dep_dd.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();

    // Non-fleetmate in matching Destroyer hull gets AccessDenied; fleetmate in Frigate gets DisallowedShipType
    move_pilot_to_system(&ctx, &mut sess4, char4, ship4, 16_236, sys_id, base1 + DVec3::new(700.0, 0.0, 0.0)).await;
    let e_nonfleet = ctx.dispatch_rpc(&mut sess4, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(dd_trace)], &kw, None).await.unwrap_err();
    assert_eq!(e_nonfleet.code, "AccessDenied");
    move_pilot_to_system(&ctx, &mut sess3, char3, ship3, 587, sys_id, base1 + DVec3::new(500.0, 0.0, 0.0)).await;
    let e_dd_wrong_hull = ctx.dispatch_rpc(&mut sess3, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(dd_trace)], &kw, None).await.unwrap_err();
    assert_eq!(e_dd_wrong_hull.code, "DisallowedShipType");

    move_pilot_to_system(&ctx, &mut sess2, char2, ship2, 16_236, sys_id, base1 + DVec3::new(300.0, 0.0, 0.0)).await;
    move_pilot_to_system(&ctx, &mut sess3, char3, ship3, 16_236, sys_id, base1 + DVec3::new(500.0, 0.0, 0.0)).await;
    let ent_dd1 = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(dd_trace)], &kw, None).await.expect("DD 1 enter");
    let ent_dd2 = ctx.dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(dd_trace)], &kw, None).await.expect("DD 2 enter");
    let dd_pid = ent_dd1.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(Some(dd_pid), ent_dd2.dict_get("pocketID").and_then(|v| v.as_i64()));
    let dd_cache_id = 1_896_000_000 + dd_pid * 10 + 1;
    ctx.db.with_snapshot(|s| {
        let mut by_tid: HashMap<u32, i32> = HashMap::new();
        for it in s.items.values().filter(|i| i.location_id == dd_cache_id) {
            *by_tid.entry(it.type_id).or_default() += it.quantity;
        }
        assert_eq!(by_tid.get(&FIL_T1_ELEC), Some(&2), "2x Destroyer scales filament loot by 2x");
        assert_eq!(by_tid.get(&48_121), Some(&4), "2x Destroyer scales T1 survey database loot by 2x (4)");
        assert_eq!(by_tid.get(&48_112), Some(&20), "2x Destroyer scales T1 zero-point condensate loot by 2x (20)");
    });
    let e_dd3 = ctx.dispatch_rpc(&mut sess3, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(dd_trace)], &kw, None).await.unwrap_err();
    assert_eq!(e_dd3.code, "TraceShipLimitReached");

    // 3. Frigate co-op in 0.8 high-sec with T4 filament: consumes 3 filaments, 3x cache loot, allows 3 fleetmates, propagates Suspect flag, returns to origin & despawns trace on last exit
    let base2 = DVec3::new(10_000_000.0, 2.0e9, 0.0);
    for (s, cid, sid, dx) in [(&mut sess1, char1, ship1, 0.0), (&mut sess2, char2, ship2, 300.0), (&mut sess3, char3, ship3, 400.0), (&mut sess4, char4, ship4, 500.0)] {
        move_pilot_to_system(&ctx, s, cid, sid, 587, sys_id, base2 + DVec3::new(dx, 0.0, 0.0)).await;
        ctx.crimewatch.write().unwrap().states.remove(&cid);
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(99_202, FleetRecord { fleet_id: 99_202, boss_id: char1, members: vec![char1, char2, char3, char4], ..Default::default() });
    });
    seed_cargo_item(&ctx, 720_100_030, FIL_T4_ELEC, char1, ship1, 3);
    let dep_fg = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(720_100_030)], &kw, None).await.expect("Frigate T4 deploy");
    assert_eq!(dep_fg.dict_get("maxShips"), Some(&PyRep::Integer(3)));
    let fg_trace = dep_fg.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();

    let dscan = ctx.dispatch_rpc(&mut sess4, "scanMgr", None, "DirectionalScan", &[PyRep::Real(360.0), PyRep::Real(1e12)], &kw, None).await.expect("D-scan");
    let PyRep::Rowset { rows, .. } = dscan else { panic!("expected Rowset") };
    assert!(rows.iter().any(|r| r.first().and_then(|v| v.as_i64()) == Some(fg_trace)), "Trace visible on D-scan");

    let ent_fg1 = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(fg_trace)], &kw, None).await.expect("FG 1");
    let _ = ctx.dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(fg_trace)], &kw, None).await.expect("FG 2");
    let _ = ctx.dispatch_rpc(&mut sess3, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(fg_trace)], &kw, None).await.expect("FG 3");
    let e_fg4 = ctx.dispatch_rpc(&mut sess4, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(fg_trace)], &kw, None).await.unwrap_err();
    assert_eq!(e_fg4.code, "TraceShipLimitReached");
    assert!(ctx.crimewatch.read().unwrap().is_suspect(char2, ctx.now_unix()), "Fleetmate receives Suspect flag on high-sec Suspect trace entry");

    let fg_pid = ent_fg1.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();
    let fg_cache_id = 1_896_000_000 + fg_pid * 10 + 1;
    ctx.db.with_snapshot(|s| {
        let mut by_tid: HashMap<u32, i32> = HashMap::new();
        for it in s.items.values().filter(|i| i.location_id == fg_cache_id) {
            *by_tid.entry(it.type_id).or_default() += it.quantity;
        }
        assert_eq!(by_tid.get(&FIL_T4_ELEC), Some(&3), "3x Frigate scales filament loot by 3x");
        assert_eq!(by_tid.get(&48_121), Some(&15), "3x Frigate scales T4 survey database loot by 3x (15)");
        assert_eq!(by_tid.get(&48_112), Some(&75), "3x Frigate scales T4 zero-point condensate loot by 3x (75)");
    });
    let fg_sys = ent_fg1.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    let fg_h = ctx.systems.get_or_spawn(fg_sys);
    for r in 0..3 {
        crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, char1, r).await;
        let gid = 1_895_000_000 + fg_pid * 10 + (r as i64 + 1);
        let gpos = fg_h.get_ball_snapshot(gid).await.unwrap().unwrap().1.position;
        for (idx, s) in [&mut sess1, &mut sess2, &mut sess3].into_iter().enumerate() {
            let sid = [ship1, ship2, ship3][idx];
            let _ = fg_h.set_ball_position(sid, gpos + DVec3::new((idx as f64 + 1.0) * 100.0, 0.0, 0.0)).await;
            if r < 2 {
                ctx.dispatch_rpc(s, "abyssalMgr", None, "AbyssalGateActivation", &[PyRep::Integer(gid)], &kw, None).await.expect("gate");
            } else {
                ctx.dispatch_rpc(s, "abyssalMgr", None, "AbyssalEndGateActivation", &[PyRep::Integer(gid)], &kw, None).await.expect("exit");
            }
        }
    }
    assert!(ctx.systems.get_or_spawn(sys_id).get_ball_snapshot(fg_trace).await.unwrap().is_none(), "Trace despawns after last frigate exits");
    let ret_pos = ctx.systems.get_or_spawn(sys_id).get_ball_snapshot(ship1).await.unwrap().unwrap().1.position;
    assert!(ret_pos.distance(base2) <= 2_000.0, "Exiting ship returns to origin location (dist = {})", ret_pos.distance(base2));

    // 4. 20-minute timer expiry destroys both ship AND capsule (670) and respawns pilot at home station
    let base3 = DVec3::new(20_000_000.0, 2.0e9, 0.0);
    move_pilot_to_system(&ctx, &mut sess1, char1, ship1, 621, sys_id, base3).await;
    seed_cargo_item(&ctx, 720_100_040, FIL_T1_ELEC, char1, ship1, 1);
    let dep_exp_run = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(720_100_040)], &kw, None).await.expect("solo deploy");
    let tr_exp = dep_exp_run.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
    let ent_exp = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr_exp)], &kw, None).await.expect("solo enter");
    let exp_sys = ent_exp.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    assert!(ctx.systems.get_or_spawn(exp_sys).get_ball_snapshot(ship1).await.unwrap().is_some(), "Ship in pocket before expiry");

    ctx.advance_secs(1_205);
    assert!(ctx.systems.get_or_spawn(exp_sys).get_ball_snapshot(ship1).await.unwrap().is_none(), "Ship ball destroyed on 20-minute expiry");
    assert!(!ctx.systems.get_or_spawn(exp_sys).list_balls().await.unwrap().iter().any(|b| b.type_id == 670), "Capsule (670) also destroyed on 20-minute expiry");
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&ship1), "Ship item destroyed on 20-minute expiry");
        let ch = &snap.characters[&char1];
        assert!(ch.station_id.is_some(), "Pilot respawned at home station after capsule destruction");
        assert!(!(32_000_000..=32_999_999).contains(&ch.solar_system_id), "Pilot no longer in Abyssal space");
    });
}
