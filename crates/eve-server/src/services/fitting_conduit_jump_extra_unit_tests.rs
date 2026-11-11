//! Deep branch-coverage and mutation-killing unit tests for `fitting_conduit_jump`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::inventory_fitting::fitting_cyno_portal::handle_cyno_portal;
use eve_db::{
    AccountRecord, InventoryFlag, InventoryItemRecord, VirtualServerClock, WorldRepository,
};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{InvType, SdeRegistry, SolarSystem, TypeListInfo};
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "fitting_group_conduit_jump_unit_tests.rs"]
mod fitting_group_conduit_jump_unit_tests;

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

pub(super) fn custom_inv_type(
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

pub(super) fn custom_sys(
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

pub(super) fn build_ctx() -> ServerContext {
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

pub(super) async fn call_beyonce(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle_cyno_portal(ctx, session, "beyonce", method, args).await.expect("handled")
}

pub(super) fn seed_pilot_and_ship(
    ctx: &ServerContext,
    char_id: i64,
    corp_id: i64,
    sys_id: u32,
    ship_id: i64,
    ship_tid: u32,
    fleet_id: Option<i64>,
) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap
            .characters
            .values()
            .min_by_key(|c| c.character_id)
            .cloned()
            .expect("seed char");
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
        ch.jump_cooldown_until_unix = 0;
        ch.jump_fatigue_until_unix = 0;
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

pub(super) async fn spawn_ship_ball(
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_rollback_arithmetic_and_conduit_distance_tether_boundaries() {
    use super::{bridge_to_resolved_beacon, group_jump_to_resolved_beacon};
    use crate::services::inventory_fitting::fitting_cyno_portal::{
        ctx_key, cyno_state, handle_cyno_generator_activation, FleetModuleBeacon,
    };
    use eve_db::{FleetMemberRecord, FleetRecord, FleetRole};

    let mut ctx = build_ctx();
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        let p25 = custom_inv_type(
            sde,
            999_814,
            657,
            7,
            100.0,
            "Jump Portal 25s",
            &[
                (73, 60_000.0),
                (713, 16274.0),
                (714, 500.0),
                (1001, 0.000001),
                (1002, 25_000.0),
                (1298, 30.0),
                (3125, 1.0),
                (3318, 861.0),
            ],
        );
        sde.types.insert(999_814, p25);
        let p_huge = custom_inv_type(
            sde,
            999_815,
            657,
            7,
            100.0,
            "Jump Portal Huge Dur",
            &[
                (73, 60_000.0),
                (713, 16274.0),
                (714, 500.0),
                (1001, 0.000001),
                (1002, 1.0e21),
                (1298, 30.0),
                (3125, 1.0),
                (3318, 861.0),
            ],
        );
        sde.types.insert(999_815, p_huge);
    }

    let (origin_sys, dest_sys) = (30_002_505_u32, 30_002_506_u32);
    let fleet_id = 740_001_i64;
    let titan_pilot = 98_240_001_i64;
    let cyno_pilot = 98_240_002_i64;
    let p_ok = 98_240_003_i64;
    let p_exact = 98_240_004_i64;
    let p_far = 98_240_005_i64;
    let p_tethered = 98_240_006_i64;
    let rorq_pilot = 98_240_007_i64;

    let titan_ship = 1_000_940_001_i64;
    let cyno_ship = 1_000_940_002_i64;
    let s_ok = 1_000_940_003_i64;
    let s_exact = 1_000_940_004_i64;
    let s_far = 1_000_940_005_i64;
    let s_tethered = 1_000_940_006_i64;
    let rorq_ship = 1_000_940_007_i64;
    let portal_mod = 1_000_940_010_i64;
    let cyno_mod = 1_000_940_011_i64;

    seed_pilot_and_ship(&ctx, titan_pilot, 98_001, origin_sys, titan_ship, 999_801, Some(fleet_id));
    seed_pilot_and_ship(&ctx, cyno_pilot, 98_001, dest_sys, cyno_ship, 587, Some(fleet_id));
    seed_pilot_and_ship(&ctx, rorq_pilot, 98_001, origin_sys, rorq_ship, 999_802, Some(fleet_id));
    for (pid, sid) in [
        (p_ok, s_ok),
        (p_exact, s_exact),
        (p_far, s_far),
        (p_tethered, s_tethered),
    ] {
        seed_pilot_and_ship(&ctx, pid, 98_001, origin_sys, sid, 999_805, Some(fleet_id));
    }

    ctx.db.with_mut_snapshot(|snap| {
        let mut f = FleetRecord {
            fleet_id,
            boss_id: rorq_pilot,
            members: vec![titan_pilot, cyno_pilot, rorq_pilot, p_ok, p_exact, p_far, p_tethered],
            ..Default::default()
        };
        for pid in [p_ok, p_exact, p_far, p_tethered] {
            f.member_details.insert(
                pid,
                FleetMemberRecord {
                    character_id: pid,
                    role: FleetRole::SquadMember,
                    wing_id: None,
                    squad_id: None,
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 0,
                    role_name: String::new(),
                    join_time_unix: 10,
                },
            );
        }
        snap.fleets.insert(fleet_id, f);
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
        snap.module_online_states.insert(portal_mod, true);
        snap.items.insert(
            1_000_940_020,
            InventoryItemRecord {
                item_id: 1_000_940_020,
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
    spawn_ship_ball(&ctx, dest_sys, cyno_ship, 587, cyno_pilot, DVec3::new(1_000.0, 0.0, 0.0), 5_000.0).await;
    spawn_ship_ball(&ctx, origin_sys, rorq_ship, 999_802, rorq_pilot, DVec3::ZERO, 50_000.0).await;
    spawn_ship_ball(&ctx, origin_sys, s_ok, 999_805, p_ok, DVec3::new(1_000.0, 0.0, 0.0), 5_000.0).await;
    spawn_ship_ball(&ctx, origin_sys, s_exact, 999_805, p_exact, DVec3::new(10_000.0, 0.0, 0.0), 5_000.0).await;
    spawn_ship_ball(&ctx, origin_sys, s_far, 999_805, p_far, DVec3::new(10_001.0, 0.0, 0.0), 5_000.0).await;
    spawn_ship_ball(&ctx, origin_sys, s_tethered, 999_805, p_tethered, DVec3::new(500.0, 0.0, 0.0), 5_000.0).await;

    let orig_handle = ctx.systems.get_or_spawn(origin_sys);
    let mut tcs = orig_handle.get_combat_state(s_tethered).await.unwrap().unwrap();
    tcs.is_tethered = true;
    let _ = orig_handle.set_combat_state(s_tethered, tcs).await;

    let dest_handle = ctx.systems.get_or_spawn(dest_sys);
    let std_cyno_t = ctx.sde.types.get(&21096).unwrap().clone();
    let cyno_kv = handle_cyno_generator_activation(&ctx, &dest_handle, cyno_pilot, cyno_ship, dest_sys, cyno_mod, &std_cyno_t)
        .await
        .unwrap()
        .expect("cyno");
    let beacon_id = cyno_kv.dict_get("beaconID").unwrap().as_i64().unwrap();
    let beacon = FleetModuleBeacon {
        beacon_id,
        char_id: cyno_pilot,
        ship_id: cyno_ship,
        solar_system_id: dest_sys,
        beacon_type_id: 21096,
        position: DVec3::new(1_000.0, 0.0, 0.0),
        expires_at_unix: 2_000_000_000,
    };

    let mut titan_sess = SessionState {
        user_id: Some(titan_pilot as u64),
        character_id: Some(titan_pilot),
        corporation_id: Some(98_001),
        solar_system_id: Some(origin_sys),
        ship_id: Some(titan_ship),
        ..Default::default()
    };

    // 1. bridge_to_resolved_beacon ozone-short rollback checks:
    //    - clears pre-reserved bridge entry (`portal_ball_id == -ship_id`, kills `-` removal & `!=` on cleanup)
    //    - now_reserve=10 with 60s (`10 - 60` panics) and 25s (`10 + 25 - 60` panics) + now=1.8e9 with 1e18s (`now*dur`, `dur*60` overflow)
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(10)));
    assert!(bridge_to_resolved_beacon(&ctx, &titan_sess, beacon.clone(), None).await.is_err());
    assert!(!cyno_state().read().unwrap().bridges.contains_key(&(ctx_key(&ctx), titan_ship)));

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&portal_mod).unwrap().type_id = 999_814;
    });
    ctx.invalidate_ship_fitting(titan_ship);
    assert!(bridge_to_resolved_beacon(&ctx, &titan_sess, beacon.clone(), None).await.is_err());
    assert!(!cyno_state().read().unwrap().bridges.contains_key(&(ctx_key(&ctx), titan_ship)));

    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&portal_mod).unwrap().type_id = 999_815;
    });
    ctx.invalidate_ship_fitting(titan_ship);
    assert!(bridge_to_resolved_beacon(&ctx, &titan_sess, beacon.clone(), None).await.is_err());
    assert!(!cyno_state().read().unwrap().bridges.contains_key(&(ctx_key(&ctx), titan_ship)));

    // Restore 999_804 + seed Liquid Ozone & Nitrogen Isotopes -> happy bridge_to_fleet_beacon + jump_through_fleet non-default returns!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&portal_mod).unwrap().type_id = 999_804;
        snap.items.insert(
            1_000_940_021,
            InventoryItemRecord {
                item_id: 1_000_940_021,
                type_id: 16274,
                owner_id: titan_pilot,
                location_id: titan_ship,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 5_000,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            1_000_940_022,
            InventoryItemRecord {
                item_id: 1_000_940_022,
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
    ctx.invalidate_ship_fitting(titan_ship);
    let br_rep = call_beyonce(
        &ctx,
        &mut titan_sess,
        "CmdBridgeToFleetModuleBeacon",
        &[PyRep::Integer(cyno_pilot), PyRep::Integer(dest_sys as i64), PyRep::Integer(beacon_id)],
    )
    .await
    .expect("happy fleet beacon bridge");
    assert_eq!(br_rep.dict_get("bridgeActive"), Some(&PyRep::Bool(true)));
    assert_eq!(br_rep.dict_get("solarSystemID"), Some(&PyRep::Integer(dest_sys as i64)));

    // Untethered passenger jumps through open portal via CmdJumpThroughFleet -> non-default outcome
    let pass_ship = 1_000_940_099_i64;
    let pass_pilot = 98_240_099_i64;
    seed_pilot_and_ship(&ctx, pass_pilot, 98_001, origin_sys, pass_ship, 999_803, Some(fleet_id));
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.get_mut(&fleet_id).unwrap().members.push(pass_pilot);
    });
    spawn_ship_ball(&ctx, origin_sys, pass_ship, 999_803, pass_pilot, DVec3::new(400.0, 0.0, 0.0), 5_000.0).await;
    let mut pass_sess = SessionState {
        user_id: Some(pass_pilot as u64),
        character_id: Some(pass_pilot),
        corporation_id: Some(98_001),
        solar_system_id: Some(origin_sys),
        ship_id: Some(pass_ship),
        ..Default::default()
    };
    let jt_rep = call_beyonce(
        &ctx,
        &mut pass_sess,
        "CmdJumpThroughFleet",
        &[PyRep::Integer(titan_pilot), PyRep::Integer(titan_ship)],
    )
    .await
    .expect("passenger jump through fleet");
    assert_eq!(jt_rep.dict_get("solarSystemID"), Some(&PyRep::Integer(dest_sys as i64)));
    assert_eq!(jt_rep.dict_get("fuelTypeID"), Some(&PyRep::Integer(17888)));

    // 2. group_jump_to_resolved_beacon exact count == 3 and passenger filter boundaries:
    //    p_ok (1_000m, untethered) + p_exact (10_000m, untethered) jump; p_far (10_001m) & p_tethered stay!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            1_000_940_023,
            InventoryItemRecord {
                item_id: 1_000_940_023,
                type_id: 17888,
                owner_id: rorq_pilot,
                location_id: rorq_ship,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 5_000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let mut rorq_sess = SessionState {
        user_id: Some(rorq_pilot as u64),
        character_id: Some(rorq_pilot),
        corporation_id: Some(98_001),
        solar_system_id: Some(origin_sys),
        ship_id: Some(rorq_ship),
        ..Default::default()
    };
    let (outcome, total_jumped) = group_jump_to_resolved_beacon(&ctx, &mut rorq_sess, beacon, false)
        .await
        .expect("conduit jump");
    assert!((outcome.dist_ly - 2.0).abs() < 1e-6);
    assert_eq!(rorq_sess.solar_system_id, Some(dest_sys));
    assert_eq!(total_jumped, 3, "only leader + p_ok (1000m) + p_exact (10000m) should jump");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&p_ok].solar_system_id, dest_sys);
        assert_eq!(snap.characters[&p_exact].solar_system_id, dest_sys);
        assert_eq!(snap.characters[&p_far].solar_system_id, origin_sys);
        assert_eq!(snap.characters[&p_tethered].solar_system_id, origin_sys);
    });

    // Also exercise CmdGroupJumpToFleetModuleBeacon happy return (kills Default::default() mutant)
    let rorq2_pilot = 98_240_008_i64;
    let rorq2_ship = 1_000_940_008_i64;
    seed_pilot_and_ship(&ctx, rorq2_pilot, 98_001, origin_sys, rorq2_ship, 999_802, Some(fleet_id));
    spawn_ship_ball(&ctx, origin_sys, rorq2_ship, 999_802, rorq2_pilot, DVec3::ZERO, 50_000.0).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.get_mut(&fleet_id).unwrap().members.push(rorq2_pilot);
        snap.items.insert(
            1_000_940_024,
            InventoryItemRecord {
                item_id: 1_000_940_024,
                type_id: 17888,
                owner_id: rorq2_pilot,
                location_id: rorq2_ship,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 5_000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let mut rorq2_sess = SessionState {
        user_id: Some(rorq2_pilot as u64),
        character_id: Some(rorq2_pilot),
        corporation_id: Some(98_001),
        solar_system_id: Some(origin_sys),
        ship_id: Some(rorq2_ship),
        ..Default::default()
    };
    let gj_rep = call_beyonce(
        &ctx,
        &mut rorq2_sess,
        "CmdGroupJumpToFleetModuleBeacon",
        &[PyRep::Integer(cyno_pilot), PyRep::Integer(dest_sys as i64), PyRep::Integer(beacon_id)],
    )
    .await
    .expect("CmdGroupJumpToFleetModuleBeacon");
    assert_eq!(gj_rep.dict_get("solarSystemID"), Some(&PyRep::Integer(dest_sys as i64)));
    assert_eq!(gj_rep.dict_get("fuelTypeID"), Some(&PyRep::Integer(17888)));
}
