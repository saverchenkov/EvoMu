use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use eve_db::{AllianceRecord, CorporationRecord};
use std::collections::HashMap;

#[test]
fn test_lookup_svc_bannedwords_and_all_lookup_rpcs() {
    let (ctx, session, _char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();

    assert!(is_word_allowed("CleanPilotName"));
    assert!(!is_word_allowed("some exploit here"));
    assert!(!is_word_allowed("some badword1 here"));

    // Seed custom player corporation and alliance for lookup/search
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            98_000_777,
            CorporationRecord {
                corporation_id: 98_000_777,
                name: "Starlight Syndicate".into(),
                ticker: "STRLT".into(),
                ceo_id: 90_000_001,
                alliance_id: Some(99_000_777),
                tax_rate: 0.05,
                description: "Test corp".into(),
                hq_station_id: Some(60_003_760),
                ..Default::default()
            },
        );
        snap.alliances.insert(
            99_000_777,
            AllianceRecord {
                alliance_id: 99_000_777,
                name: "Starlight Coalition".into(),
                short_name: "STRLA".into(),
                executor_corp_id: 98_000_777,
                member_corp_ids: vec![98_000_777].into_iter().collect(),
            },
        );
    });

    // Test all lookupSvc methods
    for (method, args) in [
        ("LookupCharacters", vec![PyRep::String("Test".into()), PyRep::Integer(0)]),
        ("LookupEvePlayerCharacters", vec![PyRep::String("Test".into()), PyRep::Integer(0)]),
        ("LookupCorporations", vec![PyRep::String("Starlight".into()), PyRep::Integer(0)]),
        ("LookupFactions", vec![PyRep::String("Caldari".into()), PyRep::Integer(0)]),
        ("LookupKnownLocationsByGroup", vec![PyRep::Integer(5), PyRep::String("Jita".into())]),
        ("LookupLocationsByGroup", vec![PyRep::Integer(15), PyRep::String("Jita".into())]),
        ("LookupNoneNPCAccountOwners", vec![PyRep::String("Starlight".into()), PyRep::Integer(0)]),
        ("LookupOwners", vec![PyRep::String("Starlight".into()), PyRep::Integer(0)]),
        ("LookupPCOwners", vec![PyRep::String("Starlight".into()), PyRep::Integer(0)]),
        (
            "LookupWarableCorporationsOrAlliances",
            vec![PyRep::String("Starlight".into()), PyRep::Integer(0)],
        ),
    ] {
        let res = handle(&ctx, &session, "lookupSvc", method, &args, &kw)
            .unwrap_or_else(|| panic!("expected Some for lookupSvc.{method}"))
            .unwrap();
        assert!(matches!(res, PyRep::List(_)), "lookupSvc.{method} did not return List");
    }

    // Test bannedwords service
    let regexes = handle(&ctx, &session, "bannedwords", "fetch_regexes", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(regexes, PyRep::List(_)));

    let ok_word = handle(
        &ctx,
        &session,
        "bannedwords",
        "check_words_allowed",
        &[PyRep::String("Hello space friends".into())],
        &kw,
    )
    .unwrap();
    assert!(ok_word.is_ok());

    let bad_word = handle(
        &ctx,
        &session,
        "bannedwords",
        "check_chat_words_allowed",
        &[PyRep::String("badword1".into())],
        &kw,
    )
    .unwrap();
    assert!(bad_word.is_err());

    // Test search.Query and search.QuickQuery
    let search_q = handle(
        &ctx,
        &session,
        "search",
        "Query",
        &[
            PyRep::String("Starlight".into()),
            PyRep::List(vec![
                PyRep::Integer(1),
                PyRep::Integer(3),
                PyRep::Integer(4),
            ]),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(search_q, PyRep::Dict(_)));

    let quick_q = handle(
        &ctx,
        &session,
        "search",
        "QuickQuery",
        &[
            PyRep::String("Starlight".into()),
            PyRep::List(vec![
                PyRep::Integer(3),
                PyRep::Integer(4),
            ]),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    if let PyRep::List(ids) = quick_q {
        assert!(ids.contains(&PyRep::Integer(98_000_777)));
        assert!(ids.contains(&PyRep::Integer(99_000_777)));
    } else {
        panic!("expected list");
    }

    // Dynamic server-configured banned words stored in service_state_blobs
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs
            .insert("bannedwords:words".into(), "forbiddenalpha,badbeta".into());
        snap.service_state_blobs
            .insert("bannedwords:regexes".into(), r"(?i)\bcustompat\b".into());
    });
    let regexes_after = handle(&ctx, &session, "bannedwords", "fetch_regexes", &[], &kw)
        .unwrap()
        .unwrap();
    let PyRep::List(r_list) = regexes_after else { panic!("expected list"); };
    assert!(r_list.iter().any(|r| r.as_str().unwrap_or("").contains("forbiddenalpha")));
    assert!(r_list.contains(&PyRep::String(r"(?i)\bcustompat\b".into())));
    assert!(handle(
        &ctx,
        &session,
        "bannedwords",
        "check_words_allowed",
        &[PyRep::String("Station forbiddenalpha".into())],
        &kw,
    )
    .unwrap()
    .is_err());
    assert!(handle(
        &ctx,
        &session,
        "bannedwords",
        "check_chat_words_allowed",
        &[PyRep::String("hello badbeta".into())],
        &kw,
    )
    .unwrap()
    .is_err());
    assert_eq!(
        handle(
            &ctx,
            &session,
            "bannedwords",
            "check_search_words_allowed",
            &[PyRep::String("forbiddenalpha".into())],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );
}
