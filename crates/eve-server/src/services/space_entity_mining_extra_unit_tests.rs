#![allow(unused_imports)]
//! Extra unit and mutation-killing tests for `space_entity_mining.rs`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::space_entity_mining::handle;
use crate::system_actor::AsteroidState;
use eve_db::{
    AsteroidRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry, TypeMaterialEntry};
use glam::DVec3;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "space_drone_mining_and_reprocessing_unit_tests.rs"]
mod space_drone_mining_and_reprocessing_unit_tests;

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn upsert_type(
    sde: &mut Arc<SdeRegistry>,
    tid: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    volume: f64,
    capacity: f64,
    portion_size: u64,
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg.types.values().next().cloned().expect("base type");
    rec.type_id = tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = name.into();
    rec.volume = volume;
    rec.capacity = capacity;
    rec.portion_size = portion_size;
    rec.base_price = 1_000.0;
    rec.default_attributes.clear();
    if category_id == 6 {
        rec.default_attributes.insert(14, 8.0);
        rec.default_attributes.insert(13, 8.0);
        rec.default_attributes.insert(12, 8.0);
        rec.default_attributes.insert(1137, 3.0);
        rec.default_attributes.insert(11, 10_000.0);
        rec.default_attributes.insert(48, 10_000.0);
    } else if category_id == 7 {
        rec.effect_ids = if group_id == 778 { vec![2663] } else { vec![11, 12, 13, 16] };
    }
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(tid, rec);
}

pub(super) fn make_session(
    char_id: i64,
    corp_id: i64,
    sys_id: Option<u32>,
    ship_id: Option<i64>,
) -> SessionState {
    let mut s = SessionState::new(701);
    s.user_id = Some(11);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = sys_id;
    s.ship_id = ship_id;
    s.role = service_roles::ROLE_PLAYER_DEFAULT;
    s
}

pub(super) fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: false,
        ..Default::default()
    }
}

