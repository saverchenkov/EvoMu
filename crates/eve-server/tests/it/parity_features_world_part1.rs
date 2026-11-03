//! Verified runtime integration tests for world parity features (Part 1: Abyssal, Navigation, PvE Dungeons, Agency & Info Panels).

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::pve_abyssal_hazards::{
    evaluate_abyssal_spatial_clouds_for_ship, FILAMENT_CLOUD_TYPE_ID, TACHYON_CLOUD_TYPE_ID,
};
use eve_server::services::pve_abyssal_pylons::build_fitted_ship_space_ball;
use eve_server::services::pve_special_sites_abyssal::{
    on_abyssal_cache_destroyed_core, BIOADAPTIVE_CACHE_TYPE_ID, BIOADAPTIVE_CACHE_WRECK_TYPE_ID,
};
use glam::DVec3;
use std::collections::HashMap;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

async fn spawn_ship(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    type_id: u32,
    pos: DVec3,
) {
    let handle = ctx.systems.get_or_spawn(sys_id);
    let _ = handle.remove_ball(ship_id).await;
    let mut ball = build_fitted_ship_space_ball(ctx, char_id, ship_id, type_id, pos);
    ball.id = ship_id;
    ball.position = pos;
    let cs = ctx.build_ship_combat_state(char_id, ship_id, type_id);
    let _ = handle.spawn_ball_with_combat(ball, cs).await;
}

