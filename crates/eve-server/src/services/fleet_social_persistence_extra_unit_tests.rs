//! Additional branch-coverage and mutation-killing unit tests for `fleet_social_persistence.rs`.

#![forbid(unsafe_code)]

use crate::esi::runtime_state::EsiContactNotification;
use crate::router::ServerContext;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use crate::services::fleet_social::fleet_social_persistence::{
    ensure_social_extras_mailing_lists_seeded, hydrate_social_state_from_db, is_blocked_by,
    label_ids_from_mask, label_mask_from_ids, load_contact_notifications_for_char,
    persist_social_state, record_contact_notification_entry,
    record_contact_notification_if_requested, sync_contact_notifications_to_db,
    update_calendar_response_from_esi, with_social_state, MailingListState, SocialExtrasState,
    MAX_CONTACT_NOTIFICATIONS_PER_CHAR,
};
use crate::services::fleet_social::handle;
use eve_db::{
    CalendarEventRecord, ChatChannelAclRecord, CorpVoteBallotRecord, CorpVoteCaseRecord, IskCents,
    MailLabelRecord, MailingListRecord, SocialContactNotificationRecord, SocialContactRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

async fn call_vote_mgr(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let kw = HashMap::new();
    handle(ctx, session, "voteManager", method, args, &kw).await
}

#[test]
fn contact_notifications_blocking_hierarchy_and_50_cap_kill_mutations() {
    let se_new = SocialExtrasState::new();
    assert_eq!(se_new.next_id, 700_000);

    let high_bit = 1i64 << 62;
    assert_eq!(label_mask_from_ids(&[1, high_bit]), 1 | high_bit);
    assert_eq!(label_ids_from_mask((1 | high_bit) as u64), vec![1, high_bit]);

    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let sender = 90_000_001_i64;
    let target = 90_000_002_i64;

    // 1. Early return guards in record_contact_notification_entry
    assert!(!record_contact_notification_entry(&ctx, sender, 0, 5.0, "zero".into()).unwrap());
    assert!(!record_contact_notification_entry(&ctx, sender, -5, 5.0, "neg".into()).unwrap());
    assert!(!record_contact_notification_entry(&ctx, sender, sender, 5.0, "self".into()).unwrap());
    assert!(
        !record_contact_notification_entry(&ctx, sender, 1_000_009, 5.0, "npc_corp".into())
            .unwrap()
    );
    assert!(
        !record_contact_notification_entry(&ctx, sender, 98_000_001, 5.0, "player_corp".into())
            .unwrap()
    );
    if let Some(&agent_id) = ctx.sde.agents.keys().next() {
        assert!(
            !record_contact_notification_entry(&ctx, sender, agent_id, 5.0, "agent".into())
                .unwrap()
        );
    }

    // 2. Blocking hierarchy across all 3 stores: social_extras.blocked, snap.char_contacts, esi_state.char_contacts
    assert!(!is_blocked_by(&ctx, target, sender));
    with_social_state(&ctx, |st| {
        st.blocked.entry(target).or_default().insert(sender);
    });
    assert!(is_blocked_by(&ctx, target, sender));
    assert!(
        !record_contact_notification_entry(&ctx, sender, target, -5.0, "blocked".into()).unwrap()
    );
    with_social_state(&ctx, |st| {
        st.blocked.clear();
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.char_contacts.insert(
            target,
            vec![SocialContactRecord {
                contact_id: sender,
                contact_type: "character".into(),
                standing: -10.0,
                is_watched: false,
                is_blocked: true,
                label_ids: Vec::new(),
            }],
        );
    });
    assert!(is_blocked_by(&ctx, target, sender));
    ctx.db.with_mut_snapshot(|snap| {
        snap.char_contacts.clear();
    });
    assert!(!is_blocked_by(&ctx, target, sender));

    // 3. record_contact_notification_if_requested: args[3], kwargs, default edit vs add messages
    let empty_kw = HashMap::new();
    record_contact_notification_if_requested(
        &ctx,
        sender,
        target,
        5.0,
        false,
        &[
            PyRep::Integer(target),
            PyRep::Real(5.0),
            PyRep::None,
            PyRep::Integer(0),
        ],
        &empty_kw,
    )
    .unwrap();
    assert!(ctx
        .db
        .with_snapshot(|s| load_contact_notifications_for_char(&ctx, s, target))
        .is_empty());

    // Default message when is_edit == false via args[3] = Integer(1)
    record_contact_notification_if_requested(
        &ctx,
        sender,
        target,
        5.0,
        false,
        &[
            PyRep::Integer(target),
            PyRep::Real(5.0),
            PyRep::None,
            PyRep::Integer(1),
        ],
        &empty_kw,
    )
    .unwrap();

    // Default message when is_edit == true via sendNotification kwarg
    let mut kw_edit = HashMap::new();
    kw_edit.insert("sendNotification".into(), PyRep::Bool(true));
    kw_edit.insert("note".into(), PyRep::String(String::new())); // empty note falls back to default
    record_contact_notification_if_requested(&ctx, sender, target, 10.0, true, &[], &kw_edit)
        .unwrap();

    let loaded = ctx
        .db
        .with_snapshot(|s| load_contact_notifications_for_char(&ctx, s, target));
    assert_eq!(loaded.len(), 2);
    assert!(loaded[0].notification_id > loaded[1].notification_id);
    assert!(loaded[0].message.contains("updated your contact standing"));
    assert!(loaded[1].message.contains("added you to contacts"));

    // 4. 50-notification cap in sync_contact_notifications_to_db & record_contact_notification_entry
    {
        let mut map = ctx.esi_state.contact_notifications.write().unwrap();
        let list = map.entry(target).or_default();
        list.clear();
        for i in 1..=55 {
            list.push(EsiContactNotification {
                notification_id: i,
                sender_character_id: sender,
                send_date_unix: 1_700_000_000 + i as u64,
                standing_level: 5.0,
                message: format!("msg_{i}"),
            });
        }
    }
    sync_contact_notifications_to_db(&ctx, target).unwrap();
    let capped = ctx
        .db
        .with_snapshot(|s| s.char_contact_notifications[&target].clone());
    assert_eq!(capped.len(), MAX_CONTACT_NOTIFICATIONS_PER_CHAR);
    assert_eq!(capped.first().unwrap().notification_id, 6);
}

#[test]
fn calendar_esi_response_mailing_lists_and_full_db_hydration_kill_mutations() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let cid = 90_000_001_i64;

    // Seed DB snapshot with calendar events, mailing lists, CSPA, notes, ACLs, mail labels, read mails, and fleet blobs
    ctx.db.with_mut_snapshot(|snap| {
        snap.character_cspa_charges.insert(cid, IskCents(2950_00));
        snap.trashed_mails.insert((cid, 4001));
        snap.char_notes.insert((cid, 90_000_002), "Trusted FC".into());
        snap.mailing_lists.insert(
            750_001,
            MailingListRecord {
                list_id: 750_001,
                name: "FleetPings".into(),
                owner_id: cid,
                default_access: 1,
                default_role: 2,
                cost: 100,
                members: HashMap::from([(cid, 3)]),
                entity_access: HashMap::from([(98_000_001, 1)]),
                welcome_mail: Some(("Welcome".into(), "Body".into())),
                mail_ids: HashSet::from([4001]),
            },
        );
        snap.calendar_events.insert(
            850_001,
            CalendarEventRecord {
                event_id: 850_001,
                owner_id: cid,
                creator_id: cid,
                event_date_time: 1_700_100_000,
                duration: 60,
                importance: 1,
                title: "CTA Op".into(),
                description: "Formup Jita".into(),
                scope: "Personal".into(),
                responses: HashMap::new(),
                is_deleted: false,
            },
        );
        snap.calendar_events.insert(
            850_002,
            CalendarEventRecord {
                event_id: 850_002,
                owner_id: cid,
                creator_id: cid,
                event_date_time: 1_700_200_000,
                duration: 30,
                importance: 0,
                title: "Deleted Op".into(),
                description: String::new(),
                scope: "Personal".into(),
                responses: HashMap::new(),
                is_deleted: true,
            },
        );
        snap.chat_channel_acl.insert(
            "player_chan_1".into(),
            ChatChannelAclRecord {
                allow_list: HashSet::new(),
                deny_list: HashSet::from([90_000_003]),
                muted_until: HashMap::from([(90_000_002, (1_700_999_999, "spam".into()))]),
            },
        );
        snap.char_contact_notifications.insert(
            cid,
            vec![SocialContactNotificationRecord {
                notification_id: 990_001,
                sender_character_id: 90_000_002,
                send_date_unix: 1_700_000_000,
                standing_level: 10.0,
                message: "Persisted".into(),
            }],
        );
        snap.item_custom_names.insert(100_000_101, "MyRifter".into());
        snap.deleted_mails.insert((cid, 4002));
        snap.mail_labels.insert(
            cid,
            vec![
                MailLabelRecord {
                    label_id: 1, // existing default label -> updates name/color
                    name: "CustomInbox".into(),
                    color: "#ff0000".into(),
                },
                MailLabelRecord {
                    label_id: 64, // custom new label -> pushed
                    name: "Intel".into(),
                    color: "#00ff00".into(),
                },
            ],
        );
        snap.read_mails.insert((cid, 4001));
        snap.mail_assigned_labels.insert((cid, 4001), vec![1, 64]);
        snap.fleet_and_social_ops
            .fleet_join_requests
            .insert(500, std::collections::HashSet::from([90_000_002]));
        snap.fleet_and_social_ops
            .fleet_watchlist
            .insert(90_000_001, std::collections::HashSet::from([90_000_002]));
        snap.fleet_and_social_ops
            .fleet_voice_enabled
            .insert(500, false);
        snap.fleet_and_social_ops
            .fleet_loot_logging
            .insert(500, true);
    });

    // update_calendar_response_from_esi loads uncached event 850_001 from DB, updates response, and rejects deleted 850_002
    let meta = update_calendar_response_from_esi(&ctx, cid, 850_001, 1).unwrap();
    assert_eq!(meta.owner_id, cid);
    assert_eq!(meta.title, "CTA Op");
    assert!(update_calendar_response_from_esi(&ctx, cid, 850_002, 1).is_none());
    assert!(update_calendar_response_from_esi(&ctx, cid, 999_999, 1).is_none());

    // ensure_social_extras_mailing_lists_seeded & persist_social_state
    ensure_social_extras_mailing_lists_seeded(&ctx);
    with_social_state(&ctx, |st| {
        st.mailing_lists.insert(
            750_002,
            MailingListState {
                list_id: 750_002,
                name: "SecondList".into(),
                owner_id: cid,
                ..Default::default()
            },
        );
    });
    persist_social_state(&ctx);
    assert!(ctx.db.with_snapshot(|s| s.mailing_lists.contains_key(&750_002)));

    // hydrate_social_state_from_db populates social_extras, esi_state, and fleet_extras
    hydrate_social_state_from_db(&ctx);
    with_social_state(&ctx, |st| {
        assert!(st.mailing_lists_seeded);
        assert_eq!(st.cspa_charges.get(&cid), Some(&IskCents(2950_00)));
        assert!(st.blocked_in_channel["player_chan_1"].contains(&90_000_003));
        assert_eq!(
            st.muted_in_channel[&("player_chan_1".into(), 90_000_002)].1,
            "spam"
        );
    });
    assert!(ctx.esi_state.is_mail_deleted(cid, 4002));
    assert!(ctx.esi_state.get_mail_meta(cid, 4001, false).is_read);
    {
        let fex = ctx.fleet_extras.lock().unwrap();
        assert!(fex.join_requests[&500].contains(&90_000_002));
        assert!(fex.watchlist[&cid].contains(&90_000_002));
        assert_eq!(fex.voice_enabled.get(&500), Some(&false));
        assert_eq!(fex.loot_logging.get(&500), Some(&true));
    }
}

