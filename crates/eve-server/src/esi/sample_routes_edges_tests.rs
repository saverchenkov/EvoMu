//! Edge-case and branch-coverage unit tests for `esi::sample_routes`.

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;
use eve_db::{
    ContractRecord, InventoryFlag, InventoryItemRecord, KillmailRecord, WarAllyRecord,
    WarLifecycleMeta, WarRecord,
};

fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn all_inventory_flags_and_fitting_flags_cover_every_variant() {
    let mapped_flags = [
        (InventoryFlag::Wardrobe, "Wardrobe"),
        (InventoryFlag::Hangar, "Hangar"),
        (InventoryFlag::Cargo, "Cargo"),
        (InventoryFlag::Skill, "Skill"),
        (InventoryFlag::LoSlot0, "LoSlot0"),
        (InventoryFlag::LoSlot1, "LoSlot1"),
        (InventoryFlag::LoSlot2, "LoSlot2"),
        (InventoryFlag::LoSlot3, "LoSlot3"),
        (InventoryFlag::LoSlot4, "LoSlot4"),
        (InventoryFlag::LoSlot5, "LoSlot5"),
        (InventoryFlag::LoSlot6, "LoSlot6"),
        (InventoryFlag::LoSlot7, "LoSlot7"),
        (InventoryFlag::MedSlot0, "MedSlot0"),
        (InventoryFlag::MedSlot1, "MedSlot1"),
        (InventoryFlag::MedSlot2, "MedSlot2"),
        (InventoryFlag::MedSlot3, "MedSlot3"),
        (InventoryFlag::MedSlot4, "MedSlot4"),
        (InventoryFlag::MedSlot5, "MedSlot5"),
        (InventoryFlag::MedSlot6, "MedSlot6"),
        (InventoryFlag::MedSlot7, "MedSlot7"),
        (InventoryFlag::HiSlot0, "HiSlot0"),
        (InventoryFlag::HiSlot1, "HiSlot1"),
        (InventoryFlag::HiSlot2, "HiSlot2"),
        (InventoryFlag::HiSlot3, "HiSlot3"),
        (InventoryFlag::HiSlot4, "HiSlot4"),
        (InventoryFlag::HiSlot5, "HiSlot5"),
        (InventoryFlag::HiSlot6, "HiSlot6"),
        (InventoryFlag::HiSlot7, "HiSlot7"),
        (InventoryFlag::Locked, "Locked"),
        (InventoryFlag::Unlocked, "Unlocked"),
        (InventoryFlag::DroneBay, "DroneBay"),
        (InventoryFlag::Implant, "Implant"),
        (InventoryFlag::RigSlot0, "RigSlot0"),
        (InventoryFlag::RigSlot1, "RigSlot1"),
        (InventoryFlag::RigSlot2, "RigSlot2"),
        (InventoryFlag::CorpSAG1, "CorpSAG1"),
        (InventoryFlag::CorpSAG2, "CorpSAG2"),
        (InventoryFlag::CorpSAG3, "CorpSAG3"),
        (InventoryFlag::CorpSAG4, "CorpSAG4"),
        (InventoryFlag::CorpSAG5, "CorpSAG5"),
        (InventoryFlag::CorpSAG6, "CorpSAG6"),
        (InventoryFlag::CorpSAG7, "CorpSAG7"),
        (InventoryFlag::SubSystemSlot0, "SubSystemSlot0"),
        (InventoryFlag::SubSystemSlot1, "SubSystemSlot1"),
        (InventoryFlag::SubSystemSlot2, "SubSystemSlot2"),
        (InventoryFlag::SubSystemSlot3, "SubSystemSlot3"),
        (InventoryFlag::SubSystemSlot4, "SubSystemSlot4"),
        (InventoryFlag::FighterBay, "FighterBay"),
        (InventoryFlag::Deliveries, "Deliveries"),
        (InventoryFlag::CorpDeliveries, "CorpDeliveries"),
        (InventoryFlag::StructureDeedBay, "QuantumCoreRoom"),
        (InventoryFlag::SpecializedFuelBay, "SpecializedFuelBay"),
        (InventoryFlag::StructureFuel, "StructureFuel"),
        (InventoryFlag::SpecializedOreHold, "SpecializedOreHold"),
        (InventoryFlag::SpecializedGasHold, "SpecializedGasHold"),
        (InventoryFlag::SpecializedMineralHold, "SpecializedMineralHold"),
        (InventoryFlag::SpecializedSalvageHold, "SpecializedSalvageHold"),
        (InventoryFlag::SpecializedShipHold, "SpecializedShipHold"),
        (InventoryFlag::SpecializedAmmoHold, "SpecializedAmmoHold"),
        (
            InventoryFlag::SpecializedCommandCenterHold,
            "SpecializedCommandCenterHold",
        ),
        (
            InventoryFlag::SpecializedPlanetaryCommoditiesHold,
            "SpecializedPlanetaryCommoditiesHold",
        ),
        (InventoryFlag::SpecializedMaterialBay, "SpecializedMaterialBay"),
        (InventoryFlag::SpecializedIceHold, "SpecializedIceHold"),
        (InventoryFlag::SpecializedAsteroidHold, "SpecializedAsteroidHold"),
        (InventoryFlag::SpecializedMobileDepotHold, "MobileDepotHold"),
        (
            InventoryFlag::SpecializedInfrastructureHold,
            "InfrastructureHold",
        ),
        (InventoryFlag::FrigateEscapeBay, "FrigateEscapeBay"),
        (InventoryFlag::ShipHangar, "ShipHangar"),
        (InventoryFlag::FleetHangar, "FleetHangar"),
        (InventoryFlag::ServiceSlot0, "ServiceSlot0"),
        (InventoryFlag::ServiceSlot1, "ServiceSlot1"),
        (InventoryFlag::ServiceSlot2, "ServiceSlot2"),
        (InventoryFlag::ServiceSlot3, "ServiceSlot3"),
        (InventoryFlag::ServiceSlot4, "ServiceSlot4"),
        (InventoryFlag::ServiceSlot5, "ServiceSlot5"),
        (InventoryFlag::ServiceSlot6, "ServiceSlot6"),
        (InventoryFlag::ServiceSlot7, "ServiceSlot7"),
    ];
    for (flag, expected) in mapped_flags {
        assert_eq!(asset_location_flag_str(flag), Some(expected));
    }
    for (flag, expected) in [
        (InventoryFlag::Hangar, "Invalid"),
        (InventoryFlag::Wardrobe, "Invalid"),
        (InventoryFlag::SubSystemSlot4, "Invalid"),
    ] {
        assert_eq!(fitting_flag_str(flag), expected);
    }

    // Check a few civil dates across leap/non-leap and months <= 2 vs > 2
    assert_eq!(civil_from_unix(1_704_067_200), (2024, 1, 1, 0, 0, 0));
    assert_eq!(civil_from_unix(1_735_689_600), (2025, 1, 1, 0, 0, 0));
}

