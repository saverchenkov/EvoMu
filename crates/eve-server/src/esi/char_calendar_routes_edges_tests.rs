//! Edge-case and branch-coverage unit tests for `esi::char_calendar_routes`.

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;

fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn body(res: &RouteExecutionResult) -> &Value {
    res.body.as_ref().expect("response body")
}

#[test]
fn owner_name_resolution_social_extras_merge_and_negative_date_clamp() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let npc_corp_id = *ctx.sde.npc_corporations.keys().next().unwrap_or(&1_000_002);

    ctx.db.with_mut_snapshot(|snap| {
        // Character owner
        snap.calendar_events.insert(
            7101,
            eve_db::CalendarEventRecord {
                event_id: 7101,
                owner_id: char_id,
                creator_id: char_id,
                event_date_time: -50,
                duration: 10,
                importance: 0,
                title: "Snap Title".to_string(),
                description: "Snap Desc".to_string(),
                scope: "Personal".to_string(),
                responses: HashMap::new(),
                is_deleted: false,
            },
        );
        // Corporation owner
        snap.calendar_events.insert(
            7102,
            eve_db::CalendarEventRecord {
                event_id: 7102,
                owner_id: corp_id,
                creator_id: char_id,
                event_date_time: 1_700_000_000,
                duration: 20,
                importance: 1,
                title: "Corp Event".to_string(),
                description: "Corp Desc".to_string(),
                scope: "Corporation".to_string(),
                responses: HashMap::new(),
                is_deleted: false,
            },
        );
        // NPC corp owner via SDE
        snap.calendar_events.insert(
            7103,
            eve_db::CalendarEventRecord {
                event_id: 7103,
                owner_id: npc_corp_id,
                creator_id: char_id,
                event_date_time: 1_700_000_100,
                duration: 30,
                importance: 0,
                title: "NPC Event".to_string(),
                description: "NPC Desc".to_string(),
                scope: "Corporation".to_string(),
                responses: HashMap::new(),
                is_deleted: false,
            },
        );
        // EVE System owner_id == 1 fallback
        snap.calendar_events.insert(
            7104,
            eve_db::CalendarEventRecord {
                event_id: 7104,
                owner_id: 1,
                creator_id: 0,
                event_date_time: 1_700_000_200,
                duration: 30,
                importance: 0,
                title: "System One".to_string(),
                description: "Sys".to_string(),
                scope: "Personal".to_string(),
                responses: HashMap::new(),
                is_deleted: false,
            },
        );
    });

    // Override 7101 in social_extras
    if let Ok(mut s) = ctx.social_extras.lock() {
        s.calendar_events.insert(
            7101,
            crate::services::fleet_social::fleet_social_persistence::CalendarEventState {
                event_id: 7101,
                owner_id: char_id,
                creator_id: char_id,
                event_date_time: -10,
                duration: 15,
                importance: 1,
                title: "Extras Title".to_string(),
                description: "Extras Desc".to_string(),
                _scope: "Personal".to_string(),
                responses: HashMap::new(),
                is_deleted: false,
            },
        );
    }

    ctx.db.with_snapshot(|snap| {
        let (events, _) = load_calendar_events_from_game_state(&ctx, snap);
        let e1 = &events[&7101];
        assert_eq!(e1.title, "Extras Title");
        assert_eq!(e1.event_date_unix, 0);
        assert_eq!(e1.owner_name, snap.characters[&char_id].name);

        let e2 = &events[&7102];
        assert_eq!(e2.owner_type, "corporation");
        assert_eq!(e2.owner_name, snap.corporations[&corp_id].name);

        let e3 = &events[&7103];
        assert_eq!(
            e3.owner_name,
            ctx.sde.npc_corporations[&npc_corp_id].name
        );

        let e4 = &events[&7104];
        assert_eq!(e4.owner_name, "EVE System");
    });
}

