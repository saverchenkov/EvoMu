//! Extended unit tests for `living_world` covering fallback tiers, boundary conditions,
//! market order updates, and multi-system hauler convoy route stepping.

use super::*;
use crate::system_actor::SystemRegistry;
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord, WorldRepository};
use eve_destiny::MovementMode;
use eve_sde::{FactionInfo, NpcCorporationInfo, SdeRegistry, SolarSystem, StationInfo};
use glam::DVec3;
use std::sync::Arc;

#[test]
fn sde_ball_and_combat_attribute_clamping_and_defaults() {
    let mut sde = SdeRegistry::with_canonical_seed();
    let base_type = sde.types.get(&648).expect("Badger in SDE").clone();

    // 1. Custom type with explicit above-floor attributes.
    let mut custom_high = base_type.clone();
    custom_high.type_id = 990_001;
    custom_high.mass = 75_000.0;
    custom_high.radius = 95.0;
    custom_high.default_attributes.clear();
    custom_high.default_attributes.insert(37, 310.0);
    custom_high.default_attributes.insert(70, 2.25);
    custom_high.default_attributes.insert(263, 2_400.0);
    custom_high.default_attributes.insert(265, 1_850.0);
    custom_high.default_attributes.insert(9, 3_100.0);
    sde.types.insert(990_001, custom_high);

    let pos = DVec3::new(10.0, -20.0, 30.0);
    let (b_high, c_high) = sde_ball_and_combat(&sde, 701, 990_001, pos);
    assert_eq!(b_high.id, 701);
    assert_eq!(b_high.type_id, 990_001);
    assert_eq!(b_high.position, pos);
    assert_eq!(b_high.velocity, DVec3::ZERO);
    assert_eq!(b_high.speed_fraction, 0.0);
    assert_eq!(b_high.mode, MovementMode::Stop);
    assert_eq!(b_high.mass, 75_000.0);
    assert_eq!(b_high.radius, 95.0);
    assert_eq!(b_high.max_velocity, 310.0);
    assert_eq!(b_high.inertia, 2.25);
    assert_eq!(c_high.shield_hp, 2_400.0);
    assert_eq!(c_high.max_shield_hp, 2_400.0);
    assert_eq!(c_high.armor_hp, 1_850.0);
    assert_eq!(c_high.max_armor_hp, 1_850.0);
    assert_eq!(c_high.structure_hp, 3_100.0);
    assert_eq!(c_high.max_structure_hp, 3_100.0);

    // 2. Custom type with sub-floor attributes -> clamped by .max(...).
    let mut custom_low = base_type.clone();
    custom_low.type_id = 990_002;
    custom_low.mass = 5.0;
    custom_low.radius = 1.0;
    custom_low.default_attributes.clear();
    custom_low.default_attributes.insert(37, 0.0);
    custom_low.default_attributes.insert(70, 0.001);
    custom_low.default_attributes.insert(263, -10.0);
    custom_low.default_attributes.insert(265, 0.0);
    custom_low.default_attributes.insert(9, 0.2);
    sde.types.insert(990_002, custom_low);

    let (b_low, c_low) = sde_ball_and_combat(&sde, 702, 990_002, DVec3::ZERO);
    assert_eq!(b_low.mass, 1_000.0);
    assert_eq!(b_low.radius, 20.0);
    assert_eq!(b_low.max_velocity, 1.0);
    assert_eq!(b_low.inertia, 0.05);
    assert_eq!(c_low.shield_hp, 1.0);
    assert_eq!(c_low.armor_hp, 1.0);
    assert_eq!(c_low.structure_hp, 1.0);

    // 3. Custom type with empty attributes -> .unwrap_or(...) defaults.
    let mut custom_empty = base_type;
    custom_empty.type_id = 990_003;
    custom_empty.mass = 2_500.0;
    custom_empty.radius = 35.0;
    custom_empty.default_attributes.clear();
    sde.types.insert(990_003, custom_empty);

    let (b_def, c_def) = sde_ball_and_combat(&sde, 703, 990_003, DVec3::ZERO);
    assert_eq!(b_def.max_velocity, 250.0);
    assert_eq!(b_def.inertia, 3.5);
    assert_eq!(c_def.shield_hp, 1_200.0);
    assert_eq!(c_def.armor_hp, 1_200.0);
    assert_eq!(c_def.structure_hp, 1_500.0);
}

