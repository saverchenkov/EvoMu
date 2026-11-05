#![forbid(unsafe_code)]

use crate::common::{self, LivePilot};
use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::serverops_cluster::{
    clear_manual_system_tidi_override, get_system_tidi_factor, get_system_worker_node,
    rebalance_overloaded_worker_nodes,
};
use eve_server::services::station_structure::structure_lifecycle::sync_all_structures;
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;
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

async fn bind_rpc(
    ctx: &ServerContext,
    p: &mut LivePilot,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(&mut p.sess, svc, Some(p.beyonce), method, args, &HashMap::new(), None)
        .await
}

async fn gm_docked_in_jita(ctx: &ServerContext, sess_id: u64, char_id: i64, ship_type_id: u32) -> (SessionState, i64) {
    let user = format!("live{char_id}");
    let pass = format!("{user}-secret");
    ctx.db.provision_gm_account(&user, &pass, char_id, &format!("Live Pilot {char_id}"));
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, &user, &pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    let created = common::slash_cmd(ctx, &mut sess, &format!("/createitem {ship_type_id} 1")).await;
    let ship_id = created.dict_get("itemID").and_then(PyRep::as_i64).expect("ship itemID");
    let skills: Vec<u32> = ctx
        .sde
        .types
        .get(&ship_type_id)
        .map(|t| {
            [182_u16, 183, 184, 1285, 1289, 1290]
                .iter()
                .filter_map(|a| t.default_attributes.get(a))
                .map(|v| v.round() as u32)
                .filter(|&s| s > 0)
                .collect()
        })
        .unwrap_or_default();
    for sid in skills {
        common::slash_cmd(ctx, &mut sess, &format!("/giveskill {sid} 5")).await;
    }
    for skill_id in [3363_u32, 3368, 11584] {
        common::slash_cmd(ctx, &mut sess, &format!("/giveskill {skill_id} 5")).await;
    }
    ctx.advance_secs(31);
    rpc(ctx, &mut sess, "ship", "Board", &[PyRep::Integer(ship_id)])
        .await
        .expect("ship.Board");
    ctx.advance_secs(31);
    (sess, ship_id)
}

async fn undock_to_system(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, ship_id: i64, sys_id: u32) -> LivePilot {
    rpc(ctx, sess, "stationSvc", "Undock", &[]).await.expect("stationSvc.Undock");
    common::slash_cmd(ctx, sess, &format!("/tr {sys_id}")).await;
    sess.ship_id = Some(ship_id);
    let bind = rpc(ctx, sess, "beyonce", "MachoBindObject", &[PyRep::Integer(i64::from(sys_id)), PyRep::None])
        .await
        .expect("MachoBindObject");
    let PyRep::Tuple(parts) = bind else { panic!("expected bind tuple") };
    let beyonce = parts[0].as_i64().expect("beyonce oid") as u64;
    ctx.advance_secs(31);
    let _ = sess.drain_notifications();
    let _ = ctx.drain_notifications_for_char(char_id);
    LivePilot {
        sess: sess.clone(),
        char_id,
        ship_id,
        beyonce,
    }
}

