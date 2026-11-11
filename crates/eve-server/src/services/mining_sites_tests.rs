use super::*;
use crate::router::ServerContext;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, SystemDynamicState,
    UpwellStructureRecord, WorldRepository,
};
use eve_sde::{
    DungeonAsteroidEntry, DungeonTemplate, InvType, RegionInfo, SdeRegistry, SolarSystem,
};
use std::collections::HashMap;
use std::sync::Arc;

fn make_inv_type(
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    volume: f64,
    radius: f64,
) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: name.into(),
        mass: 1.0,
        volume,
        capacity: 0.0,
        radius,
        base_price: 100.0,
        published: true,
        portion_size: 1,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: None,
        default_effect_id: None,
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes: HashMap::new(),
    }
}

fn make_sde_and_ctx() -> ServerContext {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.regions.insert(
        10_000_002,
        RegionInfo {
            region_id: 10_000_002,
            name: "The Forge".into(),
            center: [0.0, 0.0, 0.0],
            faction_id: None,
            wormhole_class_id: None,
        },
    );
    sde.solar_systems.insert(
        30_000_180,
        SolarSystem {
            solar_system_id: 30_000_180,
            region_id: 10_000_002,
            constellation_id: 20_000_020,
            name: "Osmon".into(),
            security_status: 0.72,
            radius: 1e12,
            galactic_pos_ly: [0.0, 0.0, 0.0],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: None,
        },
    );
    sde.solar_systems.insert(
        30_004_759,
        SolarSystem {
            solar_system_id: 30_004_759,
            region_id: 10_000_060,
            constellation_id: 20_000_696,
            name: "1DQ1-A".into(),
            security_status: -0.50,
            radius: 1e12,
            galactic_pos_ly: [0.0, 0.0, 0.0],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: None,
        },
    );
    // Blue Ice Belt dungeon 5496
    sde.dungeons.insert(
        5496,
        DungeonTemplate {
            dungeon_id: 5496,
            name: "Blue Ice Belt ".into(),
            archetype_id: Some(1),
            faction_id: None,
            difficulty: None,
        },
    );
    sde.dungeon_asteroids.insert(
        5496,
        vec![DungeonAsteroidEntry {
            room_id: 1,
            type_id: 16264,
            asteroid_count: 21,
        }],
    );
    // Large Veldspar Deposit dungeon 7001
    sde.dungeons.insert(
        7001,
        DungeonTemplate {
            dungeon_id: 7001,
            name: "Large Veldspar Deposit".into(),
            archetype_id: Some(1),
            faction_id: None,
            difficulty: None,
        },
    );
    sde.dungeon_asteroids.insert(
        7001,
        vec![
            DungeonAsteroidEntry {
                room_id: 1,
                type_id: 18,
                asteroid_count: 2,
            },
            DungeonAsteroidEntry {
                room_id: 1,
                type_id: 1230,
                asteroid_count: 3,
            },
            DungeonAsteroidEntry {
                room_id: 1,
                type_id: 1228,
                asteroid_count: 2,
            },
        ],
    );
    // Saintly Nebula gas dungeon 8001
    sde.dungeons.insert(
        8001,
        DungeonTemplate {
            dungeon_id: 8001,
            name: "Saintly Nebula".into(),
            archetype_id: Some(2),
            faction_id: None,
            difficulty: None,
        },
    );
    sde.dungeon_gas_types.insert(8001, vec![25268]);
    // Barren Perimeter Reservoir wormhole gas dungeon 8002
    sde.dungeons.insert(
        8002,
        DungeonTemplate {
            dungeon_id: 8002,
            name: "Barren Perimeter Reservoir".into(),
            archetype_id: Some(2),
            faction_id: None,
            difficulty: None,
        },
    );
    sde.dungeon_gas_types.insert(8002, vec![30370, 30371]);

    sde.types
        .insert(18, make_inv_type(18, 458, 25, "Plagioclase", 0.35, 100.0));
    sde.types
        .insert(1230, make_inv_type(1230, 462, 25, "Veldspar", 0.1, 100.0));
    sde.types
        .insert(1228, make_inv_type(1228, 460, 25, "Scordite", 0.15, 100.0));
    sde.types.insert(
        87702,
        make_inv_type(87702, 4768, 66, "Tritanium Prospecting Array 3", 100.0, 10.0),
    );

    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(Arc::new(sde), db)
}

