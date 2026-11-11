//! Edge-case unit tests for `notify_emitters_combat_ess_progression`.

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;

fn make_online_ctx() -> (ServerContext, i64) {
    let sde = Arc::new(SdeRegistry::default());
    let db = Arc::new(WorldRepository::with_demo_seed());
    let char_id = 90_000_001;
    db.acquire_online_lock(1, 501, Some(char_id), true, 1_775_400_000)
        .expect("online lock");
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
    (ctx, char_id)
}

#[test]
fn push_char_notification_with_db_missing_char_and_dropped_rx_branches() {
    let (ctx, char_id) = make_online_ctx();
    let pend: RwLock<HashMap<i64, Vec<PendingMachoNotification>>> = RwLock::new(HashMap::new());
    let (tx, rx) = broadcast::channel(8);
    drop(rx);

    // Character not in `s.characters` -> uid_opt is None, dropped broadcast receiver ignored.
    let unknown_char = 99_999_888_i64;
    push_char_notification_with_db(
        &ctx.db,
        Some(&pend),
        Some(&tx),
        unknown_char,
        "OnProgressionTaskCompleted",
        vec![PyRep::Integer(42)],
    );
    let q = pend.read().unwrap();
    let items = q.get(&unknown_char).expect("unknown char queued");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].event, "OnProgressionTaskCompleted");
    drop(q);

    // DB-backed progression emitters with Some(&tx) and with pending_notifications = None.
    let (tx2, mut rx2) = broadcast::channel(8);
    emit_progression_advanced_with_db(&ctx.db, Some(&pend), Some(&tx2), char_id, 100, 200);
    emit_progression_task_completed_with_db(&ctx.db, Some(&pend), Some(&tx2), char_id, 300);
    emit_progression_advanced_with_db(&ctx.db, None, Some(&tx2), char_id, 101, 201);
    emit_progression_task_completed_with_db(&ctx.db, None, Some(&tx2), char_id, 301);

    let first = rx2.try_recv().expect("OnProgressionAdvanced");
    assert_eq!(first.0, char_id);
    assert_eq!(first.1.event, "OnProgressionAdvanced");
    let second = rx2.try_recv().expect("OnProgressionTaskCompleted");
    assert_eq!(second.1.event, "OnProgressionTaskCompleted");
    assert!(rx2.try_recv().is_err());
}

#[test]
fn combat_message_negative_and_zero_non_wrecking_miss_branches() {
    let (ctx, char_id) = make_online_ctx();

    emit_combat_message(&ctx, char_id, "MsgNegDamage", 9101, -5.0, 2456, true);
    emit_combat_message(&ctx, char_id, "MsgZeroNonWreck", 9102, 0.0, 2456, false);

    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 2);
    for notif in drained {
        assert_eq!(notif.event, "OnCombatMessage");
        let PyRep::Dict(entries) = &notif.args[1] else {
            panic!("expected Dict messageArgs");
        };
        let hit_quality = entries
            .iter()
            .find(|(k, _)| *k == PyRep::String("hitQualityText".into()))
            .map(|(_, v)| v.clone());
        assert_eq!(hit_quality, Some(PyRep::String("misses".into())));
    }
}

#[test]
fn char_station_notification_missing_char_and_alliance_war_faction_populated() {
    let (ctx, char_id) = make_online_ctx();
    let station_id = 60_003_760_i64;
    let missing_char = 98_765_432_i64;

    // Missing char defaults corp_id = 0, alliance_id = None, war_faction_id = None
    emit_char_station_notification(&ctx, missing_char, station_id, true);
    // Populate alliance_id and war_faction_id on existing char_id
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).expect("char exists");
        ch.alliance_id = Some(99_000_777);
        ch.war_faction_id = Some(500_001);
    });
    emit_char_station_notification(&ctx, char_id, station_id, false);

    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 2);
    assert_eq!(drained[0].event, "OnCharNowInStation");
    assert_eq!(
        drained[0].args,
        vec![PyRep::List(vec![
            PyRep::Integer(missing_char),
            PyRep::Integer(0),
            PyRep::None,
            PyRep::None,
        ])]
    );
    assert_eq!(drained[1].event, "OnCharNoLongerInStation");
    assert_eq!(
        drained[1].args,
        vec![PyRep::List(vec![
            PyRep::Integer(char_id),
            PyRep::Integer(1_000_170),
            PyRep::Integer(99_000_777),
            PyRep::Integer(500_001),
        ])]
    );
}

#[test]
fn empty_scanners_empty_flag_updates_and_non_positive_death_emitters() {
    let (ctx, char_id) = make_online_ctx();
    let sys_id = 30_000_142_u32;

    // Empty scans and empty flag slices
    emit_cargo_scan_complete(&ctx, char_id, 800_001, vec![]);
    emit_ship_scan_completed(&ctx, char_id, 800_001, 0.0, 500.0, vec![]);
    emit_system_criminal_flag_updates(&ctx, sys_id, &[], &[], &[]);
    emit_system_disapproval_flag_updates(&ctx, sys_id, &[], &[]);

    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 4);
    assert_eq!(
        drained[0].args,
        vec![PyRep::Integer(800_001), PyRep::List(vec![])]
    );
    assert_eq!(
        drained[1].args,
        vec![
            PyRep::Integer(800_001),
            PyRep::Real(0.0),
            PyRep::Real(500.0),
            PyRep::List(vec![]),
        ]
    );
    assert_eq!(
        drained[2].args,
        vec![PyRep::List(vec![]), PyRep::List(vec![]), PyRep::List(vec![])]
    );
    assert_eq!(
        drained[3].args,
        vec![PyRep::List(vec![]), PyRep::List(vec![])]
    );

    // Negative char_id must be ignored across all death/pod emitters
    for bad_id in [-1_i64, -999] {
        emit_kill_notification(&ctx, bad_id);
        emit_ship_death(&ctx, bad_id);
        emit_pod_death(&ctx, bad_id);
        emit_pod_killed(&ctx, bad_id, 555_555);
        emit_player_pod_death(&ctx, bad_id);
        emit_player_pod_respawn(&ctx, bad_id);
    }
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
}
