use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;

fn make_session(char_id: i64, corp_id: i64) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        corp_role: u64::MAX,
        ..Default::default()
    }
}

#[test]
fn ccp_and_character_calendar_event_lifecycle() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let char_id = 100_000_001;
    let corp_id = 1_000_009;
    let mut session = make_session(char_id, corp_id);
    let now_ft = (ctx.now_unix() as i64 + 11_644_473_600) * 10_000_000;
    let future_ft = now_ft + 3600 * 10_000_000;

    assert!(!has_calendar_manager_role(false, 0));
    assert!(has_calendar_manager_role(true, 0));
    assert!(has_calendar_manager_role(false, eve_db::corp_roles::ROLE_DIRECTOR));
    assert!(has_calendar_manager_role(false, ROLE_CHAT_MANAGER));

    assert!(create_ccp_calendar_event(&ctx, char_id, now_ft - 10, 60, 1, "past", "desc").is_err());
    assert!(create_ccp_calendar_event(&ctx, char_id, future_ft, 20_000, 1, "long", "desc").is_err());
    assert!(create_ccp_calendar_event(&ctx, char_id, future_ft, 60, 1, "  ", "desc").is_err());

    let ccp_ev = create_ccp_calendar_event(&ctx, char_id, future_ft, 60, 1, "live event", "details").unwrap();
    assert_eq!(ccp_ev.dict_get("ownerID").and_then(|v| v.as_i64()), Some(1));

    let created = handle_calendar_create(
        &ctx,
        &mut session,
        "CreatePersonalEvent",
        &[
            PyRep::Integer(future_ft),
            PyRep::Integer(90),
            PyRep::String("roam op".into()),
            PyRep::String("bring tackle".into()),
            PyRep::Integer(1),
            PyRep::List(vec![PyRep::Integer(100_000_002)]),
        ],
    )
    .unwrap();
    let eid = created.dict_get("eventID").and_then(|v| v.as_i64()).unwrap();

    let upd_parts = handle_calendar_mutate(
        &ctx,
        &mut session,
        "UpdateEventParticipants",
        &[
            PyRep::Integer(eid),
            PyRep::List(vec![PyRep::Integer(100_000_003)]),
            PyRep::List(vec![PyRep::Integer(100_000_002)]),
        ],
    )
    .unwrap();
    assert_eq!(upd_parts.dict_get("eventID").and_then(|v| v.as_i64()), Some(eid));

    let edited = handle_calendar_mutate(
        &ctx,
        &mut session,
        "EditPersonalEvent",
        &[
            PyRep::Integer(eid),
            PyRep::Integer(future_ft),
            PyRep::Integer(future_ft + 600 * 10_000_000),
            PyRep::Integer(120),
            PyRep::String("roam op updated".into()),
            PyRep::String("updated desc".into()),
            PyRep::Integer(1),
        ],
    )
    .unwrap();
    assert_eq!(edited.dict_get("eventID").and_then(|v| v.as_i64()), Some(eid));

    let mut invitee_session = make_session(100_000_003, corp_id);
    let rsvp = handle_calendar_mutate(
        &ctx,
        &mut invitee_session,
        "SendEventResponse",
        &[PyRep::Integer(eid), PyRep::Integer(char_id), PyRep::Integer(4)],
    )
    .unwrap();
    assert_eq!(rsvp.dict_get("status").and_then(|v| v.as_i64()), Some(4));

    let svc = "calendarMgr";
    let details = handle_calendar_get(&ctx, &mut session, svc, "GetEventDetails", &[PyRep::Integer(eid)])
        .unwrap()
        .unwrap();
    assert_eq!(details.dict_get("eventID").and_then(|v| v.as_i64()), Some(eid));

    let responses = handle_calendar_get(&ctx, &mut session, svc, "GetResponsesToEvent", &[PyRep::Integer(eid)])
        .unwrap()
        .unwrap();
    assert!(!matches!(responses, PyRep::None));

    let char_resps = handle_calendar_get(&ctx, &mut invitee_session, svc, "GetResponsesForCharacter", &[])
        .unwrap()
        .unwrap();
    assert!(!matches!(char_resps, PyRep::None));

    let list_all = handle_calendar_get(&ctx, &mut session, svc, "GetEventList", &[])
        .unwrap()
        .unwrap();
    assert!(!matches!(list_all, PyRep::None));

    let deleted = handle_calendar_mutate(&ctx, &mut session, "DeleteEvent", &[PyRep::Integer(eid)]).unwrap();
    assert_eq!(deleted.dict_get("deleted").and_then(|v| v.as_bool()), Some(true));
}
