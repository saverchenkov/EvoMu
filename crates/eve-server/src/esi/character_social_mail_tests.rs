//! Unit tests for ESI character, assets, fittings, mail, contacts, bookmarks, and calendar endpoints.

use super::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use crate::router::ServerContext;
use eve_db::{
    BookmarkFolderRecord, BookmarkRecord, CalendarEventRecord,
    CharacterMedalGraphicRecord, CharacterMedalRecord, CharacterResearchAgentRecord,
    CorpProjectRecord, IskCents, StandingTransactionRecord,
};
use eve_sde::SdeRegistry;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn seed_char_context() -> (Arc<ServerContext>, String, i64, i64) {
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

    let char_id = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let event_id = 77_001_i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(99_000_001);
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.ceo_id = char_id;
            corp.members.insert(char_id);
            corp.member_roles.insert(char_id, !0_u64);
        }
        snap.standing_transactions.insert(
            char_id,
            vec![StandingTransactionRecord {
                event_id: 1,
                from_id: 1_000_035,
                to_id: char_id,
                modification: 4.5,
                event_type_id: 1,
            }],
        );
        snap.char_research_agents.insert(
            char_id,
            vec![CharacterResearchAgentRecord {
                agent_id: 3_008_416,
                skill_type_id: 11445,
                started_at_unix: now,
                points_per_day: 25.0,
                remainder_points: 100.0,
            }],
        );
        snap.char_medals.insert(
            char_id,
            vec![CharacterMedalRecord {
                medal_id: 101,
                title: "Service Star".into(),
                description: "Awarded".into(),
                corporation_id: corp_id,
                issuer_id: char_id,
                date_unix: now,
                reason: "Valor".into(),
                status: "public".into(),
                graphics: vec![CharacterMedalGraphicRecord {
                    part: 1,
                    layer: 0,
                    graphic: "caldari_medal_1".into(),
                    color: 1,
                }],
            }],
        );
        snap.bookmark_folders.insert(
            501,
            BookmarkFolderRecord {
                folder_id: 501,
                owner_id: char_id,
                folder_name: "Safe Spots".into(),
                is_shared: false,
                corporation_id: None,
                description: "Tactical bookmarks".into(),
                admin_group_id: None,
                manage_group_id: None,
                use_group_id: None,
                view_group_id: None,
            },
        );
        snap.bookmarks.insert(
            601,
            BookmarkRecord {
                bookmark_id: 601,
                owner_id: char_id,
                label: "Jita Safe".into(),
                solar_system_id: 30_000_142,
                x: 1_000.0,
                y: 2_000.0,
                z: 3_000.0,
                item_id: None,
                memo: "Tactical".into(),
                folder_id: Some(501),
                is_shared: false,
            },
        );
        snap.corp_projects.insert(
            801,
            CorpProjectRecord {
                project_id: 801,
                corporation_id: corp_id,
                name: "Mine Veldspar".into(),
                target_quantity: 1_000,
                current_quantity: 250,
                isk_per_unit: IskCents::from_isk(100),
                escrow_remaining: IskCents::from_isk(75_000),
            },
        );
        snap.calendar_events.insert(
            event_id,
            CalendarEventRecord {
                event_id,
                owner_id: char_id,
                creator_id: char_id,
                event_date_time: 0,
                duration: 60,
                importance: 1,
                title: "Fleet Formup".into(),
                description: "Undock in Rifter".into(),
                scope: "character".into(),
                responses: Default::default(),
                is_deleted: false,
            },
        );
    });

    let scopes: BTreeSet<String> = OpenApiSpec::pinned()
        .operations
        .iter()
        .flat_map(|op| op.required_scopes.iter().cloned())
        .collect();
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
        .unwrap();
    let token = ctx
        .db
        .exchange_pkce_code(
            &code,
            "evomu-cli",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            now,
        )
        .unwrap()
        .access_token;

    (Arc::new(ctx), token, char_id, corp_id)
}

