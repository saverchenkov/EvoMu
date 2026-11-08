//! Extra unit and mutation-killing tests for `services::combat_crimewatch::combat_weapons`
//! covering session/docked/module preconditions, reload & tether blocks, EWAR immunity & safety
//! levels, lowsec pod criminal escalation, laser crystal T1/T2/stack volatility, Vorton arc
//! chaining, and PvP container/jetcan aggression rules.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::combat_crimewatch::combat_weapons::handle_weapons;
use crate::services::combat_crimewatch::{with_mut_combat_runtime, SafetyLevel};
use crate::services::inventory_fitting::set_modules_reload_until;
use eve_db::{InventoryFlag, UpwellStructureRecord};
use eve_destiny::{Ball, CombatState, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

async fn spawn_weapon_ball(
    ctx: &ServerContext,
    sys_id: u32,
    id: i64,
    type_id: u32,
    pos: DVec3,
    hp: f64,
    cap: f64,
) {
    ctx.systems
        .get_or_spawn(sys_id)
        .spawn_ball_with_combat(
            Ball {
                id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: hp,
                max_shield_hp: hp.max(100.0),
                armor_hp: hp,
                max_armor_hp: hp.max(100.0),
                structure_hp: hp,
                max_structure_hp: hp.max(100.0),
                capacitor: cap,
                max_capacitor: cap.max(100.0),
                signature_radius: 100.0,
                base_signature_radius: Some(100.0),
                ..CombatState::default()
            },
        )
        .await
        .unwrap();
}

async fn lock_target(ctx: &ServerContext, sys_id: u32, ship_id: i64, target_id: i64) {
    ctx.systems
        .get_or_spawn(sys_id)
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(150_000.0),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn weapon_activation_session_docked_offline_reload_and_probe_gates() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();

    // Unhandled RPC returns None
    assert!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Deactivate", &[], &kw)
            .await
            .is_none()
    );

    // Unauthenticated session returns error
    let mut anon = SessionState::default();
    assert!(
        handle_weapons(&ctx, &mut anon, "dogmaIM", "Activate", &[], &kw)
            .await
            .unwrap()
            .is_err()
    );

    // Docked in station -> ShipDocked
    session.station_id = Some(60_003_760);
    let err_docked = handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err_docked.code, "ShipDocked");
    session.station_id = None;

    // Docked in structure without controlling it -> ShipDocked
    session.structure_id = Some(1_000_000_999_999);
    let err_struct = handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err_struct.code, "ShipDocked");
    session.structure_id = None;

    // Missing ship_id -> NoActiveShip
    session.ship_id = None;
    let err_noship = handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err_noship.code, "NoActiveShip");
    session.ship_id = Some(ship_id);

    // Missing solar_system_id -> NoSolarSystem
    session.solar_system_id = None;
    let err_nosys = handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err_nosys.code, "NoSolarSystem");
    session.solar_system_id = Some(sys_id);

    // Ship not in space -> ShipNotInSpace
    let err_space = handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err_space.code, "ShipNotInSpace");

    spawn_weapon_ball(&ctx, sys_id, ship_id, 23_915, DVec3::ZERO, 1_000.0, 500.0).await;

    // Missing module on ship -> ModuleNotFound
    let err_nomod = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(999_888)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_nomod.code, "ModuleNotFound");

    // Seed projectile turret (group 55) + interdiction probe (type 22778, group 548)
    let mut sde = (*ctx.sde).clone();
    sde.types.insert(
        55_001,
        make_fighter_type(
            55_001,
            55,
            7,
            5.0,
            vec![(51, 1_000.0), (54, 15_000.0), (158, 10_000.0), (160, 100.0), (604, 83.0)],
        ),
    );
    sde.types.insert(
        22_778,
        make_fighter_type(22_778, 548, 8, 10.0, vec![]),
    );
    ctx.sde = Arc::new(sde);

    let gun_id = 830_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            gun_id,
            make_fighter_item(gun_id, 55_001, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    // Offline module -> ModuleOffline
    let mut fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    fit.modules.insert(
        gun_id,
        eve_dogma::FittedModule {
            item_id: gun_id,
            type_id: 55_001,
            flag_id: InventoryFlag::HiSlot0 as u16,
            state: eve_dogma::ModuleState::Offline,
            charge_type_id: None,
            charge_quantity: 0,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    ctx.store_ship_fitting(ship_id, fit);

    let err_off = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(gun_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_off.code, "ModuleOffline");

    ctx.invalidate_ship_fitting(ship_id);

    // Reloading module -> ModuleIsReloading
    set_modules_reload_until(&ctx, &[gun_id], ctx.now_unix() + 10);
    let err_rel = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(gun_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_rel.code, "ModuleIsReloading");
    set_modules_reload_until(&ctx, &[gun_id], 0);

    // Targeting an interdiction sphere probe -> InvalidTarget
    let probe_ball_id = 930_001_i64;
    spawn_weapon_ball(&ctx, sys_id, probe_ball_id, 22_778, DVec3::new(2_000.0, 0.0, 0.0), 100.0, 0.0).await;
    let err_probe = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(gun_id), PyRep::String("targetAttack".into()), PyRep::Integer(probe_ball_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_probe.code, "InvalidTarget");

    // No ammo loaded -> NoAmmoLoaded
    let err_noammo = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(gun_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_noammo.code, "NoAmmoLoaded");
}

#[tokio::test]
async fn ewar_structure_immunity_safety_levels_and_lowsec_pod_escalation() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();

    let mut sde = (*ctx.sde).clone();
    // Warp Disruptor (group 52, type 52001)
    sde.types.insert(
        52_001,
        make_fighter_type(52_001, 52, 7, 5.0, vec![(51, 5_000.0), (54, 24_000.0)]),
    );
    sde.types.insert(
        670,
        make_fighter_type(670, 29, 6, 500.0, vec![(9, 400.0)]),
    );
    ctx.sde = Arc::new(sde);

    let scram_id = 840_001_i64;
    let struct_id = 1_000_000_840_001_i64;
    let victim_char_id = 90_000_055_i64;
    let victim_pod_id = 940_055_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            scram_id,
            make_fighter_item(scram_id, 52_001, char_id, ship_id, InventoryFlag::MedSlot0, 1),
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                solar_system_id: sys_id,
                type_id: 35_832,
                owner_corp_id: 1_000_009,
                state: "ShieldVulnerable".into(),
                shield_hp: 5_000.0,
                armor_hp: 5_000.0,
                hull_hp: 5_000.0,
                ..Default::default()
            },
        );
        let mut vic = snap.characters.get(&char_id).cloned().unwrap();
        vic.character_id = victim_char_id;
        vic.corporation_id = 2_000_055;
        vic.active_ship_id = victim_pod_id;
        vic.solar_system_id = sys_id;
        vic.security_status = 0.0;
        snap.characters.insert(victim_char_id, vic);
    });
    ctx.invalidate_ship_fitting(ship_id);

    spawn_weapon_ball(&ctx, sys_id, ship_id, 23_915, DVec3::ZERO, 1_000.0, 500.0).await;
    spawn_weapon_ball(&ctx, sys_id, struct_id, 35_832, DVec3::new(5_000.0, 0.0, 0.0), 5_000.0, 0.0).await;
    spawn_weapon_ball(&ctx, sys_id, victim_pod_id, 670, DVec3::new(4_000.0, 0.0, 0.0), 400.0, 100.0).await;
    lock_target(&ctx, sys_id, ship_id, struct_id).await;
    lock_target(&ctx, sys_id, ship_id, victim_pod_id).await;

    // 1. Structure is immune to EWAR
    let imm_rep = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(scram_id), PyRep::String("warpDisrupt".into()), PyRep::Integer(struct_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(imm_rep.dict_get("immune"), Some(&PyRep::Bool(true)));
    assert_eq!(imm_rep.dict_get("ewarAssistRecorded"), Some(&PyRep::Bool(false)));

    // 2. Lowsec (0.3) pod target with Yellow safety -> CantDoInCurrentSafetyLevel (pod in lowsec is criminal)
    with_mut_combat_runtime(&ctx, |rt| {
        rt.modified_system_securities.insert(sys_id, 0.30);
        rt.safety_levels.insert(char_id, SafetyLevel::Yellow);
    });
    let safe_err = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(scram_id), PyRep::String("warpDisrupt".into()), PyRep::Integer(victim_pod_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(safe_err.code, "CantDoInCurrentSafetyLevel");

    // 3. Red safety in lowsec against pod -> succeeds and sets criminal flag on attacker!
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Red);
    });
    let pod_ewar = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(scram_id), PyRep::String("warpDisrupt".into()), PyRep::Integer(victim_pod_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(pod_ewar.dict_get("ewarAssistRecorded"), Some(&PyRep::Bool(true)));
    assert!(
        ctx.crimewatch
            .read()
            .unwrap()
            .states
            .get(&char_id)
            .is_some_and(|st| st.is_criminal(ctx.now_unix()))
    );
}

