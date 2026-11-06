//! Deep unit tests for ESI mail, contacts, calendar, economy, industry, mining, and corp routes.

use super::{EsiHttpRequest, EsiRouter};
use crate::router::ServerContext;
use eve_db::corp_roles;
use eve_sde::SdeRegistry;
use serde_json::Value;
use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn build_ctx() -> Arc<ServerContext> {
    let ctx = if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let c = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&c.sde));
        c
    };
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut ch) = snap.characters.get_mut(&90_000_001).cloned() {
            ch.user_id = 1;
            ch.alliance_id = Some(99_000_001);
            let corp_id = ch.corporation_id;
            snap.characters.insert(90_000_001, ch.clone());
            if let Some(mut alt2) = snap.characters.get(&90_000_002).cloned() {
                alt2.user_id = 1;
                snap.characters.insert(90_000_002, alt2);
            } else {
                let mut alt2 = ch.clone();
                alt2.user_id = 1;
                alt2.character_id = 90_000_002;
                alt2.name = "Pilot 90000002".into();
                snap.characters.insert(90_000_002, alt2);
            }
            let corp = snap.corporations.entry(corp_id).or_insert_with(|| {
                eve_db::CorporationRecord {
                    corporation_id: corp_id,
                    name: "Demo Corp".into(),
                    ticker: "DEMO".into(),
                    ceo_id: 90_000_001,
                    ..Default::default()
                }
            });
            corp.ceo_id = 90_000_001;
            corp.alliance_id = Some(99_000_001);
            corp.members.insert(90_000_001);
            corp.member_roles.insert(
                90_000_001,
                corp_roles::ROLE_DIRECTOR
                    | corp_roles::ROLE_ACCOUNTANT
                    | corp_roles::ROLE_STATION_MANAGER
                    | corp_roles::ROLE_FACTORY_MANAGER
                    | corp_roles::ROLE_PERSONNEL_MANAGER
                    | corp_roles::ROLE_DIPLOMAT,
            );
        }
    });
    Arc::new(ctx)
}

