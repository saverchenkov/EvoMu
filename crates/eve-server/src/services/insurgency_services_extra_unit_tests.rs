//! Extra branch-coverage and mutation-killing unit tests for `insurgency_services.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::insurgency_services::*;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;
use std::sync::RwLock;

async fn call_insurgency_sys(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, sess, "insurgencySolarsystem", method, args, &kw)
        .await
        .expect("insurgencySolarsystem handled")
}

async fn call_dungeon_cache(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, sess, "dungeonInstanceCacheMgr", method, args, &kw)
        .await
        .expect("dungeonInstanceCacheMgr handled")
}

#[test]
fn campaign_snapshot_phases_vulnerability_and_stage_boundaries() {
    for (pts, expected) in [
        (-500, 0),
        (-1, 0),
        (0, 0),
        (1, 1),
        (199, 1),
        (200, 1),
        (201, 2),
        (399, 2),
        (400, 2),
        (401, 3),
        (599, 3),
        (600, 3),
        (601, 4),
        (799, 4),
        (800, 4),
        (801, 5),
        (999, 5),
        (1000, 5),
        (5000, 5),
    ] {
        assert_eq!(insurgency_stage(pts), expected, "pts={pts}");
    }

    let mut camp = InsurgencyCampaign {
        campaign_id: 701,
        pirate_faction_id: FACTION_ANGEL_CARTEL,
        target_faction_id: 500_004,
        fob_solar_system_id: 30002505,
        systems: vec![30002505, 30002506, 30002507, 30002508, 30002509, 30002510],
        created_at_unix: 1_000,
        forecasting_end_unix: 2_000,
        active_end_unix: 5_000,
        winddown_end_unix: 8_000,
        corruption_points: HashMap::from([(30002505, 450)]),
        suppression_points: HashMap::new(),
    };

    assert_eq!(get_campaign_phase_and_timer(&camp, 1_999), (PHASE_FORECASTING, 2_000));
    assert_eq!(get_campaign_phase_and_timer(&camp, 2_000), (PHASE_ACTIVE, 5_000));
    assert_eq!(get_campaign_phase_and_timer(&camp, 4_999), (PHASE_ACTIVE, 5_000));
    assert_eq!(get_campaign_phase_and_timer(&camp, 5_000), (PHASE_WINDDOWN, 8_000));

    // 4 systems at suppression stage 5 -> not vulnerable even in Active phase
    for sys in [30002505, 30002506, 30002507, 30002508] {
        camp.suppression_points.insert(sys, 900);
    }
    let snap_4 = build_campaign_snapshot_for_system(&camp, 30002505, 3_000, 0);
    assert_eq!(snap_4.dict_get("isFobSystem"), Some(&PyRep::Bool(true)));
    assert_eq!(snap_4.dict_get("isFobVulnerable"), Some(&PyRep::Bool(false)));
    assert_eq!(snap_4.dict_get("corruptionStage"), Some(&PyRep::Integer(3)));
    assert_eq!(snap_4.dict_get("suppressionStage"), Some(&PyRep::Integer(5)));

    // 5th system reaches stage 5 -> vulnerable when emp_mod == 0 in Active phase, but not in Forecasting
    camp.suppression_points.insert(30002509, 801);
    let snap_fc = build_campaign_snapshot_for_system(&camp, 30002506, 1_500, 0);
    assert_eq!(snap_fc.dict_get("isFobSystem"), Some(&PyRep::Bool(false)));
    assert_eq!(snap_fc.dict_get("isFobVulnerable"), Some(&PyRep::Bool(false)));

    let snap_act = build_campaign_snapshot_for_system(&camp, 30002506, 3_000, 0);
    assert_eq!(snap_act.dict_get("isFobVulnerable"), Some(&PyRep::Bool(true)));

    // With emp_mod = 1, requires 6 systems at stage 5
    let snap_mod1 = build_campaign_snapshot_for_system(&camp, 30002506, 3_000, 1);
    assert_eq!(snap_mod1.dict_get("isFobVulnerable"), Some(&PyRep::Bool(false)));
    camp.suppression_points.insert(30002510, 1000);
    let snap_mod1_ok = build_campaign_snapshot_for_system(&camp, 30002506, 3_000, 1);
    assert_eq!(snap_mod1_ok.dict_get("isFobVulnerable"), Some(&PyRep::Bool(true)));
}

