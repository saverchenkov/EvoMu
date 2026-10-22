//! Integration tests for Dynamic Resource / ESS Cache RPCs (`dynamicResourceCacheMgr`).

use crate::common;
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_mut_combat_runtime;
use glam::DVec3;
use std::collections::HashMap;

const NULLSEC_SYS_ID: u32 = 30_004_759; // VFK-IV (K-space null-sec)
const CHAR_ID: i64 = 90_000_001;
const SHIP_ID: i64 = 940_001;
const ESS_BALL_ID: i64 = 940_010;
const RIFTER_TYPE_ID: u32 = 587;
const ESS_BEACON_TYPE_ID: u32 = 32882;

fn kv_map(rep: &PyRep) -> HashMap<String, PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.clone(),
        PyRep::Dict(entries) => entries
            .iter()
            .filter_map(|(k, v)| match k {
                PyRep::String(s) | PyRep::Token(s) => Some((s.clone(), v.clone())),
                _ => None,
            })
            .collect(),
        _ => HashMap::new(),
    }
}

async fn setup_dynamic_resource_env(
    ctx: &ServerContext,
    main_bank_isk: f64,
    reserve_bank_isk: f64,
    dbs_multiplier: f64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(3201);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(CHAR_ID)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    sess.solar_system_id = Some(NULLSEC_SYS_ID);
    sess.ship_id = Some(SHIP_ID);
    sess.station_id = None;
    sess.structure_id = None;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = NULLSEC_SYS_ID;
            ch.active_ship_id = SHIP_ID;
        }

        snap.system_states.insert(
            NULLSEC_SYS_ID,
            eve_db::SystemDynamicState {
                solar_system_id: NULLSEC_SYS_ID,
                dbs_multiplier,
                ess_main_bank: eve_db::IskCents::from_f64_isk(main_bank_isk),
                ess_reserve_bank: eve_db::IskCents::from_f64_isk(reserve_bank_isk),
                ess_linked_character_id: None,
                ess_link_started_tick: None,
                sov_alliance_id: None,
                military_index: 2,
                industrial_index: 2,
                strategic_index: 2,
                fw_contested_pct: 0.0,
                insurgency_corruption_stage: 0,
                insurgency_suppression_stage: 0,
                wormhole_remaining_mass_kg: 0.0,
                wormhole_destination_system_id: None,
                signature_position: None,
                fw_complex_position: None,
                fw_capture_seconds_elapsed: 0,
                fw_capture_duration_seconds: 0,
                last_fw_capture_tick: 0,
                last_sov_claim_tick: 0,
            },
        );

        snap.items.insert(
            SHIP_ID,
            eve_db::InventoryItemRecord {
                item_id: SHIP_ID,
                type_id: RIFTER_TYPE_ID,
                owner_id: CHAR_ID,
                location_id: NULLSEC_SYS_ID as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(NULLSEC_SYS_ID);
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: SHIP_ID,
                type_id: RIFTER_TYPE_ID,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(RIFTER_TYPE_ID),
        )
        .await;

    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ESS_BALL_ID,
                type_id: ESS_BEACON_TYPE_ID,
                position: DVec3::new(2_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(ESS_BEACON_TYPE_ID),
        )
        .await;

    sess
}

