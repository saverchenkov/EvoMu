//! Unit and mutation-killing tests for `esi::char_calendar_routes`.

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
fn parse_path_i64_validates_missing_invalid_nonpositive_and_positive() {
    let empty = HashMap::new();
    let err_missing = parse_path_i64(&empty, "character_id").expect_err("missing param must fail");
    assert_eq!(err_missing.status_code, 400);
    assert!(body(&err_missing)
        .to_string()
        .contains("Missing path parameter 'character_id'"));

    let bad = params(&[("character_id", "abc")]);
    let err_bad = parse_path_i64(&bad, "character_id").expect_err("non-int param must fail");
    assert_eq!(err_bad.status_code, 400);
    assert!(body(&err_bad)
        .to_string()
        .contains("Invalid integer path parameter 'character_id': 'abc'"));

    for non_pos in ["0", "-1", "-999"] {
        let m = params(&[("character_id", non_pos)]);
        let err = parse_path_i64(&m, "character_id").expect_err("non-positive must fail");
        assert_eq!(err.status_code, 400);
        assert!(body(&err)
            .to_string()
            .contains("must be a positive integer"));
    }

    let ok_map = params(&[("character_id", "90000001")]);
    assert_eq!(parse_path_i64(&ok_map, "character_id").unwrap(), 90_000_001);
}

#[test]
fn is_character_invited_covers_all_owner_and_attendee_branches() {
    let base_ev = UnifiedCalendarEvent {
        event_id: 100,
        event_date_unix: 1_700_000_000,
        title: "Op".to_string(),
        text: "Desc".to_string(),
        duration_minutes: 60,
        importance: 1,
        owner_id: 90_000_099,
        owner_name: "Someone".to_string(),
        owner_type: "character".to_string(),
        attendees: vec![90_000_002],
    };

    assert!(is_character_invited(&base_ev, 90_000_099, 98_000_001, None));
    assert!(is_character_invited(&base_ev, 90_000_002, 98_000_001, None));
    assert!(!is_character_invited(
        &base_ev,
        90_000_001,
        98_000_001,
        Some(99_000_001)
    ));

    let corp_ev = UnifiedCalendarEvent {
        owner_id: 98_000_001,
        owner_type: "corporation".to_string(),
        attendees: vec![],
        ..base_ev
    };
    assert!(is_character_invited(&corp_ev, 90_000_001, 98_000_001, None));
    assert!(!is_character_invited(
        &corp_ev, 90_000_001, 98_000_002, None
    ));

    let ally_ev = UnifiedCalendarEvent {
        owner_id: 99_000_001,
        owner_type: "alliance".to_string(),
        attendees: vec![],
        ..corp_ev
    };
    assert!(is_character_invited(
        &ally_ev,
        90_000_001,
        98_000_001,
        Some(99_000_001)
    ));
    assert!(!is_character_invited(
        &ally_ev,
        90_000_001,
        98_000_001,
        Some(99_000_002)
    ));
    assert!(!is_character_invited(
        &ally_ev, 90_000_001, 98_000_001, None
    ));

    let ccp_ev = UnifiedCalendarEvent {
        owner_id: 1,
        owner_type: "eve_server".to_string(),
        attendees: vec![],
        ..ally_ev
    };
    assert!(is_character_invited(&ccp_ev, 90_000_001, 98_000_002, None));
}

