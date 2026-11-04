//! ESI calendar events and fleet operations parity suite.

#![forbid(unsafe_code)]

#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod esi_calendar_events_ext {
    use super::*;

// Parity tests for ESI Calendar endpoints:
// - `GET /characters/{character_id}/calendar`
// - `GET /characters/{character_id}/calendar/{event_id}`
// - `PUT /characters/{character_id}/calendar/{event_id}`
// - `GET /characters/{character_id}/calendar/{event_id}/attendees`

use crate::common;
use eve_db::{CorporationRecord, WorldRepository};
use eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token(ctx: &ServerContext, character_id: i64, scopes: Vec<&str>) -> String {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&character_id) { ch.user_id = 1; }
    });
    let now = ctx.now_unix();
    let scps: Vec<String> = scopes.into_iter().map(String::from).collect();
    let code = ctx.db.authorize_pkce("demo", "demo-secret", character_id, "test-client", "https://localhost/cb", scps, PKCE_CHALLENGE, "S256", now).expect("auth_pkce");
    ctx.db.exchange_pkce_code(&code, "test-client", PKCE_VERIFIER, now).expect("exchange_pkce").access_token
}

fn esi_req(router: &EsiRouter, spec: &OpenApiSpec, req: &EsiHttpRequest, exp_status: u16) -> Value {
    let resp = router.handle_request(req);
    let raw_path = req.raw_target.split('?').next().unwrap_or(&req.raw_target);
    assert_eq!(resp.status_code, exp_status, "{} {} -> {} ({})", req.method, req.raw_target, resp.status_code, String::from_utf8_lossy(&resp.body));
    spec.validate_response(&req.method, raw_path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI validation failed for {} {}: {e}", req.method, req.raw_target));
    if exp_status == 204 || exp_status == 304 { Value::Null } else { resp.json().expect("valid JSON") }
}

fn get_json(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: Option<&str>, status: u16) -> Value {
    let mut req = EsiHttpRequest::new("GET", path);
    if let Some(tok) = token { req = req.with_bearer_token(tok); }
    esi_req(router, spec, &req, status)
}

fn put_json(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: Option<&str>, body: &Value, status: u16) -> Value {
    let mut req = EsiHttpRequest::new("PUT", path).with_body(serde_json::to_vec(body).unwrap());
    if let Some(tok) = token { req = req.with_bearer_token(tok); }
    esi_req(router, spec, &req, status)
}

fn assert_etag_and_304(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: &str) {
    let req = EsiHttpRequest::new("GET", path).with_bearer_token(token);
    let resp = router.handle_request(&req);
    assert_eq!(resp.status_code, 200);
    let etag = resp.header("etag").expect("ETag header").to_string();
    assert!(resp.header("cache-control").is_some() && resp.header("expires").is_some());
    let mut cond = req.clone();
    cond.headers.insert("if-none-match".to_string(), etag);
    esi_req(router, spec, &cond, 304);
}

fn temp_db(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("evomu_esi_cal_{}_{}.sqlite", tag, std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, m: &str, args: &[PyRep]) -> PyRep {
    ctx.dispatch_rpc(sess, svc, None, m, args, &HashMap::new(), None).await.unwrap_or_else(|e| panic!("{svc}.{m}: {e:?}"))
}

fn make_sess(sess_id: u64, uid: u64, cid: i64, corp_id: i64) -> SessionState {
    let mut s = SessionState::new(sess_id);
    s.user_id = Some(uid);
    s.role = ROLE_PLAYER_DEFAULT;
    s.character_id = Some(cid);
    s.corporation_id = Some(corp_id);
    s.station_id = Some(60_003_760);
    s
}

#[tokio::test]
async fn test_parity_esi_calendar_full_lifecycle_and_schema_validation() {
    // parity: esi:GET /characters/{character_id}/calendar, esi:GET /characters/{character_id}/calendar/{event_id}
    // parity: esi:PUT /characters/{character_id}/calendar/{event_id}, esi:GET /characters/{character_id}/calendar/{event_id}/attendees
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/calendar",
        "esi:GET /characters/{character_id}/calendar/{event_id}",
        "esi:PUT /characters/{character_id}/calendar/{event_id}",
        "esi:GET /characters/{character_id}/calendar/{event_id}/attendees",
    ]);

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let (ceo_id, mem_id, str_id, corp_id) = (90_000_001_i64, 90_000_002_i64, 90_000_003_i64, 98_000_001_i64);
    ctx.db.with_mut_snapshot(|snap| {
        let tpl = snap.characters[&ceo_id].clone();
        for (cid, name, corp) in [(ceo_id, "Cal Lead", corp_id), (mem_id, "Cal Member", corp_id), (str_id, "Stranger", 98_000_002)] {
            let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| tpl.clone());
            ch.character_id = cid; ch.name = name.into(); ch.corporation_id = corp; ch.station_id = Some(60_003_760);
            snap.characters.insert(cid, ch);
        }
        snap.corporations.insert(corp_id, CorporationRecord {
            corporation_id: corp_id, name: "Cal Wing".into(), ticker: "CALW".into(), ceo_id,
            members: HashSet::from([ceo_id, mem_id]), member_roles: HashMap::from([(ceo_id, u64::MAX), (mem_id, 0)]),
            ..CorporationRecord::default()
        });
    });

    let mut ceo_sess = make_sess(71_001, 1, ceo_id, corp_id);
    let ft1 = ((ctx.now_unix() as i64 + 3_600) + 11_644_473_600) * 10_000_000;
    let r1 = rpc(&ctx, &mut ceo_sess, "calendarMgr", "CreatePersonalEvent", &[
        PyRep::Integer(ft1), PyRep::Integer(45), PyRep::String("1-on-1 Tactical Briefing".into()),
        PyRep::String("Review fleet composition".into()), PyRep::Integer(0), PyRep::List(vec![PyRep::Integer(mem_id)]),
    ]).await;
    let id_1 = r1.as_i64().or_else(|| r1.dict_get("eventID").and_then(|v| v.as_i64())).unwrap();

    let ft2 = ((ctx.now_unix() as i64 + 7_200) + 11_644_473_600) * 10_000_000;
    let r2 = rpc(&ctx, &mut ceo_sess, "calendarMgr", "CreateCorporationEvent", &[
        PyRep::Integer(ft2), PyRep::Integer(90), PyRep::String("Corporate Moon Mining".into()),
        PyRep::String("Refinery extraction event".into()), PyRep::Integer(1),
    ]).await;
    let id_2 = r2.as_i64().or_else(|| r2.dict_get("eventID").and_then(|v| v.as_i64())).unwrap();

    let ft3 = ((ctx.now_unix() as i64 + 14_400) + 11_644_473_600) * 10_000_000;
    let r3 = rpc(&ctx, &mut ceo_sess, "calendarMgr", "CreateCorporationEvent", &[
        PyRep::Integer(ft3), PyRep::Integer(120), PyRep::String("Alliance Stratop Deployment".into()),
        PyRep::String("Staging move".into()), PyRep::Integer(2),
    ]).await;
    let id_3 = r3.as_i64().or_else(|| r3.dict_get("eventID").and_then(|v| v.as_i64())).unwrap();
    assert!(id_1 < id_2 && id_2 < id_3);

    // Clear ephemeral esi_state to prove queries read real snapshot/persistence state
    ctx.esi_state.calendar_events.write().unwrap().clear();
    ctx.esi_state.calendar_responses.write().unwrap().clear();

    let ceo_tok = issue_token(&ctx, ceo_id, vec!["esi-calendar.read_calendar_events.v1", "esi-calendar.respond_calendar_events.v1"]);
    let mem_tok = issue_token(&ctx, mem_id, vec!["esi-calendar.read_calendar_events.v1", "esi-calendar.respond_calendar_events.v1"]);
    let ro_tok = issue_token(&ctx, mem_id, vec!["esi-calendar.read_calendar_events.v1"]);
    let str_tok = issue_token(&ctx, str_id, vec!["esi-calendar.read_calendar_events.v1", "esi-calendar.respond_calendar_events.v1"]);

    // 1. GET /characters/{character_id}/calendar (auth, pagination, etag)
    get_json(&router, &spec, &format!("/v1/characters/{ceo_id}/calendar/"), None, 401);
    assert_etag_and_304(&router, &spec, &format!("/v1/characters/{ceo_id}/calendar/"), &ceo_tok);

    let list = get_json(&router, &spec, &format!("/v1/characters/{ceo_id}/calendar/"), Some(&ceo_tok), 200);
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0]["event_id"], json!(id_1));
    assert_eq!(arr[1]["event_id"], json!(id_2));
    assert_eq!(arr[2]["event_id"], json!(id_3));
    assert_eq!(arr[0]["title"], json!("1-on-1 Tactical Briefing"));
    assert_eq!(arr[0]["event_response"], json!("accepted"));

    // Pagination tests
    let paged2 = get_json(&router, &spec, &format!("/v1/characters/{ceo_id}/calendar/?from_event={id_2}"), Some(&ceo_tok), 200);
    assert_eq!(paged2.as_array().unwrap().len(), 2);
    assert_eq!(paged2[0]["event_id"], json!(id_2));

    let paged3 = get_json(&router, &spec, &format!("/v1/characters/{ceo_id}/calendar/?from_event_id={id_3}"), Some(&ceo_tok), 200);
    assert_eq!(paged3.as_array().unwrap().len(), 1);
    assert_eq!(paged3[0]["event_id"], json!(id_3));

    let paged_empty = get_json(&router, &spec, &format!("/v1/characters/{ceo_id}/calendar/?from_event=999999"), Some(&ceo_tok), 200);
    assert_eq!(paged_empty, json!([]));
    get_json(&router, &spec, &format!("/v1/characters/{ceo_id}/calendar/?from_event=-1"), Some(&ceo_tok), 400);

    let str_list = get_json(&router, &spec, &format!("/v1/characters/{str_id}/calendar/"), Some(&str_tok), 200);
    assert_eq!(str_list, json!([]));

    // 2. GET /characters/{character_id}/calendar/{event_id}
    assert_etag_and_304(&router, &spec, &format!("/v3/characters/{ceo_id}/calendar/{id_1}/"), &ceo_tok);
    let d1 = get_json(&router, &spec, &format!("/v3/characters/{ceo_id}/calendar/{id_1}/"), Some(&ceo_tok), 200);
    assert_eq!(d1["event_id"], json!(id_1));
    assert_eq!(d1["title"], json!("1-on-1 Tactical Briefing"));
    assert_eq!(d1["text"], json!("Review fleet composition"));
    assert_eq!(d1["duration"], json!(45));
    assert_eq!(d1["importance"], json!(0));
    assert_eq!(d1["owner_id"], json!(ceo_id));
    assert_eq!(d1["owner_type"], json!("character"));
    assert_eq!(d1["response"], json!("accepted"));

    get_json(&router, &spec, &format!("/v3/characters/{ceo_id}/calendar/999999/"), Some(&ceo_tok), 404);
    get_json(&router, &spec, &format!("/v3/characters/{str_id}/calendar/{id_1}/"), Some(&str_tok), 403);

    // 3. PUT /characters/{character_id}/calendar/{event_id} (accepted, tentative, declined, errors)
    put_json(&router, &spec, &format!("/v3/characters/{mem_id}/calendar/{id_1}/"), Some(&mem_tok), &json!({"response": "accepted"}), 204);
    let md1 = get_json(&router, &spec, &format!("/v3/characters/{mem_id}/calendar/{id_1}/"), Some(&mem_tok), 200);
    assert_eq!(md1["response"], json!("accepted"));

    put_json(&router, &spec, &format!("/v3/characters/{mem_id}/calendar/{id_2}/"), Some(&mem_tok), &json!({"response": "tentative"}), 204);
    let md2 = get_json(&router, &spec, &format!("/v3/characters/{mem_id}/calendar/{id_2}/"), Some(&mem_tok), 200);
    assert_eq!(md2["response"], json!("tentative"));

    put_json(&router, &spec, &format!("/v3/characters/{mem_id}/calendar/{id_2}/"), Some(&mem_tok), &json!({"response": "declined"}), 204);
    let md2_dec = get_json(&router, &spec, &format!("/v3/characters/{mem_id}/calendar/{id_2}/"), Some(&mem_tok), 200);
    assert_eq!(md2_dec["response"], json!("declined"));

    put_json(&router, &spec, &format!("/v3/characters/{mem_id}/calendar/{id_2}/"), Some(&mem_tok), &json!({"response": "invalid"}), 400);
    put_json(&router, &spec, &format!("/v3/characters/{mem_id}/calendar/{id_2}/"), Some(&ro_tok), &json!({"response": "accepted"}), 403);
    put_json(&router, &spec, &format!("/v3/characters/{str_id}/calendar/{id_1}/"), Some(&str_tok), &json!({"response": "accepted"}), 403);
    put_json(&router, &spec, &format!("/v3/characters/{mem_id}/calendar/999999/"), Some(&mem_tok), &json!({"response": "accepted"}), 404);

    // 4. GET /characters/{character_id}/calendar/{event_id}/attendees
    assert_etag_and_304(&router, &spec, &format!("/v1/characters/{ceo_id}/calendar/{id_1}/attendees/"), &ceo_tok);
    let att1 = get_json(&router, &spec, &format!("/v1/characters/{ceo_id}/calendar/{id_1}/attendees/"), Some(&ceo_tok), 200);
    let att1_arr = att1.as_array().unwrap();
    assert!(att1_arr.iter().any(|a| a["character_id"] == json!(ceo_id) && a["event_response"] == json!("accepted")));
    assert!(att1_arr.iter().any(|a| a["character_id"] == json!(mem_id) && a["event_response"] == json!("accepted")));

    let att2 = get_json(&router, &spec, &format!("/v1/characters/{ceo_id}/calendar/{id_2}/attendees/"), Some(&ceo_tok), 200);
    assert!(att2.as_array().unwrap().iter().any(|a| a["character_id"] == json!(mem_id) && a["event_response"] == json!("declined")));

    get_json(&router, &spec, &format!("/v1/characters/{str_id}/calendar/{id_1}/attendees/"), Some(&str_tok), 403);
    get_json(&router, &spec, &format!("/v1/characters/{ceo_id}/calendar/999999/attendees/"), Some(&ceo_tok), 404);
}

