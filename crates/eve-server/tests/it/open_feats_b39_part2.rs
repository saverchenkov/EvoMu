//! `open_feats_b39_part2` — Standup Structure Weapons (`feat:structures.standup-point-defense-battery`,
//! `feat:structures.standup-guided-bomb-launcher`, `feat:structures.standup-superweapon-arcing-vorton`).

use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::structure_standup_weapons::{
    TYPE_KEEPSTAR,
};
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
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(solar_system_id);

    ctx.db.with_mut_snapshot(|s| {
        s.corporations.entry(corp_id).or_default().member_roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR);
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
async fn test_b39_standup_structure_weapons() {
    // parity: feat:structures.standup-point-defense-battery, feat:structures.standup-guided-bomb-launcher, feat:structures.standup-superweapon-arcing-vorton
    eve_testkit::attest(&[
        "feat:structures.standup-point-defense-battery",
        "feat:structures.standup-guided-bomb-launcher",
        "feat:structures.standup-superweapon-arcing-vorton",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let hisec_sys = 30_000_144_u32; // Jita (0.95 high-sec)
    let null_sys = 30_000_199_u32;  // Poitot (0.0 null-sec)
    let corp_id = 98_000_555_i64;
    let gunner_id = 91_000_031_i64;
    let gunner_ship = 101_000_031_i64;
    let keepstar_id = 101_000_500_i64;
    let pdb_mod_id = 101_000_501_i64;
    let bomb_mod_id = 101_000_502_i64;
    let vorton_mod_id = 101_000_503_i64;
    let target_ship = 101_000_510_i64;
    let drone_id = 101_000_511_i64;

    let mut sess = setup_pilot(&ctx, gunner_id, gunner_ship, 587, hisec_sys, DVec3::ZERO, corp_id).await;
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
                solar_system_id: hisec_sys,
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
                location_id: hisec_sys as i64,
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

    let hi_handle = ctx.systems.get_or_spawn(hisec_sys);
    let _ = hi_handle
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

    // Board Keepstar
    sess.structure_id = Some(keepstar_id);
    ctx.dispatch_rpc(&mut sess, "structureControl", None, "BoardStructure", &[PyRep::Integer(keepstar_id)], &kw, None)
        .await
        .expect("BoardStructure");

    // 1. High-sec negative test: Standup PDB and Guided Bomb Launcher disallowed in high-sec
    let pdb_hi_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pdb_mod_id)], &kw, None)
        .await
        .expect_err("PDB disallowed in high-sec");
    assert_eq!(pdb_hi_err.code, "CannotActivateInHighSec");

    let bomb_hi_err = ctx
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
        .expect_err("Guided Bomb Launcher disallowed in high-sec");
    assert_eq!(bomb_hi_err.code, "CannotActivateInHighSec");

    // 2. Move Keepstar and gunner to null-sec (system 30_000_199)
    ctx.db.with_mut_snapshot(|s| {
        s.structures.get_mut(&keepstar_id).unwrap().solar_system_id = null_sys;
        s.items.get_mut(&keepstar_id).unwrap().location_id = null_sys as i64;
        s.characters.get_mut(&gunner_id).unwrap().solar_system_id = null_sys;
    });
    sess.solar_system_id = Some(null_sys);
    let null_handle = ctx.systems.get_or_spawn(null_sys);
    let _ = null_handle
        .spawn_ball_with_combat(
            Ball {
                id: keepstar_id,
                type_id: TYPE_KEEPSTAR,
                position: DVec3::new(10_000_000.0, 0.0, 0.0),
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
        (target_ship, 587_u32, DVec3::new(10_002_000.0, 0.0, 0.0)),
        (drone_id, 2454_u32, DVec3::new(10_002_200.0, 0.0, 0.0)),
    ] {
        let _ = null_handle
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

    let kv_get = |rep: &PyRep, key: &str| -> Option<PyRep> {
        match rep {
            PyRep::Dict(_) => rep.dict_get(key).cloned(),
            PyRep::KeyVal(map) => map.get(key).cloned(),
            PyRep::Object { args, .. } => args.dict_get(key).cloned(),
            _ => None,
        }
    };

    // 3. Standup Point Defense Battery: hits hostile ship & drone in short-range sphere
    let pdb_rep = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pdb_mod_id)], &kw, None)
        .await
        .expect("Activate Standup Point Defense Battery in null-sec");
    assert_eq!(kv_get(&pdb_rep, "targetsHit").and_then(|v| v.as_i64()), Some(2));
    // Rate limit: immediate re-activation rejected because module is cycling
    let pdb_dup_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pdb_mod_id)], &kw, None)
        .await
        .expect_err("PDB duplicate activation rejected during cycle");
    assert_eq!(pdb_dup_err.code, "ModuleAlreadyCycling");

    // Advance 13s -> PDB cycle completes
    ctx.advance_secs(13);

    // 4. Standup Guided Bomb Launcher: requires loaded charge and locked target
    let _ = null_handle
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

    // Firing without loaded guided bomb charge is rejected
    let no_ammo_err = ctx
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
        .expect_err("Guided Bomb Launcher without loaded ammo rejected");
    assert_eq!(no_ammo_err.code, "NoAmmoLoaded");

    // Load Standup Light Guided Bombs (type 37850) into Keepstar
    let bomb_item_id = 101_000_520_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            bomb_item_id,
            InventoryItemRecord {
                item_id: bomb_item_id,
                type_id: 37850, // Standup Light Guided Bomb
                owner_id: corp_id,
                location_id: keepstar_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
    });

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
    let small_sig_dmg = kv_get(&bomb_rep, "appliedDamage").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let small_sig_scale = kv_get(&bomb_rep, "signatureScale").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(small_sig_dmg > 0.0 && small_sig_scale <= 1.0, "Bomb damage applied with sig scaling ({small_sig_scale})");

    // Verify 1 bomb charge was consumed
    let remaining_bombs = ctx.db.with_snapshot(|s| s.items.get(&bomb_item_id).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(remaining_bombs, 9, "Consumed 1 guided bomb charge");

    // Rate limit: immediate re-activation rejected
    let bomb_dup_err = ctx
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
        .expect_err("Bomb Launcher duplicate activation rejected");
    assert_eq!(bomb_dup_err.code, "ModuleAlreadyCycling");

    // Advance 61s -> Bomb launcher cycle completes
    ctx.advance_secs(61);

    // Larger signature target (Raven battleship 638) takes higher signature-scaled bomb damage
    let bs_target = 101_000_610_i64;
    let _ = null_handle
        .spawn_ball_with_combat(
            Ball {
                id: bs_target,
                type_id: 638,
                position: DVec3::new(3_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 150.0,
                inertia: 0.5,
                mass: 100_000_000.0,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(638),
        )
        .await;
    let _ = null_handle
        .add_target_lock(
            keepstar_id,
            eve_destiny::TargetLock {
                target_id: bs_target,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                max_range_m: Some(100_000.0),
                locked: true,
            },
        )
        .await;
    let bomb_bs_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(bomb_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(bs_target)],
            &kw,
            None,
        )
        .await
        .expect("Activate Standup Guided Bomb Launcher on Battleship");
    let bs_sig_dmg = kv_get(&bomb_bs_rep, "appliedDamage").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(bs_sig_dmg > small_sig_dmg, "Larger signature target takes more bomb damage ({bs_sig_dmg} > {small_sig_dmg})");

    // 5. Standup Arcing Vorton Projector: Keepstar-only refusal check + 5-target chain
    ctx.db.with_mut_snapshot(|s| {
        s.structures.get_mut(&keepstar_id).unwrap().type_id = 35833; // Fortizar (not Keepstar)
    });
    let non_ks_err = ctx
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
        .expect_err("Vorton rejected on non-Keepstar");
    assert_eq!(non_ks_err.code, "CantFitToStructure");
    ctx.db.with_mut_snapshot(|s| {
        s.structures.get_mut(&keepstar_id).unwrap().type_id = TYPE_KEEPSTAR;
    });

    for idx in 0..5_i64 {
        let tid = 101_000_603 + idx;
        let _ = null_handle
            .spawn_ball_with_combat(
                Ball {
                    id: tid,
                    type_id: 587,
                    position: DVec3::new(2_500.0 + (idx as f64) * 200.0, 0.0, 0.0),
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
    }
    let primary_target = 101_000_603_i64;
    let _ = null_handle.add_target_lock(keepstar_id, eve_destiny::TargetLock {
        target_id: primary_target, lock_time_ms: 0.0, remaining_lock_ms: 0.0, max_range_m: Some(100_000.0), locked: true
    }).await;
    let vorton_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(vorton_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(primary_target)],
            &kw,
            None,
        )
        .await
        .expect("Activate Standup Arcing Vorton Projector");
    assert_eq!(kv_get(&vorton_rep, "targetsHit").and_then(|v| v.as_i64()), Some(5));

    // WeaponCycleImmobilized on gunner
    let immob_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pdb_mod_id)], &kw, None)
        .await
        .expect_err("Gunner weapon cycle immobilized during Arcing Vorton");
    assert_eq!(immob_err.code, "WeaponCycleImmobilized");
}
