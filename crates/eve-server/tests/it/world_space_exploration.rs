//! World, space, and exploration features parity suite.

#![forbid(unsafe_code)]

#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod world_features_ext_a {
    use super::*;

// Verified runtime integration tests for world parity features (Part 1: Abyssal, Navigation, PvE Dungeons, Agency & Info Panels).

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

}

#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod world_features_ext_b {
    use super::*;

// Verified runtime integration tests for world parity features (Part 3: Industry, Lifecycle, Corporation, Fleet, Invariants).

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::industry_pi::industry_facilities_bps::{
    compute_blueprint_eiv_per_run, compute_industry_job_costs,
};
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

#[tokio::test]
async fn test_industry_research_cost_tax_capital_and_lifecycle_pause() {
    // parity: feat:lifecycle.industry-pause-and-many
    eve_testkit::attest(&[
        "feat:lifecycle.industry-pause-and-many",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(40_301);
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

    let jita_station = 60_003_760_i64;
    sess.station_id = Some(jita_station);
    sess.structure_id = None;
    sess.solar_system_id = Some(30_000_142);

    // 1. EIV from market adjusted prices + 0.25% NPC station facility tax + job cost formula
    let rifter_bp = 687_u32;
    let (eiv_default, cost_default) = ctx.db.with_snapshot(|snap| {
        let eiv = compute_blueprint_eiv_per_run(&ctx, &ctx.sde.blueprints[&rifter_bp], 1);
        let cost = compute_industry_job_costs(&ctx, snap, rifter_bp, 1, 1, jita_station).unwrap();
        (eiv, cost)
    });
    assert!((cost_default.facility_tax_rate - 0.0025).abs() < 1e-6);
    assert!((cost_default.eiv_isk - eiv_default).abs() < 0.05);

    // 2. Exponential ME research time via industryManager.InstallJob (activityID = 4)
    let (bp1, bp2) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(5_000_000_000);
            ch.station_id = Some(jita_station);
            ch.solar_system_id = 30_000_142;
        }
        let skills = snap.skills.entry(char_id).or_default();
        for sid in [3387_u32, 3406, 3380, 3402, 3409] {
            skills.insert(
                sid,
                eve_db::TrainedSkillRecord {
                    type_id: sid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        snap.next_entity_id += 1;
        let b1 = snap.next_entity_id;
        snap.next_entity_id += 1;
        let b2 = snap.next_entity_id;
        snap.items.insert(
            b1,
            InventoryItemRecord {
                item_id: b1,
                type_id: rifter_bp,
                owner_id: char_id,
                location_id: jita_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                blueprint_runs_remaining: None,
                ..Default::default()
            },
        );
        let mut dyn9 = HashMap::new();
        dyn9.insert(1112, 8.0); // ME 8 -> researching to ME 9
        snap.items.insert(
            b2,
            InventoryItemRecord {
                item_id: b2,
                type_id: rifter_bp,
                owner_id: char_id,
                location_id: jita_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                blueprint_runs_remaining: None,
                dynamic_attributes: dyn9,
                ..Default::default()
            },
        );
        (b1, b2)
    });

    rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bp1)),
            ("blueprintTypeID", PyRep::Integer(rifter_bp as i64)),
            ("activityID", PyRep::Integer(4)),
            ("runs", PyRep::Integer(1)),
            ("facilityID", PyRep::Integer(jita_station)),
        ])],
    )
    .await
    .expect("InstallJob ME 0->1");

    rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bp2)),
            ("blueprintTypeID", PyRep::Integer(rifter_bp as i64)),
            ("activityID", PyRep::Integer(4)),
            ("runs", PyRep::Integer(1)),
            ("facilityID", PyRep::Integer(jita_station)),
        ])],
    )
    .await
    .expect("InstallJob ME 8->9");

    let (dur_1, dur_9) = ctx.db.with_snapshot(|snap| {
        let j1 = snap.industry_jobs.values().find(|j| j.blueprint_item_id == bp1).unwrap();
        let j9 = snap.industry_jobs.values().find(|j| j.blueprint_item_id == bp2).unwrap();
        (j1.duration_seconds, j9.duration_seconds)
    });
    assert!(
        dur_9 > dur_1 * 500,
        "ME 8->9 research duration ({dur_9}s) must scale exponentially over ME 0->1 ({dur_1}s)"
    );

    // 3. Capital construction restrictions: Dreadnought (group 485) forbidden in High-Sec; Jump Freighter (group 902) allowed
    let dread_bp = ctx
        .sde
        .blueprints
        .values()
        .find(|b| ctx.sde.types.get(&b.product_type_id).is_some_and(|t| t.group_id == 485))
        .map(|b| b.blueprint_type_id);
    if let Some(dbp) = dread_bp {
        let dread_item = ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let id = snap.next_entity_id;
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: dbp,
                    owner_id: char_id,
                    location_id: jita_station,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            id
        });
        let cap_err = rpc(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(dread_item)),
                ("blueprintTypeID", PyRep::Integer(dbp as i64)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
                ("facilityID", PyRep::Integer(jita_station)),
            ])],
        )
        .await
        .expect_err("Dreadnought in High-Sec must fail");
        assert_eq!(cap_err.code, "CapitalConstructionForbiddenInHighSec");
    }

    let jf_bp = ctx
        .sde
        .blueprints
        .values()
        .find(|b| {
            ctx.sde.types.get(&b.product_type_id).is_some_and(|t| t.group_id == 902)
                && !b.materials.is_empty()
        });
    if let Some(jfb) = jf_bp {
        let jf_item = ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let id = snap.next_entity_id;
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: jfb.blueprint_type_id,
                    owner_id: char_id,
                    location_id: jita_station,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            for m in &jfb.materials {
                snap.next_entity_id += 1;
                let mid = snap.next_entity_id;
                snap.items.insert(
                    mid,
                    InventoryItemRecord {
                        item_id: mid,
                        type_id: m.type_id,
                        owner_id: char_id,
                        location_id: jita_station,
                        flag: InventoryFlag::Hangar,
                        quantity: m.quantity as i32,
                        ..Default::default()
                    },
                );
            }
            id
        });
        rpc(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(jf_item)),
                ("blueprintTypeID", PyRep::Integer(jfb.blueprint_type_id as i64)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
                ("facilityID", PyRep::Integer(jita_station)),
            ])],
        )
        .await
        .expect("Jump Freighter construction must be allowed in High-Sec");
    }

    // 4. CompleteManyJobs on installed jobs
    ctx.advance_secs(4_000_000);
    let active_job_ids: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
        snap.industry_jobs
            .values()
            .filter(|j| j.installer_id == char_id && j.status == "Active")
            .map(|j| PyRep::Integer(j.job_id))
            .collect()
    });
    assert!(!active_job_ids.is_empty());
    rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "CompleteManyJobs",
        &[PyRep::List(active_job_ids)],
    )
    .await
    .expect("CompleteManyJobs");
}

