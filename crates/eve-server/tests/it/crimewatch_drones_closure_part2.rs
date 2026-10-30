//! End-to-end integration tests closing Crimewatch movement, Drones, Fighters, and Corporation Structure rows:
//! - `feat:crimewatch.weapons-timer-dock-jump-gate`
//! - `feat:lifecycle.crimewatch-highsec-criminal-movement`
//! - `feat:drones.bandwidth-and-bay-constraints`
//! - `feat:drones.sentry-drone-stationary-tracking`
//! - `feat:drones.drone-npc-aggro-switching`
//! - `feat:drones.carrier-launch-tubes-and-flight-control`
//! - `feat:corporation.corp-structure-ownership`

use crate::crimewatch_drones_closure::{fit_online, lock_target, new_ctx, rpc, seed_item, spawn_pilot};
use eve_db::{CorporationRecord, InventoryFlag, TrainedSkillRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use eve_server::services::combat_crimewatch::{spawn_specialized_npc_entity, with_combat_runtime};
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_crimewatch_weapons_timer_and_highsec_criminal_movement_restrictions() {
    // parity: feat:crimewatch.weapons-timer-dock-jump-gate, feat:lifecycle.crimewatch-highsec-criminal-movement
    eve_testkit::attest(&[
        "feat:crimewatch.weapons-timer-dock-jump-gate",
        "feat:lifecycle.crimewatch-highsec-criminal-movement",
    ]);

    let ctx = new_ctx();
    let jita = 30_000_142u32;
    let lowsec = 30_002_537u32;

    // 1. Verify turret, missile launcher, EWAR/neut, smartbomb, bomb launcher, and drone `CmdEngage` each initiate a 60s Weapons Timer
    {
        let nullsec = 30_004_759u32;
        let (a_id, b_id, sa_id, sb_id) = (95_000_001, 95_000_002, 85_000_001, 85_000_002);
        let mut sa_sess = spawn_pilot(&ctx, 901, a_id, 1_000_009, sa_id, 626, nullsec, DVec3::ZERO).await;
        let _sb_sess = spawn_pilot(&ctx, 902, b_id, 1_000_010, sb_id, 587, nullsec, DVec3::new(1_000.0, 0.0, 0.0)).await;
        fit_online(&ctx, a_id, sa_id, 85_000_101, 486, InventoryFlag::HiSlot0, Some((85_000_102, 185, 50)));
        fit_online(&ctx, a_id, sa_id, 85_000_103, 501, InventoryFlag::HiSlot1, Some((85_000_104, 209, 50)));
        fit_online(&ctx, a_id, sa_id, 85_000_105, 447, InventoryFlag::MedSlot0, None);
        fit_online(&ctx, a_id, sa_id, 85_000_106, 3953, InventoryFlag::HiSlot2, None);
        fit_online(&ctx, a_id, sa_id, 85_000_107, 27914, InventoryFlag::HiSlot3, Some((85_000_108, 27920, 5)));
        seed_item(&ctx, 85_000_109, 2454, a_id, sa_id, InventoryFlag::DroneBay, 1, true);
        lock_target(&ctx, &mut sa_sess, nullsec, sb_id).await;
        rpc(&ctx, &mut sa_sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(85_000_109)])]).await.unwrap();
        let drone_ball = with_combat_runtime(&ctx, |rt| rt.active_drones_in_space.keys().copied().next().unwrap());

        for (label, svc, method, args) in [
            ("turret", "dogmaIM", "Activate", vec![PyRep::Integer(85_000_101), PyRep::Integer(sb_id)]),
            ("missile", "dogmaIM", "Activate", vec![PyRep::Integer(85_000_103), PyRep::Integer(sb_id)]),
            ("ewar", "dogmaIM", "Activate", vec![PyRep::Integer(85_000_105), PyRep::Integer(sb_id)]),
            ("smartbomb", "dogmaIM", "Activate", vec![PyRep::Integer(85_000_106)]),
            ("bomb", "dogmaIM", "Activate", vec![PyRep::Integer(85_000_107)]),
            ("drone", "entity", "CmdEngage", vec![PyRep::List(vec![PyRep::Integer(drone_ball)]), PyRep::Integer(sb_id)]),
        ] {
            ctx.advance_virtual_clock(61); // Let previous 60s Weapons Timer expire naturally
            let now = ctx.now_unix();
            rpc(&ctx, &mut sa_sess, svc, None, method, &args).await.unwrap_or_else(|e| panic!("{label} failed: {e:?}"));
            let wt = ctx.crimewatch.read().unwrap().states[&a_id].weapon_timer_until_unix;
            assert_eq!(wt, now + 60, "{label} must initiate a 60-second Weapons Timer");
        }

        let (npc_hunter_c, npc_hunter_s) = (95_000_003, 85_000_003);
        let mut hunter_sess = spawn_pilot(&ctx, 903, npc_hunter_c, 1_000_009, npc_hunter_s, 626, nullsec, DVec3::ZERO).await;
        fit_online(&ctx, npc_hunter_c, npc_hunter_s, 85_000_120, 486, InventoryFlag::HiSlot0, Some((85_000_121, 185, 50)));
        fit_online(&ctx, npc_hunter_c, npc_hunter_s, 85_000_122, 501, InventoryFlag::HiSlot1, Some((85_000_123, 209, 50)));
        let npc_bid = spawn_specialized_npc_entity(&ctx, nullsec, 10004, "Pirate", 1, DVec3::new(2_000.0, 0.0, 0.0)).await;
        lock_target(&ctx, &mut hunter_sess, nullsec, npc_bid).await;
        rpc(&ctx, &mut hunter_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(85_000_120), PyRep::Integer(npc_bid)]).await.unwrap();
        rpc(&ctx, &mut hunter_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(85_000_122), PyRep::Integer(npc_bid)]).await.unwrap();
        let wt_npc = ctx.crimewatch.read().unwrap().states.get(&npc_hunter_c).map(|s| s.weapon_timer_until_unix).unwrap_or(0);
        assert_eq!(wt_npc, 0, "Attacking an NPC with turrets or missiles must NOT initiate a Weapons Timer");
    }

    // 2a. High-sec Criminal Timer (acquired organically via pod EWAR in low-sec Agoze 30_003_787 -> stargate 50_004_205 -> Intaki 30_003_788):
    //     non-capsule hull blocked from warp, stargate, wormhole, jump drive/bridge/portal, station/structure dock, eject, and board!
    let (agoze, intaki, agoze_gate, intaki_gate, intaki_station) = (30_003_787u32, 30_003_788u32, 50_004_205i64, 50_004_430i64, 60_009_385i64);
    let agoze_gate_pos = DVec3::new(-2_769_606_942_720.0, 116_952_145_920.0, 2_253_235_445_760.0);
    let (c_id, pod_vic_id, hs_vic_id) = (95_000_010, 95_000_011, 95_000_012);
    let (ship_id, pod_vic_s, hs_vic_s, spare_ship_id, intaki_struct) = (85_000_010, 85_000_011, 85_000_012, 85_000_013, 1_040_000_000_950i64);
    let mut sess = spawn_pilot(&ctx, 910, c_id, 1_000_009, ship_id, 587, agoze, agoze_gate_pos).await;
    let _pod_vic = spawn_pilot(&ctx, 911, pod_vic_id, 1_000_010, pod_vic_s, 670, agoze, agoze_gate_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    fit_online(&ctx, c_id, ship_id, 85_000_201, 447, InventoryFlag::MedSlot0, None);
    fit_online(&ctx, c_id, ship_id, 85_000_202, 486, InventoryFlag::HiSlot0, Some((85_000_203, 185, 50)));
    lock_target(&ctx, &mut sess, agoze, pod_vic_s).await;
    rpc(&ctx, &mut sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    rpc(&ctx, &mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(85_000_201), PyRep::Integer(pod_vic_s)]).await.unwrap();
    ctx.advance_virtual_clock(61); // 60s Weapons Timer expires naturally; 839s of live Criminal Timer remains
    let agoze_b = ctx.bind_object(&sess, "beyonce", agoze as i64).unwrap();
    rpc(&ctx, &mut sess, "beyonce", Some(agoze_b), "CmdStargateJump", &[PyRep::Integer(agoze_gate)]).await.expect("Low-sec criminal jumps into Intaki");
    assert_eq!(sess.solar_system_id, Some(intaki));
    ctx.advance_virtual_clock(12); // Clear 10s post-jump session change timer while 827s of Criminal Timer remains

    let _ = rpc(&ctx, &mut sess, "scanMgr", None, "GetSystemScanMgr", &[]).await.unwrap();
    let (intaki_wh_sig, intaki_wh_pos) = ctx.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures[&intaki].iter().find(|s| s.sig_group == "Wormhole").unwrap();
        (sig.signature_id.clone(), sig.position)
    });
    let intaki_wh_id = eve_server::services::space_destiny::wormhole_item_id_for_signature(intaki, &intaki_wh_sig);
    let intaki_pos = DVec3::new(intaki_wh_pos[0] + 400.0, intaki_wh_pos[1], intaki_wh_pos[2]);
    let intaki_h = ctx.systems.get_or_spawn(intaki);
    let _ = intaki_h.set_ball_position(ship_id, intaki_pos).await;
    seed_item(&ctx, spare_ship_id, 587, c_id, intaki as i64, InventoryFlag::None, 1, true);
    let _ = intaki_h.spawn_ball_with_combat(
        Ball { id: spare_ship_id, type_id: 587, position: intaki_pos + DVec3::new(300.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 2.0, mass: 1_200_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        ctx.build_type_combat_state(587),
    ).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(intaki_struct, UpwellStructureRecord {
            structure_id: intaki_struct, type_id: 35832, solar_system_id: intaki, owner_corp_id: 1_000_009,
            name: "Intaki Astrahus".into(), position: intaki_pos.to_array(), state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 100, shield_hp: 100_000.0, armor_hp: 100_000.0, hull_hp: 100_000.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
    });

    let intaki_b = ctx.bind_object(&sess, "beyonce", intaki as i64).unwrap();
    assert_eq!(rpc(&ctx, &mut sess, "beyonce", Some(intaki_b), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(intaki_station), PyRep::Real(0.0)]).await.unwrap_err().code, "CantWarpAwayGlobalCriminalFlag");
    assert_eq!(rpc(&ctx, &mut sess, "beyonce", Some(intaki_b), "CmdStargateJump", &[PyRep::Integer(intaki_gate)]).await.unwrap_err().code, "CantJumpGlobalCriminalFlag");
    assert_eq!(rpc(&ctx, &mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(intaki_wh_id)]).await.unwrap_err().code, "CantEnterWormholeGlobalCriminalFlag");
    assert_eq!(rpc(&ctx, &mut sess, "beyonce", Some(intaki_b), "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(c_id), PyRep::Integer(lowsec as i64), PyRep::Integer(1)]).await.unwrap_err().code, "CantBeaconJumpGlobalCriminalFlag");
    assert_eq!(rpc(&ctx, &mut sess, "beyonce", Some(intaki_b), "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(c_id), PyRep::Integer(lowsec as i64), PyRep::Integer(1)]).await.unwrap_err().code, "CantBeaconJumpGlobalCriminalFlag");
    assert_eq!(rpc(&ctx, &mut sess, "beyonce", Some(intaki_b), "CmdJumpThroughFleet", &[PyRep::Integer(c_id), PyRep::Integer(ship_id)]).await.unwrap_err().code, "CantBeaconJumpGlobalCriminalFlag");
    assert_eq!(rpc(&ctx, &mut sess, "stationSvc", None, "Dock", &[PyRep::Integer(intaki_station)]).await.unwrap_err().code, "CantDockGlobalCriminalFlag");
    assert_eq!(rpc(&ctx, &mut sess, "structureDocking", None, "Dock", &[PyRep::Integer(intaki_struct)]).await.unwrap_err().code, "CantDockGlobalCriminalFlag");
    assert_eq!(rpc(&ctx, &mut sess, "ship", None, "Eject", &[]).await.unwrap_err().code, "CannotEjectInvolvedInCrime");
    assert_eq!(rpc(&ctx, &mut sess, "ship", None, "Board", &[PyRep::Integer(spare_ship_id)]).await.unwrap_err().code, "CannotBoardShipInHighSecAsCriminal");

    // CONCORD executes `ship_id` in Intaki (`0.6 -> 14s`) via `tick_background_cluster`, placing `c_id` into a real Capsule (`670`)!
    for _ in 0..61 {
        let _ = intaki_h.step_manual(1.0).await;
    }
    let _hs_vic = spawn_pilot(&ctx, 912, hs_vic_id, 1_000_010, hs_vic_s, 587, intaki, intaki_pos + DVec3::new(800.0, 0.0, 0.0)).await;
    lock_target(&ctx, &mut sess, intaki, hs_vic_s).await;
    rpc(&ctx, &mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(85_000_202), PyRep::Integer(hs_vic_s)]).await.unwrap();
    ctx.advance_virtual_clock(14);
    tick_background_cluster(&ctx).await;
    let pod_id = ctx.db.get_character(c_id).unwrap().active_ship_id;
    assert_eq!(ctx.db.get_item(pod_id).unwrap().type_id, 670);
    sess.ship_id = Some(pod_id);
    ctx.advance_virtual_clock(47); // Let 60s Weapons Timer expire while >800s of Criminal Timer remains on the real Capsule (`670`)
    assert!(ctx.crimewatch.read().unwrap().states[&c_id].is_criminal(ctx.now_unix()));
    assert_eq!(rpc(&ctx, &mut sess, "ship", None, "Board", &[PyRep::Integer(spare_ship_id)]).await.unwrap_err().code, "CannotBoardShipInHighSecAsCriminal");
    rpc(&ctx, &mut sess, "beyonce", Some(intaki_b), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(intaki_station), PyRep::Real(0.0)]).await.expect("Criminal capsule CAN warp in high-sec");

    // 2b. Non-criminal 60s Weapons Timer (acquired organically via live duel turret fire in Jita):
    let (w_id, d_id, ws_id, ds_id, w_spare, jita_struct) = (95_000_020, 95_000_021, 85_000_020, 85_000_021, 85_000_022, 1_040_000_000_951i64);
    let mut w_sess = spawn_pilot(&ctx, 920, w_id, 1_000_009, ws_id, 587, jita, DVec3::ZERO).await;
    let mut d_sess = spawn_pilot(&ctx, 921, d_id, 1_000_010, ds_id, 587, jita, DVec3::ZERO).await;
    let _ = rpc(&ctx, &mut w_sess, "scanMgr", None, "GetSystemScanMgr", &[]).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let sigs = snap.cosmic_signatures.entry(jita).or_default();
        if !sigs.iter().any(|s| s.sig_group == "Wormhole") {
            sigs.push(eve_db::CosmicSignatureRecord {
                signature_id: "JMM-142".into(),
                solar_system_id: jita,
                sig_group: "Wormhole".into(),
                type_name: "Unstable Wormhole (K162)".into(),
                position: [149_597_870_700.0, 0.0, 0.0],
                signal_strength: 1.0,
                wormhole_type_id: Some(26_272),
                destination_system_id: Some(31_000_005),
                created_unix: ctx.now_unix(),
                lifetime_seconds: 86_400,
                max_mass_kg: 2_000_000_000.0,
                remaining_mass_kg: 2_000_000_000.0,
                max_jump_mass_kg: 375_000_000.0,
                collapsed: false,
            });
        }
    });
    let (wh_sig, wh_pos) = ctx.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures[&jita].iter().find(|s| s.sig_group == "Wormhole").unwrap();
        (sig.signature_id.clone(), sig.position)
    });
    let wh_item_id = eve_server::services::space_destiny::wormhole_item_id_for_signature(jita, &wh_sig);
    let jita_gate = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == jita).cloned().unwrap();
    let station_id = 60_003_760i64;
    let pos = DVec3::new(wh_pos[0] + 400.0, wh_pos[1], wh_pos[2]);
    let sys_h = ctx.systems.get_or_spawn(jita);
    let _ = sys_h.set_ball_position(ws_id, pos).await;
    let _ = sys_h.set_ball_position(ds_id, pos + DVec3::new(800.0, 0.0, 0.0)).await;
    fit_online(&ctx, w_id, ws_id, 85_000_301, 486, InventoryFlag::HiSlot0, Some((85_000_302, 185, 50)));
    lock_target(&ctx, &mut w_sess, jita, ds_id).await;
    seed_item(&ctx, w_spare, 587, w_id, jita as i64, InventoryFlag::None, 1, true);
    let _ = sys_h.spawn_ball_with_combat(
        Ball { id: w_spare, type_id: 587, position: pos + DVec3::new(300.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 2.0, mass: 1_200_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        ctx.build_type_combat_state(587),
    ).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(jita_struct, UpwellStructureRecord {
            structure_id: jita_struct, type_id: 35832, solar_system_id: jita, owner_corp_id: 1_000_009,
            name: "Gatehouse Astrahus".into(), position: pos.to_array(), state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 100, shield_hp: 100_000.0, armor_hp: 100_000.0, hull_hp: 100_000.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
    });

    rpc(&ctx, &mut w_sess, "crimewatch", None, "StartDuelChallenge", &[PyRep::Integer(d_id)]).await.unwrap();
    rpc(&ctx, &mut d_sess, "crimewatch", None, "RespondToDuelChallenge", &[PyRep::Integer(w_id), PyRep::Bool(true)]).await.unwrap();
    rpc(&ctx, &mut w_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(85_000_301), PyRep::Integer(ds_id)]).await.unwrap();

    let beyonce = ctx.bind_object(&w_sess, "beyonce", jita as i64).unwrap();
    assert_eq!(rpc(&ctx, &mut w_sess, "stationSvc", None, "Dock", &[PyRep::Integer(station_id)]).await.unwrap_err().code, "WeaponsTimerActive");
    assert_eq!(rpc(&ctx, &mut w_sess, "structureDocking", None, "Dock", &[PyRep::Integer(jita_struct)]).await.unwrap_err().code, "WeaponsTimerActive");
    assert_eq!(rpc(&ctx, &mut w_sess, "beyonce", Some(beyonce), "CmdStargateJump", &[PyRep::Integer(jita_gate.stargate_id)]).await.unwrap_err().code, "CannotJumpWithWeaponsTimer");
    assert_eq!(rpc(&ctx, &mut w_sess, "beyonce", Some(beyonce), "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(w_id), PyRep::Integer(lowsec as i64), PyRep::Integer(1)]).await.unwrap_err().code, "CannotJumpWithWeaponsTimer");
    assert_eq!(rpc(&ctx, &mut w_sess, "beyonce", Some(beyonce), "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(w_id), PyRep::Integer(lowsec as i64), PyRep::Integer(1)]).await.unwrap_err().code, "CannotJumpWithWeaponsTimer");
    assert_eq!(rpc(&ctx, &mut w_sess, "beyonce", Some(beyonce), "CmdJumpThroughFleet", &[PyRep::Integer(w_id), PyRep::Integer(ws_id)]).await.unwrap_err().code, "CannotJumpWithWeaponsTimer");
    assert_eq!(rpc(&ctx, &mut w_sess, "structureCynoBeaconMgr", None, "CmdJumpToStructureBeacon", &[PyRep::Integer(lowsec as i64), PyRep::Integer(jita_struct)]).await.unwrap_err().code, "CannotJumpWithWeaponsTimer");
    assert_eq!(rpc(&ctx, &mut w_sess, "ship", None, "Eject", &[]).await.unwrap_err().code, "CannotEjectActiveWeaponsTimer");
    assert_eq!(rpc(&ctx, &mut w_sess, "ship", None, "Board", &[PyRep::Integer(w_spare)]).await.unwrap_err().code, "CannotStoreShipActiveWeaponsTimer");

    // Once 60s Weapons Timer expires, docking succeeds; undock, re-trigger 60s Weapons Timer via live turret fire, and verify WormholeJump
    ctx.advance_virtual_clock(61);
    rpc(&ctx, &mut w_sess, "structureDocking", None, "Dock", &[PyRep::Integer(jita_struct)]).await.expect("Dock succeeds after 60s Weapons Timer expires");
    ctx.advance_virtual_clock(12);
    rpc(&ctx, &mut w_sess, "structureDocking", None, "Undock", &[]).await.expect("Undock");
    let beyonce = ctx.bind_object(&w_sess, "beyonce", jita as i64).unwrap();
    rpc(&ctx, &mut w_sess, "beyonce", Some(beyonce), "CmdStop", &[]).await.expect("Stop after undock");
    let _ = sys_h.set_ball_position(ws_id, pos).await;
    lock_target(&ctx, &mut w_sess, jita, ds_id).await;
    rpc(&ctx, &mut w_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(85_000_301), PyRep::Integer(ds_id)]).await.unwrap();

    // Per live EVE V24.01 ground truth (`eve-wiki/articles/Wormholes.md:68`: "Weapon timers do not affect
    // travel through wormholes"), `feat:navigation.wormhole-aggression-exemption`, and `reviewer_brief.md`
    // ground-truth precedence, a non-criminal 60s Weapons Timer permits `wormholeMgr.WormholeJump`
    // (whereas high-sec Criminal Timer above rejected `WormholeJump` with `CantEnterWormholeGlobalCriminalFlag`).
    rpc(&ctx, &mut w_sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)]).await.expect("WormholeJump allowed under non-criminal Weapons Timer");
    assert_eq!(w_sess.solar_system_id, Some(31_000_005));
}

#[tokio::test]
async fn test_drones_bandwidth_sentry_and_npc_aggro_switching_end_to_end() {
    // parity: feat:drones.bandwidth-and-bay-constraints
    // parity: feat:drones.sentry-drone-stationary-tracking
    // parity: feat:drones.drone-npc-aggro-switching
    eve_testkit::attest(&[
        "feat:drones.bandwidth-and-bay-constraints",
        "feat:drones.sentry-drone-stationary-tracking",
        "feat:drones.drone-npc-aggro-switching",
    ]);

    let ctx = new_ctx();
    let sys_id = 30_000_142u32;
    let c1 = 96_000_001i64;
    let vexor_id = 86_000_001i64;
    let mut sess = spawn_pilot(&ctx, 1001, c1, 1_000_009, vexor_id, 626, sys_id, DVec3::ZERO).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);

    // 1. Bandwidth, bay capacity (125 m3 on Vexor), and 5-drone skill cap
    let (ogre_1, ogre_2, ogre_3, garde_id, extra_ogre, cargo_ogre) = (86_000_010, 86_000_011, 86_000_012, 86_000_013, 86_000_014, 86_000_015);
    for id in [ogre_1, ogre_2, ogre_3, extra_ogre] {
        seed_item(&ctx, id, 2444, c1, vexor_id, InventoryFlag::DroneBay, 1, true);
    }
    seed_item(&ctx, garde_id, 23561, c1, vexor_id, InventoryFlag::DroneBay, 1, true);
    seed_item(&ctx, cargo_ogre, 2444, c1, vexor_id, InventoryFlag::Cargo, 1, true);
    let inv_bound = ctx.bind_object(&sess, "invbroker", vexor_id).unwrap();
    assert_eq!(rpc(&ctx, &mut sess, "invbroker", Some(inv_bound), "Add", &[PyRep::Integer(cargo_ogre), PyRep::Integer(vexor_id), PyRep::Integer(87)]).await.unwrap_err().code, "CapacityExceeded");

    ctx.db.with_mut_snapshot(|s| { s.skills.entry(c1).or_default().remove(&3436); });
    ctx.invalidate_ship_fitting(vexor_id);
    assert_eq!(rpc(&ctx, &mut sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(ogre_1)])]).await.unwrap_err().code, "NoDroneManagementAbilities");

    ctx.db.with_mut_snapshot(|s| { s.skills.entry(c1).or_default().insert(3436, TrainedSkillRecord { type_id: 3436, level: 1, skill_points: 250.0 }); });
    ctx.invalidate_ship_fitting(vexor_id);
    assert_eq!(rpc(&ctx, &mut sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(ogre_1), PyRep::Integer(ogre_2)])]).await.unwrap_err().code, "NoDroneManagementAbilitiesLeft");

    ctx.db.with_mut_snapshot(|s| { s.skills.entry(c1).or_default().insert(3436, TrainedSkillRecord { type_id: 3436, level: 5, skill_points: 256_000.0 }); });
    ctx.invalidate_ship_fitting(vexor_id);
    rpc(&ctx, &mut sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(garde_id)])]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(ogre_1), PyRep::Integer(ogre_2), PyRep::Integer(ogre_3)])]).await.unwrap_err().code, "MaxBandwidthExceeded");

    // 2. Sentry drone stationary tracking (`Garde I` 23561 stays at 0 m/s; >2500m scoop/return rejected)
    let garde_ball = with_combat_runtime(&ctx, |rt| rt.active_drones_in_space.values().find(|d| d.type_id == 23561).unwrap().ball_id);
    let (_, garde_ball_snap) = sys_h.get_ball_snapshot(garde_ball).await.unwrap().unwrap();
    assert_eq!(garde_ball_snap.max_velocity, 0.0, "Sentry drone must have max_velocity == 0");

    let _ = rpc(&ctx, &mut sess, "entity", None, "CmdReturnAndOrbit", &[PyRep::List(vec![PyRep::Integer(garde_ball)])]).await.expect("CmdReturnAndOrbit on sentry drone succeeds");
    let (_, garde_after_snap) = sys_h.get_ball_snapshot(garde_ball).await.unwrap().unwrap();
    assert_eq!(garde_after_snap.max_velocity, 0.0, "Sentry drone remains stationary at 0 m/s");

    // Sentry drone optimal, falloff, and tracking turret formula hit-chance evaluation
    let (hit_opt, hit_falloff, hit_transversal) = with_combat_runtime(&ctx, |rt| {
        let d = rt.active_drones_in_space.get(&garde_ball).unwrap();
        let d_ball = Ball { id: garde_ball, type_id: 23561, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1000.0, radius: 10.0, speed_fraction: 0.0, mode: MovementMode::Stop };
        let falloff = d.effective_falloff_range(&ctx);
        let t_opt = Ball { id: 10004, type_id: 10004, position: DVec3::new(d.optimal_range, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop };
        let t_falloff = Ball { id: 10004, type_id: 10004, position: DVec3::new(d.optimal_range + falloff, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop };
        let t_trans = Ball { id: 10004, type_id: 10004, position: DVec3::new(d.optimal_range, 0.0, 0.0), velocity: DVec3::new(0.0, 5000.0, 0.0), max_velocity: 5000.0, inertia: 1.0, mass: 1000.0, radius: 25.0, speed_fraction: 1.0, mode: MovementMode::Stop };
        (
            d.compute_combat_hit_multiplier(&ctx, &d_ball, &t_opt, 50.0),
            d.compute_combat_hit_multiplier(&ctx, &d_ball, &t_falloff, 50.0),
            d.compute_combat_hit_multiplier(&ctx, &d_ball, &t_trans, 25.0),
        )
    });
    assert!((hit_opt - 1.0).abs() < 1e-3, "Stationary target at optimal has 1.0 hit chance");
    assert!((hit_falloff - 0.5).abs() < 0.05, "Target at optimal + falloff has ~0.5 hit chance");
    assert!(hit_transversal < 0.25, "High transversal tracking penalty drops hit chance");

    let _ = sys_h.set_ball_position(vexor_id, DVec3::new(4_000.0, 0.0, 0.0)).await;
    assert_eq!(rpc(&ctx, &mut sess, "entity", None, "CmdReturnBay", &[PyRep::List(vec![PyRep::Integer(garde_ball)])]).await.unwrap_err().code, "SentryDroneOutOfScoopRange");
    let _ = sys_h.set_ball_position(vexor_id, DVec3::ZERO).await;
    rpc(&ctx, &mut sess, "ship", None, "ScoopToDroneBay", &[PyRep::Integer(garde_ball)]).await.unwrap();

    // 5-drone cap check at Drones V: 6x Hobgoblin I (30 Mbit/s <= 75 Mbit/s) rejected with NoDroneManagementAbilitiesLeft
    ctx.db.with_mut_snapshot(|s| { for id in [ogre_1, ogre_2, ogre_3, extra_ogre, garde_id] { s.items.remove(&id); } });
    let hobs: Vec<i64> = (0..6).map(|i| 86_000_100 + i).collect();
    for &hid in &hobs {
        seed_item(&ctx, hid, 2454, c1, vexor_id, InventoryFlag::DroneBay, 1, true);
    }
    assert_eq!(
        rpc(&ctx, &mut sess, "ship", None, "LaunchDrones", &[PyRep::List(hobs.iter().map(|&id| PyRep::Integer(id)).collect())]).await.unwrap_err().code,
        "NoDroneManagementAbilitiesLeft"
    );

    // 3a. Frigate (`593` Tristan, sig_radius 36m -> base threat ~109.0, 25 Mbit/s bandwidth) vs idle drone (~58.0): idle drone NEVER steals aggro from a Frigate!
    let frig_sys = 30_000_144u32; // Perimeter
    let (frig_c, tristan_id, frig_hob_item) = (96_000_050i64, 86_000_050i64, 86_000_051i64);
    let mut frig_sess = spawn_pilot(&ctx, 1002, frig_c, 1_000_009, tristan_id, 593, frig_sys, DVec3::ZERO).await;
    let frig_h = ctx.systems.get_or_spawn(frig_sys);
    seed_item(&ctx, frig_hob_item, 2454, frig_c, tristan_id, InventoryFlag::DroneBay, 1, true);
    let frig_npc = spawn_specialized_npc_entity(&ctx, frig_sys, 10004, "Pirate", 1, DVec3::new(1_200.0, 0.0, 0.0)).await;
    lock_target(&ctx, &mut frig_sess, frig_sys, frig_npc).await;
    ctx.advance_virtual_clock(1);
    tick_background_cluster(&ctx).await;
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.npc_entities[&frig_npc].target_id), Some(tristan_id));

    rpc(&ctx, &mut frig_sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(frig_hob_item)])]).await.unwrap();
    let frig_hob_ball = with_combat_runtime(&ctx, |rt| rt.active_drones_in_space.values().find(|d| d.owner_char_id == frig_c).unwrap().ball_id);
    let _ = frig_h.set_ball_position(frig_hob_ball, DVec3::new(1_000.0, 0.0, 0.0)).await;
    ctx.advance_virtual_clock(1);
    tick_background_cluster(&ctx).await;
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.npc_entities[&frig_npc].target_id), Some(tristan_id), "Idle drone must NOT steal aggro from a Frigate hull");

    rpc(&ctx, &mut frig_sess, "entity", None, "CmdEngage", &[PyRep::List(vec![PyRep::Integer(frig_hob_ball)]), PyRep::Integer(frig_npc)]).await.unwrap();
    ctx.advance_virtual_clock(1);
    tick_background_cluster(&ctx).await;
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.npc_entities[&frig_npc].target_id), Some(frig_hob_ball), "Engaging drone switches NPC aggro off Frigate");
    rpc(&ctx, &mut frig_sess, "entity", None, "CmdReturnBay", &[PyRep::List(vec![PyRep::Integer(frig_hob_ball)])]).await.unwrap();
    ctx.advance_virtual_clock(1);
    tick_background_cluster(&ctx).await;
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.npc_entities[&frig_npc].target_id), Some(tristan_id), "NPC returns aggro to Frigate after drone recall");

    // 3b. Cruiser (`626` Vexor) multi-drone threat ranking (Heavy `Ogre I` 2444 vs Light `Hobgoblin I` 2454) & real NPC damage application
    seed_item(&ctx, ogre_1, 2444, c1, vexor_id, InventoryFlag::DroneBay, 1, true);
    let npc_id = spawn_specialized_npc_entity(&ctx, sys_id, 10004, "Pirate", 1, DVec3::new(1_200.0, 0.0, 0.0)).await;
    lock_target(&ctx, &mut sess, sys_id, npc_id).await;

    ctx.advance_virtual_clock(1);
    tick_background_cluster(&ctx).await;
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.npc_entities[&npc_id].target_id), Some(vexor_id));

    rpc(&ctx, &mut sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(ogre_1), PyRep::Integer(hobs[0])])]).await.unwrap();
    let (ogre_ball, hob_ball) = with_combat_runtime(&ctx, |rt| {
        let ob = rt.active_drones_in_space.values().find(|d| d.type_id == 2444).unwrap().ball_id;
        let hb = rt.active_drones_in_space.values().find(|d| d.type_id == 2454).unwrap().ball_id;
        (ob, hb)
    });
    let _ = sys_h.set_ball_position(ogre_ball, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let _ = sys_h.set_ball_position(hob_ball, DVec3::new(22_000.0, 0.0, 0.0)).await;
    ctx.advance_virtual_clock(1);
    tick_background_cluster(&ctx).await;
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.npc_entities[&npc_id].target_id), Some(vexor_id), "Idle drones must not steal NPC aggro");

    let ogre_cs_before = sys_h.get_combat_state(ogre_ball).await.unwrap().unwrap();
    rpc(&ctx, &mut sess, "entity", None, "CmdEngage", &[PyRep::List(vec![PyRep::Integer(ogre_ball), PyRep::Integer(hob_ball)]), PyRep::Integer(npc_id)]).await.unwrap();
    ctx.advance_virtual_clock(1);
    tick_background_cluster(&ctx).await;
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.npc_entities[&npc_id].target_id), Some(ogre_ball), "NPC must switch aggro onto highest-threat active drone");
    let ogre_cs_after = sys_h.get_combat_state(ogre_ball).await.unwrap().unwrap();
    assert!(ogre_cs_after.shield_hp < ogre_cs_before.shield_hp, "Attacking drone must take real NPC weapon damage");

    let _ = sys_h.set_ball_position(hob_ball, DVec3::new(500.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut sess, "entity", None, "CmdReturnBay", &[PyRep::List(vec![PyRep::Integer(ogre_ball), PyRep::Integer(hob_ball)])]).await.unwrap();
    assert_eq!(ctx.db.get_item(ogre_1).unwrap().flag, InventoryFlag::DroneBay);
    ctx.advance_virtual_clock(1);
    tick_background_cluster(&ctx).await;
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.npc_entities[&npc_id].target_id), Some(vexor_id), "NPC must switch aggro back to player ship when drones recall");
}

