#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, OwnerNoteRecord, VirtualServerClock, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
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
    volume: f64,
    capacity: f64,
    name: &str,
    attrs: &[(u16, f64)],
) -> InvType {
    let mut t = sde.types.get(&587).expect("Rifter seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.volume = volume;
    t.capacity = capacity;
    t.name = name.into();
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t
}

fn test_ctx_with_custom_types<F: FnOnce(&mut SdeRegistry)>(mutate_sde: F) -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    mutate_sde(&mut sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

fn test_ctx() -> ServerContext {
    test_ctx_with_custom_types(|_| {})
}

fn ensure_char(ctx: &ServerContext, char_id: i64, corp_id: i64, station_id: Option<i64>, sys_id: u32, ship_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo seed character");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.station_id = station_id;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.security_status = 5.0;
        ch.clone_grade = 2;
        ch.fleet_id = None;
        snap.characters.entry(char_id).or_insert(ch);

        let mut corp = snap.corporations.values().next().cloned().expect("demo seed corp");
        corp.corporation_id = corp_id;
        corp.name = format!("Corp {corp_id}");
        corp.ticker = "TST".into();
        corp.ceo_id = char_id;
        corp.members.insert(char_id);
        snap.corporations.entry(corp_id).or_insert(corp);

        snap.skills.entry(char_id).or_default().insert(
            3330,
            eve_db::TrainedSkillRecord {
                type_id: 3330,
                level: 2,
                skill_points: 10_000.0,
            },
        );
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

fn make_session(char_id: i64, corp_id: i64, station_id: Option<i64>, sys_id: Option<u32>, ship_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id,
        structure_id: None,
        solar_system_id: sys_id,
        ship_id,
        ..Default::default()
    }
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

async fn spawn_ship_ball(ctx: &ServerContext, sys_id: u32, ball_id: i64, type_id: u32, owner_id: i64, pos: DVec3) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ball_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 2_500.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 200.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let meta = SlimItemMeta {
        group_id: 25,
        category_id: 6,
        owner_id,
        char_id: Some(owner_id),
        corp_id: 98_000_001,
        alliance_id: 0,
        security_status: 5.0,
        is_cloaked: false,
        ..Default::default()
    };
    sys.spawn_ball_with_meta(ball, CombatState::default(), meta)
        .await
        .expect("spawn ball");
}

#[test]
fn bay_flags_and_jettison_flags_match_canonical_ids() {
    assert!(is_ship_bay_flag(InventoryFlag::ShipHangar as u16));
    assert!(is_ship_bay_flag(InventoryFlag::SpecializedShipHold as u16));
    assert!(!is_ship_bay_flag(InventoryFlag::Cargo as u16));
    assert!(!is_ship_bay_flag(InventoryFlag::FrigateEscapeBay as u16));

    assert!(is_jettisonable_flag(InventoryFlag::Cargo));
    assert!(is_jettisonable_flag(InventoryFlag::FleetHangar));
    assert!(is_jettisonable_flag(InventoryFlag::SpecializedAmmoHold));
    assert!(!is_jettisonable_flag(InventoryFlag::HiSlot0));
    assert!(!is_jettisonable_flag(InventoryFlag::ShipHangar));
}

#[tokio::test]
async fn board_stored_ship_docked_happy_path_and_feb_guards() {
    // 19720 = Revelation (has SMA attr 908), 641 = Megathron (has FEB attr 3020), 606 = Velator (frigate), 16227 = Iteron Mark V (hauler), 587 = Rifter
    let ctx = test_ctx_with_custom_types(|sde| {
        let t1 = custom_inv_type(sde, 999_001, 485, 6, 1_000_000.0, 5_000.0, "Test Carrier Host", &[(908, 1_000_000.0)]);
        let t2 = custom_inv_type(sde, 999_002, 27, 6, 485_000.0, 500.0, "Test FEB Battleship", &[(3020, 50_000.0)]);
        let t3 = custom_inv_type(sde, 999_003, 25, 6, 100.0, 100.0, "Test Stored Rifter", &[]);
        sde.types.insert(999_001, t1);
        sde.types.insert(999_002, t2);
        sde.types.insert(999_003, t3);
    });

    let char_id = 91_000_001;
    let corp_id = 98_000_001;
    let station_id = 60_003_760;
    let sys_id = 30_000_142;
    let active_pod_id = 1_000_100_001;
    let carrier_id = 1_000_100_002;
    let stored_rifter_id = 1_000_100_003;
    let feb_battleship_id = 1_000_100_004;
    let invalid_non_frigate_in_feb_id = 1_000_100_005;

    ensure_char(&ctx, char_id, corp_id, Some(station_id), sys_id, active_pod_id);
    insert_item(&ctx, active_pod_id, 670, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, carrier_id, 999_001, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, stored_rifter_id, 999_003, char_id, carrier_id, InventoryFlag::ShipHangar, 1, true);
    insert_item(&ctx, feb_battleship_id, 999_002, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, invalid_non_frigate_in_feb_id, 16227, char_id, feb_battleship_id, InventoryFlag::FrigateEscapeBay, 1, true);

    let kw = HashMap::new();
    let mut sess = make_session(char_id, corp_id, Some(station_id), None, Some(active_pod_id));

    // Error: ShipNotFound
    let err_missing = board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(carrier_id), PyRep::Integer(999_999_999)], &kw)
        .await
        .unwrap_err();
    assert_eq!(err_missing.code, "ShipNotFound");

    // Error: InvalidFrigateGroup when hauler is inside FrigateEscapeBay
    let err_non_frig = board_stored_ship(
        &ctx,
        &mut sess,
        &[PyRep::Integer(feb_battleship_id), PyRep::Integer(invalid_non_frigate_in_feb_id)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err_non_frig.code, "InvalidFrigateGroup");

    // Error: NoFrigateEscapeBay when host lacks attr 3020
    let bogus_feb_id = 1_000_100_006;
    insert_item(&ctx, bogus_feb_id, 999_003, char_id, carrier_id, InventoryFlag::FrigateEscapeBay, 1, true);
    let err_no_feb = board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(carrier_id), PyRep::Integer(bogus_feb_id)], &kw)
        .await
        .unwrap_err();
    assert_eq!(err_no_feb.code, "NoFrigateEscapeBay");

    // Error: OwnershipViolation on another pilot's stored ship
    let foreign_rifter_id = 1_000_100_007;
    insert_item(&ctx, foreign_rifter_id, 999_003, 99_111_222, carrier_id, InventoryFlag::ShipHangar, 1, true);
    let err_owner = board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(carrier_id), PyRep::Integer(foreign_rifter_id)], &kw)
        .await
        .unwrap_err();
    assert_eq!(err_owner.code, "OwnershipViolation");

    // Happy path: board stored Rifter from docked carrier SMA
    let ok = board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(carrier_id), PyRep::Integer(stored_rifter_id)], &kw)
        .await
        .expect("board stored ship");
    assert_eq!(sess.ship_id, Some(stored_rifter_id));
    assert_eq!(ok.dict_get("shipID").and_then(|v| v.as_i64()), Some(stored_rifter_id));
    ctx.db.with_snapshot(|snap| {
        let ship = snap.items.get(&stored_rifter_id).unwrap();
        assert_eq!(ship.location_id, station_id);
        assert_eq!(ship.flag, InventoryFlag::Hangar);
    });

    // Error: CannotBoardFromOwnBay when currently piloting the host ship
    sess.ship_id = Some(carrier_id);
    let another_rifter_id = 1_000_100_008;
    insert_item(&ctx, another_rifter_id, 999_003, char_id, carrier_id, InventoryFlag::ShipHangar, 1, true);
    let err_own_bay = board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(carrier_id), PyRep::Integer(another_rifter_id)], &kw)
        .await
        .unwrap_err();
    assert_eq!(err_own_bay.code, "CannotBoardFromOwnBay");
}