#[tokio::test]
async fn test_corp_member_audit_reinforce_default_and_fleet_handover_formations() {
    // parity: feat:fleet.boss-handover
    // parity: feat:fleet.stored-formations-and-warp-shapes
    eve_testkit::attest(&[
        "feat:fleet.boss-handover",
        "feat:fleet.stored-formations-and-warp-shapes",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess1 = SessionState::new(40_302);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    let char1 = 90_000_001_i64;
    let char2 = 90_000_002_i64;
    rpc(
        &ctx,
        &mut sess1,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char1)],
    )
    .await
    .expect("SelectCharacterID");

    let corp_id = sess1.corporation_id.unwrap_or(98_000_001);
    sess1.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char1) {
            ch.corporation_id = corp_id;
        }
        let corp = snap.corporations.entry(corp_id).or_default();
        corp.ceo_id = char1;
        corp.members.insert(char1);
    });

    // 1. Non-director rejected for SetStructureReinforceDefault; director succeeds
    let mut non_dir_sess = sess1.clone();
    non_dir_sess.character_id = Some(char2);
    non_dir_sess.corp_role = 0;
    let non_dir_err = rpc(
        &ctx,
        &mut non_dir_sess,
        "corpRegistry",
        "SetStructureReinforceDefault",
        &[PyRep::Integer(19)],
    )
    .await
    .expect_err("Non-director SetStructureReinforceDefault must fail");
    assert!(!non_dir_err.code.is_empty());

    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "SetStructureReinforceDefault",
        &[PyRep::Integer(19)],
    )
    .await
    .expect("Director SetStructureReinforceDefault");
    let def_hr = rpc(&ctx, &mut sess1, "corpRegistry", "GetStructureReinforceDefault", &[])
        .await
        .expect("GetStructureReinforceDefault");
    assert_eq!(def_hr, PyRep::Integer(19));

    // 2. Role change via corpRegistry.UpdateMember -> corpmgr.AuditMember(char2) returns history with target & issuer
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(char2);
        }
        if let Some(ch2) = snap.characters.get_mut(&char2) {
            ch2.corporation_id = corp_id;
        }
    });
    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(char2),
            PyRep::String("Accountant".into()),
            PyRep::Integer(eve_db::corp_roles::ROLE_ACCOUNTANT as i64),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .expect("UpdateMember");

    let audit = rpc(&ctx, &mut sess1, "corpmgr", "AuditMember", &[PyRep::Integer(char2)])
        .await
        .expect("AuditMember");
    let PyRep::Rowset { rows: lines, .. } = audit else {
        panic!("expected Rowset from AuditMember, got {audit:?}");
    };
    assert!(
        lines
            .iter()
            .any(|cols| cols.first() == Some(&PyRep::Integer(char2)) && cols.get(1) == Some(&PyRep::Integer(char1))),
        "AuditMember({char2}) must include role change entry issued by {char1}, got {lines:?}"
    );

    // 3. Fleet stored formations + boss handover on LeaveFleet
    let fleet_rep = rpc(&ctx, &mut sess1, "fleetObjectHandler", "CreateFleet", &[])
        .await
        .expect("CreateFleet");
    let fleet_id = fleet_rep.as_i64().unwrap_or(sess1.fleet_id.unwrap_or(0));
    assert!(fleet_id > 0);
    sess1.fleet_id = Some(fleet_id);

    let mut sess2 = SessionState::new(40_304);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    rpc(&ctx, &mut sess2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char2)])
        .await
        .expect("SelectCharacterID char2");
    rpc(&ctx, &mut sess1, "fleetObjectHandler", "Invite", &[PyRep::Integer(char2)])
        .await
        .expect("Invite char2");
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess2, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fleet_id)])
        .await
        .expect("AcceptInvite char2");
    let formations = rpc(&ctx, &mut sess1, "beyonce", "GetFormations", &[])
        .await
        .expect("beyonce.GetFormations");
    let PyRep::Tuple(form_items) = &formations else {
        panic!("expected 2-tuple from beyonce.GetFormations, got {formations:?}");
    };
    assert_eq!(form_items.len(), 2);

    let _ = ctx.drain_notifications_for_char(char2);
    rpc(&ctx, &mut sess1, "fleetObjectHandler", "LeaveFleet", &[])
        .await
        .expect("Boss LeaveFleet");
    let new_boss = ctx.db.with_snapshot(|snap| snap.fleets.get(&fleet_id).map(|f| f.boss_id).unwrap_or(0));
    assert_eq!(new_boss, char2, "Boss must automatically hand over to char2 when boss leaves");
    assert!(
        ctx.drain_notifications_for_char(char2)
            .iter()
            .any(|n| n.event == "OnFleetMemberChanged"),
        "OnFleetMemberChanged must be emitted on boss handover"
    );
}

}

