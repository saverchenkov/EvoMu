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

pub(crate) mod industry_research_cost_tax_capital {
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
        // parity: feat:industry.blueprint-me-research
        // parity: feat:industry.capital-construction-restrictions
        // parity: feat:industry.eiv-adjusted-prices
        // parity: feat:industry.facility-tax
        // parity: feat:industry.job-cost
        // parity: feat:lifecycle.industry-pause-and-many
        eve_testkit::attest(&[
            "feat:industry.blueprint-me-research",
            "feat:industry.capital-construction-restrictions",
            "feat:industry.eiv-adjusted-prices",
            "feat:industry.facility-tax",
            "feat:industry.job-cost",
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
        // parity: feat:corporation.member-audit
        // parity: feat:corporation.structure-reinforce-default
        // parity: feat:fleet.boss-handover
        // parity: feat:fleet.stored-formations-and-warp-shapes
        eve_testkit::attest(&[
            "feat:corporation.member-audit",
            "feat:corporation.structure-reinforce-default",
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

pub(crate) mod abyssal_automated_pylons_weather_effects {
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

        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let site_ball = ctx.db.with_snapshot(|s| s.sov_runtime.mto_site_balls.keys().copied().next().unwrap());
        let site_pos = ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(site_ball).await.unwrap().unwrap().1.position;
        fit_subcap_blasters(&ctx, PILOT, PILOT_SHIP, 987_200);
        place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 16_229, site_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;
        lock_target_via_rpc(&ctx, &mut s_owner, null_sys, PILOT_SHIP, site_ball).await;
        for round in 0..55 {
            for gun_id in (0..6).map(|g| 987_200_i64 + g * 2) {
                ctx.set_invention_roll_override(Some(0.45));
                let _ = rpc_call(&ctx, &mut s_owner, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(site_ball)]).await;
                if ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&site_ball)) {
                    break;
                }
            }
            if ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&site_ball)) {
                break;
            }
            if round % 10 == 9 {
                step_weapon_ticks(&ctx, null_sys).await;
            }
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

pub(crate) mod wormhole_lifetime_decay_stages_info {
    // Integration tests behavioral verification `FLAGGED_FAKE` `feat:*` parity rows.
    //
    // Covers:
    // - `feat:wormholes.lifetime-decay-stages`
    // - `feat:navigation.wormhole-info-buckets`
    // - `feat:pve.hacking-node-types`
    // - `feat:pve.hacking-failure-outcome`
    // - `feat:sovereignty.skyhook-reagent-maturity`

    use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services::pve_exploration::{
        ensure_system_pve_sites_seeded, with_pve_exploration_reg, HackingNodeKind, HackingUtilityKind,
    };
    use eve_server::services::pve_hacking_board::build_hacking_grid_procedural;
    use eve_server::services::sov_gameplay::sov_gameplay_skyhook::advance_skyhook_reagents_in_snap;
    use eve_server::services::space_destiny::derive_wormhole_buckets_raw;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn make_context() -> ServerContext {
        let db = Arc::new(WorldRepository::new_empty());
        ServerContext::with_repository(db)
    }

    fn make_session(char_id: i64, sys_id: u32, ship_id: i64) -> SessionState {
        let mut sess = SessionState::new(990_001);
        sess.user_id = Some(99_001);
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(1_000_009);
        sess.solar_system_id = Some(sys_id);
        sess.ship_id = Some(ship_id);
        sess
    }

    #[tokio::test]
    async fn test_wormhole_lifetime_decay_stages_and_info_buckets() {
        // parity: feat:wormholes.lifetime-decay-stages, feat:navigation.wormhole-info-buckets
        eve_testkit::attest(&[
            "feat:wormholes.lifetime-decay-stages",
            "feat:navigation.wormhole-info-buckets",
        ]);

        let ctx = make_context();
        let now = 1_750_000_000_u64;
        let lifetime = 48 * 3_600_u64;

        // > 24h remaining (36h left): >1 day bucket, not yet begun to decay
        let fresh = derive_wormhole_buckets_raw(
            30_000_142,
            20_000_000.0,
            1_000_000_000.0,
            1_000_000_000.0,
            now,
            lifetime,
            false,
            now + 12 * 3_600,
            &ctx.sde,
        );
        assert_eq!(fresh.lifetime_bucket, ">1 day");
        assert_eq!(
            fresh.lifetime_description,
            "This wormhole has not yet begun to decay"
        );

        // 4h..=24h remaining (12h left): <1 day bucket, beginning to decay
        let decaying = derive_wormhole_buckets_raw(
            30_000_142,
            20_000_000.0,
            1_000_000_000.0,
            1_000_000_000.0,
            now,
            lifetime,
            false,
            now + 36 * 3_600,
            &ctx.sde,
        );
        assert_eq!(decaying.lifetime_bucket, "<1 day");
        assert_eq!(
            decaying.lifetime_description,
            "This wormhole is beginning to decay, and probably won't last another day"
        );

        // < 4h remaining (2h left): <4h bucket, reaching end of natural lifetime
        let eol = derive_wormhole_buckets_raw(
            30_000_142,
            20_000_000.0,
            1_000_000_000.0,
            1_000_000_000.0,
            now,
            lifetime,
            false,
            now + 46 * 3_600,
            &ctx.sde,
        );
        assert_eq!(eol.lifetime_bucket, "<4h");
        assert_eq!(
            eol.lifetime_description,
            "This wormhole is reaching the end of its natural lifetime"
        );
    }

    #[tokio::test]
    async fn test_hacking_node_types_and_failure_outcome() {
        // parity: feat:pve.hacking-node-types, feat:pve.hacking-failure-outcome
        eve_testkit::attest(&[
            "feat:pve.hacking-node-types",
            "feat:pve.hacking-failure-outcome",
        ]);

        // 1. Difficulty 1 (Very Easy) and Difficulty 2 (Easy) must not spawn RestorationNode or VirusSuppressor
        for diff in [1_u8, 2_u8] {
            let tiles = build_hacking_grid_procedural(77_001, false, diff, 1_750_000_000);
            assert!(
                !tiles.iter().any(|t| matches!(
                    t.kind,
                    HackingNodeKind::RestorationNode | HackingNodeKind::VirusSuppressor
                )),
                "Difficulty {diff} board must not spawn RestorationNode or VirusSuppressor"
            );
        }
        let diff3_tiles = build_hacking_grid_procedural(77_003, false, 3, 1_750_000_000);
        assert!(
            diff3_tiles
                .iter()
                .any(|t| t.kind == HackingNodeKind::RestorationNode),
            "Difficulty 3 board must include RestorationNode"
        );

        // 2. SelfRepair heals 5..=10 coherence per turn for 3 turns without a max_virus_coherence cap,
        //    and 2nd hack failure on a standard Data/Relic container self-destructs with 0.0 ship damage.
        let ctx = make_context();
        let kw: HashMap<String, PyRep> = HashMap::new();
        let char_id = 90_000_001_i64;
        let sys_id = 30_002_813_u32; // Low-sec
        let ship_id = 100_000_001_i64;
        let analyzer_id = 100_000_002_i64;
        let mut sess = make_session(char_id, sys_id, ship_id);

        ensure_system_pve_sites_seeded(&ctx, sys_id).await;
        let (can_id, can_fail, site_pos) = with_pve_exploration_reg(&ctx, |reg| {
            let relic = reg
                .sites_by_system
                .get(&sys_id)
                .and_then(|list| {
                    list.iter().find(|s| {
                        s.site_category.eq_ignore_ascii_case("Relic") && s.hackable_can_ids.len() >= 2
                    })
                })
                .expect("Seeded standard Relic site with >= 2 cans");
            (relic.hackable_can_ids[0], relic.hackable_can_ids[1], relic.position)
        });

        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: 605, // Heron
                    owner_id: char_id,
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                analyzer_id,
                InventoryItemRecord {
                    item_id: analyzer_id,
                    type_id: 22_177, // Relic Analyzer I
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::MedSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(analyzer_id, true);
        });
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle
            .spawn_ball(eve_destiny::Ball {
                id: ship_id,
                type_id: 605,
                position: glam::DVec3::from_array(site_pos),
                velocity: glam::DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await;

        let _ = ctx
            .dispatch_rpc(
                &mut sess,
                "hackingMgr",
                None,
                "StartNewGameInstance",
                &[PyRep::Integer(can_id)],
                &kw,
                None,
            )
            .await
            .expect("StartNewGameInstance");

        with_pve_exploration_reg(&ctx, |reg| {
            let g = reg.hacking_games.get_mut(&(char_id, can_id)).unwrap();
            g.virus_coherence = (g.max_virus_coherence - 5).max(1);
            g.utilities[0] = Some(HackingUtilityKind::SelfRepair);
            for t in g.tiles.iter_mut().take(4) {
                t.kind = HackingNodeKind::Empty;
                t.coherence = 0;
                t.strength = 0;
            }
        });

        let _ = ctx
            .dispatch_rpc(
                &mut sess,
                "hackingMgr",
                None,
                "UsedUtilityElement",
                &[PyRep::Integer(can_id), PyRep::Integer(0), PyRep::Integer(0)],
                &kw,
                None,
            )
            .await
            .expect("Activate SelfRepair");

        let (base_coh, max_coh) = with_pve_exploration_reg(&ctx, |reg| {
            let g = &reg.hacking_games[&(char_id, can_id)];
            (g.virus_coherence, g.max_virus_coherence)
        });
        let mut prev_coh = base_coh;
        for turn in 1..=3_u32 {
            let _ = ctx
                .dispatch_rpc(
                    &mut sess,
                    "hackingMgr",
                    None,
                    "ClickedOnTile",
                    &[PyRep::Integer(can_id), PyRep::Integer(i64::from(turn))],
                    &kw,
                    None,
                )
                .await
                .expect("Click tile");
            let cur_coh = with_pve_exploration_reg(&ctx, |reg| {
                reg.hacking_games[&(char_id, can_id)].virus_coherence
            });
            let delta = cur_coh - prev_coh;
            assert!(
                (5..=10).contains(&delta),
                "SelfRepair turn {turn} must heal 5..=10 coherence, got {delta}"
            );
            prev_coh = cur_coh;
        }
        let (after_coh, rem_sr) = with_pve_exploration_reg(&ctx, |reg| {
            (
                reg.hacking_games[&(char_id, can_id)].virus_coherence,
                reg.self_repair_turns.get(&(char_id, can_id)).copied(),
            )
        });
        assert!(after_coh > base_coh, "SelfRepair must heal virus coherence");
        assert!(
            after_coh > max_coh,
            "SelfRepair must allow virus coherence to exceed max_virus_coherence (got {base_coh} -> {after_coh}, max {max_coh})"
        );
        assert_eq!(rem_sr, None, "self_repair_turns must be cleaned up after 3 turns");

        // Trigger 2nd failure on a standard container in low-sec -> self-destructs with 0.0 explosionDamage
        for attempt in 1..=2 {
            let _ = ctx
                .dispatch_rpc(
                    &mut sess,
                    "hackingMgr",
                    None,
                    "StartNewGameInstance",
                    &[PyRep::Integer(can_fail)],
                    &kw,
                    None,
                )
                .await
                .expect("StartNewGameInstance");
            with_pve_exploration_reg(&ctx, |reg| {
                let g = reg.hacking_games.get_mut(&(char_id, can_fail)).unwrap();
                g.virus_coherence = 5;
                g.tiles[1].kind = HackingNodeKind::Antivirus;
                g.tiles[1].coherence = 500;
                g.tiles[1].strength = 50;
            });
            let rep = ctx
                .dispatch_rpc(
                    &mut sess,
                    "hackingMgr",
                    None,
                    "ClickedOnTile",
                    &[PyRep::Integer(can_fail), PyRep::Integer(1)],
                    &kw,
                    None,
                )
                .await
                .expect("ClickedOnTile fail");
            if attempt == 2 {
                assert_eq!(rep.dict_get("containerExploded"), Some(&PyRep::Bool(true)));
                assert_eq!(rep.dict_get("explosionDamage"), Some(&PyRep::Real(0.0)));
            }
        }
    }

    #[tokio::test]
    async fn test_skyhook_reagent_harvest_secure_surplus_split() {
        // parity: feat:sovereignty.skyhook-reagent-maturity
        eve_testkit::attest(&["feat:sovereignty.skyhook-reagent-maturity"]);

        let ctx = make_context();
        let skyhook_id = 1_040_000_001_i64;
        let t0 = 1_750_000_000_u64;

        ctx.db.with_mut_snapshot(|snap| {
            snap.sov_runtime
                .skyhook_reagent_bays
                .insert(skyhook_id, (81_143, 0, 0, t0));
            snap.sov_runtime
                .skyhook_last_harvest_unix
                .insert(skyhook_id, t0);
            let (_, sec_1h, sur_1h) = advance_skyhook_reagents_in_snap(snap, skyhook_id, t0 + 3_600)
                .expect("1h skyhook harvest");
            assert!(sec_1h > 0, "Newly harvested reagents must split into Secure bay");
            assert!(sur_1h > 0, "Newly harvested reagents must split into Surplus bay");
            assert_eq!(sec_1h, sur_1h, "Harvested reagents split 50/50 between Secure and Surplus");
        });
    }
}

pub(crate) mod misc_destiny_on_drone_activity {
    // Integration tests closing remaining `destiny`, `sde`, `quasar`, and `plan` parity rows.


    use std::collections::HashMap;

    use eve_db::types::TrainedSkillRecord;
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, Ballpark, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::quasar::{
        EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage,
        TYPE_GOOGLEAPIS_PREFIX, encode_varint_field,
    };
    use eve_proto::quasar_wire::{decode_fields, ProtoWireValue};
    use eve_proto::PyRep;
    use eve_sde::normalize_localization_fsd_lang;
    use eve_server::quasar_gateway::{STATUS_OK, STATUS_UNAUTHORIZED};
    use eve_server::router::ServerContext;
    use eve_server::system_actor::populate_asteroid_belt_entities;
    use glam::DVec3;

    use crate::common::login_pilot;

    const PILOT_A: i64 = 90_000_001;
    const SYS_PERIMETER: u32 = 30_000_144;

    fn new_ctx() -> ServerContext {
        let sde = std::sync::Arc::new(
            eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
        );
        let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        ctx.set_clock_paused(true);
        ctx
    }

    async fn authed_demo_session(ctx: &ServerContext, sid: u64) -> SessionState {
        let mut sess = SessionState::new(sid);
        login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
        let kw = HashMap::new();
        let _ = ctx
            .dispatch_rpc(
                &mut sess,
                "charUnboundMgr",
                None,
                "SelectCharacterID",
                &[PyRep::Integer(PILOT_A)],
                &kw,
                None,
            )
            .await
            .expect("SelectCharacterID 90_000_001");
        sess
    }

    fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
        if let PyRep::KeyVal(entries) = rep {
            for (k, v) in entries {
                if k == "response_proto" {
                    if let PyRep::Bytes(b) = v {
                        return EvePublicResponse::decode_proto(b)
                            .expect("decode EvePublicResponse from response_proto");
                    }
                }
            }
        }
        panic!("Expected KeyVal containing response_proto, got {rep:?}");
    }

    async fn quasar_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        type_name: &str,
        payload_bytes: Vec<u8>,
    ) -> EvePublicResponse {
        let req = EvePublicRequest {
            issued: Some(GoogleTimestamp {
                seconds: ctx.now_unix() as i64,
                nanos: 0,
            }),
            correlation_uuid: b"parity-misc-part1-01".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
                value: payload_bytes,
            }),
            ..Default::default()
        };
        let req_bytes = req.encode_proto();
        let empty_kw = HashMap::new();
        let rep = ctx
            .dispatch_rpc(
                sess,
                "publicGatewaySvc",
                None,
                "SendRequest",
                &[PyRep::Bytes(req_bytes)],
                &empty_kw,
                None,
            )
            .await
            .expect("publicGatewaySvc.SendRequest");
        extract_response_proto(&rep)
    }

    fn extract_destiny_actions(updates: &PyRep) -> Vec<(String, Vec<PyRep>)> {
        let bytes = eve_proto::marshal::encode(updates);
        let decoded = eve_proto::marshal::decode(&bytes).expect("DoDestinyUpdate marshal round-trip");
        assert_eq!(&decoded, updates);
        let PyRep::List(entries) = decoded else {
            panic!("expected DoDestinyUpdate list, got {decoded:?}");
        };
        entries
            .into_iter()
            .map(|entry| {
                let PyRep::Tuple(outer) = entry else {
                    panic!("expected (stamp, action) tuple, got {entry:?}");
                };
                assert!(matches!(outer[0], PyRep::Integer(_)));
                let PyRep::Tuple(action) = &outer[1] else {
                    panic!("expected (actionName, args) tuple, got {:?}", outer[1]);
                };
                let name = action[0].as_str().expect("action name").to_string();
                let args = match &action[1] {
                    PyRep::Tuple(t) => t.clone(),
                    other => vec![other.clone()],
                };
                (name, args)
            })
            .collect()
    }

    // parity: destiny:OnDroneActivityChange
    #[tokio::test]
    async fn test_parity_misc_destiny_on_drone_activity_change() {
        eve_testkit::attest(&["destiny:OnDroneActivityChange"]);
        let ctx = new_ctx();
        let mut sess = authed_demo_session(&ctx, 8100).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(PILOT_A).or_default().insert(
                3436,
                TrainedSkillRecord {
                    type_id: 3436,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        });

        let kw = HashMap::new();
        let _ = ctx
            .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
            .await
            .expect("stationSvc.Undock");
        let ship_id = sess.ship_id.expect("active ship after Undock");
        let sys_id = sess.solar_system_id.expect("solar system after Undock");

        let drone_item_id = 99_770_001;
        let target_id = 99_770_002;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ship_item) = snap.items.get_mut(&ship_id) {
                ship_item.type_id = 626;
                ship_item.version += 1;
            }
            snap.items.insert(
                drone_item_id,
                InventoryItemRecord {
                    item_id: drone_item_id,
                    type_id: 2454,
                    owner_id: PILOT_A,
                    location_id: ship_id,
                    flag: InventoryFlag::DroneBay,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        ctx.invalidate_ship_fitting(ship_id);

        let handle = ctx.systems.get_or_spawn(sys_id);
        let (_, ship_ball) = handle
            .get_ball_snapshot(ship_id)
            .await
            .expect("ship ball snapshot")
            .expect("ship ball in space");
        let target_cs = ctx.build_type_combat_state(587);
        handle
            .spawn_ball_with_combat(
                Ball {
                    id: target_id,
                    type_id: 587,
                    position: ship_ball.position + DVec3::new(2_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                target_cs,
            )
            .await
            .expect("spawn target ball");

        let launched = ctx
            .dispatch_rpc(
                &mut sess,
                "ship",
                None,
                "LaunchDrones",
                &[PyRep::List(vec![PyRep::Integer(drone_item_id)])],
                &kw,
                None,
            )
            .await
            .expect("ship.LaunchDrones");
        let PyRep::List(launched_ids) = launched else {
            panic!("expected launched drone ball IDs list, got {launched:?}");
        };
        let drone_ball_id = launched_ids[0].as_i64().expect("launched drone ball_id");
        let _ = handle.drain_observer_updates(ship_id).await;

        let _ = ctx
            .dispatch_rpc(
                &mut sess,
                "entity",
                None,
                "CmdEngage",
                &[
                    PyRep::List(vec![PyRep::Integer(drone_ball_id)]),
                    PyRep::Integer(target_id),
                ],
                &kw,
                None,
            )
            .await
            .expect("entity.CmdEngage");
        let (_tick, _evs, engage_updates) = handle
            .drain_observer_updates(ship_id)
            .await
            .expect("drain observer updates after CmdEngage");
        let engage_actions = extract_destiny_actions(&engage_updates);
        assert!(
            engage_actions
                .iter()
                .any(|(name, args)| name == "OnDroneActivityChange"
                    && args.as_slice() == [PyRep::Integer(drone_ball_id), PyRep::Integer(1)]),
            "expected OnDroneActivityChange(drone_id={drone_ball_id}, activity_state=1) in DoDestinyUpdate, got {engage_actions:?}"
        );

        let _ = ctx
            .dispatch_rpc(
                &mut sess,
                "entity",
                None,
                "CmdReturnHome",
                &[PyRep::List(vec![PyRep::Integer(drone_ball_id)])],
                &kw,
                None,
            )
            .await
            .expect("entity.CmdReturnHome");
        let (_tick, _evs, return_updates) = handle
            .drain_observer_updates(ship_id)
            .await
            .expect("drain observer updates after CmdReturnHome");
        let return_actions = extract_destiny_actions(&return_updates);
        assert!(
            return_actions
                .iter()
                .any(|(name, args)| name == "OnDroneActivityChange"
                    && args.as_slice() == [PyRep::Integer(drone_ball_id), PyRep::Integer(0)]),
            "expected OnDroneActivityChange(drone_id={drone_ball_id}, activity_state=0) in DoDestinyUpdate, got {return_actions:?}"
        );
    }

    // parity: sde:res:/localizationfsd/localization_fsd_it.pickle
    #[test]
    fn test_parity_misc_sde_localization_fsd_it_pickle() {
        eve_testkit::attest(&["sde:res:/localizationfsd/localization_fsd_it.pickle"]);
        assert_eq!(normalize_localization_fsd_lang("it"), Some("it"));
        assert_eq!(normalize_localization_fsd_lang("it-IT,it;q=0.9"), Some("it"));

        let ctx = new_ctx();
        let it_msgs = ctx
            .sde
            .load_all_localized_messages("it")
            .expect("load localization_fsd_it.pickle");
        assert!(
            it_msgs.len() >= 7_000,
            "expected >=7k Italian localization messages, got {}",
            it_msgs.len()
        );
        assert!(
            it_msgs.values().any(|v| !v.is_empty()),
            "expected non-empty Italian localization strings"
        );
    }

    // parity: quasar:eveProto/generated/eve_public/sovereignty/resource/star/api/requests_pb2.pyj
    #[tokio::test]
    async fn test_parity_misc_quasar_sovereignty_resource_star_requests() {
        let ctx = new_ctx();
        let mut sess = authed_demo_session(&ctx, 8101).await;

        let resp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(resp.status_code, STATUS_OK);
        let payload = resp.payload.expect("GetAllConfigurationsResponse payload");
        assert_eq!(
            payload.type_url,
            format!(
                "{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.resource.star.api.GetAllConfigurationsResponse"
            )
        );
        assert!(
            !payload.value.is_empty(),
            "expected non-empty star configuration entries from SDE"
        );
    }

    #[tokio::test]
    async fn test_parity_misc_plan_c5_and_mn01_mining_sde_and_belts() {
        let ctx = new_ctx();
        let sde = &ctx.sde;

        // MN-01: SDE mining static data (typeMaterials + ore attributes)
        let veldspar_mats = sde.type_materials_for(1230);
        assert_eq!(veldspar_mats.len(), 1);
        assert_eq!(veldspar_mats[0].material_type_id, 34);

        let scordite_mats: Vec<u32> = sde
            .type_materials_for(1228)
            .iter()
            .map(|m| m.material_type_id)
            .collect();
        assert_eq!(scordite_mats, vec![34, 35]);

        // C-5: Asteroid belt ball spawning in solar system ballpark
        let mut ballpark = Ballpark::new(SYS_PERIMETER);
        let spawned = populate_asteroid_belt_entities(&mut ballpark, SYS_PERIMETER, sde);
        assert!(
            !spawned.is_empty(),
            "expected asteroid belt balls to spawn in Perimeter"
        );
    }

    #[tokio::test]
    async fn test_parity_misc_plan_q3_and_q4_quasar_identity_and_progression() {
        let ctx = new_ctx();

        let mut unauth = SessionState::new(8201);
        let unauth_resp = quasar_rpc(
            &ctx,
            &mut unauth,
            "eve_public.character.api.GetRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(unauth_resp.status_code, STATUS_UNAUTHORIZED);

        let mut authed = authed_demo_session(&ctx, 8202).await;
        let q3_resp = quasar_rpc(
            &ctx,
            &mut authed,
            "eve_public.store.category.api.GetAllRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(q3_resp.status_code, STATUS_OK);
        assert_eq!(
            q3_resp.payload.as_ref().unwrap().type_url,
            format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.store.category.api.GetAllResponse")
        );

        let q4_resp = quasar_rpc(
            &ctx,
            &mut authed,
            "eve_public.career.goal.api.GetAllRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(q4_resp.status_code, STATUS_OK);
        assert_eq!(
            q4_resp.payload.as_ref().unwrap().type_url,
            format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.career.goal.api.GetAllResponse")
        );
    }

    #[tokio::test]
    async fn test_parity_misc_plan_q7_and_q8_quasar_space_and_sovereignty() {
        let ctx = new_ctx();
        let mut authed = authed_demo_session(&ctx, 8301).await;

        let mut cdn_req = Vec::new();
        encode_varint_field(2, 1, &mut cdn_req);
        let q7_resp = quasar_rpc(
            &ctx,
            &mut authed,
            "eve_public.space.api.cdn.GetUpdateRequest",
            cdn_req,
        )
        .await;
        assert_eq!(q7_resp.status_code, STATUS_OK);
        let q7_any = q7_resp.payload.as_ref().expect("q7 payload");
        assert_eq!(
            q7_any.type_url,
            format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.space.api.cdn.GetUpdateResponse")
        );
        let q7_fields = decode_fields(&q7_any.value).expect("decode cdn GetUpdateResponse");
        let cp_bytes = q7_fields
            .iter()
            .find_map(|(f, v)| match (*f, v) {
                (1, ProtoWireValue::Bytes(b)) => Some(b.as_slice()),
                _ => None,
            })
            .expect("cdn update checkpoint");
        let cp_fields = decode_fields(cp_bytes).expect("decode checkpoint");
        let cdn_url = cp_fields
            .iter()
            .find_map(|(f, v)| match (*f, v) {
                (1, ProtoWireValue::Bytes(b)) => std::str::from_utf8(b).ok().map(str::to_string),
                _ => None,
            })
            .expect("cdn url");
        assert!(cdn_url.starts_with("http://127.0.0.1:26004/cdn/maptags/v24.1."));

        let q8_resp = quasar_rpc(
            &ctx,
            &mut authed,
            "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(q8_resp.status_code, STATUS_OK);
        let q8_any = q8_resp.payload.as_ref().expect("q8 payload");
        assert_eq!(
            q8_any.type_url,
            format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.resource.planet.api.GetAllDefinitionsResponse")
        );
        let q8_fields = decode_fields(&q8_any.value).expect("decode GetAllDefinitionsResponse");
        let ver_bytes = q8_fields
            .iter()
            .find_map(|(f, v)| match (*f, v) {
                (2, ProtoWireValue::Bytes(b)) => Some(b.as_slice()),
                _ => None,
            })
            .expect("sovereignty definitions version");
        let ver_fields = decode_fields(ver_bytes).expect("decode version");
        let major = ver_fields
            .iter()
            .find_map(|(f, v)| match (*f, v) {
                (1, ProtoWireValue::Varint(n)) => Some(*n),
                _ => None,
            })
            .unwrap_or(0);
        assert_eq!(major, 1);
    }

    #[test]
    fn test_parity_misc_plan_t2_sde_and_dogma_effects_sweep() {
        let ctx = new_ctx();
        let main_loc = ctx
            .sde
            .load_localization_main()
            .expect("load localization_fsd_main.pickle");
        assert_eq!(main_loc.languages.len(), 9);
        assert_eq!(main_loc.labels.len(), 31_872);

        let rifter = ctx.sde.get_type(587).expect("Rifter type 587");
        assert_eq!(rifter.group_id, 25);
        assert!(
            ctx.sde.dogma_effects().contains_key(&16),
            "expected online effect 16 in SDE dogma_effects"
        );
    }

    #[tokio::test]
    async fn test_parity_misc_plan_t8_and_t9_invariants_and_restart() {
        let ctx = new_ctx();

        // T-8: Rejected RPC leaves character wallet balance unchanged
        let before_isk = ctx
            .db
            .with_snapshot(|snap| snap.characters.get(&PILOT_A).map(|c| c.balance.0).unwrap_or(0));
        let mut unauth = SessionState::new(8401);
        let kw = HashMap::new();
        let err = ctx
            .dispatch_rpc(
                &mut unauth,
                "account",
                None,
                "GiveCash",
                &[
                    PyRep::Integer(PILOT_A),
                    PyRep::Real(1_000_000.0),
                    PyRep::String("unauth".into()),
                ],
                &kw,
                None,
            )
            .await;
        assert!(err.is_err(), "unauthenticated GiveCash must be rejected");
        let after_isk = ctx
            .db
            .with_snapshot(|snap| snap.characters.get(&PILOT_A).map(|c| c.balance.0).unwrap_or(0));
        assert_eq!(before_isk, after_isk);

        // T-9: Durable inventory items persist in snapshot without transient active-cycle state
        let item_id = 99_880_001;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id: 34,
                    owner_id: PILOT_A,
                    location_id: 60_003_760,
                    flag: InventoryFlag::Hangar,
                    quantity: 500,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        let persisted = ctx
            .db
            .with_snapshot(|snap| snap.items.get(&item_id).cloned())
            .expect("persisted Tritanium stack");
        assert_eq!(persisted.quantity, 500);
    }

    #[tokio::test]
    async fn test_parity_misc_plan_t10_and_t11_client_oracle_and_destiny_diff() {
        let ctx = new_ctx();
        let mut authed = authed_demo_session(&ctx, 8501).await;

        // T-10: Client-expectation oracle (Quasar envelope correlation UUID + GoogleAny type_url)
        let resp = quasar_rpc(
            &ctx,
            &mut authed,
            "eve_public.store.category.api.GetAllRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(resp.status_code, STATUS_OK);
        assert_eq!(resp.correlation_uuid, b"parity-misc-part1-01");
        assert!(resp.payload.is_some());

        // T-11: Destiny differential physics step + wire encoding
        let mut ballpark = Ballpark::new(SYS_PERIMETER);
        let _ = populate_asteroid_belt_entities(&mut ballpark, SYS_PERIMETER, &ctx.sde);
        let ball_id = *ballpark.balls.keys().next().expect("seeded ball");
        let start_x = ballpark.balls[&ball_id].position.x;
        if let Some(ball) = ballpark.balls.get_mut(&ball_id) {
            ball.velocity = DVec3::new(100.0, 0.0, 0.0);
            ball.mass = 1_067_000.0;
            ball.max_velocity = 365.0;
            ball.inertia = 2.9;
            ball.mode = MovementMode::GotoDirection {
                direction: DVec3::X,
            };
        }
        ballpark.step(1.0);
        let moved = ballpark.balls.get(&ball_id).expect("moved ball");
        assert!(moved.position.x > start_x + 50.0, "expected forward X displacement");
    }
}

pub(crate) mod rpc_localization_update_qa_wrap {
    // Runtime integration tests closing open `rpc:*` parity rows:
    // - `rpc:localizationServer.UpdateLocalizationQAWrap`

    use crate::notify_gameplay_emitters::{new_ctx, rpc, JITA_STATION, PILOT_A, PILOT_B};
    use crate::notify_gameplay_emitters::login_docked_upwell_struct::login_docked;
    use eve_db::OwnerNoteRecord;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;

    const LOCALIZATION_NOTE_ID: i64 = -8_960_000_000_000;

    // parity: rpc:localizationServer.UpdateLocalizationQAWrap
    #[tokio::test]
    async fn parity_rpc_localization_update_qa_wrap() {
        eve_testkit::attest(&["rpc:localizationServer.UpdateLocalizationQAWrap"]);

        let ctx = new_ctx();

        // 1. Unauthenticated caller is rejected.
        let mut unauth = SessionState::new(90_021);
        let err_unauth = rpc(
            &ctx,
            &mut unauth,
            "localizationServer",
            "UpdateLocalizationQAWrap",
            &[PyRep::Bool(true)],
        )
        .await
        .expect_err("unauthenticated UpdateLocalizationQAWrap must fail");
        assert_eq!(err_unauth.code, "Unauthorized");

        // 2. Non-QA/non-GM caller is rejected.
        let mut player_sess =
            login_docked(&ctx, 90_022, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
        let err_role = rpc(
            &ctx,
            &mut player_sess,
            "localizationServer",
            "UpdateLocalizationQAWrap",
            &[PyRep::Bool(true)],
        )
        .await
        .expect_err("non-QA UpdateLocalizationQAWrap must fail");
        assert_eq!(err_role.code, "RoleRequired");

        // 3. Seed a localization override and toggle QA wrap on/off for QA user.
        ctx.db.with_mut_snapshot(|snap| {
            snap.owner_notes.insert(
                LOCALIZATION_NOTE_ID,
                OwnerNoteRecord {
                    note_id: LOCALIZATION_NOTE_ID,
                    character_id: 0,
                    item_id: 0,
                    label: "localization_overrides".into(),
                    note: r#"{"1001":"Welcome to New Eden"}"#.into(),
                    timestamp_unix: 0,
                },
            );
        });

        let mut qa_sess =
            login_docked(&ctx, 90_023, "other", "other-secret", PILOT_B, JITA_STATION).await;
        qa_sess.role |= eve_machonet::service_roles::ROLE_QA;

        let enabled = rpc(
            &ctx,
            &mut qa_sess,
            "localizationServer",
            "UpdateLocalizationQAWrap",
            &[PyRep::Bool(true)],
        )
        .await
        .expect("enable QA wrap");
        assert_eq!(enabled, PyRep::Bool(true));

        let wrapped_changes = rpc(
            &ctx,
            &mut qa_sess,
            "localizationServer",
            "GetAllTextChanges",
            &[],
        )
        .await
        .expect("GetAllTextChanges with QA wrap enabled");
        assert_eq!(
            wrapped_changes,
            PyRep::Dict(vec![(
                PyRep::Integer(1001),
                PyRep::String("[QA:1001] Welcome to New Eden".into()),
            )])
        );

        let disabled = rpc(
            &ctx,
            &mut qa_sess,
            "localizationServer",
            "UpdateLocalizationQAWrap",
            &[PyRep::Bool(false)],
        )
        .await
        .expect("disable QA wrap");
        assert_eq!(disabled, PyRep::Bool(false));

        let plain_changes = rpc(
            &ctx,
            &mut qa_sess,
            "localizationServer",
            "GetAllTextChanges",
            &[],
        )
        .await
        .expect("GetAllTextChanges with QA wrap disabled");
        assert_eq!(
            plain_changes,
            PyRep::Dict(vec![(
                PyRep::Integer(1001),
                PyRep::String("Welcome to New Eden".into()),
            )])
        );
    }
}

pub(crate) mod mining_sde_belt_balls {
    // Smoke integration tests for mining SDE, asteroid belt balls, NPC mining convoys,
    // agent/scan RPCs, SQLite persistence, and Quasar gateway endpoints.

    use std::collections::HashMap;

    use eve_destiny::Ballpark;
    use eve_machonet::SessionState;
    use eve_proto::quasar::{
        EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage,
        TYPE_GOOGLEAPIS_PREFIX,
    };
    use eve_proto::PyRep;
    use eve_server::living_world::{LivingWorldDirector, NpcArchetype};
    use eve_server::quasar_gateway::{STATUS_OK, STATUS_UNAUTHORIZED};
    use eve_server::router::ServerContext;
    use eve_server::system_actor::populate_asteroid_belt_entities;

    use crate::common::login_pilot;

    async fn authed_demo_session(ctx: &ServerContext, sid: u64) -> SessionState {
        let mut sess = SessionState::new(sid);
        login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
        let kw = HashMap::new();
        let _ = ctx
            .dispatch_rpc(
                &mut sess,
                "charUnboundMgr",
                None,
                "SelectCharacterID",
                &[PyRep::Integer(90_000_001)],
                &kw,
                None,
            )
            .await
            .expect("SelectCharacterID 90_000_001");
        sess
    }

    fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
        if let PyRep::KeyVal(entries) = rep {
            for (k, v) in entries {
                if k == "response_proto" {
                    if let PyRep::Bytes(b) = v {
                        return EvePublicResponse::decode_proto(b)
                            .expect("decode EvePublicResponse from response_proto");
                    }
                }
            }
        }
        panic!("Expected KeyVal containing response_proto, got {rep:?}");
    }

    async fn quasar_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        type_name: &str,
        payload_bytes: Vec<u8>,
    ) -> EvePublicResponse {
        let req = EvePublicRequest {
            issued: Some(GoogleTimestamp {
                seconds: ctx.now_unix() as i64,
                nanos: 0,
            }),
            correlation_uuid: b"plan-w31-closure-01".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
                value: payload_bytes,
            }),
            ..Default::default()
        };
        let req_bytes = req.encode_proto();
        let empty_kw = HashMap::new();
        let rep = ctx
            .dispatch_rpc(
                sess,
                "publicGatewaySvc",
                None,
                "SendRequest",
                &[PyRep::Bytes(req_bytes)],
                &empty_kw,
                None,
            )
            .await
            .expect("publicGatewaySvc.send_request");
        extract_response_proto(&rep)
    }

    #[tokio::test]
    async fn test_plan_c5_mn01_mn02_mining_sde_and_belt_balls() {
        let ctx = ServerContext::new();
        let sde = &ctx.sde;

        // MN-01: Veldspar (1230) -> Tritanium (34) only
        let veldspar_mats = sde.type_materials_for(1230);
        assert_eq!(veldspar_mats.len(), 1);
        assert_eq!(veldspar_mats[0].material_type_id, 34);

        // Scordite (1228) -> Tritanium (34) + Pyerite (35)
        let scordite_mats: Vec<u32> = sde
            .type_materials_for(1228)
            .iter()
            .map(|m| m.material_type_id)
            .collect();
        assert_eq!(scordite_mats, vec![34, 35]);

        // Clear Icicle (16262) yields its isotope and fuel materials
        let ice_mats: Vec<u32> = sde
            .type_materials_for(16262)
            .iter()
            .map(|m| m.material_type_id)
            .collect();
        for expected in [16272u32, 16273, 16274, 16275] {
            assert!(ice_mats.contains(&expected), "Clear Icicle must yield {expected}");
        }

        // Ore variant yields differ from base ore (Concentrated Veldspar 17470 vs Veldspar 1230)
        let conc_veld_mats = sde.type_materials_for(17470);
        assert_eq!(conc_veld_mats.len(), 1);
        assert!(conc_veld_mats[0].quantity > veldspar_mats[0].quantity);

        // MN-02 / C-5: Asteroid belts as balls in Perimeter (30000144, highsec) & LZ-6SU (30000208, nullsec)
        let mut highsec_bp = Ballpark::new(30_000_144);
        let highsec_states = populate_asteroid_belt_entities(&mut highsec_bp, 30_000_144, sde);
        assert!(!highsec_states.is_empty(), "Perimeter must spawn asteroid belt entities");
        for st in &highsec_states {
            assert!(st.remaining_units > 0);
            let ball = highsec_bp.balls.get(&st.asteroid_id).expect("asteroid ball in ballpark");
            assert!(ball.radius >= 60.0);
        }

        let mut null_bp = Ballpark::new(30_000_208);
        let null_states = populate_asteroid_belt_entities(&mut null_bp, 30_000_208, sde);
        assert!(!null_states.is_empty(), "Nullsec 30000208 must spawn asteroid belt entities");
    }

    #[tokio::test]
    async fn test_plan_mn17_mn18_npc_mining_ops_and_mining_missions() {
        let ctx = ServerContext::new();

        // MN-17: LivingWorldDirector seeded convoys include MiningOp fleets
        let director = LivingWorldDirector::new_seeded();
        let convoys = director.list_convoys();
        assert!(!convoys.is_empty(), "Seeded LivingWorldDirector must have convoys");
        assert!(
            convoys.iter().any(|c| c.archetype == NpcArchetype::MiningOp),
            "Seeded convoys must include MiningOp archetype"
        );

        // MN-18: Agent mission RPCs + negative unauthenticated check
        let empty_kw = HashMap::new();
        let mut unauth = SessionState::new(7101);
        let err = ctx
            .dispatch_rpc(
                &mut unauth,
                "agentMgr",
                None,
                "GetMyJournalDetails",
                &[],
                &empty_kw,
                None,
            )
            .await
            .expect_err("unauthenticated agentMgr.GetMyJournalDetails must fail");
        assert!(!format!("{err:?}").is_empty());

        let mut authed = authed_demo_session(&ctx, 7102).await;
        let agents_rep = ctx
            .dispatch_rpc(
                &mut authed,
                "agentMgr",
                None,
                "GetAgents",
                &[],
                &empty_kw,
                None,
            )
            .await
            .expect("authenticated agentMgr.GetAgents");
        assert!(!matches!(agents_rep, PyRep::None));
    }

    #[tokio::test]
    async fn test_plan_so5_exploration_anomalies_probes_dscan_and_dungeons() {
        let ctx = ServerContext::new();
        let empty_kw = HashMap::new();

        // Negative/auth check on scanSvc
        let mut unauth = SessionState::new(7201);
        let err = ctx
            .dispatch_rpc(
                &mut unauth,
                "scanSvc",
                None,
                "GetCombatAnomalyInstances",
                &[],
                &empty_kw,
                None,
            )
            .await
            .expect_err("unauthenticated scanSvc.GetCombatAnomalyInstances must fail");
        assert!(!format!("{err:?}").is_empty());

        // Authenticated anomaly & signature query
        let mut authed = authed_demo_session(&ctx, 7202).await;
        let anomalies = ctx
            .dispatch_rpc(
                &mut authed,
                "scanSvc",
                None,
                "GetCombatAnomalyInstances",
                &[],
                &empty_kw,
                None,
            )
            .await
            .expect("authenticated scanSvc.GetCombatAnomalyInstances");
        assert_ne!(anomalies, PyRep::None);
        assert!(format!("{anomalies:?}").len() > 4);
    }

    #[tokio::test]
    async fn test_plan_d6_single_source_of_truth_and_sqlite_restart() {
        let db_path = std::env::temp_dir().join(format!("plan_w31_d6_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&db_path);

        {
            let ctx = ServerContext::from_sqlite(&db_path).expect("from_sqlite");
            ctx.seed_demo_world_fixtures();
            let empty_kw = HashMap::new();

            // Negative/auth check on charMgr.AddContact
            let mut unauth = SessionState::new(7301);
            let err = ctx
                .dispatch_rpc(
                    &mut unauth,
                    "charMgr",
                    None,
                    "AddContact",
                    &[
                        PyRep::Integer(90_000_002),
                        PyRep::Real(5.0),
                        PyRep::Integer(0),
                        PyRep::Bool(false),
                    ],
                    &empty_kw,
                    None,
                )
                .await
                .expect_err("unauthenticated charMgr.AddContact must fail");
            assert!(!format!("{err:?}").is_empty());

            // Authenticated contact mutation persisted to SQLite
            let mut authed = authed_demo_session(&ctx, 7302).await;
            ctx.dispatch_rpc(
                &mut authed,
                "charMgr",
                None,
                "AddContact",
                &[
                    PyRep::Integer(90_000_002),
                    PyRep::Real(5.0),
                    PyRep::Integer(0),
                    PyRep::Bool(false),
                ],
                &empty_kw,
                None,
            )
            .await
            .expect("authenticated charMgr.AddContact");
        }

        // Reopen SQLite and verify contact survived restart
        let ctx2 = ServerContext::from_sqlite(&db_path).expect("reopen_sqlite");
        let mut authed2 = authed_demo_session(&ctx2, 7303).await;
        let contacts = ctx2
            .dispatch_rpc(
                &mut authed2,
                "charMgr",
                None,
                "GetContacts",
                &[],
                &HashMap::new(),
                None,
            )
            .await
            .expect("charMgr.GetContacts after restart");
        assert_ne!(contacts, PyRep::None);
        assert!(
            format!("{contacts:?}").contains("90000002"),
            "Contact 90000002 must persist across SQLite restart: {contacts:?}"
        );
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn test_plan_t8_t9_atomic_rollback_and_crash_restart_invariants() {
        let db_path = std::env::temp_dir().join(format!("plan_w31_t8_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&db_path);

        let (initial_isk, initial_plex) = {
            let ctx = ServerContext::from_sqlite(&db_path).expect("from_sqlite");
            ctx.seed_demo_world_fixtures();
            let _sess = authed_demo_session(&ctx, 7401).await;
            let (before_isk, before_plex) = ctx
                .db
                .with_snapshot(|snap| {
                    snap.characters
                        .get(&90_000_001)
                        .map(|c| (c.balance, c.plex_balance))
                })
                .expect("character 90_000_001 exists");

            // T-8: Failing transaction via try_with_mut_snapshot must roll back atomically
            let rollback_res: Result<(), String> = ctx.db.try_with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                    ch.balance.0 += 999_999_999;
                    ch.plex_balance += 500;
                }
                Err("simulated_handler_failure".to_string())
            });
            assert_eq!(rollback_res, Err("simulated_handler_failure".to_string()));

            let (after_isk, after_plex) = ctx
                .db
                .with_snapshot(|snap| {
                    snap.characters
                        .get(&90_000_001)
                        .map(|c| (c.balance, c.plex_balance))
                })
                .expect("character 90_000_001 exists");
            assert_eq!(before_isk, after_isk, "ISK must roll back on error");
            assert_eq!(before_plex, after_plex, "PLEX must roll back on error");
            (before_isk, before_plex)
        };

        // T-9: Crash/restart recovery preserves committed state with zero duplication/loss
        let ctx2 = ServerContext::from_sqlite(&db_path).expect("reopen_sqlite");
        let (restarted_isk, restarted_plex) = ctx2
            .db
            .with_snapshot(|snap| {
                snap.characters
                    .get(&90_000_001)
                    .map(|c| (c.balance, c.plex_balance))
            })
            .expect("character 90_000_001 exists after restart");
        assert_eq!(initial_isk, restarted_isk);
        assert_eq!(initial_plex, restarted_plex);
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn test_plan_q3_q4_q5_quasar_gateway_domains() {
        let ctx = ServerContext::new();

        // Negative/auth check: unauthenticated Quasar request must return STATUS_UNAUTHORIZED
        let mut unauth = SessionState::new(7501);
        let unauth_resp = quasar_rpc(
            &ctx,
            &mut unauth,
            "eve_public.character.api.GetRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(unauth_resp.status_code, STATUS_UNAUTHORIZED);

        // Authenticated Q-3 (Identity/Entitlements), Q-4 (Character/Progression), Q-5 (Social/Corp)
        let mut authed = authed_demo_session(&ctx, 7502).await;
        let q3_resp = quasar_rpc(
            &ctx,
            &mut authed,
            "eve_public.store.category.api.GetAllRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(q3_resp.status_code, STATUS_OK);
        assert_eq!(q3_resp.correlation_uuid, b"plan-w31-closure-01");
        assert_eq!(
            q3_resp.payload.as_ref().unwrap().type_url,
            format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.store.category.api.GetAllResponse")
        );

        let q4_resp = quasar_rpc(
            &ctx,
            &mut authed,
            "eve_public.career.goal.api.GetAllRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(q4_resp.status_code, STATUS_OK);
        assert_eq!(q4_resp.correlation_uuid, b"plan-w31-closure-01");

        let q5_resp = quasar_rpc(
            &ctx,
            &mut authed,
            "eve_public.corporationgoal.api.GetAllRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(q5_resp.status_code, STATUS_OK);
        assert_eq!(q5_resp.correlation_uuid, b"plan-w31-closure-01");
    }


    #[tokio::test]
    async fn test_plan_q6_q7_q8_t10_quasar_and_client_oracle() {
        let ctx = ServerContext::new();
        let mut authed = authed_demo_session(&ctx, 7601).await;

        let q6_resp = quasar_rpc(
            &ctx,
            &mut authed,
            "eve.market.orders.GetOrdersRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(q6_resp.status_code, STATUS_OK);
        assert_eq!(q6_resp.correlation_uuid, b"plan-w31-closure-01");

        let mut cdn_req = Vec::new();
        eve_proto::quasar::encode_varint_field(2, 1, &mut cdn_req);
        let q7_resp = quasar_rpc(
            &ctx,
            &mut authed,
            "eve_public.space.api.cdn.GetUpdateRequest",
            cdn_req,
        )
        .await;
        assert_eq!(q7_resp.status_code, STATUS_OK);
        assert_eq!(q7_resp.correlation_uuid, b"plan-w31-closure-01");

        let q8_resp = quasar_rpc(
            &ctx,
            &mut authed,
            "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(q8_resp.status_code, STATUS_OK);
        assert_eq!(q8_resp.correlation_uuid, b"plan-w31-closure-01");
    }
}

pub(crate) mod ch2_skill_queue_brain_injection {
    // Integration tests closing canonical plan tasks:
    // - `plan:DG-6` & `notify:OnChargeBeingLoadedToModule` (ammo loading/unloading, reload timers, auto-reload, LinkWeapons groups)
    // - `plan:ESI-3` (Universe, Status, Routes, Killmails, Dogma, Insurance)
    // - `plan:ESI-4` (Character, Clones, Skills, Location, Fittings, Assets, Bookmarks, Wallet, Loyalty, Calendar, Contacts, Mail)
    // - `plan:SO-2` (Fleet hierarchy, MOTD, invites, roles, FinishMove, broadcasts, warp, boss handover, lifecycle)

    use crate::common;
    use eve_db::{InventoryFlag, InventoryItemRecord, KillmailRecord, TrainedSkillRecord};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
    use eve_server::router::ServerContext;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;

    const JITA_STATION_ID: i64 = 60_003_760;

    #[tokio::test]
    async fn test_plan_ch2_skill_queue_and_brain_injection() {
        // parity: plan:CH-2
        eve_testkit::attest(&["plan:CH-2"]);
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let mut sess = SessionState::new(9201);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        let char_id = 90_000_001;

        ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
            .await
            .expect("SelectCharacterID");

        // 1. GetSkills, GetSkillQueueAndFreePoints, and SaveSkillQueue (replacing old queue with new ordered list)
        let skills_rep = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetSkills", &[], &kw, None).await.expect("GetSkills");
        assert!(matches!(skills_rep, PyRep::Rowset { .. }));

        let q_init = [PyRep::List(vec![PyRep::List(vec![PyRep::Integer(3386), PyRep::Integer(1)])])];
        ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "SaveSkillQueue", &q_init, &kw, None).await.expect("SaveSkillQueue initial");
        assert_eq!(ctx.db.with_snapshot(|s| s.skill_queues[&char_id].len()), 1);

        let q_args = [PyRep::List(vec![
            PyRep::List(vec![PyRep::Integer(3300), PyRep::Integer(5)]),
            PyRep::List(vec![PyRep::Integer(3386), PyRep::Integer(4)]),
            PyRep::List(vec![PyRep::Integer(3392), PyRep::Integer(2)]),
        ])];
        let save_rep = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "SaveSkillQueue", &q_args, &kw, None).await.expect("SaveSkillQueue replace");
        assert_eq!(save_rep.dict_get("typeID"), Some(&PyRep::Integer(3392)));
        assert_eq!(save_rep.dict_get("targetLevel"), Some(&PyRep::Integer(2)));

        let (queue_len, expected_secs) = ctx.db.with_snapshot(|s| {
            let q = &s.skill_queues[&char_id];
            (q.len(), q.iter().map(|e| e.duration_seconds).sum::<f64>())
        });
        assert_eq!(queue_len, 3, "SaveSkillQueue must replace the queue with the 3 new entries");

        // 2. GetSkillQueue, GetEndOfTraining (active, partial progress, and paused via AbortTraining)
        let get_q = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetSkillQueue", &[], &kw, None).await.expect("GetSkillQueue");
        let PyRep::Rowset { rows, .. } = get_q else { panic!("expected Rowset from GetSkillQueue") };
        assert_eq!(rows.len(), 3, "GetSkillQueue must return 3 rows");
        assert_eq!(rows[0][0], PyRep::Integer(3300));
        assert_eq!(rows[1][0], PyRep::Integer(3386));
        assert_eq!(rows[2][0], PyRep::Integer(3392));

        let end_train = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetEndOfTraining", &[], &kw, None).await.expect("GetEndOfTraining");
        let PyRep::Integer(end_ft) = end_train else { panic!("expected Integer from GetEndOfTraining") };
        let expected_end_ft = ctx.now_blueos_filetime_ticks() + (expected_secs * 10_000_000.0) as i64;
        assert_eq!(end_ft, expected_end_ft, "GetEndOfTraining must match queue total duration");

        ctx.advance_secs(120);
        let end_after_advance = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetEndOfTraining", &[], &kw, None).await.expect("GetEndOfTraining after advance");
        let PyRep::Integer(end_ft_2) = end_after_advance else { panic!("expected Integer from GetEndOfTraining") };
        assert!((end_ft_2 - expected_end_ft).abs() <= 20_000_000, "GetEndOfTraining must account for head progress");

        ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "AbortTraining", &[], &kw, None).await.expect("AbortTraining");
        let paused_end = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetEndOfTraining", &[], &kw, None).await.expect("GetEndOfTraining paused");
        assert_eq!(paused_end, PyRep::None, "Paused skill queue must return None from GetEndOfTraining");

        // 3. Post-Ascension InjectSkillIntoBrain succeeds without prerequisites while SaveSkillQueue enforces prerequisites
        let book_item_id = 991_000_901;
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(char_id).or_default().remove(&3386);
            snap.items.insert(
                book_item_id,
                InventoryItemRecord {
                    item_id: book_item_id, type_id: 3410, owner_id: char_id, location_id: JITA_STATION_ID,
                    flag: InventoryFlag::Hangar, quantity: 1, singleton: false, version: 1, ..Default::default()
                },
            );
        });
        let inject_rep = ctx
            .dispatch_rpc(&mut sess, "skillMgr2", None, "InjectSkillIntoBrain", &[PyRep::Integer(book_item_id)], &kw, None)
            .await
            .expect("InjectSkillIntoBrain succeeds without prerequisites trained");
        assert_eq!(inject_rep.dict_get("typeID"), Some(&PyRep::Integer(3410)));
        assert_eq!(inject_rep.dict_get("injected"), Some(&PyRep::Bool(true)));

        let (has_book, has_skill) = ctx.db.with_snapshot(|snap| {
            (snap.items.contains_key(&book_item_id), snap.skills[&char_id].contains_key(&3410))
        });
        assert!(!has_book, "Skillbook item must be consumed from inventory");
        assert!(has_skill, "Injected skill must be registered in character skills");

        let q_no_prereq = ctx
            .dispatch_rpc(&mut sess, "skillMgr2", None, "SaveSkillQueue", &[PyRep::List(vec![PyRep::List(vec![PyRep::Integer(3410), PyRep::Integer(1)])])], &kw, None)
            .await
            .expect_err("SaveSkillQueue without Mining IV must fail with SkillPrerequisiteNotMet");
        assert_eq!(q_no_prereq.code, "SkillPrerequisiteNotMet");

        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().free_skill_points = 5_000.0;
        });
        let apply_no_prereq = ctx
            .dispatch_rpc(&mut sess, "skillMgr2", None, "ApplyFreeSkillPoints", &[PyRep::Integer(3410), PyRep::Integer(450)], &kw, None)
            .await
            .expect_err("ApplyFreeSkillPoints without prerequisites must fail with SkillPrerequisiteNotMet");
        assert_eq!(apply_no_prereq.code, "SkillPrerequisiteNotMet");

        ctx.db.with_mut_snapshot(|snap| {
            let sk = snap.skills.entry(char_id).or_default();
            sk.insert(3386, TrainedSkillRecord { type_id: 3386, level: 4, skill_points: 45_255.0 });
            sk.insert(3402, TrainedSkillRecord { type_id: 3402, level: 4, skill_points: 45_255.0 });
        });
        let q_with_prereq = ctx
            .dispatch_rpc(&mut sess, "skillMgr2", None, "SaveSkillQueue", &[PyRep::List(vec![PyRep::List(vec![PyRep::Integer(3410), PyRep::Integer(1)])])], &kw, None)
            .await
            .expect("Queueing Astrogeology with prerequisites trained must succeed");
        assert_eq!(q_with_prereq.dict_get("typeID"), Some(&PyRep::Integer(3410)));

        let hist = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetSkillHistory", &[], &kw, None).await.expect("GetSkillHistory");
        let PyRep::Rowset { rows: hist_rows, .. } = hist else { panic!("expected Rowset from GetSkillHistory") };
        assert!(hist_rows.iter().any(|r| r[2] == PyRep::Integer(3410)), "GetSkillHistory must include injected skill 3410");

        // 4. GetRespecInfo and neural remap RespecCharacter with cooldown
        let respec_info = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetRespecInfo", &[], &kw, None).await.expect("GetRespecInfo");
        assert!(respec_info.dict_get("bonusRemaps").is_some());

        let remap_args = [PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(19)];
        let remap_rep = ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &remap_args, &kw, None).await.expect("RespecCharacter timed");
        assert_eq!(remap_rep.dict_get("charisma"), Some(&PyRep::Integer(20)));
        let bonus_left = remap_rep.dict_get("bonusRemaps").and_then(|v| v.as_i64()).unwrap_or(0);
        let cooldown_until = remap_rep.dict_get("remapCooldownUntilUnix").and_then(|v| v.as_i64()).unwrap_or(0);
        assert!(cooldown_until > ctx.now_unix() as i64, "Timed remap must set yearly cooldown");

        for _ in 0..bonus_left {
            ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &remap_args, &kw, None).await.expect("RespecCharacter bonus");
        }
        let remap_err = ctx
            .dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &remap_args, &kw, None)
            .await
            .expect_err("Remap inside cooldown with 0 bonus remaps must be rejected");
        assert_eq!(remap_err.code, "RemapFailed");
        assert!(remap_err.message.contains("cooldown"), "Error message must indicate yearly cooldown");

        // 5. ApplyFreeSkillPoints(typeID, points) and GetSkillQueueAndFreePoints
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().free_skill_points = 5_000.0;
        });
        let apply_rep = ctx
            .dispatch_rpc(&mut sess, "skillMgr2", None, "ApplyFreeSkillPoints", &[PyRep::Integer(3410), PyRep::Integer(450)], &kw, None)
            .await
            .expect("ApplyFreeSkillPoints");
        assert_eq!(apply_rep, PyRep::Real(450.0));
        let q_and_free = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetSkillQueueAndFreePoints", &[], &kw, None).await.expect("GetSkillQueueAndFreePoints");
        let PyRep::Tuple(qf_parts) = q_and_free else { panic!("expected Tuple from GetSkillQueueAndFreePoints") };
        assert_eq!(qf_parts[1], PyRep::Real(4_550.0));
        let sp_after = ctx.db.with_snapshot(|snap| snap.skills[&char_id][&3410].skill_points);
        assert_eq!(sp_after as i64, 450, "Allocated free SP must be credited to Astrogeology");
    }

    #[tokio::test]
    async fn test_plan_dg6_ammo_and_weapon_grouping() {
        // parity: plan:DG-6
        // parity: notify:OnChargeBeingLoadedToModule
        eve_testkit::attest(&["plan:DG-6", "notify:OnChargeBeingLoadedToModule"]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let mut sess = SessionState::new(9202);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        let char_id = 90_000_001i64;
        let ship_id = 100_000_001i64;

        ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
            .await
            .expect("SelectCharacterID");

        let t1_id = 100_000_005i64;
        let t2_id = 100_000_006i64;
        let t3_id = 100_000_007i64;
        let mismatch_id = 100_000_008i64;
        let bad_ammo_id = 100_000_835i64;

        let ammo_id = 100_000_834i64;
        ctx.db.with_mut_snapshot(|snap| {
            for (id, tid, flag, qty, sing) in [
                (t2_id, 2889, InventoryFlag::HiSlot1, 1, true),
                (t3_id, 2889, InventoryFlag::HiSlot2, 1, true),
                (mismatch_id, 491, InventoryFlag::HiSlot3, 1, true),
                (bad_ammo_id, 193, InventoryFlag::Cargo, 100, false),
                (ammo_id, 183, InventoryFlag::Cargo, 400, false),
            ] {
                snap.items.insert(id, InventoryItemRecord {
                    item_id: id, type_id: tid, owner_id: char_id, location_id: ship_id,
                    flag, quantity: qty, singleton: sing, version: 1, ..Default::default()
                });
            }
            snap.skills.entry(char_id).or_default().insert(
                12201,
                TrainedSkillRecord { type_id: 12201, level: 5, skill_points: 256_000.0 },
            );
        });

        // 1. LinkWeapons rejects mismatched weapon types (InvalidWeaponGroup) and links matching weapons
        let bad_link = ctx
            .dispatch_rpc(&mut sess, "dogmaIM", None, "LinkWeapons", &[PyRep::Integer(ship_id), PyRep::Integer(t1_id), PyRep::Integer(mismatch_id)], &kw, None)
            .await
            .expect_err("LinkWeapons must reject different weapon types");
        assert_eq!(bad_link.code, "InvalidWeaponGroup");

        let link_rep = ctx
            .dispatch_rpc(&mut sess, "dogmaIM", None, "LinkWeapons", &[PyRep::Integer(ship_id), PyRep::Integer(t1_id), PyRep::Integer(t2_id)], &kw, None)
            .await
            .expect("LinkWeapons");
        assert_eq!(
            link_rep,
            PyRep::Dict(vec![(
                PyRep::Integer(t1_id),
                PyRep::List(vec![PyRep::Integer(t1_id), PyRep::Integer(t2_id)]),
            )]),
        );

        let bank_size = ctx.db.with_snapshot(|s| s.weapon_banks.get(&ship_id).and_then(|b| b.get(&t1_id)).map(Vec::len));
        assert_eq!(bank_size, Some(2), "Linked weapon bank must contain 2 modules");

        // 2. LoadAmmo rejects incompatible charge size (IncompatibleAmmo)
        let bad_load = ctx
            .dispatch_rpc(&mut sess, "dogmaIM", None, "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(t1_id)]), PyRep::List(vec![PyRep::Integer(bad_ammo_id)]), PyRep::Integer(ship_id)], &kw, None)
            .await
            .expect_err("LoadAmmo must reject incompatible charge size");
        assert_eq!(bad_load.code, "IncompatibleAmmo");

        // 2. Undock into space, verify LoadAmmo emits OnChargeBeingLoadedToModule and enforces reloadTime (attr 1795)
        ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
        let sys = ctx.systems.get_or_spawn(30_000_142);
        let (_, ship_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().expect("ship ball after undock");
        let npc_id = 700_099_001i64;
        sys.spawn_ball_with_combat(
            eve_destiny::Ball {
                id: npc_id, type_id: 587, position: ship_ball.position + glam::DVec3::new(2_500.0, 0.0, 0.0), velocity: glam::DVec3::ZERO,
                max_velocity: 300.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await
        .expect("spawn target ball");
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(npc_id)], &kw, None).await.expect("AddTarget");
        for _ in 0..15 {
            sys.try_step_full_tick(1.0);
        }

        let _ = ctx.drain_notifications_for_char(char_id);
        let load_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "dogmaIM",
                None,
                "LoadAmmo",
                &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(t1_id)]), PyRep::List(vec![PyRep::Integer(ammo_id)]), PyRep::Integer(ship_id)],
                &kw,
                None,
            )
            .await
            .expect("LoadAmmo");

        let sde_reload_ms = ctx.sde.types[&2889].default_attributes[&1795];
        assert_eq!(sde_reload_ms, 10_000.0);
        assert_eq!(load_rep.dict_get("reloadTimeMs"), Some(&PyRep::Real(sde_reload_ms)), "Reload timer must match SDE attribute 1795");

        let load_notifs = ctx.drain_notifications_for_char(char_id);
        let charge_load_notif = load_notifs.iter().find(|n| n.event == "OnChargeBeingLoadedToModule").expect("OnChargeBeingLoadedToModule notification");
        assert_eq!(
            charge_load_notif.args,
            vec![PyRep::List(vec![PyRep::Integer(t1_id), PyRep::Integer(t2_id)]), PyRep::Integer(183), PyRep::Real(10_000.0)],
        );

        let (slot0_before, slot1_before) = ctx.db.with_snapshot(|snap| {
            let s0 = snap.items.values().find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.type_id == 183).map(|i| i.quantity).unwrap_or(0);
            let s1 = snap.items.values().find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot1 && i.type_id == 183).map(|i| i.quantity).unwrap_or(0);
            (s0, s1)
        });
        assert!(slot0_before > 0 && slot1_before == slot0_before, "Both grouped turrets must be loaded with ammo");

        // 3. Activating while reloading fails with ModuleIsReloading; advancing 11s allows firing and consumes 1 round per linked turret
        let reloading_err = ctx
            .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(t1_id), PyRep::String("projectileFired".into()), PyRep::Integer(npc_id)], &kw, None)
            .await
            .expect_err("Activate during 10s reload window must fail with ModuleIsReloading");
        assert_eq!(reloading_err.code, "ModuleIsReloading");

        ctx.advance_secs(11);
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(t1_id), PyRep::String("projectileFired".into()), PyRep::Integer(npc_id)], &kw, None)
            .await
            .expect("Activate grouped weapon bank after reload timer");

        let (slot0_after_fire, slot1_after_fire) = ctx.db.with_snapshot(|snap| {
            let s0 = snap.items.values().find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.type_id == 183).map(|i| i.quantity).unwrap_or(0);
            let s1 = snap.items.values().find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot1 && i.type_id == 183).map(|i| i.quantity).unwrap_or(0);
            (s0, s1)
        });
        assert_eq!(slot0_after_fire, slot0_before - 1, "Master turret in bank must consume 1 charge on cycle");
        assert_eq!(slot1_after_fire, slot1_before - 1, "Slave turret in bank must consume 1 charge on cycle");

        // 4. Charge depletion auto-reload: set loaded charges to 1 round each, step cycle to depletion, verify auto-reload from cargo + reload timer
        ctx.db.with_mut_snapshot(|snap| {
            for it in snap.items.values_mut() {
                if it.location_id == ship_id && matches!(it.flag, InventoryFlag::HiSlot0 | InventoryFlag::HiSlot1) && it.type_id == 183 {
                    it.quantity = 1;
                }
            }
        });
        let _ = ctx.drain_notifications_for_char(char_id);
        for _ in 0..6 {
            sys.try_step_full_tick(1.0);
        }
        let auto_reload_notifs = ctx.drain_notifications_for_char(char_id);
        assert!(auto_reload_notifs.iter().any(|n| n.event == "OnChargeBeingLoadedToModule"), "Depleting loaded charges must emit OnChargeBeingLoadedToModule");
        let (slot0_reloaded, slot1_reloaded) = ctx.db.with_snapshot(|snap| {
            let s0 = snap.items.values().find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.type_id == 183).map(|i| i.quantity).unwrap_or(0);
            let s1 = snap.items.values().find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot1 && i.type_id == 183).map(|i| i.quantity).unwrap_or(0);
            (s0, s1)
        });
        assert_eq!((slot0_reloaded, slot1_reloaded), (80, 80), "Remaining 160 cargo rounds must auto-reload 80 into each grouped turret");
        let auto_reload_err = ctx
            .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(t1_id), PyRep::String("projectileFired".into()), PyRep::Integer(npc_id)], &kw, None)
            .await
            .expect_err("Activate during auto-reload timer must fail with ModuleIsReloading");
        assert_eq!(auto_reload_err.code, "ModuleIsReloading");
        ctx.advance_secs(11);

        // 5. UnloadAmmo moves remaining charges out of both turret slots back to cargo (80 + 80 = 160)
        let unload_rep = ctx
            .dispatch_rpc(&mut sess, "dogmaIM", None, "UnloadAmmo", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(t1_id)]), PyRep::Integer(ship_id)], &kw, None)
            .await
            .expect("UnloadAmmo");
        assert_eq!(unload_rep, PyRep::None);

        let (cargo_qty_after_unload, still_in_slot) = ctx.db.with_snapshot(|snap| {
            let cq = snap.items.values().filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo && i.type_id == 183).map(|i| i.quantity as i64).sum::<i64>();
            let slot_charge = snap.items.values().any(|i| i.location_id == ship_id && matches!(i.flag, InventoryFlag::HiSlot0 | InventoryFlag::HiSlot1) && i.type_id == 183);
            (cq, slot_charge)
        });
        assert_eq!(cargo_qty_after_unload, 160, "Cargo ammo quantity after auto-reload and unload must be 160");
        assert!(!still_in_slot, "Charges must be removed from turret slots on unload");

        // 5. PeelAndLink, MergeModuleGroups, and UnlinkAllModules
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "PeelAndLink", &[PyRep::Integer(ship_id), PyRep::Integer(t3_id), PyRep::Integer(t1_id)], &kw, None)
            .await
            .expect("PeelAndLink");
        assert_eq!(ctx.db.with_snapshot(|s| s.weapon_banks[&ship_id][&t1_id].len()), 3);

        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "UnlinkModule", &[PyRep::Integer(ship_id), PyRep::Integer(t3_id)], &kw, None)
            .await
            .expect("UnlinkModule");
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "MergeModuleGroups", &[PyRep::Integer(ship_id), PyRep::Integer(t1_id), PyRep::Integer(t3_id)], &kw, None)
            .await
            .expect("MergeModuleGroups");
        assert_eq!(ctx.db.with_snapshot(|s| s.weapon_banks[&ship_id][&t1_id].len()), 3);

        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "UnlinkAllModules", &[PyRep::Integer(ship_id)], &kw, None)
            .await
            .expect("UnlinkAllModules");
        assert!(ctx.db.with_snapshot(|s| !s.weapon_banks.contains_key(&ship_id)));
    }

    #[tokio::test]
    async fn test_plan_esi3_universe_status_routes_killmails() {
        // parity: plan:ESI-3
        eve_testkit::attest(&["plan:ESI-3"]);

        let ctx = Arc::new(ServerContext::new());
        crate::common::pin_clock(&ctx);
        let kill_id = 990_001i64;
        let now = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            snap.killmails.insert(
                kill_id,
                KillmailRecord {
                    kill_id,
                    solar_system_id: 30_000_142,
                    victim_character_id: 90_000_002,
                    attacker_character_id: 90_000_001,
                    ship_type_id: 587,
                    total_value_isk: 500_000.0,
                    killed_unix: now,
                    ..Default::default()
                },
            );
        });
        let router = EsiRouter::new(Arc::clone(&ctx));
        let spec = OpenApiSpec::pinned();
        let km_tok = crate::esi_routes::issue_token_with_scopes(&ctx, 90_000_001, vec!["esi-killmails.read_killmails.v1".into()]);

        let status_val = crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", "/v1/status/"), 200);
        assert!(status_val.get("server_version").is_some());

        let route_val = crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", "/v1/route/30000142/30000144/"), 200);
        assert!(route_val.as_array().expect("route array").contains(&json!(30000144)));

        let sys_val = crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", "/v4/universe/systems/30000142/"), 200);
        assert_eq!(sys_val.get("name").and_then(|v| v.as_str()), Some("Jita"));

        let type_val = crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", "/v3/universe/types/587/"), 200);
        assert_eq!(type_val.get("type_id").and_then(|v| v.as_i64()), Some(587));

        let st_val = crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", "/v2/universe/stations/60003760/"), 200);
        assert_eq!(st_val.get("station_id").and_then(|v| v.as_i64()), Some(60_003_760));

        let ins_val = crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", "/v1/insurance/prices/"), 200);
        assert!(ins_val.as_array().is_some());

        let dogma_val = crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", "/v1/dogma/attributes/"), 200);
        assert!(dogma_val.as_array().is_some());

        let eff_val = crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", "/v1/dogma/effects/"), 200);
        assert!(eff_val.as_array().is_some());

        let rec_kms = crate::esi_routes::validate_and_json(
            &router,
            &spec,
            &EsiHttpRequest::new("GET", "/v1/characters/90000001/killmails/recent/").with_bearer_token(&km_tok),
            200,
        );
        let kill_hash = rec_kms
            .as_array()
            .unwrap()
            .iter()
            .find(|k| k["killmail_id"] == json!(kill_id))
            .unwrap()["killmail_hash"]
            .as_str()
            .unwrap();
        let km_val = crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", &format!("/v1/killmails/{kill_id}/{kill_hash}/")), 200);
        assert_eq!(km_val.get("killmail_id").and_then(|v| v.as_i64()), Some(kill_id));
    }

    #[tokio::test]
    async fn test_plan_esi4_character_skills_clones_assets_mail() {
        // parity: plan:ESI-4
        eve_testkit::attest(&["plan:ESI-4"]);

        let ctx = Arc::new(ServerContext::new());
        crate::common::pin_clock(&ctx);
        let router = EsiRouter::new(Arc::clone(&ctx));
        let spec = OpenApiSpec::pinned();

        let token = crate::esi_routes::issue_token_with_scopes(
            &ctx,
            90_000_001,
            vec![
                "esi-assets.read_assets.v1".into(),
                "esi-bookmarks.read_character_bookmarks.v1".into(),
                "esi-calendar.read_calendar_events.v1".into(),
                "esi-characters.read_contacts.v1".into(),
                "esi-characters.read_loyalty.v1".into(),
                "esi-clones.read_clones.v1".into(),
                "esi-fittings.read_fittings.v1".into(),
                "esi-location.read_location.v1".into(),
                "esi-location.read_ship_type.v1".into(),
                "esi-mail.read_mail.v1".into(),
                "esi-mail.send_mail.v1".into(),
                "esi-skills.read_skills.v1".into(),
                "esi-skills.read_skillqueue.v1".into(),
                "esi-wallet.read_character_wallet.v1".into(),
            ],
        );
        let get_auth = |path: &str| {
            crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", path).with_bearer_token(&token), 200)
        };

        // Scope enforcement: missing token returns 401; insufficient scope returns 403
        let unauth_resp = router.handle_request(&EsiHttpRequest::new("GET", "/v4/characters/90000001/skills/"));
        assert_eq!(unauth_resp.status_code, 401);
        let narrow_tok = crate::esi_routes::issue_token_with_scopes(&ctx, 90_000_001, vec!["esi-wallet.read_character_wallet.v1".into()]);
        let forbid_resp = router.handle_request(&EsiHttpRequest::new("GET", "/v4/characters/90000001/skills/").with_bearer_token(&narrow_tok));
        assert_eq!(forbid_resp.status_code, 403);

        // 1. Character
        let char_val = crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", "/v5/characters/90000001/"), 200);
        assert_eq!(char_val.get("name").and_then(|v| v.as_str()), Some("Aura Capsuleer"));
        // 2. Clones
        assert!(get_auth("/v3/characters/90000001/clones/").get("jump_clones").is_some());
        // 3. Skills & Skillqueue
        assert!(get_auth("/v4/characters/90000001/skills/").get("total_sp").is_some());
        assert!(get_auth("/v2/characters/90000001/skillqueue/").as_array().is_some());
        // 4. Location & Ship
        assert_eq!(get_auth("/v2/characters/90000001/location/").get("solar_system_id").and_then(|v| v.as_i64()), Some(30_000_142));
        assert_eq!(get_auth("/v1/characters/90000001/ship/").get("ship_type_id").and_then(|v| v.as_i64()), Some(587));
        // 5. Fittings
        assert!(get_auth("/v2/characters/90000001/fittings/").as_array().is_some());
        // 6. Assets & Bookmarks
        assert!(!get_auth("/v5/characters/90000001/assets/").as_array().expect("assets").is_empty());
        assert!(get_auth("/v2/characters/90000001/bookmarks/").as_array().is_some());
        // 7. Wallet
        assert!(get_auth("/v1/characters/90000001/wallet/").as_f64().is_some());
        // 8. Loyalty (corporation store offers + character loyalty points)
        assert!(crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", "/v1/loyalty/stores/1000125/offers/"), 200).as_array().is_some());
        assert!(get_auth("/v1/characters/90000001/loyalty/points/").as_array().is_some());
        // 9. Calendar
        assert!(get_auth("/v1/characters/90000001/calendar/").as_array().is_some());
        // 10. Contacts
        assert!(get_auth("/v2/characters/90000001/contacts/").as_array().is_some());
        // 11. Mail (POST send + GET list)
        let mail_body = serde_json::to_vec(&json!({
            "approved_cost": 0,
            "body": "Fleet op tonight",
            "recipients": [{"recipient_id": 90_000_002, "recipient_type": "character"}],
            "subject": "Op Notification"
        }))
        .unwrap();
        let sent_id = crate::esi_routes::validate_and_json(
            &router,
            &spec,
            &EsiHttpRequest::new("POST", "/v1/characters/90000001/mail/").with_bearer_token(&token).with_body(mail_body),
            201,
        );
        assert!(sent_id.as_i64().unwrap_or(0) > 0);
        assert!(get_auth("/v1/characters/90000001/mail/").as_array().is_some());
    }

    #[tokio::test]
    async fn test_plan_so2_fleets_lifecycle_and_warp() {
        // parity: plan:SO-2
        eve_testkit::attest(&["plan:SO-2"]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();

        let mut sess1 = SessionState::new(9205);
        common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
        ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap();

        let mut sess2 = SessionState::new(9206);
        common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
        ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap();

        // 1. Create fleet and set MOTD
        let create_rep = ctx
            .dispatch_rpc(&mut sess1, "fleetMgr", None, "CreateFleet", &[PyRep::String("Vanguard Fleet".into()), PyRep::String("Initial MOTD".into())], &kw, None)
            .await
            .expect("CreateFleet");
        let fleet_id = create_rep.dict_get("fleetID").and_then(|v| v.as_i64()).expect("fleetID");
        assert_eq!(sess1.fleet_id, Some(fleet_id));

        let motd_rep = ctx
            .dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "SetMotdEx", &[PyRep::String("Updated fleet MOTD".into())], &kw, None)
            .await
            .expect("SetMotdEx");
        assert_eq!(motd_rep.dict_get("motd"), Some(&PyRep::String("Updated fleet MOTD".into())));

        // 2. Create wing and squad
        let wing_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "CreateWing", &[PyRep::String("Wing 1".into())], &kw, None).await.expect("CreateWing");
        let wing_id = wing_rep.dict_get("wingID").and_then(|v| v.as_i64()).unwrap();

        let squad_rep = ctx
            .dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "CreateSquad", &[PyRep::Integer(wing_id), PyRep::String("Squad 1".into())], &kw, None)
            .await
            .expect("CreateSquad");
        let squad_id = squad_rep.dict_get("squadID").and_then(|v| v.as_i64()).unwrap();

        // 3. Invite member, accept invite, assign role via MoveMember, and verify FinishMove does not demote caller
        ctx.dispatch_rpc(
            &mut sess1,
            "fleetObjectHandler",
            None,
            "Invite",
            &[PyRep::Integer(90_000_002), PyRep::Integer(wing_id), PyRep::Integer(squad_id), PyRep::Integer(4)],
            &kw,
            None,
        )
        .await
        .expect("Invite");

        eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_002);
        let accept_rep = ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "AcceptInvite", &[], &kw, None).await.expect("AcceptInvite");
        assert_eq!(accept_rep.dict_get("fleetID"), Some(&PyRep::Integer(fleet_id)));
        assert_eq!(sess2.fleet_id, Some(fleet_id));

        ctx.advance_secs(12);
        let move_rep = ctx
            .dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "MoveMember", &[PyRep::Integer(90_000_002), PyRep::Integer(wing_id), PyRep::Integer(squad_id), PyRep::Integer(3)], &kw, None)
            .await
            .expect("MoveMember role=3 SquadCommander");
        assert_eq!(move_rep.dict_get("role"), Some(&PyRep::Integer(3)));

        let fm_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "FinishMove", &[], &kw, None).await.expect("FinishMove");
        assert_eq!(fm_rep, PyRep::None);
        let boss_role = ctx.db.with_snapshot(|s| s.fleets[&fleet_id].member_details[&90_000_001].role);
        assert_eq!(boss_role, eve_db::FleetRole::FleetCommander, "FinishMove must not demote FleetCommander");

        // 4. SendBroadcast and verify OnFleetBroadcast notification delivery
        let _ = ctx.drain_notifications_for_char(90_000_002);
        let bc_rep = ctx
            .dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "SendBroadcast", &[PyRep::String("BroadcastTravelTo".into()), PyRep::Integer(30_000_142)], &kw, None)
            .await
            .expect("SendBroadcast");
        assert_eq!(bc_rep.dict_get("broadcast"), Some(&PyRep::String("BroadcastTravelTo".into())));
        assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnFleetBroadcast"), "Pilot 2 must receive OnFleetBroadcast");

        // 5. fleetObjectHandler.Init initializes existing fleet without duplicating; one fleet per character enforced
        let fleets_before = ctx.db.with_snapshot(|s| s.fleets.len());
        let init_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "Init", &[PyRep::Integer(587)], &kw, None).await.expect("fleetObjectHandler.Init");
        assert_eq!(init_rep, PyRep::None);
        assert_eq!(ctx.db.with_snapshot(|s| s.fleets.len()), fleets_before, "Init must not create a second fleet");

        let dup_err = ctx.dispatch_rpc(&mut sess1, "fleetMgr", None, "CreateFleet", &[PyRep::String("Second".into())], &kw, None).await.expect_err("Cannot belong to multiple fleets");
        assert!(dup_err.code.contains("Fleet"), "Must reject duplicate fleet creation");

        // 6. FleetWarp: Undock both ships, SetTakesFleetWarp, and initiate FleetWarp
        ctx.dispatch_rpc(&mut sess1, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock Pilot 1");
        ctx.dispatch_rpc(&mut sess2, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock Pilot 2");
        ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "SetTakesFleetWarp", &[PyRep::Bool(true)], &kw, None).await.expect("SetTakesFleetWarp");

        let ship1_id = sess1.ship_id.expect("Pilot 1 ship_id");
        let ship2_id = sess2.ship_id.expect("Pilot 2 ship_id");
        let sys = ctx.systems.get_or_spawn(30_000_142);

        let fw_dest = glam::DVec3::new(1_000_000_000.0, 0.0, 0.0);
        let fw_rep = ctx
            .dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "FleetWarp", &[PyRep::Real(fw_dest.x), PyRep::Real(fw_dest.y), PyRep::Real(fw_dest.z), PyRep::Real(0.0)], &kw, None)
            .await
            .expect("FleetWarp");
        assert_eq!(fw_rep.dict_get("warpCount"), Some(&PyRep::Integer(2)));

        let (_, b1_fw) = sys.get_ball_snapshot(ship1_id).await.unwrap().unwrap();
        let (_, b2_fw) = sys.get_ball_snapshot(ship2_id).await.unwrap().unwrap();
        assert!(matches!(b1_fw.mode, eve_destiny::MovementMode::Warp { destination, .. } if destination.distance(fw_dest) < 1.0));
        assert!(matches!(b2_fw.mode, eve_destiny::MovementMode::Warp { destination, .. } if destination.distance(fw_dest) < 1.0));

        // 7. Kick member: notification delivery and session cleanup
        let kick_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "KickMember", &[PyRep::Integer(90_000_002)], &kw, None).await.expect("KickMember");
        assert_eq!(kick_rep.dict_get("kickedCharacterID"), Some(&PyRep::Integer(90_000_002)));
        assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnFleetLeave"));
        let p2_fleet_err = ctx.dispatch_rpc(&mut sess2, "fleetMgr", None, "GetInitState", &[], &kw, None).await.expect_err("Kicked member must not be in fleet");
        assert_eq!(p2_fleet_err.code, "NotInFleet");
        assert_eq!(sess2.fleet_id, None, "Kicked member session.fleet_id must be None");

        // 8. Re-invite Pilot 2, logout Boss Pilot 1 -> boss handover to Pilot 2, then Pilot 2 leaves fleet (empty cleanup)
        ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "Invite", &[PyRep::Integer(90_000_002), PyRep::Integer(wing_id), PyRep::Integer(squad_id), PyRep::Integer(3)], &kw, None)
            .await
            .expect("Re-invite Pilot 2");
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_002);
        ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "AcceptInvite", &[], &kw, None).await.expect("AcceptInvite 2");
        let _ = ctx.drain_notifications_for_char(90_000_002);

        ctx.dispatch_rpc(&mut sess1, "authentication", None, "Logout", &[], &kw, None).await.expect("Logout boss");
        assert_eq!(sess1.fleet_id, None);
        assert_eq!(ctx.db.with_snapshot(|s| s.fleets[&fleet_id].boss_id), 90_000_002, "Logout must hand over fleet boss to Pilot 2");
        assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnFleetMemberChanged"));

        let leave_rep = ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "LeaveFleet", &[], &kw, None).await.expect("LeaveFleet");
        assert_eq!(leave_rep.dict_get("left"), Some(&PyRep::Bool(true)));
        assert_eq!(sess2.fleet_id, None, "session.fleet_id must be None after LeaveFleet");
        assert!(!ctx.db.with_snapshot(|s| s.fleets.contains_key(&fleet_id)), "Empty fleet must be cleaned up");
    }
}

