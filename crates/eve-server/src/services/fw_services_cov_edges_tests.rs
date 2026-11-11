//! Edge-case, boundary, and error-path tests for `fw_services`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_db::{CorporationRecord, FwSystemOccupationRecord};
use eve_machonet::SessionState;
use std::collections::HashMap;

fn kv_field<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    if let PyRep::KeyVal(pairs) = rep {
        pairs.iter().find(|(k, _)| k.as_str() == key).map(|(_, v)| v)
    } else {
        None
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fw_vp_capture_downtime_missing_system_states_and_clamps() {
    let ctx = test_ctx();
    let wz_cal_sys = 30_045_307u32;
    let wz_amarr_sys = 30_002_978u32;
    let non_wz_sys = 39_999_999u32;

    assert_eq!(add_victory_points(&ctx, non_wz_sys, 50), (0, 0.0));
    assert_eq!(
        capture_warzone_system(&ctx, non_wz_sys, FACTION_GALLENTE)
            .unwrap_err()
            .code,
        "SystemNotWarzone"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.remove(&wz_cal_sys);
        snap.system_states.remove(&wz_amarr_sys);
        snap.fw_system_occupations.insert(
            wz_cal_sys,
            FwSystemOccupationRecord {
                solar_system_id: wz_cal_sys,
                owner_faction_id: FACTION_CALDARI,
                occupier_faction_id: FACTION_CALDARI,
                victory_points: 50,
                victory_points_threshold: 0, // tests div-by-zero max(1.0) guard
            },
        );
        snap.fw_system_occupations.insert(
            wz_amarr_sys,
            FwSystemOccupationRecord {
                solar_system_id: wz_amarr_sys,
                owner_faction_id: FACTION_AMARR,
                occupier_faction_id: FACTION_AMARR,
                victory_points: 10,
                victory_points_threshold: 100,
            },
        );
    });

    assert_eq!(add_victory_points(&ctx, wz_cal_sys, -500), (0, 0.0));
    let (vp, pct) = add_victory_points(&ctx, wz_cal_sys, 2);
    assert_eq!(vp, 2);
    assert!((pct - 100.0).abs() < 1e-6);

    assert_eq!(
        capture_warzone_system(&ctx, wz_amarr_sys, FACTION_MINMATAR)
            .unwrap_err()
            .code,
        "SystemNotVulnerable"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.get_mut(&wz_cal_sys).unwrap().victory_points_threshold = 100;
        snap.fw_runtime.pending_downtime_captures.insert(non_wz_sys, FACTION_AMARR);
        snap.fw_runtime.pending_downtime_captures.insert(wz_cal_sys, FACTION_GALLENTE);
        snap.insurgency_runtime.surrender_ihub_vulnerable_unix.insert(wz_cal_sys, 1_700_000_000);
    });
    let captured = process_pending_fw_captures_on_downtime(&ctx);
    assert_eq!(captured, 1);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.fw_runtime.pending_downtime_captures.contains_key(&wz_cal_sys));
        assert!(!snap.fw_runtime.pending_downtime_captures.contains_key(&non_wz_sys));
        assert!(!snap.insurgency_runtime.surrender_ihub_vulnerable_unix.contains_key(&wz_cal_sys));
        assert_eq!(snap.fw_system_occupations[&wz_cal_sys].occupier_faction_id, FACTION_GALLENTE);
    });

    let mut sess = auth_session(1, 90_000_001, 1_000_009, 30_000_142);
    let states_res = handle(
        &ctx,
        &mut sess,
        "fwWarzoneSolarsystem",
        "GetAllWarzonesOccupationStates",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("ok");
    let PyRep::Dict(wz_entries) = states_res else {
        panic!("expected Dict");
    };
    let wz1 = wz_entries
        .iter()
        .find(|(k, _)| k == &PyRep::Integer(1))
        .expect("wz1")
        .1
        .clone();
    let wz2 = wz_entries
        .iter()
        .find(|(k, _)| k == &PyRep::Integer(2))
        .expect("wz2")
        .1
        .clone();
    let PyRep::Dict(wz1_sys) = wz1 else { panic!("wz1 dict"); };
    let PyRep::Dict(wz2_sys) = wz2 else { panic!("wz2 dict"); };
    assert!(wz1_sys.iter().any(|(k, _)| k == &PyRep::Integer(i64::from(wz_cal_sys))));
    assert!(!wz1_sys.iter().any(|(k, _)| k == &PyRep::Integer(i64::from(wz_amarr_sys))));
    assert!(wz2_sys.iter().any(|(k, _)| k == &PyRep::Integer(i64::from(wz_amarr_sys))));
    assert!(!wz2_sys.iter().any(|(k, _)| k == &PyRep::Integer(i64::from(wz_cal_sys))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fw_battlefield_filtering_kill_lp_guards_and_stat_event_edge_cases() {
    let ctx = test_ctx();
    let sys_a = 30_002_978u32;
    let sys_b = 30_002_979u32;

    let id_a = spawn_battlefield_site(&ctx, sys_a, FACTION_CALDARI, "Site A");
    let id_a_done = spawn_battlefield_site(&ctx, sys_a, FACTION_CALDARI, "Site A Done");
    let id_b = spawn_battlefield_site(&ctx, sys_b, FACTION_GALLENTE, "Site B");
    with_fw_runtime(&ctx, |rt| {
        if let Some(b) = rt.battlefield_instances.iter_mut().find(|b| b.instance_id == id_a_done) {
            b.completed = true;
        }
    });

    let mut sess = auth_session(1, 90_000_001, 1_000_009, sys_a);
    let res_a = handle(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetFactionWarfareBattlefieldInstances",
        &[PyRep::Integer(i64::from(sys_a))],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::List(list_a) = res_a else { panic!("expected List"); };
    assert_eq!(list_a.len(), 1);
    assert_eq!(kv_field(&list_a[0], "instanceID").and_then(|v| v.as_i64()), Some(id_a));

    let mut sess_b = SessionState {
        solar_system_id: Some(sys_b),
        ..auth_session(1, 90_000_001, 1_000_009, sys_b)
    };
    let res_b = handle(
        &ctx,
        &mut sess_b,
        "dungeonInstanceCacheMgr",
        "GetFactionWarfareBattlefieldInstances",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::List(list_b) = res_b else { panic!("expected List"); };
    assert_eq!(list_b.len(), 1);
    assert_eq!(kv_field(&list_b[0], "instanceID").and_then(|v| v.as_i64()), Some(id_b));

    sess_b.solar_system_id = None;
    let res_all = handle(
        &ctx,
        &mut sess_b,
        "dungeonInstanceCacheMgr",
        "GetFactionWarfareBattlefieldInstances",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::List(list_all) = res_all else { panic!("expected List"); };
    assert!(list_all.len() >= 2);
    assert!(!list_all.iter().any(|x| kv_field(x, "instanceID").and_then(|v| v.as_i64()) == Some(id_a_done)));

    let unenlisted_char = 95_700_001i64;
    let cal_killer = 95_700_002i64;
    let gal_victim = 95_700_003i64;
    let ally_corp_id = 98_700_001i64;
    let alliance_id = 99_700_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.insert(
            sys_a,
            FwSystemOccupationRecord {
                solar_system_id: sys_a,
                owner_faction_id: FACTION_CALDARI,
                occupier_faction_id: FACTION_CALDARI,
                victory_points: 0,
                victory_points_threshold: 1000,
            },
        );
        snap.corporations.insert(
            ally_corp_id,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = ally_corp_id; __corp.name = "FwCorp".into(); __corp.alliance_id = Some(alliance_id); __corp.ensure_wallet_divisions(); __corp },
        );
        for (cid, co, fac) in [
            (unenlisted_char, 1_000_009, None),
            (cal_killer, ally_corp_id, Some(FACTION_CALDARI)),
            (gal_victim, 1_000_009, Some(FACTION_GALLENTE)),
        ] {
            let mut ch = snap.characters.values().next().cloned().unwrap();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.corporation_id = co;
            ch.war_faction_id = fac;
            ch.alliance_id = None;
            snap.characters.insert(cid, ch);
        }
    });

    assert_eq!(award_fw_kill_lp(&ctx, cal_killer, sys_a, 0.0), 0);
    assert_eq!(award_fw_kill_lp(&ctx, cal_killer, sys_a, -100.0), 0);

    // Split guards: unenlisted victim -> 0, victim == attacker / unenlisted attacker -> 0, zero ISK -> 0
    assert_eq!(award_fw_kill_lp_split(&ctx, &[cal_killer], unenlisted_char, sys_a, 587, 10_000_000.0), 0);
    assert_eq!(award_fw_kill_lp_split(&ctx, &[gal_victim, unenlisted_char], gal_victim, sys_a, 587, 10_000_000.0), 0);
    assert_eq!(award_fw_kill_lp_split(&ctx, &[cal_killer], gal_victim, sys_a, 587, 0.0), 0);

    ctx.db.with_mut_snapshot(|snap| {
        let before = snap.fw_runtime.stat_events.len();
        record_fw_stat_event_in_snap(snap, 1_700_000_000, cal_killer, Some(FACTION_CALDARI), 0, 0);
        assert_eq!(snap.fw_runtime.stat_events.len(), before);

        record_fw_stat_event_in_snap(snap, 1_700_000_001, cal_killer, Some(FACTION_CALDARI), 1, 10);
        assert_eq!(snap.fw_runtime.stat_events.len(), before + 1);
        let ev = snap.fw_runtime.stat_events.last().unwrap();
        assert_eq!(ev.corporation_id, ally_corp_id);
        assert_eq!(ev.alliance_id, Some(alliance_id));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mutant_kill_fw_hq_system_ids_and_all_warzones() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 1_000_009, 30_000_142);
    let kw = HashMap::new();

    // GetAllWarzones returns zone 1 ([Caldari, Gallente]) and zone 2 ([Amarr, Minmatar])
    let PyRep::Dict(wz_entries) = handle(&ctx, &mut sess, "fwWarzoneSolarsystem", "GetAllWarzones", &[], &kw)
        .await
        .expect("handled")
        .expect("ok")
    else {
        panic!("expected Dict from GetAllWarzones");
    };
    assert_eq!(wz_entries.len(), 2);
    assert!(wz_entries.contains(&(
        PyRep::Integer(1),
        PyRep::List(vec![PyRep::Integer(FACTION_CALDARI), PyRep::Integer(FACTION_GALLENTE)]),
    )));
    assert!(wz_entries.contains(&(
        PyRep::Integer(2),
        PyRep::List(vec![PyRep::Integer(FACTION_AMARR), PyRep::Integer(FACTION_MINMATAR)]),
    )));

    // Empty-SDE context guarantees all 4 HQ system mappings are returned unconditionally
    let mut custom_sde = (*ctx.sde).clone();
    custom_sde.solar_systems.clear();
    let empty_sde_ctx = ServerContext::with_sde_and_repository(
        std::sync::Arc::new(custom_sde),
        std::sync::Arc::new(eve_db::WorldRepository::new_empty()),
    );
    let PyRep::Dict(hqs_empty) = handle(&empty_sde_ctx, &mut sess, "fwWarzoneSolarsystem", "GetHQSystemIDs", &[], &kw)
        .await
        .expect("handled")
        .expect("ok")
    else {
        panic!("expected Dict from GetHQSystemIDs");
    };
    assert_eq!(hqs_empty.len(), 4);
    for (fid, sid) in [
        (FACTION_CALDARI, HQ_SYSTEM_ONNAMON),
        (FACTION_MINMATAR, HQ_SYSTEM_AMO),
        (FACTION_AMARR, HQ_SYSTEM_MEHATOOR),
        (FACTION_GALLENTE, HQ_SYSTEM_INTAKI),
    ] {
        assert!(hqs_empty.contains(&(PyRep::Integer(fid), PyRep::Integer(i64::from(sid)))));
    }

    // Populated SDE context also returns a valid Dict matching SDE systems
    let hqs_real = handle(&ctx, &mut sess, "fwWarzoneSolarsystem", "GetHQSystemIDs", &[], &kw)
        .await
        .expect("handled")
        .expect("ok");
    assert!(matches!(hqs_real, PyRep::Dict(_)));
}