#[test]
fn can_view_event_attendees_and_route_error_paths() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let char2_id = 90_000_002_i64;
    let alliance_id = 99_000_001_i64;
    let empty_q = HashMap::new();

    // Ensure char2 is in corp_id with no officer roles, and char_id is CEO
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.ceo_id = char_id;
            c.member_roles.remove(&char2_id);
        }
        snap.calendar_events.insert(
            8201,
            eve_db::CalendarEventRecord {
                event_id: 8201,
                owner_id: 1,
                creator_id: char_id,
                event_date_time: 1_700_000_000,
                duration: 30,
                importance: 0,
                title: "CCP".to_string(),
                description: "CCP".to_string(),
                scope: "CCP".to_string(),
                responses: HashMap::new(),
                is_deleted: false,
            },
        );
        snap.calendar_events.insert(
            8202,
            eve_db::CalendarEventRecord {
                event_id: 8202,
                owner_id: corp_id,
                creator_id: char_id,
                event_date_time: 1_700_000_000,
                duration: 30,
                importance: 0,
                title: "Corp".to_string(),
                description: "Corp".to_string(),
                scope: "Corporation".to_string(),
                responses: HashMap::from([(char2_id, 4)]),
                is_deleted: false,
            },
        );
        snap.calendar_events.insert(
            8203,
            eve_db::CalendarEventRecord {
                event_id: 8203,
                owner_id: alliance_id,
                creator_id: char_id,
                event_date_time: 1_700_000_000,
                duration: 30,
                importance: 0,
                title: "Ally".to_string(),
                description: "Ally".to_string(),
                scope: "Alliance".to_string(),
                responses: HashMap::from([(char2_id, 2)]),
                is_deleted: false,
            },
        );
        snap.calendar_events.insert(
            8204,
            eve_db::CalendarEventRecord {
                event_id: 8204,
                owner_id: char_id,
                creator_id: char_id,
                event_date_time: 1_700_000_000,
                duration: 30,
                importance: 0,
                title: "Personal".to_string(),
                description: "Personal".to_string(),
                scope: "Personal".to_string(),
                responses: HashMap::from([(char2_id, 5)]),
                is_deleted: false,
            },
        );
        // Add >50 events to verify truncation to 50
        for eid in 9001..9060 {
            snap.calendar_events.insert(
                eid,
                eve_db::CalendarEventRecord {
                    event_id: eid,
                    owner_id: char_id,
                    creator_id: char_id,
                    event_date_time: 1_700_000_000 + eid as i64,
                    duration: 15,
                    importance: 0,
                    title: format!("Bulk {eid}"),
                    description: "Bulk".to_string(),
                    scope: "Personal".to_string(),
                    responses: HashMap::new(),
                    is_deleted: false,
                },
            );
        }
    });

    // Attendees permission checks:
    // CCP event -> 403 even for creator
    assert_eq!(
        dispatch_char_calendar_route(
            &ctx,
            "GET",
            "/characters/{character_id}/calendar/{event_id}/attendees",
            &params(&[("character_id", &char_id.to_string()), ("event_id", "8201")]),
            &empty_q,
            b"",
        )
        .unwrap()
        .status_code,
        403
    );
    // Corp event: char_id (creator/CEO) -> 200; char2 (regular member) -> 403
    assert_eq!(
        dispatch_char_calendar_route(
            &ctx,
            "GET",
            "/characters/{character_id}/calendar/{event_id}/attendees",
            &params(&[("character_id", &char_id.to_string()), ("event_id", "8202")]),
            &empty_q,
            b"",
        )
        .unwrap()
        .status_code,
        200
    );
    assert_eq!(
        dispatch_char_calendar_route(
            &ctx,
            "GET",
            "/characters/{character_id}/calendar/{event_id}/attendees",
            &params(&[("character_id", &char2_id.to_string()), ("event_id", "8202")]),
            &empty_q,
            b"",
        )
        .unwrap()
        .status_code,
        403
    );
    // Alliance event: char_id -> 200; char2 -> 403
    assert_eq!(
        dispatch_char_calendar_route(
            &ctx,
            "GET",
            "/characters/{character_id}/calendar/{event_id}/attendees",
            &params(&[("character_id", &char_id.to_string()), ("event_id", "8203")]),
            &empty_q,
            b"",
        )
        .unwrap()
        .status_code,
        200
    );
    assert_eq!(
        dispatch_char_calendar_route(
            &ctx,
            "GET",
            "/characters/{character_id}/calendar/{event_id}/attendees",
            &params(&[("character_id", &char2_id.to_string()), ("event_id", "8203")]),
            &empty_q,
            b"",
        )
        .unwrap()
        .status_code,
        403
    );
    // Personal event: invited non-creator attendee char2 -> 403
    assert_eq!(
        dispatch_char_calendar_route(
            &ctx,
            "GET",
            "/characters/{character_id}/calendar/{event_id}/attendees",
            &params(&[("character_id", &char2_id.to_string()), ("event_id", "8204")]),
            &empty_q,
            b"",
        )
        .unwrap()
        .status_code,
        403
    );

    // PUT accepted and declined responses
    for resp in ["accepted", "declined"] {
        let res = dispatch_char_calendar_route(
            &ctx,
            "PUT",
            "/characters/{character_id}/calendar/{event_id}",
            &params(&[("character_id", &char2_id.to_string()), ("event_id", "8204")]),
            &empty_q,
            format!(r#"{{"response":"{resp}"}}"#).as_bytes(),
        )
        .unwrap();
        assert_eq!(res.status_code, 204);
    }

    // List truncation at 50
    let list_all = dispatch_char_calendar_route(
        &ctx,
        "GET",
        "/characters/{character_id}/calendar",
        &params(&[("character_id", &char_id.to_string())]),
        &empty_q,
        b"",
    )
    .unwrap();
    assert_eq!(list_all.status_code, 200);
    assert_eq!(body(&list_all).as_array().unwrap().len(), 50);

    // Invalid path params and missing character/event 404s across all routes
    for route in [
        ("GET", "/characters/{character_id}/calendar"),
        ("GET", "/characters/{character_id}/calendar/{event_id}"),
        ("PUT", "/characters/{character_id}/calendar/{event_id}"),
        ("GET", "/characters/{character_id}/calendar/{event_id}/attendees"),
    ] {
        // Invalid character_id -> 400
        assert_eq!(
            dispatch_char_calendar_route(
                &ctx,
                route.0,
                route.1,
                &params(&[("character_id", "0"), ("event_id", "8204")]),
                &empty_q,
                br#"{"response":"accepted"}"#,
            )
            .unwrap()
            .status_code,
            400
        );
        // Unknown character -> 404
        assert_eq!(
            dispatch_char_calendar_route(
                &ctx,
                route.0,
                route.1,
                &params(&[("character_id", "99999999"), ("event_id", "8204")]),
                &empty_q,
                br#"{"response":"accepted"}"#,
            )
            .unwrap()
            .status_code,
            404
        );
    }

    for route in [
        ("GET", "/characters/{character_id}/calendar/{event_id}"),
        ("PUT", "/characters/{character_id}/calendar/{event_id}"),
        ("GET", "/characters/{character_id}/calendar/{event_id}/attendees"),
    ] {
        // Invalid event_id -> 400
        assert_eq!(
            dispatch_char_calendar_route(
                &ctx,
                route.0,
                route.1,
                &params(&[("character_id", &char_id.to_string()), ("event_id", "bad")]),
                &empty_q,
                br#"{"response":"accepted"}"#,
            )
            .unwrap()
            .status_code,
            400
        );
        // Unknown event_id -> 404
        assert_eq!(
            dispatch_char_calendar_route(
                &ctx,
                route.0,
                route.1,
                &params(&[("character_id", &char_id.to_string()), ("event_id", "7777777")]),
                &empty_q,
                br#"{"response":"accepted"}"#,
            )
            .unwrap()
            .status_code,
            404
        );
    }
}
