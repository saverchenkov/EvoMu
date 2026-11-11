//! Semantic unit tests split from `fitting_conduit_jump_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::super::*;
use super::*;
use crate::services::inventory_fitting::fitting_cyno_portal::{
    handle_cyno_generator_activation, is_ship_cyno_active, on_cyno_ship_destroyed,
    sweep_expired_cyno_beacons,
};
use eve_db::{
    FleetMemberRecord, FleetRecord, FleetRole, InventoryFlag, InventoryItemRecord,
    TrainedSkillRecord, VirtualServerClock, WorldRepository,
};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;

#[tokio::test]
async fn bridge_to_fleet_beacon_and_jump_through_fleet_all_error_and_destroyed_cyno_branches() {
    let ctx = build_ctx();
    let (origin_sys, dest_sys, far_sys, poch_sys) = (30_002_505u32, 30_002_506u32, 30_002_507u32, 30_002_508u32);
    let (titan_pilot, cyno_pilot, pass_pilot, non_fleet_pilot) = (98_200_001, 98_200_002, 98_200_003, 98_200_004);
    let fleet_id = 710_001i64;
    let (titan_ship, cyno_ship, pass_ship, portal_mod, cyno_mod) =
        (1_000_900_001, 1_000_900_002, 1_000_900_003, 1_000_900_010, 1_000_900_011);

    seed_pilot_and_ship(&ctx, titan_pilot, 98_001, origin_sys, titan_ship, 999_801, Some(fleet_id));
    seed_pilot_and_ship(&ctx, cyno_pilot, 98_001, dest_sys, cyno_ship, 587, Some(fleet_id));
    seed_pilot_and_ship(&ctx, pass_pilot, 98_001, origin_sys, pass_ship, 587, Some(fleet_id));
    seed_pilot_and_ship(&ctx, non_fleet_pilot, 98_001, origin_sys, 1_000_900_004, 587, None);

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            fleet_id,
            FleetRecord { fleet_id, boss_id: titan_pilot, members: vec![titan_pilot, cyno_pilot, pass_pilot], ..Default::default() },
        );
        snap.items.insert(
            1_000_900_020,
            InventoryItemRecord {
                item_id: 1_000_900_020,
                type_id: 16273,
                owner_id: cyno_pilot,
                location_id: cyno_ship,
                flag: InventoryFlag::Cargo,
                quantity: 1000,
                singleton: false,
                ..Default::default()
            },
        );
    });

    spawn_ship_ball(&ctx, origin_sys, titan_ship, 999_801, titan_pilot, DVec3::ZERO, 50_000.0).await;
    spawn_ship_ball(&ctx, dest_sys, cyno_ship, 587, cyno_pilot, DVec3::new(10_000.0, 0.0, 0.0), 5_000.0).await;
    spawn_ship_ball(&ctx, origin_sys, pass_ship, 587, pass_pilot, DVec3::new(500.0, 0.0, 0.0), 5_000.0).await;

    let mut titan_sess = SessionState {
        user_id: Some(titan_pilot as u64),
        character_id: Some(titan_pilot),
        corporation_id: Some(98_001),
        solar_system_id: Some(origin_sys),
        ship_id: Some(titan_ship),
        ..Default::default()
    };

    // 1. Bridge destination restrictions: wormhole, Zarzakh, Pochven, out-of-range
    for (target_sys, expected_code) in [
        (31_000_005i64, "JumpDriveForbiddenInWormholeSpace"),
        (30_100_000i64, "JumpDriveForbiddenInZarzakh"),
        (poch_sys as i64, "CannotJumpToPochven"),
        (far_sys as i64, "JumpDistanceOutOfRange"),
    ] {
        let err = call_beyonce(
            &ctx,
            &mut titan_sess,
            "CmdBridgeToFleetModuleBeacon",
            &[PyRep::Integer(cyno_pilot), PyRep::Integer(target_sys), PyRep::Integer(1)],
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, expected_code);
    }

    // 2. Light a covert cyno on cyno_ship -> Titan with standard JPG cannot bridge to covert beacon
    let dest_handle = ctx.systems.get_or_spawn(dest_sys);
    let cov_cyno_type = ctx.sde.types.get(&28646).unwrap().clone();
    let cyno_kv = handle_cyno_generator_activation(&ctx, &dest_handle, cyno_pilot, cyno_ship, dest_sys, cyno_mod, &cov_cyno_type)
        .await
        .unwrap()
        .expect("light covert cyno");
    let cov_beacon_id = cyno_kv.dict_get("beaconID").unwrap().as_i64().unwrap();
    assert!(is_ship_cyno_active(&ctx, cyno_ship));

    assert_eq!(
        call_beyonce(&ctx, &mut titan_sess, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(cyno_pilot), PyRep::Integer(dest_sys as i64), PyRep::Integer(cov_beacon_id)])
            .await.unwrap_err().code,
        "NoJumpPortalGenerator"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            portal_mod,
            InventoryItemRecord {
                item_id: portal_mod,
                type_id: 999_804,
                owner_id: titan_pilot,
                location_id: titan_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(titan_ship);
    assert_eq!(
        call_beyonce(&ctx, &mut titan_sess, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(cyno_pilot), PyRep::Integer(dest_sys as i64), PyRep::Integer(cov_beacon_id)])
            .await.unwrap_err().code,
        "ShipCannotBridgeToBeacon"
    );

    // Destroy covert cyno and light standard cyno (21096)
    on_cyno_ship_destroyed(&ctx, cyno_ship);
    let _ = dest_handle.remove_module_cycle(cyno_ship, cyno_mod).await;
    let std_cyno_type = ctx.sde.types.get(&21096).unwrap().clone();
    let std_kv = handle_cyno_generator_activation(&ctx, &dest_handle, cyno_pilot, cyno_ship, dest_sys, cyno_mod, &std_cyno_type)
        .await
        .unwrap()
        .expect("light std cyno");
    let std_beacon_id = std_kv.dict_get("beaconID").unwrap().as_i64().unwrap();

    assert_eq!(
        call_beyonce(&ctx, &mut titan_sess, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(cyno_pilot), PyRep::Integer(dest_sys as i64), PyRep::Integer(std_beacon_id)])
            .await.unwrap_err().code,
        "NotEnoughFuel"
    );

    // Add Helium Isotopes & skill 24562 (Jump Portal Generation IV -> 40% reduction: 500 * 0.6 = 300)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(titan_pilot).or_default().insert(24562, TrainedSkillRecord { type_id: 24562, level: 4, skill_points: 100_000.0 });
        snap.items.insert(
            1_000_900_021,
            InventoryItemRecord {
                item_id: 1_000_900_021,
                type_id: 16274,
                owner_id: titan_pilot,
                location_id: titan_ship,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 300,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let bridge_ok = call_beyonce(
        &ctx,
        &mut titan_sess,
        "CmdBridgeThroughStargate",
        &[PyRep::Integer(cyno_pilot), PyRep::Integer(dest_sys as i64), PyRep::Integer(std_beacon_id)],
    )
    .await
    .expect("bridge open");
    assert_eq!(bridge_ok.dict_get("activationConsumptionQuantity"), Some(&PyRep::Integer(300)));

    // 3. CmdJumpThroughFleet error branches:
    let mut nf_sess = SessionState {
        user_id: Some(non_fleet_pilot as u64),
        character_id: Some(non_fleet_pilot),
        corporation_id: Some(98_001),
        solar_system_id: Some(origin_sys),
        ship_id: Some(1_000_900_004),
        ..Default::default()
    };
    assert_eq!(
        call_beyonce(&ctx, &mut nf_sess, "CmdJumpThroughFleet", &[PyRep::Integer(titan_pilot), PyRep::Integer(titan_ship)])
            .await.unwrap_err().code,
        "FleetNotInFleet"
    );

    // - passenger tethered -> CannotJumpWhileTethered
    let orig_handle = ctx.systems.get_or_spawn(origin_sys);
    let mut pass_cs = orig_handle.get_combat_state(pass_ship).await.unwrap().unwrap();
    pass_cs.is_tethered = true;
    let _ = orig_handle.set_combat_state(pass_ship, pass_cs.clone()).await;
    let mut pass_sess = SessionState {
        user_id: Some(pass_pilot as u64),
        character_id: Some(pass_pilot),
        corporation_id: Some(98_001),
        solar_system_id: Some(origin_sys),
        ship_id: Some(pass_ship),
        ..Default::default()
    };
    assert_eq!(
        call_beyonce(&ctx, &mut pass_sess, "CmdJumpThroughPortal", &[PyRep::Integer(titan_pilot), PyRep::Integer(titan_ship)])
            .await.unwrap_err().code,
        "CannotJumpWhileTethered"
    );
    pass_cs.is_tethered = false;
    let _ = orig_handle.set_combat_state(pass_ship, pass_cs).await;

    // - passenger > 2,500 m from Titan -> OutOfPortalRange; then Rifter lacks attr 861 -> JumpBridgeRestrictedShip
    spawn_ship_ball(&ctx, origin_sys, pass_ship, 587, pass_pilot, DVec3::new(4_000.0, 0.0, 0.0), 5_000.0).await;
    assert_eq!(
        call_beyonce(&ctx, &mut pass_sess, "CmdJumpThroughFleet", &[PyRep::Integer(titan_pilot), PyRep::Integer(titan_ship)])
            .await.unwrap_err().code,
        "OutOfPortalRange"
    );
    spawn_ship_ball(&ctx, origin_sys, pass_ship, 587, pass_pilot, DVec3::new(500.0, 0.0, 0.0), 5_000.0).await;
    assert_eq!(
        call_beyonce(&ctx, &mut pass_sess, "CmdJumpThroughFleet", &[PyRep::Integer(titan_pilot), PyRep::Integer(titan_ship)])
            .await.unwrap_err().code,
        "JumpBridgeRestrictedShip"
    );

    // - Upgrade passenger ship to 999_803 (has attr 861), destroy cyno ship to exercise 15s grace window -> lands at star!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&pass_ship).unwrap().type_id = 999_803;
        snap.items.insert(
            1_000_900_022,
            InventoryItemRecord {
                item_id: 1_000_900_022,
                type_id: 17888,
                owner_id: titan_pilot,
                location_id: titan_ship,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 50_000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(pass_ship);
    on_cyno_ship_destroyed(&ctx, cyno_ship);
    let jt_star = call_beyonce(&ctx, &mut pass_sess, "CmdJumpThroughFleet", &[PyRep::Integer(titan_pilot), PyRep::Integer(titan_ship)])
        .await
        .expect("jump through destroyed cyno lands at star");
    assert_eq!(jt_star.dict_get("solarSystemID"), Some(&PyRep::Integer(dest_sys as i64)));
}

#[tokio::test]
async fn group_conduit_jump_restrictions_rorqual_passengers_and_jammer_state() {
    let ctx = build_ctx();
    let (origin_sys, dest_sys, poch_sys) = (30_002_505u32, 30_002_506u32, 30_002_508u32);
    let (leader, cyno_pilot, barge_pilot) = (98_200_010, 98_200_011, 98_200_012);
    let fleet_id = 710_002i64;
    let (leader_ship, cyno_ship, barge_ship) = (1_000_910_001, 1_000_910_002, 1_000_910_003);

    seed_pilot_and_ship(&ctx, leader, 98_001, origin_sys, leader_ship, 999_801, Some(fleet_id));
    seed_pilot_and_ship(&ctx, cyno_pilot, 98_001, dest_sys, cyno_ship, 587, Some(fleet_id));
    seed_pilot_and_ship(&ctx, barge_pilot, 98_001, origin_sys, barge_ship, 999_805, Some(fleet_id));

    ctx.db.with_mut_snapshot(|snap| {
        let mut f = FleetRecord { fleet_id, boss_id: leader, members: vec![leader, cyno_pilot, barge_pilot], ..Default::default() };
        f.member_details.insert(
            barge_pilot,
            FleetMemberRecord {
                character_id: barge_pilot,
                role: FleetRole::SquadMember,
                wing_id: None,
                squad_id: None,
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: String::new(),
                join_time_unix: 1_800_000_000,
            },
        );
        snap.fleets.insert(fleet_id, f);
        snap.items.insert(
            1_000_910_020,
            InventoryItemRecord {
                item_id: 1_000_910_020,
                type_id: 16273,
                owner_id: cyno_pilot,
                location_id: cyno_ship,
                flag: InventoryFlag::Cargo,
                quantity: 1000,
                singleton: false,
                ..Default::default()
            },
        );
    });

    spawn_ship_ball(&ctx, origin_sys, leader_ship, 999_801, leader, DVec3::ZERO, 50_000.0).await;
    spawn_ship_ball(&ctx, dest_sys, cyno_ship, 587, cyno_pilot, DVec3::ZERO, 5_000.0).await;
    spawn_ship_ball(&ctx, origin_sys, barge_ship, 999_805, barge_pilot, DVec3::new(1_500.0, 0.0, 0.0), 5_000.0).await;

    let dest_handle = ctx.systems.get_or_spawn(dest_sys);
    let std_cyno_type = ctx.sde.types.get(&21096).unwrap().clone();
    let std_kv = handle_cyno_generator_activation(&ctx, &dest_handle, cyno_pilot, cyno_ship, dest_sys, 1_000_910_011, &std_cyno_type)
        .await
        .unwrap()
        .expect("light cyno");
    let beacon_id = std_kv.dict_get("beaconID").unwrap().as_i64().unwrap();

    let mut sess = SessionState {
        user_id: Some(leader as u64),
        character_id: Some(leader),
        corporation_id: Some(98_001),
        solar_system_id: Some(origin_sys),
        ship_id: Some(leader_ship),
        ..Default::default()
    };

    // 1. Group jump destination validation + TitanCannotConduitJump
    for (target_sys, expected_code) in [
        (31_000_005i64, "JumpDriveForbiddenInWormholeSpace"),
        (origin_sys as i64, "InvalidJumpDestination"),
        (30_100_000i64, "JumpDriveForbiddenInZarzakh"),
        (poch_sys as i64, "CannotJumpToPochven"),
        (dest_sys as i64, "TitanCannotConduitJump"),
    ] {
        assert_eq!(
            call_beyonce(&ctx, &mut sess, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_pilot), PyRep::Integer(target_sys), PyRep::Integer(beacon_id)])
                .await.unwrap_err().code,
            expected_code
        );
    }

    // 2. Carrier without attr 3131 -> ShipCannotConduitJump
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&leader_ship).unwrap().type_id = 999_803;
    });
    ctx.invalidate_ship_fitting(leader_ship);
    assert_eq!(
        call_beyonce(&ctx, &mut sess, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_pilot), PyRep::Integer(dest_sys as i64), PyRep::Integer(beacon_id)])
            .await.unwrap_err().code,
        "ShipCannotConduitJump"
    );

    // 3. Rorqual (999_802) with active Industrial Core -> IndustrialCoreActive; low cap -> NotEnoughCapacitor; then valid conduit jump!
    let indy_core_id = 1_000_910_030i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&leader_ship).unwrap().type_id = 999_802;
        snap.items.insert(
            indy_core_id,
            InventoryItemRecord {
                item_id: indy_core_id,
                type_id: 28583,
                owner_id: leader,
                location_id: leader_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(indy_core_id);
    });
    ctx.invalidate_ship_fitting(leader_ship);
    assert_eq!(
        call_beyonce(&ctx, &mut sess, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_pilot), PyRep::Integer(dest_sys as i64), PyRep::Integer(beacon_id)])
            .await.unwrap_err().code,
        "IndustrialCoreActive"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&indy_core_id);
    });
    let orig_handle = ctx.systems.get_or_spawn(origin_sys);
    let mut rorq_cs = orig_handle.get_combat_state(leader_ship).await.unwrap().unwrap();
    rorq_cs.capacitor = 10.0;
    let _ = orig_handle.set_combat_state(leader_ship, rorq_cs.clone()).await;
    assert_eq!(
        call_beyonce(&ctx, &mut sess, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_pilot), PyRep::Integer(dest_sys as i64), PyRep::Integer(beacon_id)])
            .await.unwrap_err().code,
        "NotEnoughCapacitor"
    );

    rorq_cs.capacitor = 50_000.0;
    let _ = orig_handle.set_combat_state(leader_ship, rorq_cs).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            1_000_910_031,
            InventoryItemRecord {
                item_id: 1_000_910_031,
                type_id: 17888,
                owner_id: leader,
                location_id: leader_ship,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 5_000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    on_cyno_ship_destroyed(&ctx, cyno_ship);
    let rorq_conduit = call_beyonce(
        &ctx,
        &mut sess,
        "CmdGroupJumpToFleetModuleBeacon",
        &[PyRep::Integer(cyno_pilot), PyRep::Integer(dest_sys as i64), PyRep::Integer(beacon_id)],
    )
    .await
    .expect("rorqual conduit jump");
    assert_eq!(rorq_conduit.dict_get("solarSystemID"), Some(&PyRep::Integer(dest_sys as i64)));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&barge_ship].location_id), dest_sys as i64);

    // 4. GetCynoJammerState branches: missing solarSystemID, Zarzakh jammed
    let mut no_sys_sess = SessionState::default();
    assert_eq!(call_beyonce(&ctx, &mut no_sys_sess, "GetCynoJammerState", &[]).await.unwrap_err().code, "NoSolarSystem");
    let mut zar_sess = SessionState { solar_system_id: Some(30_100_000), ..Default::default() };
    assert!(matches!(call_beyonce(&ctx, &mut zar_sess, "GetCynoJammerState", &[]).await.unwrap(), PyRep::Integer(_)));
    sweep_expired_cyno_beacons(&ctx);
}

