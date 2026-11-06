//! Deep branch-coverage & mutation-killing unit tests for reprocessing yield, efficiency,
//! scrapmetal, implant modifiers, structure rigs, randomized materials, and `reprocessingSvc` RPCs.

use crate::router::ServerContext;
use crate::services::mining_advanced::mining_compression::{
    compute_structure_reprocessing_base_yield, compute_structure_rigged_base_yield,
    structure_has_online_reprocessing_service,
};
use crate::services::mining_cycle::{
    compute_reprocessing_rates_for_item, compute_reprocessing_rates_for_item_at,
};
use crate::services::space_entity_mining::handle;
use crate::services::space_mining_harvest::{
    resolve_reprocessable_materials, resolve_reprocessable_materials_for_portions,
};
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, StandingTransactionRecord,
    TrainedSkillRecord, UpwellStructureRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::sync::Arc;

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SYSTEM_ID: u32 = 30_000_142;
const THE_FORGE_REGION_ID: u32 = 10_000_002;

fn make_test_context() -> ServerContext {
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(sde, db)
}

fn make_session(
    char_id: i64,
    corp_id: i64,
    station_id: Option<i64>,
    structure_id: Option<i64>,
    ship_id: Option<i64>,
) -> SessionState {
    SessionState {
        session_id: char_id as u64,
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id,
        structure_id,
        solar_system_id: Some(JITA_SYSTEM_ID),
        constellation_id: Some(20_000_020),
        region_id: Some(THE_FORGE_REGION_ID),
        ship_id,
        ..Default::default()
    }
}

#[test]
fn reprocessing_yield_scrapmetal_implant_and_standing_tax_formulas() {
    let ctx = make_test_context();
    let char_id = 930_001;
    let corp_id = 98_000_301;
    let athanor_id = 1_000_000_930_001;
    let tatara_id = 1_000_000_930_002;

    let veldspar = ctx.sde.types.get(&1230).expect("Veldspar 1230 in SDE");
    let rifter = ctx.sde.types.get(&587).expect("Rifter 587 in SDE");
    let rx_implant_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.default_attributes.get(&379).copied() == Some(4.0))
        .map(|t| t.type_id)
        .unwrap_or(27_174);
    let ore_skill_id = veldspar
        .default_attributes
        .get(&eve_sde::attr_ids::REPROCESSING_SKILL_TYPE)
        .map(|&v| v as u32)
        .unwrap_or(12195);
    let jita_owner_corp = ctx
        .sde
        .stations
        .get(&JITA_STATION_ID)
        .map(|st| st.owner_corporation_id)
        .unwrap_or(1_000_035);

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = "Refiner".into();
        ch.corporation_id = corp_id;
        ch.station_id = Some(JITA_STATION_ID);
        ch.solar_system_id = JITA_SYSTEM_ID;
        ch.constellation_id = 20_000_020;
        ch.region_id = THE_FORGE_REGION_ID;
        ch.active_ship_id = char_id + 500;
        ch.balance = IskCents::from_isk(50_000_000);
        ch.standings.clear();
        snap.characters.insert(char_id, ch);

        snap.structures.insert(
            athanor_id,
            UpwellStructureRecord {
                structure_id: athanor_id,
                type_id: 35835, // Athanor (+2% -> 0.51 base yield)
                owner_corp_id: corp_id,
                solar_system_id: JITA_SYSTEM_ID,
                name: "Refinery Athanor".into(),
                state: "ShieldVulnerable".into(),
                fuel_blocks: 500,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            tatara_id,
            UpwellStructureRecord {
                structure_id: tatara_id,
                type_id: 35836, // Tatara (+5.5% -> 0.5275 base yield)
                owner_corp_id: corp_id,
                solar_system_id: 31_000_005, // Wormhole space -> 2357 (1.12x) security multiplier
                name: "Refinery Tatara".into(),
                state: "ShieldVulnerable".into(),
                fuel_blocks: 500,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
    });

    // 1. Structure base yields: Raitaru (35825) = 0.50, Athanor (35835) = 0.51, Tatara (35836) = 0.5275
    assert!((compute_structure_reprocessing_base_yield(&ctx.sde, 35825) - 0.50).abs() < 1e-6);
    assert!((compute_structure_reprocessing_base_yield(&ctx.sde, 35835) - 0.51).abs() < 1e-6);
    assert!((compute_structure_reprocessing_base_yield(&ctx.sde, 35836) - 0.5275).abs() < 1e-6);

    // 2. Unskilled pilot at Jita station: base = 0.50, yield = 0.50, tax = 0.05 (5%)
    ctx.db.with_snapshot(|snap| {
        let (base, yld, tax) =
            compute_reprocessing_rates_for_item(&ctx.sde, snap, char_id, JITA_STATION_ID, veldspar);
        assert!((base - 0.50).abs() < 1e-6);
        assert!((yld - 0.50).abs() < 1e-6);
        assert!((tax - 0.05).abs() < 1e-6);
    });

    // 3. Train Reprocessing (3385) V, Reprocessing Efficiency (3389) V, Ore skill V, and plug in +4% implant
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        for sid in [3385, 3389, ore_skill_id, 12196, 3359] {
            sk.insert(
                sid,
                TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 },
            );
        }
        snap.active_implants.insert(char_id, vec![rx_implant_id]);

        // Give pilot 5.0 raw standing with Jita owner corp + a mission completion transaction (event_type_id 85)
        // With Connections V: effective standing = 5.0 + (10.0 - 5.0) * 0.20 = 6.0 -> tax = 0.05 - 0.0075 * 6.0 = 0.005 (0.5%)
        snap.characters
            .get_mut(&char_id)
            .unwrap()
            .standings
            .insert(jita_owner_corp, 5.0);
        snap.standing_transactions
            .entry(char_id)
            .or_default()
            .push(StandingTransactionRecord {
                event_id: 1,
                from_id: jita_owner_corp,
                to_id: char_id,
                event_type_id: 85,
                modification: 0.5,
            });
    });

    ctx.db.with_snapshot(|snap| {
        let (base, ore_yield, tax) = compute_reprocessing_rates_for_item_at(
            &ctx.sde,
            snap,
            char_id,
            JITA_STATION_ID,
            veldspar,
            Some(200),
        );
        assert!((base - 0.50).abs() < 1e-6);
        assert!((ore_yield - (0.50 * 1.15 * 1.10 * 1.10 * 1.04)).abs() < 1e-6);
        assert!((tax - 0.005).abs() < 1e-6);

        // Scrapmetal yield on Rifter (587): ignores 3385/3389/implant and structure bonus!
        let (sc_base, sc_yield, _) =
            compute_reprocessing_rates_for_item(&ctx.sde, snap, char_id, athanor_id, rifter);
        assert!((sc_base - 0.50).abs() < 1e-6);
        assert!((sc_yield - 0.55).abs() < 1e-6);
    });

    // 4. Fit an L-Set Reprocessing rig (group 1944/1945, rigSize 3.0) on the W-space Tatara and verify rigged yield > unrigged
    let l_rig_tid = ctx
        .sde
        .types
        .values()
        .find(|t| {
            matches!(t.group_id, 1944 | 1945)
                && t.default_attributes.get(&1547) == Some(&3.0)
                && t.default_attributes.get(&717).is_some_and(|&v| v > 0.50)
        })
        .map(|t| t.type_id)
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            930_201,
            InventoryItemRecord {
                item_id: 930_201,
                type_id: l_rig_tid,
                owner_id: corp_id,
                location_id: tatara_id,
                flag: InventoryFlag::RigSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    ctx.db.with_snapshot(|snap| {
        let rigged_base =
            compute_structure_rigged_base_yield(&ctx.sde, snap, tatara_id, Some(veldspar));
        assert!(rigged_base > 0.5275);
    });
}

