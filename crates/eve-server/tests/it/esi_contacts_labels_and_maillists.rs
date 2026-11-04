//! End-to-end MachoNet RPC + ESI + SQLite restart parity tests for:
//! - `GET /characters/{character_id}/contacts/labels`
//! - `GET /corporations/{corporation_id}/contacts` (`feat:esi.contacts.corp`)
//! - `GET /corporations/{corporation_id}/contacts/labels`
//! - `GET /alliances/{alliance_id}/contacts` (`feat:esi.contacts.alliance`)
//! - `GET /alliances/{alliance_id}/contacts/labels`
//! - `GET /characters/{character_id}/mail/lists` (`feat:esi.mail.lists`)

#![forbid(unsafe_code)]

use crate::common;
use eve_db::corp_roles::ROLE_DIRECTOR;
use eve_db::{AllianceRecord, CorporationRecord, IskCents, WorldRepository};
use eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
static DB_SEQ: AtomicU64 = AtomicU64::new(1);

fn temp_db_path(tag: &str) -> PathBuf {
    let seq = DB_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let p = std::env::temp_dir().join(format!("evomu_esi_contacts_maillists_{tag}_{}_{seq}_{nanos}.sqlite", std::process::id()));
    cleanup_sqlite_files(&p);
    p
}

fn cleanup_sqlite_files(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}

fn issue_token(ctx: &ServerContext, character_id: i64, scopes: &[&str]) -> String {
    let now = ctx.now_unix();
    let scps: Vec<String> = scopes.iter().map(|s| (*s).to_string()).collect();
    let code = ctx.db.authorize_pkce("demo", "demo-secret", character_id, "test-client", "https://localhost/cb", scps, PKCE_CHALLENGE, "S256", now).expect("authorize_pkce");
    ctx.db.exchange_pkce_code(&code, "test-client", PKCE_VERIFIER, now).expect("exchange_pkce_code").access_token
}

fn esi_get(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: Option<&str>, expected_status: u16) -> (Value, HashMap<String, String>) {
    let mut req = EsiHttpRequest::new("GET", path);
    if let Some(tok) = token {
        req = req.with_bearer_token(tok);
    }
    let resp = router.handle_request(&req);
    let raw_path = req.raw_target.split('?').next().unwrap_or(&req.raw_target);
    assert_eq!(resp.status_code, expected_status, "GET {path} -> {} (body: {})", resp.status_code, String::from_utf8_lossy(&resp.body));
    spec.validate_response("GET", raw_path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI validation failure for GET {path}: {e}"));
    let body = if expected_status == 204 || expected_status == 304 { Value::Null } else { resp.json().expect("valid JSON response") };
    (body, resp.headers)
}

fn assert_etag_and_304(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: &str) {
    let req = EsiHttpRequest::new("GET", path).with_bearer_token(token);
    let resp = router.handle_request(&req);
    assert_eq!(resp.status_code, 200);
    let etag = resp.header("etag").expect("ETag header").to_string();
    assert!(resp.header("cache-control").is_some());
    let mut cond = req.clone();
    cond.headers.insert("if-none-match".to_string(), etag);
    let cond_resp = router.handle_request(&cond);
    assert_eq!(cond_resp.status_code, 304);
    let raw_path = cond.raw_target.split('?').next().unwrap_or(&cond.raw_target);
    spec.validate_response("GET", raw_path, cond_resp.status_code, &cond_resp.headers, &cond_resp.body).expect("304 OpenAPI validation");
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method} failed: {e:?}"))
}

fn extract_label_id(rep: &PyRep) -> i64 {
    rep.as_i64()
        .or_else(|| rep.dict_get("labelID").and_then(|v| v.as_i64()))
        .expect("labelID integer")
}

