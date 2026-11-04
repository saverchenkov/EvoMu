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
        (SYS_08_LASHESIH, FIL_T4_ELEC, None, false),
        (SYS_07_ORDIZE, FIL_T4_ELEC, None, false),
        (SYS_07_ORDIZE, FIL_T5_ELEC, None, false),
        (SYS_06_SOOMA, FIL_T5_ELEC, None, false),
        (SYS_06_SOOMA, FIL_T6_ELEC, None, false),
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
                let sus_on_exit = rep.dict_get("suspectFlagOnExit") == Some(&PyRep::Bool(true));
                assert_eq!(sus_on_exit, expected_suspect, "suspect mismatch for sys {sys_id} fil {fil_tid}");
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
    eve_server::session_cluster_lifecycle::record_pvp_aggression_and_renew_logoff_timer(&ctx, char_id, 90_000_002, ctx.now_unix());
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
    let e_fg_nofleet = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(720_100_010)], &kw, None).await.unwrap_err();
    assert_eq!(e_fg_nofleet.code, "NotInFleet", "Fleetless Frigate filament deployment must be rejected");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&720_100_010].quantity), 3, "Filaments not consumed on NotInFleet");
    move_pilot_to_system(&ctx, &mut sess1, char1, ship1, 16_236, sys_id, base1).await;
    seed_cargo_item(&ctx, 720_100_020, FIL_T1_ELEC, char1, ship1, 2);
    let e_dd_nofleet = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(720_100_020)], &kw, None).await.unwrap_err();
    assert_eq!(e_dd_nofleet.code, "NotInFleet", "Fleetless Destroyer filament deployment must be rejected");

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(99_201, FleetRecord { fleet_id: 99_201, boss_id: char1, members: vec![char1, char2, char3], ..Default::default() });
    });
    move_pilot_to_system(&ctx, &mut sess1, char1, ship1, exp_frig_tid, sys_id, base1 + DVec3::new(2_000_000.0, 0.0, 0.0)).await;
    let dep_exp = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(720_100_010)], &kw, None).await.expect("Expedition Frigate deploy");
    assert_eq!(dep_exp.dict_get("maxShips"), Some(&PyRep::Integer(3)), "Expedition Frigate (group 1283) allows 3-ship co-op");

    // 2. Destroyer co-op: requires 2 filaments (1 rejected), allows 2 fleetmates, rejects 3rd (TraceShipLimitReached), fleetless (NotInFleet), & wrong fleet (AccessDenied)
    move_pilot_to_system(&ctx, &mut sess1, char1, ship1, 16_236, sys_id, base1).await;
    ctx.db.with_mut_snapshot(|snap| snap.items.get_mut(&720_100_020).unwrap().quantity = 1);
    let ins_dd = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(720_100_020)], &kw, None).await.unwrap_err();
    assert_eq!(ins_dd.code, "InsufficientFilamentQuantity");

    ctx.db.with_mut_snapshot(|snap| snap.items.get_mut(&720_100_020).unwrap().quantity = 2);
    let dep_dd = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(720_100_020)], &kw, None).await.expect("Destroyer deploy");
    assert_eq!(dep_dd.dict_get("maxShips"), Some(&PyRep::Integer(2)));
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&720_100_020).map(|i| i.quantity).unwrap_or(0)), 0, "2 filaments consumed");
    let dd_trace = dep_dd.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();

    // Fleetless pilot gets NotInFleet; wrong-fleet pilot gets AccessDenied; fleetmate in Frigate gets DisallowedShipType
    move_pilot_to_system(&ctx, &mut sess4, char4, ship4, 16_236, sys_id, base1 + DVec3::new(700.0, 0.0, 0.0)).await;
    let e_nonfleet = ctx.dispatch_rpc(&mut sess4, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(dd_trace)], &kw, None).await.unwrap_err();
    assert_eq!(e_nonfleet.code, "NotInFleet");
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(99_205, FleetRecord { fleet_id: 99_205, boss_id: char4, members: vec![char4], ..Default::default() });
    });
    let e_wrong_fleet = ctx.dispatch_rpc(&mut sess4, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(dd_trace)], &kw, None).await.unwrap_err();
    assert_eq!(e_wrong_fleet.code, "AccessDenied");
    ctx.db.with_mut_snapshot(|snap| { snap.fleets.remove(&99_205); });
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
        snap.fleets.remove(&99_201);
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
    let ent_fg2 = ctx.dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(fg_trace)], &kw, None).await.expect("FG 2");
    let _ = ctx.dispatch_rpc(&mut sess3, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(fg_trace)], &kw, None).await.expect("FG 3");
    let e_fg4 = ctx.dispatch_rpc(&mut sess4, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(fg_trace)], &kw, None).await.unwrap_err();
    assert_eq!(e_fg4.code, "TraceShipLimitReached");
    assert_eq!(ent_fg2.dict_get("suspectFlagOnExit"), Some(&PyRep::Bool(true)), "Fleetmate receives Suspect flag on exit from high-sec Suspect trace");

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
    ctx.crimewatch.write().unwrap().states.remove(&char1);
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

#[tokio::test]
async fn test_abyssal_coop_staggered_rooms_reentry_kinematics_and_pod_corpse() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = SYS_08_LASHESIH;
    let (char1, char2) = (90_000_001_i64, 90_000_002_i64);
    let uid3 = ctx.db.provision_player_account("abyss_stagger3", "pw123");
    let char3 = ctx.db.create_character(uid3, "Amarr Abyss Pilot", 1, 1, 11, 17, &ctx.sde).unwrap().character_id;
    let (ship1, ship2, ship3) = (625_000_001_i64, 625_000_002_i64, 625_000_003_i64);
    let base = DVec3::new(35_000_000.0, 2.0e9, 0.0);

    // 1. BF-06 & R-ABYSS-CLOUD-FIT-01: activate_trace enforces cloak/tether checks, 30m un-entered lifetime, and fitted Dogma kinematics across ClientIsReady & exit
    let mut sess1 = login_pilot_in_space(&ctx, 887_401, "demo", "demo-secret", char1, ship1, 624, sys_id, base).await;
    let nano_id = 725_100_099_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(nano_id, InventoryItemRecord {
            item_id: nano_id, type_id: 2_605, owner_id: char1, location_id: ship1, flag: InventoryFlag::LoSlot0, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        snap.module_online_states.insert(nano_id, true);
    });
    ctx.invalidate_ship_fitting(ship1);
    let raw_maller_vel = ctx.sde.types[&624].default_attributes[&37];
    let raw_maller_in = ctx.sde.types[&624].default_attributes[&70];
    let pre_fit = ctx.get_or_rebuild_ship_fitting(char1, ship1).expect("fitted Maller");
    let (fit_maller_vel, fit_maller_in) = (pre_fit.ship.get_attribute(37, &ctx.sde), pre_fit.ship.get_attribute(70, &ctx.sde));
    assert!((fit_maller_vel - raw_maller_vel).abs() > 1.0 && (fit_maller_in - raw_maller_in).abs() > 0.01, "Nanofiber II modifies Maller speed & agility");
    seed_cargo_item(&ctx, 725_100_001, FIL_T1_ELEC, char1, ship1, 1);
    let dep1 = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(725_100_001)], &kw, None).await.expect("dep1");
    let tr1 = dep1.dict_get("traceBallID").unwrap().as_i64().unwrap();
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    { let mut bp = sys_h.ballpark.write().unwrap(); bp.combat_states.get_mut(&ship1).unwrap().is_cloaked = true; }
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr1)], &kw, None).await.unwrap_err().code, "ShipIsCloaked");
    { let mut bp = sys_h.ballpark.write().unwrap(); let cs = bp.combat_states.get_mut(&ship1).unwrap(); cs.is_cloaked = false; cs.is_tethered = true; }
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr1)], &kw, None).await.unwrap_err().code, "ShipIsTethered");
    { let mut bp = sys_h.ballpark.write().unwrap(); bp.combat_states.get_mut(&ship1).unwrap().is_tethered = false; }

    ctx.advance_secs(1_250); // > 20m, < 30m: un-entered trace still valid
    let ent1 = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr1)], &kw, None).await.expect("enter at 1250s");
    let pid1 = ent1.dict_get("pocketID").unwrap().as_i64().unwrap();
    let p_sys = ent1.dict_get("solarSystemID").unwrap().as_i64().unwrap() as u32;
    let p_h = ctx.systems.get_or_spawn(p_sys);
    let ready_rep = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "ClientIsReady", &[], &kw, None).await.expect("abyssalMgr.ClientIsReady");
    assert_eq!(ready_rep.dict_get("ready"), Some(&PyRep::Bool(true)));
    let (_, ready_maller) = p_h.get_ball_snapshot(ship1).await.unwrap().expect("in-pocket Maller ball after ClientIsReady");
    assert!((ready_maller.max_velocity - fit_maller_vel).abs() < 1e-3 && (ready_maller.max_velocity - raw_maller_vel).abs() > 1.0, "ClientIsReady preserves fitted max_velocity");
    assert!((ready_maller.inertia - fit_maller_in).abs() < 1e-4 && (ready_maller.inertia - 0.5).abs() > 1e-3, "ClientIsReady preserves fitted inertia (not hardcoded 0.5)");
    for r in 0..3 {
        crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, char1, r).await;
        let gid = 1_895_000_000 + pid1 * 10 + (r as i64 + 1);
        let gpos = p_h.get_ball_snapshot(gid).await.unwrap().unwrap().1.position;
        let _ = p_h.set_ball_position(ship1, gpos + DVec3::new(100.0, 0.0, 0.0)).await;
        let method = if r < 2 { "AbyssalGateActivation" } else { "AbyssalEndGateActivation" };
        ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, method, &[PyRep::Integer(gid)], &kw, None).await.expect("transit");
    }
    let (_, exit_maller) = sys_h.get_ball_snapshot(ship1).await.unwrap().expect("exited Maller ball");
    let exp_fit = ctx.get_or_rebuild_ship_fitting(char1, ship1).expect("Maller fitting");
    assert!((exit_maller.max_velocity - exp_fit.ship.get_attribute(37, &ctx.sde)).abs() < 1e-3 && (exit_maller.max_velocity - 250.0).abs() > 1.0, "Exit ball uses fitted Dogma max_velocity (not hardcoded 250.0)");
    assert!((exit_maller.inertia - exp_fit.ship.get_attribute(70, &ctx.sde)).abs() < 1e-4 && (exit_maller.inertia - 0.5).abs() > 1e-3, "Exit ball uses fitted Dogma inertia (not hardcoded 0.5)");

    // 2. BF-02 & BF-06: Co-op staggered room boundary safety, TraceAlreadyEntered re-entry refusal, and boundary pod corpse/implant/faction corvette
    let base_c = DVec3::new(50_000_000.0, 2.0e9, 0.0);
    move_pilot_to_system(&ctx, &mut sess1, char1, ship1, 587, sys_id, base_c).await;
    let mut sess2 = login_pilot_in_space(&ctx, 887_402, "other", "other-secret", char2, ship2, 587, sys_id, base_c + DVec3::new(200.0, 0.0, 0.0)).await;
    let mut sess3 = login_pilot_in_space(&ctx, 887_403, "abyss_stagger3", "pw123", char3, ship3, 587, sys_id, base_c + DVec3::new(400.0, 0.0, 0.0)).await;
    let implant_iid = 725_200_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(99_401, FleetRecord { fleet_id: 99_401, boss_id: char1, members: vec![char1, char2, char3], ..Default::default() });
        snap.characters.get_mut(&char3).unwrap().race_id = 4; // Amarr -> Impairor (596)
        snap.items.insert(implant_iid, InventoryItemRecord {
            item_id: implant_iid, type_id: 9_899, owner_id: char3, location_id: char3, flag: InventoryFlag::Implant, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        snap.active_implants.insert(char3, vec![9_899]);
    });
    seed_cargo_item(&ctx, 725_100_010, FIL_T1_ELEC, char1, ship1, 3);
    let dep_c = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(725_100_010)], &kw, None).await.expect("dep coop");
    let tr_c = dep_c.dict_get("traceBallID").unwrap().as_i64().unwrap();
    let e_c1 = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr_c)], &kw, None).await.expect("c1");
    let e_c2 = ctx.dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr_c)], &kw, None).await.expect("c2");
    let _ = ctx.dispatch_rpc(&mut sess3, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr_c)], &kw, None).await.expect("c3");
    let c_pid = e_c1.dict_get("pocketID").unwrap().as_i64().unwrap();
    assert_eq!(Some(c_pid), e_c2.dict_get("pocketID").and_then(|v| v.as_i64()));

    // Pilot 1 advances to Room 3 while Pilot 2 stays in Room 1 -> Pilot 2 takes ZERO boundary damage
    for r in 0..2 {
        crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, char1, r).await;
        let gid = 1_895_000_000 + c_pid * 10 + (r as i64 + 1);
        let gpos = p_h.get_ball_snapshot(gid).await.unwrap().unwrap().1.position;
        let _ = p_h.set_ball_position(ship1, gpos + DVec3::new(100.0, 0.0, 0.0)).await;
        ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalGateActivation", &[PyRep::Integer(gid)], &kw, None).await.expect("c1 advance");
    }
    let ship2_hp_before = p_h.get_combat_state(ship2).await.unwrap().unwrap().structure_hp;
    for _ in 0..4 {
        let mut bp = p_h.ballpark.write().unwrap();
        let _ = eve_server::services::pve_special_sites_abyssal::step_abyssal_hazards_and_pylons_in_ballpark(&mut bp, Some(&ctx.sde), Some(&ctx.db), p_sys);
    }
    assert_eq!(p_h.get_combat_state(ship2).await.unwrap().unwrap().structure_hp, ship2_hp_before, "Pilot 2 in Room 1 takes zero boundary damage when Pilot 1 is in Room 3");

    // Pilot 1 exits Room 3 while Pilot 2 & 3 remain -> Pilot 1 re-entry into open trace rejected with TraceAlreadyEntered
    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, char1, 2).await;
    let r3_gid = 1_895_000_000 + c_pid * 10 + 3;
    let r3_gpos = p_h.get_ball_snapshot(r3_gid).await.unwrap().unwrap().1.position;
    let _ = p_h.set_ball_position(ship1, r3_gpos + DVec3::new(100.0, 0.0, 0.0)).await;
    ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEndGateActivation", &[PyRep::Integer(r3_gid)], &kw, None).await.expect("c1 exit");
    let tr_pos = sys_h.get_ball_snapshot(tr_c).await.unwrap().expect("trace still open for c2/c3").1.position;
    let _ = sys_h.set_ball_position(ship1, tr_pos + DVec3::new(200.0, 0.0, 0.0)).await;
    let re_err = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr_c)], &kw, None).await.unwrap_err();
    assert_eq!(re_err.code, "TraceAlreadyEntered", "Exited pilot cannot re-enter same trace without filaments");

    // Pilot 3 strays > 75 km in Room 1 -> ship & capsule destroyed -> implant destroyed, Corpse (25) spawned, DetailedKillmail & Amarr Impairor (596)
    let r1_center = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| DVec3::from_array(reg.abyssal_pockets[&char3].grid_offset));
    let _ = p_h.set_ball_position(ship3, r1_center + DVec3::new(85_000.0, 0.0, 0.0)).await;
    if let Ok(Some(mut cs)) = p_h.get_combat_state(ship3).await { cs.structure_hp = 200.0; let _ = p_h.set_combat_state(ship3, cs).await; }
    for _ in 0..30 {
        if ctx.db.with_snapshot(|s| s.characters[&char3].station_id.is_some()) { break; }
        let mut bp = p_h.ballpark.write().unwrap();
        let _ = eve_server::services::pve_special_sites_abyssal::step_abyssal_hazards_and_pylons_in_ballpark(&mut bp, Some(&ctx.sde), Some(&ctx.db), p_sys);
    }
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&implant_iid), "Plugged-in implant destroyed on boundary pod death");
        assert!(!snap.active_implants.contains_key(&char3), "active_implants cleared on boundary pod death");
        assert!(snap.items.values().any(|i| i.type_id == 25 && i.owner_id == char3 && i.location_id == p_sys as i64), "Corpse (type 25) spawned in Abyssal space");
        assert!(snap.combat_crimewatch_state.detailed_killmails.values().any(|km| {
            km.victim_character_id == char3 && km.victim_ship_type_id == 670 && km.destroyed_items.iter().any(|d| d.type_id == 9_899)
        }), "DetailedKillmail recorded for capsule with destroyed implant 9899");
        let new_ship_id = snap.characters[&char3].active_ship_id;
        assert_eq!(snap.items[&new_ship_id].type_id, 596, "Amarr pilot respawned in Impairor (596) faction rookie corvette");
    });
}