#[tokio::test]
async fn test_parity_esi_calendar_sqlite_durability_across_restarts() {
    // parity: esi:GET /characters/{character_id}/calendar, esi:GET /characters/{character_id}/calendar/{event_id}
    // parity: esi:PUT /characters/{character_id}/calendar/{event_id}, esi:GET /characters/{character_id}/calendar/{event_id}/attendees
    let db_path = temp_db("durability");
    let spec = OpenApiSpec::pinned();
    let (ceo_id, mem_id, corp_id) = (90_000_001_i64, 90_000_002_i64, 98_000_001_i64);

    let event_id = {
        let repo1 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 1"));
        repo1.with_mut_snapshot(|snap| {
            *snap = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
            snap.corporations.insert(corp_id, CorporationRecord {
                corporation_id: corp_id, name: "Persistent Corp".into(), ticker: "PCORP".into(), ceo_id,
                members: HashSet::from([ceo_id, mem_id]), member_roles: HashMap::from([(ceo_id, u64::MAX), (mem_id, 0)]),
                ..CorporationRecord::default()
            });
            for cid in [ceo_id, mem_id] {
                if let Some(ch) = snap.characters.get_mut(&cid) { ch.corporation_id = corp_id; ch.user_id = 1; }
            }
        });
        let ctx1 = Arc::new(ServerContext::with_repository(repo1));
        common::pin_clock(&ctx1);
        let router1 = EsiRouter::new(Arc::clone(&ctx1));

        let mut ceo_sess = make_sess(72_001, 1, ceo_id, corp_id);
        let ft = ((ctx1.now_unix() as i64 + 10_000) + 11_644_473_600) * 10_000_000;
        let rep = rpc(&ctx1, &mut ceo_sess, "calendarMgr", "CreateCorporationEvent", &[
            PyRep::Integer(ft), PyRep::Integer(60), PyRep::String("Durable Defense Fleet".into()),
            PyRep::String("Defend POS anchor".into()), PyRep::Integer(1),
        ]).await;
        let eid = rep.as_i64().or_else(|| rep.dict_get("eventID").and_then(|v| v.as_i64())).unwrap();

        let mem_token = issue_token(&ctx1, mem_id, vec!["esi-calendar.read_calendar_events.v1", "esi-calendar.respond_calendar_events.v1"]);
        put_json(&router1, &spec, &format!("/v3/characters/{mem_id}/calendar/{eid}/"), Some(&mem_token), &json!({"response": "accepted"}), 204);

        eve_server::services::fleet_social::persist_social_state(&ctx1);
        ctx1.db.save_to_sqlite(&db_path).expect("save_to_sqlite");
        eid
    };

    // Reopen database from SQLite file on disk in fresh ServerContext
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 2"));
    let ctx2 = Arc::new(ServerContext::with_repository(repo2));
    common::pin_clock(&ctx2);
    let router2 = EsiRouter::new(Arc::clone(&ctx2));

    ctx2.esi_state.calendar_events.write().unwrap().clear();
    ctx2.esi_state.calendar_responses.write().unwrap().clear();

    let ceo_token = issue_token(&ctx2, ceo_id, vec!["esi-calendar.read_calendar_events.v1", "esi-calendar.respond_calendar_events.v1"]);

    let list = get_json(&router2, &spec, &format!("/v1/characters/{ceo_id}/calendar/"), Some(&ceo_token), 200);
    let item = list.as_array().unwrap().iter().find(|e| e["event_id"] == json!(event_id)).expect("persisted event in calendar list");
    assert_eq!(item["title"], json!("Durable Defense Fleet"));
    assert_eq!(item["importance"], json!(1));

    let detail = get_json(&router2, &spec, &format!("/v3/characters/{ceo_id}/calendar/{event_id}/"), Some(&ceo_token), 200);
    assert_eq!(detail["event_id"], json!(event_id));
    assert_eq!(detail["title"], json!("Durable Defense Fleet"));
    assert_eq!(detail["text"], json!("Defend POS anchor"));
    assert_eq!(detail["duration"], json!(60));
    assert_eq!(detail["owner_type"], json!("corporation"));

    let attendees = get_json(&router2, &spec, &format!("/v1/characters/{ceo_id}/calendar/{event_id}/attendees/"), Some(&ceo_token), 200);
    assert!(
        attendees.as_array().unwrap().iter().any(|a| a["character_id"] == json!(mem_id) && a["event_response"] == json!("accepted")),
        "Member accepted response must persist in SQLite attendees list across restart"
    );

    let _ = std::fs::remove_file(&db_path);
}

}

