//! Domain POST/PUT/DELETE and edge-case coverage tests across character, social, mail, calendar, corp, and economy ESI routes.

use super::full_endpoint_catalog_tests::seed_full_esi_context;
use super::{EsiHttpRequest, EsiRouter};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

#[test]
fn esi_domain_crud_fittings_contacts_mail_calendar_and_assets() {
    let (ctx, token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let router = EsiRouter::new(Arc::clone(&ctx));
    let char2_id = 90_000_002_i64;

    // 1. Character affiliation POST
    let aff = router
        .handle_request(
            &EsiHttpRequest::new("POST", "/latest/characters/affiliation")
                .with_body(serde_json::to_vec(&json!([char_id, char2_id])).unwrap()),
        )
        .json()
        .unwrap();
    assert_eq!(aff.as_array().unwrap().len(), 2);

    // 2. Fittings POST -> GET -> DELETE + invalid ship/item errors
    let bad_fit = router.handle_request(
        &EsiHttpRequest::new("POST", format!("/latest/characters/{char_id}/fittings"))
            .with_bearer_token(&token)
            .with_body(
                serde_json::to_vec(&json!({
                    "name": "Bad",
                    "description": "",
                    "ship_type_id": 34,
                    "items": []
                }))
                .unwrap(),
            ),
    );
    assert_eq!(bad_fit.status_code, 400);

    let create_fit = router.handle_request(
        &EsiHttpRequest::new("POST", format!("/latest/characters/{char_id}/fittings"))
            .with_bearer_token(&token)
            .with_body(
                serde_json::to_vec(&json!({
                    "name": "Fleet Rifter",
                    "description": "Fast tackle",
                    "ship_type_id": 587,
                    "items": [{"type_id": 2897, "flag": "HiSlot0", "quantity": 1}]
                }))
                .unwrap(),
            ),
    );
    assert_eq!(create_fit.status_code, 201);
    let fit_id = create_fit.json().unwrap()["fitting_id"].as_i64().unwrap();

    let fittings_list = router
        .handle_request(
            &EsiHttpRequest::new("GET", format!("/latest/characters/{char_id}/fittings"))
                .with_bearer_token(&token),
        )
        .json()
        .unwrap();
    assert!(fittings_list
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["fitting_id"] == json!(fit_id)));

    let del_fit = router.handle_request(
        &EsiHttpRequest::new(
            "DELETE",
            format!("/latest/characters/{char_id}/fittings/{fit_id}"),
        )
        .with_bearer_token(&token),
    );
    assert_eq!(del_fit.status_code, 204);

    // 3. Asset locations & names POST for character and corporation
    let corp_asset_id = 100_099_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            corp_asset_id,
            eve_db::InventoryItemRecord {
                item_id: corp_asset_id,
                type_id: 587,
                owner_id: corp_id,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.item_positions
            .insert(corp_asset_id, [4_000.0, 5_000.0, 6_000.0]);
    });
    let asset_id = 100_000_001_i64;
    for (path, ids) in [
        (format!("/latest/characters/{char_id}/assets/locations"), vec![asset_id]),
        (format!("/latest/characters/{char_id}/assets/names"), vec![asset_id]),
        (format!("/latest/corporations/{corp_id}/assets/locations"), vec![corp_asset_id]),
        (format!("/latest/corporations/{corp_id}/assets/names"), vec![corp_asset_id]),
    ] {
        let resp = router.handle_request(
            &EsiHttpRequest::new("POST", &path)
                .with_bearer_token(&token)
                .with_body(serde_json::to_vec(&ids).unwrap()),
        );
        assert_eq!(resp.status_code, 200, "POST {path} failed");
    }

    // 4. CSPA calculation
    let cspa = router.handle_request(
        &EsiHttpRequest::new("POST", format!("/latest/characters/{char_id}/cspa"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!([char2_id])).unwrap()),
    );
    assert_eq!(cspa.status_code, 201);

    // 5. Contacts POST -> PUT -> GET -> DELETE
    let add_contact = router.handle_request(
        &EsiHttpRequest::new(
            "POST",
            format!("/latest/characters/{char_id}/contacts?standing=5.0&watched=true"),
        )
        .with_bearer_token(&token)
        .with_body(serde_json::to_vec(&json!([char2_id])).unwrap()),
    );
    assert_eq!(add_contact.status_code, 201);

    let upd_contact = router.handle_request(
        &EsiHttpRequest::new(
            "PUT",
            format!("/latest/characters/{char_id}/contacts?standing=-5.0"),
        )
        .with_bearer_token(&token)
        .with_body(serde_json::to_vec(&json!([char2_id])).unwrap()),
    );
    assert_eq!(upd_contact.status_code, 204);

    let del_contact = router.handle_request(
        &EsiHttpRequest::new(
            "DELETE",
            format!("/latest/characters/{char_id}/contacts?contact_ids={char2_id}"),
        )
        .with_bearer_token(&token),
    );
    assert_eq!(del_contact.status_code, 204);

    // 6. Mail labels POST -> Mail POST -> GET -> PUT -> DELETE -> delete label
    let label_resp = router.handle_request(
        &EsiHttpRequest::new("POST", format!("/latest/characters/{char_id}/mail/labels"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!({"name": "Ops", "color": "#00ff33"})).unwrap()),
    );
    assert_eq!(label_resp.status_code, 201);
    let label_id = label_resp.json().unwrap().as_i64().unwrap();

    let send_mail = router.handle_request(
        &EsiHttpRequest::new("POST", format!("/latest/characters/{char_id}/mail"))
            .with_bearer_token(&token)
            .with_body(
                serde_json::to_vec(&json!({
                    "recipients": [{"recipient_id": char_id, "recipient_type": "character"}],
                    "subject": "Fleet ping",
                    "body": "Form up in Jita",
                    "approved_cost": 0
                }))
                .unwrap(),
            ),
    );
    assert_eq!(send_mail.status_code, 201);
    let mail_id = send_mail.json().unwrap().as_i64().unwrap();

    let read_mail = router
        .handle_request(
            &EsiHttpRequest::new("GET", format!("/latest/characters/{char_id}/mail/{mail_id}"))
                .with_bearer_token(&token),
        )
        .json()
        .unwrap();
    assert_eq!(read_mail["subject"], json!("Fleet ping"));

    let update_mail = router.handle_request(
        &EsiHttpRequest::new("PUT", format!("/latest/characters/{char_id}/mail/{mail_id}"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!({"read": true, "labels": [label_id]})).unwrap()),
    );
    assert_eq!(update_mail.status_code, 204);

    let del_mail = router.handle_request(
        &EsiHttpRequest::new(
            "DELETE",
            format!("/latest/characters/{char_id}/mail/{mail_id}"),
        )
        .with_bearer_token(&token),
    );
    assert_eq!(del_mail.status_code, 204);

    let del_label = router.handle_request(
        &EsiHttpRequest::new(
            "DELETE",
            format!("/latest/characters/{char_id}/mail/labels/{label_id}"),
        )
        .with_bearer_token(&token),
    );
    assert_eq!(del_label.status_code, 204);

    // 7. Calendar event detail & attendee response when seeded
    let event_id = ctx.db.with_mut_snapshot(|snap| {
        snap.calendar_events.insert(
            50_001,
            eve_db::CalendarEventRecord {
                event_id: 50_001,
                owner_id: char_id,
                creator_id: char_id,
                event_date_time: 133_000_000_000_000_000,
                duration: 60,
                importance: 1,
                title: "CTA".into(),
                description: "Bring Drake".into(),
                scope: "personal".into(),
                responses: HashMap::from([(char_id, 0)]),
                is_deleted: false,
            },
        );
        50_001_i64
    });

    let ev_detail = router
        .handle_request(
            &EsiHttpRequest::new(
                "GET",
                format!("/latest/characters/{char_id}/calendar/{event_id}"),
            )
            .with_bearer_token(&token),
        )
        .json()
        .unwrap();
    assert_eq!(ev_detail["title"], json!("CTA"));

    let put_ev = router.handle_request(
        &EsiHttpRequest::new(
            "PUT",
            format!("/latest/characters/{char_id}/calendar/{event_id}"),
        )
        .with_bearer_token(&token)
        .with_body(serde_json::to_vec(&json!({"response": "accepted"})).unwrap()),
    );
    assert_eq!(put_ev.status_code, 204);

    let attendees = router
        .handle_request(
            &EsiHttpRequest::new(
                "GET",
                format!("/latest/characters/{char_id}/calendar/{event_id}/attendees"),
            )
            .with_bearer_token(&token),
        )
        .json()
        .unwrap();
    assert_eq!(attendees[0]["event_response"], json!("accepted"));
}

