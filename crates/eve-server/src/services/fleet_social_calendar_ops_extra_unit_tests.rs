//! Extra branch-coverage and mutation-killing unit tests for `fleet_social_calendar_ops.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::fleet_social::fleet_social_calendar_ops::{
    create_ccp_calendar_event, handle_calendar_create, handle_calendar_get, handle_calendar_mutate,
    ROLE_CHAT_MANAGER,
};
use crate::services::fleet_social::fleet_social_chat::CalendarEventState;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use eve_db::{
    corp_roles, AllianceRecord, CalendarEventRecord, CorporationRecord, MoonExtractionRecord,
    UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

fn i(v: i64) -> PyRep {
    PyRep::Integer(v)
}

fn s(v: &str) -> PyRep {
    PyRep::String(v.to_string())
}

fn make_session(char_id: i64, corp_id: i64, ally_id: Option<i64>, corp_role: u64) -> SessionState {
    SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        alliance_id: ally_id,
        solar_system_id: Some(30_000_142),
        corp_role,
        ..Default::default()
    }
}

fn call_create(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle_calendar_create(ctx, session, method, args)
}

fn call_mutate(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle_calendar_mutate(ctx, session, method, args)
}

fn call_get(
    ctx: &ServerContext,
    session: &mut SessionState,
    proxy: bool,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let service = if proxy { "calendarProxy" } else { "calendarMgr" };
    handle_calendar_get(ctx, session, service, method, args).expect("calendar get handled")
}

#[test]
fn ccp_calendar_event_boundaries_and_db_id_monotonicity() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let creator_id = 100_000_001_i64;
    let now_ft = (ctx.now_unix() as i64 + 11_644_473_600) * 10_000_000;

    for (dt, dur, title) in [(now_ft - 1, 0, "Title"), (now_ft, -1, "Title"), (now_ft, 10_001, "Title")] {
        assert_eq!(
            create_ccp_calendar_event(&ctx, creator_id, dt, dur, 0, title, "Desc").unwrap_err().code,
            "UserError"
        );
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.calendar_events.insert(
            800_000,
            CalendarEventRecord {
                event_id: 800_000,
                owner_id: 1,
                creator_id,
                event_date_time: now_ft,
                duration: 60,
                importance: 0,
                title: "Seeded".into(),
                description: String::new(),
                scope: "CCP".into(),
                responses: HashMap::new(),
                is_deleted: false,
            },
        );
    });

    let ok_zero_dur =
        create_ccp_calendar_event(&ctx, creator_id, now_ft, 0, 1, "  Trimmed CCP  ", "  Body  ").unwrap();
    assert_eq!(ok_zero_dur.dict_get("eventID"), Some(&i(800_001)));
    assert_eq!(ok_zero_dur.dict_get("ownerID"), Some(&i(1)));
    assert_eq!(ok_zero_dur.dict_get("title"), Some(&s("Trimmed CCP")));
    assert_eq!(ok_zero_dur.dict_get("scope"), Some(&s("CCP")));

    let ok_max_dur = create_ccp_calendar_event(&ctx, creator_id, now_ft, 10_000, 0, "MaxDur", "Desc").unwrap();
    assert_eq!(ok_max_dur.dict_get("eventID"), Some(&i(800_002)));
}

