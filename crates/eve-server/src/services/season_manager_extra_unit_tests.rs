//! Extra unit and mutation-killing tests for `services::season_manager`
//! covering fallback blob loading (`season_manager:<cid>` and `season_manager` map),
//! `default_challenge` FSD lookup vs fallback, `get_current_activity_count` filters,
//! `ensure_character_challenges_with_limit` eviction & one-shot rules, `select_season`
//! (clear/invalid/ended/valid), `get_season_data_for_character` multi-active/none-active,
//! `process_client_event` error & string event branches, `claim_challenge_rewards`
//! (ISK, `next_challenge` wake/insert, docked vs in-space item delivery, Omega goal track),
//! `get_loot_table_by_typeID`, `get_character_progress_by_character_id`, and `claim_goal_reward`.

use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use crate::services::season_manager::*;
use eve_db::{IndustryJobRecord, KillmailRecord, MiningLedgerRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::{BTreeMap, HashMap};

fn season_sess(char_id: i64, station_id: Option<i64>) -> SessionState {
    let mut s = SessionState::default();
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.station_id = station_id;
    s
}

fn call_sm(
    ctx: &crate::router::ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    handle_season_manager(ctx, sess, "seasonManager", method, args, &kw).unwrap()
}

#[test]
fn load_season_state_fallbacks_activity_count_filters_and_challenge_limit_eviction() {
    let (ctx, _session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let now_ft = ctx.now_blueos_filetime_ticks();

    let defs = default_seasonal_challenge_definitions();
    if let Some(first_def) = defs.first() {
        let known_ch = default_challenge(first_def.challenge_id, 2, now_ft);
        assert_eq!((known_ch.challenge_id, known_ch.title), (first_def.challenge_id, first_def.title.clone()));
    }
    let unknown_ch = default_challenge(999_888_777, 3, now_ft);
    assert_eq!((unknown_ch.title.as_str(), unknown_ch.points), ("Challenge 999888777", 10));

    let (c_alt, c_map) = (910_001i64, 910_002i64);
    ctx.db.with_mut_snapshot(|snap| {
        let mut st_alt = CharacterSeasonState::default();
        st_alt.season_points = 42;
        snap.service_state_blobs.insert(format!("season_manager:{c_alt}"), serde_json::to_string(&st_alt).unwrap());

        let mut st_map = CharacterSeasonState::default();
        st_map.season_points = 84;
        let mut map = HashMap::new();
        map.insert(c_map, st_map);
        snap.service_state_blobs.insert("season_manager".into(), serde_json::to_string(&map).unwrap());

        assert_eq!(load_season_state_from_blobs(snap, c_alt).season_points, 42);
        assert_eq!(load_season_state_from_blobs(snap, c_map).season_points, 84);
        assert_eq!(load_season_state_from_blobs(snap, 910_999).season_points, 0);
    });

    let other_char = 910_050i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&char_id).unwrap().clone();
        ch.character_id = other_char;
        snap.characters.insert(other_char, ch);

        for (cid, qty) in [(char_id, 120), (other_char, 999)] {
            snap.mining_ledger.push(MiningLedgerRecord {
                character_id: cid, solar_system_id: 30_000_142, type_id: 1230,
                quantity: qty, date: "2026-11-05".into(), timestamp_unix: 100,
            });
        }
        assert_eq!(get_current_activity_count("totalMined", snap, char_id), 120);

        let corp_id = *snap.corporations.keys().next().unwrap();
        for (kid, vic) in [(1, 0), (2, char_id), (3, other_char), (4, corp_id), (5, 3_000_999)] {
            snap.killmails.insert(
                90_000 + kid,
                KillmailRecord {
                    kill_id: 90_000 + kid, victim_character_id: vic, attacker_character_id: char_id,
                    solar_system_id: 30_000_142, ship_type_id: 587, ..Default::default()
                },
            );
        }
        assert_eq!(get_current_activity_count("destroyHostileNPC", snap, char_id), 1);

        for (jid, act, status) in [(1, 1u8, "completed"), (2, 1, "102"), (3, 1, "delivered"), (4, 1, "active"), (5, 3, "delivered")] {
            snap.industry_jobs.insert(
                80_000 + jid,
                IndustryJobRecord {
                    job_id: 80_000 + jid, installer_id: char_id, activity_id: act, status: status.into(), ..Default::default()
                },
            );
        }
        assert_eq!(get_current_activity_count("typeManufacturedCount", snap, char_id), 3);

        let custom_defs = vec![
            SeasonalChallengeDef {
                challenge_id: 1001, season_id: 1, event_type: "totalMined".into(), title: "OneShot Claimed".into(),
                points: 10, max_progress: 100, should_only_appear_once_per_season: true, ..Default::default()
            },
            SeasonalChallengeDef {
                challenge_id: 1002, season_id: 1, event_type: "totalMined".into(), title: "Expired Challenge".into(),
                points: 10, max_progress: 100, ..Default::default()
            },
            SeasonalChallengeDef {
                challenge_id: 1003, season_id: 1, event_type: "totalMined".into(), title: "Replacement Challenge".into(),
                points: 20, max_progress: 50, ..Default::default()
            },
        ];
        let mut st = CharacterSeasonState::default();
        let mut ch1 = custom_defs[0].to_character_challenge(1, now_ft);
        ch1.claimed = true;
        st.challenges.insert(1001, ch1);

        let mut ch2 = custom_defs[1].to_character_challenge(1, now_ft);
        ch2.expiration_date = now_ft - 10;
        st.challenges.insert(1002, ch2);

        let expired_pairs = ensure_character_challenges_with_limit(
            &mut st, &custom_defs, snap, char_id, now_ft, now_ft + 1_000_000_000, 1,
        );
        assert!(st.claimed_challenges.contains_key(&1001) && st.completed_once_per_season_ids.contains(&1001));
        assert_eq!(expired_pairs.len(), 1);
        assert_eq!((expired_pairs[0].0, expired_pairs[0].1.challenge_id), (1002, 1003));
        assert_eq!(st.challenges.get(&1003).unwrap().baseline_progress, 120);
    });
}