// parity: feat:serverops.node-balancing, feat:serverops.time-dilation
#[tokio::test]
async fn test_serverops_node_balancing_and_time_dilation_live() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    const JITA: u32 = 30_000_142;
    const PERIMETER: u32 = 30_000_144; // Sibling system in Kimotoro constellation (20_000_020)
    const MAURASI: u32 = 30_000_140;

    // 1. Constellation grouping: Jita & Perimeter start on the same constellation worker node
    let jita_initial_node = get_system_worker_node(&ctx, JITA);
    let perim_initial_node = get_system_worker_node(&ctx, PERIMETER);
    assert_eq!(jita_initial_node, perim_initial_node, "Kimotoro systems share constellation worker node initially");

    let mut jita_pilot = common::gm_pilot_in_space(&ctx, 7101, 93_100_001, 606, JITA).await;
    let mut perim_pilot = common::gm_pilot_in_space(&ctx, 7102, 93_100_002, 606, PERIMETER).await;
    let _maurasi_pilot = common::gm_pilot_in_space(&ctx, 7103, 93_100_003, 606, MAURASI).await;

    // 2. Reinforce Jita for fleet fight via /reinforce -> isolates onto dedicated node >= 100
    let reinforced_node = common::slash_cmd(&ctx, &mut jita_pilot.sess, "/reinforce")
        .await
        .as_i64()
        .expect("reinforced node ID");
    assert!(reinforced_node >= 100, "Reinforced node must be dedicated ID >= 100, got {reinforced_node}");
    assert_eq!(get_system_worker_node(&ctx, JITA), reinforced_node as u32);
    assert_ne!(
        get_system_worker_node(&ctx, PERIMETER),
        reinforced_node as u32,
        "Sibling system Perimeter must migrate off dedicated node"
    );

    // machoNet.GetNodeFromAddress resolves registered reinforced node endpoint
    let resolved = rpc(
        &ctx,
        &mut jita_pilot.sess,
        "machoNet",
        "GetNodeFromAddress",
        &[PyRep::Tuple(vec![
            PyRep::String("127.0.0.1".into()),
            PyRep::Integer(26_000 + reinforced_node),
        ])],
    )
    .await
    .expect("GetNodeFromAddress");
    assert_eq!(resolved, PyRep::Integer(reinforced_node));

    // Rebalance overloaded general worker nodes (never touches Jita's dedicated node)
    let _migrated = rebalance_overloaded_worker_nodes(&ctx);
    assert_eq!(get_system_worker_node(&ctx, JITA), reinforced_node as u32);

    // 3. Manual TiDi (/tidi 0.50): DoSimClockRebase sent ONLY to Jita pilot, simulation & module cycles scale 0.5x
    common::slash_cmd(&ctx, &mut jita_pilot.sess, "/giveskill 3450 5").await;
    common::slash_cmd(&ctx, &mut perim_pilot.sess, "/giveskill 3450 5").await;
    let ab_jita = common::slash_cmd(&ctx, &mut jita_pilot.sess, "/fit me 438")
        .await
        .dict_get("itemID")
        .and_then(PyRep::as_i64)
        .expect("fit AB Jita");
    let ab_perim = common::slash_cmd(&ctx, &mut perim_pilot.sess, "/fit me 438")
        .await
        .dict_get("itemID")
        .and_then(PyRep::as_i64)
        .expect("fit AB Perim");

    let _ = jita_pilot.sess.drain_notifications();
    let _ = perim_pilot.sess.drain_notifications();
    let _ = ctx.drain_notifications_for_char(jita_pilot.char_id);
    let _ = ctx.drain_notifications_for_char(perim_pilot.char_id);

    let applied_tidi = common::slash_cmd(&ctx, &mut jita_pilot.sess, "/tidi 0.50")
        .await
        .as_f64()
        .expect("/tidi factor");
    assert!((applied_tidi - 0.50).abs() < 1e-6);

    let jita_notifs = ctx.drain_notifications_for_char(jita_pilot.char_id);
    let perim_notifs = ctx.drain_notifications_for_char(perim_pilot.char_id);
    let rebase = jita_notifs
        .iter()
        .find(|n| n.event == "DoSimClockRebase")
        .expect("DoSimClockRebase must be emitted to pilot in dilated system");
    assert_eq!(rebase.args.len(), 1, "DoSimClockRebase carries 1 tuple arg (oldSimTime, newSimTime)");
    let PyRep::Tuple(times) = &rebase.args[0] else {
        panic!("expected tuple payload in DoSimClockRebase");
    };
    let old_t = times[0].as_f64().expect("oldSimTime");
    let new_t = times[1].as_f64().expect("newSimTime");
    assert!(new_t >= old_t);
    assert!(
        !perim_notifs.iter().any(|n| n.event == "DoSimClockRebase"),
        "Pilots in undilated Perimeter must NOT receive Jita's DoSimClockRebase"
    );

    // Module cycle duration under TiDi 0.50 takes 2x wall-clock time to complete vs Perimeter (TiDi 1.00)
    // Pass repeat=0 (4th arg) so each Afterburner executes a single cycle and finishes cleanly
    let jita_dur = rpc(
        &ctx,
        &mut jita_pilot.sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(ab_jita), PyRep::None, PyRep::None, PyRep::Integer(0)],
    )
    .await
    .expect("Activate in Jita")
    .dict_get("duration")
    .and_then(PyRep::as_f64)
    .expect("Jita duration");
    let perim_dur = rpc(
        &ctx,
        &mut perim_pilot.sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(ab_perim), PyRep::None, PyRep::None, PyRep::Integer(0)],
    )
    .await
    .expect("Activate in Perimeter")
    .dict_get("duration")
    .and_then(PyRep::as_f64)
    .expect("Perimeter duration");
    assert!((jita_dur - perim_dur).abs() < 0.01 && perim_dur > 5.0 && perim_dur < 10.0);

    let jita_handle = ctx.systems.get_or_spawn(JITA);
    let perim_handle = ctx.systems.get_or_spawn(PERIMETER);
    // After 9s wall-clock: Perimeter (TiDi 1.00 -> 9.0s sim > 7.5s) finished cycle; Jita (TiDi 0.50 -> 4.5s sim < 7.5s) still cycling
    for _ in 0..9 {
        let _ = jita_handle.step_manual(1.0).await.expect("step Jita");
        let _ = perim_handle.step_manual(1.0).await.expect("step Perimeter");
    }
    assert!(
        !perim_handle.has_module_cycle(perim_pilot.ship_id, ab_perim).await,
        "Perimeter (TiDi 1.00) 7.5s module cycle must complete within 9s wall-clock"
    );
    assert!(
        jita_handle.has_module_cycle(jita_pilot.ship_id, ab_jita).await,
        "Jita (TiDi 0.50) 7.5s module cycle must still be active at 9s wall-clock (4.5s sim time)"
    );
    // After +7s wall-clock (16s total -> 8.0s sim > 7.5s): Jita finishes its dilated cycle
    for _ in 0..7 {
        let _ = jita_handle.step_manual(1.0).await.expect("step Jita completion");
    }
    assert!(
        !jita_handle.has_module_cycle(jita_pilot.ship_id, ab_jita).await,
        "Jita (TiDi 0.50) 7.5s module cycle must complete by 16s wall-clock (2x normal duration)"
    );

    // Movement delta in Jita (TiDi 0.50) is half of Perimeter (TiDi 1.00) at steady speed over identical wall dt
    bind_rpc(&ctx, &mut jita_pilot, "beyonce", "CmdSetSpeedFraction", &[PyRep::Real(1.0)])
        .await
        .expect("CmdSetSpeedFraction Jita");
    bind_rpc(&ctx, &mut perim_pilot, "beyonce", "CmdSetSpeedFraction", &[PyRep::Real(1.0)])
        .await
        .expect("CmdSetSpeedFraction Perimeter");
    let _ = jita_handle.step_manual(120.0).await.expect("accelerate Jita (60s sim) to steady speed");
    let _ = perim_handle.step_manual(60.0).await.expect("accelerate Perimeter (60s sim) to steady speed");
    let j0 = jita_handle.get_ball_snapshot(jita_pilot.ship_id).await.unwrap().unwrap().1.position;
    let p0 = perim_handle.get_ball_snapshot(perim_pilot.ship_id).await.unwrap().unwrap().1.position;
    let _b_jita = jita_handle.step_manual(4.0).await.expect("step Jita");
    let _b_perim = perim_handle.step_manual(4.0).await.expect("step Perimeter");
    let dist_jita = jita_handle.get_ball_snapshot(jita_pilot.ship_id).await.unwrap().unwrap().1.position.distance(j0);
    let dist_perim = perim_handle.get_ball_snapshot(perim_pilot.ship_id).await.unwrap().unwrap().1.position.distance(p0);
    assert!(dist_jita > 0.0 && (dist_jita * 2.0 - dist_perim).abs() < 0.5);

    // 4. Load-induced automatic TiDi down to 0.10 (10%) via /autotidi + tick_background_cluster
    clear_manual_system_tidi_override(&ctx, PERIMETER);
    let _ = ctx.drain_notifications_for_char(perim_pilot.char_id);
    common::slash_cmd(&ctx, &mut perim_pilot.sess, "/autotidi 1").await;
    for i in 0..12_u64 {
        common::slash_cmd(&ctx, &mut perim_pilot.sess, "/spawn 587 1").await;
        if i == 0 {
            common::slash_cmd(&ctx, &mut perim_pilot.sess, "/fit me 3634").await;
        }
    }
    tick_background_cluster(&ctx).await;
    let auto_tidi = get_system_tidi_factor(&ctx, PERIMETER);
    assert!(
        (auto_tidi - 0.10).abs() < 1e-6,
        "Heavy load (>10x threshold=1) must clamp auto-TiDi at 0.10, got {auto_tidi}"
    );
    let auto_notifs = ctx.drain_notifications_for_char(perim_pilot.char_id);
    assert!(
        auto_notifs.iter().any(|n| n.event == "DoSimClockRebase"),
        "Auto-TiDi transition must emit DoSimClockRebase to affected pilots"
    );
}

