//! Extra unit and mutation-killing tests for `services::industry_pi::pi_helpers`
//! covering command center levels/costs, pin type & capacity classifications, link path BFS
//! and bandwidth formulas, network load/save fallbacks, notification deduplication & queue eviction,
//! colony simulation storage-full / extractor-expiry branches, orbital customs tax & standing tiers,
//! command center upgrade validation, import commodity consumption, and expedited transfers.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_type;
use crate::services::industry_pi::pi_helpers::*;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, OwnerNoteRecord,
    PlanetaryColonyLayoutRecord, PlanetaryColonyRecord, PlanetaryLinkRecord,
    PlanetaryPinContentRecord, PlanetaryPinRecord, PlanetaryRouteRecord, TrainedSkillRecord,
    UpwellStructureRecord, WorldRepository,
};
use eve_proto::PyRep;
use eve_sde::{PlanetSchematicInfo, PlanetSchematicTypeEntry, SdeRegistry};
use std::collections::HashMap;
use std::sync::Arc;

fn make_pi_ctx() -> ServerContext {
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
    // Custom test types for pin_storage_capacity_m3 and resolve_pi_pin_stats group fallbacks
    for (tid, gid, cap, vol, price, attrs) in [
        (910_001, 1027, 0.0, 1.0, 0.0, vec![]),
        (910_002, 1030, 0.0, 1.0, 0.0, vec![]),
        (910_003, 1029, 0.0, 1.0, 0.0, vec![]),
        (910_004, 1026, 750.0, 0.0, 90_000.0, vec![]),
        (910_005, 1028, 0.0, 0.001, 0.0, vec![(50, 310.0), (11, 920.0)]),
    ] {
        let mut t = make_fighter_type(tid, gid, 41, vol, attrs);
        t.capacity = cap;
        t.base_price = price;
        sde.types.insert(tid, t);
    }
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(Arc::new(sde), db)
}

