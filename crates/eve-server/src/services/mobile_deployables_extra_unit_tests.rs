//! Deep branch-coverage and mutation-killing unit tests for `services::mobile_deployables`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::mobile_deployables::*;
use eve_db::{
    FleetRecord, InventoryFlag, InventoryItemRecord, OwnerNoteRecord, UpwellStructureRecord,
    WorldRepository, WorldStateSnapshot,
};
use eve_destiny::{Ball, Ballpark, CombatState, DamageVector, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn put_space_item(
    snap: &mut WorldStateSnapshot,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    sys_id: u32,
    pos: [f64; 3],
) {
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id: sys_id as i64,
            flag: InventoryFlag::None,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        },
    );
    snap.item_positions.insert(item_id, pos);
}

#[test]
fn state_corrupt_json_and_all_deployment_proximity_restrictions() {
    let ctx = test_ctx();
    let sde = &ctx.sde;

    // 1. Corrupt owner_note JSON returns default state cleanly
    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.insert(
            MOBILE_DEPLOYABLE_NOTE_ID,
            OwnerNoteRecord {
                note_id: MOBILE_DEPLOYABLE_NOTE_ID,
                character_id: MOBILE_DEPLOYABLE_NOTE_OWNER,
                item_id: 0,
                label: "S:MOBILE_DEPLOYABLE_RUNTIME_STATE".into(),
                note: "{not-valid-json".into(),
                timestamp_unix: 100,
            },
        );
    });
    assert!(read_deployable_state(&ctx).deployables.is_empty());

    // 2. Find a low-sec non-starter system with a station and a celestial (group 6..=9)
    let lowsec_sys = sde
        .solar_systems
        .values()
        .find(|s| {
            s.security_status > 0.1
                && s.security_status < 0.4
                && (30_000_000..31_000_000).contains(&s.solar_system_id)
                && !is_starter_or_career_system(sde, s.solar_system_id)
                && !s.stations.is_empty()
        })
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_537);

    let mut sess = SessionState::new(94_100);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.solar_system_id = Some(lowsec_sys);

    // 3. Cyno Inhibitor in wormhole (31_000_001) and Zarzakh (30_100_000) rejected, and < 200km overlap rejected
    for forbidden_sys in [31_000_001_u32, 30_100_000_u32] {
        let err = check_mobile_deployment_restrictions(
            &ctx,
            &sess,
            910_001,
            TYPE_MOBILE_CYNO_INHIBITOR,
            GROUP_MOBILE_CYNO_INHIBITOR,
            forbidden_sys,
            DVec3::new(5e11, 5e11, 5e11),
        )
        .unwrap_err();
        assert_eq!(err.code, "CantAnchorInHighSecOrWormhole");
    }

    let deep = DVec3::new(6e11, 6e11, 6e11);
    ctx.db.with_mut_snapshot(|snap| {
        put_space_item(
            snap,
            910_010,
            TYPE_MOBILE_CYNO_INHIBITOR,
            90_000_001,
            lowsec_sys,
            [deep.x, deep.y, deep.z],
        );
    });
    let err_inhib = check_mobile_deployment_restrictions(
        &ctx,
        &sess,
        910_011,
        TYPE_MOBILE_CYNO_INHIBITOR,
        GROUP_MOBILE_CYNO_INHIBITOR,
        lowsec_sys,
        deep + DVec3::new(150_000.0, 0.0, 0.0), // 150km < 200km
    )
    .unwrap_err();
    assert_eq!(err_inhib.code, "MobileCynoInhibitorOverlap");

    // 4. Station distance check (< 50km)
    if let Some(st) = sde.stations.values().find(|st| st.solar_system_id == lowsec_sys) {
        let err_station = check_mobile_deployment_restrictions(
            &ctx,
            &sess,
            910_012,
            TYPE_MOBILE_DEPOT,
            GROUP_MOBILE_DEPOT,
            lowsec_sys,
            st.position + DVec3::new(10_000.0, 0.0, 0.0),
        )
        .unwrap_err();
        assert_eq!(err_station.code, "DeploymentTooCloseToStation");
    }

    // 5. Mobile Depot near celestial (< 5km), near another Depot (< 5km), and near another deployable (< 5km)
    if let Some(cel) = sde
        .celestials_in_system(lowsec_sys)
        .into_iter()
        .find(|c| matches!(c.group_id, 6..=9) && c.position != DVec3::ZERO)
    {
        let err_cel = check_mobile_deployment_restrictions(
            &ctx,
            &sess,
            910_013,
            TYPE_MOBILE_DEPOT,
            GROUP_MOBILE_DEPOT,
            lowsec_sys,
            cel.position + DVec3::new(1_000.0, 0.0, 0.0),
        )
        .unwrap_err();
        assert_eq!(err_cel.code, "DeploymentTooCloseToCelestial");
    }

    let depot_pos = DVec3::new(7e11, 7e11, 7e11);
    let other_dep_pos = DVec3::new(8e11, 8e11, 8e11);
    ctx.db.with_mut_snapshot(|snap| {
        put_space_item(
            snap,
            910_020,
            TYPE_MOBILE_DEPOT,
            90_000_001,
            lowsec_sys,
            [depot_pos.x, depot_pos.y, depot_pos.z],
        );
        put_space_item(
            snap,
            910_021,
            TYPE_MTU_STANDARD,
            90_000_001,
            lowsec_sys,
            [other_dep_pos.x, other_dep_pos.y, other_dep_pos.z],
        );
    });
    let err_same_depot = check_mobile_deployment_restrictions(
        &ctx,
        &sess,
        910_022,
        TYPE_MOBILE_DEPOT,
        GROUP_MOBILE_DEPOT,
        lowsec_sys,
        depot_pos + DVec3::new(2_000.0, 0.0, 0.0),
    )
    .unwrap_err();
    assert_eq!(err_same_depot.code, "DeploymentTooCloseToSameType");

    let err_other_dep = check_mobile_deployment_restrictions(
        &ctx,
        &sess,
        910_023,
        TYPE_MOBILE_DEPOT,
        GROUP_MOBILE_DEPOT,
        lowsec_sys,
        other_dep_pos + DVec3::new(2_000.0, 0.0, 0.0),
    )
    .unwrap_err();
    assert_eq!(err_other_dep.code, "DeploymentTooCloseToStructure");

    // 6. Mobile Siphon (group 1247) near Athanor (35835) uses 5km min distance instead of 50km
    let athanor_pos = DVec3::new(9e11, 9e11, 9e11);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            1_049_000_001,
            UpwellStructureRecord {
                structure_id: 1_049_000_001,
                type_id: 35835,
                owner_corp_id: 98_000_001,
                solar_system_id: lowsec_sys,
                name: "Test Athanor".into(),
                state: "ShieldVulnerable".into(),
                position: [athanor_pos.x, athanor_pos.y, athanor_pos.z],
                shield_hp: 100.0,
                armor_hp: 100.0,
                hull_hp: 100.0,
                quantum_core_installed: true,
                fuel_blocks: 100,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        put_space_item(
            snap,
            910_023,
            14343,
            90_000_001,
            lowsec_sys,
            [athanor_pos.x, athanor_pos.y, athanor_pos.z],
        );
    });
    assert_eq!(
        check_mobile_deployment_restrictions(
            &ctx,
            &sess,
            910_024,
            33477,
            1247,
            lowsec_sys,
            athanor_pos + DVec3::new(3_000.0, 0.0, 0.0),
        )
        .unwrap_err()
        .code,
        "DeploymentTooCloseToStructure"
    );
    assert!(check_mobile_deployment_restrictions(
        &ctx,
        &sess,
        910_024,
        33477,
        1247,
        lowsec_sys,
        athanor_pos + DVec3::new(10_000.0, 0.0, 0.0),
    )
    .is_ok());
}

