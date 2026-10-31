//! Batch 39 open `feat:` parity integration tests (part 3: Clone Vat Bay fuel, immobilization, fleet clone install & CloneJump).

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

async fn setup_pilot(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
    corp_id: i64,
    station_id: Option<i64>,
) -> SessionState {
    let mut sess = SessionState::new((900_000 + (char_id % 100_000)) as u64);
    sess.user_id = Some(char_id as u64);
    ctx.db.with_mut_snapshot(|s| {
        let mut ch = s.characters.get(&90_000_001).cloned().unwrap();
        ch.character_id = char_id;
        ch.name = format!("Pilot-{char_id}");
        ch.corporation_id = corp_id;
        ch.alliance_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.station_id = station_id;
        ch.structure_id = None;
        ch.security_status = 0.0;
        ch.balance = IskCents::from_isk(500_000_000);
        s.characters.insert(char_id, ch);
        s.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: station_id.unwrap_or(sys_id as i64),
                flag: if station_id.is_some() { InventoryFlag::Hangar } else { InventoryFlag::None },
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if station_id.is_none() {
            s.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
        }
    });
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = station_id;
    sess.structure_id = None;

    if station_id.is_none() {
        let ship_type = ctx.sde.types.get(&ship_type_id);
        let handle = ctx.systems.get_or_spawn(sys_id);
        let _ = handle
            .spawn_ball_with_combat(
                Ball {
                    id: ship_id,
                    type_id: ship_type_id,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 250.0,
                    inertia: 0.5,
                    mass: ship_type.map(|t| t.mass).unwrap_or(1_500_000.0),
                    radius: ship_type.map(|t| t.radius).unwrap_or(50.0),
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(ship_type_id),
            )
            .await;
    }
    sess
}

#[tokio::test]
async fn test_b39_clone_vat_bay_titan_and_rorqual_install_and_jump() {
    // parity: feat:modules.clone-vat-bay-fuel
    eve_testkit::attest(&["feat:modules.clone-vat-bay-fuel"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let sys_host = 30_000_012_u32; // Asabona (low-sec)
    let jita_sys = 30_000_142_u32;
    let jita_station = 60_003_760_i64;

    // 1. Negative cases: non-Titan/Rorqual hull rejected & insufficient Liquid Ozone rejected
    let c_subcap = 91_000_050_i64;
    let s_subcap = 101_000_050_i64;
    let mut sess_subcap = setup_pilot(&ctx, c_subcap, s_subcap, 24692, sys_host, DVec3::new(100_000.0, 0.0, 0.0), 1_000_009, None).await;
    let bad_cvb = 101_000_700_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            bad_cvb,
            InventoryItemRecord {
                item_id: bad_cvb,
                type_id: 23735, // Clone Vat Bay I
                owner_id: c_subcap,
                location_id: s_subcap,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.module_online_states.insert(bad_cvb, true);
    });
    let hull_err = ctx
        .dispatch_rpc(&mut sess_subcap, "dogmaIM", None, "Activate", &[PyRep::Integer(bad_cvb)], &kw, None)
        .await
        .expect_err("Non-Titan/Rorqual hull cannot activate Clone Vat Bay");
    assert_eq!(hull_err.code, "ShipCannotFitModule");

    // 2. Titan (Avatar 11567) AND Rorqual (28352): SpecializedFuelBay priority, single LO consumption, ship immobilization, fleet install + CloneJump!
    let c_titan = 91_000_051_i64;
    let s_titan = 101_000_051_i64;
    let c_rorq = 91_000_055_i64;
    let s_rorq = 101_000_055_i64;
    let c_mate = 91_000_052_i64;
    let s_mate = 101_000_052_i64;
    let c_outsider = 91_000_053_i64;
    let s_outsider = 101_000_053_i64;

    let titan_pos = DVec3::new(5_000_000_000.0, 0.0, 0.0);
    let rorq_pos = DVec3::new(5_000_500_000.0, 0.0, 0.0);
    let mut sess_titan = setup_pilot(&ctx, c_titan, s_titan, 11567, sys_host, titan_pos, 1_000_009, None).await;
    let mut sess_rorq = setup_pilot(&ctx, c_rorq, s_rorq, 28352, sys_host, rorq_pos, 1_000_009, None).await;
    let mut sess_mate = setup_pilot(&ctx, c_mate, s_mate, 587, sys_host, titan_pos + DVec3::new(25_000.0, 0.0, 0.0), 1_000_009, None).await;
    let _sess_outsider = setup_pilot(&ctx, c_outsider, s_outsider, 587, sys_host, titan_pos + DVec3::new(40_000.0, 0.0, 0.0), 1_000_010, None).await;
    let beyonce_titan = ctx.bind_object(&mut sess_titan, "beyonce", sys_host as i64).unwrap();

    let cvb_titan = 101_000_701_i64;
    let cvb_rorq = 101_000_711_i64;
    let lo_fuel_bay = 101_000_702_i64;
    let lo_cargo = 101_000_703_i64;
    let lo_rorq = 101_000_712_i64;

    ctx.db.with_mut_snapshot(|s| {
        for cid in [c_mate, c_outsider] {
            s.skills.entry(cid).or_default().insert(
                24242, // Infomorph Psychology V
                TrainedSkillRecord { type_id: 24242, level: 5, skill_points: 256_000.0 },
            );
        }
        s.fleets.insert(
            889,
            eve_db::FleetRecord {
                fleet_id: 889,
                boss_id: c_titan,
                members: vec![c_titan, c_rorq, c_mate],
                ..Default::default()
            },
        );
        for (mid, owner, ship) in [(cvb_titan, c_titan, s_titan), (cvb_rorq, c_rorq, s_rorq)] {
            s.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: 23735, // Clone Vat Bay I
                    owner_id: owner,
                    location_id: ship,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            s.module_online_states.insert(mid, true);
        }
        // Start with 0 LO on Titan -> NotEnoughFuel
        s.items.insert(
            lo_fuel_bay,
            InventoryItemRecord {
                item_id: lo_fuel_bay,
                type_id: 16273,
                owner_id: c_titan,
                location_id: s_titan,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 0,
                singleton: false,
                ..Default::default()
            },
        );
        s.items.insert(
            lo_rorq,
            InventoryItemRecord {
                item_id: lo_rorq,
                type_id: 16273,
                owner_id: c_rorq,
                location_id: s_rorq,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 1_000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    sess_titan.fleet_id = Some(889);
    sess_rorq.fleet_id = Some(889);
    sess_mate.fleet_id = Some(889);

    // Insufficient fuel check
    let no_fuel_err = ctx
        .dispatch_rpc(&mut sess_titan, "dogmaIM", None, "Activate", &[PyRep::Integer(cvb_titan)], &kw, None)
        .await
        .expect_err("Insufficient Liquid Ozone rejected");
    assert_eq!(no_fuel_err.code, "NotEnoughFuel");

    // Clone offer before activation is rejected
    let inactive_err = ctx
        .dispatch_rpc(
            &mut sess_titan,
            "jumpCloneSvc",
            None,
            "OfferShipCloneInstallation",
            &[PyRep::Integer(c_mate), PyRep::Integer(s_titan)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(inactive_err.code, "NoActiveCloneVatBay");

    // Top up Titan SpecializedFuelBay (500 LO) + Cargo (500 LO) to verify SpecializedFuelBay priority & zero double-consumption
    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&lo_fuel_bay).unwrap().quantity = 500;
        s.items.insert(
            lo_cargo,
            InventoryItemRecord {
                item_id: lo_cargo,
                type_id: 16273,
                owner_id: c_titan,
                location_id: s_titan,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                ..Default::default()
            },
        );
    });

    ctx.dispatch_rpc(&mut sess_titan, "dogmaIM", None, "Activate", &[PyRep::Integer(cvb_titan)], &kw, None)
        .await
        .expect("Titan activates Clone Vat Bay I");
    ctx.dispatch_rpc(&mut sess_rorq, "dogmaIM", None, "Activate", &[PyRep::Integer(cvb_rorq)], &kw, None)
        .await
        .expect("Rorqual activates Clone Vat Bay I");

    let (fb_after_act, cargo_after_act, rorq_after_act) = ctx.db.with_snapshot(|s| {
        (
            s.items.get(&lo_fuel_bay).map(|i| i.quantity).unwrap_or(0),
            s.items.get(&lo_cargo).map(|i| i.quantity).unwrap_or(0),
            s.items.get(&lo_rorq).map(|i| i.quantity).unwrap_or(0),
        )
    });
    assert!(fb_after_act < 500, "Titan consumed Liquid Ozone from SpecializedFuelBay ({fb_after_act} < 500)");
    assert_eq!(cargo_after_act, 500, "Cargo LO untouched when SpecializedFuelBay has enough fuel");
    assert!(rorq_after_act < 1_000, "Rorqual consumed Liquid Ozone ({rorq_after_act} < 1000)");

    // Ship immobilization enforced during active Clone Vat Bay cycle
    let sys_handle = ctx.systems.get_or_spawn(sys_host);
    let (_, titan_ball) = sys_handle.get_ball_snapshot(s_titan).await.unwrap().unwrap();
    assert_eq!(titan_ball.max_velocity, 0.0, "Titan max_velocity set to 0 while Clone Vat Bay is active");
    let warp_err = ctx
        .dispatch_rpc(
            &mut sess_titan,
            "beyonce",
            Some(beyonce_titan),
            "CmdGotoDirection",
            &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await;
    let _ = warp_err;
    sys_handle.step_manual(2.0).await.unwrap();
    let (_, titan_ball_after_step) = sys_handle.get_ball_snapshot(s_titan).await.unwrap().unwrap();
    assert!((titan_ball_after_step.position - titan_pos).length() < 1.0, "Immobilized Titan cannot move during cycle");

    // Non-fleet outsider cannot be offered a clone
    let non_fleet_offer_err = ctx
        .dispatch_rpc(
            &mut sess_titan,
            "jumpCloneSvc",
            None,
            "OfferShipCloneInstallation",
            &[PyRep::Integer(c_outsider), PyRep::Integer(s_titan)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(non_fleet_offer_err.code, "TargetNotInSameFleetOrSystem");

    // Fleetmate installs clone in Titan AND in Rorqual (verify ZERO extra LO deducted on AcceptShipCloneInstallation!)
    ctx.dispatch_rpc(
        &mut sess_titan,
        "jumpCloneSvc",
        None,
        "OfferShipCloneInstallation",
        &[PyRep::Integer(c_mate), PyRep::Integer(s_titan)],
        &kw,
        None,
    )
    .await
    .expect("Offer clone on Titan");
    ctx.dispatch_rpc(&mut sess_mate, "jumpCloneSvc", None, "AcceptShipCloneInstallation", &[], &kw, None)
        .await
        .expect("Accept clone on Titan");

    let fb_after_install = ctx.db.with_snapshot(|s| s.items.get(&lo_fuel_bay).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(fb_after_install, fb_after_act, "AcceptShipCloneInstallation does NOT double-deduct Liquid Ozone");

    // Also install a second clone on the Rorqual
    let _ = sys_handle.set_ball_position(s_mate, rorq_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    ctx.dispatch_rpc(
        &mut sess_rorq,
        "jumpCloneSvc",
        None,
        "OfferShipCloneInstallation",
        &[PyRep::Integer(c_mate), PyRep::Integer(s_rorq)],
        &kw,
        None,
    )
    .await
    .expect("Offer clone on Rorqual");
    ctx.dispatch_rpc(&mut sess_mate, "jumpCloneSvc", None, "AcceptShipCloneInstallation", &[], &kw, None)
        .await
        .expect("Accept clone on Rorqual");

    let (titan_clone_id, rorq_clone_id) = ctx.db.with_snapshot(|s| {
        let clones = s.jump_clones.get(&c_mate).unwrap();
        let tc = clones.iter().find(|c| c.location_id == s_titan).unwrap().jump_clone_id;
        let rc = clones.iter().find(|c| c.location_id == s_rorq).unwrap().jump_clone_id;
        (tc, rc)
    });

    // 3. Dock fleetmate in Jita 4-4 NPC station (60_003_760) in a Capsule (670) and test `jumpCloneSvc.CloneJump` to the ship clone!
    let jita_pod_id = 101_000_999_i64;
    let _ = sys_handle.remove_ball(s_mate).await;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            jita_pod_id,
            InventoryItemRecord {
                item_id: jita_pod_id,
                type_id: 670,
                owner_id: c_mate,
                location_id: jita_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let ch = s.characters.get_mut(&c_mate).unwrap();
        ch.solar_system_id = jita_sys;
        ch.station_id = Some(jita_station);
        ch.structure_id = None;
        ch.active_ship_id = jita_pod_id;
        s.items.get_mut(&s_mate).unwrap().location_id = jita_station;
        s.items.get_mut(&s_mate).unwrap().flag = InventoryFlag::Hangar;
    });
    sess_mate.solar_system_id = Some(jita_sys);
    sess_mate.station_id = Some(jita_station);
    sess_mate.ship_id = Some(jita_pod_id);

    // Negative check: leaving fleet blocks `CloneJump` to ship clone!
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.get_mut(&889).unwrap().members.retain(|&m| m != c_mate);
    });
    sess_mate.fleet_id = None;
    let left_fleet_err = ctx
        .dispatch_rpc(
            &mut sess_mate,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(titan_clone_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Non-fleet pilot cannot CloneJump into ship Clone Vat Bay");
    assert_eq!(left_fleet_err.code, "NotInHostShipFleet");

    // Rejoin fleet, set non-trivial corp/alliance/sec-status on jumper, and verify live Ballpark position + cloak checks + SlimItemMeta!
    let (mate_corp, mate_alliance) = (98_000_777_i64, 99_000_777_i64);
    let moved_titan_pos = DVec3::new(7_500_000_000.0, 250_000.0, -100_000.0);
    let _ = sys_handle.set_ball_position(s_titan, moved_titan_pos).await;
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.get_mut(&889).unwrap().members.push(c_mate);
        // Deliberately leave stale `[0, 0, 0]` in `item_positions` to prove live Ballpark coordinates are used!
        s.item_positions.insert(s_titan, [0.0, 0.0, 0.0]);
        let ch = s.characters.get_mut(&c_mate).unwrap();
        ch.corporation_id = mate_corp;
        ch.alliance_id = Some(mate_alliance);
        ch.security_status = 2.75;
    });
    sess_mate.fleet_id = Some(889);
    sess_mate.corporation_id = Some(mate_corp);

    // Negative check: Cloaked host Titan must block `CloneJump`!
    assert!(sys_handle.try_update_combat_state(s_titan, |cs| cs.is_cloaked = true));
    let cloaked_jump_err = ctx
        .dispatch_rpc(
            &mut sess_mate,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(titan_clone_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Cloaked host Titan must reject CloneJump");
    assert_eq!(cloaked_jump_err.code, "ShipCloaked");
    assert!(sys_handle.try_update_combat_state(s_titan, |cs| cs.is_cloaked = false));

    ctx.dispatch_rpc(
        &mut sess_mate,
        "jumpCloneSvc",
        None,
        "CloneJump",
        &[PyRep::Integer(titan_clone_id)],
        &kw,
        None,
    )
    .await
    .expect("Fleet member CloneJumps to active uncloaked Titan Clone Vat Bay");

    assert_eq!(sess_mate.solar_system_id, Some(sys_host));
    assert_eq!(sess_mate.station_id, None, "Ship clone jump places pilot in space, not docked");
    assert_eq!(sess_mate.structure_id, None);
    let pod_id = sess_mate.ship_id.expect("spawned capsule ID");
    let pod_type = ctx.db.with_snapshot(|s| s.items.get(&pod_id).map(|i| i.type_id));
    assert_eq!(pod_type, Some(670), "Jumper spawned in Capsule (type 670)");
    let (_, pod_ball) = sys_handle.get_ball_snapshot(pod_id).await.unwrap().expect("Capsule ball in space");
    assert!((pod_ball.position - moved_titan_pos).length() < 10_000.0, "Capsule spawned adjacent to live Titan Ball coordinates, not stale [0,0,0]");
    let pod_meta = sys_handle.ballpark.read().unwrap().ball_slim_meta.get(&pod_id).cloned().expect("Capsule SlimItemMeta");
    assert_eq!(pod_meta.corp_id, mate_corp, "SlimItemMeta populated with jumper corporation_id");
    assert_eq!(pod_meta.alliance_id, mate_alliance, "SlimItemMeta populated with jumper alliance_id");
    assert!((pod_meta.security_status - 2.75).abs() < 1e-6, "SlimItemMeta populated with jumper security_status");

    // 4. Dock mate back in Jita, verify missing live Ballpark host ball rejects CloneJump even while DB CloneVatBayState is active, then verify cycle expiry!
    let _ = sys_handle.remove_ball(pod_id).await;
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&c_mate).unwrap();
        ch.solar_system_id = jita_sys;
        ch.station_id = Some(jita_station);
        ch.active_ship_id = jita_pod_id;
        ch.last_clone_jump_unix = 0;
    });
    sess_mate.solar_system_id = Some(jita_sys);
    sess_mate.station_id = Some(jita_station);
    sess_mate.ship_id = Some(jita_pod_id);
    ctx.advance_secs(15);

    let (_, saved_rorq_ball) = sys_handle.get_ball_snapshot(s_rorq).await.unwrap().unwrap();
    let saved_rorq_cs = sys_handle.get_combat_state(s_rorq).await.unwrap().unwrap();
    let _ = sys_handle.remove_ball(s_rorq).await;
    let missing_ball_err = ctx
        .dispatch_rpc(
            &mut sess_mate,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(rorq_clone_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Missing live Ballpark host ship must block CloneJump");
    assert_eq!(missing_ball_err.code, "NoActiveCloneVatBay");
    let _ = sys_handle.spawn_ball_with_combat(saved_rorq_ball, saved_rorq_cs).await;

    ctx.advance_secs(305);
    sys_handle.step_manual(305.0).await.unwrap();

    let expired_offer_err = ctx
        .dispatch_rpc(
            &mut sess_rorq,
            "jumpCloneSvc",
            None,
            "OfferShipCloneInstallation",
            &[PyRep::Integer(c_mate), PyRep::Integer(s_rorq)],
            &kw,
            None,
        )
        .await
        .expect_err("Expired Clone Vat Bay cycle blocks clone installation");
    assert_eq!(expired_offer_err.code, "NoActiveCloneVatBay");

    let expired_jump_err = ctx
        .dispatch_rpc(
            &mut sess_mate,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(rorq_clone_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Expired Clone Vat Bay cycle blocks CloneJump");
    assert_eq!(expired_jump_err.code, "NoActiveCloneVatBay");
}
