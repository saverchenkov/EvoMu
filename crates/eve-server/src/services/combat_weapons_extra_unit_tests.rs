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

#[tokio::test]
async fn rpc_args_tethered_invulnerable_range_capacitor_and_projectile_zero_cap() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    let hybrid_tid = 74_501u32;
    let hybrid_charge_tid = 85_501u32;
    let proj_tid = 55_501u32;
    let proj_charge_tid = 83_501u32;
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        sde.types.insert(
            hybrid_tid,
            make_fighter_type(
                hybrid_tid,
                74,
                7,
                50.0,
                vec![(6, 50.0), (51, 3_000.0), (54, 5_000.0), (158, 2_000.0), (160, 1.0), (604, 85.0)],
            ),
        );
        sde.types.insert(
            hybrid_charge_tid,
            make_fighter_type(hybrid_charge_tid, 85, 8, 0.01, vec![(117, 100.0), (9, 1.0)]),
        );
        sde.types.insert(
            proj_tid,
            make_fighter_type(
                proj_tid,
                55,
                7,
                50.0,
                vec![(6, 999.0), (51, 3_000.0), (54, 10_000.0), (158, 2_000.0), (160, 1.0), (604, 83.0)],
            ),
        );
        sde.types.insert(
            proj_charge_tid,
            make_fighter_type(proj_charge_tid, 83, 8, 0.01, vec![(117, 80.0), (9, 1.0)]),
        );
    }

    let hybrid_id = 860_001i64;
    let hybrid_ammo_id = 860_002i64;
    let proj_id = 860_003i64;
    let proj_ammo_id = 860_004i64;
    let target_char_id = char_id + 100;
    let target_ship_id = 860_100i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);

    ctx.db.with_mut_snapshot(|snap| {
        let mut target_ch = snap.characters[&char_id].clone();
        target_ch.character_id = target_char_id;
        target_ch.user_id = target_char_id as u64;
        target_ch.corporation_id = corp_id + 1;
        target_ch.solar_system_id = sys_id;
        target_ch.active_ship_id = target_ship_id;
        target_ch.station_id = None;
        snap.characters.insert(target_char_id, target_ch);

        snap.items.insert(
            target_ship_id,
            make_fighter_item(target_ship_id, 587, target_char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(hybrid_id, make_fighter_item(hybrid_id, hybrid_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(hybrid_ammo_id, make_fighter_item(hybrid_ammo_id, hybrid_charge_tid, char_id, ship_id, InventoryFlag::HiSlot0, 5));
        snap.items.insert(proj_id, make_fighter_item(proj_id, proj_tid, char_id, ship_id, InventoryFlag::HiSlot1, 1));
        snap.items.insert(proj_ammo_id, make_fighter_item(proj_ammo_id, proj_charge_tid, char_id, ship_id, InventoryFlag::HiSlot1, 5));
        snap.module_online_states.insert(hybrid_id, true);
        snap.module_online_states.insert(proj_id, true);
    });

    spawn_weapon_ball(&ctx, sys_id, ship_id, 23_915, DVec3::ZERO, 2000.0, 200.0).await;
    spawn_weapon_ball(&ctx, sys_id, target_ship_id, 587, DVec3::new(2_000.0, 0.0, 0.0), 2000.0, 200.0).await;
    lock_target(&ctx, sys_id, ship_id, target_ship_id).await;

    // W2 RpcArgs invalid moduleID / bad target type.
    for bad_args in [
        vec![PyRep::Integer(0)],
        vec![PyRep::Integer(-5), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_id)],
        vec![PyRep::Integer(hybrid_id), PyRep::String("targetAttack".into()), PyRep::String("bad_target".into())],
    ] {
        let err = handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &bad_args, &kw)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, "InvalidArgument");
    }

    // Tethered blocks offensive activation with no cap/ammo consumption.
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    {
        let mut cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        cs.is_tethered = true;
        sys_handle.set_combat_state(ship_id, cs).await.unwrap();
    }
    let tether_err = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(hybrid_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(tether_err.code, "CantActivateWhileTethered");
    assert!((sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().capacitor - 200.0).abs() < 1e-6);
    assert_eq!(ctx.db.get_item(hybrid_ammo_id).unwrap().quantity, 5);

    {
        let mut cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        cs.is_tethered = false;
        sys_handle.set_combat_state(ship_id, cs).await.unwrap();
    }

    // Location-change invulnerability rejects with TargetInvulnerable.
    crate::services::space_destiny::grant_location_change_invulnerability(&ctx, target_char_id, ctx.now_unix() + 60);
    let invuln_err = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(hybrid_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(invuln_err.code, "TargetInvulnerable");
    assert_eq!(ctx.db.get_item(hybrid_ammo_id).unwrap().quantity, 5);

    // Clear invulnerability, move target outside optimal+falloff range -> TargetOutOfRange.
    crate::services::space_destiny::grant_location_change_invulnerability(&ctx, target_char_id, 0);
    spawn_weapon_ball(&ctx, sys_id, target_ship_id, 587, DVec3::new(250_000.0, 0.0, 0.0), 2000.0, 200.0).await;
    lock_target(&ctx, sys_id, ship_id, target_ship_id).await;
    let range_err = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(hybrid_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(range_err.code, "TargetOutOfRange");
    assert!((sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().capacitor - 200.0).abs() < 1e-6);
    assert_eq!(ctx.db.get_item(hybrid_ammo_id).unwrap().quantity, 5);

    // Move target back into range; insufficient cap rejects Hybrid (preserves ammo), whereas 0 cap allows Projectile.
    spawn_weapon_ball(&ctx, sys_id, target_ship_id, 587, DVec3::new(2_000.0, 0.0, 0.0), 2000.0, 200.0).await;
    lock_target(&ctx, sys_id, ship_id, target_ship_id).await;
    {
        let mut cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        cs.capacitor = 10.0;
        sys_handle.set_combat_state(ship_id, cs).await.unwrap();
    }
    let cap_err = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(hybrid_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(cap_err.code, "NotEnoughCapacitor");
    assert_eq!(ctx.db.get_item(hybrid_ammo_id).unwrap().quantity, 5);

    {
        let mut cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        cs.capacitor = 0.0;
        sys_handle.set_combat_state(ship_id, cs).await.unwrap();
    }
    let proj_ok = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(proj_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("projectile fires at 0 cap");
    assert!((proj_ok.dict_get("capacitorAfter").and_then(|v| v.as_f64()).unwrap() - 0.0).abs() < 1e-6);
    assert_eq!(ctx.db.get_item(proj_ammo_id).unwrap().quantity, 4);
}

#[tokio::test]
async fn entropic_disintegrator_spool_already_active_and_target_switch_reset() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    ctx.set_clock(Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let kw = HashMap::new();
    let ent_tid = 19_860u32;
    let ent_charge_tid = 19_870u32;
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        sde.types.insert(
            ent_tid,
            make_fighter_type(
                ent_tid,
                1986,
                7,
                50.0,
                vec![(6, 20.0), (51, 3_000.0), (54, 10_000.0), (158, 1_000.0), (604, 1987.0), (2733, 0.25), (2734, 1.5)],
            ),
        );
        sde.types.insert(
            ent_charge_tid,
            make_fighter_type(ent_charge_tid, 1987, 8, 0.01, vec![(114, 100.0), (118, 100.0), (9, 1.0)]),
        );
    }

    let ent_id = 861_001i64;
    let ent_ammo_id = 861_002i64;
    let t1_id = 861_101i64;
    let t2_id = 861_102i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ent_id, make_fighter_item(ent_id, ent_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(ent_ammo_id, make_fighter_item(ent_ammo_id, ent_charge_tid, char_id, ship_id, InventoryFlag::HiSlot0, 10));
        snap.module_online_states.insert(ent_id, true);
    });
    spawn_weapon_ball(&ctx, sys_id, ship_id, 23_915, DVec3::ZERO, 2000.0, 500.0).await;
    spawn_weapon_ball(&ctx, sys_id, t1_id, 587, DVec3::new(2_000.0, 0.0, 0.0), 5000.0, 500.0).await;
    spawn_weapon_ball(&ctx, sys_id, t2_id, 587, DVec3::new(2_500.0, 0.0, 0.0), 5000.0, 500.0).await;
    lock_target(&ctx, sys_id, ship_id, t1_id).await;
    lock_target(&ctx, sys_id, ship_id, t2_id).await;

    let shot1 = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(ent_id), PyRep::String("targetAttack".into()), PyRep::Integer(t1_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(shot1.dict_get("spoolCycles"), Some(&PyRep::Integer(0)));
    assert!((shot1.dict_get("spoolMultiplier").and_then(|v| v.as_f64()).unwrap() - 1.0).abs() < 1e-6);

    // Calling Activate while already active on same target returns AlreadyActive.
    let dup_err = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(ent_id), PyRep::String("targetAttack".into()), PyRep::Integer(t1_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(dup_err.code, "AlreadyActive");

    // Advancing spool cycles increments multiplier along per_cycle_bonus (0.25) and clamps to 1.0 + max_bonus.
    let (c1, m1) = crate::services::combat_weapons_spool::advance_entropic_spool_cycle(&ctx, ship_id, ent_id);
    assert_eq!(c1, 1);
    assert!((m1 - 1.25).abs() < 1e-6);
    let (c2, m2) = crate::services::combat_weapons_spool::advance_entropic_spool_cycle(&ctx, ship_id, ent_id);
    assert_eq!(c2, 2);
    assert!((m2 - 1.5).abs() < 1e-6);
    assert!((crate::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, ship_id, ent_id) - 1.5).abs() < 1e-6);

    // Switching target to t2 resets spool back to cycle 0 and 1.0x.
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.remove_module_cycle(ship_id, ent_id).await;
    ctx.module_reactivation_cooldowns.write().unwrap().clear();
    ctx.db.with_mut_snapshot(|s| {
        s.module_active_states.remove(&ent_id);
        if let Some(m) = s.items.get_mut(&ent_id) {
            m.flag = InventoryFlag::HiSlot0;
        }
    });
    if let Ok(mut fit) = ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
        if let Some(fm) = fit.modules.get_mut(&ent_id) {
            fm.state = eve_dogma::ModuleState::Online;
        }
        ctx.store_ship_fitting(ship_id, fit);
    }
    let shot_switch = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(ent_id), PyRep::String("targetAttack".into()), PyRep::Integer(t2_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(shot_switch.dict_get("spoolCycles"), Some(&PyRep::Integer(0)));
    assert!((shot_switch.dict_get("spoolMultiplier").and_then(|v| v.as_f64()).unwrap() - 1.0).abs() < 1e-6);
    assert!((crate::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, ship_id, ent_id) - 1.0).abs() < 1e-6);
}

#[tokio::test]
async fn salvager_tractor_smartbomb_safety_aoe_and_bomb_highsec() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    let salv_tid = 11_220u32;
    let trac_tid = 6_500u32;
    let sb_tid = 7_200u32;
    let bomb_tid = 8_620u32;
    let bomb_charge_tid = 8_621u32;
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        sde.types.insert(salv_tid, make_fighter_type(salv_tid, 1122, 7, 5.0, vec![(54, 6_000.0), (51, 5_000.0)]));
        sde.types.insert(trac_tid, make_fighter_type(trac_tid, 650, 7, 5.0, vec![(54, 20_000.0), (51, 5_000.0)]));
        sde.types.insert(sb_tid, make_fighter_type(sb_tid, 72, 7, 50.0, vec![(6, 25.0), (99, 5_000.0), (114, 500.0), (51, 5_000.0)]));
        sde.types.insert(bomb_tid, make_fighter_type(bomb_tid, 862, 7, 50.0, vec![(51, 5_000.0)]));
        sde.types.insert(bomb_charge_tid, make_fighter_type(bomb_charge_tid, 863, 8, 75.0, vec![(114, 1_000.0)]));
    }

    let salv_id = 862_001i64;
    let trac_id = 862_002i64;
    let sb_id = 862_003i64;
    let bomb_id = 862_004i64;
    let bomb_charge_id = 862_005i64;
    let near_npc = 862_101i64;
    let far_npc = 862_102i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(salv_id, make_fighter_item(salv_id, salv_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(trac_id, make_fighter_item(trac_id, trac_tid, char_id, ship_id, InventoryFlag::HiSlot1, 1));
        snap.items.insert(sb_id, make_fighter_item(sb_id, sb_tid, char_id, ship_id, InventoryFlag::HiSlot2, 1));
        snap.items.insert(bomb_id, make_fighter_item(bomb_id, bomb_tid, char_id, ship_id, InventoryFlag::HiSlot3, 1));
        snap.items.insert(bomb_charge_id, make_fighter_item(bomb_charge_id, bomb_charge_tid, char_id, ship_id, InventoryFlag::HiSlot3, 2));
        for id in [salv_id, trac_id, sb_id, bomb_id] {
            snap.module_online_states.insert(id, true);
        }
    });

    spawn_weapon_ball(&ctx, sys_id, ship_id, 23_915, DVec3::ZERO, 2000.0, 500.0).await;
    spawn_weapon_ball(&ctx, sys_id, near_npc, 587, DVec3::new(2_000.0, 0.0, 0.0), 2000.0, 200.0).await;
    spawn_weapon_ball(&ctx, sys_id, far_npc, 587, DVec3::new(15_000.0, 0.0, 0.0), 2000.0, 200.0).await;

    // Salvager & Tractor target guards.
    for mid in [salv_id, trac_id] {
        assert_eq!(
            handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(mid)], &kw)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            "TargetRequired"
        );
        assert_eq!(
            handle_weapons(
                &ctx,
                &mut session,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(mid), PyRep::String("Salvage".into()), PyRep::Integer(999_888)],
                &kw,
            )
            .await
            .unwrap()
            .unwrap_err()
            .code,
            "TargetNotInSpace"
        );
    }

    // Smartbomb Green safety blocked in highsec, Red safety damages near ball and spares far ball.
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Green);
    });
    assert_eq!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(sb_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CantDoInCurrentSafetyLevel"
    );

    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Red);
    });
    handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(sb_id)], &kw)
        .await
        .unwrap()
        .expect("smartbomb Red safety");
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let near_sh = sys_handle.get_combat_state(near_npc).await.unwrap().unwrap().shield_hp;
    let far_sh = sys_handle.get_combat_state(far_npc).await.unwrap().unwrap().shield_hp;
    assert!(near_sh < 2000.0, "near target should take smartbomb AoE damage");
    assert!((far_sh - 2000.0).abs() < 1e-6, "far target should remain untouched");

    // BombLauncher forbidden in highsec (30_000_142).
    assert_eq!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(bomb_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "BombDisallowedInHighSec"
    );
}

