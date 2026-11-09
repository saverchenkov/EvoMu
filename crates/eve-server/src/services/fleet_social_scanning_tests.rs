use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use eve_db::{CosmicAnomalyRecord, UpwellStructureRecord};

fn make_session(char_id: i64, sys_id: u32) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_id),
        ..Default::default()
    }
}

#[tokio::test]
async fn signal_tracker_signatures_and_request_scans() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let char_id = 100_000_001;
    let sys_id = 30_000_142;
    let mut session = make_session(char_id, sys_id);
    let kw = HashMap::new();
    let svc = "scanMgr";

    let reg = handle_scanning_and_wormholes(&ctx, &mut session, svc, "SignalTrackerRegister", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reg.dict_get("registered").and_then(|v| v.as_bool()), Some(true));

    let sigs = handle_scanning_and_wormholes(&ctx, &mut session, svc, "GetSignatures", &[PyRep::Integer(sys_id as i64)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(!matches!(sigs, PyRep::None));

    assert!(handle_scanning_and_wormholes(&ctx, &mut session, svc, "RequestScans", &[], &kw).await.is_none());

    let scans = handle_scanning_and_wormholes(
        &ctx,
        &mut session,
        svc,
        "RequestScans",
        &[PyRep::List(vec![]), PyRep::Real(AU_IN_METERS), PyRep::Real(50.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!matches!(scans, PyRep::None));

    let sig_rec = CosmicSignatureRecord {
        signature_id: "ABC-123".into(),
        solar_system_id: sys_id,
        sig_group: "Relic Site".into(),
        type_name: "Ruined Site".into(),
        signal_strength: 0.5,
        position: [1000.0, 2000.0, 3000.0],
        wormhole_type_id: None,
        destination_system_id: None,
        created_unix: 1000,
        lifetime_seconds: 3600,
        max_mass_kg: 0.0,
        remaining_mass_kg: 0.0,
        max_jump_mass_kg: 0.0,
        collapsed: false,
    };
    let kv = build_signal_tracker_signature_kv(&ctx.sde, &sig_rec);
    assert_eq!(kv.dict_get("targetID").and_then(|v| v.as_str()), Some("ABC-123"));

    let anom_rec = CosmicAnomalyRecord {
        anomaly_id: "ANOM-1".into(),
        solar_system_id: sys_id,
        anomaly_type: "Combat".into(),
        name: "Hideaway".into(),
        signal_strength: 1.0,
        position: [10.0, 20.0, 30.0],
        ore_types: Vec::new(),
        remaining_volume: 0.0,
        created_unix: 1000,
        lifetime_seconds: 3600,
        cleared: false,
    };
    let akv = build_signal_tracker_anomaly_kv(&ctx.sde, &anom_rec);
    assert_eq!(akv.dict_get("instanceID").and_then(|v| v.as_str()), Some("ANOM-1"));
    assert!(
        akv.dict_get("dungeonID").and_then(|v| v.as_i64()).is_some_and(|id| id > 0),
        "anomaly KeyVal must carry positive integer dungeonID (#747)"
    );
    assert!(
        akv.dict_get("archetypeID").and_then(|v| v.as_i64()).is_some_and(|id| id > 0),
        "anomaly KeyVal must carry positive integer archetypeID (#747)"
    );
    assert!(
        akv.dict_get("factionID").and_then(|v| v.as_i64()).is_some_and(|id| id > 0),
        "anomaly KeyVal must carry positive integer factionID (#747)"
    );
    assert!(
        akv.dict_get("scanStrengthAttribute").and_then(|v| v.as_i64()).is_some_and(|id| id > 0),
        "anomaly KeyVal must carry positive integer scanStrengthAttribute (#747)"
    );

    let ore_anom = CosmicAnomalyRecord {
        anomaly_id: "DNG-10778-42".into(),
        solar_system_id: sys_id,
        anomaly_type: "OreAnomaly".into(),
        name: "Veldspar Deposit".into(),
        signal_strength: 1.0,
        position: [10.0, 20.0, 30.0],
        ore_types: vec![1230],
        remaining_volume: 1000.0,
        created_unix: 1000,
        lifetime_seconds: 3600,
        cleared: false,
    };
    let ore_kv = build_signal_tracker_anomaly_kv(&ctx.sde, &ore_anom);
    assert_eq!(ore_kv.dict_get("dungeonID").and_then(|v| v.as_i64()), Some(10_778));
    assert_eq!(ore_kv.dict_get("archetypeID").and_then(|v| v.as_i64()), Some(27));
    assert_eq!(ore_kv.dict_get("scanStrengthAttribute").and_then(|v| v.as_i64()), Some(211));

    let st_rec = UpwellStructureRecord {
        structure_id: 1_000_000_000_555,
        type_id: 35832,
        owner_corp_id: 1_000_009,
        solar_system_id: sys_id,
        name: "Astrahus".into(),
        state: "ShieldVulnerable".into(),
        position: [0.0, 0.0, 0.0],
        quantum_core_installed: true,
        fuel_blocks: 100,
        ..Default::default()
    };
    let skv = build_signal_tracker_structure_kv(&ctx, &st_rec);
    assert_eq!(skv.dict_get("targetID").and_then(|v| v.as_i64()), Some(1_000_000_000_555));

    emit_signal_tracker_signature_delta(&ctx, sys_id, &[sig_rec], &["OLD-1".into()]);
    emit_signal_tracker_anomaly_delta(&ctx, sys_id, &[anom_rec], &["OLD-A".into()]);
    emit_signal_tracker_structure_delta(&ctx, sys_id, &[st_rec], &[1_000_000_000_444]);
}