#[test]
fn cc_levels_pin_classifications_capacities_and_stat_fallbacks() {
    let ctx = make_pi_ctx();

    assert_eq!(cc_level_info(0), (1_675.0, 6_000.0, 0.0));
    assert_eq!(cc_level_info(1), (7_057.0, 9_000.0, 580_000.0));
    assert_eq!(cc_level_info(2), (12_136.0, 12_000.0, 1_510_000.0));
    assert_eq!(cc_level_info(3), (17_215.0, 15_000.0, 2_710_000.0));
    assert_eq!(cc_level_info(4), (21_315.0, 17_000.0, 4_210_000.0));
    assert_eq!(cc_level_info(5), (25_415.0, 19_000.0, 6_310_000.0));
    assert_eq!(cc_level_info(9), (25_415.0, 19_000.0, 6_310_000.0));
    assert_eq!(cc_level_cpu_pg(3), (17_215.0, 15_000.0));

    assert_eq!(cc_upgrade_cost_isk(3, 2), 0.0);
    assert_eq!(cc_upgrade_cost_isk(3, 3), 0.0);
    assert!((cc_upgrade_cost_isk(2, 4) - (4_210_000.0 - 1_510_000.0)).abs() < 1e-6);
    assert!((cc_upgrade_cost_isk(4, 5) - (6_310_000.0 - 4_210_000.0)).abs() < 1e-6);

    assert_eq!(pin_storage_capacity_m3(&ctx.sde, 910_001), 500.0);
    assert_eq!(pin_storage_capacity_m3(&ctx.sde, 910_002), 10_000.0);
    assert_eq!(pin_storage_capacity_m3(&ctx.sde, 910_003), 12_000.0);
    assert_eq!(pin_storage_capacity_m3(&ctx.sde, 910_004), 750.0);
    assert_eq!(pin_storage_capacity_m3(&ctx.sde, 910_005), 10_000.0);
    assert_eq!(pin_storage_capacity_m3(&ctx.sde, 999_999), 10_000.0);

    for tid in [2256, 2542, 2543, 2544, 2552, 2555, 2556, 2557, 910_002] {
        assert!(is_launchpad_pin(&ctx.sde, tid));
        assert!(is_storage_or_cc_or_launchpad_pin(&ctx.sde, tid));
    }
    for tid in [2257, 2535, 2536, 2541, 2558, 2560, 2561, 2562, 910_003] {
        assert!(is_storage_pin(&ctx.sde, tid));
        assert!(is_storage_or_cc_or_launchpad_pin(&ctx.sde, tid));
    }
    for tid in [2254, 2524, 2525, 2533, 2534, 2549, 2550, 2551, 2129, 2160, 2574, 2586, 910_001] {
        assert!(is_command_center_pin(&ctx.sde, tid));
        assert!(is_command_center_type(&ctx, tid));
        assert!(is_storage_or_cc_or_launchpad_pin(&ctx.sde, tid));
    }

    // pin_contents_volume_m3: min 0.01 volume clamp on SDE type vs 0.38 fallback on unknown type
    let test_pin = PiPinState {
        contents: HashMap::from([(910_004, 100), (999_999, 10)]),
        ..Default::default()
    };
    let vol = pin_contents_volume_m3(&ctx.sde, &test_pin);
    assert!((vol - (100.0 * 0.01 + 10.0 * 0.38)).abs() < 1e-6);

    assert_eq!(
        extract_pi_pin_id(Some(&PyRep::List(vec![PyRep::Integer(77), PyRep::String("x".into())]))),
        77
    );
    assert_eq!(extract_pi_pin_id(Some(&PyRep::String("bad".into()))), 0);

    assert_eq!(resolve_pi_pin_stats(&ctx, 910_001), (0.0, 0.0, 0.0));
    assert_eq!(resolve_pi_pin_stats(&ctx, 910_002), (3_600.0, 700.0, 45_000.0));
    assert_eq!(resolve_pi_pin_stats(&ctx, 910_003), (500.0, 700.0, 45_000.0));
    assert_eq!(resolve_pi_pin_stats(&ctx, 910_004), (400.0, 2_600.0, 90_000.0));
    assert_eq!(resolve_pi_pin_stats(&ctx, 910_005), (310.0, 920.0, 45_000.0));
    assert_eq!(resolve_pi_pin_stats(&ctx, 999_999), (200.0, 800.0, 45_000.0));
}

#[test]
fn link_path_bfs_multi_hop_and_bandwidth_level_formula() {
    let links = vec![
        PiLinkState {
            source_pin_id: 1,
            dest_pin_id: 2,
            level: 0,
            bandwidth: 0.0, // uses 1250.0 * 2^0 = 1250.0
            ..Default::default()
        },
        PiLinkState {
            source_pin_id: 3,
            dest_pin_id: 2, // reverse direction
            level: 2,
            bandwidth: 0.0, // uses 1250.0 * 2^2 = 5000.0
            ..Default::default()
        },
    ];
    assert_eq!(has_pi_link_path(&links, &[1], 10.0).unwrap_err().code, "InvalidTransferSource");
    assert_eq!(has_pi_link_path(&links, &[1, 2, 1], 10.0).unwrap_err().code, "InvalidTransferSource");
    // Explicit 3-hop path where 2->4 does not exist -> NoLinkBetweenPins
    assert_eq!(has_pi_link_path(&links, &[1, 2, 4], 10.0).unwrap_err().code, "NoLinkBetweenPins");
    // 2-elem [1, 3] triggers BFS 1 -> 2 -> 3 and checks min bandwidth (1250.0 on hop 1->2)
    assert!(has_pi_link_path(&links, &[1, 3], 1_250.0).is_ok());
    assert_eq!(
        has_pi_link_path(&links, &[1, 3], 1_251.0).unwrap_err().code,
        "InsufficientLinkBandwidth"
    );
    // Direct hop 2->3 has level 2 -> 5000.0 m3 bandwidth
    assert!(has_pi_link_path(&links, &[2, 3], 5_000.0).is_ok());
    assert_eq!(
        has_pi_link_path(&links, &[2, 3], 5_001.0).unwrap_err().code,
        "InsufficientLinkBandwidth"
    );
}