fn build_conduit_action2_ctx() -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    sde.solar_systems.insert(30_002_505, custom_sys(&sde, 30_002_505, 10_000_030, -0.5, [0.0, 0.0, 0.0]));
    sde.solar_systems.insert(30_002_506, custom_sys(&sde, 30_002_506, 10_000_030, -0.5, [2.0, 0.0, 0.0]));
    sde.solar_systems.insert(30_000_142, custom_sys(&sde, 30_000_142, 10_000_002, 0.9, [1.0, 0.0, 0.0]));
    sde.solar_systems.insert(32_000_005, custom_sys(&sde, 32_000_005, 10_000_030, -1.0, [1.0, 0.0, 0.0]));

    sde.type_lists.insert(
        99_401,
        TypeListInfo {
            type_list_id: 99_401,
            name: "Standard Cyno Beacons".into(),
            included_type_ids: vec![21094, 21096, 56099, 57425, 35840],
            ..Default::default()
        },
    );
    sde.type_lists.insert(
        99_402,
        TypeListInfo {
            type_list_id: 99_402,
            name: "Covert Cyno Beacons".into(),
            included_type_ids: vec![28646, 59631],
            ..Default::default()
        },
    );

    sde.types.insert(57425, custom_inv_type(&sde, 57425, 650, 2, 1.0, "Cynosural Field", &[]));
    sde.types.insert(56099, custom_inv_type(&sde, 56099, 650, 2, 1.0, "Mobile Cynosural Beacon", &[]));
    sde.types.insert(59631, custom_inv_type(&sde, 59631, 650, 2, 1.0, "Covert Cynosural Field", &[(1252, 1.0)]));
    sde.types.insert(35840, custom_inv_type(&sde, 35840, 1408, 65, 1.0, "Pharolux Cyno Beacon", &[]));
    sde.types.insert(
        21096,
        custom_inv_type(&sde, 21096, 658, 7, 10.0, "Cynosural Field Generator I", &[(73, 60_000.0), (713, 16273.0), (714, 100.0)]),
    );
    sde.types.insert(
        28646,
        custom_inv_type(&sde, 28646, 658, 7, 10.0, "Covert Cynosural Field Generator I", &[(73, 60_000.0), (713, 16273.0), (714, 10.0), (1252, 1.0)]),
    );
    sde.types.insert(
        11578,
        custom_inv_type(&sde, 11578, 330, 7, 5.0, "Covert Ops Cloaking Device II", &[(1298, 834.0)]),
    );
    sde.types.insert(
        999_804,
        custom_inv_type(
            &sde,
            999_804,
            657,
            7,
            100.0,
            "Jump Portal Generator I",
            &[(73, 60_000.0), (713, 16274.0), (714, 100.0), (1001, 0.000001), (1002, 30_000.0), (1298, 30.0), (3125, 1.0), (3318, 861.0)],
        ),
    );
    sde.types.insert(
        28652,
        custom_inv_type(
            &sde,
            28652,
            657,
            7,
            100.0,
            "Covert Jump Portal Generator I",
            &[(73, 60_000.0), (713, 16274.0), (714, 100.0), (1001, 0.000001), (1002, 30_000.0), (1252, 1.0), (1298, 898.0), (3125, 1.0), (3318, 3322.0)],
        ),
    );
    // Standard Titan, Carrier (547), Lancer Dread (5120), Black Ops (898)
    sde.types.insert(
        999_801,
        custom_inv_type(&sde, 999_801, 30, 6, 100_000_000.0, "Test Titan", &[(861, 1.0), (866, 17888.0), (867, 6.0), (868, 500.0), (898, 0.20), (3125, 1.0), (3317, 99_401.0)]),
    );
    sde.types.insert(
        999_810,
        custom_inv_type(&sde, 999_810, 898, 6, 12_000_000.0, "Test Black Ops", &[(861, 1.0), (866, 17888.0), (867, 8.0), (868, 400.0), (898, 0.25), (3125, 1.0), (3131, 150.0), (3317, 99_402.0), (3322, 1.0)]),
    );
    sde.types.insert(
        999_811,
        custom_inv_type(&sde, 999_811, 5120, 6, 50_000_000.0, "Test Lancer", &[(861, 1.0), (866, 17888.0), (867, 6.0), (868, 500.0), (898, 0.20), (3131, 150.0), (3317, 99_401.0)]),
    );
    sde.types.insert(
        999_812,
        custom_inv_type(&sde, 999_812, 547, 6, 40_000_000.0, "Test Carrier", &[(861, 1.0), (866, 17888.0), (867, 6.0), (868, 500.0), (898, 0.20), (3131, 150.0), (3317, 99_401.0)]),
    );
    sde.types.insert(
        999_813,
        custom_inv_type(&sde, 999_813, 834, 6, 1_500_000.0, "Test Bomber", &[(3322, 1.0)]),
    );
    sde.types.insert(
        999_814,
        custom_inv_type(&sde, 999_814, 26, 6, 10_000_000.0, "Test Cruiser", &[(861, 1.0)]),
    );
    sde.types.insert(
        999_815,
        custom_inv_type(&sde, 999_815, 26, 6, 10_000_000.0, "NonCap Cruiser", &[]),
    );

    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