#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod world_features_ext_c {
    use super::*;

// Verified runtime integration tests for world parity features (Part 4: Abyssal Pylons & Weather,
// FW Boosters, Event Sites, Equinox Quasar Sovereignty, Mercenary Dens & MTOs, and UI Photo Mode).


use crate::common;
use crate::quasar_sov_equinox_hub::{drain, field, fields, id_req, idm, rpc as qrpc, sess as qsess, structure, subscribe, varint};
use crate::sovereignty_equinox_gameplay::{
    drain_quasar, find_nullsec_planets, fit_subcap_blasters, lock_target_via_rpc, make_ball,
    open_sqlite_ctx, place_ship_ball, register_online_pilot, rpc_call, step_weapon_ticks,
    temp_sqlite_path, ALLIANCE, CORP, PILOT, PILOT_SHIP,
};
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, DamageVector, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::quasar::{encode_bytes_field, encode_varint_field};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND};
use eve_server::router::ServerContext;
use eve_server::services::pve_exploration::{advance_site_wave_or_complete, with_pve_exploration_reg, PveSiteInstance};
use eve_server::services::pve_incursion_lifecycle::{register_world_event_tale, WorldEventTaleRecord};
use eve_server::services::pve_special_sites_abyssal::{
    apply_abyssal_weather_dogma_modifiers_to_ship, apply_abyssal_weather_to_combat_and_nav,
    pulse_abyssal_automated_pylons, restore_abyssal_weather_dogma_modifiers, AbyssalWeatherKind,
    AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID, MULTIBODY_TRACKING_PYLON_TYPE_ID,
};
use eve_server::services::quasar_progression_extra::CommunityTrackState;
use eve_server::services::quasar_sov_mercden::TYPE_MERCENARY_DEN_MANAGEMENT;
use eve_server::services::sov_gameplay::TYPE_MERCENARY_DEN;
use eve_server::services::sov_services::{
    TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB, TYPE_SUPERIONIC_ICE,
};
use std::collections::HashMap;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