#[tokio::test]
async fn test_carrier_supercarrier_launch_tubes_and_flight_control() {
    // parity: feat:drones.carrier-launch-tubes-and-flight-control
    eve_testkit::attest(&["feat:drones.carrier-launch-tubes-and-flight-control"]);

    let ctx = new_ctx();
    let sys_id = 30_002_537u32; // Amamake (low-sec)
    let c1 = 97_000_001i64;
    let vanguard_id = 87_000_001i64;
    let archon_id = 87_000_002i64;
    let aeon_id = 87_000_003i64;

    // 1. 3-tube hull (`Vanguard` 42132: tubes 0..=2), 4-tube Carrier (`Archon` 23757: max 3 light), 5-tube Supercarrier (`Aeon` 23919: 0 support)
    let mut sess = spawn_pilot(&ctx, 1101, c1, 1_000_009, vanguard_id, 42132, sys_id, DVec3::ZERO).await;
    seed_item(&ctx, 87_000_101, 23057, c1, vanguard_id, InventoryFlag::FighterBay, 6, true);
    rpc(&ctx, &mut sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(2), PyRep::Integer(87_000_101)]).await.expect("3-tube hull tube 2 valid");
    assert_eq!(rpc(&ctx, &mut sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(3), PyRep::Integer(87_000_101)]).await.unwrap_err().code, "InvalidFighterTube");

    let mut aeon_sess = spawn_pilot(&ctx, 1102, c1, 1_000_009, aeon_id, 23919, sys_id, DVec3::ZERO).await;
    seed_item(&ctx, 87_000_201, 23057, c1, aeon_id, InventoryFlag::FighterBay, 6, true);
    seed_item(&ctx, 87_000_202, 40346, c1, aeon_id, InventoryFlag::FighterBay, 3, true);
    rpc(&ctx, &mut aeon_sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(4), PyRep::Integer(87_000_201)]).await.expect("Supercarrier tube 4 valid");
    assert_eq!(rpc(&ctx, &mut aeon_sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(5), PyRep::Integer(87_000_201)]).await.unwrap_err().code, "InvalidFighterTube");
    assert_eq!(rpc(&ctx, &mut aeon_sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(87_000_202)]).await.unwrap_err().code, "CannotLoadFighterTubeNoTubesExistForClass");

    let mut archon_sess = spawn_pilot(&ctx, 1103, c1, 1_000_009, archon_id, 23757, sys_id, DVec3::ZERO).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    fit_online(&ctx, c1, archon_id, 87_000_399, 11370, InventoryFlag::HiSlot0, None); // Prototype Cloaking Device I
    seed_item(&ctx, 87_000_301, 23057, c1, archon_id, InventoryFlag::FighterBay, 15, false);
    for i in 1..4 {
        seed_item(&ctx, 87_000_301 + i, 23057, c1, archon_id, InventoryFlag::FighterBay, 6, true);
    }

    assert_eq!(rpc(&ctx, &mut archon_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])]).await.unwrap_err().code, "EmptyFighterTube");
    rpc(&ctx, &mut archon_sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(87_000_301)]).await.unwrap();
    assert_eq!(ctx.db.get_item(87_000_301).unwrap().quantity, 6, "Oversized fighter stack must clamp to maxSquadronSize (6)");
    rpc(&ctx, &mut archon_sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(1), PyRep::Integer(87_000_302)]).await.unwrap();
    rpc(&ctx, &mut archon_sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(2), PyRep::Integer(87_000_303)]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut archon_sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(3), PyRep::Integer(87_000_304)]).await.unwrap_err().code, "CannotLoadFighterTubeNoTubesAvailableForClass");

    // 2. Launch forbidden while cloaked (`dogmaIM.Activate` 11370), tethered (`sync_tether` near friendly Fortizar), or in warp (`CmdWarpToStuff`)
    rpc(&ctx, &mut archon_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(87_000_399)]).await.expect("Activate Cloaking Device");
    assert_eq!(rpc(&ctx, &mut archon_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])]).await.unwrap_err().code, "CannotOpenFighterTubesWhileCloaked");
    rpc(&ctx, &mut archon_sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(87_000_399)]).await.expect("Deactivate Cloaking Device");

    let tether_fort = 1_040_000_000_970i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(tether_fort, UpwellStructureRecord {
            structure_id: tether_fort, type_id: 35833, solar_system_id: sys_id, owner_corp_id: 1_000_009,
            name: "Carrier Fortizar".into(), position: [0.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 100, shield_hp: 100_000.0, armor_hp: 100_000.0, hull_hp: 100_000.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
    });
    assert_eq!(rpc(&ctx, &mut archon_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])]).await.unwrap_err().code, "DeniedTethered");
    assert!(sys_h.get_combat_state(archon_id).await.unwrap().unwrap().is_tethered, "Archon must be actively tethered to friendly Fortizar");

    let ama_beyonce = ctx.bind_object(&archon_sess, "beyonce", sys_id as i64).unwrap();
    rpc(&ctx, &mut archon_sess, "beyonce", Some(ama_beyonce), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(60_002_590), PyRep::Real(0.0)]).await.expect("Archon enters warp");
    assert_eq!(rpc(&ctx, &mut archon_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])]).await.unwrap_err().code, "CannotOpenFighterTubesWhileWarping");
    for _ in 0..120 {
        let _ = sys_h.step_manual(5.0).await;
        if matches!(sys_h.get_ball_snapshot(archon_id).await.unwrap().unwrap().1.mode, MovementMode::Stop) {
            break;
        }
    }
    let (_, archon_landed) = sys_h.get_ball_snapshot(archon_id).await.unwrap().unwrap();
    assert!(matches!(archon_landed.mode, MovementMode::Stop));

    // 3. Launch tube 0, expend a Slot 2 missile charge, suffer 1 fighter unit loss via live hostile Smartbomb (`dogmaIM.Activate`), then recall -> refills to 6 & restocks charges
    let npc_id = spawn_specialized_npc_entity(&ctx, sys_id, 10004, "Pirate", 1, archon_landed.position + DVec3::new(1_000.0, 0.0, 0.0)).await;
    lock_target(&ctx, &mut archon_sess, sys_id, npc_id).await;
    let PyRep::List(launched) = rpc(&ctx, &mut archon_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])]).await.unwrap() else { panic!() };
    let f_ball = launched[0].as_i64().unwrap();
    let ab = rpc(&ctx, &mut archon_sess, "fighterMgr", None, "CmdActivateAbilitySlots", &[PyRep::List(vec![PyRep::Integer(f_ball)]), PyRep::Integer(2), PyRep::Integer(npc_id)]).await.unwrap();
    let max_ch = ab.dict_get("chargeCount").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(ab.dict_get("remainingCharges").and_then(|v| v.as_i64()), Some(max_ch - 1));
    assert!(!ctx.fighter_charge_state.read().unwrap().is_empty());

    let (_, fb_snap) = sys_h.get_ball_snapshot(f_ball).await.unwrap().unwrap();
    let mut sb_sess = spawn_pilot(&ctx, 1104, 97_000_099, 1_000_010, 87_000_099, 24692, sys_id, fb_snap.position + DVec3::new(200.0, 0.0, 0.0)).await;
    fit_online(&ctx, 97_000_099, 87_000_099, 87_000_499, 14550, InventoryFlag::HiSlot0, None);
    rpc(&ctx, &mut sb_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    for _ in 0..6 {
        rpc(&ctx, &mut sb_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(87_000_499)]).await.unwrap();
        ctx.advance_virtual_clock(11);
    }
    assert_eq!(ctx.active_fighters.read().unwrap()[&f_ball].squadron_size, 5, "Live Smartbomb damage must destroy 1 fighter unit (5 remaining)");

    rpc(&ctx, &mut archon_sess, "fighterMgr", None, "RecallFightersToTubes", &[PyRep::List(vec![PyRep::Integer(f_ball)])]).await.unwrap();
    assert_eq!(ctx.fighter_tube_loads.read().unwrap().get(&(archon_id, 0)).copied(), Some(87_000_301), "Squadron must land back in tube 0");
    assert_eq!(ctx.db.get_item(87_000_301).unwrap().quantity, 6, "Recalled squadron must refill lost unit from FighterBay reserve");
    assert!(ctx.fighter_charge_state.read().unwrap().is_empty(), "Recalled squadron must restock ability charges");
    assert_eq!(rpc(&ctx, &mut archon_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])]).await.unwrap_err().code, "TubeRearming");
}