#[tokio::test]
async fn deployable_beacons_bridge_lifecycle_and_covert_or_pharolux_paths() {
    let ctx = build_conduit_action2_ctx();
    let (origin_sys, dest_sys, highsec_sys, abyss_sys) = (30_002_505u32, 30_002_506u32, 30_000_142u32, 32_000_005u32);
    let (titan_p, cov_p, cyno_p, pass_p) = (98_210_001, 98_210_002, 98_210_003, 98_210_004);
    let fleet_id = 710_101i64;
    let (titan_s, cov_s, cyno_s, pass_s) = (1_000_920_001, 1_000_920_002, 1_000_920_003, 1_000_920_004);
    let (dep_beacon_id, pharolux_id) = (1_000_920_050i64, 1_000_920_051i64);

    for (cid, sys, sid, stid) in [
        (titan_p, origin_sys, titan_s, 999_801),
        (cov_p, origin_sys, cov_s, 999_810),
        (cyno_p, dest_sys, cyno_s, 587),
        (pass_p, origin_sys, pass_s, 999_814),
    ] {
        seed_pilot_and_ship(&ctx, cid, 98_001, sys, sid, stid, Some(fleet_id));
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(fleet_id, FleetRecord { fleet_id, boss_id: titan_p, members: vec![titan_p, cov_p, cyno_p, pass_p], ..Default::default() });
        snap.items.insert(1_000_920_010, InventoryItemRecord { item_id: 1_000_920_010, type_id: 999_804, owner_id: titan_p, location_id: titan_s, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default() });
        snap.items.insert(1_000_920_011, InventoryItemRecord { item_id: 1_000_920_011, type_id: 28652, owner_id: cov_p, location_id: cov_s, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default() });
        snap.items.insert(1_000_920_012, InventoryItemRecord { item_id: 1_000_920_012, type_id: 16274, owner_id: titan_p, location_id: titan_s, flag: InventoryFlag::SpecializedFuelBay, quantity: 50_000, ..Default::default() });
        snap.items.insert(1_000_920_013, InventoryItemRecord { item_id: 1_000_920_013, type_id: 17888, owner_id: titan_p, location_id: titan_s, flag: InventoryFlag::SpecializedFuelBay, quantity: 50_000, ..Default::default() });
        snap.items.insert(1_000_920_014, InventoryItemRecord { item_id: 1_000_920_014, type_id: 16274, owner_id: cov_p, location_id: cov_s, flag: InventoryFlag::SpecializedFuelBay, quantity: 50_000, ..Default::default() });
        snap.items.insert(1_000_920_015, InventoryItemRecord { item_id: 1_000_920_015, type_id: 17888, owner_id: cov_p, location_id: cov_s, flag: InventoryFlag::SpecializedFuelBay, quantity: 50_000, ..Default::default() });
        snap.items.insert(1_000_920_016, InventoryItemRecord { item_id: 1_000_920_016, type_id: 16273, owner_id: cyno_p, location_id: cyno_s, flag: InventoryFlag::Cargo, quantity: 1_000, ..Default::default() });
        snap.items.insert(dep_beacon_id, InventoryItemRecord { item_id: dep_beacon_id, type_id: 56099, owner_id: cyno_p, location_id: dest_sys as i64, flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default() });
        snap.structures.insert(pharolux_id, eve_db::UpwellStructureRecord {
            structure_id: pharolux_id,
            type_id: 35840,
            owner_corp_id: 98_001,
            solar_system_id: dest_sys,
            name: "Pharolux Test".into(),
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 500,
            ..Default::default()
        });
    });

    spawn_ship_ball(&ctx, origin_sys, titan_s, 999_801, titan_p, DVec3::ZERO, 50_000.0).await;
    spawn_ship_ball(&ctx, origin_sys, cov_s, 999_810, cov_p, DVec3::new(100.0, 0.0, 0.0), 50_000.0).await;
    spawn_ship_ball(&ctx, origin_sys, pass_s, 999_814, pass_p, DVec3::new(300.0, 0.0, 0.0), 5_000.0).await;
    spawn_ship_ball(&ctx, dest_sys, cyno_s, 587, cyno_p, DVec3::new(500.0, 0.0, 0.0), 5_000.0).await;
    let dest_handle = ctx.systems.get_or_spawn(dest_sys);
    let _ = dest_handle.spawn_ball(Ball { id: dep_beacon_id, type_id: 56099, position: DVec3::new(10_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;

    let mut titan_sess = SessionState { user_id: Some(titan_p as u64), character_id: Some(titan_p), corporation_id: Some(98_001), solar_system_id: Some(origin_sys), ship_id: Some(titan_s), ..Default::default() };
    let mut cov_sess = SessionState { user_id: Some(cov_p as u64), character_id: Some(cov_p), corporation_id: Some(98_001), solar_system_id: Some(origin_sys), ship_id: Some(cov_s), ..Default::default() };
    let mut pass_sess = SessionState { user_id: Some(pass_p as u64), character_id: Some(pass_p), corporation_id: Some(98_001), solar_system_id: Some(origin_sys), ship_id: Some(pass_s), ..Default::default() };

    // 1. Abyssal & HighSec destination guards on deployable bridge/group/resolved helpers
    assert_eq!(call_beyonce(&ctx, &mut titan_sess, "CmdBridgeToFleetDeployableBeacon", &[PyRep::Integer(cyno_p), PyRep::Integer(abyss_sys as i64), PyRep::Integer(dep_beacon_id)]).await.unwrap_err().code, "JumpDriveForbiddenInAbyssalSpace");
    assert_eq!(call_beyonce(&ctx, &mut cov_sess, "CmdGroupJumpToFleetDeployableBeacon", &[PyRep::Integer(cyno_p), PyRep::Integer(abyss_sys as i64), PyRep::Integer(dep_beacon_id)]).await.unwrap_err().code, "JumpDriveForbiddenInAbyssalSpace");
    let hs_beacon = FleetModuleBeacon { beacon_id: 9001, char_id: cyno_p, ship_id: cyno_s, beacon_type_id: 57425, solar_system_id: highsec_sys, position: DVec3::ZERO, expires_at_unix: u64::MAX };
    assert_eq!(bridge_to_resolved_beacon(&ctx, &titan_sess, hs_beacon.clone(), None).await.unwrap_err().code, "CannotJumpToHighSec");
    assert_eq!(group_jump_to_resolved_beacon(&ctx, &mut cov_sess, hs_beacon, false).await.err().expect("CannotJumpToHighSec").code, "CannotJumpToHighSec");

    // 2. Open deployable bridge -> duplicate blocked by active immobilizing cycle / bridge -> advance past duration purges expired bridge -> re-open succeeds!
    call_beyonce(&ctx, &mut titan_sess, "CmdBridgeToFleetDeployableBeacon", &[PyRep::Integer(cyno_p), PyRep::Integer(dest_sys as i64), PyRep::Integer(dep_beacon_id)]).await.expect("deployable bridge open");
    assert_eq!(call_beyonce(&ctx, &mut titan_sess, "CmdBridgeToFleetDeployableBeacon", &[PyRep::Integer(cyno_p), PyRep::Integer(dest_sys as i64), PyRep::Integer(dep_beacon_id)]).await.unwrap_err().code, "WarpScrambled");
    let orig_handle = ctx.systems.get_or_spawn(origin_sys);
    let _ = orig_handle.remove_module_cycle(titan_s, 1_000_920_010).await;
    assert_eq!(call_beyonce(&ctx, &mut titan_sess, "CmdBridgeToFleetDeployableBeacon", &[PyRep::Integer(cyno_p), PyRep::Integer(dest_sys as i64), PyRep::Integer(dep_beacon_id)]).await.unwrap_err().code, "CynosuralFieldActive");
    ctx.advance_secs(100);
    call_beyonce(&ctx, &mut titan_sess, "CmdBridgeToFleetDeployableBeacon", &[PyRep::Integer(cyno_p), PyRep::Integer(dest_sys as i64), PyRep::Integer(dep_beacon_id)]).await.expect("expired bridge purged");

    // 3. NoActiveBridge variants: wrong origin system, missing bridge, bridger removed from space; + SessionChangeTimerActive
    spawn_ship_ball(&ctx, dest_sys, pass_s, 999_814, pass_p, DVec3::ZERO, 5_000.0).await;
    let mut wrong_sys_sess = pass_sess.clone();
    wrong_sys_sess.solar_system_id = Some(dest_sys);
    assert_eq!(call_beyonce(&ctx, &mut wrong_sys_sess, "CmdJumpThroughFleet", &[PyRep::Integer(titan_p), PyRep::Integer(titan_s)]).await.unwrap_err().code, "NoActiveBridge");
    let _ = dest_handle.remove_ball(pass_s).await;
    assert_eq!(call_beyonce(&ctx, &mut pass_sess, "CmdJumpThroughFleet", &[PyRep::Integer(cov_p), PyRep::Integer(cov_s)]).await.unwrap_err().code, "NoActiveBridge");

    let cur_now = ctx.now_unix();
    crate::services::space_destiny::record_session_change_timer(&ctx, pass_p, cur_now, origin_sys, 1, "test");
    assert_eq!(call_beyonce(&ctx, &mut pass_sess, "CmdJumpThroughFleet", &[PyRep::Integer(titan_p), PyRep::Integer(titan_s)]).await.unwrap_err().code, "SessionChangeTimerActive");
    ctx.advance_secs(20);

    // 4. Covert Portal (28652) to covert cyno (28646) + JumpThroughFleet covert passenger check
    let cov_cyno_t = ctx.sde.types.get(&28646).unwrap().clone();
    let cov_kv = handle_cyno_generator_activation(&ctx, &dest_handle, cyno_p, cyno_s, dest_sys, 1_000_920_020, &cov_cyno_t).await.unwrap().expect("covert cyno");
    let cov_bid = cov_kv.dict_get("beaconID").unwrap().as_i64().unwrap();
    call_beyonce(&ctx, &mut cov_sess, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(cyno_p), PyRep::Integer(dest_sys as i64), PyRep::Integer(cov_bid)]).await.expect("covert bridge");
    assert_eq!(call_beyonce(&ctx, &mut pass_sess, "CmdJumpThroughFleet", &[PyRep::Integer(cov_p), PyRep::Integer(cov_s)]).await.unwrap_err().code, "JumpBridgeRestrictedShip");
    ctx.db.with_mut_snapshot(|s| { s.items.get_mut(&pass_s).unwrap().type_id = 999_813; });
    ctx.invalidate_ship_fitting(pass_s);
    let cov_jtf = call_beyonce(&ctx, &mut pass_sess, "CmdJumpThroughFleet", &[PyRep::Integer(cov_p), PyRep::Integer(cov_s)]).await.expect("covert passenger jumps");
    assert_eq!(cov_jtf.dict_get("solarSystemID"), Some(&PyRep::Integer(dest_sys as i64)));

    // 5. Immobilizing module cycle blocks bridge -> clearing cycle allows Pharolux structure beacon (35_840) bridge; bridger ball removed -> NoActiveBridge
    ctx.advance_secs(120);
    let _ = orig_handle.remove_module_cycle(titan_s, 1_000_920_010).await;
    let _ = orig_handle.add_module_cycle(crate::system_actor::ActiveModuleCycle {
        ship_id: titan_s,
        module_id: 9_999_001,
        module_type_id: 999_804,
        effect_name: "busy".into(),
        repeat: 1,
        duration_s: 60.0,
        immobilize_self: true,
        ..Default::default()
    }).await;
    let pharolux_b = FleetModuleBeacon { beacon_id: pharolux_id, char_id: cyno_p, ship_id: 0, beacon_type_id: 35_840, solar_system_id: dest_sys, position: DVec3::new(20_000.0, 0.0, 0.0), expires_at_unix: u64::MAX };
    assert_eq!(bridge_to_resolved_beacon(&ctx, &titan_sess, pharolux_b.clone(), Some(pharolux_id)).await.unwrap_err().code, "WarpScrambled");
    let _ = orig_handle.remove_module_cycle(titan_s, 9_999_001).await;
    bridge_to_resolved_beacon(&ctx, &titan_sess, pharolux_b, Some(pharolux_id)).await.expect("pharolux bridge");

    // Reset pass_s back to origin_sys within 2,500m of Titan to jump via Pharolux 35_840, then remove titan ball -> NoActiveBridge
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&pass_p).unwrap();
        ch.solar_system_id = origin_sys;
        ch.jump_cooldown_until_unix = 0;
        let sh = s.items.get_mut(&pass_s).unwrap();
        sh.type_id = 999_814;
        sh.location_id = origin_sys as i64;
    });
    pass_sess.solar_system_id = Some(origin_sys);
    spawn_ship_ball(&ctx, origin_sys, pass_s, 999_814, pass_p, DVec3::new(300.0, 0.0, 0.0), 5_000.0).await;
    let ph_res = call_beyonce(&ctx, &mut pass_sess, "CmdJumpThroughFleet", &[PyRep::Integer(titan_p), PyRep::Integer(titan_s)]).await.expect("pharolux passenger jump");
    assert_eq!(ph_res.dict_get("solarSystemID"), Some(&PyRep::Integer(dest_sys as i64)));

    let _ = orig_handle.remove_module_cycle(titan_s, 1_000_920_010).await;
    let _ = orig_handle.remove_ball(titan_s).await;
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&pass_p).unwrap();
        ch.solar_system_id = origin_sys;
        ch.jump_cooldown_until_unix = 0;
        s.items.get_mut(&pass_s).unwrap().location_id = origin_sys as i64;
    });
    pass_sess.solar_system_id = Some(origin_sys);
    spawn_ship_ball(&ctx, origin_sys, pass_s, 999_814, pass_p, DVec3::new(300.0, 0.0, 0.0), 5_000.0).await;
    assert_eq!(call_beyonce(&ctx, &mut pass_sess, "CmdJumpThroughFleet", &[PyRep::Integer(titan_p), PyRep::Integer(titan_s)]).await.unwrap_err().code, "NoActiveBridge");

    // 6. CmdJumpToFleetDeployableBeacon & CmdJumpToFleetModuleBeacon standalone jumps (Titan jumps cleanly!)
    spawn_ship_ball(&ctx, origin_sys, titan_s, 999_801, titan_p, DVec3::ZERO, 50_000.0).await;
    let jmp_dep = call_beyonce(&ctx, &mut titan_sess, "CmdJumpToFleetDeployableBeacon", &[PyRep::Integer(cyno_p), PyRep::Integer(dest_sys as i64), PyRep::Integer(dep_beacon_id)]).await.expect("jump deployable");
    assert_eq!(jmp_dep.dict_get("solarSystemID"), Some(&PyRep::Integer(dest_sys as i64)));
    ctx.advance_secs(400);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&titan_p).unwrap().solar_system_id = origin_sys;
        s.characters.get_mut(&titan_p).unwrap().jump_cooldown_until_unix = 0;
        s.items.get_mut(&titan_s).unwrap().location_id = origin_sys as i64;
    });
    titan_sess.solar_system_id = Some(origin_sys);
    spawn_ship_ball(&ctx, origin_sys, titan_s, 999_801, titan_p, DVec3::ZERO, 50_000.0).await;
    let std_cyno_t = ctx.sde.types.get(&21096).unwrap().clone();
    on_cyno_ship_destroyed(&ctx, cyno_s);
    let _ = dest_handle.remove_module_cycle(cyno_s, 1_000_920_020).await;
    let std_kv = handle_cyno_generator_activation(&ctx, &dest_handle, cyno_p, cyno_s, dest_sys, 1_000_920_021, &std_cyno_t).await.unwrap().expect("std cyno");
    let std_bid = std_kv.dict_get("beaconID").unwrap().as_i64().unwrap();
    let jmp_mod = call_beyonce(&ctx, &mut titan_sess, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_p), PyRep::Integer(dest_sys as i64), PyRep::Integer(std_bid)]).await.expect("jump module");
    assert_eq!(jmp_mod.dict_get("solarSystemID"), Some(&PyRep::Integer(dest_sys as i64)));
}