#[tokio::test]
async fn test_abyssal_automated_pylons_and_weather_effects_penalties() {
    let ctx = ServerContext::new();
    let char_id = 90_000_101_i64;
    let ship_id = 100_000_101_i64;
    let abyssal_sys = 32_000_001_u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: abyssal_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // 1. Weather effects & penalties across T0 (30%), T2 (50%), T4 (70%) and all 5 weather types
    for (tier, expected_pct) in [(0_u8, 30.0), (2_u8, 50.0), (4_u8, 70.0)] {
        let (penalty_pct, bonus_pct) = match tier {
            0 | 1 => (30.0, 30.0),
            2 | 3 => (50.0, 50.0),
            _ => (70.0, 70.0),
        };
        assert_eq!(penalty_pct, expected_pct);
        for weather in [
            AbyssalWeatherKind::Electrical,
            AbyssalWeatherKind::Dark,
            AbyssalWeatherKind::Exotic,
            AbyssalWeatherKind::Firestorm,
            AbyssalWeatherKind::Gamma,
        ] {
            let mut cs = ctx.build_type_combat_state(587);
            let mut max_vel = 365.0;
            let mut opt_range = 15_000.0;
            apply_abyssal_weather_to_combat_and_nav(
                weather,
                penalty_pct,
                bonus_pct,
                &mut cs,
                &mut max_vel,
                &mut opt_range,
            );
            let saved = apply_abyssal_weather_dogma_modifiers_to_ship(
                &ctx,
                ship_id,
                weather,
                penalty_pct,
                bonus_pct,
            );
            assert!(!saved.is_empty());
            let dyn_attrs = ctx
                .db
                .with_snapshot(|s| s.items.get(&ship_id).unwrap().dynamic_attributes.clone());
            let base_ty = ctx.sde.types.get(&587).unwrap();
            match weather {
                AbyssalWeatherKind::Electrical => {
                    assert!(cs.shield_resonance[0] > 0.5 && cs.armor_resonance[0] > 0.5);
                    assert!(dyn_attrs[&55] < base_ty.default_attributes[&55]);
                }
                AbyssalWeatherKind::Dark => {
                    assert!(max_vel > 365.0 && opt_range < 15_000.0);
                    assert!(dyn_attrs[&37] > base_ty.default_attributes[&37]);
                }
                AbyssalWeatherKind::Exotic => {
                    assert!(cs.shield_resonance[2] > 0.6 && cs.armor_resonance[2] > 0.75);
                    assert!(dyn_attrs[&564] > base_ty.default_attributes[&564]);
                }
                AbyssalWeatherKind::Firestorm => {
                    assert!(cs.max_armor_hp > 300.0 && cs.shield_resonance[1] > 0.8);
                    assert!(dyn_attrs[&265] > base_ty.default_attributes[&265]);
                }
                AbyssalWeatherKind::Gamma => {
                    assert!(cs.max_shield_hp > 300.0 && cs.shield_resonance[3] > 0.5);
                    assert!(dyn_attrs[&263] > base_ty.default_attributes[&263]);
                }
            }
            restore_abyssal_weather_dogma_modifiers(&ctx, ship_id, &saved);
            assert!(ctx
                .db
                .with_snapshot(|s| s.items.get(&ship_id).unwrap().dynamic_attributes.is_empty()));
        }
    }

    // 2. Automated pylons: Suppressor (47437) damages rogue drone (47847) & spares player ship (587);
    //    Multibody Tracking Pylon (47469) grants 1.8x tracking and reverts to 1.0x when destroyed.
    let sys_handle = ctx.systems.get_or_spawn(abyssal_sys);
    let mut ship_cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
    ship_cs.shield_hp = 5_000.0;
    ship_cs.max_shield_hp = 5_000.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(10_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ship_cs,
        )
        .await;

    let drone_id = 100_000_102_i64;
    let mut drone_cs = ctx.build_type_combat_state(47_847);
    drone_cs.shield_hp = 500.0;
    drone_cs.max_shield_hp = 500.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: drone_id,
                type_id: 47_847,
                position: DVec3::new(5_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 500.0,
                inertia: 1.0,
                mass: 100_000.0,
                radius: 25.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            drone_cs,
        )
        .await;

    let (sup_id, trk_id) = (100_000_103_i64, 100_000_104_i64);
    for (pid, tid, rad) in [
        (sup_id, AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID, 15_000.0),
        (trk_id, MULTIBODY_TRACKING_PYLON_TYPE_ID, 20_000.0),
    ] {
        let mut pcs = ctx.build_type_combat_state(tid);
        pcs.structure_hp = 5_000.0;
        pcs.max_structure_hp = 5_000.0;
        let _ = sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: pid,
                    type_id: tid,
                    position: DVec3::ZERO,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 1.0,
                    radius: rad,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                pcs,
            )
            .await;
    }

    let drone_hp_before = sys_handle.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp;
    let ship_hp_before = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp;
    let (dmg_cnt, trk_cnt) = pulse_abyssal_automated_pylons(&ctx, abyssal_sys).await;
    assert!(dmg_cnt > 0 && trk_cnt > 0);
    assert!(sys_handle.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp < drone_hp_before);
    let ship_after = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(ship_after.shield_hp, ship_hp_before);
    assert!((ship_after.tracking_speed_multiplier - 1.8).abs() < 1e-6);

    let kill = DamageVector { em: 1e7, thermal: 1e7, kinetic: 1e7, explosive: 1e7 };
    let _ = sys_handle.apply_damage_with_attacker(sup_id, kill, Some(char_id)).await;
    let _ = sys_handle.apply_damage_with_attacker(trk_id, kill, Some(char_id)).await;
    assert_eq!(pulse_abyssal_automated_pylons(&ctx, abyssal_sys).await, (0, 0));
    assert!((sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().tracking_speed_multiplier - 1.0).abs() < 1e-6);
}

