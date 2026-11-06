//! Deep branch-coverage and mutation-killing unit tests for `fitting_conduit_jump`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::inventory_fitting::fitting_cyno_portal::{
    handle_cyno_generator_activation, handle_cyno_portal, is_ship_cyno_active,
    on_cyno_ship_destroyed, sweep_expired_cyno_beacons,
};
use eve_db::{
    AccountRecord, FleetMemberRecord, FleetRecord, FleetRole, InventoryFlag, InventoryItemRecord,
    TrainedSkillRecord, VirtualServerClock, WorldRepository,
};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{InvType, SdeRegistry, SolarSystem, TypeListInfo};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn custom_inv_type(
    sde: &SdeRegistry,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    mass: f64,
    name: &str,
    attrs: &[(u16, f64)],
) -> InvType {
    let mut t = sde.types.get(&587).expect("seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.mass = mass;
    t.name = name.into();
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t
}

fn custom_sys(
    sde: &SdeRegistry,
    sys_id: u32,
    region_id: u32,
    sec: f64,
    pos_ly: [f64; 3],
) -> SolarSystem {
    let mut s = sde.solar_systems.get(&30_002_505).expect("seed sys").clone();
    s.solar_system_id = sys_id;
    s.region_id = region_id;
    s.security_status = sec;
    s.galactic_pos_ly = pos_ly;
    s
}

fn build_ctx() -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    sde.solar_systems.insert(30_002_505, custom_sys(&sde, 30_002_505, 10_000_030, -0.5, [0.0, 0.0, 0.0]));
    sde.solar_systems.insert(30_002_506, custom_sys(&sde, 30_002_506, 10_000_030, -0.5, [2.0, 0.0, 0.0]));
    sde.solar_systems.insert(30_002_507, custom_sys(&sde, 30_002_507, 10_000_030, -0.5, [50.0, 0.0, 0.0]));
    sde.solar_systems.insert(30_000_142, custom_sys(&sde, 30_000_142, 10_000_002, 0.9, [1.0, 0.0, 0.0]));
    sde.solar_systems.insert(30_002_508, custom_sys(&sde, 30_002_508, 10_000_070, -1.0, [1.0, 0.0, 0.0]));

    let mut std_beacons: Vec<u32> = sde
        .type_lists
        .values()
        .flat_map(|l| l.included_type_ids.iter().copied())
        .chain([21096, 52694, 57425])
        .filter(|&tid| {
            !matches!(tid, 28646 | 28650 | 56488 | 59630 | 59631)
                && sde
                    .types
                    .get(&tid)
                    .is_none_or(|t| t.default_attributes.get(&1252).copied().unwrap_or(0.0) <= 0.0)
        })
        .collect();
    std_beacons.sort_unstable();
    std_beacons.dedup();
    sde.type_lists.insert(
        99_401,
        TypeListInfo {
            type_list_id: 99_401,
            name: "Standard Test Beacons".into(),
            included_type_ids: std_beacons,
            ..Default::default()
        },
    );
    sde.types.insert(57425, custom_inv_type(&sde, 57425, 650, 2, 1.0, "Cynosural Field", &[]));
    sde.types.insert(59631, custom_inv_type(&sde, 59631, 650, 2, 1.0, "Covert Cynosural Field", &[(1252, 1.0)]));
    sde.types.insert(
        28646,
        custom_inv_type(&sde, 28646, 658, 7, 10.0, "Covert Cynosural Field Generator I", &[(73, 60_000.0), (713, 16273.0), (714, 10.0), (1252, 1.0)]),
    );
    sde.types.insert(
        21096,
        custom_inv_type(&sde, 21096, 658, 7, 10.0, "Cynosural Field Generator I", &[(73, 60_000.0), (713, 16273.0), (714, 100.0)]),
    );
    sde.types.insert(
        999_801,
        custom_inv_type(
            &sde,
            999_801,
            30,
            6,
            100_000_000.0,
            "Test Avatar",
            &[(861, 1.0), (866, 17888.0), (867, 6.0), (868, 1000.0), (898, 0.20), (3125, 1.0), (3317, 99_401.0)],
        ),
    );
    sde.types.insert(
        999_802,
        custom_inv_type(
            &sde,
            999_802,
            883,
            6,
            50_000_000.0,
            "Test Rorqual",
            &[(861, 1.0), (866, 17888.0), (867, 6.0), (868, 800.0), (898, 0.25), (3125, 1.0), (3131, 150.0), (3317, 99_401.0)],
        ),
    );
    sde.types.insert(
        999_803,
        custom_inv_type(&sde, 999_803, 547, 6, 20_000_000.0, "NoConduitCarrier", &[(861, 1.0), (866, 17888.0), (867, 6.0), (3317, 99_401.0)]),
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
            &[(73, 60_000.0), (713, 16274.0), (714, 500.0), (1001, 0.000001), (1002, 60_000.0), (1298, 30.0), (3125, 1.0), (3318, 861.0)],
        ),
    );
    sde.types.insert(28583, custom_inv_type(&sde, 28583, 515, 7, 100.0, "Industrial Core I", &[]));
    sde.types.insert(999_805, custom_inv_type(&sde, 999_805, 463, 6, 2_000_000.0, "Test Procurer", &[(3324, 1.0)]));

    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

async fn call_beyonce(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle_cyno_portal(ctx, session, "beyonce", method, args).await.expect("handled")
}

fn seed_pilot_and_ship(
    ctx: &ServerContext,
    char_id: i64,
    corp_id: i64,
    sys_id: u32,
    ship_id: i64,
    ship_tid: u32,
    fleet_id: Option<i64>,
) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.clone_grade = 2;
        ch.name = format!("Pilot_{char_id}");
        ch.corporation_id = corp_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.fleet_id = fleet_id;
        snap.characters.insert(char_id, ch);
        snap.accounts.insert(
            format!("u_{char_id}"),
            AccountRecord {
                user_id: char_id as u64,
                username: format!("u_{char_id}"),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_tid,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let _ = ctx.db.acquire_online_lock(char_id as u64, char_id as u64, Some(char_id), true, ctx.now_unix_secs());
}

async fn spawn_ship_ball(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
    type_id: u32,
    owner_id: i64,
    pos: DVec3,
    cap: f64,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ship_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 1_000.0,
        inertia: 1.0,
        mass: 5_000_000.0,
        radius: 500.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut cs = CombatState::default();
    cs.capacitor = cap;
    cs.max_capacitor = cap.max(1000.0);
    let meta = SlimItemMeta {
        group_id: 30,
        category_id: 6,
        owner_id,
        char_id: Some(owner_id),
        corp_id: 98_001,
        ..Default::default()
    };
    let _ = sys.spawn_ball_with_meta(ball, cs, meta).await;
}

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