#[tokio::test]
async fn group_conduit_jump_passenger_filters_lancer_carrier_caps_and_cap_writeback() {
    let ctx = build_conduit_action2_ctx();
    let (origin_sys, dest_sys) = (30_002_505u32, 30_002_506u32);
    let (leader_p, cyno_p) = (98_220_001, 98_220_002);
    let (leader_s, cyno_s) = (1_000_930_001, 1_000_930_002);
    let fleet_id = 710_102i64;

    seed_pilot_and_ship(&ctx, leader_p, 98_001, origin_sys, leader_s, 999_810, Some(fleet_id));
    seed_pilot_and_ship(&ctx, cyno_p, 98_001, dest_sys, cyno_s, 587, Some(fleet_id));

    // Passengers for Blops covert conduit filter matrix:
    // p_bomber (ok), p_cloaked_cruiser (ok via fitted 11578), p_plain_cruiser (skip no cloak),
    // p_declined (skip !accepts_conduit_jumps), p_cd (skip cooldown), p_tether (skip tethered), p_far (skip >10km)
    let pass_pids: Vec<i64> = (10..=16).map(|n| 98_220_000 + n).collect();
    let pass_sids: Vec<i64> = (10..=16).map(|n| 1_000_930_000 + n).collect();
    for (idx, (&pid, &sid)) in pass_pids.iter().zip(pass_sids.iter()).enumerate() {
        let tid = if idx == 0 || idx >= 4 { 999_813 } else { 999_815 };
        seed_pilot_and_ship(&ctx, pid, 98_001, origin_sys, sid, tid, Some(fleet_id));
    }

    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut f = FleetRecord { fleet_id, boss_id: leader_p, members: vec![leader_p, cyno_p], ..Default::default() };
        for (idx, &pid) in pass_pids.iter().enumerate() {
            f.members.push(pid);
            f.member_details.insert(pid, FleetMemberRecord {
                character_id: pid,
                role: FleetRole::SquadMember,
                wing_id: None,
                squad_id: None,
                takes_fleet_warp: false,
                accepts_regroup: true,
                accepts_conduit_jumps: idx != 3,
                job: 0,
                role_name: String::new(),
                join_time_unix: 0,
            });
        }
        snap.fleets.insert(fleet_id, f);
        snap.characters.get_mut(&pass_pids[4]).unwrap().jump_cooldown_until_unix = now_u + 120;
        // Fitted covert cloak (11578) on plain cruiser pass_sids[1]
        snap.items.insert(1_000_930_090, InventoryItemRecord {
            item_id: 1_000_930_090, type_id: 11578, owner_id: pass_pids[1], location_id: pass_sids[1], flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(1_000_930_091, InventoryItemRecord {
            item_id: 1_000_930_091, type_id: 17888, owner_id: leader_p, location_id: leader_s, flag: InventoryFlag::SpecializedFuelBay, quantity: 50_000, ..Default::default()
        });
        snap.items.insert(1_000_930_092, InventoryItemRecord {
            item_id: 1_000_930_092, type_id: 16273, owner_id: cyno_p, location_id: cyno_s, flag: InventoryFlag::Cargo, quantity: 1_000, ..Default::default()
        });
    });

    spawn_ship_ball(&ctx, origin_sys, leader_s, 999_810, leader_p, DVec3::ZERO, 50_000.0).await;
    spawn_ship_ball(&ctx, dest_sys, cyno_s, 587, cyno_p, DVec3::ZERO, 5_000.0).await;
    for (idx, (&pid, &sid)) in pass_pids.iter().zip(pass_sids.iter()).enumerate() {
        let tid = if idx == 0 || idx >= 4 { 999_813 } else { 999_815 };
        let pos = if idx == 6 { DVec3::new(15_000.0, 0.0, 0.0) } else { DVec3::new(1_000.0 + idx as f64 * 50.0, 0.0, 0.0) };
        spawn_ship_ball(&ctx, origin_sys, sid, tid, pid, pos, 5_000.0).await;
    }
    let orig_handle = ctx.systems.get_or_spawn(origin_sys);
    let mut tether_cs = orig_handle.get_combat_state(pass_sids[5]).await.unwrap().unwrap();
    tether_cs.is_tethered = true;
    let _ = orig_handle.set_combat_state(pass_sids[5], tether_cs).await;

    let dest_handle = ctx.systems.get_or_spawn(dest_sys);
    let cov_cyno_t = ctx.sde.types.get(&28646).unwrap().clone();
    let cov_kv = handle_cyno_generator_activation(&ctx, &dest_handle, cyno_p, cyno_s, dest_sys, 1_000_930_095, &cov_cyno_t).await.unwrap().expect("covert cyno");
    let cov_bid = cov_kv.dict_get("beaconID").unwrap().as_i64().unwrap();

    let mut leader_sess = SessionState { user_id: Some(leader_p as u64), character_id: Some(leader_p), corporation_id: Some(98_001), solar_system_id: Some(origin_sys), ship_id: Some(leader_s), ..Default::default() };

    // Leader session change timer -> SessionChangeTimerActive
    crate::services::space_destiny::record_session_change_timer(&ctx, leader_p, now_u, origin_sys, 1, "test");
    assert_eq!(call_beyonce(&ctx, &mut leader_sess, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_p), PyRep::Integer(dest_sys as i64), PyRep::Integer(cov_bid)]).await.unwrap_err().code, "SessionChangeTimerActive");
    ctx.advance_secs(20);

    // Execute Blops group conduit jump -> only pass_sids[0] (bomber) + pass_sids[1] (fitted covert cloak) jump!
    let cov_b = FleetModuleBeacon { beacon_id: cov_bid, char_id: cyno_p, ship_id: cyno_s, beacon_type_id: 28646, solar_system_id: dest_sys, position: DVec3::ZERO, expires_at_unix: u64::MAX };
    let (_, blops_jumped) = group_jump_to_resolved_beacon(&ctx, &mut leader_sess, cov_b, false).await.expect("blops conduit jump");
    assert_eq!(blops_jumped, 3, "Only Blops + bomber + cloaked cruiser jump");
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&pass_sids[0]].location_id, dest_sys as i64);
        assert_eq!(s.items[&pass_sids[1]].location_id, dest_sys as i64);
        for skipped in &pass_sids[2..] {
            assert_eq!(s.items[skipped].location_id, origin_sys as i64);
        }
    });
    // post_jump_cap writeback verified (started at 50_000, consumed 95% or jump cap fraction < 50_000)
    let dest_cap = dest_handle.get_combat_state(leader_s).await.unwrap().unwrap().capacitor;
    assert!(dest_cap < 50_000.0, "post_jump_cap must persist to destination CombatState");

    // Verify Carrier (547) cap of 5 passengers vs Lancer (5120) doubled cap of 10 with 6 eligible cruiser passengers!
    on_cyno_ship_destroyed(&ctx, cyno_s);
    let _ = dest_handle.remove_module_cycle(cyno_s, 1_000_930_095).await;
    let std_cyno_t = ctx.sde.types.get(&21096).unwrap().clone();
    let std_kv = handle_cyno_generator_activation(&ctx, &dest_handle, cyno_p, cyno_s, dest_sys, 1_000_930_096, &std_cyno_t).await.unwrap().expect("std cyno");
    let std_b = FleetModuleBeacon { beacon_id: std_kv.dict_get("beaconID").unwrap().as_i64().unwrap(), char_id: cyno_p, ship_id: cyno_s, beacon_type_id: 21096, solar_system_id: dest_sys, position: DVec3::ZERO, expires_at_unix: u64::MAX };

    ctx.advance_secs(400);
    let mut car_pids = Vec::new();
    let mut car_sids = Vec::new();
    for i in 0..6 {
        let pid = 98_220_100 + i;
        let sid = 1_000_931_000 + i;
        seed_pilot_and_ship(&ctx, pid, 98_001, origin_sys, sid, 999_815, Some(fleet_id));
        car_pids.push(pid);
        car_sids.push(sid);
    }
    ctx.db.with_mut_snapshot(|snap| {
        let f = snap.fleets.get_mut(&fleet_id).unwrap();
        for &pid in &car_pids {
            f.members.push(pid);
            f.member_details.insert(pid, FleetMemberRecord {
                character_id: pid,
                role: FleetRole::SquadMember,
                wing_id: None,
                squad_id: None,
                takes_fleet_warp: false,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: String::new(),
                join_time_unix: 0,
            });
        }
        snap.characters.get_mut(&leader_p).unwrap().solar_system_id = origin_sys;
        snap.characters.get_mut(&leader_p).unwrap().jump_cooldown_until_unix = 0;
        snap.items.get_mut(&leader_s).unwrap().location_id = origin_sys as i64;
        snap.items.get_mut(&leader_s).unwrap().type_id = 999_812; // Carrier (547) -> max_passengers = 5
    });
    ctx.invalidate_ship_fitting(leader_s);
    spawn_ship_ball(&ctx, origin_sys, leader_s, 999_812, leader_p, DVec3::ZERO, 50_000.0).await;
    for (i, (&pid, &sid)) in car_pids.iter().zip(car_sids.iter()).enumerate() {
        spawn_ship_ball(&ctx, origin_sys, sid, 999_815, pid, DVec3::new(500.0 + i as f64 * 10.0, 0.0, 0.0), 5_000.0).await;
    }
    leader_sess.solar_system_id = Some(origin_sys);
    let (_, carrier_jumped) = group_jump_to_resolved_beacon(&ctx, &mut leader_sess, std_b.clone(), false).await.expect("carrier conduit");
    assert_eq!(carrier_jumped, 6, "Carrier at cjpg=0 caps passengers at 5 (total 6)");

    // Return all 6 + leader back and test Lancer (5120 -> 2x = 10 cap), jumping all 6 passengers!
    ctx.advance_secs(400);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&leader_p).unwrap().solar_system_id = origin_sys;
        snap.characters.get_mut(&leader_p).unwrap().jump_cooldown_until_unix = 0;
        snap.items.get_mut(&leader_s).unwrap().location_id = origin_sys as i64;
        snap.items.get_mut(&leader_s).unwrap().type_id = 999_811; // Lancer (5120) -> 2x cap = 10
        for (&pid, &sid) in car_pids.iter().zip(car_sids.iter()) {
            snap.characters.get_mut(&pid).unwrap().solar_system_id = origin_sys;
            snap.characters.get_mut(&pid).unwrap().jump_cooldown_until_unix = 0;
            snap.items.get_mut(&sid).unwrap().location_id = origin_sys as i64;
        }
    });
    ctx.invalidate_ship_fitting(leader_s);
    spawn_ship_ball(&ctx, origin_sys, leader_s, 999_811, leader_p, DVec3::ZERO, 50_000.0).await;
    for (i, (&pid, &sid)) in car_pids.iter().zip(car_sids.iter()).enumerate() {
        spawn_ship_ball(&ctx, origin_sys, sid, 999_815, pid, DVec3::new(500.0 + i as f64 * 10.0, 0.0, 0.0), 5_000.0).await;
    }
    leader_sess.solar_system_id = Some(origin_sys);
    let (_, lancer_jumped) = group_jump_to_resolved_beacon(&ctx, &mut leader_sess, std_b, false).await.expect("lancer conduit");
    assert_eq!(lancer_jumped, 7, "Lancer doubles max passengers to 10 so all 6 jump (total 7)");
}