pub(super) fn make_ship_ball(ship_id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id: ship_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ice_harvesting_module_compatibility_and_bound_system_resolution() {
    let mut ctx = test_ctx();
    let (sys_id, char_id, corp_id) = (30_000_142, 90_000_001, 98_000_001);
    let (ship_id, ice_mod_id, ore_mod_id, ice_ast_id) =
        (1_090_000_001, 1_090_000_002, 1_090_000_003, 1_090_000_010);

    // Mackinaw-like ship with SpecializedIceHold (attr 3136)
    upsert_type(
        &mut ctx.sde,
        996_001,
        543,
        6,
        "Test Mackinaw",
        28_000.0,
        500.0,
        1,
        &[(3136, 45_000.0)],
    );
    upsert_type(
        &mut ctx.sde,
        996_002,
        464,
        7,
        "Ice Harvester II",
        5.0,
        0.0,
        1,
        &[(77, 1_000.0), (54, 12_000.0), (3154, 34.0), (780, 1.0)],
    );
    upsert_type(
        &mut ctx.sde,
        996_003,
        54,
        7,
        "Miner I",
        5.0,
        0.0,
        1,
        &[(77, 50.0), (54, 10_000.0)],
    );
    upsert_type(&mut ctx.sde, 996_004, 465, 25, "Clear Icicle", 1_000.0, 0.0, 1, &[]);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(ship_id, 996_001, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            ice_mod_id,
            make_item(ice_mod_id, 996_002, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            ore_mod_id,
            make_item(ore_mod_id, 996_003, char_id, ship_id, InventoryFlag::HiSlot1, 1),
        );
        snap.items.insert(
            1_090_000_004,
            make_item(1_090_000_004, 996_002, char_id, ship_id, InventoryFlag::Cargo, 1),
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(
            make_ship_ball(ship_id, 996_001, DVec3::ZERO),
            CombatState::default(),
        )
        .await;
    // Spawn ice asteroid directly in ballpark (without snap.asteroids entry, belt_id = 77)
    let _ = sys_handle
        .spawn_asteroid(
            AsteroidState {
                asteroid_id: ice_ast_id,
                belt_id: 77,
                type_id: 996_004,
                initial_units: 10,
                remaining_units: 10,
                unit_volume: 1_000.0,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: ice_ast_id,
                type_id: 996_004,
                position: DVec3::new(2_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e10,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await;

    // Session without solar_system_id, resolved via bound_entry for "beyonce"
    let mut sess = make_session(char_id, corp_id, None, Some(ship_id));
    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: 701,
        user_id: 11,
        character_id: char_id,
        service: "beyonce".into(),
        object_id: sys_id as i64,
        oid_str: "oid:1".into(),
        ref_id: 1,
    };

    // Specifying incompatible ore_mod_id on Ice asteroid -> InvalidMiningModule
    let incompat_err = handle(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdMineRepeatedly",
        &[PyRep::Integer(ice_ast_id), PyRep::Integer(ore_mod_id)],
        Some(&bound),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(incompat_err.code, "InvalidMiningModule");

    // Specifying non-fitted module ID (in cargo) -> InvalidMiningModule
    let unfitted_err = handle(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdMineRepeatedly",
        &[PyRep::Integer(ice_ast_id), PyRep::Integer(1_090_000_004)],
        Some(&bound),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(unfitted_err.code, "InvalidMiningModule");

    // Auto-selects compatible Ice Harvester II and deposits into SpecializedIceHold (181)
    let ok = handle(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdMineRepeatedly",
        &[PyRep::Integer(ice_ast_id)],
        Some(&bound),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        ok.dict_get("flagID").and_then(|v| v.as_i64()),
        Some(InventoryFlag::SpecializedIceHold as i64)
    );
    assert_eq!(ok.dict_get("minedUnits").and_then(|v| v.as_i64()), Some(1));
    // Verify snap.asteroids entry was created because belt_id != 0
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.asteroids.get(&ice_ast_id).map(|a| a.belt_id), Some(77));
    });
}

#[tokio::test]
async fn test_gh1183_ship_cargo_and_mineral_hold_capacity_and_flag_checks() {
    let ctx = test_ctx();
    let station_id = 60_003_760i64;
    let char_id = 97_183_001i64;
    let corp_id = 98_183_001i64;
    let rifter_id = 97_183_100i64;
    let kryos_id = 97_183_101i64;
    let existing_trit_id = 97_183_102i64;
    let big_comp_ore_id = 97_183_103i64;
    let small_comp_ore_id = 97_183_104i64;
    let ice_id = 97_183_105i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 97_183;
        ch.corporation_id = corp_id;
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = rifter_id;
        ch.balance = IskCents::from_f64_isk(100_000_000.0);
        snap.characters.insert(char_id, ch);

        let mut rifter = make_item(rifter_id, 587, char_id, station_id, InventoryFlag::Hangar, 1);
        rifter.singleton = true;
        snap.items.insert(rifter_id, rifter);

        let mut kryos = make_item(kryos_id, 654, char_id, station_id, InventoryFlag::Hangar, 1);
        kryos.singleton = true;
        snap.items.insert(kryos_id, kryos);

        // Rifter cargo = 140 m3, pre-fill 4,000 Tritanium (40 m3) -> 100 m3 free.
        snap.items.insert(
            existing_trit_id,
            make_item(existing_trit_id, 34, char_id, rifter_id, InventoryFlag::Cargo, 4_000),
        );
        // 50,000 Compressed Veldspar (50 m3 packed -> 100,000 Tritanium = 1,000 m3 output at 50% yield).
        snap.items.insert(
            big_comp_ore_id,
            make_item(
                big_comp_ore_id,
                62_516,
                char_id,
                station_id,
                InventoryFlag::Hangar,
                50_000,
            ),
        );
        // 4,000 Compressed Veldspar (8,000 Tritanium = 80 m3 output <= 100 m3 free).
        snap.items.insert(
            small_comp_ore_id,
            make_item(
                small_comp_ore_id,
                62_516,
                char_id,
                station_id,
                InventoryFlag::Hangar,
                4_000,
            ),
        );
        // Clear Icicle (outputs Heavy Water etc., group != 18).
        snap.items.insert(
            ice_id,
            make_item(ice_id, 16_262, char_id, station_id, InventoryFlag::Hangar, 1),
        );
    });

    let mut sess = make_session(char_id, corp_id, Some(30_000_142), Some(rifter_id));
    sess.station_id = Some(station_id);
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);

    // 1. 1,000 m3 output into Rifter Cargo (100 m3 free) -> NotEnoughCargoSpace, atomically unchanged.
    let err = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "Reprocess",
        &[
            PyRep::List(vec![PyRep::Integer(big_comp_ore_id)]),
            PyRep::Integer(station_id),
            PyRep::Integer(char_id),
            PyRep::Integer(rifter_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        None,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "NotEnoughCargoSpace");
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items.get(&big_comp_ore_id).map(|i| i.quantity), Some(50_000));
        assert_eq!(
            s.items
                .values()
                .filter(|i| i.location_id == rifter_id && i.flag == InventoryFlag::Cargo)
                .map(|i| i.quantity)
                .sum::<i32>(),
            4_000
        );
        assert_eq!(s.characters[&char_id].balance, bal_before);
    });

    // 2. Disallowed ship flags rejected cleanly (`HiSlot0`, `DroneBay`, `Hangar`, `Locked`, `SpecializedOreHold`).
    for bad_flag in [
        InventoryFlag::HiSlot0,
        InventoryFlag::DroneBay,
        InventoryFlag::Hangar,
        InventoryFlag::Locked,
        InventoryFlag::SpecializedOreHold,
    ] {
        let bad_err = handle(
            &ctx,
            &mut sess,
            "reprocessingSvc",
            "Reprocess",
            &[
                PyRep::List(vec![PyRep::Integer(small_comp_ore_id)]),
                PyRep::Integer(station_id),
                PyRep::Integer(char_id),
                PyRep::Integer(rifter_id),
                PyRep::Integer(bad_flag as i64),
            ],
            None,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(
            bad_err.code, "InvalidOutputLocation",
            "expected InvalidOutputLocation for flag {bad_flag:?}"
        );
    }

    // 3. Rifter has 0 m3 SpecializedMineralHold -> NotEnoughSpecialBaySpace.
    let no_min_hold_err = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "Reprocess",
        &[
            PyRep::List(vec![PyRep::Integer(small_comp_ore_id)]),
            PyRep::Integer(station_id),
            PyRep::Integer(char_id),
            PyRep::Integer(rifter_id),
            PyRep::Integer(InventoryFlag::SpecializedMineralHold as i64),
        ],
        None,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(no_min_hold_err.code, "NotEnoughSpecialBaySpace");

    // 4. Ice products (non-group-18) into Kryos SpecializedMineralHold -> InvalidItemTypeForHold.
    let ice_hold_err = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "Reprocess",
        &[
            PyRep::List(vec![PyRep::Integer(ice_id)]),
            PyRep::Integer(station_id),
            PyRep::Integer(char_id),
            PyRep::Integer(kryos_id),
            PyRep::Integer(InventoryFlag::SpecializedMineralHold as i64),
        ],
        None,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(ice_hold_err.code, "InvalidItemTypeForHold");
    assert_eq!(ctx.db.get_item(ice_id).map(|i| i.quantity), Some(1));

    // 5. Positive Kryos SpecializedMineralHold accepts 1,000 m3 Tritanium (cap 50,000 m3).
    handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "Reprocess",
        &[
            PyRep::List(vec![PyRep::Integer(big_comp_ore_id)]),
            PyRep::Integer(station_id),
            PyRep::Integer(char_id),
            PyRep::Integer(kryos_id),
            PyRep::Integer(InventoryFlag::SpecializedMineralHold as i64),
        ],
        None,
    )
    .await
    .unwrap()
    .unwrap();
    ctx.db.with_snapshot(|s| {
        assert!(!s.items.contains_key(&big_comp_ore_id));
        let kryos_minerals: i32 = s
            .items
            .values()
            .filter(|i| {
                i.location_id == kryos_id
                    && i.flag == InventoryFlag::SpecializedMineralHold
                    && i.type_id == 34
            })
            .map(|i| i.quantity)
            .sum();
        assert!(kryos_minerals >= 100_000, "got {kryos_minerals}");
    });

    // 6. Positive Rifter Cargo accepts 80 m3 Tritanium (within 100 m3 remaining free capacity).
    handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "Reprocess",
        &[
            PyRep::List(vec![PyRep::Integer(small_comp_ore_id)]),
            PyRep::Integer(station_id),
            PyRep::Integer(char_id),
            PyRep::Integer(rifter_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        None,
    )
    .await
    .unwrap()
    .unwrap();
    ctx.db.with_snapshot(|s| {
        assert!(!s.items.contains_key(&small_comp_ore_id));
        let total_trit: i32 = s
            .items
            .values()
            .filter(|i| {
                i.location_id == rifter_id && i.flag == InventoryFlag::Cargo && i.type_id == 34
            })
            .map(|i| i.quantity)
            .sum();
        assert_eq!(total_trit, 12_000);
    });
}