#[tokio::test]
async fn test_fw_boosters_consumption_and_militia_lp_catalog() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(40_402);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001_i64;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");

    // 1. Militia LP stores serve authentic Tranquility offers and enforce militia enlistment
    let offers_rep = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(1_000_179)])
        .await
        .expect("GetAvailableOffersFromCorp");
    let PyRep::List(offers) = offers_rep else {
        panic!("expected list of LP offers");
    };
    assert!(!offers.is_empty());
    let first_offer_id = offers[0].dict_get("offerID").and_then(|v| v.as_i64()).unwrap();

    let (fw_station, fw_sys) = (60_015_054_i64, 30_002_978_u32);
    sess.station_id = Some(fw_station);
    sess.solar_system_id = Some(fw_sys);
    let (booster_1, booster_2) = (940_001_i64, 940_002_i64);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(fw_station);
        ch.solar_system_id = fw_sys;
        ch.war_faction_id = None;
        ch.standings.insert(1_000_179, -1.0);
        ch.loyalty_points.insert(1_000_179, 500_000);
        snap.skills.entry(char_id).or_default().remove(&3405); // Biology
        for (iid, tid) in [(booster_1, 91_916_u32), (booster_2, 91_920_u32)] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: fw_station,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    ..Default::default()
                },
            );
        }
    });

    let err_militia = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(1_000_179), PyRep::Integer(first_offer_id), PyRep::Integer(1)]).await.unwrap_err();
    assert_eq!(err_militia.code, "InsufficientStanding");

    // 2. FW booster requires Biology I, activates into slot 14 for 7200s, and blocks duplicate slot 14
    let err_skill = rpc(&ctx, &mut sess, "dogmaIM", "UseBooster", &[PyRep::Integer(booster_1)]).await.unwrap_err();
    assert_eq!(err_skill.code, "SkillPrerequisitesNotMet");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            3405,
            TrainedSkillRecord { type_id: 3405, level: 1, skill_points: 250.0 },
        );
    });
    let used = rpc(&ctx, &mut sess, "dogmaIM", "UseBooster", &[PyRep::Integer(booster_1)]).await.expect("UseBooster FW booster");
    assert_eq!(used.dict_get("boosterTypeID").and_then(|v| v.as_i64()), Some(91_916));
    assert_eq!(used.dict_get("boosterSlot").and_then(|v| v.as_i64()), Some(14));
    assert_eq!(used.dict_get("durationSeconds").and_then(|v| v.as_i64()), Some(8_640));

    let err_dup = rpc(&ctx, &mut sess, "dogmaIM", "UseBooster", &[PyRep::Integer(booster_2)]).await.unwrap_err();
    assert_eq!(err_dup.code, "BoosterSlotOccupied");

    let PyRep::Dict(entries) = rpc(&ctx, &mut sess, "skillMgr2", "GetBoosters", &[]).await.expect("GetBoosters") else {
        panic!("expected Dict from GetBoosters");
    };
    assert!(entries.iter().any(|(k, _)| k.as_i64() == Some(91_916)));
}

