//! Unit tests for `pvp_filament_mgr` (Proving Grounds store, leaderboards, combat results, queue & event RPCs).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "pvp_filament_mgr_extra_unit_tests.rs"]
mod extra_unit_tests;

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

#[test]
fn date_parsing_sde_events_leaderboard_and_store_roundtrip() {
    let ts_start = parse_ymd_utc_seconds("2025-06-15", false).unwrap();
    let ts_end = parse_ymd_utc_seconds("2025-06-15", true).unwrap();
    assert_eq!(ts_end - ts_start, 86_399);
    assert!(parse_ymd_utc_seconds("2025-13-01", false).is_none());
    assert!(parse_ymd_utc_seconds("invalid", false).is_none());

    let ctx = test_ctx();
    let mut store = PvpFilamentStore::default();
    ensure_events_from_sde(&ctx.sde, &mut store);
    assert!(store.seeded_default_events);

    if let Some(ev) = store.events.first() {
        assert_eq!(
            lookup_sde_event_ids(&ctx.sde, ev.event_id),
            Some((ev.match_type_id, ev.schedule_id))
        );
    }
    assert!(lookup_sde_event_ids(&ctx.sde, -999_999).is_none());

    let spec = get_pvp_filament_match_spec(&ctx.sde, 56_088);
    assert!(spec.fleet_size >= 1);
    assert!(spec.number_of_fleets >= 2);

    // Populate leaderboard stats and verify sorting (score desc, wins desc, kills desc, dmg desc, char_id asc)
    store.stats.insert(101, (3, 1, 300, 3, 12_000.0));
    store.stats.insert(102, (5, 0, 500, 5, 25_000.0));
    store.stats.insert(103, (0, 0, 0, 0, 0.0)); // Excluded zero-activity entry

    let lb = build_leaderboard_rows(&store);
    assert_eq!(lb.len(), 2);
    assert_eq!(lb[0].dict_get("characterID").and_then(|v| v.as_i64()), Some(102));
    assert_eq!(lb[0].dict_get("rank").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(lb[1].dict_get("characterID").and_then(|v| v.as_i64()), Some(101));
    assert_eq!(lb[1].dict_get("rank").and_then(|v| v.as_i64()), Some(2));

    let dict_lb = build_leaderboard_rows_dict_for_event(&store, 171);
    assert_eq!(dict_lb.len(), 2);
    assert_eq!(
        dict_lb[0].dict_get("character_id").and_then(|v| v.as_i64()),
        Some(102)
    );

    let char_stat = build_character_statistics(&store, 102);
    assert_eq!(char_stat.dict_get("rank").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(char_stat.dict_get("wins").and_then(|v| v.as_i64()), Some(5));

    let char_dict = build_character_statistics_dict_for_event(&store, 171, 101);
    assert_eq!(char_dict.dict_get("rank").and_then(|v| v.as_i64()), Some(2));

    // Persist and reload store
    ctx.db.with_mut_snapshot(|snap| {
        save_pvp_store(snap, &store);
    });
    let reloaded = ctx.db.with_snapshot(load_pvp_store);
    assert_eq!(reloaded.stats.len(), 3);
}

#[test]
fn combat_result_and_arena_match_attribution() {
    let ctx = test_ctx();
    let winner = 950_001_i64;
    let loser = 950_002_i64;
    let arena_sys = 30000142_u32;

    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_pvp_store(snap);
        st.active_matches.insert(
            77,
            PvpArenaMatchInstance {
                match_id: 77,
                event_id: 171,
                arena_solar_system_id: arena_sys,
                team_a: vec![winner],
                team_b: vec![loser],
                origin_systems: HashMap::new(),
                started_unix: ctx.now_unix(),
                active: true,
                winner_char_id: None,
                exited_pilots: HashSet::new(),
                end_gate_id: 88_001,
            },
        );
        save_pvp_store(snap, &st);
    });

    record_proving_grounds_combat_result(&ctx, winner, loser, 8_500.0)
        .expect("record combat result");

    let store = ctx.db.with_snapshot(load_pvp_store);
    let w_stats = build_character_statistics(&store, winner);
    let l_stats = build_character_statistics(&store, loser);
    assert_eq!(w_stats.dict_get("wins").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(l_stats.dict_get("losses").and_then(|v| v.as_i64()), Some(1));

    // Seed an active arena match and verify record_proving_grounds_combat_result_if_queued
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_pvp_store(snap);
        if let Some(m) = st.active_matches.get_mut(&77) {
            m.active = true;
            m.winner_char_id = None;
        }
        save_pvp_store(snap, &st);
    });

    // Self-hit or different system ignored
    record_proving_grounds_combat_result_if_queued(&ctx, winner, winner, arena_sys, 500.0);
    record_proving_grounds_combat_result_if_queued(&ctx, winner, loser, 30002187, 500.0);

    // Valid arena kill increments winner wins again
    record_proving_grounds_combat_result_if_queued(&ctx, winner, loser, arena_sys, 1_500.0);
    let store2 = ctx.db.with_snapshot(load_pvp_store);
    let w2 = build_character_statistics(&store2, winner);
    assert_eq!(w2.dict_get("wins").and_then(|v| v.as_i64()), Some(2));
}

