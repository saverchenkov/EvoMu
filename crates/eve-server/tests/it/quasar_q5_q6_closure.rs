//! Integration tests for Quasar Q-5 (Social & Organization) and Q-6 (Economy, Industry & Planetary) package closure.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use std::collections::HashMap;

fn make_session(sid: u64, uid: u64, cid: i64, corp_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(uid);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(30_000_142);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    sess
}

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

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    payload_bytes: Vec<u8>,
) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"q5-q6-closure-01".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload_bytes,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-q5-q6-test".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let empty_kw = HashMap::new();
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

#[tokio::test]
async fn test_q5_q6_social_and_economy_package_closure() {
    // parity: plan:Q-5, plan:Q-6
    let ctx = ServerContext::new();
    let mut sess = make_session(1, 1, 90_000_001, 1_000_009);
    let mut unauth = SessionState::new(2);

    // 1. corporation package: skill plans
    let unauth_corp = quasar_rpc(
        &ctx,
        &mut unauth,
        "eve_public.corporation.skill.plan.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_ne!(unauth_corp.status_code, STATUS_OK);

    let corp_plans = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.corporation.skill.plan.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(corp_plans.status_code, STATUS_OK);
    assert_eq!(
        corp_plans.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.corporation.skill.plan.GetAllResponse"
    );

    // 2. corporationgoal package: active/all goals
    let corp_goals = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.corporationgoal.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(corp_goals.status_code, STATUS_OK);
    assert_eq!(
        corp_goals.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.corporationgoal.api.GetAllResponse"
    );

    // 3. freelance package: active projects & creation limits
    let freelance_active = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.freelance.project.api.GetAllActiveRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(freelance_active.status_code, STATUS_OK);
    assert_eq!(
        freelance_active.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.freelance.project.api.GetAllActiveResponse"
    );

    let freelance_limits = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.freelance.project.api.GetCreationLimitsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(freelance_limits.status_code, STATUS_OK);
    assert_eq!(
        freelance_limits.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.freelance.project.api.GetCreationLimitsResponse"
    );

    // 4. Verify every quasar: row in Q-5 and Q-6 packages is DONE or NA in parity.json
    let q5_q6_packages = [
        // Q-5: Social and organization
        "corporation",
        "corporationgoal",
        "freelance",
        "alliance",
        "contact",
        "standing",
        "mail",
        "chat",
        "notification",
        "calendar",
        "fleet",
        "medal",
        "presence",
        "search",
        "bookmark",
        "tag",
        "war",
        "duel",
        "securitystatus",
        // Q-6: Economy and industry
        "market",
        "contract",
        "industry",
        "blueprint",
        "asset",
        "assetholding",
        "inventory",
        "container",
        "accounting",
        "loyaltypoints",
        "planet",
        "planetinteraction",
        "ore",
        "moon",
        "asteroidbelt",
    ];
    let candidate_paths = [
        std::path::PathBuf::from(".evomu-reviews/parity.json"),
        std::path::PathBuf::from(".evomu-reviews/preflight/parity.json"),
        std::path::PathBuf::from(".agents/skills/evomu-adversarial-review/references/parity/auditor_rows.json"),
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.evomu-reviews/parity.json"),
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.evomu-reviews/preflight/parity.json"),
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.agents/skills/evomu-adversarial-review/references/parity/auditor_rows.json"),
    ];
    let parity_path = candidate_paths
        .into_iter()
        .find(|p| p.exists())
        .expect("parity.json or auditor_rows.json must exist");
    let is_auditor_rows = parity_path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "auditor_rows.json");
    let raw = std::fs::read_to_string(&parity_path).expect("read parity file");
    let val: serde_json::Value = serde_json::from_str(&raw).expect("parse parity file");
    let rows_val = val.get("rows").unwrap_or(&val);
    let mut checked_rows = 0usize;
    let mut check_row = |id: &str, status: &str| {
        if let Some(rest) = id.strip_prefix("quasar:") {
            let segs: Vec<&str> = rest.split('/').collect();
            let pkg = segs
                .iter()
                .position(|&s| s == "eve_public" || s == "eve")
                .and_then(|idx| segs.get(idx + 1).copied())
                .filter(|seg| !seg.ends_with(".pyj") && !seg.ends_with(".py"))
                .or_else(|| {
                    rest.strip_prefix("eve_public.")
                        .or_else(|| rest.strip_prefix("eve."))
                        .and_then(|s| s.split('.').next())
                });
            if let Some(pkg) = pkg {
                if q5_q6_packages.contains(&pkg) {
                    checked_rows += 1;
                    if is_auditor_rows {
                        panic!("Q-5/Q-6 quasar row {id} (pkg={pkg}) must not be open/flagged in auditor_rows.json");
                    }
                    assert!(
                        matches!(status, "DONE" | "NA"),
                        "Q-5/Q-6 quasar row {id} (pkg={pkg}) must be DONE or NA, got {status}"
                    );
                }
            }
        }
    };
    if let Some(obj) = rows_val.as_object() {
        for (k, v) in obj {
            let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("");
            check_row(k, status);
        }
    } else if let Some(arr) = rows_val.as_array() {
        for item in arr {
            let id = item.get("id").and_then(|s| s.as_str()).unwrap_or("");
            let status = item.get("status").and_then(|s| s.as_str()).unwrap_or("");
            check_row(id, status);
        }
    }
    if !is_auditor_rows {
        assert!(
            checked_rows > 0,
            "expected at least one Q-5/Q-6 quasar row in parity.json"
        );
    }
}
