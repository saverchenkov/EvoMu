use super::*;
use crate::router::ServerContext;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, PlanetaryColonyRecord,
    TrainedSkillRecord, WorldRepository,
};
use eve_sde::{PlanetSchematicInfo, PlanetSchematicTypeEntry, SdeRegistry};
use std::collections::HashMap;
use std::sync::Arc;

fn make_ctx() -> ServerContext {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.planet_schematics.insert(
        65,
        PlanetSchematicInfo {
            schematic_id: 65,
            name: "Water".into(),
            cycle_time: 1800,
            types: vec![
                PlanetSchematicTypeEntry {
                    type_id: 2268,
                    quantity: 3000,
                    is_input: true,
                },
                PlanetSchematicTypeEntry {
                    type_id: 3645,
                    quantity: 20,
                    is_input: false,
                },
            ],
            pins: vec![2469],
        },
    );

    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(Arc::new(sde), db)
}

#[test]
fn test_cc_levels_pin_classification_and_link_paths() {
    let ctx = make_ctx();
    assert_eq!(cc_level_info(0), (1_675.0, 6_000.0, 0.0));
    assert_eq!(cc_level_info(1), (7_057.0, 9_000.0, 580_000.0));
    assert_eq!(cc_level_info(5), (25_415.0, 19_000.0, 6_310_000.0));
    assert_eq!(cc_upgrade_cost_isk(2, 2), 0.0);
    assert!((cc_upgrade_cost_isk(0, 1) - 580_000.0).abs() < 1e-6);
    assert!((cc_upgrade_cost_isk(1, 3) - (2_710_000.0 - 580_000.0)).abs() < 1e-6);

    assert!(is_launchpad_pin(&ctx.sde, 2256));
    assert!(is_storage_pin(&ctx.sde, 2257));
    assert!(is_command_center_pin(&ctx.sde, 2254));
    assert!(is_storage_or_cc_or_launchpad_pin(&ctx.sde, 2256));
    assert!(!is_storage_or_cc_or_launchpad_pin(&ctx.sde, 2469));

    assert_eq!(extract_pi_pin_id(Some(&PyRep::Integer(42))), 42);
    assert_eq!(
        extract_pi_pin_id(Some(&PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(99)]))),
        99
    );
    assert_eq!(extract_pi_pin_id(None), 0);

    assert_eq!(resolve_pi_pin_stats(&ctx, 2254), (0.0, 0.0, 0.0));
    let (f_cpu, f_pg, f_isk) = resolve_pi_pin_stats(&ctx, 2469);
    assert_eq!((f_cpu, f_pg, f_isk), (200.0, 800.0, 75_000.0));

    let links = vec![
        PiLinkState {
            source_pin_id: 1,
            dest_pin_id: 2,
            level: 0,
            bandwidth: 1250.0,
            ..Default::default()
        },
        PiLinkState {
            source_pin_id: 2,
            dest_pin_id: 3,
            level: 1,
            bandwidth: 2500.0,
            ..Default::default()
        },
    ];
    assert!(has_pi_link_path(&links, &[1, 3], 1000.0).is_ok());
    assert_eq!(
        has_pi_link_path(&links, &[1, 1], 10.0).unwrap_err().code,
        "InvalidTransferSource"
    );
    assert_eq!(
        has_pi_link_path(&links, &[1, 4], 10.0).unwrap_err().code,
        "NoLinkBetweenPins"
    );
    assert_eq!(
        has_pi_link_path(&links, &[1, 3], 1500.0).unwrap_err().code,
        "InsufficientLinkBandwidth"
    );
}

