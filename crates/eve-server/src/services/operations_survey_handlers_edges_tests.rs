#![forbid(unsafe_code)]

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use eve_db::clock::VirtualServerClock;
use std::sync::Arc;

#[test]
fn survey_dmy_catalog_eligibility_and_slash_parsing_edges() {
    let (ctx, _sess, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000));
    ctx.set_clock(clock);

    // 1. parse_dmy_to_unix invalid & leap-year / non-leap-year edge cases
    assert_eq!(parse_dmy_to_unix("01/01"), None);
    assert_eq!(parse_dmy_to_unix("01/01/2024/01"), None);
    assert_eq!(parse_dmy_to_unix("00/01/2024"), None);
    assert_eq!(parse_dmy_to_unix("32/01/2024"), None);
    assert_eq!(parse_dmy_to_unix("01/00/2024"), None);
    assert_eq!(parse_dmy_to_unix("01/13/2024"), None);
    assert_eq!(parse_dmy_to_unix("01/01/1969"), None);
    assert_eq!(parse_dmy_to_unix("aa/01/2024"), None);
    assert!(parse_dmy_to_unix("29/02/2000").is_some()); // divisible by 400 -> leap
    assert!(parse_dmy_to_unix("01/03/2023").is_some());

    // 2. combined_survey_catalog adds new custom survey and overrides existing SDE survey if present
    let sde_first_id = get_survey_catalog(&ctx.sde).first().map(|s| s.survey_id);
    let override_id = sde_first_id.unwrap_or(9_901);
    let custom_list = vec![
        SurveyDefinition {
            survey_id: 99_500,
            name: "Custom Survey".into(),
            description: "Custom".into(),
            start_date: None,
            end_date: None,
            start_unix: Some(1_799_000_000),
            expires_at_unix: Some(1_801_000_000),
            grace_period_days: 0,
            min_account_age_hours: 24,
            item_rewards: vec![(34, 5)],
            vgs_rewards: vec![101],
        },
        SurveyDefinition {
            survey_id: override_id,
            name: "Overridden".into(),
            description: "Overridden".into(),
            start_date: None,
            end_date: None,
            start_unix: None,
            expires_at_unix: None,
            grace_period_days: 0,
            min_account_age_hours: 0,
            item_rewards: vec![],
            vgs_rewards: vec![],
        },
    ];
    let combined = combined_survey_catalog(&ctx.sde, &custom_list);
    assert!(combined.iter().any(|s| s.survey_id == 99_500));
    assert_eq!(
        combined.iter().find(|s| s.survey_id == override_id).unwrap().name,
        "Overridden"
    );

    // 3. eligible_surveys: completed, claimed, not started, expired, account too young
    let mut rec = CharacterOperationsRecord::default();
    rec.completed_surveys.insert(1);
    rec.claimed_surveys.insert(2);
    let cat = vec![
        SurveyDefinition { survey_id: 1, name: "".into(), description: "".into(), start_date: None, end_date: None, start_unix: None, expires_at_unix: None, grace_period_days: 0, min_account_age_hours: 0, item_rewards: vec![], vgs_rewards: vec![] },
        SurveyDefinition { survey_id: 2, name: "".into(), description: "".into(), start_date: None, end_date: None, start_unix: None, expires_at_unix: None, grace_period_days: 0, min_account_age_hours: 0, item_rewards: vec![], vgs_rewards: vec![] },
        SurveyDefinition { survey_id: 3, name: "".into(), description: "".into(), start_date: None, end_date: None, start_unix: Some(2_000_000_000), expires_at_unix: None, grace_period_days: 0, min_account_age_hours: 0, item_rewards: vec![], vgs_rewards: vec![] },
        SurveyDefinition { survey_id: 4, name: "".into(), description: "".into(), start_date: None, end_date: None, start_unix: None, expires_at_unix: Some(1_700_000_000), grace_period_days: 0, min_account_age_hours: 0, item_rewards: vec![], vgs_rewards: vec![] },
        SurveyDefinition { survey_id: 5, name: "".into(), description: "".into(), start_date: None, end_date: None, start_unix: None, expires_at_unix: None, grace_period_days: 0, min_account_age_hours: 48, item_rewards: vec![], vgs_rewards: vec![] },
        SurveyDefinition { survey_id: 6, name: "".into(), description: "".into(), start_date: None, end_date: None, start_unix: Some(1_799_000_000), expires_at_unix: Some(1_801_000_000), grace_period_days: 0, min_account_age_hours: 1, item_rewards: vec![], vgs_rewards: vec![] },
    ];
    let elig = eligible_surveys(&rec, 1_799_990_000, &cat, 1_800_000_000);
    assert_eq!(elig, vec![6]);

    // 4. build_scheduled_survey_from_slash error paths & '-' placeholders
    assert!(build_scheduled_survey_from_slash(&ctx.sde, 1_800_000_000, &[]).is_err());
    assert!(build_scheduled_survey_from_slash(&ctx.sde, 1_800_000_000, &["0"]).is_err());
    assert!(build_scheduled_survey_from_slash(&ctx.sde, 1_800_000_000, &["9100", "0"]).is_err());
    assert!(build_scheduled_survey_from_slash(&ctx.sde, 1_800_000_000, &["9100", "abc"]).is_err());
    assert!(build_scheduled_survey_from_slash(&ctx.sde, 1_800_000_000, &["9100", "3600", "badpair"]).is_err());
    assert!(build_scheduled_survey_from_slash(&ctx.sde, 1_800_000_000, &["9100", "3600", "999999999:1"]).is_err());
    assert!(build_scheduled_survey_from_slash(&ctx.sde, 1_800_000_000, &["9100", "3600", "34:0"]).is_err());
    assert!(build_scheduled_survey_from_slash(&ctx.sde, 1_800_000_000, &["9100", "3600", "-", "0"]).is_err());
    let ok_default = build_scheduled_survey_from_slash(&ctx.sde, 1_800_000_000, &["9100"]).unwrap();
    assert_eq!(ok_default.expires_at_unix, Some(1_800_086_400));

    // 5. register_survey inserts and then updates existing survey
    register_survey(&ctx, ok_default.clone());
    let mut updated = ok_default;
    updated.name = "Updated Name".into();
    register_survey(&ctx, updated);
    ctx.db.with_snapshot(|snap| {
        let st = load_store_pub(snap);
        assert_eq!(
            st.custom_surveys.iter().find(|s| s.survey_id == 9100).unwrap().name,
            "Updated Name"
        );
    });

    // 6. handle_survey_slash non-matching, invalid subcommand, bad schedule, complete without active
    assert!(handle_survey_slash(&ctx, char_id, "/help").is_none());
    assert_eq!(
        handle_survey_slash(&ctx, char_id, "/survey invalid").unwrap().unwrap_err().code,
        "InvalidSlashArgument"
    );
    assert_eq!(
        handle_survey_slash(&ctx, char_id, "/survey schedule 0").unwrap().unwrap_err().code,
        "InvalidSlashArgument"
    );
    assert_eq!(
        handle_survey_slash(&ctx, char_id, "/survey complete").unwrap().unwrap_err().code,
        "NoActiveSurvey"
    );
    let act = handle_survey_slash(&ctx, char_id, "/survey activate 9101 7200 34:1 777").unwrap().unwrap();
    assert_eq!(act.dict_get("surveyID"), Some(&PyRep::Integer(9101)));
}