#[test]
fn create_event_roles_corporation_alliance_and_invitee_caps() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (ceo_id, dir_id, comm_id, member_id) = (100_000_010, 100_000_011, 100_000_012, 100_000_013);
    let (corp_id, non_exec_corp_id, ally_id) = (98_000_010, 98_000_011, 99_000_010);
    let now_ft = (ctx.now_unix() as i64 + 11_644_473_600) * 10_000_000;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Exec Corp".into();
                __corp.ticker = "EXEC".into();
                __corp.ceo_id = ceo_id;
                __corp.alliance_id = Some(ally_id);
                __corp.tax_rate = 0.05;
                __corp.member_roles = HashMap::from([
                    (dir_id, corp_roles::ROLE_DIRECTOR),
                    (comm_id, ROLE_CHAT_MANAGER),
                    (member_id, 0),
                ]);
                __corp },
        );
        snap.corporations.insert(
            non_exec_corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = non_exec_corp_id;
                __corp.name = "Sub Corp".into();
                __corp.ticker = "SUBC".into();
                __corp.ceo_id = dir_id;
                __corp.alliance_id = Some(ally_id);
                __corp.tax_rate = 0.05;
                __corp },
        );
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "Test Alliance".into(),
                short_name: "TALLY".into(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id, non_exec_corp_id]),
            },
        );
    });

    let mut unauth = SessionState::default();
    assert!(call_create(&ctx, &mut unauth, "CreatePersonalEvent", &[]).is_err());

    let mut mem_sess = make_session(member_id, corp_id, Some(ally_id), 0);
    let args = [i(now_ft), i(60), s("Corp Op")];
    assert_eq!(
        call_create(&ctx, &mut mem_sess, "CreateCorporationEvent", &args).unwrap_err().code,
        "CalendarAccessDenied"
    );

    let mut ceo_sess = make_session(ceo_id, corp_id, Some(ally_id), 0);
    let ceo_ev = call_create(&ctx, &mut ceo_sess, "CreateCorporationEvent", &args).unwrap();
    assert_eq!(ceo_ev.dict_get("ownerID"), Some(&i(corp_id)));

    let mut dir_sess = make_session(dir_id, corp_id, Some(ally_id), corp_roles::ROLE_DIRECTOR);
    assert!(call_create(&ctx, &mut dir_sess, "CreateCorporationEvent", &args).is_ok());

    let mut comm_sess = make_session(comm_id, corp_id, Some(ally_id), ROLE_CHAT_MANAGER);
    assert!(call_create(&ctx, &mut comm_sess, "CreateCorporationEvent", &args).is_ok());

    let mut no_ally_sess = make_session(ceo_id, corp_id, None, corp_roles::ROLE_DIRECTOR);
    assert_eq!(
        call_create(&ctx, &mut no_ally_sess, "CreateAllianceEvent", &args).unwrap_err().code,
        "CalendarAccessDenied"
    );
    let mut sub_corp_ceo = make_session(dir_id, non_exec_corp_id, Some(ally_id), u64::MAX);
    assert_eq!(
        call_create(&ctx, &mut sub_corp_ceo, "CreateAllianceEvent", &args).unwrap_err().code,
        "CalendarAccessDenied"
    );
    assert_eq!(
        call_create(&ctx, &mut mem_sess, "CreateAllianceEvent", &args).unwrap_err().code,
        "CalendarAccessDenied"
    );
    let ally_ev = call_create(&ctx, &mut comm_sess, "CreateAllianceEvent", &args).unwrap();
    assert_eq!(ally_ev.dict_get("ownerID"), Some(&i(ally_id)));

    assert!(call_create(&ctx, &mut mem_sess, "CreatePersonalEvent", &[]).is_err());
    for (dt, dur, title) in [(now_ft - 1, 30, "T"), (now_ft, -1, "T"), (now_ft, 10_001, "T"), (now_ft, 10, "   ")] {
        assert!(call_create(&ctx, &mut mem_sess, "CreatePersonalEvent", &[i(dt), i(dur), s(title)]).is_err());
    }

    let inv_50: Vec<PyRep> = (200_001..=200_050).map(i).collect();
    assert!(call_create(
        &ctx,
        &mut mem_sess,
        "CreatePersonalEvent",
        &[i(now_ft), i(0), s("50 Invitees"), s("Desc"), i(1), PyRep::List(inv_50)]
    )
    .is_ok());

    let inv_51: Vec<PyRep> = (200_001..=200_051).map(i).collect();
    assert_eq!(
        call_create(
            &ctx,
            &mut mem_sess,
            "CreatePersonalEvent",
            &[i(now_ft), i(10_000), s("51 Invitees"), s("Desc"), i(1), PyRep::List(inv_51.clone())]
        )
        .unwrap_err()
        .code,
        "UserError"
    );
    assert!(call_create(
        &ctx,
        &mut ceo_sess,
        "CreateCorporationEvent",
        &[i(now_ft), i(10_000), s("51 Corp Invitees"), s("Desc"), i(1), PyRep::List(inv_51)]
    )
    .is_ok());
}