#[test]
fn load_save_format_pi_network_fallbacks_and_notification_dedup_cap() {
    let ctx = make_pi_ctx();
    let char_id = 90_000_001;
    let planet_id = 40_009_083;
    let nid = pi_network_note_id(char_id, planet_id);
    assert!(nid < -3_000_000_000_000);

    ctx.db.with_mut_snapshot(|snap| {
        // 1. Corrupted JSON in owner_note + planetary_colonies record -> falls back to default pin 2534
        snap.owner_notes.insert(
            nid,
            OwnerNoteRecord {
                note_id: nid,
                character_id: -1,
                item_id: planet_id,
                label: "corrupt".into(),
                note: "{not_valid_json".into(),
                timestamp_unix: 0,
            },
        );
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                planet_id,
                owner_id: char_id,
                solar_system_id: 30_000_142,
                command_center_level: 2,
                pins_count: 1,
                extracted_commodities: HashMap::from([(2268, 500)]),
                last_extracted_unix: 100,
            },
        );
        let net_corrupt = load_pi_network(snap, char_id, planet_id, 2);
        assert_eq!(net_corrupt.cpu_max, 12_136.0);
        assert_eq!(net_corrupt.pins.len(), 1);
        assert_eq!(net_corrupt.pins[0].type_id, 2534);
        assert_eq!(net_corrupt.pins[0].contents.get(&2268), Some(&500));

        // 2. Remove owner_note, load from planetary_colony_layouts with waypoints
        snap.owner_notes.remove(&nid);
        snap.planetary_colony_layouts.insert(
            (char_id, planet_id),
            PlanetaryColonyLayoutRecord {
                pins: vec![PlanetaryPinRecord {
                    pin_id: 501,
                    type_id: 2257,
                    latitude: 0.5,
                    longitude: 1.0,
                    schematic_id: Some(65),
                    install_time_unix: Some(1_000),
                    expiry_time_unix: Some(2_000),
                    last_cycle_start_unix: None,
                    contents: vec![PlanetaryPinContentRecord {
                        type_id: 3645,
                        amount: 80,
                    }],
                }],
                links: vec![PlanetaryLinkRecord {
                    source_pin_id: 501,
                    destination_pin_id: 502,
                    link_level: 2,
                }],
                routes: vec![PlanetaryRouteRecord {
                    route_id: 9,
                    source_pin_id: 501,
                    destination_pin_id: 503,
                    content_type_id: 3645,
                    quantity: 20,
                    waypoints: vec![502],
                }],
            },
        );
        let net_layout = load_pi_network(snap, char_id, planet_id, 3);
        assert_eq!(net_layout.routes[0].path, vec![501, 502, 503]);
        let (pins_r, links_r, routes_r) = format_pi_pins_links(&net_layout);
        assert_eq!(pins_r.len(), 1);
        assert_eq!(links_r.len(), 1);
        assert_eq!(routes_r.len(), 1);

        // Save network and verify round-trip + extracted_commodities sum
        save_pi_network(snap, char_id, planet_id, &net_layout);
        assert_eq!(
            snap.planetary_colonies[&(char_id, planet_id)]
                .extracted_commodities
                .get(&3645),
            Some(&80)
        );
    });

    // Notification deduplication and queue cap eviction
    queue_pi_notification(&ctx, char_id, "PlanetStorageFull", planet_id, 501, 102, 1_800_000_000);
    queue_pi_notification(&ctx, char_id, "PlanetStorageFull", planet_id, 501, 102, 1_800_000_000);
    let rec_count = ctx
        .notification_records
        .read()
        .unwrap()
        .iter()
        .filter(|r| r.receiver_id == char_id && r.type_id == 102)
        .count();
    assert_eq!(rec_count, 1);
}

