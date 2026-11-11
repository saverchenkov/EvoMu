use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;

#[test]
fn test_operations_survey_handlers_dmy_slash_checks_and_claim() {
    let (ctx, mut session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    session.station_id = Some(60_003_760);

    // Test parse_dmy_to_unix
    assert_eq!(parse_dmy_to_unix("01/01/1970"), Some(0));
    assert!(parse_dmy_to_unix("29/02/2024").is_some());
    assert!(parse_dmy_to_unix("invalid").is_none());

    // Ensure character account age satisfies min_account_age_hours
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.create_date_unix = 1_000;
        }
    });

    // Schedule a survey via /survey schedule slash command with item and VGS rewards
    let sched = handle_survey_slash(
        &ctx,
        char_id,
        "/survey schedule 9001 86400 34:10 501",
    )
    .unwrap()
    .unwrap();
    assert_eq!(sched.dict_get("surveyID").and_then(|v| v.as_i64()), Some(9001));
    assert_eq!(sched.dict_get("scheduled").and_then(|v| v.as_bool()), Some(true));

    // PerformSurveyChecks should find survey 9001 eligible and set it active
    let checks = handle_survey_perform_checks(&ctx, &session).unwrap();
    if let PyRep::List(list) = checks {
        assert!(list.contains(&PyRep::Integer(9001)));
    } else {
        panic!("expected list");
    }

    // Claiming before completion fails with SurveyNotCompleted
    let claim_early = handle_survey_claim_rewards(&ctx, &session, &[PyRep::Integer(9001)]);
    assert!(claim_early.is_err());

    // Complete survey via /survey complete slash command
    let comp = handle_survey_slash(&ctx, char_id, "/survey complete 9001")
        .unwrap()
        .unwrap();
    assert_eq!(comp.dict_get("completed").and_then(|v| v.as_bool()), Some(true));

    // Claim survey rewards
    let claim_ok = handle_survey_claim_rewards(&ctx, &session, &[PyRep::Integer(9001)]).unwrap();
    assert_eq!(claim_ok, PyRep::Bool(true));

    // Re-claiming same survey fails with SurveyAlreadyClaimed
    let claim_dup = handle_survey_claim_rewards(&ctx, &session, &[PyRep::Integer(9001)]);
    assert!(claim_dup.is_err());
}

