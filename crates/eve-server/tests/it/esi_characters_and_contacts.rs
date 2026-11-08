//! ESI characters and contacts parity suite.

#![forbid(unsafe_code)]

#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod esi_contacts_ops_ext {
    use super::*;

// Parity remediation integration tests for ESI Alliance Contacts:
// - `GET /alliances/{alliance_id}/contacts`
// - `GET /alliances/{alliance_id}/contacts/labels`

use crate::common;
use eve_db::{AllianceRecord, CorporationRecord, WorldRepository};
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
        "evomu_esi_contacts_{}_{}_{}.sqlite",
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
    alliance_id: Option<i64>,
) -> SessionState {
    let mut s = SessionState::new(sess_id);
    s.user_id = Some(uid);
    s.role = ROLE_PLAYER_DEFAULT;
    s.character_id = Some(cid);
    s.corporation_id = Some(corp_id);
    s.alliance_id = alliance_id;
    s.station_id = Some(60_003_760);
    s
}

#[tokio::test]
async fn test_parity_remediation_alliance_contacts_and_labels() {
    let db_path = temp_db_path("ally_contacts");
    let spec = OpenApiSpec::pinned();

    let exec_ceo_id = 90_000_001_i64;
    let ally_member_id = 90_000_002_i64;
    let outsider_id = 90_000_003_i64;
    let exec_corp_id = 98_000_001_i64;
    let member_corp_id = 98_000_002_i64;
    let outsider_corp_id = 98_000_003_i64;
    let alliance_id = 99_000_001_i64;
    let target_corp_id = 98_000_099_i64;

    let (label_logistics, label_diplomatic) = {
        let repo1 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 1"));
        repo1.with_mut_snapshot(|snap| {
            *snap = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
            snap.alliances.insert(
                alliance_id,
                AllianceRecord {
                    alliance_id,
                    name: "Remediation Alliance".into(),
                    short_name: "REMED".into(),
                    executor_corp_id: exec_corp_id,
                    member_corp_ids: HashSet::from([exec_corp_id, member_corp_id]),
                },
            );
            snap.corporations.insert(
                exec_corp_id,
                { let mut __corp = CorporationRecord::default();
                    __corp.corporation_id = exec_corp_id;
                    __corp.name = "Executor Wing".into();
                    __corp.ticker = "EXECW".into();
                    __corp.ceo_id = exec_ceo_id;
                    __corp.alliance_id = Some(alliance_id);
                    __corp.members = HashSet::from([exec_ceo_id]);
                    __corp.member_roles = HashMap::from([(exec_ceo_id, u64::MAX)]);
                    __corp },
            );
            snap.corporations.insert(
                member_corp_id,
                { let mut __corp = CorporationRecord::default();
                    __corp.corporation_id = member_corp_id;
                    __corp.name = "Member Wing".into();
                    __corp.ticker = "MEMBW".into();
                    __corp.ceo_id = ally_member_id;
                    __corp.alliance_id = Some(alliance_id);
                    __corp.members = HashSet::from([ally_member_id]);
                    __corp.member_roles = HashMap::from([(ally_member_id, u64::MAX)]);
                    __corp },
            );
            snap.corporations.insert(
                outsider_corp_id,
                { let mut __corp = CorporationRecord::default();
                    __corp.corporation_id = outsider_corp_id;
                    __corp.name = "Outsider Corp".into();
                    __corp.ticker = "OUTS".into();
                    __corp.ceo_id = outsider_id;
                    __corp.alliance_id = None;
                    __corp.members = HashSet::from([outsider_id]);
                    __corp },
            );
            snap.corporations.insert(
                target_corp_id,
                { let mut __corp = CorporationRecord::default();
                    __corp.corporation_id = target_corp_id;
                    __corp.name = "Hostile Target Corp".into();
                    __corp.ticker = "HOSTL".into();
                    __corp.ceo_id = 90_000_099;
                    __corp.alliance_id = None;
                    __corp },
            );
            let tpl = snap.characters[&exec_ceo_id].clone();
            for (cid, corpid, allyid, name) in [
                (exec_ceo_id, exec_corp_id, Some(alliance_id), "Exec CEO"),
                (ally_member_id, member_corp_id, Some(alliance_id), "Ally Member"),
                (outsider_id, outsider_corp_id, None, "Outsider"),
            ] {
                let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| tpl.clone());
                ch.character_id = cid;
                ch.name = name.into();
                ch.corporation_id = corpid;
                ch.alliance_id = allyid;
                ch.user_id = 1;
                snap.characters.insert(cid, ch);
            }
        });
        let ctx1 = Arc::new(ServerContext::with_repository(repo1));
        common::pin_clock(&ctx1);

        let mut exec_sess = make_session(
            91_001,
            1,
            exec_ceo_id,
            exec_corp_id,
            Some(alliance_id),
        );

        // 1. Create two alliance contact labels via MachoNet RPC
        let l1_rep = rpc(
            &ctx1,
            &mut exec_sess,
            "allianceRegistry",
            "CreateLabel",
            &[PyRep::String("Friendly Logistics".into()), PyRep::Integer(1)],
        )
        .await;
        let l1_id = l1_rep
            .as_i64()
            .or_else(|| l1_rep.dict_get("labelID").and_then(|v| v.as_i64()))
            .unwrap_or(1);

        let l2_rep = rpc(
            &ctx1,
            &mut exec_sess,
            "allianceRegistry",
            "CreateLabel",
            &[PyRep::String("Hostile Standings".into()), PyRep::Integer(2)],
        )
        .await;
        let l2_id = l2_rep
            .as_i64()
            .or_else(|| l2_rep.dict_get("labelID").and_then(|v| v.as_i64()))
            .unwrap_or(2);

        // 2. Add two alliance contacts via MachoNet RPC
        // Contact 1: target_corp_id with -10.0 standing and label 2
        rpc(
            &ctx1,
            &mut exec_sess,
            "allianceRegistry",
            "AddAllianceContact",
            &[
                PyRep::Integer(target_corp_id),
                PyRep::Real(-10.0),
                PyRep::Integer(l2_id),
            ],
        )
        .await;

        // Contact 2: 500_001 (faction) with +5.0 standing and label 1
        rpc(
            &ctx1,
            &mut exec_sess,
            "allianceRegistry",
            "AddAllianceContact",
            &[
                PyRep::Integer(500_001),
                PyRep::Real(5.0),
                PyRep::Integer(l1_id),
            ],
        )
        .await;

        eve_server::services::fleet_social::persist_social_state(&ctx1);
        ctx1.db.save_to_sqlite(&db_path).expect("save_to_sqlite");
        (l1_id, l2_id)
    };

    // Reload from SQLite disk file into fresh ServerContext
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 2"));
    let ctx2 = Arc::new(ServerContext::with_repository(repo2));
    common::pin_clock(&ctx2);
    let router2 = EsiRouter::new(Arc::clone(&ctx2));

    // Clear ephemeral esi_state completely to prove reads originate from durable state
    ctx2.esi_state.alliance_contacts.write().unwrap().clear();
    ctx2.esi_state
        .alliance_contact_labels
        .write()
        .unwrap()
        .clear();

    let exec_tok = issue_token(
        &ctx2,
        exec_ceo_id,
        vec!["esi-alliances.read_contacts.v1"],
    );
    let member_tok = issue_token(
        &ctx2,
        ally_member_id,
        vec!["esi-alliances.read_contacts.v1"],
    );
    let no_scope_tok = issue_token(&ctx2, exec_ceo_id, vec!["esi-wallet.read_character_wallet.v1"]);
    let outsider_tok = issue_token(
        &ctx2,
        outsider_id,
        vec!["esi-alliances.read_contacts.v1"],
    );

    // ==========================================
    // ESI: GET /alliances/{alliance_id}/contacts
    // ==========================================
    // 401 without auth token
    get_json(
        &router2,
        &spec,
        &format!("/v2/alliances/{alliance_id}/contacts/"),
        None,
        401,
    );

    // 403 when token lacks esi-alliances.read_contacts.v1 scope
    get_json(
        &router2,
        &spec,
        &format!("/v2/alliances/{alliance_id}/contacts/"),
        Some(&no_scope_tok),
        403,
    );

    // 403 when caller is not in the alliance
    get_json(
        &router2,
        &spec,
        &format!("/v2/alliances/{alliance_id}/contacts/"),
        Some(&outsider_tok),
        403,
    );

    // 404 when alliance does not exist
    get_json(
        &router2,
        &spec,
        "/v2/alliances/999999/contacts/",
        Some(&exec_tok),
        404,
    );

    // 200 OK for member with proper ETag, Cache-Control, and X-Pages headers
    assert_etag_and_304(
        &router2,
        &spec,
        &format!("/v2/alliances/{alliance_id}/contacts/"),
        &member_tok,
    );

    let contacts_val = get_json(
        &router2,
        &spec,
        &format!("/v2/alliances/{alliance_id}/contacts/"),
        Some(&exec_tok),
        200,
    );
    let contacts_arr = contacts_val.as_array().expect("array of contacts");
    assert_eq!(contacts_arr.len(), 2);

    let c1 = contacts_arr
        .iter()
        .find(|c| c["contact_id"] == json!(500_001))
        .expect("faction contact");
    assert_eq!(c1["contact_type"], json!("character"));
    assert_eq!(c1["standing"], json!(5.0));
    assert_eq!(c1["label_ids"], json!([label_logistics]));

    let c2 = contacts_arr
        .iter()
        .find(|c| c["contact_id"] == json!(target_corp_id))
        .expect("target corp contact");
    assert_eq!(c2["contact_type"], json!("corporation"));
    assert_eq!(c2["standing"], json!(-10.0));
    assert_eq!(c2["label_ids"], json!([label_diplomatic]));

    // Pagination query parameters
    let page1_val = get_json(
        &router2,
        &spec,
        &format!("/v2/alliances/{alliance_id}/contacts/?page=1"),
        Some(&exec_tok),
        200,
    );
    assert_eq!(page1_val.as_array().unwrap().len(), 2);

    get_json(
        &router2,
        &spec,
        &format!("/v2/alliances/{alliance_id}/contacts/?page=2"),
        Some(&exec_tok),
        404,
    );

    get_json(
        &router2,
        &spec,
        &format!("/v2/alliances/{alliance_id}/contacts/?page=0"),
        Some(&exec_tok),
        400,
    );

    get_json(
        &router2,
        &spec,
        &format!("/v2/alliances/{alliance_id}/contacts/?page=-1"),
        Some(&exec_tok),
        400,
    );

    // =================================================
    // ESI: GET /alliances/{alliance_id}/contacts/labels
    // =================================================
    // 401 without auth token
    get_json(
        &router2,
        &spec,
        &format!("/v1/alliances/{alliance_id}/contacts/labels/"),
        None,
        401,
    );

    // 403 when token lacks scope
    get_json(
        &router2,
        &spec,
        &format!("/v1/alliances/{alliance_id}/contacts/labels/"),
        Some(&no_scope_tok),
        403,
    );

    // 403 when caller is not in the alliance
    get_json(
        &router2,
        &spec,
        &format!("/v1/alliances/{alliance_id}/contacts/labels/"),
        Some(&outsider_tok),
        403,
    );

    // 404 when alliance does not exist
    get_json(
        &router2,
        &spec,
        "/v1/alliances/999999/contacts/labels/",
        Some(&exec_tok),
        404,
    );

    // 200 OK with ETag/304 check
    assert_etag_and_304(
        &router2,
        &spec,
        &format!("/v1/alliances/{alliance_id}/contacts/labels/"),
        &member_tok,
    );

    let labels_val = get_json(
        &router2,
        &spec,
        &format!("/v1/alliances/{alliance_id}/contacts/labels/"),
        Some(&exec_tok),
        200,
    );
    let labels_arr = labels_val.as_array().expect("array of labels");
    assert_eq!(labels_arr.len(), 2);
    assert!(labels_arr
        .iter()
        .any(|l| l["label_id"] == json!(label_logistics)
            && l["label_name"] == json!("Friendly Logistics")));
    assert!(labels_arr
        .iter()
        .any(|l| l["label_id"] == json!(label_diplomatic)
            && l["label_name"] == json!("Hostile Standings")));

    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_contact_notifications_block_suppression_and_50_cap() {
    // parity: esi:GET /characters/{character_id}/notifications/contacts, feat:esi.character.contact-notifications
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/notifications/contacts",
        "feat:esi.character.contact-notifications",
    ]);

    let db_path = temp_db_path("contact_notifs_bf02");
    let sender_id = 90_000_001;
    let target_id = 90_000_002;
    let kw = HashMap::new();

    {
        let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
        let seed_snap = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        repo.with_mut_snapshot(|snap| *snap = seed_snap);
        let ctx = Arc::new(ServerContext::with_repository(repo));
        common::pin_clock(&ctx);
        let router = EsiRouter::new(Arc::clone(&ctx));
        let spec = OpenApiSpec::pinned();

        let mut s_sender = make_session(77_101, 1, sender_id, 1_000_009, None);
        let mut s_target = make_session(77_102, 2, target_id, 1_000_009, None);

        let sender_tok = issue_token(
            &ctx,
            sender_id,
            vec!["esi-characters.read_contacts.v1", "esi-characters.write_contacts.v1"],
        );
        let target_tok = issue_token(&ctx, target_id, vec!["esi-characters.read_notifications.v1"]);

        // 1. Negative standing (-10.0) via ESI POST /contacts must NOT set is_blocked = true
        let post_neg = EsiHttpRequest::new(
            "POST",
            format!("/v2/characters/{sender_id}/contacts/?standing=-10.0&watched=false"),
        )
        .with_bearer_token(&sender_tok)
        .with_body(serde_json::to_vec(&json!([target_id])).unwrap());
        assert_eq!(esi_call(&router, &spec, &post_neg, 201), json!([target_id]));

        let sender_contacts = get_json(
            &router,
            &spec,
            &format!("/v2/characters/{sender_id}/contacts/"),
            Some(&sender_tok),
            200,
        );
        assert_eq!(sender_contacts[0]["contact_id"], json!(target_id));
        assert_eq!(sender_contacts[0]["standing"], json!(-10.0));
        assert_eq!(sender_contacts[0]["is_blocked"], json!(false));

        let init_notifs = get_json(
            &router,
            &spec,
            &format!("/v1/characters/{target_id}/notifications/contacts/"),
            Some(&target_tok),
            200,
        );
        assert_eq!(init_notifs.as_array().unwrap().len(), 1);
        assert_eq!(init_notifs[0]["standing_level"], json!(-10.0));

        // Explicit BlockOwners sets is_blocked = true, and ESI PUT preserves existing is_blocked = true
        rpc(&ctx, &mut s_sender, "charMgr", "BlockOwners", &[PyRep::List(vec![PyRep::Integer(target_id)])]).await;
        let put_blocked = EsiHttpRequest::new(
            "PUT",
            format!("/v2/characters/{sender_id}/contacts/?standing=-8.0&watched=false"),
        )
        .with_bearer_token(&sender_tok)
        .with_body(serde_json::to_vec(&json!([target_id])).unwrap());
        esi_call(&router, &spec, &put_blocked, 204);
        let after_put = get_json(&router, &spec, &format!("/v2/characters/{sender_id}/contacts/"), Some(&sender_tok), 200);
        assert_eq!(after_put[0]["is_blocked"], json!(true));
        rpc(&ctx, &mut s_sender, "charMgr", "UnblockOwners", &[PyRep::List(vec![PyRep::Integer(target_id)])]).await;

        // 2. Block suppression on BOTH charMgr.AddContact/EditContact AND ESI POST /contacts
        rpc(&ctx, &mut s_target, "charMgr", "BlockOwners", &[PyRep::List(vec![PyRep::Integer(sender_id)])]).await;
        rpc(
            &ctx, &mut s_sender, "charMgr", "AddContact",
            &[PyRep::Integer(target_id), PyRep::Real(5.0), PyRep::Bool(false), PyRep::Bool(true), PyRep::String("Blocked add".into())],
        ).await;
        rpc(
            &ctx, &mut s_sender, "charMgr", "EditContact",
            &[PyRep::Integer(target_id), PyRep::Real(6.0), PyRep::Bool(false), PyRep::Integer(1), PyRep::String("Blocked edit".into())],
        ).await;
        let post_blocked = EsiHttpRequest::new(
            "POST",
            format!("/v2/characters/{sender_id}/contacts/?standing=7.0&watched=false"),
        )
        .with_bearer_token(&sender_tok)
        .with_body(serde_json::to_vec(&json!([target_id])).unwrap());
        assert_eq!(esi_call(&router, &spec, &post_blocked, 201), json!([target_id]));

        let still_one = get_json(&router, &spec, &format!("/v1/characters/{target_id}/notifications/contacts/"), Some(&target_tok), 200);
        assert_eq!(still_one.as_array().unwrap().len(), 1, "blocked sender must not add notifications via charMgr or ESI POST");

        // 3. Unblock, verify notify=0/false suppression, integer notify=1 & positional note without corrupting labelMask, and 50-cap
        rpc(&ctx, &mut s_target, "charMgr", "UnblockOwners", &[PyRep::List(vec![PyRep::Integer(sender_id)])]).await;
        rpc(
            &ctx, &mut s_sender, "charMgr", "EditContact",
            &[PyRep::Integer(target_id), PyRep::Real(4.0), PyRep::Bool(true), PyRep::Integer(0), PyRep::String("No notify".into())],
        ).await;
        assert_eq!(get_json(&router, &spec, &format!("/v1/characters/{target_id}/notifications/contacts/"), Some(&target_tok), 200).as_array().unwrap().len(), 1);

        let add_res = ctx.dispatch_rpc(
            &mut s_sender, "charMgr", None, "AddContact",
            &[PyRep::Integer(target_id), PyRep::Real(9.0), PyRep::Bool(true), PyRep::Integer(1), PyRep::String("Msg 1".into())],
            &kw, None,
        ).await.expect("AddContact int notify");
        assert_eq!(add_res.dict_get("labelMask"), Some(&PyRep::Integer(0)), "args[3]=1 is notify, not labelMask");

        for idx in 2..=54 {
            rpc(
                &ctx, &mut s_sender, "charMgr", "EditContact",
                &[PyRep::Integer(target_id), PyRep::Real(10.0), PyRep::Bool(true), PyRep::Bool(true), PyRep::String(format!("Msg {idx}"))],
            ).await;
        }
        assert_eq!(esi_call(&router, &spec, &post_blocked, 201), json!([target_id])); // 55th post-unblock notification via ESI POST

        let cnotifs = get_json(&router, &spec, &format!("/v1/characters/{target_id}/notifications/contacts/"), Some(&target_tok), 200);
        let arr = cnotifs.as_array().unwrap();
        assert_eq!(arr.len(), 50);
        assert_eq!(arr[0]["message"], json!(format!("Character {sender_id} added you to contacts")));
        assert_eq!(arr[1]["message"], json!("Msg 54"));
        assert_eq!(arr[49]["message"], json!("Msg 6"));
    }

    // 4. Verify SQLite restart durability of the capped 50 notifications
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = Arc::new(ServerContext::with_repository(repo2));
    common::pin_clock(&ctx2);
    let router2 = EsiRouter::new(Arc::clone(&ctx2));
    let spec2 = OpenApiSpec::pinned();
    let target_tok2 = issue_token(&ctx2, target_id, vec!["esi-characters.read_notifications.v1"]);
    let persisted = get_json(&router2, &spec2, &format!("/v1/characters/{target_id}/notifications/contacts/"), Some(&target_tok2), 200);
    let parr = persisted.as_array().unwrap();
    assert_eq!(parr.len(), 50);
    assert_eq!(parr[1]["message"], json!("Msg 54"));
    assert_eq!(parr[49]["message"], json!("Msg 6"));

    let _ = std::fs::remove_file(&db_path);
}

}
