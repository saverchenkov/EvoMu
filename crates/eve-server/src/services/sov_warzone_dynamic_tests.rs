use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use std::collections::HashMap;

#[tokio::test]
async fn test_sov_warzone_dynamic_dbs_ess_fw_and_sov_indices() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    assert!(handle(&ctx, &mut sess, "other", "GetDBSMapData", &[], &kw).await.is_none());

    // 1. Military and industrial index tier boundaries
    assert_eq!(military_index_for_kills(0), 0);
    assert_eq!(military_index_for_kills(4), 0);
    assert_eq!(military_index_for_kills(5), 1);
    assert_eq!(military_index_for_kills(15), 2);
    assert_eq!(military_index_for_kills(35), 3);
    assert_eq!(military_index_for_kills(70), 4);
    assert_eq!(military_index_for_kills(120), 5);

    assert_eq!(industrial_index_for_mined(0), 0);
    assert_eq!(industrial_index_for_mined(999), 0);
    assert_eq!(industrial_index_for_mined(1_000), 1);
    assert_eq!(industrial_index_for_mined(5_000), 2);
    assert_eq!(industrial_index_for_mined(15_000), 3);
    assert_eq!(industrial_index_for_mined(35_000), 4);
    assert_eq!(industrial_index_for_mined(75_000), 5);

    // 2. sov_upgrade_site_specs branches
    assert_eq!(sov_upgrade_site_specs("Major Threat Detection Array III", -0.6).len(), 5);
    assert_eq!(sov_upgrade_site_specs("Minor Threat Detection Array I", -0.2).len(), 1);
    for ore_label in [
        "Tritanium Prospecting Array I",
        "Pyerite Prospecting Array II",
        "Mexallon Prospecting Array III",
        "Isogen Prospecting Array",
        "Nocxium Prospecting Array",
        "Zydrine Prospecting Array",
        "Megacyte Prospecting Array",
        "Morphite Prospecting Array",
    ] {
        let specs = sov_upgrade_site_specs(ore_label, -0.5);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].0, "Ore");
    }
    assert_eq!(sov_upgrade_site_specs("Exploration Detector II", -0.5).len(), 3);
    assert!(sov_upgrade_site_specs("Unknown Upgrade", -0.5).is_empty());

    // 3. Seed nullsec system state, FW warzone, and sov hub
    let null_sys = 30_004_759_u32;
    let hub_id = 88_600_1;
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            null_sys,
            eve_db::SystemDynamicState {
                solar_system_id: null_sys,
                sov_alliance_id: Some(99_000_001),
                dbs_multiplier: 1.45,
                ess_main_bank: eve_db::IskCents::from_isk(10_000_000),
                ess_reserve_bank: eve_db::IskCents::from_isk(50_000_000),
                military_index: 5,
                industrial_index: 5,
                ..Default::default()
            },
        );
        snap.sov_runtime.sov_npc_kills.insert(null_sys, 100);
        snap.sov_runtime.sov_mined_units.insert(null_sys, 50_000);
        snap.sov_runtime
            .sov_dev_decay_last_unix
            .insert(null_sys, now.saturating_sub(2 * 86_400));
        let decayed = decay_sov_dev_indices_in_snap(snap, now);
        assert!(decayed.contains(&null_sys));

        snap.structures.insert(
            hub_id,
            eve_db::UpwellStructureRecord {
                structure_id: hub_id,
                type_id: 32458,
                owner_corp_id: 1_000_009,
                solar_system_id: null_sys,
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.sov_runtime
            .adm_vulnerability_windows
            .insert(hub_id, (3_600, 14_400));
        init_hub_campaign_scores_in_snap(snap, hub_id, null_sys);
        refresh_system_adm_vulnerability_windows_in_snap(snap, null_sys);
    });

    // 4. dynamicResourceCacheMgr & sovMgr & fwWarzoneSolarsystem RPCs
    let dbs = handle(&ctx, &mut sess, "dynamicResourceCacheMgr", "GetDBSMapData", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(dbs, PyRep::Dict(ref d) if !d.is_empty()));

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
    assert!(settings.dict_get("bountyRiskModifierMax").is_some());
    assert_eq!(settings.dict_get("earlyPayoutThresholdFactor"), Some(&PyRep::Real(0.25)));
    assert_eq!(settings.dict_get("latePayoutThresholdFactor"), Some(&PyRep::Real(0.75)));
    assert_eq!(settings.dict_get("reservePulseIntervalSeconds"), Some(&PyRep::Integer(60)));

    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(
            "dynamic_resource_settings".into(),
            r#"{"reservePulseIntervalSeconds":90}"#.into(),
        );
    });
    let overridden = handle(
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
    assert_eq!(overridden.dict_get("reservePulseIntervalSeconds"), Some(&PyRep::Integer(90)));

    let agency = handle(&ctx, &mut sess, "dynamicResourceCacheMgr", "GetESSAgencyData", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(agency, PyRep::Dict(ref d) if !d.is_empty()));

    let keys = handle(&ctx, &mut sess, "dynamicResourceCacheMgr", "GetESSReserveBankKeys", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(keys, PyRep::List(ref l) if l.len() == 8));

    let details = handle(
        &ctx,
        &mut sess,
        "dynamicResourceCacheMgr",
        "GetESSSystemDetails",
        &[PyRep::Integer(null_sys as i64)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(details.dict_get("solarSystemID"), Some(&PyRep::Integer(null_sys as i64)));

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "dynamicResourceCacheMgr",
            "get",
            &[PyRep::Integer(null_sys as i64)],
            &kw
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Real(1.45)
    );
    let get_set = handle(
        &ctx,
        &mut sess,
        "dynamicResourceCacheMgr",
        "get",
        &[PyRep::String("settings".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(get_set.dict_get("mainBankShare").is_some());

    let iteritems = handle(&ctx, &mut sess, "dynamicResourceCacheMgr", "iteritems", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(iteritems, PyRep::List(ref l) if !l.is_empty()));

    let sov_oid = ServerContext::method_call_object_id("sovMgr", "GetSovStructuresInfoForSolarSystem");
    ctx.put_cached_object(sov_oid.clone(), PyRep::List(vec![]), true);
    assert!(ctx.object_cache.read().unwrap().contains_key(&crate::router::cache_object_key(&sov_oid)));

    assert_eq!(
        handle(&ctx, &mut sess, "sovMgr", "remove_from_cache", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    assert!(!ctx.object_cache.read().unwrap().contains_key(&crate::router::cache_object_key(&sov_oid)));

    for m in ["iteritems", "itervalues", "keys"] {
        let r = handle(&ctx, &mut sess, "fwWarzoneSolarsystem", m, &[], &kw)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(r, PyRep::List(_)));
    }

    // 5. Command node spawning and unattended regeneration
    spawn_command_nodes_for_hub(&ctx, hub_id, null_sys).await;
    regenerate_unattended_command_nodes(&ctx, now + 6_000).await;
    let campaign_cleared = ctx.db.with_snapshot(|s| !s.sov_campaign_scores.contains_key(&hub_id));
    assert!(campaign_cleared);
}