#[tokio::test]
async fn test_abyssal_staggered_coop_r1_to_r2_preserves_conduit_cache_and_interactivity() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = SYS_08_LASHESIH;
    let (char1, char2) = (90_000_001_i64, 90_000_002_i64);
    let (ship1, ship2) = (626_000_001_i64, 626_000_002_i64);
    let base = DVec3::new(60_000_000.0, 2.0e9, 0.0);

    // Destroyer co-op (Coercer 16236 -> maxShips == 2)
    let mut sess1 = login_pilot_in_space(&ctx, 887_501, "demo", "demo-secret", char1, ship1, 16_236, sys_id, base).await;
    let mut sess2 = login_pilot_in_space(&ctx, 887_502, "other", "other-secret", char2, ship2, 16_236, sys_id, base + DVec3::new(300.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(99_501, FleetRecord { fleet_id: 99_501, boss_id: char1, members: vec![char1, char2], ..Default::default() });
    });

    // P2-2: Non-positive itemID / gateID rejected without scanning registry
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(0)], &kw, None).await.unwrap_err().code, "MissingRequiredArgument");
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(-1)], &kw, None).await.unwrap_err().code, "MissingRequiredArgument");

    seed_cargo_item(&ctx, 726_100_001, FIL_T1_ELEC, char1, ship1, 2);
    let dep = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(726_100_001)], &kw, None).await.expect("deploy destroyer trace");
    assert_eq!(dep.dict_get("maxShips"), Some(&PyRep::Integer(2)));
    let tr_id = dep.dict_get("traceBallID").unwrap().as_i64().unwrap();

    // Pilot 1 enters Room 1, clears Room 1 NPCs, and jumps R1 -> R2 BEFORE Pilot 2 enters the trace
    ctx.set_mechanic_rng_seed(Some(4));
    let ent1 = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr_id)], &kw, None).await.expect("pilot 1 enters R1");
    let pid = ent1.dict_get("pocketID").unwrap().as_i64().unwrap();
    let p_sys = ent1.dict_get("solarSystemID").unwrap().as_i64().unwrap() as u32;
    let p_h = ctx.systems.get_or_spawn(p_sys);
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalGateActivation", &[PyRep::Integer(0)], &kw, None).await.unwrap_err().code, "InvalidGate");

    let r1_gate = 1_895_000_000 + pid * 10 + 1;
    let r1_cache = 1_896_000_000 + pid * 10 + 1;
    let r1_subnode = r1_cache + 500_000;
    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, char1, 0).await;
    let g1_pos = p_h.get_ball_snapshot(r1_gate).await.unwrap().unwrap().1.position;
    let _ = p_h.set_ball_position(ship1, g1_pos + DVec3::new(100.0, 0.0, 0.0)).await;
    let adv1 = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalGateActivation", &[PyRep::Integer(r1_gate)], &kw, None).await.expect("pilot 1 jumps R1->R2 before pilot 2 enters");
    assert_eq!(adv1.dict_get("room"), Some(&PyRep::Integer(2)));

    // Room 1 conduit, Bioadaptive Cache + loot, Subnode, and sampled hazards MUST remain intact for Pilot 2
    assert!(p_h.get_ball_snapshot(r1_gate).await.unwrap().is_some(), "Room 1 conduit preserved for un-entered co-op slot");
    assert!(p_h.get_ball_snapshot(r1_cache).await.unwrap().is_some(), "Room 1 Bioadaptive Cache ball preserved");
    assert!(p_h.get_ball_snapshot(r1_subnode).await.unwrap().is_some(), "Room 1 Extraction Subnode ball preserved");
    ctx.db.with_snapshot(|s| {
        assert!(s.items.contains_key(&r1_cache) && s.items.values().any(|i| i.location_id == r1_cache), "Room 1 cache DB item & loot preserved");
        assert!(s.items.contains_key(&r1_subnode), "Room 1 subnode DB item preserved");
    });

    // Pilot 2 activates origin trace to enter Room 1 -> receives SetBallInteractive(r1_gate, true) and transits R1 -> R2
    let ent2 = ctx.dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr_id)], &kw, None).await.expect("pilot 2 staggered entry into R1");
    assert_eq!(ent2.dict_get("pocketID"), Some(&PyRep::Integer(pid)));
    let (_, obs2, _) = p_h.drain_observer_updates(ship2).await.expect("ship2 observer updates");
    assert!(
        obs2.iter().any(|u| matches!(*u, eve_destiny::DestinyEvent::SetBallInteractive { ball_id, is_interactive: true } if ball_id == r1_gate)),
        "Pilot 2 receives SetBallInteractive(true) for already-cleared Room 1 conduit upon staggered entry"
    );

    let _ = p_h.set_ball_position(ship2, g1_pos + DVec3::new(150.0, 0.0, 0.0)).await;
    let adv2 = ctx.dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalGateActivation", &[PyRep::Integer(r1_gate)], &kw, None).await.expect("pilot 2 jumps R1->R2 via preserved conduit");
    assert_eq!(adv2.dict_get("room"), Some(&PyRep::Integer(2)));
    assert!(p_h.get_ball_snapshot(r1_gate).await.unwrap().is_none(), "Room 1 conduit cleaned up once final co-op pilot advances to R2");
}

