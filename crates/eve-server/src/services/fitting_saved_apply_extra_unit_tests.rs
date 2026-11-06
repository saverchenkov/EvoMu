//! Extra branch-coverage and mutation-killing unit tests for `fitting_saved_apply`.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_type;
use crate::services::fitting_saved_apply::{
    dna_to_fitting, fitting_to_dna, get_ship_hold_capacity, handle_fit_ships,
    is_item_allowed_in_ship_hold, saved_fitting_to_dna,
};
use eve_db::{
    InventoryFlag, InventoryItemRecord, SavedFittingRecord, TrainedSkillRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{InvType, SdeRegistry};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_custom_type(
    type_id: u32,
    group_id: u32,
    category_id: u32,
    volume: f64,
    capacity: f64,
    attrs: &[(u16, f64)],
    effects: &[u16],
) -> InvType {
    let mut t = make_fighter_type(type_id, group_id, category_id, volume, attrs.to_vec());
    t.capacity = capacity;
    t.effect_ids = effects.to_vec();
    t
}

async fn call_fit_ships(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    handle_fit_ships(ctx, session, args, kwargs)
        .await
        .expect("handle_fit_ships always returns Some")
}

#[test]
fn test_all_hold_capacity_and_allowed_item_branches() {
    let base_ship = make_custom_type(990_001, 25, 6, 10_000.0, 220.0, &[], &[]);

    // Flag 5: Cargo
    assert_eq!(get_ship_hold_capacity(5, &base_ship, Some(333.0)), 333.0);
    assert_eq!(get_ship_hold_capacity(5, &base_ship, None), 220.0);
    assert!(is_item_allowed_in_ship_hold(5, &base_ship, &base_ship));

    // Default / unknown flag
    assert_eq!(get_ship_hold_capacity(999, &base_ship, None), 0.0);
    assert!(is_item_allowed_in_ship_hold(999, &base_ship, &base_ship));

    // Flag 148 defaults to 5000.0 when attr 1646 is absent, or uses attr 1646 when present
    assert_eq!(get_ship_hold_capacity(148, &base_ship, None), 5_000.0);
    let ship_148 = make_custom_type(990_002, 25, 6, 10_000.0, 200.0, &[(1646, 1_234.0)], &[]);
    assert_eq!(get_ship_hold_capacity(148, &ship_148, None), 1_234.0);

    // Fallbacks in get_ship_hold_capacity
    let ship_primary = make_custom_type(
        990_003,
        25,
        6,
        10_000.0,
        200.0,
        &[
            (eve_sde::attr_ids::DRONE_CAPACITY, 75.0),
            (2055, 2_500.0),
            (1549, 600.0),
            (1556, 7_000.0),
            (1770, 8_000.0),
            (1558, 9_000.0),
            (1557, 10_000.0),
            (3020, 11_000.0),
            (5325, 12_000.0),
            (3136, 13_000.0),
            (5646, 14_000.0),
            (3227, 15_000.0),
            (1573, 16_000.0),
            (1653, 17_000.0),
        ],
        &[],
    );
    assert_eq!(get_ship_hold_capacity(87, &ship_primary, None), 75.0);
    assert_eq!(get_ship_hold_capacity(158, &ship_primary, None), 2_500.0);
    assert_eq!(get_ship_hold_capacity(133, &ship_primary, None), 600.0);
    assert_eq!(get_ship_hold_capacity(134, &ship_primary, None), 7_000.0);
    assert_eq!(get_ship_hold_capacity(151, &ship_primary, None), 7_000.0);
    assert_eq!(get_ship_hold_capacity(136, &ship_primary, None), 9_000.0);
    assert_eq!(get_ship_hold_capacity(135, &ship_primary, None), 10_000.0);
    assert_eq!(get_ship_hold_capacity(177, &ship_primary, None), 11_000.0);
    assert_eq!(get_ship_hold_capacity(179, &ship_primary, None), 11_000.0);
    assert_eq!(get_ship_hold_capacity(181, &ship_primary, None), 13_000.0);
    assert_eq!(get_ship_hold_capacity(150, &ship_primary, None), 14_000.0);
    assert_eq!(get_ship_hold_capacity(185, &ship_primary, None), 14_000.0);
    assert_eq!(get_ship_hold_capacity(182, &ship_primary, None), 14_000.0);
    assert_eq!(get_ship_hold_capacity(143, &ship_primary, None), 16_000.0);
    assert_eq!(get_ship_hold_capacity(149, &ship_primary, None), 17_000.0);

    // Secondary attribute fallbacks when primary is absent
    let ship_fallback = make_custom_type(
        990_004,
        25,
        6,
        10_000.0,
        200.0,
        &[(1770, 8_100.0), (5325, 12_100.0), (3227, 15_100.0)],
        &[],
    );
    assert_eq!(get_ship_hold_capacity(134, &ship_fallback, None), 8_100.0);
    assert_eq!(get_ship_hold_capacity(151, &ship_fallback, None), 8_100.0);
    assert_eq!(get_ship_hold_capacity(177, &ship_fallback, None), 12_100.0);
    assert_eq!(get_ship_hold_capacity(179, &ship_fallback, None), 12_100.0);
    assert_eq!(get_ship_hold_capacity(182, &ship_fallback, None), 15_100.0);

    let ship_1556_only = make_custom_type(990_005, 25, 6, 10_000.0, 200.0, &[(1556, 4_200.0)], &[]);
    assert_eq!(get_ship_hold_capacity(135, &ship_1556_only, None), 4_200.0);
    assert_eq!(get_ship_hold_capacity(181, &ship_1556_only, None), 4_200.0);

    // is_item_allowed_in_ship_hold exhaustive checks
    let fighter = make_custom_type(990_010, 1652, 87, 100.0, 0.0, &[], &[]);
    assert!(is_item_allowed_in_ship_hold(158, &fighter, &base_ship));
    assert!(!is_item_allowed_in_ship_hold(158, &base_ship, &base_ship));

    for grp in [423, 497, 1136] {
        let fuel = make_custom_type(990_011, grp, 4, 5.0, 0.0, &[], &[]);
        assert!(is_item_allowed_in_ship_hold(133, &fuel, &base_ship));
    }
    assert!(!is_item_allowed_in_ship_hold(133, &base_ship, &base_ship));

    let ore = make_custom_type(990_012, 450, 25, 1.0, 0.0, &[], &[]);
    let gas_711 = make_custom_type(990_013, 711, 4, 1.0, 0.0, &[], &[]);
    let gas_4168 = make_custom_type(990_014, 4168, 4, 1.0, 0.0, &[], &[]);
    assert!(is_item_allowed_in_ship_hold(134, &ore, &base_ship));
    assert!(is_item_allowed_in_ship_hold(151, &gas_711, &base_ship));
    assert!(is_item_allowed_in_ship_hold(134, &gas_4168, &base_ship));
    assert!(!is_item_allowed_in_ship_hold(134, &base_ship, &base_ship));

    assert!(is_item_allowed_in_ship_hold(135, &gas_711, &base_ship));
    assert!(is_item_allowed_in_ship_hold(135, &gas_4168, &base_ship));
    assert!(!is_item_allowed_in_ship_hold(135, &ore, &base_ship));

    let mineral = make_custom_type(990_015, 18, 4, 0.01, 0.0, &[], &[]);
    assert!(is_item_allowed_in_ship_hold(136, &mineral, &base_ship));
    assert!(!is_item_allowed_in_ship_hold(136, &ore, &base_ship));

    let charge = make_custom_type(990_016, 83, 8, 0.01, 0.0, &[], &[]);
    assert!(is_item_allowed_in_ship_hold(143, &charge, &base_ship));
    assert!(!is_item_allowed_in_ship_hold(143, &ore, &base_ship));

    let cmd_center = make_custom_type(990_017, 1027, 41, 1000.0, 0.0, &[], &[]);
    assert!(is_item_allowed_in_ship_hold(148, &cmd_center, &base_ship));
    assert!(!is_item_allowed_in_ship_hold(148, &ore, &base_ship));

    let pi_42 = make_custom_type(990_018, 1032, 42, 0.5, 0.0, &[], &[]);
    let pi_43 = make_custom_type(990_019, 1033, 43, 0.5, 0.0, &[], &[]);
    assert!(is_item_allowed_in_ship_hold(149, &pi_42, &base_ship));
    assert!(is_item_allowed_in_ship_hold(149, &pi_43, &base_ship));
    assert!(!is_item_allowed_in_ship_hold(149, &ore, &base_ship));

    let grp_1246 = make_custom_type(990_020, 1246, 6, 2500.0, 0.0, &[], &[]);
    assert!(is_item_allowed_in_ship_hold(177, &grp_1246, &base_ship));
    for grp in [25, 324, 830, 831, 834, 893, 1283, 1527, 2001] {
        let frig = make_custom_type(990_021, grp, 6, 2500.0, 0.0, &[], &[]);
        assert!(is_item_allowed_in_ship_hold(179, &frig, &ship_primary));
        assert!(!is_item_allowed_in_ship_hold(179, &frig, &base_ship));
    }

    let ice = make_custom_type(990_022, 465, 25, 1000.0, 0.0, &[], &[]);
    assert!(is_item_allowed_in_ship_hold(181, &ice, &base_ship));
    assert!(!is_item_allowed_in_ship_hold(181, &ore, &base_ship));

    for cat in [23, 39, 40, 41, 42, 43, 46, 65, 66, 2143] {
        let infra = make_custom_type(990_023, 1, cat, 10.0, 0.0, &[], &[]);
        assert!(is_item_allowed_in_ship_hold(150, &infra, &base_ship));
        assert!(is_item_allowed_in_ship_hold(185, &infra, &base_ship));
        assert!(is_item_allowed_in_ship_hold(182, &infra, &ship_primary));
    }
    let grp_4729 = make_custom_type(990_024, 4729, 4, 10.0, 0.0, &[], &[]);
    assert!(is_item_allowed_in_ship_hold(150, &grp_4729, &base_ship));
    assert!(is_item_allowed_in_ship_hold(182, &grp_4729, &ship_primary));
    assert!(!is_item_allowed_in_ship_hold(150, &ore, &base_ship));
    assert!(!is_item_allowed_in_ship_hold(182, &ore, &ship_primary));
    assert!(is_item_allowed_in_ship_hold(182, &ore, &base_ship));
    assert!(!is_item_allowed_in_ship_hold(182, &mineral, &base_ship));
}

#[test]
fn test_dna_encoding_and_decoding_edge_cases() {
    let dna = fitting_to_dna(587, &[(100, -5), (100, 0), (200, 2), (300, 1), (200, 3)]);
    assert_eq!(dna, "587:200;5:300;1::");

    let saved = SavedFittingRecord {
        fitting_id: 1,
        owner_id: 1,
        ship_type_id: 621,
        name: "Caracal".into(),
        description: String::new(),
        items: vec![(200, InventoryFlag::HiSlot0, 1), (200, InventoryFlag::HiSlot1, 1)],
    };
    assert_eq!(saved_fitting_to_dna(&saved), "621:200;2::");

    // Token without semicolon defaults to quantity 1; zero/negative qty skipped
    let (ship_tid, items) =
        dna_to_fitting("  621: 484 : 178 ; 20 : 999 ; 0 : 888 ; -2 :: ").expect("valid DNA");
    assert_eq!(ship_tid, 621);
    assert_eq!(items, vec![(484, 1), (178, 20)]);

    assert!(dna_to_fitting("   ").is_err());
    assert!(dna_to_fitting("587:bad_type;1::").is_err());
    assert!(dna_to_fitting("587:484;bad_qty::").is_err());
}

#[tokio::test]
async fn test_handle_fit_ships_validation_and_error_branches() {
    let mut sde = (*shared_sde()).clone();
    let no_fit_station_id = 60_099_999_i64;
    let mut no_fit_st = sde.stations.values().next().expect("seed station").clone();
    no_fit_st.station_id = no_fit_station_id;
    no_fit_st.operation_id = 999_999; // Operation without fitting service (17)
    sde.stations.insert(no_fit_station_id, no_fit_st);

    // Custom types for slot, rig, hardpoint, maxGroup/maxType, drone/fighter/specialized holds
    let custom_ship_tid = 991_000_u32;
    sde.types.insert(
        custom_ship_tid,
        make_custom_type(
            custom_ship_tid,
            25,
            6,
            10_000.0,
            50.0, // cargo cap = 50
            &[
                (eve_sde::attr_ids::LOW_SLOTS, 1.0),
                (eve_sde::attr_ids::MED_SLOTS, 1.0),
                (eve_sde::attr_ids::HI_SLOTS, 2.0),
                (eve_sde::attr_ids::RIG_SLOTS, 1.0),
                (eve_sde::attr_ids::RIG_SIZE, 1.0),
                (eve_sde::attr_ids::UPGRADE_CAPACITY, 400.0),
                (eve_sde::attr_ids::TURRET_SLOTS_LEFT, 1.0),
                (eve_sde::attr_ids::LAUNCHER_SLOTS_LEFT, 1.0),
                (eve_sde::attr_ids::DRONE_CAPACITY, 10.0),
                (1549, 20.0), // FuelBay (133) capacity = 20
            ],
            &[],
        ),
    );
    // Medium rig (rigSize = 2) -> triggers RigSizeMismatch on size 1 hull
    let med_rig_tid = 991_001_u32;
    sde.types.insert(
        med_rig_tid,
        make_custom_type(
            med_rig_tid,
            773,
            7,
            5.0,
            0.0,
            &[(eve_sde::attr_ids::RIG_SIZE, 2.0), (eve_sde::attr_ids::UPGRADE_COST, 100.0)],
            &[2663],
        ),
    );
    // Expensive small rig (upgradeCost = 500 > 400) -> UpgradeCapacityExceeded
    let heavy_rig_tid = 991_002_u32;
    sde.types.insert(
        heavy_rig_tid,
        make_custom_type(
            heavy_rig_tid,
            773,
            7,
            5.0,
            0.0,
            &[(eve_sde::attr_ids::RIG_SIZE, 1.0), (eve_sde::attr_ids::UPGRADE_COST, 500.0)],
            &[2663],
        ),
    );
    // Turret module (effect 12 hiPower + 42 turretFitted)
    let turret_tid = 991_003_u32;
    sde.types.insert(
        turret_tid,
        make_custom_type(turret_tid, 55, 7, 5.0, 0.0, &[], &[12, 42]),
    );
    // Launcher module (effect 12 hiPower + 40 launcherFitted)
    let launcher_tid = 991_004_u32;
    sde.types.insert(
        launcher_tid,
        make_custom_type(launcher_tid, 56, 7, 5.0, 0.0, &[], &[12, 40]),
    );
    // Module with maxGroupFitted = 1 (attr 1544)
    let max_grp_tid = 991_005_u32;
    sde.types.insert(
        max_grp_tid,
        make_custom_type(max_grp_tid, 60, 7, 5.0, 0.0, &[(1544, 1.0)], &[12]),
    );
    // Module with maxTypeFitted = 1 (attr 2431)
    let max_type_tid = 991_006_u32;
    sde.types.insert(
        max_type_tid,
        make_custom_type(max_type_tid, 61, 7, 5.0, 0.0, &[(2431, 1.0)], &[12]),
    );
    // Drone (category 18, vol 15 > drone cap 10)
    let big_drone_tid = 991_007_u32;
    sde.types.insert(
        big_drone_tid,
        make_custom_type(big_drone_tid, 100, 18, 15.0, 0.0, &[], &[]),
    );
    // Fuel block (group 1136, vol 5)
    let fuel_block_tid = 991_008_u32;
    sde.types.insert(
        fuel_block_tid,
        make_custom_type(fuel_block_tid, 1136, 4, 5.0, 0.0, &[], &[]),
    );
    // Module requiring skill 3300 level 5 and huge CPU (attr 50)
    let skill_mod_tid = 991_009_u32;
    sde.types.insert(
        skill_mod_tid,
        make_custom_type(
            skill_mod_tid,
            62,
            7,
            5.0,
            0.0,
            &[(182, 3300.0), (277, 5.0)],
            &[11], // loPower
        ),
    );

    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_100_001_i64;
    let station_id = 60_003_760_i64;
    let ship_id = 96_100_100_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: custom_ship_tid,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut session = SessionState::default();
    let empty_kw = HashMap::new();

    // 1. Unauthenticated session -> error
    assert!(call_fit_ships(&ctx, &mut session, &[], &empty_kw).await.is_err());

    // 2. Not docked -> error
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.ship_id = Some(ship_id);
    assert!(call_fit_ships(&ctx, &mut session, &[], &empty_kw).await.is_err());

    // 3. Station without fitting service 17 -> error
    session.station_id = Some(no_fit_station_id);
    assert!(call_fit_ships(&ctx, &mut session, &[], &empty_kw).await.is_err());
    session.station_id = Some(station_id);

    // 4. Invalid flag in fitData (< 0 and disallowed flag 4)
    let bad_neg_flag = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(turret_tid as i64),
        PyRep::Integer(-1),
        PyRep::Integer(1),
    ])]);
    assert!(call_fit_ships(&ctx, &mut session, &[bad_neg_flag], &empty_kw).await.is_err());

    let bad_hangar_flag = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(turret_tid as i64),
        PyRep::Integer(4),
        PyRep::Integer(1),
    ])]);
    assert!(call_fit_ships(&ctx, &mut session, &[bad_hangar_flag], &empty_kw).await.is_err());

    // 5. Invalid quantity > i32::MAX
    let bad_qty_fit = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(turret_tid as i64),
        PyRep::Integer(5),
        PyRep::Integer(i64::from(i32::MAX) + 10),
    ])]);
    assert!(call_fit_ships(&ctx, &mut session, &[bad_qty_fit], &empty_kw).await.is_err());

    // 6. ShipNotFound & Locked ship & ShipTypeMismatch
    let missing_ship_args = [PyRep::List(vec![PyRep::Integer(999_999_999)])];
    assert!(call_fit_ships(&ctx, &mut session, &missing_ship_args, &empty_kw).await.is_err());

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().flag = InventoryFlag::Locked;
    });
    assert!(call_fit_ships(&ctx, &mut session, &[], &empty_kw).await.is_err());
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().flag = InventoryFlag::Hangar;
        snap.saved_fittings.insert(
            700,
            SavedFittingRecord {
                fitting_id: 700,
                owner_id: char_id,
                ship_type_id: 587, // Mismatch with custom_ship_tid
                name: "WrongHull".into(),
                description: String::new(),
                items: vec![(turret_tid, InventoryFlag::HiSlot0, 1)],
            },
        );
    });
    assert!(
        call_fit_ships(&ctx, &mut session, &[PyRep::Integer(700)], &empty_kw)
            .await
            .is_err()
    );

    // 7. Slot checks: duplicate slot, slot index exceeds hull slots (low/med/hi/rig), slot effect mismatch
    for fit_tuples in [
        // Duplicate HiSlot0
        vec![(turret_tid, 27, 1), (turret_tid, 27, 1)],
        // LowSlot1 (12) exceeds 1 low slot
        vec![(skill_mod_tid, 12, 1)],
        // MedSlot1 (20) exceeds 1 med slot
        vec![(turret_tid, 20, 1)],
        // HiSlot2 (29) exceeds 2 hi slots
        vec![(turret_tid, 29, 1)],
        // RigSlot1 (93) exceeds 1 rig slot
        vec![(med_rig_tid, 93, 1)],
        // SlotEffectMismatch: turret in LowSlot0 (11)
        vec![(turret_tid, 11, 1)],
        // RigSizeMismatch: med rig in RigSlot0 (92)
        vec![(med_rig_tid, 92, 1)],
        // UpgradeCapacityExceeded: heavy rig in RigSlot0 (92)
        vec![(heavy_rig_tid, 92, 1)],
        // InsufficientTurretHardpoints: 2 turrets on 1-turret hull
        vec![(turret_tid, 27, 1), (turret_tid, 28, 1)],
        // InsufficientLauncherHardpoints: 2 launchers on 1-launcher hull
        vec![(launcher_tid, 27, 1), (launcher_tid, 28, 1)],
        // MaxGroupFittedExceeded
        vec![(max_grp_tid, 27, 1), (max_grp_tid, 28, 1)],
        // MaxTypeFittedExceeded
        vec![(max_type_tid, 27, 1), (max_type_tid, 28, 1)],
        // Cargo CapacityExceeded (100 * 5m3 = 500 > 50)
        vec![(fuel_block_tid, 5, 100)],
        // ItemNotADrone in DroneBay (87)
        vec![(fuel_block_tid, 87, 1)],
        // DroneBay CapacityExceeded
        vec![(big_drone_tid, 87, 1)],
        // ShipHasNoFighterBay (158)
        vec![(big_drone_tid, 158, 1)],
        // InvalidItemTypeForHold in FuelBay (133)
        vec![(big_drone_tid, 133, 1)],
        // Specialized hold CapacityExceeded in FuelBay (133: 10 * 5m3 = 50 > 20)
        vec![(fuel_block_tid, 133, 10)],
        // Specialized hold ship lacks capacity (136 MineralHold = 0)
        vec![(18, 136, 1)],
    ] {
        let py_list = PyRep::List(
            fit_tuples
                .into_iter()
                .map(|(t, f, q)| {
                    PyRep::Tuple(vec![
                        PyRep::Integer(t as i64),
                        PyRep::Integer(f),
                        PyRep::Integer(q),
                    ])
                })
                .collect(),
        );
        assert!(
            call_fit_ships(&ctx, &mut session, &[py_list], &empty_kw)
                .await
                .is_err()
        );
    }

    // 8. Stack splitting + offline module fitting when character lacks required skill level
    let stack_id = 96_100_300_i64;
    let mod_stack_id = 96_100_301_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            stack_id,
            InventoryItemRecord {
                item_id: stack_id,
                type_id: fuel_block_tid,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_stack_id,
            InventoryItemRecord {
                item_id: mod_stack_id,
                type_id: skill_mod_tid,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 2, // stack of 2 modules -> splits 1 onto ship
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // Train skill 3300 to level 2 (< required level 5 -> module fits offline)
        snap.skills.entry(char_id).or_default().insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 2,
                skill_points: 8_000.0,
            },
        );
    });

    let valid_fit = PyRep::List(vec![
        PyRep::Tuple(vec![
            PyRep::Integer(skill_mod_tid as i64),
            PyRep::Integer(11), // LoSlot0
            PyRep::Integer(1),
        ]),
        PyRep::Tuple(vec![
            PyRep::Integer(fuel_block_tid as i64),
            PyRep::Integer(133), // FuelBay (2 * 5m3 = 10 <= 20m3)
            PyRep::Integer(2),
        ]),
    ]);
    let ok_res = call_fit_ships(&ctx, &mut session, &[valid_fit], &empty_kw)
        .await
        .expect("fit with stack split and offline skill module succeeds");
    assert_eq!(ok_res.dict_get("fittedCount").and_then(|v| v.as_i64()), Some(1));

    ctx.db.with_snapshot(|snap| {
        // Original fuel stack reduced from 10 to 8
        assert_eq!(snap.items.get(&stack_id).map(|i| i.quantity), Some(8));
        // Original module stack reduced from 2 to 1
        assert_eq!(snap.items.get(&mod_stack_id).map(|i| i.quantity), Some(1));
        // Split module fitted in LoSlot0 is offline (false) due to missing skill level
        let fitted_mod = snap
            .items
            .values()
            .find(|i| i.location_id == ship_id && i.flag == InventoryFlag::LoSlot0)
            .expect("fitted module present");
        assert_eq!(snap.module_online_states.get(&fitted_mod.item_id), Some(&false));
    });
}