#[test]
fn resolve_reprocessable_materials_fixed_and_randomized_batches() {
    let ctx = make_test_context();
    let veldspar = ctx.sde.types.get(&1230).unwrap();
    let tritanium = ctx.sde.types.get(&34).unwrap();

    assert!(resolve_reprocessable_materials(&ctx.sde, tritanium, 1, 1, 0, true).is_none());

    let mats_1 = resolve_reprocessable_materials(&ctx.sde, veldspar, 1, 1, 0, true).unwrap();
    let mats_4 =
        resolve_reprocessable_materials_for_portions(&ctx.sde, veldspar, 1, 1, 0, 4, false)
            .unwrap();
    assert_eq!(mats_1.len(), 1);
    assert_eq!(mats_4[0].quantity, mats_1[0].quantity * 4);

    if let Some(prism) = ctx.sde.types.get(&90_041) {
        let zero_batches =
            resolve_reprocessable_materials_for_portions(&ctx.sde, prism, 10, 20, 42, 0, false)
                .unwrap();
        assert!(zero_batches.is_empty());

        let quote_mats =
            resolve_reprocessable_materials_for_portions(&ctx.sde, prism, 10, 20, 42, 5, true)
                .unwrap();
        let rand_mats =
            resolve_reprocessable_materials_for_portions(&ctx.sde, prism, 10, 20, 42, 5, false)
                .unwrap();
        let large_mats =
            resolve_reprocessable_materials_for_portions(&ctx.sde, prism, 10, 20, 42, 12_000, false)
                .unwrap();
        assert!(!quote_mats.is_empty());
        assert!(!rand_mats.is_empty());
        assert!(!large_mats.is_empty());
    }
}

