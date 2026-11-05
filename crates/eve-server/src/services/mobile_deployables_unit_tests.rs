//! Unit and mutation-killing tests for `services::mobile_deployables`
//! (deployment restrictions, registration, lifetime refresh, MTU tractoring/looting,
//! Mobile Depot 25% shield reinforcement, and Mobile Micro Jump Unit activation).

use super::*;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, Ballpark, CombatState, DamageVector, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use glam::DVec3;
use std::sync::Arc;

fn find_non_starter_lowsec_system(sde: &SdeRegistry) -> u32 {
    sde.solar_systems
        .values()
        .find(|s| {
            s.security_status > 0.1
                && s.security_status < 0.4
                && (30_000_000..31_000_000).contains(&s.solar_system_id)
                && !is_starter_or_career_system(sde, s.solar_system_id)
        })
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_537)
}

#[test]
fn mobile_structure_predicates_and_deployment_restrictions() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let sde = &ctx.sde;

    assert!(is_mobile_structure(sde, TYPE_MOBILE_DEPOT));
    assert!(is_mobile_structure(sde, TYPE_MTU_STANDARD));
    assert!(is_mobile_structure(sde, TYPE_MOBILE_MJU));
    assert!(is_mobile_structure(sde, TYPE_MOBILE_CYNO_INHIBITOR));
    assert!(!is_mobile_structure(sde, 587)); // Rifter is not a mobile structure

    assert!(can_deployable_be_scooped(GROUP_MOBILE_DEPOT));
    assert!(can_deployable_be_scooped(GROUP_MOBILE_TRACTOR));
    assert!(!can_deployable_be_scooped(GROUP_CRAB_BEACON));

    let starter_sys = sde
        .schools
        .values()
        .find(|s| (11..=22).contains(&s.school_id))
        .map(|s| s.solar_system_id)
        .unwrap_or(30_000_142);
    assert!(is_starter_or_career_system(sde, starter_sys));

    let lowsec_sys = find_non_starter_lowsec_system(sde);
    assert!(!is_starter_or_career_system(sde, lowsec_sys));

    let mut sess = SessionState::new(93_001);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.solar_system_id = Some(lowsec_sys);

    // 1. Starter system restriction
    let err_starter = check_mobile_deployment_restrictions(
        &ctx,
        &sess,
        900_001,
        TYPE_MOBILE_DEPOT,
        GROUP_MOBILE_DEPOT,
        starter_sys,
        DVec3::new(1e9, 1e9, 1e9),
    )
    .unwrap_err();
    assert_eq!(err_starter.code, "RestrictionsInEffect");

    // 2. Cyno Inhibitor in high-sec (Jita 30000142) rejected
    let highsec_non_starter = sde
        .solar_systems
        .values()
        .find(|s| {
            s.security_status >= 0.5
                && (30_000_000..31_000_000).contains(&s.solar_system_id)
                && !is_starter_or_career_system(sde, s.solar_system_id)
        })
        .map(|s| s.solar_system_id)
        .unwrap_or(30_000_142);
    let err_cyno_hs = check_mobile_deployment_restrictions(
        &ctx,
        &sess,
        900_002,
        TYPE_MOBILE_CYNO_INHIBITOR,
        GROUP_MOBILE_CYNO_INHIBITOR,
        highsec_non_starter,
        DVec3::new(1e11, 1e11, 1e11),
    )
    .unwrap_err();
    assert_eq!(err_cyno_hs.code, "CantAnchorInHighSecOrWormhole");

    // 3. Stargate distance check (< 50 km)
    if let Some(gate) = sde.stargates().values().find(|g| g.from_solar_system_id == lowsec_sys) {
        let near_gate = gate.position + DVec3::new(1_000.0, 0.0, 0.0);
        let err_gate = check_mobile_deployment_restrictions(
            &ctx,
            &sess,
            900_003,
            TYPE_MTU_STANDARD,
            GROUP_MOBILE_TRACTOR,
            lowsec_sys,
            near_gate,
        )
        .unwrap_err();
        assert_eq!(err_gate.code, "DeploymentTooCloseToGate");
    }

    // 4. Upwell structure distance check (< 50 km) and MTU-to-MTU distance check (< 5 km)
    let deep_pos = DVec3::new(5e11, 5e11, 5e11);
    let st_id = 1_000_000_910_001;
    let existing_mtu_id = 900_010;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: lowsec_sys,
                name: "Test Structure".into(),
                state: "ShieldVulnerable".into(),
                position: [deep_pos.x, deep_pos.y, deep_pos.z],
                shield_hp: 100.0,
                armor_hp: 100.0,
                hull_hp: 100.0,
                quantum_core_installed: true,
                fuel_blocks: 1000,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            existing_mtu_id,
            InventoryItemRecord {
                item_id: existing_mtu_id,
                type_id: TYPE_MTU_STANDARD,
                owner_id: 90_000_001,
                location_id: lowsec_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(
            existing_mtu_id,
            [deep_pos.x + 200_000.0, deep_pos.y, deep_pos.z],
        );
    });

    let err_st = check_mobile_deployment_restrictions(
        &ctx,
        &sess,
        900_004,
        TYPE_MTU_STANDARD,
        GROUP_MOBILE_TRACTOR,
        lowsec_sys,
        deep_pos + DVec3::new(10_000.0, 0.0, 0.0),
    )
    .unwrap_err();
    assert_eq!(err_st.code, "DeploymentTooCloseToStructure");

    let err_mtu = check_mobile_deployment_restrictions(
        &ctx,
        &sess,
        900_005,
        TYPE_MTU_STANDARD,
        GROUP_MOBILE_TRACTOR,
        lowsec_sys,
        deep_pos + DVec3::new(201_000.0, 0.0, 0.0),
    )
    .unwrap_err();
    assert_eq!(err_mtu.code, "DeploymentTooCloseToSameType");

    // Valid deep-space position succeeds
    assert!(check_mobile_deployment_restrictions(
        &ctx,
        &sess,
        900_006,
        TYPE_MTU_STANDARD,
        GROUP_MOBILE_TRACTOR,
        lowsec_sys,
        deep_pos + DVec3::new(500_000.0, 0.0, 0.0),
    )
    .is_ok());
}