#[tokio::test]
async fn test_gh1183_station_container_capacity_lock_password_and_type_checks() {
    use crate::services::inventory_fitting::fitting_containers_ships::write_container_ship_state;

    let ctx = test_ctx();
    let station_id = 60_003_760i64;
    let char_id = 97_183_201i64;
    let other_char_id = 97_183_202i64;
    let corp_id = 98_183_201i64;
    let ship_id = 97_183_210i64;
    let small_cont_id = 97_183_211i64;
    let alsc_cont_id = 97_183_212i64;
    let mod_id = 97_183_213i64;
    let bpo_id = 97_183_214i64;
    let wrap_id = 97_183_215i64;
    let hangar_cont_0m3_id = 97_183_216i64;
    let depot_id = 97_183_217i64;
    let packaged_cont_id = 97_183_218i64;
    let foreign_cont_id = 97_183_219i64;
    let cont_in_ship_id = 97_183_220i64;
    let ore_big_id = 97_183_230i64;
    let ore_batch_a = 97_183_231i64;
    let ore_batch_b = 97_183_232i64;
    let ore_fit_id = 97_183_233i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 97_184;
        ch.corporation_id = corp_id;
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        ch.balance = IskCents::from_f64_isk(100_000_000.0);
        snap.characters.insert(char_id, ch);

        let mut ship = make_item(ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1);
        ship.singleton = true;
        snap.items.insert(ship_id, ship);

        let mut small_cont =
            make_item(small_cont_id, 3297, char_id, station_id, InventoryFlag::Hangar, 1);
        small_cont.singleton = true;
        snap.items.insert(small_cont_id, small_cont);
        // 2,000 Tritanium = 20 m3 -> 100 m3 free in 120 m3 Small Standard Container.
        snap.items.insert(
            97_183_221,
            make_item(97_183_221, 34, char_id, small_cont_id, InventoryFlag::Hangar, 2_000),
        );

        let mut alsc =
            make_item(alsc_cont_id, 17_363, char_id, station_id, InventoryFlag::Hangar, 1);
        alsc.singleton = true;
        snap.items.insert(alsc_cont_id, alsc);

        let mut mod_it = make_item(mod_id, 2048, char_id, station_id, InventoryFlag::Hangar, 1);
        mod_it.singleton = true;
        snap.items.insert(mod_id, mod_it);

        let mut bpo_it = make_item(bpo_id, 691, char_id, station_id, InventoryFlag::Hangar, 1);
        bpo_it.singleton = true;
        snap.items.insert(bpo_id, bpo_it);

        let mut wrap_it = make_item(wrap_id, 3468, char_id, station_id, InventoryFlag::Hangar, 1);
        wrap_it.singleton = true;
        snap.items.insert(wrap_id, wrap_it);

        let mut hc_0m3 = make_item(
            hangar_cont_0m3_id,
            41_567,
            char_id,
            station_id,
            InventoryFlag::Hangar,
            1,
        );
        hc_0m3.singleton = true;
        snap.items.insert(hangar_cont_0m3_id, hc_0m3);

        let mut depot_it =
            make_item(depot_id, 33_474, char_id, station_id, InventoryFlag::Hangar, 1);
        depot_it.singleton = true;
        snap.items.insert(depot_id, depot_it);

        // Packaged (singleton: false) container.
        snap.items.insert(
            packaged_cont_id,
            make_item(packaged_cont_id, 3297, char_id, station_id, InventoryFlag::Hangar, 1),
        );

        let mut foreign_cont = make_item(
            foreign_cont_id,
            3297,
            other_char_id,
            station_id,
            InventoryFlag::Hangar,
            1,
        );
        foreign_cont.singleton = true;
        snap.items.insert(foreign_cont_id, foreign_cont);

        let mut in_ship_cont =
            make_item(cont_in_ship_id, 3297, char_id, ship_id, InventoryFlag::Cargo, 1);
        in_ship_cont.singleton = true;
        snap.items.insert(cont_in_ship_id, in_ship_cont);

        snap.items.insert(
            ore_big_id,
            make_item(ore_big_id, 62_516, char_id, station_id, InventoryFlag::Hangar, 50_000),
        );
        snap.items.insert(
            ore_batch_a,
            make_item(ore_batch_a, 62_516, char_id, station_id, InventoryFlag::Hangar, 3_500),
        );
        snap.items.insert(
            ore_batch_b,
            make_item(ore_batch_b, 62_516, char_id, station_id, InventoryFlag::Hangar, 3_500),
        );
        snap.items.insert(
            ore_fit_id,
            make_item(ore_fit_id, 62_516, char_id, station_id, InventoryFlag::Hangar, 2_000),
        );
    });

    let mut sess = make_session(char_id, corp_id, Some(30_000_142), Some(ship_id));
    sess.station_id = Some(station_id);

    // 1. 100 m3 free container receiving 1,000 m3 output -> CapacityExceeded, atomic rollback.
    let err_cap = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "Reprocess",
        &[
            PyRep::List(vec![PyRep::Integer(ore_big_id)]),
            PyRep::Integer(station_id),
            PyRep::Integer(char_id),
            PyRep::Integer(small_cont_id),
            PyRep::Integer(InventoryFlag::Hangar as i64),
        ],
        None,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_cap.code, "CapacityExceeded");
    assert_eq!(ctx.db.get_item(ore_big_id).map(|i| i.quantity), Some(50_000));

    // 2. Cumulative batch overflow: each stack outputs 70 m3 (<= 100 m3), together 140 m3 (> 100 m3).
    let err_batch = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "Reprocess",
        &[
            PyRep::List(vec![PyRep::Integer(ore_batch_a), PyRep::Integer(ore_batch_b)]),
            PyRep::Integer(station_id),
            PyRep::Integer(char_id),
            PyRep::Integer(small_cont_id),
            PyRep::Integer(InventoryFlag::Hangar as i64),
        ],
        None,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_batch.code, "CapacityExceeded");
    assert_eq!(ctx.db.get_item(ore_batch_a).map(|i| i.quantity), Some(3_500));
    assert_eq!(ctx.db.get_item(ore_batch_b).map(|i| i.quantity), Some(3_500));

    // 3. Invalid non-container targets, packaged container, nested-in-ship container, and invalid flags.
    for (loc, flag, exp_code) in [
        (mod_id, InventoryFlag::Hangar, "InvalidOutputLocation"),
        (bpo_id, InventoryFlag::Hangar, "InvalidOutputLocation"),
        (wrap_id, InventoryFlag::Hangar, "InvalidOutputLocation"),
        (hangar_cont_0m3_id, InventoryFlag::Hangar, "InvalidOutputLocation"),
        (depot_id, InventoryFlag::Hangar, "InvalidOutputLocation"),
        (packaged_cont_id, InventoryFlag::Hangar, "InvalidOutputLocation"),
        (cont_in_ship_id, InventoryFlag::Hangar, "InvalidOutputLocation"),
        (foreign_cont_id, InventoryFlag::Hangar, "OwnershipOrLocationViolation"),
        (small_cont_id, InventoryFlag::HiSlot0, "InvalidOutputLocation"),
        (small_cont_id, InventoryFlag::Cargo, "InvalidOutputLocation"),
        (small_cont_id, InventoryFlag::Locked, "InvalidOutputLocation"),
    ] {
        let e = handle(
            &ctx,
            &mut sess,
            "reprocessingSvc",
            "Reprocess",
            &[
                PyRep::List(vec![PyRep::Integer(ore_fit_id)]),
                PyRep::Integer(station_id),
                PyRep::Integer(char_id),
                PyRep::Integer(loc),
                PyRep::Integer(flag as i64),
            ],
            None,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(e.code, exp_code, "loc={loc} flag={flag:?}");
    }

    // 4. ALSC lock (`alsc_locked_items`) -> ItemLocked.
    write_container_ship_state(&ctx, |st| {
        st.alsc_locked_items.insert(alsc_cont_id);
    });
    let err_lock = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "Reprocess",
        &[
            PyRep::List(vec![PyRep::Integer(ore_fit_id)]),
            PyRep::Integer(station_id),
            PyRep::Integer(char_id),
            PyRep::Integer(alsc_cont_id),
            PyRep::Integer(InventoryFlag::Hangar as i64),
        ],
        None,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_lock.code, "ItemLocked");

    // 5. ALSC password -> ALSCPasswordRequired until unlocked for character.
    write_container_ship_state(&ctx, |st| {
        st.alsc_locked_items.remove(&alsc_cont_id);
        st.container_passwords.insert(alsc_cont_id, "pw123".into());
    });
    let err_pw = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "Reprocess",
        &[
            PyRep::List(vec![PyRep::Integer(ore_fit_id)]),
            PyRep::Integer(station_id),
            PyRep::Integer(char_id),
            PyRep::Integer(alsc_cont_id),
            PyRep::Integer(InventoryFlag::Hangar as i64),
        ],
        None,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_pw.code, "ALSCPasswordRequired");

    // 6. Unlocked ALSC container accepts 40 m3 Tritanium (<= 120 m3 capacity).
    write_container_ship_state(&ctx, |st| {
        st.unlocked_containers.insert((alsc_cont_id, char_id));
    });
    handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "Reprocess",
        &[
            PyRep::List(vec![PyRep::Integer(ore_fit_id)]),
            PyRep::Integer(station_id),
            PyRep::Integer(char_id),
            PyRep::Integer(alsc_cont_id),
            PyRep::Integer(InventoryFlag::Hangar as i64),
        ],
        None,
    )
    .await
    .unwrap()
    .unwrap();
    ctx.db.with_snapshot(|s| {
        assert!(!s.items.contains_key(&ore_fit_id));
        let in_alsc: i32 = s
            .items
            .values()
            .filter(|i| i.location_id == alsc_cont_id && i.type_id == 34)
            .map(|i| i.quantity)
            .sum();
        assert!(in_alsc >= 4_000);
    });
}

