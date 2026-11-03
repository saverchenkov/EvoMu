//! ESI-4 integration tests (Part 2): Character public info, corp history, CSPA, roles,
//! titles, notifications, and loyalty points.

#![forbid(unsafe_code)]

use crate::character_esi_social_mail::{
    assert_etag_and_304, issue_token_with_scopes, validate_and_json,
};
use eve_db::{CorporationRecord, IskCents};
use eve_server::esi::runtime_state::{EsiContactNotification, EsiCorpHistoryEntry};
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[tokio::test]
async fn test_esi4_character_public_history_cspa_roles_titles() {
    // parity: esi:GET /characters/{character_id}, feat:esi.character.public
    // parity: esi:GET /characters/{character_id}/corporationhistory, feat:esi.character.corp-history
    // parity: esi:POST /characters/{character_id}/cspa, feat:esi.character.cspa
    // parity: esi:GET /characters/{character_id}/titles, feat:esi.character.titles
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}",
        "feat:esi.character.public",
        "esi:GET /characters/{character_id}/corporationhistory",
        "feat:esi.character.corp-history",
        "esi:POST /characters/{character_id}/cspa",
        "feat:esi.character.cspa",
        "esi:GET /characters/{character_id}/titles",
        "feat:esi.character.titles",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let char_id = 90_000_001;
    let stranger_id = 90_000_002;
    let corp_id = 98_000_888;
    let alliance_id = 99_000_888;

    ctx.db.with_mut_snapshot(|snap| {
        let role_mask = eve_db::corp_roles::ROLE_DIRECTOR
            | eve_db::corp_roles::ROLE_PERSONNEL_MANAGER
            | eve_db::corp_roles::ROLE_FACTORY_MANAGER;
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Vanguard Ind".to_string(),
                ticker: "VGND".to_string(),
                ceo_id: char_id,
                alliance_id: Some(alliance_id),
                tax_rate: 0.10,
                master_wallet: IskCents(0),
                members: HashSet::from([char_id]),
                member_roles: HashMap::from([(char_id, role_mask)]),
                ..Default::default()
            },
        );
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = corp_id;
            c.alliance_id = Some(alliance_id);
            c.race_id = 2; // Minmatar
            c.bloodline_id = 4; // Brutor
            c.gender = 0; // female
            c.security_status = 2.75;
            c.description = "Fleet Commander".to_string();
            c.title = "Wing Commander".to_string();
        }
        snap.character_cspa_charges
            .insert(stranger_id, IskCents::from_isk(2950));
    });
    ctx.esi_state.char_corp_history.write().unwrap().insert(
        char_id,
        vec![
            EsiCorpHistoryEntry {
                corporation_id: corp_id,
                is_deleted: false,
                record_id: 2002,
                start_date_unix: 1_700_000_000,
            },
            EsiCorpHistoryEntry {
                corporation_id: 1_000_125,
                is_deleted: true,
                record_id: 2001,
                start_date_unix: 1_680_000_000,
            },
        ],
    );
    ctx.esi_state.char_titles.write().unwrap().insert(
        char_id,
        vec![
            (1, "Wing Commander".to_string()),
            (2, "Capital Director".to_string()),
        ],
    );

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-characters.read_contacts.v1".to_string(),
            "esi-characters.read_fw_stats.v1".to_string(),
            "esi-characters.read_corporation_roles.v1".to_string(),
            "esi-characters.read_titles.v1".to_string(),
        ],
    );

    // 1. GET /v5/characters/{character_id}/ (404 on unknown, 200 + ETag/304 on char_id)
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v5/characters/999999999/"),
        404,
    );
    let pub_req = EsiHttpRequest::new("GET", format!("/v5/characters/{char_id}/"));
    assert_etag_and_304(&router, &spec, &pub_req);
    let pub_info = validate_and_json(&router, &spec, &pub_req, 200);
    assert_eq!(pub_info["corporation_id"], json!(corp_id));
    assert_eq!(pub_info["alliance_id"], json!(alliance_id));
    assert_eq!(pub_info["race_id"], json!(2));
    assert_eq!(pub_info["bloodline_id"], json!(4));
    assert_eq!(pub_info["gender"], json!("female"));
    assert_eq!(pub_info["security_status"], json!(2.75));
    assert_eq!(pub_info["description"], json!("Fleet Commander"));
    assert_eq!(pub_info["title"], json!("Wing Commander"));

    // 2. GET /v1/characters/{character_id}/corporationhistory/
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/999999999/corporationhistory/"),
        404,
    );
    let hist_req =
        EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/corporationhistory/"));
    assert_etag_and_304(&router, &spec, &hist_req);
    let hist = validate_and_json(&router, &spec, &hist_req, 200);
    assert_eq!(hist.as_array().unwrap().len(), 2);
    assert_eq!(hist[0]["corporation_id"], json!(corp_id));
    assert_eq!(hist[0]["record_id"], json!(2002));
    assert_eq!(hist[1]["corporation_id"], json!(1_000_125));
    assert_eq!(hist[1]["record_id"], json!(2001));
    assert_eq!(hist[1]["is_deleted"], json!(true));

    // 3. POST /v5/characters/{character_id}/cspa/
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", format!("/v5/characters/{char_id}/cspa/"))
            .with_body(serde_json::to_vec(&json!([stranger_id])).unwrap()),
        401,
    );
    let cspa_charge = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", format!("/v5/characters/{char_id}/cspa/"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!([stranger_id, char_id])).unwrap()),
        201,
    );
    assert_eq!(cspa_charge, json!(2950.0));

    // 4. GET /v2/characters/{character_id}/roles/
    let roles_req = EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/roles/"))
        .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &roles_req);
    let roles = validate_and_json(&router, &spec, &roles_req, 200);
    let role_list = roles["roles"].as_array().expect("roles array");
    assert!(role_list.contains(&json!("Director")));
    assert!(role_list.contains(&json!("Personnel_Manager")));
    assert!(role_list.contains(&json!("Factory_Manager")));

    // 5. GET /v1/characters/{character_id}/titles/
    let titles_req = EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/titles/"))
        .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &titles_req);
    let titles = validate_and_json(&router, &spec, &titles_req, 200);
    assert_eq!(
        titles,
        json!([
            { "title_id": 1, "name": "Wing Commander" },
            { "title_id": 2, "name": "Capital Director" }
        ])
    );
}