// parity: feat:structures.anchor-restrictions
#[tokio::test]
async fn test_structures_anchor_restrictions_live() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    const JITA: u32 = 30_000_142;
    const KISOGO: u32 = 30_001_407; // Caldari career-agent / starter system
    const PERIMETER: u32 = 30_000_144; // Highsec non-trade-hub
    const RANCER: u32 = 30_002_718; // Lowsec

    let (mut sess, _ship_id) = gm_docked_in_jita(&ctx, 7201, 93_200_001, 20_185).await;
    common::slash_cmd(&ctx, &mut sess, "/giveskill 3340 5").await;
    common::slash_cmd(&ctx, &mut sess, "/giveskill 20525 5").await;
    common::slash_cmd(&ctx, &mut sess, "/giveskill 3394 5").await;
    // Fit 3x Expanded Cargohold II (1319) into low slots (11..=13) while docked in Jita so Keepstar (800,000 m³) fits in cargo
    for flag in [11_i64, 12, 13] {
        let mod_id = common::slash_cmd(&ctx, &mut sess, "/createitem 1319 1")
            .await
            .dict_get("itemID")
            .and_then(PyRep::as_i64)
            .expect("Expanded Cargohold itemID");
        rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(mod_id), PyRep::Integer(_ship_id), PyRep::Integer(flag)])
            .await
            .expect("fit Expanded Cargohold II in station");
    }
    let kw = HashMap::new();

    // Create a brand-new player corporation via corpRegistry.AddCorporation (age = 0s < 7 days)
    ctx.advance_secs(15);
    let _corp_id = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Young Anchor Corp".into()),
            PyRep::String("YANC".into()),
            PyRep::String("Test".into()),
            PyRep::String("https://evomu.local".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");

    let mut pilot = undock_to_system(&ctx, &mut sess, 93_200_001, _ship_id, PERIMETER).await;
    let ast_id = common::stock_ship_bay(&ctx, &mut pilot, 35832, 1, 5).await;
    let ks1_id = common::stock_ship_bay(&ctx, &mut pilot, 35834, 1, 5).await;

    // 1. Corp < 7 days old -> CorporationTooYoungToAnchor
    let err_young = ctx
        .dispatch_rpc(
            &mut pilot.sess,
            "structureDeployment",
            None,
            "Anchor",
            &[PyRep::Integer(ast_id), PyRep::String("Too Young".into())],
            &kw,
            None,
        )
        .await
        .expect_err("Corp < 7 days must be rejected");
    assert_eq!(err_young.code, "CorporationTooYoungToAnchor");

    // Advance virtual clock 7 days + 10s so corp meets minimum 7-day age requirement
    ctx.advance_secs(7 * 86_400 + 10);

    // 2. Trade hub (Jita) -> AnchoringDisallowedInTradeHub
    common::slash_cmd(&ctx, &mut pilot.sess, &format!("/tr {JITA}")).await;
    let err_hub = rpc(&ctx, &mut pilot.sess, "structureDeployment", "Anchor", &[PyRep::Integer(ast_id), PyRep::String("Jita Hub".into())])
        .await
        .expect_err("Jita trade hub must reject anchoring");
    assert_eq!(err_hub.code, "AnchoringDisallowedInTradeHub");

    // 3. Starter / career-agent system (Kisogo) -> AnchoringDisallowedInStarterOrCareerSystem
    common::slash_cmd(&ctx, &mut pilot.sess, &format!("/tr {KISOGO}")).await;
    let err_starter = rpc(&ctx, &mut pilot.sess, "structureDeployment", "Anchor", &[PyRep::Integer(ast_id), PyRep::String("Kisogo Starter".into())])
        .await
        .expect_err("Starter system must reject anchoring");
    assert_eq!(err_starter.code, "AnchoringDisallowedInStarterOrCareerSystem");

    // 4. FLEX Pharolux Cyno Beacon (35840) in lowsec/highsec -> CannotAnchorFlexOutsideSovNull
    common::slash_cmd(&ctx, &mut pilot.sess, &format!("/tr {RANCER}")).await;
    let err_flex = rpc(&ctx, &mut pilot.sess, "structureDeployment", "Anchor", &[PyRep::Integer(35840), PyRep::String("Rancer Cyno".into())])
        .await
        .expect_err("Pharolux outside sov null must fail");
    assert_eq!(err_flex.code, "CannotAnchorFlexOutsideSovNull");

    // 5. Keepstar (35834) in HighSec (Perimeter 0.86) -> CannotAnchorInHighSec
    common::slash_cmd(&ctx, &mut pilot.sess, &format!("/tr {PERIMETER}")).await;
    let err_hs = rpc(&ctx, &mut pilot.sess, "structureDeployment", "Anchor", &[PyRep::Integer(ks1_id), PyRep::String("Perimeter Keepstar".into())])
        .await
        .expect_err("Keepstar in highsec must fail");
    assert_eq!(err_hs.code, "CannotAnchorInHighSec");

    // 6. Anchor first Keepstar (35834) in Rancer -> succeeds
    common::slash_cmd(&ctx, &mut pilot.sess, &format!("/tr {RANCER}")).await;
    let ks1 = rpc(&ctx, &mut pilot.sess, "structureDeployment", "Anchor", &[PyRep::Integer(ks1_id), PyRep::String("Rancer KS 1".into())])
        .await
        .expect("First Keepstar in Rancer succeeds");
    assert!(ks1.dict_get("structureID").and_then(PyRep::as_i64).is_some());

    // Distance < 1000 km from existing structure -> TooCloseToStructureOrStation
    let err_close = rpc(&ctx, &mut pilot.sess, "structureDeployment", "Anchor", &[PyRep::Integer(ast_id), PyRep::String("Too Close Astrahus".into())])
        .await
        .expect_err("Anchoring within 1000 km of existing structure must fail");
    assert_eq!(err_close.code, "TooCloseToStructureOrStation");

    // Second Keepstar in same system -> KeepstarPerSystemLimit
    let ks2_id = common::stock_ship_bay(&ctx, &mut pilot, 35834, 1, 5).await;
    let err_ks2 = rpc(&ctx, &mut pilot.sess, "structureDeployment", "Anchor", &[PyRep::Integer(ks2_id), PyRep::String("Rancer KS 2".into())])
        .await
        .expect_err("Second Keepstar in same solar system must fail");
    assert_eq!(err_ks2.code, "KeepstarPerSystemLimit");

    // 7. Valid Astrahus (35832) in Perimeter with aged corp succeeds in AnchorVulnerable
    common::slash_cmd(&ctx, &mut pilot.sess, &format!("/tr {PERIMETER}")).await;
    let ok_res = rpc(&ctx, &mut pilot.sess, "structureDeployment", "Anchor", &[PyRep::Integer(ast_id), PyRep::String("Valid Perimeter Astrahus".into())])
        .await
        .expect("Valid Astrahus in Perimeter must succeed");
    assert!(ok_res.dict_get("structureID").and_then(PyRep::as_i64).is_some_and(|s| s > 0));
}

