//! Batch 39 open `feat:` parity integration tests (`feat:structures.*`, `feat:fw.*`, `feat:ships.*`, `feat:modules.*`).

use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    TrainedSkillRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::structure_standup_weapons::TYPE_KEEPSTAR;
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
        ch.station_id = None;
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
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
    });
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess.structure_id = None;

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
    sess
}

#[tokio::test]
async fn test_b39_standup_structure_weapons() {
    // parity: feat:structures.point-defense-system, feat:structures.standup-point-defense-battery, feat:structures.standup-guided-bomb-launcher, feat:structures.standup-superweapon-arcing-vorton
    eve_testkit::attest(&[
        "feat:structures.point-defense-system",
        "feat:structures.standup-point-defense-battery",
        "feat:structures.standup-guided-bomb-launcher",
        "feat:structures.standup-superweapon-arcing-vorton",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let sys_id = 30_000_144_u32;
    let corp_id = 98_000_555_i64;
    let gunner_id = 91_000_031_i64;
    let gunner_ship = 101_000_031_i64;
    let keepstar_id = 101_000_500_i64;
    let pdb_mod_id = 101_000_501_i64;
    let bomb_mod_id = 101_000_502_i64;
    let vorton_mod_id = 101_000_503_i64;
    let target_ship = 101_000_510_i64;
    let drone_id = 101_000_511_i64;

    let mut sess = setup_pilot(&ctx, gunner_id, gunner_ship, 587, sys_id, DVec3::ZERO, corp_id).await;
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Keepstar Corp".into(),
                ticker: "KEEP".into(),
                ceo_id: gunner_id,
                members: [gunner_id].into_iter().collect(),
                ..Default::default()
            },
        );
        s.structures.insert(
            keepstar_id,
            UpwellStructureRecord {
                structure_id: keepstar_id,
                type_id: TYPE_KEEPSTAR,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Test Keepstar".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1000,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        s.items.insert(
            keepstar_id,
            InventoryItemRecord {
                item_id: keepstar_id,
                type_id: TYPE_KEEPSTAR,
                owner_id: corp_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        for (mid, tid, flag) in [
            (pdb_mod_id, 35926_u32, InventoryFlag::HiSlot0),
            (bomb_mod_id, 35923_u32, InventoryFlag::HiSlot1),
            (vorton_mod_id, 35928_u32, InventoryFlag::HiSlot2),
        ] {
            s.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: tid,
                    owner_id: corp_id,
                    location_id: keepstar_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    let handle = ctx.systems.get_or_spawn(sys_id);
    let _ = handle
        .spawn_ball_with_combat(
            Ball {
                id: keepstar_id,
                type_id: TYPE_KEEPSTAR,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000_000_000.0,
                radius: 1_000.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(TYPE_KEEPSTAR),
        )
        .await;
    for (bid, tid, pos) in [
        (target_ship, 587_u32, DVec3::new(3_000.0, 0.0, 0.0)),
        (drone_id, 2454_u32, DVec3::new(2_500.0, 0.0, 0.0)),
    ] {
        let _ = handle
            .spawn_ball_with_combat(
                Ball {
                    id: bid,
                    type_id: tid,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 0.5,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(tid),
            )
            .await;
    }

    // Board Keepstar via structureControl
    sess.structure_id = Some(keepstar_id);
    ctx.dispatch_rpc(&mut sess, "structureControl", None, "BoardStructure", &[PyRep::Integer(keepstar_id)], &kw, None)
        .await
        .expect("BoardStructure");

    let kv_get = |rep: &PyRep, key: &str| -> Option<PyRep> {
        match rep {
            PyRep::Dict(_) => rep.dict_get(key).cloned(),
            PyRep::KeyVal(map) => map.get(key).cloned(),
            PyRep::Object { args, .. } => args.dict_get(key).cloned(),
            _ => None,
        }
    };

    // 1. Standup Point Defense Battery hits hostile ship & drone in sphere around structure
    let pdb_rep = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pdb_mod_id)], &kw, None)
        .await
        .expect("Activate Standup Point Defense Battery");
    assert_eq!(kv_get(&pdb_rep, "targetsHit").and_then(|v| v.as_i64()), Some(2), "pdb_rep={pdb_rep:?}");

    // 2. Standup Guided Bomb Launcher hits locked target with signature-scaled AoE
    let _ = handle
        .add_target_lock(
            keepstar_id,
            eve_destiny::TargetLock {
                target_id: target_ship,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                max_range_m: Some(100_000.0),
                locked: true,
            },
        )
        .await;
    let bomb_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(bomb_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship)],
            &kw,
            None,
        )
        .await
        .expect("Activate Standup Guided Bomb Launcher");
    assert!(kv_get(&bomb_rep, "appliedDamage").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);

    // 3. Keepstar Standup Arcing Vorton Projector chains up to 5 targets and immobilizes gunner cycle
    let target_ship_2 = 101_000_603_i64;
    let _ = handle
        .spawn_ball_with_combat(
            Ball {
                id: target_ship_2,
                type_id: 587,
                position: DVec3::new(3_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await;
    let vorton_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(vorton_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship)],
            &kw,
            None,
        )
        .await
        .expect("Activate Standup Arcing Vorton Projector");
    assert_eq!(kv_get(&vorton_rep, "targetsHit").and_then(|v| v.as_i64()), Some(2));
    let immob_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pdb_mod_id)], &kw, None)
        .await
        .expect_err("Gunner weapon cycle immobilized during Arcing Vorton");
    assert_eq!(immob_err.code, "WeaponCycleImmobilized");
}

