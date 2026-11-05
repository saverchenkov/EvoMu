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

fn custom_inv_type(sde: &SdeRegistry, type_id: u32, group_id: u32, category_id: u32, radius: f64, name: &str, attrs: &[(u16, f64)]) -> InvType {
    let mut t = sde.types.get(&587).expect("seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.radius = radius;
    t.name = name.into();
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t
}

fn custom_system(sde: &SdeRegistry, solar_system_id: u32, region_id: u32, security_status: f64, galactic_pos_ly: [f64; 3]) -> SolarSystem {
    let mut s = sde.solar_systems.get(&30_002_505).expect("seed system").clone();
    s.solar_system_id = solar_system_id;
    s.region_id = region_id;
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
        let mut ch = snap.characters.values().next().cloned().expect("demo seed character");
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

async fn spawn_ship(ctx: &ServerContext, sys_id: u32, ship_id: i64, type_id: u32, owner_id: i64, pos: DVec3, speed: f64, cap: f64) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ship_id,
        type_id,
        position: pos,
        velocity: DVec3::new(speed, 0.0, 0.0),
        max_velocity: 2_000.0,
        inertia: 1.0,
        mass: 5_000_000.0,
        radius: 600.0,
        speed_fraction: 0.0,
        mode: if speed > 0.0 {
            MovementMode::GotoDirection { direction: DVec3::X }
        } else {
            MovementMode::Stop
        },
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

#[test]
fn beacon_types_cyno_jammers_and_jump_fuel_consumption_math() {
    let ctx = test_ctx_with_custom(|sde| {
        let s1 = custom_system(sde, 30_100_000, 10_000_001, -1.0, [0.0, 0.0, 0.0]);
        let s2 = custom_system(sde, 30_100_001, 10_000_070, -1.0, [0.0, 0.0, 0.0]);
        let t1 = custom_inv_type(sde, 999_301, 1299, 7, 10.0, "Jump Drive Economizer I", &[(1296, -15.0)]);
        sde.solar_systems.insert(30_100_000, s1);
        sde.solar_systems.insert(30_100_001, s2);
        sde.types.insert(999_301, t1);
    });

    assert!(is_covert_beacon_type(59630));
    assert!(!is_covert_beacon_type(57425));
    assert_eq!(effective_beacon_field_type(&ctx, 56488), 59631);
    assert_eq!(effective_beacon_field_type(&ctx, 56099), 57425);

    // Jammer checks: Zarzakh, Pochven, deployed item jammer (33700), vs clean system
    assert!(active_cyno_jammer(&ctx, 30_100_000).is_some());
    assert!(active_cyno_jammer(&ctx, 30_100_001).is_some());
    let clean_lowsec = 30_002_505;
    assert!(active_cyno_jammer(&ctx, clean_lowsec).is_none());
    ensure_char(&ctx, 95_000_001, 98_000_001, clean_lowsec, 1_000_500_000, None);
    insert_item(&ctx, 1_000_500_001, 33700, 95_000_001, clean_lowsec as i64, InventoryFlag::None, 1, true);
    assert!(active_cyno_jammer(&ctx, clean_lowsec).is_some());

    // Jump Fuel Conservation + Jump Freighter skill + Economizer stacking
    let char_id = 95_000_002;
    let jf_ship_id = 1_000_500_002;
    ensure_char(&ctx, char_id, 98_000_001, clean_lowsec, jf_ship_id, None);
    grant_skill(&ctx, char_id, 21610, 5); // -50%
    grant_skill(&ctx, char_id, 29029, 5); // -50% on group 902
    insert_item(&ctx, jf_ship_id, 28850, char_id, clean_lowsec as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, 1_000_500_003, 999_301, char_id, jf_ship_id, InventoryFlag::LoSlot0, 1, true);
    let jf_type = custom_inv_type(&ctx.sde, 28850, 902, 6, 800.0, "Rhea", &[(868, 1_000.0)]);
    let per_ly = effective_jump_drive_consumption_per_ly(&ctx, char_id, jf_ship_id, &jf_type);
    // 1000 * 0.5 (JFC V) * 0.5 (JF V) * 0.85 (Economizer) = 212.5
    assert!((per_ly - 212.5).abs() < 1e-6);
}

#[tokio::test]
async fn light_cyno_validation_guards_active_beacon_count_and_destroyed_fallback() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.solar_systems.insert(30_002_505, custom_system(sde, 30_002_505, 10_000_001, -0.5, [0.0, 0.0, 0.0]));
        let t1 = custom_inv_type(sde, 999_310, 27, 6, 500.0, "Test Cyno Battleship", &[]);
        let t2 = custom_inv_type(
            sde,
            21096,
            658,
            7,
            20.0,
            "Cynosural Field Generator I",
            &[(713, 16273.0), (714, 500.0), (73, 600_000.0), (6, 500.0)],
        );
        sde.types.insert(999_310, t1);
        sde.types.insert(21096, t2);
    });

    let char_id = 95_000_010;
    let fleet_mate_id = 95_000_011;
    let fleet_id = 700_020;
    let lowsec_sys = 30_002_505;
    let ship_id = 1_000_500_101;
    let cyno_mod_id = 1_000_500_102;
    let ozone_id = 1_000_500_103;

    ensure_char(&ctx, char_id, 98_000_010, lowsec_sys, ship_id, None);
    ensure_char(&ctx, fleet_mate_id, 98_000_010, lowsec_sys, 1_000_500_109, Some(fleet_id));
    insert_item(&ctx, ship_id, 999_310, char_id, lowsec_sys as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, cyno_mod_id, 21096, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    spawn_ship(&ctx, lowsec_sys, ship_id, 999_310, char_id, DVec3::new(50_000.0, 0.0, 0.0), 600.0, 5_000.0).await;

    let sys = ctx.systems.get_or_spawn(lowsec_sys);
    let cyno_type = ctx.sde.types.get(&21096).unwrap().clone();

    // 1. Zarzakh guard
    assert_eq!(
        light_cyno(&ctx, &sys, char_id, ship_id, 30_100_000, cyno_mod_id, &cyno_type)
            .await
            .unwrap_err()
            .code,
        "CynoDisallowedInZarzakh"
    );

    // 2. Highsec guard (Jita 30_000_142)
    assert_eq!(
        light_cyno(&ctx, &sys, char_id, ship_id, 30_000_142, cyno_mod_id, &cyno_type)
            .await
            .unwrap_err()
            .code,
        "CannotLightCynoInSystem"
    );

    // 3. Must be in fleet
    assert_eq!(
        light_cyno(&ctx, &sys, char_id, ship_id, lowsec_sys, cyno_mod_id, &cyno_type)
            .await
            .unwrap_err()
            .code,
        "MustBeInFleet"
    );

    // Join fleet, fail speed check (>= 500 m/s), slow ship down, fail NotEnoughFuel
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().fleet_id = Some(fleet_id);
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: char_id,
                members: vec![char_id, fleet_mate_id],
                ..Default::default()
            },
        );
    });
    assert_eq!(
        light_cyno(&ctx, &sys, char_id, ship_id, lowsec_sys, cyno_mod_id, &cyno_type)
            .await
            .unwrap_err()
            .code,
        "ShipMovingTooFast"
    );

    sys.try_stop_ball(ship_id);
    assert_eq!(
        light_cyno(&ctx, &sys, char_id, ship_id, lowsec_sys, cyno_mod_id, &cyno_type)
            .await
            .unwrap_err()
            .code,
        "NotEnoughFuel"
    );

    // 4. Load Liquid Ozone (16273) and light cyno!
    insert_item(&ctx, ozone_id, 16273, char_id, ship_id, InventoryFlag::Cargo, 1_000, false);
    let cyno_rep = handle_cyno_generator_activation(&ctx, &sys, char_id, ship_id, lowsec_sys, cyno_mod_id, &cyno_type)
        .await
        .expect("matched")
        .expect("cyno lit");
    let beacon_id = cyno_rep.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();
    assert!(is_ship_cyno_active(&ctx, ship_id));
    assert_eq!(active_cyno_beacon_counts(&ctx).get(&i64::from(lowsec_sys)), Some(&1));

    // Destroy cyno ship -> beacon moves to destroyed_beacons grace cache
    on_cyno_ship_destroyed(&ctx, ship_id);
    assert!(!is_ship_cyno_active(&ctx, ship_id));
    let (_, was_destroyed) = resolve_fleet_beacon_or_destroyed(&ctx, fleet_mate_id, char_id, beacon_id, lowsec_sys, false)
        .await
        .expect("destroyed beacon fallback");
    assert!(was_destroyed);
}