#[test]
fn step_actor_expiry_sweep_mtu_tractoring_and_ball_damage_destruction_loot() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let owner_id = 90_000_001_i64;
    let expired_id = 920_001_i64;
    let expired_child_id = 920_002_i64;
    let mtu_id = 920_003_i64;
    let far_wreck_id = 920_004_i64;
    let far_loot_id = 920_005_i64;
    let depot_anchoring_id = 920_006_i64;
    let depot_reinforced_id = 920_007_i64;
    let depot_loot_id = 920_008_i64;
    let beacon_id = 920_009_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&owner_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
        }
        snap.fleets.insert(
            777,
            FleetRecord {
                fleet_id: 777,
                boss_id: owner_id,
                members: vec![owner_id],
                wings: Vec::new(),
                squads: Vec::new(),
                free_move: false,
                is_registered: false,
                wing_details: Default::default(),
                squad_details: Default::default(),
                member_details: Default::default(),
                invites: HashSet::new(),
                motd: String::new(),
            },
        );
        put_space_item(snap, expired_id, TYPE_MOBILE_DEPOT, owner_id, sys_id, [0.0; 3]);
        put_space_item(snap, mtu_id, TYPE_MTU_STANDARD, owner_id, sys_id, [0.0; 3]);
        put_space_item(snap, far_wreck_id, 26557, owner_id, sys_id, [10_000.0, 0.0, 0.0]);
        put_space_item(snap, depot_anchoring_id, TYPE_MOBILE_DEPOT, owner_id, sys_id, [50_000.0, 0.0, 0.0]);
        put_space_item(snap, depot_reinforced_id, TYPE_MOBILE_DEPOT, owner_id, sys_id, [60_000.0, 0.0, 0.0]);
        put_space_item(snap, beacon_id, TYPE_MOBILE_CYNO_BEACON, owner_id, sys_id, [70_000.0, 0.0, 0.0]);
        for (iid, loc_id, qty) in [
            (expired_child_id, expired_id, 10),
            (far_loot_id, far_wreck_id, 50),
            (depot_loot_id, depot_reinforced_id, 100),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 34,
                    owner_id,
                    location_id: loc_id,
                    flag: InventoryFlag::Cargo,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    register_mobile_deployable(&ctx, expired_id, TYPE_MOBILE_DEPOT, GROUP_MOBILE_DEPOT, owner_id, sys_id, 60, DVec3::ZERO);
    register_mobile_deployable(&ctx, mtu_id, TYPE_MTU_STANDARD, GROUP_MOBILE_TRACTOR, owner_id, sys_id, 60, DVec3::ZERO);
    register_mobile_deployable(&ctx, depot_anchoring_id, TYPE_MOBILE_DEPOT, GROUP_MOBILE_DEPOT, owner_id, sys_id, 60, DVec3::new(50_000.0, 0.0, 0.0));
    register_mobile_deployable(&ctx, depot_reinforced_id, TYPE_MOBILE_DEPOT, GROUP_MOBILE_DEPOT, owner_id, sys_id, 60, DVec3::new(60_000.0, 0.0, 0.0));
    register_mobile_deployable(&ctx, beacon_id, TYPE_MOBILE_CYNO_BEACON, GROUP_MOBILE_CYNO_BEACON, owner_id, sys_id, 60, DVec3::new(70_000.0, 0.0, 0.0));

    // Fleet ID must be captured on registration
    assert_eq!(
        read_deployable_state(&ctx).deployables[&mtu_id].deployed_fleet_id,
        Some(777)
    );

    let now = ctx.now_unix() + 120;
    write_deployable_state(&ctx, |st| {
        st.deployables.get_mut(&expired_id).unwrap().expires_at_unix = now - 1;
        // Keep depot_anchoring_id still anchoring at `now`
        st.deployables.get_mut(&depot_anchoring_id).unwrap().anchor_start_unix = now - 10;
        // Mark depot_reinforced_id anchored and with expired reinforcement timer so step clears it
        let dr = st.deployables.get_mut(&depot_reinforced_id).unwrap();
        dr.anchor_completed = true;
        dr.reinforced_until_unix = Some(now - 5);
    });

    let mut ballpark = Ballpark::new(sys_id);
    ballpark.set_ball_slim_meta(
        100_000_001,
        SlimItemMeta {
            char_id: Some(owner_id),
            owner_id,
            ..Default::default()
        },
    );
    for (bid, tid, x) in [
        (expired_id, TYPE_MOBILE_DEPOT, 0.0),
        (mtu_id, TYPE_MTU_STANDARD, 0.0),
        (far_wreck_id, 26557, 10_000.0),
        (depot_anchoring_id, TYPE_MOBILE_DEPOT, 50_000.0),
        (depot_reinforced_id, TYPE_MOBILE_DEPOT, 60_000.0),
        (beacon_id, TYPE_MOBILE_CYNO_BEACON, 70_000.0),
    ] {
        ballpark.add_ball(Ball {
            id: bid,
            type_id: tid,
            position: DVec3::new(x, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 10_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        });
        ballpark.combat_states.insert(
            bid,
            CombatState {
                shield_hp: 200.0, // <= 25% of 1000 max shield for depot_reinforced_id
                max_shield_hp: 1_000.0,
                armor_hp: 500.0,
                max_armor_hp: 500.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                ..Default::default()
            },
        );
    }

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
        2.0, // dt = 2.0s -> pulls wreck closer by 2.0 * tractor_speed
        now,
    );

    // Expired deployable and its child item are swept
    assert!(!ballpark.balls.contains_key(&expired_id));
    assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&expired_id)));
    assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&expired_child_id)));

    // Far wreck was tractored closer than 10,000m
    let new_wreck_x = ballpark.balls[&far_wreck_id].position.x;
    assert!(new_wreck_x < 10_000.0 && new_wreck_x >= 2_500.0);

    // Anchoring depot takes direct lethal damage without reinforcement
    let db_opt = Some(Arc::clone(&ctx.db));
    let sde_opt = Some(Arc::clone(&ctx.sde));
    let lethal = DamageVector {
        em: 10_000.0,
        thermal: 0.0,
        kinetic: 0.0,
        explosive: 0.0,
    };
    let (_, destroyed_anch, _) = handle_ball_damage_in_actor(
        &db_opt,
        &sde_opt,
        sys_id,
        depot_anchoring_id,
        lethal,
        &mut ballpark,
        now,
    )
    .unwrap();
    assert!(destroyed_anch);

    // Anchored depot at <= 25% shield takes lethal damage and drops cargo container (type 23)
    let (_, destroyed_reinf, _) = handle_ball_damage_in_actor(
        &db_opt,
        &sde_opt,
        sys_id,
        depot_reinforced_id,
        lethal,
        &mut ballpark,
        now,
    )
    .unwrap();
    assert!(destroyed_reinf);
    assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&depot_reinforced_id)));

    // MTU & Cyno Beacon lethal damage destruction
    let (_, destroyed_mtu, _) =
        handle_ball_damage_in_actor(&db_opt, &sde_opt, sys_id, mtu_id, lethal, &mut ballpark, now)
            .unwrap();
    assert!(destroyed_mtu);

    let (_, destroyed_beacon, _) = handle_ball_damage_in_actor(
        &db_opt,
        &sde_opt,
        sys_id,
        beacon_id,
        lethal,
        &mut ballpark,
        now,
    )
    .unwrap();
    assert!(destroyed_beacon);
}

