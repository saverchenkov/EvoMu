//! Live integration tests for server-ops TiDi, cluster worker-node balancing, and industry pause/batch completion.
//!
//! Features covered:
//! - `feat:serverops.node-balancing`
//! - `feat:serverops.time-dilation`
//! - `feat:lifecycle.industry-pause-and-many`

#![forbid(unsafe_code)]

use crate::common;
use crate::industry_live_support::{
    board_loaded_freighter, bom, buy, deploy_online_upwell, fuel_cargo, fund_pilots, install,
    items_at, login, move_item, rpc, split_off, train_skills, upwell_cargo, PERIMETER_SYS,
    PILOT_1, PILOT_2,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::serverops_cluster::{
    clear_manual_system_tidi_override, compute_tidi_from_node_load, get_system_tidi_factor,
    get_system_worker_node, get_worker_node_load, is_auto_cluster_tidi_enabled,
    rebalance_overloaded_worker_nodes, reinforce_solar_system_for_fleet_fight,
    set_auto_cluster_tidi_enabled, set_cluster_node_load_threshold, set_system_tidi_factor,
};
use eve_server::world_seeder::seed_full_market_catalog;

const JITA: u32 = 30_000_142;
const PERIMETER: u32 = 30_000_144;
const PILOT_A: i64 = 92_100_001;
const PILOT_B: i64 = 92_100_002;
const VEXOR: u32 = 626;

const RIFTER_BP: u32 = 691;
const RAITARU: u32 = 35_825;
const MFG_PLANT: u32 = 35_878;
const RESEARCH_LAB: u32 = 35_891;

fn drain_events(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        if !out.iter().any(|(e, a)| e == &n.event && a == &n.args) {
            out.push((n.event, n.args));
        }
    }
    out
}

async fn unload(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, hangar: i64) {
    let (owner, ship) = (sess.character_id.unwrap(), sess.ship_id.unwrap());
    for (item, _) in items_at(ctx, owner, ship, type_id) {
        move_item(ctx, sess, item, hangar, 4).await;
    }
}