#[test]
fn test_displayed_security_and_site_bands_and_timers() {
    assert!((displayed_security(0.0) - 0.0).abs() < 1e-9);
    assert!((displayed_security(0.001) - 0.1).abs() < 1e-9);
    assert!((displayed_security(0.049) - 0.1).abs() < 1e-9);
    assert!((displayed_security(0.05) - 0.1).abs() < 1e-9);
    assert!((displayed_security(0.45) - 0.5).abs() < 1e-9);
    assert!((displayed_security(0.84) - 0.8).abs() < 1e-9);
    assert!((displayed_security(-0.14) - (-0.1)).abs() < 1e-9);

    assert!(!SiteBand::HighSec.matches(31_000_000, 1.0));
    assert!(SiteBand::HighSecMid.matches(30_000_180, 0.50));
    assert!(SiteBand::HighSecMid.matches(30_000_180, 0.80));
    assert!(!SiteBand::HighSecMid.matches(30_000_180, 0.90));
    assert!(SiteBand::HighSec.matches(30_000_180, 0.90));
    assert!(SiteBand::LowSec.matches(30_000_180, 0.01));
    assert!(SiteBand::LowSec.matches(30_000_180, 0.44));
    assert!(!SiteBand::LowSec.matches(30_000_180, 0.45));
    assert!(SiteBand::NullSec.matches(30_004_759, 0.0));
    assert!(!SiteBand::NullSec.matches(30_004_759, 0.01));

    let pochven_sys = SolarSystem {
        solar_system_id: 30_000_157,
        region_id: 10_000_070,
        constellation_id: 20_000_020,
        name: "Kino".into(),
        security_status: -1.0,
        radius: 1e12,
        galactic_pos_ly: [0.0, 0.0, 0.0],
        planet_ids: vec![],
        stations: vec![],
        wormhole_class_id: None,
    };
    assert!(!SiteBand::NullSec.matches_system(&pochven_sys));

    assert!(!is_ore_anomaly_expired(ORE_ANOMALY_EXPIRY_SECS - 1));
    assert!(is_ore_anomaly_expired(ORE_ANOMALY_EXPIRY_SECS));
    assert!(!is_ice_belt_ready_to_respawn(
        ICE_BELT_RESPAWN_DELAY_SECS - 1
    ));
    assert!(is_ice_belt_ready_to_respawn(ICE_BELT_RESPAWN_DELAY_SECS));

    for sys in [
        30_000_180, 30_000_156, 30_002_510, 30_002_659, 30_003_495, 30_000_192,
    ] {
        assert!(is_designated_ice_system(sys));
    }
    assert!(!is_designated_ice_system(30_000_142));

    let off0 = asteroid_offset(0);
    let off1 = asteroid_offset(1);
    assert!((off0[0] - 1_500.0).abs() < 1e-6);
    assert_eq!(off0[1], 0.0);
    assert!(off1[0] != off0[0]);
}

#[test]
fn test_resolve_ore_gas_ice_and_wormhole_sites() {
    let ctx = make_sde_and_ctx();
    assert_eq!(region_name(&ctx.sde, 30_000_180), Some("The Forge"));
    assert_eq!(find_dungeon_id(&ctx.sde, "Blue Ice Belt"), Some(5496));

    let ice = resolve_regional_ice_belt(&ctx.sde, 30_000_180)
        .expect("should match Blue Ice Belt")
        .expect("should resolve");
    assert_eq!(ice.dungeon_id, 5496);
    assert_eq!(ice.asteroids.len(), 21);
    assert_eq!(ice.ore_types(), vec![16264]);
    assert!(ice.asteroids.iter().all(|a| a.units == 1_000));

    let gas = resolve_gas_site(&ctx.sde, &GAS_SITES[0]).expect("should resolve Saintly Nebula");
    assert!(gas.dungeon_id > 0);
    assert!(gas.gas_type_id > 0);
    assert!(!gas.cloud_units.is_empty());

    let (wh_did, wh_clouds) =
        resolve_wormhole_gas_site(&ctx.sde, &WORMHOLE_GAS_SITES[0]).expect("should resolve WH gas");
    assert!(wh_did > 0);
    assert!(!wh_clouds.is_empty());

    let sov_site = resolve_ore_site(&ctx.sde, &SOV_PROSPECTING_DEPOSITS[0].site)
        .expect("Large Veldspar Deposit should resolve");
    assert_eq!(sov_site.ore_types(), vec![18, 1228, 1230]);
}