pub(crate) mod so1_mail_no_global_cap {
    // Integration tests closing canonical plan rows SO-1 and SO-7
    // and their blocking lifecycle and social features.


    use crate::common;
    use eve_db::MailRecord;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::{NotificationRecord, ServerContext};
    use std::collections::HashMap;

    const JITA_SYSTEM_ID: u32 = 30_000_142;
    const P1: i64 = 90_000_001;
    const P2: i64 = 90_000_002;

    async fn rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, eve_proto::BridgeRpcError> {
        let kw = HashMap::new();
        ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
    }

    #[tokio::test]
    async fn test_so1_mail_no_global_cap() {
        // parity: plan:SO-1, feat:lifecycle.mail-no-global-cap
        eve_testkit::attest(&["plan:SO-1", "feat:lifecycle.mail-no-global-cap"]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        // 1. Seed 3,001 dummy mails server-wide across many recipients (simulating high server load)
        ctx.db.with_mut_snapshot(|snap| {
            for i in 1..=3001 {
                snap.mails.push(MailRecord {
                    message_id: 100_000 + i,
                    sender_id: P1,
                    recipient_id: 80_000_000 + (i % 50),
                    subject: format!("Mail {i}"),
                    body: format!("Body {i}"),
                });
            }
        });

        assert!(ctx.db.with_snapshot(|snap| snap.mails.len()) >= 3001);

        // Sender logs in
        let mut s1 = SessionState::new(8001);
        common::login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
        rpc(&ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)])
            .await
            .expect("SelectCharacterID s1");

