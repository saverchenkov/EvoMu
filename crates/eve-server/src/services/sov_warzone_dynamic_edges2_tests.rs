use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::tactical_modules::{ActiveEntosisLinkState, db_ptr_key};
use eve_db::{
    FwSystemOccupationRecord, IskCents, SovCampaignScoreRecord, SystemDynamicState,
    UpwellStructureRecord,
};
use std::collections::HashMap;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sov_warzone_dynamic_handle_unknown_and_settings_json_override() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    // Unknown service/method returns None
    assert!(
        handle(&ctx, &mut sess, "other", "GetDBSMapData", &[], &kw)
            .await
            .is_none()
    );
    assert!(
        handle(&ctx, &mut sess, "dynamicResourceCacheMgr", "GetTime", &[], &kw)
            .await
            .is_none()
    );
    assert!(
        handle(&ctx, &mut sess, "sovMgr", "GetTime", &[], &kw)
            .await
            .is_none()
    );
    assert!(
        handle(&ctx, &mut sess, "fwWarzoneSolarsystem", "values", &[], &kw)
            .await
            .is_none()
    );

    // dynamic_resource_settings_from_db JSON override branch
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(
            "dynamic_resource_settings".to_string(),
            serde_json::json!({
                "mainBankShare": 0.42,
                "reserveBankShare": 0.18,
                "minOutput": 0.65,
                "maxOutput": 1.85,
                "linkDurationSeconds": 450
            })
            .to_string(),
        );
    });
    let settings = handle(
        &ctx,
        &mut sess,
        "dynamicResourceCacheMgr",
        "GetDynamicResourceSettings",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(settings.dict_get("mainBankShare"), Some(&PyRep::Real(0.42)));
    assert_eq!(settings.dict_get("reserveBankShare"), Some(&PyRep::Real(0.18)));
    assert_eq!(settings.dict_get("minOutput"), Some(&PyRep::Real(0.65)));
    assert_eq!(settings.dict_get("maxOutput"), Some(&PyRep::Real(1.85)));
    assert_eq!(settings.dict_get("linkDurationSeconds"), Some(&PyRep::Integer(450)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sov_warzone_dynamic_ess_details_reserve_fallback_and_dbs_filtering() {
    let ctx = test_ctx();
    let mut sess = auth_session(2, 90_000_002, 98_000_001, 30_000_142); // high-sec Jita
    let kw = HashMap::new();

    let null_sys = ctx
        .sde
        .solar_systems
        .iter()
        .find(|(&id, s)| id < 31_000_000 && s.security_status <= 0.0)
        .map(|(&id, _)| id)
        .expect("nullsec system in SDE");

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            null_sys,
            SystemDynamicState {
                solar_system_id: null_sys,
                dbs_multiplier: 1.33,
                ess_main_bank: IskCents::from_isk(5_000_000),
                ess_reserve_bank: IskCents::from_isk(2_000_000),
                ess_linked_character_id: None,
                ..Default::default()
            },
        );
        crate::services::space_destiny::set_ess_reserve_pulses(
            snap,
            null_sys,
            &crate::services::space_destiny::EssReservePulseState {
                pulses_total: 10,
                pulses_remaining: 5,
                last_pulse_sec: None,
                reserve_linked_character_id: Some(90_000_999),
                reserve_link_started_tick: Some(12),
                link_started_unix: Some(now.saturating_sub(100)),
                unlocked_until_unix: now.saturating_sub(10), // expired
            },
        );
        // High-sec system in system_states should be filtered out of GetDBSMapData
        snap.system_states.insert(
            30_000_142,
            SystemDynamicState {
                solar_system_id: 30_000_142,
                dbs_multiplier: 0.99,
                ..Default::default()
            },
        );
    });

    // GetDBSMapData filters out highsec and includes nullsec
    let dbs = handle(&ctx, &mut sess, "dynamicResourceCacheMgr", "GetDBSMapData", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    if let PyRep::Dict(pairs) = &dbs {
        assert!(
            pairs
                .iter()
                .any(|(k, v)| *k == PyRep::Integer(i64::from(null_sys)) && *v == PyRep::Real(1.33))
        );
        assert!(!pairs.iter().any(|(k, _)| *k == PyRep::Integer(30_000_142)));
    } else {
        panic!("expected Dict from GetDBSMapData");
    }

    // GetESSAgencyData: expired unlock -> vaultOpen == false
    let agency = handle(&ctx, &mut sess, "dynamicResourceCacheMgr", "GetESSAgencyData", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    if let PyRep::Dict(entries) = agency {
        let (_, entry) = entries
            .iter()
            .find(|(k, _)| *k == PyRep::Integer(i64::from(null_sys)))
            .expect("null_sys ESS agency entry");
        assert_eq!(entry.dict_get("vaultOpen"), Some(&PyRep::Bool(false)));
    } else {
        panic!("expected Dict from GetESSAgencyData");
    }

    // GetESSSystemDetails with explicit arg for null_sys -> picks reserve_linked_character_id
    let details_null = handle(
        &ctx,
        &mut sess,
        "dynamicResourceCacheMgr",
        "GetESSSystemDetails",
        &[PyRep::Integer(i64::from(null_sys))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        details_null.dict_get("mainBankValue"),
        Some(&PyRep::Real(5_000_000.0))
    );
    assert_eq!(
        details_null.dict_get("reserveBankValue"),
        Some(&PyRep::Real(2_000_000.0))
    );
    assert_eq!(
        details_null.dict_get("linkedCharacterID"),
        Some(&PyRep::Integer(90_000_999))
    );

    // GetESSSystemDetails for unknown system -> defaults (st == None branch)
    let details_missing = handle(
        &ctx,
        &mut sess,
        "dynamicResourceCacheMgr",
        "GetESSSystemDetails",
        &[PyRep::Integer(30_009_999)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        details_missing.dict_get("mainBankValue"),
        Some(&PyRep::Real(0.0))
    );
    assert_eq!(
        details_missing.dict_get("reserveBankValue"),
        Some(&PyRep::Real(0.0))
    );
    assert_eq!(details_missing.dict_get("linkedCharacterID"), Some(&PyRep::None));

    // GetESSReserveBankKeys returns all 8 reserve keys (57714..=57721)
    let keys = handle(
        &ctx,
        &mut sess,
        "dynamicResourceCacheMgr",
        "GetESSReserveBankKeys",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    if let PyRep::List(list) = keys {
        assert_eq!(list.len(), 8);
    } else {
        panic!("expected List of 8 reserve bank keys");
    }

    // get for unknown system returns None
    let get_unknown = handle(
        &ctx,
        &mut sess,
        "dynamicResourceCacheMgr",
        "get",
        &[PyRep::Integer(30_009_999)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(get_unknown, PyRep::None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sov_warzone_dynamic_remove_from_cache_with_arg_and_fw_warzone_threshold_zero() {
    let ctx = test_ctx();
    let mut sess = auth_session(3, 90_000_003, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    // Put two cached sovMgr entries; pass arg for one and ensure unrelated stays
    let oid_a = ServerContext::method_call_object_id("sovMgr", "GetSystemSovereigntyInfo");
    let oid_b = ServerContext::method_call_object_id("sovMgr", "GetInfrastructureHubInfo");
    ctx.put_cached_object(oid_a.clone(), PyRep::Integer(1), true);
    ctx.put_cached_object(oid_b.clone(), PyRep::Integer(2), true);

    assert_eq!(
        handle(&ctx, &mut sess, "sovMgr", "remove_from_cache", &[oid_a.clone()], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    let cache = ctx.object_cache.read().unwrap();
    assert!(!cache.contains_key(&crate::router::cache_object_key(&oid_a)));
    assert!(cache.contains_key(&crate::router::cache_object_key(&oid_b)));
    drop(cache);

    // FW system with victory_points_threshold == 0 should safely clamp divisor via .max(1.0)
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.insert(
            30_002_062,
            FwSystemOccupationRecord {
                solar_system_id: 30_002_062,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_004,
                victory_points: 5,
                victory_points_threshold: 0,
            },
        );
    });
    let all = collect_all_warzone_systems(&ctx);
    let found = all
        .into_iter()
        .find(|(id, ..)| *id == 30_002_062)
        .expect("FW system present");
    assert!((found.1 - 100.0).abs() < 1e-6);

    let occ = build_warzone_occupation_state(&ctx, 30_002_062, 100.0, 500_001, 500_004, 1);
    assert!(matches!(occ, PyRep::KeyVal(_)));
}

#[test]
fn sov_warzone_dynamic_decay_multi_day_and_init() {
    let ctx = test_ctx();
    let changed = ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            30_004_001,
            SystemDynamicState {
                solar_system_id: 30_004_001,
                sov_alliance_id: Some(99_000_001),
                military_index: 5,
                industrial_index: 5,
                ..Default::default()
            },
        );
        snap.sov_runtime.sov_npc_kills.insert(30_004_001, 100);
        snap.sov_runtime.sov_mined_units.insert(30_004_001, 100_000);

        // First call initializes sov_dev_decay_last_unix and does not decay
        let init_changed = decay_sov_dev_indices_in_snap(snap, 1_000_000);
        assert!(init_changed.is_empty());
        assert_eq!(
            snap.sov_runtime.sov_dev_decay_last_unix.get(&30_004_001),
            Some(&1_000_000)
        );

        // elapsed < 86_400 does nothing
        assert!(decay_sov_dev_indices_in_snap(snap, 1_050_000).is_empty());

        // 2 full days later -> 0.75^2 = 0.5625
        decay_sov_dev_indices_in_snap(snap, 1_000_000 + 2 * 86_400)
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.sov_runtime.sov_npc_kills[&30_004_001], 56);
        assert_eq!(snap.sov_runtime.sov_mined_units[&30_004_001], 56_250);
        assert!(changed.contains(&30_004_001));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sov_warzone_dynamic_regenerate_nodes_skips_active_links_and_reinforced() {
    let ctx = test_ctx();
    let now = 2_000_000u64;
    let hub_id = 1_090_000_001i64;
    let node_id = 1_090_000_002i64;
    let sys_id = 30_000_142u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                solar_system_id: sys_id,
                state: "Reinforced".to_string(),
                reinforced_until_unix: now + 500,
                ..Default::default()
            },
        );
        snap.sov_campaign_scores.insert(
            hub_id,
            SovCampaignScoreRecord {
                structure_id: hub_id,
                attackers_score: 0.4,
                defender_score: 0.6,
            },
        );
        snap.sov_runtime.command_nodes.insert(node_id, (hub_id, sys_id, 1));
        snap.sov_runtime
            .command_node_scores
            .insert(hub_id, vec![(node_id, 0.6)]);
    });

    // Reinforced hub skips regen and updates regen_last_unix
    regenerate_unattended_command_nodes(&ctx, now + 100).await;
    ctx.db.with_snapshot(|snap| {
        assert!((snap.sov_campaign_scores[&hub_id].defender_score - 0.6).abs() < 1e-6);
        assert_eq!(
            snap.sov_runtime.command_node_regen_last_unix.get(&hub_id),
            Some(&(now + 100))
        );
    });

    // Clear reinforcement, attach an active entosis link to node_id -> skips regen
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&hub_id).unwrap().reinforced_until_unix = 0;
    });
    let db_key = db_ptr_key(&ctx.db);
    {
        let mut tst = ctx.tactical_state.write().unwrap();
        tst.entosis_active_links.insert(
            (db_key, 888_001),
            ActiveEntosisLinkState {
                module_id: 888_002,
                char_id: 888_003,
                ship_id: 888_001,
                side_id: 99_000_001,
                target_id: node_id,
                cycle_duration_s: 300.0,
                started_unix: now + 100,
                expires_unix: now + 400,
                cycle_index: 1,
                is_capital: false,
            },
        );
    }
    regenerate_unattended_command_nodes(&ctx, now + 1_000).await;
    ctx.db.with_snapshot(|snap| {
        assert!((snap.sov_campaign_scores[&hub_id].defender_score - 0.6).abs() < 1e-6);
        assert_eq!(
            snap.sov_runtime.command_node_regen_last_unix.get(&hub_id),
            Some(&(now + 1_000))
        );
    });

    // Remove link -> partial unattended regen updates score without deleting campaign
    ctx.tactical_state
        .write()
        .unwrap()
        .entosis_active_links
        .remove(&(db_key, 888_001));
    regenerate_unattended_command_nodes(&ctx, now + 1_588).await;
    ctx.db.with_snapshot(|snap| {
        let sc = &snap.sov_campaign_scores[&hub_id];
        assert!(sc.defender_score > 0.6 && sc.defender_score < 1.0);
        assert!(snap.sov_runtime.command_nodes.contains_key(&node_id));
    });
}

#[test]
fn sov_warzone_dynamic_upgrade_specs_adm_windows_and_init_scores_edges() {
    // All Prospecting Array mineral variants + Threat Detection + unknown
    for (name, expected_ore) in [
        ("Tritanium Prospecting Array 1", 1230u32),
        ("Pyerite Prospecting Array 1", 1228),
        ("Mexallon Prospecting Array 1", 20),
        ("Isogen Prospecting Array 1", 1227),
        ("Nocxium Prospecting Array 1", 1232),
        ("Zydrine Prospecting Array 1", 1223),
        ("Megacyte Prospecting Array 1", 19),
        ("Generic Prospecting Array 1", 1229),
    ] {
        let specs = sov_upgrade_site_specs(name, -0.5);
        assert!(!specs.is_empty(), "expected specs for {name}");
        assert_eq!(specs[0].1, format!("Sovereign {name}"));
        assert!(
            specs[0].3.contains(&expected_ore),
            "expected ore {expected_ore} in {name}, got {:?}",
            specs[0].3
        );
    }
    let threat = sov_upgrade_site_specs("Major Threat Detection Array 2", -0.6);
    assert!(!threat.is_empty());
    let minor_threat = sov_upgrade_site_specs("Minor Threat Detection Array 1", -0.2);
    assert!(!minor_threat.is_empty());
    assert!(sov_upgrade_site_specs("Cyno Navigation Upgrade", -0.5).is_empty());

    // init_hub_campaign_scores_in_snap when structure and sov_alliance_id are missing + idempotence
    let ctx = test_ctx();
    ctx.db.with_mut_snapshot(|snap| {
        init_hub_campaign_scores_in_snap(snap, 777_001, 30_004_999);
        assert!(
            (snap.sov_campaign_scores[&777_001].defender_score
                - crate::services::sov_gameplay::SOV_COMMAND_NODE_INITIAL_PROGRESS)
                .abs()
                < 1e-6
        );
        snap.sov_campaign_scores.get_mut(&777_001).unwrap().defender_score = 0.77;
        // Second call is idempotent (doesn't overwrite existing campaign score)
        init_hub_campaign_scores_in_snap(snap, 777_001, 30_004_999);
        assert!((snap.sov_campaign_scores[&777_001].defender_score - 0.77).abs() < 1e-6);

        // refresh_system_adm_vulnerability_windows_in_snap: wraparound window, zero window, Destroyed skipped
        snap.structures.insert(
            777_010,
            UpwellStructureRecord {
                structure_id: 777_010,
                solar_system_id: 30_004_999,
                state: "ShieldVulnerable".to_string(),
                ..Default::default()
            },
        );
        snap.sov_runtime
            .adm_vulnerability_windows
            .insert(777_010, (82_800, 7_200));

        snap.structures.insert(
            777_011,
            UpwellStructureRecord {
                structure_id: 777_011,
                solar_system_id: 30_004_999,
                state: "Destroyed".to_string(),
                ..Default::default()
            },
        );
        snap.sov_runtime
            .adm_vulnerability_windows
            .insert(777_011, (3_600, 14_400));

        snap.structures.insert(
            777_012,
            UpwellStructureRecord {
                structure_id: 777_012,
                solar_system_id: 30_004_999,
                state: "ShieldVulnerable".to_string(),
                ..Default::default()
            },
        );
        snap.sov_runtime
            .adm_vulnerability_windows
            .insert(777_012, (0, 0));

        refresh_system_adm_vulnerability_windows_in_snap(snap, 30_004_999);
        assert!(snap.sov_runtime.adm_vulnerability_windows.contains_key(&777_010));
        assert_eq!(snap.sov_runtime.adm_vulnerability_windows[&777_011], (3_600, 14_400));
        assert_eq!(snap.sov_runtime.adm_vulnerability_windows[&777_012], (0, 0));
    });
}