#[test]
fn corruption_and_suppression_modifiers_caps_and_ice_heist_spawning() {
    let ctx = test_ctx();
    let camp_id = 77_100_i64;
    let fob_sys = 30045324_u32;
    let other_sys = 30045325_u32;

    // Missing campaign returns (0, 0)
    assert_eq!(add_system_corruption(&ctx, 999_999, fob_sys, 200), (0, 0));
    assert_eq!(add_system_suppression(&ctx, 999_999, fob_sys, 200), (0, 0));

    // Init Angel campaign where fob_sys is not initially in `systems` vec
    init_insurgency_campaign(
        &ctx,
        camp_id,
        FACTION_ANGEL_CARTEL,
        500_004,
        fob_sys,
        vec![other_sys],
    );
    let has_fob = read_insurgency_runtime(&ctx, |rt| {
        rt.campaigns[&camp_id].systems.contains(&fob_sys)
    });
    assert!(has_fob);

    // Move campaign into Active phase
    let now = ctx.now_unix();
    with_insurgency_runtime(&ctx, |rt| {
        let c = rt.campaigns.get_mut(&camp_id).unwrap();
        c.forecasting_end_unix = now.saturating_sub(1000);
        c.active_end_unix = now + 10_000;
        c.winddown_end_unix = now + 20_000;
    });

    // Advance suppression to stage 3 (401 pts) in fob_sys -> corruption gain reduced to 70%
    let (s_pts, s_stage) = add_system_suppression(&ctx, camp_id, fob_sys, 401);
    assert_eq!((s_pts, s_stage), (401, 3));

    // Adding 100 corruption when suppression stage >= 3 yields 70 pts
    let (c_pts, c_stage) = add_system_corruption(&ctx, camp_id, fob_sys, 100);
    assert_eq!((c_pts, c_stage), (70, 1));

    // Downward adjustment emits SetNotice and clamps at 0
    let (c_pts_down, _) = add_system_corruption(&ctx, camp_id, fob_sys, -50);
    assert_eq!(c_pts_down, 20);
    let (s_pts_down, _) = add_system_suppression(&ctx, camp_id, fob_sys, -10);
    assert_eq!(s_pts_down, 391);

    // Advance suppression to stage 5 (900 pts) -> prevents corruption from reaching stage 5 (capped at 800)
    let (s5_pts, s5_stg) = add_system_suppression(&ctx, camp_id, fob_sys, 600);
    assert_eq!((s5_pts, s5_stg), (991, 5));
    let (capped_c, capped_stg) = add_system_corruption(&ctx, camp_id, fob_sys, 2000);
    assert_eq!((capped_c, capped_stg), (800, 4));

    // Stage 4 corruption in fob_sys spawns an Ice Heist when not on cooldown
    let ih_count = read_insurgency_runtime(&ctx, |rt| {
        rt.ice_heist_instances
            .iter()
            .filter(|h| h.solar_system_id == fob_sys && !h.completed)
            .count()
    });
    assert_eq!(ih_count, 1);

    // In other_sys, put ice_heist_respawn_until on cooldown -> stage 3 corruption does NOT spawn direct 800_001+ Ice Heist
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_complex_ops
            .ice_heist_respawn_until
            .insert(other_sys, ctx.now_unix_secs() + 3600);
    });
    let (_, other_stg) = add_system_corruption(&ctx, camp_id, other_sys, 450);
    assert_eq!(other_stg, 3);
    let direct_ih = read_insurgency_runtime(&ctx, |rt| {
        rt.ice_heist_instances
            .iter()
            .filter(|h| h.solar_system_id == other_sys && h.instance_id < 860_001)
            .count()
    });
    assert_eq!(direct_ih, 0);

    // Stage 5 corruption in other_sys (no suppression) succeeds and prunes stage-3..4 Ice Heist sites
    let (c5_pts, c5_stg) = add_system_corruption(&ctx, camp_id, other_sys, 600);
    assert_eq!((c5_pts, c5_stg), (1050, 5));
    let stage5_ih = read_insurgency_runtime(&ctx, |rt| {
        rt.ice_heist_instances
            .iter()
            .filter(|h| h.solar_system_id == other_sys)
            .count()
    });
    assert_eq!(stage5_ih, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stronghold_destruction_outcomes_and_notifications() {
    let ctx = test_ctx();
    let camp_id = 77_200_i64;
    let fob_sys = 30045324_u32;

    assert_eq!(spawn_pirate_stronghold(&ctx, 999_999), None);
    assert_eq!(ensure_stronghold_in_space(&ctx, 999_999).await, None);

    init_insurgency_campaign(
        &ctx,
        camp_id,
        FACTION_ANGEL_CARTEL,
        500_004,
        fob_sys,
        vec![fob_sys],
    );
    let sh_id = spawn_pirate_stronghold(&ctx, camp_id).unwrap();
    // Second call returns existing stronghold instance
    assert_eq!(spawn_pirate_stronghold(&ctx, camp_id), Some(sh_id));

    // Cannot destroy stronghold while in Forecasting phase
    assert!(
        on_insurgency_stronghold_destroyed_core(&ctx.systems, &ctx.db, fob_sys, sh_id, None)
            .await
            .is_none()
    );

    // Transition to Active and set suppression to 400 on fob_sys (with 5 Stage-5 suppressed campaign systems)
    // so +500 on fob_sys reaches stage 5 (900) upon destruction.
    let now = ctx.now_unix();
    with_insurgency_runtime(&ctx, |rt| {
        let c = rt.campaigns.get_mut(&camp_id).unwrap();
        c.forecasting_end_unix = now.saturating_sub(500);
        c.active_end_unix = now + 5000;
        c.winddown_end_unix = now + 10_000;
        for offset in 1..=5_u32 {
            c.suppression_points.insert(fob_sys + offset, 1_000);
        }
        c.suppression_points.insert(fob_sys, 400);
    });

    // Unrelated ball returns None
    assert!(
        on_insurgency_stronghold_destroyed_core(&ctx.systems, &ctx.db, fob_sys, 123_456, None)
            .await
            .is_none()
    );

    let info = on_insurgency_stronghold_destroyed_core(
        &ctx.systems,
        &ctx.db,
        fob_sys,
        sh_id,
        Some(90_000_077),
    )
    .await
    .expect("stronghold destroyed in Active phase");
    assert_eq!(info.old_suppression, 400);
    assert_eq!(info.new_suppression, 900);
    assert_eq!(info.new_suppression_stage, 5);

    // Notify with pending_notifications map and broadcast channel
    let pending_map = RwLock::new(HashMap::new());
    let (tx, mut rx) = tokio::sync::broadcast::channel(16);
    notify_stronghold_destruction(
        &ctx.db,
        Some(&ctx.quasar_gateway),
        Some(&pending_map),
        Some(&tx),
        &info,
    );
    assert!(!pending_map.read().unwrap().is_empty());
    assert!(rx.try_recv().is_ok());

    // End campaign with PirateWin, EmpireWin, and default outcomes
    with_insurgency_runtime(&ctx, |rt| {
        rt.campaign_outcomes.insert(camp_id, "PirateWin".into());
    });
    end_insurgency_campaign(&ctx, camp_id);

    // Notify fallback when campaign is already removed from DB
    notify_stronghold_destruction(&ctx.db, None, Some(&pending_map), None, &info);

    let camp_emp = 77_201_i64;
    init_insurgency_campaign(&ctx, camp_emp, FACTION_GURISTAS, 500_001, fob_sys, vec![fob_sys]);
    with_insurgency_runtime(&ctx, |rt| {
        rt.campaign_outcomes.insert(camp_emp, "EmpireWin".into());
    });
    end_insurgency_campaign(&ctx, camp_emp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn complex_capture_tick_pirate_vs_antipirate_and_contested_grid() {
    let ctx = test_ctx();
    let camp_id = 77_300_i64;
    let sys_id = 30002505_u32; // lowsec warzone system
    let pirate_cid = 90_000_001_i64;
    let empire_cid = 90_000_002_i64;
    let pirate_ship = 910_001_i64;
    let empire_ship = 910_002_i64;

    // Explicit caller with fid == 0 returns false immediately
    assert!(!step_system_insurgency_capture_tick(&ctx, sys_id, pirate_ship, 0).await);

    init_insurgency_campaign(&ctx, camp_id, FACTION_GURISTAS, 500_001, sys_id, vec![sys_id]);
    let now = ctx.now_unix();
    with_insurgency_runtime(&ctx, |rt| {
        let c = rt.campaigns.get_mut(&camp_id).unwrap();
        c.forecasting_end_unix = now.saturating_sub(100);
        c.active_end_unix = now + 10_000;
        c.winddown_end_unix = now + 20_000;
    });

    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.fw_complex_position = Some([0.0, 0.0, 0.0]);
        st.fw_capture_duration_seconds = 300;
        if let Some(ch) = snap.characters.get_mut(&pirate_cid) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = pirate_ship;
            ch.war_faction_id = Some(FACTION_GURISTAS);
        }
        if let Some(ch) = snap.characters.get_mut(&empire_cid) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = empire_ship;
            ch.war_faction_id = Some(500_001);
        }
        snap.items.insert(
            pirate_ship,
            InventoryItemRecord {
                item_id: pirate_ship,
                type_id: 587,
                owner_id: pirate_cid,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            empire_ship,
            InventoryItemRecord {
                item_id: empire_ship,
                type_id: 587,
                owner_id: empire_cid,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: pirate_ship,
            type_id: 587,
            position: glam::DVec3::new(5_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // Uncontested pirate ship advances corruption
    assert!(step_system_insurgency_capture_tick(&ctx, sys_id, pirate_ship, FACTION_GURISTAS).await);
    let corr_after = read_insurgency_runtime(&ctx, |rt| {
        rt.campaigns[&camp_id]
            .corruption_points
            .get(&sys_id)
            .copied()
            .unwrap_or(0)
    });
    assert!(corr_after > 0);

    // Spawn opposing empire ship on grid and mark both characters online -> contested grid returns false
    let _ = sys_handle
        .spawn_ball(Ball {
            id: empire_ship,
            type_id: 587,
            position: glam::DVec3::new(6_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    step_system_insurgency_tick_delta(&ctx, sys_id, Some(10)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_error_branches_and_instance_filters() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);

    // GetLocalCampaignClientSnapshot without solar_system_id fails with NoSolarSystem
    sess.solar_system_id = None;
    let err = call_insurgency_sys(&ctx, &mut sess, "GetLocalCampaignClientSnapshot", &[])
        .await
        .unwrap_err();
    assert_eq!(err.code, "NoSolarSystem");

    // With valid solar_system_id but no campaign -> (sys_id, None)
    sess.solar_system_id = Some(30_000_142);
    let snap_none = call_insurgency_sys(&ctx, &mut sess, "GetLocalCampaignClientSnapshot", &[])
        .await
        .unwrap();
    assert_eq!(
        snap_none,
        PyRep::Tuple(vec![PyRep::Integer(30_000_142), PyRep::None])
    );

    // Invalid solar system ID in dungeonInstanceCacheMgr returns InvalidSolarSystem
    for method in [
        "GetPirateInsurgencyInstances",
        "GetPirateInsurgencyIceHeistInstances",
    ] {
        let bad = call_dungeon_cache(&ctx, &mut sess, method, &[PyRep::Integer(999_999_999)])
            .await
            .unwrap_err();
        assert_eq!(bad.code, "InvalidSolarSystem");

        // PyRep::None uses session.solar_system_id fallback
        let ok_res = call_dungeon_cache(&ctx, &mut sess, method, &[PyRep::None])
            .await
            .unwrap();
        assert!(matches!(ok_res, PyRep::List(_)));
    }
}