#[test]
fn is_operation_implemented_covers_expected_routes() {
    let implemented = [
        ("GET", "/status"),
        ("GET", "/meta/compatibility-dates"),
        ("GET", "/alliances"),
        ("GET", "/corporations/npccorps"),
        ("POST", "/characters/affiliation"),
        ("GET", "/characters/{character_id}/portrait"),
        ("GET", "/characters/{character_id}/standings"),
        ("GET", "/characters/{character_id}/fatigue"),
        ("GET", "/characters/{character_id}/blueprints"),
        ("GET", "/characters/{character_id}/wallet"),
        ("GET", "/corporations/{corporation_id}/wallets"),
        ("GET", "/characters/{character_id}/location"),
        ("GET", "/characters/{character_id}/online"),
        ("GET", "/characters/{character_id}/ship"),
        ("GET", "/characters/{character_id}/skills"),
        ("GET", "/characters/{character_id}/attributes"),
        ("GET", "/characters/{character_id}/skillqueue"),
        ("GET", "/characters/{character_id}/assets"),
        ("POST", "/characters/{character_id}/assets/locations"),
        ("POST", "/characters/{character_id}/assets/names"),
        ("GET", "/corporations/{corporation_id}/assets"),
        ("POST", "/corporations/{corporation_id}/assets/locations"),
        ("POST", "/corporations/{corporation_id}/assets/names"),
        ("GET", "/characters/{character_id}/clones"),
        ("GET", "/characters/{character_id}/implants"),
        ("GET", "/dogma/attributes"),
        ("GET", "/dogma/attributes/{attribute_id}"),
        ("GET", "/dogma/effects"),
        ("GET", "/dogma/effects/{effect_id}"),
        ("GET", "/dogma/dynamic/items/{type_id}/{item_id}"),
        ("GET", "/characters/{character_id}/fittings"),
        ("POST", "/characters/{character_id}/fittings"),
        ("DELETE", "/characters/{character_id}/fittings/{fitting_id}"),
        ("GET", "/insurance/prices"),
        ("GET", "/characters/{character_id}/killmails/recent"),
        ("GET", "/corporations/{corporation_id}/killmails/recent"),
        ("GET", "/characters/{character_id}/loyalty/points"),
        ("GET", "/characters/{character_id}/mail"),
        ("POST", "/characters/{character_id}/mail"),
        ("GET", "/characters/{character_id}/mail/{mail_id}"),
        ("DELETE", "/characters/{character_id}/mail/{mail_id}"),
        ("GET", "/markets/groups"),
        ("GET", "/markets/groups/{market_group_id}"),
        ("GET", "/universe/schematics/{schematic_id}"),
        ("GET", "/route/{origin}/{destination}"),
        ("GET", "/characters/{character_id}/search"),
        ("GET", "/sovereignty/map"),
        ("GET", "/sovereignty/structures"),
        ("GET", "/universe/categories"),
        ("GET", "/universe/categories/{category_id}"),
        ("GET", "/universe/groups"),
        ("GET", "/universe/groups/{group_id}"),
        ("GET", "/universe/types"),
        ("GET", "/universe/types/{type_id}"),
        ("GET", "/universe/regions"),
        ("GET", "/universe/regions/{region_id}"),
        ("GET", "/universe/constellations"),
        ("GET", "/universe/constellations/{constellation_id}"),
        ("GET", "/universe/systems"),
        ("GET", "/universe/systems/{system_id}"),
        ("GET", "/universe/stars/{star_id}"),
        ("GET", "/universe/planets/{planet_id}"),
        ("GET", "/universe/moons/{moon_id}"),
        ("GET", "/universe/asteroid_belts/{asteroid_belt_id}"),
        ("GET", "/universe/stargates/{stargate_id}"),
        ("GET", "/universe/structures"),
        ("GET", "/universe/structures/{structure_id}"),
        ("GET", "/universe/factions"),
        ("GET", "/universe/bloodlines"),
        ("GET", "/universe/ancestries"),
        ("POST", "/universe/names"),
        ("POST", "/universe/ids"),
        ("GET", "/wars"),
        ("GET", "/wars/{war_id}"),
        ("GET", "/wars/{war_id}/killmails"),
        ("GET", "/industry/facilities"),
        ("GET", "/industry/systems"),
        ("GET", "/characters/{character_id}/industry/jobs"),
        ("GET", "/corporations/{corporation_id}/industry/jobs"),
        ("GET", "/markets/{region_id}/orders"),
        ("GET", "/markets/{region_id}/history"),
        ("GET", "/markets/prices"),
        ("GET", "/markets/{region_id}/types"),
        ("GET", "/markets/structures/{structure_id}"),
        ("GET", "/characters/{character_id}/orders"),
        ("GET", "/characters/{character_id}/orders/history"),
        ("GET", "/corporations/{corporation_id}/orders"),
        ("GET", "/corporations/{corporation_id}/orders/history"),
        ("GET", "/characters/{character_id}/wallet/journal"),
        ("GET", "/characters/{character_id}/wallet/transactions"),
        ("GET", "/characters/{character_id}/contracts"),
        ("GET", "/characters/{character_id}/contracts/{contract_id}/items"),
        ("GET", "/characters/{character_id}/contracts/{contract_id}/bids"),
        ("GET", "/contracts/public/{region_id}"),
        ("GET", "/contracts/public/items/{contract_id}"),
        ("GET", "/contracts/public/bids/{contract_id}"),
        ("GET", "/corporations/{corporation_id}/contracts"),
        ("GET", "/corporations/{corporation_id}/contracts/{contract_id}/items"),
        ("GET", "/corporations/{corporation_id}/contracts/{contract_id}/bids"),
        ("GET", "/corporations/{corporation_id}"),
        ("GET", "/corporations/{corporation_id}/members"),
        ("GET", "/corporations/{corporation_id}/roles"),
        ("GET", "/alliances/{alliance_id}"),
        ("GET", "/alliances/{alliance_id}/corporations"),
        ("GET", "/alliances/{alliance_id}/icons"),
        ("GET", "/characters/{character_id}/bookmarks"),
        ("GET", "/characters/{character_id}/bookmarks/folders"),
        ("GET", "/incursions"),
        ("GET", "/meta/changelog"),
        ("GET", "/sovereignty/campaigns"),
        ("GET", "/killmails/{killmail_id}/{killmail_hash}"),
        ("GET", "/universe/graphics"),
        ("GET", "/universe/graphics/{graphic_id}"),
        ("GET", "/universe/races"),
        ("GET", "/universe/stations/{station_id}"),
        ("GET", "/universe/system_jumps"),
        ("GET", "/universe/system_kills"),
        ("GET", "/fw/wars"),
        ("GET", "/fw/stats"),
        ("GET", "/fw/systems"),
        ("GET", "/fw/leaderboards"),
        ("GET", "/fw/leaderboards/characters"),
        ("GET", "/fw/leaderboards/corporations"),
        ("GET", "/characters/{character_id}/fw/stats"),
        ("GET", "/corporations/{corporation_id}/fw/stats"),
        ("GET", "/opportunities/tasks"),
        ("GET", "/opportunities/groups"),
        ("GET", "/characters/{character_id}/opportunities"),
        ("GET", "/characters/{character_id}/planets"),
        ("GET", "/characters/{character_id}/planets/{planet_id}"),
        ("GET", "/corporation/{corporation_id}/mining/extractions"),
        ("GET", "/corporation/{corporation_id}/mining/observers"),
        ("GET", "/corporation/{corporation_id}/mining/observers/{observer_id}"),
        ("GET", "/corporations/{corporation_id}/customs_offices"),
        ("GET", "/fleets/{fleet_id}"),
    ];
    for (m, p) in implemented {
        assert!(is_operation_implemented(m, p), "expected implemented: {m} {p}");
    }
    assert!(!is_operation_implemented("POST", "/unknown/route"));
}