#[test]
fn mutate_delete_edit_participants_and_rsvp_authorization_matrix() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (owner_id, peer_id, stranger_id) = (100_000_020, 100_000_021, 100_000_022);
    let (corp_id, ally_id) = (98_000_020, 99_000_020);
    let now_ft = (ctx.now_unix() as i64 + 11_644_473_600) * 10_000_000;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Mut Corp".into();
                __corp.ticker = "MUTC".into();
                __corp.ceo_id = owner_id;
                __corp.alliance_id = Some(ally_id);
                __corp },
        );
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "Mut Ally".into(),
                short_name: "MUTA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
            },
        );
    });

    {
        let mut sc = ctx.social_extras.lock().unwrap();
        for (eid, scope, ev_owner) in [
            (10, "Automated", corp_id),
            (11, "CCP", 1),
            (12, "Corporation", corp_id),
            (13, "Alliance", ally_id),
            (14, "Personal", owner_id),
        ] {
            sc.calendar_events.insert(
                eid,
                CalendarEventState {
                    event_id: eid,
                    owner_id: ev_owner,
                    creator_id: owner_id,
                    event_date_time: now_ft + 100_000_000,
                    duration: 60,
                    importance: 0,
                    title: format!("Event {eid}"),
                    description: "Initial".into(),
                    _scope: scope.into(),
                    responses: HashMap::from([(owner_id, 4), (peer_id, 3)]),
                    is_deleted: false,
                },
            );
        }
        sc.next_id = 20;
    }

    let mut owner_sess = make_session(owner_id, corp_id, Some(ally_id), corp_roles::ROLE_DIRECTOR);
    let mut peer_sess = make_session(peer_id, corp_id, Some(ally_id), 0);
    let mut stranger_sess = make_session(stranger_id, 98_999_999, None, 0);

    for bad_id in [0, -1, 999] {
        assert_eq!(call_mutate(&ctx, &mut owner_sess, "DeleteEvent", &[i(bad_id)]).unwrap_err().code, "CalendarEventNotFound");
    }
    for denied_id in [10, 11] {
        assert_eq!(call_mutate(&ctx, &mut owner_sess, "DeleteEvent", &[i(denied_id)]).unwrap_err().code, "CalendarAccessDenied");
    }
    let mut gmh_sess = owner_sess.clone();
    gmh_sess.role = eve_machonet::service_roles::ROLE_GMH;
    assert!(call_mutate(&ctx, &mut gmh_sess, "DeleteEvent", &[i(11)]).is_ok());
    assert_eq!(call_mutate(&ctx, &mut gmh_sess, "DeleteEvent", &[i(11)]).unwrap_err().code, "CalendarEventNotFound");

    assert_eq!(
        call_mutate(&ctx, &mut peer_sess, "UpdateEventParticipants", &[i(14), PyRep::List(vec![]), PyRep::List(vec![])])
            .unwrap_err()
            .code,
        "CalendarAccessDenied"
    );
    assert!(call_mutate(
        &ctx,
        &mut owner_sess,
        "UpdateEventParticipants",
        &[i(14), PyRep::List(vec![i(stranger_id), i(owner_id)]), PyRep::List(vec![i(peer_id), i(owner_id)])]
    )
    .is_ok());

    assert_eq!(
        call_mutate(&ctx, &mut peer_sess, "SendEventResponse", &[i(14), i(owner_id), i(4)]).unwrap_err().code,
        "CalendarAccessDenied"
    );
    assert_eq!(
        call_mutate(&ctx, &mut stranger_sess, "SendEventResponse", &[i(14)]).unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        call_mutate(&ctx, &mut stranger_sess, "SendEventResponse", &[i(14), i(999), i(4)]).unwrap_err().code,
        "CalendarEventNotFound"
    );
    for bad_code in [0, 1, 3, 6] {
        assert_eq!(
            call_mutate(&ctx, &mut stranger_sess, "SendEventResponse", &[i(14), i(owner_id), i(bad_code)]).unwrap_err().code,
            "UserError"
        );
    }
    for valid_code in [2, 4, 5] {
        let res = call_mutate(&ctx, &mut stranger_sess, "SendEventResponse", &[i(14), i(owner_id), i(valid_code)]).unwrap();
        assert_eq!(res.dict_get("status"), Some(&i(valid_code)));
    }
    assert!(call_mutate(&ctx, &mut peer_sess, "SendEventResponse", &[i(12), i(corp_id), i(2)]).is_ok());
    assert!(call_mutate(&ctx, &mut peer_sess, "SendEventResponse", &[i(13), i(ally_id), i(5)]).is_ok());

    assert_eq!(
        call_mutate(&ctx, &mut owner_sess, "EditCorporationEvent", &[i(14), i(now_ft), i(30), s("X")]).unwrap_err().code,
        "CalendarAccessDenied"
    );
    for (dt, dur, title) in [(now_ft - 1, 30, "T"), (now_ft, 10_001, "T"), (now_ft, 30, "   ")] {
        assert!(call_mutate(&ctx, &mut owner_sess, "EditCorporationEvent", &[i(12), i(dt), i(dur), s(title)]).is_err());
        assert!(call_mutate(
            &ctx,
            &mut owner_sess,
            "EditPersonalEvent",
            &[i(14), i(10_001), i(dt), i(dur), s(title), s("D"), i(1)]
        )
        .is_err());
    }
    assert!(call_mutate(
        &ctx,
        &mut owner_sess,
        "EditCorporationEvent",
        &[i(12), i(now_ft), PyRep::None, s("Corp Updated"), s("New Corp Desc"), i(2)]
    )
    .is_ok());
    assert!(call_mutate(&ctx, &mut owner_sess, "EditAllianceEvent", &[i(13), s("Ally Short"), s("Desc")]).is_ok());
    assert!(call_mutate(
        &ctx,
        &mut owner_sess,
        "EditPersonalEvent",
        &[i(14), i(10_001), i(now_ft), PyRep::None, s("Valid 7-arg"), s("Desc"), i(3)]
    )
    .is_ok());
    assert!(call_mutate(&ctx, &mut owner_sess, "DeleteEvent", &[i(12)]).is_ok());
    assert!(call_mutate(&ctx, &mut owner_sess, "DeleteEvent", &[i(13)]).is_ok());
}

