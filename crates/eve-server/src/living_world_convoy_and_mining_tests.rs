//! Semantic unit tests split from `living_world_extra_tests`.

use super::super::*;
use crate::system_actor::SystemRegistry;
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord, WorldRepository};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::sync::Arc;

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
