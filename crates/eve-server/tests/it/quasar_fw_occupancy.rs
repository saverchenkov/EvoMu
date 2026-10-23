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
    sess.solar_system_id = Some(30_000_142);
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
    // parity: quasar:eveProto/generated/eve_public/faction/activity/activity_pb2.pyj, quasar:eveProto/generated/eve_public/faction/activity/admin_pb2.pyj
    let reg = QuasarDescriptorRegistry::open_default().expect("registry");
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    const FW_SYSTEM_ID: u32 = 30_000_142; // Warzone system
    const NON_FW_SYSTEM_ID: u32 = 30_000_001; // Non-warzone system (Tanoo)
    const FACTION_CALDARI: u64 = 500_001;
    const FACTION_GALLENTE: u64 = 500_002;
    const DYNAMIC_THRESHOLD: i64 = 5_000;

    // 1. Seed snapshot with FW warzone occupation having a dynamic threshold of 5,000 (not default 3,000)
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.insert(
            FW_SYSTEM_ID,
            FwSystemOccupationRecord {
                solar_system_id: FW_SYSTEM_ID,
                owner_faction_id: FACTION_CALDARI as i64,
                occupier_faction_id: FACTION_GALLENTE as i64,
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

    // 2. Query valid FW system -> Returns occupier faction and victory points
    let mut get_fw_req = Vec::new();
    encode_id_field(1, FW_SYSTEM_ID as u64, &mut get_fw_req);
    let rsp = quasar_rpc(
        &ctx,
        &mut user_sess,
        "eve_public.faction.activity.GetSolarSystemScoresRequest",
        get_fw_req,
        b"corr-fw-scores-valid",
    )
    .await;
    assert_eq!(rsp.status_code, STATUS_OK, "Valid FW system query must succeed");
    let dyn_scores = reg
        .decode_dynamic(
            "eve_public.faction.activity.GetSolarSystemScoresResponse",
            &rsp.payload.unwrap().value,
        )
        .expect("decode GetSolarSystemScoresResponse");
    let Some(DynamicValue::Repeated(score_entries)) = dyn_scores.get(2) else {
        panic!("expected repeated score entries at field 2");
    };
    assert_eq!(score_entries.len(), 1);
    let DynamicValue::Message(first_score) = &score_entries[0] else {
        panic!("expected message in score entry");
    };
    // Field 1 is occupier faction (Gallente 500002)
    let Some(DynamicValue::Message(fac_msg)) = first_score.get(1) else {
        panic!("expected faction id message");
    };
    assert_eq!(fac_msg.get(1), Some(&DynamicValue::Uint64(FACTION_GALLENTE)));
    // Field 2 is victory points (1000)
    assert_eq!(first_score.get(2), Some(&DynamicValue::Uint32(1_000)));

    // 3. Query non-FW system -> Rejected with 400 Bad Request (no station-owner fallback for non-FW)
    let mut get_non_fw_req = Vec::new();
    encode_id_field(1, NON_FW_SYSTEM_ID as u64, &mut get_non_fw_req);
    let rsp_non_fw = quasar_rpc(
        &ctx,
        &mut user_sess,
        "eve_public.faction.activity.GetSolarSystemScoresRequest",
        get_non_fw_req,
        b"corr-fw-scores-non-fw",
    )
    .await;
    assert_eq!(
        rsp_non_fw.status_code, STATUS_BAD_REQUEST,
        "Non-warzone system must be rejected with 400 Bad Request"
    );

    // 4. Non-admin attempting to mutate score is rejected with 403 Forbidden
    let mut inc_req = Vec::new();
    encode_id_field(1, FW_SYSTEM_ID as u64, &mut inc_req);
    encode_id_field(2, FACTION_GALLENTE, &mut inc_req);
    encode_varint_field(3, 1_500, &mut inc_req);
    let rsp_unauth = quasar_rpc(
        &ctx,
        &mut user_sess,
        "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
        inc_req.clone(),
        b"corr-fw-inc-unauth",
    )
    .await;
    assert_eq!(
        rsp_unauth.status_code, STATUS_FORBIDDEN,
        "Non-admin score mutation must be forbidden"
    );

    // 5. Admin mutating non-FW system is rejected and does NOT write an occupation
    let mut inc_non_fw_req = Vec::new();
    encode_id_field(1, NON_FW_SYSTEM_ID as u64, &mut inc_non_fw_req);
    encode_id_field(2, FACTION_CALDARI, &mut inc_non_fw_req);
    encode_varint_field(3, 500, &mut inc_non_fw_req);
    let rsp_inc_non_fw = quasar_rpc(
        &ctx,
        &mut admin_sess,
        "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
        inc_non_fw_req,
        b"corr-fw-inc-non-fw",
    )
    .await;
    assert_eq!(
        rsp_inc_non_fw.status_code, STATUS_BAD_REQUEST,
        "Admin increment on non-warzone system must be rejected"
    );
    // Verify no occupation was spuriously inserted for non-FW system
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.fw_system_occupations.contains_key(&NON_FW_SYSTEM_ID),
            "Non-FW system must NOT have an occupation record inserted"
        );
    });

    // 6. Admin increment on valid FW system respects dynamic threshold
    // Increment by 1,500 VP -> total 2,500 VP out of 5,000 threshold = 50.0% contested
    let rsp_inc = quasar_rpc(
        &ctx,
        &mut admin_sess,
        "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
        inc_req,
        b"corr-fw-inc-admin",
    )
    .await;
    assert_eq!(rsp_inc.status_code, STATUS_OK, "Admin increment must succeed");

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
    encode_id_field(2, FACTION_GALLENTE, &mut set_req);
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
}
