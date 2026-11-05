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