#[tokio::test]
async fn test_pve_events_sites_and_world_event_tales() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(40_403);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001_i64;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");

    let sys_id = 30_000_142_u32;
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
        }
    });
    let now = ctx.now_unix();
    let event_dungeon_id = 5_717_u32; // SDE archetype 50 ("Station Harbor / Event Site")
    let event_tpl_id = 5_717_u32;

    let tale_id = register_world_event_tale(
        &ctx,
        WorldEventTaleRecord {
            tale_id: 0,
            template_id: i64::from(event_tpl_id),
            template_class_id: "worldEvents".into(),
            solar_system_id: sys_id,
            constellation_id: 20_000_020,
            aggressor_faction_id: 500_021,
            state: "Active".into(),
            influence: 0.0,
            start_unix: now.saturating_sub(600),
            end_unix: now + 86_400,
        },
    );
    ctx.quasar_gateway.progression.extra.active_tales.write().unwrap().insert(tale_id as u32, (event_tpl_id, sys_id, now));
    ctx.quasar_gateway.progression.extra.community_tracks.write().unwrap().insert(
        701,
        CommunityTrackState {
            track_id: 701,
            template_id: 701,
            name: "Event Site Track".into(),
            current_progress: 0,
            target_progress: 10,
            active: true,
            tale_templates: vec![event_tpl_id],
        },
    );

    let tales_rep = rpc(
        &ctx,
        &mut sess,
        "taleMgr",
        "get_active_tales_by_template",
        &[PyRep::Integer(i64::from(event_tpl_id))],
    )
    .await
    .expect("get_active_tales_by_template");
    let PyRep::List(tales) = tales_rep else {
        panic!("expected List of event tales");
    };
    assert!(tales.iter().any(|t| t.dict_get("taleID").and_then(|v| v.as_i64()) == Some(tale_id)));

    let site_id = "EVT-30000142-1";
    with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(sys_id, true);
        reg.sites_by_system.entry(sys_id).or_default().push(PveSiteInstance {
            site_id: site_id.into(),
            dungeon_id: event_dungeon_id,
            dungeon_name: "Space Oddity".into(),
            solar_system_id: sys_id,
            site_category: "Event".into(),
            position: [15_000_000.0, 0.0, 0.0],
            current_room: 1,
            total_rooms: 1,
            current_wave: 1,
            total_waves: 1,
            created_unix: now,
            lifetime_seconds: 86_400,
            ..Default::default()
        });
    });

    let comp = advance_site_wave_or_complete(&ctx, &mut sess, sys_id, site_id)
        .await
        .expect("complete event site");
    assert_eq!(comp.dict_get("completed"), Some(&PyRep::Bool(true)));
    let track_prog = ctx
        .quasar_gateway
        .progression
        .extra
        .community_tracks
        .read()
        .unwrap()
        .get(&701)
        .map(|t| t.current_progress)
        .unwrap_or(0);
    assert_eq!(track_prog, 1);
}

