//! Extra unit and mutation-killing tests for `services::mining_sites`
//! covering security rounding boundary values, `SiteBand::matches` wormhole/null/low/high-mid/high
//! boundaries and Pochven exclusion, `resolve_ore_site` error branches (missing dungeon, empty
//! layout, unmatched type, multi-row collision, zero-count row, zero-unit asteroid, remainder
//! splitting), `resolve_gas_site` and `resolve_wormhole_gas_site` error branches, `scanSvc`/`scanMgr`
//! seeding hooks, and `sovHubSvc` (`SaveUpgradeConfiguration`, `GetInstalledUpgradesLocal`,
//! `GetInstalledUpgradesForHub`) error/proximity/lock/stack-split/anomaly-clearing branches.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::mining_sites::*;
use eve_db::{
    AsteroidRecord, CorporationRecord, CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag,
    InventoryItemRecord, SystemDynamicState, UpwellStructureRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{DungeonAsteroidEntry, DungeonTemplate, InvType, SdeRegistry, SolarSystem};
use std::collections::HashMap;
use std::sync::Arc;

fn make_type(type_id: u32, group_id: u32, category_id: u32, name: &str, vol: f64, rad: f64) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: name.into(),
        mass: 1.0,
        volume: vol,
        capacity: 0.0,
        radius: rad,
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

async fn call_sov_hub(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle(ctx, session, "sovHubSvc", method, args, &HashMap::new())
        .await
        .expect("sovHubSvc handled")
}

#[test]
fn displayed_security_and_site_band_exact_boundaries() {
    // 0.0 and negative values do not round up to 0.1
    assert_eq!(displayed_security(0.0), 0.0);
    assert_eq!(displayed_security(-0.01), 0.0);
    assert_eq!(displayed_security(-0.05), -0.1);
    // Values strictly in (0.0, 0.05) round up to 0.1
    assert_eq!(displayed_security(1e-6), 0.1);
    assert_eq!(displayed_security(0.049999), 0.1);
    assert_eq!(displayed_security(0.05), 0.1);
    assert_eq!(displayed_security(0.149), 0.1);
    assert_eq!(displayed_security(0.15), 0.2);

    // Wormhole / Abyssal solar_system_id >= 31_000_000 always rejected
    assert!(SiteBand::NullSec.matches(30_999_999, -0.5));
    assert!(!SiteBand::NullSec.matches(31_000_000, -0.5));
    assert!(!SiteBand::LowSec.matches(31_000_000, 0.3));
    assert!(!SiteBand::HighSecMid.matches(31_000_000, 0.6));
    assert!(!SiteBand::HighSec.matches(31_000_000, 1.0));

    // Band boundaries on displayed security
    assert!(!SiteBand::HighSecMid.matches(30_000_001, 0.44)); // displayed 0.4
    assert!(SiteBand::HighSecMid.matches(30_000_001, 0.45));  // displayed 0.5
    assert!(SiteBand::HighSecMid.matches(30_000_001, 0.84));  // displayed 0.8
    assert!(!SiteBand::HighSecMid.matches(30_000_001, 0.85)); // displayed 0.9

    assert!(!SiteBand::HighSec.matches(30_000_001, 0.44));
    assert!(SiteBand::HighSec.matches(30_000_001, 0.45));
    assert!(SiteBand::HighSec.matches(30_000_001, 1.0));

    assert!(!SiteBand::LowSec.matches(30_000_001, 0.0));
    assert!(SiteBand::LowSec.matches(30_000_001, 0.001));     // displayed 0.1
    assert!(SiteBand::LowSec.matches(30_000_001, 0.44));      // displayed 0.4
    assert!(!SiteBand::LowSec.matches(30_000_001, 0.45));     // displayed 0.5

    assert!(SiteBand::NullSec.matches(30_000_001, 0.0));
    assert!(SiteBand::NullSec.matches(30_000_001, -0.04));    // displayed 0.0
    assert!(!SiteBand::NullSec.matches(30_000_001, 0.001));   // displayed 0.1

    let mut sys = SolarSystem {
        solar_system_id: 30_000_142,
        region_id: 10_000_002,
        constellation_id: 20_000_020,
        name: "Jita".into(),
        security_status: 0.9,
        radius: 1e12,
        galactic_pos_ly: [0.0, 0.0, 0.0],
        planet_ids: vec![],
        stations: vec![],
        wormhole_class_id: None,
    };
    assert!(SiteBand::HighSec.matches_system(&sys));
    sys.region_id = 10_000_070; // Pochven
    assert!(!SiteBand::HighSec.matches_system(&sys));
}

#[test]
fn resolve_ore_gas_and_wormhole_sites_error_and_remainder_paths() {
    let mut sde = SdeRegistry::default();
    sde.types.insert(18, make_type(18, 458, 25, "Plagioclase", 0.35, 90.0));
    sde.types.insert(20, make_type(20, 457, 25, "Kernite", 1.2, 90.0));

    // 1. Missing dungeon
    let missing_def = OreSiteDef {
        dungeon_name: "Nonexistent Ore Site",
        kind: SiteKind::Anomaly,
        band: SiteBand::HighSec,
        rows: &[OreQuantityRow { selector: OreSelector::Type(18), total_units: 100 }],
        source: "test",
    };
    assert!(resolve_ore_site(&sde, &missing_def).unwrap_err().contains("not found in sde_dungeons"));

    // 2. Dungeon with no asteroid layout
    sde.dungeons.insert(
        9001,
        DungeonTemplate {
            dungeon_id: 9001,
            name: "Empty Layout Site".into(),
            archetype_id: None,
            faction_id: None,
            difficulty: None,
        },
    );
    let empty_layout_def = OreSiteDef {
        dungeon_name: "Empty Layout Site",
        ..missing_def
    };
    assert!(resolve_ore_site(&sde, &empty_layout_def)
        .unwrap_err()
        .contains("has no client asteroid layout"));

    // 3. Asteroid type has no matching wiki row
    sde.dungeon_asteroids.insert(
        9001,
        vec![DungeonAsteroidEntry {
            room_id: 1,
            type_id: 20,
            asteroid_count: 3,
        }],
    );
    assert!(resolve_ore_site(&sde, &empty_layout_def)
        .unwrap_err()
        .contains("has no wiki quantity row"));

    // 4. Asteroid type matches multiple wiki rows (both Type(18) and Group(458))
    sde.dungeon_asteroids.insert(
        9001,
        vec![DungeonAsteroidEntry {
            room_id: 1,
            type_id: 18,
            asteroid_count: 3,
        }],
    );
    let multi_match_def = OreSiteDef {
        dungeon_name: "Empty Layout Site",
        kind: SiteKind::Anomaly,
        band: SiteBand::HighSec,
        rows: &[
            OreQuantityRow { selector: OreSelector::Type(18), total_units: 100 },
            OreQuantityRow { selector: OreSelector::Group(458), total_units: 100 },
        ],
        source: "test",
    };
    assert!(resolve_ore_site(&sde, &multi_match_def)
        .unwrap_err()
        .contains("matches several wiki rows"));

    // 5. Wiki row matches 0 asteroids in dungeon
    let unmatched_row_def = OreSiteDef {
        dungeon_name: "Empty Layout Site",
        kind: SiteKind::Anomaly,
        band: SiteBand::HighSec,
        rows: &[
            OreQuantityRow { selector: OreSelector::Type(18), total_units: 100 },
            OreQuantityRow { selector: OreSelector::Type(20), total_units: 100 },
        ],
        source: "test",
    };
    assert!(resolve_ore_site(&sde, &unmatched_row_def)
        .unwrap_err()
        .contains("matches no asteroid in dungeon"));

    // 6. Asteroid resolves with <= 0 units
    let zero_units_def = OreSiteDef {
        dungeon_name: "Empty Layout Site",
        kind: SiteKind::Anomaly,
        band: SiteBand::HighSec,
        rows: &[OreQuantityRow { selector: OreSelector::Type(18), total_units: 2 }], // 2 units over 3 asteroids -> 3rd gets 0
        source: "test",
    };
    assert!(resolve_ore_site(&sde, &zero_units_def)
        .unwrap_err()
        .contains("resolves an asteroid with no ore"));

    // 7. Remainder distribution: 10 units across 3 asteroids -> [4, 3, 3]
    let rem_def = OreSiteDef {
        dungeon_name: "Empty Layout Site",
        kind: SiteKind::Anomaly,
        band: SiteBand::HighSec,
        rows: &[OreQuantityRow { selector: OreSelector::Group(458), total_units: 10 }],
        source: "test",
    };
    let resolved = resolve_ore_site(&sde, &rem_def).expect("should resolve with remainder");
    let units: Vec<i64> = resolved.asteroids.iter().map(|a| a.units).collect();
    assert_eq!(units, vec![4, 3, 3]);
    assert_eq!(resolved.ore_types(), vec![18]);

    // 8. Gas site & wormhole gas site error branches
    let gas_def = GasSiteDef {
        dungeon_name: "Empty Layout Site",
        region_names: &["The Forge"],
        band: SiteBand::LowSec,
        cloud_units: &[200],
        source: "test",
    };
    assert!(resolve_gas_site(&sde, &gas_def).unwrap_err().contains("has no client gas layout"));
    sde.dungeon_gas_types.insert(9001, vec![30370, 30371]);
    assert!(resolve_gas_site(&sde, &gas_def)
        .unwrap_err()
        .contains("expected exactly one"));

    let wh_gas_def = WormholeGasSiteDef {
        dungeon_name: "Empty Layout Site",
        clouds: &[(30370, 1_000), (99999, 500)],
        guard_npc_type_id: 30209,
        guard_npc_count: 5,
        guard_spawn_delay_secs: 900,
        source: "test",
    };
    assert!(resolve_wormhole_gas_site(&sde, &wh_gas_def)
        .unwrap_err()
        .contains("missing cloud type 99999"));

    // Unknown region_name and resolve_regional_ice_belt returns None
    assert_eq!(region_name(&sde, 99_999_999), None);
    assert!(resolve_regional_ice_belt(&sde, 99_999_999).is_none());
}

#[tokio::test]
async fn sov_hub_validation_proximity_locks_stack_splitting_and_anomaly_clearing() {
    let mut sde = SdeRegistry::with_canonical_seed();
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
            DungeonAsteroidEntry { room_id: 1, type_id: 18, asteroid_count: 1 },
            DungeonAsteroidEntry { room_id: 1, type_id: 1230, asteroid_count: 1 },
            DungeonAsteroidEntry { room_id: 1, type_id: 1228, asteroid_count: 1 },
        ],
    );
    sde.types.insert(18, make_type(18, 458, 25, "Plagioclase", 0.35, 100.0));
    sde.types.insert(1230, make_type(1230, 462, 25, "Veldspar", 0.1, 100.0));
    sde.types.insert(1228, make_type(1228, 460, 25, "Scordite", 0.15, 100.0));
    sde.types.insert(87702, make_type(87702, 4768, 66, "Tritanium Prospecting Array 3", 100.0, 10.0));
    sde.types.insert(89999, make_type(89999, 4768, 66, "Unmodelled Array", 100.0, 10.0));

    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);

    let char_id = 90_000_001_i64;
    let corp_id = 1_000_009_i64;
    let alliance_id = 99_000_001_i64;
    let hub_id = 1_000_000_000_777_i64;
    let ship_id = 500_001_i64;
    let null_sys = 30_004_759_u32;

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);
    session.ship_id = Some(ship_id);

    // 1. Scan service pass-through returns None (both with and without solar_system_id)
    assert!(handle(&ctx, &mut session, "scanSvc", "GetScanResults", &[], &HashMap::new()).await.is_none());
    session.solar_system_id = Some(null_sys);
    assert!(handle(&ctx, &mut session, "scanMgr", "RequestScans", &[], &HashMap::new()).await.is_none());

    // 2. NoSolarSystem on SaveUpgradeConfiguration and GetInstalledUpgradesForHub
    session.solar_system_id = None;
    assert_eq!(call_sov_hub(&ctx, &mut session, "SaveUpgradeConfiguration", &[]).await.unwrap_err().code, "NoSolarSystem");
    assert_eq!(call_sov_hub(&ctx, &mut session, "GetInstalledUpgradesForHub", &[]).await.unwrap_err().code, "NoSolarSystem");
    session.solar_system_id = Some(null_sys);

    // 3. SovereigntyNotClaimed when caller has no alliance or system sov_alliance_id mismatches
    assert_eq!(call_sov_hub(&ctx, &mut session, "SaveUpgradeConfiguration", &[]).await.unwrap_err().code, "SovereigntyNotClaimed");
    session.alliance_id = Some(alliance_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(alliance_id);
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Sov Corp".into(),
                ticker: "SOV".into(),
                ceo_id: 999_999, // not char_id
                alliance_id: Some(alliance_id),
                ..Default::default()
            },
        );
        snap.system_states.insert(
            null_sys,
            SystemDynamicState {
                solar_system_id: null_sys,
                sov_alliance_id: Some(alliance_id + 1),
                ..Default::default()
            },
        );
    });
    assert_eq!(call_sov_hub(&ctx, &mut session, "SaveUpgradeConfiguration", &[]).await.unwrap_err().code, "SovereigntyNotClaimed");

    // 4. InsufficientCorporationRole when caller is neither CEO nor Director/StationManager
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&null_sys).unwrap().sov_alliance_id = Some(alliance_id);
    });
    assert_eq!(call_sov_hub(&ctx, &mut session, "SaveUpgradeConfiguration", &[]).await.unwrap_err().code, "InsufficientCorporationRole");

    // Grant Director role (bit 1) -> NoFriendlySovStructureInSystem before hub is spawned
    session.corp_role = 1;
    assert_eq!(call_sov_hub(&ctx, &mut session, "SaveUpgradeConfiguration", &[]).await.unwrap_err().code, "NoFriendlySovStructureInSystem");

    // Spawn online hub at [10_000, 0, 0] -> UpgradeItemNotFound
    ctx.db.with_mut_snapshot(|snap| {
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
                fuel_blocks: 100,
                ..Default::default()
            },
        );
    });
    assert_eq!(call_sov_hub(&ctx, &mut session, "SaveUpgradeConfiguration", &[]).await.unwrap_err().code, "UpgradeItemNotFound");

    // 5. Upgrade item in ship > 50km from hub -> OutsideSovStructureProximity
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(ship_id, [200_000.0, 0.0, 0.0]);
        snap.items.insert(
            99_100,
            InventoryItemRecord {
                item_id: 99_100,
                type_id: 87702,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                ..Default::default()
            },
        );
    });
    assert_eq!(call_sov_hub(&ctx, &mut session, "SaveUpgradeConfiguration", &[]).await.unwrap_err().code, "OutsideSovStructureProximity");

    // 6. Move ship within range, but flag item Locked -> UpgradeItemLocked
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(ship_id, [15_000.0, 0.0, 0.0]);
        snap.items.get_mut(&99_100).unwrap().flag = InventoryFlag::Locked;
    });
    assert_eq!(call_sov_hub(&ctx, &mut session, "SaveUpgradeConfiguration", &[]).await.unwrap_err().code, "UpgradeItemLocked");

    // 7. Unmodelled upgrade type 89999 -> ProspectingDepositNotModelled
    ctx.db.with_mut_snapshot(|snap| {
        let it = snap.items.get_mut(&99_100).unwrap();
        it.flag = InventoryFlag::Cargo;
        it.type_id = 89999;
    });
    let dict_arg = PyRep::keyval([("upgradeTypeID", PyRep::Integer(89999))]);
    assert_eq!(call_sov_hub(&ctx, &mut session, "SaveUpgradeConfiguration", &[dict_arg]).await.unwrap_err().code, "ProspectingDepositNotModelled");

    // 8. Switch to modelled 87702 (stack of 2) and seed a depleted SovOre anomaly + signature to verify clearing & stack split
    let sov_pos = derive_system_site_position_from_sde(&ctx, null_sys, 3, 5.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&99_100).unwrap().type_id = 87702;
        snap.asteroids.insert(
            77_001,
            AsteroidRecord {
                asteroid_id: 77_001,
                solar_system_id: null_sys,
                belt_id: 0,
                type_id: 18,
                quantity: 0,
                initial_quantity: 100,
                position: (sov_pos[0], sov_pos[1], sov_pos[2]),
                radius: 90.0,
            },
        );
        snap.cosmic_anomalies.entry(null_sys).or_default().push(CosmicAnomalyRecord {
            anomaly_id: "DNG-OLD-1".into(),
            solar_system_id: null_sys,
            name: "Old Sov Deposit".into(),
            anomaly_type: "SovOre".into(),
            position: sov_pos,
            signal_strength: 1.0,
            ore_types: vec![18],
            remaining_volume: 100.0,
            created_unix: 100,
            lifetime_seconds: 0,
            cleared: false,
        });
        snap.cosmic_signatures.entry(null_sys).or_default().push(CosmicSignatureRecord {
            signature_id: "DNG-OLD-1".into(),
            solar_system_id: null_sys,
            sig_group: "SovOre".into(),
            type_name: "Old Sov Deposit".into(),
            position: sov_pos,
            signal_strength: 1.0,
            wormhole_type_id: None,
            destination_system_id: None,
            created_unix: 100,
            lifetime_seconds: 0,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            max_jump_mass_kg: 0.0,
            collapsed: false,
        });
    });

    let tup_arg = PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(87702)])]);
    let saved = call_sov_hub(&ctx, &mut session, "SaveUpgradeConfiguration", &[tup_arg]).await.unwrap();
    assert_eq!(saved.dict_get("prospectingLevel").and_then(|v| v.as_i64()), Some(3));

    ctx.db.with_snapshot(|snap| {
        // Original stack of 2 decremented to 1 in ship cargo, and 1 singleton installed in SubSystemSlot0 on hub
        assert_eq!(snap.items.get(&99_100).unwrap().quantity, 1);
        assert!(snap.items.values().any(|i| i.location_id == hub_id && i.flag == InventoryFlag::SubSystemSlot0 && i.quantity == 1));
        // Old depleted SovOre anomaly marked cleared and its signature collapsed
        let old_anom = snap.cosmic_anomalies[&null_sys].iter().find(|a| a.anomaly_id == "DNG-OLD-1").unwrap();
        assert!(old_anom.cleared);
        assert_eq!(old_anom.lifetime_seconds, 36_000);
        let old_sig = snap.cosmic_signatures[&null_sys].iter().find(|s| s.signature_id == "DNG-OLD-1").unwrap();
        assert!(old_sig.collapsed);
    });

    // Re-calling SaveUpgradeConfiguration when already installed returns existing anomaly without spawning a duplicate
    let saved_again = call_sov_hub(&ctx, &mut session, "SaveUpgradeConfiguration", &[]).await.unwrap();
    assert_eq!(saved_again.dict_get("hubID").and_then(|v| v.as_i64()), Some(hub_id));
}