pub(crate) mod abyssal_in_pocket_restrictions_boundary {
    // End-to-end integration tests for Abyssal in-pocket restrictions, spherical 75km
    // boundary damage, pocket clearing rules, automated pylons, caches, clouds, and weathers.


    use crate::abyssal_deadspace::{
        login_pilot_in_space, move_pilot_to_system, seed_cargo_item, FIL_T0_ELEC, FIL_T1_ELEC,
        FIL_T3_ELEC, SYS_08_LASHESIH, SYS_09_JITA, SYS_LOW_TAMA,
    };
    use crate::common;
    use eve_db::{BookmarkRecord, InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, DamageVector, MovementMode};
    use eve_proto::quasar::{
        encode_string_field, EvePublicRequest, GoogleAny, GoogleTimestamp, TYPE_GOOGLEAPIS_PREFIX,
    };
    use eve_proto::PyRep;
    use eve_server::quasar_gateway::STATUS_FORBIDDEN;
    use eve_server::router::ServerContext;
    use eve_server::services::pve_special_sites::with_pve_special_reg;
    use eve_server::services::pve_special_sites_abyssal::{
        evacuate_abyssal_pockets_for_downtime, evaluate_abyssal_spatial_clouds_for_ship,
        pulse_abyssal_automated_pylons, roll_abyssal_weather_penalty_pct, spatial_cloud_radius_m,
        step_abyssal_hazards_and_pylons_in_ballpark, suppressor_pulse_damage, suppressor_radius_m,
        tracking_pylon_radius_and_mult, AbyssalWeatherKind, AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID, AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID,
        AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID, BIOLUMINESCENT_CLOUD_LARGE_TYPE_ID,
        BIOLUMINESCENT_CLOUD_SMALL_TYPE_ID, BIOLUMINESCENT_CLOUD_TYPE_ID,
        FILAMENT_CLOUD_LARGE_TYPE_ID, FILAMENT_CLOUD_SMALL_TYPE_ID, FILAMENT_CLOUD_TYPE_ID,
        MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID, MULTIBODY_TRACKING_PYLON_TYPE_ID,
        PROVING_TACHYON_CLOUD_TYPE_ID, SPATIAL_CLOUD_RADIUS_M, TACHYON_CLOUD_TYPE_ID,
    };
    use glam::DVec3;
    use std::collections::HashMap;