#[test]
fn load_calendar_events_merges_social_extras_filetime_scopes_and_responses() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let win_filetime = (1_700_000_000_i64 + 11_644_473_600_i64) * 10_000_000_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.calendar_events.insert(
            7001,
            eve_db::CalendarEventRecord {
                event_id: 7001,
                owner_id: char_id,
                creator_id: char_id,
                event_date_time: 1_700_000_000,
                duration: 30,
                importance: 0,
                title: "Deleted".to_string(),
                description: "Skip me".to_string(),
                scope: "Personal".to_string(),
                responses: HashMap::new(),
                is_deleted: true,
            },
        );
        let mut resp = HashMap::new();
        resp.insert(char_id, 4);
        resp.insert(90_000_002, 2);
        resp.insert(90_000_003, 5);
        resp.insert(90_000_004, 99);
        snap.calendar_events.insert(
            7002,
            eve_db::CalendarEventRecord {
                event_id: 7002,
                owner_id: 500_001,
                creator_id: 90_000_005,
                event_date_time: win_filetime,
                duration: 45,
                importance: 1,
                title: "Caldari State Muster".to_string(),
                description: "State ops".to_string(),
                scope: "Faction".to_string(),
                responses: resp,
                is_deleted: false,
            },
        );
        snap.calendar_events.insert(
            7003,
            eve_db::CalendarEventRecord {
                event_id: 7003,
                owner_id: 88_888_888,
                creator_id: 0,
                event_date_time: 1_700_001_000,
                duration: 15,
                importance: 0,
                title: "Mystery Owner".to_string(),
                description: "Unknown".to_string(),
                scope: "Personal".to_string(),
                responses: HashMap::new(),
                is_deleted: false,
            },
        );
        snap.calendar_events.insert(
            7004,
            eve_db::CalendarEventRecord {
                event_id: 7004,
                owner_id: 77_777_777,
                creator_id: 0,
                event_date_time: 1_700_002_000,
                duration: 60,
                importance: 1,
                title: "Server Event".to_string(),
                description: "Downtime".to_string(),
                scope: "CCP".to_string(),
                responses: HashMap::new(),
                is_deleted: false,
            },
        );
        snap.calendar_events.insert(
            7005,
            eve_db::CalendarEventRecord {
                event_id: 7005,
                owner_id: 99_000_001,
                creator_id: char_id,
                event_date_time: 1_700_003_000,
                duration: 90,
                importance: 1,
                title: "Alliance CTA".to_string(),
                description: "Form up".to_string(),
                scope: "Alliance".to_string(),
                responses: HashMap::new(),
                is_deleted: false,
            },
        );
    });

    ctx.db.with_snapshot(|snap| {
        let (events, responses) = load_calendar_events_from_game_state(&ctx, snap);
        assert!(!events.contains_key(&7001));

        let fac_ev = events.get(&7002).expect("faction event loaded");
        assert_eq!(fac_ev.event_date_unix, 1_700_000_000);
        assert_eq!(fac_ev.owner_type, "faction");
        assert!(!fac_ev.owner_name.is_empty());
        assert!(fac_ev.attendees.contains(&90_000_005));
        assert_eq!(
            responses.get(&(char_id, 7002)).map(String::as_str),
            Some("accepted")
        );
        assert_eq!(
            responses.get(&(90_000_002, 7002)).map(String::as_str),
            Some("declined")
        );
        assert_eq!(
            responses.get(&(90_000_003, 7002)).map(String::as_str),
            Some("tentative")
        );
        assert_eq!(
            responses.get(&(90_000_004, 7002)).map(String::as_str),
            Some("not_responded")
        );

        let unk_ev = events.get(&7003).expect("unknown owner event loaded");
        assert_eq!(unk_ev.owner_type, "character");
        assert_eq!(unk_ev.owner_name, "Owner 88888888");

        let ccp_ev = events.get(&7004).expect("CCP event loaded");
        assert_eq!(ccp_ev.owner_type, "eve_server");
        assert_eq!(ccp_ev.owner_name, "EVE System");

        let ally_ev = events.get(&7005).expect("social_extras alliance event loaded");
        assert_eq!(ally_ev.owner_type, "alliance");
        assert_eq!(ally_ev.duration_minutes, 90);
        assert!(is_character_invited(
            ally_ev,
            90_000_002,
            corp_id,
            Some(99_000_001)
        ));
    });
}