#[tokio::test]
async fn activate_micro_jump_unit_rejects_range_mass_warp_cloak_and_scram() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let mju_id = 930_001_i64;

    let mut sess = SessionState::new(94_200);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    // 1. Missing ship, missing system, invalid mju_id <= 0, and unknown mju_id
    let mut no_ship = sess.clone();
    no_ship.ship_id = None;
    assert_eq!(handle_activate_micro_jump_unit(&ctx, &no_ship, mju_id).await.unwrap_err().code, "NoActiveShip");
    let mut no_sys = sess.clone();
    no_sys.solar_system_id = None;
    assert_eq!(handle_activate_micro_jump_unit(&ctx, &no_sys, mju_id).await.unwrap_err().code, "NoSolarSystem");
    assert_eq!(handle_activate_micro_jump_unit(&ctx, &sess, 0).await.unwrap_err().code, "MissingItemID");
    assert_eq!(handle_activate_micro_jump_unit(&ctx, &sess, 999_999).await.unwrap_err().code, "DeployableNotFound");

    register_mobile_deployable(
        &ctx,
        mju_id,
        TYPE_MOBILE_MJU,
        GROUP_MOBILE_MICRO_JUMP_UNIT,
        char_id,
        sys_id,
        MMJU_ACTIVATION_DELAY_SECS,
        DVec3::ZERO,
    );
    ctx.advance_secs(MMJU_ACTIVATION_DELAY_SECS + 5);

    assert_eq!(handle_activate_micro_jump_unit(&ctx, &sess, mju_id).await.unwrap_err().code, "ShipNotInSpace");

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let spawn_ship = |pos: DVec3, mass: f64, mode: MovementMode, cs: CombatState| {
        let h = sys_h.clone();
        async move {
            let _ = h
                .spawn_ball_with_meta(
                    Ball {
                        id: ship_id,
                        type_id: 587,
                        position: pos,
                        velocity: DVec3::ZERO,
                        max_velocity: 300.0,
                        inertia: 2.0,
                        mass,
                        radius: 40.0,
                        speed_fraction: 0.0,
                        mode,
                    },
                    cs,
                    SlimItemMeta {
                        char_id: Some(char_id),
                        owner_id: char_id,
                        ..Default::default()
                    },
                )
                .await;
        }
    };

    spawn_ship(DVec3::new(3_000.0, 0.0, 0.0), 1_000_000.0, MovementMode::Stop, CombatState::default()).await;
    assert_eq!(handle_activate_micro_jump_unit(&ctx, &sess, mju_id).await.unwrap_err().code, "OutOfRange");

    spawn_ship(DVec3::new(500.0, 0.0, 0.0), MMJU_MAX_SHIP_MASS_KG, MovementMode::Stop, CombatState::default()).await;
    assert_eq!(handle_activate_micro_jump_unit(&ctx, &sess, mju_id).await.unwrap_err().code, "ShipTooHeavy");

    spawn_ship(
        DVec3::new(500.0, 0.0, 0.0),
        1_000_000.0,
        MovementMode::new_warp(DVec3::ZERO, DVec3::new(1e8, 0.0, 0.0), 3.0),
        CombatState::default(),
    )
    .await;
    assert_eq!(handle_activate_micro_jump_unit(&ctx, &sess, mju_id).await.unwrap_err().code, "ShipInWarp");

    spawn_ship(
        DVec3::new(500.0, 0.0, 0.0),
        1_000_000.0,
        MovementMode::Stop,
        CombatState { is_cloaked: true, ..Default::default() },
    )
    .await;
    assert_eq!(handle_activate_micro_jump_unit(&ctx, &sess, mju_id).await.unwrap_err().code, "ShipCloaked");

    spawn_ship(
        DVec3::new(500.0, 0.0, 0.0),
        1_000_000.0,
        MovementMode::Stop,
        CombatState { is_mwd_mjd_disabled: true, ..Default::default() },
    )
    .await;
    assert_eq!(handle_activate_micro_jump_unit(&ctx, &sess, mju_id).await.unwrap_err().code, "WarpScrambled");

    write_deployable_state(&ctx, |st| {
        st.deployables.get_mut(&mju_id).unwrap().expires_at_unix = 1;
    });
    assert_eq!(handle_activate_micro_jump_unit(&ctx, &sess, mju_id).await.unwrap_err().code, "DeployableExpired");
    write_deployable_state(&ctx, |st| {
        st.deployables.get_mut(&mju_id).unwrap().is_unanchored = true;
    });
    assert_eq!(handle_activate_micro_jump_unit(&ctx, &sess, mju_id).await.unwrap_err().code, "InvalidDeployable");
}
