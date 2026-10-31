//! `open_feats_b39_part4` — Cyno and Jump Portal Bridge Rules (`feat:modules.covert-cyno-rules`,
//! `feat:modules.industrial-cyno-rules`, `feat:modules.jump-portal-bridge-rules`, `feat:modules.covert-jump-portal-generator`).

use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
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
async fn test_b39_cyno_and_portal_rules() {
    // parity: feat:modules.covert-cyno-rules, feat:modules.industrial-cyno-rules, feat:modules.jump-portal-bridge-rules, feat:modules.covert-jump-portal-generator
    eve_testkit::attest(&[
        "feat:modules.covert-cyno-rules",
        "feat:modules.industrial-cyno-rules",
        "feat:modules.jump-portal-bridge-rules",
        "feat:modules.covert-jump-portal-generator",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let sys_a = 30_000_012_u32; // Asabona (low-sec)
    let sys_b = 30_000_014_u32; // Shamahi (low-sec, 2.10 LY from Asabona)
    let c_cov = 91_000_041_i64;
    let s_cov = 101_000_041_i64;
    let c_bo = 91_000_042_i64;
    let s_bo = 101_000_042_i64;
    let c_bomber = 91_000_043_i64;
    let s_bomber = 101_000_043_i64;
    let c_rifter = 91_000_046_i64;
    let s_rifter = 101_000_046_i64;

    // Hound (12034 Stealth Bomber) in sys_b, Redeemer (22428 Black Ops) in sys_a
    let mut sess_cov = setup_pilot(&ctx, c_cov, s_cov, 12034, sys_b, DVec3::new(100_000.0, 0.0, 0.0), 1_000_009).await;
    let mut sess_bo = setup_pilot(&ctx, c_bo, s_bo, 22428, sys_a, DVec3::new(100_000.0, 0.0, 0.0), 1_000_009).await;
    let mut sess_bomber = setup_pilot(&ctx, c_bomber, s_bomber, 12034, sys_a, DVec3::new(100_500.0, 0.0, 0.0), 1_000_009).await;
    let mut sess_rifter = setup_pilot(&ctx, c_rifter, s_rifter, 587, sys_a, DVec3::new(100_500.0, 0.0, 0.0), 1_000_009).await;

    let cov_cyno_mod = 101_000_601_i64;
    let cov_portal_mod = 101_000_602_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(
            888,
            eve_db::FleetRecord {
                fleet_id: 888,
                boss_id: c_cov,
                members: vec![c_cov, c_bo, c_bomber, c_rifter],
                ..Default::default()
            },
        );
        for cid in [c_cov, c_bo, c_bomber, c_rifter] {
            s.skills.entry(cid).or_default().insert(
                21603, // Cynosural Field Theory V
                TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 },
            );
            s.skills.entry(cid).or_default().insert(
                3456, // Jump Fuel Conservation V
                TrainedSkillRecord { type_id: 3456, level: 5, skill_points: 256_000.0 },
            );
            s.skills.entry(cid).or_default().insert(
                24562, // Jump Portal Generation V
                TrainedSkillRecord { type_id: 24562, level: 5, skill_points: 256_000.0 },
            );
        }
        s.items.insert(
            cov_cyno_mod,
            InventoryItemRecord {
                item_id: cov_cyno_mod,
                type_id: 28646, // Covert Cynosural Field Generator I
                owner_id: c_cov,
                location_id: s_cov,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.items.insert(
            101_000_603,
            InventoryItemRecord {
                item_id: 101_000_603,
                type_id: 16273, // Liquid Ozone
                owner_id: c_cov,
                location_id: s_cov,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                ..Default::default()
            },
        );
        s.items.insert(
            cov_portal_mod,
            InventoryItemRecord {
                item_id: cov_portal_mod,
                type_id: 28652, // Covert Jump Portal Generator I
                owner_id: c_bo,
                location_id: s_bo,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.items.insert(
            101_000_604,
            InventoryItemRecord {
                item_id: 101_000_604,
                type_id: 16274, // Helium Isotopes
                owner_id: c_bo,
                location_id: s_bo,
                flag: InventoryFlag::Cargo,
                quantity: 50_000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    sess_cov.fleet_id = Some(888);
    sess_bo.fleet_id = Some(888);
    sess_bomber.fleet_id = Some(888);
    sess_rifter.fleet_id = Some(888);

    // 1. Covert Cyno: CFT IV missing skill negative test
    ctx.db.with_mut_snapshot(|s| {
        s.skills.get_mut(&c_cov).unwrap().get_mut(&21603).unwrap().level = 4;
    });
    let sk_err = ctx.dispatch_rpc(&mut sess_cov, "dogmaIM", None, "Activate", &[PyRep::Integer(cov_cyno_mod)], &kw, None).await.unwrap_err();
    assert_eq!(sk_err.code, "SkillMissing");
    ctx.db.with_mut_snapshot(|s| {
        s.skills.get_mut(&c_cov).unwrap().get_mut(&21603).unwrap().level = 5;
    });

    // Hull restriction: Rifter cannot operate Covert Cyno
    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&cov_cyno_mod).unwrap().location_id = s_rifter;
        s.items.get_mut(&cov_cyno_mod).unwrap().owner_id = c_rifter;
    });
    let hull_err = ctx.dispatch_rpc(&mut sess_rifter, "dogmaIM", None, "Activate", &[PyRep::Integer(cov_cyno_mod)], &kw, None).await.unwrap_err();
    assert_eq!(hull_err.code, "CannotLightCynoInSystem");
    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&cov_cyno_mod).unwrap().location_id = s_cov;
        s.items.get_mut(&cov_cyno_mod).unwrap().owner_id = c_cov;
    });

    // Cyno jammer bypass: covert cyno succeeds even if cyno jammer is active in system
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(999_001, InventoryItemRecord {
            item_id: 999_001, type_id: 33700, owner_id: 1, location_id: sys_b as i64, flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
        });
    });

    let kv_get = |rep: &PyRep, key: &str| -> Option<PyRep> {
        match rep {
            PyRep::Dict(_) => rep.dict_get(key).cloned(),
            PyRep::KeyVal(map) => map.get(key).cloned(),
            PyRep::Object { args, .. } => args.dict_get(key).cloned(),
            _ => None,
        }
    };

    // Light Covert Cyno on Hound: uses 25 LO (1/10th), on-grid-only visibility
    let cov_cyno_rep = ctx.dispatch_rpc(&mut sess_cov, "dogmaIM", None, "Activate", &[PyRep::Integer(cov_cyno_mod)], &kw, None).await.expect("Light Covert Cyno");
    let cov_beacon_id = match &cov_cyno_rep {
        PyRep::KeyVal(m) => m.get("beaconID").and_then(|v| v.as_i64()).unwrap(),
        _ => panic!("Expected keyval"),
    };
    let cov_lo = ctx.db.with_snapshot(|s| s.items.get(&101_000_603).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(cov_lo, 475, "Covert cyno consumes 25 Liquid Ozone (500 - 25 = 475)");
    assert_eq!(kv_get(&cov_cyno_rep, "isCovert"), Some(PyRep::Bool(true)));
    assert_eq!(kv_get(&cov_cyno_rep, "onGridOnly"), Some(PyRep::Bool(true)));

    // Non-Black Ops jump refusal: Avatar Titan cannot jump to Covert Cyno
    let c_titan = 91_000_047_i64;
    let s_titan = 101_000_047_i64;
    let mut sess_titan = setup_pilot(&ctx, c_titan, s_titan, 11567, sys_a, DVec3::new(100_000.0, 0.0, 0.0), 1_000_009).await;
    sess_titan.fleet_id = Some(888);
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.get_mut(&888).unwrap().members.push(c_titan);
        s.skills.entry(c_titan).or_default().insert(3456, TrainedSkillRecord { type_id: 3456, level: 5, skill_points: 256_000.0 });
    });
    let titan_jump_err = ctx.dispatch_rpc(&mut sess_titan, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(c_cov), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id)], &kw, None).await.unwrap_err();
    assert_eq!(titan_jump_err.code, "ShipCannotJumpToBeacon");

    // 2. Covert Jump Portal Generator on Redeemer Black Ops
    ctx.dispatch_rpc(&mut sess_bo, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(c_cov), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id)], &kw, None).await.expect("Redeemer opens Covert Jump Portal");

    // Passenger range check: jumper beyond 2,500 m is refused
    let c_far = 91_000_048_i64;
    let s_far = 101_000_048_i64;
    let mut sess_far = setup_pilot(&ctx, c_far, s_far, 12034, sys_a, DVec3::new(105_000.0, 0.0, 0.0), 1_000_009).await; // 5 km away
    sess_far.fleet_id = Some(888);
    ctx.db.with_mut_snapshot(|s| s.fleets.get_mut(&888).unwrap().members.push(c_far));
    let far_err = ctx.dispatch_rpc(&mut sess_far, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(c_bo), PyRep::Integer(s_bo)], &kw, None).await.unwrap_err();
    assert_eq!(far_err.code, "OutOfPortalRange");

    // Warp scramble / bubble check: scrambled passenger cannot take portal
    let handle_a = ctx.systems.get_or_spawn(sys_a);
    let _ = handle_a.set_warp_scramble_strength(s_bomber, 2).await;
    let scram_err = ctx.dispatch_rpc(&mut sess_bomber, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(c_bo), PyRep::Integer(s_bo)], &kw, None).await.unwrap_err();
    assert_eq!(scram_err.code, "WarpScrambled");
    let _ = handle_a.set_warp_scramble_strength(s_bomber, 0).await;

    // Non-covert passenger (Rifter) is refused
    let rifter_err = ctx.dispatch_rpc(&mut sess_rifter, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(c_bo), PyRep::Integer(s_bo)], &kw, None).await.unwrap_err();
    assert_eq!(rifter_err.code, "JumpBridgeRestrictedShip");

    // Eligible covert passenger (Stealth Bomber) takes portal -> consumes isotopes
    let bo_fuel_before = ctx.db.with_snapshot(|s| s.items.get(&101_000_604).map(|i| i.quantity).unwrap_or(0));
    let jump_rep = ctx.dispatch_rpc(&mut sess_bomber, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(c_bo), PyRep::Integer(s_bo)], &kw, None).await.expect("Stealth Bomber jumps through covert portal");
    let fuel_consumed = kv_get(&jump_rep, "fuelConsumed").and_then(|v| v.as_i64()).unwrap();
    let bo_fuel_after = ctx.db.with_snapshot(|s| s.items.get(&101_000_604).map(|i| i.quantity).unwrap_or(0));
    assert_eq!((bo_fuel_before - bo_fuel_after) as i64, fuel_consumed);
    assert!(fuel_consumed > 0, "Consumed isotope fuel from Black Ops");

    // 3. Industrial Cyno Rules
    let c_indy = 91_000_044_i64;
    let s_venture = 101_000_044_i64;
    let mut sess_venture = setup_pilot(&ctx, c_indy, s_venture, 32880, sys_b, DVec3::new(200_000.0, 0.0, 0.0), 1_000_009).await; // Venture
    sess_venture.fleet_id = Some(888);
    ctx.db.with_mut_snapshot(|s| s.fleets.get_mut(&888).unwrap().members.push(c_indy));

    let indy_cyno_mod = 101_000_605_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(c_indy).or_default().insert(21603, TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
        s.items.insert(indy_cyno_mod, InventoryItemRecord {
            item_id: indy_cyno_mod, type_id: 52694, owner_id: c_indy, location_id: s_venture, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
        });
        s.items.insert(101_000_606, InventoryItemRecord {
            item_id: 101_000_606, type_id: 16273, owner_id: c_indy, location_id: s_venture, flag: InventoryFlag::Cargo, quantity: 1_000, singleton: false, ..Default::default()
        });
    });

    // Industrial cyno is blocked by cyno jammer
    let jam_err = ctx.dispatch_rpc(&mut sess_venture, "dogmaIM", None, "Activate", &[PyRep::Integer(indy_cyno_mod)], &kw, None).await.unwrap_err();
    assert_eq!(jam_err.code, "CynoJammed");
    ctx.db.with_mut_snapshot(|s| { s.items.remove(&999_001); });

    // Venture role bonus: 800 base * 0.5 (CFT V) * 0.5 (Venture role bonus) = 200 LO
    let venture_rep = ctx.dispatch_rpc(&mut sess_venture, "dogmaIM", None, "Activate", &[PyRep::Integer(indy_cyno_mod)], &kw, None).await.expect("Light Industrial Cyno on Venture");
    let indy_beacon_id = kv_get(&venture_rep, "beaconID").and_then(|v| v.as_i64()).unwrap();
    let venture_lo = ctx.db.with_snapshot(|s| s.items.get(&101_000_606).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(venture_lo, 800, "Venture consumes 200 Liquid Ozone with role bonus (1000 - 200 = 800)");
    assert_eq!(kv_get(&venture_rep, "onGridOnly"), Some(PyRep::Bool(false)), "Industrial cyno appears on overview");

    // Hauler (Badger 648) without Venture role bonus: 800 base * 0.5 = 400 LO
    let c_badger = 91_000_045_i64;
    let s_badger = 101_000_049_i64;
    let mut sess_badger = setup_pilot(&ctx, c_badger, s_badger, 648, sys_b, DVec3::new(300_000.0, 0.0, 0.0), 1_000_009).await;
    sess_badger.fleet_id = Some(888);
    let indy_cyno_mod2 = 101_000_607_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.get_mut(&888).unwrap().members.push(c_badger);
        s.skills.entry(c_badger).or_default().insert(21603, TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
        s.items.insert(indy_cyno_mod2, InventoryItemRecord {
            item_id: indy_cyno_mod2, type_id: 52694, owner_id: c_badger, location_id: s_badger, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
        });
        s.items.insert(101_000_608, InventoryItemRecord {
            item_id: 101_000_608, type_id: 16273, owner_id: c_badger, location_id: s_badger, flag: InventoryFlag::Cargo, quantity: 1_000, singleton: false, ..Default::default()
        });
    });
    ctx.dispatch_rpc(&mut sess_badger, "dogmaIM", None, "Activate", &[PyRep::Integer(indy_cyno_mod2)], &kw, None).await.expect("Light Industrial Cyno on Hauler");
    let badger_lo = ctx.db.with_snapshot(|s| s.items.get(&101_000_608).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(badger_lo, 600, "Hauler consumes 400 Liquid Ozone with CFT V (1000 - 400 = 600)");

    // Black Ops (Redeemer) can jump to Industrial Cyno
    ctx.dispatch_rpc(&mut sess_bo, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(c_indy), PyRep::Integer(sys_b as i64), PyRep::Integer(indy_beacon_id)], &kw, None).await.expect("Black Ops jumps to Industrial Cyno");

    // Standard Combat Capital (Avatar Titan) cannot jump or bridge to Industrial Cyno
    let titan_indy_err = ctx.dispatch_rpc(&mut sess_titan, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(c_indy), PyRep::Integer(sys_b as i64), PyRep::Integer(indy_beacon_id)], &kw, None).await.unwrap_err();
    assert_eq!(titan_indy_err.code, "ShipCannotJumpToBeacon");

    // Titan bridge to industrial cyno refused
    let titan_portal_mod = 101_000_609_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(titan_portal_mod, InventoryItemRecord {
            item_id: titan_portal_mod, type_id: 23953, owner_id: c_titan, location_id: s_titan, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
        });
        s.items.insert(101_000_611, InventoryItemRecord {
            item_id: 101_000_611, type_id: 16275, owner_id: c_titan, location_id: s_titan, flag: InventoryFlag::Cargo, quantity: 5_000, singleton: false, ..Default::default()
        });
    });
    let titan_bridge_err = ctx.dispatch_rpc(&mut sess_titan, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(c_indy), PyRep::Integer(sys_b as i64), PyRep::Integer(indy_beacon_id)], &kw, None).await.unwrap_err();
    assert_eq!(titan_bridge_err.code, "ShipCannotBridgeToBeacon");
}