fn make_session(
    sess_id: u64,
    cid: i64,
    corp_id: i64,
    alliance_id: Option<i64>,
    corp_role: u64,
) -> SessionState {
    let mut s = SessionState::new(sess_id);
    s.user_id = Some(1);
    s.role = ROLE_PLAYER_DEFAULT;
    s.character_id = Some(cid);
    s.corporation_id = Some(corp_id);
    s.alliance_id = alliance_id;
    s.corp_role = corp_role;
    s.station_id = Some(60_003_760);
    s
}

fn seed_orgs_and_pilots_sqlite(db_path: &Path) -> Arc<WorldRepository> {
    let repo = Arc::new(WorldRepository::open_sqlite(db_path).expect("open_sqlite"));
    repo.with_mut_snapshot(|snap| {
        *snap = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        let ceo_id = 90_000_001_i64;
        let line_member_id = 90_000_002_i64;
        let ally_pilot_id = 90_000_003_i64;
        let outsider_id = 90_000_004_i64;
        let exec_corp_id = 98_710_001_i64;
        let member_corp_id = 98_710_002_i64;
        let outsider_corp_id = 98_710_003_i64;
        let rival_corp_id = 98_710_099_i64;
        let alliance_id = 99_710_001_i64;
        let rival_alliance_id = 99_710_099_i64;

        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "Vanguard Coalition".into(),
                short_name: "VGDC".into(),
                executor_corp_id: exec_corp_id,
                member_corp_ids: HashSet::from([exec_corp_id, member_corp_id]),
            },
        );
        snap.alliances.insert(
            rival_alliance_id,
            AllianceRecord {
                alliance_id: rival_alliance_id,
                name: "Red Syndicate".into(),
                short_name: "REDS".into(),
                executor_corp_id: rival_corp_id,
                member_corp_ids: HashSet::from([rival_corp_id]),
            },
        );
        snap.corporations.insert(
            exec_corp_id,
            CorporationRecord {
                corporation_id: exec_corp_id,
                name: "Vanguard Executor".into(),
                ticker: "VGEX".into(),
                ceo_id,
                alliance_id: Some(alliance_id),
                tax_rate: 0.05,
                members: HashSet::from([ceo_id, line_member_id]),
                member_roles: HashMap::from([(ceo_id, ROLE_DIRECTOR), (line_member_id, 0)]),
                ..CorporationRecord::default()
            },
        );
        snap.corporations.insert(
            member_corp_id,
            CorporationRecord {
                corporation_id: member_corp_id,
                name: "Vanguard Wing".into(),
                ticker: "VGWNG".into(),
                ceo_id: ally_pilot_id,
                alliance_id: Some(alliance_id),
                members: HashSet::from([ally_pilot_id]),
                member_roles: HashMap::from([(ally_pilot_id, ROLE_DIRECTOR)]),
                ..CorporationRecord::default()
            },
        );
        snap.corporations.insert(
            outsider_corp_id,
            CorporationRecord {
                corporation_id: outsider_corp_id,
                name: "Independent Haulers".into(),
                ticker: "INDH".into(),
                ceo_id: outsider_id,
                members: HashSet::from([outsider_id]),
                ..CorporationRecord::default()
            },
        );
        snap.corporations.insert(
            rival_corp_id,
            CorporationRecord {
                corporation_id: rival_corp_id,
                name: "Syndicate Raiders".into(),
                ticker: "SYNR".into(),
                ceo_id: 90_000_099,
                alliance_id: Some(rival_alliance_id),
                ..CorporationRecord::default()
            },
        );

        let tpl = snap.characters[&ceo_id].clone();
        for (cid, corpid, allyid, name) in [
            (ceo_id, exec_corp_id, Some(alliance_id), "Vanguard CEO"),
            (line_member_id, exec_corp_id, Some(alliance_id), "Line Pilot"),
            (ally_pilot_id, member_corp_id, Some(alliance_id), "Wing Pilot"),
            (outsider_id, outsider_corp_id, None, "Neutral Outsider"),
        ] {
            let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| tpl.clone());
            ch.character_id = cid;
            ch.name = name.into();
            ch.corporation_id = corpid;
            ch.alliance_id = allyid;
            ch.user_id = 1;
            ch.balance = IskCents::from_isk(100_000_000);
            snap.characters.insert(cid, ch);
        }
    });
    repo
}

