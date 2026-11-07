#![forbid(unsafe_code)]

use super::*;
use eve_db::{
    FleetRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord, VirtualServerClock,
    WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry, SolarSystem};
use glam::DVec3;
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

fn custom_system(sde: &SdeRegistry, solar_system_id: u32, security_status: f64, galactic_pos_ly: [f64; 3]) -> SolarSystem {
    let mut s = sde.solar_systems.get(&30_002_505).expect("seed system").clone();
    s.solar_system_id = solar_system_id;
    s.security_status = security_status;
    s.galactic_pos_ly = galactic_pos_ly;
    s
}

fn test_ctx_with_custom<F: FnOnce(&mut SdeRegistry)>(mutate_sde: F) -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    mutate_sde(&mut sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

fn ensure_char(ctx: &ServerContext, char_id: i64, corp_id: i64, sys_id: u32, ship_id: i64, fleet_id: Option<i64>) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap
            .characters
            .get(&90_000_001)
            .cloned()
            .or_else(|| snap.characters.values().min_by_key(|c| c.character_id).cloned())
            .expect("demo seed character");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.security_status = 5.0;
        ch.fleet_id = fleet_id;
        ch.clone_grade = 1;
        ch.jump_fatigue_minutes = 0.0;
        ch.jump_cooldown_until_unix = 0;
        ch.jump_fatigue_until_unix = 0;
        ch.last_clone_jump_unix = 0;
        ch.autopilot_waypoints.clear();
        snap.characters.entry(char_id).or_insert(ch);
        snap.accounts.insert(
            format!("user_{char_id}"),
            eve_db::AccountRecord {
                user_id: char_id as u64,
                username: format!("user_{char_id}"),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    let _ = ctx
        .db
        .acquire_online_lock(char_id as u64, char_id as u64, Some(char_id), true, ctx.now_unix_secs());
}

fn grant_skill(ctx: &ServerContext, char_id: i64, skill_tid: u32, level: u8) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            skill_tid,
            TrainedSkillRecord {
                type_id: skill_tid,
                level,
                skill_points: 256_000.0,
            },
        );
    });
}

fn insert_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton,
                ..Default::default()
            },
        );
    });
}

async fn spawn_ship(ctx: &ServerContext, sys_id: u32, ship_id: i64, type_id: u32, owner_id: i64, pos: DVec3, cap: f64) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ship_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 2_000.0,
        inertia: 1.0,
        mass: 5_000_000.0,
        radius: 500.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut cs = CombatState::default();
    cs.capacitor = cap;
    cs.max_capacitor = cap;
    let meta = SlimItemMeta {
        group_id: 27,
        category_id: 6,
        owner_id,
        char_id: Some(owner_id),
        corp_id: 98_000_001,
        alliance_id: 0,
        security_status: 5.0,
        is_cloaked: false,
        ..Default::default()
    };
    sys.spawn_ball_with_meta(ball, cs, meta)
        .await
        .expect("spawn ship");
}

#[tokio::test]
async fn hull_allowed_covert_capability_and_beacon_sweep() {
    let ctx = test_ctx_with_custom(|_| {});
    let rifter = custom_inv_type(&ctx.sde, 587, 25, 6, 1_000_000.0, "Rifter", &[]);
    let covops = custom_inv_type(&ctx.sde, 12032, 830, 6, 1_000_000.0, "Buzzard", &[]);

    assert!(!is_covert_ops_cloak_capable(&ctx, &rifter));
    assert!(is_covert_ops_cloak_capable(&ctx, &covops));

    let u = csprng_unit_f64(&ctx);
    assert!((0.0..=1.0).contains(&u));
    let arrival = random_arrival_position(&ctx, DVec3::new(1_000_000.0, 0.0, 0.0));
    assert!((arrival.distance(DVec3::new(1_000_000.0, 0.0, 0.0)) - 5_000.0).abs() < 1e-3);

    // Seed expired and live beacons in cyno_state and sweep expired ones
    let ctx_k = ctx.instance_id as usize;
    {
        let mut st = cyno_state().write().unwrap();
        st.beacons.insert(
            (ctx_k, 8_880_001),
            FleetModuleBeacon {
                beacon_id: 8_880_101,
                char_id: 98_000_001,
                ship_id: 8_880_001,
                beacon_type_id: 57425,
                solar_system_id: 30_002_505,
                position: DVec3::ZERO,
                expires_at_unix: 1_799_999_999,
            },
        );
        st.beacons.insert(
            (ctx_k, 8_880_002),
            FleetModuleBeacon {
                beacon_id: 8_880_102,
                char_id: 98_000_002,
                ship_id: 8_880_002,
                beacon_type_id: 57425,
                solar_system_id: 30_002_505,
                position: DVec3::ZERO,
                expires_at_unix: 1_800_000_500,
            },
        );
    }
    sweep_expired_cyno_beacons(&ctx);
    let st = cyno_state().read().unwrap();
    assert!(!st.beacons.contains_key(&(ctx_k, 8_880_001)));
    assert!(st.beacons.contains_key(&(ctx_k, 8_880_002)));
}

