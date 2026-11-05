//! Unit tests for `notify_emitters_ess_bounties`.

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
fn push_char_notification_with_db_handles_guards_queue_cap_and_broadcast() {
    let (ctx, char_id) = make_online_ctx();
    let pend: RwLock<HashMap<i64, Vec<PendingMachoNotification>>> = RwLock::new(HashMap::new());
    let (tx, mut rx) = broadcast::channel(300);

    // Non-positive char_id is ignored
    push_char_notification_with_db(&ctx.db, Some(&pend), Some(&tx), 0, "OnStep", vec![]);
    push_char_notification_with_db(&ctx.db, Some(&pend), Some(&tx), -5, "OnStep", vec![]);
    assert!(pend.read().unwrap().is_empty());

    // None pending_notifications is ignored
    push_char_notification_with_db(&ctx.db, None, Some(&tx), char_id, "OnStep", vec![]);
    assert!(rx.try_recv().is_err());

    // Fill queue up to 256 and verify 257th drops oldest (index 0)
    for i in 0..257_i64 {
        push_char_notification_with_db(
            &ctx.db,
            Some(&pend),
            Some(&tx),
            char_id,
            "OnStep",
            vec![PyRep::Integer(i)],
        );
    }
    let guard = pend.read().unwrap();
    let q = guard.get(&char_id).expect("queue present");
    assert_eq!(q.len(), 256);
    assert_eq!(q[0].args, vec![PyRep::Integer(1)]);
    assert_eq!(q[255].args, vec![PyRep::Integer(256)]);
    assert_eq!(q[0].id_type, "charid");
    assert_eq!(q[0].recipient_char_id, char_id);
    let (recv_cid, recv_notif) = rx.try_recv().expect("broadcast sent");
    assert_eq!(recv_cid, char_id);
    assert_eq!(recv_notif.event, "OnStep");
}

#[test]
fn emit_progression_advanced_and_task_completed_both_ctx_and_db_variants() {
    let (ctx, char_id) = make_online_ctx();

    emit_progression_advanced(&ctx, char_id, 42, 108);
    emit_progression_task_completed(&ctx, char_id, 109);
    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 2);
    assert_eq!(drained[0].event, "OnProgressionAdvanced");
    assert_eq!(drained[0].id_type, "charid");
    assert_eq!(
        drained[0].args,
        vec![PyRep::Integer(42), PyRep::Integer(108)]
    );
    assert_eq!(drained[1].event, "OnProgressionTaskCompleted");
    assert_eq!(drained[1].id_type, "charid");
    assert_eq!(drained[1].args, vec![PyRep::Integer(109)]);

    let pend: RwLock<HashMap<i64, Vec<PendingMachoNotification>>> = RwLock::new(HashMap::new());
    emit_progression_advanced_with_db(&ctx.db, Some(&pend), None, char_id, 77, 88);
    emit_progression_task_completed_with_db(&ctx.db, Some(&pend), None, char_id, 99);
    let guard = pend.read().unwrap();
    let q = guard.get(&char_id).expect("db notifications queued");
    assert_eq!(q.len(), 2);
    assert_eq!(q[0].event, "OnProgressionAdvanced");
    assert_eq!(q[0].args, vec![PyRep::Integer(77), PyRep::Integer(88)]);
    assert_eq!(q[1].event, "OnProgressionTaskCompleted");
    assert_eq!(q[1].args, vec![PyRep::Integer(99)]);
}

#[test]
fn emit_combat_message_classifies_miss_hit_and_wrecking_shots() {
    let (ctx, char_id) = make_online_ctx();

    // damage == 0.0 -> "misses" even if is_wrecking is true
    emit_combat_message(&ctx, char_id, "CombatHit1", 500_001, 0.0, 2456, true);
    // negative damage -> "misses"
    emit_combat_message(&ctx, char_id, "CombatHitNeg", 500_002, -1.0, 2456, false);
    // positive damage, non-wrecking -> "hits"
    emit_combat_message(&ctx, char_id, "CombatHit2", 500_003, 125.5, 3186, false);
    // positive damage, wrecking -> "wrecks"
    emit_combat_message(&ctx, char_id, "CombatHit3", 500_004, 450.0, 3188, true);

    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 4);

    let extract_dict_str = |notif: &PendingMachoNotification, key: &str| -> String {
        let PyRep::Dict(entries) = &notif.args[1] else {
            panic!("expected dict");
        };
        entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .and_then(|(_, v)| v.as_str())
            .unwrap_or("")
            .to_string()
    };

    assert_eq!(drained[0].event, "OnCombatMessage");
    assert_eq!(drained[0].id_type, "charid");
    assert_eq!(drained[0].args[0], PyRep::String("CombatHit1".into()));
    assert_eq!(extract_dict_str(&drained[0], "hitQualityText"), "misses");

    assert_eq!(extract_dict_str(&drained[1], "hitQualityText"), "misses");
    assert_eq!(extract_dict_str(&drained[2], "hitQualityText"), "hits");
    assert_eq!(extract_dict_str(&drained[3], "hitQualityText"), "wrecks");

    let PyRep::Dict(ref entries) = drained[3].args[1] else {
        panic!("expected dict");
    };
    assert!(entries.contains(&(PyRep::String("specialObject".into()), PyRep::Integer(500_004))));
    assert!(entries.contains(&(PyRep::String("damage".into()), PyRep::Real(450.0))));
    assert!(entries.contains(&(PyRep::String("type".into()), PyRep::Integer(3188))));
    assert!(entries.contains(&(PyRep::String("weapon".into()), PyRep::Integer(3188))));
    assert!(entries.contains(&(PyRep::String("isWrecking".into()), PyRep::Bool(true))));
}