#[test]
fn pvp_filament_rpc_events_stats_and_queue_leave() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let ctx = test_ctx();
    let cid = 950_010_i64;
    let ship_id = 88_950_010_i64;
    let fil_id = 88_950_011_i64;
    let now = ctx.now_unix() as i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = cid;
        ch.solar_system_id = 30000142;
        ch.station_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(cid, ch);

        let mut st = load_pvp_store(snap);
        st.seeded_default_events = true;
        st.events = vec![
            PvpEventSchedule {
                event_id: 201,
                schedule_id: 1,
                event_name: "Active Frigate Proving Grounds".into(),
                description: "1v1 T1 Frigate".into(),
                match_type_id: 10,
                start_unix: now - 3_600,
                end_unix: now + 3_600,
                type_ids: vec![56_088],
            },
            PvpEventSchedule {
                event_id: 202,
                schedule_id: 2,
                event_name: "Future Destroyer Event".into(),
                description: "2v2 Destroyer".into(),
                match_type_id: 11,
                start_unix: now + 86_400,
                end_unix: now + 172_800,
                type_ids: vec![56_089],
            },
        ];
        st.stats.insert(cid, (4, 1, 400, 4, 15_000.0));
        st.queue.insert(
            cid,
            PvpQueueCharacterEntry {
                item_id: fil_id,
                type_id: 56_088,
                location_id: ship_id,
                flag: InventoryFlag::Cargo as u16,
                solar_system_id: 30000142,
                joined_unix: now as u64,
            },
        );
        save_pvp_store(snap, &st);
    });

    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(cid);
    sess.solar_system_id = Some(30000142);
    let kw = HashMap::new();

    let all_ev = rt
        .block_on(handle(&ctx, &mut sess, "pvpFilamentMgr", "GetAllEvents", &[], &kw))
        .unwrap()
        .expect("GetAllEvents");
    assert!(matches!(all_ev, PyRep::Dict(ref d) if d.len() == 2));

    let act_ev = rt
        .block_on(handle(&ctx, &mut sess, "pvpFilamentMgr", "GetActiveEvents", &[], &kw))
        .unwrap()
        .expect("GetActiveEvents");
    assert!(matches!(act_ev, PyRep::Dict(ref d) if d.len() == 1));

    let recent_ev = rt
        .block_on(handle(&ctx, &mut sess, "pvpFilamentMgr", "GetMostRecentEvent", &[], &kw))
        .unwrap()
        .expect("GetMostRecentEvent");
    assert_eq!(recent_ev.dict_get("eventID").and_then(|v| v.as_i64()), Some(201));

    let next_dt = rt
        .block_on(handle(&ctx, &mut sess, "pvpFilamentMgr", "GetNextEventDate", &[], &kw))
        .unwrap()
        .expect("GetNextEventDate");
    assert!(next_dt.as_i64().is_some());

    let lb = rt
        .block_on(handle(
            &ctx,
            &mut sess,
            "pvpFilamentMgr",
            "GetLeaderboard",
            &[PyRep::Integer(201)],
            &kw,
        ))
        .unwrap()
        .expect("GetLeaderboard");
    assert!(matches!(lb, PyRep::List(ref rows) if !rows.is_empty()));

    let char_st = rt
        .block_on(handle(
            &ctx,
            &mut sess,
            "pvpFilamentMgr",
            "GetCharacterStatistics",
            &[PyRep::Integer(201), PyRep::Integer(cid)],
            &kw,
        ))
        .unwrap()
        .expect("GetCharacterStatistics");
    assert_eq!(char_st.dict_get("wins").and_then(|v| v.as_i64()), Some(4));

    // Wrong filament item ID on LeavePVPQueue returns FilamentNotFound
    let wrong_id = rt
        .block_on(handle(
            &ctx,
            &mut sess,
            "pvpFilamentMgr",
            "LeavePVPQueue",
            &[PyRep::Integer(999_999)],
            &kw,
        ))
        .unwrap()
        .unwrap_err();
    assert_eq!(wrong_id.code, "FilamentNotFound");

    // Valid LeavePVPQueue removes queue entry and refunds filament item
    rt.block_on(handle(
        &ctx,
        &mut sess,
        "pvpFilamentMgr",
        "LeavePVPQueue",
        &[PyRep::Integer(fil_id)],
        &kw,
    ))
    .unwrap()
    .expect("LeavePVPQueue");
    ctx.db.with_snapshot(|snap| {
        let st = load_pvp_store(snap);
        assert!(!st.queue.contains_key(&cid));
        assert!(snap
            .items
            .values()
            .any(|it| it.owner_id == cid && it.type_id == 56_088 && it.quantity >= 1));
    });

    // Docked JoinPVPQueue rejected with MustBeInSpace
    sess.station_id = Some(60003760);
    let docked_err = rt
        .block_on(handle(
            &ctx,
            &mut sess,
            "pvpFilamentMgr",
            "JoinPVPQueue",
            &[PyRep::Integer(fil_id)],
            &kw,
        ))
        .unwrap()
        .unwrap_err();
    assert_eq!(docked_err.code, "MustBeInSpace");

    // Undocked without item fails ItemNotFound
    sess.station_id = None;
    sess.ship_id = Some(ship_id);
    let missing_item_err = rt
        .block_on(handle(
            &ctx,
            &mut sess,
            "pvpFilamentMgr",
            "JoinPVPQueue",
            &[PyRep::Integer(777_888)],
            &kw,
        ))
        .unwrap()
        .unwrap_err();
    assert_eq!(missing_item_err.code, "ItemNotFound");

    // Unknown service / unknown method returns None
    assert!(rt
        .block_on(handle(&ctx, &mut sess, "", "", &[], &kw))
        .is_none());
    assert!(rt
        .block_on(handle(&ctx, &mut sess, "pvpFilamentMgr", "", &[], &kw))
        .is_none());
}

