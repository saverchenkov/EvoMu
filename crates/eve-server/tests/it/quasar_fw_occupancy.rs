//! Integration tests for Faction Warfare live system occupancy, dynamic threshold, and warzone scoping.

use eve_db::snapshot_ext::FwSystemOccupationRecord;
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, DynamicValue, EvePublicRequest, EvePublicResponse,
    GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_OK};
use eve_server::router::ServerContext;
use std::collections::HashMap;

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b)
                        .expect("decode EvePublicResponse from response_proto");
                }
            }
        }
    }
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

fn build_sess(sid: u64, uid: u64, cid: u32, is_admin: bool) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(uid);
    sess.character_id = Some(cid as i64);
    sess.corporation_id = Some(1_000_001);
    sess.solar_system_id = Some(eve_server::services::fw_services::HQ_SYSTEM_AMO);
    if is_admin {
        sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    }
    sess
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_url: &str,
    payload_value: Vec<u8>,
    corr: &[u8],
) -> EvePublicResponse {
    let empty_kw = HashMap::new();
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: corr.to_vec(),
        payload: Some(GoogleAny {
            type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) {
                type_url.to_string()
            } else {
                format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}")
            },
            value: payload_value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-quasar-fw".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest");
    extract_response_proto(&rep)
}

fn encode_id_field(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut id_bytes = Vec::new();
    encode_varint_field(1, id, &mut id_bytes);
    encode_bytes_field(field_num, &id_bytes, out);
}