#[tokio::test]
async fn test_dynamic_resource_cache_dbs_and_settings() {
    // parity: rpc:dynamicResourceCacheMgr.GetDBSMapData
    // parity: rpc:dynamicResourceCacheMgr.GetDynamicResourceSettings
    // parity: rpc:dynamicResourceCacheMgr.get
    // parity: rpc:dynamicResourceCacheMgr.iteritems
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = setup_dynamic_resource_env(&ctx, 100_000_000.0, 500_000_000.0, 1.45).await;

    // 1. GetDBSMapData
    let dbs_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dynamicResourceCacheMgr",
            None,
            "GetDBSMapData",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetDBSMapData");
    match dbs_rep {
        PyRep::Dict(map) => {
            let found = map.iter().any(|(k, v)| {
                k.as_i64() == Some(NULLSEC_SYS_ID as i64)
                    && (v.as_f64().unwrap_or(0.0) - 1.45).abs() < 1e-4
            });
            assert!(found, "DBS map data must contain nullsec system with DBS 1.45");
        }
        other => panic!("Expected Dict for GetDBSMapData, got {other:?}"),
    }

    // 2. GetDynamicResourceSettings
    let settings_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dynamicResourceCacheMgr",
            None,
            "GetDynamicResourceSettings",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetDynamicResourceSettings");
    match settings_rep {
        PyRep::Dict(settings) => {
            let mut keys: HashMap<String, PyRep> = HashMap::new();
            for (k, v) in settings {
                if let Some(ks) = k.as_str() {
                    keys.insert(ks.to_string(), v);
                }
            }
            assert_eq!(
                keys.get("bountyRiskModifierMax")
                    .and_then(|v| v.as_f64()),
                Some(2.0)
            );
            assert_eq!(
                keys.get("bountyRiskModifierMin")
                    .and_then(|v| v.as_f64()),
                Some(0.5)
            );
            assert_eq!(
                keys.get("linkDurationSeconds")
                    .and_then(|v| v.as_i64()),
                Some(300)
            );
            assert_eq!(
                keys.get("autoPayoutTimeSeconds")
                    .and_then(|v| v.as_i64()),
                Some(10800)
            );
        }
        other => panic!("Expected Dict for GetDynamicResourceSettings, got {other:?}"),
    }

    // 3. get (by system ID, string "settings", and default fallback)
    let get_sys = ctx
        .dispatch_rpc(
            &mut sess,
            "dynamicResourceCacheMgr",
            None,
            "get",
            &[PyRep::Integer(NULLSEC_SYS_ID as i64)],
            &kw,
            None,
        )
        .await
        .expect("get sys_id");
    assert!(
        (get_sys.as_f64().unwrap_or(0.0) - 1.45).abs() < 1e-4,
        "get(sys_id) must return DBS multiplier 1.45"
    );

    let get_settings = ctx
        .dispatch_rpc(
            &mut sess,
            "dynamicResourceCacheMgr",
            None,
            "get",
            &[PyRep::String("settings".into())],
            &kw,
            None,
        )
        .await
        .expect("get settings");
    assert!(matches!(get_settings, PyRep::Dict(_)));

    let get_default = ctx
        .dispatch_rpc(
            &mut sess,
            "dynamicResourceCacheMgr",
            None,
            "get",
            &[PyRep::Integer(99_999_999), PyRep::Real(1.0)],
            &kw,
            None,
        )
        .await
        .expect("get default");
    assert_eq!(get_default.as_f64(), Some(1.0));

    // 4. iteritems
    let iter_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dynamicResourceCacheMgr",
            None,
            "iteritems",
            &[],
            &kw,
            None,
        )
        .await
        .expect("iteritems");
    match iter_rep {
        PyRep::List(items) => {
            let found = items.iter().any(|item| {
                if let PyRep::Tuple(pair) = item {
                    pair.first().and_then(|k| k.as_i64()) == Some(NULLSEC_SYS_ID as i64)
                        && (pair.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0) - 1.45).abs() < 1e-4
                } else {
                    false
                }
            });
            assert!(found, "iteritems must yield (sys_id, 1.45)");
        }
        other => panic!("Expected List for iteritems, got {other:?}"),
    }
}