#[tokio::test]
async fn test_gh1183_corp_office_and_division_acl_checks() {
    use eve_db::corp_roles;

    let ctx = test_ctx();
    let station_id = 60_003_760i64;
    let ceo_id = 97_183_301i64;
    let member_id = 97_183_302i64;
    let corp_id = 98_183_300i64;
    let office_id = 97_183_310i64;
    let corp_cont_id = 97_183_311i64;
    let small_corp_cont_id = 97_183_312i64;
    let ore1_id = 97_183_320i64;
    let ore2_id = 97_183_321i64;
    let ore_big_id = 97_183_322i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut mem_ch = snap.characters.values().next().cloned().expect("seed char");
        mem_ch.character_id = member_id;
        mem_ch.user_id = 97_185;
        mem_ch.corporation_id = corp_id;
        mem_ch.station_id = Some(station_id);
        mem_ch.solar_system_id = 30_000_142;
        mem_ch.balance = IskCents::from_f64_isk(100_000_000.0);
        snap.characters.insert(member_id, mem_ch);

        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Corp Reproc ACL".into();
        corp.ticker = "CRAC".into();
        corp.ceo_id = ceo_id;
        corp.members.insert(ceo_id);
        corp.members.insert(member_id);
        // Give member access ONLY to CorpSAG1 initially (not CorpSAG2).
        corp.member_roles
            .insert(member_id, corp_roles::ROLE_HANGAR_CAN_TAKE[0]);
        corp.set_division_balance(1, IskCents::from_f64_isk(50_000_000.0));
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);

        snap.items.insert(
            ore1_id,
            make_item(ore1_id, 1230, member_id, station_id, InventoryFlag::Hangar, 200),
        );
        snap.items.insert(
            ore2_id,
            make_item(ore2_id, 1230, member_id, station_id, InventoryFlag::Hangar, 200),
        );
        snap.items.insert(
            ore_big_id,
            make_item(
                ore_big_id,
                62_516,
                member_id,
                station_id,
                InventoryFlag::Hangar,
                50_000,
            ),
        );
    });

    let mut sess = make_session(member_id, corp_id, Some(30_000_142), None);
    sess.station_id = Some(station_id);

    // 1. No rented office at station -> NoOfficeAtStation.
    let no_office_err = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "Reprocess",
        &[
            PyRep::List(vec![PyRep::Integer(ore1_id)]),
            PyRep::Integer(station_id),
            PyRep::Integer(corp_id),
            PyRep::Integer(station_id),
            PyRep::Integer(InventoryFlag::CorpSAG2 as i64),
        ],
        None,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(no_office_err.code, "NoOfficeAtStation");

    // Seed rented office & two corp containers in CorpSAG2.
    ctx.db.with_mut_snapshot(|snap| {
        let mut office = make_item(office_id, 27, corp_id, station_id, InventoryFlag::Hangar, 1);
        office.singleton = true;
        snap.items.insert(office_id, office);
        snap.corp_governance
            .entry(corp_id)
            .or_default()
            .offices
            .insert(station_id, office_id);

        let mut big_cont = make_item(
            corp_cont_id,
            17_366,
            corp_id,
            office_id,
            InventoryFlag::CorpSAG2,
            1,
        );
        big_cont.singleton = true;
        snap.items.insert(corp_cont_id, big_cont);

        let mut sm_cont = make_item(
            small_corp_cont_id,
            3297,
            corp_id,
            office_id,
            InventoryFlag::CorpSAG2,
            1,
        );
        sm_cont.singleton = true;
        snap.items.insert(small_corp_cont_id, sm_cont);
    });

    // 2. Member lacks CorpSAG2 query/take role -> CrpAccessDenied on office & corp container.
    let denied_office = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "Reprocess",
        &[
            PyRep::List(vec![PyRep::Integer(ore1_id)]),
            PyRep::Integer(station_id),
            PyRep::Integer(corp_id),
            PyRep::Integer(office_id),
            PyRep::Integer(InventoryFlag::CorpSAG2 as i64),
        ],
        None,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(denied_office.code, "CrpAccessDenied");

    let denied_cont = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "Reprocess",
        &[
            PyRep::List(vec![PyRep::Integer(ore1_id)]),
            PyRep::Integer(station_id),
            PyRep::Integer(corp_id),
            PyRep::Integer(corp_cont_id),
            PyRep::Integer(InventoryFlag::Hangar as i64),
        ],
        None,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(denied_cont.code, "CrpAccessDenied");

    // 3. Grant ROLE_HANGAR_CAN_QUERY[1] for CorpSAG2 -> office & corp container succeed!
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(
                member_id,
                corp_roles::ROLE_HANGAR_CAN_TAKE[0] | corp_roles::ROLE_HANGAR_CAN_QUERY[1],
            );
        }
    });

    // Corp container overflow still enforces capacity -> CapacityExceeded.
    let corp_cap_err = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "Reprocess",
        &[
            PyRep::List(vec![PyRep::Integer(ore_big_id)]),
            PyRep::Integer(station_id),
            PyRep::Integer(corp_id),
            PyRep::Integer(small_corp_cont_id),
            PyRep::Integer(InventoryFlag::Hangar as i64),
        ],
        None,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(corp_cap_err.code, "CapacityExceeded");

    // Deposit into CorpSAG2 at `office_id` succeeds.
    handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "Reprocess",
        &[
            PyRep::List(vec![PyRep::Integer(ore1_id)]),
            PyRep::Integer(station_id),
            PyRep::Integer(corp_id),
            PyRep::Integer(office_id),
            PyRep::Integer(InventoryFlag::CorpSAG2 as i64),
        ],
        None,
    )
    .await
    .unwrap()
    .unwrap();

    // Deposit into large corp container `corp_cont_id` inside CorpSAG2 succeeds.
    handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "Reprocess",
        &[
            PyRep::List(vec![PyRep::Integer(ore2_id)]),
            PyRep::Integer(station_id),
            PyRep::Integer(corp_id),
            PyRep::Integer(corp_cont_id),
            PyRep::Integer(InventoryFlag::Hangar as i64),
        ],
        None,
    )
    .await
    .unwrap()
    .unwrap();

    ctx.db.with_snapshot(|s| {
        assert!(!s.items.contains_key(&ore1_id));
        assert!(!s.items.contains_key(&ore2_id));
        assert!(s.items.values().any(|i| {
            i.owner_id == corp_id
                && i.location_id == office_id
                && i.flag == InventoryFlag::CorpSAG2
                && i.type_id == 34
                && i.quantity > 0
        }));
        assert!(s.items.values().any(|i| {
            i.owner_id == corp_id
                && i.location_id == corp_cont_id
                && i.flag == InventoryFlag::Hangar
                && i.type_id == 34
                && i.quantity > 0
        }));
    });
}