#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod esi_calendar_ops_ext {
    use super::*;

// Parity remediation integration tests for ESI Character Calendar:
// - `GET /characters/{character_id}/calendar`
// - `GET /characters/{character_id}/calendar/{event_id}`
// - `PUT /characters/{character_id}/calendar/{event_id}`
// - `GET /characters/{character_id}/calendar/{event_id}/attendees`

use crate::common;
use eve_db::{CorporationRecord, WorldRepository};
use eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token(ctx: &ServerContext, character_id: i64, scopes: Vec<&str>) -> String {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&character_id) {
            ch.user_id = 1;
        }
    });
    let now = ctx.now_unix();
    let scps: Vec<String> = scopes.into_iter().map(String::from).collect();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            character_id,
            "test-client",
            "https://localhost/cb",
            scps,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("auth_pkce");
    ctx.db
        .exchange_pkce_code(&code, "test-client", PKCE_VERIFIER, now)
        .expect("exchange_pkce")
        .access_token
}

fn esi_call(
    router: &EsiRouter,
    spec: &OpenApiSpec,
    req: &EsiHttpRequest,
    expected_status: u16,
) -> Value {
    let resp = router.handle_request(req);
    let raw_path = req.raw_target.split('?').next().unwrap_or(&req.raw_target);
    assert_eq!(
        resp.status_code,
        expected_status,
        "{} {} -> {} (body: {})",
        req.method,
        req.raw_target,
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
    spec.validate_response(
        &req.method,
        raw_path,
        resp.status_code,
        &resp.headers,
        &resp.body,
    )
    .unwrap_or_else(|e| {
        panic!(
            "OpenAPI validation failure for {} {}: {e}",
            req.method, req.raw_target
        )
    });
    if expected_status == 204 || expected_status == 304 {
        Value::Null
    } else {
        resp.json().expect("valid JSON response")
    }
}

fn get_json(
    router: &EsiRouter,
    spec: &OpenApiSpec,
    path: &str,
    token: Option<&str>,
    status: u16,
) -> Value {
    let mut req = EsiHttpRequest::new("GET", path);
    if let Some(tok) = token {
        req = req.with_bearer_token(tok);
    }
    esi_call(router, spec, &req, status)
}

fn put_json(
    router: &EsiRouter,
    spec: &OpenApiSpec,
    path: &str,
    token: Option<&str>,
    body: &Value,
    status: u16,
) -> Value {
    let mut req = EsiHttpRequest::new("PUT", path).with_body(serde_json::to_vec(body).unwrap());
    if let Some(tok) = token {
        req = req.with_bearer_token(tok);
    }
    esi_call(router, spec, &req, status)
}

fn assert_etag_and_304(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: &str) {
    let req = EsiHttpRequest::new("GET", path).with_bearer_token(token);
    let resp = router.handle_request(&req);
    assert_eq!(resp.status_code, 200);
    let etag = resp.header("etag").expect("ETag header").to_string();
    assert!(resp.header("cache-control").is_some());
    assert!(resp.header("expires").is_some());
    let mut cond = req.clone();
    cond.headers.insert("if-none-match".to_string(), etag);
    esi_call(router, spec, &cond, 304);
}

fn temp_db_path(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "evomu_esi_cal_{}_{}_{}.sqlite",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> PyRep {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method}: {e:?}"))
}

fn make_session(
    sess_id: u64,
    uid: u64,
    cid: i64,
    corp_id: i64,
) -> SessionState {
    let mut s = SessionState::new(sess_id);
    s.user_id = Some(uid);
    s.role = ROLE_PLAYER_DEFAULT;
    s.character_id = Some(cid);
    s.corporation_id = Some(corp_id);
    s.station_id = Some(60_003_760);
    s
}

#[tokio::test]
async fn test_parity_remediation_character_calendar_lifecycle_and_sqlite() {
    // parity: esi:GET /characters/{character_id}/calendar
    // parity: esi:GET /characters/{character_id}/calendar/{event_id}
    // parity: esi:PUT /characters/{character_id}/calendar/{event_id}
    // parity: esi:GET /characters/{character_id}/calendar/{event_id}/attendees
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/calendar",
        "esi:GET /characters/{character_id}/calendar/{event_id}",
        "esi:PUT /characters/{character_id}/calendar/{event_id}",
        "esi:GET /characters/{character_id}/calendar/{event_id}/attendees",
    ]);

    let db_path = temp_db_path("char_cal");
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let member_id = 90_000_002_i64;
    let outsider_id = 90_000_003_i64;
    let corp_id = 98_000_001_i64;

    let (event_personal_id, event_corp_id) = {
        let repo1 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 1"));
        repo1.with_mut_snapshot(|snap| {
            *snap = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
            snap.corporations.insert(
                corp_id,
                CorporationRecord {
                    corporation_id: corp_id,
                    name: "Calendar Tactical Wing".into(),
                    ticker: "CTW".into(),
                    ceo_id,
                    members: HashSet::from([ceo_id, member_id]),
                    member_roles: HashMap::from([(ceo_id, u64::MAX), (member_id, 0)]),
                    ..CorporationRecord::default()
                },
            );
            let tpl = snap.characters[&ceo_id].clone();
            for (cid, corpid, name) in [
                (ceo_id, corp_id, "Calendar Lead"),
                (member_id, corp_id, "Calendar Member"),
                (outsider_id, 98_000_099, "Calendar Outsider"),
            ] {
                let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| tpl.clone());
                ch.character_id = cid;
                ch.name = name.into();
                ch.corporation_id = corpid;
                ch.user_id = 1;
                snap.characters.insert(cid, ch);
            }
        });
        let ctx1 = Arc::new(ServerContext::with_repository(repo1));
        common::pin_clock(&ctx1);

        let mut ceo_sess = make_session(92_001, 1, ceo_id, corp_id);

        // Create personal event with member invited
        let ft1 = ((ctx1.now_unix() as i64 + 3_600) + 11_644_473_600) * 10_000_000;
        let r1 = rpc(
            &ctx1,
            &mut ceo_sess,
            "calendarMgr",
            "CreatePersonalEvent",
            &[
                PyRep::Integer(ft1),
                PyRep::Integer(45),
                PyRep::String("Skirmish Debrief".into()),
                PyRep::String("Analyze combat logs".into()),
                PyRep::Integer(0),
                PyRep::List(vec![PyRep::Integer(member_id)]),
            ],
        )
        .await;
        let p_id = r1
            .as_i64()
            .or_else(|| r1.dict_get("eventID").and_then(|v| v.as_i64()))
            .expect("personal event id");

        // Create corporation event
        let ft2 = ((ctx1.now_unix() as i64 + 7_200) + 11_644_473_600) * 10_000_000;
        let r2 = rpc(
            &ctx1,
            &mut ceo_sess,
            "calendarMgr",
            "CreateCorporationEvent",
            &[
                PyRep::Integer(ft2),
                PyRep::Integer(90),
                PyRep::String("Strategic Citadel Defense".into()),
                PyRep::String("Anchor and online defense modules".into()),
                PyRep::Integer(2),
            ],
        )
        .await;
        let c_id = r2
            .as_i64()
            .or_else(|| r2.dict_get("eventID").and_then(|v| v.as_i64()))
            .expect("corp event id");

        // Respond "accepted" to personal event from member via ESI PUT
        let router1 = EsiRouter::new(Arc::clone(&ctx1));
        let mem_rw_tok = issue_token(
            &ctx1,
            member_id,
            vec![
                "esi-calendar.read_calendar_events.v1",
                "esi-calendar.respond_calendar_events.v1",
            ],
        );

        put_json(
            &router1,
            &spec,
            &format!("/v3/characters/{member_id}/calendar/{p_id}/"),
            Some(&mem_rw_tok),
            &json!({ "response": "accepted" }),
            204,
        );

        // Respond "tentative" to corporation event from member via ESI PUT
        put_json(
            &router1,
            &spec,
            &format!("/v3/characters/{member_id}/calendar/{c_id}/"),
            Some(&mem_rw_tok),
            &json!({ "response": "tentative" }),
            204,
        );

        eve_server::services::fleet_social::persist_social_state(&ctx1);
        ctx1.db.save_to_sqlite(&db_path).expect("save_to_sqlite");
        (p_id, c_id)
    };

    // Reopen database from SQLite disk file into fresh ServerContext
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 2"));
    let ctx2 = Arc::new(ServerContext::with_repository(repo2));
    common::pin_clock(&ctx2);
    let router2 = EsiRouter::new(Arc::clone(&ctx2));

    // Clear ephemeral runtime state to guarantee queries read SQLite-persisted records
    ctx2.esi_state.calendar_events.write().unwrap().clear();
    ctx2.esi_state.calendar_responses.write().unwrap().clear();

    let ceo_tok = issue_token(
        &ctx2,
        ceo_id,
        vec![
            "esi-calendar.read_calendar_events.v1",
            "esi-calendar.respond_calendar_events.v1",
        ],
    );
    let mem_tok = issue_token(
        &ctx2,
        member_id,
        vec![
            "esi-calendar.read_calendar_events.v1",
            "esi-calendar.respond_calendar_events.v1",
        ],
    );
    let ro_tok = issue_token(
        &ctx2,
        member_id,
        vec!["esi-calendar.read_calendar_events.v1"],
    );
    let outsider_tok = issue_token(
        &ctx2,
        outsider_id,
        vec![
            "esi-calendar.read_calendar_events.v1",
            "esi-calendar.respond_calendar_events.v1",
        ],
    );

    // ============================================
    // ESI: GET /characters/{character_id}/calendar
    // ============================================
    // 401 without auth token
    get_json(
        &router2,
        &spec,
        &format!("/v1/characters/{ceo_id}/calendar/"),
        None,
        401,
    );

    // 403 when token character does not match path
    get_json(
        &router2,
        &spec,
        &format!("/v1/characters/{ceo_id}/calendar/"),
        Some(&outsider_tok),
        403,
    );

    // 200 OK with ETag/304 check
    assert_etag_and_304(
        &router2,
        &spec,
        &format!("/v1/characters/{ceo_id}/calendar/"),
        &ceo_tok,
    );

    let ceo_events = get_json(
        &router2,
        &spec,
        &format!("/v1/characters/{ceo_id}/calendar/"),
        Some(&ceo_tok),
        200,
    );
    let arr = ceo_events.as_array().expect("array of calendar events");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["event_id"], json!(event_personal_id));
    assert_eq!(arr[0]["title"], json!("Skirmish Debrief"));
    assert_eq!(arr[0]["event_response"], json!("accepted"));
    assert_eq!(arr[1]["event_id"], json!(event_corp_id));
    assert_eq!(arr[1]["title"], json!("Strategic Citadel Defense"));

    // Query param `from_event`
    let filtered = get_json(
        &router2,
        &spec,
        &format!("/v1/characters/{ceo_id}/calendar/?from_event={event_corp_id}"),
        Some(&ceo_tok),
        200,
    );
    assert_eq!(filtered.as_array().unwrap().len(), 1);
    assert_eq!(filtered[0]["event_id"], json!(event_corp_id));

    // Negative from_event -> 400
    get_json(
        &router2,
        &spec,
        &format!("/v1/characters/{ceo_id}/calendar/?from_event=-1"),
        Some(&ceo_tok),
        400,
    );

    // Outsider sees empty calendar
    let out_events = get_json(
        &router2,
        &spec,
        &format!("/v1/characters/{outsider_id}/calendar/"),
        Some(&outsider_tok),
        200,
    );
    assert_eq!(out_events, json!([]));

    // ========================================================
    // ESI: GET /characters/{character_id}/calendar/{event_id}
    // ========================================================
    get_json(
        &router2,
        &spec,
        &format!("/v3/characters/{ceo_id}/calendar/999999/"),
        Some(&ceo_tok),
        404,
    );

    get_json(
        &router2,
        &spec,
        &format!("/v3/characters/{outsider_id}/calendar/{event_personal_id}/"),
        Some(&outsider_tok),
        403,
    );

    assert_etag_and_304(
        &router2,
        &spec,
        &format!("/v3/characters/{ceo_id}/calendar/{event_personal_id}/"),
        &ceo_tok,
    );

    let detail = get_json(
        &router2,
        &spec,
        &format!("/v3/characters/{ceo_id}/calendar/{event_personal_id}/"),
        Some(&ceo_tok),
        200,
    );
    assert_eq!(detail["event_id"], json!(event_personal_id));
    assert_eq!(detail["title"], json!("Skirmish Debrief"));
    assert_eq!(detail["text"], json!("Analyze combat logs"));
    assert_eq!(detail["duration"], json!(45));
    assert_eq!(detail["importance"], json!(0));
    assert_eq!(detail["owner_id"], json!(ceo_id));
    assert_eq!(detail["owner_type"], json!("character"));
    assert_eq!(detail["response"], json!("accepted"));

    // Check member's persisted response on the corp event ("tentative")
    let mem_detail = get_json(
        &router2,
        &spec,
        &format!("/v3/characters/{member_id}/calendar/{event_corp_id}/"),
        Some(&mem_tok),
        200,
    );
    assert_eq!(mem_detail["response"], json!("tentative"));

    // ========================================================
    // ESI: PUT /characters/{character_id}/calendar/{event_id}
    // ========================================================
    // 401 without auth token
    put_json(
        &router2,
        &spec,
        &format!("/v3/characters/{member_id}/calendar/{event_corp_id}/"),
        None,
        &json!({ "response": "declined" }),
        401,
    );

    // 403 with read-only token
    put_json(
        &router2,
        &spec,
        &format!("/v3/characters/{member_id}/calendar/{event_corp_id}/"),
        Some(&ro_tok),
        &json!({ "response": "declined" }),
        403,
    );

    // 400 with invalid response value
    put_json(
        &router2,
        &spec,
        &format!("/v3/characters/{member_id}/calendar/{event_corp_id}/"),
        Some(&mem_tok),
        &json!({ "response": "invalid_status" }),
        400,
    );

    // 403 for outsider not invited
    put_json(
        &router2,
        &spec,
        &format!("/v3/characters/{outsider_id}/calendar/{event_personal_id}/"),
        Some(&outsider_tok),
        &json!({ "response": "accepted" }),
        403,
    );

    // 404 for unknown event
    put_json(
        &router2,
        &spec,
        &format!("/v3/characters/{member_id}/calendar/999999/"),
        Some(&mem_tok),
        &json!({ "response": "accepted" }),
        404,
    );

    // 204 update response to "declined"
    put_json(
        &router2,
        &spec,
        &format!("/v3/characters/{member_id}/calendar/{event_corp_id}/"),
        Some(&mem_tok),
        &json!({ "response": "declined" }),
        204,
    );

    let updated_detail = get_json(
        &router2,
        &spec,
        &format!("/v3/characters/{member_id}/calendar/{event_corp_id}/"),
        Some(&mem_tok),
        200,
    );
    assert_eq!(updated_detail["response"], json!("declined"));

    // ==================================================================
    // ESI: GET /characters/{character_id}/calendar/{event_id}/attendees
    // ==================================================================
    get_json(
        &router2,
        &spec,
        &format!("/v1/characters/{ceo_id}/calendar/999999/attendees/"),
        Some(&ceo_tok),
        404,
    );

    get_json(
        &router2,
        &spec,
        &format!("/v1/characters/{outsider_id}/calendar/{event_personal_id}/attendees/"),
        Some(&outsider_tok),
        403,
    );

    assert_etag_and_304(
        &router2,
        &spec,
        &format!("/v1/characters/{ceo_id}/calendar/{event_personal_id}/attendees/"),
        &ceo_tok,
    );

    let att_p = get_json(
        &router2,
        &spec,
        &format!("/v1/characters/{ceo_id}/calendar/{event_personal_id}/attendees/"),
        Some(&ceo_tok),
        200,
    );
    let att_p_arr = att_p.as_array().expect("array of attendees");
    assert!(att_p_arr
        .iter()
        .any(|a| a["character_id"] == json!(ceo_id) && a["event_response"] == json!("accepted")));
    assert!(att_p_arr
        .iter()
        .any(|a| a["character_id"] == json!(member_id) && a["event_response"] == json!("accepted")));

    let att_c = get_json(
        &router2,
        &spec,
        &format!("/v1/characters/{ceo_id}/calendar/{event_corp_id}/attendees/"),
        Some(&ceo_tok),
        200,
    );
    let att_c_arr = att_c.as_array().expect("corp attendees");
    assert!(att_c_arr
        .iter()
        .any(|a| a["character_id"] == json!(member_id) && a["event_response"] == json!("declined")));

    let _ = std::fs::remove_file(&db_path);
}

}
