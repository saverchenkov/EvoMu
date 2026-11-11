//! Unit tests for `notify_emitters_combat_ess_progression`.

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
fn push_char_notification_with_db_and_progression_combat_emitters() {
    let (ctx, char_id) = make_online_ctx();
    let pend: RwLock<HashMap<i64, Vec<PendingMachoNotification>>> = RwLock::new(HashMap::new());
    let (tx, mut rx) = broadcast::channel(300);

    push_char_notification_with_db(&ctx.db, Some(&pend), Some(&tx), 0, "OnStep", vec![]);
    push_char_notification_with_db(&ctx.db, Some(&pend), Some(&tx), -10, "OnStep", vec![]);
    push_char_notification_with_db(&ctx.db, None, Some(&tx), char_id, "OnStep", vec![]);
    assert!(pend.read().unwrap().is_empty());
    assert!(rx.try_recv().is_err());

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
    assert_eq!(pend.read().unwrap().get(&char_id).unwrap().len(), 256);
    assert_eq!(
        pend.read().unwrap().get(&char_id).unwrap()[0].args,
        vec![PyRep::Integer(1)]
    );

    pend.write().unwrap().clear();
    emit_progression_advanced_with_db(&ctx.db, Some(&pend), None, char_id, 11, 22);
    emit_progression_task_completed_with_db(&ctx.db, Some(&pend), None, char_id, 33);
    assert_eq!(pend.read().unwrap().get(&char_id).unwrap().len(), 2);

    emit_progression_advanced(&ctx, char_id, 44, 55);
    emit_progression_task_completed(&ctx, char_id, 66);
    emit_combat_message(&ctx, char_id, "MsgMiss", 9001, 0.0, 2456, true);
    emit_combat_message(&ctx, char_id, "MsgHit", 9002, 50.0, 2456, false);
    emit_combat_message(&ctx, char_id, "MsgWreck", 9003, 150.0, 2456, true);

    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 5);
    assert_eq!(drained[0].event, "OnProgressionAdvanced");
    assert_eq!(drained[1].event, "OnProgressionTaskCompleted");
    assert_eq!(drained[2].event, "OnCombatMessage");
    assert_eq!(drained[3].event, "OnCombatMessage");
    assert_eq!(drained[4].event, "OnCombatMessage");
}

#[test]
fn station_scanner_mining_damage_and_ess_notifications() {
    let (ctx, char_id) = make_online_ctx();
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;

    emit_char_station_notification(&ctx, char_id, station_id, true);
    emit_char_station_notification(&ctx, char_id, station_id, false);
    emit_cargo_scan_complete(&ctx, char_id, 800_001, vec![(34, 1000), (35, 500)]);
    emit_ship_scan_completed(&ctx, char_id, 800_001, 450.0, 1000.0, vec![(2456, 1)]);
    emit_ore_mined(&ctx, char_id, 1230, 800, 1000, 200, 50, 700_001, 600_001);
    emit_damage_state_change(&ctx, char_id, 100_000_001, 0.75, 0.9, 1.0);
    emit_system_ess_data_changed(&ctx, i64::from(sys_id), PyRep::Integer(1));
    emit_system_dynamic_bounty_state_changed(&ctx, i64::from(sys_id), 1.35);
    emit_ess_main_bank_link_notification(&ctx, sys_id);
    emit_ess_reserve_bank_link_notification(&ctx, sys_id);
    emit_main_bank_player_link_disconnected(&ctx, char_id, sys_id, "OutOfRange", 404);
    emit_reserve_bank_player_link_disconnected(&ctx, char_id, sys_id, 2);
    emit_main_bank_player_hack_success(&ctx, char_id, sys_id, 405);
    emit_reserve_bank_unlock(&ctx, sys_id, 10_000_002);

    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 14);
    assert_eq!(drained[0].event, "OnCharNowInStation");
    assert_eq!(
        drained[0].args,
        vec![PyRep::List(vec![
            PyRep::Integer(char_id),
            PyRep::Integer(1_000_170),
            PyRep::None,
            PyRep::None,
        ])]
    );
    assert_eq!(drained[1].event, "OnCharNoLongerInStation");
    assert_eq!(drained[2].event, "OnCargoScanComplete");
    assert_eq!(
        drained[2].args,
        vec![
            PyRep::Integer(800_001),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(1000)]),
                PyRep::Tuple(vec![PyRep::Integer(35), PyRep::Integer(500)]),
            ]),
        ]
    );
    assert_eq!(drained[3].event, "OnShipScanCompleted");
    assert_eq!(drained[4].event, "OnOreMined");
    assert_eq!(drained[5].event, "OnDamageStateChange");
    assert_eq!(drained[6].event, "OnSystemESSDataChanged");
    assert_eq!(drained[6].id_type, "solarsystemid");
    assert_eq!(drained[7].event, "OnSystemDynamicBountyStateChanged");
    assert_eq!(
        drained[7].args,
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::Real(1.35)]
    );
    assert_eq!(drained[8].event, "OnESSMainBankLinkNotification");
    assert_eq!(drained[9].event, "OnESSReserveBankLinkNotification");
    assert_eq!(drained[10].event, "OnMainBankPlayerLinkDisconnected");
    assert_eq!(drained[11].event, "OnReserveBankPlayerLinkDisconnected");
    assert_eq!(drained[12].event, "OnMainBankPlayerHackSuccess");
    assert_eq!(drained[13].event, "OnReserveBankUnlock");
}

