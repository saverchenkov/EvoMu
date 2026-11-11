//! Extra branch-coverage and mutation-killing unit tests for `sov_warzone_dynamic`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::sov_warzone_dynamic::{
    collect_all_warzone_systems, constellation_systems_for_hub, decay_sov_dev_indices_in_snap,
    handle, industrial_index_for_mined, init_hub_campaign_scores_in_snap,
    military_index_for_kills, refresh_system_adm_vulnerability_windows_in_snap,
    regenerate_unattended_command_nodes, sov_upgrade_site_specs, spawn_command_nodes_for_hub,
};
use eve_db::{FwSystemOccupationRecord, IskCents, SystemDynamicState, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

fn unauth_session() -> SessionState {
    SessionState::new(999_999)
}

async fn call_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let kw = HashMap::new();
    handle(ctx, sess, svc, method, args, &kw).await
}

#[test]
fn index_thresholds_and_upgrade_site_spec_boundaries() {
    // Exact lower and upper boundaries for military_index_for_kills
    for (kills, expected) in [
        (0, 0),
        (4, 0),
        (5, 1),
        (14, 1),
        (15, 2),
        (34, 2),
        (35, 3),
        (69, 3),
        (70, 4),
        (119, 4),
        (120, 5),
        (500, 5),
    ] {
        assert_eq!(military_index_for_kills(kills), expected, "kills={kills}");
    }

    // Exact lower and upper boundaries for industrial_index_for_mined
    for (mined, expected) in [
        (0, 0),
        (999, 0),
        (1_000, 1),
        (4_999, 1),
        (5_000, 2),
        (14_999, 2),
        (15_000, 3),
        (34_999, 3),
        (35_000, 4),
        (74_999, 4),
        (75_000, 5),
        (200_000, 5),
    ] {
        assert_eq!(industrial_index_for_mined(mined), expected, "mined={mined}");
    }

    // Threat Detection security_status boundary at -0.45 vs -0.44 and numeric tier "2" / "3"
    let deep_null = sov_upgrade_site_specs("Major Threat Detection Array 3", -0.45);
    assert_eq!(deep_null.len(), 5);
    assert_eq!(deep_null[0].1, "Forsaken Hub");
    assert_eq!(deep_null[3].1, "Sanctum");
    assert_eq!(deep_null[0].4, 5);

    let shallow_null = sov_upgrade_site_specs("Threat Detection Array 2", -0.44);
    assert_eq!(shallow_null.len(), 2);
    assert_eq!(shallow_null[0].1, "Forsaken Refuge");
    assert_eq!(shallow_null[1].1, "Drone Den");
    assert_eq!(shallow_null[0].4, 4);

    // Exploration Detector alternating Relic and Data sites
    let exp_sites = sov_upgrade_site_specs("Exploration Detector III", -0.5);
    assert_eq!(exp_sites.len(), 4);
    assert_eq!(exp_sites[0].0, "Relic");
    assert_eq!(exp_sites[1].0, "Data");
    assert_eq!(exp_sites[2].0, "Relic");
    assert_eq!(exp_sites[3].0, "Data");
}