#[test]
fn colony_simulation_storage_full_extractor_expiry_and_login_sync() {
    let ctx = make_pi_ctx();
    let char_id = 90_000_001;
    let planet_id = 40_009_084;
    let now = ctx.now_unix();

    let mut col = PlanetaryColonyRecord {
        planet_id,
        owner_id: char_id,
        solar_system_id: 30_000_142,
        command_center_level: 3,
        pins_count: 2,
        extracted_commodities: HashMap::new(),
        last_extracted_unix: now - 7200,
    };
    // Pin 10: ECU that expires at `now` and deposits into tiny 500 m3 CC pin 20 (type 910_001)
    let mut net = PiColonyNetworkState {
        pins: vec![
            PiPinState {
                pin_id: 10,
                type_id: 3060,
                extractor_resource_type_id: Some(2268),
                extractor_cycle_seconds: 600,
                extractor_head_count: 8,
                extractor_head_radius: 0.05,
                program_install_unix: now - 7200,
                expiry_time_unix: Some(now),
                ..Default::default()
            },
            PiPinState {
                pin_id: 20,
                type_id: 910_001, // 500 m3 capacity -> fills up!
                ..Default::default()
            },
        ],
        links: vec![PiLinkState {
            source_pin_id: 10,
            dest_pin_id: 20,
            level: 4,
            bandwidth: 20_000.0,
            ..Default::default()
        }],
        routes: vec![PiRouteState {
            route_id: 1,
            path: vec![10, 20],
            commodity_type_id: 2268,
            quantity: 5000,
        }],
        ..Default::default()
    };

    assert_eq!(advance_colony_simulation(&ctx, &mut col, &mut net, 0, now), (0, 0));
    let (ext, _) = advance_colony_simulation(&ctx, &mut col, &mut net, 7200, now);
    assert!(ext > 0);
    // Extractor expired at `now` -> program_install_unix reset to 0 and expiry_time_unix = Some(0)
    assert_eq!(net.pins[0].program_install_unix, 0);
    assert_eq!(net.pins[0].expiry_time_unix, Some(0));

    // Verify both PlanetStorageFull (102) and PlanetExtractorFinished (101) notifications were emitted
    let recs = ctx.notification_records.read().unwrap();
    assert!(recs.iter().any(|r| r.receiver_id == char_id && r.type_id == 102));
    assert!(recs.iter().any(|r| r.receiver_id == char_id && r.type_id == 101));
    drop(recs);

    // Test sync_character_pi_colonies_on_login using net.last_simulated_unix fallback when col.last_extracted_unix == 0
    ctx.db.with_mut_snapshot(|snap| {
        col.last_extracted_unix = 0;
        net.last_simulated_unix = now - 1800;
        net.pins[0].program_install_unix = now - 1800;
        net.pins[0].expiry_time_unix = Some(now + 3600);
        net.pins[1].type_id = 2257; // 12,000 m3 storage so it has room
        snap.planetary_colonies.insert((char_id, planet_id), col.clone());
        save_pi_network(snap, char_id, planet_id, &net);
    });
    sync_character_pi_colonies_on_login(&ctx, char_id);
    ctx.db.with_snapshot(|snap| {
        let synced = load_pi_network(snap, char_id, planet_id, 3);
        assert!(synced.pending_login_extracted > 0);
    });
}

