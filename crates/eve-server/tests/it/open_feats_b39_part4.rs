//! `open_feats_b39_part4` — Covert Cynosural Field & Covert Jump Portal Generator
//! (`feat:modules.covert-cyno-rules`, `feat:modules.covert-jump-portal-generator`).

use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

fn kv_get(rep: &PyRep, key: &str) -> Option<PyRep> {
    match rep {
        PyRep::Dict(_) => rep.dict_get(key).cloned(),
        PyRep::KeyVal(map) => map.get(key).cloned(),
        PyRep::Object { args, .. } => args.dict_get(key).cloned(),
        _ => None,
    }
}

async fn setup_pilot(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    solar_system_id: u32,
    position: DVec3,
    corp_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(char_id as u64);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(solar_system_id);

    ctx.db.with_mut_snapshot(|s| {
        let mut ch = s.characters.get(&90_000_001).cloned().unwrap();
        ch.character_id = char_id;
        ch.name = format!("Pilot-{char_id}");
        ch.corporation_id = corp_id;
        ch.alliance_id = None;
        ch.solar_system_id = solar_system_id;
        ch.active_ship_id = ship_id;
        ch.station_id = None;
        ch.structure_id = None;
        s.characters.insert(char_id, ch);
        s.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: solar_system_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.item_positions.insert(ship_id, [position.x, position.y, position.z]);
    });

    let ship_type = ctx.sde.types.get(&ship_type_id);
    let handle = ctx.systems.get_or_spawn(solar_system_id);
    let _ = handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position,
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
    sess
}

