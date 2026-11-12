//! Deep branch-coverage and mutation-killing unit tests for `living_world`, `living_world_haulers`, and `world_seeder`.

use crate::living_world::{
    ConvoyRouteState, ConvoyStepStatus, LivingWorldConvoy, LivingWorldDirector, NpcArchetype,
    compute_trade_route, generate_hauler_sde_cargo_loot, hauler_type_for_system,
    hauler_wreck_type_id, miner_type_for_system, sde_ball_and_combat,
};
use crate::system_actor::SystemRegistry;
use crate::world_seeder::{
    TRADE_HUB_STATIONS, build_bom_cost_map, compute_bom_cost, seed_dual_anchor_markets,
    seed_full_market_catalog, select_full_market_types, select_seeded_market_types,
};
use eve_db::clock::{ServerClock, VirtualServerClock};
use eve_db::WorldRepository;
use eve_destiny::MovementMode;
use eve_sde::{
    BlueprintMaterial, ConstellationInfo, FactionInfo, NpcCorporationInfo, RegionInfo,
    SdeRegistry, SolarSystem, StargateJump, StationInfo,
};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn custom_sde_for_world_tests() -> SdeRegistry {
    let mut sde = SdeRegistry::with_canonical_seed();
    let template_type = sde.types.values().min_by_key(|t| t.type_id).unwrap().clone();

    // Custom types: zero-capacity hull, T2 hull, T2 blueprint, faction module, unpriced BOM component & product
    let mut zero_cap_hull = template_type.clone();
    zero_cap_hull.type_id = 990_001;
    zero_cap_hull.group_id = 9901;
    zero_cap_hull.category_id = 6;
    zero_cap_hull.name = "Tiny Hauler".into();
    zero_cap_hull.mass = 2_000_000.0;
    zero_cap_hull.capacity = 0.0;
    zero_cap_hull.base_price = 50_000.0;
    zero_cap_hull.published = true;
    zero_cap_hull.market_group_id = Some(4);
    zero_cap_hull.default_attributes = HashMap::new();
    sde.types.insert(990_001, zero_cap_hull);

    let mut t2_ship = template_type.clone();
    t2_ship.type_id = 990_002;
    t2_ship.group_id = 9901;
    t2_ship.category_id = 6;
    t2_ship.name = "T2 Blockade Runner".into();
    t2_ship.mass = 8_000_000.0;
    t2_ship.capacity = 4_000.0;
    t2_ship.base_price = 0.0;
    t2_ship.published = true;
    t2_ship.market_group_id = Some(4);
    t2_ship.meta_group_id = Some(2);
    t2_ship.default_attributes = HashMap::from([(422, 2.0), (1556, 1200.0)]);
    sde.types.insert(990_002, t2_ship);

    let mut intermediate_comp = template_type.clone();
    intermediate_comp.type_id = 990_003;
    intermediate_comp.group_id = 334;
    intermediate_comp.category_id = 4;
    intermediate_comp.base_price = 0.0;
    intermediate_comp.published = true;
    intermediate_comp.market_group_id = Some(1027);
    intermediate_comp.default_attributes = HashMap::new();
    sde.types.insert(990_003, intermediate_comp);

    let mut t2_bp = template_type.clone();
    t2_bp.type_id = 990_004;
    t2_bp.group_id = 105;
    t2_bp.category_id = 9;
    t2_bp.base_price = 1_000_000.0;
    t2_bp.published = true;
    t2_bp.market_group_id = Some(2);
    t2_bp.default_attributes = HashMap::new();
    sde.types.insert(990_004, t2_bp);

    let mut unpriced_bp = template_type.clone();
    unpriced_bp.type_id = 990_005;
    unpriced_bp.group_id = 105;
    unpriced_bp.category_id = 9;
    unpriced_bp.base_price = 0.0;
    unpriced_bp.published = true;
    unpriced_bp.market_group_id = Some(2);
    unpriced_bp.default_attributes = HashMap::new();
    sde.types.insert(990_005, unpriced_bp);

    // Faction item (Caldari State = 500_001) + non-matching faction item (500_010)
    let mut caldari_fac_item = template_type.clone();
    caldari_fac_item.type_id = 990_006;
    caldari_fac_item.group_id = 55;
    caldari_fac_item.category_id = 7;
    caldari_fac_item.base_price = 0.0;
    caldari_fac_item.published = true;
    caldari_fac_item.market_group_id = Some(9);
    caldari_fac_item.meta_group_id = Some(4);
    caldari_fac_item.faction_id = Some(500_001);
    caldari_fac_item.default_attributes = HashMap::new();
    sde.types.insert(990_006, caldari_fac_item);

    // Cosmetic category 91 (SKINs) must be excluded even if published & market-grouped
    let mut cosmetic_skin = template_type;
    cosmetic_skin.type_id = 990_007;
    cosmetic_skin.group_id = 1950;
    cosmetic_skin.category_id = 91;
    cosmetic_skin.base_price = 100.0;
    cosmetic_skin.published = true;
    cosmetic_skin.market_group_id = Some(19);
    cosmetic_skin.default_attributes = HashMap::new();
    sde.types.insert(990_007, cosmetic_skin);

    // Blueprints: pass-1 component (990_003 from Tritanium 34) + pass-2 T2 hull (990_002 from 990_003)
    let template_bp = sde
        .blueprints
        .values()
        .min_by_key(|b| b.blueprint_type_id)
        .unwrap()
        .clone();
    let mut bp1 = template_bp.clone();
    bp1.blueprint_type_id = 990_005;
    bp1.product_type_id = 990_003;
    bp1.product_quantity = 2;
    bp1.materials = vec![BlueprintMaterial {
        type_id: 34,
        quantity: 100,
    }];
    sde.blueprints.insert(990_005, bp1);

    let mut bp2 = template_bp;
    bp2.blueprint_type_id = 990_004;
    bp2.product_type_id = 990_002;
    bp2.product_quantity = 1;
    bp2.materials = vec![
        BlueprintMaterial {
            type_id: 990_003,
            quantity: 4,
        },
        BlueprintMaterial {
            type_id: 999_999_999,
            quantity: 2,
        },
    ];
    sde.blueprints.insert(990_004, bp2);

    // Synthetic regions, constellations, corps, stations & systems covering all 4 empires + Guristas + self-loop
    let empire_specs = [
        (39_900_001_u32, 29_900_001_u32, 19_900_001_u32, 500_002_u32, 1_990_001_i64, 69_900_001_i64),
        (39_900_002, 29_900_002, 19_900_002, 500_003, 1_990_002, 69_900_002),
        (39_900_003, 29_900_003, 19_900_003, 500_004, 1_990_003, 69_900_003),
        (39_900_004, 29_900_004, 19_900_004, 500_010, 1_990_004, 69_900_004),
    ];
    for (sys_id, con_id, reg_id, fac_id, corp_id, sta_id) in empire_specs {
        sde.regions.insert(
            reg_id,
            RegionInfo {
                region_id: reg_id,
                name: format!("Reg-{reg_id}"),
                center: [0.0; 3],
                faction_id: Some(fac_id),
                wormhole_class_id: None,
            },
        );
        sde.constellations.insert(
            con_id,
            ConstellationInfo {
                constellation_id: con_id,
                region_id: reg_id,
                name: format!("Con-{con_id}"),
                center: [0.0; 3],
                faction_id: None,
                wormhole_class_id: None,
            },
        );
        sde.npc_corporations.insert(
            corp_id,
            NpcCorporationInfo {
                corporation_id: corp_id,
                name: format!("Corp-{corp_id}"),
                ticker_name: "TST".into(),
                faction_id: Some(fac_id),
                solar_system_id: Some(sys_id),
                station_id: Some(sta_id),
                tax_rate: 0.05,
                ceo_id: None,
            },
        );
        let sta = StationInfo {
            station_id: sta_id,
            solar_system_id: sys_id,
            constellation_id: con_id,
            region_id: reg_id,
            station_type_id: 1529,
            name: format!("Station-{sta_id}"),
            position: DVec3::new(100_000.0, 0.0, 0.0),
            owner_corporation_id: corp_id,
            owner_faction_id: Some(fac_id as i64),
            operation_id: 1,
            reprocessing_efficiency: 0.5,
            reprocessing_stations_take: 0.05,
            is_conquerable: false,
            orbit_id: None,
            has_clone_bay: true,
        };
        sde.stations.insert(sta_id, sta.clone());
        sde.solar_systems.insert(
            sys_id,
            SolarSystem {
                solar_system_id: sys_id,
                constellation_id: con_id,
                region_id: reg_id,
                name: format!("Sys-{sys_id}"),
                security_status: 0.9,
                radius: 1.0e12,
                galactic_pos_ly: [0.0; 3],
                planet_ids: Vec::new(),
                stations: if sys_id == 39_900_003 { Vec::new() } else { vec![sta] },
                wormhole_class_id: None,
            },
        );
    }

    // Faction homeworld fallback system with no region/constellation/station faction
    sde.solar_systems.insert(
        39_900_005,
        SolarSystem {
            solar_system_id: 39_900_005,
            constellation_id: 29_900_005,
            region_id: 19_900_005,
            name: "FactionHome".into(),
            security_status: 0.5,
            radius: 1.0e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: Vec::new(),
            stations: Vec::new(),
            wormhole_class_id: None,
        },
    );
    sde.factions.insert(
        500_999,
        FactionInfo {
            faction_id: 500_999,
            name: "CustomFaction".into(),
            corporation_id: None,
            militia_corporation_id: None,
            solar_system_id: Some(39_900_005),
            size_factor: 1.0,
            description: String::new(),
        },
    );
    // Self-loop gate so compute_trade_route exercises `if route.len() == 1` fallback
    sde.jumps.insert(
        59_900_001,
        StargateJump {
            stargate_id: 59_900_001,
            from_solar_system_id: 39_900_005,
            to_solar_system_id: 39_900_005,
            destination_stargate_id: 59_900_001,
            position: DVec3::new(50_000.0, 0.0, 0.0),
        },
    );

    sde
}