// parity: feat:structures.repairshop-service-quotes-and-restoration
#[tokio::test]
async fn test_structures_repairshop_quotes_and_restoration_live() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    const PERIMETER: u32 = 30_000_144;
    let (mut sess, charon_id) = gm_docked_in_jita(&ctx, 7301, 93_300_001, 20_185).await;
    common::slash_cmd(&ctx, &mut sess, "/giveskill 3340 5").await;
    common::slash_cmd(&ctx, &mut sess, "/giveskill 3327 5").await;
    common::slash_cmd(&ctx, &mut sess, "/giveskill 3331 5").await;
    common::slash_cmd(&ctx, &mut sess, "/giveskill 3300 5").await;
    common::slash_cmd(&ctx, &mut sess, "/giveskill 3301 5").await;
    common::slash_cmd(&ctx, &mut sess, "/giveskill 3436 5").await;
    common::slash_cmd(&ctx, &mut sess, "/giveisk 50000000").await;
    ctx.advance_secs(15);
    rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Repairshop Test Corp".into()),
            PyRep::String("REPC".into()),
            PyRep::String("Repair".into()),
            PyRep::String("https://evomu.local".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    ctx.advance_secs(7 * 86_400 + 10);

    let mut pilot = undock_to_system(&ctx, &mut sess, 93_300_001, charon_id, PERIMETER).await;

    // Anchor Astrahus into NoCore first and verify repairshop rejects without Quantum Core
    let ast_id = common::stock_ship_bay(&ctx, &mut pilot, 35832, 1, 5).await;
    let anchor_res = rpc(
        &ctx,
        &mut pilot.sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(ast_id), PyRep::String("Repairshop Astrahus".into())],
    )
    .await
    .expect("Anchor");
    let sid = anchor_res
        .as_i64()
        .or_else(|| anchor_res.dict_get("structureID").and_then(PyRep::as_i64))
        .expect("structureID");
    ctx.advance_secs(87_305);
    let _ = sync_all_structures(&ctx);

    let st_pos = ctx
        .systems
        .get_or_spawn(PERIMETER)
        .get_ball_snapshot(sid)
        .await
        .unwrap()
        .expect("anchored structure ball")
        .1
        .position;
    common::fly_to(&ctx, &mut pilot, st_pos - DVec3::new(15_000.0, 0.0, 0.0)).await;

    rpc(&ctx, &mut pilot.sess, "stationSvc", "Dock", &[PyRep::Integer(sid), PyRep::Integer(charon_id)])
        .await
        .expect("Dock in NoCore structure");
    let no_core_err = rpc(&ctx, &mut pilot.sess, "repairSvc", "GetRepairQuotes", &[PyRep::Integer(charon_id)])
        .await
        .expect_err("Repairshop without Quantum Core must fail");
    assert!(
        matches!(no_core_err.code.as_str(), "QuantumCoreRequired" | "StructureNotOperational"),
        "expected QuantumCoreRequired or StructureNotOperational, got {:?}",
        no_core_err.code
    );

    // Install Quantum Core (56201) -> LowPower operational with repairshop available
    let core_id = common::slash_cmd(&ctx, &mut pilot.sess, "/createitem 56201 1")
        .await
        .dict_get("itemID")
        .and_then(PyRep::as_i64)
        .expect("core ID");
    rpc(&ctx, &mut pilot.sess, "invbroker", "Add", &[PyRep::Integer(core_id), PyRep::Integer(sid), PyRep::Integer(180)])
        .await
        .expect("insert core");
    ctx.advance_secs(905);
    let _ = sync_all_structures(&ctx);

    // Grant Tristan (593) SDE prerequisites + weapon/drone skills and board Tristan inside structure (has 25 m³ drone bay)
    const TRISTAN: u32 = 593;
    let tristan_req_skills: Vec<u32> = ctx
        .sde
        .types
        .get(&TRISTAN)
        .map(|t| {
            [182_u16, 183, 184, 1285, 1289, 1290]
                .into_iter()
                .filter_map(|a| t.default_attributes.get(&a).copied())
                .map(|v| v.round() as u32)
                .filter(|&id| id > 0)
                .collect()
        })
        .unwrap_or_default();
    for skill_id in tristan_req_skills.into_iter().chain([3327, 3328, 3386, 3300, 3301, 3436]) {
        common::slash_cmd(&ctx, &mut pilot.sess, &format!("/giveskill {skill_id} 5")).await;
    }
    let ship_id = common::slash_cmd(&ctx, &mut pilot.sess, &format!("/createitem {TRISTAN} 1"))
        .await
        .dict_get("itemID")
        .and_then(PyRep::as_i64)
        .expect("Tristan itemID");
    rpc(&ctx, &mut pilot.sess, "ship", "Board", &[PyRep::Integer(ship_id)])
        .await
        .expect("board Tristan in structure");
    pilot.ship_id = ship_id;
    pilot.sess.ship_id = Some(ship_id);

    // Fit module (Civilian Gatling Autocannon 3651) and add Warrior I (2486) in drone bay (87)
    let mod_id = common::stock_ship_bay(&ctx, &mut pilot, 3651, 1, i64::from(InventoryFlag::HiSlot0 as u16)).await;
    let drone_id = common::stock_ship_bay(&ctx, &mut pilot, 2486, 1, i64::from(InventoryFlag::DroneBay as u16)).await;

    // Damage ship hull (attr 3), ship armor (attr 266), fitted module (attr 3), and drone (attr 3)
    common::slash_cmd(&ctx, &mut pilot.sess, "/setattr me 3 25.0").await;
    common::slash_cmd(&ctx, &mut pilot.sess, "/setattr me 266 40.0").await;
    common::slash_cmd(&ctx, &mut pilot.sess, &format!("/setattr {mod_id} 3 10.0")).await;
    common::slash_cmd(&ctx, &mut pilot.sess, &format!("/setattr {drone_id} 3 8.0")).await;

    // Configure a 5.0% Repairshop tax profile (setting 13 = 5.0%) so repair quotes charge real ISK tax
    let rep_prof = rpc(
        &ctx,
        &mut pilot.sess,
        "structureProfiles",
        "CreateProfile",
        &[PyRep::String("Repairshop 5% Tax".into())],
    )
    .await
    .expect("CreateProfile repairshop");
    let rep_prof_id = rep_prof
        .dict_get("profileID")
        .and_then(PyRep::as_i64)
        .or_else(|| rep_prof.as_i64())
        .expect("rep_prof_id");
    rpc(
        &ctx,
        &mut pilot.sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(rep_prof_id),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(13),
                PyRep::Real(0.05),
                PyRep::Integer(0),
            ])]),
        ],
    )
    .await
    .expect("SaveProfileSettings repairshop tax");
    rpc(
        &ctx,
        &mut pilot.sess,
        "structureProfiles",
        "ChangeProfiles",
        &[PyRep::List(vec![PyRep::Integer(sid)]), PyRep::Integer(rep_prof_id)],
    )
    .await
    .expect("ChangeProfiles repairshop");
    ctx.advance_secs(10_805);
    let _ = sync_all_structures(&ctx);

    let bal_before = rpc(&ctx, &mut pilot.sess, "account", "GetCashBalance", &[])
        .await
        .expect("GetCashBalance before")
        .as_f64()
        .expect("balance f64");

    let quotes_rep = rpc(
        &ctx,
        &mut pilot.sess,
        "repairSvc",
        "GetRepairQuotes",
        &[PyRep::List(vec![
            PyRep::Integer(ship_id),
            PyRep::Integer(mod_id),
            PyRep::Integer(drone_id),
        ])],
    )
    .await
    .expect("GetRepairQuotes");

    let PyRep::Dict(q_dict) = quotes_rep else { panic!("expected Dict from GetRepairQuotes") };
    let mut quoted_by_item = HashMap::new();
    for (k, v) in q_dict {
        let key_id = k.as_i64().unwrap();
        let PyRep::List(rows) = v else { panic!("expected List of quotes") };
        assert!(!rows.is_empty(), "Repair quote list for {key_id} must not be empty");
        for row in rows {
            let item_id = row.dict_get("itemID").and_then(PyRep::as_i64).unwrap_or(key_id);
            let dmg = row.dict_get("damage").and_then(PyRep::as_f64).unwrap_or(0.0);
            let cost = row.dict_get("totalRepairCost").and_then(PyRep::as_f64).unwrap_or(0.0);
            assert!(dmg > 0.0, "Item {item_id} must report positive damage");
            assert!(cost > 0.0, "Item {item_id} must report positive totalRepairCost");
            quoted_by_item.insert(item_id, cost);
        }
    }
    assert!(
        quoted_by_item.contains_key(&ship_id)
            && quoted_by_item.contains_key(&mod_id)
            && quoted_by_item.contains_key(&drone_id)
    );
    let quoted_total_cost: f64 = quoted_by_item.values().sum();

    // Execute RepairItemsInStructure across ship, module, and drone
    let repair_res = rpc(
        &ctx,
        &mut pilot.sess,
        "repairSvc",
        "RepairItemsInStructure",
        &[PyRep::List(vec![
            PyRep::Integer(ship_id),
            PyRep::Integer(mod_id),
            PyRep::Integer(drone_id),
        ])],
    )
    .await
    .expect("RepairItemsInStructure");
    let paid_cost = repair_res.dict_get("totalCostISK").and_then(PyRep::as_f64).unwrap_or(0.0);
    assert!((paid_cost - quoted_total_cost).abs() < 0.05, "Charged ISK {paid_cost} must match quoted {quoted_total_cost}");

    let bal_after = rpc(&ctx, &mut pilot.sess, "account", "GetCashBalance", &[])
        .await
        .expect("GetCashBalance after")
        .as_f64()
        .expect("balance f64");
    assert!((bal_before - bal_after - paid_cost).abs() < 0.05, "Pilot wallet debited by repair cost");

    // Re-quoting confirms zero remaining damage on ship, module, and drone
    let post_quotes = rpc(
        &ctx,
        &mut pilot.sess,
        "repairSvc",
        "GetRepairQuotes",
        &[PyRep::List(vec![
            PyRep::Integer(ship_id),
            PyRep::Integer(mod_id),
            PyRep::Integer(drone_id),
        ])],
    )
    .await
    .expect("post GetRepairQuotes");
    let PyRep::Dict(post_dict) = post_quotes else { panic!("expected Dict") };
    for (_k, v) in post_dict {
        let PyRep::List(rows) = v else { panic!() };
        assert_eq!(rows[0].dict_get("damage").and_then(PyRep::as_f64), Some(0.0));
        assert_eq!(rows[0].dict_get("totalRepairCost").and_then(PyRep::as_f64), Some(0.0));
    }
}

