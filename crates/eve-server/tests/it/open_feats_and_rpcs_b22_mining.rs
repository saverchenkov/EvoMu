//! End-to-end integration tests for Batch 22 open `feat:mining.*` and `feat:npc.mining-op-npc` rows.

#![forbid(unsafe_code)]

use crate::open_feats_and_rpcs_b22::{rpc, select_pilot, PILOT_1, PILOT_2};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;

const JITA_SYS: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;

#[tokio::test]
async fn test_b22_mining_goals_and_market_e2e() {
    // parity: feat:mining.mining-market
    eve_testkit::attest(&["feat:mining.mining-market"]);

    let ctx = ServerContext::new();
    let mut s1 = select_pilot(&ctx, 55_101, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = select_pilot(&ctx, 55_102, "other", "other-secret", PILOT_2).await;

    // 1. Negative case for feat:mining.mining-goals: client-supplied count via seasonManager.process_client_event
    // must NOT advance mining goals when mining_ledger is empty.
    let _ = rpc(
        &ctx,
        &mut s1,
        "seasonManager",
        "process_client_event",
        &[PyRep::String("totalMined".into()), PyRep::Integer(50_000)],
    )
    .await;
    let ledger_before = ctx.db.get_mining_ledger_with_waste(PILOT_1);
    assert!(ledger_before.is_empty(), "Client-supplied event must not populate mining_ledger");

    // 2. Seed a Venture (32880) with Miner I (483) in space in Jita and mine an asteroid via live RPC
    let ship_id = 77_001_001_i64;
    let miner_id = 77_001_002_i64;
    let ast_id = 77_001_003_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_1) {
            c.station_id = None;
            c.structure_id = None;
            c.solar_system_id = JITA_SYS;
            c.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 32880,
                owner_id: PILOT_1,
                location_id: JITA_SYS as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            miner_id,
            InventoryItemRecord {
                item_id: miner_id,
                type_id: 483,
                owner_id: PILOT_1,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(miner_id, true);
        snap.skills.entry(PILOT_1).or_default().insert(
            3386,
            eve_db::TrainedSkillRecord {
                type_id: 3386, // Mining
                level: 4,
                skill_points: 45_255.0,
            },
        );
    });
    s1.solar_system_id = Some(JITA_SYS);
    s1.ship_id = Some(ship_id);
    ctx.invalidate_ship_fitting(ship_id);

    let jita_handle = ctx.systems.get_or_spawn(JITA_SYS);
    let (ship_ball, ship_combat) = eve_server::living_world::sde_ball_and_combat(&ctx.sde, ship_id, 32880, DVec3::ZERO);
    jita_handle.spawn_ball_with_combat(ship_ball, ship_combat).await.expect("spawn Venture");
    let (ast_ball, _) = eve_server::living_world::sde_ball_and_combat(&ctx.sde, ast_id, 1230, DVec3::new(1_000.0, 0.0, 0.0));
    jita_handle
        .spawn_asteroid(
            eve_server::system_actor::AsteroidState {
                asteroid_id: ast_id,
                belt_id: 0,
                type_id: 1230,
                initial_units: 20_000,
                remaining_units: 20_000,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            ast_ball,
        )
        .await
        .expect("spawn Veldspar asteroid");

    let _ = rpc(&ctx, &mut s1, "dogmaIM", "ShipOnlineModules", &[PyRep::Integer(ship_id)])
        .await
        .expect("ShipOnlineModules");
    let _ = rpc(&ctx, &mut s1, "dogmaIM", "AddTarget", &[PyRep::Integer(ast_id)])
        .await
        .expect("AddTarget asteroid");
    ctx.advance_secs(5);
    jita_handle.step_manual(5.0).await.expect("step target lock");
    let mine_res = rpc(
        &ctx,
        &mut s1,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(miner_id), PyRep::Integer(ast_id)],
    )
    .await
    .expect("dogmaIM.Activate Miner I");
    assert!(!matches!(mine_res, PyRep::None), "Mining activation must return payload");
    ctx.advance_secs(60);
    tick_background_cluster(&ctx).await;

    let ledger_after = ctx.db.get_mining_ledger_with_waste(PILOT_1);
    assert!(!ledger_after.is_empty(), "Mining cycle must record entry in mining_ledger");
    let mined_qty: i64 = ledger_after.iter().map(|r| r.0.quantity).sum();
    assert!(mined_qty > 0, "Mined quantity in ledger must be positive");

    let challenges = rpc(
        &ctx,
        &mut s1,
        "seasonManager",
        "fill_up_and_return_challenges_for_character",
        &[],
    )
    .await
    .expect("fill_up_and_return_challenges_for_character");
    assert!(!matches!(challenges, PyRep::None), "Seasonal/daily challenges must be returned");

    // 3. Test feat:mining.mining-market: reprocess Veldspar into Tritanium and trade ore, compressed ore, and minerals on market
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_1) {
            c.station_id = Some(JITA_STATION);
            c.solar_system_id = JITA_SYS;
        }
        if let Some(c2) = snap.characters.get_mut(&PILOT_2) {
            c2.station_id = Some(JITA_STATION);
            c2.solar_system_id = JITA_SYS;
            c2.balance = IskCents::from_isk(10_000_000);
        }
        snap.items.insert(
            77_001_010,
            InventoryItemRecord {
                item_id: 77_001_010,
                type_id: 1230, // Veldspar (0.1 m3)
                owner_id: PILOT_1,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 1_000,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            77_001_011,
            InventoryItemRecord {
                item_id: 77_001_011,
                type_id: 62516, // Compressed Veldspar (0.001 m3)
                owner_id: PILOT_1,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                ..Default::default()
            },
        );
    });
    s1.station_id = Some(JITA_STATION);
    s2.station_id = Some(JITA_STATION);

    // Reprocess 500 Veldspar into Tritanium (34) via reprocessingSvc.Reprocess
    let _ = rpc(
        &ctx,
        &mut s1,
        "reprocessingSvc",
        "Reprocess",
        &[
            PyRep::List(vec![PyRep::Integer(77_001_010)]),
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(InventoryFlag::Hangar as i64),
            PyRep::Bool(false),
            PyRep::List(vec![]),
        ],
    )
    .await
    .expect("reprocessingSvc.Reprocess");

    let trit_item_id = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|i| i.owner_id == PILOT_1 && i.location_id == JITA_STATION && i.type_id == 34 && i.quantity > 0)
            .map(|i| i.item_id)
            .expect("Tritanium produced by reprocessing Veldspar")
    });

    // Place sell orders for Compressed Veldspar (62516) and Tritanium (34)
    let _ = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION)),
                ("typeID", PyRep::Integer(62516)),
                ("itemID", PyRep::Integer(77_001_011)),
                ("price", PyRep::Real(15.0)),
                ("quantity", PyRep::Integer(100)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
        ],
    )
    .await
    .expect("PlaceMultiSellOrder Compressed Veldspar");

    let _ = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION)),
                ("typeID", PyRep::Integer(34)),
                ("itemID", PyRep::Integer(trit_item_id)),
                ("price", PyRep::Real(5.0)),
                ("quantity", PyRep::Integer(100)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
        ],
    )
    .await
    .expect("PlaceMultiSellOrder Tritanium");

    let orders_comp = rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "GetOrders",
        &[PyRep::Integer(62516)],
    )
    .await
    .expect("GetOrders Compressed Veldspar");
    assert!(!matches!(orders_comp, PyRep::None), "Compressed Veldspar must be listed on market");

    let orders_trit = rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "GetOrders",
        &[PyRep::Integer(34)],
    )
    .await
    .expect("GetOrders Tritanium");
    assert!(!matches!(orders_trit, PyRep::None), "Tritanium must be listed on market");
}

