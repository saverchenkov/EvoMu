//! Live mining operations, phased asteroid field lifecycle, and WH site test suite:
//! - `feat:mining.mining-goals`: Negative client event rejection, live mining cycle via dogmaIM,
//!   and ledger-verified progression tracking.
//! - `feat:mining.mining-market`: SDE ore/mineral volumetric specifications, raw ore reprocessing,
//!   and market sell order placement for ore, compressed ore, and minerals.
//! - `feat:mining.mobile-phase-anchor`: High-sec deployment rejection and live null-sec deployment
//!   spawning anchor ball in ballpark.
//! - `feat:mining.phased-asteroid-field-lifecycle`: High-sec rejection, null-sec cosmic signature spawn,
//!   hyperspace fracture progression, beacon emergence, ICS efficiency boost, and collapse.
//! - `feat:mining.wormhole-ore-sites`: Verification of 11 wormhole ore site definitions, Sleeper guards,
//!   and SDE seeding in Thera.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;

use crate::common;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

// parity: feat:mining.mining-goals
// parity: feat:mining.mining-market
#[tokio::test]
async fn test_live_mining_goals_and_market() {
    eve_testkit::attest(&[
        "feat:mining.mining-goals",
        "feat:mining.mining-market",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let jita_sys_id = 30_000_142_u32;
    let jita_station_id = 60_003_760_i64;
    let pilot_char_id = 91_300_011_i64;

    // 1. Negative case for feat:mining.mining-goals:
    // Client-supplied count via seasonManager.process_client_event must NOT populate mining ledger.
    let mut pilot = common::gm_pilot_in_space(&ctx, 55_001, pilot_char_id, 32880, jita_sys_id).await; // Venture
    let _ = rpc(
        &ctx,
        &mut pilot.sess,
        "seasonManager",
        "process_client_event",
        &[PyRep::String("totalMined".into()), PyRep::Integer(99_999)],
    )
    .await;
    let ledger_before = ctx.db.get_mining_ledger_with_waste(pilot_char_id);
    assert!(
        ledger_before.is_empty(),
        "Client-supplied event must not populate mining_ledger"
    );

    // 2. Positive live mining in space for feat:mining.mining-goals:
    // Train mining skills and fit Miner I (483) using GM slash commands.
    common::slash_cmd(&ctx, &mut pilot.sess, "/giveskill 3386 5").await; // Mining V
    common::slash_cmd(&ctx, &mut pilot.sess, "/giveskill 3429 5").await; // Target Management V
    let fit_rep = common::slash_cmd(&ctx, &mut pilot.sess, "/fit 483").await;
    let miner_id = fit_rep
        .dict_get("itemID")
        .expect("miner itemID")
        .as_i64()
        .expect("miner i64");

    // Spawn an authentic asteroid into the system actor ballpark and DB.
    let rock_id = 91_300_091_i64;
    let rock_pos = DVec3::new(10_000.0, 500.0, 0.0);
    let rock_qty = 10_000;
    ctx.db.spawn_asteroid(eve_db::AsteroidRecord {
        asteroid_id: rock_id,
        solar_system_id: jita_sys_id,
        belt_id: 40_000_001,
        type_id: 1230, // Veldspar
        quantity: rock_qty,
        initial_quantity: rock_qty,
        position: (rock_pos.x, rock_pos.y, rock_pos.z),
        radius: 50.0,
    });
    let jita_sys = ctx.systems.get_or_spawn(jita_sys_id);
    jita_sys
        .set_ball_position(pilot.ship_id, DVec3::new(10_000.0, 0.0, 0.0))
        .await;
    let _ = ctx
        .dispatch_rpc(
            &mut pilot.sess,
            "beyonce",
            Some(pilot.beyonce),
            "CmdStop",
            &[],
            &HashMap::new(),
            None,
        )
        .await;
    jita_sys
        .spawn_asteroid(
            eve_server::system_actor::AsteroidState {
                asteroid_id: rock_id,
                belt_id: 40_000_001,
                type_id: 1230,
                initial_units: rock_qty as u64,
                remaining_units: rock_qty as u64,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            eve_destiny::Ball {
                id: rock_id,
                type_id: 1230,
                position: rock_pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e10,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
        )
        .await
        .unwrap();

    let _ = rpc(
        &ctx,
        &mut pilot.sess,
        "dogmaIM",
        "ShipOnlineModules",
        &[PyRep::Integer(pilot.ship_id)],
    )
    .await
    .expect("ShipOnlineModules");

    // Lock asteroid target via dogmaIM
    rpc(
        &ctx,
        &mut pilot.sess,
        "dogmaIM",
        "AddTarget",
        &[PyRep::Integer(rock_id)],
    )
    .await
    .expect("dogmaIM.AddTarget");

    for _ in 0..10 {
        ctx.advance_secs(1);
        jita_sys.step_manual(1.0).await.unwrap();
        if let Ok(locks) = jita_sys.get_target_locks(pilot.ship_id).await {
            if locks.iter().any(|l| l.target_id == rock_id && l.locked) {
                break;
            }
        }
    }

    // Activate Miner I on targeted asteroid
    let mine_res = rpc(
        &ctx,
        &mut pilot.sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(miner_id), PyRep::Integer(rock_id)],
    )
    .await
    .expect("dogmaIM.Activate Miner I");
    assert!(
        !matches!(mine_res, PyRep::None),
        "Mining activation must succeed"
    );

    // Advance virtual clock to complete the mining cycle and tick background cluster
    ctx.advance_secs(65);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // Verify mining ledger records verified mined units
    let ledger_after = ctx.db.get_mining_ledger_with_waste(pilot_char_id);
    assert!(
        !ledger_after.is_empty(),
        "Mining cycle must record entry in mining_ledger"
    );
    let mined_qty: i64 = ledger_after.iter().map(|r| r.0.quantity).sum();
    assert!(mined_qty > 0, "Mined quantity in ledger must be positive");

    let challenges = rpc(
        &ctx,
        &mut pilot.sess,
        "seasonManager",
        "fill_up_and_return_challenges_for_character",
        &[],
    )
    .await
    .expect("fill_up_and_return_challenges_for_character");
    assert!(
        !matches!(challenges, PyRep::None),
        "Seasonal challenges must be queryable"
    );

    // 3. feat:mining.mining-market:
    // SDE volumetric specification check:
    let veldspar_type = ctx.sde.types.get(&1230).expect("Veldspar 1230");
    let trit_type = ctx.sde.types.get(&34).expect("Tritanium 34");
    assert!(veldspar_type.published && (veldspar_type.volume - 0.10).abs() < 1e-4);
    assert!(trit_type.published && (trit_type.volume - 0.01).abs() < 1e-4);

    let comp_type = ctx
        .sde
        .types
        .get(&62516)
        .or_else(|| ctx.sde.types.get(&28432))
        .expect("Compressed Veldspar");
    assert!(comp_type.volume < veldspar_type.volume);
    assert!(comp_type.market_group_id.is_some());
    let comp_tid = comp_type.type_id;

    // Verify negative docking check when outside docking perimeter
    let far_dock_err = rpc(
        &ctx,
        &mut pilot.sess,
        "stationSvc",
        "Dock",
        &[PyRep::Integer(jita_station_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(far_dock_err.code, "OutsideDockingPerimeter");

    // Move ship into docking perimeter of Jita 4-4 station
    let st = ctx
        .sde
        .stations
        .get(&jita_station_id)
        .expect("Jita 4-4 station in SDE");
    let st_pos = DVec3::new(st.position[0], st.position[1], st.position[2]);
    jita_sys.set_ball_position(pilot.ship_id, st_pos).await;

    // Dock pilot at Jita IV - Moon 4 station
    rpc(
        &ctx,
        &mut pilot.sess,
        "stationSvc",
        "Dock",
        &[PyRep::Integer(jita_station_id)],
    )
    .await
    .expect("stationSvc.Dock at Jita 4-4");

    // Seed raw Veldspar in station hangar via GM slash command
    let raw_veld_rep = common::slash_cmd(&ctx, &mut pilot.sess, "/createitem 1230 1000").await;
    let raw_veld_id = raw_veld_rep
        .dict_get("itemID")
        .expect("veld itemID")
        .as_i64()
        .expect("veld i64");

    // Reprocessing quotes check
    let quotes = rpc(
        &ctx,
        &mut pilot.sess,
        "reprocessingSvc",
        "GetQuotes",
        &[PyRep::List(vec![PyRep::Integer(raw_veld_id)])],
    )
    .await
    .expect("reprocessingSvc.GetQuotes");
    assert!(
        !matches!(quotes, PyRep::None),
        "Reprocessing quotes must be returned"
    );

    // Reprocess raw Veldspar into Tritanium (34)
    let _ = rpc(
        &ctx,
        &mut pilot.sess,
        "reprocessingSvc",
        "Reprocess",
        &[
            PyRep::List(vec![PyRep::Integer(raw_veld_id)]),
            PyRep::Integer(jita_station_id),
            PyRep::Integer(eve_db::InventoryFlag::Hangar as i64),
            PyRep::Bool(false),
            PyRep::List(vec![]),
        ],
    )
    .await
    .expect("reprocessingSvc.Reprocess");

    let trit_item_id = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|i| {
                i.owner_id == pilot_char_id
                    && i.location_id == jita_station_id
                    && i.type_id == 34
                    && i.quantity > 0
            })
            .map(|i| i.item_id)
            .expect("Tritanium produced by reprocessing Veldspar")
    });

    // Create Compressed Veldspar stack in station hangar
    let comp_ore_rep =
        common::slash_cmd(&ctx, &mut pilot.sess, &format!("/createitem {comp_tid} 500")).await;
    let comp_ore_id = comp_ore_rep
        .dict_get("itemID")
        .expect("comp itemID")
        .as_i64()
        .expect("comp i64");

    // Place live sell orders on market for Compressed Veldspar and Tritanium
    let _ = rpc(
        &ctx,
        &mut pilot.sess,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(jita_station_id)),
                ("typeID", PyRep::Integer(i64::from(comp_tid))),
                ("itemID", PyRep::Integer(comp_ore_id)),
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
        &mut pilot.sess,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(jita_station_id)),
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

    let comp_orders = rpc(
        &ctx,
        &mut pilot.sess,
        "marketProxy",
        "GetOrders",
        &[PyRep::Integer(i64::from(comp_tid))],
    )
    .await
    .expect("GetOrders Compressed Veldspar");
    assert!(
        !matches!(comp_orders, PyRep::None),
        "Compressed ore must be listed on market"
    );

    let trit_orders = rpc(
        &ctx,
        &mut pilot.sess,
        "marketProxy",
        "GetOrders",
        &[PyRep::Integer(34)],
    )
    .await
    .expect("GetOrders Tritanium");
    assert!(
        !matches!(trit_orders, PyRep::None),
        "Tritanium must be listed on market"
    );
}

// parity: feat:mining.mobile-phase-anchor
// parity: feat:mining.phased-asteroid-field-lifecycle
// parity: feat:mining.wormhole-ore-sites
#[tokio::test]
async fn test_live_phased_asteroid_fields_anchor_and_wormhole_sites() {
    eve_testkit::attest(&[
        "feat:mining.mobile-phase-anchor",
        "feat:mining.phased-asteroid-field-lifecycle",
        "feat:mining.wormhole-ore-sites",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let jita_sys = 30_000_142_u32;
    let null_sys = 30_004_759_u32; // 1DQ1-A (null-sec)
    let pilot_hs_id = 91_300_021_i64;
    let pilot_null_id = 91_300_022_i64;

    // 1. feat:mining.phased-asteroid-field-lifecycle:
    // Phased Asteroid Field rejected in high-sec and wormhole space
    let hs_field = eve_server::services::mining_phased_autotarget::spawn_phased_asteroid_field(
        &ctx,
        jita_sys,
        "PHS-HS-01",
        99_001,
        10_000,
        (0.0, 0.0, 0.0),
    );
    assert!(
        hs_field.is_none(),
        "High-sec space must reject Phased Asteroid Field spawning"
    );

    let wh_field = eve_server::services::mining_phased_autotarget::spawn_phased_asteroid_field(
        &ctx,
        31_000_005,
        "PHS-WH-01",
        99_002,
        10_000,
        (0.0, 0.0, 0.0),
    );
    assert!(
        wh_field.is_none(),
        "Wormhole space must reject Phased Asteroid Field spawning"
    );

    // Spawns legitimately in null-sec K-space
    let field = eve_server::services::mining_phased_autotarget::spawn_phased_asteroid_field(
        &ctx,
        null_sys,
        "PHS-NULL-01",
        99_003,
        50_000,
        (150_000.0, 0.0, 0.0),
    )
    .expect("Phased Asteroid Field spawns in null-sec");
    assert_eq!(field.mining_efficiency(), 0.10);
    assert!(!field.beacon_spawned);
    assert!(!field.collapsed);

    // Advance hyperspace fracture to 0.45: spawns cosmic anomaly beacon
    let advanced = eve_server::services::mining_phased_autotarget::advance_phased_asteroid_field(
        &ctx, null_sys, 0.45,
    )
    .expect("advance_phased_asteroid_field 0.45");
    assert!(advanced.beacon_spawned);
    assert!(!advanced.collapsed);

    let beacon_in_db = ctx.db.with_snapshot(|s| {
        s.cosmic_anomalies
            .get(&null_sys)
            .map(|list| list.iter().any(|a| a.anomaly_id == "PHS-NULL-01"))
            .unwrap_or(false)
    });
    assert!(
        beacon_in_db,
        "Cosmic anomaly beacon must spawn in DB at >= 0.45 fracture progress"
    );

    // Industrial Command Ship connection: 100 energy required to reach 100% efficiency
    let mut field_ics = advanced.clone();
    assert!(!field_ics.connect_industrial_command_ship(88_001, 50));
    assert_eq!(field_ics.mining_efficiency(), 0.10);
    assert!(field_ics.connect_industrial_command_ship(88_002, 50));
    assert_eq!(field_ics.mining_efficiency(), 1.0);

    // Advance hyperspace fracture to 1.0: collapses field
    let collapsed = eve_server::services::mining_phased_autotarget::advance_phased_asteroid_field(
        &ctx, null_sys, 0.55,
    )
    .expect("advance_phased_asteroid_field 0.55");
    assert!(collapsed.collapsed);
    assert_eq!(collapsed.mining_efficiency(), 0.0);

    let sig_collapsed = ctx.db.with_snapshot(|s| {
        s.cosmic_signatures
            .get(&null_sys)
            .and_then(|sigs| {
                sigs.iter()
                    .find(|s| s.signature_id == "PHS-NULL-01")
                    .map(|s| s.collapsed)
            })
            .unwrap_or(false)
    });
    assert!(sig_collapsed, "Cosmic signature must be collapsed in DB");

    // 2. feat:mining.mobile-phase-anchor:
    // High-sec pilot cannot drop Mobile Phase Anchor (90037) via ship.Drop
    let mut pilot_hs =
        common::gm_pilot_in_space(&ctx, 60_001, pilot_hs_id, 587, jita_sys).await;
    let hs_anchor_rep =
        common::slash_cmd(&ctx, &mut pilot_hs.sess, "/createitem 90037 1").await;
    let hs_anchor_id = hs_anchor_rep
        .dict_get("itemID")
        .expect("anchor itemID")
        .as_i64()
        .expect("anchor i64");
    let hs_drop_err = rpc(
        &ctx,
        &mut pilot_hs.sess,
        "ship",
        "Drop",
        &[PyRep::Integer(hs_anchor_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(
        hs_drop_err.code, "CannotDeployInHighSec",
        "High-sec space must reject Mobile Phase Anchor deployment"
    );

    // Null-sec pilot successfully drops Mobile Phase Anchor into space
    let mut pilot_null =
        common::gm_pilot_in_space(&ctx, 60_002, pilot_null_id, 587, null_sys).await;
    let null_anchor_rep =
        common::slash_cmd(&ctx, &mut pilot_null.sess, "/createitem 90037 1").await;
    let null_anchor_id = null_anchor_rep
        .dict_get("itemID")
        .expect("null anchor itemID")
        .as_i64()
        .expect("null anchor i64");
    let drop_res = rpc(
        &ctx,
        &mut pilot_null.sess,
        "ship",
        "Drop",
        &[PyRep::Integer(null_anchor_id)],
    )
    .await
    .expect("Drop Mobile Phase Anchor in null-sec");
    assert!(
        !matches!(drop_res, PyRep::None),
        "ship.Drop must return dropped payload"
    );

    let deployed_anchor = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .find(|it| {
                it.type_id == 90037
                    && it.location_id == i64::from(null_sys)
                    && it.owner_id == pilot_null_id
            })
            .map(|it| it.item_id)
    });
    assert!(
        deployed_anchor.is_some(),
        "Anchor entity must be recorded in solar system items"
    );
    let null_sys_handle = ctx.systems.get_or_spawn(null_sys);
    let anchor_ball = null_sys_handle
        .get_ball_snapshot(deployed_anchor.unwrap())
        .await
        .unwrap();
    assert!(
        anchor_ball.is_some(),
        "Anchor ball must be active in null-sec ballpark"
    );

    // 3. feat:mining.wormhole-ore-sites:
    // All 11 Wormhole Ore Site definitions resolve with valid SDE ore types and Sleeper guards
    assert_eq!(
        eve_server::services::mining_sites::WORMHOLE_ORE_SITES.len(),
        11,
        "Must define all 11 wormhole ore sites"
    );
    for def in eve_server::services::mining_sites::WORMHOLE_ORE_SITES {
        let resolved =
            eve_server::services::mining_sites::resolve_wormhole_ore_site(&ctx.sde, def)
                .expect("Wormhole site ores must resolve against SDE");
        assert!(
            !resolved.is_empty(),
            "Wormhole site must contain asteroid distributions"
        );
        assert!(
            !def.guards.is_empty(),
            "Wormhole site must have Sleeper guard definitions"
        );
        for &(guard_tid, count) in def.guards {
            assert!(
                ctx.sde.types.contains_key(&guard_tid),
                "Guard type ID {guard_tid} must exist in SDE"
            );
            assert!(count > 0, "Guard count must be positive");
        }
    }

    // Seed wormhole system Thera (31000005) from SDE
    eve_server::router::dispatch::seed_system_mining_sites_from_sde(&ctx, 31_000_005).await;
    let thera_sigs = ctx.db.with_snapshot(|s| {
        s.cosmic_signatures
            .get(&31_000_005)
            .cloned()
            .unwrap_or_default()
    });
    let has_wh_ore_sig = thera_sigs
        .iter()
        .any(|sig| sig.sig_group == "OreAnomaly" || sig.sig_group == "Ore");
    assert!(
        has_wh_ore_sig,
        "Thera must contain seeded wormhole ore cosmic signatures"
    );

    let thera_asteroids = ctx.db.with_snapshot(|s| {
        s.asteroids
            .values()
            .filter(|a| a.solar_system_id == 31_000_005)
            .map(|a| a.type_id)
            .collect::<Vec<_>>()
    });
    assert!(
        !thera_asteroids.is_empty(),
        "Thera must have spawned wormhole ore asteroids"
    );
}