#[tokio::test]
async fn test_abyssal_spatial_hazard_clouds_and_bioadaptive_cache_rewards() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(40_101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");

    let abyss_sys = 32_000_001_u32;
    let ship_id = sess.ship_id.unwrap_or(100_000_001);
    let ship_type = 606_u32; // Velator / frigate
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(abyss_sys);

    let base_ball = build_fitted_ship_space_ball(&ctx, char_id, ship_id, ship_type, DVec3::ZERO);
    let base_cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type);

    spawn_ship(&ctx, abyss_sys, char_id, ship_id, ship_type, DVec3::ZERO).await;
    let handle = ctx.systems.get_or_spawn(abyss_sys);

    // 1. Enter Tachyon Cloud -> +300% max velocity (4x) and +300% signature radius bloom (4x)
    let tachyon_id = 1_897_000_001_i64;
    let _ = handle
        .spawn_ball(Ball {
            id: tachyon_id,
            type_id: TACHYON_CLOUD_TYPE_ID,
            position: DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 15_000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let active = evaluate_abyssal_spatial_clouds_for_ship(&ctx, abyss_sys, ship_id).await;
    assert_eq!(active, Some("TachyonCloud"));
    let (_, b_tach) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let cs_tach = handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((b_tach.max_velocity - base_ball.max_velocity * 4.0).abs() < 1e-3);
    assert!((cs_tach.signature_radius - base_cs.signature_radius * 4.0).abs() < 1e-3);

    // 2. Replace with Filament Cloud -> shield booster modifier 0.6x
    let _ = handle.remove_ball(tachyon_id).await;
    let filament_id = 1_897_000_002_i64;
    let _ = handle
        .spawn_ball(Ball {
            id: filament_id,
            type_id: FILAMENT_CLOUD_TYPE_ID,
            position: DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 15_000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let active_fil = evaluate_abyssal_spatial_clouds_for_ship(&ctx, abyss_sys, ship_id).await;
    assert_eq!(active_fil, Some("FilamentCloud"));
    let cs_fil = handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((cs_fil.shield_boost_multiplier - 0.6).abs() < 1e-6);

    // 3. Leave cloud boundary -> all modifiers reset immediately to 1.0x
    spawn_ship(
        &ctx,
        abyss_sys,
        char_id,
        ship_id,
        ship_type,
        DVec3::new(40_000.0, 0.0, 0.0),
    )
    .await;
    let active_out = evaluate_abyssal_spatial_clouds_for_ship(&ctx, abyss_sys, ship_id).await;
    assert_eq!(active_out, None);
    let (_, b_out) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let cs_out = handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((b_out.max_velocity - base_ball.max_velocity).abs() < 1e-3);
    assert!((cs_out.signature_radius - base_cs.signature_radius).abs() < 1e-3);
    assert!((cs_out.shield_boost_multiplier - 1.0).abs() < 1e-6);

    // 4. Bioadaptive Cache destruction -> spawns Bioadaptive Cache Wreck and transfers loot
    let (cache_id, loot_id) = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let cid = snap.next_entity_id;
        snap.next_entity_id += 1;
        let lid = snap.next_entity_id;
        snap.items.insert(
            cid,
            InventoryItemRecord {
                item_id: cid,
                type_id: BIOADAPTIVE_CACHE_TYPE_ID,
                owner_id: 500_026,
                location_id: abyss_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(cid, [5_000.0, 0.0, 0.0]);
        snap.items.insert(
            lid,
            InventoryItemRecord {
                item_id: lid,
                type_id: 30_745, // Triglavian Survey Database
                owner_id: 500_026,
                location_id: cid,
                flag: InventoryFlag::None,
                quantity: 3,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        (cid, lid)
    });

    on_abyssal_cache_destroyed_core(&ctx.systems, &ctx.db, abyss_sys, cache_id, Some(char_id)).await;

    let wreck_id = ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&cache_id), "cache must be removed on destruction");
        let loot = snap.items.get(&loot_id).expect("loot item preserved");
        let wid = loot.location_id;
        assert_ne!(wid, cache_id);
        let wreck = snap.items.get(&wid).expect("wreck item created");
        assert_eq!(wreck.type_id, BIOADAPTIVE_CACHE_WRECK_TYPE_ID);
        wid
    });
    assert!(
        handle.get_ball_snapshot(wreck_id).await.unwrap().is_some(),
        "wreck ball must be spawned in space"
    );
}

#[tokio::test]
async fn test_navigation_warp_landing_points_and_min_distance() {
    // parity: feat:navigation.warp-landing-points
    eve_testkit::attest(&["feat:navigation.warp-landing-points"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(40_102);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");

    let jita_sys = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let ship_id = sess.ship_id.unwrap_or(100_000_001);
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(jita_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = jita_sys;
        }
    });

    let station = ctx.sde.stations.get(&station_id).expect("Jita 4-4 in SDE");
    let st_pos = station.position;
    let st_radius = ctx
        .sde
        .types
        .get(&station.station_type_id)
        .map(|t| t.radius)
        .filter(|&r| r > 0.0)
        .unwrap_or(0.0);

    // 1. Warps closer than 150 km to landing point must be refused with WarpDistanceTooShort
    let close_pos = st_pos + DVec3::new(st_radius + 100_000.0, 0.0, 0.0);
    spawn_ship(&ctx, jita_sys, char_id, ship_id, 606, close_pos).await;
    let short_err = rpc(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdWarpToStuff",
        &[
            PyRep::String("item".into()),
            PyRep::Integer(station_id),
            PyRep::Real(0.0),
        ],
    )
    .await
    .expect_err("Warp < 150km must fail");
    assert_eq!(short_err.code, "WarpDistanceTooShort");

    // 2. Warp to station at 0 m lands at station docking perimeter (radius + 2,500 m standoff)
    let far_pos = st_pos + DVec3::new(st_radius + 5_000_000.0, 0.0, 0.0);
    spawn_ship(&ctx, jita_sys, char_id, ship_id, 606, far_pos).await;
    rpc(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdWarpToStuff",
        &[
            PyRep::String("item".into()),
            PyRep::Integer(station_id),
            PyRep::Real(0.0),
        ],
    )
    .await
    .expect("CmdWarpToStuff 0m");

    let handle = ctx.systems.get_or_spawn(jita_sys);
    let (_, b0) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let dest0 = match b0.mode {
        MovementMode::Warp { destination, .. } => destination,
        other => panic!("expected Warp mode, got {other:?}"),
    };
    let expected_perimeter = st_pos + DVec3::new(st_radius, 0.0, 0.0);
    assert!(
        dest0.distance(expected_perimeter) < 1.0,
        "0m warp must land at station perimeter: got {dest0:?}, expected {expected_perimeter:?}"
    );

    // 3. Warp with 25 km offset lands 25,000 m short of the landing point along approach vector
    spawn_ship(&ctx, jita_sys, char_id, ship_id, 606, far_pos).await;
    rpc(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdWarpToStuff",
        &[
            PyRep::String("item".into()),
            PyRep::Integer(station_id),
            PyRep::Real(25_000.0),
        ],
    )
    .await
    .expect("CmdWarpToStuff 25km");
    let (_, b25) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let dest25 = match b25.mode {
        MovementMode::Warp { destination, .. } => destination,
        other => panic!("expected Warp mode, got {other:?}"),
    };
    assert!(
        (dest25.distance(expected_perimeter) - 25_000.0).abs() < 1.0,
        "25km offset must be relative to landing point"
    );
}

#[tokio::test]
async fn test_pve_dungeon_system_and_agency_ui_services() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(40_103);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");

    sess.station_id = None;
    sess.solar_system_id = Some(30_000_142);
    sess.role |= eve_machonet::service_roles::ROLE_GM_MASK;

    // 1. SDE Dungeon query + GM PlayDungeon / GetCurrentDungeonForCharacter / ResetDungeon
    let dungeons_rep = rpc(&ctx, &mut sess, "dungeon", "DEGetDungeons", &[])
        .await
        .expect("DEGetDungeons");
    let PyRep::Rowset { rows: dungeon_rows, .. } = dungeons_rep else {
        panic!("unexpected DEGetDungeons shape: {dungeons_rep:?}");
    };
    assert!(!dungeon_rows.is_empty(), "SDE must contain dungeons");
    let first_dungeon_id = dungeon_rows
        .iter()
        .filter_map(|cols| cols[0].as_i64())
        .find(|&did| ctx.sde.dungeon_asteroids.contains_key(&(did as u32)))
        .expect("SDE dungeon with rooms");

    rpc(
        &ctx,
        &mut sess,
        "keeper",
        "PlayDungeon",
        &[PyRep::Integer(first_dungeon_id)],
    )
    .await
    .expect("keeper.PlayDungeon");

    let cur_dun = rpc(&ctx, &mut sess, "keeper", "GetEditingDungeonID", &[])
        .await
        .expect("GetEditingDungeonID");
    assert_eq!(
        cur_dun,
        PyRep::Integer(first_dungeon_id),
        "GetEditingDungeonID must reflect active played dungeon"
    );

    rpc(&ctx, &mut sess, "keeper", "ResetDungeon", &[])
        .await
        .expect("keeper.ResetDungeon");
    let after_reset = rpc(&ctx, &mut sess, "keeper", "GetEditingDungeonID", &[])
        .await
        .expect("GetEditingDungeonID after reset");
    assert_eq!(after_reset, PyRep::None);

    // 2. Agency UI services
    let combat_anoms = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetCombatAnomalyInstances",
        &[],
    )
    .await
    .expect("GetCombatAnomalyInstances");
    assert!(matches!(combat_anoms, PyRep::List(_) | PyRep::Dict(_)));

    let ore_anoms = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetOreAnomalyInstances",
        &[],
    )
    .await
    .expect("GetOreAnomalyInstances");
    assert!(matches!(ore_anoms, PyRep::List(_) | PyRep::Dict(_)));

    let agency_content = rpc(&ctx, &mut sess, "custom_agency_provider", "get_content_data", &[])
        .await
        .expect("custom_agency_provider.get_content_data");
    assert!(agency_content != PyRep::None);

    // 3. Info panel backing services (insurgency, incursion, FW)
    let campaigns = rpc(
        &ctx,
        &mut sess,
        "insurgencySolarsystem",
        "GetAllVisibleCampaigns",
        &[],
    )
    .await
    .expect("GetAllVisibleCampaigns");
    assert!(matches!(campaigns, PyRep::List(_)));

    let incursions = rpc(&ctx, &mut sess, "incursion", "GetIncursionLPLog", &[])
        .await
        .expect("GetIncursionLPLog");
    assert!(matches!(incursions, PyRep::List(_) | PyRep::Dict(_) | PyRep::Object { .. } | PyRep::Rowset { .. }));

    let fw_zones = rpc(&ctx, &mut sess, "facWarMgr", "GetStats_TopAndAllKillsAndVPs", &[])
        .await
        .expect("GetStats_TopAndAllKillsAndVPs");
    assert!(fw_zones != PyRep::None);
}
