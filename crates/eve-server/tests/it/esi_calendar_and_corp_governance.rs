//! End-to-end integration tests unifying ESI calendar endpoints with `calendarMgr`
//! and remediating corporation governance and Upwell structure ESI endpoints.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{corp_roles, WorldRepository};
use eve_machonet::service_roles::ROLE_GMH;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use eve_server::services::structure_acl_defense_rules as rules;
use glam::DVec3;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<eve_sde::SdeRegistry> {
    static SDE: OnceLock<Arc<eve_sde::SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(eve_sde::SdeRegistry::with_canonical_seed())))
}

fn fast_demo_ctx() -> ServerContext {
    static BASE: OnceLock<(Arc<eve_sde::SdeRegistry>, eve_db::WorldStateSnapshot)> = OnceLock::new();
    let (sde, snap) = BASE.get_or_init(|| {
        let sde = shared_sde();
        let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
        let _ = eve_server::world_seeder::seed_dual_anchor_markets(&db, &sde);
        (sde, db.with_snapshot(|s| s.clone()))
    });
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    db.with_mut_snapshot(|s| *s = snap.clone());
    let ctx = ServerContext::with_sde_and_repository(Arc::clone(sde), db)
        .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED);
    common::pin_clock(&ctx);
    ctx
}

const CEO_ID: i64 = 90_000_001;
const MEM_ID: i64 = 90_000_002;
const GM_ID: i64 = 90_000_003;
const JITA_SYS: u32 = 30_000_142;
const PERIMETER_SYS: u32 = 30_000_144;
const JITA_STATION: i64 = 60_003_760;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_pkce_token(ctx: &ServerContext, user: &str, pass: &str, char_id: i64, scopes: &[&str]) -> String {
    let now = ctx.now_unix();
    let scps: Vec<String> = scopes.iter().map(|s| (*s).to_string()).collect();
    let code = ctx
        .db
        .authorize_pkce(user, pass, char_id, "evomu-esi-test", "https://localhost/callback", scps, PKCE_CHALLENGE, "S256", now)
        .expect("authorize_pkce");
    ctx.db.exchange_pkce_code(&code, "evomu-esi-test", PKCE_VERIFIER, now).expect("exchange_pkce_code").access_token
}

fn send_esi(router: &EsiRouter, spec: &OpenApiSpec, req: &EsiHttpRequest, exp_status: u16) -> (Value, HashMap<String, String>) {
    let resp = router.handle_request(req);
    let raw_path = req.raw_target.split('?').next().unwrap_or(&req.raw_target);
    assert_eq!(
        resp.status_code,
        exp_status,
        "{} {} -> {} ({})",
        req.method,
        req.raw_target,
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
    spec.validate_response(&req.method, raw_path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI validation failed for {} {}: {e}", req.method, req.raw_target));
    let val = if exp_status == 204 || exp_status == 304 { Value::Null } else { resp.json().expect("valid JSON response") };
    let norm_headers = resp.headers.into_iter().map(|(k, v)| (k.to_ascii_lowercase(), v)).collect();
    (val, norm_headers)
}

fn get_json(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: Option<&str>, status: u16) -> Value {
    let mut req = EsiHttpRequest::new("GET", path);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    send_esi(router, spec, &req, status).0
}

fn put_json(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: Option<&str>, body: &Value, status: u16) -> Value {
    let mut req = EsiHttpRequest::new("PUT", path).with_body(serde_json::to_vec(body).unwrap());
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    send_esi(router, spec, &req, status).0
}

fn assert_etag_304(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: &str) {
    let req = EsiHttpRequest::new("GET", path).with_bearer_token(token);
    let (_, headers) = send_esi(router, spec, &req, 200);
    let etag = headers.get("etag").expect("etag header").clone();
    assert!(headers.contains_key("cache-control") && headers.contains_key("expires"));
    let mut cond = req.clone();
    cond.headers.insert("if-none-match".to_string(), etag);
    send_esi(router, spec, &cond, 304);
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method} failed: {e:?}"))
}

async fn login_pilot(ctx: &ServerContext, sess_id: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String(user.into()), PyRep::String(pass.into())]).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await;
    ctx.advance_virtual_clock(12);
    sess
}

fn collect_skill_prereqs(ctx: &ServerContext, type_id: u32, out: &mut Vec<(u32, u8)>) {
    let Some(t) = ctx.sde.types.get(&type_id) else { return };
    for (sk_attr, lv_attr) in [(182_u16, 277_u16), (183, 278), (184, 279), (1285, 1286), (1289, 1287), (1290, 1288)] {
        if let Some(&sk_f) = t.default_attributes.get(&sk_attr) {
            let (sk, lv) = (sk_f as u32, t.default_attributes.get(&lv_attr).copied().unwrap_or(1.0) as u8);
            if sk > 0 && lv > 0 {
                collect_skill_prereqs(ctx, sk, out);
                if let Some(pos) = out.iter().position(|(id, _)| *id == sk) {
                    out[pos].1 = out[pos].1.max(lv);
                } else {
                    out.push((sk, lv));
                }
            }
        }
    }
}

async fn train_skills_live(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, targets: &[(u32, u8)]) {
    for &(tid, target_lv) in targets {
        let cur_lv = ctx.db.with_snapshot(|s| s.skills.get(&char_id).and_then(|m| m.get(&tid)).map(|r| r.level));
        if cur_lv.is_none() {
            rpc(ctx, sess, "skillMgr2", "PurchaseSkills", &[PyRep::List(vec![PyRep::Integer(i64::from(tid))])]).await;
        }
        let start_lv = cur_lv.unwrap_or(0);
        if start_lv < target_lv {
            let q: Vec<PyRep> = ((start_lv + 1)..=target_lv)
                .map(|lv| PyRep::Tuple(vec![PyRep::Integer(i64::from(tid)), PyRep::Integer(i64::from(lv))]))
                .collect();
            rpc(ctx, sess, "skillMgr2", "SaveSkillQueue", &[PyRep::List(q)]).await;
            ctx.advance_virtual_clock(35 * 86_400);
            rpc(ctx, sess, "skillMgr2", "GetSkills", &[]).await;
        }
    }
}

