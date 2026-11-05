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
            CorporationRecord {
                corporation_id: corp_id,
                name: "Sov Corp".into(),
                ticker: "SOV".into(),
                ceo_id: char_id,
                alliance_id: Some(alliance_id),
                tax_rate: 0.05,
                ..Default::default()
            },
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