#[tokio::test]
async fn unauth_errors_dbs_fallback_ess_unlocked_and_get_branches() {
    let ctx = test_ctx();
    let mut unauth = unauth_session();

    for (svc, method) in [
        ("dynamicResourceCacheMgr", "GetDBSMapData"),
        ("dynamicResourceCacheMgr", "GetDynamicResourceSettings"),
        ("dynamicResourceCacheMgr", "GetESSAgencyData"),
        ("dynamicResourceCacheMgr", "GetESSReserveBankKeys"),
        ("dynamicResourceCacheMgr", "GetESSSystemDetails"),
        ("dynamicResourceCacheMgr", "get"),
        ("dynamicResourceCacheMgr", "iteritems"),
        ("sovMgr", "remove_from_cache"),
        ("fwWarzoneSolarsystem", "iteritems"),
        ("fwWarzoneSolarsystem", "itervalues"),
        ("fwWarzoneSolarsystem", "keys"),
    ] {
        assert!(
            call_rpc(&ctx, &mut unauth, svc, method, &[])
                .await
                .unwrap()
                .is_err(),
            "{svc}::{method} must require character"
        );
    }

    let null_sys = 30_004_759_u32;
    let mut sess = auth_session(1, 90_000_001, 98_000_001, null_sys);

    // When system_states has no nullsec entries, GetDBSMapData falls back to session's nullsec solar_system_id (1.35)
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.clear();
    });
    let dbs_fallback = call_rpc(&ctx, &mut sess, "dynamicResourceCacheMgr", "GetDBSMapData", &[])
        .await
        .unwrap()
        .unwrap();
    if let PyRep::Dict(entries) = dbs_fallback {
        assert_eq!(
            entries,
            vec![(PyRep::Integer(null_sys as i64), PyRep::Real(1.35))]
        );
    } else {
        panic!("expected Dict from GetDBSMapData");
    }

    // GetESSSystemDetails with no args and session.solar_system_id == None -> NoSolarSystem
    sess.solar_system_id = None;
    let no_sys_err = call_rpc(&ctx, &mut sess, "dynamicResourceCacheMgr", "GetESSSystemDetails", &[])
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(no_sys_err.code, "NoSolarSystem");
    sess.solar_system_id = Some(null_sys);

    // Seed null_sys with active ESS link and unlocked reserve bank in combat runtime
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            null_sys,
            SystemDynamicState {
                solar_system_id: null_sys,
                dbs_multiplier: 1.65,
                ess_main_bank: IskCents::from_isk(25_000_000),
                ess_reserve_bank: IskCents::from_isk(80_000_000),
                ess_linked_character_id: Some(90_000_001),
                ess_link_started_tick: Some(42),
                ..Default::default()
            },
        );
    });
    ctx.combat_ephemeral
        .write()
        .unwrap()
        .ess_reserve_unlocked_until
        .insert(null_sys, now + 125);

    let agency = call_rpc(&ctx, &mut sess, "dynamicResourceCacheMgr", "GetESSAgencyData", &[])
        .await
        .unwrap()
        .unwrap();
    if let PyRep::Dict(entries) = agency {
        let (_, sys_dict) = entries
            .iter()
            .find(|(k, _)| *k == PyRep::Integer(null_sys as i64))
            .expect("null_sys entry");
        assert_eq!(sys_dict.dict_get("vaultOpen"), Some(&PyRep::Bool(true)));
        assert_eq!(sys_dict.dict_get("isLinked"), Some(&PyRep::Bool(true)));
        assert_eq!(
            sys_dict.dict_get("linkedCharacterID"),
            Some(&PyRep::Integer(90_000_001))
        );
    } else {
        panic!("expected Dict from GetESSAgencyData");
    }

    let details = call_rpc(&ctx, &mut sess, "dynamicResourceCacheMgr", "GetESSSystemDetails", &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(details.dict_get("activeLinks"), Some(&PyRep::Integer(1)));
    assert_eq!(details.dict_get("reserveTimeWindowState"), Some(&PyRep::Integer(2)));
    assert_eq!(details.dict_get("isUnlocked"), Some(&PyRep::Bool(true)));
    assert_eq!(details.dict_get("totalPulses"), Some(&PyRep::Integer(3)));
    assert_eq!(details.dict_get("linkStartedTick"), Some(&PyRep::Integer(42)));

    // dynamicResourceCacheMgr.get fallback branches: empty args, unknown sys_id with default, unknown string key, non-int/str key
    assert_eq!(
        call_rpc(&ctx, &mut sess, "dynamicResourceCacheMgr", "get", &[])
            .await
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    let def_marker = PyRep::Real(0.99);
    assert_eq!(
        call_rpc(
            &ctx,
            &mut sess,
            "dynamicResourceCacheMgr",
            "get",
            &[PyRep::Integer(30_000_142), def_marker.clone()],
        )
        .await
        .unwrap()
        .unwrap(),
        def_marker
    );
    assert_eq!(
        call_rpc(
            &ctx,
            &mut sess,
            "dynamicResourceCacheMgr",
            "get",
            &[PyRep::String("other_key".into()), def_marker.clone()],
        )
        .await
        .unwrap()
        .unwrap(),
        def_marker
    );
    assert_eq!(
        call_rpc(
            &ctx,
            &mut sess,
            "dynamicResourceCacheMgr",
            "get",
            &[PyRep::List(vec![]), def_marker.clone()],
        )
        .await
        .unwrap()
        .unwrap(),
        def_marker
    );
}

#[tokio::test]
async fn warzone_fallback_sov_decay_and_partial_command_node_regeneration() {
    let ctx = test_ctx();
    let null_sys = 30_004_759_u32;
    let hub_id = 88_700_1_i64;
    let now = ctx.now_unix();

    // 1. collect_all_warzone_systems fallback when system_states lacks the FW system
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.remove(&null_sys);
        snap.fw_system_occupations.insert(
            null_sys,
            FwSystemOccupationRecord {
                solar_system_id: null_sys,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_004,
                victory_points: 1_500,
                victory_points_threshold: 3_000,
            },
        );
        snap.fw_runtime.system_victory_points.insert(null_sys, 2_250);
    });
    let wz = collect_all_warzone_systems(&ctx);
    let (_, contested, owner_f, occ_f, _) = wz
        .into_iter()
        .find(|(s, _, _, _, _)| *s == null_sys)
        .expect("null_sys warzone entry");
    assert!((contested - 75.0).abs() < 1e-6);
    assert_eq!(owner_f, 500_001);
    assert_eq!(occ_f, 500_004);

    // 2. constellation_systems_for_hub for known vs unknown solar system
    assert!(!constellation_systems_for_hub(&ctx, null_sys).is_empty());
    assert_eq!(constellation_systems_for_hub(&ctx, 999_999_999), vec![999_999_999]);

    // 3. decay_sov_dev_indices_in_snap skips < 86_400s elapsed and non-sov systems
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            null_sys,
            SystemDynamicState {
                solar_system_id: null_sys,
                sov_alliance_id: None,
                military_index: 4,
                industrial_index: 4,
                ..Default::default()
            },
        );
        snap.sov_runtime
            .sov_dev_decay_last_unix
            .insert(null_sys, now.saturating_sub(100_000));
        assert!(decay_sov_dev_indices_in_snap(snap, now).is_empty());

        snap.system_states.get_mut(&null_sys).unwrap().sov_alliance_id = Some(99_000_001);
        snap.sov_runtime
            .sov_dev_decay_last_unix
            .insert(null_sys, now.saturating_sub(86_399));
        assert!(decay_sov_dev_indices_in_snap(snap, now).is_empty());
    });

    // 4. init_hub_campaign_scores_in_snap corp fallback when sov_alliance_id is None,
    //    and refresh_system_adm_vulnerability_windows_in_snap skipping (0, 0) & Destroyed structures
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&null_sys).unwrap().sov_alliance_id = None;
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: 32458,
                owner_corp_id: 98_000_001,
                solar_system_id: null_sys,
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.sov_runtime.command_node_scores.remove(&hub_id);
        snap.sov_campaign_scores.remove(&hub_id);
        init_hub_campaign_scores_in_snap(snap, hub_id, null_sys);
        assert_eq!(snap.sov_runtime.command_node_scores[&hub_id][0].0, -98_000_001);

        snap.sov_runtime.adm_vulnerability_windows.insert(hub_id, (0, 0));
        refresh_system_adm_vulnerability_windows_in_snap(snap, null_sys);
    });

    // 5. Command node regeneration: reinforced guard, active capture guard, partial progress, then completion
    spawn_command_nodes_for_hub(&ctx, hub_id, null_sys).await;
    let first_node_id = ctx.db.with_snapshot(|snap| {
        *snap
            .sov_runtime
            .command_nodes
            .iter()
            .find(|(_, &(h, _, _))| h == hub_id)
            .unwrap()
            .0
    });

    // Reinforced structure guard skips regeneration
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&hub_id).unwrap().reinforced_until_unix = now + 5_000;
    });
    regenerate_unattended_command_nodes(&ctx, now + 1_000).await;
    let def_score_reinf = ctx.db.with_snapshot(|s| s.sov_campaign_scores[&hub_id].defender_score);
    assert!((def_score_reinf - 0.6).abs() < 1e-6);

    // Clear reinforced, set entosis_capture_progress on first_node_id -> active guard skips regeneration
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&hub_id).unwrap().reinforced_until_unix = 0;
        snap.sov_runtime
            .entosis_capture_progress
            .insert(first_node_id, (90_000_002, 98_000_001, 99_000_001, 0.5, now + 1_000));
    });
    regenerate_unattended_command_nodes(&ctx, now + 2_000).await;
    let def_score_active = ctx.db.with_snapshot(|s| s.sov_campaign_scores[&hub_id].defender_score);
    assert!((def_score_active - 0.6).abs() < 1e-6);

    // Remove active capture and advance by 588s (+0.10 defender score -> 0.70, partial regeneration)
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.entosis_capture_progress.remove(&first_node_id);
    });
    regenerate_unattended_command_nodes(&ctx, now + 2_588).await;
    ctx.db.with_snapshot(|snap| {
        let sc = &snap.sov_campaign_scores[&hub_id];
        assert!((sc.defender_score - 0.70).abs() < 1e-6);
        assert!((sc.attackers_score - 0.30).abs() < 1e-6);
    });
}