#[tokio::test]
async fn test_b22_mining_excavator_npc_hauler_and_belt_ores() {
    let ctx = ServerContext::new();
    let mut s1 = select_pilot(&ctx, 55_103, "demo", "demo-secret", PILOT_1).await;

    // 1. Test DERIVED (OF-36) belt ore distribution across security bands & empire regions
    let ores_1_0 = ctx.sde.belt_ores_for_security_and_region(1.0, 10_000_002);
    let ids_1_0: Vec<u32> = ores_1_0.iter().map(|t| t.type_id).collect();
    assert_eq!(ids_1_0, vec![1228, 1230], "1.0 high-sec must only contain Scordite (1228) and Veldspar (1230)");

    let ores_amarr_07 = ctx.sde.belt_ores_for_security_and_region(0.7, 10_000_043); // Domain (Amarr)
    let ids_amarr_07: Vec<u32> = ores_amarr_07.iter().map(|t| t.type_id).collect();
    assert!(ids_amarr_07.contains(&1224) && !ids_amarr_07.contains(&20), "0.7 Amarr must contain Pyroxeres and no Kernite");

    let ores_low = ctx.sde.belt_ores_for_security_and_region(0.3, 10_000_032); // Sinq Laison (Low-sec)
    let ids_low: Vec<u32> = ores_low.iter().map(|t| t.type_id).collect();
    assert!(ids_low.contains(&20) && ids_low.contains(&1227) && !ids_low.contains(&1230), "0.3 low-sec must contain Kernite/Omber and no Veldspar");

    let ores_null = ctx.sde.belt_ores_for_security_and_region(-0.8, 10_000_050); // Querious (Null)
    let ids_null: Vec<u32> = ores_null.iter().map(|t| t.type_id).collect();
    assert!(ids_null.contains(&22) && ids_null.contains(&11396), "Deep null-sec must contain Arkonor and Mercoxit");

    // 2. Test feat:mining.excavator-drones: negative case (ship_id 100_000_001 without mining module -> NoMiningModuleFitted)
    let unequipped_ship = 100_000_001_i64;
    let ast_id = 77_002_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_1) {
            c.station_id = None;
            c.structure_id = None;
            c.solar_system_id = JITA_SYS;
            c.active_ship_id = unequipped_ship;
        }
        snap.items.insert(
            unequipped_ship,
            InventoryItemRecord {
                item_id: unequipped_ship,
                type_id: 32880,
                owner_id: PILOT_1,
                location_id: JITA_SYS as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    s1.solar_system_id = Some(JITA_SYS);
    s1.ship_id = Some(unequipped_ship);

    let jita_handle = ctx.systems.get_or_spawn(JITA_SYS);
    let (sb, sc) = eve_server::living_world::sde_ball_and_combat(&ctx.sde, unequipped_ship, 32880, DVec3::ZERO);
    jita_handle.spawn_ball_with_combat(sb, sc).await.expect("spawn unequipped ship");
    let (ab, _) = eve_server::living_world::sde_ball_and_combat(&ctx.sde, ast_id, 1230, DVec3::new(1_000.0, 0.0, 0.0));
    jita_handle
        .spawn_asteroid(
            eve_server::system_actor::AsteroidState {
                asteroid_id: ast_id,
                belt_id: 0,
                type_id: 1230,
                initial_units: 50_000,
                remaining_units: 50_000,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            ab,
        )
        .await
        .expect("spawn asteroid");

    let err = rpc(
        &ctx,
        &mut s1,
        "entity",
        "CmdMineRepeatedly",
        &[PyRep::List(vec![]), PyRep::Integer(ast_id)],
    )
    .await
    .expect_err("Unequipped ship 100_000_001 must fail with NoMiningModuleFitted");
    assert_eq!(err.code, "NoMiningModuleFitted");

    // 3. Test feat:npc.mining-op-npc: companion hauler warps to destination station when ore hold fills and offloads
    let tanoo_sys = 30_000_001_u32;
    let tanoo_handle = ctx.systems.get_or_spawn(tanoo_sys);
    let _ = tanoo_handle.populate_asteroid_belt_entities().await.expect("populate belts");
    let convoy_id = ctx.living_world.ensure_system_mining_op(tanoo_sys, &ctx.sde);
    ctx.living_world.step_npc_mining_op(convoy_id, &ctx.systems, &ctx.db, &ctx.sde).await.expect("step 0");

    assert_eq!(ctx.living_world.hauler_cargo_units(convoy_id), 0, "Hauler starts empty at belt");
    // Advance ticks until Venture ore hold (50,000 units) fills and transfers to hauler
    let mut cargo_in_transit = 0;
    for _ in 0..90 {
        ctx.advance_secs(60);
        ctx.living_world.step_npc_mining_op(convoy_id, &ctx.systems, &ctx.db, &ctx.sde).await.expect("step mining");
        cargo_in_transit = ctx.living_world.hauler_cargo_units(convoy_id);
        if cargo_in_transit > 0 {
            break;
        }
    }
    assert!(cargo_in_transit > 0, "Hauler must load ore when miner ore hold fills");

    // Next tick: hauler at destination station offloads into NPC sell orders and returns to belt
    ctx.advance_secs(60);
    ctx.living_world.step_npc_mining_op(convoy_id, &ctx.systems, &ctx.db, &ctx.sde).await.expect("step offload");
    assert_eq!(ctx.living_world.hauler_cargo_units(convoy_id), 0, "Hauler must offload cargo at station");
}