#[test]
fn select_season_get_season_data_and_process_client_event_edge_cases() {
    let (ctx, _session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx.db.acquire_online_lock(uid, 1, Some(char_id), true, ctx.now_unix());
    let mut sess = season_sess(char_id, Some(60_003_760));
    let now_ft = ctx.now_blueos_filetime_ticks();

    ctx.db.with_mut_snapshot(|snap| {
        let mut cfg = SeasonConfig::default();
        cfg.max_active_challenges = 6;
        snap.service_state_blobs.insert(SEASON_CONFIG_KEY.into(), serde_json::to_string(&cfg).unwrap());
    });
    assert_eq!(
        call_sm(&ctx, &mut sess, "get_max_active_challenges", &[]).unwrap(),
        PyRep::Integer(6)
    );

    ctx.db.with_mut_snapshot(|snap| {
        let mut cat = BTreeMap::new();
        for (sid, end_ft) in [(1, now_ft + 5_000_000_000), (2, now_ft + 5_000_000_000), (99, now_ft - 100)] {
            let mut s = SeasonConfig::default();
            s.season_id = sid;
            s.end_time_ft = end_ft;
            cat.insert(sid, s);
        }
        snap.service_state_blobs.insert(SEASONS_CATALOG_KEY.into(), serde_json::to_string(&cat).unwrap());
    });

    let multi_data = call_sm(&ctx, &mut sess, "get_season_data_for_character", &[]).unwrap();
    assert!(matches!(multi_data, PyRep::List(ref l) if l.len() == 2));

    for bad_arg in [PyRep::String("bad".into()), PyRep::Integer(777), PyRep::Integer(99)] {
        let err = call_sm(&ctx, &mut sess, "select_season", &[bad_arg]).unwrap_err();
        assert_eq!(err.code, "InvalidSeasonId");
    }

    let _ = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        call_sm(&ctx, &mut sess, "select_season", &[PyRep::Integer(2)]).unwrap(),
        PyRep::Bool(true)
    );
    assert!(ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnSeasonSelected"));
    assert!(call_sm(&ctx, &mut sess, "select_season", &[PyRep::Integer(0)]).is_ok());

    let err = call_sm(&ctx, &mut sess, "process_client_event", &[]).unwrap_err();
    assert_eq!(err.code, "ChallengeNotFound");
    let err = call_sm(&ctx, &mut sess, "process_client_event", &[PyRep::Integer(2001), PyRep::Integer(0)]).unwrap_err();
    assert_eq!(err.code, "InvalidEventProgress");

    let custom_defs = vec![
        SeasonalChallengeDef {
            challenge_id: 2001, season_id: 1, event_type: "client_open_map".into(), title: "Open Star Map".into(),
            points: 10, max_progress: 1, client_event_driven: true, ..Default::default()
        },
        SeasonalChallengeDef {
            challenge_id: 2002, season_id: 1, event_type: "totalMined".into(), title: "Server Mined".into(),
            points: 10, max_progress: 100, client_event_driven: false, ..Default::default()
        },
        SeasonalChallengeDef {
            challenge_id: 2003, season_id: 1, event_type: "client_dormant".into(), title: "Dormant".into(),
            points: 10, max_progress: 1, client_event_driven: true, is_dormant: true, ..Default::default()
        },
    ];
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(SEASONAL_CHALLENGES_CONFIG_KEY.into(), serde_json::to_string(&custom_defs).unwrap());
        let mut st = CharacterSeasonState::default();
        st.selected_season_id = 1;
        for d in &custom_defs {
            st.challenges.insert(d.challenge_id, d.to_character_challenge(1, now_ft));
        }
        save_season_state_to_blobs(snap, char_id, &st);
    });

    let err = call_sm(&ctx, &mut sess, "process_client_event", &[PyRep::Integer(2002), PyRep::Integer(1)]).unwrap_err();
    assert_eq!(err.code, "InvalidChallengeEvent");
    let err = call_sm(&ctx, &mut sess, "process_client_event", &[PyRep::Integer(2003), PyRep::Integer(1)]).unwrap_err();
    assert_eq!(err.code, "ChallengeDormant");

    let _ = ctx.drain_notifications_for_char(char_id);
    assert!(call_sm(&ctx, &mut sess, "process_client_event", &[PyRep::String("open_map".into()), PyRep::Integer(1)]).is_ok());
    let ev_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(ev_notifs.iter().any(|n| n.event == "OnChallengeProgressUpdate") && ev_notifs.iter().any(|n| n.event == "OnChallengeCompleted"));
    assert_eq!(
        call_sm(&ctx, &mut sess, "challenge_is_dormant", &[PyRep::Integer(2001)]).unwrap(),
        PyRep::Bool(true)
    );
}