fn call_json(router: &EsiRouter, method: &str, path: &str, token: &str, body: Option<Value>) -> (u16, Value) {
    let mut req = EsiHttpRequest::new(method, path).with_bearer_token(token);
    if let Some(b) = body {
        req = req
            .with_header("Content-Type", "application/json")
            .with_body(serde_json::to_vec(&b).unwrap());
    }
    let resp = router.handle_request(&req);
    let val = if resp.body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&resp.body).unwrap_or(Value::Null)
    };
    (resp.status_code, val)
}

#[test]
fn esi_character_profile_assets_and_fittings_crud() {
    let (ctx, token, cid, corpid) = seed_char_context();
    let router = EsiRouter::new(Arc::clone(&ctx));

    let (st, info) = call_json(&router, "GET", &format!("/latest/characters/{cid}/"), &token, None);
    assert_eq!(st, 200);
    assert_eq!(info["corporation_id"], json!(corpid));

    let (st, aff) = call_json(
        &router,
        "POST",
        "/latest/characters/affiliation/",
        &token,
        Some(json!([cid, 90_000_002])),
    );
    assert_eq!(st, 200);
    assert_eq!(aff.as_array().unwrap().len(), 2);

    for suffix in [
        "corporationhistory",
        "portrait",
        "attributes",
        "blueprints",
        "clones",
        "fatigue",
        "implants",
        "location",
        "online",
        "ship",
        "skillqueue",
        "skills",
        "standings",
        "roles",
        "titles",
        "agents_research",
        "medals",
        "opportunities",
        "notifications",
        "notifications/contacts",
        "bookmarks",
        "bookmarks/folders",
    ] {
        let (code, _) = call_json(
            &router,
            "GET",
            &format!("/latest/characters/{cid}/{suffix}/"),
            &token,
            None,
        );
        assert_eq!(code, 200, "GET /characters/{cid}/{suffix}/");
    }

    // Assets list + locations + names (character & corporation)
    let (st, assets) = call_json(&router, "GET", &format!("/latest/characters/{cid}/assets/"), &token, None);
    assert_eq!(st, 200);
    let item_id = assets.as_array().unwrap()[0]["item_id"].as_i64().unwrap();
    let (st_loc, _) = call_json(
        &router,
        "POST",
        &format!("/latest/characters/{cid}/assets/locations/"),
        &token,
        Some(json!([item_id])),
    );
    assert_eq!(st_loc, 200);
    let (st_nam, _) = call_json(
        &router,
        "POST",
        &format!("/latest/characters/{cid}/assets/names/"),
        &token,
        Some(json!([item_id])),
    );
    assert_eq!(st_nam, 200);
    assert_eq!(
        call_json(&router, "GET", &format!("/latest/corporations/{corpid}/assets/"), &token, None).0,
        200
    );

    // Fittings create -> read -> delete
    let (st_fit, fit_created) = call_json(
        &router,
        "POST",
        &format!("/latest/characters/{cid}/fittings/"),
        &token,
        Some(json!({
            "name": "Combat Rifter",
            "description": "PvP fit",
            "ship_type_id": 587,
            "items": [{"type_id": 484, "flag": "HiSlot0", "quantity": 1}]
        })),
    );
    assert_eq!(st_fit, 201);
    let fid = fit_created["fitting_id"].as_i64().unwrap();
    let (st_list, fits) = call_json(&router, "GET", &format!("/latest/characters/{cid}/fittings/"), &token, None);
    assert_eq!(st_list, 200);
    assert!(fits.as_array().unwrap().iter().any(|f| f["fitting_id"] == json!(fid)));
    assert_eq!(
        call_json(
            &router,
            "DELETE",
            &format!("/latest/characters/{cid}/fittings/{fid}/"),
            &token,
            None,
        )
        .0,
        204
    );
}