#[tokio::test]
async fn test_esi4_character_notifications_and_loyalty_points() {
    // parity: esi:GET /characters/{character_id}/notifications, feat:esi.character.notifications
    // parity: esi:GET /characters/{character_id}/notifications/contacts, feat:esi.character.contact-notifications
    // parity: esi:GET /characters/{character_id}/loyalty/points, feat:esi.loyalty.points
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/notifications",
        "feat:esi.character.notifications",
        "esi:GET /characters/{character_id}/notifications/contacts",
        "feat:esi.character.contact-notifications",
        "esi:GET /characters/{character_id}/loyalty/points",
        "feat:esi.loyalty.points",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let char_id = 90_000_001;
    let sender_char_id = 90_000_002;
    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-characters.read_notifications.v1".to_string(),
            "esi-characters.read_loyalty.v1".to_string(),
        ],
    );

    // 1. Empty state returns [] (no dummy fallback rows)
    let empty_notifs = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v5/characters/{char_id}/notifications/"))
            .with_bearer_token(&token),
        200,
    );
    assert_eq!(empty_notifs, json!([]));

    let empty_contact_notifs = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/characters/{char_id}/notifications/contacts/"),
        )
        .with_bearer_token(&token),
        200,
    );
    assert_eq!(empty_contact_notifs, json!([]));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.loyalty_points.clear();
        }
    });
    let empty_lp = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/loyalty/points/"))
            .with_bearer_token(&token),
        200,
    );
    assert_eq!(empty_lp, json!([]));

    // 2. Seed real notifications, contact notifications, and loyalty points
    let created_rec = ctx.create_character_notification(
        1_000_125,
        char_id,
        5, // AllWarDeclaredMsg
        1,
        "againstID: 99000001\ndeclaredByID: 99000002",
    );
    ctx.esi_state
        .contact_notifications
        .write()
        .unwrap()
        .insert(
            char_id,
            vec![EsiContactNotification {
                notification_id: 99_001,
                send_date_unix: 1_704_067_200,
                sender_character_id: sender_char_id,
                message: "Added you as fleet ally".to_string(),
                standing_level: 10.0,
            }],
        );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.loyalty_points.insert(1_000_125, 14_500);
            c.loyalty_points.insert(1_000_180, 3_200);
        }
    });

    let notif_req = EsiHttpRequest::new("GET", format!("/v5/characters/{char_id}/notifications/"))
        .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &notif_req);
    let notifs = validate_and_json(&router, &spec, &notif_req, 200);
    assert_eq!(notifs.as_array().unwrap().len(), 1);
    assert_eq!(notifs[0]["notification_id"], json!(created_rec.notification_id));
    assert_eq!(notifs[0]["sender_id"], json!(1_000_125));
    assert_eq!(notifs[0]["sender_type"], json!("corporation"));
    assert_eq!(notifs[0]["type"], json!("AllWarDeclaredMsg"));
    assert_eq!(
        notifs[0]["text"],
        json!("againstID: 99000001\ndeclaredByID: 99000002")
    );
    assert_eq!(notifs[0]["is_read"], json!(false));

    let cnotif_req = EsiHttpRequest::new(
        "GET",
        format!("/v1/characters/{char_id}/notifications/contacts/"),
    )
    .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &cnotif_req);
    let cnotifs = validate_and_json(&router, &spec, &cnotif_req, 200);
    assert_eq!(
        cnotifs,
        json!([{
            "message": "Added you as fleet ally",
            "notification_id": 99_001,
            "send_date": "2024-01-01T00:00:00Z",
            "sender_character_id": sender_char_id,
            "standing_level": 10.0
        }])
    );

    let lp_req = EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/loyalty/points/"))
        .with_bearer_token(&token);
    assert_etag_and_304(&router, &spec, &lp_req);
    let lp = validate_and_json(&router, &spec, &lp_req, 200);
    assert_eq!(
        lp,
        json!([
            { "corporation_id": 1_000_125, "loyalty_points": 14_500 },
            { "corporation_id": 1_000_180, "loyalty_points": 3_200 }
        ])
    );
}
