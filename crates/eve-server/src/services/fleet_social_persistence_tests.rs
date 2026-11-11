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

#[test]
fn label_mask_bit_boundaries_contact_notif_merge_exclusions_and_calendar_tombstones() {
    use crate::esi::runtime_state::EsiContactNotification;
    use eve_db::{
        CalendarEventRecord, FleetRecord, MailingListRecord, SocialContactNotificationRecord,
        SocialContactRecord,
    };

    // Bit-mask boundaries: bits 0..63 supported, bit 63 (1u64 << 63) excluded
    assert_eq!(label_mask_from_ids(&[1, 2, 1 << 62]), (1i64 | 2i64 | (1i64 << 62)));
    assert_eq!(label_ids_from_mask((1u64 << 62) | 1), vec![1, 1i64 << 62]);
    assert!(label_ids_from_mask(1u64 << 63).is_empty());

    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let sender = 90_000_001_i64;

    // Exact boundary exclusions in record_contact_notification_entry
    assert!(!record_contact_notification_entry(&ctx, sender, 0, 5.0, "zero".into()).unwrap());
    assert!(!record_contact_notification_entry(&ctx, sender, -5, 5.0, "neg".into()).unwrap());
    assert!(!record_contact_notification_entry(&ctx, sender, 500_000, 5.0, "npc_lo".into()).unwrap());
    assert!(!record_contact_notification_entry(&ctx, sender, 1_999_999, 5.0, "npc_hi".into()).unwrap());
    assert!(!record_contact_notification_entry(&ctx, sender, 98_000_000, 5.0, "corp_lo".into()).unwrap());
    assert!(!record_contact_notification_entry(&ctx, sender, 99_999_999, 5.0, "ally_hi".into()).unwrap());
    if !ctx.sde.agents.contains_key(&499_999) {
        assert!(record_contact_notification_entry(&ctx, sender, 499_999, 5.0, "below_500k".into()).unwrap());
    }
    if !ctx.sde.agents.contains_key(&2_000_000) {
        assert!(record_contact_notification_entry(&ctx, sender, 2_000_000, 5.0, "at_2m".into()).unwrap());
    }
    if !ctx.sde.agents.contains_key(&97_999_999) {
        assert!(record_contact_notification_entry(&ctx, sender, 97_999_999, 5.0, "below_98m".into()).unwrap());
    }
    if !ctx.sde.agents.contains_key(&100_000_000) {
        assert!(record_contact_notification_entry(&ctx, sender, 100_000_000, 5.0, "at_100m".into()).unwrap());
    }
    if let Some(&agent_id) = ctx.sde.agents.keys().next() {
        assert!(!record_contact_notification_entry(&ctx, sender, agent_id, 5.0, "agent".into()).unwrap());
    }

    // load_contact_notifications_for_char: runtime overrides duplicate DB id, sorts descending, truncates at 50
    let merge_target = 100_000_055_i64;
    let db_rows: Vec<SocialContactNotificationRecord> = (1..=30)
        .map(|i| SocialContactNotificationRecord {
            notification_id: i * 2,
            sender_character_id: sender,
            send_date_unix: 1_700_000_000 + i as u64,
            standing_level: 5.0,
            message: format!("db_{i}"),
        })
        .collect();
    ctx.db.with_mut_snapshot(|snap| {
        snap.char_contact_notifications.insert(merge_target, db_rows);
    });
    {
        let mut map = ctx.esi_state.contact_notifications.write().unwrap();
        let mem_rows = map.entry(merge_target).or_default();
        // Overlap on ID 60 (runtime must overwrite db_30), plus odd IDs up to 119 -> total unique > 50
        mem_rows.push(EsiContactNotification {
            notification_id: 60,
            sender_character_id: sender,
            send_date_unix: 1_700_000_999,
            standing_level: 10.0,
            message: "mem_override_60".into(),
        });
        for i in 1..=30 {
            mem_rows.push(EsiContactNotification {
                notification_id: i * 2 + 1,
                sender_character_id: sender,
                send_date_unix: 1_700_000_500 + i as u64,
                standing_level: 5.0,
                message: format!("mem_{i}"),
            });
        }
    }
    let merged = ctx
        .db
        .with_snapshot(|s| load_contact_notifications_for_char(&ctx, s, merge_target));
    assert_eq!(merged.len(), MAX_CONTACT_NOTIFICATIONS_PER_CHAR);
    assert!(merged.windows(2).all(|w| w[0].notification_id > w[1].notification_id));
    let n60 = merged.iter().find(|n| n.notification_id == 60).unwrap();
    assert_eq!(n60.message, "mem_override_60");

    // Sticky calendar tombstone: DB is_deleted=true must win over stale social_extras is_deleted=false
    ctx.db.with_mut_snapshot(|snap| {
        snap.calendar_events.insert(
            880_001,
            CalendarEventRecord {
                event_id: 880_001,
                owner_id: sender,
                creator_id: sender,
                event_date_time: 1_700_500_000,
                duration: 45,
                importance: 1,
                title: "TombstoneEvent".into(),
                description: "Should stay deleted".into(),
                scope: "Personal".into(),
                responses: HashMap::new(),
                is_deleted: true,
            },
        );
        snap.mailing_lists.insert(
            990_001,
            MailingListRecord {
                list_id: 990_001,
                name: "HydratedList".into(),
                owner_id: sender,
                default_access: 1,
                default_role: 2,
                cost: 0,
                members: HashMap::new(),
                entity_access: HashMap::new(),
                welcome_mail: None,
                mail_ids: HashSet::new(),
            },
        );
        snap.char_contacts.insert(
            sender,
            vec![SocialContactRecord {
                contact_id: merge_target,
                contact_type: "character".into(),
                standing: 10.0,
                is_watched: true,
                is_blocked: false,
                label_ids: vec![1, 4],
            }],
        );
        snap.fleets.insert(
            501,
            FleetRecord {
                fleet_id: 501,
                boss_id: sender,
                members: vec![sender],
                wings: Vec::new(),
                squads: Vec::new(),
                free_move: false,
                is_registered: false,
                wing_details: HashMap::new(),
                squad_details: HashMap::new(),
                member_details: HashMap::new(),
                invites: HashSet::new(),
                motd: String::new(),
            },
        );
    });
    with_social_state(&ctx, |st| {
        st.calendar_events.insert(
            880_001,
            CalendarEventState {
                event_id: 880_001,
                owner_id: sender,
                creator_id: sender,
                event_date_time: 1_700_500_000,
                duration: 45,
                importance: 1,
                title: "StaleLive".into(),
                description: "Not deleted in mem".into(),
                _scope: "Personal".into(),
                responses: HashMap::new(),
                is_deleted: false,
            },
        );
        st.contacts.insert(999, HashMap::from([(888, (1.0, false, 0))]));
        st.mailing_lists.insert(
            123,
            MailingListState {
                list_id: 123,
                name: "StaleListToClear".into(),
                owner_id: sender,
                ..Default::default()
            },
        );
    });

    ensure_social_extras_mailing_lists_seeded(&ctx);
    with_social_state(&ctx, |st| {
        assert!(!st.mailing_lists.contains_key(&123));
        assert!(st.mailing_lists.contains_key(&990_001));
        assert!(st.next_id >= 990_001);
    });

    persist_social_state(&ctx);
    assert!(ctx.db.with_snapshot(|s| s.calendar_events[&880_001].is_deleted));

    ctx.db.with_mut_snapshot(|snap| {
        let sender_char = crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::clone_char_with(
            snap,
            sender,
            "SenderPersist",
            98_000_001,
            30_000_142,
            100_000_999,
        );
        snap.characters.insert(sender, sender_char);
        snap.accounts.insert(
            format!("acct_{sender}"),
            eve_db::AccountRecord {
                user_id: sender as u64,
                username: format!("acct_{sender}"),
                password_token: "pw".into(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.fleet_and_social_ops
            .fleet_join_requests
            .insert(501, HashSet::from([merge_target]));
        snap.fleet_and_social_ops
            .fleet_voice_enabled
            .insert(501, true);
        snap.fleet_and_social_ops
            .fleet_loot_logging
            .insert(501, true);
        snap.fleet_and_social_ops
            .fleet_watchlist
            .insert(sender, HashSet::from([merge_target]));
    });
    ctx.db
        .acquire_online_lock(sender as u64, 1, Some(sender), false, 1_700_000_000)
        .unwrap();
    hydrate_social_state_from_db(&ctx);
    with_social_state(&ctx, |st| {
        assert!(!st.contacts.contains_key(&999));
        assert_eq!(st.contacts[&sender][&merge_target], (10.0, true, 5));
        assert!(st.calendar_events[&880_001].is_deleted);
    });
    assert!(ctx.esi_state.char_contacts.read().unwrap()[&sender]
        .iter()
        .any(|e| e.contact_id == merge_target && e.label_ids == vec![1, 4]));
    {
        let fex = ctx.fleet_extras.lock().unwrap();
        assert!(fex.watchlist_loaded);
        assert!(fex.join_requests[&501].contains(&merge_target));
        assert!(fex.watchlist[&sender].contains(&merge_target));
        assert_eq!(fex.voice_enabled.get(&501), Some(&true));
        assert_eq!(fex.loot_logging.get(&501), Some(&true));
    }
}

#[test]
fn eval_vote_case_winner_ties_parameter_fallbacks_and_vote_query_boundaries() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let corp_id = 98_000_123_i64;
    let other_corp = 98_000_124_i64;

    // 1. Direct unit tests for eval_vote_case_winner
    let mut ballots: HashMap<(i64, i64), CorpVoteBallotRecord> = HashMap::new();
    let vc_two = CorpVoteCaseRecord {
        case_id: 1,
        corporation_id: corp_id,
        vote_type: 4,
        parameter: 777,
        text: "TwoOpt".into(),
        description: String::new(),
        start_ts: 100,
        end_ts: 200,
        options: vec![(1, "Yes".into(), 0), (2, "No".into(), 0)],
        executed: false,
    };
    // 0 votes -> false
    assert_eq!(eval_vote_case_winner(&vc_two, &ballots), (false, 777, 0));
    // Exact tie yes=50 vs no=50 -> false
    ballots.insert((1, 1), CorpVoteBallotRecord { case_id: 1, voter_id: 1, option_id: 1, weight: 50, voted_at_unix: 100 });
    ballots.insert((1, 2), CorpVoteBallotRecord { case_id: 1, voter_id: 2, option_id: 2, weight: 50, voted_at_unix: 100 });
    assert_eq!(eval_vote_case_winner(&vc_two, &ballots), (false, 777, 50));
    // yes=60 > no=50 with option 1 param == 0 -> falls back to vc.parameter (777)
    ballots.get_mut(&(1, 1)).unwrap().weight = 60;
    assert_eq!(eval_vote_case_winner(&vc_two, &ballots), (true, 777, 60));
    // Option 1 explicit non-zero param overrides vc.parameter
    let mut vc_two_override = vc_two.clone();
    vc_two_override.options[0].2 = 888;
    assert_eq!(eval_vote_case_winner(&vc_two_override, &ballots), (true, 888, 60));

    // >2 options: tie between two candidates -> false
    let vc_multi = CorpVoteCaseRecord {
        case_id: 2,
        corporation_id: corp_id,
        vote_type: 0,
        parameter: 0,
        text: "CEO".into(),
        description: String::new(),
        start_ts: 100,
        end_ts: 200,
        options: vec![
            (1, "Cand1".into(), 101),
            (2, "Cand2".into(), 102),
            (3, "Reject".into(), 0),
        ],
        executed: false,
    };
    ballots.insert((2, 1), CorpVoteBallotRecord { case_id: 2, voter_id: 1, option_id: 1, weight: 100, voted_at_unix: 100 });
    ballots.insert((2, 2), CorpVoteBallotRecord { case_id: 2, voter_id: 2, option_id: 2, weight: 100, voted_at_unix: 100 });
    assert_eq!(eval_vote_case_winner(&vc_multi, &ballots), (false, 101, 100));
    // Reject option (param == 0) outvoting top candidate (150 > 120) -> fails!
    ballots.get_mut(&(2, 1)).unwrap().weight = 120;
    ballots.insert((2, 3), CorpVoteBallotRecord { case_id: 2, voter_id: 3, option_id: 3, weight: 150, voted_at_unix: 100 });
    assert_eq!(eval_vote_case_winner(&vc_multi, &ballots), (false, 101, 120));
    // Unlisted option ID (oid=99) outvoting best_opt -> runner_up captures 99 -> fails!
    ballots.get_mut(&(2, 3)).unwrap().weight = 10;
    ballots.insert((2, 4), CorpVoteBallotRecord { case_id: 2, voter_id: 4, option_id: 99, weight: 200, voted_at_unix: 100 });
    assert_eq!(eval_vote_case_winner(&vc_multi, &ballots), (false, 101, 120));

    // 2. handle_vote_queries boundaries (`now_unix == end_ts` closed, sorting, max_len, sanctioned filters)
    let now = 1_700_000_000_u64;
    ctx.db.with_mut_snapshot(|snap| {
        // Case 10: boundary end_ts == now (must be closed, passed, unexecuted)
        snap.corp_vote_cases.insert(
            10,
            CorpVoteCaseRecord {
                case_id: 10,
                corporation_id: corp_id,
                vote_type: 4,
                parameter: 10,
                text: "BoundaryClosed".into(),
                description: String::new(),
                start_ts: 900,
                end_ts: now,
                options: Vec::new(),
                executed: false,
            },
        );
        // Case 11: same start_ts = 900 as case 10, closed & executed (sorts before case 10 due to case_id desc)
        snap.corp_vote_cases.insert(
            11,
            CorpVoteCaseRecord {
                case_id: 11,
                corporation_id: corp_id,
                vote_type: 4,
                parameter: 11,
                text: "Executed".into(),
                description: String::new(),
                start_ts: 900,
                end_ts: now - 10,
                options: Vec::new(),
                executed: true,
            },
        );
        // Case 12: open (end_ts == now + 1), passing
        snap.corp_vote_cases.insert(
            12,
            CorpVoteCaseRecord {
                case_id: 12,
                corporation_id: corp_id,
                vote_type: 4,
                parameter: 12,
                text: "StillOpen".into(),
                description: String::new(),
                start_ts: 950,
                end_ts: now + 1,
                options: Vec::new(),
                executed: false,
            },
        );
        // Case 13: closed & failed (option 2 wins)
        snap.corp_vote_cases.insert(
            13,
            CorpVoteCaseRecord {
                case_id: 13,
                corporation_id: corp_id,
                vote_type: 4,
                parameter: 13,
                text: "FailedClosed".into(),
                description: String::new(),
                start_ts: 960,
                end_ts: now - 5,
                options: Vec::new(),
                executed: false,
            },
        );
        // Case 14: belongs to other_corp (must never leak into corp_id results)
        snap.corp_vote_cases.insert(
            14,
            CorpVoteCaseRecord {
                case_id: 14,
                corporation_id: other_corp,
                vote_type: 4,
                parameter: 14,
                text: "OtherCorp".into(),
                description: String::new(),
                start_ts: 999,
                end_ts: now - 1,
                options: Vec::new(),
                executed: false,
            },
        );
        for cid in [10, 11, 12, 14] {
            snap.corp_vote_ballots.insert(
                (cid, 90_000_001),
                CorpVoteBallotRecord {
                    case_id: cid,
                    voter_id: 90_000_001,
                    option_id: 1,
                    weight: 100,
                    voted_at_unix: 900,
                },
            );
        }
        snap.corp_vote_ballots.insert(
            (13, 90_000_001),
            CorpVoteBallotRecord {
                case_id: 13,
                voter_id: 90_000_001,
                option_id: 2,
                weight: 100,
                voted_at_unix: 900,
            },
        );
    });

    // GetVotes 2-arg [corp_id, case_id] returns rows for case 10
    let votes_2arg = handle_vote_queries(&ctx, corp_id, now, "GetVotes", &[PyRep::Integer(corp_id), PyRep::Integer(10)])
        .unwrap()
        .unwrap();
    if let PyRep::Rowset { rows, .. } = votes_2arg {
        assert_eq!(rows.len(), 1);
    } else {
        panic!("expected Rowset");
    }

    // Wrong corp fails VoteCaseNotFound for GetVoteCase and GetVoteCaseOptions
    assert_eq!(
        handle_vote_queries(&ctx, corp_id, now, "GetVoteCase", &[PyRep::Integer(corp_id), PyRep::Integer(14)])
            .unwrap()
            .unwrap_err()
            .code,
        "VoteCaseNotFound"
    );
    assert_eq!(
        handle_vote_queries(&ctx, corp_id, now, "GetVoteCaseOptions", &[PyRep::Integer(14)])
            .unwrap()
            .unwrap_err()
            .code,
        "VoteCaseNotFound"
    );

    // GetVoteCase 1-arg exact boundary: now == end_ts -> status == 1 (closed), now < end_ts -> status == 2 (open)
    let vc_closed = handle_vote_queries(&ctx, corp_id, now, "GetVoteCase", &[PyRep::Integer(10)])
        .unwrap()
        .unwrap();
    assert_eq!(vc_closed.dbrow_get("status"), Some(&PyRep::Integer(1)));
    let vc_open = handle_vote_queries(&ctx, corp_id, now, "GetVoteCase", &[PyRep::Integer(12)])
        .unwrap()
        .unwrap();
    assert_eq!(vc_open.dbrow_get("status"), Some(&PyRep::Integer(2)));

    // GetVoteCasesByCorporation: open only case 12, closed [13, 11, 10] sorted start_ts desc then case_id desc
    let grouped = handle_vote_queries(&ctx, corp_id, now, "GetVoteCasesByCorporation", &[PyRep::Integer(corp_id)])
        .unwrap()
        .unwrap();
    let PyRep::Dict(d) = grouped else { panic!() };
    let open_rows = d.iter().find(|(k, _)| k == &PyRep::Integer(2)).unwrap().1.crowset_rows().unwrap();
    let closed_rows = d.iter().find(|(k, _)| k == &PyRep::Integer(1)).unwrap().1.crowset_rows().unwrap();
    assert_eq!(open_rows.len(), 1);
    let closed_ids: Vec<i64> = closed_rows
        .iter()
        .filter_map(|r| r.dbrow_get("voteCaseID").and_then(|v| v.as_i64()))
        .collect();
    assert_eq!(closed_ids, vec![13, 11, 10]);

    // GetCorpSanctionedActions exact filter sets
    let extract_ids = |rep: PyRep| -> Vec<i64> {
        let PyRep::Rowset { rows, .. } = rep else { panic!() };
        rows.into_iter().filter_map(|r| r.first().and_then(|v| v.as_i64())).collect()
    };
    let pending_ids = extract_ids(
        handle_vote_queries(&ctx, corp_id, now, "GetCorpSanctionedActions", &[PyRep::Integer(corp_id)])
            .unwrap()
            .unwrap(),
    );
    assert_eq!(pending_ids, vec![10]);
    let exec_ids = extract_ids(
        handle_vote_queries(
            &ctx,
            corp_id,
            now,
            "GetCorpSanctionedActions",
            &[PyRep::Integer(corp_id), PyRep::String("active".into())],
        )
        .unwrap()
        .unwrap(),
    );
    assert_eq!(exec_ids, vec![11]);
    let all_ids = extract_ids(
        handle_vote_queries(
            &ctx,
            corp_id,
            now,
            "GetCorpSanctionedActions",
            &[PyRep::Integer(corp_id), PyRep::Integer(0)],
        )
        .unwrap()
        .unwrap(),
    );
    assert_eq!(all_ids, vec![11, 10]);
    let limited_ids = extract_ids(
        handle_vote_queries(
            &ctx,
            corp_id,
            now,
            "GetCorpSanctionedActions",
            &[PyRep::Integer(corp_id), PyRep::String("all".into()), PyRep::Integer(1)],
        )
        .unwrap()
        .unwrap(),
    );
    assert_eq!(limited_ids, vec![11]);
}