#[tokio::test]
async fn board_stored_ship_in_space_range_and_success() {
    let ctx = test_ctx_with_custom_types(|sde| {
        let t3 = custom_inv_type(sde, 999_003, 25, 6, 100.0, 100.0, "Test Stored Frigate", &[]);
        sde.types.insert(999_003, t3);
    });
    let char_id = 91_000_010;
    let corp_id = 98_000_001;
    let sys_id = 30_000_142;
    let pod_id = 1_000_100_101;
    let carrier_id = 1_000_100_102;
    let stored_frigate_id = 1_000_100_103;

    ensure_char(&ctx, char_id, corp_id, None, sys_id, pod_id);
    insert_item(&ctx, pod_id, 670, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, carrier_id, 19720, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, stored_frigate_id, 999_003, char_id, carrier_id, InventoryFlag::ShipHangar, 1, true);

    spawn_ship_ball(&ctx, sys_id, pod_id, 670, char_id, DVec3::new(0.0, 0.0, 0.0)).await;
    spawn_ship_ball(&ctx, sys_id, carrier_id, 19720, char_id, DVec3::new(5_000.0, 0.0, 0.0)).await;

    let kw = HashMap::new();
    let mut sess = make_session(char_id, corp_id, None, Some(sys_id), Some(pod_id));

    // Error: out of 2500m boarding range
    let err_range = board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(carrier_id), PyRep::Integer(stored_frigate_id)], &kw)
        .await
        .unwrap_err();
    assert_eq!(err_range.code, "ShipOutOfBoardingRange");

    // Move carrier within 1000m and board
    ctx.systems
        .get_or_spawn(sys_id)
        .try_set_ball_position(carrier_id, DVec3::new(1_000.0, 0.0, 0.0));
    let res = board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(carrier_id), PyRep::Integer(stored_frigate_id)], &kw)
        .await
        .expect("board in space");
    assert_eq!(sess.ship_id, Some(stored_frigate_id));
    assert_eq!(res.dict_get("shipID").and_then(|v| v.as_i64()), Some(stored_frigate_id));
}

#[tokio::test]
async fn ship_jettison_happy_path_cooldown_and_guards() {
    let ctx = test_ctx();
    let char_id = 91_000_020;
    let corp_id = 98_000_001;
    let sys_id = 30_000_142;
    let ship_id = 1_000_100_201;
    let cargo_ore_id = 1_000_100_202;
    let courier_item_id = 1_000_100_203;
    let fitted_gun_id = 1_000_100_204;

    ensure_char(&ctx, char_id, corp_id, None, sys_id, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, cargo_ore_id, 1230, char_id, ship_id, InventoryFlag::Cargo, 50, false);
    insert_item(&ctx, courier_item_id, 1230, char_id, ship_id, InventoryFlag::Cargo, 10, false);
    insert_item(&ctx, fitted_gun_id, 484, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_custom_names.insert(courier_item_id, "courier_cargo:999".into());
    });
    spawn_ship_ball(&ctx, sys_id, ship_id, 587, char_id, DVec3::new(100_000.0, 0.0, 0.0)).await;

    let kw = HashMap::new();
    // Error: docked
    let mut docked_sess = make_session(char_id, corp_id, Some(60_003_760), None, Some(ship_id));
    assert_eq!(
        ship_jettison(&ctx, &mut docked_sess, &[PyRep::List(vec![PyRep::Integer(cargo_ore_id)])], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotJettisonWhileDocked"
    );

    let mut sess = make_session(char_id, corp_id, None, Some(sys_id), Some(ship_id));
    // Error: courier item cannot be jettisoned
    assert_eq!(
        ship_jettison(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Integer(courier_item_id)])], &kw)
            .await
            .unwrap_err()
            .code,
        "ItemCannotBeJettisoned"
    );

    // Error: fitted module cannot be jettisoned
    assert_eq!(
        ship_jettison(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Integer(fitted_gun_id)])], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotJettisonFittedModule"
    );

    // Happy path: jettison ore into new jetcan
    let jetcan_rep = ship_jettison(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Integer(cargo_ore_id)])], &kw)
        .await
        .expect("jettison cargo");
    let jetcan_id = match &jetcan_rep {
        PyRep::Tuple(parts) => match parts.first() {
            Some(PyRep::List(ids)) => ids.first().and_then(|v| v.as_i64()),
            _ => None,
        },
        _ => None,
    }
    .expect("jetcan id");
    assert!(jetcan_id > 0);
    ctx.db.with_snapshot(|snap| {
        let can = snap.items.get(&jetcan_id).expect("jetcan exists");
        assert_eq!(can.type_id, 23);
        let ore = snap.items.get(&cargo_ore_id).expect("ore moved");
        assert_eq!(ore.location_id, jetcan_id);
        assert_eq!(ore.flag, InventoryFlag::Cargo);
    });

    // Cooldown enforced immediately after jettison
    let extra_ore_id = 1_000_100_205;
    insert_item(&ctx, extra_ore_id, 1230, char_id, ship_id, InventoryFlag::Cargo, 10, false);
    assert_eq!(
        ship_jettison(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Integer(extra_ore_id)])], &kw)
            .await
            .unwrap_err()
            .code,
        "JettisonCooldownActive"
    );

    // Advance clock past 180s -> cooldown clears
    ctx.db.with_mut_snapshot(|snap| {
        let note_key = -10_000_018 - char_id;
        snap.owner_notes.insert(
            note_key,
            OwnerNoteRecord {
                note_id: note_key,
                character_id: char_id,
                item_id: 0,
                label: "last_jettison".into(),
                note: "1799999000".into(),
                timestamp_unix: 1_799_999_000,
            },
        );
    });
    assert!(ship_jettison(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Integer(extra_ore_id)])], &kw)
        .await
        .is_ok());
}