    // parity: feat:pve.abyssal-in-pocket-restrictions, feat:pve.abyssal-boundary-damage, feat:pve.abyssal-pocket-clearing-rules
    #[tokio::test]
    async fn test_abyssal_in_pocket_restrictions_boundary_damage_and_clearing() {
        eve_testkit::attest(&[
            "feat:pve.abyssal-in-pocket-restrictions",
            "feat:pve.abyssal-boundary-damage",
            "feat:pve.abyssal-pocket-clearing-rules",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let kw = HashMap::new();
        let sys_id = SYS_08_LASHESIH;
        let char1 = 90_000_001_i64;
        let ship1 = 630_000_001_i64;
        let mut sess1 = login_pilot_in_space(&ctx, 887_201, "demo", "demo-secret", char1, ship1, 621, sys_id, DVec3::new(0.0, 2.0e9, 0.0)).await;

        let cloak_id = 730_100_001_i64;
        let depot_id = 730_100_002_i64;
        let fil_id = 730_100_003_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(cloak_id, InventoryItemRecord {
                item_id: cloak_id, type_id: 11_578, owner_id: char1, location_id: ship1, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
            snap.module_online_states.insert(cloak_id, true);
            snap.items.insert(depot_id, InventoryItemRecord {
                item_id: depot_id, type_id: 33_474, owner_id: char1, location_id: ship1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
        });
        seed_cargo_item(&ctx, fil_id, FIL_T1_ELEC, char1, ship1, 1);

        let dep = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fil_id)], &kw, None).await.expect("dep");
        let tr1 = dep.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
        let ent = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr1)], &kw, None).await.expect("ent");
        let abyss_sys = ent.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
        let pid = ent.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();
        let abyss_h = ctx.systems.get_or_spawn(abyss_sys);

        // 1. Pocket clearing rules: spawn ~50 km from gate, gate locked until NPCs cleared, no generic NPC wrecks
        let r1_gate = 1_895_000_000 + pid * 10 + 1;
        let gate_pos = abyss_h.get_ball_snapshot(r1_gate).await.unwrap().unwrap().1.position;
        let ship_pos = abyss_h.get_ball_snapshot(ship1).await.unwrap().unwrap().1.position;
        let dist_to_gate = ship_pos.distance(gate_pos);
        assert!((35_000.0..=70_000.0).contains(&dist_to_gate), "Arriving ship spawns ~50km from gate (got {dist_to_gate})");

        let locked_err = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalGateActivation", &[PyRep::Integer(r1_gate)], &kw, None).await.unwrap_err();
        assert_eq!(locked_err.code, "GateLocked");

        // 2. In-pocket restrictions: warp, bookmark align, cloak, mobile depot launch/anchor, Local chat, D-scan, star map
        let bm_id = 730_300_001_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.bookmarks.insert(bm_id, BookmarkRecord {
                bookmark_id: bm_id, owner_id: char1, label: "Abyss BM".into(), solar_system_id: abyss_sys,
                x: gate_pos.x + 200_000.0, y: gate_pos.y, z: gate_pos.z, item_id: None, memo: String::new(), folder_id: None, is_shared: false,
            });
        });
        let beyonce_oid = ctx.bind_object(&sess1, "beyonce", abyss_sys as i64).expect("bind beyonce");
        let e_warp = ctx.dispatch_rpc(&mut sess1, "beyonce", Some(beyonce_oid), "CmdGotoBookmark", &[PyRep::Integer(bm_id)], &kw, None).await.unwrap_err();
        assert_eq!(e_warp.code, "CannotWarpInAbyssalSpace");

        let mut align_kw = HashMap::new();
        align_kw.insert("bookmarkID".into(), PyRep::Integer(bm_id));
        let e_align = ctx.dispatch_rpc(&mut sess1, "beyonce", Some(beyonce_oid), "CmdAlignTo", &[], &align_kw, None).await.unwrap_err();
        assert_eq!(e_align.code, "CannotWarpInAbyssalSpace");

        let e_cloak = ctx.dispatch_rpc(&mut sess1, "dogmaIM", None, "Activate", &[PyRep::Integer(cloak_id), PyRep::String("cloaking".into())], &kw, None).await.unwrap_err();
        assert_eq!(e_cloak.code, "CannotCloakInAbyssalSpace");

        let drop_arg = PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(depot_id), PyRep::Integer(1)])]);
        let e_drop = ctx.dispatch_rpc(&mut sess1, "ship", None, "Drop", &[drop_arg], &kw, None).await.unwrap_err();
        assert_eq!(e_drop.code, "CannotDeployInAbyssalSpace");

        let e_anchor = ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_id)], &kw, None).await.unwrap_err();
        assert_eq!(e_anchor.code, "CannotDeployInAbyssalSpace");

        let local_chan = format!("solarsystemid2_{abyss_sys}");
        let e_lsc = ctx.dispatch_rpc(&mut sess1, "LSC", None, "JoinChannel", &[PyRep::List(vec![PyRep::String(local_chan)])], &kw, None).await.unwrap_err();
        assert_eq!(e_lsc.code, "LocalChannelDisabledInAbyssalSpace");

        let mut q_bytes = Vec::new();
        encode_string_field(1, "blocked in abyss", &mut q_bytes);
        let q_req = EvePublicRequest {
            issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
            correlation_uuid: b"abyss-chat-01".to_vec(),
            payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.chat.local.api.BroadcastMessageRequest"), value: q_bytes }),
            external_origin: "eve-client".into(),
            application_instance_uuid: b"app-01".to_vec(),
            authoritative_context: None,
            trace_context: None,
        };
        let q_rsp = ctx.quasar_gateway.handle_public_request(&sess1, &q_req, None, false, ctx.now_unix());
        assert_eq!(q_rsp.status_code, STATUS_FORBIDDEN);

        let e_dscan = ctx.dispatch_rpc(&mut sess1, "scanMgr", None, "DirectionalScan", &[PyRep::Real(360.0), PyRep::Real(1e12)], &kw, None).await.unwrap_err();
        assert_eq!(e_dscan.code, "CannotDscanInThisSystem");

        let e_map = ctx.dispatch_rpc(&mut sess1, "map", None, "GetHistory", &[PyRep::Integer(3), PyRep::Integer(24)], &kw, None).await.unwrap_err();
        assert_eq!(e_map.code, "MapDisabledInAbyssalSpace");

        // Clear Room 1 NPCs -> zero wrecks of any type, zero bounty, zero faction standing change, 300 HP Bioadaptive Cache
        let r1_cache_id = 1_896_000_000 + pid * 10 + 1;
        let cache_cs = abyss_h.get_combat_state(r1_cache_id).await.unwrap().expect("Bioadaptive Cache combat state");
        let cache_total_hp = cache_cs.shield_hp + cache_cs.armor_hp + cache_cs.structure_hp;
        assert!((cache_total_hp - 300.0).abs() < 1e-6, "Triglavian Bioadaptive Cache must have 300 total HP (got {cache_total_hp})");

        let (pre_balance, pre_standings, pre_standing_tx) = ctx.db.with_snapshot(|snap| {
            (
                snap.characters[&char1].balance,
                snap.characters[&char1].standings.clone(),
                snap.standing_transactions.get(&char1).cloned(),
            )
        });
        crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, char1, 0).await;
        assert!(
            !abyss_h.list_balls().await.unwrap().iter().any(|b| ctx.sde.wrecks.values().any(|&w| w == b.type_id) || ctx.sde.types.get(&b.type_id).is_some_and(|t| t.group_id == 186)),
            "Abyssal NPCs must leave zero wrecks of any type in space"
        );
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.characters[&char1].balance, pre_balance, "Abyssal NPCs must pay zero bounty");
            assert_eq!(snap.characters[&char1].standings, pre_standings, "Abyssal NPCs must cause zero faction standing change");
            assert_eq!(snap.standing_transactions.get(&char1).cloned(), pre_standing_tx, "Abyssal NPCs must record zero standing transactions");
            assert!(
                !snap.items.values().any(|i| i.location_id == abyss_sys as i64 && (ctx.sde.wrecks.values().any(|&w| w == i.type_id) || ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 186))),
                "Abyssal NPCs must leave zero wreck items in DB"
            );
        });
        let _ = abyss_h.set_ball_position(ship1, gate_pos + DVec3::new(100.0, 0.0, 0.0)).await;
        ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalGateActivation", &[PyRep::Integer(r1_gate)], &kw, None).await.expect("R1->R2");
        let r2_gate = 1_895_000_000 + pid * 10 + 2;
        let r2_gate_pos = abyss_h.get_ball_snapshot(r2_gate).await.unwrap().unwrap().1.position;
        let r2_ship_pos = abyss_h.get_ball_snapshot(ship1).await.unwrap().unwrap().1.position;
        assert!((35_000.0..=70_000.0).contains(&r2_ship_pos.distance(r2_gate_pos)), "Room 2 arrival ~50km from gate");

        // 3. Spherical 75 km room boundary damage: escalates, resets inside <= 75km, destroys probe, ejects pod & kills pod
        let room2_center = with_pve_special_reg(&ctx, |reg| {
            DVec3::from_array(reg.abyssal_pockets[&char1].grid_offset) + DVec3::new(0.0, 0.0, 50_000.0)
        });
        let probe_id = 730_400_001_i64;
        let probe_ball = Ball {
            id: probe_id, type_id: 30_028, position: room2_center + DVec3::new(80_000.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 10.0, radius: 5.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        };
        abyss_h.try_spawn_ball_with_meta(
            probe_ball,
            ctx.build_type_combat_state(30_028),
            ctx.build_ship_slim_meta(char1, 30_028),
        );

        if let Ok(Some(mut cs)) = abyss_h.get_combat_state(ship1).await {
            cs.max_structure_hp = 50_000.0;
            cs.structure_hp = 50_000.0;
            let _ = abyss_h.set_combat_state(ship1, cs).await;
        }
        let _ = abyss_h.set_ball_position(ship1, room2_center + DVec3::new(80_000.0, 0.0, 0.0)).await;
        let hp0 = abyss_h.get_combat_state(ship1).await.unwrap().unwrap().structure_hp;
        {
            let mut bp = abyss_h.ballpark.write().unwrap();
            step_abyssal_hazards_and_pylons_in_ballpark(&mut bp, Some(&ctx.sde), Some(&ctx.db), abyss_sys);
        }
        assert!(abyss_h.get_ball_snapshot(probe_id).await.unwrap().is_none(), "Out-of-bounds probe destroyed");
        let hp1 = abyss_h.get_combat_state(ship1).await.unwrap().unwrap().structure_hp;
        let dmg1 = hp0 - hp1;
        assert!(dmg1 > 0.0, "Boundary damage applied outside 75km");

        {
            let mut bp = abyss_h.ballpark.write().unwrap();
            step_abyssal_hazards_and_pylons_in_ballpark(&mut bp, Some(&ctx.sde), Some(&ctx.db), abyss_sys);
        }
        let hp2 = abyss_h.get_combat_state(ship1).await.unwrap().unwrap().structure_hp;
        let dmg2 = hp1 - hp2;
        assert!(dmg2 > dmg1, "Boundary damage escalates per tick ({dmg2} > {dmg1})");

        let _ = abyss_h.set_ball_position(ship1, room2_center + DVec3::new(10_000.0, 0.0, 0.0)).await;
        {
            let mut bp = abyss_h.ballpark.write().unwrap();
            step_abyssal_hazards_and_pylons_in_ballpark(&mut bp, Some(&ctx.sde), Some(&ctx.db), abyss_sys);
        }
        assert_eq!(abyss_h.get_combat_state(ship1).await.unwrap().unwrap().structure_hp, hp2, "No boundary damage inside 75km");
        let _ = abyss_h.set_ball_position(ship1, room2_center + DVec3::new(80_000.0, 0.0, 0.0)).await;
        {
            let mut bp = abyss_h.ballpark.write().unwrap();
            step_abyssal_hazards_and_pylons_in_ballpark(&mut bp, Some(&ctx.sde), Some(&ctx.db), abyss_sys);
        }
        let hp3 = abyss_h.get_combat_state(ship1).await.unwrap().unwrap().structure_hp;
        assert!((hp2 - hp3 - dmg1).abs() < 1e-3, "Escalation counter reset after returning inside 75km");

        if let Ok(Some(mut cs)) = abyss_h.get_combat_state(ship1).await {
            cs.structure_hp = 300.0;
            let _ = abyss_h.set_combat_state(ship1, cs).await;
        }
        for _ in 0..40 {
            if abyss_h.get_ball_snapshot(ship1).await.unwrap().is_none() {
                break;
            }
            let mut bp = abyss_h.ballpark.write().unwrap();
            step_abyssal_hazards_and_pylons_in_ballpark(&mut bp, Some(&ctx.sde), Some(&ctx.db), abyss_sys);
        }
        let pod_id = ctx.db.with_snapshot(|s| s.characters[&char1].active_ship_id);
        assert_ne!(pod_id, ship1, "Capsule ejected when ship destroyed by boundary");
        assert_eq!(ctx.db.with_snapshot(|s| s.items[&pod_id].type_id), 670);

        for _ in 0..20 {
            if ctx.db.with_snapshot(|s| s.characters[&char1].station_id.is_some()) { break; }
            let mut bp = abyss_h.ballpark.write().unwrap();
            step_abyssal_hazards_and_pylons_in_ballpark(&mut bp, Some(&ctx.sde), Some(&ctx.db), abyss_sys);
        }
        assert!(ctx.db.with_snapshot(|s| s.characters[&char1].station_id.is_some()), "Pilot respawned at home station after pod boundary death");
        let origin_h = ctx.systems.get_or_spawn(sys_id);
        assert!(origin_h.get_ball_snapshot(tr1).await.unwrap().is_none(), "Origin trace ball despawned after boundary pod death");
        assert!(with_pve_special_reg(&ctx, |reg| !reg.abyssal_traces.contains_key(&tr1)), "abyssal_traces entry removed after boundary pod death");

        // 4. Downtime evacuation: preserves DroneBay drones, destroys deployed drones in space, spawns ship in origin ballpark, despawns trace, restores Local chat
        let sec_init = ctx.now_unix() % 86_400;
        let target_pre_dt = 11 * 3_600 - 300;
        ctx.advance_virtual_clock(if sec_init <= target_pre_dt { target_pre_dt - sec_init } else { 86_400 - sec_init + target_pre_dt });
        let (char2, ship2) = (90_000_002_i64, 630_000_002_i64);
        let mut sess2 = login_pilot_in_space(&ctx, 887_202, "other", "other-secret", char2, ship2, 621, sys_id, DVec3::new(30_000_000.0, 2.0e9, 0.0)).await;
        let (bay_drone, space_drone) = (730_500_001_i64, 730_500_002_i64);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(bay_drone, InventoryItemRecord {
                item_id: bay_drone, type_id: 2456, owner_id: char2, location_id: ship2, flag: InventoryFlag::DroneBay, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
            snap.items.insert(space_drone, InventoryItemRecord {
                item_id: space_drone, type_id: 2456, owner_id: char2, location_id: abyss_sys as i64, flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
        });
        seed_cargo_item(&ctx, 730_500_003, FIL_T1_ELEC, char2, ship2, 1);
        let dep2 = ctx.dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(730_500_003)], &kw, None).await.expect("dep2");
        let tr2 = dep2.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
        let ent2 = ctx.dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr2)], &kw, None).await.expect("ent2");
        let p2_sys = ent2.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
        assert!(origin_h.get_ball_snapshot(tr2).await.unwrap().is_some(), "Origin trace ball present before downtime evacuation");
        eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
            rt.active_drones_in_space.insert(space_drone, eve_server::services::combat_crimewatch::DroneSpaceState {
                ball_id: space_drone, drone_item_id: space_drone, type_id: 2456, group_id: 100, owner_char_id: char2,
                original_owner_char_id: char2, parent_ship_id: ship2, solar_system_id: p2_sys, target_id: None, activity_state: 0,
                assist_char_id: None, guard_char_id: None, is_abandoned: false, is_sentry: false, bandwidth_used: 5.0,
                max_velocity: 400.0, optimal_range: 5_000.0, damage_vector: DamageVector::default(), shield_repair_amount: 0.0,
                armor_repair_amount: 0.0, hull_repair_amount: 0.0, web_speed_factor: 1.0, neut_amount: 0.0, ecm_strength: 0.0, tp_sig_bonus_pct: 0.0,
            });
        });

        // Downtime outside 11:00 UTC window (10:55 UTC) must NOT evacuate active pocket; inside 11:00 UTC window (11:01 UTC) evacuates & destroys deployed drones
        let dt_non_window = eve_server::services::wallet_serverops::run_daily_downtime_and_jobs(&ctx);
        assert_eq!(dt_non_window.get("is1100UtcWindow"), Some(&0));
        assert_eq!(dt_non_window.get("evacuatedAbyssalPilots"), Some(&0), "Outside 11:00 UTC window must not evacuate pocket");
        assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char2].solar_system_id), p2_sys);

        ctx.advance_virtual_clock(360); // 10:55 UTC -> 11:01 UTC (6m elapsed, well within 20m Abyssal timer)
        let dt_window = eve_server::services::wallet_serverops::run_daily_downtime_and_jobs(&ctx);
        assert_eq!(dt_window.get("is1100UtcWindow"), Some(&1));
        assert!(dt_window.get("evacuatedAbyssalPilots").copied().unwrap_or(0) >= 1, "11:00 UTC downtime rescues active pocket");
        assert!(origin_h.get_ball_snapshot(ship2).await.unwrap().is_some(), "Evacuated ship ball spawned in origin solar system ballpark");
        assert!(origin_h.get_ball_snapshot(tr2).await.unwrap().is_none(), "Origin trace ball despawned on downtime evacuation");
        assert!(with_pve_special_reg(&ctx, |reg| !reg.abyssal_traces.contains_key(&tr2)), "abyssal_traces entry removed on downtime evacuation");
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.characters[&char2].solar_system_id, sys_id, "Pilot returned to origin system");
            assert!(snap.items.contains_key(&bay_drone), "Drone in DroneBay preserved");
            assert!(!snap.items.contains_key(&space_drone), "Deployed drone in space destroyed");
            let origin_chan = format!("solarsystemid2:{sys_id}");
            assert!(snap.chat_channels.get(&origin_chan).is_some_and(|c| c.members.contains(&char2)), "Origin Local chat restored");
        });

        // Verify Triglavian Extraction SubNode (49662) rolls limited loot pool and may be empty (~35% chance)
        let mut empty_seed = None;
        let mut full_seed = None;
        for s in 1_u64..200 {
            ctx.set_mechanic_rng_seed(Some(s));
            let _ = ctx.mechanic_rng.roll(0.5); // consumed by roll_abyssal_weather_penalty_pct
            let _ = ctx.mechanic_rng.try_unit_f64(); // consumed by sample_abyssal_room_hazards
            let u = ctx.mechanic_rng.try_unit_f64().unwrap();
            if u < 0.35 && empty_seed.is_none() { empty_seed = Some(s); }
            if u >= 0.35 && full_seed.is_none() { full_seed = Some(s); }
            if empty_seed.is_some() && full_seed.is_some() { break; }
        }
        for (idx, (seed, expect_loot)) in [(empty_seed.unwrap(), false), (full_seed.unwrap(), true)].into_iter().enumerate() {
            let pos = DVec3::new((idx as f64 + 1.0) * 8_000_000.0, 2.0e9, 0.0);
            move_pilot_to_system(&ctx, &mut sess1, char1, ship1, 621, sys_id, pos).await;
            ctx.set_mechanic_rng_seed(Some(seed));
            let fiid = 730_500_000 + idx as i64;
            seed_cargo_item(&ctx, fiid, FIL_T1_ELEC, char1, ship1, 1);
            let d_sub = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fiid)], &kw, None).await.expect("dep");
            let t_sub = d_sub.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
            let e_sub = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(t_sub)], &kw, None).await.expect("ent");
            let pid_sub = e_sub.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();
            let sub_id = 1_896_000_000 + pid_sub * 10 + 1 + 500_000;
            ctx.db.with_snapshot(|snap| {
                assert_eq!(snap.items[&sub_id].type_id, 49_662, "Extraction SubNode spawned in T1");
                let child_count = snap.items.values().filter(|i| i.location_id == sub_id).count();
                if expect_loot {
                    assert!(child_count > 0, "Non-empty subnode roll must contain limited loot items");
                } else {
                    assert_eq!(child_count, 0, "Empty subnode roll (~35%) must contain 0 items");
                }
            });
            let _ = evacuate_abyssal_pockets_for_downtime(&ctx);
        }
    }

    // NOTE: `feat:abyssal.automated-pylons` stays intentionally OPEN in docs/BLOCKERS.md (§7) due to
    // V24 SDE type 47438 maxRange=40km vs auditor 25km spec; runtime pylon mechanics are verified below.
    #[tokio::test]
    async fn test_abyssal_pylons_caches_hazard_clouds_and_weather_matrix() {
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let kw = HashMap::new();
        let abyss_sys = 32_000_005_u32;
        let sys_h = ctx.systems.get_or_spawn(abyss_sys);

        // 1. Automated Pylons (V24 SDE): Suppressors (47437=15km, 47438=40km, 48254=40km explosive-only fallback)
        // and Multibody Tracking Pylons (47469=15km 1.8x, 47470=40km 1.6x; buff player ships, Sentry Drones, and NPC ships)
        assert_eq!(suppressor_radius_m(Some(&ctx.sde), AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID, 0.0), 15_000.0);
        assert_eq!(suppressor_radius_m(Some(&ctx.sde), AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID, 0.0), 40_000.0);
        assert_eq!(suppressor_radius_m(Some(&ctx.sde), AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID, 0.0), 40_000.0);
        for fb_dmg in [
            suppressor_pulse_damage(None, AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID),
            suppressor_pulse_damage(Some(&ctx.sde), AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID),
        ] {
            assert_eq!(fb_dmg, DamageVector { em: 0.0, thermal: 0.0, kinetic: 0.0, explosive: 90.0 }, "Suppressor fallback must be explosive-only (zero thermal)");
        }
        assert_eq!(tracking_pylon_radius_and_mult(Some(&ctx.sde), MULTIBODY_TRACKING_PYLON_TYPE_ID, 0.0), (15_000.0, 1.8));
        assert_eq!(tracking_pylon_radius_and_mult(Some(&ctx.sde), MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID, 0.0), (40_000.0, 1.6));

        let ship_id = 740_000_001_i64;
        let sentry_id = 740_000_002_i64;
        let drone_id = 740_000_003_i64;
        let missile_id = 740_000_004_i64;
        let fighter_id = 740_000_005_i64;
        let rogue_npc_id = 740_000_006_i64;
        let trig_npc_id = 740_000_007_i64;
        let tessera_id = 740_000_008_i64;

        for (bid, tid, pos, max_vel) in [
            (ship_id, 621_u32, DVec3::new(10_000.0, 0.0, 0.0), 250.0),
            (sentry_id, 23_559, DVec3::new(10_500.0, 0.0, 0.0), 0.0), // Warden I Sentry Drone (group 100)
            (drone_id, 2456, DVec3::new(35_000.0, 0.0, 0.0), 400.0), // Hobgoblin I at 35km
            (missile_id, 204, DVec3::new(35_000.0, 100.0, 0.0), 2_000.0), // Inferno Cruise Missile at 35km
            (fighter_id, 23_057, DVec3::new(35_000.0, 200.0, 0.0), 800.0), // Dragonfly I Fighter at 35km
            (rogue_npc_id, 47_847, DVec3::new(35_000.0, 300.0, 0.0), 500.0), // Sparkneedle Tessella (group 1997) at 35km
            (trig_npc_id, 48_092, DVec3::new(12_000.0, 0.0, 0.0), 400.0), // Striking Damavik NPC (id >= 700M) at 12km
            (tessera_id, 48_262, DVec3::new(10_000.0, 200.0, 0.0), 300.0), // Blastlance Tessera cruiser spared at 10km
        ] {
            let mut cs = ctx.build_type_combat_state(tid);
            cs.shield_hp = 5_000.0;
            cs.max_shield_hp = 5_000.0;
            cs.structure_hp = 5_000.0;
            cs.max_structure_hp = 5_000.0;
            let _ = sys_h.spawn_ball_with_combat(Ball {
                id: bid, type_id: tid, position: pos, velocity: DVec3::ZERO, max_velocity: max_vel,
                inertia: 1.0, mass: 1_000.0, radius: 25.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            }, cs).await;
        }

        let _ = sys_h.spawn_ball(Ball {
            id: 740_010_001, type_id: AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID, position: DVec3::ZERO, velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: 15_000.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;
        let _ = pulse_abyssal_automated_pylons(&ctx, abyss_sys).await;
        assert_eq!(sys_h.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp, 5_000.0, "Small suppressor (15km) spares drone at 35km");
        assert_eq!(sys_h.get_combat_state(sentry_id).await.unwrap().unwrap().shield_hp, 5_000.0, "Suppressor spares stationary Sentry Drone at 10.5km");
        assert_eq!(sys_h.get_combat_state(tessera_id).await.unwrap().unwrap().shield_hp, 5_000.0, "Suppressor spares Tessera cruiser at 10km");
        assert_eq!(sys_h.get_combat_state(trig_npc_id).await.unwrap().unwrap().shield_hp, 5_000.0, "Suppressor spares Triglavian NPC at 12km");
        assert_eq!(sys_h.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp, 5_000.0, "Suppressor spares player ship at 10km");
        sys_h.try_remove_ball(740_010_001);

        let _ = sys_h.spawn_ball(Ball {
            id: 740_010_002, type_id: AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID, position: DVec3::ZERO, velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: 40_000.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;
        let _ = pulse_abyssal_automated_pylons(&ctx, abyss_sys).await;
        assert!(sys_h.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp < 5_000.0, "Medium suppressor (SDE 40km) damages drone at 35km");
        assert!(sys_h.get_combat_state(missile_id).await.unwrap().unwrap().shield_hp < 5_000.0, "Medium suppressor damages missile at 35km");
        assert!(sys_h.get_combat_state(fighter_id).await.unwrap().unwrap().shield_hp < 5_000.0, "Medium suppressor damages fighter at 35km");
        assert!(sys_h.get_combat_state(rogue_npc_id).await.unwrap().unwrap().shield_hp < 5_000.0, "Medium suppressor damages rogue drone NPC at 35km");
        assert_eq!(sys_h.get_combat_state(sentry_id).await.unwrap().unwrap().shield_hp, 5_000.0, "Medium suppressor still spares Sentry Drone");

        let _ = sys_h.set_ball_position(drone_id, DVec3::new(42_000.0, 0.0, 0.0)).await;
        let hp_42 = sys_h.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp;
        let _ = pulse_abyssal_automated_pylons(&ctx, abyss_sys).await;
        assert_eq!(sys_h.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp, hp_42, "Medium suppressor (40km) spares drone at 42km");
        sys_h.try_remove_ball(740_010_002);

        let _ = sys_h.set_ball_position(drone_id, DVec3::new(38_000.0, 0.0, 0.0)).await;
        { let mut bp = sys_h.ballpark.write().unwrap(); let cs = bp.combat_states.get_mut(&drone_id).unwrap(); cs.shield_resonance = [0.0, 0.0, 0.0, 1.0]; }
        let _ = sys_h.spawn_ball(Ball {
            id: 740_010_003, type_id: AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID, position: DVec3::ZERO, velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: 40_000.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;
        let _ = pulse_abyssal_automated_pylons(&ctx, abyss_sys).await;
        assert!((hp_42 - sys_h.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp - 90.0).abs() < 1e-3, "Large suppressor (48254) deals 90.0 pure explosive damage (0 thermal)");
        sys_h.try_remove_ball(740_010_003);

        // Medium-Range Tracking Pylon (47470, 40km, +60% -> 1.6x) & Short-Range Tracking Pylon (47469, 15km, +80% -> 1.8x)
        let _ = sys_h.spawn_ball(Ball {
            id: 740_010_004, type_id: MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID, position: DVec3::ZERO, velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: 40_000.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;
        let _ = pulse_abyssal_automated_pylons(&ctx, abyss_sys).await;
        assert!((sys_h.get_combat_state(ship_id).await.unwrap().unwrap().tracking_speed_multiplier - 1.6).abs() < 1e-6);
        assert!((sys_h.get_combat_state(sentry_id).await.unwrap().unwrap().tracking_speed_multiplier - 1.6).abs() < 1e-6);
        assert!((sys_h.get_combat_state(trig_npc_id).await.unwrap().unwrap().tracking_speed_multiplier - 1.6).abs() < 1e-6, "NPC ship receives 47470 tracking buff");
        let _ = sys_h.spawn_ball(Ball {
            id: 740_010_005, type_id: MULTIBODY_TRACKING_PYLON_TYPE_ID, position: DVec3::ZERO, velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: 15_000.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;
        let _ = pulse_abyssal_automated_pylons(&ctx, abyss_sys).await;
        assert!((sys_h.get_combat_state(ship_id).await.unwrap().unwrap().tracking_speed_multiplier - 1.8).abs() < 1e-6);
        assert!((sys_h.get_combat_state(trig_npc_id).await.unwrap().unwrap().tracking_speed_multiplier - 1.8).abs() < 1e-6, "NPC ship receives 47469 1.8x tracking buff");

        // 2. Bioadaptive Cache (47951) vs Extraction Subnode (49662): T0 has NO Extraction Subnode; T1+ spawns both & 48120 wreck on destroy
        let char1 = 90_000_001_i64;
        let c_ship = 640_000_020_i64;
        let mut sess1 = login_pilot_in_space(&ctx, 887_301, "demo", "demo-secret", char1, c_ship, 621, SYS_09_JITA, DVec3::new(0.0, 2.0e9, 0.0)).await;
        seed_cargo_item(&ctx, 740_100_010, FIL_T0_ELEC, char1, c_ship, 1);
        let d0 = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(740_100_010)], &kw, None).await.expect("dep T0");
        let t0 = d0.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
        let e0 = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(t0)], &kw, None).await.expect("ent T0");
        let pid0 = e0.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();
        let subnode0_id = 1_896_000_000 + pid0 * 10 + 1 + 500_000;
        assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&subnode0_id)), "T0 Tranquil pocket must NOT spawn Extraction Subnode");
        for r in 0..3 {
            crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, char1, r).await;
            let gid = 1_895_000_000 + pid0 * 10 + (r as i64 + 1);
            if r < 2 {
                ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalGateActivation", &[PyRep::Integer(gid)], &kw, None).await.expect("g");
            } else {
                ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEndGateActivation", &[PyRep::Integer(gid)], &kw, None).await.expect("exit");
            }
        }

        move_pilot_to_system(&ctx, &mut sess1, char1, c_ship, 621, SYS_08_LASHESIH, DVec3::new(5_000_000.0, 2.0e9, 0.0)).await;
        seed_cargo_item(&ctx, 740_100_011, FIL_T3_ELEC, char1, c_ship, 1);
        let d3 = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(740_100_011)], &kw, None).await.expect("dep T3");
        let t3 = d3.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
        ctx.set_mechanic_rng_seed(Some(4));
        let e3 = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(t3)], &kw, None).await.expect("ent T3");
        let pid3 = e3.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();
        let p3_sys = e3.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
        let cache3_id = 1_896_000_000 + pid3 * 10 + 1;
        let subnode3_id = cache3_id + 500_000;
        ctx.db.with_snapshot(|s| {
            assert_eq!(s.items[&cache3_id].type_id, 47_951, "Bioadaptive Cache type 47951");
            assert_eq!(s.items[&subnode3_id].type_id, 49_662, "Extraction Subnode type 49662");
        });
        // BF-04 & R-ABYSS-PYLON-02: Room 1 spawns a seeded subset (<8) of clouds+pylons, and Suppressor kill unlocks conduit
        let p3_h = ctx.systems.get_or_spawn(p3_sys);
        let r1_haz_cnt = {
            let bp = p3_h.ballpark.read().unwrap();
            (1..=3_i64).filter(|c| bp.balls.contains_key(&(1_898_000_000 + pid3 * 100 + 10 + c))).count()
                + (1..=5_i64).filter(|py| bp.balls.contains_key(&(1_899_000_000 + pid3 * 100 + 10 + py))).count()
        };
        assert!((2..=4).contains(&r1_haz_cnt), "Room spawns a varied subset of clouds+pylons ({r1_haz_cnt}), not all 8");
        let r1_npcs = with_pve_special_reg(&ctx, |reg| reg.abyssal_pockets[&char1].rooms[0].npc_ball_ids.clone());
        for &nid in &r1_npcs[1..] {
            let _ = p3_h.apply_damage_with_attacker(nid, DamageVector { em: 1e7, thermal: 1e7, kinetic: 1e7, explosive: 1e7 }, Some(char1)).await;
        }
        let last_npc = r1_npcs[0];
        let sup_pos = p3_h.get_ball_snapshot(1_899_000_000 + pid3 * 100 + 11).await.unwrap().unwrap().1.position;
        {
            let mut bp = p3_h.ballpark.write().unwrap();
            if let Some(b) = bp.balls.get_mut(&last_npc) { b.type_id = 47_847; b.position = sup_pos; }
            if let Some(cs) = bp.combat_states.get_mut(&last_npc) { cs.shield_hp = 0.0; cs.armor_hp = 0.0; cs.structure_hp = 10.0; }
        }
        let _ = pulse_abyssal_automated_pylons(&ctx, p3_sys).await;
        assert!(with_pve_special_reg(&ctx, |reg| reg.abyssal_pockets[&char1].rooms[0].cleared), "Suppressor kill unlocks room conduit");
        assert!(!eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| rt.npc_entities.contains_key(&last_npc)), "Suppressor kill cleans npc_entities");

        eve_server::services::pve_special_sites_abyssal::on_abyssal_cache_destroyed_core(&ctx.systems, &ctx.db, p3_sys, cache3_id, Some(char1)).await;
        ctx.db.with_snapshot(|s| {
            let wreck = s.items.values().find(|i| i.type_id == 48_120 && i.location_id == p3_sys as i64).expect("Triglavian Bioadaptive Wreck 48120 spawned");
            assert!(s.items.values().any(|i| i.location_id == wreck.item_id && i.quantity >= 4), "Loot transferred to wreck");
        });
        let _ = evacuate_abyssal_pockets_for_downtime(&ctx);

        // 3. Spatial hazard clouds (Bioluminescence, Filament Small/Medium/Large, Tachyon): SDE radii, numeric modifiers & exit restoration
        assert_eq!(spatial_cloud_radius_m(Some(&ctx.sde), BIOLUMINESCENT_CLOUD_SMALL_TYPE_ID, 0.0), 5_000.0);
        assert_eq!(spatial_cloud_radius_m(Some(&ctx.sde), BIOLUMINESCENT_CLOUD_TYPE_ID, 0.0), 15_000.0);
        assert_eq!(spatial_cloud_radius_m(Some(&ctx.sde), BIOLUMINESCENT_CLOUD_LARGE_TYPE_ID, 0.0), 40_000.0);
        assert_eq!(spatial_cloud_radius_m(Some(&ctx.sde), FILAMENT_CLOUD_SMALL_TYPE_ID, 0.0), 5_000.0);
        assert_eq!(spatial_cloud_radius_m(Some(&ctx.sde), FILAMENT_CLOUD_TYPE_ID, 0.0), 15_000.0);
        assert_eq!(spatial_cloud_radius_m(Some(&ctx.sde), FILAMENT_CLOUD_LARGE_TYPE_ID, 0.0), 40_000.0);
        assert_eq!(spatial_cloud_radius_m(Some(&ctx.sde), PROVING_TACHYON_CLOUD_TYPE_ID, 0.0), 40_000.0);

        let (default_vel, default_in) = ctx.sde.types.get(&621).map(|t| (t.default_attributes.get(&37).copied().unwrap_or(250.0), t.default_attributes.get(&70).copied().unwrap_or(0.5))).unwrap_or((250.0, 0.5));
        for (cid, ctid, cx) in [(740_020_001, BIOLUMINESCENT_CLOUD_TYPE_ID, 0.0), (740_020_002, FILAMENT_CLOUD_TYPE_ID, 50_000.0), (740_020_003, TACHYON_CLOUD_TYPE_ID, -50_000.0)] {
            let _ = sys_h.spawn_ball(Ball { id: cid, type_id: ctid, position: DVec3::new(cx, 50_000.0, 0.0), velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: SPATIAL_CLOUD_RADIUS_M, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
        }

        let _ = sys_h.set_ball_position(ship_id, DVec3::new(0.0, 50_000.0, 0.0)).await;
        assert_eq!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, abyss_sys, ship_id).await, Some("BioluminescentCloud"));
        assert!((sys_h.get_combat_state(ship_id).await.unwrap().unwrap().signature_radius - 500.0).abs() < 1e-3);
        assert!((sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.max_velocity - default_vel).abs() < 1e-3);

        let _ = sys_h.set_ball_position(ship_id, DVec3::new(50_000.0, 50_000.0, 0.0)).await;
        assert_eq!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, abyss_sys, ship_id).await, Some("FilamentCloud"));
        let fil_cs = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!((fil_cs.shield_boost_multiplier - 0.6).abs() < 1e-6);
        assert!((fil_cs.signature_radius - 125.0).abs() < 1e-3);

        let _ = sys_h.set_ball_position(ship_id, DVec3::new(-50_000.0, 50_000.0, 0.0)).await;
        assert_eq!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, abyss_sys, ship_id).await, Some("TachyonCloud"));
        let (_, tach_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        assert!((tach_ball.max_velocity - default_vel * 4.0).abs() < 1e-3);
        assert!((tach_ball.inertia - default_in * 0.5).abs() < 1e-3);
        assert!((sys_h.get_combat_state(ship_id).await.unwrap().unwrap().signature_radius - 500.0).abs() < 1e-3);

        let _ = sys_h.set_ball_position(ship_id, DVec3::new(0.0, 0.0, 0.0)).await;
        assert!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, abyss_sys, ship_id).await.is_none());
        let (_, exit_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        let exit_cs = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!((exit_ball.max_velocity - default_vel).abs() < 1e-3);
        assert!((exit_ball.inertia - default_in).abs() < 1e-3);
        assert!((exit_cs.signature_radius - 125.0).abs() < 1e-3);
        assert!((exit_cs.shield_boost_multiplier - 1.0).abs() < 1e-6);

        // 4. Weather penalty RNG distribution (30%/50% for T0..=T3, 50%/70% for T4..=T6) & all 5 weather types across player & NPC ships
        let mut low_seed = None;
        let mut high_seed = None;
        for s in 1_u64..100 {
            ctx.set_mechanic_rng_seed(Some(s));
            if ctx.mechanic_rng.roll(0.5) {
                if high_seed.is_none() { high_seed = Some(s); }
            } else if low_seed.is_none() {
                low_seed = Some(s);
            }
            if low_seed.is_some() && high_seed.is_some() { break; }
        }
        let (low_seed, high_seed) = (low_seed.unwrap(), high_seed.unwrap());
        ctx.set_mechanic_rng_seed(Some(low_seed));
        assert_eq!(roll_abyssal_weather_penalty_pct(&ctx, 1), 30.0);
        ctx.set_mechanic_rng_seed(Some(high_seed));
        assert_eq!(roll_abyssal_weather_penalty_pct(&ctx, 1), 50.0);
        ctx.set_mechanic_rng_seed(Some(low_seed));
        assert_eq!(roll_abyssal_weather_penalty_pct(&ctx, 4), 50.0);
        ctx.set_mechanic_rng_seed(Some(high_seed));
        assert_eq!(roll_abyssal_weather_penalty_pct(&ctx, 4), 70.0);

        let base_ship_cs = ctx.build_ship_combat_state(char1, c_ship, 621);
        let fitted_base_vel = eve_server::services::pve_abyssal_pylons::build_fitted_ship_space_ball(&ctx, char1, c_ship, 621, DVec3::ZERO).max_velocity;
        let base_opt = ctx.sde.types.get(&621).and_then(|t| t.default_attributes.get(&54).copied()).unwrap_or(15_000.0);
        let base_scan = ctx.sde.types.get(&621).and_then(|t| t.default_attributes.get(&564).copied()).unwrap_or(250.0);
        let weather_cases = [
            (47_765_u32, AbyssalWeatherKind::Electrical, low_seed, 30.0_f64),
            (47_762, AbyssalWeatherKind::Dark, high_seed, 50.0),
            (47_761, AbyssalWeatherKind::Exotic, low_seed, 30.0),
            (47_763, AbyssalWeatherKind::Firestorm, high_seed, 50.0),
            (47_764, AbyssalWeatherKind::Gamma, low_seed, 30.0),
            (47_906, AbyssalWeatherKind::Electrical, high_seed, 70.0),
            (47_894, AbyssalWeatherKind::Dark, high_seed, 70.0),
            (47_890, AbyssalWeatherKind::Exotic, high_seed, 70.0),
            (47_898, AbyssalWeatherKind::Firestorm, high_seed, 70.0),
            (47_902, AbyssalWeatherKind::Gamma, high_seed, 70.0),
        ];
        for (idx, (fil_tid, weather, seed, exp_pen)) in weather_cases.into_iter().enumerate() {
            move_pilot_to_system(&ctx, &mut sess1, char1, c_ship, 621, SYS_LOW_TAMA, DVec3::new((idx as f64 + 1.0) * 6_000_000.0, 2.0e9, 0.0)).await;
            ctx.set_mechanic_rng_seed(Some(seed));
            let fiid = 740_200_000 + idx as i64;
            seed_cargo_item(&ctx, fiid, fil_tid, char1, c_ship, 1);
            let d_w = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fiid)], &kw, None).await.expect("dep w");
            let t_w = d_w.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
            let e_w = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(t_w)], &kw, None).await.expect("ent w");
            let w_sys = e_w.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
            let w_h = ctx.systems.get_or_spawn(w_sys);
            let npc_id = with_pve_special_reg(&ctx, |reg| {
                let p = &reg.abyssal_pockets[&char1];
                assert_eq!(p.weather, weather);
                assert_eq!(p.weather_penalty_pct, exp_pen);
                p.rooms[0].npc_ball_ids[0]
            });
            let (_, s_ball) = w_h.get_ball_snapshot(c_ship).await.unwrap().unwrap();
            let s_cs = w_h.get_combat_state(c_ship).await.unwrap().unwrap();
            let (_, n_ball) = w_h.get_ball_snapshot(npc_id).await.unwrap().unwrap();
            let n_cs = w_h.get_combat_state(npc_id).await.unwrap().unwrap();
            let n_base_cs = ctx.build_type_combat_state(n_ball.type_id);
            let n_base_vel = ctx.sde.types.get(&n_ball.type_id).and_then(|t| t.default_attributes.get(&37).copied()).filter(|&v| v > 0.0).unwrap_or(380.0);
            let pen = exp_pen / 100.0;
            let dyn_attrs = ctx.db.with_snapshot(|s| s.items[&c_ship].dynamic_attributes.clone());
            let pen_res = |r: f64| (r * (1.0 + pen)).clamp(0.0, 1.0);
            match weather {
                AbyssalWeatherKind::Electrical => {
                    assert!((s_cs.shield_resonance[0] - pen_res(base_ship_cs.shield_resonance[0])).abs() < 1e-4);
                    assert!((s_cs.armor_resonance[0] - pen_res(base_ship_cs.armor_resonance[0])).abs() < 1e-4);
                    assert!((s_cs.structure_resonance[0] - pen_res(base_ship_cs.structure_resonance[0])).abs() < 1e-4);
                    assert!((s_cs.capacitor_recharge_time_ms - (base_ship_cs.capacitor_recharge_time_ms * 0.5).max(1_000.0)).abs() < 1.0);
                    assert!((n_cs.armor_resonance[0] - pen_res(n_base_cs.armor_resonance[0])).abs() < 1e-4);
                }
                AbyssalWeatherKind::Dark => {
                    assert!((s_ball.max_velocity - fitted_base_vel * 1.5).abs() < 1e-3);
                    assert!((dyn_attrs[&54] - base_opt * (1.0 - pen)).abs() < 1.0);
                    assert!((n_ball.max_velocity - n_base_vel * 1.5).abs() < 1e-3);
                }
                AbyssalWeatherKind::Exotic => {
                    assert!((s_cs.shield_resonance[2] - pen_res(base_ship_cs.shield_resonance[2])).abs() < 1e-4);
                    assert!((s_cs.armor_resonance[2] - pen_res(base_ship_cs.armor_resonance[2])).abs() < 1e-4);
                    assert!((s_cs.structure_resonance[2] - pen_res(base_ship_cs.structure_resonance[2])).abs() < 1e-4);
                    assert!((dyn_attrs[&564] - base_scan * 1.5).abs() < 1e-3);
                    assert!((n_cs.armor_resonance[2] - pen_res(n_base_cs.armor_resonance[2])).abs() < 1e-4);
                }
                AbyssalWeatherKind::Firestorm => {
                    assert!((s_cs.max_armor_hp - base_ship_cs.max_armor_hp * 1.5).abs() < 1.0);
                    assert!((s_cs.armor_resonance[1] - pen_res(base_ship_cs.armor_resonance[1])).abs() < 1e-4);
                    assert!((s_cs.structure_resonance[1] - pen_res(base_ship_cs.structure_resonance[1])).abs() < 1e-4);
                    assert!((n_cs.armor_resonance[1] - pen_res(n_base_cs.armor_resonance[1])).abs() < 1e-4);
                }
                AbyssalWeatherKind::Gamma => {
                    assert!((s_cs.max_shield_hp - base_ship_cs.max_shield_hp * 1.5).abs() < 1.0);
                    assert!((s_cs.shield_resonance[3] - pen_res(base_ship_cs.shield_resonance[3])).abs() < 1e-4);
                    assert!((s_cs.structure_resonance[3] - pen_res(base_ship_cs.structure_resonance[3])).abs() < 1e-4);
                    assert!((n_cs.shield_resonance[3] - pen_res(n_base_cs.shield_resonance[3])).abs() < 1e-4);
                }
            }
            let _ = evacuate_abyssal_pockets_for_downtime(&ctx);
            assert!(ctx.db.with_snapshot(|s| s.items[&c_ship].dynamic_attributes.is_empty()), "Ship attributes restored on exit");
        }

        // BF-03: Live Gunnery turret range penalty in Dark weather (-50%: ~44.9km skilled -> ~22.4km, so 30km fails in Dark and succeeds after exit)
        move_pilot_to_system(&ctx, &mut sess1, char1, c_ship, 624, SYS_LOW_TAMA, DVec3::new(90_000_000.0, 2.0e9, 0.0)).await;
        let (turret_id, ammo_id) = (740_300_001_i64, 740_300_002_i64);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(turret_id, InventoryItemRecord { item_id: turret_id, type_id: 487, owner_id: char1, location_id: c_ship, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 1, ..Default::default() });
            snap.items.insert(ammo_id, InventoryItemRecord { item_id: ammo_id, type_id: 182, owner_id: char1, location_id: c_ship, flag: InventoryFlag::HiSlot0, quantity: 40, singleton: false, version: 1, ..Default::default() });
            snap.module_online_states.insert(turret_id, true);
        });
        ctx.invalidate_ship_fitting(c_ship);
        ctx.set_mechanic_rng_seed(Some(low_seed)); // T5 Dark -> -50% penalty
        seed_cargo_item(&ctx, 740_300_003, 47_894, char1, c_ship, 1);
        let d_dk = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(740_300_003)], &kw, None).await.expect("dep dark");
        let e_dk = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(d_dk.dict_get("traceBallID").unwrap().as_i64().unwrap())], &kw, None).await.expect("ent dark");
        let dk_sys = e_dk.dict_get("solarSystemID").unwrap().as_i64().unwrap() as u32;
        let dk_npc = e_dk.dict_get("npcBallIDs").and_then(|v| if let PyRep::List(l) = v { l[0].as_i64() } else { None }).unwrap();
        let dk_h = ctx.systems.get_or_spawn(dk_sys);
        let _ = dk_h.set_ball_position(c_ship, DVec3::ZERO).await;
        let _ = dk_h.set_ball_position(dk_npc, DVec3::new(30_000.0, 0.0, 0.0)).await;
        let _ = dk_h.add_target_lock(c_ship, eve_destiny::TargetLock { target_id: dk_npc, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await;
        let oor = eve_server::services::combat_crimewatch::dispatch_combat_rpc(&ctx, &mut sess1, "dogmaIM", "Activate", &[PyRep::Integer(turret_id), PyRep::String("projectileFired".into()), PyRep::Integer(dk_npc)], &kw).await.unwrap_err();
        assert_eq!(oor.code, "TargetOutOfRange", "Dark weather -50% turret optimal/falloff blocks shot at 30km");
        let _ = ctx.dispatch_rpc(&mut sess1, "dogmaIM", None, "Activate", &[PyRep::Integer(turret_id), PyRep::String("projectileFired".into()), PyRep::Integer(dk_npc)], &kw, None).await;
        let _ = dk_h.try_step_full_tick(1.0);
        assert!(!dk_h.has_module_cycle(c_ship, turret_id).await, "Module cycle auto-deactivates beyond Dark-penalized range");
        let _ = evacuate_abyssal_pockets_for_downtime(&ctx);
        assert!(ctx.db.with_snapshot(|s| s.items[&turret_id].dynamic_attributes.is_empty()), "Turret dynamic_attributes restored on exit");
        move_pilot_to_system(&ctx, &mut sess1, char1, c_ship, 624, SYS_LOW_TAMA, DVec3::ZERO).await;
        let tama_h = ctx.systems.get_or_spawn(SYS_LOW_TAMA);
        let dummy_t = 740_300_099_i64;
        let _ = tama_h.spawn_ball_with_combat(Ball { id: dummy_t, type_id: 48_090, position: DVec3::new(30_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx.build_type_combat_state(48_090)).await;
        let _ = tama_h.add_target_lock(c_ship, eve_destiny::TargetLock { target_id: dummy_t, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await;
        eve_server::services::combat_crimewatch::dispatch_combat_rpc(&ctx, &mut sess1, "dogmaIM", "Activate", &[PyRep::Integer(turret_id), PyRep::String("projectileFired".into()), PyRep::Integer(dummy_t)], &kw).await.expect("Restored turret hits 30km outside Dark weather");
    }
}

pub(crate) mod abyssal_frigate_destroyer_fleet_only {
    // Continuation of Abyssal Deadspace closure tests:
    // - Fleet-only Frigate/Destroyer activation and bound `fleet_id` enforcement (`BF-01`)
    // - Downtime evacuation purge of abandoned and orphaned drones in Abyssal pockets (`NBF-02`)
    // - Monotonic `WorldRepository::instance_id` isolation in the ballpark registry (`NBF-01`)


    use crate::abyssal_deadspace::{
        login_pilot_in_space, move_pilot_to_system, seed_cargo_item, FIL_T1_ELEC, SYS_08_LASHESIH,
    };
    use crate::common;
    use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
    use eve_destiny::{Ball, MovementMode};
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime};
    use eve_server::services::pve_special_sites::with_pve_special_reg;
    use eve_server::system_actor::try_remove_ball_for_db;
    use glam::DVec3;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_abyssal_frigate_destroyer_fleet_only_and_bound_fleet_id() {
        eve_testkit::attest(&[
            "feat:pve.abyssal-entry-ship-limits",
            "feat:pve.abyssal-coop-frigate-destroyer",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let kw = HashMap::new();
        let sys_id = SYS_08_LASHESIH;

        let (char1, char2) = (90_000_001_i64, 90_000_002_i64);
        let (ship1, ship2) = (640_000_001_i64, 640_000_002_i64);
        let base = DVec3::new(20_000_000.0, 2.0e9, 0.0);
        let mut sess1 = login_pilot_in_space(&ctx, 888_101, "demo", "demo-secret", char1, ship1, 587, sys_id, base).await;
        let mut sess2 = login_pilot_in_space(&ctx, 888_102, "other", "other-secret", char2, ship2, 587, sys_id, base + DVec3::new(300.0, 0.0, 0.0)).await;

        // 1. Solo fleetless Frigate (587) rejected on both AbyssalEntranceDeployment and direct AbyssalEntranceGateActivation
        seed_cargo_item(&ctx, 740_100_001, FIL_T1_ELEC, char1, ship1, 3);
        let err_dep_fg = ctx
            .dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(740_100_001)], &kw, None)
            .await
            .unwrap_err();
        assert_eq!(err_dep_fg.code, "NotInFleet");
        let err_act_fg = ctx
            .dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(740_100_001)], &kw, None)
            .await
            .unwrap_err();
        assert_eq!(err_act_fg.code, "NotInFleet");
        assert_eq!(ctx.db.with_snapshot(|s| s.items[&740_100_001].quantity), 3, "Filaments preserved on NotInFleet");

        // 2. Solo fleetless Destroyer (16236) rejected on both deployment and direct inventory activation
        move_pilot_to_system(&ctx, &mut sess1, char1, ship1, 16_236, sys_id, base).await;
        seed_cargo_item(&ctx, 740_100_002, FIL_T1_ELEC, char1, ship1, 2);
        let err_dep_dd = ctx
            .dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(740_100_002)], &kw, None)
            .await
            .unwrap_err();
        assert_eq!(err_dep_dd.code, "NotInFleet");
        let err_act_dd = ctx
            .dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(740_100_002)], &kw, None)
            .await
            .unwrap_err();
        assert_eq!(err_act_dd.code, "NotInFleet");
        assert_eq!(ctx.db.with_snapshot(|s| s.items[&740_100_002].quantity), 2);

        // 3. Solo fleetless Cruiser (621) IS allowed without a fleet (maxShips == 1)
        move_pilot_to_system(&ctx, &mut sess1, char1, ship1, 621, sys_id, base).await;
        seed_cargo_item(&ctx, 740_100_003, FIL_T1_ELEC, char1, ship1, 1);
        let dep_cr = ctx
            .dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(740_100_003)], &kw, None)
            .await
            .expect("Solo Cruiser deployment allowed without fleet");
        assert_eq!(dep_cr.dict_get("maxShips"), Some(&PyRep::Integer(1)));

        // 4. Opener in fleet 99_301 deploys Frigate trace (bound fleet_id == Some(99_301)):
        //    - If opener leaves fleet before activating trace -> NotInFleet
        //    - If opener switches to a different fleet 99_302 -> AccessDenied
        //    - Fleetmate in bound fleet 99_301 can still enter even while opener is outside 99_301
        //    - Opener rejoining 99_301 enters the same pocket
        move_pilot_to_system(&ctx, &mut sess1, char1, ship1, 587, sys_id, base + DVec3::new(2_000_000.0, 0.0, 0.0)).await;
        move_pilot_to_system(&ctx, &mut sess2, char2, ship2, 587, sys_id, base + DVec3::new(2_000_300.0, 0.0, 0.0)).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.fleets.insert(
                99_301,
                FleetRecord { fleet_id: 99_301, boss_id: char1, members: vec![char1, char2], ..Default::default() },
            );
        });
        let dep_fg = ctx
            .dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(740_100_001)], &kw, None)
            .await
            .expect("Fleet member Frigate deploy");
        let tr_id = dep_fg.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
        assert_eq!(
            with_pve_special_reg(&ctx, |reg| reg.abyssal_traces.get(&tr_id).and_then(|t| t.fleet_id)),
            Some(99_301)
        );

        ctx.db.with_mut_snapshot(|snap| {
            snap.fleets.get_mut(&99_301).unwrap().members.retain(|&m| m != char1);
        });
        let err_opener_left = ctx
            .dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr_id)], &kw, None)
            .await
            .unwrap_err();
        assert_eq!(err_opener_left.code, "NotInFleet", "Opener who left fleet cannot activate Frigate trace");

        ctx.db.with_mut_snapshot(|snap| {
            snap.fleets.insert(
                99_302,
                FleetRecord { fleet_id: 99_302, boss_id: char1, members: vec![char1], ..Default::default() },
            );
        });
        let err_opener_other_fleet = ctx
            .dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr_id)], &kw, None)
            .await
            .unwrap_err();
        assert_eq!(err_opener_other_fleet.code, "AccessDenied", "Opener in a different fleet cannot activate bound trace");

        let ent2 = ctx
            .dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr_id)], &kw, None)
            .await
            .expect("Member of bound fleet 99_301 can enter even after opener left");
        let pid = ent2.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();

        ctx.db.with_mut_snapshot(|snap| {
            snap.fleets.remove(&99_302);
            snap.fleets.get_mut(&99_301).unwrap().members.push(char1);
        });
        let ent1 = ctx
            .dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr_id)], &kw, None)
            .await
            .expect("Opener rejoining bound fleet 99_301 enters same pocket");
        assert_eq!(ent1.dict_get("pocketID").and_then(|v| v.as_i64()), Some(pid));
    }

    #[tokio::test]
    async fn test_abyssal_downtime_evacuation_purges_abandoned_and_orphaned_drones() {
        eve_testkit::attest(&["feat:pve.abyssal-in-pocket-restrictions"]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let sec_init = ctx.now_unix() % 86_400;
        let target_pre_dt = 11 * 3_600 - 300;
        ctx.advance_virtual_clock(if sec_init <= target_pre_dt { target_pre_dt - sec_init } else { 86_400 - sec_init + target_pre_dt });

        let kw = HashMap::new();
        let sys_id = SYS_08_LASHESIH;
        let (char1, ship1) = (90_000_001_i64, 641_000_001_i64);
        let mut sess1 = login_pilot_in_space(&ctx, 888_201, "demo", "demo-secret", char1, ship1, 621, sys_id, DVec3::new(0.0, 2.0e9, 0.0)).await;

        let bay_drone_id = 741_500_001_i64;
        let orphan_drone_id = 741_500_002_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                bay_drone_id,
                InventoryItemRecord {
                    item_id: bay_drone_id, type_id: 2454, owner_id: char1, location_id: ship1,
                    flag: InventoryFlag::DroneBay, quantity: 1, singleton: true, version: 1, ..Default::default()
                },
            );
        });
        seed_cargo_item(&ctx, 741_500_010, FIL_T1_ELEC, char1, ship1, 1);
        let ent = ctx
            .dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(741_500_010)], &kw, None)
            .await
            .expect("Enter Abyssal pocket");
        let p_sys = ent.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
        let p_h = ctx.systems.get_or_spawn(p_sys);

        // Launch drone and abandon it (`owner_char_id = 0, parent_ship_id = 0, original_owner_char_id = char1`)
        ctx.dispatch_rpc(&mut sess1, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(bay_drone_id)])], &kw, None)
            .await
            .expect("LaunchDrones");
        let abandoned_ball_id = with_combat_runtime(&ctx, |rt| {
            rt.active_drones_in_space.keys().copied().next().expect("launched drone ball")
        });
        ctx.dispatch_rpc(&mut sess1, "entity", None, "CmdAbandonDrone", &[PyRep::List(vec![PyRep::Integer(abandoned_ball_id)])], &kw, None)
            .await
            .expect("CmdAbandonDrone");
        with_combat_runtime(&ctx, |rt| {
            let d = &rt.active_drones_in_space[&abandoned_ball_id];
            assert!(d.is_abandoned);
            assert_eq!(d.owner_char_id, 0);
            assert_eq!(d.parent_ship_id, 0);
            assert_eq!(d.original_owner_char_id, char1);
        });

        // Also place an orphaned drone item & ball (`owner_id = 0`) in `p_sys`
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                orphan_drone_id,
                InventoryItemRecord {
                    item_id: orphan_drone_id, type_id: 2454, owner_id: 0, location_id: p_sys as i64,
                    flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
                },
            );
            snap.item_positions.insert(orphan_drone_id, [100.0, 0.0, 0.0]);
        });
        let _ = p_h
            .spawn_ball(Ball {
                id: orphan_drone_id, type_id: 2454, position: DVec3::new(100.0, 0.0, 0.0),
                velocity: DVec3::ZERO, max_velocity: 400.0, inertia: 1.0, mass: 1.0, radius: 10.0,
                speed_fraction: 0.0, mode: MovementMode::Stop,
            })
            .await;

        ctx.advance_virtual_clock(360);
        let dt_res = eve_server::services::wallet_serverops::run_daily_downtime_and_jobs(&ctx);
        assert_eq!(dt_res.get("is1100UtcWindow"), Some(&1));
        assert!(dt_res.get("evacuatedAbyssalPilots").copied().unwrap_or(0) >= 1);

        with_combat_runtime(&ctx, |rt| {
            assert!(!rt.active_drones_in_space.contains_key(&abandoned_ball_id), "Abandoned drone purged from active_drones_in_space");
        });
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.characters[&char1].solar_system_id, sys_id);
            assert!(!snap.items.contains_key(&abandoned_ball_id), "Abandoned drone item purged");
            assert!(!snap.items.contains_key(&bay_drone_id), "Launched drone item purged");
            assert!(!snap.items.contains_key(&orphan_drone_id), "Orphaned drone item in abyss_sys purged");
        });
        assert!(p_h.get_ball_snapshot(abandoned_ball_id).await.unwrap().is_none(), "Abandoned drone ball removed");
        assert!(p_h.get_ball_snapshot(orphan_drone_id).await.unwrap().is_none(), "Orphaned drone ball removed");
        with_mut_combat_runtime(&ctx, |rt| rt.active_drones_in_space.clear());
    }

    #[tokio::test]
    async fn test_world_repository_monotonic_instance_id_and_ballpark_isolation() {
        let r1 = Arc::new(WorldRepository::with_demo_seed());
        let r2 = Arc::new(WorldRepository::new_empty_with_snapshot_path(None).unwrap());
        let tmp = std::env::temp_dir().join(format!("evomu_repo_id_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let r3 = Arc::new(WorldRepository::open_sqlite(&tmp).unwrap());
        let _ = std::fs::remove_file(&tmp);

        assert!(r1.instance_id() > 0);
        assert!(r2.instance_id() > r1.instance_id());
        assert!(r3.instance_id() > r2.instance_id());

        let ctx_a = ServerContext::with_repository(r1.clone());
        let ctx_b = ServerContext::with_repository(r2.clone());
        let sys_id = SYS_08_LASHESIH;
        let ball_id = 742_900_001_i64;
        let h_a = ctx_a.systems.get_or_spawn(sys_id);
        let h_b = ctx_b.systems.get_or_spawn(sys_id);
        let ball = Ball {
            id: ball_id, type_id: 47_458, position: DVec3::ZERO, velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        };
        let _ = h_a.spawn_ball(ball.clone()).await;
        let _ = h_b.spawn_ball(ball).await;

        try_remove_ball_for_db(&r1, sys_id, ball_id);
        assert!(h_a.get_ball_snapshot(ball_id).await.unwrap().is_none(), "Ball removed in r1 ballpark");
        assert!(h_b.get_ball_snapshot(ball_id).await.unwrap().is_some(), "Ball in r2 ballpark unaffected");
    }
}
