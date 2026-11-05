//! Comprehensive unit and mutation-killing tests for ESI character, calendar, social, mail, and runtime_state modules.

use super::full_endpoint_catalog_tests::seed_full_esi_context;
use super::runtime_state::{EsiContactEntry, EsiMailMeta, EsiRuntimeState};
use super::{EsiHttpRequest, EsiRouter};
use crate::router::NotificationRecord;
use eve_db::{
    CalendarEventRecord, CharacterMedalGraphicRecord, CharacterMedalRecord, FwCharacterStatsRecord,
    FwWindowedStat,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

#[test]
fn runtime_state_methods_cover_alloc_mail_meta_deletion_and_custom_names() {
    let rt = EsiRuntimeState::default();
    assert!(rt.char_contacts.read().unwrap().is_empty());

    let id1 = rt.alloc_id();
    let id2 = rt.alloc_id();
    assert_eq!(id1, 16);
    assert_eq!(id2, 17);

    let default_labels = EsiRuntimeState::default_mail_labels();
    assert_eq!(default_labels.len(), 4);
    assert_eq!(default_labels[0].label_id, 1);
    assert_eq!(default_labels[0].name, "Inbox");
    assert_eq!(default_labels[1].label_id, 2);
    assert_eq!(default_labels[2].label_id, 4);
    assert_eq!(default_labels[3].label_id, 8);

    // Mail deletion tracking
    assert!(!rt.is_mail_deleted(90_000_001, 500));
    rt.mark_mail_deleted(90_000_001, 500);
    assert!(rt.is_mail_deleted(90_000_001, 500));
    assert!(!rt.is_mail_deleted(90_000_002, 500));

    // Mail meta defaults (sender vs recipient) and custom overrides
    let recipient_meta = rt.get_mail_meta(90_000_001, 600, false);
    assert!(!recipient_meta.is_read);
    assert_eq!(recipient_meta.labels, vec![1]);

    let sender_meta = rt.get_mail_meta(90_000_001, 601, true);
    assert!(sender_meta.is_read);
    assert_eq!(sender_meta.labels, vec![2]);

    rt.set_mail_meta(
        90_000_001,
        600,
        EsiMailMeta {
            is_read: true,
            labels: vec![1, 4],
        },
    );
    let updated_meta = rt.get_mail_meta(90_000_001, 600, false);
    assert!(updated_meta.is_read);
    assert_eq!(updated_meta.labels, vec![1, 4]);

    // Custom item names
    assert_eq!(rt.get_custom_item_name(100_001), None);
    rt.set_custom_item_name(100_001, "SES Flagship");
    assert_eq!(
        rt.get_custom_item_name(100_001),
        Some("SES Flagship".to_string())
    );
}

#[test]
fn char_calendar_fw_medals_notifications_roles_and_titles_routes() {
    let (ctx, token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let router = EsiRouter::new(Arc::clone(&ctx));
    let char2_id = 90_000_002_i64;
    let now = ctx.now_unix();

    let ev1_id = 501_i64;
    let ev2_id = 502_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.title = "High Admiral".into();
        }
        snap.calendar_events.insert(
            ev1_id,
            CalendarEventRecord {
                event_id: ev1_id,
                owner_id: char_id,
                creator_id: char_id,
                event_date_time: 0,
                duration: 60,
                importance: 1,
                title: "Fleet Ops".into(),
                description: "Formup in Jita".into(),
                scope: "Personal".into(),
                responses: HashMap::from([(char_id, 4), (char2_id, 2)]),
                is_deleted: false,
            },
        );
        snap.calendar_events.insert(
            ev2_id,
            CalendarEventRecord {
                event_id: ev2_id,
                owner_id: corp_id,
                creator_id: char_id,
                event_date_time: 0,
                duration: 120,
                importance: 0,
                title: "Corp Townhall".into(),
                description: "Monthly review".into(),
                scope: "Corporation".into(),
                responses: HashMap::new(),
                is_deleted: false,
            },
        );
        snap.fw_character_stats.insert(
            char_id,
            FwCharacterStatsRecord {
                character_id: char_id,
                faction_id: Some(500_001),
                enlisted_on_unix: Some(now.saturating_sub(86_400)),
                current_rank: 4,
                highest_rank: 5,
                kills: FwWindowedStat {
                    yesterday: 2,
                    last_week: 10,
                    total: 50,
                },
                victory_points: FwWindowedStat {
                    yesterday: 100,
                    last_week: 500,
                    total: 2500,
                },
            },
        );
        snap.char_medals.entry(char_id).or_default().push(CharacterMedalRecord {
            medal_id: 301,
            title: "Purple Heart".into(),
            description: "Valor in combat".into(),
            corporation_id: corp_id,
            issuer_id: char2_id,
            date_unix: now.saturating_sub(3600),
            reason: "Fleet Hero".into(),
            status: "public".into(),
            graphics: vec![CharacterMedalGraphicRecord {
                part: 1,
                layer: 0,
                graphic: "caldari.1_1".into(),
                color: 123,
            }],
        });
    });

    {
        let mut notifs = ctx.notification_records.write().unwrap();
        for (nid, tid, sender_id, processed) in [
            (701, 2, char2_id, true),
            (702, 5, corp_id, false),
            (703, 10, char2_id, false),
            (704, 14, 99_000_001, true),
            (705, 16, 500_001, false),
            (706, 34, 0, false),
            (707, 35, char2_id, false),
            (708, 45, corp_id, false),
            (709, 75, corp_id, false),
            (710, 88, corp_id, false),
            (711, 999, char2_id, false),
        ] {
            notifs.push(NotificationRecord {
                notification_id: nid,
                type_id: tid,
                group_id: 1,
                sender_id,
                receiver_id: char_id,
                processed,
                created: (now.saturating_sub(60)) as i64,
                data: "msg: test".into(),
                deleted: false,
                interacted: false,
            });
        }
    }

    // Calendar list + from_event filter
    let cal_all = router
        .handle_request(
            &EsiHttpRequest::new("GET", format!("/latest/characters/{char_id}/calendar"))
                .with_bearer_token(&token),
        )
        .json()
        .unwrap();
    assert!(cal_all.as_array().unwrap().len() >= 2);

    let cal_filtered = router
        .handle_request(
            &EsiHttpRequest::new(
                "GET",
                format!("/latest/characters/{char_id}/calendar?from_event={ev1_id}"),
            )
            .with_bearer_token(&token),
        )
        .json()
        .unwrap();
    assert!(cal_filtered
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["event_id"] == json!(ev2_id)));

    // Calendar detail (character owner & corporation owner & 404)
    let ev1_detail = router
        .handle_request(
            &EsiHttpRequest::new(
                "GET",
                format!("/latest/characters/{char_id}/calendar/{ev1_id}"),
            )
            .with_bearer_token(&token),
        )
        .json()
        .unwrap();
    assert_eq!(ev1_detail["owner_type"], json!("character"));
    assert_eq!(ev1_detail["response"], json!("accepted"));

    let ev2_detail = router
        .handle_request(
            &EsiHttpRequest::new(
                "GET",
                format!("/latest/characters/{char_id}/calendar/{ev2_id}"),
            )
            .with_bearer_token(&token),
        )
        .json()
        .unwrap();
    assert_eq!(ev2_detail["owner_type"], json!("corporation"));

    let ev_404 = router.handle_request(
        &EsiHttpRequest::new("GET", format!("/latest/characters/{char_id}/calendar/999999"))
            .with_bearer_token(&token),
    );
    assert_eq!(ev_404.status_code, 404);

    // Calendar PUT response validation and update
    for bad_body in [
        b"invalid".to_vec(),
        serde_json::to_vec(&json!({})).unwrap(),
        serde_json::to_vec(&json!({"response": "not_responded"})).unwrap(),
    ] {
        let resp = router.handle_request(
            &EsiHttpRequest::new(
                "PUT",
                format!("/latest/characters/{char_id}/calendar/{ev1_id}"),
            )
            .with_bearer_token(&token)
            .with_body(bad_body),
        );
        assert_eq!(resp.status_code, 400);
    }

    let put_404 = router.handle_request(
        &EsiHttpRequest::new("PUT", format!("/latest/characters/{char_id}/calendar/999999"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!({"response": "tentative"})).unwrap()),
    );
    assert_eq!(put_404.status_code, 404);

    let put_ok = router.handle_request(
        &EsiHttpRequest::new(
            "PUT",
            format!("/latest/characters/{char_id}/calendar/{ev1_id}"),
        )
        .with_bearer_token(&token)
        .with_body(serde_json::to_vec(&json!({"response": "tentative"})).unwrap()),
    );
    assert_eq!(put_ok.status_code, 204);

    // Attendees GET
    let att_404 = router.handle_request(
        &EsiHttpRequest::new(
            "GET",
            format!("/latest/characters/{char_id}/calendar/999999/attendees"),
        )
        .with_bearer_token(&token),
    );
    assert_eq!(att_404.status_code, 404);

    let att = router
        .handle_request(
            &EsiHttpRequest::new(
                "GET",
                format!("/latest/characters/{char_id}/calendar/{ev1_id}/attendees"),
            )
            .with_bearer_token(&token),
        )
        .json()
        .unwrap();
    assert_eq!(att.as_array().unwrap().len(), 2);

    // FW stats, medals, notifications, contact notifications, roles, titles
    let fw = router
        .handle_request(
            &EsiHttpRequest::new("GET", format!("/latest/characters/{char_id}/fw/stats"))
                .with_bearer_token(&token),
        )
        .json()
        .unwrap();
    assert_eq!(fw["faction_id"], json!(500_001));
    assert_eq!(fw["current_rank"], json!(4));

    let medals = router
        .handle_request(
            &EsiHttpRequest::new("GET", format!("/latest/characters/{char_id}/medals"))
                .with_bearer_token(&token),
        )
        .json()
        .unwrap();
    assert_eq!(medals[0]["medal_id"], json!(301));

    let notifs = router
        .handle_request(
            &EsiHttpRequest::new("GET", format!("/latest/characters/{char_id}/notifications"))
                .with_bearer_token(&token),
        )
        .json()
        .unwrap();
    assert!(notifs.as_array().unwrap().len() >= 11);

    let contact_notifs = router
        .handle_request(
            &EsiHttpRequest::new(
                "GET",
                format!("/latest/characters/{char_id}/notifications/contacts"),
            )
            .with_bearer_token(&token),
        )
        .json()
        .unwrap();
    assert!(contact_notifs.as_array().is_some());

    let roles = router
        .handle_request(
            &EsiHttpRequest::new("GET", format!("/latest/characters/{char_id}/roles"))
                .with_bearer_token(&token),
        )
        .json()
        .unwrap();
    assert!(!roles["roles"].as_array().unwrap().is_empty());

    let titles = router
        .handle_request(
            &EsiHttpRequest::new("GET", format!("/latest/characters/{char_id}/titles"))
                .with_bearer_token(&token),
        )
        .json()
        .unwrap();
    assert_eq!(titles[0]["name"], json!("High Admiral"));
}