#[test]
fn haulers_math_factions_bom_and_world_seeder_branches() {
    let sde = custom_sde_for_world_tests();

    // 1. compute_trade_route: multi-hop Jita route, self-loop fallback, isolated empty
    let jita_route = compute_trade_route(&sde, 30_000_142);
    assert!(jita_route.len() >= 2 && jita_route.len() <= 4);
    assert_eq!(jita_route[0], 30_000_142);

    let self_loop_route = compute_trade_route(&sde, 39_900_005);
    assert_eq!(self_loop_route, vec![39_900_005, 39_900_005]);

    let isolated_route = compute_trade_route(&sde, 39_900_001);
    assert_eq!(isolated_route, vec![39_900_001]);

    // 2. generate_hauler_sde_cargo_loot: zero-cap hull hits minimum cargo clamps
    let tiny_loot = generate_hauler_sde_cargo_loot(&sde, 990_001);
    assert_eq!(
        tiny_loot,
        vec![
            (34, 40_000), // (1000 * 0.40) / 0.01 = 40_000 > 10_000
            (35, 25_000), // (1000 * 0.25) / 0.01 = 25_000 > 2_500
            (36, 15_000), // (1000 * 0.15) / 0.01 = 15_000 > 500
            (37, 8_000),  // (1000 * 0.08) / 0.01 = 8_000 > 100
            (3828, 24),   // (1000 * 0.12) / 5.0 = 24 > 20
        ]
    );
    assert_eq!(hauler_wreck_type_id(&sde, 990_001), 26_557);

    // 3. Faction resolution across all 4 empires + non-empire + station constellation fallback
    assert_eq!(hauler_type_for_system(&sde, 30_000_142), 648);
    assert_eq!(hauler_type_for_system(&sde, 39_900_001), 653);
    assert_eq!(hauler_type_for_system(&sde, 39_900_002), 649);
    assert_eq!(hauler_type_for_system(&sde, 39_900_003), 651);
    assert_eq!(miner_type_for_system(&sde, 30_000_142), 21_654);
    assert_eq!(miner_type_for_system(&sde, 39_900_001), 20_163);
    assert_eq!(miner_type_for_system(&sde, 39_900_002), 26_750);
    assert_eq!(miner_type_for_system(&sde, 39_900_003), 21_840);

    let director = LivingWorldDirector::new();
    assert_eq!(director.ensure_system_navy_patrol(30_000_142, &sde), Some(800_300_142));
    assert_eq!(director.ensure_system_navy_patrol(39_900_001, &sde), Some(800_300_001));
    assert_eq!(director.ensure_system_navy_patrol(39_900_002, &sde), Some(800_300_002));
    assert_eq!(director.ensure_system_navy_patrol(39_900_003, &sde), Some(800_300_003));
    assert_eq!(director.ensure_system_navy_patrol(39_900_004, &sde), None);
    assert_eq!(director.ensure_system_navy_patrol(999_999_999, &sde), None);

    // 4. BOM cost map two-pass T2 component propagation & compute_bom_cost
    let bom = build_bom_cost_map(&sde);
    assert!(bom.get(&990_003).copied().unwrap_or(0.0) > 0.0);
    assert!(bom.get(&990_002).copied().unwrap_or(0.0) > 0.0);
    assert_eq!(compute_bom_cost(999_999_888, &sde), 0.0);
    assert!(compute_bom_cost(990_002, &sde) >= 10.0);

    // 5. select_full_market_types vs select_seeded_market_types filtering & seeding idempotency
    let full = select_full_market_types(&sde);
    let seeded = select_seeded_market_types(&sde);
    assert!(full.contains(&990_001));
    assert!(full.contains(&990_002)); // T2 in full catalog
    assert!(!full.contains(&990_007)); // cosmetic category 91 excluded
    assert!(seeded.contains(&990_001));
    assert!(!seeded.contains(&990_002)); // T2 excluded from baseline
    assert!(!seeded.contains(&990_004)); // T2 blueprint excluded from baseline

    let db = WorldRepository::with_demo_seed();
    let count1 = seed_full_market_catalog(&db, &sde);
    assert!(count1 > 0);
    // Calling seed_full_market_catalog again inserts 0 duplicate orders
    assert_eq!(seed_full_market_catalog(&db, &sde), 0);
    // Faction item 990_006 seeded only at Jita (Caldari 500_001) with finite stock 100
    let jita_fac_orders = db.with_snapshot(|snap| {
        snap.market_orders
            .values()
            .filter(|o| o.type_id == 990_006 && o.station_id == TRADE_HUB_STATIONS[0].0)
            .count()
    });
    assert_eq!(jita_fac_orders, 1);

    // Non-persistent DB skips once markets_seeded == true
    let ephem = WorldRepository::new_empty();
    let e_cnt = seed_dual_anchor_markets(&ephem, &sde);
    assert!(e_cnt > 0);
    assert_eq!(seed_dual_anchor_markets(&ephem, &sde), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn living_world_convoy_hauler_and_mining_full_lifecycle() {
    let sde = Arc::new(custom_sde_for_world_tests());
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_700_200_000));
    let raw_db = WorldRepository::new_empty();
    raw_db.set_clock(clock.clone());
    let db = Arc::new(raw_db);
    let systems = Arc::new(SystemRegistry::with_sde(Arc::clone(&sde)));
    let director = LivingWorldDirector::new();

    // 1. Interdiction & step error guards on unknown / LOD 1 / destroyed convoys
    assert!(director.interdict_convoy(999_999, 90_000_001, &db).is_err());
    let cid = director.ensure_system_hauler_convoy(30_000_142, &sde);
    assert_eq!(director.ensure_system_hauler_convoy(30_000_142, &sde), cid);

    let err_lod = director.interdict_convoy(cid, 90_000_001, &db).unwrap_err();
    assert!(err_lod.contains("LOD 2"));
    assert!(director.step_npc_mining_op(cid, &systems, &db, &sde).await.is_err());

    // Materialize Jita LOD 2, interdict convoy -> creates wreck (26557) + cargo loot
    let spawned = director.materialize_system_lod2(30_000_142, &systems, &sde).await;
    assert_eq!(spawned, 1);
    assert_eq!(director.materialize_system_lod2(30_000_142, &systems, &sde).await, 0);

    let (wreck_id, loot_id, ask_price) = director.interdict_convoy(cid, 90_000_001, &db).unwrap();
    assert!(ask_price.0 > 0);
    db.with_snapshot(|snap| {
        assert_eq!(snap.items[&wreck_id].type_id, 26_557);
        assert_eq!(snap.items[&loot_id].location_id, wreck_id);
    });
    // Destroyed convoy rejects re-interdiction and returns Destroyed on step_hauler_convoy
    assert!(director.interdict_convoy(cid, 90_000_001, &db).is_err());
    assert_eq!(
        director.step_hauler_convoy(cid, &systems, &db, &sde).await.unwrap(),
        ConvoyStepStatus::Destroyed
    );
    // Cooldown prevents immediate respawn while window is active
    assert_eq!(director.ensure_system_hauler_convoy(30_000_142, &sde), cid);

    // 2. Hauler convoy full multi-hop route: Traverse -> JumpGate -> DockedAtDestination -> AlreadyDocked
    let custom_hauler_id = 888_100_001;
    let route = compute_trade_route(&sde, 30_000_142);
    let dest_sys = *route.last().unwrap();
    director.register_convoy(LivingWorldConvoy {
        convoy_id: custom_hauler_id,
        archetype: NpcArchetype::HaulerConvoy,
        ship_type_id: 648,
        origin_system_id: 30_000_142,
        current_system_id: 30_000_142,
        destination_system_id: dest_sys,
        destination_station_id: 60_003_760,
        cargo_type_id: 34,
        cargo_quantity: 50_000,
        lod_level: 1,
        destroyed: false,
    });
    director.set_convoy_route(custom_hauler_id, route.clone());
    assert!(director.get_convoy_route(custom_hauler_id).is_some());
    assert!(!director.is_convoy_docked(custom_hauler_id));

    director.materialize_system_lod2(30_000_142, &systems, &sde).await;
    // Tick 1: initializes last_hop_unix & transitions ball into Warp toward exit gate
    let s1 = director
        .step_hauler_convoy(custom_hauler_id, &systems, &db, &sde)
        .await
        .unwrap();
    assert_eq!(s1, ConvoyStepStatus::TraversingSystem { system_id: 30_000_142 });
    let jita_handle = systems.get_or_spawn(30_000_142);
    let (_, hball) = jita_handle.get_ball_snapshot(custom_hauler_id).await.unwrap().unwrap();
    assert!(matches!(hball.mode, MovementMode::Warp { .. }));

    // Tick 2 with short dt (< transit duration): stays TraversingSystem
    clock.advance_secs(2);
    let batch_trav = director.step_all_hauler_convoys(&systems, &db, &sde).await;
    assert_eq!(batch_trav[0].1.as_ref().unwrap(), &ConvoyStepStatus::TraversingSystem { system_id: 30_000_142 });

    // Advance across all hops until DockedAtDestination + AlreadyDocked
    let mut saw_docked = false;
    for _ in 0..10 {
        clock.advance_secs(600);
        let st = director
            .step_hauler_convoy(custom_hauler_id, &systems, &db, &sde)
            .await
            .unwrap();
        if matches!(st, ConvoyStepStatus::DockedAtDestination { .. }) {
            saw_docked = true;
            break;
        }
    }
    assert!(saw_docked);
    assert!(director.is_convoy_docked(custom_hauler_id));
    assert_eq!(
        director
            .step_hauler_convoy(custom_hauler_id, &systems, &db, &sde)
            .await
            .unwrap(),
        ConvoyStepStatus::AlreadyDocked
    );

    // 3. NPC MiningOp lifecycle: mine -> fill hold -> haul to station -> replenish market + orphaned cargo cleanup
    let mine_id = director.ensure_system_mining_op(30_000_144, &sde);
    assert!(director.get_mining_hauler_convoy(999).is_none());
    assert!(director.get_mining_hauler_convoy(mine_id + 1_000_000).is_some());

    // First tick (elapsed == 0): spawns miner + hauler balls
    let m1 = director.step_all_mining_ops(&systems, &db, &sde).await;
    assert_eq!(m1[0].1.as_ref().unwrap().mined_units, 0);

    // Set cargo near full hold and advance clock so next tick mines + loads hauler + warps to station
    {
        let mut convoys = director.convoys.write().unwrap();
        convoys.get_mut(&mine_id).unwrap().cargo_quantity = 50_000;
    }
    clock.advance_secs(5);
    let m2 = director
        .step_npc_mining_op(mine_id, &systems, &db, &sde)
        .await
        .unwrap();
    assert!(!m2.hauled_to_station);
    assert!(director.hauler_cargo_units(&db, mine_id) > 0);
    // Ensure hauler ball is positioned away from destination station while mid-warp
    let _ = systems
        .get_or_spawn(30_000_144)
        .set_ball_position(mine_id + 1_000_000, DVec3::ZERO)
        .await;

    // Short tick while in transit (< 15s): stays in transit
    clock.advance_secs(3);
    let m3 = director
        .step_npc_mining_op(mine_id, &systems, &db, &sde)
        .await
        .unwrap();
    assert!(!m3.hauled_to_station);

    // Arrival tick (>= 15s): unloads ore & inserts NPC market order in DB
    clock.advance_secs(20);
    let m4 = director
        .step_npc_mining_op(mine_id, &systems, &db, &sde)
        .await
        .unwrap();
    assert!(m4.hauled_to_station);
    assert_eq!(director.hauler_cargo_units(&db, mine_id), 0);

    // Destruction guard on mining op
    director.mark_convoy_destroyed(mine_id);
    assert!(director.step_npc_mining_op(mine_id, &systems, &db, &sde).await.is_err());

    // Direct sde_ball_and_combat fallback for unknown type
    let (fb_ball, fb_combat) = sde_ball_and_combat(&sde, 42, 999_999_777, DVec3::ZERO);
    assert_eq!(fb_ball.id, 42);
    assert_eq!(fb_combat.shield_hp, 1_500.0);

    let _ = ConvoyRouteState {
        route: Vec::new(),
        route_index: 0,
        docked: false,
        last_hop_unix: 0,
    };
}