#[test]
fn faction_and_station_resolution_fallback_tiers() {
    let mut sde = SdeRegistry::with_canonical_seed();
    let jita = sde.solar_systems.get(&30_000_142).unwrap().clone();
    let jita_const = sde
        .constellations
        .get(&jita.constellation_id)
        .unwrap()
        .clone();
    let jita_region = sde.regions.get(&jita.region_id).unwrap().clone();
    let jita_station = jita.stations.first().unwrap().clone();

    // Tier 2: Constellation has faction_id = None, Region has faction_id = Some(500_002) (Minmatar).
    let mut reg_min = jita_region.clone();
    reg_min.region_id = 10_800_002;
    reg_min.faction_id = Some(500_002);
    sde.regions.insert(10_800_002, reg_min);

    let mut const_none = jita_const.clone();
    const_none.constellation_id = 20_800_002;
    const_none.region_id = 10_800_002;
    const_none.faction_id = None;
    sde.constellations.insert(20_800_002, const_none.clone());

    let mut sys_tier2: SolarSystem = jita.clone();
    sys_tier2.solar_system_id = 30_800_002;
    sys_tier2.constellation_id = 20_800_002;
    sys_tier2.region_id = 10_800_002;
    sys_tier2.stations.clear();
    sde.solar_systems.insert(30_800_002, sys_tier2.clone());

    assert_eq!(hauler_type_for_system(&sde, 30_800_002), 653);
    assert_eq!(miner_type_for_system(&sde, 30_800_002), 20163);

    // Tier 3a & 3b: Region & Constellation have faction_id = None; station owner_faction_id or npc_corp resolves faction.
    let mut reg_none = jita_region;
    reg_none.region_id = 10_800_003;
    reg_none.faction_id = None;
    sde.regions.insert(10_800_003, reg_none);

    let mut const_t3 = const_none;
    const_t3.constellation_id = 20_800_003;
    const_t3.region_id = 10_800_003;
    sde.constellations.insert(20_800_003, const_t3);

    let mut st_t3a: StationInfo = jita_station.clone();
    st_t3a.station_id = 60_800_003;
    st_t3a.solar_system_id = 30_800_003;
    st_t3a.constellation_id = 20_800_003;
    st_t3a.region_id = 10_800_003;
    st_t3a.owner_faction_id = Some(500_003); // Amarr
    st_t3a.owner_corporation_id = 0;

    let mut sys_tier3a = sys_tier2.clone();
    sys_tier3a.solar_system_id = 30_800_003;
    sys_tier3a.constellation_id = 20_800_003;
    sys_tier3a.region_id = 10_800_003;
    sys_tier3a.stations = vec![st_t3a.clone()];
    sde.stations.insert(60_800_003, st_t3a);
    sde.solar_systems.insert(30_800_003, sys_tier3a);
    assert_eq!(hauler_type_for_system(&sde, 30_800_003), 649);
    assert_eq!(miner_type_for_system(&sde, 30_800_003), 26750);

    // Tier 3b: station owner_faction_id = None, resolved via sde.npc_corporations.
    sde.npc_corporations.insert(
        1_800_004,
        NpcCorporationInfo {
            corporation_id: 1_800_004,
            name: "Synthetic Gallente Corp".to_string(),
            ticker_name: "SGC".to_string(),
            faction_id: Some(500_004),
            solar_system_id: Some(30_800_004),
            station_id: Some(60_800_004),
            tax_rate: 0.1,
        },
    );
    let mut st_t3b = jita_station;
    st_t3b.station_id = 60_800_004;
    st_t3b.solar_system_id = 30_800_004;
    st_t3b.constellation_id = 20_800_003;
    st_t3b.region_id = 10_800_003;
    st_t3b.owner_faction_id = None;
    st_t3b.owner_corporation_id = 1_800_004;

    let mut sys_tier3b = sys_tier2.clone();
    sys_tier3b.solar_system_id = 30_800_004;
    sys_tier3b.constellation_id = 20_800_003;
    sys_tier3b.region_id = 10_800_003;
    sys_tier3b.stations = vec![st_t3b];
    sde.solar_systems.insert(30_800_004, sys_tier3b);
    assert_eq!(hauler_type_for_system(&sde, 30_800_004), 651);
    assert_eq!(miner_type_for_system(&sde, 30_800_004), 21840);

    // Tier 4: No station/constellation/region faction, resolved via sde.factions capital solar_system_id.
    let mut sys_tier4 = sys_tier2;
    sys_tier4.solar_system_id = 30_800_005;
    sys_tier4.constellation_id = 20_800_003;
    sys_tier4.region_id = 10_800_003;
    sys_tier4.stations.clear();
    sde.solar_systems.insert(30_800_005, sys_tier4);
    sde.factions.insert(
        500_002,
        FactionInfo {
            faction_id: 500_002,
            name: "Minmatar Republic".to_string(),
            description: String::new(),
            corporation_id: Some(1_000_051),
            militia_corporation_id: None,
            solar_system_id: Some(30_800_005),
            size_factor: 1.0,
        },
    );
    assert_eq!(hauler_type_for_system(&sde, 30_800_005), 653);

    // Station fallback tiers in resolve_npc_station_for_system:
    // 30_800_005 has no local stations, but shares constellation 20_800_003 with station 60_800_003.
    let director = LivingWorldDirector::new();
    let op_const_fb = director.ensure_system_mining_op(30_800_005, &sde);
    assert_eq!(
        director.get_convoy(op_const_fb).unwrap().destination_station_id,
        60_800_003
    );

    // System in another constellation of region 10_800_003 falls back to region station 60_800_003.
    let mut const_reg_fb = jita_const;
    const_reg_fb.constellation_id = 20_800_099;
    const_reg_fb.region_id = 10_800_003;
    const_reg_fb.faction_id = None;
    sde.constellations.insert(20_800_099, const_reg_fb);
    let mut sys_reg_fb = jita;
    sys_reg_fb.solar_system_id = 30_800_099;
    sys_reg_fb.constellation_id = 20_800_099;
    sys_reg_fb.region_id = 10_800_003;
    sys_reg_fb.stations.clear();
    sde.solar_systems.insert(30_800_099, sys_reg_fb);
    let op_reg_fb = director.ensure_system_mining_op(30_800_099, &sde);
    assert_eq!(
        director.get_convoy(op_reg_fb).unwrap().destination_station_id,
        60_800_003
    );

    // Unknown system -> destination_station_id == 0.
    let op_unknown = director.ensure_system_mining_op(99_888_777, &sde);
    assert_eq!(
        director.get_convoy(op_unknown).unwrap().destination_station_id,
        0
    );

    // Navy patrol fallback via region faction (30_800_002) and non-empire faction rejection (500_010).
    let navy_reg = director.ensure_system_navy_patrol(30_800_002, &sde);
    assert!(navy_reg.is_some());
    assert_eq!(director.ensure_system_navy_patrol(30_800_002, &sde), navy_reg);
    sde.regions.get_mut(&10_800_002).unwrap().faction_id = Some(500_010);
    assert!(director.ensure_system_navy_patrol(30_800_002, &sde).is_none());
}