#[test]
fn char_social_mail_and_character_routes_error_and_happy_paths() {
    let (ctx, token, char_id, _corp_id, planet_id) = seed_full_esi_context();
    let router = EsiRouter::new(Arc::clone(&ctx));
    let char2_id = 90_000_002_i64;

    // CSPA error paths and positive contact waiver
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch2) = snap.characters.get_mut(&char2_id) {
            ch2.corporation_id = 1_000_126; // distinct corp so CSPA applies unless contact-exempt
        }
        snap.character_cspa_charges
            .insert(char2_id, eve_db::IskCents::from_isk(2950));
    });
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("POST", format!("/latest/characters/{char_id}/cspa"))
                    .with_bearer_token(&token)
                    .with_body(b"[]".to_vec()),
            )
            .status_code,
        400
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("POST", "/latest/characters/999888777/cspa")
                    .with_bearer_token(&token)
                    .with_body(serde_json::to_vec(&json!([char2_id])).unwrap()),
            )
            .status_code,
        403
    );
    let cspa_default = router
        .handle_request(
            &EsiHttpRequest::new("POST", format!("/latest/characters/{char_id}/cspa"))
                .with_bearer_token(&token)
                .with_body(serde_json::to_vec(&json!([char_id, char2_id])).unwrap()),
        )
        .json()
        .unwrap();
    assert_eq!(cspa_default, json!(2950.0));

    // Add char_id as positive contact on char2_id in esi_state -> CSPA waived to 0.0
    ctx.esi_state
        .char_contacts
        .write()
        .unwrap()
        .entry(char2_id)
        .or_default()
        .push(EsiContactEntry {
            contact_id: char_id,
            contact_type: "character".into(),
            standing: 5.0,
            is_watched: true,
            is_blocked: false,
            label_ids: vec![],
        });
    let cspa_waived = router
        .handle_request(
            &EsiHttpRequest::new("POST", format!("/latest/characters/{char_id}/cspa"))
                .with_bearer_token(&token)
                .with_body(serde_json::to_vec(&json!([char2_id])).unwrap()),
        )
        .json()
        .unwrap();
    assert_eq!(cspa_waived, json!(0.0));

    // Mail labels validation (empty name, bad color, reserved delete)
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("POST", format!("/latest/characters/{char_id}/mail/labels"))
                    .with_bearer_token(&token)
                    .with_body(serde_json::to_vec(&json!({"name": ""})).unwrap()),
            )
            .status_code,
        400
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("POST", format!("/latest/characters/{char_id}/mail/labels"))
                    .with_bearer_token(&token)
                    .with_body(
                        serde_json::to_vec(&json!({"name": "Valid", "color": "#badcolor"})).unwrap()
                    ),
            )
            .status_code,
        400
    );
    let create_lbl = router.handle_request(
        &EsiHttpRequest::new("POST", format!("/latest/characters/{char_id}/mail/labels"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!({"name": "Allies", "color": "#ffffff"})).unwrap()),
    );
    assert_eq!(create_lbl.status_code, 201);
    let custom_lbl_id = create_lbl.json().unwrap().as_i64().unwrap();

    // Reserved label 1 cannot be deleted
    let del_sys_lbl = router.handle_request(
        &EsiHttpRequest::new(
            "DELETE",
            format!("/latest/characters/{char_id}/mail/labels/1"),
        )
        .with_bearer_token(&token),
    );
    assert!(matches!(del_sys_lbl.status_code, 400 | 422));

    let del_custom_lbl = router.handle_request(
        &EsiHttpRequest::new(
            "DELETE",
            format!("/latest/characters/{char_id}/mail/labels/{custom_lbl_id}"),
        )
        .with_bearer_token(&token),
    );
    assert_eq!(del_custom_lbl.status_code, 204);

    // Send mail -> GET/PUT/DELETE mail/{mail_id}
    let send_mail = router.handle_request(
        &EsiHttpRequest::new("POST", format!("/latest/characters/{char_id}/mail"))
            .with_bearer_token(&token)
            .with_body(
                serde_json::to_vec(&json!({
                    "subject": "Op Notice",
                    "body": "Form up at 20:00",
                    "approved_cost": 10000,
                    "recipients": [{"recipient_id": char2_id, "recipient_type": "character"}]
                }))
                .unwrap(),
            ),
    );
    assert_eq!(send_mail.status_code, 201);
    let mail_id = send_mail.json().unwrap().as_i64().unwrap();

    let mail_detail = router
        .handle_request(
            &EsiHttpRequest::new("GET", format!("/latest/characters/{char_id}/mail/{mail_id}"))
                .with_bearer_token(&token),
        )
        .json()
        .unwrap();
    assert_eq!(mail_detail["subject"], json!("Op Notice"));

    let put_mail = router.handle_request(
        &EsiHttpRequest::new("PUT", format!("/latest/characters/{char_id}/mail/{mail_id}"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!({"read": true, "labels": [1, 2]})).unwrap()),
    );
    assert_eq!(put_mail.status_code, 204);

    let del_mail = router.handle_request(
        &EsiHttpRequest::new(
            "DELETE",
            format!("/latest/characters/{char_id}/mail/{mail_id}"),
        )
        .with_bearer_token(&token),
    );
    assert_eq!(del_mail.status_code, 204);

    // Character search validation and strict vs non-strict
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new(
                    "GET",
                    format!("/latest/characters/{char_id}/search?categories=character&search=")
                )
                .with_bearer_token(&token),
            )
            .status_code,
        400
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new(
                    "GET",
                    format!("/latest/characters/{char_id}/search?categories=bad_cat&search=Jita")
                )
                .with_bearer_token(&token),
            )
            .status_code,
        400
    );
    let search_ok = router
        .handle_request(
            &EsiHttpRequest::new(
                "GET",
                format!(
                    "/latest/characters/{char_id}/search?categories=solar_system,inventory_type,character,corporation,alliance&search=Jita&strict=false"
                ),
            )
            .with_bearer_token(&token),
        )
        .json()
        .unwrap();
    assert!(search_ok["solar_system"]
        .as_array()
        .unwrap()
        .contains(&json!(30_000_142)));

    // Planets detail 200 and 404
    let planet_ok = router.handle_request(
        &EsiHttpRequest::new(
            "GET",
            format!("/latest/characters/{char_id}/planets/{planet_id}"),
        )
        .with_bearer_token(&token),
    );
    assert_eq!(planet_ok.status_code, 200);

    let planet_404 = router.handle_request(
        &EsiHttpRequest::new(
            "GET",
            format!("/latest/characters/{char_id}/planets/99999999"),
        )
        .with_bearer_token(&token),
    );
    assert_eq!(planet_404.status_code, 404);
}