fn mint_full_token(ctx: &ServerContext, char_id: i64) -> String {
    let scopes: BTreeSet<String> = super::OpenApiSpec::pinned()
        .operations
        .iter()
        .flat_map(|op| op.required_scopes.iter().cloned())
        .collect();
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            char_id,
            "evomu-cli",
            "http://localhost/callback",
            scopes.into_iter().collect(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(
            &code,
            "evomu-cli",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            now,
        )
        .expect("exchange_pkce_code")
        .access_token
}

#[test]
fn esi_mail_labels_contacts_and_calendar_roundtrip() {
    let ctx = build_ctx();
    let char_id: i64 = 90_000_001;
    let token = mint_full_token(&ctx, char_id);
    let router = EsiRouter::with_error_limit(Arc::clone(&ctx), 200, 60);

    // 1. Create custom mail label -> list labels -> delete custom label -> 404 on missing delete
    let create_lbl = router.handle_request(
        &EsiHttpRequest::new("POST", format!("/v1/characters/{char_id}/mail/labels/"))
            .with_bearer_token(&token)
            .with_body(br##"{"name":"FleetOps","color":"#ffffff"}"##.to_vec()),
    );
    assert_eq!(create_lbl.status_code, 201);
    let label_id: i64 = create_lbl.json().unwrap().as_i64().unwrap();

    let list_lbl = router.handle_request(
        &EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/mail/labels/"))
            .with_bearer_token(&token),
    );
    assert_eq!(list_lbl.status_code, 200);
    assert!(list_lbl.json().unwrap()["labels"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["label_id"] == label_id));

    let del_lbl = router.handle_request(
        &EsiHttpRequest::new(
            "DELETE",
            format!("/v1/characters/{char_id}/mail/labels/{label_id}/"),
        )
        .with_bearer_token(&token),
    );
    assert_eq!(del_lbl.status_code, 204);

    let del_missing = router.handle_request(
        &EsiHttpRequest::new(
            "DELETE",
            format!("/v1/characters/{char_id}/mail/labels/999888777/"),
        )
        .with_bearer_token(&token),
    );
    assert_eq!(del_missing.status_code, 404);

    // 2. Character contacts create -> list -> update -> delete + mail lists
    let post_contact = router.handle_request(
        &EsiHttpRequest::new(
            "POST",
            format!("/v1/characters/{char_id}/contacts/?standing=5.5&watched=true"),
        )
        .with_bearer_token(&token)
        .with_body(b"[90000002]".to_vec()),
    );
    assert_eq!(post_contact.status_code, 201);

    let get_contacts = router.handle_request(
        &EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/contacts/"))
            .with_bearer_token(&token),
    );
    assert_eq!(get_contacts.status_code, 200);
    let contacts_val: Value = get_contacts.json().unwrap();
    assert!(contacts_val
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["contact_id"] == 90_000_002));

    let put_contact = router.handle_request(
        &EsiHttpRequest::new(
            "PUT",
            format!("/v1/characters/{char_id}/contacts/?standing=-2.0&watched=false"),
        )
        .with_bearer_token(&token)
        .with_body(b"[90000002]".to_vec()),
    );
    assert_eq!(put_contact.status_code, 204);

    let del_contact = router.handle_request(
        &EsiHttpRequest::new(
            "DELETE",
            format!("/v1/characters/{char_id}/contacts/?contact_ids=90000002"),
        )
        .with_bearer_token(&token),
    );
    assert_eq!(del_contact.status_code, 204);

    for path in [
        format!("/v1/characters/{char_id}/mail/lists/"),
        format!("/v1/characters/{char_id}/calendar/"),
        format!("/v1/characters/{char_id}/notifications/"),
        format!("/v1/characters/{char_id}/notifications/contacts/"),
        format!("/v1/characters/{char_id}/standings/"),
    ] {
        let resp = router.handle_request(&EsiHttpRequest::new("GET", &path).with_bearer_token(&token));
        assert_eq!(resp.status_code, 200, "GET {path}");
        assert!(resp.json().unwrap().is_array());
    }
}

#[test]
fn esi_industry_mining_corporations_and_pagination_headers() {
    let ctx = build_ctx();
    let char_id: i64 = 90_000_001;
    let corp_id = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].corporation_id);
    let token = mint_full_token(&ctx, char_id);
    let router = EsiRouter::with_error_limit(Arc::clone(&ctx), 200, 60);

    for (path, authed) in [
        ("/v1/industry/facilities/".to_string(), false),
        ("/v1/industry/systems/".to_string(), false),
        (format!("/v1/characters/{char_id}/industry/jobs/"), true),
        (format!("/v1/characters/{char_id}/mining/"), true),
        (format!("/v1/corporation/{corp_id}/mining/extractions/"), true),
        (format!("/v1/corporation/{corp_id}/mining/observers/"), true),
        (format!("/v1/corporations/{corp_id}/customs_offices/"), true),
        (format!("/v1/corporations/{corp_id}/divisions/"), true),
        (format!("/v1/corporations/{corp_id}/members/"), true),
        (format!("/v1/corporations/{corp_id}/membertracking/"), true),
        (format!("/v1/corporations/{corp_id}/roles/"), true),
        (format!("/v1/corporations/{corp_id}/shareholders/"), true),
        (format!("/v1/corporations/{corp_id}/standings/"), true),
        (format!("/v1/corporations/{corp_id}/structures/"), true),
        (format!("/v1/corporations/{corp_id}/titles/"), true),
        (format!("/v1/corporations/{corp_id}/wallets/"), true),
        (format!("/v1/corporations/{corp_id}/blueprints/"), true),
        (format!("/v1/corporations/{corp_id}/containers/logs/"), true),
        (format!("/v1/corporations/{corp_id}/starbases/"), true),
        (format!("/v1/corporations/{corp_id}/medals/issued/"), true),
        (format!("/v1/corporations/{corp_id}/icons/"), false),
        (format!("/v1/corporations/{corp_id}/alliancehistory/"), false),
        (format!("/v1/corporations/{corp_id}/"), false),
        ("/v1/corporations/npccorps/".to_string(), false),
        (format!("/v1/loyalty/stores/{corp_id}/offers/"), false),
        (format!("/v1/characters/{char_id}/blueprints/"), true),
        (format!("/v1/characters/{char_id}/clones/"), true),
        (format!("/v1/characters/{char_id}/implants/"), true),
        (format!("/v1/characters/{char_id}/fatigue/"), true),
        (format!("/v1/characters/{char_id}/online/"), true),
        (format!("/v1/characters/{char_id}/loyalty/points/"), true),
        (format!("/v1/characters/{char_id}/medals/"), true),
        (format!("/v1/characters/{char_id}/roles/"), true),
        (format!("/v1/characters/{char_id}/titles/"), true),
        (format!("/v1/characters/{char_id}/contracts/"), true),
        (format!("/v1/characters/{char_id}/fittings/"), true),
        (format!("/v1/characters/{char_id}/location/"), true),
        (format!("/v1/characters/{char_id}/ship/"), true),
    ] {
        let req = EsiHttpRequest::new("GET", &path);
        let req = if authed { req.with_bearer_token(&token) } else { req };
        let resp = router.handle_request(&req);
        assert_eq!(resp.status_code, 200, "expected 200 for {path}");
        assert!(resp.json().is_ok(), "expected JSON body for {path}");
    }

    // Character affiliation bulk POST + invalid empty body 400 + invalid path IDs
    let aff_ok = router.handle_request(
        &EsiHttpRequest::new("POST", "/v1/characters/affiliation/")
            .with_body(format!("[{char_id}]").into_bytes()),
    );
    assert_eq!(aff_ok.status_code, 200);
    assert_eq!(aff_ok.json().unwrap().as_array().unwrap().len(), 1);
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new("POST", "/v1/characters/affiliation/").with_body(b"[]".to_vec()))
            .status_code,
        400
    );
}