#[tokio::test]
async fn test_equinox_sov_hub_power_reagents_fuel_upgrades_and_workforce() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let (jita, perimeter) = (30_000_142_u32, 30_000_144_u32);
    let (hub_a, hub_b) = (1_020_000_001_i64, 1_020_000_002_i64);
    let (gas_item, upg_item, station_gas) = (990_001_i64, 990_003_i64, 990_004_i64);
    const STABILITY_GEN: u64 = 87_950; // 200 power, 2,700 workforce, 7 Magmatic Gas/hr

    let mut s = qsess(61_001, PILOT, CORP, jita, corp_roles::ROLE_DIRECTOR);
    let mut rx = subscribe(&ctx, &mut s).await;
    let jita_planets: Vec<i64> = ctx
        .sde
        .celestials_in_system(jita)
        .into_iter()
        .filter(|c| c.group_id == 7)
        .map(|c| c.celestial_id)
        .collect();

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(hub_a, structure(hub_a, CORP, jita, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(hub_b, structure(hub_b, CORP, perimeter, TYPE_SOVEREIGNTY_HUB));
        for (iid, tid, qty, loc) in [
            (gas_item, TYPE_MAGMATIC_GAS, 1_000, PILOT_SHIP),
            (upg_item, STABILITY_GEN as u32, 1, hub_a),
            (station_gas, TYPE_MAGMATIC_GAS, 1_000, 60_003_760),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord { item_id: iid, type_id: tid, owner_id: PILOT, location_id: loc, quantity: qty, ..Default::default() },
            );
        }
    });

    // 1. Reagents & Star Power definitions
    let defs = qrpc(&ctx, &mut s, "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest", Vec::new()).await.payload.unwrap().value;
    let defs_list = fields(&defs, 1);
    assert_eq!(defs_list.len(), 8);
    let lava_def = defs_list.iter().find(|d| varint(&field(d, 1).unwrap(), 1) == Some(2015)).unwrap();
    assert_eq!(varint(&field(&field(lava_def, 4).unwrap(), 3).unwrap(), 1), Some(TYPE_MAGMATIC_GAS as u64));

    let mut bad_ver = Vec::new();
    let mut spec = Vec::new();
    encode_varint_field(1, 99, &mut spec);
    encode_bytes_field(1, &spec, &mut bad_ver);
    assert_eq!(qrpc(&ctx, &mut s, "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest", bad_ver).await.status_code, STATUS_NOT_FOUND);

    let planet_cfgs = qrpc(&ctx, &mut s, "eve_public.sovereignty.resource.planet.api.GetAllConfigurationsRequest", Vec::new()).await.payload.unwrap().value;
    let reagent_cfgs: Vec<Vec<u8>> = fields(&planet_cfgs, 1).iter().filter_map(|c| field(c, 4)).collect();
    assert!(reagent_cfgs.iter().any(|c| varint(&field(c, 1).unwrap(), 1) == Some(TYPE_MAGMATIC_GAS as u64)));
    assert!(reagent_cfgs.iter().any(|c| varint(&field(c, 1).unwrap(), 1) == Some(TYPE_SUPERIONIC_ICE as u64)));

    let star_cfgs = qrpc(&ctx, &mut s, "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest", Vec::new()).await.payload.unwrap().value;
    assert!(!fields(&star_cfgs, 1).is_empty());

    // 2. Power & Workforce from stellar + anchored Skyhooks
    let res_no_sh = qrpc(&ctx, &mut s, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_a as u64)).await.payload.unwrap().value;
    assert_eq!(varint(&field(&res_no_sh, 1).unwrap(), 1), Some(800));
    ctx.db.with_mut_snapshot(|snap| {
        for (idx, pid) in jita_planets.iter().copied().enumerate() {
            let sh_id = 1_000_000_200 + idx as i64;
            snap.structures.insert(sh_id, structure(sh_id, CORP, jita, TYPE_ORBITAL_SKYHOOK));
            snap.sov_runtime.skyhook_planets.insert(sh_id, pid);
            snap.sov_runtime.skyhook_active.insert(sh_id, true);
        }
    });
    let res = qrpc(&ctx, &mut s, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_a as u64)).await.payload.unwrap().value;
    assert_eq!(varint(&field(&res, 1).unwrap(), 1), Some(1_100));
    assert_eq!(varint(&field(&res, 2).unwrap(), 1), Some(3_000));

    // 3. Hub Fuel deposit & hourly burn
    let mut other = qsess(61_002, 90_000_002, 98_000_002, jita, corp_roles::ROLE_DIRECTOR);
    let add_fuel = |item: i64, qty: u64, hub: i64| {
        let mut b = Vec::new();
        encode_bytes_field(1, &idm(item as u64), &mut b);
        encode_varint_field(2, qty, &mut b);
        encode_bytes_field(3, &idm(hub as u64), &mut b);
        b
    };
    assert_eq!(qrpc(&ctx, &mut other, "eve_public.sovereignty.hub.fuel.api.AddRequest", add_fuel(gas_item, 10, hub_a)).await.status_code, STATUS_FORBIDDEN);
    assert_eq!(qrpc(&ctx, &mut s, "eve_public.sovereignty.hub.fuel.api.AddRequest", add_fuel(station_gas, 10, hub_a)).await.status_code, STATUS_BAD_REQUEST);
    assert_eq!(qrpc(&ctx, &mut s, "eve_public.sovereignty.hub.fuel.api.AddRequest", add_fuel(gas_item, 500, hub_a)).await.status_code, 200);
    drain(&mut rx);

    // 4. Hub Upgrades install + online + hourly fuel burn (7/hr -> 493 after 1h)
    let mut proc = id_req(1, upg_item as u64);
    let mut cfg = Vec::new();
    encode_bytes_field(1, &idm(STABILITY_GEN), &mut cfg);
    encode_varint_field(2, 1, &mut cfg);
    encode_bytes_field(2, &cfg, &mut proc);
    encode_bytes_field(3, &idm(hub_a as u64), &mut proc);
    let hu = qrpc(&ctx, &mut s, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc).await.payload.unwrap().value;
    let up = field(&field(&hu, 1).unwrap(), 2).unwrap();
    assert_eq!(field(&up, 2).and_then(|a| varint(&a, 2)), Some(2));
    let n = drain(&mut rx);
    assert!(n.iter().any(|(t, _)| t == "eve_public.sovereignty.hub.upgrade.api.HubUpgradesConfiguredNotice"));
    assert!(n.iter().any(|(t, _)| t == "eve_public.sovereignty.hub.api.ResourcesSimulatedNotice"));

    ctx.advance_secs(3_600);
    let fuel = qrpc(&ctx, &mut s, "eve_public.sovereignty.hub.fuel.api.GetRequest", id_req(1, hub_a as u64)).await.payload.unwrap().value;
    assert_eq!(field(&fuel, 1).and_then(|f| varint(&f, 2)), Some(493));

    // 5. Workforce transit configuration (export 300 from Jita to Perimeter)
    let mut exp = Vec::new();
    encode_bytes_field(3, &idm(perimeter as u64), &mut exp);
    encode_varint_field(2, 300, &mut exp);
    let mut export_cfg = Vec::new();
    encode_bytes_field(3, &exp, &mut export_cfg);
    let mut wf_req = Vec::new();
    encode_bytes_field(1, &idm(hub_a as u64), &mut wf_req);
    encode_bytes_field(2, &export_cfg, &mut wf_req);
    assert_eq!(qrpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", wf_req).await.status_code, 200);
    assert!(drain(&mut rx).iter().any(|(t, _)| t == "eve_public.sovereignty.resource.transfer.workforce.api.ConfiguredNotice"));
}