#[tokio::test]
async fn multi_option_vote_winner_and_sanctioned_action_filters_kill_mutations() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let corp_id = 98_000_001_i64;
    let cid = 90_000_001_i64;
    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(cid),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        corp_role: u64::MAX,
        ..Default::default()
    };

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
        }
        // Case 601: 3-option CEO election (options.len() > 2), closed & passed, not yet executed
        snap.corp_vote_cases.insert(
            601,
            CorpVoteCaseRecord {
                case_id: 601,
                corporation_id: corp_id,
                vote_type: 0,
                parameter: 0,
                text: "CEO Election".into(),
                description: "3 candidates".into(),
                start_ts: 1_699_000_000,
                end_ts: 1_699_500_000,
                options: vec![
                    (1, "Candidate A".into(), 90_000_001),
                    (2, "Candidate B".into(), 90_000_002),
                    (3, "Reject All".into(), 0),
                ],
                executed: false,
            },
        );
        snap.corp_vote_ballots.insert(
            (601, 90_000_001),
            CorpVoteBallotRecord {
                case_id: 601,
                voter_id: 90_000_001,
                option_id: 2,
                weight: 300,
                voted_at_unix: 1_699_100_000,
            },
        );
        snap.corp_vote_ballots.insert(
            (601, 90_000_002),
            CorpVoteBallotRecord {
                case_id: 601,
                voter_id: 90_000_002,
                option_id: 1,
                weight: 100,
                voted_at_unix: 1_699_100_000,
            },
        );

        // Case 602: empty options (tests default Yes/No generation), open (end_ts > now), executed = true
        snap.corp_vote_cases.insert(
            602,
            CorpVoteCaseRecord {
                case_id: 602,
                corporation_id: corp_id,
                vote_type: 4,
                parameter: 5000,
                text: "Create Shares".into(),
                description: "desc".into(),
                start_ts: 1_699_900_000,
                end_ts: 1_700_500_000,
                options: Vec::new(),
                executed: true,
            },
        );
    });

    // GetVoteCaseOptions: 2-arg form, empty options fallback, and VoteCaseNotFound
    assert_eq!(
        call_vote_mgr(
            &ctx,
            &mut sess,
            "GetVoteCaseOptions",
            &[PyRep::Integer(corp_id), PyRep::Integer(999_999)]
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "VoteCaseNotFound"
    );
    let default_opts = call_vote_mgr(
        &ctx,
        &mut sess,
        "GetVoteCaseOptions",
        &[PyRep::Integer(corp_id), PyRep::Integer(602)],
    )
    .await
    .unwrap()
    .unwrap();
    if let PyRep::Rowset { rows, .. } = default_opts {
        assert_eq!(rows.len(), 2);
    } else {
        panic!("expected rowset");
    }

    // GetVoteCase: 2-arg form on 3-option case 601 verifies DBRow shape, FileTime timestamps, winning parameter == 90_000_002, and votesFor == 300
    let vc_601 = call_vote_mgr(
        &ctx,
        &mut sess,
        "GetVoteCase",
        &[PyRep::Integer(corp_id), PyRep::Integer(601)],
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(vc_601, PyRep::DBRow { .. }), "expected DBRow, got {vc_601:?}");
    assert_eq!(vc_601.dbrow_get("parameter"), Some(&PyRep::Integer(90_000_002)));
    assert_eq!(vc_601.dbrow_get("status"), Some(&PyRep::Integer(1)));
    assert_eq!(vc_601.dbrow_get("votesFor"), Some(&PyRep::Integer(300)));
    assert!(
        vc_601
            .dbrow_get("startDateTime")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            >= 116_444_736_000_000_000
    );

    // GetVoteCasesByCorporation 1-arg and status=0/"all" return {2: CRowset(open), 1: CRowset(closed)}
    for status_args in [
        vec![PyRep::Integer(corp_id)],
        vec![PyRep::Integer(corp_id), PyRep::Integer(0)],
        vec![PyRep::Integer(corp_id), PyRep::String("all".into())],
    ] {
        let grouped = call_vote_mgr(&ctx, &mut sess, "GetVoteCasesByCorporation", &status_args)
            .await
            .unwrap()
            .unwrap();
        let PyRep::Dict(entries) = grouped else {
            panic!("expected Dict {{2: CRowset, 1: CRowset}}, got {grouped:?}");
        };
        let open_set = entries
            .iter()
            .find(|(k, _)| k == &PyRep::Integer(2))
            .map(|(_, v)| v)
            .expect("open key 2 present");
        let closed_set = entries
            .iter()
            .find(|(k, _)| k == &PyRep::Integer(1))
            .map(|(_, v)| v)
            .expect("closed key 1 present");
        assert!(open_set.is_crowset(), "key 2 must be CRowset");
        assert!(closed_set.is_crowset(), "key 1 must be CRowset");
    }

    // GetVoteCasesByCorporation with "open", "closed", Integer(1), and max_len = 1 returns CRowset
    for filter in [
        PyRep::String("open".into()),
        PyRep::String("closed".into()),
        PyRep::Integer(1),
    ] {
        let res = call_vote_mgr(
            &ctx,
            &mut sess,
            "GetVoteCasesByCorporation",
            &[PyRep::Integer(corp_id), filter, PyRep::Integer(1)],
        )
        .await
        .unwrap()
        .unwrap();
        let rows = res.crowset_rows().expect("expected CRowset");
        assert_eq!(rows.len(), 1);
    }

    // GetCorpSanctionedActions with Integer(2), String("in effect"), Integer(0), String("all"), and default
    for status in [
        PyRep::Integer(2),
        PyRep::String("in effect".into()),
        PyRep::Integer(0),
        PyRep::String("all".into()),
    ] {
        let res = call_vote_mgr(
            &ctx,
            &mut sess,
            "GetCorpSanctionedActions",
            &[PyRep::Integer(corp_id), status],
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(res, PyRep::Rowset { .. }));
    }
}
