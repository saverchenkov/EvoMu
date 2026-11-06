//! Deep branch-coverage and mutation-killing unit tests for Planetary Interaction:
//! extractor head diminishing returns & overlap factor, schematic cycle routing,
//! storage buffer overflow & notifications, and POCO/InterBus/Skyhook import/export/CC launch tax math.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_type;
use crate::services::industry_pi::extractor_overlap_factor;
use crate::services::industry_pi::pi_helpers::*;
use crate::services::industry_pi::pi_orbitals::{handle_pi_orbitals, pi_commodity_tier_base_cost};
use eve_db::{
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    PlanetaryColonyRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{PlanetSchematicInfo, PlanetSchematicTypeEntry, SdeRegistry};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn make_pi_test_ctx() -> ServerContext {
    let mut sde = SdeRegistry::with_canonical_seed();
    // Register multi-input P2 schematic 68: 40x P1_A (2389) + 40x P1_B (2390) -> 5x P2 (2328)
    sde.planet_schematics.insert(
        68,
        PlanetSchematicInfo {
            schematic_id: 68,
            name: "Water-Cooled CPU".into(),
            cycle_time: 3_600,
            types: vec![
                PlanetSchematicTypeEntry {
                    type_id: 2389,
                    quantity: 40,
                    is_input: true,
                },
                PlanetSchematicTypeEntry {
                    type_id: 2390,
                    quantity: 40,
                    is_input: true,
                },
                PlanetSchematicTypeEntry {
                    type_id: 2328,
                    quantity: 5,
                    is_input: false,
                },
            ],
            pins: vec![2469, 2470],
        },
    );
    // Register R0..P4 test commodity types with exact group_ids and volumes
    for (tid, gid, vol, price) in [
        (2268, 1032, 0.01, 5.0),           // R0 Aqueous Liquids
        (2389, 1042, 0.38, 400.0),         // P1 Plasmoids
        (2390, 1042, 0.38, 400.0),         // P1 Electrolytes
        (2328, 1034, 1.50, 7_200.0),       // P2 Water-Cooled CPU
        (2345, 1040, 6.00, 60_000.0),      // P3 Camera Drones
        (2875, 1041, 100.0, 1_200_000.0),  // P4 Broadcast Node
    ] {
        let mut t = make_fighter_type(tid, gid, 43, vol, vec![]);
        t.base_price = price;
        sde.types.insert(tid, t);
    }
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(Arc::new(sde), db)
}

#[test]
fn extractor_overlap_factor_and_head_diminishing_returns_across_cycles() {
    // 0 or 1 head always returns 1.0
    assert!((extractor_overlap_factor(&[], 0.05) - 1.0).abs() < 1e-9);
    assert!((extractor_overlap_factor(&[(1, 0.2, 0.3)], 0.05) - 1.0).abs() < 1e-9);

    // Non-overlapping heads (distance >= 2*r = 0.10) have 0 penalty -> factor 1.0
    let non_overlap = vec![(1, 0.0, 0.0), (2, 0.10, 0.0)];
    assert!((extractor_overlap_factor(&non_overlap, 0.05) - 1.0).abs() < 1e-9);

    // Two heads at distance d = r = 0.05 (half of 2*r = 0.10):
    // penalty = 0.35 * (1 - 0.05 / 0.10) = 0.175 -> factor = 0.825
    let half_overlap = vec![(1, 0.0, 0.0), (2, 0.05, 0.0)];
    assert!((extractor_overlap_factor(&half_overlap, 0.05) - 0.825).abs() < 1e-9);

    // Coincident heads (d = 0): 1 pair -> penalty = 0.35 -> factor = 0.65
    let coincident_pair = vec![(1, 0.1, 0.1), (2, 0.1, 0.1)];
    assert!((extractor_overlap_factor(&coincident_pair, 0.05) - 0.65).abs() < 1e-9);

    // 4 coincident heads (6 pairs * 0.35 = 2.10 penalty) clamps to minimum 0.25
    let four_coincident = vec![
        (1, 0.1, 0.1),
        (2, 0.1, 0.1),
        (3, 0.1, 0.1),
        (4, 0.1, 0.1),
    ];
    assert!((extractor_overlap_factor(&four_coincident, 0.05) - 0.25).abs() < 1e-9);

    // Radius clamp: r < 0.005 clamps to 0.005 (2*r = 0.01); d = 0.005 -> penalty 0.175 -> 0.825
    let tiny_r_heads = vec![(1, 0.0, 0.0), (2, 0.005, 0.0)];
    assert!((extractor_overlap_factor(&tiny_r_heads, 0.0001) - 0.825).abs() < 1e-9);

    // Verify diminishing returns per cycle in advance_colony_simulation:
    // cycle 1: decay = exp(0) = 1.0 -> batch = round(1000 * 2 * 0.825 * 1.0) = 1650
    // cycle 2: decay = exp(-0.015) -> batch = round(1650 * exp(-0.015)) = 1625
    // cycle 3: decay = exp(-0.030) -> batch = round(1650 * exp(-0.030)) = 1601
    let ctx = make_pi_test_ctx();
    let char_id = 91_001i64;
    let planet_id = 40_000_101i64;
    let install_ts = 1_800_000_000u64;
    let mut col = PlanetaryColonyRecord {
        owner_id: char_id,
        planet_id,
        solar_system_id: 30_000_142,
        command_center_level: 4,
        pins_count: 2,
        last_extracted_unix: install_ts,
        extracted_commodities: HashMap::new(),
    };
    let mut net = PiColonyNetworkState {
        pins: vec![
            PiPinState {
                pin_id: 1,
                type_id: 2848, // Extractor Control Unit
                extractor_resource_type_id: Some(2268),
                extractor_heads: half_overlap,
                extractor_head_count: 2,
                extractor_head_radius: 0.05,
                extractor_cycle_seconds: 1_800,
                program_install_unix: install_ts,
                expiry_time_unix: Some(install_ts + 86_400),
                ..Default::default()
            },
            PiPinState {
                pin_id: 2,
                type_id: 2256, // Launchpad (10,000 m3 capacity)
                ..Default::default()
            },
        ],
        links: vec![PiLinkState {
            source_pin_id: 1,
            dest_pin_id: 2,
            level: 0,
            bandwidth: 10_000.0,
            ..Default::default()
        }],
        routes: vec![PiRouteState {
            route_id: 10,
            path: vec![1, 2],
            commodity_type_id: 2268,
            quantity: 5_000,
        }],
        last_simulated_unix: install_ts,
        ..Default::default()
    };

    // Simulate 3 cycles (5,400 seconds)
    let (produced, _) = advance_colony_simulation(&ctx, &mut col, &mut net, 5_400, install_ts + 5_400);
    let c1 = (1000.0_f64 * 2.0 * 0.825 * (-0.015_f64 * 0.0).exp()).round() as u64;
    let c2 = (1000.0_f64 * 2.0 * 0.825 * (-0.015_f64 * 1.0).exp()).round() as u64;
    let c3 = (1000.0_f64 * 2.0 * 0.825 * (-0.015_f64 * 2.0).exp()).round() as u64;
    assert_eq!((c1, c2, c3), (1650, 1625, 1601));
    assert_eq!(produced, c1 + c2 + c3);
    assert_eq!(net.pins[1].contents.get(&2268).copied(), Some(c1 + c2 + c3));

    // Advance past expiry_time_unix -> emits PlanetExtractorFinished notification (101)
    let _ = advance_colony_simulation(&ctx, &mut col, &mut net, 90_000, install_ts + 95_400);
    let recs = ctx.notification_records.read().unwrap();
    assert!(recs.iter().any(|r| r.receiver_id == char_id && r.type_id == 101));
}

#[test]
fn schematic_cycle_routing_link_validation_and_storage_buffer_overflow() {
    let ctx = make_pi_test_ctx();
    let char_id = 91_002i64;
    let planet_id = 40_000_102i64;
    let now = 1_800_000_000u64;

    let mut col = PlanetaryColonyRecord {
        owner_id: char_id,
        planet_id,
        solar_system_id: 30_000_142,
        command_center_level: 4,
        pins_count: 3,
        last_extracted_unix: now,
        extracted_commodities: HashMap::new(),
    };

    // Pin 10: Launchpad holding 120x 2389 and 120x 2390 (enough for 3 cycles of schematic 68)
    // Pin 20: Processor running schematic 68 (cycle 3600s, outputs 5x 2328 @ 1.5 m3 = 7.5 m3/cycle)
    // Pin 30: Command Center (capacity 500 m3), pre-filled to 490 m3 so only 1 cycle (7.5 m3 -> 497.5 m3) fits,
    //         and cycle 2 (497.5 + 7.5 = 505.0 > 500.0 m3) triggers storage buffer overflow!
    let mut net = PiColonyNetworkState {
        pins: vec![
            PiPinState {
                pin_id: 10,
                type_id: 2256, // Launchpad (10,000 m3)
                contents: HashMap::from([(2389, 120), (2390, 120)]),
                ..Default::default()
            },
            PiPinState {
                pin_id: 20,
                type_id: 2470, // Processor
                schematic_id: Some(68),
                ..Default::default()
            },
            PiPinState {
                pin_id: 30,
                type_id: 2254, // Command Center (500 m3 capacity)
                contents: HashMap::from([(2268, 49_000)]), // 49,000 * 0.01 m3 = 490.0 m3
                ..Default::default()
            },
        ],
        // Initially omit link 20 -> 30 so output route is disconnected
        links: vec![PiLinkState {
            source_pin_id: 10,
            dest_pin_id: 20,
            level: 0,
            bandwidth: 10_000.0,
            ..Default::default()
        }],
        routes: vec![
            PiRouteState {
                route_id: 1,
                path: vec![10, 20],
                commodity_type_id: 2389,
                quantity: 40,
            },
            PiRouteState {
                route_id: 2,
                path: vec![10, 20],
                commodity_type_id: 2390,
                quantity: 40,
            },
            PiRouteState {
                route_id: 3,
                path: vec![20, 30],
                commodity_type_id: 2328,
                quantity: 5,
            },
        ],
        last_simulated_unix: now,
        ..Default::default()
    };

    // 1) With output link 20 -> 30 missing, processor cannot route output -> 0 inputs consumed
    advance_colony_simulation(&ctx, &mut col, &mut net, 7_200, now + 7_200);
    assert_eq!(net.pins[0].contents.get(&2389).copied(), Some(120));
    assert_eq!(net.pins[2].contents.get(&2328), None);

    // 2) Connect link 20 -> 30 and simulate 2 cycles (7,200s):
    //    Cycle 1 succeeds (consumes 40x 2389 & 40x 2390, mints 5x 2328 into pin 30 -> 497.5 m3).
    //    Cycle 2 overflows pin 30 (497.5 + 7.5 > 500 m3) -> halts without consuming cycle 2 inputs and emits PlanetStorageFull!
    net.links.push(PiLinkState {
        source_pin_id: 20,
        dest_pin_id: 30,
        level: 0,
        bandwidth: 10_000.0,
        ..Default::default()
    });
    advance_colony_simulation(&ctx, &mut col, &mut net, 7_200, now + 14_400);
    assert_eq!(net.pins[0].contents.get(&2389).copied(), Some(80));
    assert_eq!(net.pins[0].contents.get(&2390).copied(), Some(80));
    assert_eq!(net.pins[2].contents.get(&2328).copied(), Some(5));
    let recs = ctx.notification_records.read().unwrap();
    assert!(recs.iter().any(|r| r.receiver_id == char_id && r.type_id == 102));
}

#[test]
fn customs_office_tier_base_costs_interbus_floor_and_standings_tax_brackets() {
    let ctx = make_pi_test_ctx();
    assert_eq!(pi_commodity_tier_base_cost(&ctx.sde, 2268), 5.0);
    assert_eq!(pi_commodity_tier_base_cost(&ctx.sde, 2389), 400.0);
    assert_eq!(pi_commodity_tier_base_cost(&ctx.sde, 2328), 7_200.0);
    assert_eq!(pi_commodity_tier_base_cost(&ctx.sde, 2345), 60_000.0);
    assert_eq!(pi_commodity_tier_base_cost(&ctx.sde, 2875), 1_200_000.0);

    let planet_id = 40_000_201i64;
    let owner_corp = 98_401i64;
    let ally_corp = 98_402i64;
    let ally_id = 99_401i64;
    let (corp_pilot, ally_pilot, outsider_pilot) = (91_101i64, 91_102i64, 91_103i64);

    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "POCO Ally".into(),
                short_name: "POCA".into(),
                executor_corp_id: owner_corp,
                member_corp_ids: HashSet::from([owner_corp, ally_corp]),
                ..Default::default()
            },
        );
        for (corp, ally) in [(owner_corp, Some(ally_id)), (ally_corp, Some(ally_id))] {
            snap.corporations.insert(
                corp,
                CorporationRecord {
                    corporation_id: corp,
                    name: format!("Corp_{corp}"),
                    ticker: "CRP".into(),
                    ceo_id: corp_pilot,
                    alliance_id: ally,
                    ..Default::default()
                },
            );
        }
        for (cid, corp, ally) in [
            (corp_pilot, owner_corp, Some(ally_id)),
            (ally_pilot, ally_corp, Some(ally_id)),
            (outsider_pilot, 98_999, None),
        ] {
            let mut ch = snap.characters.values().next().cloned().expect("demo char");
            ch.character_id = cid;
            ch.corporation_id = corp;
            ch.alliance_id = ally;
            snap.characters.insert(cid, ch);
        }

        // 1) InterBus Customs Office (orbital_type_id == 4318) enforces >= 17% floor
        save_orbital_settings(
            snap,
            planet_id,
            &OrbitalCustomsSettings {
                planet_id,
                orbital_type_id: 4_318,
                standing_neutral_tax_rate: 0.05,
                ..Default::default()
            },
        );
        let (rate_ib, tid_ib) =
            resolve_character_orbital_access_and_tax(snap, outsider_pilot, planet_id).expect("interbus");
        assert_eq!(tid_ib, 4_318);
        assert!((rate_ib - 0.17).abs() < 1e-9);

        // 2) Replace with player POCO (type 2233) and configure custom tax brackets
        let cfg = OrbitalCustomsSettings {
            planet_id,
            orbital_type_id: 2_233,
            owner_corp_id: owner_corp,
            corporation_tax_rate: 0.02,
            alliance_tax_rate: 0.04,
            standing_high_tax_rate: 0.06,
            standing_good_tax_rate: 0.08,
            standing_neutral_tax_rate: 0.12,
            standing_bad_tax_rate: 0.18,
            standing_horrible_tax_rate: 0.25,
            allow_alliance: true,
            allow_standings: true,
            minimum_standing: -5.0,
        };
        save_orbital_settings(snap, planet_id, &cfg);

        // Owning corp pilot -> 2%, alliance pilot -> 4%
        assert!((resolve_character_orbital_access_and_tax(snap, corp_pilot, planet_id).unwrap().0 - 0.02).abs() < 1e-9);
        assert!((resolve_character_orbital_access_and_tax(snap, ally_pilot, planet_id).unwrap().0 - 0.04).abs() < 1e-9);

        // Outsider standings brackets: > 5.0 (high), 5.0 (good), 0.0 (neutral), -1.0 (bad), -5.1 (denied < -5.0)
        for (standing, expected_rate) in [(5.1, 0.06), (5.0, 0.08), (0.0, 0.12), (-1.0, 0.18)] {
            snap.characters
                .get_mut(&outsider_pilot)
                .unwrap()
                .standings
                .insert(owner_corp, standing);
            let (r, _) = resolve_character_orbital_access_and_tax(snap, outsider_pilot, planet_id)
                .expect("standing allowed");
            assert!((r - expected_rate).abs() < 1e-9, "standing={standing}");
        }
        snap.characters
            .get_mut(&outsider_pilot)
            .unwrap()
            .standings
            .insert(owner_corp, -5.1);
        assert_eq!(
            resolve_character_orbital_access_and_tax(snap, outsider_pilot, planet_id)
                .unwrap_err()
                .code,
            "CustomsOfficeAccessDenied"
        );
    });
}