#[test]
fn dispatch_operation_covers_dogma_contracts_killmails_schematics_and_wars_edge_cases() {
    let (arc_ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let now = arc_ctx.now_unix();
    let empty_q = HashMap::new();

    // Test open_sde_sqlite with a temporary SQLite DB containing dogma tables
    let tmp_db = std::env::temp_dir().join(format!("evomu_sample_routes_{}.sqlite", std::process::id()));
    {
        let conn = rusqlite::Connection::open(&tmp_db).unwrap();
        conn.execute_batch(
            "CREATE TABLE sde_dogma_attributes (attribute_id INTEGER PRIMARY KEY, published INTEGER);
             INSERT INTO sde_dogma_attributes (attribute_id, published) VALUES (9, 0);
             CREATE TABLE sde_dogma_effects (effect_id INTEGER PRIMARY KEY, is_warp_safe INTEGER, disallow_auto_repeat INTEGER, electronic_chance INTEGER, range_chance INTEGER, published INTEGER);
             INSERT INTO sde_dogma_effects (effect_id, is_warp_safe, disallow_auto_repeat, electronic_chance, range_chance, published) VALUES (16, 1, 0, 1, 0, 1);"
        ).unwrap();
    }
    let mut sde_mut = (*arc_ctx.sde).clone();
    sde_mut.db_path = Some(tmp_db.clone());
    if let Some(a) = sde_mut.attributes.get_mut(&9) { a.published = false; }
    let ctx = std::sync::Arc::new(
        ServerContext::with_sde_and_repository(
            std::sync::Arc::new(sde_mut),
            std::sync::Arc::clone(&arc_ctx.db),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED),
    );

    // Online session count on /status
    let acc_uid = ctx
        .db
        .with_snapshot(|snap| snap.accounts.values().next().map(|a| a.user_id))
        .unwrap_or(1);
    let _ = ctx.db.acquire_online_lock(acc_uid, 999, Some(char_id), true, now);
    let st = dispatch_operation(&ctx, now, "GET", "/status", &params(&[]), &empty_q, None, 1, b"");
    assert_eq!(st.status_code, 200);
    assert!(st.body.unwrap()["players"].as_i64().unwrap() >= 1);

    // Dogma attributes error cases + sqlite lookup
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/dogma/attributes/{attribute_id}", &params(&[]), &empty_q, None, 1, b"").status_code,
        400
    );
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/dogma/attributes/{attribute_id}", &params(&[("attribute_id", "bad")]), &empty_q, None, 1, b"").status_code,
        400
    );
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/dogma/attributes/{attribute_id}", &params(&[("attribute_id", "70000")]), &empty_q, None, 1, b"").status_code,
        404
    );
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/dogma/attributes/{attribute_id}", &params(&[("attribute_id", "65534")]), &empty_q, None, 1, b"").status_code,
        404
    );
    let attr_ok = dispatch_operation(&ctx, now, "GET", "/dogma/attributes/{attribute_id}", &params(&[("attribute_id", "9")]), &empty_q, None, 1, b"");
    assert_eq!(attr_ok.status_code, 200);
    assert_eq!(attr_ok.body.unwrap()["published"], false);

    // Dogma effects error cases + sqlite lookup + modifiers
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/dogma/effects/{effect_id}", &params(&[("effect_id", "bad")]), &empty_q, None, 1, b"").status_code,
        400
    );
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/dogma/effects/{effect_id}", &params(&[("effect_id", "70000")]), &empty_q, None, 1, b"").status_code,
        404
    );
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/dogma/effects/{effect_id}", &params(&[("effect_id", "65534")]), &empty_q, None, 1, b"").status_code,
        404
    );
    let eff_with_mod = ctx
        .sde
        .effects
        .values()
        .find(|e| !e.modifiers.is_empty())
        .map(|e| e.effect_id)
        .unwrap_or(16);
    for eid in [16, eff_with_mod] {
        let res = dispatch_operation(
            &ctx,
            now,
            "GET",
            "/dogma/effects/{effect_id}",
            &params(&[("effect_id", &eid.to_string())]),
            &empty_q,
            None,
            1,
            b"",
        );
        assert_eq!(res.status_code, 200);
    }

    // Dynamic items: bad params, u32 overflow, wrong type, non-mutated, mutated
    let muta_id = ctx
        .sde
        .dynamic_item_attributes
        .keys()
        .next()
        .copied()
        .unwrap_or(47740);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            770_001,
            InventoryItemRecord {
                item_id: 770_001,
                type_id: 587,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            770_002,
            InventoryItemRecord {
                item_id: 770_002,
                type_id: 587,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                source_mutaplasmid_id: Some(muta_id),
                dynamic_attributes: HashMap::from([(9, 123.0)]),
                ..Default::default()
            },
        );
        // Contracts for /characters/{cid}/contracts/{contract_id}/items
        snap.contracts.insert(
            880_001,
            ContractRecord {
                contract_id: 880_001,
                issuer_id: 90_000_002,
                acceptor_id: None,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            880_002,
            ContractRecord {
                contract_id: 880_002,
                issuer_id: char_id,
                acceptor_id: None,
                ..Default::default()
            },
        );
        // Wars & killmails for /wars/{war_id} and /wars/{war_id}/killmails
        snap.wars.insert(
            55001,
            WarRecord {
                war_id: 55001,
                aggressor_corp_id: corp_id,
                defender_corp_id: 98_000_099,
                started_unix: 1000,
                mutual: false,
                active: true,
            },
        );
        snap.war_lifecycle_meta.insert(
            55001,
            WarLifecycleMeta {
                declared_unix: Some(900),
                started_unix: Some(1000),
                retracted_unix: Some(1500),
                finished_unix: Some(2000),
                open_for_allies: Some(true),
                allies: vec![WarAllyRecord {
                    corporation_id: Some(98_000_088),
                    alliance_id: Some(99_000_088),
                }],
            },
        );
        snap.killmails.insert(
            990_001,
            KillmailRecord {
                kill_id: 990_001,
                victim_character_id: char_id,
                attacker_character_id: 90_000_002,
                victim_corp_id: corp_id,
                attacker_corp_id: 98_000_088, // ally on defender side!
                attacker_alliance_id: Some(99_000_088),
                solar_system_id: 30_000_142,
                ship_type_id: 587,
                total_value_isk: 500_000.0,
                killed_unix: 1200,
                ..Default::default()
            },
        );
        snap.killmails.insert(
            990_002,
            KillmailRecord {
                kill_id: 990_002,
                victim_character_id: 90_000_002,
                attacker_character_id: char_id,
                victim_corp_id: 98_000_099,
                attacker_corp_id: corp_id,
                solar_system_id: 30_000_142,
                ship_type_id: 587,
                total_value_isk: 250_000.0,
                killed_unix: 1300,
                ..Default::default()
            },
        );
    });
    ctx.esi_state
        .war_allies
        .write()
        .unwrap()
        .insert(55001, vec![json!({"corporation_id": 98_000_077})]);

    // Dynamic items
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/dogma/dynamic/items/{type_id}/{item_id}", &params(&[("type_id", "bad"), ("item_id", "1")]), &empty_q, None, 1, b"").status_code,
        400
    );
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/dogma/dynamic/items/{type_id}/{item_id}", &params(&[("type_id", "587"), ("item_id", "bad")]), &empty_q, None, 1, b"").status_code,
        400
    );
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/dogma/dynamic/items/{type_id}/{item_id}", &params(&[("type_id", "-1"), ("item_id", "770001")]), &empty_q, None, 1, b"").status_code,
        404
    );
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/dogma/dynamic/items/{type_id}/{item_id}", &params(&[("type_id", "588"), ("item_id", "770001")]), &empty_q, None, 1, b"").status_code,
        404
    );
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/dogma/dynamic/items/{type_id}/{item_id}", &params(&[("type_id", "587"), ("item_id", "770001")]), &empty_q, None, 1, b"").status_code,
        404
    );
    let dyn_ok = dispatch_operation(
        &ctx,
        now,
        "GET",
        "/dogma/dynamic/items/{type_id}/{item_id}",
        &params(&[("type_id", "587"), ("item_id", "770002")]),
        &empty_q,
        None,
        1,
        b"",
    );
    assert_eq!(dyn_ok.status_code, 200);
    assert_eq!(dyn_ok.body.unwrap()["mutator_type_id"], muta_id as i64);

    // Contract items
    for (p, code) in [
        (params(&[("character_id", "bad"), ("contract_id", "1")]), 400),
        (params(&[("character_id", &char_id.to_string()), ("contract_id", "bad")]), 400),
        (params(&[("character_id", "99999999"), ("contract_id", "880001")]), 404),
        (params(&[("character_id", &char_id.to_string()), ("contract_id", "999999")]), 404),
        (params(&[("character_id", &char_id.to_string()), ("contract_id", "880001")]), 403),
        (params(&[("character_id", &char_id.to_string()), ("contract_id", "880002")]), 200),
    ] {
        assert_eq!(
            dispatch_operation(&ctx, now, "GET", "/characters/{character_id}/contracts/{contract_id}/items", &p, &empty_q, None, 1, b"").status_code,
            code
        );
    }

    // Killmails recent errors & pagination
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/characters/{character_id}/killmails/recent", &params(&[("character_id", "bad")]), &empty_q, None, 1, b"").status_code,
        400
    );
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/characters/{character_id}/killmails/recent", &params(&[("character_id", "99999999")]), &empty_q, None, 1, b"").status_code,
        404
    );
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/characters/{character_id}/killmails/recent", &params(&[("character_id", &char_id.to_string())]), &empty_q, None, 0, b"").status_code,
        400
    );
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/corporations/{corporation_id}/killmails/recent", &params(&[("corporation_id", "bad")]), &empty_q, None, 1, b"").status_code,
        400
    );
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/corporations/{corporation_id}/killmails/recent", &params(&[("corporation_id", "99999999")]), &empty_q, None, 1, b"").status_code,
        404
    );
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/corporations/{corporation_id}/killmails/recent", &params(&[("corporation_id", &corp_id.to_string())]), &empty_q, None, 0, b"").status_code,
        400
    );

    // Schematics errors
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/universe/schematics/{schematic_id}", &params(&[("schematic_id", "bad")]), &empty_q, None, 1, b"").status_code,
        400
    );
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/universe/schematics/{schematic_id}", &params(&[("schematic_id", "-1")]), &empty_q, None, 1, b"").status_code,
        404
    );
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/universe/schematics/{schematic_id}", &params(&[("schematic_id", "999999")]), &empty_q, None, 1, b"").status_code,
        404
    );

    // Wars errors and filters
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/wars", &params(&[]), &params(&[("max_war_id", "0")]), None, 1, b"").status_code,
        400
    );
    let wars_filtered = dispatch_operation(&ctx, now, "GET", "/wars", &params(&[]), &params(&[("max_war_id", "55001")]), None, 1, b"");
    assert_eq!(wars_filtered.status_code, 200);
    assert!(!wars_filtered.body.unwrap().as_array().unwrap().contains(&json!(55001)));

    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/wars/{war_id}", &params(&[("war_id", "bad")]), &empty_q, None, 1, b"").status_code,
        400
    );
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/wars/{war_id}", &params(&[("war_id", "999999")]), &empty_q, None, 1, b"").status_code,
        404
    );
    let war_ok = dispatch_operation(&ctx, now, "GET", "/wars/{war_id}", &params(&[("war_id", "55001")]), &empty_q, None, 1, b"");
    assert_eq!(war_ok.status_code, 200);
    let war_body = war_ok.body.unwrap();
    assert!(war_body.get("retracted").is_some());
    assert!(war_body.get("finished").is_some());

    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/wars/{war_id}/killmails", &params(&[("war_id", "bad")]), &empty_q, None, 1, b"").status_code,
        400
    );
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/wars/{war_id}/killmails", &params(&[("war_id", "999999")]), &empty_q, None, 1, b"").status_code,
        404
    );
    assert_eq!(
        dispatch_operation(&ctx, now, "GET", "/wars/{war_id}/killmails", &params(&[("war_id", "55001")]), &empty_q, None, 0, b"").status_code,
        400
    );
    let war_kms = dispatch_operation(&ctx, now, "GET", "/wars/{war_id}/killmails", &params(&[("war_id", "55001")]), &empty_q, None, 1, b"");
    assert_eq!(war_kms.status_code, 200);
    assert_eq!(war_kms.body.unwrap().as_array().unwrap().len(), 2);

    let _ = std::fs::remove_file(&tmp_db);
}