#[tokio::test]
async fn ship_drop_deployables_partial_split_and_security_guards() {
    // 33474 = Mobile Depot (group 1246, category 22 Deployable)
    let sys_id = 30_002_505u32;
    let ctx = test_ctx_with_custom_types(|sde| {
        sde.types.insert(
            33474,
            custom_inv_type(sde, 33474, 1246, 22, 50.0, 4_000.0, "Mobile Depot", &[]),
        );
        if let Some(mut sys) = sde.solar_systems.get(&30_000_142).cloned() {
            sys.solar_system_id = sys_id;
            sys.security_status = -0.5;
            sde.solar_systems.insert(sys_id, sys);
        }
    });
    let char_id = 91_000_030;
    let corp_id = 98_000_001;
    let ship_id = 1_000_100_301;
    let depot_stack_id = 1_000_100_302;
    let non_deployable_id = 1_000_100_303;

    ensure_char(&ctx, char_id, corp_id, None, sys_id, ship_id);
    insert_item(&ctx, ship_id, 16227, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, depot_stack_id, 33474, char_id, ship_id, InventoryFlag::Cargo, 3, false);
    insert_item(&ctx, non_deployable_id, 1230, char_id, ship_id, InventoryFlag::Cargo, 100, false);
    spawn_ship_ball(&ctx, sys_id, ship_id, 16227, char_id, DVec3::new(10_000_000_000.0, 0.0, 0.0)).await;

    let kw = HashMap::new();
    let mut sess = make_session(char_id, corp_id, None, Some(sys_id), Some(ship_id));

    // Non-deployable item fails
    assert!(
        ship_drop(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Integer(non_deployable_id)])], &kw)
            .await
            .is_err()
    );

    // Abyssal system guard (32_000_000..=32_999_999)
    let mut abyss_sess = make_session(char_id, corp_id, None, Some(32_000_001), Some(ship_id));
    assert!(
        ship_drop(&ctx, &mut abyss_sess, &[PyRep::List(vec![PyRep::Integer(depot_stack_id)])], &kw)
            .await
            .is_err()
    );

    // Partial stack drop: 1 out of 3 Mobile Depots
    let partial_arg = PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(depot_stack_id), PyRep::Integer(1)])]);
    let res = ship_drop(&ctx, &mut sess, &[partial_arg], &kw).await.expect("partial ship drop");
    match res {
        PyRep::Dict(entries) => assert_eq!(entries.len(), 1),
        other => panic!("expected dict from ship_drop, got {other:?}"),
    }
    ctx.db.with_snapshot(|snap| {
        let orig = snap.items.get(&depot_stack_id).expect("remaining depot stack");
        assert_eq!(orig.quantity, 2);
        let deployed_count = snap
            .items
            .values()
            .filter(|i| i.type_id == 33474 && i.location_id == sys_id as i64)
            .count();
        assert_eq!(deployed_count, 1);
    });
}

#[tokio::test]
async fn ship_scoop_and_store_docked_and_space_operations() {
    let ctx = test_ctx_with_custom_types(|sde| {
        sde.types.insert(
            999_010,
            custom_inv_type(
                sde,
                999_010,
                902,
                6,
                500_000.0,
                60_000.0,
                "Test Orca Host",
                &[(908, 400_000.0), (912, 40_000.0)],
            ),
        );
    });

    let char_id = 91_000_040;
    let corp_id = 98_000_001;
    let station_id = 60_003_760;
    let sys_id = 30_000_142;
    let pod_id = 1_000_100_401;
    let orca_id = 1_000_100_402;
    let rifter_to_store = 1_000_100_403;

    ensure_char(&ctx, char_id, corp_id, Some(station_id), sys_id, pod_id);
    insert_item(&ctx, pod_id, 670, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, orca_id, 999_010, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, rifter_to_store, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);

    let kw = HashMap::new();
    let mut sess = make_session(char_id, corp_id, Some(station_id), None, Some(pod_id));

    // Docked StoreVessel: Rifter into Orca SMA
    let stored_res = ship_scoop_and_store(
        &ctx,
        &mut sess,
        "StoreVessel",
        &[PyRep::Integer(rifter_to_store), PyRep::Integer(orca_id)],
        &kw,
    )
    .await
    .expect("docked store vessel");
    assert_eq!(stored_res.dict_get("hostShipID").and_then(|v| v.as_i64()), Some(orca_id));
    ctx.db.with_snapshot(|snap| {
        let r = snap.items.get(&rifter_to_store).unwrap();
        assert_eq!(r.location_id, orca_id);
        assert_eq!(r.flag, InventoryFlag::ShipHangar);
    });

    // Space ScoopToSMA: scoop unpiloted ship from space into Orca SMA while piloting Orca
    let floating_rifter_id = 1_000_100_404;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&orca_id).unwrap().location_id = sys_id as i64;
    });
    insert_item(&ctx, floating_rifter_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_ship_ball(&ctx, sys_id, orca_id, 999_010, char_id, DVec3::new(50_000.0, 0.0, 0.0)).await;
    spawn_ship_ball(&ctx, sys_id, floating_rifter_id, 587, char_id, DVec3::new(50_800.0, 0.0, 0.0)).await;

    let mut space_sess = make_session(char_id, corp_id, None, Some(sys_id), Some(orca_id));
    let scoop_res = ship_scoop_and_store(
        &ctx,
        &mut space_sess,
        "ScoopToSMA",
        &[PyRep::Integer(floating_rifter_id)],
        &kw,
    )
    .await
    .expect("scoop ship to SMA");
    assert_eq!(scoop_res.dict_get("hostShipID").and_then(|v| v.as_i64()), Some(orca_id));
    ctx.db.with_snapshot(|snap| {
        let r = snap.items.get(&floating_rifter_id).unwrap();
        assert_eq!(r.location_id, orca_id);
        assert_eq!(r.flag, InventoryFlag::ShipHangar);
    });

    // Space StoreVessel on active piloted ship: ejects pilot into new capsule and stores ship in host SMA
    let pilot_rifter_id = 1_000_100_405;
    insert_item(&ctx, pilot_rifter_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_ship_ball(&ctx, sys_id, pilot_rifter_id, 587, char_id, DVec3::new(50_500.0, 0.0, 0.0)).await;
    space_sess.ship_id = Some(pilot_rifter_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.active_ship_id = pilot_rifter_id;
        }
    });
    let active_store_res = ship_scoop_and_store(
        &ctx,
        &mut space_sess,
        "StoreVessel",
        &[PyRep::Integer(pilot_rifter_id), PyRep::Integer(orca_id)],
        &kw,
    )
    .await
    .expect("store active ship in host SMA");
    let new_pod_id = active_store_res.dict_get("capsuleID").and_then(|v| v.as_i64()).expect("ejected capsuleID");
    assert_eq!(space_sess.ship_id, Some(new_pod_id));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&pilot_rifter_id).unwrap().location_id, orca_id);
        assert_eq!(snap.items.get(&new_pod_id).unwrap().type_id, 670);
    });
}