#[test]
fn test_advance_colony_simulation_and_login_sync() {
    let ctx = make_ctx();
    let char_id = 90_000_001;
    let planet_id = 40_009_082;
    let now = ctx.now_unix();

    let mut col = PlanetaryColonyRecord {
        planet_id,
        owner_id: char_id,
        solar_system_id: 30_000_142,
        command_center_level: 3,
        pins_count: 3,
        extracted_commodities: HashMap::from([(2268, 6000)]),
        last_extracted_unix: now - 3600,
    };
    let mut net = PiColonyNetworkState {
        cpu_max: 17_215.0,
        pg_max: 15_000.0,
        pins: vec![
            PiPinState {
                pin_id: 10,
                type_id: 2848, // ECU
                extractor_resource_type_id: Some(2268),
                extractor_cycle_seconds: 1800,
                extractor_head_count: 2,
                extractor_head_radius: 0.02,
                program_install_unix: now - 3600,
                expiry_time_unix: Some(now + 3600),
                ..Default::default()
            },
            PiPinState {
                pin_id: 20,
                type_id: 2257, // Storage Facility
                contents: HashMap::from([(2268, 6000)]),
                ..Default::default()
            },
            PiPinState {
                pin_id: 30,
                type_id: 2469, // Basic Industry Facility
                schematic_id: Some(65),
                ..Default::default()
            },
            PiPinState {
                pin_id: 40,
                type_id: 2256, // Launchpad
                ..Default::default()
            },
        ],
        links: vec![
            PiLinkState {
                source_pin_id: 10,
                dest_pin_id: 20,
                level: 1,
                bandwidth: 2500.0,
                ..Default::default()
            },
            PiLinkState {
                source_pin_id: 20,
                dest_pin_id: 30,
                level: 1,
                bandwidth: 2500.0,
                ..Default::default()
            },
            PiLinkState {
                source_pin_id: 30,
                dest_pin_id: 40,
                level: 1,
                bandwidth: 2500.0,
                ..Default::default()
            },
        ],
        routes: vec![
            PiRouteState {
                route_id: 1,
                path: vec![10, 20],
                commodity_type_id: 2268,
                quantity: 2000,
            },
            PiRouteState {
                route_id: 2,
                path: vec![20, 30],
                commodity_type_id: 2268,
                quantity: 3000,
            },
            PiRouteState {
                route_id: 3,
                path: vec![30, 40],
                commodity_type_id: 3645,
                quantity: 20,
            },
        ],
        ..Default::default()
    };

    let (ext_qty, proc_qty) = advance_colony_simulation(&ctx, &mut col, &mut net, 3600, now);
    assert!(ext_qty > 0);
    assert_eq!(proc_qty, 40); // 2 cycles * 20 Water
    assert_eq!(net.pins[3].contents.get(&3645).copied(), Some(40));
}

#[test]
fn test_orbital_access_tax_upgrade_cc_and_expedited_transfer() {
    let ctx = make_ctx();
    let char_id = 90_000_001;
    let planet_id = 40_009_082;
    let owner_corp = 2_000_001;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            owner_corp,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = owner_corp;
                __corp.name = "POCO Corp".into();
                __corp.ticker = "POCO".into();
                __corp.ceo_id = 90_000_099;
                __corp.alliance_id = Some(99_000_001);
                __corp.tax_rate = 0.05;
                __corp },
        );
        let cfg = OrbitalCustomsSettings {
            planet_id,
            orbital_type_id: 2233,
            owner_corp_id: owner_corp,
            corporation_tax_rate: 0.01,
            alliance_tax_rate: 0.02,
            standing_high_tax_rate: 0.03,
            standing_good_tax_rate: 0.04,
            standing_neutral_tax_rate: 0.08,
            standing_bad_tax_rate: 0.12,
            standing_horrible_tax_rate: 0.20,
            allow_alliance: true,
            allow_standings: true,
            minimum_standing: -5.0,
        };
        save_orbital_settings(snap, planet_id, &cfg);

        // Neutral standing -> 0.08
        let (rate_neut, otid) =
            resolve_character_orbital_access_and_tax(snap, char_id, planet_id).unwrap();
        assert!((rate_neut - 0.08).abs() < 1e-6);
        assert_eq!(otid, 2233);

        // High standing (> 5.0) -> 0.03
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(owner_corp, 6.0);
        }
        let (rate_high, _) =
            resolve_character_orbital_access_and_tax(snap, char_id, planet_id).unwrap();
        assert!((rate_high - 0.03).abs() < 1e-6);

        // Horrible standing (< minimum_standing -5.0) -> denied
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(owner_corp, -8.0);
        }
        assert_eq!(
            resolve_character_orbital_access_and_tax(snap, char_id, planet_id)
                .unwrap_err()
                .code,
            "CustomsOfficeAccessDenied"
        );

        // Exact minimum_standing boundary (-5.0 >= -5.0) -> allowed with bad standing tax rate (0.12)
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(owner_corp, -5.0);
        }
        let (rate_bad, _) =
            resolve_character_orbital_access_and_tax(snap, char_id, planet_id).unwrap();
        assert!((rate_bad - 0.12).abs() < 1e-6);

        // Good standing (> 0.0 and <= 5.0) -> 0.04
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(owner_corp, 5.0);
        }
        let (rate_good, _) =
            resolve_character_orbital_access_and_tax(snap, char_id, planet_id).unwrap();
        assert!((rate_good - 0.04).abs() < 1e-6);

        // Setup colony + skill 2505 and test upgrade_command_center_logic & user_transfer_commodities_logic
        snap.skills.entry(char_id).or_default().insert(
            2505,
            TrainedSkillRecord {
                type_id: 2505,
                level: 4,
                skill_points: 100_000.0,
            },
        );
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                planet_id,
                owner_id: char_id,
                solar_system_id: 30_000_142,
                command_center_level: 0,
                pins_count: 1,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: 0,
            },
        );
        let upg = upgrade_command_center_logic(snap, char_id, planet_id, 2).unwrap();
        assert_eq!(upg.dict_get("commandCenterLevel").and_then(|v| v.as_i64()), Some(2));

        let net = PiColonyNetworkState {
            cpu_max: 12_136.0,
            pg_max: 12_000.0,
            pins: vec![
                PiPinState {
                    pin_id: 101,
                    type_id: 2257,
                    contents: HashMap::from([(3645, 100)]),
                    ..Default::default()
                },
                PiPinState {
                    pin_id: 102,
                    type_id: 2256,
                    ..Default::default()
                },
            ],
            links: vec![PiLinkState {
                source_pin_id: 101,
                dest_pin_id: 102,
                level: 1,
                bandwidth: 2500.0,
                ..Default::default()
            }],
            ..Default::default()
        };
        save_pi_network(snap, char_id, planet_id, &net);

        let xfer = user_transfer_commodities_logic(
            &ctx,
            snap,
            char_id,
            planet_id,
            &[
                PyRep::Integer(planet_id),
                PyRep::List(vec![PyRep::Integer(101), PyRep::Integer(102)]),
                PyRep::Dict(vec![(PyRep::Integer(3645), PyRep::Integer(40))]),
            ],
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(xfer.dict_get("quantity").and_then(|v| v.as_i64()), Some(40));

        // Second immediate transfer hits cooldown
        let cd_err = user_transfer_commodities_logic(
            &ctx,
            snap,
            char_id,
            planet_id,
            &[
                PyRep::Integer(planet_id),
                PyRep::List(vec![PyRep::Integer(101), PyRep::Integer(102)]),
                PyRep::Dict(vec![(PyRep::Integer(3645), PyRep::Integer(10))]),
            ],
            &HashMap::new(),
        )
        .unwrap_err();
        assert_eq!(cd_err.code, "ExpeditedTransferOnCooldown");

        // Test consume_pi_import_commodities
        snap.items.insert(
            555_001,
            InventoryItemRecord {
                item_id: 555_001,
                type_id: 3645,
                owner_id: char_id,
                location_id: planet_id,
                flag: InventoryFlag::Hangar,
                quantity: 50,
                ..Default::default()
            },
        );
        consume_pi_import_commodities(snap, char_id, planet_id, 3645, 30, None, None, None)
            .unwrap();
        assert_eq!(snap.items[&555_001].quantity, 20);
    });
}