#[test]
fn claim_challenge_rewards_isk_next_challenge_item_delivery_omega_goals_and_career_goals() {
    let (ctx, _session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx.db.acquire_online_lock(uid, 1, Some(char_id), true, ctx.now_unix());
    let now_ft = ctx.now_blueos_filetime_ticks();

    let defs = vec![
        SeasonalChallengeDef {
            challenge_id: 3001, season_id: 1, event_type: "totalMined".into(), title: "Tier 1 Mining".into(),
            points: 15, max_progress: 10, client_event_driven: false, reward_type_id: 34, reward_quantity: 500,
            isk_awarded: 250_000, next_challenge: Some(3002), should_only_appear_once_per_season: true, ..Default::default()
        },
        SeasonalChallengeDef {
            challenge_id: 3002, season_id: 1, event_type: "totalMined".into(), title: "Tier 2 Mining".into(),
            points: 20, max_progress: 10, client_event_driven: false, reward_type_id: 34, reward_quantity: 1_000,
            is_dormant: true, ..Default::default()
        },
        SeasonalChallengeDef {
            challenge_id: 3003, season_id: 1, event_type: "totalMined".into(), title: "Incomplete Challenge".into(),
            points: 10, max_progress: 9_999, client_event_driven: false, ..Default::default()
        },
    ];

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 2;
        ch.station_id = Some(60_003_760);

        snap.service_state_blobs.insert(SEASONAL_CHALLENGES_CONFIG_KEY.into(), serde_json::to_string(&defs).unwrap());
        let mut st = CharacterSeasonState::default();
        st.selected_season_id = 1;
        let mut c1 = defs[0].to_character_challenge(1, now_ft);
        c1.progress = 10;
        c1.completed = true;
        st.challenges.insert(3001, c1);
        st.challenges.insert(3003, defs[2].to_character_challenge(1, now_ft));
        save_season_state_to_blobs(snap, char_id, &st);
    });

    let mut docked_sess = season_sess(char_id, Some(60_003_760));

    let err = call_sm(&ctx, &mut docked_sess, "claim_challenge_rewards", &[PyRep::Integer(3003)]).unwrap_err();
    assert_eq!(err.code, "ChallengeNotCompleted");

    let _ = ctx.drain_notifications_for_char(char_id);
    let claim1 = call_sm(&ctx, &mut docked_sess, "claim_challenge_rewards", &[PyRep::Integer(3001)]).unwrap();
    assert!(claim1.dict_get("deliveredItemID").and_then(|v| v.as_i64()).is_some());
    let notifs1 = ctx.drain_notifications_for_char(char_id);
    for ev in ["OnAccountChange", "OnItemChange", "OnSeasonalGoalCompleted", "OnSeasonOmegaRewardsClaimed"] {
        assert!(notifs1.iter().any(|n| n.event == ev), "missing {ev}");
    }

    let err = call_sm(&ctx, &mut docked_sess, "claim_challenge_rewards", &[PyRep::Integer(3001)]).unwrap_err();
    assert_eq!(err.code, "ChallengeAlreadyClaimed");

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = None;
        let mut st = load_season_state_from_blobs(snap, char_id);
        let c2 = st.challenges.get_mut(&3002).expect("3002 inserted by next_challenge");
        assert!(!c2.is_dormant);
        c2.progress = 10;
        c2.completed = true;
        save_season_state_to_blobs(snap, char_id, &st);
    });
    let mut space_sess = season_sess(char_id, None);
    let _ = ctx.drain_notifications_for_char(char_id);
    let claim2 = call_sm(&ctx, &mut space_sess, "claim_challenge_rewards", &[PyRep::Integer(3002)]).unwrap();
    assert!(claim2.dict_get("deliveredItemID").and_then(|v| v.as_i64()).is_some());
    assert!(ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnRedeemingTokensUpdated"));

    let err = call_sm(&ctx, &mut space_sess, "get_loot_table_by_typeID", &[PyRep::Integer(-1)]).unwrap_err();
    assert_eq!(err.code, "InvalidTypeId");
    let valid_tid = *ctx.sde.types.keys().next().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let mut loot_map: HashMap<u32, Vec<(u32, i64, f64)>> = HashMap::new();
        loot_map.insert(valid_tid, vec![(34, 100, 0.75)]);
        snap.service_state_blobs.insert(SEASON_LOOT_TABLES_KEY.into(), serde_json::to_string(&loot_map).unwrap());
    });
    let loot_rep = call_sm(&ctx, &mut space_sess, "get_loot_table_by_typeID", &[PyRep::Integer(valid_tid as i64)]).unwrap();
    assert!(matches!(loot_rep, PyRep::List(ref l) if l.len() == 1));

    let err = call_sm(&ctx, &mut space_sess, "get_character_progress_by_character_id", &[PyRep::Integer(-5)]).unwrap_err();
    assert_eq!(err.code, "InvalidGoalId");
    let err = call_sm(&ctx, &mut space_sess, "claim_goal_reward", &[PyRep::Integer(-5)]).unwrap_err();
    assert_eq!(err.code, "InvalidGoalId");
    let err = call_sm(&ctx, &mut space_sess, "claim_goal_reward", &[PyRep::Integer(999_999_999)]).unwrap_err();
    assert_eq!(err.code, "ChallengeNotFound");

    let mining_ms = crate::services::quasar_career_goals::default_career_milestones()
        .into_iter()
        .find(|m| m.career_id == Some(2))
        .unwrap();
    let err = call_sm(&ctx, &mut space_sess, "claim_goal_reward", &[PyRep::Integer(mining_ms.milestone_id as i64)]).unwrap_err();
    assert_eq!(err.code, "GoalProgressIncomplete");

    ctx.db.with_mut_snapshot(|snap| {
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: char_id, solar_system_id: 30_000_142, type_id: 1230,
            quantity: 100_000, date: "2026-11-05".into(), timestamp_unix: 200,
        });
    });
    let prog_goal = call_sm(&ctx, &mut space_sess, "get_character_progress_by_character_id", &[PyRep::Integer(mining_ms.milestone_id as i64)]).unwrap();
    assert_eq!(prog_goal.dict_get("verifiedFromLedger"), Some(&PyRep::Bool(true)));

    let goal_ok = call_sm(&ctx, &mut space_sess, "claim_goal_reward", &[PyRep::Integer(mining_ms.milestone_id as i64)]).unwrap();
    assert!(goal_ok.dict_get("freeSkillPoints").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);

    let err = call_sm(&ctx, &mut space_sess, "claim_goal_reward", &[PyRep::Integer(mining_ms.milestone_id as i64)]).unwrap_err();
    assert_eq!(err.code, "GoalAlreadyClaimed");
}