#[test]
fn dispatch_calendar_list_detail_put_and_attendees_branches() {
    let (ctx, _token, char_id, _corp_id, _planet_id) = seed_full_esi_context();
    let empty_q = HashMap::new();

    assert!(dispatch_char_calendar_route(
        &ctx,
        "DELETE",
        "/characters/{character_id}/calendar",
        &params(&[("character_id", &char_id.to_string())]),
        &empty_q,
        b""
    )
    .is_none());

    let bad_from = dispatch_char_calendar_route(
        &ctx,
        "GET",
        "/characters/{character_id}/calendar",
        &params(&[("character_id", &char_id.to_string())]),
        &params(&[("from_event", "0")]),
        b"",
    )
    .expect("handled");
    assert_eq!(bad_from.status_code, 400);

    let no_char = dispatch_char_calendar_route(
        &ctx,
        "GET",
        "/characters/{character_id}/calendar",
        &params(&[("character_id", "99999999")]),
        &empty_q,
        b"",
    )
    .expect("handled");
    assert_eq!(no_char.status_code, 404);

    ctx.db.with_mut_snapshot(|snap| {
        let mut r = HashMap::new();
        r.insert(90_000_002, 4);
        snap.calendar_events.insert(
            8100,
            eve_db::CalendarEventRecord {
                event_id: 8100,
                owner_id: 90_000_002,
                creator_id: 90_000_002,
                event_date_time: 1_700_010_000,
                duration: 30,
                importance: 0,
                title: "Private Event With Attendees".to_string(),
                description: "Secret".to_string(),
                scope: "Personal".to_string(),
                responses: r,
                is_deleted: false,
            },
        );
        snap.calendar_events.insert(
            8101,
            eve_db::CalendarEventRecord {
                event_id: 8101,
                owner_id: 90_000_002,
                creator_id: 0,
                event_date_time: 1_700_020_000,
                duration: 30,
                importance: 0,
                title: "Private Event Empty Attendees".to_string(),
                description: "Secret 2".to_string(),
                scope: "Personal".to_string(),
                responses: HashMap::new(),
                is_deleted: false,
            },
        );
    });

    let p_8100 = params(&[
        ("character_id", &char_id.to_string()),
        ("event_id", "8100"),
    ]);
    assert_eq!(
        dispatch_char_calendar_route(
            &ctx,
            "GET",
            "/characters/{character_id}/calendar/{event_id}",
            &p_8100,
            &empty_q,
            b""
        )
        .unwrap()
        .status_code,
        403
    );
    assert_eq!(
        dispatch_char_calendar_route(
            &ctx,
            "GET",
            "/characters/{character_id}/calendar/{event_id}/attendees",
            &p_8100,
            &empty_q,
            b""
        )
        .unwrap()
        .status_code,
        403
    );
    assert_eq!(
        dispatch_char_calendar_route(
            &ctx,
            "PUT",
            "/characters/{character_id}/calendar/{event_id}",
            &p_8100,
            &empty_q,
            br#"{"response":"accepted"}"#
        )
        .unwrap()
        .status_code,
        403
    );

    let p_8101 = params(&[
        ("character_id", &char_id.to_string()),
        ("event_id", "8101"),
    ]);
    assert_eq!(
        dispatch_char_calendar_route(
            &ctx,
            "GET",
            "/characters/{character_id}/calendar/{event_id}",
            &p_8101,
            &empty_q,
            b""
        )
        .unwrap()
        .status_code,
        404
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.calendar_events.insert(
            1,
            eve_db::CalendarEventRecord {
                event_id: 1,
                owner_id: char_id,
                creator_id: char_id,
                event_date_time: 1_700_000_000,
                duration: 60,
                importance: 0,
                title: "Op".to_string(),
                description: "Desc".to_string(),
                scope: "Personal".to_string(),
                responses: HashMap::new(),
                is_deleted: false,
            },
        );
    });
    let p_1 = params(&[("character_id", &char_id.to_string()), ("event_id", "1")]);
    for bad_body in [&b"not-json"[..], &br"{}"[..], &br#"{"response":"maybe"}"#[..]] {
        assert_eq!(
            dispatch_char_calendar_route(
                &ctx,
                "PUT",
                "/characters/{character_id}/calendar/{event_id}",
                &p_1,
                &empty_q,
                bad_body
            )
            .unwrap()
            .status_code,
            400
        );
    }

    let put_ok = dispatch_char_calendar_route(
        &ctx,
        "PUT",
        "/characters/{character_id}/calendar/{event_id}",
        &p_1,
        &empty_q,
        br#"{"response":"tentative"}"#,
    )
    .unwrap();
    assert_eq!(put_ok.status_code, 204);

    let detail_ok = dispatch_char_calendar_route(
        &ctx,
        "GET",
        "/characters/{character_id}/calendar/{event_id}",
        &p_1,
        &empty_q,
        b"",
    )
    .unwrap();
    assert_eq!(detail_ok.status_code, 200);
    assert_eq!(body(&detail_ok)["event_id"], 1);
    assert_eq!(body(&detail_ok)["response"], "tentative");

    let att_ok = dispatch_char_calendar_route(
        &ctx,
        "GET",
        "/characters/{character_id}/calendar/{event_id}/attendees",
        &p_1,
        &empty_q,
        b"",
    )
    .unwrap();
    assert_eq!(att_ok.status_code, 200);
    let att_arr = body(&att_ok).as_array().expect("attendees array");
    assert!(att_arr
        .iter()
        .any(|a| a["character_id"] == char_id && a["event_response"] == "tentative"));

    let list_filtered = dispatch_char_calendar_route(
        &ctx,
        "GET",
        "/characters/{character_id}/calendar",
        &params(&[("character_id", &char_id.to_string())]),
        &params(&[("from_event_id", "1")]),
        b"",
    )
    .unwrap();
    assert_eq!(list_filtered.status_code, 200);
    assert!(!body(&list_filtered).as_array().unwrap().is_empty());
}