#[tokio::test]
async fn t1_and_stacked_crystal_volatility_vorton_arc_and_already_active_checks() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let kw = HashMap::new();

    let mut sde = (*ctx.sde).clone();
    // Energy Laser (group 53, type 53002)
    sde.types.insert(
        53_002,
        make_fighter_type(
            53_002,
            53,
            7,
            5.0,
            vec![(6, 10.0), (51, 10_000.0), (54, 20_000.0), (64, 1.0), (158, 5_000.0), (160, 100.0), (604, 86.0)],
        ),
    );
    // T1 Crystal (attr 786 == 0.0 -> never degrades)
    sde.types.insert(
        86_010,
        make_fighter_type(86_010, 86, 8, 1.0, vec![(9, 1.0), (114, 80.0), (786, 0.0)]),
    );
    // Volatile Crystal with high damage (786=1, 783=1.0, 784=1.0, hp=1.0 -> shatters in 1 shot)
    sde.types.insert(
        86_011,
        make_fighter_type(
            86_011,
            86,
            8,
            1.0,
            vec![(9, 1.0), (114, 80.0), (783, 1.0), (784, 1.0), (786, 1.0)],
        ),
    );
    // Vorton Projector (group 4060, type 40601) + Vorton charge (group 4061, type 40611)
    sde.types.insert(
        40_601,
        make_fighter_type(
            40_601,
            4060,
            7,
            5.0,
            vec![
                (6, 20.0),
                (51, 1_000.0),
                (54, 25_000.0),
                (64, 1.0),
                (158, 5_000.0),
                (604, 4061.0),
                (653, 200.0),
                (654, 40.0),
                (1353, 0.5),
                (3036, 10_000.0),
                (3037, 4.0),
            ],
        ),
    );
    sde.types.insert(
        40_611,
        make_fighter_type(40_611, 4061, 8, 0.01, vec![(114, 120.0), (117, 120.0)]),
    );
    // Target NPC type (cat 11)
    sde.types.insert(
        10_050,
        make_fighter_type(10_050, 550, 11, 1_000.0, vec![]),
    );
    ctx.sde = Arc::new(sde);

    let laser_id = 850_001_i64;
    let crystal_id = 850_002_i64;
    let vorton_id = 850_003_i64;
    let vorton_ammo_id = 850_004_i64;
    let npc1 = 950_001_i64;
    let npc2 = 950_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            laser_id,
            make_fighter_item(laser_id, 53_002, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            crystal_id,
            make_fighter_item(crystal_id, 86_010, char_id, ship_id, InventoryFlag::HiSlot0, 2),
        );
        snap.items.insert(
            vorton_id,
            make_fighter_item(vorton_id, 40_601, char_id, ship_id, InventoryFlag::HiSlot1, 1),
        );
        snap.items.insert(
            vorton_ammo_id,
            make_fighter_item(vorton_ammo_id, 40_611, char_id, ship_id, InventoryFlag::HiSlot1, 5),
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    spawn_weapon_ball(&ctx, sys_id, ship_id, 23_915, DVec3::ZERO, 2_000.0, 500.0).await;
    spawn_weapon_ball(&ctx, sys_id, npc1, 10_050, DVec3::new(5_000.0, 0.0, 0.0), 5_000.0, 100.0).await;
    spawn_weapon_ball(&ctx, sys_id, npc2, 10_050, DVec3::new(8_000.0, 0.0, 0.0), 5_000.0, 100.0).await;
    lock_target(&ctx, sys_id, ship_id, npc1).await;

    // 1. T1 crystal shot does not degrade crystal
    let t1_shot = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc1)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(t1_shot.dict_get("crystalDamageFraction"), Some(&PyRep::Real(0.0)));
    assert_eq!(t1_shot.dict_get("crystalShattered"), Some(&PyRep::Bool(false)));

    // 2. Calling Activate again while in reactivation delay returns ModuleAlreadyActive
    let active_err = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc1)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(active_err.code, "ModuleAlreadyActive");

    // 3. Switch crystal to volatile type with quantity=2 and fire via Overload -> shatters 1 crystal, quantity becomes 1!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&crystal_id).unwrap().type_id = 86_011;
    });
    let shatter_stack = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Overload",
        &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc1)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(shatter_stack.dict_get("crystalShattered"), Some(&PyRep::Bool(true)));
    assert_eq!(
        ctx.db.with_snapshot(|snap| snap.items.get(&crystal_id).map(|i| i.quantity)),
        Some(1)
    );

    // 4. Vorton projector hits npc1 and arcs to npc2 (`secondaryTargetsHit == 1`)
    let vorton_shot = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(vorton_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc1)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(vorton_shot.dict_get("isWreckingShot"), Some(&PyRep::Bool(false)));
    assert_eq!(vorton_shot.dict_get("secondaryTargetsHit"), Some(&PyRep::Integer(1)));
    assert_eq!(
        ctx.db.with_snapshot(|snap| snap.items.get(&vorton_ammo_id).map(|i| i.quantity)),
        Some(4)
    );
}