#[test]
fn pi_1hz_processor_ticks_complete_cycle_and_background_tick() {
    let ctx = make_ctx();
    let base_now = 1_750_000_000_u64;
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(base_now)));
    let char_id = 90_000_001_i64;
    let planet_id = 40_009_091_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                planet_id,
                owner_id: char_id,
                solar_system_id: 30_000_142,
                command_center_level: 2,
                pins_count: 2,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: 0,
            },
        );
        let net = PiColonyNetworkState {
            cpu_max: 12_136.0,
            pg_max: 12_000.0,
            pins: vec![
                PiPinState {
                    pin_id: 201,
                    type_id: 2469,
                    schematic_id: Some(65),
                    contents: HashMap::from([(2268, 3000)]),
                    last_cycle_unix: Some(base_now),
                    ..Default::default()
                },
                PiPinState {
                    pin_id: 202,
                    type_id: 2256,
                    ..Default::default()
                },
            ],
            links: vec![PiLinkState {
                source_pin_id: 201,
                dest_pin_id: 202,
                level: 1,
                bandwidth: 2500.0,
                ..Default::default()
            }],
            routes: vec![PiRouteState {
                route_id: 1,
                path: vec![201, 202],
                commodity_type_id: 3645,
                quantity: 20,
            }],
            ..Default::default()
        };
        save_pi_network(snap, char_id, planet_id, &net);
    });

    // Simulate 1,799 1-Hz background ticks: remainder accumulates without completing early.
    for sec in 1..=1_799 {
        tick_all_colonies_background(&ctx, base_now + sec);
    }
    ctx.db.with_snapshot(|snap| {
        let net = load_pi_network(snap, char_id, planet_id, 2);
        let proc_pin = net.pins.iter().find(|p| p.pin_id == 201).unwrap();
        let launchpad = net.pins.iter().find(|p| p.pin_id == 202).unwrap();
        assert_eq!(proc_pin.contents.get(&2268).copied(), Some(3000));
        assert_eq!(launchpad.contents.get(&3645).copied().unwrap_or(0), 0);
        assert_eq!(proc_pin.last_cycle_unix, Some(base_now));
    });

    // At 1,800th 1-Hz tick, full P0 -> P1 cycle completes and routes output to launchpad.
    tick_all_colonies_background(&ctx, base_now + 1_800);
    ctx.db.with_snapshot(|snap| {
        let net = load_pi_network(snap, char_id, planet_id, 2);
        let proc_pin = net.pins.iter().find(|p| p.pin_id == 201).unwrap();
        let launchpad = net.pins.iter().find(|p| p.pin_id == 202).unwrap();
        assert_eq!(proc_pin.contents.get(&2268).copied().unwrap_or(0), 0);
        assert_eq!(launchpad.contents.get(&3645).copied(), Some(20));
        assert_eq!(proc_pin.last_cycle_unix, Some(base_now + 1_800));
    });
}