#[tokio::test]
async fn reprocessing_svc_reprocess_and_get_quotes_error_and_success_paths() {
    let ctx = make_test_context();
    let char_id = 931_001;
    let corp_id = 98_000_311;
    let ship_id = 931_500;
    let struct_id = 1_000_000_931_001;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = "RPC Refiner".into();
        ch.corporation_id = corp_id;
        ch.station_id = Some(JITA_STATION_ID);
        ch.solar_system_id = JITA_SYSTEM_ID;
        ch.constellation_id = 20_000_020;
        ch.region_id = THE_FORGE_REGION_ID;
        ch.active_ship_id = ship_id;
        ch.balance = IskCents::from_isk(10_000_000);
        snap.characters.insert(char_id, ch);

        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Refiner Corp".into(),
                ticker: "REFC".into(),
                ceo_id: char_id,
                master_wallet: IskCents::from_isk(10_000_000),
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35825, // Raitaru (requires fitted online reprocessing module)
                owner_corp_id: corp_id,
                solar_system_id: JITA_SYSTEM_ID,
                name: "Raitaru No Reproc".into(),
                state: "ShieldVulnerable".into(),
                fuel_blocks: 500,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            931_101,
            InventoryItemRecord {
                item_id: 931_101,
                type_id: 34,
                owner_id: char_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1_000,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            931_102,
            InventoryItemRecord {
                item_id: 931_102,
                type_id: 587,
                owner_id: char_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            931_103,
            InventoryItemRecord {
                item_id: 931_103,
                type_id: 1230,
                owner_id: char_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 50,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            931_104,
            InventoryItemRecord {
                item_id: 931_104,
                type_id: 1230,
                owner_id: char_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 250,
                singleton: false,
                ..Default::default()
            },
        );
    });

    // 1. NotDocked error when in space
    let mut undocked = make_session(char_id, corp_id, None, None, Some(ship_id));
    assert_eq!(
        handle(&ctx, &mut undocked, "reprocessingSvc", "Reprocess", &[PyRep::Integer(931_104)], None)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotDocked"
    );
    assert_eq!(
        handle(&ctx, &mut undocked, "reprocessingSvc", "GetQuotes", &[], None)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotDocked"
    );

    // 2. Structure without online Reprocessing service module -> ReprocessingServiceOffline
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            931_199,
            InventoryItemRecord {
                item_id: 931_199,
                type_id: 35899,
                owner_id: corp_id,
                location_id: struct_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
        assert!(!structure_has_online_reprocessing_service(&ctx.sde, snap, struct_id, 35825));
    });
    let mut struct_sess = make_session(char_id, corp_id, None, Some(struct_id), Some(ship_id));
    assert_eq!(
        handle(&ctx, &mut struct_sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(931_104)], None)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ReprocessingServiceOffline"
    );

    // 3. Docked at Jita: validation error paths
    let mut sess = make_session(char_id, corp_id, Some(JITA_STATION_ID), None, Some(ship_id));
    for (args, expected_code) in [
        (vec![], "InvalidArgument"),
        (vec![PyRep::Integer(999_999_999)], "ItemNotFound"),
        (vec![PyRep::Integer(ship_id)], "CannotReprocessActiveShip"),
        (vec![PyRep::Integer(931_101)], "ItemNotReprocessable"),
        (vec![PyRep::Integer(931_102)], "ItemMustBeRepackaged"),
        (vec![PyRep::Integer(931_103)], "InsufficientBatchPortion"),
    ] {
        assert_eq!(
            handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &args, None)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            expected_code
        );
    }

    // 4. GetQuotes: 0 args (KeyVal), 1 list arg (List), and 2 args (Tuple)
    let q_empty = handle(&ctx, &mut sess, "reprocessingSvc", "GetQuotes", &[], None)
        .await
        .unwrap()
        .unwrap();
    assert!(q_empty.dict_get("stationBaseYield").is_some());

    let q_list = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "GetQuotes",
        &[PyRep::List(vec![PyRep::Integer(931_104)])],
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(q_list, PyRep::List(ref l) if l.len() == 1));

    let q_tuple = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "GetQuotes",
        &[PyRep::List(vec![PyRep::Integer(931_104)]), PyRep::Integer(ship_id)],
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(q_tuple, PyRep::Tuple(ref t) if t.len() == 3));

    // 5. Reprocess 250 Veldspar (931_104) -> consumes 200 units, leaves 50 remainder, mints Tritanium, deducts tax
    let bal_before = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
    let rep_res = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "Reprocess",
        &[PyRep::Integer(931_104)],
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rep_res.dict_get("remainderQuantity").and_then(|v| v.as_i64()), Some(50));
    let trit_qty = rep_res.dict_get("tritaniumQuantity").and_then(|v| v.as_i64()).unwrap();
    assert!(trit_qty > 0);

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&931_104).unwrap().quantity, 50);
        let bal_after = snap.characters.get(&char_id).unwrap().balance;
        assert!(bal_after < bal_before);
        assert!(snap
            .wallet_journal
            .iter()
            .any(|j| j.character_id == char_id && j.entry_type == "ReprocessingTax"));
    });
}