fn temp_sqlite_path(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("evomu_esi_gov_{}_{}.sqlite", tag, std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

#[tokio::test]
async fn test_esi_calendar_machonet_roundtrip_and_sqlite_durability() {
    // parity: esi:GET /characters/{character_id}/calendar, esi:GET /characters/{character_id}/calendar/{event_id}
    // parity: esi:PUT /characters/{character_id}/calendar/{event_id}, esi:GET /characters/{character_id}/calendar/{event_id}/attendees
    // parity: feat:esi.calendar.list, feat:esi.calendar.event, feat:esi.calendar.respond, feat:esi.calendar.attendees
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/calendar",
        "esi:GET /characters/{character_id}/calendar/{event_id}",
        "esi:PUT /characters/{character_id}/calendar/{event_id}",
        "esi:GET /characters/{character_id}/calendar/{event_id}/attendees",
        "feat:esi.calendar.list",
        "feat:esi.calendar.event",
        "feat:esi.calendar.respond",
        "feat:esi.calendar.attendees",
    ]);

    let db_path = temp_sqlite_path("cal_roundtrip");
    let spec = OpenApiSpec::pinned();

    let (id_1, id_2, id_3, ccp_id, corp_id, alliance_id) = {
        let ctx1 = Arc::new(fast_demo_ctx());
        ctx1.db.provision_gm_account("admin", "admin-secret", GM_ID, "ISD Event Marshal");
        let router1 = EsiRouter::new(Arc::clone(&ctx1));

        let mut ceo_sess = login_pilot(&ctx1, 81_001, "demo", "demo-secret", CEO_ID).await;
        let mut mem_sess = login_pilot(&ctx1, 81_002, "other", "other-secret", MEM_ID).await;
        let mut gm_sess = login_pilot(&ctx1, 81_003, "admin", "admin-secret", GM_ID).await;
        gm_sess.role |= ROLE_GMH;

        let corp_rep = rpc(&ctx1, &mut ceo_sess, "corpRegistry", "AddCorporation", &[PyRep::String("Calendar Vanguard".into()), PyRep::String("CVGD".into()), PyRep::Real(0.05)]).await;
        let corp_id = corp_rep.dict_get("corporationID").and_then(|v| v.as_i64()).expect("corp_id");
        ctx1.advance_virtual_clock(12);

        let app_rep = rpc(&ctx1, &mut mem_sess, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Ready for ops".into())]).await;
        let app_id = app_rep.as_i64().or_else(|| app_rep.dict_get("applicationID").and_then(|v| v.as_i64())).expect("app_id");
        rpc(&ctx1, &mut ceo_sess, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())]).await;
        ctx1.advance_virtual_clock(12);
        ctx1.sync_session(&mut mem_sess);

        rpc(&ctx1, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/giveisk 2000000000".into())]).await;
        rpc(&ctx1, &mut gm_sess, "account", "GiveCash", &[PyRep::Integer(corp_id), PyRep::Real(1_500_000_000.0), PyRep::String("Alliance fee".into())]).await;
        let ally_rep = rpc(&ctx1, &mut ceo_sess, "corpRegistry", "CreateAlliance", &[PyRep::String("Calendar Coalition".into()), PyRep::String("CALC".into()), PyRep::String("Coalition ops".into()), PyRep::String("https://calc.example".into())]).await;
        let alliance_id = ally_rep.dict_get("allianceID").and_then(|v| v.as_i64()).expect("alliance_id");
        ctx1.sync_session(&mut ceo_sess);
        ctx1.sync_session(&mut mem_sess);

        let ft1 = ((ctx1.now_unix() as i64 + 3_600) + 11_644_473_600) * 10_000_000;
        let ev1 = rpc(&ctx1, &mut ceo_sess, "calendarMgr", "CreatePersonalEvent", &[PyRep::Integer(ft1), PyRep::Integer(45), PyRep::String("Tactical Fleet Sync".into()), PyRep::String("Bring logi cruisers".into()), PyRep::Integer(0), PyRep::List(vec![PyRep::Integer(MEM_ID)])]).await;
        let id_1 = ev1.dict_get("eventID").and_then(|v| v.as_i64()).expect("id_1");

        let ft2 = ((ctx1.now_unix() as i64 + 7_200) + 11_644_473_600) * 10_000_000;
        let ev2 = rpc(&ctx1, &mut ceo_sess, "calendarMgr", "CreateCorporationEvent", &[PyRep::Integer(ft2), PyRep::Integer(90), PyRep::String("Moon Extraction Fleet".into()), PyRep::String("Mining barge formup".into()), PyRep::Integer(1)]).await;
        let id_2 = ev2.dict_get("eventID").and_then(|v| v.as_i64()).expect("id_2");

        let ft3 = ((ctx1.now_unix() as i64 + 10_800) + 11_644_473_600) * 10_000_000;
        let ev3 = rpc(&ctx1, &mut ceo_sess, "calendarMgr", "CreateAllianceEvent", &[PyRep::Integer(ft3), PyRep::Integer(120), PyRep::String("Coalition Move Op".into()), PyRep::String("Jump freighter convoy".into()), PyRep::Integer(1)]).await;
        let id_3 = ev3.dict_get("eventID").and_then(|v| v.as_i64()).expect("id_3");

        let ft4 = ((ctx1.now_unix() as i64 + 14_400) + 11_644_473_600) * 10_000_000;
        let ccp_ev = rpc(&ctx1, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String(format!("/createccpevent {ft4} 180 1 Live Alliance Tournament | Broadcast"))]).await;
        let ccp_id = ccp_ev.dict_get("eventID").and_then(|v| v.as_i64()).expect("ccp_id");

        let ceo_tok = issue_pkce_token(&ctx1, "demo", "demo-secret", CEO_ID, &["esi-calendar.read_calendar_events.v1", "esi-calendar.respond_calendar_events.v1"]);
        let mem_tok = issue_pkce_token(&ctx1, "other", "other-secret", MEM_ID, &["esi-calendar.read_calendar_events.v1", "esi-calendar.respond_calendar_events.v1"]);
        let mem_ro_tok = issue_pkce_token(&ctx1, "other", "other-secret", MEM_ID, &["esi-calendar.read_calendar_events.v1"]);
        let out_tok = issue_pkce_token(&ctx1, "admin", "admin-secret", GM_ID, &["esi-calendar.read_calendar_events.v1", "esi-calendar.respond_calendar_events.v1"]);

        get_json(&router1, spec, &format!("/v1/characters/{CEO_ID}/calendar/"), None, 401);
        get_json(&router1, spec, &format!("/v1/characters/{CEO_ID}/calendar/"), Some(&mem_tok), 403);
        get_json(&router1, spec, &format!("/v1/characters/{CEO_ID}/calendar/?from_event=-5"), Some(&ceo_tok), 400);
        assert_etag_304(&router1, spec, &format!("/v1/characters/{CEO_ID}/calendar/"), &ceo_tok);

        let list = get_json(&router1, spec, &format!("/v1/characters/{CEO_ID}/calendar/"), Some(&ceo_tok), 200);
        assert_eq!(list.as_array().unwrap().iter().map(|v| v["event_id"].as_i64().unwrap()).collect::<Vec<_>>(), vec![id_1, id_2, id_3, ccp_id]);
        assert_eq!(get_json(&router1, spec, &format!("/v1/characters/{CEO_ID}/calendar/?from_event={id_2}"), Some(&ceo_tok), 200)[0]["event_id"], json!(id_2));
        assert_eq!(get_json(&router1, spec, &format!("/v1/characters/{CEO_ID}/calendar/?from_event_id={id_3}"), Some(&ceo_tok), 200)[0]["event_id"], json!(id_3));

        let out_list = get_json(&router1, spec, &format!("/v1/characters/{GM_ID}/calendar/"), Some(&out_tok), 200);
        assert_eq!(out_list.as_array().unwrap().len(), 1);
        assert_eq!(out_list[0]["event_id"], json!(ccp_id));
        get_json(&router1, spec, &format!("/v3/characters/{GM_ID}/calendar/{id_1}/"), Some(&out_tok), 403);
        get_json(&router1, spec, &format!("/v3/characters/{CEO_ID}/calendar/9999999/"), Some(&ceo_tok), 404);

        assert_etag_304(&router1, spec, &format!("/v3/characters/{CEO_ID}/calendar/{id_1}/"), &ceo_tok);
        assert_eq!(get_json(&router1, spec, &format!("/v3/characters/{CEO_ID}/calendar/{id_1}/"), Some(&ceo_tok), 200)["owner_type"], json!("character"));
        assert_eq!(get_json(&router1, spec, &format!("/v3/characters/{CEO_ID}/calendar/{id_2}/"), Some(&ceo_tok), 200)["owner_type"], json!("corporation"));
        let d3 = get_json(&router1, spec, &format!("/v3/characters/{CEO_ID}/calendar/{id_3}/"), Some(&ceo_tok), 200);
        assert_eq!(d3["owner_type"], json!("alliance"));
        assert_eq!(d3["owner_name"], json!("Calendar Coalition"));
        let d_ccp = get_json(&router1, spec, &format!("/v3/characters/{CEO_ID}/calendar/{ccp_id}/"), Some(&ceo_tok), 200);
        assert_eq!(d_ccp["owner_type"], json!("eve_server"));
        assert_eq!(d_ccp["owner_name"], json!("EVE System"));

        // MachoNet SendEventResponse (5 = tentative) -> immediately visible on ESI detail & attendees
        rpc(&ctx1, &mut mem_sess, "calendarMgr", "SendEventResponse", &[PyRep::Integer(id_1), PyRep::Integer(CEO_ID), PyRep::Integer(5)]).await;
        assert_eq!(get_json(&router1, spec, &format!("/v3/characters/{MEM_ID}/calendar/{id_1}/"), Some(&mem_tok), 200)["response"], json!("tentative"));
        let att_tent = get_json(&router1, spec, &format!("/v1/characters/{CEO_ID}/calendar/{id_1}/attendees/"), Some(&ceo_tok), 200);
        assert!(att_tent.as_array().unwrap().iter().any(|a| a["character_id"] == json!(MEM_ID) && a["event_response"] == json!("tentative")));

        // ESI PUT response -> updates calendarMgr.GetResponsesToEvent and emits OnEventResponseByExternal
        let _ = ctx1.drain_notifications_for_char(MEM_ID);
        put_json(&router1, spec, &format!("/v3/characters/{MEM_ID}/calendar/{id_1}/"), Some(&mem_tok), &json!({"response": "accepted"}), 204);
        assert!(ctx1.drain_notifications_for_char(MEM_ID).iter().any(|n| n.event == "OnEventResponseByExternal"));
        let PyRep::Rowset { rows, .. } = rpc(&ctx1, &mut ceo_sess, "calendarMgr", "GetResponsesToEvent", &[PyRep::Integer(id_1)]).await else {
            panic!("expected rowset from GetResponsesToEvent");
        };
        assert!(rows.iter().any(|r| r[0] == PyRep::Integer(MEM_ID) && r[1] == PyRep::Integer(4)));

        put_json(&router1, spec, &format!("/v3/characters/{MEM_ID}/calendar/{id_2}/"), Some(&mem_tok), &json!({"response": "declined"}), 204);
        put_json(&router1, spec, &format!("/v3/characters/{MEM_ID}/calendar/{id_2}/"), Some(&mem_tok), &json!({"response": "bogus"}), 400);
        put_json(&router1, spec, &format!("/v3/characters/{MEM_ID}/calendar/{id_2}/"), Some(&mem_ro_tok), &json!({"response": "accepted"}), 403);
        put_json(&router1, spec, &format!("/v3/characters/{GM_ID}/calendar/{id_1}/"), Some(&out_tok), &json!({"response": "accepted"}), 403);
        put_json(&router1, spec, &format!("/v3/characters/{MEM_ID}/calendar/9999999/"), Some(&mem_tok), &json!({"response": "accepted"}), 404);

        assert_etag_304(&router1, spec, &format!("/v1/characters/{CEO_ID}/calendar/{id_1}/attendees/"), &ceo_tok);
        get_json(&router1, spec, &format!("/v1/characters/{GM_ID}/calendar/{id_1}/attendees/"), Some(&out_tok), 403);
        get_json(&router1, spec, &format!("/v1/characters/{CEO_ID}/calendar/9999999/attendees/"), Some(&ceo_tok), 404);

        // Live EditPersonalEvent & DeleteEvent propagate immediately to ESI without split-brain
        rpc(&ctx1, &mut ceo_sess, "calendarMgr", "EditPersonalEvent", &[PyRep::Integer(id_1), PyRep::Integer(ft1), PyRep::Integer(ft1 + 10_000_000 * 600), PyRep::Integer(75), PyRep::String("Updated Tactical Sync".into()), PyRep::String("Bring heavy assault cruisers".into()), PyRep::Integer(1)]).await;
        let edited = get_json(&router1, spec, &format!("/v3/characters/{CEO_ID}/calendar/{id_1}/"), Some(&ceo_tok), 200);
        assert_eq!(edited["title"], json!("Updated Tactical Sync"));
        assert_eq!(edited["text"], json!("Bring heavy assault cruisers"));
        assert_eq!(edited["duration"], json!(75));

        rpc(&ctx1, &mut ceo_sess, "calendarMgr", "DeleteEvent", &[PyRep::Integer(id_3)]).await;
        get_json(&router1, spec, &format!("/v3/characters/{CEO_ID}/calendar/{id_3}/"), Some(&ceo_tok), 404);

        eve_server::services::fleet_social::persist_social_state(&ctx1);
        ctx1.db.save_to_sqlite(&db_path).expect("save_to_sqlite");
        (id_1, id_2, id_3, ccp_id, corp_id, alliance_id)
    };

    // Reopen fresh ServerContext from SQLite and verify full durability
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite"));
    let ctx2 = Arc::new(ServerContext::with_sde_and_repository(shared_sde(), repo2));
    common::pin_clock(&ctx2);
    eve_server::services::fleet_social::hydrate_social_state_from_db(&ctx2);
    let router2 = EsiRouter::new(Arc::clone(&ctx2));
    let ceo_tok2 = issue_pkce_token(&ctx2, "demo", "demo-secret", CEO_ID, &["esi-calendar.read_calendar_events.v1"]);

    let list2 = get_json(&router2, spec, &format!("/v1/characters/{CEO_ID}/calendar/"), Some(&ceo_tok2), 200);
    assert_eq!(list2.as_array().unwrap().iter().map(|v| v["event_id"].as_i64().unwrap()).collect::<Vec<_>>(), vec![id_1, id_2, ccp_id]);
    assert_eq!(get_json(&router2, spec, &format!("/v3/characters/{CEO_ID}/calendar/{id_1}/"), Some(&ceo_tok2), 200)["title"], json!("Updated Tactical Sync"));
    let att1 = get_json(&router2, spec, &format!("/v1/characters/{CEO_ID}/calendar/{id_1}/attendees/"), Some(&ceo_tok2), 200);
    assert!(att1.as_array().unwrap().iter().any(|a| a["character_id"] == json!(MEM_ID) && a["event_response"] == json!("accepted")));
    let att2 = get_json(&router2, spec, &format!("/v1/characters/{CEO_ID}/calendar/{id_2}/attendees/"), Some(&ceo_tok2), 200);
    assert!(att2.as_array().unwrap().iter().any(|a| a["character_id"] == json!(MEM_ID) && a["event_response"] == json!("declined")));
    get_json(&router2, spec, &format!("/v3/characters/{CEO_ID}/calendar/{id_3}/"), Some(&ceo_tok2), 404);
    assert!(corp_id >= 98_000_000 && alliance_id >= 99_000_000);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_esi_corp_members_limit_roles_and_history_live_rpcs() {
    // parity: esi:GET /corporations/{corporation_id}/members/limit, esi:GET /corporations/{corporation_id}/roles
    // parity: esi:GET /corporations/{corporation_id}/roles/history, feat:esi.corporation.members, feat:esi.corporation.roles
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/members/limit",
        "esi:GET /corporations/{corporation_id}/roles",
        "esi:GET /corporations/{corporation_id}/roles/history",
        "feat:esi.corporation.members",
        "feat:esi.corporation.roles",
    ]);

    let ctx = Arc::new(fast_demo_ctx());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let mut ceo_sess = login_pilot(&ctx, 82_001, "demo", "demo-secret", CEO_ID).await;
    let mut mem_sess = login_pilot(&ctx, 82_002, "other", "other-secret", MEM_ID).await;

    let corp_rep = rpc(&ctx, &mut ceo_sess, "corpRegistry", "AddCorporation", &[PyRep::String("Governance Directorate".into()), PyRep::String("GOVD".into()), PyRep::Real(0.05)]).await;
    let corp_id = corp_rep.dict_get("corporationID").and_then(|v| v.as_i64()).expect("corp_id");
    ctx.advance_virtual_clock(12);

    let gov_scopes = [
        "esi-corporations.track_members.v1",
        "esi-corporations.read_corporation_membership.v1",
        "esi-characters.read_corporation_roles.v1",
    ];
    let ceo_tok = issue_pkce_token(&ctx, "demo", "demo-secret", CEO_ID, &gov_scopes);
    let ceo_bad_scope_tok = issue_pkce_token(&ctx, "demo", "demo-secret", CEO_ID, &["esi-calendar.read_calendar_events.v1"]);
    let outsider_tok = issue_pkce_token(&ctx, "other", "other-secret", MEM_ID, &gov_scopes);

    // 1. /members/limit immediately reflects founding CEO Corporation Management I (20) before UpdateCorporationAbilities
    get_json(&router, spec, &format!("/v1/corporations/{corp_id}/members/limit/"), None, 401);
    get_json(&router, spec, &format!("/v1/corporations/{corp_id}/members/limit/"), Some(&ceo_bad_scope_tok), 403);
    get_json(&router, spec, &format!("/v1/corporations/{corp_id}/members/limit/"), Some(&outsider_tok), 403);
    get_json(&router, spec, "/v1/corporations/1000009/members/limit/", Some(&ceo_tok), 403);
    assert_etag_304(&router, spec, &format!("/v1/corporations/{corp_id}/members/limit/"), &ceo_tok);
    assert_eq!(get_json(&router, spec, &format!("/v1/corporations/{corp_id}/members/limit/"), Some(&ceo_tok), 200), json!(20));

    let mut targets = Vec::new();
    collect_skill_prereqs(&ctx, 3731, &mut targets);
    if let Some(pos) = targets.iter().position(|(id, _)| *id == 3363) {
        targets[pos].1 = 5;
    } else {
        targets.push((3363, 5));
    }
    targets.push((3731, 3));
    train_skills_live(&ctx, &mut ceo_sess, CEO_ID, &targets).await;
    rpc(&ctx, &mut ceo_sess, "corpRegistry", "UpdateCorporationAbilities", &[]).await;
    let ceo_tok = issue_pkce_token(&ctx, "demo", "demo-secret", CEO_ID, &gov_scopes);
    let ceo_bad_scope_tok = issue_pkce_token(&ctx, "demo", "demo-secret", CEO_ID, &["esi-calendar.read_calendar_events.v1"]);
    let outsider_tok = issue_pkce_token(&ctx, "other", "other-secret", MEM_ID, &gov_scopes);

    let expected_limit = ((ctx.sde.types[&3363].default_attributes[&191].round() as i64) * 5 + (ctx.sde.types[&3731].default_attributes[&191].round() as i64) * 3).max(10);
    assert_eq!(get_json(&router, spec, &format!("/v1/corporations/{corp_id}/members/limit/"), Some(&ceo_tok), 200), json!(expected_limit));

    get_json(&router, spec, &format!("/v2/corporations/{corp_id}/roles/"), Some(&outsider_tok), 403);
    get_json(&router, spec, &format!("/v1/corporations/{corp_id}/roles/history/"), Some(&outsider_tok), 403);

    // Recruit MEM_ID via live InsertApplication + UpdateApplicationOffer
    let app_rep = rpc(&ctx, &mut mem_sess, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Apply".into())]).await;
    let app_id = app_rep.as_i64().or_else(|| app_rep.dict_get("applicationID").and_then(|v| v.as_i64())).expect("app_id");
    rpc(&ctx, &mut ceo_sess, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())]).await;
    ctx.sync_session(&mut mem_sess);

    get_json(&router, spec, &format!("/v1/corporations/{corp_id}/roles/history/"), Some(&outsider_tok), 403);
    get_json(&router, spec, &format!("/v1/corporations/{corp_id}/roles/history/"), Some(&ceo_bad_scope_tok), 403);

    // CEO u64::MAX roles validate cleanly on both /corporations/{id}/roles and /characters/{id}/roles
    assert_etag_304(&router, spec, &format!("/v2/corporations/{corp_id}/roles/"), &ceo_tok);
    let ceo_char_roles = get_json(&router, spec, &format!("/v3/characters/{CEO_ID}/roles/"), Some(&ceo_tok), 200);
    let ceo_role_arr = ceo_char_roles["roles"].as_array().unwrap();
    assert_eq!(ceo_role_arr.len(), 54);
    for expected in ["Director", "Config_Starbase_Equipment", "Communications_Officer", "Starbase_Defense_Operator", "Starbase_Fuel_Technician", "Brand_Manager", "Project_Manager", "Skill_Plan_Manager"] {
        assert!(ceo_role_arr.contains(&json!(expected)), "missing V24 role {expected}");
    }

    // Live UpdateMember across all 8 positional role categories, followed by a role revocation
    let r_main = (corp_roles::ROLE_PERSONNEL_MANAGER | corp_roles::ROLE_STATION_MANAGER | corp_roles::ROLE_STARBASE_CONFIG) as i64;
    rpc(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateMember",
        &[PyRep::List(vec![
            PyRep::Integer(MEM_ID),
            PyRep::Integer(r_main),
            PyRep::Integer(0),
            PyRep::Integer(corp_roles::ROLE_HANGAR_CAN_TAKE[0] as i64),
            PyRep::Integer(corp_roles::ROLE_SECURITY_OFFICER as i64),
            PyRep::Integer(corp_roles::ROLE_CONTAINER_CAN_TAKE[0] as i64),
            PyRep::Integer(corp_roles::ROLE_FACTORY_MANAGER as i64),
            PyRep::Integer(corp_roles::ROLE_HANGAR_CAN_QUERY[1] as i64),
            PyRep::Integer(corp_roles::ROLE_CHAT_MANAGER as i64),
            PyRep::Integer(corp_roles::ROLE_STARBASE_CARETAKER as i64),
            PyRep::Integer(JITA_STATION),
        ])],
    )
    .await;

    let roles_doc = get_json(&router, spec, &format!("/v2/corporations/{corp_id}/roles/"), Some(&ceo_tok), 200);
    let mem_entry = roles_doc.as_array().unwrap().iter().find(|e| e["character_id"] == json!(MEM_ID)).expect("MEM_ID roles");
    assert_eq!(mem_entry["roles"], json!(["Personnel_Manager", "Station_Manager", "Config_Starbase_Equipment"]));
    assert_eq!(mem_entry["grantable_roles"], json!(["Hangar_Take_1"]));
    assert_eq!(mem_entry["roles_at_hq"], json!(["Security_Officer"]));
    assert_eq!(mem_entry["grantable_roles_at_hq"], json!(["Container_Take_1"]));
    assert_eq!(mem_entry["roles_at_base"], json!(["Factory_Manager"]));
    assert_eq!(mem_entry["grantable_roles_at_base"], json!(["Hangar_Query_2"]));
    assert_eq!(mem_entry["roles_at_other"], json!(["Communications_Officer"]));
    assert_eq!(mem_entry["grantable_roles_at_other"], json!(["Starbase_Fuel_Technician"]));

    // Personnel_Manager can read /roles, while /roles/history requires Director
    get_json(&router, spec, &format!("/v2/corporations/{corp_id}/roles/"), Some(&outsider_tok), 200);
    get_json(&router, spec, &format!("/v1/corporations/{corp_id}/roles/history/"), Some(&outsider_tok), 403);
    assert_etag_304(&router, spec, &format!("/v1/corporations/{corp_id}/roles/history/"), &ceo_tok);

    ctx.advance_virtual_clock(15);
    let r_revoked = (corp_roles::ROLE_STATION_MANAGER | corp_roles::ROLE_STARBASE_CONFIG) as i64;
    rpc(&ctx, &mut ceo_sess, "corpRegistry", "UpdateMember", &[PyRep::Dict(vec![(PyRep::String("characterID".into()), PyRep::Integer(MEM_ID)), (PyRep::String("roles".into()), PyRep::Integer(r_revoked))])]).await;

    let req_hist = EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/roles/history/?page=1")).with_bearer_token(&ceo_tok);
    let (hist_val, hist_hdrs) = send_esi(&router, spec, &req_hist, 200);
    assert_eq!(hist_hdrs.get("x-pages").map(String::as_str), Some("1"));
    let hist = hist_val.as_array().unwrap();
    assert_eq!(hist.len(), 9, "8 initial role_type grants + 1 subsequent roles revocation");
    assert!(hist[0]["changed_at"].as_str().unwrap() > hist[1]["changed_at"].as_str().unwrap());
    assert_eq!(hist[0]["role_type"], json!("roles"));
    assert_eq!(hist[0]["issuer_id"], json!(CEO_ID));
    assert_eq!(hist[0]["character_id"], json!(MEM_ID));
    assert_eq!(hist[0]["old_roles"], json!(["Personnel_Manager", "Station_Manager", "Config_Starbase_Equipment"]));
    assert_eq!(hist[0]["new_roles"], json!(["Station_Manager", "Config_Starbase_Equipment"]));

    let role_types_seen: HashSet<&str> = hist.iter().map(|h| h["role_type"].as_str().unwrap()).collect();
    for expected_rt in ["roles", "grantable_roles", "roles_at_hq", "grantable_roles_at_hq", "roles_at_base", "grantable_roles_at_base", "roles_at_other", "grantable_roles_at_other"] {
        assert!(role_types_seen.contains(expected_rt), "missing role_type {expected_rt} in history");
    }
}