#[test]
fn test_pi_commodity_type_validation_and_strict_import_consumption() {
    let ctx = make_ctx();
    // All PI tiers P0..P4 are valid commodities/resources (SDE categories 42 and 43).
    for valid_tid in [2268, 3645, 44, 2344, 2867] {
        assert!(
            is_valid_pi_commodity_type(&ctx.sde, valid_tid),
            "expected PI commodity type {valid_tid} to be valid"
        );
        assert!(ensure_valid_pi_commodity_type(&ctx.sde, valid_tid).is_ok());
    }
    // Zero, blueprints (cat 9), ships (cat 6), modules (cat 7), PI pins (cat 41), and unknown IDs are rejected.
    for invalid_tid in [0, 683, 587, 380, 2534, 2543, 999_999] {
        assert!(
            !is_valid_pi_commodity_type(&ctx.sde, invalid_tid),
            "expected non-PI type {invalid_tid} to be invalid"
        );
        assert_eq!(
            ensure_valid_pi_commodity_type(&ctx.sde, invalid_tid)
                .unwrap_err()
                .code,
            "InvalidCommodity"
        );
    }

    let char_id = 90_000_001_i64;
    let planet_id = 40_009_082_i64;
    let ship_id = 777_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        // Zero quantity import must fail explicitly.
        let err_zero = consume_pi_import_commodities(
            snap,
            char_id,
            planet_id,
            3645,
            0,
            Some(ship_id),
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(err_zero.code, "InvalidCommodityQuantity");

        // Singleton, BPC, and mutated/dynamic items must all be rejected intact without consumption.
        snap.items.insert(
            900_001,
            InventoryItemRecord {
                item_id: 900_001,
                type_id: 3645,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let err_singleton = consume_pi_import_commodities(
            snap,
            char_id,
            planet_id,
            3645,
            1,
            Some(ship_id),
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(err_singleton.code, "InvalidCommodityItemState");
        assert_eq!(snap.items[&900_001].quantity, 1);

        snap.items.insert(
            900_002,
            InventoryItemRecord {
                item_id: 900_002,
                type_id: 683,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                is_blueprint_copy: true,
                blueprint_runs_remaining: Some(1),
                version: 1,
                ..Default::default()
            },
        );
        let err_bpc = consume_pi_import_commodities(
            snap,
            char_id,
            planet_id,
            683,
            1,
            Some(ship_id),
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(err_bpc.code, "InvalidCommodityItemState");
        assert!(snap.items[&900_002].is_blueprint_copy);
        assert_eq!(snap.items[&900_002].blueprint_runs_remaining, Some(1));

        snap.items.insert(
            900_003,
            InventoryItemRecord {
                item_id: 900_003,
                type_id: 2268,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 5,
                singleton: false,
                dynamic_attributes: HashMap::from([(64, 1.25)]),
                source_mutaplasmid_id: Some(47_841),
                version: 1,
                ..Default::default()
            },
        );
        // Also place a valid 5-unit stack so total = 10 only if the mutated stack were allowed.
        snap.items.insert(
            900_004,
            InventoryItemRecord {
                item_id: 900_004,
                type_id: 2268,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        let err_mutated = consume_pi_import_commodities(
            snap,
            char_id,
            planet_id,
            2268,
            10,
            Some(ship_id),
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(err_mutated.code, "InvalidCommodityItemState");
        assert_eq!(snap.items[&900_003].quantity, 5);
        assert_eq!(snap.items[&900_004].quantity, 5);

        // Overflowing qty > i32::MAX must not narrow-wrap (`qty as i32`) when stock is small.
        let over_i32 = (i32::MAX as u64) + 1;
        let err_over = consume_pi_import_commodities(
            snap,
            char_id,
            planet_id,
            2268,
            over_i32,
            Some(ship_id),
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(err_over.code, "InvalidCommodityItemState");
        assert_eq!(snap.items[&900_004].quantity, 5);

        // Multi-stack crossing i32::MAX boundary consumes cleanly across two valid stacks.
        snap.items.insert(
            900_005,
            InventoryItemRecord {
                item_id: 900_005,
                type_id: 44,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedPlanetaryCommoditiesHold,
                quantity: i32::MAX,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            900_006,
            InventoryItemRecord {
                item_id: 900_006,
                type_id: 44,
                owner_id: char_id,
                location_id: planet_id,
                flag: InventoryFlag::Hangar,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        consume_pi_import_commodities(
            snap,
            char_id,
            planet_id,
            44,
            (i32::MAX as u64) + 2,
            Some(ship_id),
            None,
            None,
        )
        .expect("multi-stack import across i32::MAX boundary succeeds");
        assert_eq!(
            snap.items.get(&900_005).map(|i| i.quantity).unwrap_or(0),
            0
        );
        assert_eq!(snap.items[&900_006].quantity, 3);

        // Courier mission cargo tagged in item_custom_names must never be consumed under reject_singleton.
        snap.items.insert(
            900_007,
            InventoryItemRecord {
                item_id: 900_007,
                type_id: 3645,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_custom_names
            .insert(900_007, "courier_cargo:90000001:42".into());
        let err_courier = consume_pi_import_commodities(
            snap,
            char_id,
            planet_id,
            3645,
            5,
            Some(ship_id),
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(err_courier.code, "InvalidCommodityItemState");
        assert_eq!(snap.items[&900_007].quantity, 5);
    });
}

#[test]
fn test_pi_extractor_cycle_time_scaling_24h_curve() {
    let mut sde = SdeRegistry::with_canonical_seed();
    if let Some(st) = sde.types.get_mut(&2257) {
        st.capacity = 200_000.0;
    }
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    let now = ctx.now_unix();
    let day = 86_400_u64;

    let run_for_cycle = |cycle_secs: u64| -> u64 {
        let mut col = PlanetaryColonyRecord {
            planet_id: 40_009_082,
            owner_id: 90_000_001,
            solar_system_id: 30_000_142,
            command_center_level: 3,
            pins_count: 2,
            extracted_commodities: HashMap::new(),
            last_extracted_unix: now,
        };
        let mut net = PiColonyNetworkState {
            cpu_max: 17_215.0,
            pg_max: 15_000.0,
            pins: vec![
                PiPinState {
                    pin_id: 10,
                    type_id: 2848,
                    extractor_resource_type_id: Some(2268),
                    extractor_cycle_seconds: cycle_secs,
                    extractor_head_count: 1,
                    extractor_head_radius: 0.02,
                    program_install_unix: now,
                    expiry_time_unix: Some(now + day),
                    ..Default::default()
                },
                PiPinState {
                    pin_id: 20,
                    type_id: 2257,
                    ..Default::default()
                },
            ],
            links: vec![PiLinkState {
                source_pin_id: 10,
                dest_pin_id: 20,
                level: 1,
                bandwidth: 1_250.0,
                ..Default::default()
            }],
            routes: vec![PiRouteState {
                route_id: 1,
                path: vec![10, 20],
                commodity_type_id: 2268,
                quantity: 10_000,
            }],
            ..Default::default()
        };
        let (ext, _) = advance_colony_simulation(&ctx, &mut col, &mut net, day, now + day);
        ext
    };

    let yield_60 = run_for_cycle(60);
    let yield_900 = run_for_cycle(900);
    let yield_1800 = run_for_cycle(1800);

    assert!(yield_60 > 25_000, "60s 24h run must not truncate at 200 cycles: {yield_60}");
    let rel_60_900 = ((yield_60 as f64) - (yield_900 as f64)).abs() / (yield_900 as f64);
    let rel_60_1800 = ((yield_60 as f64) - (yield_1800 as f64)).abs() / (yield_1800 as f64);
    assert!(
        rel_60_900 < 0.015,
        "60s yield ({yield_60}) must track 900s canonical curve ({yield_900}), rel diff={rel_60_900}"
    );
    assert!(
        rel_60_1800 < 0.015,
        "60s yield ({yield_60}) must track 1800s canonical curve ({yield_1800}), rel diff={rel_60_1800}"
    );
}