#[tokio::test]
async fn test_fw_occupancy_warzone_scope_and_dynamic_threshold() {
    // parity: feat:quasar.faction, quasar:eveProto/generated/eve_public/faction/activity/activity_pb2.pyj, quasar:eveProto/generated/eve_public/faction/activity/admin_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.faction",
        "quasar:eveProto/generated/eve_public/faction/activity/activity_pb2.pyj",
        "quasar:eveProto/generated/eve_public/faction/activity/admin_pb2.pyj",
    ]);
    let reg = QuasarDescriptorRegistry::open_default().expect("registry");
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    const FW_SYSTEM_ID: u32 = eve_server::services::fw_services::HQ_SYSTEM_AMO; // 30002055, Amo (Metropolis FW warzone)
    const NON_FW_SYSTEM_ID: u32 = 30_000_142; // Jita (High-sec trade hub, non-warzone)
    const FACTION_CALDARI: u64 = 500_001;
    const FACTION_MINMATAR: u64 = 500_002;
    const DYNAMIC_THRESHOLD: i64 = 5_000;

    // 1. Seed snapshot with FW warzone occupation having a dynamic threshold of 5,000 (not default 3,000)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = FW_SYSTEM_ID;
        }
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.solar_system_id = FW_SYSTEM_ID;
        }
        snap.fw_system_occupations.insert(
            FW_SYSTEM_ID,
            FwSystemOccupationRecord {
                solar_system_id: FW_SYSTEM_ID,
                owner_faction_id: FACTION_CALDARI as i64,
                occupier_faction_id: FACTION_MINMATAR as i64,
                victory_points: 1_000,
                victory_points_threshold: DYNAMIC_THRESHOLD,
            },
        );
        snap.system_states.insert(
            FW_SYSTEM_ID,
            eve_db::types::SystemDynamicState {
                fw_contested_pct: 20.0, // 1000 / 5000 = 20%
                ..Default::default()
            },
        );
    });

    let mut user_sess = build_sess(60_001, 101, 90_000_001, false);
    let mut admin_sess = build_sess(60_002, 102, 90_000_002, true);
    let _rx = ctx.quasar_gateway.subscribe_session(&user_sess, Vec::new(), ctx.now_unix());

    // 2. Query valid FW system -> Returns occupier faction and victory points
    let mut get_fw_req = Vec::new();
    encode_id_field(1, FW_SYSTEM_ID as u64, &mut get_fw_req);
    let rsp = quasar_rpc(&ctx, &mut user_sess, "eve_public.faction.activity.GetSolarSystemScoresRequest", get_fw_req, b"corr-fw-scores-valid").await;
    assert_eq!(rsp.status_code, STATUS_OK, "Valid FW system query must succeed");
    let dyn_scores = reg
        .decode_dynamic("eve_public.faction.activity.GetSolarSystemScoresResponse", &rsp.payload.unwrap().value)
        .expect("decode GetSolarSystemScoresResponse");
    let Some(DynamicValue::Repeated(score_entries)) = dyn_scores.get(2) else { panic!("expected repeated score entries at field 2") };
    assert_eq!(score_entries.len(), 1);
    let DynamicValue::Message(first_score) = &score_entries[0] else { panic!("expected message in score entry") };
    let Some(DynamicValue::Message(fac_msg)) = first_score.get(1) else { panic!("expected faction id message") };
    assert_eq!(fac_msg.get(1), Some(&DynamicValue::Uint64(FACTION_MINMATAR)));
    assert_eq!(first_score.get(2), Some(&DynamicValue::Uint32(1_000)));

    // 3. Query non-FW system -> Rejected with 400 Bad Request (no station-owner fallback for non-FW)
    let mut get_non_fw_req = Vec::new();
    encode_id_field(1, NON_FW_SYSTEM_ID as u64, &mut get_non_fw_req);
    let rsp_non_fw = quasar_rpc(&ctx, &mut user_sess, "eve_public.faction.activity.GetSolarSystemScoresRequest", get_non_fw_req, b"corr-fw-scores-non-fw").await;
    assert_eq!(rsp_non_fw.status_code, STATUS_BAD_REQUEST, "Non-warzone system must be rejected with 400 Bad Request");

    // 4. Non-admin attempting to mutate score is rejected with 403 Forbidden
    let mut inc_req = Vec::new();
    encode_id_field(1, FW_SYSTEM_ID as u64, &mut inc_req);
    encode_id_field(2, FACTION_MINMATAR, &mut inc_req);
    encode_varint_field(3, 1_500, &mut inc_req);
    let rsp_unauth = quasar_rpc(&ctx, &mut user_sess, "eve_public.faction.activity.admin.IncrementContributionScoreRequest", inc_req.clone(), b"corr-fw-inc-unauth").await;
    assert_eq!(rsp_unauth.status_code, STATUS_FORBIDDEN, "Non-admin score mutation must be forbidden");

    // 5. Admin mutating non-FW system is rejected and does NOT write an occupation
    let mut inc_non_fw_req = Vec::new();
    encode_id_field(1, NON_FW_SYSTEM_ID as u64, &mut inc_non_fw_req);
    encode_id_field(2, FACTION_CALDARI, &mut inc_non_fw_req);
    encode_varint_field(3, 500, &mut inc_non_fw_req);
    let rsp_inc_non_fw = quasar_rpc(&ctx, &mut admin_sess, "eve_public.faction.activity.admin.IncrementContributionScoreRequest", inc_non_fw_req, b"corr-fw-inc-non-fw").await;
    assert_eq!(rsp_inc_non_fw.status_code, STATUS_BAD_REQUEST, "Admin increment on non-warzone system must be rejected");
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.fw_system_occupations.contains_key(&NON_FW_SYSTEM_ID), "Non-FW system must NOT have an occupation record inserted");
    });

    // 6. Admin increment on valid FW system respects dynamic threshold and emits ScoreContributionNotice + SolarSystemScoresNotice
    let rsp_inc = quasar_rpc(&ctx, &mut admin_sess, "eve_public.faction.activity.admin.IncrementContributionScoreRequest", inc_req, b"corr-fw-inc-admin").await;
    assert_eq!(rsp_inc.status_code, STATUS_OK, "Admin increment must succeed");
    let inc_notifs = ctx.quasar_gateway.poll_notices(user_sess.session_id);
    assert!(inc_notifs.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("faction.activity.ScoreContributionNotice"))));
    assert!(inc_notifs.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("faction.activity.SolarSystemScoresNotice"))));

    // Verify snapshot: dynamic threshold was used (50.0%), NOT hardcoded 3,000 (which would give 83.33%)
    ctx.db.with_snapshot(|snap| {
        let occ = snap.fw_system_occupations.get(&FW_SYSTEM_ID).expect("FW occ");
        assert_eq!(occ.victory_points, 2_500, "VP should increment to 2,500");
        assert_eq!(occ.victory_points_threshold, DYNAMIC_THRESHOLD);

        let st = snap.system_states.get(&FW_SYSTEM_ID).expect("system dynamic state");
        let expected_pct = (2_500.0 / DYNAMIC_THRESHOLD as f64) * 100.0;
        assert!(
            (st.fw_contested_pct - expected_pct).abs() < 1e-3,
            "Contested pct must be dynamically computed against threshold: expected {expected_pct}%, got {}%",
            st.fw_contested_pct
        );
    });

    // 7. Admin SetContributionScoreRequest to 4,000 VP -> 80.0% contested
    let mut set_req = Vec::new();
    encode_id_field(1, FW_SYSTEM_ID as u64, &mut set_req);
    encode_id_field(2, FACTION_MINMATAR, &mut set_req);
    encode_varint_field(3, 4_000, &mut set_req);
    let rsp_set = quasar_rpc(
        &ctx,
        &mut admin_sess,
        "eve_public.faction.activity.admin.SetContributionScoreRequest",
        set_req,
        b"corr-fw-set-admin",
    )
    .await;
    assert_eq!(rsp_set.status_code, STATUS_OK, "Admin set must succeed");

    ctx.db.with_snapshot(|snap| {
        let occ = snap.fw_system_occupations.get(&FW_SYSTEM_ID).expect("FW occ");
        assert_eq!(occ.victory_points, 4_000);
        let st = snap.system_states.get(&FW_SYSTEM_ID).expect("system dynamic state");
        let expected_pct = (4_000.0 / DYNAMIC_THRESHOLD as f64) * 100.0; // 80.0%
        assert!(
            (st.fw_contested_pct - expected_pct).abs() < 1e-3,
            "Contested pct must equal 80.0%, got {}%",
            st.fw_contested_pct
        );
    });

    // 7b. Exercise eve.faction.api.GetRequest, SearchRequest, and eve.faction.activity.*
    let mut fac_get_req = Vec::new();
    encode_id_field(1, FACTION_MINMATAR, &mut fac_get_req);
    let fac_rsp = quasar_rpc(&ctx, &mut user_sess, "eve.faction.api.GetRequest", fac_get_req, b"corr-fac-get").await;
    assert_eq!(fac_rsp.status_code, STATUS_OK);
    let fac_dyn = reg.decode_dynamic("eve.faction.api.GetResponse", &fac_rsp.payload.unwrap().value).expect("decode fac");
    let Some(DynamicValue::Message(fac_attr)) = fac_dyn.get(1) else { panic!("expected faction attributes") };
    assert!(matches!(fac_attr.get(1), Some(DynamicValue::String(s)) if s.to_lowercase().contains("minmatar")));

    let mut fac_missing_req = Vec::new();
    encode_id_field(1, 999_999, &mut fac_missing_req);
    let fac_missing_rsp = quasar_rpc(&ctx, &mut user_sess, "eve.faction.api.GetRequest", fac_missing_req, b"corr-fac-404").await;
    assert_eq!(fac_missing_rsp.status_code, 404, "Unknown faction must return 404 Not Found");

    let mut kw_msg = Vec::new();
    eve_proto::quasar::encode_string_field(1, "Minmatar", &mut kw_msg);
    let mut search_req = Vec::new();
    eve_proto::quasar::encode_bytes_field(1, &kw_msg, &mut search_req);
    let search_rsp = quasar_rpc(&ctx, &mut user_sess, "eve.faction.SearchRequest", search_req, b"corr-fac-search").await;
    assert_eq!(search_rsp.status_code, STATUS_OK);
    let search_dyn = reg.decode_dynamic("eve.faction.SearchResponse", &search_rsp.payload.unwrap().value).expect("decode search");
    assert!(search_dyn.get(1).is_some(), "SearchResponse must contain matching faction IDs");

    let mut int_scores_req = Vec::new();
    encode_id_field(1, FW_SYSTEM_ID as u64, &mut int_scores_req);
    let int_scores_rsp = quasar_rpc(&ctx, &mut user_sess, "eve.faction.activity.GetSolarSystemScoresRequest", int_scores_req, b"corr-fac-int-scores").await;
    assert_eq!(int_scores_rsp.status_code, STATUS_OK);
    let int_scores_dyn = reg.decode_dynamic("eve.faction.activity.GetSolarSystemScoresResponse", &int_scores_rsp.payload.unwrap().value).expect("decode int scores");
    let Some(DynamicValue::Repeated(scores)) = int_scores_dyn.get(2) else { panic!("expected scores") };
    let Some(DynamicValue::Message(sc0)) = scores.first() else { panic!("expected score entry") };
    assert_eq!(sc0.get(2), Some(&DynamicValue::Uint32(4_000)));
    let all_scores_rsp = quasar_rpc(&ctx, &mut user_sess, "eve.faction.activity.api.GetAllSolarSystemScoresRequest", Vec::new(), b"corr-fac-all-scores").await;
    assert_eq!(all_scores_rsp.status_code, STATUS_OK);
    let all_scores_dyn = reg.decode_dynamic("eve.faction.activity.api.GetAllSolarSystemScoresResponse", &all_scores_rsp.payload.unwrap().value).expect("decode all scores");
    assert!(all_scores_dyn.get(1).is_some());

    // 8. fwVictoryPointMgr.GetLocalVictoryPointState reads dynamic threshold (5,000, not hardcoded 3,000)
    let empty_kw = HashMap::new();
    let local_vp = ctx
        .dispatch_rpc(
            &mut user_sess,
            "fwVictoryPointMgr",
            None,
            "GetLocalVictoryPointState",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("GetLocalVictoryPointState on warzone system");
    assert_eq!(
        local_vp.dict_get("solarSystemID"),
        Some(&PyRep::Integer(FW_SYSTEM_ID as i64))
    );
    assert_eq!(
        local_vp.dict_get("victoryPoints"),
        Some(&PyRep::Integer(4_000))
    );
    assert_eq!(
        local_vp.dict_get("threshold"),
        Some(&PyRep::Integer(DYNAMIC_THRESHOLD)),
        "Threshold must dynamically equal 5000, not hardcoded 3000"
    );
    let pct = local_vp
        .dict_get("contestedPercentage")
        .and_then(|v| v.as_f64())
        .expect("contestedPercentage float");
    assert!(
        (pct - 80.0).abs() < 1e-3,
        "Contested percentage must equal 80.0%, got {pct}%"
    );

    // 9. fwVictoryPointMgr.GetAllVictoryPointStates includes dynamic threshold
    let all_vp = ctx
        .dispatch_rpc(
            &mut user_sess,
            "fwVictoryPointMgr",
            None,
            "GetAllVictoryPointStates",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("GetAllVictoryPointStates");
    let PyRep::Dict(all_vp_map) = all_vp else {
        panic!("Expected Dict from GetAllVictoryPointStates");
    };
    let fw_entry = all_vp_map
        .iter()
        .find(|(k, _)| k == &PyRep::Integer(FW_SYSTEM_ID as i64))
        .expect("FW system in GetAllVictoryPointStates");
    assert_eq!(
        fw_entry.1.dict_get("threshold"),
        Some(&PyRep::Integer(DYNAMIC_THRESHOLD))
    );

    // 10. fwWarzoneSolarsystem.GetLocalOccupationState & GetAllWarzonesOccupationStates
    let local_occ = ctx
        .dispatch_rpc(
            &mut user_sess,
            "fwWarzoneSolarsystem",
            None,
            "GetLocalOccupationState",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("GetLocalOccupationState");
    match local_occ {
        PyRep::Tuple(pair) => {
            assert_eq!(pair.first().and_then(|v| v.as_i64()), Some(FW_SYSTEM_ID as i64));
            let occ_rep = pair.get(1).expect("occupation state");
            assert_eq!(
                occ_rep.dict_get("occupierFactionID"),
                Some(&PyRep::Integer(FACTION_MINMATAR as i64))
            );
            assert_eq!(
                occ_rep.dict_get("frontlineStatus"),
                Some(&PyRep::Integer(eve_server::services::fw_services::STATUS_REARGUARD)),
                "Amo is naturally Rearguard when surrounded by friendly systems"
            );
        }
        other => panic!("Expected Tuple, got {other:?}"),
    }

    // Dynamic frontline shift: simulate enemy Amarr conquest of adjacent system 30002056
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(occ) = snap.fw_system_occupations.get_mut(&30002056) {
            occ.occupier_faction_id = eve_server::services::fw_services::FACTION_AMARR;
        }
    });
    let local_occ_front = ctx
        .dispatch_rpc(
            &mut user_sess,
            "fwWarzoneSolarsystem",
            None,
            "GetLocalOccupationState",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("GetLocalOccupationState when adjacent to enemy");
    match local_occ_front {
        PyRep::Tuple(pair) => {
            let occ_rep = pair.get(1).expect("occupation state");
            assert_eq!(
                occ_rep.dict_get("frontlineStatus"),
                Some(&PyRep::Integer(eve_server::services::fw_services::STATUS_FRONTLINE)),
                "Amo dynamically transitions to Frontline when adjacent to enemy-held system"
            );
        }
        other => panic!("Expected Tuple, got {other:?}"),
    }
    // Restore 30002056 back to Minmatar
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(occ) = snap.fw_system_occupations.get_mut(&30002056) {
            occ.occupier_faction_id = FACTION_MINMATAR as i64;
        }
    });

    let all_occ = ctx
        .dispatch_rpc(
            &mut user_sess,
            "fwWarzoneSolarsystem",
            None,
            "GetAllWarzonesOccupationStates",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("GetAllWarzonesOccupationStates");
    match all_occ {
        PyRep::List(list) => {
            assert!(!list.is_empty(), "Warzone list should not be empty");
            let fw_entry = list
                .iter()
                .find(|item| item.dict_get("solarSystemID") == Some(&PyRep::Integer(FW_SYSTEM_ID as i64)))
                .expect("FW system present in GetAllWarzonesOccupationStates");
            assert_eq!(
                fw_entry.dict_get("occupierFactionID"),
                Some(&PyRep::Integer(FACTION_MINMATAR as i64))
            );
            assert_eq!(
                fw_entry.dict_get("frontlineStatus"),
                Some(&PyRep::Integer(eve_server::services::fw_services::STATUS_REARGUARD))
            );

            // Natural frontline system (30002087, border system with Amarr)
            let frontline_entry = list
                .iter()
                .find(|item| item.dict_get("solarSystemID") == Some(&PyRep::Integer(30002087)))
                .expect("30002087 in GetAllWarzonesOccupationStates");
            assert_eq!(
                frontline_entry.dict_get("frontlineStatus"),
                Some(&PyRep::Integer(eve_server::services::fw_services::STATUS_FRONTLINE))
            );

            // Natural command system (30002086, adjacent to frontline 30002087)
            let command_entry = list
                .iter()
                .find(|item| item.dict_get("solarSystemID") == Some(&PyRep::Integer(30002086)))
                .expect("30002086 in GetAllWarzonesOccupationStates");
            assert_eq!(
                command_entry.dict_get("frontlineStatus"),
                Some(&PyRep::Integer(eve_server::services::fw_services::STATUS_COMMAND))
            );

            let non_fw = list
                .iter()
                .find(|item| item.dict_get("solarSystemID") == Some(&PyRep::Integer(NON_FW_SYSTEM_ID as i64)));
            assert!(non_fw.is_none(), "Non-warzone system must not be in GetAllWarzonesOccupationStates");
        }
        other => panic!("Expected List, got {other:?}"),
    }

    let fw_keys = ctx
        .dispatch_rpc(
            &mut user_sess,
            "fwWarzoneSolarsystem",
            None,
            "keys",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("fwWarzoneSolarsystem.keys");
    match fw_keys {
        PyRep::List(list) => {
            assert!(list.contains(&PyRep::Integer(FW_SYSTEM_ID as i64)));
            assert!(!list.contains(&PyRep::Integer(NON_FW_SYSTEM_ID as i64)));
        }
        other => panic!("Expected List, got {other:?}"),
    }

    let fw_items = ctx
        .dispatch_rpc(
            &mut user_sess,
            "fwWarzoneSolarsystem",
            None,
            "iteritems",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("fwWarzoneSolarsystem.iteritems");
    match fw_items {
        PyRep::List(list) => {
            let found = list.iter().any(|item| match item {
                PyRep::Tuple(pair) => pair.first() == Some(&PyRep::Integer(FW_SYSTEM_ID as i64)),
                _ => false,
            });
            assert!(found, "FW system must be present in iteritems");
            let not_found = list.iter().any(|item| match item {
                PyRep::Tuple(pair) => pair.first() == Some(&PyRep::Integer(NON_FW_SYSTEM_ID as i64)),
                _ => false,
            });
            assert!(!not_found, "Non-warzone system must not be in iteritems");
        }
        other => panic!("Expected List, got {other:?}"),
    }

    // 11. Non-warzone solar system (Jita) returns None for both local RPCs
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = NON_FW_SYSTEM_ID;
        }
    });
    user_sess.solar_system_id = Some(NON_FW_SYSTEM_ID);
    let non_fw_vp = ctx
        .dispatch_rpc(
            &mut user_sess,
            "fwVictoryPointMgr",
            None,
            "GetLocalVictoryPointState",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("GetLocalVictoryPointState on non-warzone system");
    assert_eq!(non_fw_vp, PyRep::None, "Non-warzone system must return None");

    let non_fw_occ = ctx
        .dispatch_rpc(
            &mut user_sess,
            "fwWarzoneSolarsystem",
            None,
            "GetLocalOccupationState",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("GetLocalOccupationState on non-warzone system");
    match non_fw_occ {
        PyRep::Tuple(pair) => {
            assert_eq!(pair.first().and_then(|v| v.as_i64()), Some(NON_FW_SYSTEM_ID as i64));
            assert_eq!(pair.get(1), Some(&PyRep::None), "Non-warzone occupation state must be None");
        }
        other => panic!("Expected Tuple, got {other:?}"),
    }

    // 12. add_victory_points: strictly warzone scoped and dynamic threshold
    let (non_vp, non_pct) = eve_server::services::fw_services::add_victory_points(&ctx, NON_FW_SYSTEM_ID, 500);
    assert_eq!(non_vp, 0, "add_victory_points on non-warzone system must return 0");
    assert_eq!(non_pct, 0.0);

    let (added_vp, added_pct) = eve_server::services::fw_services::add_victory_points(&ctx, FW_SYSTEM_ID, 500);
    assert_eq!(added_vp, 4_500, "VP increments to 4,500");
    let expected_added_pct = (4_500.0 / DYNAMIC_THRESHOLD as f64) * 100.0; // 90.0%
    assert!(
        (added_pct - expected_added_pct).abs() < 1e-3,
        "Contested pct must equal 90.0%, got {added_pct}%"
    );
}

#[tokio::test]
async fn test_fw_sqlite_production_world_initialization() {
    let tmp_db = std::env::temp_dir().join(format!("evomu_fw_sqlite_test_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&tmp_db);
    let sde_path = std::env::var("EVOMU_SDE_PATH")
        .map(std::path::PathBuf::from)
        .ok()
        .filter(|p| p.exists())
        .unwrap_or_else(|| {
            let default_p = eve_sde::SdeRegistry::default_db_path();
            if default_p.exists() {
                default_p
            } else {
                std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/evomu_sde.sqlite"))
            }
        });
    let sde_path_toml = sde_path.to_string_lossy().replace('\\', "/");
    let tmp_db_toml = tmp_db.to_string_lossy().replace('\\', "/");

    let custom_toml = format!(
        r#"
bind_address = "127.0.0.1"
macho_port = 0
bridge_port = 0
esi_port = 0
sde_path = "{}"
world_db_path = "{}"

[feature_flags]
enable_quasar_gateway = true
enable_esi_http = false
enable_living_world = false
seed_demo = false
"#,
        sde_path_toml, tmp_db_toml
    );
    let parsed_cfg = eve_server::config::ServerConfig::from_toml_str(&custom_toml)
        .expect("ServerConfig::from_toml_str");
    let ctx = ServerContext::from_config(&parsed_cfg)
        .expect("ServerContext::from_config with SQLite world");

    // 1. Assert all 175 FW warzone systems are backfilled with default threshold 75,000 on fresh SQLite store
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.fw_system_occupations.len(),
            175,
            "Fresh production SQLite store must backfill all 175 warzone systems"
        );
        for &(sys_id, owner) in eve_db::DEFAULT_FW_WARZONE_SYSTEMS {
            let occ = snap
                .fw_system_occupations
                .get(&sys_id)
                .expect("FW warzone system present in SQLite store");
            assert_eq!(occ.solar_system_id, sys_id);
            assert_eq!(occ.owner_faction_id, owner);
            assert_eq!(occ.occupier_faction_id, owner);
            assert_eq!(occ.victory_points, 0);
            assert_eq!(occ.victory_points_threshold, 75_000);
        }
    });

    // 2. Assert GetLocalOccupationState returns valid warzone state on SQLite store
    let mut sess = SessionState::new(99_001);
    sess.character_id = Some(90_000_001);
    sess.solar_system_id = Some(30002055); // Amo
    let kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(
            &mut sess,
            "fwWarzoneSolarsystem",
            None,
            "GetLocalOccupationState",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetLocalOccupationState on SQLite store");
    match rep {
        PyRep::Tuple(pair) => {
            let occ_rep = pair.get(1).expect("occupation state");
            assert_eq!(
                occ_rep.dict_get("solarSystemID"),
                Some(&PyRep::Integer(30002055))
            );
            assert_eq!(
                occ_rep.dict_get("frontlineStatus"),
                Some(&PyRep::Integer(eve_server::services::fw_services::STATUS_REARGUARD))
            );
            assert_eq!(
                occ_rep.dict_get("occupierFactionID"),
                Some(&PyRep::Integer(500002))
            );
        }
        other => panic!("Expected Tuple, got {other:?}"),
    }

    // 3. Assert GetLocalVictoryPointState returns 75_000 threshold on SQLite store
    let vp_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "fwVictoryPointMgr",
            None,
            "GetLocalVictoryPointState",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetLocalVictoryPointState on SQLite store");
    assert_eq!(
        vp_rep.dict_get("threshold"),
        Some(&PyRep::Integer(75_000)),
        "SQLite world GetLocalVictoryPointState must report 75,000 threshold"
    );
    assert_eq!(
        vp_rep.dict_get("solarSystemID"),
        Some(&PyRep::Integer(30002055))
    );

    let _ = std::fs::remove_file(&tmp_db);
}
