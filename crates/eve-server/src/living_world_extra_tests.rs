//! Extended unit tests for `living_world` covering fallback tiers, boundary conditions,
//! market order updates, and multi-system hauler convoy route stepping.

use super::*;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::MovementMode;
use eve_sde::{FactionInfo, NpcCorporationInfo, SdeRegistry, SolarSystem, StationInfo};
use glam::DVec3;

#[cfg(test)]
#[path = "living_world_convoy_and_mining_tests.rs"]
mod living_world_convoy_and_mining_tests;

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