#[tokio::test]
async fn test_b39_covert_cyno_rules() {
    // parity: feat:modules.covert-cyno-rules
    eve_testkit::attest(&["feat:modules.covert-cyno-rules"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let sys_a = 30_000_012_u32; // Asabona (low-sec)
    let sys_b = 30_000_014_u32; // Shamahi (low-sec, 2.10 LY from Asabona)

    let c_cov = 91_000_041_i64;
    let s_cov = 101_000_041_i64; // Hound (12034)
    let c_prospect = 91_000_042_i64;
    let s_prospect = 101_000_042_i64; // Prospect (33697)
    let c_etana = 91_000_043_i64;
    let s_etana = 101_000_043_i64; // Etana (32790)
    let c_rabisu = 91_000_044_i64;
    let s_rabisu = 101_000_044_i64; // Rabisu (42245)
    let c_rifter = 91_000_045_i64;
    let s_rifter = 101_000_045_i64; // Rifter (587, non-covert)
    let c_offgrid = 91_000_046_i64;
    let s_offgrid = 101_000_046_i64; // 20,000 km away in sys_b (different grid bubble!)
    let c_ongrid = 91_000_047_i64;
    let s_ongrid = 101_000_047_i64; // 5 km away in sys_b (same grid bubble!)

    let mut sess_cov = setup_pilot(&ctx, c_cov, s_cov, 12034, sys_b, DVec3::new(100_000.0, 0.0, 0.0), 1_000_009).await;
    let mut sess_prospect = setup_pilot(&ctx, c_prospect, s_prospect, 33697, sys_b, DVec3::new(200_000.0, 0.0, 0.0), 1_000_009).await;
    let mut sess_etana = setup_pilot(&ctx, c_etana, s_etana, 32790, sys_b, DVec3::new(300_000.0, 0.0, 0.0), 1_000_009).await;
    let mut sess_rabisu = setup_pilot(&ctx, c_rabisu, s_rabisu, 42245, sys_b, DVec3::new(400_000.0, 0.0, 0.0), 1_000_009).await;
    let mut sess_rifter = setup_pilot(&ctx, c_rifter, s_rifter, 587, sys_b, DVec3::new(500_000.0, 0.0, 0.0), 1_000_009).await;
    let mut sess_offgrid = setup_pilot(&ctx, c_offgrid, s_offgrid, 587, sys_b, DVec3::new(20_000_000.0, 0.0, 0.0), 1_000_010).await;
    let mut sess_ongrid = setup_pilot(&ctx, c_ongrid, s_ongrid, 587, sys_b, DVec3::new(105_000.0, 0.0, 0.0), 1_000_010).await;

    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(
            888,
            eve_db::FleetRecord {
                fleet_id: 888,
                boss_id: c_cov,
                members: vec![c_cov, c_prospect, c_etana, c_rabisu, c_rifter],
                ..Default::default()
            },
        );
        for (idx, (cid, sid)) in [
            (c_cov, s_cov),
            (c_prospect, s_prospect),
            (c_etana, s_etana),
            (c_rabisu, s_rabisu),
            (c_rifter, s_rifter),
        ]
        .into_iter()
        .enumerate()
        {
            s.skills.entry(cid).or_default().insert(
                21603, // Cynosural Field Theory V
                TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 },
            );
            let mid = 101_000_800 + idx as i64;
            let loid = 101_000_810 + idx as i64;
            s.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: 28646, // Covert Cynosural Field Generator I
                    owner_id: cid,
                    location_id: sid,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            s.module_online_states.insert(mid, true);
            s.items.insert(
                loid,
                InventoryItemRecord {
                    item_id: loid,
                    type_id: 16273, // Liquid Ozone
                    owner_id: cid,
                    location_id: sid,
                    flag: InventoryFlag::Cargo,
                    quantity: 200,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
    });
    for sess in [&mut sess_cov, &mut sess_prospect, &mut sess_etana, &mut sess_rabisu, &mut sess_rifter] {
        sess.fleet_id = Some(888);
    }

    // 1. Requires Cynosural Field Theory V (CFT IV rejected) & fits only covert-cloak hulls + Etana/Prospect/Rabisu
    ctx.db.with_mut_snapshot(|s| {
        s.skills.get_mut(&c_cov).unwrap().get_mut(&21603).unwrap().level = 4;
    });
    let sk_err = ctx
        .dispatch_rpc(&mut sess_cov, "dogmaIM", None, "Activate", &[PyRep::Integer(101_000_800)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(sk_err.code, "SkillMissing");
    ctx.db.with_mut_snapshot(|s| {
        s.skills.get_mut(&c_cov).unwrap().get_mut(&21603).unwrap().level = 5;
    });

    let rifter_err = ctx
        .dispatch_rpc(&mut sess_rifter, "dogmaIM", None, "Activate", &[PyRep::Integer(101_000_804)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(rifter_err.code, "CannotLightCynoInSystem");

    // Strategic Cruiser without Covert Reconfiguration subsystem cannot light Covert Cyno, then WITH 45589 succeeds!
    let c_t3 = 91_000_048_i64;
    let s_t3 = 101_000_048_i64;
    let mut sess_t3 = setup_pilot(&ctx, c_t3, s_t3, 29984, sys_b, DVec3::new(600_000.0, 0.0, 0.0), 1_000_009).await;
    sess_t3.fleet_id = Some(888);
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.get_mut(&888).unwrap().members.push(c_t3);
        s.skills.entry(c_t3).or_default().insert(21603, TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
        s.skills.entry(c_t3).or_default().insert(30544, TrainedSkillRecord { type_id: 30544, level: 5, skill_points: 256_000.0 });
        s.items.insert(101_000_809, InventoryItemRecord { item_id: 101_000_809, type_id: 28646, owner_id: c_t3, location_id: s_t3, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default() });
        s.items.insert(101_000_819, InventoryItemRecord { item_id: 101_000_819, type_id: 16273, owner_id: c_t3, location_id: s_t3, flag: InventoryFlag::Cargo, quantity: 200, singleton: false, ..Default::default() });
        s.module_online_states.insert(101_000_809, true);
    });
    ctx.invalidate_ship_fitting(s_t3);
    assert_eq!(ctx.dispatch_rpc(&mut sess_t3, "dogmaIM", None, "Activate", &[PyRep::Integer(101_000_809)], &kw, None).await.unwrap_err().code, "CannotLightCynoInSystem");
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(101_000_829, InventoryItemRecord { item_id: 101_000_829, type_id: 45589, owner_id: c_t3, location_id: s_t3, flag: InventoryFlag::SubSystemSlot1, quantity: 1, singleton: true, ..Default::default() });
        s.module_online_states.insert(101_000_829, true);
    });
    ctx.invalidate_ship_fitting(s_t3);
    ctx.dispatch_rpc(&mut sess_t3, "dogmaIM", None, "Activate", &[PyRep::Integer(101_000_809)], &kw, None).await.expect("Tengu with Covert Reconfiguration lights Covert Cyno");

    // 2. Cyno-jammed system: normal cyno (21096) on Force Recon (Pilgrim 11965) fails with CynoJammed, while Covert Cyno succeeds!
    let c_pilgrim = 91_000_047_i64;
    let s_pilgrim = 101_000_047_i64;
    let mut sess_pilgrim = setup_pilot(&ctx, c_pilgrim, s_pilgrim, 11965, sys_b, DVec3::new(110_000.0, 0.0, 0.0), 1_000_009).await;
    sess_pilgrim.fleet_id = Some(888);
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.get_mut(&888).unwrap().members.push(c_pilgrim);
        s.skills.entry(c_pilgrim).or_default().insert(21603, TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
        s.items.insert(
            999_001,
            InventoryItemRecord {
                item_id: 999_001, type_id: 33700, owner_id: 1, location_id: sys_b as i64,
                flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
            },
        );
        s.items.insert(
            101_000_820,
            InventoryItemRecord {
                item_id: 101_000_820, type_id: 21096, owner_id: c_pilgrim, location_id: s_pilgrim,
                flag: InventoryFlag::HiSlot1, quantity: 1, singleton: true, ..Default::default()
            },
        );
        s.items.insert(
            101_000_821,
            InventoryItemRecord {
                item_id: 101_000_821, type_id: 16273, owner_id: c_pilgrim, location_id: s_pilgrim,
                flag: InventoryFlag::Cargo, quantity: 500, singleton: false, ..Default::default()
            },
        );
    });
    let norm_jam_err = ctx
        .dispatch_rpc(&mut sess_pilgrim, "dogmaIM", None, "Activate", &[PyRep::Integer(101_000_820)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(norm_jam_err.code, "CynoJammed");

    let cov_rep = ctx
        .dispatch_rpc(&mut sess_cov, "dogmaIM", None, "Activate", &[PyRep::Integer(101_000_800)], &kw, None)
        .await
        .expect("Hound lights Covert Cyno in cyno-jammed system");
    let cov_beacon_id = kv_get(&cov_rep, "beaconID").and_then(|v| v.as_i64()).unwrap();

    // Verify 1/10 cycle time (60s vs 600s) and 1/10 fuel (25 LO vs 250 LO at CFT V)
    assert_eq!(kv_get(&cov_rep, "durationSeconds").and_then(|v| v.as_f64()), Some(60.0));
    assert_eq!(kv_get(&cov_rep, "consumptionQuantity").and_then(|v| v.as_i64()), Some(25));
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&101_000_810).unwrap().quantity), 175);

    // Prospect (33697), Etana (32790), and Rabisu (42245) all successfully light Covert Cyno!
    for (sess, mid) in [
        (&mut sess_prospect, 101_000_801_i64),
        (&mut sess_etana, 101_000_802_i64),
        (&mut sess_rabisu, 101_000_803_i64),
    ] {
        ctx.dispatch_rpc(sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mid)], &kw, None)
            .await
            .expect("Special hull (Prospect/Etana/Rabisu) lights Covert Cyno");
    }

    // 3. On-grid-only visibility: hidden from `map.GetBeaconCount` and off-grid `scanMgr.ConeScan`, visible to on-grid `scanMgr.ConeScan`!
    let map_counts = ctx.dispatch_rpc(&mut sess_cov, "map", None, "GetBeaconCount", &[], &kw, None).await.unwrap();
    let sys_b_count = match &map_counts {
        PyRep::Dict(d) => d.iter().find(|(k, _)| k.as_i64() == Some(sys_b as i64)).and_then(|(_, v)| v.as_i64()),
        _ => None,
    };
    assert!(sys_b_count.is_none(), "Covert cyno hidden from system map beacon count");

    let au_m = 14.3 * 149_597_870_700.0;
    let offgrid_dscan = ctx
        .dispatch_rpc(&mut sess_offgrid, "scanMgr", None, "ConeScan", &[PyRep::Real(360.0), PyRep::Real(au_m)], &kw, None)
        .await
        .unwrap();
    let offgrid_list = match &offgrid_dscan {
        PyRep::List(l) => l,
        _ => panic!("ConeScan list"),
    };
    let offgrid_sees_beacon = offgrid_list.iter().any(|entry| kv_get(entry, "itemID").and_then(|v| v.as_i64()) == Some(cov_beacon_id));
    assert!(!offgrid_sees_beacon, "Off-grid ship cannot see Covert Cyno on D-Scan");

    let ongrid_dscan = ctx
        .dispatch_rpc(&mut sess_ongrid, "scanMgr", None, "ConeScan", &[PyRep::Real(360.0), PyRep::Real(au_m)], &kw, None)
        .await
        .unwrap();
    let ongrid_list = match &ongrid_dscan {
        PyRep::List(l) => l,
        _ => panic!("ConeScan list"),
    };
    let ongrid_sees_beacon = ongrid_list.iter().any(|entry| kv_get(entry, "itemID").and_then(|v| v.as_i64()) == Some(cov_beacon_id));
    assert!(ongrid_sees_beacon, "On-grid ship sees Covert Cyno on grid");

    // 4. Jump & bridge restrictions: ONLY Black Ops can jump or bridge to a Covert Cyno (Dreadnought & Titan rejected)
    let c_titan = 91_000_049_i64;
    let s_titan = 101_000_049_i64;
    let mut sess_titan = setup_pilot(&ctx, c_titan, s_titan, 11567, sys_a, DVec3::new(100_000.0, 0.0, 0.0), 1_000_009).await;
    let c_blops = 91_000_059_i64;
    let s_blops = 101_000_059_i64;
    let mut sess_blops = setup_pilot(&ctx, c_blops, s_blops, 22428, sys_a, DVec3::new(105_000.0, 0.0, 0.0), 1_000_009).await;
    sess_titan.fleet_id = Some(888);
    sess_blops.fleet_id = Some(888);

    ctx.db.with_mut_snapshot(|s| {
        s.fleets.get_mut(&888).unwrap().members.extend([c_titan, c_blops]);
        for cid in [c_titan, c_blops] {
            s.skills.entry(cid).or_default().insert(3456, TrainedSkillRecord { type_id: 3456, level: 5, skill_points: 256_000.0 });
            s.skills.entry(cid).or_default().insert(24562, TrainedSkillRecord { type_id: 24562, level: 5, skill_points: 256_000.0 });
        }
        // Standard Jump Portal Generator I (23953) on Titan
        s.items.insert(
            101_000_830,
            InventoryItemRecord {
                item_id: 101_000_830, type_id: 23953, owner_id: c_titan, location_id: s_titan,
                flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
            },
        );
        // Helium Isotopes on Black Ops
        s.items.insert(
            101_000_831,
            InventoryItemRecord {
                item_id: 101_000_831, type_id: 16274, owner_id: c_blops, location_id: s_blops,
                flag: InventoryFlag::SpecializedFuelBay, quantity: 50_000, singleton: false, ..Default::default()
            },
        );
    });

    let titan_jump_err = ctx
        .dispatch_rpc(
            &mut sess_titan,
            "beyonce",
            None,
            "CmdJumpToFleetModuleBeacon",
            &[PyRep::Integer(c_cov), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(titan_jump_err.code, "ShipCannotJumpToBeacon");

    let titan_bridge_err = ctx
        .dispatch_rpc(
            &mut sess_titan,
            "beyonce",
            None,
            "CmdBridgeToFleetModuleBeacon",
            &[PyRep::Integer(c_cov), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(titan_bridge_err.code, "ShipCannotBridgeToBeacon");

    ctx.dispatch_rpc(
        &mut sess_blops,
        "beyonce",
        None,
        "CmdJumpToFleetModuleBeacon",
        &[PyRep::Integer(c_cov), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id)],
        &kw,
        None,
    )
    .await
    .expect("Black Ops jumps to Covert Cynosural Field");
}

#[tokio::test]
async fn test_b39_covert_jump_portal_generator_all_hulls_and_fuel() {
    // parity: feat:modules.covert-jump-portal-generator
    eve_testkit::attest(&["feat:modules.covert-jump-portal-generator"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let sys_a = 30_000_012_u32; // Asabona
    let sys_b = 30_000_014_u32; // Shamahi (2.10 LY)

    let c_cyno = 91_000_101_i64;
    let s_cyno = 101_000_101_i64;
    let c_bo = 91_000_102_i64;
    let s_bo = 101_000_102_i64;
    let bo_pos = DVec3::new(100_000.0, 0.0, 0.0);

    let mut sess_cyno = setup_pilot(&ctx, c_cyno, s_cyno, 12034, sys_b, DVec3::new(100_000.0, 0.0, 0.0), 1_000_009).await;
    let mut sess_bo = setup_pilot(&ctx, c_bo, s_bo, 22428, sys_a, bo_pos, 1_000_009).await;

    // 6 eligible covert passenger hulls + 2 ineligible hulls (Tengu WITHOUT covert subsystem & Rifter)
    let passengers = [
        (91_000_111_i64, 101_000_111_i64, 12034_u32, true),  // Stealth Bomber (Hound, 1.455M kg)
        (91_000_112_i64, 101_000_112_i64, 11188_u32, true),  // Covert Ops Frigate (Anathema)
        (91_000_113_i64, 101_000_113_i64, 11965_u32, true),  // Force Recon (Pilgrim)
        (91_000_114_i64, 101_000_114_i64, 12729_u32, true),  // Blockade Runner (Crane, 11M kg)
        (91_000_115_i64, 101_000_115_i64, 29984_u32, true),  // Strategic Cruiser (Tengu WITH 45589 Covert Reconfiguration)
        (91_000_118_i64, 101_000_118_i64, 22428_u32, true),  // Black Ops (Redeemer)
        (91_000_116_i64, 101_000_116_i64, 29984_u32, false), // Strategic Cruiser WITHOUT covert subsystem -> rejected!
        (91_000_117_i64, 101_000_117_i64, 587_u32, false),   // Non-covert Rifter -> rejected!
    ];

    let mut p_sessions = Vec::new();
    for (idx, &(cid, sid, tid, _)) in passengers.iter().enumerate() {
        let mut ps = setup_pilot(
            &ctx,
            cid,
            sid,
            tid,
            sys_a,
            bo_pos + DVec3::new(300.0 + (idx as f64) * 100.0, 0.0, 0.0),
            1_000_009,
        )
        .await;
        ps.fleet_id = Some(999);
        p_sessions.push(ps);
    }
    sess_cyno.fleet_id = Some(999);
    sess_bo.fleet_id = Some(999);

    let cyno_mod = 101_000_901_i64;
    let portal_mod = 101_000_902_i64;
    let bo_fuel_bay_id = 101_000_903_i64;
    let bo_cargo_fuel_id = 101_000_904_i64;
    let tengu_cov_sub_id = 101_000_905_i64;

    ctx.db.with_mut_snapshot(|s| {
        let mut members = vec![c_cyno, c_bo];
        members.extend(passengers.iter().map(|p| p.0));
        s.fleets.insert(
            999,
            eve_db::FleetRecord { fleet_id: 999, boss_id: c_bo, members, ..Default::default() },
        );
        s.skills.entry(c_cyno).or_default().insert(21603, TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
        s.skills.entry(c_bo).or_default().insert(24562, TrainedSkillRecord { type_id: 24562, level: 5, skill_points: 256_000.0 });
        s.skills.entry(c_bo).or_default().insert(21610, TrainedSkillRecord { type_id: 21610, level: 4, skill_points: 90_510.0 }); // Jump Fuel Conservation IV
        for t3_cid in [91_000_115_i64, 91_000_116_i64] {
            s.skills.entry(t3_cid).or_default().insert(30544, TrainedSkillRecord { type_id: 30544, level: 5, skill_points: 256_000.0 }); // Caldari Defensive Systems V
        }

        s.items.insert(
            cyno_mod,
            InventoryItemRecord {
                item_id: cyno_mod, type_id: 28646, owner_id: c_cyno, location_id: s_cyno,
                flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
            },
        );
        s.items.insert(
            101_000_910,
            InventoryItemRecord {
                item_id: 101_000_910, type_id: 16273, owner_id: c_cyno, location_id: s_cyno,
                flag: InventoryFlag::Cargo, quantity: 200, singleton: false, ..Default::default()
            },
        );
        s.items.insert(
            portal_mod,
            InventoryItemRecord {
                item_id: portal_mod, type_id: 28652, owner_id: c_bo, location_id: s_bo,
                flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
            },
        );
        // Put Helium Isotopes (16274) in BOTH SpecializedFuelBay and Cargo to verify SpecializedFuelBay priority
        s.items.insert(
            bo_fuel_bay_id,
            InventoryItemRecord {
                item_id: bo_fuel_bay_id, type_id: 16274, owner_id: c_bo, location_id: s_bo,
                flag: InventoryFlag::SpecializedFuelBay, quantity: 100_000, singleton: false, ..Default::default()
            },
        );
        s.items.insert(
            bo_cargo_fuel_id,
            InventoryItemRecord {
                item_id: bo_cargo_fuel_id, type_id: 16274, owner_id: c_bo, location_id: s_bo,
                flag: InventoryFlag::Cargo, quantity: 20_000, singleton: false, ..Default::default()
            },
        );
        // Fit Tengu Defensive - Covert Reconfiguration (45589) ONLY on passenger #4 (101_000_115)
        s.items.insert(
            tengu_cov_sub_id,
            InventoryItemRecord {
                item_id: tengu_cov_sub_id,
                type_id: 45589,
                owner_id: 91_000_115,
                location_id: 101_000_115,
                flag: InventoryFlag::SubSystemSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.module_online_states.insert(tengu_cov_sub_id, true);
    });
    ctx.invalidate_ship_fitting(101_000_115);
    ctx.invalidate_ship_fitting(101_000_116);

    let cyno_rep = ctx
        .dispatch_rpc(&mut sess_cyno, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod)], &kw, None)
        .await
        .expect("Light Covert Cyno");
    let cov_beacon_id = kv_get(&cyno_rep, "beaconID").and_then(|v| v.as_i64()).unwrap();

    // Negative check: Covert Jump Portal Generator cannot bridge to a normal non-covert cyno!
    let c_norm = 91_000_199_i64;
    let s_norm = 101_000_199_i64;
    let mut sess_norm = setup_pilot(&ctx, c_norm, s_norm, 11965, sys_b, DVec3::new(300_000.0, 0.0, 0.0), 1_000_009).await;
    sess_norm.fleet_id = Some(999);
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.get_mut(&999).unwrap().members.push(c_norm);
        s.skills.entry(c_norm).or_default().insert(21603, TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
        s.items.insert(101_000_998, InventoryItemRecord { item_id: 101_000_998, type_id: 21096, owner_id: c_norm, location_id: s_norm, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default() });
        s.items.insert(101_000_999, InventoryItemRecord { item_id: 101_000_999, type_id: 16273, owner_id: c_norm, location_id: s_norm, flag: InventoryFlag::Cargo, quantity: 500, singleton: false, ..Default::default() });
    });
    let norm_rep = ctx.dispatch_rpc(&mut sess_norm, "dogmaIM", None, "Activate", &[PyRep::Integer(101_000_998)], &kw, None).await.unwrap();
    let norm_beacon_id = kv_get(&norm_rep, "beaconID").and_then(|v| v.as_i64()).unwrap();
    let norm_bridge_err = ctx
        .dispatch_rpc(
            &mut sess_bo,
            "beyonce",
            None,
            "CmdBridgeToFleetModuleBeacon",
            &[PyRep::Integer(c_norm), PyRep::Integer(sys_b as i64), PyRep::Integer(norm_beacon_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Covert Jump Portal Generator must NEVER bridge onto a normal cyno");
    assert_eq!(norm_bridge_err.code, "ShipCannotBridgeToBeacon");

    // Open Covert Jump Portal to Covert Cynosural Field
    ctx.dispatch_rpc(
        &mut sess_bo,
        "beyonce",
        None,
        "CmdBridgeToFleetModuleBeacon",
        &[PyRep::Integer(c_cyno), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id)],
        &kw,
        None,
    )
    .await
    .expect("Black Ops opens Covert Jump Portal to Covert Cyno");

    // Non-covert Tengu (#6) and Rifter (#7) are rejected with JumpBridgeRestrictedShip
    for bad_idx in [6_usize, 7_usize] {
        let err = ctx
            .dispatch_rpc(
                &mut p_sessions[bad_idx],
                "beyonce",
                None,
                "CmdJumpThroughFleet",
                &[PyRep::Integer(c_bo), PyRep::Integer(s_bo)],
                &kw,
                None,
            )
            .await
            .expect_err("Non-covert hull must be rejected by Covert Jump Portal");
        assert_eq!(err.code, "JumpBridgeRestrictedShip");
    }

    // All 6 covert-capable hull classes (Stealth Bomber, Covert Ops, Force Recon, Blockade Runner, Covert Tengu, Black Ops) jump through!
    let mut fuels_by_hull = Vec::new();
    for ok_idx in 0..6_usize {
        let fb_before = ctx.db.with_snapshot(|s| s.items.get(&bo_fuel_bay_id).unwrap().quantity);
        let jump_rep = ctx
            .dispatch_rpc(
                &mut p_sessions[ok_idx],
                "beyonce",
                None,
                "CmdJumpThroughFleet",
                &[PyRep::Integer(c_bo), PyRep::Integer(s_bo)],
                &kw,
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("Eligible covert passenger #{ok_idx} failed: {e:?}"));
        let consumed = kv_get(&jump_rep, "fuelConsumed").and_then(|v| v.as_i64()).unwrap();
        let fb_after = ctx.db.with_snapshot(|s| s.items.get(&bo_fuel_bay_id).unwrap().quantity);
        assert_eq!((fb_before - fb_after) as i64, consumed, "Fuel deducted from Black Ops SpecializedFuelBay first");
        assert_eq!(p_sessions[ok_idx].solar_system_id, Some(sys_b));
        fuels_by_hull.push(consumed);
    }

    // Cargo isotopes untouched because SpecializedFuelBay had plenty, and heavier Crane (11M kg) used >7x more isotopes than Hound (1.455M kg)!
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&bo_cargo_fuel_id).unwrap().quantity), 20_000);
    assert!(
        fuels_by_hull[3] > fuels_by_hull[0] * 6,
        "Isotope consumption scales with ship mass * LY: Crane={} vs Hound={}",
        fuels_by_hull[3],
        fuels_by_hull[0]
    );

    // Bubbled vs point-scrambled Black Ops host (`B39-REV-P0-01`):
    // Inside a warp disruption bubble (`warp_scramble_strength == 0`), jumping self is blocked (`WarpScrambled`),
    // whereas opening a Covert Jump Portal (`CmdBridgeToFleetModuleBeacon`) SUCCEEDS; targeted point-scramble blocks both!
    ctx.advance_secs(65);
    let cyno_rep2 = ctx.dispatch_rpc(&mut sess_cyno, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod)], &kw, None).await.expect("Re-light Covert Cyno");
    let cov_beacon_id2 = kv_get(&cyno_rep2, "beaconID").and_then(|v| v.as_i64()).unwrap();
    let sys_a_h = ctx.systems.get_or_spawn(sys_a);
    assert!(sys_a_h.add_interdiction_sphere(101_000_977, bo_pos, 20_000.0).await.unwrap());
    assert!(sys_a_h.is_ball_scrambled(s_bo).await.unwrap(), "Black Ops is inside warp disruption bubble");
    let bubbled_jump_err = ctx
        .dispatch_rpc(
            &mut sess_bo,
            "beyonce",
            None,
            "CmdJumpToFleetModuleBeacon",
            &[PyRep::Integer(c_cyno), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id2)],
            &kw,
            None,
        )
        .await
        .expect_err("Self-jumping out of warp disruption bubble must fail");
    assert_eq!(bubbled_jump_err.code, "WarpScrambled");

    ctx.dispatch_rpc(
        &mut sess_bo,
        "beyonce",
        None,
        "CmdBridgeToFleetModuleBeacon",
        &[PyRep::Integer(c_cyno), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id2)],
        &kw,
        None,
    )
    .await
    .expect("Non-point-scrambled Black Ops inside a warp disruption bubble opens Covert Jump Portal");

    sys_a_h.set_warp_scramble_strength(s_bo, 2).await.unwrap();
    let point_scram_err = ctx
        .dispatch_rpc(
            &mut sess_bo,
            "beyonce",
            None,
            "CmdBridgeToFleetModuleBeacon",
            &[PyRep::Integer(c_cyno), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id2)],
            &kw,
            None,
        )
        .await
        .expect_err("Targeted warp scrambler must block Covert Jump Portal activation");
    assert_eq!(point_scram_err.code, "WarpScrambled");
}