#[tokio::test]
async fn user_launch_commodities_import_05x_export_10x_and_command_center_15x_tax_math() {
    let ctx = make_pi_test_ctx();
    let char_id = 91_201i64;
    let corp_id = 98_501i64;
    let poco_corp = 98_502i64;
    let sys_id = 30_004_759u32; // nullsec (npc_customs_tax_rate = 0.0)
    let planet_id = 40_000_301i64;
    let ship_id = 891_001i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.balance = IskCents::from_isk(1_000_000);
        snap.characters.insert(char_id, ch);

        let mut owner_corp_rec = CorporationRecord {
            corporation_id: poco_corp,
            name: "POCO Tax Corp".into(),
            ticker: "PTAX".into(),
            ceo_id: 99_999,
            master_wallet: IskCents(0),
            ..Default::default()
        };
        owner_corp_rec.ensure_wallet_divisions();
        snap.corporations.insert(poco_corp, owner_corp_rec);

        // Configure 10% neutral tax on planet_id
        save_orbital_settings(
            snap,
            planet_id,
            &OrbitalCustomsSettings {
                planet_id,
                orbital_type_id: 2_233,
                owner_corp_id: poco_corp,
                standing_neutral_tax_rate: 0.10,
                allow_standings: true,
                minimum_standing: -10.0,
                ..Default::default()
            },
        );

        // Seed colony with Launchpad (pin 100) and Command Center (pin 101)
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                owner_id: char_id,
                planet_id,
                solar_system_id: sys_id,
                command_center_level: 4,
                pins_count: 2,
                last_extracted_unix: 1_800_000_000,
                extracted_commodities: HashMap::new(),
            },
        );
        let net = PiColonyNetworkState {
            pins: vec![
                PiPinState {
                    pin_id: 100,
                    type_id: 2256, // Launchpad holding 20x P2 (2328, base 7200 ISK)
                    contents: HashMap::from([(2328, 20)]),
                    ..Default::default()
                },
                PiPinState {
                    pin_id: 101,
                    type_id: 2254, // Command Center holding 10x P2 (2328) and 10x P4 (2875, 100 m3 each)
                    contents: HashMap::from([(2328, 10), (2875, 10)]),
                    ..Default::default()
                },
            ],
            last_simulated_unix: 1_800_000_000,
            ..Default::default()
        };
        save_pi_network(snap, char_id, planet_id, &net);

        // Put 10x P2 (2328) in ship cargo for ImportToColony
        snap.items.insert(
            891_010,
            InventoryItemRecord {
                item_id: 891_010,
                type_id: 2328,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState::new(1);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let kw = HashMap::new();

    // Base value of 10x P2 (2328 @ 7,200 ISK) = 72,000 ISK; 10% tax rate -> base export tax = 7,200 ISK.
    // 1) ImportToColony: 0.5x multiplier = 3,600 ISK
    let imp_rep = handle_pi_orbitals(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(planet_id),
            PyRep::Integer(2328),
            PyRep::Integer(10),
            PyRep::String("ImportToColony".into()),
        ],
        &kw,
        None,
    )
    .unwrap()
    .expect("ImportToColony");
    assert_eq!(imp_rep.dict_get("importTaxISK"), Some(&PyRep::Real(3_600.0)));

    // 2) Export to CustomsOffice: 1.0x multiplier = 7,200 ISK
    let exp_rep = handle_pi_orbitals(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(planet_id),
            PyRep::Integer(2328),
            PyRep::Integer(10),
            PyRep::String("CustomsOffice".into()),
        ],
        &kw,
        None,
    )
    .unwrap()
    .expect("CustomsOffice export");
    assert_eq!(exp_rep.dict_get("exportTaxISK"), Some(&PyRep::Real(7_200.0)));

    // 3) CommandCenterLaunch: 1.5x multiplier = 10,800 ISK
    let cc_rep = handle_pi_orbitals(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(planet_id),
            PyRep::Integer(101),
            PyRep::Integer(2328),
            PyRep::Integer(10),
        ],
        &kw,
        None,
    )
    .unwrap()
    .expect("CommandCenterLaunch");
    assert_eq!(cc_rep.dict_get("exportTaxISK"), Some(&PyRep::Real(10_800.0)));

    // 4) CommandCenterLaunch exceeding 500 m3 (6x P4 = 600 m3) -> CommandCenterLaunchCapacityExceeded
    let cc_err = handle_pi_orbitals(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(planet_id),
            PyRep::Integer(101),
            PyRep::Integer(2875),
            PyRep::Integer(6),
        ],
        &kw,
        None,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(cc_err.code, "CommandCenterLaunchCapacityExceeded");

    // Verify total tax (3,600 + 7,200 + 10,800 = 21,600 ISK) deducted from pilot and credited to POCO owner corp
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].balance, IskCents::from_isk(978_400));
        assert_eq!(snap.corporations[&poco_corp].master_wallet, IskCents::from_isk(21_600));
    });
}
