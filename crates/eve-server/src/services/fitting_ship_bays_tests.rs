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