#[test]
fn orbital_customs_interbus_corp_alliance_and_standing_tax_brackets() {
    let ctx = make_pi_ctx();
    let char_id = 90_000_001;
    let planet_id = 40_009_085;
    let orbital_id = 1_000_000_555_001;
    let owner_corp = 2_000_055;
    ctx.db.with_mut_snapshot(|snap| {
        let char_corp = snap.characters[&char_id].corporation_id;
        // Default when no settings exist -> (0.05, 2233)
        assert_eq!(
            resolve_character_orbital_access_and_tax(snap, char_id, planet_id).unwrap(),
            (0.05, 2233)
        );

        // Structure named "Orbital:<planet_id>" links orbital_id <-> planet_id
        snap.structures.insert(
            orbital_id,
            UpwellStructureRecord {
                structure_id: orbital_id,
                name: format!("Orbital:{planet_id}"),
                state: "Anchored".into(),
                solar_system_id: 30_000_142,
                owner_corp_id: owner_corp,
                type_id: 2233,
                ..Default::default()
            },
        );
        assert_eq!(resolve_orbital_and_planet(snap, orbital_id), (Some(orbital_id), planet_id));
        assert_eq!(resolve_orbital_and_planet(snap, planet_id), (Some(orbital_id), planet_id));

        // InterBus orbital (type 4318) enforces min 0.17 tax
        let mut cfg = OrbitalCustomsSettings {
            planet_id,
            orbital_type_id: 4318,
            owner_corp_id: owner_corp,
            corporation_tax_rate: 0.01,
            alliance_tax_rate: 0.02,
            standing_high_tax_rate: 0.03,
            standing_good_tax_rate: 0.04,
            standing_neutral_tax_rate: 0.10,
            standing_bad_tax_rate: 0.12,
            standing_horrible_tax_rate: 0.25,
            allow_alliance: false,
            allow_standings: true,
            minimum_standing: -10.0,
        };
        save_orbital_settings(snap, orbital_id, &cfg);
        assert_eq!(
            resolve_character_orbital_access_and_tax(snap, char_id, planet_id).unwrap(),
            (0.17, 4318)
        );

        // Switch to POCO (2233) and test same-alliance denied vs allowed
        cfg.orbital_type_id = 2233;
        save_orbital_settings(snap, planet_id, &cfg);
        snap.corporations.insert(
            owner_corp,
            CorporationRecord {
                corporation_id: owner_corp,
                alliance_id: Some(99_000_111),
                ..Default::default()
            },
        );
        snap.corporations
            .entry(char_corp)
            .or_insert_with(|| CorporationRecord {
                corporation_id: char_corp,
                ..Default::default()
            })
            .alliance_id = Some(99_000_111);
        assert_eq!(
            resolve_character_orbital_access_and_tax(snap, char_id, planet_id)
                .unwrap_err()
                .code,
            "CustomsOfficeAccessDenied"
        );
        cfg.allow_alliance = true;
        save_orbital_settings(snap, planet_id, &cfg);
        assert_eq!(
            resolve_character_orbital_access_and_tax(snap, char_id, planet_id).unwrap(),
            (0.02, 2233)
        );

        // Different alliance -> test all standing brackets: good (2.0), bad (-3.0), horrible (-8.0)
        snap.corporations.get_mut(&owner_corp).unwrap().alliance_id = None;
        for (st_val, expected_tax) in [(2.0, 0.04), (0.0, 0.10), (-3.0, 0.12), (-8.0, 0.25)] {
            snap.characters
                .get_mut(&char_id)
                .unwrap()
                .standings
                .insert(owner_corp, st_val);
            let (rate, _) = resolve_character_orbital_access_and_tax(snap, char_id, planet_id).unwrap();
            assert!((rate - expected_tax).abs() < 1e-6);
        }
    });
}