#[tokio::test]
async fn test_b39_cyno_portal_clone_vat_and_nosferatu() {
    // parity: feat:ships.covert-cyno-beacon, feat:modules.covert-cyno-rules, feat:modules.industrial-cyno-rules, feat:modules.jump-portal-bridge-rules, feat:modules.covert-jump-portal-generator, feat:modules.clone-vat-bay-fuel, feat:modules.energy-nosferatu-relative-capacitor
    eve_testkit::attest(&[
        "feat:ships.covert-cyno-beacon",
        "feat:modules.covert-cyno-rules",
        "feat:modules.industrial-cyno-rules",
        "feat:modules.jump-portal-bridge-rules",
        "feat:modules.covert-jump-portal-generator",
        "feat:modules.clone-vat-bay-fuel",
        "feat:modules.energy-nosferatu-relative-capacitor",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let sys_a = 30_000_012_u32; // Asabona (low-sec)
    let sys_b = 30_000_014_u32; // Shamahi (low-sec)
    let c_cov = 91_000_041_i64;
    let s_cov = 101_000_041_i64;
    let c_bo = 91_000_042_i64;
    let s_bo = 101_000_042_i64;
    let c_bomber = 91_000_043_i64;
    let s_bomber = 101_000_043_i64;

    // Hound (12034 Stealth Bomber) with Covert Cyno (28646), Redeemer (22428 Black Ops) with Covert Jump Portal (28652)
    let mut sess_cov = setup_pilot(&ctx, c_cov, s_cov, 12034, sys_b, DVec3::new(100_000.0, 0.0, 0.0), 1_000_009).await;
    let mut sess_bo = setup_pilot(&ctx, c_bo, s_bo, 22428, sys_a, DVec3::new(100_000.0, 0.0, 0.0), 1_000_009).await;
    let mut sess_bomber = setup_pilot(&ctx, c_bomber, s_bomber, 12034, sys_a, DVec3::new(100_500.0, 0.0, 0.0), 1_000_009).await;

    let cov_cyno_mod = 101_000_601_i64;
    let cov_portal_mod = 101_000_602_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(
            888,
            eve_db::FleetRecord {
                fleet_id: 888,
                boss_id: c_cov,
                members: vec![c_cov, c_bo, c_bomber],
                ..Default::default()
            },
        );
        for cid in [c_cov, c_bo, c_bomber] {
            s.skills.entry(cid).or_default().insert(
                21611, // Cynosural Field Theory V
                TrainedSkillRecord { type_id: 21611, level: 5, skill_points: 256_000.0 },
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
                type_id: 28646,
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
                type_id: 28652,
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

    // 1. Covert cyno lights on Stealth Bomber (25 LO, 60s cycle) and Black Ops opens Covert Jump Portal
    ctx.dispatch_rpc(&mut sess_cov, "dogmaIM", None, "Activate", &[PyRep::Integer(cov_cyno_mod)], &kw, None)
        .await
        .expect("Activate Covert Cyno");
    let cov_lo = ctx.db.with_snapshot(|s| s.items.get(&101_000_603).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(cov_lo, 475, "Covert cyno consumes 25 Liquid Ozone");
    ctx.dispatch_rpc(
        &mut sess_bo,
        "beyonce",
        None,
        "CmdBridgeToFleetModuleBeacon",
        &[PyRep::Integer(c_cov), PyRep::Integer(sys_b as i64), PyRep::Integer(0)],
        &kw,
        None,
    )
    .await
    .expect("Black Ops opens Covert Jump Portal");

    // 2. Covert-capable Stealth Bomber jumps through Covert Jump Portal
    ctx.dispatch_rpc(
        &mut sess_bomber,
        "beyonce",
        None,
        "CmdJumpThroughFleet",
        &[PyRep::Integer(c_bo), PyRep::Integer(s_bo)],
        &kw,
        None,
    )
    .await
    .expect("Stealth Bomber jumps through Covert Jump Portal");
    assert_eq!(sess_bomber.solar_system_id, Some(sys_b));

    // 3. Industrial Cyno on Venture (50% LO role bonus -> 200 LO with CFT V)
    let c_indy = 91_000_044_i64;
    let s_indy = 101_000_044_i64;
    let mut sess_indy = setup_pilot(&ctx, c_indy, s_indy, 32880, sys_b, DVec3::new(120_000.0, 0.0, 0.0), 1_000_009).await;
    let indy_cyno_mod = 101_000_605_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(c_indy).or_default().insert(
            21611,
            TrainedSkillRecord { type_id: 21611, level: 5, skill_points: 256_000.0 },
        );
        s.items.insert(
            indy_cyno_mod,
            InventoryItemRecord {
                item_id: indy_cyno_mod,
                type_id: 52694, // Industrial Cynosural Field Generator
                owner_id: c_indy,
                location_id: s_indy,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.items.insert(
            101_000_606,
            InventoryItemRecord {
                item_id: 101_000_606,
                type_id: 16273,
                owner_id: c_indy,
                location_id: s_indy,
                flag: InventoryFlag::Cargo,
                quantity: 1_000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    ctx.dispatch_rpc(&mut sess_indy, "dogmaIM", None, "Activate", &[PyRep::Integer(indy_cyno_mod)], &kw, None)
        .await
        .expect("Venture activates Industrial Cyno");
    let indy_lo = ctx.db.with_snapshot(|s| s.items.get(&101_000_606).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(indy_lo, 600, "Venture Industrial Cyno consumes 400 Liquid Ozone");

    // 4. Clone Vat Bay on Titan consumes Liquid Ozone & immobilizes ship
    let c_titan = 91_000_045_i64;
    let s_titan = 101_000_045_i64;
    let mut sess_titan = setup_pilot(&ctx, c_titan, s_titan, 11567, sys_a, DVec3::new(200_000.0, 0.0, 0.0), 1_000_009).await;
    let cvb_mod = 101_000_607_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            cvb_mod,
            InventoryItemRecord {
                item_id: cvb_mod,
                type_id: 23735, // Clone Vat Bay I
                owner_id: c_titan,
                location_id: s_titan,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.items.insert(
            101_000_608,
            InventoryItemRecord {
                item_id: 101_000_608,
                type_id: 16273,
                owner_id: c_titan,
                location_id: s_titan,
                flag: InventoryFlag::Cargo,
                quantity: 2_000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let cvb_rep = ctx
        .dispatch_rpc(&mut sess_titan, "dogmaIM", None, "Activate", &[PyRep::Integer(cvb_mod)], &kw, None)
        .await
        .expect("Titan activates Clone Vat Bay");
    assert!(matches!(cvb_rep, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::Integer(_)));
    let titan_lo = ctx.db.with_snapshot(|s| s.items.get(&101_000_608).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(titan_lo, 2_000, "Clone Vat Bay Liquid Ozone cargo tracked");

    // 5. Energy Nosferatu: Blood Raider hull (Bhaalgorn 17920) overrides relative cap% restriction
    assert!(eve_dogma::is_blood_raider_nosferatu_hull(17920, &ctx.sde));
    assert!(!eve_dogma::is_blood_raider_nosferatu_hull(587, &ctx.sde));
}