#[test]
fn survey_completion_checks_and_claim_space_and_error_paths() {
    let (ctx, mut session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000));
    ctx.set_clock(clock.clone());
    session.station_id = None;
    session.structure_id = None;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.create_date_unix = 1_000;
            c.station_id = None;
            c.structure_id = None;
        }
    });

    // Unauthenticated checks & claim
    let unauth = SessionState::default();
    assert_eq!(handle_survey_perform_checks(&ctx, &unauth).unwrap_err().code, "Unauthorized");
    assert_eq!(handle_survey_claim_rewards(&ctx, &unauth, &[]).unwrap_err().code, "Unauthorized");

    // Missing character in snap for mark_survey_completed and claim
    assert_eq!(mark_survey_completed(&ctx, 999_888, 9200).unwrap_err().code, "CharacterNotFound");
    let mut ghost_sess = session.clone();
    ghost_sess.character_id = Some(999_888);
    assert_eq!(handle_survey_claim_rewards(&ctx, &ghost_sess, &[]).unwrap_err().code, "CharacterNotFound");

    // InvalidSurveyID, NoActiveSurvey
    assert_eq!(mark_survey_completed(&ctx, char_id, 999_777).unwrap_err().code, "InvalidSurveyID");
    assert_eq!(handle_survey_claim_rewards(&ctx, &session, &[]).unwrap_err().code, "NoActiveSurvey");

    // Expired & future surveys
    register_survey(
        &ctx,
        SurveyDefinition {
            survey_id: 9201,
            name: "Expired".into(),
            description: "".into(),
            start_date: None,
            end_date: None,
            start_unix: Some(1_700_000_000),
            expires_at_unix: Some(1_750_000_000),
            grace_period_days: 0,
            min_account_age_hours: 0,
            item_rewards: vec![(34, 1)],
            vgs_rewards: vec![],
        },
    );
    register_survey(
        &ctx,
        SurveyDefinition {
            survey_id: 9202,
            name: "Future".into(),
            description: "".into(),
            start_date: None,
            end_date: None,
            start_unix: Some(1_900_000_000),
            expires_at_unix: Some(1_950_000_000),
            grace_period_days: 0,
            min_account_age_hours: 0,
            item_rewards: vec![(34, 1)],
            vgs_rewards: vec![],
        },
    );
    assert_eq!(mark_survey_completed(&ctx, char_id, 9201).unwrap_err().code, "SurveyExpired");
    assert_eq!(mark_survey_completed(&ctx, char_id, 9202).unwrap_err().code, "SurveyNotActive");
    assert_eq!(
        handle_survey_claim_rewards(&ctx, &session, &[PyRep::Integer(999_777)]).unwrap_err().code,
        "InvalidSurveyID"
    );
    assert_eq!(
        handle_survey_claim_rewards(&ctx, &session, &[PyRep::Integer(9201)]).unwrap_err().code,
        "SurveyExpired"
    );
    assert_eq!(
        handle_survey_claim_rewards(&ctx, &session, &[PyRep::Integer(9202)]).unwrap_err().code,
        "SurveyNotActive"
    );

    // Active survey 9203 with (34, 0) zero-amount + (34, 1) singleton + (35, 5) stack + VGS reward
    register_survey(
        &ctx,
        SurveyDefinition {
            survey_id: 9203,
            name: "SpaceClaim".into(),
            description: "".into(),
            start_date: None,
            end_date: None,
            start_unix: Some(1_799_000_000),
            expires_at_unix: Some(1_801_000_000),
            grace_period_days: 0,
            min_account_age_hours: 0,
            item_rewards: vec![(34, 0), (34, 1), (35, 5)],
            vgs_rewards: vec![801, 801],
        },
    );

    // Set stale active_survey_id and run handle_survey_perform_checks -> resets to 9203
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_store_pub(snap);
        st.by_character.entry(char_id).or_default().active_survey_id = Some(999_111);
        save_store_pub(snap, &st, 1_800_000_000);
    });
    let checks = handle_survey_perform_checks(&ctx, &session).unwrap();
    if let PyRep::List(ids) = checks {
        assert!(ids.contains(&PyRep::Integer(9203)));
    } else {
        panic!("expected list");
    }

    // Implicit claim (uses active_survey_id) fails with SurveyNotCompleted
    assert_eq!(
        handle_survey_claim_rewards(&ctx, &session, &[]).unwrap_err().code,
        "SurveyNotCompleted"
    );

    // Complete via /survey complete (uses active_survey_id 9203)
    let slash_comp = handle_survey_slash(&ctx, char_id, "/survey complete").unwrap().unwrap();
    assert_eq!(slash_comp.dict_get("surveyID"), Some(&PyRep::Integer(9203)));
    // Error propagation from /survey complete with invalid id
    assert_eq!(
        handle_survey_slash(&ctx, char_id, "/survey complete 999777").unwrap().unwrap_err().code,
        "InvalidSurveyID"
    );

    // Claim in space (no station_id/structure_id) -> enqueues redeem tokens instead of hangar items!
    let claim_ok = handle_survey_claim_rewards(&ctx, &session, &[]).unwrap();
    assert_eq!(claim_ok, PyRep::Bool(true));

    // Re-marking already claimed survey fails with SurveyAlreadyClaimed
    assert_eq!(
        mark_survey_completed(&ctx, char_id, 9203).unwrap_err().code,
        "SurveyAlreadyClaimed"
    );

    // Verify unclaimed completed survey fallback when active_survey_id is None
    register_survey(
        &ctx,
        SurveyDefinition {
            survey_id: 9204,
            name: "DockedClaim".into(),
            description: "".into(),
            start_date: None,
            end_date: None,
            start_unix: Some(1_799_000_000),
            expires_at_unix: Some(1_801_000_000),
            grace_period_days: 0,
            min_account_age_hours: 0,
            item_rewards: vec![(34, 1)],
            vgs_rewards: vec![],
        },
    );
    mark_survey_completed(&ctx, char_id, 9204).unwrap();
    session.station_id = Some(60_003_760);
    assert_eq!(handle_survey_claim_rewards(&ctx, &session, &[]).unwrap(), PyRep::Bool(true));
}