#[test]
fn register_scoop_touch_step_mtu_and_depot_reinforcement() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let sys_id = 30_000_142;
    let owner_id = 90_000_001;
    let depot_id = 901_001;
    let mtu_id = 901_002;
    let wreck_id = 901_003;
    let loot_id = 901_004;

    // Seed items in space and ensure owner is in space
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&owner_id) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
        }
        for (iid, tid, pos) in [
            (depot_id, TYPE_MOBILE_DEPOT, [0.0, 0.0, 0.0]),
            (mtu_id, TYPE_MTU_STANDARD, [10_000.0, 0.0, 0.0]),
            (wreck_id, 26557, [11_000.0, 0.0, 0.0]), // 1,000m from MTU -> within 2,500m loot range
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(iid, pos);
        }
        snap.items.insert(
            loot_id,
            InventoryItemRecord {
                item_id: loot_id,
                type_id: 34,
                owner_id,
                location_id: wreck_id,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Record scooped damage, then register deployable -> returns preserved damage
    record_scooped_deployable_damage(
        &ctx,
        depot_id,
        &CombatState {
            shield_hp: 800.0,
            max_shield_hp: 1_000.0,
            armor_hp: 900.0,
            max_armor_hp: 1_000.0,
            structure_hp: 1_000.0,
            max_structure_hp: 1_000.0,
            ..Default::default()
        },
    );
    let restored = register_mobile_deployable(
        &ctx,
        depot_id,
        TYPE_MOBILE_DEPOT,
        GROUP_MOBILE_DEPOT,
        owner_id,
        sys_id,
        60,
        DVec3::ZERO,
    );
    assert!(restored.is_some());
    assert_eq!(restored.unwrap().shield_hp, 800.0);

    register_mobile_deployable(
        &ctx,
        mtu_id,
        TYPE_MTU_STANDARD,
        GROUP_MOBILE_TRACTOR,
        owner_id,
        sys_id,
        60,
        DVec3::new(10_000.0, 0.0, 0.0),
    );

    ctx.advance_secs(120);
    touch_mobile_depot_interaction(&ctx, depot_id);
    let now_unix = ctx.now_unix();

    let mut ballpark = Ballpark::new(sys_id);
    ballpark.set_ball_slim_meta(
        100_000_001,
        SlimItemMeta {
            char_id: Some(owner_id),
            owner_id,
            ..Default::default()
        },
    );
    ballpark.add_ball(Ball {
        id: depot_id,
        type_id: TYPE_MOBILE_DEPOT,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 10_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    ballpark.combat_states.insert(
        depot_id,
        CombatState {
            shield_hp: 1_000.0,
            max_shield_hp: 1_000.0,
            armor_hp: 1_000.0,
            max_armor_hp: 1_000.0,
            structure_hp: 1_000.0,
            max_structure_hp: 1_000.0,
            ..Default::default()
        },
    );

    let mut cycles = Vec::new();
    step_mobile_deployables_in_actor(
        &mut ballpark,
        &mut cycles,
        &ctx.mechanic_rng,
        None,
        Some(&ctx.sde),
        Some(&ctx.db),
        Some(&ctx.mechanic_rng),
        sys_id,
        1.0,
        now_unix,
    );

    // MTU must have looted `loot_id` from `wreck_id` into `mtu_id`
    let loot_loc = ctx.db.with_snapshot(|s| s.items.get(&loot_id).map(|i| i.location_id));
    assert_eq!(loot_loc, Some(mtu_id));

    // Mobile Depot damage triggers 48h reinforcement at 25% shield HP (250.0)
    let db_opt = Some(Arc::clone(&ctx.db));
    let sde_opt = Some(Arc::clone(&ctx.sde));
    let (applied, destroyed, _) = handle_ball_damage_in_actor(
        &db_opt,
        &sde_opt,
        sys_id,
        depot_id,
        DamageVector {
            em: 900.0,
            thermal: 0.0,
            kinetic: 0.0,
            explosive: 0.0,
        },
        &mut ballpark,
        now_unix,
    )
    .expect("depot damage handled");
    assert!(!destroyed);
    assert!((applied - 750.0).abs() < 1e-6);
    assert!(read_deployable_state(&ctx)
        .deployables
        .get(&depot_id)
        .unwrap()
        .reinforced_until_unix
        .is_some());

    // While reinforced, depot is invulnerable (0.0 damage applied)
    let (reinf_applied, reinf_destroyed, _) = handle_ball_damage_in_actor(
        &db_opt,
        &sde_opt,
        sys_id,
        depot_id,
        DamageVector {
            em: 5_000.0,
            thermal: 0.0,
            kinetic: 0.0,
            explosive: 0.0,
        },
        &mut ballpark,
        now_unix + 10,
    )
    .unwrap();
    assert_eq!(reinf_applied, 0.0);
    assert!(!reinf_destroyed);
}

#[tokio::test]
async fn handle_activate_micro_jump_unit_validates_and_spools() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let mju_id = 902_001;

    let mut sess = SessionState::new(93_010);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    register_mobile_deployable(
        &ctx,
        mju_id,
        TYPE_MOBILE_MJU,
        GROUP_MOBILE_MICRO_JUMP_UNIT,
        char_id,
        sys_id,
        MMJU_ACTIVATION_DELAY_SECS,
        DVec3::new(500.0, 0.0, 0.0),
    );

    // Still anchoring -> DeployableAnchoring
    let err_anch = handle_activate_micro_jump_unit(&ctx, &sess, mju_id)
        .await
        .unwrap_err();
    assert_eq!(err_anch.code, "DeployableAnchoring");

    // Advance past 60s anchoring delay and spawn ship ball within 2,500m
    ctx.advance_secs(MMJU_ACTIVATION_DELAY_SECS + 5);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta {
                char_id: Some(char_id),
                owner_id: char_id,
                ..Default::default()
            },
        )
        .await;

    let res = handle_activate_micro_jump_unit(&ctx, &sess, mju_id)
        .await
        .expect("activate MMJU");
    assert_eq!(res.dict_get("mjuID"), Some(&eve_proto::PyRep::Integer(mju_id)));

    // Second call while spooling -> MicroJumpAlreadySpooling
    let err_dup = handle_activate_micro_jump_unit(&ctx, &sess, mju_id)
        .await
        .unwrap_err();
    assert_eq!(err_dup.code, "MicroJumpAlreadySpooling");
}