#[tokio::test]
async fn jump_to_fleet_beacon_relocates_capital_ship_with_fuel_cap_and_fatigue() {
    let origin_sys = 30_002_505u32;
    let dest_sys = 30_002_506u32;
    let ctx = test_ctx_with_custom(|sde| {
        sde.solar_systems.insert(origin_sys, custom_system(sde, origin_sys, 10_000_001, -0.5, [0.0, 0.0, 0.0]));
        sde.solar_systems.insert(dest_sys, custom_system(sde, dest_sys, 10_000_001, -0.5, [2.0, 0.0, 0.0]));
        sde.type_lists.insert(
            99_317,
            eve_sde::TypeListInfo {
                type_list_id: 99_317,
                name: "Standard Cyno Beacons".into(),
                included_type_ids: vec![21096, 57425],
                ..Default::default()
            },
        );
        sde.types.insert(
            999_320,
            custom_inv_type(
                sde,
                999_320,
                547,
                6,
                800.0,
                "Test Jump Carrier",
                &[(861, 1.0), (866, 17888.0), (867, 6.0), (868, 1_000.0), (898, 0.25), (3317, 99_317.0)],
            ),
        );
    });

    let jumper_id = 95_000_020;
    let cyno_pilot_id = 95_000_021;
    let fleet_id = 700_030;
    let carrier_id = 1_000_500_201;
    let cyno_ship_id = 1_000_500_202;
    let beacon_id = 1_000_500_203;
    let fuel_id = 1_000_500_204;

    ensure_char(&ctx, jumper_id, 98_000_020, origin_sys, carrier_id, Some(fleet_id));
    ensure_char(&ctx, cyno_pilot_id, 98_000_020, dest_sys, cyno_ship_id, Some(fleet_id));
    grant_skill(&ctx, jumper_id, 21611, 5); // Jump Drive Calibration V
    grant_skill(&ctx, jumper_id, 21610, 4); // Jump Fuel Conservation IV (-40% fuel)
    grant_skill(&ctx, jumper_id, 3456, 4); // Jump Drive Operation IV (-20% cap)

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: cyno_pilot_id,
                members: vec![cyno_pilot_id, jumper_id],
                ..Default::default()
            },
        );
    });
    insert_item(&ctx, carrier_id, 999_320, jumper_id, origin_sys as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, fuel_id, 17888, jumper_id, carrier_id, InventoryFlag::Cargo, 5_000, false);
    spawn_ship(&ctx, origin_sys, carrier_id, 999_320, jumper_id, DVec3::ZERO, 0.0, 20_000.0).await;

    // Seed active fleet beacon in dest_sys
    let ctx_k = ctx.instance_id as usize;
    cyno_state().write().unwrap().beacons.insert(
        (ctx_k, cyno_ship_id),
        FleetModuleBeacon {
            beacon_id,
            char_id: cyno_pilot_id,
            ship_id: cyno_ship_id,
            beacon_type_id: 57425,
            solar_system_id: dest_sys,
            position: DVec3::new(1_000_000.0, 0.0, 0.0),
            expires_at_unix: ctx.now_unix_secs() + 600,
        },
    );

    let mut sess = SessionState {
        user_id: Some(jumper_id as u64),
        character_id: Some(jumper_id),
        corporation_id: Some(98_000_020),
        solar_system_id: Some(origin_sys),
        ship_id: Some(carrier_id),
        ..Default::default()
    };

    let outcome = jump_to_fleet_beacon(
        &ctx,
        &mut sess,
        &[
            PyRep::Integer(cyno_pilot_id),
            PyRep::Integer(i64::from(dest_sys)),
            PyRep::Integer(beacon_id),
        ],
        false,
    )
    .await
    .expect("jump to fleet beacon");
    assert_eq!(outcome.fuel_type, 17888);
    assert_eq!(outcome.fuel_qty, 1_200); // 2 LY * 1000 * (1 - 0.40) = 1200
    assert_eq!(sess.solar_system_id, Some(dest_sys));
    assert!(outcome.fatigue_min > 0.0);
    assert!(ctx
        .drain_notifications_for_char(jumper_id)
        .iter()
        .any(|n| n.event == "OnJumpTimersUpdated"));
}