#[test]
fn get_event_details_responses_month_year_filtering_and_moon_extractions() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (owner_id, stranger_id, corp_id, ally_id) = (100_000_030, 100_000_031, 98_000_030, 99_000_030);
    let nov_2023_unix = 1_700_000_000_i64;
    let nov_2023_ft = (nov_2023_unix + 11_644_473_600) * 10_000_000;
    let jan_2024_unix = 1_705_276_800_i64;

    {
        let mut sc = ctx.social_extras.lock().unwrap();
        for (eid, scope, ev_owner, dt) in [
            (30, "Automated", corp_id, nov_2023_ft),
            (31, "CCP", 1, nov_2023_ft),
            (32, "Corporation", corp_id, nov_2023_ft),
            (33, "Alliance", ally_id, nov_2023_ft),
            (34, "Personal", owner_id, jan_2024_unix),
        ] {
            sc.calendar_events.insert(
                eid,
                CalendarEventState {
                    event_id: eid,
                    owner_id: ev_owner,
                    creator_id: owner_id,
                    event_date_time: dt,
                    duration: 120,
                    importance: 1,
                    title: format!("Title {eid}"),
                    description: format!("Desc {eid}"),
                    _scope: scope.into(),
                    responses: HashMap::from([(owner_id, 4)]),
                    is_deleted: false,
                },
            );
        }
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            700_001,
            UpwellStructureRecord {
                structure_id: 700_001,
                type_id: 35835,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Athanor Refinery".into(),
                position: [0.0, 0.0, 0.0],
                state: "Online".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.moon_extractions.insert(
            700_001,
            MoonExtractionRecord {
                structure_id: 700_001,
                moon_id: 40_009_082,
                extraction_start_unix: 1_699_900_000,
                chunk_arrival_unix: nov_2023_unix as u64,
                natural_decay_unix: (nov_2023_unix + 7200) as u64,
                status: "Started".into(),
                event_id: 888,
                auto_fracture: false,
                moon_ores: Vec::new(),
                spawned_asteroid_ids: Vec::new(),
            },
        );
    });

    let mut owner_sess = make_session(owner_id, corp_id, Some(ally_id), 0);
    let mut stranger_sess = make_session(stranger_id, 98_888_888, None, 0);

    for (eid, exp_flag, exp_auto) in [
        (30, 16, i(1)),
        (31, 8, PyRep::None),
        (32, 2, PyRep::None),
        (33, 4, PyRep::None),
        (34, 1, PyRep::None),
    ] {
        let det = call_get(&ctx, &mut owner_sess, false, "GetEventDetails", &[i(eid)]).unwrap();
        assert_eq!(det.dict_get("flag"), Some(&i(exp_flag)));
        assert_eq!(det.dict_get("autoEventType"), Some(&exp_auto));
    }

    for method in ["GetEventDetails", "GetResponsesToEvent"] {
        let missing_err = call_get(&ctx, &mut owner_sess, false, method, &[i(34), i(999)]).unwrap_err();
        assert_eq!(missing_err.code, "CalendarEventNotFound");
        let denied_err = call_get(&ctx, &mut stranger_sess, false, method, &[i(34)]).unwrap_err();
        assert_eq!(denied_err.code, "CalendarAccessDenied");
    }
    assert!(call_get(&ctx, &mut stranger_sess, false, "GetEventDetails", &[i(31)]).is_ok());

    let proxy_nov = call_get(&ctx, &mut owner_sess, true, "GetEventList", &[i(2023), i(11)]).unwrap();
    let PyRep::List(proxy_list) = proxy_nov else { panic!("expected list") };
    assert_eq!(proxy_list.len(), 1);

    let jan_only = call_get(&ctx, &mut owner_sess, false, "GetEventList", &[i(1), i(2024)]).unwrap();
    assert!(!matches!(jan_only, PyRep::None));
}