#[tokio::test]
async fn test_sov_hub_save_and_get_installed_upgrades() {
    let ctx = make_sde_and_ctx();
    let char_id = 90_000_001;
    let corp_id = 1_000_009;
    let alliance_id = 99_000_001;
    let hub_id = 1_000_000_000_555;
    let null_sys = 30_004_759;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(alliance_id);
            ch.solar_system_id = null_sys;
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Sov Corp".into();
                __corp.ticker = "SOV".into();
                __corp.ceo_id = char_id;
                __corp.alliance_id = Some(alliance_id);
                __corp.tax_rate = 0.05;
                __corp },
        );
        snap.system_states.insert(
            null_sys,
            SystemDynamicState {
                solar_system_id: null_sys,
                sov_alliance_id: Some(alliance_id),
                ..Default::default()
            },
        );
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: null_sys,
                name: "Sov Hub".into(),
                state: "ShieldVulnerable".into(),
                position: [10_000.0, 0.0, 0.0],
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );
        snap.items.insert(
            99_001,
            InventoryItemRecord {
                item_id: 99_001,
                type_id: 87702,
                owner_id: char_id,
                location_id: hub_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);
    session.alliance_id = Some(alliance_id);
    session.solar_system_id = Some(30_000_180); // high-sec first -> rejected

    let err_hs = handle(
        &ctx,
        &mut session,
        "sovHubSvc",
        "SaveUpgradeConfiguration",
        &[PyRep::Integer(87702)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_hs.code, "NotNullSecSystem");

    session.solar_system_id = Some(null_sys);
    let saved = handle(
        &ctx,
        &mut session,
        "sovHubSvc",
        "SaveUpgradeConfiguration",
        &[PyRep::Integer(87702)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .expect("should install upgrade");
    assert_eq!(saved.dict_get("hubID").and_then(|v| v.as_i64()), Some(hub_id));
    assert_eq!(
        saved.dict_get("prospectingLevel").and_then(|v| v.as_i64()),
        Some(3)
    );

    let installed = handle(
        &ctx,
        &mut session,
        "sovHubSvc",
        "GetInstalledUpgradesLocal",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .expect("should return installed upgrades");
    assert_eq!(
        installed
            .dict_get("prospectingLevel")
            .and_then(|v| v.as_i64()),
        Some(3)
    );
}

#[test]
fn mining_sites_pure_helpers_dungeon_lookup_offsets_and_templates() {
    use eve_sde::{DungeonTemplate, RegionInfo, SdeRegistry, SolarSystem};

    // find_dungeon_id picks lowest matching dungeon_id and ignores substring mismatches
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.dungeons.insert(
        50,
        DungeonTemplate {
            dungeon_id: 50,
            name: "Hidden Mercoxit Asteroid Belt".to_string(),
            archetype_id: None,
            faction_id: None,
            difficulty: None,
        },
    );
    sde.dungeons.insert(
        20,
        DungeonTemplate {
            dungeon_id: 20,
            name: "Mercoxit Asteroid Belt".to_string(),
            archetype_id: None,
            faction_id: None,
            difficulty: None,
        },
    );
    sde.dungeons.insert(
        10,
        DungeonTemplate {
            dungeon_id: 10,
            name: "Mercoxit Asteroid".to_string(),
            archetype_id: None,
            faction_id: None,
            difficulty: None,
        },
    );
    assert_eq!(find_dungeon_id(&sde, "Mercoxit Asteroid Belt"), Some(20));
    assert_eq!(find_dungeon_id(&sde, "Nonexistent Belt"), None);

    // asteroid_offset(i) exact radius 1500 + 250*i and y == 0.0
    for i in [0usize, 1, 7] {
        let [dx, dy, dz] = asteroid_offset(i);
        assert_eq!(dy, 0.0);
        let r = (dx * dx + dz * dz).sqrt();
        let expected_r = 1500.0 + 250.0 * (i as f64);
        assert!((r - expected_r).abs() < 1e-6, "i={i} r={r}");
    }

    // All 5 ICE_BELT_SITES match, plus Heimatar LowSec vs unmapped region -> None
    assert_eq!(ICE_BELT_SITES.len(), 5);
    sde.regions.insert(
        90_000_001,
        RegionInfo {
            region_id: 90_000_001,
            name: "Heimatar".into(),
            center: [0.0; 3],
            faction_id: None,
            wormhole_class_id: None,
        },
    );
    sde.solar_systems.insert(
        30_900_101,
        SolarSystem {
            solar_system_id: 30_900_101,
            constellation_id: 1,
            region_id: 90_000_001,
            name: "Heimatar-Low".into(),
            security_status: 0.3,
            radius: 1.0e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: None,
        },
    );
    sde.regions.insert(
        90_000_002,
        RegionInfo {
            region_id: 90_000_002,
            name: "Unknown Neverland Region".into(),
            center: [0.0; 3],
            faction_id: None,
            wormhole_class_id: None,
        },
    );
    sde.solar_systems.insert(
        30_900_102,
        SolarSystem {
            solar_system_id: 30_900_102,
            constellation_id: 1,
            region_id: 90_000_002,
            name: "Neverland-HS".into(),
            security_status: 0.5,
            radius: 1.0e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: None,
        },
    );
    assert!(resolve_regional_ice_belt(&sde, 30_900_101).is_some());
    assert!(resolve_regional_ice_belt(&sde, 30_900_102).is_none());

    // resolve_wormhole_gas_site: missing dungeon returns Err
    let empty_sde = SdeRegistry::default();
    assert!(resolve_wormhole_gas_site(&empty_sde, &WORMHOLE_GAS_SITES[0]).is_err());
}

#[tokio::test]
async fn mining_sites_handle_passthrough_locks_existing_sov_guard_and_hub_selection() {
    use eve_db::types::{
        CosmicAnomalyRecord, InventoryItemRecord, OwnerNoteRecord, UpwellStructureRecord,
    };

    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde, db.clone());

    let char_id = 31_800_001i64;
    let corp_id = 98_800_001i64;
    let foreign_corp_id = 98_800_002i64;
    let nullsec_sys = 30_800_001u32;
    let ship_id = 1_080_000_001i64;
    let hub_foreign_low = 1_080_000_010i64;
    let hub_own_high = 1_080_000_020i64;
    let hub_nocore = 1_080_000_015i64;
    let locked_item_id = 1_080_000_030i64;

    db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            hub_foreign_low,
            UpwellStructureRecord {
                structure_id: hub_foreign_low,
                solar_system_id: nullsec_sys,
                owner_corp_id: foreign_corp_id,
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                ..Default::default()
            },
        );
        snap.structures.insert(
            hub_nocore,
            UpwellStructureRecord {
                structure_id: hub_nocore,
                solar_system_id: nullsec_sys,
                owner_corp_id: corp_id,
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: false,
                fuel_blocks: 50,
                ..Default::default()
            },
        );
        snap.structures.insert(
            hub_own_high,
            UpwellStructureRecord {
                structure_id: hub_own_high,
                solar_system_id: nullsec_sys,
                owner_corp_id: corp_id,
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                ..Default::default()
            },
        );

        snap.items.insert(
            locked_item_id,
            InventoryItemRecord {
                item_id: locked_item_id,
                type_id: 82_589,
                owner_id: char_id,
                location_id: ship_id,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.owner_notes.insert(
            88_001,
            OwnerNoteRecord {
                note_id: 88_001,
                character_id: -74,
                item_id: locked_item_id,
                label: "raffle".to_string(),
                note: format!(r#"{{"item_id":{locked_item_id},"expired":false,"winner_id":null}}"#),
                timestamp_unix: 100,
            },
        );

        snap.cosmic_anomalies
            .entry(nullsec_sys)
            .or_default()
            .push(CosmicAnomalyRecord {
                anomaly_id: "ORE-801".to_string(),
                solar_system_id: nullsec_sys,
                name: "Existing Deposit".to_string(),
                anomaly_type: "SovOre".to_string(),
                position: [1_000.0, 0.0, 0.0],
                signal_strength: 1.0,
                ore_types: vec![],
                remaining_volume: 0.0,
                created_unix: 100,
                lifetime_seconds: 0,
                cleared: true,
            });
    });

    let mut session = eve_machonet::SessionState::new(1);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);
    session.corp_role = eve_db::corp_roles::ROLE_STATION_MANAGER;
    session.solar_system_id = Some(nullsec_sys);
    session.ship_id = Some(ship_id);
    let kw = HashMap::new();

    // Pass-through returns None for scanSvc / scanMgr
    assert!(handle(&ctx, &mut session, "scanSvc", "GetSystemScanMgr", &[], &kw)
        .await
        .is_none());
    assert!(handle(&ctx, &mut session, "scanMgr", "GetScanResults", &[], &kw)
        .await
        .is_none());

    // GetInstalledUpgradesLocal selects own-corp lowest structure_id (hub_nocore) while SaveUpgradeConfiguration skips !quantum_core_installed
    let local = handle(&ctx, &mut session, "sovHubSvc", "GetInstalledUpgradesLocal", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        local.dict_get("hubID").and_then(|v| v.as_i64()),
        Some(hub_nocore)
    );

    // GetInstalledUpgradesForHub with explicit hubID pointing to foreign hub returns hub_foreign_low
    let explicit = handle(
        &ctx,
        &mut session,
        "sovHubSvc",
        "GetInstalledUpgradesForHub",
        &[PyRep::Integer(hub_foreign_low)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        explicit.dict_get("hubID").and_then(|v| v.as_i64()),
        Some(hub_foreign_low)
    );

    // Missing system security fails when calling SaveUpgradeConfiguration
    let sec_err = handle(
        &ctx,
        &mut session,
        "sovHubSvc",
        "SaveUpgradeConfiguration",
        &[
            PyRep::Integer(hub_own_high),
            PyRep::List(vec![PyRep::Integer(locked_item_id)]),
        ],
        &kw,
    )
    .await;
    assert!(sec_err.expect("handled").is_err());
}