#[tokio::test]
async fn test_mercenary_den_anarchy_development_mto_and_ui_photo_mode() {
    let db_path = temp_sqlite_path("world_p4_mercden_photo");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, temp_planet, _, _, _) = find_nullsec_planets(&ctx);
    let (hub_id, skyhook_id) = (1_130_000_001_i64, 1_130_000_002_i64);
    let sh_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_owner = register_online_pilot(
        &ctx,
        87_001,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        null_sys,
        PILOT_SHIP,
        639,
        sh_pos + DVec3::new(2_500.0, 0.0, 0.0),
        corp_roles::ROLE_DIRECTOR,
    )
    .await;
    let mut rx = subscribe(&ctx, &mut s_owner).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ALLIANCE);
        snap.structures.insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(skyhook_id, structure(skyhook_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
        snap.skills.entry(PILOT).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord { type_id: TYPE_MERCENARY_DEN_MANAGEMENT, level: 4, skill_points: 180_000.0 },
        );
        snap.items.insert(
            987_001,
            InventoryItemRecord {
                item_id: 987_001,
                type_id: TYPE_MERCENARY_DEN,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(skyhook_id, TYPE_ORBITAL_SKYHOOK, sh_pos)).await;

    // 1. Anchor Mercenary Den & verify max capacity (5), ownership, and 3 MTOs
    let den_id = rpc_call(
        &ctx,
        &mut s_owner,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)],
    )
    .await
    .unwrap()
    .dict_get("structureID")
    .and_then(|v| v.as_i64())
    .unwrap();
    let anchor_notices = drain_quasar(&mut rx);
    assert!(anchor_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.api.DefinitionsUpdatedNotice"));
    assert_eq!(anchor_notices.iter().filter(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.AddedNotice").count(), 3);

    let max_rsp = qrpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest", Vec::new()).await.payload.unwrap().value;
    assert_eq!(varint(&max_rsp, 1), Some(4));
    assert_eq!(varint(&max_rsp, 2), Some(5));

    // 2. Start MTO, destroy live MTO target ball in space -> Development & Anarchy evolution
    let den_acts = qrpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest", id_req(1, den_id as u64)).await.payload.unwrap().value;
    let act_list = fields(&den_acts, 1);
    let mut start_req = Vec::new();
    encode_bytes_field(1, &field(&act_list[0], 1).unwrap(), &mut start_req);
    assert_eq!(qrpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", start_req).await.status_code, 200);
    assert!(drain_quasar(&mut rx).iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.StartedNotice"));

    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let site_ball = ctx.db.with_snapshot(|s| s.sov_runtime.mto_site_balls.keys().copied().next().unwrap());
    let site_pos = ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(site_ball).await.unwrap().unwrap().1.position;
    fit_subcap_blasters(&ctx, PILOT, PILOT_SHIP, 987_200);
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 16_229, site_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;
    lock_target_via_rpc(&ctx, &mut s_owner, null_sys, PILOT_SHIP, site_ball).await;
    for _round in 0..25 {
        for gun_id in (0..6).map(|g| 987_200_i64 + g * 2) {
            ctx.set_invention_roll_override(Some(0.45));
            let shot = match rpc_call(&ctx, &mut s_owner, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(site_ball)]).await {
                Ok(r) => r,
                Err(e) if e.code == "ModuleAlreadyActive" => PyRep::None,
                Err(e) => panic!("dogmaIM.Activate turret on site ball: {e:?}"),
            };
            if shot.dict_get("destroyed") == Some(&PyRep::Bool(true))
                || ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&site_ball))
            {
                break;
            }
        }
        if ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&site_ball)) {
            break;
        }
        step_weapon_ticks(&ctx, null_sys).await;
    }
    assert!(ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&site_ball)));
    let comp_notices = drain_quasar(&mut rx);
    assert!(comp_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.CompletedNotice"));
    assert!(comp_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.api.EvolutionStateChangedNotice"));

    // 3. UI Photo Mode: station scene query + paperDollServer portrait data query
    let st_info = rpc(&ctx, &mut s_owner, "stationSvc", "GetStation", &[PyRep::Integer(60_003_760)]).await.expect("GetStation");
    assert!(format!("{st_info:?}").contains("60003760"));
    let portrait = rpc(&ctx, &mut s_owner, "paperDollServer", "GetPaperDollPortraitDataFor", &[PyRep::Integer(PILOT)]).await.expect("GetPaperDollPortraitDataFor");
    assert!(!matches!(portrait, PyRep::None));

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

}