#[test]
fn adm_vulnerability_window_refresh_full_boundary_matrix() {
    let ctx = test_ctx();
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.0 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_505);
    let hub_id = 1_091_000_777_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            null_sys,
            SystemDynamicState {
                solar_system_id: null_sys,
                sov_alliance_id: Some(99_000_001),
                military_index: 0,
                industrial_index: 0,
                strategic_index: 0,
                ..Default::default()
            },
        );
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: 32458,
                owner_corp_id: 98_000_001,
                solar_system_id: null_sys,
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );

        // Invalid sentinel / out-of-range pairs must NOT trigger refresh (must stay skipped):
        // - (0, 0): sv == 0 && ev == 0
        // - (0, 86_400): sv == 0 && ev >= 86_400
        // - (90_000, 3_600): sv > 86_400
        // - (3_600, 90_000): ev > 86_400
        for &(sv, ev) in &[(0_u64, 0_u64), (0, 86_400), (90_000, 3_600), (3_600, 90_000)] {
            snap.sov_runtime
                .adm_vulnerability_windows
                .insert(hub_id, (sv, ev));
            snap.structure_vulnerability_windows.remove(&hub_id);
            refresh_system_adm_vulnerability_windows_in_snap(snap, null_sys);
            assert_eq!(
                snap.sov_runtime.adm_vulnerability_windows.get(&hub_id).copied(),
                Some((sv, ev)),
                "invalid window ({sv}, {ev}) should not be refreshed"
            );
            assert!(
                !snap.structure_vulnerability_windows.contains_key(&hub_id),
                "invalid window ({sv}, {ev}) should not update structure_vulnerability_windows"
            );
        }

        // Valid boundary pairs MUST trigger refresh and populate structure_vulnerability_windows:
        // - (0, 3_600): sv == 0, non-zero ev < 86_400
        // - (3_600, 0): non-zero sv <= 86_400, ev == 0
        // - (3_600, 86_400): non-zero sv, ev == 86_400 (boundary of > 86_400 and != sv==0)
        // - (86_400, 3_600): sv == 86_400 (boundary of > 86_400)
        // - (3_600, 7_200): both strictly inside (0, 86_400)
        for &(sv, ev) in &[
            (0_u64, 3_600_u64),
            (3_600, 0),
            (3_600, 86_400),
            (86_400, 3_600),
            (3_600, 7_200),
        ] {
            snap.sov_runtime
                .adm_vulnerability_windows
                .insert(hub_id, (sv, ev));
            snap.structure_vulnerability_windows.remove(&hub_id);
            refresh_system_adm_vulnerability_windows_in_snap(snap, null_sys);
            let refreshed = snap
                .structure_vulnerability_windows
                .get(&hub_id)
                .copied();
            assert!(
                refreshed.is_some(),
                "valid window ({sv}, {ev}) must refresh structure_vulnerability_windows"
            );
            assert_eq!(
                snap.sov_runtime.adm_vulnerability_windows.get(&hub_id).copied(),
                refreshed,
                "valid window ({sv}, {ev}) must sync adm_vulnerability_windows"
            );
            assert_ne!(
                snap.sov_runtime.adm_vulnerability_windows.get(&hub_id).copied(),
                Some((sv, ev)),
                "valid window ({sv}, {ev}) must change to ADM-scaled duration"
            );
        }
    });
}