#[tokio::test]
async fn test_dynamic_resource_cache_ess_details_and_keys() {
    // parity: rpc:dynamicResourceCacheMgr.GetESSAgencyData
    // parity: rpc:dynamicResourceCacheMgr.GetESSReserveBankKeys
    // parity: rpc:dynamicResourceCacheMgr.GetESSSystemDetails
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = setup_dynamic_resource_env(&ctx, 60_000_000.0, 300_000_000.0, 1.35).await;

    // 1. GetESSReserveBankKeys
    let keys_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dynamicResourceCacheMgr",
            None,
            "GetESSReserveBankKeys",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetESSReserveBankKeys");
    match keys_rep {
        PyRep::List(keys) => {
            assert_eq!(keys.len(), 8, "Must return 8 Reserve Bank Key definitions (57714..57721)");
            for k in &keys {
                let m = kv_map(k);
                let tid = m.get("typeID").and_then(|v| v.as_i64()).unwrap_or(0);
                assert!((57714..=57721).contains(&tid));
                assert_eq!(
                    m.get("groupID").and_then(|v| v.as_i64()),
                    Some(4102)
                );
                assert_eq!(
                    m.get("unlockDurationSeconds").and_then(|v| v.as_i64()),
                    Some(2700)
                );
            }
        }
        other => panic!("Expected List for GetESSReserveBankKeys, got {other:?}"),
    }

    // 2. GetESSSystemDetails (explicit sys_id & alias GetSolarSystemDynamicResourceData)
    let details_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dynamicResourceCacheMgr",
            None,
            "GetESSSystemDetails",
            &[PyRep::Integer(NULLSEC_SYS_ID as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetESSSystemDetails");
    let det_map = kv_map(&details_rep);
    assert_eq!(
        det_map.get("solarSystemID").and_then(|v| v.as_i64()),
        Some(NULLSEC_SYS_ID as i64)
    );
    assert!(
        (det_map.get("mainBankValue").and_then(|v| v.as_f64()).unwrap_or(0.0) - 60_000_000.0).abs() < 1e-2
    );
    assert!(
        (det_map.get("reserveBankValue").and_then(|v| v.as_f64()).unwrap_or(0.0) - 300_000_000.0).abs() < 1e-2
    );
    assert_eq!(
        det_map.get("isUnlocked").and_then(|v| v.as_bool()),
        Some(false)
    );

    // 3. GetESSAgencyData
    let agency_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dynamicResourceCacheMgr",
            None,
            "GetESSAgencyData",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetESSAgencyData");
    match agency_rep {
        PyRep::List(cards) => {
            let found = cards.iter().any(|c| {
                kv_map(c).get("solarSystemID").and_then(|v| v.as_i64()) == Some(NULLSEC_SYS_ID as i64)
            });
            assert!(found, "GetESSAgencyData must include active nullsec ESS system");
        }
        other => panic!("Expected List for GetESSAgencyData, got {other:?}"),
    }

    // 4. P1-01 Verification: Unlocked Reserve Bank allows linking without a key in cargo!
    let now = ctx.now_unix();
    with_mut_combat_runtime(&ctx, |rt| {
        rt.ess_reserve_unlocked_until.insert(NULLSEC_SYS_ID, now + 1800);
    });

    // Character cargo has NO reserve bank key. Because the reserve bank is unlocked, AttemptLinkToReserveBank
    // must succeed without demanding or consuming a key (`P1-01`).
    let link_res = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "AttemptLinkToReserveBank",
            &[PyRep::Integer(ESS_BALL_ID)],
            &kw,
            None,
        )
        .await;
    assert!(
        link_res.is_ok(),
        "P1-01: AttemptLinkToReserveBank must succeed without a key when bank is unlocked, got {link_res:?}"
    );

    // Verify unlocked status reflected in GetESSSystemDetails
    let updated_details = ctx
        .dispatch_rpc(
            &mut sess,
            "dynamicResourceCacheMgr",
            None,
            "GetESSSystemDetails",
            &[PyRep::Integer(NULLSEC_SYS_ID as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetESSSystemDetails after unlock");
    let upd_map = kv_map(&updated_details);
    assert_eq!(
        upd_map.get("isUnlocked").and_then(|v| v.as_bool()),
        Some(true),
        "ESSSystemDetails must reflect isUnlocked = true"
    );
    assert_eq!(
        upd_map.get("linkedCharacterID").and_then(|v| v.as_i64()),
        Some(CHAR_ID),
        "ESSSystemDetails must show character actively linked"
    );
}