#[test]
fn death_pod_and_system_flag_security_emitters_respect_guards() {
    let (ctx, char_id) = make_online_ctx();
    let sys_id = 30_000_142_u32;

    // Non-positive char_id guards
    emit_kill_notification(&ctx, 0);
    emit_ship_death(&ctx, 0);
    emit_pod_death(&ctx, 0);
    emit_pod_killed(&ctx, 0, 999);
    emit_player_pod_death(&ctx, 0);
    emit_player_pod_respawn(&ctx, 0);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    emit_kill_notification(&ctx, char_id);
    emit_ship_death(&ctx, char_id);
    emit_pod_death(&ctx, char_id);
    emit_pod_killed(&ctx, char_id, 555_001);
    emit_player_pod_death(&ctx, char_id);
    emit_player_pod_respawn(&ctx, char_id);
    emit_system_criminal_flag_updates(&ctx, sys_id, &[1], &[2], &[3]);
    emit_system_disapproval_flag_updates(&ctx, sys_id, &[4], &[5]);
    emit_security_modified(&ctx, sys_id, -0.05, 0.85);

    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 9);
    assert_eq!(drained[0].event, "OnKillNotification");
    assert_eq!(drained[1].event, "OnShipDeath");
    assert_eq!(drained[2].event, "OnPodDeath");
    assert_eq!(drained[3].event, "OnPodKilled");
    assert_eq!(drained[3].args, vec![PyRep::Integer(555_001)]);
    assert_eq!(drained[4].event, "OnPlayerPodDeath");
    assert_eq!(drained[5].event, "OnPlayerPodRespawn");
    assert_eq!(drained[6].event, "OnSystemCriminalFlagUpdates");
    assert_eq!(
        drained[6].args,
        vec![
            PyRep::List(vec![PyRep::Integer(1)]),
            PyRep::List(vec![PyRep::Integer(2)]),
            PyRep::List(vec![PyRep::Integer(3)]),
        ]
    );
    assert_eq!(drained[7].event, "OnSystemDisapprovalFlagUpdates");
    assert_eq!(
        drained[7].args,
        vec![
            PyRep::List(vec![PyRep::Integer(4)]),
            PyRep::List(vec![PyRep::Integer(5)]),
        ]
    );
    assert_eq!(drained[8].event, "OnSecurityModified");
    assert_eq!(
        drained[8].args,
        vec![
            PyRep::Integer(i64::from(sys_id)),
            PyRep::Real(-0.05),
            PyRep::Real(0.85),
        ]
    );
}