#[tokio::test]
async fn jump_portal_bridge_jump_through_conduit_and_jammer_state_rpcs() {
    let origin_sys = 30_002_505u32;
    let dest_sys = 30_002_506u32;
    let ctx = test_ctx_with_custom(|sde| {
        sde.solar_systems.insert(origin_sys, custom_system(sde, origin_sys, -0.5, [0.0, 0.0, 0.0]));
        sde.solar_systems.insert(dest_sys, custom_system(sde, dest_sys, -0.5, [2.0, 0.0, 0.0]));
        sde.type_lists.insert(
            99_318,
            eve_sde::TypeListInfo {
                type_list_id: 99_318,
                name: "Covert Beacons".into(),
                included_type_ids: vec![59630, 59631],
                ..Default::default()
            },
        );
        sde.types.insert(
            59631,
            custom_inv_type(sde, 59631, 650, 1, 1.0, "Covert Cynosural Field", &[(3132, 1.0)]),
        );
        sde.types.insert(
            999_701,
            custom_inv_type(
                sde,
                999_701,
                898,
                6,
                15_000_000.0,
                "Test Redeemer",
                &[
                    (861, 1.0),
                    (866, 17888.0),
                    (867, 8.0),
                    (868, 500.0),
                    (898, 0.15),
                    (3125, 1.0),
                    (3131, 100.0),
                    (3317, 99_318.0),
                    (3318, 3319.0),
                ],
            ),
        );
        sde.types.insert(
            999_702,
            custom_inv_type(
                sde,
                999_702,
                657,
                7,
                100.0,
                "Covert Jump Portal Generator I",
                &[
                    (6, 500.0),
                    (73, 60_000.0),
                    (713, 17888.0),
                    (714, 100.0),
                    (1001, 0.000001),
                    (1002, 60_000.0),
                    (1252, 1.0),
                    (3125, 1.0),
                    (3318, 3319.0),
                ],
            ),
        );
        sde.types.insert(
            999_703,
            custom_inv_type(sde, 999_703, 830, 6, 1_200_000.0, "Test Purifier", &[(3319, 1.0)]),
        );
    });

    let blops_pilot = 98_100_001;
    let cyno_pilot = 98_100_002;
    let bomber_pilot = 98_100_003;
    let fleet_id = 700_050;
    let blops_ship_id = 1_000_800_001;
    let portal_mod_id = 1_000_800_002;
    let blops_fuel_id = 1_000_800_003;
    let bomber_ship_id = 1_000_800_004;
    let beacon_id = 1_000_800_010;

    ensure_char(&ctx, blops_pilot, 98_000_030, origin_sys, blops_ship_id, Some(fleet_id));
    ensure_char(&ctx, cyno_pilot, 98_000_030, dest_sys, 1_000_800_009, Some(fleet_id));
    ensure_char(&ctx, bomber_pilot, 98_000_030, origin_sys, bomber_ship_id, Some(fleet_id));
    grant_skill(&ctx, blops_pilot, 21611, 5);
    grant_skill(&ctx, blops_pilot, 21610, 5);
    grant_skill(&ctx, blops_pilot, 3456, 5);
    grant_skill(&ctx, blops_pilot, 24568, 5); // Jump Portal Generation V

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: blops_pilot,
                members: vec![blops_pilot, cyno_pilot, bomber_pilot],
                ..Default::default()
            },
        );
    });
    insert_item(&ctx, blops_ship_id, 999_701, blops_pilot, origin_sys as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, portal_mod_id, 999_702, blops_pilot, blops_ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, blops_fuel_id, 17888, blops_pilot, blops_ship_id, InventoryFlag::Cargo, 10_000, false);
    insert_item(&ctx, bomber_ship_id, 999_703, bomber_pilot, origin_sys as i64, InventoryFlag::None, 1, true);

    spawn_ship(&ctx, origin_sys, blops_ship_id, 999_701, blops_pilot, DVec3::new(1_000.0, 0.0, 0.0), 50_000.0).await;
    spawn_ship(&ctx, origin_sys, bomber_ship_id, 999_703, bomber_pilot, DVec3::new(2_500.0, 0.0, 0.0), 5_000.0).await;

    let ctx_k = ctx.instance_id as usize;
    cyno_state().write().unwrap().beacons.insert(
        (ctx_k, 1_000_800_009),
        FleetModuleBeacon {
            beacon_id,
            char_id: cyno_pilot,
            ship_id: 1_000_800_009,
            beacon_type_id: 59631,
            solar_system_id: dest_sys,
            position: DVec3::new(2_000_000.0, 0.0, 0.0),
            expires_at_unix: ctx.now_unix_secs() + 600,
        },
    );

    let mut blops_sess = SessionState {
        user_id: Some(blops_pilot as u64),
        character_id: Some(blops_pilot),
        corporation_id: Some(98_000_030),
        solar_system_id: Some(origin_sys),
        ship_id: Some(blops_ship_id),
        ..Default::default()
    };

    // 1. GetCynoJammerState on clean lowsec returns None
    let jam_res = handle_cyno_portal(&ctx, &mut blops_sess, "beyonce", "GetCynoJammerState", &[])
        .await
        .unwrap()
        .expect("jammer query");
    assert_eq!(jam_res, PyRep::None);

    // 2. Open Jump Bridge (`CmdBridgeToFleetModuleBeacon`) -> spawns portal ball & opens active bridge
    let bridge_res = handle_cyno_portal(
        &ctx,
        &mut blops_sess,
        "beyonce",
        "CmdBridgeToFleetModuleBeacon",
        &[PyRep::Integer(cyno_pilot), PyRep::Integer(i64::from(dest_sys)), PyRep::Integer(beacon_id)],
    )
    .await
    .unwrap()
    .expect("open jump bridge");
    let portal_ball_id = bridge_res.dict_get("portalBallID").and_then(|v| v.as_i64()).unwrap();
    assert!(portal_ball_id > 0);

    // 3. Bomber pilot jumps through portal (`CmdJumpThroughFleet`)
    let mut bomber_sess = SessionState {
        user_id: Some(bomber_pilot as u64),
        character_id: Some(bomber_pilot),
        corporation_id: Some(98_000_030),
        solar_system_id: Some(origin_sys),
        ship_id: Some(bomber_ship_id),
        ..Default::default()
    };
    let jump_through = handle_cyno_portal(
        &ctx,
        &mut bomber_sess,
        "beyonce",
        "CmdJumpThroughFleet",
        &[PyRep::Integer(blops_pilot), PyRep::Integer(blops_ship_id)],
    )
    .await
    .unwrap()
    .expect("bomber jump through portal");
    assert_eq!(bomber_sess.solar_system_id, Some(dest_sys));
    assert!(jump_through.dict_get("fuelConsumed").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    // 4. Conduit Jump (`CmdGroupJumpToFleetModuleBeacon`): moves remaining nearby passenger + Black Ops together!
    let bomber2_pilot = 98_100_004;
    let bomber2_ship = 1_000_800_005;
    ensure_char(&ctx, bomber2_pilot, 98_000_030, origin_sys, bomber2_ship, Some(fleet_id));
    insert_item(&ctx, bomber2_ship, 999_703, bomber2_pilot, origin_sys as i64, InventoryFlag::None, 1, true);
    spawn_ship(&ctx, origin_sys, bomber2_ship, 999_703, bomber2_pilot, DVec3::new(2_200.0, 0.0, 0.0), 5_000.0).await;
    ctx.db.with_mut_snapshot(|snap| {
        let f = snap.fleets.get_mut(&fleet_id).unwrap();
        f.members.push(bomber2_pilot);
        f.member_details.insert(
            bomber2_pilot,
            eve_db::FleetMemberRecord {
                character_id: bomber2_pilot,
                role: eve_db::FleetRole::SquadMember,
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
    });
    ctx.session_change_timers.lock().unwrap().remove(&blops_pilot);
    let _ = ctx.systems.get_or_spawn(origin_sys).remove_module_cycle(blops_ship_id, portal_mod_id).await;
    cyno_state().write().unwrap().bridges.remove(&(ctx_k, blops_ship_id));

    let conduit_res = handle_cyno_portal(
        &ctx,
        &mut blops_sess,
        "beyonce",
        "CmdGroupJumpToFleetModuleBeacon",
        &[PyRep::Integer(cyno_pilot), PyRep::Integer(i64::from(dest_sys)), PyRep::Integer(beacon_id)],
    )
    .await
    .unwrap()
    .expect("conduit jump");
    assert_eq!(blops_sess.solar_system_id, Some(dest_sys));
    assert!(conduit_res.dict_get("fuelConsumed").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&bomber2_ship).unwrap().location_id, i64::from(dest_sys));
    });
}