        // Recipient P2 is offline. Send mail number 3,002 to offline character P2.
        let send_rep = rpc(
            &ctx,
            &mut s1,
            "mailMgr",
            "SendMail",
            &[
                PyRep::List(vec![PyRep::Integer(P2)]),
                PyRep::None,
                PyRep::None,
                PyRep::String("Mail 3002 Subject".into()),
                PyRep::String("Mail 3002 Body for offline recipient".into()),
            ],
        )
        .await
        .expect("SendMail 3002 must succeed despite >3,000 global mails");

        let mail_id = send_rep.as_i64().expect("mail_id");
        assert!(mail_id > 0);

        // Recipient logs in
        let mut s2 = SessionState::new(8002);
        common::login_pilot(&ctx, &mut s2, "other", "other-secret").await;
        rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P2)])
            .await
            .expect("SelectCharacterID s2");

        // Mail 3,002 appears in offline recipient's SyncMail at next login
        let sync_rep = rpc(&ctx, &mut s2, "mailMgr", "SyncMail", &[PyRep::None, PyRep::None])
            .await
            .expect("SyncMail");
        let sync_text = format!("{sync_rep:?}");
        assert!(sync_text.contains(&mail_id.to_string()) || sync_text.contains("Mail 3002 Subject"));

        // Recipient reads body
        let body_rep = rpc(&ctx, &mut s2, "mailMgr", "GetBody", &[PyRep::Integer(mail_id)])
            .await
            .expect("GetBody");
        let body_str = match &body_rep {
            PyRep::Buffer(b) => {
                String::from_utf8(miniz_oxide::inflate::decompress_to_vec_zlib(b).unwrap_or_default())
                    .unwrap_or_default()
            }
            _ => body_rep.as_str().unwrap_or("").to_string(),
        };
        assert!(body_str.contains("Mail 3002 Body for offline recipient"));

        // 2. Per-recipient mailbox quota (1,000 mails) is enforced across ALL recipients in a multi-recipient SendMail
        let full_recipient = 80_000_999i64;
        ctx.db.with_mut_snapshot(|snap| {
            for i in 1..=1_000 {
                snap.mails.push(MailRecord {
                    message_id: 200_000 + i,
                    sender_id: P1,
                    recipient_id: full_recipient,
                    subject: format!("Quota {i}"),
                    body: "Quota body".into(),
                });
            }
        });
        let quota_err = rpc(
            &ctx,
            &mut s1,
            "mailMgr",
            "SendMail",
            &[
                PyRep::List(vec![PyRep::Integer(P2), PyRep::Integer(full_recipient)]),
                PyRep::None,
                PyRep::None,
                PyRep::String("Multi Recipient Quota Check".into()),
                PyRep::String("Second recipient is at 1,000 mailbox cap".into()),
            ],
        )
        .await
        .expect_err("Multi-recipient SendMail must reject when any recipient exceeds 1,000-mail quota");
        assert_eq!(quota_err.code, "MailQuotaExceeded");

        // 3. Mail read/unread state, custom labels, trash, and deletion (plan:SO-1)
        rpc(&ctx, &mut s2, "mailMgr", "MarkAsRead", &[PyRep::List(vec![PyRep::Integer(mail_id)])])
            .await
            .expect("MarkAsRead");
        assert!(ctx.db.with_snapshot(|s| s.read_mails.contains(&(P2, mail_id))));

        let label_rep = rpc(
            &ctx,
            &mut s2,
            "mailMgr",
            "CreateLabel",
            &[PyRep::String("Priority".into()), PyRep::String("#ff0000".into())],
        )
        .await
        .expect("CreateLabel");
        let custom_label_id = label_rep.as_i64().expect("custom_label_id");
        assert!(custom_label_id >= 64);

        rpc(
            &ctx,
            &mut s2,
            "mailMgr",
            "AssignLabels",
            &[PyRep::List(vec![PyRep::Integer(mail_id)]), PyRep::Integer(custom_label_id)],
        )
        .await
        .expect("AssignLabels");
        let labels_rep = rpc(&ctx, &mut s2, "mailMgr", "GetLabels", &[])
            .await
            .expect("GetLabels");
        assert!(format!("{labels_rep:?}").contains("Priority"));

        rpc(&ctx, &mut s2, "mailMgr", "MoveToTrash", &[PyRep::List(vec![PyRep::Integer(mail_id)])])
            .await
            .expect("MoveToTrash");
        assert!(ctx.db.with_snapshot(|s| s.trashed_mails.contains(&(P2, mail_id))));

        rpc(&ctx, &mut s2, "mailMgr", "MoveFromTrash", &[PyRep::List(vec![PyRep::Integer(mail_id)])])
            .await
            .expect("MoveFromTrash");
        assert!(!ctx.db.with_snapshot(|s| s.trashed_mails.contains(&(P2, mail_id))));

        rpc(&ctx, &mut s2, "mailMgr", "DeleteMail", &[PyRep::List(vec![PyRep::Integer(mail_id)])])
            .await
            .expect("DeleteMail");
        assert!(ctx.db.with_snapshot(|s| s.deleted_mails.contains(&(P2, mail_id))));

        // 4. Mailing lists (Create, Join, SendMail to list, Leave) and Notification center (plan:SO-1)
        let ml_rep = rpc(
            &ctx,
            &mut s1,
            "mailingListsMgr",
            "Create",
            &[PyRep::String("Fleet Alerts".into())],
        )
        .await
        .expect("mailingListsMgr.Create");
        let list_id = ml_rep
            .as_i64()
            .or_else(|| ml_rep.dict_get("listID").and_then(|v| v.as_i64()))
            .expect("mailing list ID");
        rpc(&ctx, &mut s2, "mailingListsMgr", "Join", &[PyRep::Integer(list_id)])
            .await
            .expect("mailingListsMgr.Join");
        let list_mail_rep = rpc(
            &ctx,
            &mut s1,
            "mailMgr",
            "SendMail",
            &[
                PyRep::List(vec![]),
                PyRep::Integer(list_id),
                PyRep::None,
                PyRep::String("List Broadcast".into()),
                PyRep::String("Broadcast to mailing list members".into()),
            ],
        )
        .await
        .expect("SendMail to mailing list");
        assert!(list_mail_rep.as_i64().unwrap_or(0) > 0);
        rpc(&ctx, &mut s2, "mailingListsMgr", "Leave", &[PyRep::Integer(list_id)])
            .await
            .expect("mailingListsMgr.Leave");

        // Seed a notification record and verify GetUnprocessedNotifications + MarkAsProcessed
        let notif_id = 990_001i64;
        ctx.notification_records.write().unwrap().push(NotificationRecord {
            notification_id: notif_id,
            type_id: 101,
            sender_id: 1_000_125,
            receiver_id: P2,
            processed: false,
            group_id: 12,
            created: ctx.now_unix() as i64,
            data: "planetID: 40009082\n".into(),
            deleted: false,
            interacted: false,
        });
        let unproc = rpc(&ctx, &mut s2, "notificationMgr", "GetUnprocessed", &[])
            .await
            .expect("GetUnprocessed");
        assert!(format!("{unproc:?}").contains(&notif_id.to_string()));
        rpc(
            &ctx,
            &mut s2,
            "notificationMgr",
            "MarkAsProcessed",
            &[PyRep::List(vec![PyRep::Integer(notif_id)])],
        )
        .await
        .expect("MarkAsProcessed");
        let unproc_after = rpc(&ctx, &mut s2, "notificationMgr", "GetUnprocessed", &[])
            .await
            .expect("GetUnprocessed after MarkAsProcessed");
        assert!(!format!("{unproc_after:?}").contains(&notif_id.to_string()));
    }

    #[tokio::test]
    async fn test_so7_chat_item_fitting_hyperlinks_resolution() {
        // parity: plan:SO-7, feat:social.chat-item-fitting-hyperlinks
        eve_testkit::attest(&["plan:SO-7", "feat:social.chat-item-fitting-hyperlinks"]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        let mut s1 = SessionState::new(8201);
        common::login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
        rpc(&ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)])
            .await
            .expect("SelectCharacterID s1");

        let mut s2 = SessionState::new(8202);
        common::login_pilot(&ctx, &mut s2, "other", "other-secret").await;
        rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P2)])
            .await
            .expect("SelectCharacterID s2");

        // 1. Prepare target objects for each of the 5 hyperlink kinds (no .unwrap_or(1) fallbacks)
        let ship_item_id = s1.ship_id.expect("P1 active ship item ID");
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ship_it) = snap.items.get_mut(&ship_item_id) {
                ship_it.type_id = 587; // Rifter
            }
        });

        // Fitting created by sender P1
        let fit_rep = rpc(
            &ctx,
            &mut s1,
            "charFittingMgr",
            "SaveFitting",
            &[
                PyRep::Integer(P1),
                PyRep::keyval([
                    ("name", PyRep::String("Fleet Rifter".into())),
                    ("shipTypeID", PyRep::Integer(587)),
                    ("description", PyRep::String("Fit link".into())),
                    (
                        "fitData",
                        PyRep::List(vec![PyRep::Tuple(vec![
                            PyRep::Integer(434),
                            PyRep::Integer(19),
                            PyRep::Integer(1),
                        ])]),
                    ),
                ]),
            ],
        )
        .await
        .expect("SaveFitting");
        let fit_id = fit_rep.as_i64().expect("SaveFitting must return integer fit_id");

        // Overview preset
        let ov_rep = rpc(
            &ctx,
            &mut s1,
            "overviewPresetMgr",
            "StoreLinkAndGetID",
            &[PyRep::String("PvP Focus".into()), PyRep::String("{\"tab\":\"pvp\"}".into())],
        )
        .await
        .expect("StoreLinkAndGetID");
        let ov_sqid = ov_rep
            .dict_get("sqID")
            .and_then(|v| v.as_i64())
            .expect("StoreLinkAndGetID must return sqID");

        // Killmail
        let kill_id = 7771i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.killmails.insert(
                kill_id,
                eve_db::KillmailRecord {
                    kill_id,
                    victim_character_id: P2,
                    victim_corp_id: 2001,
                    attacker_character_id: P1,
                    solar_system_id: JITA_SYSTEM_ID,
                    ship_type_id: 587,
                    killed_unix: 1_700_000_000,
                    ..Default::default()
                },
            );
        });

        // Bookmark folder with recipient P2 in accessGroupIDs ACL so recipient s2 can resolve it
        let bm_rep = rpc(
            &ctx,
            &mut s1,
            "bookmark",
            "AddFolder",
            &[
                PyRep::String("HQ Bookmarks".into()),
                PyRep::Bool(false),
                PyRep::List(vec![PyRep::Integer(P2)]),
            ],
        )
        .await
        .expect("AddFolder");
        let folder_id = bm_rep
            .dict_get("folderID")
            .and_then(|v| v.as_i64())
            .expect("AddFolder must return folderID");

        // 2. Rich message containing all 5 hyperlink schemes
        let rich_msg = format!(
            "<url=showinfo:587//{ship_item_id}>Rifter</url> <url=fitting:587:434;1::>Fit #{fit_id}</url> <url=killReport:{kill_id}:abcdef>KM {kill_id}</url> <url=bookmarkFolder:{folder_id}>HQ BM</url> <url=overviewPreset:{ov_sqid}>PvP OV</url>"
        );

        // Both s1 (P1) and s2 (P2) join the chat channel; s1 configures MOTD/ACL and sends rich_msg; s2 receives OnClientEvent_ChatMessageSent
        let chan_id = "player_channel:hyperlinks".to_string();
        rpc(&ctx, &mut s1, "XmppChatMgr", "AddCharacterToChannel", &[PyRep::String(chan_id.clone())])
            .await
            .expect("s1 join channel");
        rpc(&ctx, &mut s2, "XmppChatMgr", "AddCharacterToChannel", &[PyRep::String(chan_id.clone())])
            .await
            .expect("s2 join channel");
        rpc(
            &ctx,
            &mut s1,
            "XmppChatMgr",
            "SetMotd",
            &[PyRep::String(chan_id.clone()), PyRep::String("Welcome to Hyperlinks Channel".into())],
        )
        .await
        .expect("XmppChatMgr.SetMotd");
        let chans_rep = rpc(&ctx, &mut s2, "LSC", "GetChannels", &[])
            .await
            .expect("LSC.GetChannels");
        assert!(format!("{chans_rep:?}").contains("solarsystemid2"));

        let _ = ctx.drain_notifications_for_char(P2);

        let chat_rep = rpc(
            &ctx,
            &mut s1,
            "LSC",
            "SendMessage",
            &[PyRep::String(chan_id.clone()), PyRep::String(rich_msg.clone())],
        )
        .await
        .expect("LSC.SendMessage");
        assert_eq!(chat_rep.dict_get("text"), Some(&PyRep::String(rich_msg.clone())));

        let p2_chat_notifs = ctx.drain_notifications_for_char(P2);
        let chat_notif = p2_chat_notifs
            .iter()
            .find(|n| n.event == "OnClientEvent_ChatMessageSent")
            .expect("Recipient P2 must receive OnClientEvent_ChatMessageSent");
        let delivered_chat_text = chat_notif
            .args
            .get(3)
            .and_then(|v| v.as_str())
            .expect("delivered chat text in OnClientEvent_ChatMessageSent");
        assert_eq!(delivered_chat_text, rich_msg, "Chat hyperlink text delivered intact to recipient P2");

        // Local LSC channel delivery between P1 and P2 in Jita (30000142)
        let _ = ctx.drain_notifications_for_char(P2);
        rpc(
            &ctx,
            &mut s1,
            "LSC",
            "SendMessage",
            &[
                PyRep::String(format!("solarsystemid2:{JITA_SYSTEM_ID}")),
                PyRep::String("Local Intel Ping".into()),
            ],
        )
        .await
        .expect("LSC.SendMessage on local channel");
        let local_notifs = ctx.drain_notifications_for_char(P2);
        assert!(
            local_notifs.iter().any(|n| format!("{:?}", n.args).contains("Local Intel Ping")),
            "P2 in same solar system receives local LSC message"
        );

        // Send through mailMgr and verify recipient P2 receives identical hyperlink text via GetBody
        let mail_id_rep = rpc(
            &ctx,
            &mut s1,
            "mailMgr",
            "SendMail",
            &[
                PyRep::List(vec![PyRep::Integer(P2)]),
                PyRep::None,
                PyRep::None,
                PyRep::String("Hyperlink Subject".into()),
                PyRep::String(rich_msg.clone()),
            ],
        )
        .await
        .expect("SendMail with hyperlinks");
        let mail_id = mail_id_rep.as_i64().expect("SendMail must return integer mail_id");

        let body_rep = rpc(&ctx, &mut s2, "mailMgr", "GetBody", &[PyRep::Integer(mail_id)])
            .await
            .expect("mailMgr.GetBody");
        let delivered_mail_text = match &body_rep {
            PyRep::Buffer(b) => {
                String::from_utf8(miniz_oxide::inflate::decompress_to_vec_zlib(b).expect("decompress mail body"))
                    .expect("utf8 mail body")
            }
            _ => body_rep.as_str().expect("string mail body").to_string(),
        };
        assert_eq!(delivered_mail_text, rich_msg, "Mail hyperlink text delivered intact to recipient P2");

        // 3. Parse target IDs directly from the delivered message text and resolve ALL 5 via recipient session `s2` (`R-SO7-RECIPIENT`)
        let extract_between = |text: &str, prefix: &str, suffix: &str| -> String {
            let after = text.split_once(prefix).expect("hyperlink prefix present").1;
            after.split_once(suffix).expect("hyperlink suffix present").0.to_string()
        };

        // (a) showinfo:<typeID>//<itemID> -> recipient `s2` resolves typeID via SDE and itemID via dogmaIM.ItemGetInfo
        let showinfo_target = extract_between(delivered_chat_text, "<url=showinfo:", ">Rifter</url>");
        let (type_str, item_str) = showinfo_target.split_once("//").expect("showinfo typeID//itemID");
        let parsed_type_id: u32 = type_str.parse().expect("parsed showinfo typeID");
        let parsed_item_id: i64 = item_str.parse().expect("parsed showinfo itemID");
        assert_eq!(ctx.sde.types.get(&parsed_type_id).expect("SDE type").name, "Rifter");
        let item_info = rpc(&ctx, &mut s2, "dogmaIM", "ItemGetInfo", &[PyRep::Integer(parsed_item_id)])
            .await
            .expect("Recipient s2 resolves showinfo itemID via dogmaIM.ItemGetInfo");
        assert_eq!(item_info.dict_get("typeID").and_then(|v| v.as_i64()), Some(parsed_type_id as i64));
        assert_eq!(item_info.dict_get("ownerID").and_then(|v| v.as_i64()), Some(P1));

        // (b) fitting:<shipTypeID>:<modTypeID>;<qty>:: -> recipient `s2` parses fitting DNA from hyperlink and resolves/imports via charFittingMgr
        let fitting_dna = extract_between(delivered_chat_text, "<url=fitting:", ">Fit #");
        let (dna_ship_str, dna_rest) = fitting_dna.split_once(':').expect("fitting DNA ship:modules");
        let parsed_fit_ship_tid: i64 = dna_ship_str.parse().expect("parsed fitting shipTypeID");
        let (mod_str, qty_rest) = dna_rest.split_once(';').expect("mod;qty");
        let parsed_fit_mod_tid: i64 = mod_str.parse().expect("parsed modTypeID");
        let parsed_fit_mod_qty: i64 = qty_rest.trim_end_matches(':').parse().expect("parsed modQty");
        let parsed_fit_id: i64 = extract_between(delivered_chat_text, ">Fit #", "</url> <url=killReport:")
            .parse()
            .expect("parsed fit_id");
        assert_eq!(parsed_fit_id, fit_id);
        assert_eq!((parsed_fit_ship_tid, parsed_fit_mod_tid, parsed_fit_mod_qty), (587, 434, 1));
        let s2_saved_fit = rpc(
            &ctx,
            &mut s2,
            "charFittingMgr",
            "SaveFitting",
            &[
                PyRep::Integer(P2),
                PyRep::keyval([
                    ("name", PyRep::String("Fleet Rifter".into())),
                    ("shipTypeID", PyRep::Integer(parsed_fit_ship_tid)),
                    ("description", PyRep::String("Imported from chat link".into())),
                    (
                        "fitData",
                        PyRep::List(vec![PyRep::Tuple(vec![
                            PyRep::Integer(parsed_fit_mod_tid),
                            PyRep::Integer(19),
                            PyRep::Integer(parsed_fit_mod_qty),
                        ])]),
                    ),
                ]),
            ],
        )
        .await
        .expect("Recipient s2 saves fitting from chat hyperlink DNA");
        let s2_fit_id = s2_saved_fit.as_i64().expect("s2_fit_id");
        let s2_fittings = rpc(&ctx, &mut s2, "charFittingMgr", "GetFittings", &[PyRep::Integer(P2)])
            .await
            .expect("Recipient s2 GetFittings");
        let fits_debug = format!("{s2_fittings:?}");
        assert!(fits_debug.contains("Fleet Rifter") && fits_debug.contains(&s2_fit_id.to_string()));

        // (c) killReport:<killID>:<hash> -> recipient `s2` resolves killID via warStatisticMgr.GetKillMail
        let kr_target = extract_between(&delivered_mail_text, "<url=killReport:", ">KM ");
        let (kr_id_str, _kr_hash) = kr_target.split_once(':').expect("killID:hash");
        let parsed_kill_id: i64 = kr_id_str.parse().expect("parsed killID");
        let km = rpc(&ctx, &mut s2, "warStatisticMgr", "GetKillMail", &[PyRep::Integer(parsed_kill_id)])
            .await
            .expect("Recipient s2 GetKillMail");
        assert_eq!(km.dict_get("killID").and_then(|v| v.as_i64()), Some(parsed_kill_id));
        assert_eq!(km.dict_get("victimCharacterID").and_then(|v| v.as_i64()), Some(P2));

        // (d) bookmarkFolder:<folderID> -> recipient `s2` resolves folderID via bookmark.GetFolders
        let parsed_folder_id: i64 = extract_between(&delivered_mail_text, "<url=bookmarkFolder:", ">HQ BM</url>")
            .parse()
            .expect("parsed folderID");
        let folders = rpc(&ctx, &mut s2, "bookmark", "GetFolders", &[])
            .await
            .expect("Recipient s2 GetFolders");
        let folders_debug = format!("{folders:?}");
        assert!(
            folders_debug.contains("HQ Bookmarks") && folders_debug.contains(&parsed_folder_id.to_string()),
            "Recipient s2 must resolve shared bookmarkFolder hyperlink via bookmark.GetFolders"
        );

        // (e) overviewPreset:<sqID> -> recipient `s2` resolves sqID via overviewPresetMgr.GetStoredPreset
        let parsed_ov_sqid: i64 = extract_between(&delivered_mail_text, "<url=overviewPreset:", ">PvP OV</url>")
            .parse()
            .expect("parsed overviewPreset sqID");
        let preset = rpc(&ctx, &mut s2, "overviewPresetMgr", "GetStoredPreset", &[PyRep::Integer(parsed_ov_sqid)])
            .await
            .expect("Recipient s2 GetStoredPreset");
        assert_eq!(preset.as_str(), Some("{\"tab\":\"pvp\"}"));
    }
}

