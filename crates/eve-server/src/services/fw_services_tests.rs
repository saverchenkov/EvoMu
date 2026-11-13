#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use std::collections::HashMap;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fw_services_helpers_and_rpc_mutations() {
    assert_eq!(fw_opposing_faction(FACTION_CALDARI), Some(FACTION_GALLENTE));
    assert_eq!(fw_opposing_faction(FACTION_GALLENTE), Some(FACTION_CALDARI));
    assert_eq!(fw_opposing_faction(FACTION_MINMATAR), Some(FACTION_AMARR));
    assert_eq!(fw_opposing_faction(FACTION_AMARR), Some(FACTION_MINMATAR));
    assert_eq!(fw_opposing_faction(999), None);

    assert!(is_opposing_militia(FACTION_CALDARI, FACTION_GALLENTE));
    assert!(is_opposing_militia(500010, FACTION_CALDARI));
    assert!(is_opposing_militia(FACTION_GALLENTE, 500010));
    assert!(is_opposing_militia(500011, FACTION_MINMATAR));
    assert!(is_opposing_militia(FACTION_AMARR, 500011));
    assert!(!is_opposing_militia(FACTION_CALDARI, FACTION_AMARR));

    assert_eq!(calculate_militia_rank(0.0), 0);
    assert_eq!(calculate_militia_rank(0.74), 0);
    assert_eq!(calculate_militia_rank(0.75), 1);
    assert_eq!(calculate_militia_rank(1.50), 2);
    assert_eq!(calculate_militia_rank(10.0), 9);

    assert!((calculate_capture_speed_multiplier(0.0, 0.0) - 1.0).abs() < 1e-6);
    assert!((calculate_capture_speed_multiplier(100.0, 0.0) - 1.5).abs() < 1e-6);
    assert_eq!(calculate_capture_speed_multiplier(-200.0, 200.0), 0.5);
    assert_eq!(calculate_capture_speed_multiplier(500.0, 0.0), 2.0);

    assert_eq!(calculate_fw_lp_reward(1000, STATUS_FRONTLINE), 1500);
    assert_eq!(calculate_fw_lp_reward(1000, STATUS_COMMAND), 1000);
    assert_eq!(calculate_fw_lp_reward(1000, STATUS_REARGUARD), 10);
    assert_eq!(calculate_fw_lp_reward(1000, 99), 1000);

    assert_eq!(compute_fw_kill_base_lp(-100.0), 0);
    assert_eq!(compute_fw_kill_base_lp(0.0), 0);
    assert_eq!(compute_fw_kill_base_lp(10_000_000.0), 3250);
    assert_eq!(compute_fw_kill_base_lp(1_000_000_000_000.0), 50_000);

    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    let unknown_svc = "other";
    let unknown_method = "Unknown";
    assert!(handle(&ctx, &mut sess, unknown_svc, "GetHQSystemIDs", &[], &kw)
        .await
        .is_none());
    assert!(handle(&ctx, &mut sess, "fwWarzoneSolarsystem", unknown_method, &[], &kw)
        .await
        .is_none());

    let hqs = handle(&ctx, &mut sess, "fwWarzoneSolarsystem", "GetHQSystemIDs", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let PyRep::Dict(hq_entries) = hqs else {
        panic!("Expected Dict from GetHQSystemIDs, got {hqs:?}");
    };
    assert_eq!(hq_entries.len(), 4);
    assert_eq!(
        hq_entries.iter().find(|(k, _)| *k == PyRep::Integer(FACTION_CALDARI)).map(|(_, v)| v),
        Some(&PyRep::Integer(i64::from(HQ_SYSTEM_ONNAMON)))
    );
    assert_eq!(
        hq_entries.iter().find(|(k, _)| *k == PyRep::Integer(FACTION_MINMATAR)).map(|(_, v)| v),
        Some(&PyRep::Integer(i64::from(HQ_SYSTEM_AMO)))
    );
    assert_eq!(
        hq_entries.iter().find(|(k, _)| *k == PyRep::Integer(FACTION_AMARR)).map(|(_, v)| v),
        Some(&PyRep::Integer(i64::from(HQ_SYSTEM_MEHATOOR)))
    );
    assert_eq!(
        hq_entries.iter().find(|(k, _)| *k == PyRep::Integer(FACTION_GALLENTE)).map(|(_, v)| v),
        Some(&PyRep::Integer(i64::from(HQ_SYSTEM_INTAKI)))
    );

    let wzs = handle(&ctx, &mut sess, "fwWarzoneSolarsystem", "GetAllWarzones", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let PyRep::Dict(wz_entries) = wzs else {
        panic!("Expected Dict from GetAllWarzones, got {wzs:?}");
    };
    assert_eq!(wz_entries.len(), 2);
    assert_eq!(
        wz_entries.iter().find(|(k, _)| *k == PyRep::Integer(1)).map(|(_, v)| v),
        Some(&PyRep::List(vec![PyRep::Integer(FACTION_CALDARI), PyRep::Integer(FACTION_GALLENTE)]))
    );

    let occ_all = handle(
        &ctx,
        &mut sess,
        "fwWarzoneSolarsystem",
        "GetAllWarzonesOccupationStates",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Dict(wz_entries) = occ_all else {
        panic!("expected dict");
    };
    assert_eq!(wz_entries.len(), 2);

    // Pick a known warzone system from snapshot
    let wz_sys_id = ctx.db.with_snapshot(|snap| {
        snap.fw_system_occupations.keys().next().copied().unwrap_or(30045324)
    });

    sess.solar_system_id = None;
    assert!(handle(&ctx, &mut sess, "fwWarzoneSolarsystem", "GetLocalOccupationState", &[], &kw)
        .await
        .unwrap()
        .is_err());
    assert!(handle(&ctx, &mut sess, "fwVictoryPointMgr", "GetLocalVictoryPointState", &[], &kw)
        .await
        .unwrap()
        .is_err());

    sess.solar_system_id = Some(wz_sys_id);
    let local_occ = handle(
        &ctx,
        &mut sess,
        "fwWarzoneSolarsystem",
        "GetLocalOccupationState",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Tuple(local_tup) = local_occ else {
        panic!("expected tuple");
    };
    assert_eq!(local_tup[0], PyRep::Integer(i64::from(wz_sys_id)));

    let local_vp = handle(&ctx, &mut sess, "fwVictoryPointMgr", "GetLocalVictoryPointState", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(local_vp.dict_get("solarSystemID"), Some(&PyRep::Integer(i64::from(wz_sys_id))));

    let all_vp = handle(&ctx, &mut sess, "fwVictoryPointMgr", "GetAllVictoryPointStates", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(all_vp, PyRep::Dict(_)));

    let (adv_f, adv_e) = update_system_advantage(&ctx, wz_sys_id, 25.0, 10.0);
    assert_eq!(adv_f, 25.0);
    assert_eq!(adv_e, 10.0);

    let (vp_val, pct_val) = add_victory_points(&ctx, wz_sys_id, 100);
    assert!(vp_val >= 100);
    assert!(pct_val > 0.0);

    let bf_id = spawn_battlefield_site(&ctx, wz_sys_id, FACTION_CALDARI, "State Battlefield");
    assert!(bf_id >= 700_001);
    let bfs = handle(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetFactionWarfareBattlefieldInstances",
        &[PyRep::Integer(i64::from(wz_sys_id))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!bfs.as_list().unwrap().is_empty());

    // Capsule kill pays 0 LP
    assert_eq!(award_fw_kill_lp_split(&ctx, &[90_000_001], 90_000_002, wz_sys_id, 670, 50_000_000.0), 0);

    // Prune and record stat events
    ctx.db.with_mut_snapshot(|snap| {
        record_fw_stat_event_in_snap(snap, 1_000_000, 90_000_001, Some(FACTION_CALDARI), 2, 50);
        prune_and_fold_fw_stat_events_in_snap(snap, 2_000_000);
        assert_eq!(snap.fw_runtime.historical_char_totals.get(&90_000_001), Some(&(2, 50)));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warzone_occupation_adjacency_state_uses_v24_two_char_codes() {
    // Issue #981: V24.01 `factionwarfare/const.pyj` & `fwwarzone/client/dashboard/const.pyj`
    // define `ADJACENCY_STATES = {'FL', 'SL', 'BL'}`.
    assert_eq!(frontline_status_to_adjacency_state(STATUS_FRONTLINE), "FL");
    assert_eq!(frontline_status_to_adjacency_state(STATUS_COMMAND), "SL");
    assert_eq!(frontline_status_to_adjacency_state(STATUS_REARGUARD), "BL");
    assert_eq!(frontline_status_to_adjacency_state(0), "BL");
    assert_eq!(frontline_status_to_adjacency_state(-7), "BL");
    assert_eq!(frontline_status_to_adjacency_state(99), "BL");

    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let kw = HashMap::new();
    let rear_sys = 30_002_055_u32; // Amo (Rearguard / Backline)
    let cmd_sys = 30_002_086_u32; // Secondline / Command Operations
    let front_sys = 30_002_087_u32; // Frontline

    let mut sess = auth_session(1, cid, 98_000_001, rear_sys);
    let _ = ctx
        .db
        .acquire_online_lock(1, sess.session_id.max(1), Some(cid), true, ctx.now_unix());
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.solar_system_id = rear_sys;
        }
    });

    for method in [
        "GetAllWarzonesOccupationStates",
        "GetAllWarzonesOccupationStatesUncached",
    ] {
        let all_rep = handle(&ctx, &mut sess, "fwWarzoneSolarsystem", method, &[], &kw)
            .await
            .expect("handled")
            .expect("ok");
        let PyRep::Dict(by_wz) = all_rep else {
            panic!("expected Dict from {method}");
        };
        let find_sys = |sid: u32| -> &PyRep {
            by_wz
                .iter()
                .find_map(|(_, v)| match v {
                    PyRep::Dict(m) => m
                        .iter()
                        .find_map(|(k, kv)| (*k == PyRep::Integer(i64::from(sid))).then_some(kv)),
                    _ => None,
                })
                .expect("system must exist in warzone occupation dict")
        };
        assert_eq!(
            find_sys(rear_sys).dict_get("adjacencyState"),
            Some(&PyRep::String("BL".into()))
        );
        assert_eq!(
            find_sys(cmd_sys).dict_get("adjacencyState"),
            Some(&PyRep::String("SL".into()))
        );
        assert_eq!(
            find_sys(front_sys).dict_get("adjacencyState"),
            Some(&PyRep::String("FL".into()))
        );
    }

    for (sys_id, expected_code) in [(rear_sys, "BL"), (cmd_sys, "SL"), (front_sys, "FL")] {
        sess.solar_system_id = Some(sys_id);
        let local_rep = handle(
            &ctx,
            &mut sess,
            "fwWarzoneSolarsystem",
            "GetLocalOccupationState",
            &[],
            &kw,
        )
        .await
        .expect("handled")
        .expect("GetLocalOccupationState ok");
        let PyRep::Tuple(tup) = local_rep else {
            panic!("expected 2-tuple");
        };
        assert_eq!(tup[0], PyRep::Integer(i64::from(sys_id)));
        assert_eq!(
            tup[1].dict_get("adjacencyState"),
            Some(&PyRep::String(expected_code.into()))
        );
    }

    // Negative paths: missing solar_system_id fails with NoSolarSystem; non-warzone returns None;
    // capture_warzone_system rejects non-warzone and <100% contested systems.
    sess.solar_system_id = None;
    let no_sys_err = handle(
        &ctx,
        &mut sess,
        "fwWarzoneSolarsystem",
        "GetLocalOccupationState",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("missing solar_system_id must fail");
    assert_eq!(no_sys_err.code, "NoSolarSystem");

    sess.solar_system_id = Some(30_000_142);
    let non_wz = handle(
        &ctx,
        &mut sess,
        "fwWarzoneSolarsystem",
        "GetLocalOccupationState",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("non-warzone ok");
    assert_eq!(non_wz, PyRep::Tuple(vec![PyRep::Integer(30_000_142), PyRep::None]));
    assert_eq!(
        capture_warzone_system(&ctx, 30_000_142, FACTION_AMARR)
            .expect_err("non-warzone system cannot be captured")
            .code,
        "SystemNotWarzone"
    );
    assert_eq!(
        capture_warzone_system(&ctx, front_sys, FACTION_AMARR)
            .expect_err("uncontested system cannot be captured")
            .code,
        "SystemNotVulnerable"
    );

    // Notification emission (`OnWarzoneOccupationStateUpdated`) on VP gain and full capture
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.solar_system_id = front_sys;
        }
    });
    let _ = ctx.drain_notifications_for_char(cid);
    let threshold = ctx
        .db
        .with_snapshot(|s| s.fw_system_occupations[&front_sys].victory_points_threshold);
    add_victory_points(&ctx, front_sys, threshold);
    let vp_notifs = ctx.drain_notifications_for_char(cid);
    let vp_occ_notif = vp_notifs
        .iter()
        .find(|n| n.event == "OnWarzoneOccupationStateUpdated")
        .expect("add_victory_points must emit OnWarzoneOccupationStateUpdated");
    assert_eq!(
        vp_occ_notif.args[1].dict_get("adjacencyState"),
        Some(&PyRep::String("FL".into()))
    );

    capture_warzone_system(&ctx, front_sys, FACTION_AMARR).expect("capture vulnerable warzone");
    let cap_notifs = ctx.drain_notifications_for_char(cid);
    let cap_occ_notif = cap_notifs
        .iter()
        .find(|n| n.event == "OnWarzoneOccupationStateUpdated")
        .expect("capture_warzone_system must emit OnWarzoneOccupationStateUpdated");
    let cap_adj = cap_occ_notif.args[1]
        .dict_get("adjacencyState")
        .and_then(PyRep::as_str)
        .expect("string adjacencyState on capture notification");
    assert!(matches!(cap_adj, "FL" | "SL" | "BL"));

    // Collection methods (`iteritems`, `itervalues`) also expose 2-char string `adjacencyState`
    sess.solar_system_id = Some(front_sys);
    let iter_vals = crate::services::sov_warzone_dynamic::handle(
        &ctx,
        &mut sess,
        "fwWarzoneSolarsystem",
        "itervalues",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("itervalues ok");
    let PyRep::List(vals) = iter_vals else {
        panic!("expected List from itervalues");
    };
    assert!(!vals.is_empty());
    for entry in &vals {
        let code = entry
            .dict_get("adjacencyState")
            .and_then(PyRep::as_str)
            .expect("itervalues entry must carry string adjacencyState");
        assert!(matches!(code, "FL" | "SL" | "BL"));
    }
}

#[cfg(test)]
#[path = "fw_services_cov_edges_tests.rs"]
mod fw_services_cov_edges_tests;