#[test]
fn combat_result_ranking_and_store_roundtrip_persistence() {
    let ctx = test_ctx();
    let cid1 = 910_011_i64;
    let cid2 = 910_012_i64;
    let mut store = PvpFilamentStore::default();
    ensure_events_from_sde(&ctx.sde, &mut store);
    let event_id = store.events.first().map(|e| e.event_id).unwrap_or(1);

    ctx.db.with_mut_snapshot(|snap| {
        let mut c1 = snap.characters.values().next().unwrap().clone();
        c1.character_id = cid1;
        c1.name = "ProvingAce".into();
        let mut c2 = c1.clone();
        c2.character_id = cid2;
        c2.name = "ProvingContender".into();
        snap.characters.insert(cid1, c1);
        snap.characters.insert(cid2, c2);
        store.active_matches.insert(
            99,
            PvpArenaMatchInstance {
                match_id: 99,
                event_id,
                arena_solar_system_id: 30000142,
                team_a: vec![cid1],
                team_b: vec![cid2],
                origin_systems: HashMap::new(),
                started_unix: ctx.now_unix(),
                active: true,
                winner_char_id: None,
                exited_pilots: HashSet::new(),
                end_gate_id: 88_002,
            },
        );
        save_pvp_store(snap, &store);
    });

    // Record combat results across two pilots and verify ranking order
    record_proving_grounds_combat_result(&ctx, cid1, cid2, 12_000.0).expect("match 1");
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_pvp_store(snap);
        if let Some(m) = st.active_matches.get_mut(&99) {
            m.active = true;
            m.winner_char_id = None;
        }
        save_pvp_store(snap, &st);
    });
    record_proving_grounds_combat_result(&ctx, cid1, cid2, 8_000.0).expect("match 2");

    let reloaded = ctx.db.with_snapshot(load_pvp_store);
    let lb_rows = build_leaderboard_rows(&reloaded);
    assert!(lb_rows.len() >= 2);
    assert_eq!(lb_rows[0].dict_get("characterID").and_then(|v| v.as_i64()), Some(cid1));
    assert_eq!(lb_rows[0].dict_get("wins").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(lb_rows[1].dict_get("characterID").and_then(|v| v.as_i64()), Some(cid2));
    assert_eq!(lb_rows[1].dict_get("losses").and_then(|v| v.as_i64()), Some(2));

    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut sess = SessionState::default();
    sess.user_id = Some(11);
    sess.character_id = Some(cid1);
    sess.solar_system_id = Some(30000142);
    let kw = HashMap::new();

    let all_ev = rt
        .block_on(handle(&ctx, &mut sess, "pvpFilamentMgr", "GetAllEvents", &[], &kw))
        .unwrap()
        .expect("GetAllEvents");
    assert!(matches!(all_ev, PyRep::Dict(_)));

    let lb_rpc = rt
        .block_on(handle(
            &ctx,
            &mut sess,
            "pvpFilamentMgr",
            "GetLeaderboard",
            &[PyRep::Integer(event_id)],
            &kw,
        ))
        .unwrap()
        .expect("GetLeaderboard");
    assert!(matches!(lb_rpc, PyRep::List(_)));
}
