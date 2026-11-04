//! Parity closure integration tests for Project Discovery RPCs and Quasar Sovereignty Planet API requests:
//! - `rpc:ProjectDiscovery.get_data_from_url`
//! - `rpc:ProjectDiscovery.get_tutorial_xp_reward`
//! - `rpc:ProjectDiscovery.new_training_task`
//! - `quasar:eveProto/generated/eve_public/sovereignty/resource/planet/api/requests_pb2.pyj`

#![forbid(unsafe_code)]

use crate::common;
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, EvePublicRequest, EvePublicResponse,
    GoogleAny, GoogleTimestamp, QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_NOT_FOUND, STATUS_OK, STATUS_UNAUTHORIZED};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT: i64 = 90_000_001;
const NULLSEC_SYSTEM: u32 = 30_004_759;

async fn select_pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    s
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn quasar_sess(sid: u64, cid: Option<i64>, corp: Option<i64>, sys: Option<u32>) -> SessionState {
    let mut s = SessionState::new(sid);
    s.user_id = Some(1);
    s.character_id = cid;
    s.corporation_id = corp;
    s.solar_system_id = sys;
    s.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    s
}

async fn quasar_rpc(ctx: &ServerContext, s: &mut SessionState, ty: &str, value: Vec<u8>) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: b"planet-closure-test".to_vec(),
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{ty}"), value }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-planet-closure".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx
        .dispatch_rpc(s, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &HashMap::new(), None)
        .await
        .expect("SendRequest");
    let PyRep::KeyVal(entries) = &rep else { panic!("expected KeyVal from SendRequest, got {rep:?}") };
    let bytes = entries.iter().find_map(|(k, v)| match (k.as_str(), v) {
        ("response_proto", PyRep::Bytes(b)) => Some(b.clone()),
        _ => None,
    }).expect("response_proto in SendRequest reply");
    EvePublicResponse::decode_proto(&bytes).expect("decode EvePublicResponse")
}

fn wm(bytes: &[u8]) -> WireMessage {
    WireMessage::decode(bytes).expect("decode WireMessage")
}

fn fields(m: &WireMessage, field: u32) -> Vec<WireMessage> {
    m.fields.iter().filter(|f| f.field_number == field).filter_map(|f| f.value.as_bytes())
        .map(|b| WireMessage::decode(b).expect("decode repeated field")).collect()
}

fn field_msg(m: &WireMessage, field: u32) -> Option<WireMessage> {
    m.fields.iter().find(|f| f.field_number == field).and_then(|f| f.value.as_bytes())
        .map(|b| WireMessage::decode(b).expect("decode submessage"))
}

fn varint(m: &WireMessage, field: u32) -> Option<u64> {
    m.fields.iter().find(|f| f.field_number == field).and_then(|f| f.value.as_varint())
}

