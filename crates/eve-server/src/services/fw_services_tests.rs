#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use std::collections::HashMap;

#[tokio::test]
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
    let hq_list = hqs.as_list().unwrap();
    assert_eq!(hq_list.len(), 4);
    assert!(hq_list.contains(&PyRep::Integer(i64::from(HQ_SYSTEM_MEHATOOR))));

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