#[tokio::test]
async fn group_2b_board_stored_ship_corp_password_space_guards_and_capsule_swap() {
    let ctx = test_ctx_with_custom_types(|sde| {
        sde.types.insert(
            999_020,
            custom_inv_type(sde, 999_020, 485, 6, 1_000_000.0, 5_000.0, "Group2B Carrier", &[(908, 1_000_000.0)]),
        );
        sde.types.insert(
            999_021,
            custom_inv_type(sde, 999_021, 25, 6, 100.0, 100.0, "Group2B Stored Frigate", &[]),
        );
    });

    let char_id = 91_002_001;
    let ceo_id = 91_002_002;
    let other_char_id = 91_002_003;
    let corp_id = 98_002_001;
    let station_id = 60_003_760;
    let sys_id = 30_000_142; // Jita (sys_sec >= 0.45)

    let active_pod_id = 1_000_200_001;
    let personal_host_cargo_id = 1_000_200_002;
    let corp_host_bad_flag_id = 1_000_200_003;
    let corp_host_sag2_id = 1_000_200_004;
    let foreign_host_id = 1_000_200_005;
    let stored_non_ship_id = 1_000_200_006;
    let stored_unpackaged_id = 1_000_200_007;
    let stored_in_cargo_id = 1_000_200_008;
    let stored_corp_ship_id = 1_000_200_009;
    let stored_pw_ship_id = 1_000_200_010;
    let stored_bad_host_ship_id = 1_000_200_011;

    ensure_char(&ctx, ceo_id, corp_id, Some(station_id), sys_id, active_pod_id);
    ensure_char(&ctx, char_id, corp_id, Some(station_id), sys_id, active_pod_id);
    ctx.db.with_mut_snapshot(|snap| {
        // Ensure char_id is NOT treated as CEO when checking corp role gates.
        snap.corporations.get_mut(&corp_id).unwrap().ceo_id = ceo_id;
    });

    insert_item(&ctx, active_pod_id, 670, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, personal_host_cargo_id, 999_020, char_id, station_id, InventoryFlag::Cargo, 1, true);
    insert_item(&ctx, corp_host_bad_flag_id, 999_020, corp_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, corp_host_sag2_id, 999_020, corp_id, station_id, InventoryFlag::CorpSAG2, 1, true);
    insert_item(&ctx, foreign_host_id, 999_020, other_char_id, station_id, InventoryFlag::Hangar, 1, true);

    insert_item(&ctx, stored_non_ship_id, 1230, char_id, corp_host_sag2_id, InventoryFlag::ShipHangar, 1, true);
    insert_item(&ctx, stored_unpackaged_id, 999_021, char_id, corp_host_sag2_id, InventoryFlag::ShipHangar, 1, false);
    insert_item(&ctx, stored_in_cargo_id, 999_021, char_id, corp_host_sag2_id, InventoryFlag::Cargo, 1, true);
    insert_item(&ctx, stored_bad_host_ship_id, 999_021, char_id, 1230, InventoryFlag::ShipHangar, 1, true);
    insert_item(&ctx, stored_corp_ship_id, 999_021, corp_id, corp_host_bad_flag_id, InventoryFlag::ShipHangar, 1, true);
    insert_item(&ctx, stored_pw_ship_id, 999_021, char_id, personal_host_cargo_id, InventoryFlag::ShipHangar, 1, true);

    let kw = HashMap::new();
    let mut sess = make_session(char_id, corp_id, Some(station_id), None, Some(active_pod_id));

    // ShipNotBoardable (non-ship & unpackaged ship)
    assert_eq!(
        board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(corp_host_sag2_id), PyRep::Integer(stored_non_ship_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "ShipNotBoardable"
    );
    assert_eq!(
        board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(corp_host_sag2_id), PyRep::Integer(stored_unpackaged_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "ShipNotBoardable"
    );

    // ShipNotInMaintenanceBay (stored in Cargo, or bogus non-ship/non-structure host)
    assert_eq!(
        board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(corp_host_sag2_id), PyRep::Integer(stored_in_cargo_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "ShipNotInMaintenanceBay"
    );
    assert_eq!(
        board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(0), PyRep::Integer(stored_bad_host_ship_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "ShipNotInMaintenanceBay"
    );

    // Docked host flag & ownership & corp role guards
    assert_eq!(
        board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(personal_host_cargo_id), PyRep::Integer(stored_pw_ship_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "InvalidLocation"
    );
    assert_eq!(
        board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(corp_host_bad_flag_id), PyRep::Integer(stored_corp_ship_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "InvalidLocation"
    );
    // Reassign stored_corp_ship under corp_host_sag2, move personal_host to Hangar for next checks
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&personal_host_cargo_id).unwrap().flag = InventoryFlag::Hangar;
        snap.items.get_mut(&stored_corp_ship_id).unwrap().location_id = corp_host_sag2_id;
    });
    assert_eq!(
        board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(corp_host_sag2_id), PyRep::Integer(stored_corp_ship_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );

    // Docked foreign host fails OwnershipViolation even with matching host_arg
    let foreign_ship_in_foreign_host = 1_000_200_012;
    insert_item(&ctx, foreign_ship_in_foreign_host, 999_021, char_id, foreign_host_id, InventoryFlag::ShipHangar, 1, true);
    assert_eq!(
        board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(foreign_host_id), PyRep::Integer(foreign_ship_in_foreign_host)], &kw)
            .await
            .unwrap_err()
            .code,
        "OwnershipViolation"
    );

    // Corp take role + equipment config role succeeds on corp-owned ship inside corp SAG2 host
    sess.corp_role = (1u64 << 14) | eve_db::corp_roles::ROLE_EQUIPMENT_CONFIG;
    let ok_corp = board_stored_ship(
        &ctx,
        &mut sess,
        &[PyRep::Integer(corp_host_sag2_id), PyRep::Integer(stored_corp_ship_id)],
        &kw,
    )
    .await
    .expect("corp role permits boarding stored corp ship");
    assert_eq!(ok_corp.dict_get("shipID").and_then(|v| v.as_i64()), Some(stored_corp_ship_id));

    // SMA password guard on non-owner host (corp host != char_id): missing/wrong password -> SMAAccessDenied; right password succeeds
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&stored_pw_ship_id).unwrap().location_id = corp_host_sag2_id;
    });
    crate::services::inventory_fitting::fitting_containers_ships::write_container_ship_state(&ctx, |st| {
        let cfg = st.ship_configs.entry(corp_host_sag2_id).or_default();
        cfg.allow_corp_smb = true;
        cfg.password = Some("secret_bay".into());
    });
    assert_eq!(
        board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(corp_host_sag2_id), PyRep::Integer(stored_pw_ship_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "SMAAccessDenied"
    );
    let mut kw_pw = HashMap::new();
    kw_pw.insert("password".into(), PyRep::String("secret_bay".into()));
    board_stored_ship(&ctx, &mut sess, &[PyRep::Integer(corp_host_sag2_id), PyRep::Integer(stored_pw_ship_id)], &kw_pw)
        .await
        .expect("correct password grants bay boarding access");
    sess.corp_role = 0;

    // In-space guards: NoSolarSystem, NoActiveShip, Criminal in high-sec, WeaponsTimer, ShipInWarp, ShipNotInSpace
    let space_host_id = 1_000_200_020;
    let space_pod_id = 1_000_200_021;
    let space_stored_1 = 1_000_200_022;
    let space_stored_2 = 1_000_200_023;
    let space_char_id = 91_002_004;
    ensure_char(&ctx, space_char_id, corp_id, None, sys_id, space_pod_id);
    insert_item(&ctx, space_host_id, 999_020, space_char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, space_pod_id, 670, space_char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, space_stored_1, 999_021, space_char_id, space_host_id, InventoryFlag::ShipHangar, 1, true);
    insert_item(&ctx, space_stored_2, 999_021, space_char_id, space_host_id, InventoryFlag::ShipHangar, 1, true);
    spawn_ship_ball(&ctx, sys_id, space_host_id, 999_020, space_char_id, DVec3::new(0.0, 0.0, 0.0)).await;
    spawn_ship_ball(&ctx, sys_id, space_pod_id, 670, space_char_id, DVec3::new(500.0, 0.0, 0.0)).await;

    let mut no_sys_sess = make_session(space_char_id, corp_id, None, None, Some(space_pod_id));
    assert_eq!(
        board_stored_ship(&ctx, &mut no_sys_sess, &[PyRep::Integer(space_host_id), PyRep::Integer(space_stored_1)], &kw)
            .await
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );
    let mut no_ship_sess = make_session(space_char_id, corp_id, None, Some(sys_id), None);
    assert_eq!(
        board_stored_ship(&ctx, &mut no_ship_sess, &[PyRep::Integer(space_host_id), PyRep::Integer(space_stored_1)], &kw)
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    let mut space_sess = make_session(space_char_id, corp_id, None, Some(sys_id), Some(space_pod_id));
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.states.entry(space_char_id).or_default().criminal_flag_until_unix = u64::MAX / 2;
    }
    assert_eq!(
        board_stored_ship(&ctx, &mut space_sess, &[PyRep::Integer(space_host_id), PyRep::Integer(space_stored_1)], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotBoardShipInHighSecAsCriminal"
    );
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.states.entry(space_char_id).or_default();
        st.criminal_flag_until_unix = 0;
        st.weapon_timer_until_unix = u64::MAX / 2;
    }
    assert_eq!(
        board_stored_ship(&ctx, &mut space_sess, &[PyRep::Integer(space_host_id), PyRep::Integer(space_stored_1)], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotStoreShipActiveWeaponsTimer"
    );
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.states.entry(space_char_id).or_default().weapon_timer_until_unix = 0;
    }

    // ShipInWarp guard
    {
        let sys = ctx.systems.get_or_spawn(sys_id);
        let mut bp = sys.ballpark.write().unwrap();
        bp.balls.get_mut(&space_pod_id).unwrap().mode = MovementMode::Warp {
            origin: DVec3::new(500.0, 0.0, 0.0),
            destination: DVec3::new(10_000_000.0, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 9_999_500.0,
            phase: eve_destiny::WarpPhase::Cruising,
            elapsed_warp_time: 5.0,
        };
    }
    assert_eq!(
        board_stored_ship(&ctx, &mut space_sess, &[PyRep::Integer(space_host_id), PyRep::Integer(space_stored_1)], &kw)
            .await
            .unwrap_err()
            .code,
        "ShipInWarp"
    );
    {
        let sys = ctx.systems.get_or_spawn(sys_id);
        let mut bp = sys.ballpark.write().unwrap();
        bp.balls.get_mut(&space_pod_id).unwrap().mode = MovementMode::Stop;
    }

    // Boarding from capsule in space removes old capsule from snap.items
    board_stored_ship(&ctx, &mut space_sess, &[PyRep::Integer(space_host_id), PyRep::Integer(space_stored_1)], &kw)
        .await
        .expect("board stored_1 from capsule");
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&space_pod_id), "capsule must be removed on in-space board");
        assert_eq!(snap.items[&space_stored_1].location_id, sys_id as i64);
    });

    // Boarding from non-capsule frigate preserves old frigate in space and clears its char_id
    board_stored_ship(&ctx, &mut space_sess, &[PyRep::Integer(space_host_id), PyRep::Integer(space_stored_2)], &kw)
        .await
        .expect("board stored_2 from non-capsule ship");
    ctx.db.with_snapshot(|snap| {
        assert!(snap.items.contains_key(&space_stored_1), "previous non-capsule ship must remain in space");
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let prev_char_id = sys.ballpark.read().unwrap().ball_slim_meta.get(&space_stored_1).and_then(|m| m.char_id);
    assert_eq!(prev_char_id, None);
}

#[tokio::test]
async fn group_2b_jettison_drop_and_scoop_all_guard_and_hold_branches() {
    let sys_high = 30_000_142u32; // Jita (0.9)
    let sys_low = 30_002_505u32;  // custom low/null below
    let ctx = test_ctx_with_custom_types(|sde| {
        sde.types.insert(
            999_030,
            custom_inv_type(sde, 999_030, 902, 6, 500_000.0, 50_000.0, "Group2B Carrier", &[(908, 400_000.0), (912, 40_000.0), (5646, 50_000.0), (5325, 20_000.0)]),
        );
        sde.types.insert(
            999_031,
            custom_inv_type(sde, 999_031, 902, 6, 500_000.0, 0.0, "ZeroHold Ship", &[]),
        );
        sde.types.insert(
            999_032,
            custom_inv_type(sde, 999_032, 361, 22, 100.0, 0.0, "Empire Disallowed Bubble", &[(1074, 1.0)]),
        );
        sde.types.insert(
            999_033,
            custom_inv_type(sde, 999_033, 1246, 22, 100.0, 0.0, "SecBounded Depot", &[(1032, 0.5), (1946, 0.2)]),
        );
        sde.types.insert(
            crate::services::mining_phased_autotarget::TYPE_MOBILE_PHASE_ANCHOR,
            custom_inv_type(sde, crate::services::mining_phased_autotarget::TYPE_MOBILE_PHASE_ANCHOR, 1250, 22, 100.0, 0.0, "Mobile Phase Anchor", &[]),
        );
        sde.types.insert(
            33474,
            custom_inv_type(sde, 33474, 1246, 22, 50.0, 4_000.0, "Mobile Depot", &[]),
        );
        sde.types.insert(
            999_034,
            custom_inv_type(sde, 999_034, 4137, 22, 100.0, 0.0, "CRAB Beacon", &[]),
        );
        sde.types.insert(
            999_035,
            custom_inv_type(sde, 999_035, 1657, 65, 100.0, 0.0, "Citadel", &[]),
        );
        sde.types.insert(
            999_036,
            custom_inv_type(sde, 999_036, 1230, 4, 200.0, 0.0, "Bulky Ore", &[]),
        );
        sde.types.insert(
            999_037,
            custom_inv_type(sde, 999_037, 12, 2, 10.0, 100.0, "Freight Cont", &[]),
        );
        sde.types.insert(
            999_038,
            custom_inv_type(sde, 999_038, 186, 2, 100.0, 0.0, "Wreck", &[]),
        );
        if let Some(mut sys) = sde.solar_systems.get(&30_000_142).cloned() {
            sys.solar_system_id = sys_low;
            sys.security_status = 0.0;
            sde.solar_systems.insert(sys_low, sys);
        }
    });

    let char_id = 91_003_001;
    let ceo_id = 91_003_002;
    let stranger_id = 91_003_003;
    let corp_id = 98_003_001;
    let carrier_id = 1_000_300_001;
    let zero_hold_id = 1_000_300_002;
    ensure_char(&ctx, ceo_id, corp_id, None, sys_high, carrier_id);
    ensure_char(&ctx, char_id, corp_id, None, sys_high, carrier_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().ceo_id = ceo_id;
    });
    insert_item(&ctx, carrier_id, 999_030, char_id, sys_high as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, zero_hold_id, 999_031, char_id, sys_high as i64, InventoryFlag::None, 1, true);

    let gate_pos = ctx
        .sde
        .stargates()
        .values()
        .find(|g| g.from_solar_system_id == sys_high)
        .map(|g| g.position)
        .unwrap_or(DVec3::ZERO);
    spawn_ship_ball(&ctx, sys_high, carrier_id, 999_030, char_id, gate_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    spawn_ship_ball(&ctx, sys_high, zero_hold_id, 999_031, char_id, gate_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;

    let kw = HashMap::new();
    let mut sess = make_session(char_id, corp_id, None, Some(sys_high), Some(carrier_id));

    // --- Jettison error branches ---
    let stored_ship = 1_000_300_010;
    let cargo_ship = 1_000_300_011;
    let implant_item = 1_000_300_012;
    let cont_item = 1_000_300_013;
    let oversize_ore = 1_000_300_014;
    insert_item(&ctx, stored_ship, 587, char_id, carrier_id, InventoryFlag::ShipHangar, 1, true);
    insert_item(&ctx, cargo_ship, 587, char_id, carrier_id, InventoryFlag::Cargo, 1, true);
    insert_item(&ctx, implant_item, 1230, char_id, carrier_id, InventoryFlag::Implant, 1, true);
    insert_item(&ctx, cont_item, 999_037, char_id, carrier_id, InventoryFlag::Cargo, 1, true);
    insert_item(&ctx, oversize_ore, 999_036, char_id, carrier_id, InventoryFlag::Cargo, 300, false); // 60_000 > 55_000 m3

    assert_eq!(ship_jettison(&ctx, &mut sess, &[PyRep::List(vec![])], &kw).await.unwrap_err().code, "InvalidArgument");
    assert_eq!(ship_jettison(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Integer(stored_ship)])], &kw).await.unwrap_err().code, "CannotJettisonStoredShip");
    assert_eq!(ship_jettison(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Integer(cargo_ship)])], &kw).await.unwrap_err().code, "CannotJettisonShip");
    assert_eq!(ship_jettison(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Integer(implant_item)])], &kw).await.unwrap_err().code, "CannotJettisonItem");
    assert_eq!(ship_jettison(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Integer(cont_item)])], &kw).await.unwrap_err().code, "CannotPutContainerInContainer");
    assert_eq!(ship_jettison(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Integer(oversize_ore)])], &kw).await.unwrap_err().code, "CapacityExceeded");

    // --- ship_drop security, quantity, Zarzakh, and stargate proximity guards ---
    let emp_bubble = 1_000_300_020;
    let phase_anchor = 1_000_300_021;
    let sec_depot = 1_000_300_022;
    let ok_depot = 1_000_300_023;
    insert_item(&ctx, emp_bubble, 999_032, char_id, carrier_id, InventoryFlag::Cargo, 1, false);
    insert_item(&ctx, phase_anchor, crate::services::mining_phased_autotarget::TYPE_MOBILE_PHASE_ANCHOR, char_id, carrier_id, InventoryFlag::Cargo, 1, false);
    insert_item(&ctx, sec_depot, 999_033, char_id, carrier_id, InventoryFlag::Cargo, 2, false);
    insert_item(&ctx, ok_depot, 33474, char_id, carrier_id, InventoryFlag::Cargo, 1, false);

    // Foreign drop_owner rejected
    assert!(ship_drop(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Integer(ok_depot)]), PyRep::Integer(stranger_id)], &kw).await.is_err());
    // Zarzakh rejected
    let mut zarzakh_sess = make_session(char_id, corp_id, None, Some(30_100_000), Some(carrier_id));
    assert_eq!(ship_drop(&ctx, &mut zarzakh_sess, &[PyRep::List(vec![PyRep::Integer(ok_depot)])], &kw).await.unwrap_err().code, "AnchoringDisallowedInZarzakh");
    // Empire space disallowed (group 361 / attr 1074)
    assert!(ship_drop(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Integer(emp_bubble)])], &kw).await.is_err());
    // Mobile Phase Anchor in highsec -> CannotDeployInHighSec
    assert_eq!(ship_drop(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Integer(phase_anchor)])], &kw).await.unwrap_err().code, "CannotDeployInHighSec");
    // Custom attr 1032 max_sec in 0.9 highsec -> UserError
    assert!(ship_drop(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Integer(sec_depot)])], &kw).await.is_err());
    // Move to 0.0 low/null away from stargates -> attr 1946 min_sec (0.2) rejects!
    let mut low_sess = make_session(char_id, corp_id, None, Some(sys_low), Some(carrier_id));
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&carrier_id).unwrap().location_id = sys_low as i64;
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = sys_low;
    });
    spawn_ship_ball(&ctx, sys_low, carrier_id, 999_030, char_id, DVec3::new(1_000_000.0, 0.0, 0.0)).await;
    assert!(ship_drop(&ctx, &mut low_sess, &[PyRep::List(vec![PyRep::Integer(sec_depot)])], &kw).await.is_err());
    // Bad quantity (0 and > orig_qty)
    assert!(ship_drop(&ctx, &mut low_sess, &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(ok_depot), PyRep::Integer(0)])])], &kw).await.is_err());
    assert!(ship_drop(&ctx, &mut low_sess, &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(ok_depot), PyRep::Integer(5)])])], &kw).await.is_err());
    // Close to stargate in highsec
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&carrier_id).unwrap().location_id = sys_high as i64;
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = sys_high;
    });
    let gate_err = ship_drop(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Integer(ok_depot)])], &kw).await.unwrap_err();
    assert!(gate_err.message.contains("stargate"), "expected stargate proximity error, got: {}", gate_err.message);

    // --- Scoop branches: ScoopToSMA / FleetHangar / InfrastructureHold / MobileDepotHold / Scoop ---
    let obj_pos = gate_pos + DVec3::new(1_200.0, 0.0, 0.0);
    let jetcan_space = 1_000_300_101;
    let non_empty_cont = 1_000_300_102;
    let inner_loot = 1_000_300_103;
    let crab_beacon = 1_000_300_104;
    let citadel_struct = 1_000_300_105;
    let unpackaged_ship = 1_000_300_106;
    let ship_with_ore = 1_000_300_107;
    let ship_cargo_ore = 1_000_300_108;
    let corp_ship = 1_000_300_109;
    let wreck_item = 1_000_300_110;
    let abandoned_ore = 1_000_300_111;
    let depot_obj = 1_000_300_112;
    let infra_obj = 1_000_300_113;

    insert_item(&ctx, jetcan_space, 23, char_id, sys_high as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, non_empty_cont, 999_037, char_id, sys_high as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, inner_loot, 1230, char_id, non_empty_cont, InventoryFlag::Cargo, 1, false);
    insert_item(&ctx, crab_beacon, 999_034, char_id, sys_high as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, citadel_struct, 999_035, char_id, sys_high as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, unpackaged_ship, 587, char_id, sys_high as i64, InventoryFlag::None, 1, false);
    insert_item(&ctx, ship_with_ore, 587, char_id, sys_high as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, ship_cargo_ore, 1230, char_id, ship_with_ore, InventoryFlag::Cargo, 1, false);
    insert_item(&ctx, corp_ship, 587, corp_id, sys_high as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, wreck_item, 999_038, char_id, sys_high as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, abandoned_ore, 999_036, 0, sys_high as i64, InventoryFlag::None, 5, false); // 1_000 m3
    insert_item(&ctx, depot_obj, 33474, char_id, sys_high as i64, InventoryFlag::None, 1, true);
    let foreign_depot = 1_000_300_114;
    insert_item(&ctx, foreign_depot, 33474, stranger_id, sys_high as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, infra_obj, 33474, char_id, sys_high as i64, InventoryFlag::None, 1, true);
    for id in [jetcan_space, non_empty_cont, crab_beacon, citadel_struct, unpackaged_ship, ship_with_ore, corp_ship, wreck_item, abandoned_ore, depot_obj, foreign_depot, infra_obj] {
        spawn_ship_ball(&ctx, sys_high, id, 587, char_id, obj_pos).await;
    }

    assert_eq!(ship_scoop_and_store(&ctx, &mut sess, "Scoop", &[PyRep::Integer(0)], &kw).await.unwrap_err().code, "InvalidArgument");
    assert_eq!(ship_scoop_and_store(&ctx, &mut sess, "Scoop", &[PyRep::Integer(jetcan_space)], &kw).await.unwrap_err().code, "CannotPutContainerInContainer");
    assert_eq!(ship_scoop_and_store(&ctx, &mut sess, "Scoop", &[PyRep::Integer(non_empty_cont)], &kw).await.unwrap_err().code, "CannotPutContainerInContainer");
    assert_eq!(ship_scoop_and_store(&ctx, &mut sess, "Scoop", &[PyRep::Integer(crab_beacon)], &kw).await.unwrap_err().code, "CannotScoop");
    assert_eq!(ship_scoop_and_store(&ctx, &mut sess, "Scoop", &[PyRep::Integer(citadel_struct)], &kw).await.unwrap_err().code, "CannotScoopStructure");
    assert_eq!(ship_scoop_and_store(&ctx, &mut sess, "Scoop", &[PyRep::Integer(wreck_item)], &kw).await.unwrap_err().code, "CannotScoopWreck");
    assert_eq!(ship_scoop_and_store(&ctx, &mut sess, "ScoopToFleetHangar", &[PyRep::Integer(wreck_item)], &kw).await.unwrap_err().code, "CannotScoopWreck");
    assert_eq!(ship_scoop_and_store(&ctx, &mut sess, "ScoopToMobileDepotHold", &[PyRep::Integer(foreign_depot)], &kw).await.unwrap_err().code, "OwnershipViolation");

    // ScoopToSMA error branches
    assert_eq!(ship_scoop_and_store(&ctx, &mut sess, "ScoopToSMA", &[PyRep::Integer(abandoned_ore)], &kw).await.unwrap_err().code, "CannotStoreCargoInSMA");
    assert_eq!(ship_scoop_and_store(&ctx, &mut sess, "ScoopToSMA", &[PyRep::Integer(unpackaged_ship)], &kw).await.unwrap_err().code, "OnlyAssembledShipsInSMA");
    assert_eq!(ship_scoop_and_store(&ctx, &mut sess, "ScoopToSMA", &[PyRep::Integer(ship_with_ore)], &kw).await.unwrap_err().code, "ShipHasNonChargeCargo");
    assert_eq!(ship_scoop_and_store(&ctx, &mut sess, "ScoopToSMA", &[PyRep::Integer(corp_ship)], &kw).await.unwrap_err().code, "CorpRoleDenied");

    // ZeroHold ship rejects NoShipMaintenanceBay / NoFleetHangar / NoInfrastructureHold / NoMobileDepotHold / NoCargoHold
    let mut zero_sess = make_session(char_id, corp_id, None, Some(sys_high), Some(zero_hold_id));
    assert_eq!(ship_scoop_and_store(&ctx, &mut zero_sess, "ScoopToFleetHangar", &[PyRep::Integer(abandoned_ore)], &kw).await.unwrap_err().code, "NoFleetHangar");
    assert_eq!(ship_scoop_and_store(&ctx, &mut zero_sess, "ScoopToInfrastructureHold", &[PyRep::Integer(infra_obj)], &kw).await.unwrap_err().code, "NoInfrastructureHold");
    assert_eq!(ship_scoop_and_store(&ctx, &mut zero_sess, "ScoopToMobileDepotHold", &[PyRep::Integer(depot_obj)], &kw).await.unwrap_err().code, "NoMobileDepotHold");
    assert_eq!(ship_scoop_and_store(&ctx, &mut zero_sess, "Scoop", &[PyRep::Integer(abandoned_ore)], &kw).await.unwrap_err().code, "NoCargoHold");

    // Wrong item types for FleetHangar / InfrastructureHold / MobileDepotHold / Cargo
    sess.corp_role = eve_db::corp_roles::ROLE_STATION_MANAGER;
    assert_eq!(ship_scoop_and_store(&ctx, &mut sess, "ScoopToFleetHangar", &[PyRep::Integer(corp_ship)], &kw).await.unwrap_err().code, "CannotStoreShipInFleetHangar");
    assert_eq!(ship_scoop_and_store(&ctx, &mut sess, "ScoopToInfrastructureHold", &[PyRep::Integer(abandoned_ore)], &kw).await.unwrap_err().code, "UserError");
    assert_eq!(ship_scoop_and_store(&ctx, &mut sess, "ScoopToMobileDepotHold", &[PyRep::Integer(abandoned_ore)], &kw).await.unwrap_err().code, "UserError");
    assert_eq!(ship_scoop_and_store(&ctx, &mut sess, "Scoop", &[PyRep::Integer(corp_ship)], &kw).await.unwrap_err().code, "CannotStoreShipInCargo");

    // Happy paths: ScoopToSMA (corp with role), ScoopToInfrastructureHold, ScoopToMobileDepotHold (transfers owner 0 -> char_id), ScoopToFleetHangar (transfers owner 0 -> char_id)
    let r_sma = ship_scoop_and_store(&ctx, &mut sess, "ScoopToSMA", &[PyRep::Integer(corp_ship)], &kw).await.expect("scoop corp ship to SMA");
    assert_eq!(r_sma.dict_get("flagID").and_then(|v| v.as_i64()), Some(90));
    let r_infra = ship_scoop_and_store(&ctx, &mut sess, "ScoopToInfrastructureHold", &[PyRep::Integer(infra_obj)], &kw).await.expect("scoop to infra hold");
    assert_eq!(r_infra.dict_get("flagID").and_then(|v| v.as_i64()), Some(185));
    let r_depot = ship_scoop_and_store(&ctx, &mut sess, "ScoopToMobileDepotHold", &[PyRep::Integer(depot_obj)], &kw).await.expect("scoop abandoned depot to mobile depot hold");
    assert_eq!(r_depot.dict_get("flagID").and_then(|v| v.as_i64()), Some(179));
    let r_fh = ship_scoop_and_store(&ctx, &mut sess, "ScoopToFleetHangar", &[PyRep::Integer(abandoned_ore)], &kw).await.expect("scoop abandoned ore to fleet hangar");
    assert_eq!(r_fh.dict_get("flagID").and_then(|v| v.as_i64()), Some(155));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&depot_obj].owner_id, char_id);
        assert_eq!(snap.items[&abandoned_ore].owner_id, char_id);
        assert_eq!(snap.items[&corp_ship].location_id, carrier_id);
    });
}