#[tokio::test]
async fn test_project_discovery_parity_closure() {
    // parity: rpc:ProjectDiscovery.get_data_from_url, rpc:ProjectDiscovery.get_tutorial_xp_reward, rpc:ProjectDiscovery.new_training_task
    eve_testkit::attest(&[
        "rpc:ProjectDiscovery.get_data_from_url",
        "rpc:ProjectDiscovery.get_tutorial_xp_reward",
        "rpc:ProjectDiscovery.new_training_task",
    ]);

    let ctx = ServerContext::new();
    let mut sess = select_pilot(&ctx, 62_001, "demo", "demo-secret", PILOT).await;

    // =========================================================================
    // 1. rpc:ProjectDiscovery.get_tutorial_xp_reward
    // =========================================================================
    // In unseeded default state, tutorial XP reward returns 0.0
    let unseeded_reward = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_tutorial_xp_reward", &[]).await.expect("get_tutorial_xp_reward unseeded");
    assert_eq!(unseeded_reward, PyRep::Real(0.0));

    // Also accepts optional integer argument
    let unseeded_with_arg = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_tutorial_xp_reward", &[PyRep::Integer(1)]).await.expect("get_tutorial_xp_reward unseeded with arg");
    assert_eq!(unseeded_with_arg, PyRep::Real(0.0));

    // Seed MMOS configuration: 2500.0 xp per rank, 350.0 tutorial xp reward
    ctx.db.with_mut_snapshot(|snap| {
        eve_server::services::project_discovery::configure_project_discovery_mmos(
            snap, 2_500.0, 350.0, 180.0, 90.0, 60,
        );
    });

    let configured_reward = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_tutorial_xp_reward", &[]).await.expect("get_tutorial_xp_reward configured");
    assert_eq!(configured_reward, PyRep::Real(350.0));

    // Verify tutorial reward state transitions with tutorial levels:
    // Initial tutorial_level is 1; not entitled to tutorial reward yet.
    let init_lvl = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_tutorial_level", &[]).await.expect("get_tutorial_level init");
    assert_eq!(init_lvl, PyRep::Integer(1));

    let entitled_init = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_is_player_entitled_to_tutorial_reward", &[]).await.expect("get_is_player_entitled_to_tutorial_reward init");
    assert_eq!(entitled_init, PyRep::Bool(false));

    // Claim attempt before completion fails and does not change player XP
    let claim_early = rpc(&ctx, &mut sess, "ProjectDiscovery", "give_tutorial_rewards", &[]).await.expect("give_tutorial_rewards early");
    assert_eq!(claim_early, PyRep::Bool(false));

    let state_early = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_player_state", &[]).await.expect("get_player_state early");
    assert_eq!(state_early.dict_get("experience").and_then(|v| v.as_f64()), Some(0.0));

    // Advance tutorial through levels 2, 3, 4, 5
    for lvl in 2..=5 {
        let advanced = rpc(&ctx, &mut sess, "ProjectDiscovery", "increase_tutorial_level", &[]).await.expect("increase_tutorial_level");
        assert_eq!(advanced, PyRep::Integer(lvl));
    }

    let comp_status = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_tutorial_completion_status", &[]).await.expect("get_tutorial_completion_status at lvl 5");
    assert_eq!(comp_status, PyRep::Bool(true));

    let entitled_comp = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_is_player_entitled_to_tutorial_reward", &[]).await.expect("get_is_player_entitled_to_tutorial_reward at lvl 5");
    assert_eq!(entitled_comp, PyRep::Bool(true));

    // Claim tutorial rewards: grants exactly configured tutorial_xp_reward (350.0 XP)
    let claim_done = rpc(&ctx, &mut sess, "ProjectDiscovery", "give_tutorial_rewards", &[]).await.expect("give_tutorial_rewards at lvl 5");
    assert_eq!(claim_done, PyRep::Bool(true));

    let state_claimed = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_player_state", &[]).await.expect("get_player_state after claim");
    assert_eq!(state_claimed.dict_get("experience").and_then(|v| v.as_f64()), Some(350.0));

    // Repeat claim attempt fails (idempotent, no double reward)
    let claim_repeat = rpc(&ctx, &mut sess, "ProjectDiscovery", "give_tutorial_rewards", &[]).await.expect("give_tutorial_rewards repeat");
    assert_eq!(claim_repeat, PyRep::Bool(false));

    let state_claimed_again = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_player_state", &[]).await.expect("get_player_state after repeat");
    assert_eq!(state_claimed_again.dict_get("experience").and_then(|v| v.as_f64()), Some(350.0));

    // =========================================================================
    // 2. rpc:ProjectDiscovery.new_training_task
    // =========================================================================
    // Reset tutorial level to 1 to test clean level-based task generation
    let reset_res = rpc(&ctx, &mut sess, "ProjectDiscovery", "reset_tutorial", &[]).await.expect("reset_tutorial");
    assert_eq!(reset_res, PyRep::Bool(true));

    let lvl_after_reset = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_tutorial_level", &[]).await.expect("get_tutorial_level reset");
    assert_eq!(lvl_after_reset, PyRep::Integer(1));

    // 2a. Call with no arguments -> defaults to current tutorial level 1
    let train_default = rpc(&ctx, &mut sess, "ProjectDiscovery", "new_training_task", &[]).await.expect("new_training_task no args");
    let PyRep::KeyVal(t1_kv) = train_default else { panic!("expected KeyVal from new_training_task") };
    assert_eq!(t1_kv.get("task_id").and_then(|v| v.as_str()), Some("training_task_01"));
    assert_eq!(t1_kv.get("taskId").and_then(|v| v.as_str()), Some("training_task_01"));
    assert_eq!(t1_kv.get("taskID").and_then(|v| v.as_str()), Some("training_task_01"));
    assert_eq!(t1_kv.get("level").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(t1_kv.get("task_number").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(t1_kv.get("solution"), Some(&PyRep::List(vec![PyRep::Integer(1)])));

    // 2b. Call with non-positive arguments (0 or -5) -> falls back to current tutorial level
    let train_zero = rpc(&ctx, &mut sess, "ProjectDiscovery", "new_training_task", &[PyRep::Integer(0)]).await.expect("new_training_task arg 0");
    assert_eq!(train_zero.dict_get("task_number").and_then(|v| v.as_i64()), Some(1));

    let train_neg = rpc(&ctx, &mut sess, "ProjectDiscovery", "new_training_task", &[PyRep::Integer(-5)]).await.expect("new_training_task arg -5");
    assert_eq!(train_neg.dict_get("task_number").and_then(|v| v.as_i64()), Some(1));

    // 2c. Call with explicit task number (e.g. 4) while level is 1
    let train_task4 = rpc(&ctx, &mut sess, "ProjectDiscovery", "new_training_task", &[PyRep::Integer(4)]).await.expect("new_training_task arg 4");
    assert_eq!(train_task4.dict_get("task_id").and_then(|v| v.as_str()), Some("training_task_04"));
    assert_eq!(train_task4.dict_get("task_number").and_then(|v| v.as_i64()), Some(4));
    assert_eq!(train_task4.dict_get("level").and_then(|v| v.as_i64()), Some(1));

    // 2d. Advance tutorial level to 3 and verify no-arg call tracks the new level
    let _ = rpc(&ctx, &mut sess, "ProjectDiscovery", "increase_tutorial_level", &[]).await.expect("inc to 2");
    let _ = rpc(&ctx, &mut sess, "ProjectDiscovery", "increase_tutorial_level", &[]).await.expect("inc to 3");
    let train_lvl3 = rpc(&ctx, &mut sess, "ProjectDiscovery", "new_training_task", &[]).await.expect("new_training_task at lvl 3");
    assert_eq!(train_lvl3.dict_get("task_id").and_then(|v| v.as_str()), Some("training_task_03"));
    assert_eq!(train_lvl3.dict_get("task_number").and_then(|v| v.as_i64()), Some(3));
    assert_eq!(train_lvl3.dict_get("level").and_then(|v| v.as_i64()), Some(3));

    // =========================================================================
    // 3. rpc:ProjectDiscovery.get_data_from_url
    // =========================================================================
    // 3a. Error conditions:
    let err_empty = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_data_from_url", &[PyRep::String("".into())]).await.unwrap_err();
    assert_eq!(err_empty.code.to_ascii_lowercase(), "invalidtaskurl");

    let err_ws = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_data_from_url", &[PyRep::String("   ".into())]).await.unwrap_err();
    assert_eq!(err_ws.code.to_ascii_lowercase(), "invalidtaskurl");

    let err_foreign = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_data_from_url", &[PyRep::String("https://untrusted.example.com/tasks/1".into())]).await.unwrap_err();
    assert_eq!(err_foreign.code.to_ascii_lowercase(), "invalidtaskurl");

    let err_non_num = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_data_from_url", &[PyRep::String("https://projectdiscovery.eveonline.com/tasks/invalid".into())]).await.unwrap_err();
    assert_eq!(err_non_num.code.to_ascii_lowercase(), "invalidtaskurl");

    let err_zero = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_data_from_url", &[PyRep::String("https://projectdiscovery.eveonline.com/tasks/0".into())]).await.unwrap_err();
    assert_eq!(err_zero.code.to_ascii_lowercase(), "invalidtaskurl");

    let err_neg = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_data_from_url", &[PyRep::String("https://projectdiscovery.eveonline.com/tasks/-10".into())]).await.unwrap_err();
    assert_eq!(err_neg.code.to_ascii_lowercase(), "invalidtaskurl");

    // Task number not yet issued to player (e.g. 9999) -> TaskNotFound
    let err_not_found = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_data_from_url", &[PyRep::String("https://projectdiscovery.eveonline.com/tasks/9999".into())]).await.unwrap_err();
    assert_eq!(err_not_found.code.to_ascii_lowercase(), "tasknotfound");

    // 3b. Issue a new task via get_new_task and fetch its data
    let task_issued = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_new_task", &[]).await.expect("get_new_task");
    let issued_url = task_issued.dict_get("url").and_then(|v| v.as_str()).expect("task url").to_string();
    let issued_tid = task_issued.dict_get("task_id").and_then(|v| v.as_str()).expect("task id").to_string();
    let issued_num = task_issued.dict_get("task_number").and_then(|v| v.as_i64()).expect("task num");
    assert_eq!(issued_num, 1);
    assert_eq!(issued_tid, "task_1");

    // First fetch: fetch_count starts at 1
    let data_resp1 = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_data_from_url", &[PyRep::String(issued_url.clone())]).await.expect("get_data_from_url first fetch");
    let PyRep::String(json_str1) = data_resp1 else { panic!("expected PyRep::String from get_data_from_url") };
    let parsed1: serde_json::Value = serde_json::from_str(&json_str1).expect("valid json response");
    assert_eq!(parsed1["task_id"], "task_1");
    assert_eq!(parsed1["task_number"], 1);
    assert_eq!(parsed1["project_id"], "mcgill-polyseg");
    assert_eq!(parsed1["samples"], 10);
    assert_eq!(parsed1["fetch_count"], 1);

    // Second fetch: state transition increments fetch_count to 2
    let data_resp2 = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_data_from_url", &[PyRep::String(issued_url.clone())]).await.expect("get_data_from_url second fetch");
    let parsed2: serde_json::Value = serde_json::from_str(data_resp2.as_str().unwrap()).expect("valid json 2");
    assert_eq!(parsed2["fetch_count"], 2);

    // Alternate URL format with task_1 identifier and trailing slash also resolves and increments fetch_count
    let alt_url = "https://projectdiscovery.eveonline.com/mcgill-polyseg/tasks/task_1/";
    let data_resp3 = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_data_from_url", &[PyRep::String(alt_url.into())]).await.expect("get_data_from_url alternate url");
    let parsed3: serde_json::Value = serde_json::from_str(data_resp3.as_str().unwrap()).expect("valid json 3");
    assert_eq!(parsed3["fetch_count"], 3);

    // 3c. Issue next task (task 2) and verify previous task is still queryable in player task history
    let task2 = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_new_task", &[]).await.expect("get_new_task 2");
    let task2_num = task2.dict_get("task_number").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(task2_num, 2);

    let task2_url = task2.dict_get("url").and_then(|v| v.as_str()).unwrap().to_string();
    let data_task2 = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_data_from_url", &[PyRep::String(task2_url)]).await.expect("get_data_from_url task 2");
    let parsed_t2: serde_json::Value = serde_json::from_str(data_task2.as_str().unwrap()).unwrap();
    assert_eq!(parsed_t2["task_id"], "task_2");
    assert_eq!(parsed_t2["task_number"], 2);
    assert_eq!(parsed_t2["fetch_count"], 1);

    // Fetch previous task 1 again from cache
    let data_task1_again = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_data_from_url", &[PyRep::String(issued_url)]).await.expect("get_data_from_url task 1 again");
    let parsed_t1_again: serde_json::Value = serde_json::from_str(data_task1_again.as_str().unwrap()).unwrap();
    assert_eq!(parsed_t1_again["task_number"], 1);
    assert_eq!(parsed_t1_again["fetch_count"], 4);
}

#[tokio::test]
async fn test_quasar_sovereignty_resource_planet_requests() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/resource/planet/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/sovereignty/resource/planet/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();

    // Prepare pilot in NULLSEC_SYSTEM
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT) {
            ch.solar_system_id = NULLSEC_SYSTEM;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });

    let mut sess = quasar_sess(62_010, Some(PILOT), Some(98_000_001), Some(NULLSEC_SYSTEM));

    // =========================================================================
    // 1. GetAllDefinitionsRequest
    // =========================================================================
    // 1a. Unconditioned request (empty payload) -> returns STATUS_OK, GetAllDefinitionsResponse, 8 planet definitions
    let defs_resp = quasar_rpc(&ctx, &mut sess, "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest", Vec::new()).await;
    assert_eq!(defs_resp.status_code, STATUS_OK);
    let payload = defs_resp.payload.expect("payload present");
    assert_eq!(payload.type_url, format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.resource.planet.api.GetAllDefinitionsResponse"));
    assert!(!payload.value.is_empty(), "definitions payload must not be empty");

    let defs_wm = wm(&payload.value);
    let planet_defs = fields(&defs_wm, 1);
    assert_eq!(planet_defs.len(), 8, "Expected 8 planetary definition entries");

    for pdef in &planet_defs {
        let id_sub = field_msg(pdef, 1).expect("Identifier submessage");
        let type_id = varint(&id_sub, 1).expect("type_id") as u32;
        let expected = eve_server::services::quasar_sov_hub::PLANET_RESOURCE_TABLE.iter().find(|row| row.0 == type_id).expect("known planet type");
        let power = varint(pdef, 2).unwrap_or(0);
        let workforce = varint(pdef, 3).unwrap_or(0);
        assert_eq!(power, expected.2);
        assert_eq!(workforce, expected.3);

        // If reagent definition (field 4) is present, check nested configuration
        if let Some(r_def) = field_msg(pdef, 4) {
            let r_id = field_msg(&r_def, 1).expect("reagent Identifier");
            let reagent_type_id = varint(&r_id, 1).expect("reagent type_id");
            assert!(reagent_type_id > 0);
            let amt = field_msg(&r_def, 2).expect("reagent yield amount");
            let rate = varint(&amt, 1).expect("hourly yield");
            assert_eq!(rate, eve_server::services::sov_gameplay::SKYHOOK_HOURLY_REAGENT_YIELD);
        }
    }

    // Specification version in field 2: major = 1, minor = 0, patch = 0
    let ver_spec = field_msg(&defs_wm, 2).expect("Specification version");
    assert_eq!(varint(&ver_spec, 1), Some(1)); // major
    assert_eq!(varint(&ver_spec, 2), Some(0)); // minor
    assert_eq!(varint(&ver_spec, 3), Some(0)); // patch

    // 1b. Cache negotiation: when client supplies known version (1.0.0), definitions list is omitted
    let mut known_req = Vec::new();
    let mut ver_buf = Vec::new();
    encode_varint_field(1, 1, &mut ver_buf); // major
    encode_varint_field(2, 0, &mut ver_buf); // minor
    encode_varint_field(3, 0, &mut ver_buf); // patch
    encode_bytes_field(1, &ver_buf, &mut known_req); // field 1: known_version

    let cache_hit_resp = quasar_rpc(&ctx, &mut sess, "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest", known_req).await;
    assert_eq!(cache_hit_resp.status_code, STATUS_OK);
    let cache_hit_payload = cache_hit_resp.payload.expect("payload on cache hit");
    let cache_wm = wm(&cache_hit_payload.value);
    assert!(fields(&cache_wm, 1).is_empty(), "Cached definitions must be omitted on version match");
    assert!(field_msg(&cache_wm, 2).is_some(), "Specification version still returned");

    // 1c. Explicit no_known_version = true in oneof -> returns all 8 definitions
    let mut no_known_req = Vec::new();
    encode_varint_field(2, 1, &mut no_known_req); // field 2: no_known_version = true
    let no_known_resp = quasar_rpc(&ctx, &mut sess, "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest", no_known_req).await;
    assert_eq!(no_known_resp.status_code, STATUS_OK);
    let no_known_payload = no_known_resp.payload.expect("payload present");
    let no_known_wm = wm(&no_known_payload.value);
    assert_eq!(fields(&no_known_wm, 1).len(), 8, "All definitions returned when no_known_version is true");

    // =========================================================================
    // 2. GetDefinitionsVersionRequest
    // =========================================================================
    // 2a. Default request (empty payload) -> returns STATUS_OK, GetDefinitionsVersionResponse, 8 definitions
    let ver_req_resp = quasar_rpc(&ctx, &mut sess, "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest", Vec::new()).await;
    assert_eq!(ver_req_resp.status_code, STATUS_OK);
    let ver_payload = ver_req_resp.payload.expect("payload present");
    assert_eq!(ver_payload.type_url, format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionResponse"));
    assert!(!ver_payload.value.is_empty(), "version response payload must not be empty");
    let ver_wm = wm(&ver_payload.value);
    assert_eq!(fields(&ver_wm, 1).len(), 8, "Expected 8 definitions in version response");

    // 2b. Explicit valid specification version { major: 1, minor: 0, patch: 0 } -> STATUS_OK
    let mut valid_spec_req = Vec::new();
    encode_bytes_field(1, &ver_buf, &mut valid_spec_req);
    let valid_spec_resp = quasar_rpc(&ctx, &mut sess, "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest", valid_spec_req).await;
    assert_eq!(valid_spec_resp.status_code, STATUS_OK);

    // 2c. Unknown major version (major: 99) -> returns STATUS_NOT_FOUND (404)
    let mut bad_ver_buf = Vec::new();
    encode_varint_field(1, 99, &mut bad_ver_buf);
    let mut bad_spec_req = Vec::new();
    encode_bytes_field(1, &bad_ver_buf, &mut bad_spec_req);
    let bad_ver_resp = quasar_rpc(&ctx, &mut sess, "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest", bad_spec_req).await;
    assert_eq!(bad_ver_resp.status_code, STATUS_NOT_FOUND);
    assert!(bad_ver_resp.status_message.contains("Unknown planet resource definitions major version 99"));

    // =========================================================================
    // 3. GetAllConfigurationsRequest
    // =========================================================================
    // 3a. Unauthenticated gateway session (no user login) -> STATUS_UNAUTHORIZED (401)
    let mut no_user_sess = SessionState::new(62_011);
    let no_user_resp = quasar_rpc(&ctx, &mut no_user_sess, "eve_public.sovereignty.resource.planet.api.GetAllConfigurationsRequest", Vec::new()).await;
    assert_eq!(no_user_resp.status_code, STATUS_UNAUTHORIZED);
    assert!(no_user_resp.status_message.contains("user login required"));

    // 3b. Authenticated user without active character selection -> STATUS_FORBIDDEN (403)
    let mut no_char_sess = quasar_sess(62_012, None, None, None);
    let no_char_resp = quasar_rpc(&ctx, &mut no_char_sess, "eve_public.sovereignty.resource.planet.api.GetAllConfigurationsRequest", Vec::new()).await;
    assert_eq!(no_char_resp.status_code, eve_server::quasar_gateway::STATUS_FORBIDDEN);
    assert!(no_char_resp.status_message.contains("Active character selection required"));

    // 3c. Authenticated character in NULLSEC_SYSTEM -> STATUS_OK, GetAllConfigurationsResponse, non-empty
    let cfg_resp = quasar_rpc(&ctx, &mut sess, "eve_public.sovereignty.resource.planet.api.GetAllConfigurationsRequest", Vec::new()).await;
    assert_eq!(cfg_resp.status_code, STATUS_OK);
    let cfg_payload = cfg_resp.payload.expect("payload present");
    assert_eq!(cfg_payload.type_url, format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.resource.planet.api.GetAllConfigurationsResponse"));
    assert!(!cfg_payload.value.is_empty(), "configurations payload must not be empty");

    let cfg_wm = wm(&cfg_payload.value);
    let cfgs = fields(&cfg_wm, 1);
    let expected_planets = ctx.sde.celestials_in_system(NULLSEC_SYSTEM).into_iter().filter(|c| c.group_id == 7).count();
    assert_eq!(cfgs.len(), expected_planets, "Configurations count matches system planet count");
    assert!(cfgs.len() > 0, "System must have at least one planet");

    let mut total_power = 0;
    let mut total_workforce = 0;
    let mut reagent_count = 0;

    for pcfg in &cfgs {
        let cel_id = field_msg(pcfg, 1).expect("Identifier submessage");
        let planet_celestial_id = varint(&cel_id, 1).expect("celestial_id");
        assert!(planet_celestial_id > 0);
        let power = varint(pcfg, 2).unwrap_or(0);
        let workforce = varint(pcfg, 3).unwrap_or(0);
        total_power += power;
        total_workforce += workforce;

        // If planet has reagent config (field 4)
        if let Some(r_cfg) = field_msg(pcfg, 4) {
            reagent_count += 1;
            let r_id = field_msg(&r_cfg, 1).expect("reagent Identifier");
            let reagent_type_id = varint(&r_id, 1).expect("reagent type_id");
            assert!(reagent_type_id > 0);

            let harvest = field_msg(&r_cfg, 2).expect("HarvestConfiguration");
            assert_eq!(varint(&harvest, 1), Some(eve_server::services::sov_gameplay::SKYHOOK_HOURLY_REAGENT_YIELD));
            assert_eq!(varint(&harvest, 2), Some(3_600)); // hourly cycle
            assert_eq!(varint(&harvest, 4), Some(25_000)); // max storage capacity

            let maturation = field_msg(&r_cfg, 3).expect("MaturationConfiguration");
            assert_eq!(varint(&maturation, 1), Some(100));
            assert_eq!(varint(&maturation, 2), Some(86_400)); // daily cycle
            assert_eq!(varint(&maturation, 4), Some(25_000));
        }
    }
    assert!(total_power > 0, "Nullsec system has positive total planetary power");
    assert!(total_workforce > 0, "Nullsec system has positive total planetary workforce");
    assert!(reagent_count >= 2, "Nullsec system has reagent planets (Lava and Ice)");
}