#[tokio::test]
async fn test_corporation_structure_ownership_and_station_manager_roles() {
    // parity: feat:corporation.corp-structure-ownership
    eve_testkit::attest(&["feat:corporation.corp-structure-ownership"]);

    let ctx = new_ctx();
    let sys_id = 30_000_142u32;
    let corp_id = 98_500_001i64;
    let other_corp = 98_500_002i64;
    let (ceo_id, mgr_id, outsider_id) = (98_000_001i64, 98_000_002i64, 98_000_003i64);
    let (ceo_ship, mgr_ship, out_ship) = (88_000_001i64, 88_000_002i64, 88_000_003i64);

    let mut ceo_sess = spawn_pilot(&ctx, 1201, ceo_id, corp_id, ceo_ship, 648, sys_id, DVec3::ZERO).await;
    let mut mgr_sess = spawn_pilot(&ctx, 1202, mgr_id, corp_id, mgr_ship, 648, sys_id, DVec3::new(500.0, 0.0, 0.0)).await;
    let mut out_sess = spawn_pilot(&ctx, 1203, outsider_id, other_corp, out_ship, 648, sys_id, DVec3::new(800.0, 0.0, 0.0)).await;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ceo_roles = HashMap::new();
        ceo_roles.insert(ceo_id, eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER);
        snap.corporations.insert(corp_id, CorporationRecord {
            corporation_id: corp_id, name: "Citadel Owners Corp".into(), ticker: "OWNR".into(), ceo_id,
            members: std::collections::HashSet::from([ceo_id, mgr_id]), member_roles: ceo_roles,
            ..Default::default()
        });
        snap.corporations.insert(other_corp, CorporationRecord {
            corporation_id: other_corp, name: "Outsider Corp".into(), ticker: "OUTS".into(), ceo_id: outsider_id,
            members: std::collections::HashSet::from([outsider_id]),
            ..Default::default()
        });
    });
    ceo_sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER;

    // 1. Anchoring: unprivileged member (`roles == 0`) is denied; CEO anchors Astrahus (`35832`) -> `owner_corp_id == corp_id`
    seed_item(&ctx, 88_000_101, 35832, mgr_id, mgr_ship, InventoryFlag::Cargo, 1, false);
    assert_eq!(rpc(&ctx, &mut mgr_sess, "structureDeployment", None, "Anchor", &[PyRep::Integer(88_000_101), PyRep::String("Denied Citadel".into())]).await.unwrap_err().code, "InsufficientCorpRoles");

    seed_item(&ctx, 88_000_102, 35832, ceo_id, ceo_ship, InventoryFlag::Cargo, 1, false);
    let anchor_rep = rpc(&ctx, &mut ceo_sess, "structureDeployment", None, "Anchor", &[PyRep::Integer(88_000_102), PyRep::String("Corp Astrahus".into())]).await.expect("CEO anchors structure");
    let struct_id = anchor_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&struct_id].owner_corp_id), corp_id, "Corporation must own the anchored structure");

    // Complete anchoring + quantum core onlining so fuel, profiles, and service modules can be managed
    use eve_server::services::station_structure::structure_lifecycle as life;
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS);
    rpc(&ctx, &mut ceo_sess, "structureGuests", None, "GetGuests", &[PyRep::Integer(struct_id)]).await.unwrap();
    rpc(&ctx, &mut ceo_sess, "structureDocking", None, "Dock", &[PyRep::Integer(struct_id)]).await.unwrap();
    let core_tid = ctx.sde.types[&35832].default_attributes[&life::ATTR_REQUIRES_DEED_TYPE] as u32;
    seed_item(&ctx, 88_000_103, core_tid, ceo_id, ceo_ship, InventoryFlag::Cargo, 1, true);
    let ceo_inv = ctx.bind_object(&ceo_sess, "invbroker", struct_id).unwrap();
    rpc(&ctx, &mut ceo_sess, "invbroker", Some(ceo_inv), "Add", &[PyRep::Integer(88_000_103), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::StructureDeedBay as i64)]).await.unwrap();
    rpc(&ctx, &mut ceo_sess, "structureGuests", None, "GetGuests", &[PyRep::Integer(struct_id)]).await.unwrap();
    ctx.advance_virtual_clock(life::ONLINING_SECS);
    rpc(&ctx, &mut ceo_sess, "structureGuests", None, "GetGuests", &[PyRep::Integer(struct_id)]).await.unwrap();

    rpc(&ctx, &mut mgr_sess, "structureDocking", None, "Dock", &[PyRep::Integer(struct_id)]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut out_sess, "structureDocking", None, "Dock", &[PyRep::Integer(struct_id)]).await.unwrap_err().code, "StructureDockingAccessDenied");

    // 2. CEO loads initial fuel & fits a corp service module (`88_000_212` in `ServiceSlot1`); unprivileged corp member (`roles == 0`) is denied profiles, SpecializedFuelBay (`Add` & `MultiAdd` in/out), and service module fit/unfit/destroy (`Add`, `MultiAdd`, `DestroyFitting`)
    seed_item(&ctx, 88_000_199, 4247, ceo_id, struct_id, InventoryFlag::Hangar, 1_500, false);
    seed_item(&ctx, 88_000_212, 35894, corp_id, struct_id, InventoryFlag::Hangar, 1, true);
    rpc(&ctx, &mut ceo_sess, "invbroker", Some(ceo_inv), "Add", &[PyRep::Integer(88_000_199), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::SpecializedFuelBay as i64)]).await.unwrap();
    rpc(&ctx, &mut ceo_sess, "invbroker", Some(ceo_inv), "Add", &[PyRep::Integer(88_000_212), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::ServiceSlot1 as i64)]).await.unwrap();
    rpc(&ctx, &mut ceo_sess, "dogmaIM", None, "SetModuleOnline", &[PyRep::Integer(struct_id), PyRep::Integer(88_000_212)]).await.unwrap();
    seed_item(&ctx, 88_000_201, 4247, mgr_id, struct_id, InventoryFlag::Hangar, 1_200, false);
    seed_item(&ctx, 88_000_202, 35894, mgr_id, struct_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, 88_000_203, 4247, outsider_id, struct_id, InventoryFlag::Hangar, 500, false);
    seed_item(&ctx, 88_000_204, 35894, outsider_id, struct_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, 88_000_205, 34, mgr_id, struct_id, InventoryFlag::Hangar, 100, false); // Tritanium (non-fuel)
    seed_item(&ctx, 88_000_206, 4247, mgr_id, struct_id, InventoryFlag::Hangar, 400, false);

    assert_eq!(rpc(&ctx, &mut mgr_sess, "structureDeployment", None, "RenameStructure", &[PyRep::Integer(struct_id), PyRep::String("NoRole".into())]).await.unwrap_err().code, "InsufficientCorpRoles");
    assert_eq!(rpc(&ctx, &mut mgr_sess, "structureVulnerability", None, "UpdateReinforceTimesFromClient", &[PyRep::Integer(struct_id), PyRep::Integer(18), PyRep::Integer(2)]).await.unwrap_err().code, "InsufficientCorpRoles");
    assert_eq!(rpc(&ctx, &mut mgr_sess, "structureSettings", None, "CreateProfile", &[PyRep::String("Ops".into()), PyRep::String("Desc".into())]).await.unwrap_err().code, "InsufficientCorpRoles");
    // Seed fuel stack after structure sync RPCs (`burn_fuel` absorbs `SpecializedFuelBay` stacks into `st.fuel_blocks` on sync)
    seed_item(&ctx, 88_000_210, 4247, corp_id, struct_id, InventoryFlag::SpecializedFuelBay, 300, false);
    let mgr_inv = ctx.bind_object(&mgr_sess, "invbroker", struct_id).unwrap();
    assert_eq!(rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "Add", &[PyRep::Integer(88_000_201), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::SpecializedFuelBay as i64)]).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "MultiAdd", &[PyRep::List(vec![PyRep::Integer(88_000_201)]), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::SpecializedFuelBay as i64)]).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "Add", &[PyRep::Integer(88_000_210), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::Hangar as i64)]).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "MultiAdd", &[PyRep::List(vec![PyRep::Integer(88_000_210)]), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::Hangar as i64)]).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(ctx.db.get_item(88_000_210).unwrap().flag, InventoryFlag::SpecializedFuelBay);
    assert_eq!(rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "Add", &[PyRep::Integer(88_000_202), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await.unwrap_err().code, "InsufficientCorpRoles");
    assert_eq!(rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "Add", &[PyRep::Integer(88_000_212), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::Hangar as i64)]).await.unwrap_err().code, "InsufficientCorpRoles");
    assert_eq!(rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "MultiAdd", &[PyRep::List(vec![PyRep::Integer(88_000_212)]), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::Hangar as i64)]).await.unwrap_err().code, "InvalidLocation");
    assert_eq!(rpc(&ctx, &mut mgr_sess, "dogmaIM", None, "DestroyFitting", &[PyRep::Integer(88_000_212)]).await.unwrap_err().code, "InsufficientCorpRoles");
    assert_eq!(ctx.db.get_item(88_000_212).unwrap().flag, InventoryFlag::ServiceSlot1);

    // 3. CEO grants `ROLE_STATION_MANAGER` (`1024`, without Director) to `mgr_id` via `corpRegistry.UpdateMember`
    rpc(
        &ctx, &mut ceo_sess, "corpRegistry", None, "UpdateMember",
        &[PyRep::Integer(mgr_id), PyRep::Integer(eve_db::corp_roles::ROLE_STATION_MANAGER as i64)],
    ).await.expect("Grant ROLE_STATION_MANAGER");
    rpc(&ctx, &mut mgr_sess, "structureDeployment", None, "RenameStructure", &[PyRep::Integer(struct_id), PyRep::String("StationMgr Citadel".into())]).await.expect("Station Manager RenameStructure");
    rpc(&ctx, &mut mgr_sess, "structureVulnerability", None, "UpdateReinforceTimesFromClient", &[PyRep::Integer(struct_id), PyRep::Integer(18), PyRep::Integer(2)]).await.expect("Station Manager UpdateReinforceTimesFromClient");

    // Note: V24.01 `code.ccp` `eveProto/generated/eve/structure/profile_pb2.pyj` defines only `Identifier` (0 Quasar RPCs);
    // V24 structure profile CRUD runs via MachoNet `structureProfiles` (`structureProfileController.pyj` / `allProfilesController.pyj`)
    // paired with catalog `structureSettings` (`CharacterHasService` / `CharacterHasSetting`).
    let prof_id = rpc(&ctx, &mut mgr_sess, "structureSettings", None, "CreateProfile", &[PyRep::String("Industrial ACL".into()), PyRep::String("Managed by Station Manager".into())]).await.expect("Station Manager CreateProfile").as_i64().unwrap();
    rpc(&ctx, &mut mgr_sess, "structureSettings", None, "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(struct_id)]), PyRep::Integer(prof_id)]).await.expect("Station Manager ChangeProfiles");
    assert_eq!(rpc(&ctx, &mut out_sess, "structureSettings", None, "CharacterHasService", &[PyRep::Integer(struct_id), PyRep::Integer(35894)]).await.unwrap(), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut out_sess, "structureSettings", None, "CharacterHasSetting", &[PyRep::Integer(struct_id), PyRep::Integer(1)]).await.unwrap(), PyRep::Bool(true));

    rpc(&ctx, &mut out_sess, "structureDocking", None, "Dock", &[PyRep::Integer(struct_id)]).await.expect("Public profile assigned by Station Manager allows outsider docking");
    seed_item(&ctx, 88_000_211, 4247, corp_id, struct_id, InventoryFlag::SpecializedFuelBay, 300, false);
    let out_inv = ctx.bind_object(&out_sess, "invbroker", struct_id).unwrap();
    assert_eq!(rpc(&ctx, &mut out_sess, "invbroker", Some(out_inv), "Add", &[PyRep::Integer(88_000_203), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::SpecializedFuelBay as i64)]).await.unwrap_err().code, "StructureOwnershipViolation");
    assert_eq!(rpc(&ctx, &mut out_sess, "invbroker", Some(out_inv), "MultiAdd", &[PyRep::List(vec![PyRep::Integer(88_000_203)]), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::SpecializedFuelBay as i64)]).await.unwrap_err().code, "StructureOwnershipViolation");
    assert_eq!(rpc(&ctx, &mut out_sess, "invbroker", Some(out_inv), "Add", &[PyRep::Integer(88_000_211), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::Hangar as i64)]).await.unwrap_err().code, "StructureOwnershipViolation");
    assert_eq!(rpc(&ctx, &mut out_sess, "invbroker", Some(out_inv), "MultiAdd", &[PyRep::List(vec![PyRep::Integer(88_000_211)]), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::Hangar as i64)]).await.unwrap_err().code, "StructureOwnershipViolation");
    assert_eq!(rpc(&ctx, &mut out_sess, "invbroker", Some(out_inv), "Add", &[PyRep::Integer(88_000_204), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await.unwrap_err().code, "StructureOwnershipViolation");
    assert_eq!(rpc(&ctx, &mut out_sess, "invbroker", Some(out_inv), "Add", &[PyRep::Integer(88_000_212), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::Hangar as i64)]).await.unwrap_err().code, "StructureOwnershipViolation");
    assert_eq!(rpc(&ctx, &mut out_sess, "invbroker", Some(out_inv), "MultiAdd", &[PyRep::List(vec![PyRep::Integer(88_000_212)]), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::Hangar as i64)]).await.unwrap_err().code, "ItemOwnershipViolation");
    assert_eq!(rpc(&ctx, &mut out_sess, "dogmaIM", None, "DestroyFitting", &[PyRep::Integer(88_000_212)]).await.unwrap_err().code, "StructureOwnershipViolation");

    // Negative tests for R-DF-01: Director cannot destroy personal hangar items; outsider can destroy their own
    seed_item(&ctx, 88_000_250, 34, outsider_id, struct_id, InventoryFlag::Hangar, 10, false);
    seed_item(&ctx, 88_000_251, 34, mgr_id, struct_id, InventoryFlag::Hangar, 10, false);
    assert_eq!(rpc(&ctx, &mut ceo_sess, "dogmaIM", None, "DestroyFitting", &[PyRep::Integer(88_000_250)]).await.unwrap_err().code, "ItemNotOwnedByCharacter");
    assert!(ctx.db.get_item(88_000_250).is_some(), "Outsider hangar item remains intact");
    assert_eq!(rpc(&ctx, &mut ceo_sess, "dogmaIM", None, "DestroyFitting", &[PyRep::Integer(88_000_251)]).await.unwrap_err().code, "ItemNotOwnedByCharacter");
    assert!(ctx.db.get_item(88_000_251).is_some(), "Corp member hangar item remains intact");
    rpc(&ctx, &mut out_sess, "dogmaIM", None, "DestroyFitting", &[PyRep::Integer(88_000_250)]).await.expect("Outsider destroys own hangar item");
    assert!(ctx.db.get_item(88_000_250).is_none(), "Outsider hangar item destroyed by owner");

    // Non-fuel item rejected in SpecializedFuelBay on both `Add` and `MultiAdd`
    assert_eq!(rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "MultiAdd", &[PyRep::List(vec![PyRep::Integer(88_000_205)]), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::SpecializedFuelBay as i64)]).await.unwrap_err().code, "InvalidInventoryFlag");

    // Station Manager loads personal fuel via `Add` (partial split + full move) and `MultiAdd`: `owner_id` transfers to `corp_id`!
    let mut split_kw = HashMap::new();
    split_kw.insert("qty".into(), PyRep::Integer(500));
    ctx.dispatch_rpc(&mut mgr_sess, "invbroker", Some(mgr_inv), "Add", &[PyRep::Integer(88_000_201), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::SpecializedFuelBay as i64)], &split_kw, None).await.expect("Station Manager split-loads fuel");
    rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "Add", &[PyRep::Integer(88_000_201), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::SpecializedFuelBay as i64)]).await.expect("Station Manager load SpecializedFuelBay");
    rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "MultiAdd", &[PyRep::List(vec![PyRep::Integer(88_000_206)]), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::SpecializedFuelBay as i64)]).await.expect("Station Manager MultiAdd SpecializedFuelBay");
    ctx.db.with_snapshot(|s| {
        let fuel_items: Vec<_> = s.items.values().filter(|i| i.location_id == struct_id && i.flag == InventoryFlag::SpecializedFuelBay).collect();
        assert!(!fuel_items.is_empty());
        assert!(fuel_items.iter().all(|i| i.owner_id == corp_id), "All fuel in SpecializedFuelBay must retain owning corp_id");
    });

    let _ = ctx.drain_notifications_for_char(mgr_id);
    rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "Add", &[PyRep::Integer(88_000_202), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await.expect("Station Manager fits service module");
    rpc(&ctx, &mut mgr_sess, "dogmaIM", None, "TakeModuleOffline", &[PyRep::Integer(struct_id), PyRep::Integer(88_000_202)]).await.expect("Station Manager offlines service module");
    assert_eq!(ctx.db.with_snapshot(|s| s.module_online_states.get(&88_000_202).copied()), Some(false));
    rpc(&ctx, &mut mgr_sess, "dogmaIM", None, "SetModuleOnline", &[PyRep::Integer(struct_id), PyRep::Integer(88_000_202)]).await.expect("Station Manager onlines service module");
    assert_eq!(ctx.db.with_snapshot(|s| s.module_online_states.get(&88_000_202).copied()), Some(true));
    assert!(ctx.drain_notifications_for_char(mgr_id).iter().any(|n| n.event == "OnStructureServiceChanged"));
}
