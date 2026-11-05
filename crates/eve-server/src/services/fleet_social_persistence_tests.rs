use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use eve_db::{CorpVoteBallotRecord, CorpVoteCaseRecord};

#[test]
fn label_masks_contact_sync_and_hydration() {
    assert_eq!(label_mask_from_ids(&[1, 4, 16]), 21);
    assert_eq!(label_ids_from_mask(21), vec![1, 4, 16]);
    assert_eq!(label_mask_from_ids(&[]), 0);
    assert!(label_ids_from_mask(0).is_empty());

    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let char_id = 100_000_001;
    let target_id = 100_000_002;

    ctx.esi_state.char_contacts.write().unwrap().insert(
        char_id,
        vec![
            EsiContactEntry {
                contact_id: target_id,
                contact_type: "character".into(),
                standing: 5.0,
                is_watched: true,
                is_blocked: false,
                label_ids: vec![1, 2],
            },
            EsiContactEntry {
                contact_id: 100_000_003,
                contact_type: "character".into(),
                standing: -10.0,
                is_watched: false,
                is_blocked: true,
                label_ids: Vec::new(),
            },
        ],
    );
    sync_char_contacts_to_db(&ctx, char_id).unwrap();
    assert!(ctx.social_extras.lock().unwrap().contacts.get(&char_id).unwrap().contains_key(&target_id));
    assert!(ctx.social_extras.lock().unwrap().blocked.get(&char_id).unwrap().contains(&100_000_003));

    assert!(!record_contact_notification_entry(&ctx, char_id, char_id, 5.0, "self".into()).unwrap());
    assert!(record_contact_notification_entry(&ctx, char_id, target_id, 5.0, "hi".into()).unwrap());

    let mut kw = HashMap::new();
    kw.insert("notify".to_string(), PyRep::Bool(true));
    kw.insert("message".to_string(), PyRep::String("hello friend".into()));
    record_contact_notification_if_requested(&ctx, char_id, target_id, 10.0, true, &[], &kw).unwrap();

    persist_social_state(&ctx);
    hydrate_social_state_from_db(&ctx);
}

#[test]
fn vote_evaluation_and_vote_queries() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let corp_id = 1_000_009;
    let case_id = 501;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_vote_cases.insert(
            case_id,
            CorpVoteCaseRecord {
                case_id,
                corporation_id: corp_id,
                vote_type: 4,
                parameter: 100_000_002,
                text: "vote test".into(),
                description: "desc".into(),
                start_ts: 900,
                end_ts: 2000,
                options: vec![
                    (1, "Yes".into(), 100_000_002),
                    (2, "No".into(), 0),
                ],
                executed: false,
            },
        );
        snap.corp_vote_ballots.insert(
            (case_id, 100_000_001),
            CorpVoteBallotRecord {
                case_id,
                voter_id: 100_000_001,
                option_id: 1,
                weight: 100,
                voted_at_unix: 1000,
            },
        );
    });

    let votes = handle_vote_queries(&ctx, corp_id, 1000, "GetVotes", &[PyRep::Integer(case_id)])
        .unwrap()
        .unwrap();
    assert!(!matches!(votes, PyRep::None));

    let opts = handle_vote_queries(&ctx, corp_id, 1000, "GetVoteCaseOptions", &[PyRep::Integer(case_id)])
        .unwrap()
        .unwrap();
    assert!(!matches!(opts, PyRep::None));

    let single = handle_vote_queries(&ctx, corp_id, 1000, "GetVoteCase", &[PyRep::Integer(case_id)])
        .unwrap()
        .unwrap();
    assert!(!matches!(single, PyRep::None));

    let list_open = handle_vote_queries(&ctx, corp_id, 1000, "GetVoteCasesByCorporation", &[PyRep::Integer(corp_id), PyRep::Integer(2)])
        .unwrap()
        .unwrap();
    assert!(!matches!(list_open, PyRep::None));

    let sanctioned = handle_vote_queries(&ctx, corp_id, 2500, "GetCorpSanctionedActions", &[PyRep::Integer(corp_id)])
        .unwrap()
        .unwrap();
    assert!(!matches!(sanctioned, PyRep::None));
}