#[tokio::test]
async fn test_char_and_corp_contacts_labels_macho_esi_sqlite_roundtrip() {
    // parity: esi:GET /characters/{character_id}/contacts/labels
    // parity: esi:GET /corporations/{corporation_id}/contacts
    // parity: esi:GET /corporations/{corporation_id}/contacts/labels
    // parity: feat:esi.contacts.corp
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/contacts/labels",
        "esi:GET /corporations/{corporation_id}/contacts",
        "esi:GET /corporations/{corporation_id}/contacts/labels",
        "feat:esi.contacts.corp",
    ]);

    let db_path = temp_db_path("char_corp_contacts");
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let line_member_id = 90_000_002_i64;
    let outsider_id = 90_000_004_i64;
    let exec_corp_id = 98_710_001_i64;
    let rival_corp_id = 98_710_099_i64;
    let alliance_id = 99_710_001_i64;
    let rival_alliance_id = 99_710_099_i64;
    let faction_id = 500_001_i64;

    let (char_l1, char_l2, corp_l1, corp_l2) = {
        let repo1 = seed_orgs_and_pilots_sqlite(&db_path);
        let ctx1 = Arc::new(ServerContext::with_repository(repo1));
        common::pin_clock(&ctx1);
        let router1 = EsiRouter::new(Arc::clone(&ctx1));
        let mut ceo_sess = make_session(92_001, ceo_id, exec_corp_id, Some(alliance_id), ROLE_DIRECTOR);

        // 1. Character contact labels via charMgr: Create, Edit, Delete, and AddContact interaction
        let cl1 = extract_label_id(
            &rpc(&ctx1, &mut ceo_sess, "charMgr", "CreateLabel", &[PyRep::String("Fleet Anchors".into())]).await,
        );
        let cl2 = extract_label_id(
            &rpc(&ctx1, &mut ceo_sess, "charMgr", "CreateLabel", &[PyRep::String("Draft Label".into())]).await,
        );
        let cl_temp = extract_label_id(
            &rpc(&ctx1, &mut ceo_sess, "charMgr", "CreateLabel", &[PyRep::String("Obsolete".into())]).await,
        );
        rpc(
            &ctx1,
            &mut ceo_sess,
            "charMgr",
            "EditLabel",
            &[PyRep::Integer(cl2), PyRep::String("Cyno Hunters".into())],
        )
        .await;
        // Adding a character contact afterwards must NOT clobber persisted char_contact_labels
        rpc(
            &ctx1,
            &mut ceo_sess,
            "charMgr",
            "AddContact",
            &[
                PyRep::Integer(outsider_id),
                PyRep::Real(5.0),
                PyRep::Bool(true),
                PyRep::Bool(false),
                PyRep::String(String::new()),
            ],
        )
        .await;
        rpc(
            &ctx1,
            &mut ceo_sess,
            "charMgr",
            "AssignLabels",
            &[PyRep::List(vec![PyRep::Integer(outsider_id)]), PyRep::Integer(cl1 | cl_temp)],
        )
        .await;
        rpc(&ctx1, &mut ceo_sess, "charMgr", "DeleteLabel", &[PyRep::Integer(cl_temp)]).await;

        // 2. Corporate contacts & labels via corpRegistry: Create, Edit, Delete, Add, Edit, Assign, Remove
        let cpl1 = extract_label_id(
            &rpc(&ctx1, &mut ceo_sess, "corpRegistry", "CreateLabel", &[PyRep::String("Coalition Blues".into()), PyRep::Integer(0x00FF00)]).await,
        );
        let cpl2 = extract_label_id(
            &rpc(&ctx1, &mut ceo_sess, "corpRegistry", "CreateLabel", &[PyRep::String("Temp Hostile".into()), PyRep::Integer(0xFF0000)]).await,
        );
        let cpl_del = extract_label_id(
            &rpc(&ctx1, &mut ceo_sess, "corpRegistry", "CreateLabel", &[PyRep::String("Scratch".into()), PyRep::Integer(0x888888)]).await,
        );
        rpc(
            &ctx1,
            &mut ceo_sess,
            "corpRegistry",
            "EditLabel",
            &[PyRep::Integer(cpl2), PyRep::String("Kill On Sight".into()), PyRep::Integer(0xFF0000)],
        )
        .await;

        // Add character, corporation, alliance, and faction contacts + a transient contact to remove
        rpc(&ctx1, &mut ceo_sess, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(outsider_id), PyRep::Real(-5.0), PyRep::Integer(cpl2 | cpl_del)]).await;
        rpc(&ctx1, &mut ceo_sess, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(rival_corp_id), PyRep::Real(-10.0), PyRep::Integer(0)]).await;
        rpc(&ctx1, &mut ceo_sess, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(rival_alliance_id), PyRep::Real(10.0), PyRep::Integer(cpl1)]).await;
        rpc(&ctx1, &mut ceo_sess, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(faction_id), PyRep::Real(2.5), PyRep::Integer(0)]).await;
        rpc(&ctx1, &mut ceo_sess, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(line_member_id), PyRep::Real(5.0), PyRep::Integer(0)]).await;

        rpc(&ctx1, &mut ceo_sess, "corpRegistry", "EditCorporateContact", &[PyRep::Integer(outsider_id), PyRep::Real(-8.5), PyRep::Integer(cpl2 | cpl_del)]).await;
        rpc(&ctx1, &mut ceo_sess, "corpRegistry", "AssignLabels", &[PyRep::List(vec![PyRep::Integer(rival_corp_id)]), PyRep::Integer(cpl2)]).await;
        rpc(&ctx1, &mut ceo_sess, "corpRegistry", "DeleteLabel", &[PyRep::Integer(cpl_del)]).await;
        rpc(&ctx1, &mut ceo_sess, "corpRegistry", "RemoveCorporateContacts", &[PyRep::List(vec![PyRep::Integer(line_member_id)])]).await;

        // Pre-restart auth, scope, pagination, and ETag checks
        let ceo_tok = issue_token(&ctx1, ceo_id, &["esi-characters.read_contacts.v1", "esi-corporations.read_contacts.v1"]);
        let outsider_tok = issue_token(&ctx1, outsider_id, &["esi-characters.read_contacts.v1", "esi-corporations.read_contacts.v1"]);
        let wrong_scope_tok = issue_token(&ctx1, ceo_id, &["esi-mail.read_mail.v1"]);

        esi_get(&router1, &spec, &format!("/v1/characters/{ceo_id}/contacts/labels/"), None, 401);
        esi_get(&router1, &spec, &format!("/v1/characters/{ceo_id}/contacts/labels/"), Some(&wrong_scope_tok), 403);
        esi_get(&router1, &spec, &format!("/v1/characters/{ceo_id}/contacts/labels/"), Some(&outsider_tok), 403);

        esi_get(&router1, &spec, &format!("/v2/corporations/{exec_corp_id}/contacts/"), None, 401);
        esi_get(&router1, &spec, &format!("/v2/corporations/{exec_corp_id}/contacts/"), Some(&wrong_scope_tok), 403);
        esi_get(&router1, &spec, &format!("/v2/corporations/{exec_corp_id}/contacts/"), Some(&outsider_tok), 403);
        esi_get(&router1, &spec, &format!("/v2/corporations/{exec_corp_id}/contacts/?page=0"), Some(&ceo_tok), 400);
        esi_get(&router1, &spec, &format!("/v2/corporations/{exec_corp_id}/contacts/?page=2"), Some(&ceo_tok), 404);
        esi_get(&router1, &spec, &format!("/v2/corporations/{rival_corp_id}/contacts/"), Some(&ceo_tok), 403);
        esi_get(&router1, &spec, &format!("/v1/corporations/{rival_corp_id}/contacts/labels/"), Some(&ceo_tok), 403);

        assert_etag_and_304(&router1, &spec, &format!("/v1/characters/{ceo_id}/contacts/labels/"), &ceo_tok);
        assert_etag_and_304(&router1, &spec, &format!("/v2/corporations/{exec_corp_id}/contacts/"), &ceo_tok);
        assert_etag_and_304(&router1, &spec, &format!("/v1/corporations/{exec_corp_id}/contacts/labels/"), &ceo_tok);

        (cl1, cl2, cpl1, cpl2)
    };

    // Reopen SQLite file in a brand-new ServerContext + EsiRouter
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = Arc::new(ServerContext::with_repository(repo2));
    common::pin_clock(&ctx2);
    let router2 = EsiRouter::new(Arc::clone(&ctx2));

    let ceo_tok = issue_token(&ctx2, ceo_id, &["esi-characters.read_contacts.v1", "esi-corporations.read_contacts.v1"]);
    // Non-director corporation member can also read corporate contacts & labels
    let line_tok = issue_token(&ctx2, line_member_id, &["esi-corporations.read_contacts.v1"]);

    let (char_labels, _) = esi_get(&router2, &spec, &format!("/v1/characters/{ceo_id}/contacts/labels/"), Some(&ceo_tok), 200);
    assert_eq!(
        char_labels,
        json!([
            { "label_id": char_l1, "label_name": "Fleet Anchors" },
            { "label_id": char_l2, "label_name": "Cyno Hunters" }
        ])
    );

    let (corp_labels, _) = esi_get(&router2, &spec, &format!("/v1/corporations/{exec_corp_id}/contacts/labels/"), Some(&line_tok), 200);
    assert_eq!(
        corp_labels,
        json!([
            { "label_id": corp_l1, "label_name": "Coalition Blues" },
            { "label_id": corp_l2, "label_name": "Kill On Sight" }
        ])
    );

    let (corp_contacts, headers) = esi_get(&router2, &spec, &format!("/v2/corporations/{exec_corp_id}/contacts/"), Some(&line_tok), 200);
    assert_eq!(headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("x-pages")).map(|(_, v)| v.as_str()), Some("1"));
    assert_eq!(
        corp_contacts,
        json!([
            {
                "contact_id": faction_id,
                "contact_type": "faction",
                "is_watched": false,
                "standing": 2.5
            },
            {
                "contact_id": outsider_id,
                "contact_type": "character",
                "is_watched": false,
                "label_ids": [corp_l2],
                "standing": -8.5
            },
            {
                "contact_id": rival_corp_id,
                "contact_type": "corporation",
                "is_watched": false,
                "label_ids": [corp_l2],
                "standing": -10.0
            },
            {
                "contact_id": rival_alliance_id,
                "contact_type": "alliance",
                "is_watched": false,
                "label_ids": [corp_l1],
                "standing": 10.0
            }
        ])
    );

    // Verify MachoNet RPC reads match persisted SQLite state after restart
    let mut ceo_sess2 = make_session(92_002, ceo_id, exec_corp_id, Some(alliance_id), ROLE_DIRECTOR);
    let macho_char_labels = rpc(&ctx2, &mut ceo_sess2, "charMgr", "GetLabels", &[]).await;
    let macho_char_str = format!("{macho_char_labels:?}");
    assert!(macho_char_str.contains("Fleet Anchors") && macho_char_str.contains("Cyno Hunters"));
    assert!(!macho_char_str.contains("Obsolete"));

    let macho_corp_contacts = rpc(&ctx2, &mut ceo_sess2, "corpRegistry", "GetCorporateContacts", &[]).await;
    let macho_corp_str = format!("{macho_corp_contacts:?}");
    assert!(macho_corp_str.contains(&outsider_id.to_string()) && macho_corp_str.contains("-8.5"));
    assert!(!macho_corp_str.contains(&line_member_id.to_string()));

    cleanup_sqlite_files(&db_path);
}