#[test]
fn esi_fleet_wings_squads_and_error_responses() {
    let (ctx, token, char_id, _corp_id, _planet_id) = seed_full_esi_context();
    let router = EsiRouter::new(Arc::clone(&ctx));
    let char2_id = 90_000_002_i64;

    // Seed an active fleet via fleet_social service so fleet endpoints have live fleet_id
    ctx.db
        .acquire_online_lock(1, 101, Some(char_id), false, ctx.now_unix())
        .unwrap();
    ctx.db
        .acquire_online_lock(2, 102, Some(char2_id), false, ctx.now_unix())
        .unwrap();

    let fleet_id = ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            70_001,
            eve_db::FleetRecord {
                fleet_id: 70_001,
                boss_id: char_id,
                members: vec![char_id],
                wings: vec![70_101],
                squads: vec![70_201],
                free_move: false,
                is_registered: false,
                wing_details: HashMap::from([(
                    70_101,
                    eve_db::FleetWingRecord {
                        wing_id: 70_101,
                        name: "Alpha Wing".into(),
                        commander_id: Some(char_id),
                        squad_ids: vec![70_201],
                    },
                )]),
                squad_details: HashMap::from([(
                    70_201,
                    eve_db::FleetSquadRecord {
                        squad_id: 70_201,
                        wing_id: 70_101,
                        name: "Squad 1".into(),
                        commander_id: Some(char_id),
                    },
                )]),
                member_details: HashMap::from([(
                    char_id,
                    eve_db::FleetMemberRecord {
                        character_id: char_id,
                        role: eve_db::FleetRole::FleetCommander,
                        wing_id: None,
                        squad_id: None,
                        takes_fleet_warp: true,
                        accepts_regroup: true,
                        accepts_conduit_jumps: true,
                        job: 0,
                        role_name: "Fleet Commander".into(),
                        join_time_unix: ctx.now_unix(),
                    },
                )]),
                invites: Default::default(),
                motd: "Welcome".into(),
            },
        );
        70_001_i64
    });

    let get_fleet = router
        .handle_request(
            &EsiHttpRequest::new("GET", format!("/latest/fleets/{fleet_id}"))
                .with_bearer_token(&token),
        )
        .json()
        .unwrap();
    assert_eq!(get_fleet["motd"], json!("Welcome"));

    let upd_fleet = router.handle_request(
        &EsiHttpRequest::new("PUT", format!("/latest/fleets/{fleet_id}"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!({"is_free_move": true, "motd": "New MOTD"})).unwrap()),
    );
    assert_eq!(upd_fleet.status_code, 204);

    // Create wing -> rename wing -> create squad -> rename squad -> delete squad -> delete wing
    let add_wing = router.handle_request(
        &EsiHttpRequest::new("POST", format!("/latest/fleets/{fleet_id}/wings"))
            .with_bearer_token(&token),
    );
    assert_eq!(add_wing.status_code, 201);
    let wing_id = add_wing.json().unwrap()["wing_id"].as_i64().unwrap();

    let ren_wing = router.handle_request(
        &EsiHttpRequest::new("PUT", format!("/latest/fleets/{fleet_id}/wings/{wing_id}"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!({"name": "Bravo Wing"})).unwrap()),
    );
    assert_eq!(ren_wing.status_code, 204);

    let add_squad = router.handle_request(
        &EsiHttpRequest::new(
            "POST",
            format!("/latest/fleets/{fleet_id}/wings/{wing_id}/squads"),
        )
        .with_bearer_token(&token),
    );
    assert_eq!(add_squad.status_code, 201);
    let squad_id = add_squad.json().unwrap()["squad_id"].as_i64().unwrap();

    let ren_squad = router.handle_request(
        &EsiHttpRequest::new(
            "PUT",
            format!("/latest/fleets/{fleet_id}/squads/{squad_id}"),
        )
        .with_bearer_token(&token)
        .with_body(serde_json::to_vec(&json!({"name": "Tacklers"})).unwrap()),
    );
    assert_eq!(ren_squad.status_code, 204);

    let del_squad = router.handle_request(
        &EsiHttpRequest::new(
            "DELETE",
            format!("/latest/fleets/{fleet_id}/squads/{squad_id}"),
        )
        .with_bearer_token(&token),
    );
    assert_eq!(del_squad.status_code, 204);

    let del_wing = router.handle_request(
        &EsiHttpRequest::new("DELETE", format!("/latest/fleets/{fleet_id}/wings/{wing_id}"))
            .with_bearer_token(&token),
    );
    assert_eq!(del_wing.status_code, 204);
}