#[test]
fn hauler_cargo_units_and_companion_convoy_boundaries() {
    let director = LivingWorldDirector::new();
    let db = WorldRepository::new_empty();

    // Boundary: hauler_ball_id == 1_000_000 must return None even if convoy 0 is a MiningOp.
    director.register_convoy(LivingWorldConvoy {
        convoy_id: 0,
        archetype: NpcArchetype::MiningOp,
        ship_type_id: 32880,
        origin_system_id: 30_000_144,
        current_system_id: 30_000_144,
        destination_system_id: 30_000_144,
        destination_station_id: 60_003_766,
        cargo_type_id: 1230,
        cargo_quantity: 0,
        lod_level: 1,
        destroyed: false,
    });
    director.register_convoy(LivingWorldConvoy {
        convoy_id: 1,
        archetype: NpcArchetype::MiningOp,
        ship_type_id: 32880,
        origin_system_id: 30_000_144,
        current_system_id: 30_000_144,
        destination_system_id: 30_000_144,
        destination_station_id: 60_003_766,
        cargo_type_id: 1230,
        cargo_quantity: 0,
        lod_level: 1,
        destroyed: false,
    });
    assert!(director.get_mining_hauler_convoy(1_000_000).is_none());
    assert_eq!(
        director.get_mining_hauler_convoy(1_000_001).unwrap().convoy_id,
        1
    );

    // Cargo summation filters by location_id, InventoryFlag::Cargo, and quantity > 0.
    let hauler_ball_id = 1_000_001;
    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            1,
            InventoryItemRecord {
                item_id: 1,
                type_id: 1230,
                owner_id: 1,
                location_id: hauler_ball_id,
                flag: InventoryFlag::Cargo,
                quantity: 350,
                ..Default::default()
            },
        );
        snap.items.insert(
            2,
            InventoryItemRecord {
                item_id: 2,
                type_id: 1230,
                owner_id: 1,
                location_id: hauler_ball_id,
                flag: InventoryFlag::Cargo,
                quantity: 150,
                ..Default::default()
            },
        );
        snap.items.insert(
            3,
            InventoryItemRecord {
                item_id: 3,
                type_id: 1230,
                owner_id: 1,
                location_id: hauler_ball_id,
                flag: InventoryFlag::Hangar,
                quantity: 999,
                ..Default::default()
            },
        );
        snap.items.insert(
            4,
            InventoryItemRecord {
                item_id: 4,
                type_id: 1230,
                owner_id: 1,
                location_id: hauler_ball_id,
                flag: InventoryFlag::Cargo,
                quantity: 0,
                ..Default::default()
            },
        );
    });
    assert_eq!(director.hauler_cargo_units(&db, 1), 500);
}

