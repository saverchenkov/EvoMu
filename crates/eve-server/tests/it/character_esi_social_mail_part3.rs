//! ESI-4 integration tests (Part 3): Character/Corp/Alliance contacts & labels and Mail
//! send/read/update/delete/labels/lists backed by MachoNet RPCs + SQLite restart persistence.

#![forbid(unsafe_code)]

use crate::character_esi_social_mail::{
    assert_etag_and_304, issue_token_with_scopes, validate_and_json,
};
use eve_db::corp_roles::ROLE_DIRECTOR;
use eve_db::{AllianceRecord, CorporationRecord, IskCents, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

fn temp_sqlite_path(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "evomu_esi_social_mail_{}_{}.sqlite",
        tag,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

fn seed_demo_sqlite(path: &PathBuf) -> Arc<WorldRepository> {
    let repo = Arc::new(WorldRepository::open_sqlite(path).expect("open_sqlite"));
    repo.with_mut_snapshot(|snap| {
        *snap = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    });
    repo
}

async fn rpc_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> PyRep {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, svc, None, method, args, &kw, None)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method} failed: {e:?}"))
}

#[tokio::test]
async fn test_esi4_contacts_char_crud_and_labels() {
    // parity: esi:GET /characters/{character_id}/contacts, esi:POST /characters/{character_id}/contacts
    // parity: esi:PUT /characters/{character_id}/contacts, esi:DELETE /characters/{character_id}/contacts
    // parity: esi:GET /characters/{character_id}/contacts/labels
    // parity: feat:esi.contacts.char-get, feat:esi.contacts.char-write, feat:esi.contacts.char-labels
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/contacts",
        "esi:POST /characters/{character_id}/contacts",
        "esi:PUT /characters/{character_id}/contacts",
        "esi:DELETE /characters/{character_id}/contacts",
        "esi:GET /characters/{character_id}/contacts/labels",
        "feat:esi.contacts.char-get",
        "feat:esi.contacts.char-write",
        "feat:esi.contacts.char-labels",
    ]);

    let db_path = temp_sqlite_path("char_contacts_labels");
    let char_id = 90_000_001;
    let contact_char_id = 90_000_002;

    {
        let repo1 = seed_demo_sqlite(&db_path);
        let ctx1 = ServerContext::with_repository(repo1);
        crate::common::pin_clock(&ctx1);
        let mut sess = SessionState::new(81_001);
        sess.user_id = Some(1);
        sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
        sess.character_id = Some(char_id);
        rpc_call(
            &ctx1,
            &mut sess,
            "addressbook",
            "CreateLabel",
            &[PyRep::String("Fleet Allies".into()), PyRep::Integer(1)],
        )
        .await;
    }

    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx = Arc::new(ServerContext::with_repository(repo2));
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-characters.read_contacts.v1".to_string(),
            "esi-characters.write_contacts.v1".to_string(),
        ],
    );

    // 1. GET /v1/characters/{character_id}/contacts/labels/
    let labels_req = EsiHttpRequest::new(
        "GET",
        format!("/v1/characters/{char_id}/contacts/labels/"),
    )
    .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &labels_req);
    let labels = validate_and_json(&router, &spec, &labels_req, 200);
    assert_eq!(
        labels,
        json!([{ "label_id": 1, "label_name": "Fleet Allies" }])
    );

    // 2. POST /v2/characters/{character_id}/contacts/ (400 without standing, 201 with standing/watched/label_ids)
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", format!("/v2/characters/{char_id}/contacts/"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!([contact_char_id])).unwrap()),
        400,
    );
    let created = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "POST",
            format!("/v2/characters/{char_id}/contacts/?standing=5.0&watched=true&label_ids=1"),
        )
        .with_bearer_token(&token)
        .with_body(serde_json::to_vec(&json!([contact_char_id])).unwrap()),
        201,
    );
    assert_eq!(created, json!([contact_char_id]));

    // 3. GET /v2/characters/{character_id}/contacts/ verifies persisted contact
    let get_req = EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/contacts/"))
        .with_bearer_token(&token);
    let get_resp = router.handle_request(&get_req);
    assert_eq!(get_resp.header("x-pages"), Some("1"));
    assert_etag_and_304(&router, &spec, &get_req);
    let contacts = validate_and_json(&router, &spec, &get_req, 200);
    assert_eq!(
        contacts,
        json!([{
            "contact_id": contact_char_id,
            "contact_type": "character",
            "is_blocked": false,
            "is_watched": true,
            "label_ids": [1],
            "standing": 5.0
        }])
    );

    // 4. PUT /v2/characters/{character_id}/contacts/ updates standing & watched
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "PUT",
            format!("/v2/characters/{char_id}/contacts/?standing=10.0&watched=false"),
        )
        .with_bearer_token(&token)
        .with_body(serde_json::to_vec(&json!([contact_char_id])).unwrap()),
        204,
    );
    let updated_contacts = validate_and_json(&router, &spec, &get_req, 200);
    assert_eq!(updated_contacts[0]["standing"], json!(10.0));
    assert_eq!(updated_contacts[0]["is_watched"], json!(false));

    // 5. DELETE /v2/characters/{character_id}/contacts/ removes contact
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "DELETE",
            format!("/v2/characters/{char_id}/contacts/?contact_ids={contact_char_id}"),
        )
        .with_bearer_token(&token),
        204,
    );
    let after_delete = validate_and_json(&router, &spec, &get_req, 200);
    assert_eq!(after_delete, json!([]));
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_esi4_contacts_corp_and_alliance() {
    // parity: esi:GET /corporations/{corporation_id}/contacts, esi:GET /corporations/{corporation_id}/contacts/labels
    // parity: esi:GET /alliances/{alliance_id}/contacts, esi:GET /alliances/{alliance_id}/contacts/labels
    // parity: feat:esi.contacts.corp, feat:esi.contacts.alliance
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/contacts",
        "esi:GET /corporations/{corporation_id}/contacts/labels",
        "esi:GET /alliances/{alliance_id}/contacts",
        "esi:GET /alliances/{alliance_id}/contacts/labels",
        "feat:esi.contacts.corp",
        "feat:esi.contacts.alliance",
    ]);

    let db_path = temp_sqlite_path("corp_alliance_contacts");
    let char_id = 90_000_001;
    let outsider_id = 90_000_002;
    let corp_id = 98_000_555;
    let alliance_id = 99_000_555;

    let (corp_label_id, ally_label_id) = {
        let repo1 = seed_demo_sqlite(&db_path);
        repo1.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&char_id) {
                c.corporation_id = corp_id;
                c.alliance_id = Some(alliance_id);
            }
            snap.corporations.insert(
                corp_id,
                CorporationRecord {
                    corporation_id: corp_id,
                    name: "Coalition Vanguard".to_string(),
                    ticker: "CVGD".to_string(),
                    ceo_id: char_id,
                    alliance_id: Some(alliance_id),
                    tax_rate: 0.05,
                    master_wallet: IskCents(0),
                    members: HashSet::from([char_id]),
                    member_roles: HashMap::from([(char_id, ROLE_DIRECTOR)]),
                    ..Default::default()
                },
            );
            snap.alliances.insert(
                alliance_id,
                AllianceRecord {
                    alliance_id,
                    name: "Northern Accord".to_string(),
                    short_name: "NACC".to_string(),
                    executor_corp_id: corp_id,
                    member_corp_ids: HashSet::from([corp_id]),
                },
            );
        });
        let ctx1 = ServerContext::with_repository(repo1);
        crate::common::pin_clock(&ctx1);
        let mut sess = SessionState::new(81_002);
        sess.user_id = Some(1);
        sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(corp_id);
        sess.alliance_id = Some(alliance_id);
        sess.corp_role = ROLE_DIRECTOR;

        let clbl = rpc_call(
            &ctx1,
            &mut sess,
            "corpRegistry",
            "CreateLabel",
            &[PyRep::String("Hostiles".into()), PyRep::Integer(2)],
        )
        .await
        .as_i64()
        .unwrap_or(1);
        rpc_call(
            &ctx1,
            &mut sess,
            "corpRegistry",
            "AddCorporateContact",
            &[
                PyRep::Integer(outsider_id),
                PyRep::Real(-10.0),
                PyRep::Integer(clbl),
            ],
        )
        .await;

        let albl = rpc_call(
            &ctx1,
            &mut sess,
            "allianceRegistry",
            "CreateLabel",
            &[PyRep::String("Blue Coalition".into()), PyRep::Integer(4)],
        )
        .await
        .as_i64()
        .unwrap_or(1);
        rpc_call(
            &ctx1,
            &mut sess,
            "allianceRegistry",
            "AddAllianceContact",
            &[
                PyRep::Integer(corp_id),
                PyRep::Real(10.0),
                PyRep::Integer(albl),
            ],
        )
        .await;
        (clbl, albl)
    };

    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx = Arc::new(ServerContext::with_repository(repo2));
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-corporations.read_contacts.v1".to_string(),
            "esi-alliances.read_contacts.v1".to_string(),
        ],
    );
    let outsider_token = issue_token_with_scopes(
        &ctx,
        outsider_id,
        vec!["esi-corporations.read_contacts.v1".to_string()],
    );

    // 1. Membership 403 checks
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v2/corporations/{corp_id}/contacts/"))
            .with_bearer_token(&outsider_token),
        403,
    );
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v2/alliances/{alliance_id}/contacts/"))
            .with_bearer_token(&outsider_token),
        403,
    );

    // 2. Corp contacts & labels
    let corp_contacts_req =
        EsiHttpRequest::new("GET", format!("/v2/corporations/{corp_id}/contacts/"))
            .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &corp_contacts_req);
    let corp_contacts = validate_and_json(&router, &spec, &corp_contacts_req, 200);
    assert_eq!(
        corp_contacts,
        json!([{
            "contact_id": outsider_id,
            "contact_type": "character",
            "is_watched": false,
            "label_ids": [corp_label_id],
            "standing": -10.0
        }])
    );

    let corp_labels_req = EsiHttpRequest::new(
        "GET",
        format!("/v1/corporations/{corp_id}/contacts/labels/"),
    )
    .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &corp_labels_req);
    let corp_labels = validate_and_json(&router, &spec, &corp_labels_req, 200);
    assert_eq!(
        corp_labels,
        json!([{ "label_id": corp_label_id, "label_name": "Hostiles" }])
    );

    // 3. Alliance contacts & labels
    let all_contacts_req =
        EsiHttpRequest::new("GET", format!("/v2/alliances/{alliance_id}/contacts/"))
            .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &all_contacts_req);
    let all_contacts = validate_and_json(&router, &spec, &all_contacts_req, 200);
    assert_eq!(
        all_contacts,
        json!([{
            "contact_id": corp_id,
            "contact_type": "corporation",
            "label_ids": [ally_label_id],
            "standing": 10.0
        }])
    );

    let all_labels_req = EsiHttpRequest::new(
        "GET",
        format!("/v1/alliances/{alliance_id}/contacts/labels/"),
    )
    .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &all_labels_req);
    let all_labels = validate_and_json(&router, &spec, &all_labels_req, 200);
    assert_eq!(
        all_labels,
        json!([{ "label_id": ally_label_id, "label_name": "Blue Coalition" }])
    );
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_esi4_mail_send_list_read_update_delete_labels_lists() {
    // parity: esi:PUT /characters/{character_id}/mail/{mail_id}, esi:DELETE /characters/{character_id}/mail/{mail_id}
    // parity: esi:GET /characters/{character_id}/mail/labels, esi:POST /characters/{character_id}/mail/labels
    // parity: esi:DELETE /characters/{character_id}/mail/labels/{label_id}, esi:GET /characters/{character_id}/mail/lists
    // parity: feat:esi.mail.list, feat:esi.mail.send, feat:esi.mail.read-update-delete, feat:esi.mail.labels, feat:esi.mail.lists
    eve_testkit::attest(&[
        "esi:PUT /characters/{character_id}/mail/{mail_id}",
        "esi:DELETE /characters/{character_id}/mail/{mail_id}",
        "esi:GET /characters/{character_id}/mail/labels",
        "esi:POST /characters/{character_id}/mail/labels",
        "esi:DELETE /characters/{character_id}/mail/labels/{label_id}",
        "esi:GET /characters/{character_id}/mail/lists",
        "feat:esi.mail.list",
        "feat:esi.mail.send",
        "feat:esi.mail.read-update-delete",
        "feat:esi.mail.labels",
        "feat:esi.mail.lists",
    ]);

    let db_path = temp_sqlite_path("mail_lists");
    let char_id = 90_000_001;
    let recipient_id = 90_000_002;

    let repo1 = seed_demo_sqlite(&db_path);
    let ctx1 = Arc::new(ServerContext::with_repository(repo1));
    crate::common::pin_clock(&ctx1);
    let router1 = EsiRouter::new(Arc::clone(&ctx1));
    let spec = OpenApiSpec::pinned();

    let token1 = issue_token_with_scopes(
        &ctx1,
        char_id,
        vec![
            "esi-mail.read_mail.v1".to_string(),
            "esi-mail.send_mail.v1".to_string(),
            "esi-mail.organize_mail.v1".to_string(),
        ],
    );
    let recipient_token = issue_token_with_scopes(
        &ctx1,
        recipient_id,
        vec![
            "esi-mail.read_mail.v1".to_string(),
            "esi-mail.organize_mail.v1".to_string(),
        ],
    );

    // 1. POST /v1/characters/{character_id}/mail/ enforces CSPA charge & persists timestamp
    let initial_bal = ctx1.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = 98_000_901;
        }
        snap.character_cspa_charges
            .insert(recipient_id, IskCents::from_isk(2950));
        snap.characters.get(&char_id).unwrap().balance.to_wire_f64()
    });
    let unapproved_body = json!({
        "approved_cost": 0,
        "subject": "Capital Move Op",
        "body": "Cyno beacon online at 21:00 EVE.",
        "recipients": [{ "recipient_id": recipient_id, "recipient_type": "character" }]
    });
    validate_and_json(
        &router1,
        &spec,
        &EsiHttpRequest::new("POST", format!("/v1/characters/{char_id}/mail/"))
            .with_bearer_token(&token1)
            .with_body(serde_json::to_vec(&unapproved_body).unwrap()),
        400,
    );
    let send_body = json!({
        "approved_cost": 2950,
        "subject": "Capital Move Op",
        "body": "Cyno beacon online at 21:00 EVE.",
        "recipients": [{ "recipient_id": recipient_id, "recipient_type": "character" }]
    });
    let mail_id_val = validate_and_json(
        &router1,
        &spec,
        &EsiHttpRequest::new("POST", format!("/v1/characters/{char_id}/mail/"))
            .with_bearer_token(&token1)
            .with_body(serde_json::to_vec(&send_body).unwrap()),
        201,
    );
    let mail_id = mail_id_val.as_i64().expect("created mail_id");
    let after_bal = ctx1
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).unwrap().balance.to_wire_f64());
    assert_eq!(after_bal, initial_bal - 2950.0);

    // 2. GET /v1/characters/{recipient_id}/mail/ & GET /v1/characters/{recipient_id}/mail/{mail_id}/
    let list_req = EsiHttpRequest::new("GET", format!("/v1/characters/{recipient_id}/mail/"))
        .with_bearer_token(&recipient_token);
    assert_etag_and_304(&router1, &spec, &list_req);
    let headers = validate_and_json(&router1, &spec, &list_req, 200);
    let mail_hdr = headers
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["mail_id"] == json!(mail_id))
        .expect("mail in recipient inbox");
    assert_eq!(mail_hdr["subject"], json!("Capital Move Op"));
    assert_eq!(mail_hdr["from"], json!(char_id));
    assert_eq!(mail_hdr["is_read"], json!(false));

    let detail_req = EsiHttpRequest::new(
        "GET",
        format!("/v1/characters/{recipient_id}/mail/{mail_id}/"),
    )
    .with_bearer_token(&recipient_token);
    assert_etag_and_304(&router1, &spec, &detail_req);
    let detail = validate_and_json(&router1, &spec, &detail_req, 200);
    assert_eq!(detail["subject"], json!("Capital Move Op"));
    assert_eq!(detail["body"], json!("Cyno beacon online at 21:00 EVE."));
    assert_eq!(detail["read"], json!(false));

    // 3. Mail labels: GET /v3/, POST /v2/, DELETE /v1/ (422 on system label, 204 on custom label)
    let labels_req =
        EsiHttpRequest::new("GET", format!("/v3/characters/{recipient_id}/mail/labels/"))
            .with_bearer_token(&recipient_token);
    assert_etag_and_304(&router1, &spec, &labels_req);
    let initial_labels = validate_and_json(&router1, &spec, &labels_req, 200);
    assert!(initial_labels["labels"].as_array().unwrap().len() >= 4);

    let new_label_val = validate_and_json(
        &router1,
        &spec,
        &EsiHttpRequest::new("POST", format!("/v2/characters/{recipient_id}/mail/labels/"))
            .with_bearer_token(&recipient_token)
            .with_body(
                serde_json::to_vec(&json!({ "name": "Capitals", "color": "#fe0000" })).unwrap(),
            ),
        201,
    );
    let custom_label_id = new_label_val.as_i64().expect("created label_id");

    // 4. PUT /v1/characters/{recipient_id}/mail/{mail_id}/ updates read flag and labels
    validate_and_json(
        &router1,
        &spec,
        &EsiHttpRequest::new(
            "PUT",
            format!("/v1/characters/{recipient_id}/mail/{mail_id}/"),
        )
        .with_bearer_token(&recipient_token)
        .with_body(
            serde_json::to_vec(&json!({ "read": true, "labels": [1, custom_label_id] })).unwrap(),
        ),
        204,
    );
    let updated_detail = validate_and_json(&router1, &spec, &detail_req, 200);
    assert_eq!(updated_detail["read"], json!(true));
    assert_eq!(updated_detail["labels"], json!([1, custom_label_id]));

    let updated_headers = validate_and_json(
        &router1,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/characters/{recipient_id}/mail/?labels={custom_label_id}"),
        )
        .with_bearer_token(&recipient_token),
        200,
    );
    let updated_hdr = updated_headers
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["mail_id"] == json!(mail_id))
        .expect("mail in list after PUT with labels filter");
    assert_eq!(updated_hdr["is_read"], json!(true));
    assert_eq!(updated_hdr["labels"], json!([1, custom_label_id]));

    // Delete built-in label 1 -> 422, delete custom label -> 204
    validate_and_json(
        &router1,
        &spec,
        &EsiHttpRequest::new(
            "DELETE",
            format!("/v1/characters/{recipient_id}/mail/labels/1/"),
        )
        .with_bearer_token(&recipient_token),
        422,
    );
    validate_and_json(
        &router1,
        &spec,
        &EsiHttpRequest::new(
            "DELETE",
            format!("/v1/characters/{recipient_id}/mail/labels/{custom_label_id}/"),
        )
        .with_bearer_token(&recipient_token),
        204,
    );

    // 5. DELETE /v1/characters/{recipient_id}/mail/{mail_id}/ removes mail
    validate_and_json(
        &router1,
        &spec,
        &EsiHttpRequest::new(
            "DELETE",
            format!("/v1/characters/{recipient_id}/mail/{mail_id}/"),
        )
        .with_bearer_token(&recipient_token),
        204,
    );
    validate_and_json(&router1, &spec, &detail_req, 404);

    // 6. GET /v1/characters/{character_id}/mail/lists/ via mailingListsMgr.Create + SQLite restart
    let empty_lists = validate_and_json(
        &router1,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/mail/lists/"))
            .with_bearer_token(&token1),
        200,
    );
    assert_eq!(empty_lists, json!([]));

    let mut sess = SessionState::new(81_003);
    sess.user_id = Some(1);
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    sess.character_id = Some(char_id);
    let list_id = rpc_call(
        &ctx1,
        &mut sess,
        "mailingListsMgr",
        "Create",
        &[
            PyRep::String("Alliance Logistics Ping".into()),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .as_i64()
    .expect("mailingListsMgr.Create list_id");
    drop(router1);
    drop(ctx1);

    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = Arc::new(ServerContext::with_repository(repo2));
    crate::common::pin_clock(&ctx2);
    let router2 = EsiRouter::new(Arc::clone(&ctx2));
    let token2 = issue_token_with_scopes(&ctx2, char_id, vec!["esi-mail.read_mail.v1".to_string()]);

    let lists_req = EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/mail/lists/"))
        .with_bearer_token(&token2);
    assert_etag_and_304(&router2, &spec, &lists_req);
    let populated_lists = validate_and_json(&router2, &spec, &lists_req, 200);
    assert_eq!(
        populated_lists,
        json!([{
            "mailing_list_id": list_id,
            "name": "Alliance Logistics Ping"
        }])
    );
    let _ = std::fs::remove_file(&db_path);
}