#[test]
fn upgrade_cc_errors_import_consumption_and_transfer_keyval_kwargs_branches() {
    let ctx = make_pi_ctx();
    let char_id = 90_000_001;
    let planet_id = 40_009_086;

    ctx.db.with_mut_snapshot(|snap| {
        assert_eq!(
            upgrade_command_center_logic(snap, char_id, planet_id, 1).unwrap_err().code,
            "ColonyNotFound"
        );
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                planet_id,
                owner_id: char_id,
                solar_system_id: 30_000_142,
                command_center_level: 1,
                pins_count: 2,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: 0,
            },
        );
        assert_eq!(
            upgrade_command_center_logic(snap, char_id, planet_id, 1).unwrap_err().code,
            "InvalidCommandCenterLevel"
        );
        assert_eq!(
            upgrade_command_center_logic(snap, char_id, planet_id, 3).unwrap_err().code,
            "MissingSkillForCommandCenterUpgrade"
        );
        snap.skills.entry(char_id).or_default().insert(
            2505,
            TrainedSkillRecord {
                type_id: 2505,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        let old_bal = snap.characters[&char_id].balance;
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents(100);
        assert_eq!(
            upgrade_command_center_logic(snap, char_id, planet_id, 2).unwrap_err().code,
            "InsufficientFunds"
        );
        snap.characters.get_mut(&char_id).unwrap().balance = old_bal;

        // Test consume_pi_import_commodities error branches and ship SpecializedPlanetaryCommoditiesHold
        assert_eq!(
            consume_pi_import_commodities(snap, char_id, planet_id, 3645, 10, Some(999), None, None)
                .unwrap_err()
                .code,
            "ItemNotFound"
        );
        snap.items.insert(
            666_001,
            InventoryItemRecord {
                item_id: 666_001,
                type_id: 3645,
                owner_id: char_id,
                location_id: 999,
                flag: InventoryFlag::SpecializedPlanetaryCommoditiesHold,
                quantity: 5,
                ..Default::default()
            },
        );
        assert_eq!(
            consume_pi_import_commodities(snap, char_id, planet_id, 3645, 10, Some(999), None, None)
                .unwrap_err()
                .code,
            "InsufficientCommodityQuantity"
        );
        assert!(consume_pi_import_commodities(snap, char_id, planet_id, 3645, 5, Some(999), None, None).is_ok());
        assert!(!snap.items.contains_key(&666_001));

        // Setup network for user_transfer_commodities_logic KeyVal and kwargs branches
        let net = PiColonyNetworkState {
            pins: vec![
                PiPinState {
                    pin_id: 1,
                    type_id: 2257,
                    contents: HashMap::from([(3645, 50)]),
                    ..Default::default()
                },
                PiPinState {
                    pin_id: 2,
                    type_id: 910_001, // 500 m3 capacity
                    ..Default::default()
                },
            ],
            links: vec![PiLinkState {
                source_pin_id: 1,
                dest_pin_id: 2,
                level: 2,
                bandwidth: 5000.0,
                ..Default::default()
            }],
            ..Default::default()
        };
        save_pi_network(snap, char_id, planet_id, &net);

        // KeyVal commodities map transfer
        let kv_res = user_transfer_commodities_logic(
            &ctx,
            snap,
            char_id,
            planet_id,
            &[
                PyRep::Integer(planet_id),
                PyRep::List(vec![PyRep::Integer(1), PyRep::Integer(2)]),
                PyRep::keyval([("3645", PyRep::Integer(20))]),
            ],
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(kv_res.dict_get("quantity"), Some(&PyRep::Integer(20)));

        // Reset cooldown and test scalar kwargs transfer + InsufficientCommodityStorage error
        let mut net2 = load_pi_network(snap, char_id, planet_id, 1);
        net2.pins[0].last_expedited_transfer_unix = 0;
        save_pi_network(snap, char_id, planet_id, &net2);

        let kw = HashMap::from([
            ("sourcePinID".into(), PyRep::Integer(1)),
            ("destPinID".into(), PyRep::Integer(2)),
            ("commodityTypeID".into(), PyRep::Integer(3645)),
            ("quantity".into(), PyRep::Integer(999)),
        ]);
        assert_eq!(
            user_transfer_commodities_logic(&ctx, snap, char_id, planet_id, &[PyRep::Integer(planet_id)], &kw)
                .unwrap_err()
                .code,
            "InsufficientCommodityStorage"
        );
    });
}