#[tokio::test]
async fn test_alliance_contacts_labels_and_mail_lists_macho_esi_sqlite_roundtrip() {
    // parity: esi:GET /alliances/{alliance_id}/contacts
    // parity: esi:GET /alliances/{alliance_id}/contacts/labels
    // parity: esi:GET /characters/{character_id}/mail/lists
    // parity: feat:esi.contacts.alliance
    // parity: feat:esi.mail.lists
    eve_testkit::attest(&[
        "esi:GET /alliances/{alliance_id}/contacts",
        "esi:GET /alliances/{alliance_id}/contacts/labels",
        "esi:GET /characters/{character_id}/mail/lists",
        "feat:esi.contacts.alliance",
        "feat:esi.mail.lists",
    ]);

    let db_path = temp_db_path("ally_contacts_maillists");
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let ally_pilot_id = 90_000_003_i64;
    let outsider_id = 90_000_004_i64;
    let exec_corp_id = 98_710_001_i64;
    let member_corp_id = 98_710_002_i64;
    let rival_corp_id = 98_710_099_i64;
    let alliance_id = 99_710_001_i64;
    let rival_alliance_id = 99_710_099_i64;

    let (ally_l1, ally_l2, ops_list_id, roam_list_id) = {
        let repo1 = seed_orgs_and_pilots_sqlite(&db_path);
        let ctx1 = Arc::new(ServerContext::with_repository(repo1));
        common::pin_clock(&ctx1);
        let router1 = EsiRouter::new(Arc::clone(&ctx1));

        let mut exec_sess = make_session(92_010, ceo_id, exec_corp_id, Some(alliance_id), ROLE_DIRECTOR);
        let mut wing_sess = make_session(92_011, ally_pilot_id, member_corp_id, Some(alliance_id), ROLE_DIRECTOR);

        // 1. Alliance contacts & labels via allianceRegistry
        let al1 = extract_label_id(
            &rpc(&ctx1, &mut exec_sess, "allianceRegistry", "CreateLabel", &[PyRep::String("Mutual Defense".into()), PyRep::Integer(1)]).await,
        );
        let al2 = extract_label_id(
            &rpc(&ctx1, &mut exec_sess, "allianceRegistry", "CreateLabel", &[PyRep::String("Draft Red".into()), PyRep::Integer(2)]).await,
        );
        let al_del = extract_label_id(
            &rpc(&ctx1, &mut exec_sess, "allianceRegistry", "CreateLabel", &[PyRep::String("Deprecated".into()), PyRep::Integer(3)]).await,
        );
        rpc(
            &ctx1,
            &mut exec_sess,
            "allianceRegistry",
            "EditLabel",
            &[PyRep::Integer(al2), PyRep::String("Primary Targets".into()), PyRep::Integer(2)],
        )
        .await;

        rpc(&ctx1, &mut exec_sess, "allianceRegistry", "AddAllianceContact", &[PyRep::Integer(outsider_id), PyRep::Real(-5.0), PyRep::Integer(al2 | al_del)]).await;
        rpc(&ctx1, &mut exec_sess, "allianceRegistry", "AddAllianceContact", &[PyRep::Integer(rival_corp_id), PyRep::Real(-10.0), PyRep::Integer(0)]).await;
        rpc(&ctx1, &mut exec_sess, "allianceRegistry", "AddAllianceContact", &[PyRep::Integer(rival_alliance_id), PyRep::Real(10.0), PyRep::Integer(al1)]).await;
        rpc(&ctx1, &mut exec_sess, "allianceRegistry", "AddAllianceContact", &[PyRep::Integer(member_corp_id), PyRep::Real(5.0), PyRep::Integer(0)]).await;

        rpc(&ctx1, &mut exec_sess, "allianceRegistry", "AssignLabels", &[PyRep::Integer(al2), PyRep::List(vec![PyRep::Integer(rival_corp_id)])]).await;
        rpc(&ctx1, &mut exec_sess, "allianceRegistry", "DeleteLabel", &[PyRep::Integer(al_del)]).await;
        rpc(&ctx1, &mut exec_sess, "allianceRegistry", "RemoveAllianceContacts", &[PyRep::List(vec![PyRep::Integer(member_corp_id)])]).await;

        // 2. Mailing lists via mailingListsMgr: Create, Join, Leave, Delete
        let ops_lid = rpc(
            &ctx1,
            &mut exec_sess,
            "mailingListsMgr",
            "Create",
            &[PyRep::String("Strategic Fleet Ops".into()), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(0)],
        )
        .await
        .as_i64()
        .expect("ops_lid");

        let roam_lid = rpc(
            &ctx1,
            &mut exec_sess,
            "mailingListsMgr",
            "Create",
            &[PyRep::String("Skirmish Roams".into()), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(0)],
        )
        .await
        .as_i64()
        .expect("roam_lid");

        let deleted_lid = rpc(
            &ctx1,
            &mut exec_sess,
            "mailingListsMgr",
            "Create",
            &[PyRep::String("Disbanded SIG".into()), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(0)],
        )
        .await
        .as_i64()
        .expect("deleted_lid");

        // Wing pilot joins both active lists, then leaves Skirmish Roams; CEO deletes Disbanded SIG
        rpc(&ctx1, &mut wing_sess, "mailingListsMgr", "Join", &[PyRep::Integer(ops_lid)]).await;
        rpc(&ctx1, &mut wing_sess, "mailingListsMgr", "Join", &[PyRep::Integer(roam_lid)]).await;
        rpc(&ctx1, &mut wing_sess, "mailingListsMgr", "Leave", &[PyRep::Integer(roam_lid)]).await;
        rpc(&ctx1, &mut exec_sess, "mailingListsMgr", "Delete", &[PyRep::Integer(deleted_lid)]).await;

        // Pre-restart auth, scope, membership, pagination, and ETag checks
        let wing_tok = issue_token(&ctx1, ally_pilot_id, &["esi-alliances.read_contacts.v1", "esi-mail.read_mail.v1"]);
        let outsider_tok = issue_token(&ctx1, outsider_id, &["esi-alliances.read_contacts.v1", "esi-mail.read_mail.v1"]);
        let wrong_scope_tok = issue_token(&ctx1, ceo_id, &["esi-characters.read_contacts.v1"]);

        esi_get(&router1, &spec, &format!("/v2/alliances/{alliance_id}/contacts/"), None, 401);
        esi_get(&router1, &spec, &format!("/v2/alliances/{alliance_id}/contacts/"), Some(&wrong_scope_tok), 403);
        esi_get(&router1, &spec, &format!("/v2/alliances/{alliance_id}/contacts/"), Some(&outsider_tok), 403);
        esi_get(&router1, &spec, &format!("/v2/alliances/{alliance_id}/contacts/?page=0"), Some(&wing_tok), 400);
        esi_get(&router1, &spec, &format!("/v2/alliances/{alliance_id}/contacts/?page=2"), Some(&wing_tok), 404);
        esi_get(&router1, &spec, "/v2/alliances/99999999/contacts/", Some(&wing_tok), 404);
        esi_get(&router1, &spec, "/v1/alliances/99999999/contacts/labels/", Some(&wing_tok), 404);

        esi_get(&router1, &spec, &format!("/v1/characters/{ally_pilot_id}/mail/lists/"), None, 401);
        esi_get(&router1, &spec, &format!("/v1/characters/{ally_pilot_id}/mail/lists/"), Some(&wrong_scope_tok), 403);
        esi_get(&router1, &spec, &format!("/v1/characters/{ally_pilot_id}/mail/lists/"), Some(&outsider_tok), 403);

        assert_etag_and_304(&router1, &spec, &format!("/v2/alliances/{alliance_id}/contacts/"), &wing_tok);
        assert_etag_and_304(&router1, &spec, &format!("/v1/alliances/{alliance_id}/contacts/labels/"), &wing_tok);
        assert_etag_and_304(&router1, &spec, &format!("/v1/characters/{ally_pilot_id}/mail/lists/"), &wing_tok);

        (al1, al2, ops_lid, roam_lid)
    };

    // Reopen SQLite file in a brand-new ServerContext + EsiRouter
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = Arc::new(ServerContext::with_repository(repo2));
    common::pin_clock(&ctx2);
    let router2 = EsiRouter::new(Arc::clone(&ctx2));

    let ceo_tok = issue_token(&ctx2, ceo_id, &["esi-alliances.read_contacts.v1", "esi-mail.read_mail.v1"]);
    let wing_tok = issue_token(&ctx2, ally_pilot_id, &["esi-alliances.read_contacts.v1", "esi-mail.read_mail.v1"]);

    let (ally_labels, _) = esi_get(&router2, &spec, &format!("/v1/alliances/{alliance_id}/contacts/labels/"), Some(&wing_tok), 200);
    assert_eq!(
        ally_labels,
        json!([
            { "label_id": ally_l1, "label_name": "Mutual Defense" },
            { "label_id": ally_l2, "label_name": "Primary Targets" }
        ])
    );

    let (ally_contacts, headers) = esi_get(&router2, &spec, &format!("/v2/alliances/{alliance_id}/contacts/"), Some(&wing_tok), 200);
    assert_eq!(headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("x-pages")).map(|(_, v)| v.as_str()), Some("1"));
    assert_eq!(
        ally_contacts,
        json!([
            {
                "contact_id": outsider_id,
                "contact_type": "character",
                "label_ids": [ally_l2],
                "standing": -5.0
            },
            {
                "contact_id": rival_corp_id,
                "contact_type": "corporation",
                "label_ids": [ally_l2],
                "standing": -10.0
            },
            {
                "contact_id": rival_alliance_id,
                "contact_type": "alliance",
                "label_ids": [ally_l1],
                "standing": 10.0
            }
        ])
    );

    // CEO remains subscribed to both active mailing lists; Disbanded SIG was deleted
    let (ceo_lists, _) = esi_get(&router2, &spec, &format!("/v1/characters/{ceo_id}/mail/lists/"), Some(&ceo_tok), 200);
    assert_eq!(
        ceo_lists,
        json!([
            { "mailing_list_id": ops_list_id, "name": "Strategic Fleet Ops" },
            { "mailing_list_id": roam_list_id, "name": "Skirmish Roams" }
        ])
    );

    // Wing pilot joined Strategic Fleet Ops and left Skirmish Roams
    let (wing_lists, _) = esi_get(&router2, &spec, &format!("/v1/characters/{ally_pilot_id}/mail/lists/"), Some(&wing_tok), 200);
    assert_eq!(
        wing_lists,
        json!([
            { "mailing_list_id": ops_list_id, "name": "Strategic Fleet Ops" }
        ])
    );

    // Verify MachoNet mailingListsMgr & allianceRegistry reads after restart
    let mut wing_sess2 = make_session(92_012, ally_pilot_id, member_corp_id, Some(alliance_id), ROLE_DIRECTOR);
    let joined_rpc = rpc(&ctx2, &mut wing_sess2, "mailingListsMgr", "GetJoinedLists", &[]).await;
    let joined_str = format!("{joined_rpc:?}");
    assert!(joined_str.contains("Strategic Fleet Ops") && !joined_str.contains("Skirmish Roams"));

    let ally_contacts_rpc = rpc(&ctx2, &mut wing_sess2, "allianceRegistry", "GetAllianceContacts", &[]).await;
    let ally_rpc_str = format!("{ally_contacts_rpc:?}");
    assert!(ally_rpc_str.contains(&rival_alliance_id.to_string()) && !ally_rpc_str.contains(&member_corp_id.to_string()));

    cleanup_sqlite_files(&db_path);
}
