//! Parity remediation integration tests for ESI Character Calendar:
//! - `GET /characters/{character_id}/calendar`
//! - `GET /characters/{character_id}/calendar/{event_id}`
//! - `PUT /characters/{character_id}/calendar/{event_id}`
//! - `GET /characters/{character_id}/calendar/{event_id}/attendees`

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