// parity: feat:serverops.node-balancing, feat:serverops.time-dilation
#[tokio::test]
async fn test_live_serverops_time_dilation_and_node_balancing() {
    eve_testkit::attest(&[
        "feat:serverops.node-balancing",
        "feat:serverops.time-dilation",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    // Spawn Pilot A in space in Jita and Pilot B in Perimeter
    let mut p_jita = common::gm_pilot_in_space(&ctx, 101, PILOT_A, VEXOR, JITA).await;
    let mut p_perim = common::gm_pilot_in_space(&ctx, 102, PILOT_B, VEXOR, PERIMETER).await;

    // Drain initial session-connect notifications
    let _ = drain_events(&ctx, &mut p_jita.sess, PILOT_A);
    let _ = drain_events(&ctx, &mut p_perim.sess, PILOT_B);

    // 1. Time Dilation Scaling (`feat:serverops.time-dilation`)
    // Baseline TiDi in Jita is 1.0 (no dilation)
    assert_eq!(get_system_tidi_factor(&ctx, JITA), 1.0);

    // Apply TiDi 0.50 to Jita via GM slash command
    let res = common::slash_cmd(&ctx, &mut p_jita.sess, "/tidi 0.50").await;
    assert_eq!(res, PyRep::Real(0.50));
    assert_eq!(get_system_tidi_factor(&ctx, JITA), 0.50);

    // Verify Jita pilot receives DoSimClockRebase with valid timestamp tuple
    let jita_rebase: Vec<Vec<PyRep>> = drain_events(&ctx, &mut p_jita.sess, PILOT_A)
        .into_iter()
        .filter(|(e, _)| e == "DoSimClockRebase")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(jita_rebase.len(), 1, "Pilot in Jita must receive DoSimClockRebase");
    match &jita_rebase[0][0] {
        PyRep::Tuple(times) => {
            assert_eq!(times.len(), 2, "DoSimClockRebase tuple must have 2 elements");
            let old_t = times[0].as_i64().expect("oldSimTime");
            let new_t = times[1].as_i64().expect("newSimTime");
            assert!(old_t < new_t, "sim clock must advance forward: {old_t} < {new_t}");
        }
        other => panic!("expected 2-tuple rebaseTimes, got {other:?}"),
    }

    // Pilot in Perimeter must NOT receive Jita's DoSimClockRebase
    assert!(
        !drain_events(&ctx, &mut p_perim.sess, PILOT_B)
            .iter()
            .any(|(e, _)| e == "DoSimClockRebase"),
        "Perimeter pilot must not receive Jita's DoSimClockRebase"
    );

    // Verify TiDi clamping: values below 0.10 clamp to 0.10 floor
    let clamped = set_system_tidi_factor(&ctx, JITA, 0.05);
    assert_eq!(clamped, 0.10);
    assert_eq!(get_system_tidi_factor(&ctx, JITA), 0.10);

    // Values above 1.0 clamp to 1.0
    let clamped_max = set_system_tidi_factor(&ctx, JITA, 2.50);
    assert_eq!(clamped_max, 1.0);
    assert_eq!(get_system_tidi_factor(&ctx, JITA), 1.0);

    // Configure automatic load-induced TiDi settings
    set_auto_cluster_tidi_enabled(&ctx, true);
    assert!(is_auto_cluster_tidi_enabled(&ctx));
    set_cluster_node_load_threshold(&ctx, 200);
    assert_eq!(compute_tidi_from_node_load(100, 200), 1.0);
    assert_eq!(compute_tidi_from_node_load(400, 200), 0.50);

    // Reset manual TiDi override
    clear_manual_system_tidi_override(&ctx, JITA);

    // 2. Worker-Node Load Balancing & Reinforcement (`feat:serverops.node-balancing`)
    // Initial worker node allocation is determined by constellation mapping
    let jita_initial_node = get_system_worker_node(&ctx, JITA);
    let perim_initial_node = get_system_worker_node(&ctx, PERIMETER);
    assert_eq!(
        jita_initial_node, perim_initial_node,
        "Systems in same constellation initially share general worker node"
    );

    // Reinforce Jita for a heavy fleet fight onto a dedicated cluster node (>= 100)
    let reinforced_node = reinforce_solar_system_for_fleet_fight(&ctx, JITA);
    assert!(reinforced_node >= 100, "Reinforced fleet-fight node id must be >= 100");
    assert_eq!(get_system_worker_node(&ctx, JITA), reinforced_node);

    // Sibling Perimeter system must remain isolated from the reinforced node
    assert_ne!(
        get_system_worker_node(&ctx, PERIMETER),
        reinforced_node,
        "Neighboring systems must be isolated from reinforced node"
    );

    // Query cluster node address via machoNet RPC on Perimeter pilot session
    let port = 26_000 + reinforced_node;
    let resolved_node = rpc(
        &ctx,
        &mut p_perim.sess,
        "machoNet",
        "GetNodeFromAddress",
        &[PyRep::String(format!("127.0.0.1:{port}"))],
    )
    .await
    .expect("machoNet.GetNodeFromAddress");
    assert_eq!(resolved_node, PyRep::Integer(reinforced_node as i64));

    // Verify worker node load tracking
    let jita_load = get_worker_node_load(&ctx, reinforced_node);
    assert!(jita_load >= 1, "Reinforced node must reflect active pilot entity load");

    // Run automatic worker-node load rebalancing across cluster
    let _ = rebalance_overloaded_worker_nodes(&ctx);
}

// parity: feat:lifecycle.industry-pause-and-many
#[tokio::test]
async fn test_live_industry_pause_resume_and_batch_complete() {
    eve_testkit::attest(&["feat:lifecycle.industry-pause-and-many"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);

    fund_pilots(&ctx, &[(PILOT_1, 80_000_000_000.0), (PILOT_2, 2_000_000_000.0)]).await;
    let mut s1 = login(&ctx, 8815, "demo", PILOT_1).await;
    let mut s2 = login(&ctx, 8816, "other", PILOT_2).await;
    train_skills(
        &ctx,
        &mut s1,
        &[3363, 3380, 3387, 3402, 3406, 20_185, RAITARU, MFG_PLANT, RESEARCH_LAB],
        true,
    )
    .await;
    train_skills(&ctx, &mut s2, &[3380, 3403, 3409], false).await;

    // Create corporation and seed funding
    rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("PauseMany Corp".into()), PyRep::String("PMA".into())],
    )
    .await
    .expect("AddCorporation");
    let corp_id = s1.corporation_id.unwrap();
    rpc(
        &ctx,
        &mut s1,
        "account",
        "GiveCash",
        &[PyRep::Integer(corp_id), PyRep::Real(500_000_000.0), PyRep::String("seed".into())],
    )
    .await
    .expect("GiveCash corp");
    ctx.advance_virtual_clock(8 * 86_400);

    // Buy blueprints and materials from Jita market, load freighter, and deploy Upwell Raitaru
    buy(&ctx, &mut s1, RIFTER_BP, 2).await;
    let mut cargo = upwell_cargo(&ctx, RAITARU, &[MFG_PLANT, RESEARCH_LAB]);
    cargo.extend(fuel_cargo());
    cargo.push((RIFTER_BP, 3));
    cargo.extend(bom(&ctx, RIFTER_BP, 6));

    let freighter = board_loaded_freighter(&ctx, &mut s1, &cargo).await;
    let (raitaru_id, fitted) = deploy_online_upwell(
        &ctx,
        &mut s1,
        PERIMETER_SYS,
        RAITARU,
        &[MFG_PLANT, RESEARCH_LAB],
        2_500,
    )
    .await;
    let mfg_mod_id = fitted[0];

    // Unload blueprint copies and manufacturing bill of materials into Raitaru hangar
    let mut bpos = Vec::new();
    for _ in 0..2 {
        let stack = items_at(&ctx, PILOT_1, freighter, RIFTER_BP)[0].0;
        bpos.push(split_off(&ctx, &mut s1, stack, raitaru_id, 4, 1).await);
    }
    for (tid, _) in bom(&ctx, RIFTER_BP, 1) {
        unload(&ctx, &mut s1, tid, raitaru_id).await;
    }

    // Install two independent manufacturing jobs via live industryManager.InstallJob RPC
    let ja = install(&ctx, &mut s1, bpos[0], raitaru_id, 1, 1, &[]).await.expect("job A");
    let jb = install(&ctx, &mut s1, bpos[1], raitaru_id, 1, 1, &[]).await.expect("job B");
    let (ja_id, jb_id) = (
        ja.dict_get("jobID").and_then(|v| v.as_i64()).expect("jobID A"),
        jb.dict_get("jobID").and_then(|v| v.as_i64()).expect("jobID B"),
    );
    let ja_dur = ja.dict_get("durationSeconds").and_then(|v| v.as_i64()).expect("duration A") as u64;

    // Both jobs active initially
    let ja_info = rpc(&ctx, &mut s1, "industryManager", "GetJob", &[PyRep::Integer(ja_id)]).await.unwrap();
    assert_eq!(ja_info.dict_get("status").and_then(|v| v.as_str()), Some("Active"));

    // Take the Manufacturing Plant service module offline -> jobs pause at offline moment
    let t_offline = ctx.now_unix();
    rpc(
        &ctx,
        &mut s1,
        "dogmaIM",
        "TakeModuleOffline",
        &[PyRep::Integer(raitaru_id), PyRep::Integer(mfg_mod_id)],
    )
    .await
    .expect("TakeModuleOffline");
    ctx.advance_virtual_clock(1_800);

    // Verify both jobs transition to Paused status with pauseUnix recorded
    let ja_paused = rpc(&ctx, &mut s1, "industryManager", "GetJob", &[PyRep::Integer(ja_id)]).await.unwrap();
    let jb_paused = rpc(&ctx, &mut s1, "industryManager", "GetJob", &[PyRep::Integer(jb_id)]).await.unwrap();
    assert_eq!(ja_paused.dict_get("status").and_then(|v| v.as_str()), Some("Paused"));
    assert_eq!(jb_paused.dict_get("status").and_then(|v| v.as_str()), Some("Paused"));
    assert_eq!(ja_paused.dict_get("pauseUnix").and_then(|v| v.as_i64()), Some(t_offline as i64));

    // Bring Manufacturing Plant back online -> jobs resume as Active with extended duration
    rpc(
        &ctx,
        &mut s1,
        "dogmaIM",
        "SetModuleOnline",
        &[PyRep::Integer(raitaru_id), PyRep::Integer(mfg_mod_id)],
    )
    .await
    .expect("SetModuleOnline");

    let ja_resumed = rpc(&ctx, &mut s1, "industryManager", "GetJob", &[PyRep::Integer(ja_id)]).await.unwrap();
    assert_eq!(ja_resumed.dict_get("status").and_then(|v| v.as_str()), Some("Active"));
    assert_eq!(
        ja_resumed.dict_get("durationSeconds").and_then(|v| v.as_i64()),
        Some((ja_dur + 1_800) as i64),
        "Job duration must be extended by the 1800 seconds pause duration"
    );

    // Attempting to complete before extended duration fails with JobNotYetComplete
    let premature = rpc(&ctx, &mut s1, "industryManager", "CompleteJob", &[PyRep::Integer(ja_id)])
        .await
        .expect_err("Premature completion must fail");
    assert_eq!(premature.code, "JobNotYetComplete");

    // Advance clock past the extended duration
    ctx.advance_virtual_clock(ja_dur + 1_800 + 100);

    // CompleteManyJobs delivers all ready jobs in the batch
    let batch = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "CompleteManyJobs",
        &[PyRep::List(vec![PyRep::Integer(ja_id), PyRep::Integer(jb_id)])],
    )
    .await
    .expect("CompleteManyJobs");
    let PyRep::List(completed) = batch else { panic!("Expected list from CompleteManyJobs") };
    assert_eq!(completed.len(), 2, "CompleteManyJobs must deliver both ready jobs");
}