#[test]
fn test_operations_survey_handlers_edge_errors_eligibility_and_redeem_delivery() {
    let (ctx, mut session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();

    // 1. parse_dmy_to_unix validation & leap year boundary checks
    assert_eq!(parse_dmy_to_unix("01/01/1970"), Some(0));
    assert_eq!(parse_dmy_to_unix("00/01/2020"), None);
    assert_eq!(parse_dmy_to_unix("32/01/2020"), None);
    assert_eq!(parse_dmy_to_unix("15/00/2020"), None);
    assert_eq!(parse_dmy_to_unix("15/13/2020"), None);
    assert_eq!(parse_dmy_to_unix("15/05/1969"), None);
    assert_eq!(parse_dmy_to_unix("15/05"), None);
    assert_eq!(parse_dmy_to_unix("aa/05/2020"), None);
    // 2000 is leap year (366 days), 1900 is not (if <1970 invalid, test 2023 vs 2024 Feb->Mar)
    let mar1_2023 = parse_dmy_to_unix("01/03/2023").unwrap();
    let feb28_2023 = parse_dmy_to_unix("28/02/2023").unwrap();
    assert_eq!(mar1_2023 - feb28_2023, 86_400);
    let mar1_2024 = parse_dmy_to_unix("01/03/2024").unwrap();
    let feb28_2024 = parse_dmy_to_unix("28/02/2024").unwrap();
    assert_eq!(mar1_2024 - feb28_2024, 2 * 86_400);

    // 2. build_scheduled_survey_from_slash error paths & defaults
    assert!(build_scheduled_survey_from_slash(&ctx.sde, 10_000, &[]).is_err());
    assert!(build_scheduled_survey_from_slash(&ctx.sde, 10_000, &["0"]).is_err());
    assert!(build_scheduled_survey_from_slash(&ctx.sde, 10_000, &["-5"]).is_err());
    assert!(build_scheduled_survey_from_slash(&ctx.sde, 10_000, &["9010", "0"]).is_err());
    assert!(build_scheduled_survey_from_slash(&ctx.sde, 10_000, &["9010", "abc"]).is_err());
    assert!(build_scheduled_survey_from_slash(&ctx.sde, 10_000, &["9010", "60", "badpair"]).is_err());
    assert!(build_scheduled_survey_from_slash(&ctx.sde, 10_000, &["9010", "60", "999999999:1"]).is_err());
    assert!(build_scheduled_survey_from_slash(&ctx.sde, 10_000, &["9010", "60", "34:0"]).is_err());
    assert!(build_scheduled_survey_from_slash(&ctx.sde, 10_000, &["9010", "60", "-", "0"]).is_err());
    let default_dur = build_scheduled_survey_from_slash(&ctx.sde, 10_000, &["9010"]).unwrap();
    assert_eq!(default_dur.expires_at_unix, Some(10_000 + 86_400));

    // 3. combined_survey_catalog override & register_survey update-in-place
    let mut custom_a = default_dur.clone();
    custom_a.name = "Initial".into();
    register_survey(&ctx, custom_a);
    let mut custom_b = default_dur.clone();
    custom_b.name = "Updated".into();
    register_survey(&ctx, custom_b.clone());
    ctx.db.with_snapshot(|snap| {
        let store = load_store_pub(snap);
        assert_eq!(
            store.custom_surveys.iter().filter(|s| s.survey_id == 9010).count(),
            1
        );
        assert_eq!(
            store.custom_surveys.iter().find(|s| s.survey_id == 9010).unwrap().name,
            "Updated"
        );
    });
    let merged = combined_survey_catalog(&ctx.sde, &[custom_b]);
    assert_eq!(merged.iter().find(|s| s.survey_id == 9010).unwrap().name, "Updated");

    // 4. eligible_surveys filtering: completed, claimed, future start, expired, account age
    let now = 100_000_u64;
    let mut rec = CharacterOperationsRecord::default();
    rec.completed_surveys.insert(1);
    rec.claimed_surveys.insert(2);
    let mk_surv = |id: i64, start: Option<u64>, exp: Option<u64>, min_age_h: u32| SurveyDefinition {
        survey_id: id,
        name: format!("S{id}"),
        description: String::new(),
        start_date: None,
        end_date: None,
        start_unix: start,
        expires_at_unix: exp,
        grace_period_days: 0,
        min_account_age_hours: min_age_h,
        item_rewards: vec![],
        vgs_rewards: vec![],
    };
    let test_cat = vec![
        mk_surv(1, None, None, 0),                      // already completed
        mk_surv(2, None, None, 0),                      // already claimed
        mk_surv(3, Some(now + 100), None, 0),           // not started yet
        mk_surv(4, None, Some(now - 1), 0),             // expired
        mk_surv(5, Some(now - 10), Some(now + 10), 24), // too young if create_unix = now - 3600
        mk_surv(6, Some(now - 10), Some(now + 10), 1),  // old enough (3600s >= 3600s)
    ];
    assert_eq!(eligible_surveys(&rec, now - 3_600, &test_cat, now), vec![6]);
    // char_create_unix == 0 bypasses min_account_age_hours check
    assert_eq!(eligible_surveys(&rec, 0, &test_cat, now), vec![5, 6]);

    // 5. handle_survey_slash edge paths
    assert!(handle_survey_slash(&ctx, char_id, "/help").is_none());
    assert_eq!(
        handle_survey_slash(&ctx, char_id, "/survey bogus").unwrap().unwrap_err().code,
        "InvalidSlashArgument"
    );
    assert_eq!(
        handle_survey_slash(&ctx, char_id, "/survey schedule bad").unwrap().unwrap_err().code,
        "InvalidSlashArgument"
    );
    assert_eq!(
        handle_survey_slash(&ctx, char_id, "/survey complete").unwrap().unwrap_err().code,
        "NoActiveSurvey"
    );

    // 6. mark_survey_completed error paths
    assert_eq!(mark_survey_completed(&ctx, 99_999_999, 9010).unwrap_err().code, "CharacterNotFound");
    assert_eq!(mark_survey_completed(&ctx, char_id, 88_888_888).unwrap_err().code, "InvalidSurveyID");
    let now_live = ctx.now_unix();
    register_survey(&ctx, mk_surv(9011, Some(now_live + 5_000), Some(now_live + 10_000), 0));
    assert_eq!(mark_survey_completed(&ctx, char_id, 9011).unwrap_err().code, "SurveyNotActive");
    register_survey(&ctx, mk_surv(9012, Some(10), Some(now_live.saturating_sub(10)), 0));
    assert_eq!(mark_survey_completed(&ctx, char_id, 9012).unwrap_err().code, "SurveyExpired");

    // 7. handle_survey_perform_checks & handle_survey_claim_rewards error paths & undocked redeem delivery
    let anon = SessionState::default();
    assert!(handle_survey_perform_checks(&ctx, &anon).is_err());
    assert!(handle_survey_claim_rewards(&ctx, &anon, &[]).is_err());

    let mut ghost_sess = session.clone();
    ghost_sess.character_id = Some(99_999_999);
    assert_eq!(handle_survey_claim_rewards(&ctx, &ghost_sess, &[]).unwrap_err().code, "CharacterNotFound");
    assert_eq!(handle_survey_claim_rewards(&ctx, &session, &[]).unwrap_err().code, "NoActiveSurvey");
    assert_eq!(handle_survey_claim_rewards(&ctx, &session, &[PyRep::Integer(88_888_888)]).unwrap_err().code, "InvalidSurveyID");
    assert_eq!(handle_survey_claim_rewards(&ctx, &session, &[PyRep::Integer(9011)]).unwrap_err().code, "SurveyNotActive");
    assert_eq!(handle_survey_claim_rewards(&ctx, &session, &[PyRep::Integer(9012)]).unwrap_err().code, "SurveyExpired");

    // Activate survey 9013 via /survey activate with zero-amount item skipped + valid item + duplicate vgs,
    // verify perform_checks sets active_survey_id, /survey complete without survey_id uses active_survey_id,
    // and undocked claim delivers to redeem queue!
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.create_date_unix = 1_000;
            c.station_id = None;
            c.structure_id = None;
        }
    });
    session.station_id = None;
    session.structure_id = None;

    let act_rep = handle_survey_slash(&ctx, char_id, "/survey activate 9013 3600 34:5 701,701")
        .unwrap()
        .unwrap();
    assert_eq!(act_rep.dict_get("surveyID"), Some(&PyRep::Integer(9013)));

    // Register an extra zero-qty item to cover amount == 0 skip
    register_survey(
        &ctx,
        SurveyDefinition {
            survey_id: 9013,
            name: "Active".into(),
            description: "Desc".into(),
            start_date: None,
            end_date: None,
            start_unix: Some(now_live.saturating_sub(100)),
            expires_at_unix: Some(now_live + 3_600),
            grace_period_days: 1,
            min_account_age_hours: 0,
            item_rewards: vec![(34, 0), (34, 5)],
            vgs_rewards: vec![701, 701],
        },
    );

    let checks = handle_survey_perform_checks(&ctx, &session).unwrap();
    assert!(matches!(checks, PyRep::List(ref l) if l.contains(&PyRep::Integer(9013))));

    // /survey complete without explicit survey_id completes active_survey_id (9013)
    let comp_auto = handle_survey_slash(&ctx, char_id, "/survey complete")
        .unwrap()
        .unwrap();
    assert_eq!(comp_auto.dict_get("surveyID"), Some(&PyRep::Integer(9013)));

    // Implicit claim (no args) selects unclaimed completed survey 9013 and enqueues redeem token when undocked
    let claim_rep = handle_survey_claim_rewards(&ctx, &session, &[]).unwrap();
    assert_eq!(claim_rep, PyRep::Bool(true));

    ctx.db.with_snapshot(|snap| {
        let store = load_store_pub(snap);
        let r = &store.by_character[&char_id];
        assert!(r.claimed_surveys.contains(&9013));
        assert_eq!(r.active_survey_id, None);
        assert_eq!(r.claimed_vgs_rewards.iter().filter(|&&id| id == 701).count(), 1);
    });
    assert_eq!(mark_survey_completed(&ctx, char_id, 9013).unwrap_err().code, "SurveyAlreadyClaimed");
}