#[test]
fn broadcast_delivery_station_alliance_faction_fields_negative_guards_and_empty_payloads() {
    let (ctx, char_id) = make_online_ctx();
    let other_char = 90_000_002_i64;
    let station_id = 60_003_760_i64;
    let other_station = 60_008_494_i64;
    let sys_id = 30_000_142_u32;

    // Seed character with alliance_id & war_faction_id and bring other_char online in the same station/system
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.alliance_id = Some(99_000_010);
            ch.war_faction_id = Some(500_001);
            ch.station_id = Some(station_id);
            ch.solar_system_id = sys_id;
        }
        if let Some(ch2) = snap.characters.get_mut(&other_char) {
            ch2.station_id = Some(station_id);
            ch2.solar_system_id = sys_id;
        }
    });
    ctx.db
        .acquire_online_lock(2, 502, Some(other_char), true, 1_775_400_000)
        .expect("online lock 2");

    // 1. push_char_notification_with_db broadcast delivery & unknown char_id (uid_opt = None)
    let pend: RwLock<HashMap<i64, Vec<PendingMachoNotification>>> = RwLock::new(HashMap::new());
    let (tx, mut rx) = broadcast::channel(16);
    push_char_notification_with_db(
        &ctx.db,
        Some(&pend),
        Some(&tx),
        999_888_777,
        "BroadcastTest",
        vec![PyRep::Integer(7)],
    );
    let (bc_cid, bc_notif) = rx.try_recv().expect("broadcast received");
    assert_eq!(bc_cid, 999_888_777);
    assert_eq!(bc_notif.event, "BroadcastTest");
    assert_eq!(pend.read().unwrap()[&999_888_777].len(), 1);

    // 2. Station notification includes alliance_id & war_faction_id and delivers to both pilots in station
    emit_char_station_notification(&ctx, char_id, station_id, true);
    let d1 = ctx.drain_notifications_for_char(char_id);
    let d2 = ctx.drain_notifications_for_char(other_char);
    assert_eq!(d1.len(), 1);
    assert_eq!(d2.len(), 1);
    assert_eq!(
        d1[0].args,
        vec![PyRep::List(vec![
            PyRep::Integer(char_id),
            PyRep::Integer(1_000_170),
            PyRep::Integer(99_000_010),
            PyRep::Integer(500_001),
        ])]
    );

    // Unknown char_id -> corp=0, alliance=None, war_faction=None; wrong station delivers to neither
    emit_char_station_notification(&ctx, 999_888_777, other_station, false);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // 3. Negative damage with is_wrecking=true still reports "misses"; empty scan payloads & empty flag slices
    emit_combat_message(&ctx, char_id, "MsgNeg", 9004, -5.0, 2456, true);
    emit_cargo_scan_complete(&ctx, char_id, 800_002, vec![]);
    emit_ship_scan_completed(&ctx, char_id, 800_002, 0.0, 0.0, vec![]);
    emit_system_criminal_flag_updates(&ctx, sys_id, &[], &[], &[]);
    emit_system_disapproval_flag_updates(&ctx, sys_id, &[], &[]);

    let d_neg = ctx.drain_notifications_for_char(char_id);
    assert_eq!(d_neg.len(), 5);
    assert_eq!(
        d_neg[0].args[1].dict_get("hitQualityText"),
        Some(&PyRep::String("misses".into()))
    );
    assert_eq!(
        d_neg[1].args,
        vec![PyRep::Integer(800_002), PyRep::List(vec![])]
    );
    assert_eq!(
        d_neg[2].args,
        vec![
            PyRep::Integer(800_002),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::List(vec![]),
        ]
    );
    assert_eq!(d_neg[3].event, "OnSystemCriminalFlagUpdates");
    assert_eq!(d_neg[4].event, "OnSystemDisapprovalFlagUpdates");

    // 4. Negative char_id guards on all death/pod emitters
    let _ = ctx.drain_notifications_for_char(other_char);
    emit_kill_notification(&ctx, -1);
    emit_ship_death(&ctx, -1);
    emit_pod_death(&ctx, -1);
    emit_pod_killed(&ctx, -1, 123);
    emit_player_pod_death(&ctx, -1);
    emit_player_pod_respawn(&ctx, -1);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
}