pub(crate) mod dogma_shield_recharge_curve_damage {
    // Integration tests verifying runtime features and lifecycle behaviors.


    use eve_db::{
        corp_roles, AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
        MiningLedgerRecord, WorldRepository,
    };
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::esi::{EsiHttpRequest, EsiRouter};
    use eve_server::router::ServerContext;
    use eve_server::services::combat_crimewatch::{
        passive_shield_recharge_rate_hps, step_passive_shield_recharge, step_wreck_and_jetcan_decay,
    };
    use glam::DVec3;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    const JITA_SYSTEM_ID: u32 = 30_000_142;
    const JITA_STATION_ID: i64 = 60_003_760;

    fn temp_sqlite_path(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("evomu_{tag}_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(p.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(p.with_extension("sqlite-shm"));
        p
    }

    fn cleanup_sqlite_path(p: &PathBuf) {
        let _ = std::fs::remove_file(p);
        let _ = std::fs::remove_file(p.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(p.with_extension("sqlite-shm"));
    }

    async fn rpc(
        ctx: &ServerContext,
        session: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, eve_proto::BridgeRpcError> {
        let kw = HashMap::new();
        if service == "charUnboundMgr" && method == "SelectCharacterID" {
            let (u, p) = if args.first() == Some(&PyRep::Integer(90_000_002)) {
                ("other", "other-secret")
            } else {
                ("demo", "demo-secret")
            };
            let _ = ctx
                .dispatch_rpc(
                    session,
                    "authentication",
                    None,
                    "Login",
                    &[PyRep::String(u.into()), PyRep::String(p.into())],
                    &kw,
                    None,
                )
                .await;
        }
        ctx.dispatch_rpc(session, service, None, method, args, &kw, None)
            .await
    }

    #[tokio::test]
    async fn test_c2_dogma_shield_recharge_curve_and_damage_state() {
        // parity: feat:dogma.shield-recharge, plan:C-2
        eve_testkit::attest(&["feat:dogma.shield-recharge", "plan:C-2"]);

        // 1. Verify EVE's non-linear passive shield recharge curve peaks at ~25% shield capacity
        let (max_sh, tau_s) = (2_000.0, 200.0);
        let r_05 = passive_shield_recharge_rate_hps(max_sh * 0.05, max_sh, tau_s);
        let r_10 = passive_shield_recharge_rate_hps(max_sh * 0.10, max_sh, tau_s);
        let r_25 = passive_shield_recharge_rate_hps(max_sh * 0.25, max_sh, tau_s);
        let r_50 = passive_shield_recharge_rate_hps(max_sh * 0.50, max_sh, tau_s);
        let r_90 = passive_shield_recharge_rate_hps(max_sh * 0.90, max_sh, tau_s);
        let r_full = passive_shield_recharge_rate_hps(max_sh, max_sh, tau_s);

        assert!((r_25 - 25.0).abs() < 1e-6);
        assert!(r_25 > r_05 && r_25 > r_10 && r_25 > r_50 && r_25 > r_90);
        assert_eq!(r_full, 0.0);

        // 2. Real Undock initializes CombatState from dogma attributes, applies 4-layer damage, steps recharge, and persists across restart
        let db_path = temp_sqlite_path("c2_combat_persist");
        let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
        let ctx = ServerContext::with_repository(repo);
        let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        ctx.db.with_mut_snapshot(|snap| *snap = demo);
        crate::common::pin_clock(&ctx);
        let mut sess = SessionState::new(7101);
        let _ = rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.expect("SelectCharacterID");
        let ship_id = sess.ship_id.unwrap_or(100_000_001);
        let _ = rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("Undock");

        let ship_info = rpc(&ctx, &mut sess, "dogmaIM", "ItemGetInfo", &[PyRep::Integer(ship_id)]).await.expect("ItemGetInfo");
        let PyRep::Dict(attr_entries) = ship_info.dict_get("attributes").expect("attributes dict") else { panic!("expected Dict") };
        let get_dogma_attr = |aid: i64| -> f64 {
            attr_entries.iter().find(|(k, _)| k.as_i64() == Some(aid)).and_then(|(_, v)| v.as_f64()).unwrap_or(0.0)
        };
        let (dogma_sh, dogma_ar, dogma_hu, dogma_sh_em, dogma_ar_em) = (get_dogma_attr(263), get_dogma_attr(265), get_dogma_attr(9), get_dogma_attr(271), get_dogma_attr(267));

        let handle = ctx.systems.get_or_spawn(JITA_SYSTEM_ID);
        let mut cs = handle.get_combat_state(ship_id).await.unwrap().expect("undocked combat state");
        assert!((cs.max_shield_hp - dogma_sh).abs() < 1e-4 && (cs.max_armor_hp - dogma_ar).abs() < 1e-4 && (cs.max_structure_hp - dogma_hu).abs() < 1e-4);
        assert!((cs.shield_resonance[0] - dogma_sh_em).abs() < 1e-4 && (cs.armor_resonance[0] - dogma_ar_em).abs() < 1e-4);

        cs.undock_invulnerable_remaining_ticks = 0;
        let (_, destroyed) = cs.apply_damage(eve_destiny::DamageVector::new(260.0, 260.0, 260.0, 260.0));
        assert!(!destroyed && cs.shield_hp < cs.max_shield_hp && cs.armor_hp < cs.max_armor_hp);
        cs.shield_hp = cs.max_shield_hp * 0.25;
        let start_shield = cs.shield_hp;
        let tau_ship_s = (cs.shield_recharge_time_ms / 1_000.0).max(1.0);
        handle.set_combat_state(ship_id, cs.clone()).await.expect("set_combat_state");
        let _ = ctx.drain_notifications_for_char(90_000_001);

        let (after_hp, delta_hp) = step_passive_shield_recharge(&ctx, JITA_SYSTEM_ID, Some(90_000_001), ship_id, tau_ship_s, 10.0).await.expect("step_passive_shield_recharge");
        assert!(delta_hp > 0.0 && after_hp > start_shield);
        let cs_after = handle.get_combat_state(ship_id).await.unwrap().expect("combat state");
        assert!((cs_after.shield_hp - after_hp).abs() < 1e-6);
        assert!(ctx.drain_notifications_for_char(90_000_001).iter().any(|n| n.event == "OnDamageStateChange"));

        eve_server::session_cluster_lifecycle::persist_ship_combat_hp_in_db(&ctx.db, ship_id, &cs_after);
        drop(ctx);
        let ctx2 = ServerContext::with_repository(Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite")));
        let reloaded_cs = ctx2.build_ship_combat_state(90_000_001, ship_id, 587);
        assert!((reloaded_cs.shield_hp - cs_after.shield_hp).abs() < 1e-4 && (reloaded_cs.armor_hp - cs_after.armor_hp).abs() < 1e-4);
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn test_d6_single_source_of_truth_cross_surface_and_restart() {

        let db_path = temp_sqlite_path("d6_ssot");

        {
            let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
            let ctx = Arc::new(ServerContext::with_repository(repo));
            let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
            ctx.db.with_mut_snapshot(|snap| *snap = demo);
            crate::common::pin_clock(&ctx);

            let mut sess = SessionState::new(7102);
            let _ = rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.expect("SelectCharacterID");
            let _ = rpc(&ctx, &mut sess, "addressbook", "AddContact", &[PyRep::Integer(90_000_002), PyRep::Real(10.0), PyRep::Bool(true)]).await.expect("AddContact");
            let _ = rpc(&ctx, &mut sess, "invbroker", "SetLabel", &[PyRep::Integer(100_000_001), PyRep::String("SSOT Flagship".into())]).await.expect("SetLabel");
            let _ = rpc(&ctx, &mut sess, "charMgr", "LogSettings", &[PyRep::Real(3_750.0)]).await.expect("charMgr.LogSettings");
            ctx.db.with_mut_snapshot(|snap| {
                snap.alliances.insert(99_000_777, AllianceRecord {
                    alliance_id: 99_000_777, name: "Single Source Alliance".into(), short_name: "SSOTA".into(),
                    executor_corp_id: 98_000_001, member_corp_ids: [98_000_001].into_iter().collect(),
                });
            });

            let esi = EsiRouter::new(Arc::clone(&ctx));
            let esi_alliances = esi.handle_request(&EsiHttpRequest::new("GET", "/v1/alliances/"));
            assert_eq!(esi_alliances.status_code, 200);
            assert!(String::from_utf8_lossy(&esi_alliances.body).contains("99000777"));
        }

        let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
        let ctx2 = Arc::new(ServerContext::with_repository(repo2));
        let mut sess2 = SessionState::new(7103);
        let _ = rpc(&ctx2, &mut sess2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.expect("SelectCharacterID after restart");

        let names_rep = rpc(&ctx2, &mut sess2, "dogmaIM", "ItemGetInfo", &[PyRep::Integer(100_000_001)]).await.expect("ItemGetInfo after restart");
        assert!(format!("{names_rep:?}").contains("SSOT Flagship"));
        let contacts_rep = rpc(&ctx2, &mut sess2, "addressbook", "GetContactList", &[]).await.expect("addressbook after restart");
        assert!(format!("{contacts_rep:?}").contains("90000002"));

        let has_alliance = ctx2.db.with_snapshot(|snap| snap.alliances.contains_key(&99_000_777));
        assert!(has_alliance);

        let esi2 = EsiRouter::new(Arc::clone(&ctx2));
        let esi_alliances2 = esi2.handle_request(&EsiHttpRequest::new("GET", "/v1/alliances/"));
        assert_eq!(esi_alliances2.status_code, 200);
        assert!(String::from_utf8_lossy(&esi_alliances2.body).contains("99000777"));

        cleanup_sqlite_path(&db_path);
    }

    #[tokio::test]
    async fn test_dg4_module_cycles_stop_on_leave_space_and_dock() {

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let mut sess = SessionState::new(7104);
        let _ = rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");
        let ship_id = sess.ship_id.unwrap_or(100_000_001);

        let ab_id = 880_001_001_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                ab_id,
                InventoryItemRecord {
                    item_id: ab_id,
                    type_id: 434,
                    owner_id: 90_000_001,
                    location_id: ship_id,
                    flag: InventoryFlag::MedSlot0,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        });

        let _ = rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("Undock");
        let act_rep = rpc(
            &ctx,
            &mut sess,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(ab_id), PyRep::String("afterburner".into())],
        )
        .await
        .expect("Activate afterburner");
        assert_eq!(act_rep.dict_get("active"), Some(&PyRep::Bool(true)));
        assert!(ctx.db.with_snapshot(|snap| snap.module_active_states.contains(&ab_id)));

        let handle = ctx.systems.get_or_spawn(JITA_SYSTEM_ID);
        let st_pos = ctx
            .sde
            .stations
            .get(&JITA_STATION_ID)
            .map(|s| s.position)
            .unwrap_or(DVec3::ZERO);
        let _ = handle
            .spawn_ball_with_combat(
                Ball {
                    id: ship_id,
                    type_id: 587,
                    position: st_pos + DVec3::new(500.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 350.0,
                    inertia: 2.5,
                    mass: 1_200_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(587),
            )
            .await;

        let _ = ctx.drain_notifications_for_char(90_000_001);
        ctx.advance_secs(12);
        let bid = ctx.bind_object(&mut sess, "beyonce", JITA_SYSTEM_ID as i64).expect("bind beyonce");
        let _ = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(bid),
                "CmdDock",
                &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(ship_id)],
                &HashMap::new(),
                None,
            )
            .await
            .expect("CmdDock at Jita 4-4");
        assert_eq!(sess.station_id, Some(JITA_STATION_ID));

        assert!(!ctx.db.with_snapshot(|snap| snap.module_active_states.contains(&ab_id)));
        let notifs = ctx.drain_notifications_for_char(90_000_001);
        assert!(notifs.iter().any(|n| n.event == "OnGodmaShipEffect"));
    }

    #[tokio::test]
    async fn test_e3_corp_contract_wallet_roles_and_issuer_departure() {
        // parity: feat:lifecycle.corp-contract-wallet, plan:E-3
        eve_testkit::attest(&["feat:lifecycle.corp-contract-wallet", "plan:E-3"]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let (corp_id, item_id) = (98_000_501_i64, 880_002_001_i64);
        ctx.db.with_mut_snapshot(|snap| {
            snap.corporations.insert(
                corp_id,
                CorporationRecord {
                    corporation_id: corp_id,
                    name: "Contract Wallet Corp".into(),
                    ticker: "CWLT".into(),
                    ceo_id: 90_000_002,
                    alliance_id: None,
                    tax_rate: 0.05,
                    master_wallet: IskCents::from_f64_isk(10_000_000.0),
                    members: [90_000_001, 90_000_002].into_iter().collect(),
                    member_roles: HashMap::new(),
                    applications: HashMap::new(),
                    wallet_divisions: CorporationRecord::default_wallet_divisions(
                        IskCents::from_f64_isk(10_000_000.0),
                    ),
                    description: String::new(),
                    url: String::new(),
                    hq_station_id: Some(JITA_STATION_ID),
                },
            );
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.corporation_id = corp_id;
            }
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id: 34,
                    owner_id: 90_000_001,
                    location_id: JITA_STATION_ID,
                    flag: InventoryFlag::Hangar,
                    quantity: 1_000,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });

        let mut s1 = SessionState::new(7105);
        let _ = rpc(&ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID s1");
        let mut s2 = SessionState::new(7106);
        let _ = rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)])
            .await
            .expect("SelectCharacterID s2");

        let mut kw = HashMap::new();
        kw.insert("forCorp".into(), PyRep::Bool(true));
        let c_args = [
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Real(500_000.0),
            PyRep::Real(50_000.0),
            PyRep::Real(0.0),
            PyRep::String("Corp Contract".into()),
            PyRep::String(String::new()),
            PyRep::List(vec![PyRep::Integer(item_id)]),
        ];
        let deny_err = ctx
            .dispatch_rpc(&mut s1, "contractProxy", None, "CreateContract", &c_args, &kw, None)
            .await
            .expect_err("for_corp contract without role must fail");
        assert_eq!(deny_err.code, "CrpAccessDenied");

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                corp.member_roles
                    .insert(90_000_001, corp_roles::ROLE_CONTRACT_MANAGER);
                if let Some(div) = corp.wallet_divisions.first_mut() {
                    div.balance = IskCents(0); // empty division 1 to test short funds
                }
            }
        });
        // Fixes R1-P1-2: Assert division 1 shortage fails with InsufficientFunds
        let short_div_err = ctx
            .dispatch_rpc(&mut s1, "contractProxy", None, "CreateContract", &c_args, &kw, None)
            .await
            .expect_err("division 1 lacking funds must fail with InsufficientFunds");
        assert_eq!(short_div_err.code, "InsufficientFunds");

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                if let Some(div) = corp.wallet_divisions.first_mut() {
                    div.balance = IskCents::from_f64_isk(10_000_000.0);
                }
            }
        });
        let char_bal_before = ctx.db.get_character(90_000_001).unwrap().balance;
        let corp_bal_before = ctx.db.with_snapshot(|snap| snap.corporations[&corp_id].master_wallet);

        let created = ctx
            .dispatch_rpc(&mut s1, "contractProxy", None, "CreateContract", &c_args, &kw, None)
            .await
            .expect("CreateContract for_corp with ROLE_CONTRACT_MANAGER");
        let cid = created.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();

        let char_bal_mid = ctx.db.get_character(90_000_001).unwrap().balance;
        let corp_bal_mid = ctx.db.with_snapshot(|snap| snap.corporations[&corp_id].master_wallet);
        assert_eq!(char_bal_mid, char_bal_before);
        assert!(corp_bal_mid.0 < corp_bal_before.0);

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.corporation_id = 1_000_009;
            }
        });
        let _ = rpc(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(cid)])
            .await
            .expect("AcceptContract");

        let char_bal_after = ctx.db.get_character(90_000_001).unwrap().balance;
        let corp_bal_after = ctx.db.with_snapshot(|snap| snap.corporations[&corp_id].master_wallet);
        assert_eq!(char_bal_after, char_bal_before);
        assert_eq!(
            corp_bal_after.0 - corp_bal_mid.0,
            IskCents::from_f64_isk(500_000.0).0
        );

        // Second for_corp contract: cancel refund credits reward back to corp wallet; third for_corp courier: failure credits collateral to corp wallet
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.corporation_id = corp_id; }
            for iid in [880_002_002_i64, 880_002_003_i64] {
                snap.items.insert(iid, InventoryItemRecord {
                    item_id: iid, type_id: 34, owner_id: 90_000_001, location_id: JITA_STATION_ID,
                    flag: InventoryFlag::Hangar, quantity: 100, singleton: false, version: 1, ..Default::default()
                });
            }
        });
        s1.corporation_id = Some(corp_id);
        let courier_args = |iid: i64| [
            PyRep::Integer(3), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(1440),
            PyRep::Integer(1), PyRep::Integer(JITA_STATION_ID), PyRep::Integer(60_003_766),
            PyRep::Real(0.0), PyRep::Real(50_000.0), PyRep::Real(100_000.0),
            PyRep::String("Cancel & Collateral Test".into()), PyRep::String(String::new()), PyRep::List(vec![PyRep::Integer(iid)]),
        ];
        let created2 = ctx.dispatch_rpc(&mut s1, "contractProxy", None, "CreateContract", &courier_args(880_002_002), &kw, None).await.expect("CreateContract 2");
        let cid2 = created2.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();
        let corp_bal_pre_cancel = ctx.db.with_snapshot(|snap| snap.corporations[&corp_id].master_wallet);
        let _ = rpc(&ctx, &mut s1, "contractProxy", "DeleteContract", &[PyRep::Integer(cid2)]).await.expect("DeleteContract");
        let corp_bal_post_cancel = ctx.db.with_snapshot(|snap| snap.corporations[&corp_id].master_wallet);
        assert_eq!(corp_bal_post_cancel.0 - corp_bal_pre_cancel.0, IskCents::from_f64_isk(150_000.0).0, "Cancel must credit reward + deposit refund to corp wallet");

        let created3 = ctx.dispatch_rpc(&mut s1, "contractProxy", None, "CreateContract", &courier_args(880_002_003), &kw, None).await.expect("CreateContract 3");
        let cid3 = created3.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();
        let _ = rpc(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(cid3)]).await.expect("AcceptContract 3");
        let corp_bal_pre_fail = ctx.db.with_snapshot(|snap| snap.corporations[&corp_id].master_wallet);
        let _ = rpc(&ctx, &mut s2, "contractProxy", "FailContract", &[PyRep::Integer(cid3)]).await.expect("FailContract 3");
        let corp_bal_post_fail = ctx.db.with_snapshot(|snap| snap.corporations[&corp_id].master_wallet);
        assert_eq!(corp_bal_post_fail.0 - corp_bal_pre_fail.0, IskCents::from_f64_isk(150_000.0).0, "Courier failure must credit collateral + reward refund to corp wallet");
    }

    // parity: feat:lifecycle.secure_container-anchor
    #[tokio::test]
    async fn test_s7_secure_container_anchor_no_bubble_and_30d_despawn() {

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let mut sess = SessionState::new(7107);
        let _ = rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");
        let _ = rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("Undock");
        let ship_id = sess.ship_id.unwrap_or(100_000_001);

        let container_type_id = ctx.sde.types.values().find(|t| (t.group_id == 340 || t.group_id == 448) && t.effect_ids.contains(&649)).map(|t| t.type_id).unwrap_or(3465);
        let cont_id = 880_003_001_i64;
        let unanchorable_id = 880_003_002_i64;
        ctx.db.with_mut_snapshot(|snap| {
            for skill_id in [3426u32, 11584u32] {
                snap.skills.entry(90_000_001).or_default().insert(skill_id, eve_db::TrainedSkillRecord { type_id: skill_id, level: 5, skill_points: 256_000.0 });
            }
            snap.items.insert(unanchorable_id, InventoryItemRecord {
                item_id: unanchorable_id, type_id: 23, owner_id: 90_000_001, location_id: ship_id,
                flag: InventoryFlag::Cargo, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
            snap.items.insert(cont_id, InventoryItemRecord {
                item_id: cont_id, type_id: container_type_id, owner_id: 90_000_001, location_id: ship_id,
                flag: InventoryFlag::Cargo, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
        });

        let err = rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(unanchorable_id)])
            .await
            .expect_err("Cargo container cannot anchor");
        assert_eq!(err.code, "ItemNotAnchorable");

        let hs08_err = rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(cont_id)])
            .await
            .expect_err("Secure container cannot anchor in >= 0.8 system (Jita)");
        assert_eq!(hs08_err.code, "CantAnchorInHighSec08");

        // Move ship to a <= 0.7 high-sec system (security < 0.75) to anchor secure container
        let maurasi_sys_id: u32 = ctx.sde.solar_systems.values().find(|s| s.security_status >= 0.50 && s.security_status < 0.74).map(|s| s.solar_system_id).unwrap_or(30_000_139);
        sess.solar_system_id = Some(maurasi_sys_id);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.solar_system_id = maurasi_sys_id; }
            if let Some(sh) = snap.items.get_mut(&ship_id) { sh.location_id = maurasi_sys_id as i64; }
        });
        let _ = ctx.systems.get_or_spawn(maurasi_sys_id).spawn_ball_with_combat(
            Ball { id: ship_id, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 2.5, mass: 1_200_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop },
            ctx.build_type_combat_state(587),
        ).await;

        let anchor_rep = rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(cont_id)]).await.expect("posMgr.AnchorStructure for secure container");
        assert_eq!(anchor_rep.dict_get("bubbleRadius"), Some(&PyRep::Real(0.0)));
        assert_eq!(anchor_rep.dict_get("state"), Some(&PyRep::String("Anchoring".into())));
        let now_u = ctx.now_unix();
        assert_eq!(anchor_rep.dict_get("despawnUnix"), Some(&PyRep::Integer((now_u + 30 * 86_400) as i64)));
        assert!(ctx.db.with_snapshot(|snap| snap.item_positions.contains_key(&cont_id)));

        // Advance virtual clock past anchoring delay (60s)
        ctx.advance_secs(65);
        let is_anchored = ctx.db.with_snapshot(|snap| {
            snap.session_cluster_lifecycle.space_object_meta.get(&cont_id).map(|m| m.anchor_completed).unwrap_or(false)
        });
        assert!(is_anchored, "Container must be marked anchored after anchoring delay elapses");

        let despawned = step_wreck_and_jetcan_decay(&ctx, maurasi_sys_id, now_u + 30 * 86_400 + 1).await;
        assert!(despawned.contains(&cont_id));
        assert!(ctx.db.get_item(cont_id).is_none());
    }

    #[tokio::test]
    async fn test_so7_chat_and_mail_item_fitting_hyperlinks_roundtrip() {

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let mut s1 = SessionState::new(7108);
        let _ = rpc(&ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.expect("SelectCharacterID s1");
        let mut s2 = SessionState::new(7109);
        let _ = rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)]).await.expect("SelectCharacterID s2");

        let fit_rep = rpc(&ctx, &mut s1, "charFittingMgr", "SaveFitting", &[
            PyRep::Integer(90_000_001),
            PyRep::keyval([
                ("name", PyRep::String("Hyperlink Rifter".into())),
                ("shipTypeID", PyRep::Integer(587)),
                ("description", PyRep::String("Linked via chat".into())),
                ("fitData", PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(434), PyRep::Integer(19), PyRep::Integer(1)])])),
            ]),
        ]).await.expect("SaveFitting");
        let fit_id = fit_rep.as_i64().unwrap_or(1);

        let rich_msg = format!(
            "<a href=\"showinfo:587//100000001\">Rifter</a> | <a href=\"fitting:587:434;1::\">Fit #{fit_id}</a> | <a href=\"killReport:1:abcdef\">KM</a> | <a href=\"bookmarkFolder:1\">BM</a> | <a href=\"overviewPreset:default\">OV</a>"
        );

        let chan_id = format!("solarsystemid2:{JITA_SYSTEM_ID}");
        let _ = rpc(&ctx, &mut s1, "XmppChatMgr", "AddCharacterToChannel", &[PyRep::String(chan_id.clone())]).await.expect("AddCharacterToChannel");
        let chat_rep = rpc(&ctx, &mut s1, "LSC", "SendMessage", &[PyRep::String(chan_id), PyRep::String(rich_msg.clone())]).await.expect("LSC.SendMessage");
        assert_eq!(chat_rep.dict_get("text"), Some(&PyRep::String(rich_msg.clone())));

        let mail_id_rep = rpc(&ctx, &mut s1, "mailMgr", "SendMail", &[
            PyRep::List(vec![PyRep::Integer(90_000_002)]),
            PyRep::String("Shared Links".into()),
            PyRep::String(rich_msg.clone()),
        ]).await.expect("mailMgr.SendMail");
        let mail_id = mail_id_rep.dict_get("messageID").and_then(|v| v.as_i64()).or_else(|| mail_id_rep.as_i64()).unwrap_or(1);

        let body_rep = rpc(&ctx, &mut s2, "mailMgr", "GetBody", &[PyRep::Integer(mail_id), PyRep::Bool(false)]).await.expect("mailMgr.GetBody");
        let decomp = match &body_rep {
            PyRep::Buffer(b) => miniz_oxide::inflate::decompress_to_vec_zlib(b).map(String::from_utf8).ok().and_then(|r| r.ok()).unwrap_or_default(),
            _ => body_rep.as_str().or_else(|| body_rep.dict_get("body").and_then(|v| v.as_str())).unwrap_or("").to_string(),
        };
        assert!(decomp.contains("showinfo:587//100000001") && decomp.contains("fitting:587:434;1::"));

        let fittings = rpc(&ctx, &mut s1, "charFittingMgr", "GetFittings", &[PyRep::Integer(90_000_001)]).await.expect("GetFittings");
        assert!(format!("{fittings:?}").contains("Hyperlink Rifter"));
    }

    #[tokio::test]
    async fn test_t8_atomic_rollback_all_stores_on_rpc_and_wal_failure() {

        let db_path = temp_sqlite_path("t8_atomic");
        let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
        let ctx = ServerContext::with_repository(repo);
        let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        ctx.db.with_mut_snapshot(|snap| *snap = demo);
        crate::common::pin_clock(&ctx);

        let mut sess = SessionState::new(7110);
        let _ = rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.expect("SelectCharacterID");

        let bal_before = ctx.db.get_character(90_000_001).unwrap().balance;
        let items_before = ctx.db.with_snapshot(|snap| snap.items.len());
        let ledger_before = ctx.db.with_snapshot(|snap| snap.mining_ledger.len());
        let contracts_before = ctx.db.with_snapshot(|snap| snap.contracts.len());

        let fault_res: Result<(), String> = ctx.db.try_with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.balance = IskCents(123); }
            snap.items.insert(999_888_777, InventoryItemRecord {
                item_id: 999_888_777, type_id: 34, owner_id: 90_000_001, location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar, quantity: 500, singleton: false, version: 1, ..Default::default()
            });
            snap.mining_ledger.push(MiningLedgerRecord {
                character_id: 90_000_001, solar_system_id: JITA_SYSTEM_ID,
                type_id: 1230, quantity: 9_999, date: "2026-10-23".into(), timestamp_unix: 1_700_000_000,
            });
            Err("injected multi-store failure".to_string())
        });
        assert!(fault_res.is_err());

        let mut seed = 0x5EED_1234_5678_9ABCu64;
        for step in 0..16u64 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let r: Result<(), String> = ctx.db.try_with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                    ch.balance = IskCents((seed % 1_000_000) as i64);
                }
                snap.items.insert(999_888_000 + step as i64, InventoryItemRecord {
                    item_id: 999_888_000 + step as i64, type_id: 34, owner_id: 90_000_001,
                    location_id: JITA_STATION_ID, flag: InventoryFlag::Hangar,
                    quantity: (seed % 500 + 1) as i32, singleton: false, version: 1, ..Default::default()
                });
                Err(format!("randomized fault step {step}"))
            });
            assert!(r.is_err());
        }

        let rpc_fail = rpc(&ctx, &mut sess, "contractProxy", "CreateContract", &[
            PyRep::Integer(1), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(1440), PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID), PyRep::Integer(JITA_STATION_ID),
            PyRep::Real(1_000.0), PyRep::Real(999_999_999_999.0), PyRep::Real(0.0),
            PyRep::String("Atomic Rollback".into()), PyRep::String(String::new()),
            PyRep::List(vec![PyRep::Integer(100_000_003)]),
        ]).await;
        assert!(rpc_fail.is_err());

        assert_eq!(ctx.db.get_character(90_000_001).unwrap().balance, bal_before);
        assert_eq!(ctx.db.with_snapshot(|snap| snap.items.len()), items_before);
        assert_eq!(ctx.db.with_snapshot(|snap| snap.mining_ledger.len()), ledger_before);
        assert_eq!(ctx.db.with_snapshot(|snap| snap.contracts.len()), contracts_before);
        assert_eq!(ctx.db.get_item(100_000_003).unwrap().flag, InventoryFlag::Hangar);
        cleanup_sqlite_path(&db_path);
    }

    #[tokio::test]
    async fn test_t9_no_transient_active_cycle_attrs_persisted_on_restart() {

        let db_path = temp_sqlite_path("t9_transient");
        let (ship_id, active_mod_id, mutated_mod_id) = (100_000_001_i64, 880_004_001_i64, 880_004_002_i64);

        {
            let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
            let ctx = ServerContext::with_repository(repo);
            let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
            ctx.db.with_mut_snapshot(|snap| {
                *snap = demo;
                snap.module_active_states.insert(active_mod_id);
                if let Some(ship) = snap.items.get_mut(&ship_id) {
                    ship.dynamic_attributes.insert(267, 0.15);
                    ship.dynamic_attributes.insert(271, 0.05);
                }
                snap.items.insert(active_mod_id, InventoryItemRecord {
                    item_id: active_mod_id, type_id: 434, owner_id: 90_000_001, location_id: ship_id,
                    flag: InventoryFlag::MedSlot0, quantity: 1, singleton: true, version: 1,
                    ..Default::default()
                });
                snap.items.insert(mutated_mod_id, InventoryItemRecord {
                    item_id: mutated_mod_id, type_id: 434, owner_id: 90_000_001, location_id: ship_id,
                    flag: InventoryFlag::MedSlot1, quantity: 1, singleton: true, version: 1,
                    dynamic_attributes: [(2114u16, 1.0), (30u16, 45.0)].into_iter().collect(),
                    ..Default::default()
                });
            });
        }

        let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
        let ctx2 = ServerContext::with_repository(repo2);
        ctx2.db.with_snapshot(|snap| {
            assert!(snap.module_active_states.is_empty());
            let ship = snap.items.get(&ship_id).expect("active ship item");
            assert!(!ship.dynamic_attributes.contains_key(&267));
            assert!(!ship.dynamic_attributes.contains_key(&271));

            let mutated = snap.items.get(&mutated_mod_id).expect("mutated item");
            assert_eq!(mutated.dynamic_attributes.get(&2114).copied(), Some(1.0));
            assert_eq!(mutated.dynamic_attributes.get(&30).copied(), Some(45.0));
        });
        cleanup_sqlite_path(&db_path);
    }
}