#[tokio::test]
async fn interdict_price_selection_and_hauler_convoy_stepping() {
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_770_000_000)));
    let systems = Arc::new(SystemRegistry::with_sde_and_db(sde.clone(), db.clone()));
    let director = LivingWorldDirector::new();

    // 1. Interdict when no market orders exist -> fallback price IskCents::from_isk(6).
    director.register_convoy(LivingWorldConvoy {
        convoy_id: 800_500_001,
        archetype: NpcArchetype::PirateRoamer,
        ship_type_id: 16997,
        origin_system_id: 30_000_144,
        current_system_id: 30_000_144,
        destination_system_id: 30_000_142,
        destination_station_id: 60_003_760,
        cargo_type_id: 34,
        cargo_quantity: 100,
        lod_level: 2,
        destroyed: false,
    });
    let (_, _, fallback_price) = director
        .interdict_convoy(800_500_001, 90_000_001, &db)
        .unwrap();
    assert_eq!(fallback_price, IskCents::from_isk(6));
    assert!(director.hauler_cooldowns.read().unwrap().is_empty());

    // 2. Interdict when multiple orders exist -> picks minimum NPC ask order at destination station.
    db.with_mut_snapshot(|snap| {
        // Bid order (ignored)
        snap.market_orders.insert(
            10,
            MarketOrderRecord {
                order_id: 10,
                owner_id: 1_000_035,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(1),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: true,
                escrow: IskCents(0),
            },
        );
        // Player ask order (ignored because is_npc == false)
        snap.market_orders.insert(
            11,
            MarketOrderRecord {
                order_id: 11,
                owner_id: 90_000_001,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(2),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        // Two NPC ask orders at target station -> min is 9 ISK
        for (oid, isk) in [(12, 14), (13, 9)] {
            snap.market_orders.insert(
                oid,
                MarketOrderRecord {
                    order_id: oid,
                    owner_id: 1_000_035,
                    station_id: 60_003_760,
                    region_id: 10_000_002,
                    type_id: 34,
                    is_bid: false,
                    price: IskCents::from_isk(isk),
                    vol_entered: 100,
                    vol_remaining: 100,
                    min_volume: 1,
                    is_npc: true,
                    escrow: IskCents(0),
                },
            );
        }
    });
    director.register_convoy(LivingWorldConvoy {
        convoy_id: 800_500_002,
        archetype: NpcArchetype::HaulerConvoy,
        ship_type_id: 648,
        origin_system_id: 30_000_144,
        current_system_id: 30_000_142,
        destination_system_id: 30_000_142,
        destination_station_id: 60_003_760,
        cargo_type_id: 34,
        cargo_quantity: 500,
        lod_level: 2,
        destroyed: false,
    });
    let (_, _, min_npc_ask) = director
        .interdict_convoy(800_500_002, 90_000_001, &db)
        .unwrap();
    assert_eq!(min_npc_ask, IskCents::from_isk(9));
    assert!(director
        .hauler_cooldowns
        .read()
        .unwrap()
        .contains_key(&30_000_144));

    // 3. Multi-system HaulerConvoy route stepping and step_all_hauler_convoys.
    let hc_id = director.ensure_system_hauler_convoy(30_000_142, &sde);
    director.set_convoy_route(hc_id, vec![30_000_142, 30_000_144]);
    let route_state = director.get_convoy_route(hc_id).expect("route state exists");
    assert_eq!(route_state.route, vec![30_000_142, 30_000_144]);
    assert!(!director.is_convoy_docked(hc_id));

    let _ = director
        .materialize_system_lod2(30_000_142, &systems, &sde)
        .await;
    // Tick 1: initializes last_hop_unix and returns TraversingSystem.
    let s1 = director
        .step_hauler_convoy(hc_id, &systems, &db, &sde)
        .await
        .unwrap();
    assert_eq!(
        s1,
        ConvoyStepStatus::TraversingSystem {
            system_id: 30_000_142
        }
    );

    // Tick 2: advance clock past transit duration -> JumpedGate to 30_000_144.
    db.clock().advance_secs(600);
    let batch_jump = director.step_all_hauler_convoys(&systems, &db, &sde).await;
    assert_eq!(batch_jump.len(), 1);
    assert_eq!(
        batch_jump[0].1.as_ref().unwrap(),
        &ConvoyStepStatus::JumpedGate {
            from_system: 30_000_142,
            to_system: 30_000_144
        }
    );

    // Tick 3: advance clock past transit duration in destination system -> DockedAtDestination.
    db.clock().advance_secs(600);
    let s3 = director
        .step_hauler_convoy(hc_id, &systems, &db, &sde)
        .await
        .unwrap();
    assert!(matches!(s3, ConvoyStepStatus::DockedAtDestination { .. }));
    assert!(director.is_convoy_docked(hc_id));

    // Tick 4: stepping an already-docked convoy returns AlreadyDocked, and batch skips it.
    assert_eq!(
        director
            .step_hauler_convoy(hc_id, &systems, &db, &sde)
            .await
            .unwrap(),
        ConvoyStepStatus::AlreadyDocked
    );
    assert!(director
        .step_all_hauler_convoys(&systems, &db, &sde)
        .await
        .is_empty());
}

#[tokio::test]
async fn npc_mining_op_market_order_creation_and_in_transit_warp_branches() {
    let mut sde_raw = SdeRegistry::with_canonical_seed();
    // Configure ship 32880 without attr 1556 so ore_hold_m3 falls back to t.capacity (500.0).
    if let Some(t) = sde_raw.types.get_mut(&32880) {
        t.default_attributes.remove(&1556);
        t.capacity = 500.0;
    }
    let sde = Arc::new(sde_raw);
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_780_000_000)));
    let systems = Arc::new(SystemRegistry::with_sde_and_db(sde.clone(), db.clone()));
    let director = LivingWorldDirector::new();

    // Register MiningOp with non-existent cargo_type_id (999_999) so asteroid selection uses .or_else fallback.
    let op_id = 800_600_001;
    director.register_convoy(LivingWorldConvoy {
        convoy_id: op_id,
        archetype: NpcArchetype::MiningOp,
        ship_type_id: 32880,
        origin_system_id: 30_000_144,
        current_system_id: 30_000_144,
        destination_system_id: 30_000_144,
        destination_station_id: 60_003_766,
        cargo_type_id: 999_999,
        cargo_quantity: 0,
        lod_level: 1,
        destroyed: false,
    });

    // Tick 1: spawns miner & companion hauler at belt.
    let r1 = director
        .step_npc_mining_op(op_id, &systems, &db, &sde)
        .await
        .unwrap();
    let ore_tid = r1.ore_type_id;
    assert_ne!(ore_tid, 999_999);

    // Seed in_transit_cargo on companion hauler while hauler ball is at belt (far from station)
    // and advance clock by only 5s (< 15s) -> arrived is false, hauler initiates warp without completing haul.
    let hauler_ball_id = op_id + 1_000_000;
    let handle = systems.get_or_spawn(30_000_144);
    let _ = handle
        .set_ball_position(hauler_ball_id, DVec3::new(50_000.0, 0.0, 0.0))
        .await;
    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            501,
            InventoryItemRecord {
                item_id: 501,
                type_id: ore_tid,
                owner_id: 1_000_035,
                location_id: hauler_ball_id,
                flag: InventoryFlag::Cargo,
                quantity: 800,
                ..Default::default()
            },
        );
    });
    db.clock().advance_secs(5);
    let r2 = director
        .step_npc_mining_op(op_id, &systems, &db, &sde)
        .await
        .unwrap();
    assert!(!r2.hauled_to_station);
    assert_eq!(director.hauler_cargo_units(&db, op_id), 800);

    // Advance clock +20s (>= 15s) with empty market_orders -> creates a new NPC ask MarketOrderRecord.
    db.clock().advance_secs(20);
    let r3 = director
        .step_npc_mining_op(op_id, &systems, &db, &sde)
        .await
        .unwrap();
    assert!(r3.hauled_to_station);
    assert_eq!(director.hauler_cargo_units(&db, op_id), 0);
    let created_vol = db.with_snapshot(|snap| {
        snap.market_orders
            .values()
            .find(|o| o.station_id == 60_003_766 && o.type_id == ore_tid && o.is_npc && !o.is_bid)
            .map(|o| o.vol_remaining)
            .unwrap_or(0)
    });
    assert_eq!(created_vol, 800);

    // Seed a second haul and advance clock +20s -> increments existing NPC ask order volume (800 + 400 = 1200).
    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            502,
            InventoryItemRecord {
                item_id: 502,
                type_id: ore_tid,
                owner_id: 1_000_035,
                location_id: hauler_ball_id,
                flag: InventoryFlag::Cargo,
                quantity: 400,
                ..Default::default()
            },
        );
    });
    db.clock().advance_secs(20);
    let r4 = director
        .step_npc_mining_op(op_id, &systems, &db, &sde)
        .await
        .unwrap();
    assert!(r4.hauled_to_station);
    let updated_vol = db.with_snapshot(|snap| {
        snap.market_orders
            .values()
            .find(|o| o.station_id == 60_003_766 && o.type_id == ore_tid && o.is_npc && !o.is_bid)
            .map(|o| o.vol_remaining)
            .unwrap_or(0)
    });
    assert_eq!(updated_vol, 1_200);
}