#[test]
fn esi_mail_labels_contacts_calendar_and_opportunities_crud() {
    let (ctx, token, cid, _) = seed_char_context();
    let router = EsiRouter::new(Arc::clone(&ctx));

    // Mail labels & sending / reading / updating / deleting mail
    assert_eq!(
        call_json(&router, "GET", &format!("/latest/characters/{cid}/mail/lists/"), &token, None).0,
        200
    );
    let (st_lbl, lbl_val) = call_json(
        &router,
        "POST",
        &format!("/latest/characters/{cid}/mail/labels/"),
        &token,
        Some(json!({"name": "FleetOps", "color": "#00ff33"})),
    );
    assert_eq!(st_lbl, 201);
    let lid = lbl_val.as_i64().unwrap();
    assert_eq!(
        call_json(&router, "GET", &format!("/latest/characters/{cid}/mail/labels/"), &token, None).0,
        200
    );

    let (st_send, mail_id_val) = call_json(
        &router,
        "POST",
        &format!("/latest/characters/{cid}/mail/"),
        &token,
        Some(json!({
            "approved_cost": 10000,
            "subject": "Test Op",
            "body": "Move to Jita",
            "recipients": [{"recipient_id": 90_000_002, "recipient_type": "character"}]
        })),
    );
    assert_eq!(st_send, 201);
    let mid = mail_id_val.as_i64().unwrap();
    assert_eq!(
        call_json(&router, "GET", &format!("/latest/characters/{cid}/mail/"), &token, None).0,
        200
    );
    assert_eq!(
        call_json(&router, "GET", &format!("/latest/characters/{cid}/mail/{mid}/"), &token, None).0,
        200
    );
    assert_eq!(
        call_json(
            &router,
            "PUT",
            &format!("/latest/characters/{cid}/mail/{mid}/"),
            &token,
            Some(json!({"read": true, "labels": [lid]})),
        )
        .0,
        204
    );
    assert_eq!(
        call_json(&router, "DELETE", &format!("/latest/characters/{cid}/mail/{mid}/"), &token, None).0,
        204
    );
    assert_eq!(
        call_json(
            &router,
            "DELETE",
            &format!("/latest/characters/{cid}/mail/labels/{lid}/"),
            &token,
            None,
        )
        .0,
        204
    );

    // CSPA cost calculation & Contacts CRUD
    let (st_cspa, _) = call_json(
        &router,
        "POST",
        &format!("/latest/characters/{cid}/cspa/"),
        &token,
        Some(json!([90_000_002])),
    );
    assert_eq!(st_cspa, 201);

    let (st_add_c, _) = call_json(
        &router,
        "POST",
        &format!("/latest/characters/{cid}/contacts/?standing=5.0"),
        &token,
        Some(json!([90_000_002])),
    );
    assert_eq!(st_add_c, 201);
    assert_eq!(
        call_json(
            &router,
            "PUT",
            &format!("/latest/characters/{cid}/contacts/?standing=10.0"),
            &token,
            Some(json!([90_000_002])),
        )
        .0,
        204
    );
    assert_eq!(
        call_json(&router, "GET", &format!("/latest/characters/{cid}/contacts/"), &token, None).0,
        200
    );
    assert_eq!(
        call_json(
            &router,
            "GET",
            &format!("/latest/characters/{cid}/contacts/labels/"),
            &token,
            None
        )
        .0,
        200
    );
    assert_eq!(
        call_json(
            &router,
            "DELETE",
            &format!("/latest/characters/{cid}/contacts/?contact_ids=90000002"),
            &token,
            None,
        )
        .0,
        204
    );

    // Calendar & Opportunities
    assert_eq!(
        call_json(&router, "GET", &format!("/latest/characters/{cid}/calendar/"), &token, None).0,
        200
    );
    assert_eq!(
        call_json(
            &router,
            "GET",
            &format!("/latest/characters/{cid}/calendar/77001/"),
            &token,
            None,
        )
        .0,
        200
    );
    assert_eq!(
        call_json(
            &router,
            "PUT",
            &format!("/latest/characters/{cid}/calendar/77001/"),
            &token,
            Some(json!({"response": "accepted"})),
        )
        .0,
        204
    );
    assert_eq!(
        call_json(
            &router,
            "GET",
            &format!("/latest/characters/{cid}/calendar/77001/attendees/"),
            &token,
            None,
        )
        .0,
        200
    );
    assert_eq!(call_json(&router, "GET", "/latest/opportunities/groups/", &token, None).0, 200);
    assert_eq!(call_json(&router, "GET", "/latest/opportunities/tasks/", &token, None).0, 200);
}