#[tokio::test]
async fn test_esi_corp_structures_live_anchoring_services_fuel_and_profiles() {
    // parity: esi:GET /corporations/{corporation_id}/structures
    // parity: feat:esi.corporation.structures, feat:esi.corporations.structures-services-timers
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/structures",
        "feat:esi.corporation.structures",
        "feat:esi.corporations.structures-services-timers",
    ]);

    let ctx = Arc::new(fast_demo_ctx());
    ctx.db.provision_gm_account("admin", "admin-secret", GM_ID, "ISD Logistics");
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let mut ceo_sess = login_pilot(&ctx, 83_001, "demo", "demo-secret", CEO_ID).await;
    let mut mem_sess = login_pilot(&ctx, 83_002, "other", "other-secret", MEM_ID).await;
    let mut gm_sess = login_pilot(&ctx, 83_003, "admin", "admin-secret", GM_ID).await;

    let corp_rep = rpc(&ctx, &mut ceo_sess, "corpRegistry", "AddCorporation", &[PyRep::String("Upwell Logistics Group".into()), PyRep::String("UPLG".into()), PyRep::Real(0.05)]).await;
    let corp_id = corp_rep.dict_get("corporationID").and_then(|v| v.as_i64()).expect("corp_id");
    ctx.advance_virtual_clock(12);

    let app_rep = rpc(&ctx, &mut mem_sess, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Join".into())]).await;
    let app_id = app_rep.as_i64().or_else(|| app_rep.dict_get("applicationID").and_then(|v| v.as_i64())).expect("app_id");
    rpc(&ctx, &mut ceo_sess, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())]).await;
    ctx.advance_virtual_clock(12);

    let mut targets = Vec::new();
    collect_skill_prereqs(&ctx, 20_185, &mut targets);
    train_skills_live(&ctx, &mut ceo_sess, CEO_ID, &targets).await;

    let (fort_type, clone_svc_type, plant_svc_type) = (35_833_u32, 35_894_u32, 35_878_u32);
    let core_type = ctx.sde.types[&fort_type].default_attributes[&life::ATTR_REQUIRES_DEED_TYPE] as u32;
    let fuel_group = ctx.sde.types[&clone_svc_type].default_attributes[&life::ATTR_SERVICE_FUEL_GROUP] as u32;
    let fuel_type = ctx.sde.types.values().filter(|t| t.group_id == fuel_group && t.published).map(|t| t.type_id).min().unwrap();

    // GM spawns Charon + Fortizar + Core + 2 Service Modules + 8000 Fuel Blocks at Jita 4-4 and trades them to non-GM CEO
    let mut gm_items = Vec::new();
    for (tid, qty) in [(20_185_u32, 1), (fort_type, 1), (core_type, 1), (clone_svc_type, 1), (plant_svc_type, 1), (fuel_type, 8_000)] {
        let rep = rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String(format!("/giveitem {tid} {qty}"))]).await;
        gm_items.push(rep.dict_get("itemID").and_then(|v| v.as_i64()).expect("spawned itemID"));
    }
    let trade_id = rpc(&ctx, &mut gm_sess, "trademgr", "InitiateTrade", &[PyRep::Integer(CEO_ID)]).await.dict_get("tradeContainerID").and_then(|v| v.as_i64()).expect("trade_id");
    for &item_id in &gm_items {
        rpc(&ctx, &mut gm_sess, "trademgr", "AddItem", &[PyRep::Integer(trade_id), PyRep::Integer(item_id)]).await;
    }
    rpc(&ctx, &mut gm_sess, "trademgr", "MakeOffer", &[PyRep::Integer(trade_id), PyRep::Bool(true)]).await;
    rpc(&ctx, &mut ceo_sess, "trademgr", "MakeOffer", &[PyRep::Integer(trade_id), PyRep::Bool(true)]).await;

    let (charon_id, fort_item_id, core_item_id, clone_item_id, plant_item_id, fuel_item_id) = (gm_items[0], gm_items[1], gm_items[2], gm_items[3], gm_items[4], gm_items[5]);

    ctx.advance_virtual_clock(15);
    rpc(&ctx, &mut ceo_sess, "ship", "Board", &[PyRep::Integer(charon_id)]).await;
    for &item_id in &[fort_item_id, core_item_id, clone_item_id, plant_item_id] {
        rpc(&ctx, &mut ceo_sess, "invbroker", "Add", &[PyRep::Integer(item_id), PyRep::Integer(charon_id), PyRep::Integer(5)]).await;
    }
    rpc(&ctx, &mut ceo_sess, "invbroker", "Add", &[PyRep::Integer(fuel_item_id), PyRep::Integer(charon_id), PyRep::Integer(5), PyRep::Integer(8_000)]).await;

    ctx.advance_virtual_clock(15);
    rpc(&ctx, &mut ceo_sess, "structureDocking", "Undock", &[]).await;
    ctx.advance_virtual_clock(15);
    let gate_id = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == JITA_SYS && j.to_solar_system_id == PERIMETER_SYS).map(|j| j.stargate_id).expect("Jita -> Perimeter stargate");
    assert!(ctx.systems.get_or_spawn(JITA_SYS).set_ball_position(charon_id, ctx.sde.stargates()[&gate_id].position).await);
    let beyonce_oid = ctx.bind_object(&mut ceo_sess, "beyonce", i64::from(JITA_SYS)).expect("bind beyonce");
    ctx.dispatch_rpc(&mut ceo_sess, "beyonce", Some(beyonce_oid), "CmdStargateJump", &[PyRep::Integer(gate_id)], &HashMap::new(), None).await.expect("CmdStargateJump");
    ctx.advance_virtual_clock(15);
    assert_eq!(ceo_sess.solar_system_id, Some(PERIMETER_SYS));

    let perim_station_pos = ctx.sde.stations.values().find(|s| s.solar_system_id == PERIMETER_SYS).map(|s| s.position).expect("Perimeter station");
    assert!(ctx.systems.get_or_spawn(PERIMETER_SYS).set_ball_position(charon_id, perim_station_pos + DVec3::new(0.0, 5.0e9, 0.0)).await);

    let p1_rep = rpc(&ctx, &mut ceo_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Citadel Public".into()), PyRep::String("Default profile".into())]).await;
    let prof1_id = p1_rep.as_i64().or_else(|| p1_rep.dict_get("profileID").and_then(|v| v.as_i64())).expect("prof1_id");
    let p2_rep = rpc(&ctx, &mut ceo_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Citadel Restricted".into()), PyRep::String("Updated profile".into())]).await;
    let prof2_id = p2_rep.as_i64().or_else(|| p2_rep.dict_get("profileID").and_then(|v| v.as_i64())).expect("prof2_id");

    let anchor_rep = rpc(
        &ctx,
        &mut ceo_sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(fort_item_id), PyRep::None, PyRep::None, PyRep::Real(0.0), PyRep::Integer(prof1_id), PyRep::Integer(3), PyRep::Integer(14), PyRep::String("Perimeter Trade Fortizar".into()), PyRep::String("Logistics hub".into()), PyRep::None],
    )
    .await;
    let fort_id = anchor_rep.dict_get("structureID").and_then(|v| v.as_i64()).expect("fort_id");

    let ceo_tok = issue_pkce_token(&ctx, "demo", "demo-secret", CEO_ID, &["esi-corporations.read_structures.v1"]);
    let bad_scope_tok = issue_pkce_token(&ctx, "demo", "demo-secret", CEO_ID, &["esi-corporations.read_corporation_membership.v1"]);
    let mem_tok = issue_pkce_token(&ctx, "other", "other-secret", MEM_ID, &["esi-corporations.read_structures.v1"]);
    let out_tok = issue_pkce_token(&ctx, "admin", "admin-secret", GM_ID, &["esi-corporations.read_structures.v1"]);

    get_json(&router, spec, &format!("/v4/corporations/{corp_id}/structures/"), None, 401);
    get_json(&router, spec, &format!("/v4/corporations/{corp_id}/structures/"), Some(&bad_scope_tok), 403);
    get_json(&router, spec, &format!("/v4/corporations/{corp_id}/structures/"), Some(&mem_tok), 403);
    get_json(&router, spec, &format!("/v4/corporations/{corp_id}/structures/"), Some(&out_tok), 403);

    // Stage 1: anchor_vulnerable -> anchoring -> fitting_invulnerable via live clock progression
    let s_av = &get_json(&router, spec, &format!("/v4/corporations/{corp_id}/structures/"), Some(&ceo_tok), 200)[0];
    assert_eq!(s_av["structure_id"], json!(fort_id));
    assert_eq!(s_av["type_id"], json!(fort_type));
    assert_eq!(s_av["system_id"], json!(PERIMETER_SYS));
    assert_eq!(s_av["profile_id"], json!(prof1_id));
    assert_eq!(s_av["state"], json!("anchor_vulnerable"));
    assert_eq!(s_av["reinforce_hour"], json!(14));
    assert!(s_av.get("state_timer_start").is_some() && s_av.get("state_timer_end").is_some());
    assert!(s_av.get("next_reinforce_apply").is_none(), "reinforce_weekday must not leak as bogus 1970 next_reinforce_apply");

    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS);
    let s_anch = &get_json(&router, spec, &format!("/v4/corporations/{corp_id}/structures/"), Some(&ceo_tok), 200)[0];
    assert_eq!(s_anch["state"], json!("anchoring"));
    assert!(s_anch.get("state_timer_start").is_some() && s_anch.get("state_timer_end").is_some());

    ctx.advance_virtual_clock(life::ANCHORING_SECS);
    let ceo_tok = issue_pkce_token(&ctx, "demo", "demo-secret", CEO_ID, &["esi-corporations.read_structures.v1"]);
    assert_eq!(get_json(&router, spec, &format!("/v4/corporations/{corp_id}/structures/"), Some(&ceo_tok), 200)[0]["state"], json!("fitting_invulnerable"));

    // Stage 2: Dock Charon, install Quantum Core -> onlining_vulnerable -> shield_vulnerable
    let fort_pos = DVec3::from_array(ctx.db.with_snapshot(|s| s.structures[&fort_id].position));
    assert!(ctx.systems.get_or_spawn(PERIMETER_SYS).set_ball_position(charon_id, fort_pos).await);
    ctx.advance_virtual_clock(15);
    rpc(&ctx, &mut ceo_sess, "structureDocking", "Dock", &[PyRep::Integer(fort_id)]).await;
    let core_install_unix = ctx.now_unix();
    rpc(&ctx, &mut ceo_sess, "invbroker", "Add", &[PyRep::Integer(core_item_id), PyRep::Integer(fort_id), PyRep::Integer(172)]).await;

    let s_onl = &get_json(&router, spec, &format!("/v4/corporations/{corp_id}/structures/"), Some(&ceo_tok), 200)[0];
    assert_eq!(s_onl["state"], json!("onlining_vulnerable"));
    assert!(s_onl.get("state_timer_start").is_some() && s_onl.get("state_timer_end").is_some());

    ctx.advance_virtual_clock(15);
    rpc(&ctx, &mut ceo_sess, "structureDocking", "Undock", &[]).await;
    ctx.advance_virtual_clock(life::ONLINING_SECS);
    let ceo_tok = issue_pkce_token(&ctx, "demo", "demo-secret", CEO_ID, &["esi-corporations.read_structures.v1"]);
    assert_eq!(get_json(&router, spec, &format!("/v4/corporations/{corp_id}/structures/"), Some(&ceo_tok), 200)[0]["state"], json!("shield_vulnerable"));

    // Stage 3: Fit service modules (1 online, 1 offline), load fuel, schedule profile & reinforce changes
    assert!(ctx.systems.get_or_spawn(PERIMETER_SYS).set_ball_position(charon_id, fort_pos).await);
    ctx.advance_virtual_clock(15);
    rpc(&ctx, &mut ceo_sess, "structureDocking", "Dock", &[PyRep::Integer(fort_id)]).await;

    let offline_kw = HashMap::from([("online".to_string(), PyRep::Bool(false))]);
    let fit_clone = ctx.dispatch_rpc(&mut ceo_sess, "invbroker", None, "Add", &[PyRep::Integer(clone_item_id), PyRep::Integer(fort_id), PyRep::Integer(164)], &offline_kw, None).await.expect("fit Cloning Center");
    let clone_mod_id = fit_clone.dict_get("itemID").and_then(|v| v.as_i64()).expect("clone_mod_id");
    ctx.dispatch_rpc(&mut ceo_sess, "invbroker", None, "Add", &[PyRep::Integer(plant_item_id), PyRep::Integer(fort_id), PyRep::Integer(165)], &offline_kw, None).await.expect("fit Manufacturing Plant");

    rpc(&ctx, &mut ceo_sess, "invbroker", "Add", &[PyRep::Integer(fuel_item_id), PyRep::Integer(fort_id), PyRep::Integer(133), PyRep::Integer(8_000)]).await;
    rpc(&ctx, &mut ceo_sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(fort_id), PyRep::Integer(clone_mod_id)]).await;
    rpc(&ctx, &mut ceo_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(fort_id)]), PyRep::Integer(prof2_id)]).await;
    rpc(&ctx, &mut ceo_sess, "structureVulnerability", "UpdateReinforceTimesFromClient", &[PyRep::Integer(fort_id), PyRep::Integer(20), PyRep::Integer(5)]).await;

    let fuel_tick_anchor = core_install_unix + life::ONLINING_SECS;
    let online_cost = rules::service_module_online_fuel_cost(&ctx, fort_type, clone_svc_type);
    let burn_per_hr = (ctx.sde.types[&clone_svc_type].default_attributes[&life::ATTR_SERVICE_FUEL_PER_HOUR]
        * rules::service_module_fuel_multiplier(&ctx, fort_type, clone_svc_type))
    .round() as u64;
    let exp_fuel_ts = eve_server::esi::sample_routes::format_iso8601_utc(fuel_tick_anchor + (u64::from(8_000 - online_cost) / burn_per_hr) * 3_600);

    let s_svc = &get_json(&router, spec, &format!("/v4/corporations/{corp_id}/structures/"), Some(&ceo_tok), 200)[0];
    assert_eq!(s_svc["profile_id"], json!(prof1_id), "pending profile must wait 300s before applying");
    assert_eq!(s_svc["fuel_expires"], json!(exp_fuel_ts));
    assert_eq!(s_svc["reinforce_hour"], json!(14));
    assert_eq!(s_svc["next_reinforce_hour"], json!(20));
    assert!(s_svc.get("next_reinforce_apply").is_some());
    let svcs = s_svc["services"].as_array().unwrap();
    assert!(svcs.iter().any(|sv| sv["name"] == json!("Standup Cloning Center I") && sv["state"] == json!("online")));
    assert!(svcs.iter().any(|sv| sv["name"] == json!("Standup Manufacturing Plant I") && sv["state"] == json!("offline")));

    ctx.advance_virtual_clock(305);
    assert_eq!(get_json(&router, spec, &format!("/v4/corporations/{corp_id}/structures/"), Some(&ceo_tok), 200)[0]["profile_id"], json!(prof2_id));

    ctx.advance_virtual_clock(30 * 86_400);
    let ceo_tok = issue_pkce_token(&ctx, "demo", "demo-secret", CEO_ID, &["esi-corporations.read_structures.v1"]);
    let s_reinf = &get_json(&router, spec, &format!("/v4/corporations/{corp_id}/structures/"), Some(&ceo_tok), 200)[0];
    assert_eq!(s_reinf["reinforce_hour"], json!(20));
    assert!(s_reinf.get("next_reinforce_hour").is_none() && s_reinf.get("next_reinforce_apply").is_none());

    // Stage 4: Unanchor -> unanchors_at (+7d), X-Pages, and ETag/304
    rpc(&ctx, &mut ceo_sess, "structureDeployment", "Unanchor", &[PyRep::Integer(fort_id)]).await;
    assert_etag_304(&router, spec, &format!("/v4/corporations/{corp_id}/structures/"), &ceo_tok);
    let req_st = EsiHttpRequest::new("GET", format!("/v4/corporations/{corp_id}/structures/?page=1")).with_bearer_token(&ceo_tok);
    let (st_val, st_hdrs) = send_esi(&router, spec, &req_st, 200);
    assert_eq!(st_hdrs.get("x-pages").map(String::as_str), Some("1"));
    assert_eq!(st_val[0]["unanchors_at"], json!(eve_server::esi::sample_routes::format_iso8601_utc(ctx.now_unix() + 7 * 86_400)));
}