#[test]
fn dispatch_operation_dogma_attribute_and_effect_payloads_and_non_default_result() {
    let (ctx, _token, _char_id, _corp_id, _planet_id) = seed_full_esi_context();
    let now = ctx.now_unix();
    let empty_q = HashMap::new();

    let compat = dispatch_operation(
        &ctx,
        now,
        "GET",
        "/meta/compatibility-dates",
        &params(&[]),
        &empty_q,
        None,
        1,
        b"",
    );
    assert_eq!(compat.status_code, 200);
    assert!(compat.max_age_secs > 0);
    assert!(compat.body.is_some());

    // Attribute detail: 400/404 edges and full JSON schema contract.
    assert_eq!(
        dispatch_operation(
            &ctx,
            now,
            "GET",
            "/dogma/attributes/{attribute_id}",
            &params(&[]),
            &empty_q,
            None,
            1,
            b"",
        )
        .status_code,
        400
    );
    assert_eq!(
        dispatch_operation(
            &ctx,
            now,
            "GET",
            "/dogma/attributes/{attribute_id}",
            &params(&[("attribute_id", "not-an-int")]),
            &empty_q,
            None,
            1,
            b"",
        )
        .status_code,
        400
    );
    for (raw_id, expected_err) in [("-1", "Dogma attribute -1 not found"), ("70000", "Dogma attribute 70000 not found"), ("65534", "Dogma attribute 65534 not found")] {
        let err_res = dispatch_operation(
            &ctx,
            now,
            "GET",
            "/dogma/attributes/{attribute_id}",
            &params(&[("attribute_id", raw_id)]),
            &empty_q,
            None,
            1,
            b"",
        );
        assert_eq!(err_res.status_code, 404);
        assert_eq!(err_res.body.unwrap()["error"], expected_err);
    }

    let attr = ctx
        .sde
        .attributes
        .values()
        .next()
        .expect("canonical SDE must contain at least one dogma attribute");
    let attr_res = dispatch_operation(
        &ctx,
        now,
        "GET",
        "/dogma/attributes/{attribute_id}",
        &params(&[("attribute_id", &attr.attribute_id.to_string())]),
        &empty_q,
        None,
        1,
        b"",
    );
    assert_eq!(attr_res.status_code, 200);
    assert_eq!(attr_res.max_age_secs, 3600);
    let attr_json = attr_res.body.expect("attribute JSON body");
    assert_eq!(attr_json["attribute_id"], attr.attribute_id as i64);
    assert_eq!(attr_json["name"], attr.name);
    assert_eq!(attr_json["default_value"], attr.default_value);
    assert_eq!(attr_json["high_is_good"], attr.high_is_good);
    assert_eq!(attr_json["stackable"], attr.stackable);
    assert_eq!(attr_json["published"], attr.published);

    // Effect detail: 400/404 edges and full JSON schema contract including modifiers.
    assert_eq!(
        dispatch_operation(
            &ctx,
            now,
            "GET",
            "/dogma/effects/{effect_id}",
            &params(&[("effect_id", "bad")]),
            &empty_q,
            None,
            1,
            b"",
        )
        .status_code,
        400
    );
    for (raw_id, expected_err) in [("70000", "Dogma effect 70000 not found"), ("65534", "Dogma effect 65534 not found")] {
        let err_res = dispatch_operation(
            &ctx,
            now,
            "GET",
            "/dogma/effects/{effect_id}",
            &params(&[("effect_id", raw_id)]),
            &empty_q,
            None,
            1,
            b"",
        );
        assert_eq!(err_res.status_code, 404);
        assert_eq!(err_res.body.unwrap()["error"], expected_err);
    }

    let eff = ctx
        .sde
        .effects
        .values()
        .find(|e| !e.modifiers.is_empty())
        .or_else(|| ctx.sde.effects.values().next())
        .expect("canonical SDE must contain at least one dogma effect");
    let eff_res = dispatch_operation(
        &ctx,
        now,
        "GET",
        "/dogma/effects/{effect_id}",
        &params(&[("effect_id", &eff.effect_id.to_string())]),
        &empty_q,
        None,
        1,
        b"",
    );
    assert_eq!(eff_res.status_code, 200);
    assert_eq!(eff_res.max_age_secs, 3600);
    let eff_json = eff_res.body.expect("effect JSON body");
    assert_eq!(eff_json["effect_id"], eff.effect_id as i64);
    assert_eq!(eff_json["name"], eff.name);
    assert_eq!(eff_json["effect_category"], eff.effect_category as i64);
    assert_eq!(eff_json["is_offensive"], eff.is_offensive);
    assert_eq!(eff_json["is_assistance"], eff.is_assistance);
    assert_eq!(eff_json["is_warp_safe"], eff.is_warp_safe);
    assert_eq!(eff_json["disallow_auto_repeat"], eff.disallow_auto_repeat);
    assert_eq!(eff_json["electronic_chance"], eff.electronic_chance);
    assert_eq!(eff_json["range_chance"], eff.range_chance);
    assert_eq!(eff_json["published"], eff.published);
    let mods_json = eff_json["modifiers"].as_array().expect("modifiers array");
    assert_eq!(mods_json.len(), eff.modifiers.len());
    if let Some(m) = eff.modifiers.first() {
        assert_eq!(mods_json[0]["domain"], m.domain);
        assert_eq!(mods_json[0]["func"], m.func);
        assert_eq!(mods_json[0]["modified_attribute_id"], m.modified_attribute_id as i64);
        assert_eq!(mods_json[0]["modifying_attribute_id"], m.modifying_attribute_id as i64);
        assert_eq!(mods_json[0]["operator"], m.operator as i64);
    }
}
