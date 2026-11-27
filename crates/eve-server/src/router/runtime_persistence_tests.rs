//! Targeted persistence, cold-restart, and rollback rehydration tests for `#592`:
//! `ServerContext`, `CombatRuntimeState`, and `PublicGatewayRouter` (`QuasarGateway`)
//! runtime state backed by durable SQLite snapshots (`service_state_blobs`,
//! `combat_crimewatch_state`, `quasar_state_blobs`).

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use eve_db::{InventoryFlag, InventoryItemRecord, SystemDynamicState, WorldRepository};
use eve_machonet::{service_roles::ROLE_PLAYER, SessionState};
use eve_proto::quasar::{EvePublicRequest, GoogleAny, TYPE_GOOGLEAPIS_PREFIX};
use eve_sde::SdeRegistry;

use super::context::{ActiveFighterSquadron, ActiveScannerProbe};
use super::{BoundObjectEntry, ServerContext};
use crate::services::combat_crimewatch::{
    char_drone_settings, char_safety_level, char_safety_level_in_snap, flush_combat_runtime_to_db,
    set_char_drone_settings, set_char_safety_level, with_combat_ephemeral,
    with_mut_combat_ephemeral, CombatRuntimeState, DroneBehaviorSettings, SafetyLevel,
    SelfDestructCountdown,
};
use crate::services::fighter_test_helpers;
use crate::services::mining_cycle::{ActiveMiningCycle, MiningKind};
use crate::services::quasar_daily_goals::DailyGoalTemplate;
use crate::services::quasar_social_chat::load_wormhole_speakers_from_db;
use crate::services::space_destiny::record_session_change_timer;
use crate::services::tactical_modules::db_ptr_key;

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

struct TempTestDir(PathBuf);

impl TempTestDir {
    fn new(prefix: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "evomu_{prefix}_{}_{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempTestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| {
        Arc::new(
            SdeRegistry::open(&SdeRegistry::default_db_path())
                .expect("default SDE must open for runtime persistence tests"),
        )
    }))
}

fn make_quasar_session(cid: i64, uid: u64) -> SessionState {
    let mut s = SessionState::new(77);
    s.user_id = Some(uid);
    s.character_id = Some(cid);
    s.corporation_id = Some(98_000_001);
    s.solar_system_id = Some(30_000_142);
    s.role = ROLE_PLAYER;
    s
}

fn make_public_req(type_name: &str, value: Vec<u8>) -> EvePublicRequest {
    EvePublicRequest {
        issued: None,
        correlation_uuid: vec![1, 2, 3, 4],
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "test".into(),
        application_instance_uuid: vec![9, 9, 9],
        authoritative_context: None,
        trace_context: None,
    }
}

#[test]
fn sqlite_restart_preserves_server_context_combat_and_quasar_runtime_state() {
    let dir = TempTestDir::new("restart_592");
    let db_path = dir.path().join("world_restart_592.sqlite");
    let sde = shared_sde();

    let cid = 90_000_777i64;
    let probe_id = 710_000_001i64;
    let fighter_id = 720_000_001i64;
    let ship_id = 100_000_777i64;
    let mod_id = 100_000_888i64;
    let sys_id = 30_000_142u32;

    // ------------------------------------------------------------------
    // Boot 1: mutate gameplay runtime state across all three subsystems
    // ------------------------------------------------------------------
    {
        let db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite boot 1"));
        db.with_mut_snapshot(|snap| {
            snap.system_states
                .entry(sys_id)
                .or_insert_with(|| SystemDynamicState {
                    solar_system_id: sys_id,
                    ..Default::default()
                });
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: 587,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        let mut ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db));

        // 1) ServerContext gameplay runtime state (#543 & #592)
        ctx.active_scanner_probes.write().unwrap().insert(
            probe_id,
            ActiveScannerProbe {
                probe_id,
                probe_type_id: 30_013,
                owner_char_id: cid,
                ship_id,
                launcher_module_id: mod_id,
                solar_system_id: sys_id,
                position: [1_000.0, 2_000.0, 3_000.0],
                destination: [1_500.0, 2_000.0, 3_000.0],
                range_step: 4,
                scan_range_m: 16.0 * 149_597_870_700.0,
                base_sensor_strength: 40.0,
                max_flight_time_s: 3_600.0,
                launched_at_unix: 1_700_000_000,
                state: 1,
                warp_end_ft: Some(133_500_000_000_000_000),
            },
        );
        ctx.scanned_signatures
            .write()
            .unwrap()
            .insert((cid, sys_id, 55));
        ctx.character_scan_progress
            .write()
            .unwrap()
            .insert((cid, sys_id, "ABC-123".into()), 0.75);
        fighter_test_helpers::insert_active_fighter(
            &ctx,
            fighter_id,
            ActiveFighterSquadron {
                ball_id: fighter_id,
                item_id: fighter_id,
                type_id: 23_057,
                owner_char_id: cid,
                parent_ship_id: ship_id,
                tube_index: 0,
                squadron_size: 9,
                target_id: Some(99_999),
                is_abandoned: false,
                mwd_active: true,
            },
        );
        fighter_test_helpers::set_fighter_ability_cooldown(&ctx, fighter_id, 1, 1_700_000_500);
        fighter_test_helpers::set_fighter_ability_charges_used(&ctx, fighter_id, 2, 6);
        fighter_test_helpers::set_fighter_tube_load(&ctx, ship_id, 0, fighter_id);
        ctx.record_module_reactivation_delay(ship_id, mod_id, 12.5);
        ctx.mining_cycles.write().unwrap().insert(
            mod_id,
            ActiveMiningCycle {
                char_id: cid,
                ship_id,
                module_id: mod_id,
                target_id: 88_888,
                solar_system_id: sys_id,
                kind: MiningKind::Asteroid,
                effect_name: "miningLaser".into(),
                cycle_start_ms: 1_700_000_000_000,
                duration_ms: 60_000,
            },
        );
        record_session_change_timer(&ctx, cid, 1_700_000_000, sys_id, 10, "undock");
        ctx.map_landmarks
            .write()
            .unwrap()
            .push((sys_id as i64, "Jita IV".into(), 1.0, 2.0, 3.0));
        {
            let dk = db_ptr_key(&ctx.db);
            let mut tac = ctx.tactical_state.write().unwrap();
            tac.lance_caster_lock_until
                .insert((dk, ship_id), 1_700_000_120);
            tac.tsb_self_penalty_until
                .insert((dk, ship_id), 1_700_000_300);
        }
        ctx.fleet_extras
            .lock()
            .unwrap()
            .fleet_names
            .insert(910_001, "Alpha Fleet".into());
        ctx.social_extras
            .lock()
            .unwrap()
            .delayed_local_speakers
            .insert(31_000_005.to_string(), [cid].into_iter().collect());

        // Transient handles/caches (must stay transient across restart)
        ctx.pending_notifications
            .write()
            .unwrap()
            .insert(cid, Vec::new());
        ctx.bound_objects.write().unwrap().insert(
            99_001,
            BoundObjectEntry {
                bound_id: 99_001,
                session_id: 77,
                user_id: 1,
                character_id: cid,
                service: "ship".into(),
                object_id: ship_id,
                oid_str: "99001".into(),
                ref_id: 0,
            },
        );

        // 2) CombatRuntimeState (#544 & #592) + DB-owned bounty/sys-security/ESS, safety_levels & timer states (#1541)
        db.with_mut_snapshot(|snap| {
            let cw = snap.combat_crimewatch_state.as_inner_mut();
            cw.crimewatch_timer_states
                .insert(cid, (1, 0, 1, 0, 0));
            cw.disapproval_flags.insert(cid, 1_700_000_900);
            cw.sec_status_transactions.insert(
                cid,
                vec![eve_db::PersistedSecStatusTransaction {
                    timestamp_unix: 1_700_000_010,
                    old_sec: 0.0,
                    new_sec: 0.25,
                    delta: 0.25,
                    reason: "NPC Pirate Bounty".into(),
                    solar_system_id: sys_id,
                    event_type_id: 378,
                    other_owner_id: None,
                    other_type_id: Some(17_641),
                }],
            );
            cw.player_bounty_pool.insert(cid, 50_000_000.0);
            cw.modified_system_securities.insert(sys_id, 0.85);
            cw.ess_reserve_unlocked_until
                .insert(sys_id, 1_700_009_000);
            cw.ess_reserve_bank_thefts
                .insert(sys_id, vec![(cid, 250_000.0, 1_700_008_000)]);
            cw.ess_main_bank_thefts
                .insert(sys_id, vec![(cid, 500_000.0, 1_700_008_500)]);
            cw.pending_bounty_ticks.insert((sys_id, cid), 125_000.0);
            cw.bounty_tick_next_flush_unix
                .insert((sys_id, cid), 1_700_001_200);
            cw.record_destroyed_item_id(888_777_666);
        });
        with_mut_combat_ephemeral(&ctx, |st| {
            st.self_destruct_countdowns.insert(
                ship_id,
                SelfDestructCountdown {
                    ship_id,
                    char_id: cid,
                    solar_system_id: sys_id,
                    started_unix: 1_700_000_000,
                    detonate_at_unix: 1_700_000_120,
                },
            );
        });
        flush_combat_runtime_to_db(&ctx);
        set_char_safety_level(&ctx, cid, SafetyLevel::Red);

        // 3) QuasarGateway & managers
        let q_sess = make_quasar_session(cid, 1);
        let rsp = ctx.quasar_gateway.handle_public_request(
            &q_sess,
            &make_public_req("eve.tag.api.RegisterRequest", Vec::new()),
            None,
            false,
            1_700_000_020,
        );
        assert_eq!(rsp.status_code, 200);

        ctx.quasar_gateway
            .set_server_hijack_mode(true, Some(503), Some(1.5));
        ctx.set_free_fast_checkout_plex(true);
        let _ = ctx.quasar_gateway.publish_notice(
            crate::quasar_gateway::NoticeScope::Character(cid as u32),
            GoogleAny {
                type_url: "type.googleapis.com/test.Notice".into(),
                value: vec![1],
            },
            1_700_000_010,
        );
        ctx.quasar_gateway
            .chat_moderation
            .mutes
            .write()
            .unwrap()
            .insert(cid as u32, (1_700_999_999, "spam".into()));
        ctx.quasar_gateway
            .chat_moderation
            .wormhole_speakers
            .write()
            .unwrap()
            .insert((31_000_005, cid as u32));
        ctx.quasar_gateway.chat_moderation.sync_to_db(&db);
        ctx.quasar_gateway.progression.daily_goals.configure_template(DailyGoalTemplate {
            uuid: [0xAB; 16],
            name_message: "Persistent Custom Goal".into(),
            description_message: "Survives restart".into(),
            category: 1,
            career: 3,
            target: 5,
            isk_reward: 500_000,
            evermarks_reward: 500,
            sp_reward: 10_000.0,
            completion_cost_isk: None,
            completion_cost_evermarks: None,
            name_message_id: 0,
            description_message_id: 0,
            help_text_message_id: 0,
            assigner_corporation_id: 0,
        });

        ctx.sync_runtime_to_db();
    }

    // ------------------------------------------------------------------
    // Boot 2: cold restart from same SQLite path
    // ------------------------------------------------------------------
    {
        let db2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite boot 2"));
        let ctx2 = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db2));

        // 1) ServerContext persisted state restored
        {
            let probes = ctx2.active_scanner_probes.read().unwrap();
            assert_eq!(probes.get(&probe_id).map(|p| p.range_step), Some(4));
            assert_eq!(probes.get(&probe_id).map(|p| p.probe_type_id), Some(30_013));
            assert_eq!(probes.get(&probe_id).map(|p| p.state), Some(0));
            assert_eq!(probes.get(&probe_id).and_then(|p| p.warp_end_ft), None);
        }
        assert!(ctx2
            .scanned_signatures
            .read()
            .unwrap()
            .contains(&(cid, sys_id, 55)));
        assert_eq!(
            ctx2.character_scan_progress
                .read()
                .unwrap()
                .get(&(cid, sys_id, "ABC-123".into())),
            Some(&0.75)
        );
        assert_eq!(
            fighter_test_helpers::active_fighter(&ctx2, fighter_id).map(|f| f.squadron_size),
            Some(9)
        );
        assert_eq!(
            fighter_test_helpers::fighter_ability_cooldown(&ctx2, fighter_id, 1),
            Some(1_700_000_500)
        );
        assert_eq!(
            fighter_test_helpers::fighter_ability_charges_used(&ctx2, fighter_id, 2),
            Some(6)
        );
        assert_eq!(
            fighter_test_helpers::loaded_fighter_in_tube(&ctx2, ship_id, 0),
            Some(fighter_id)
        );
        assert!(ctx2.has_any_reactivation_delays_for_test());
        assert!(ctx2.is_module_in_reactivation_delay(ship_id, mod_id));
        // Crash-lost runtime kinds must remain cleared across restart (`#920`, `#875`).
        assert!(ctx2.mining_cycles.read().unwrap().is_empty());
        assert!(crate::router::session_transition::get_session_change_timer(&ctx2, cid).is_some());
        assert!(ctx2
            .map_landmarks
            .read()
            .unwrap()
            .iter()
            .any(|(s, n, ..)| *s == sys_id as i64 && n == "Jita IV"));
        {
            let dk = db_ptr_key(&ctx2.db);
            let tac = ctx2.tactical_state.read().unwrap();
            assert_eq!(
                tac.lance_caster_lock_until.get(&(dk, ship_id)),
                Some(&1_700_000_120)
            );
            assert_eq!(
                tac.tsb_self_penalty_until.get(&(dk, ship_id)),
                Some(&1_700_000_300)
            );
        }
        assert_eq!(
            ctx2.fleet_extras
                .lock()
                .unwrap()
                .fleet_names
                .get(&910_001)
                .map(String::as_str),
            Some("Alpha Fleet")
        );
        assert!(ctx2
            .social_extras
            .lock()
            .unwrap()
            .delayed_local_speakers
            .get("31000005")
            .is_some_and(|s| s.contains(&cid)));

        // Transient state stayed transient across restart
        assert!(
            ctx2.pending_notifications.read().unwrap().is_empty(),
            "pending_notifications must remain transient across restarts"
        );
        assert!(
            ctx2.bound_objects.read().unwrap().is_empty(),
            "bound_objects handle table must remain transient across restarts"
        );

        // 2) CombatRuntimeState persisted & restored + DB-owned bounty/sys-security/ESS & timer fields preserved
        db2.with_snapshot(|snap| {
            let cw = &snap.combat_crimewatch_state;
            assert_eq!(cw.crimewatch_timer_states.get(&cid), Some(&(1, 0, 1, 0, 0)));
            assert_eq!(cw.disapproval_flags.get(&cid), Some(&1_700_000_900));
            assert_eq!(cw.sec_status_transactions.get(&cid).map(Vec::len), Some(1));
            assert_eq!(cw.player_bounty_pool.get(&cid), Some(&50_000_000.0));
            assert_eq!(cw.modified_system_securities.get(&sys_id), Some(&0.85));
            assert_eq!(
                cw.ess_reserve_unlocked_until.get(&sys_id),
                Some(&1_700_009_000)
            );
            assert_eq!(
                cw.ess_reserve_bank_thefts.get(&sys_id),
                Some(&vec![(cid, 250_000.0, 1_700_008_000)])
            );
            assert_eq!(
                cw.ess_main_bank_thefts.get(&sys_id),
                Some(&vec![(cid, 500_000.0, 1_700_008_500)])
            );
            assert_eq!(
                cw.pending_bounty_ticks.get(&(sys_id, cid)),
                Some(&125_000.0)
            );
            assert_eq!(
                cw.bounty_tick_next_flush_unix.get(&(sys_id, cid)),
                Some(&1_700_001_200)
            );
            assert!(cw.is_recently_destroyed_item_id(888_777_666));
        });
        with_combat_ephemeral(&ctx2, |st| {
            assert_eq!(
                st.self_destruct_countdowns
                    .get(&ship_id)
                    .map(|s| s.detonate_at_unix),
                Some(1_700_000_120)
            );
        });
        assert_eq!(
            ctx2.db.with_snapshot(|snap| char_safety_level_in_snap(snap, cid)),
            Some(SafetyLevel::Red)
        );

        // 3) QuasarGateway persisted state restored
        let hijack = ctx2.quasar_gateway.server_hijack_state.read().unwrap().clone();
        assert!(hijack.enabled);
        assert_eq!(hijack.error_code, Some(503));
        assert_eq!(hijack.latency_sec, Some(1.5));
        assert!(ctx2
            .quasar_gateway
            .free_fast_checkout_plex
            .load(std::sync::atomic::Ordering::Relaxed));
        assert!(
            ctx2.quasar_gateway
                .next_notice_seq
                .load(std::sync::atomic::Ordering::Relaxed)
                >= 2
        );
        assert!(ctx2
            .quasar_gateway
            .chat_moderation
            .mutes
            .read()
            .unwrap()
            .contains_key(&(cid as u32)));
        assert!(ctx2
            .quasar_gateway
            .chat_moderation
            .wormhole_speakers
            .read()
            .unwrap()
            .contains(&(31_000_005, cid as u32)));
        assert!(load_wormhole_speakers_from_db(&db2).contains(&(31_000_005, cid as u32)));
        assert!(ctx2
            .quasar_gateway
            .progression
            .daily_goals
            .templates
            .read()
            .unwrap()
            .iter()
            .any(|t| t.uuid == [0xAB; 16]));

        // Clear server_hijack_mode first so eve.tag.api.ListRequest hits the handler
        ctx2.quasar_gateway.set_server_hijack_mode(false, None, None);
        let q_sess = make_quasar_session(cid, 1);
        let list_rsp = ctx2.quasar_gateway.handle_public_request(
            &q_sess,
            &make_public_req("eve.tag.api.ListRequest", Vec::new()),
            None,
            false,
            1_700_000_030,
        );
        assert_eq!(list_rsp.status_code, 200);
        let body = list_rsp.payload.expect("ListResponse payload").value;
        assert!(
            body.windows("fleet_doctrine".len())
                .any(|w| w == b"fleet_doctrine"),
            "registered community tag must survive cold restart"
        );
    }
}

#[test]
fn rollback_reverts_dirty_runtime_state_across_all_caches_and_rehydrates_from_sqlite() {
    let dir = TempTestDir::new("rollback_592");
    let db_path = dir.path().join("world_rollback_592.sqlite");
    let db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
    let sde = shared_sde();
    let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db));

    let cid = 90_000_888i64;
    let ship_id = 100_000_888i64;
    let sys_id = 30_000_142u32;

    // 1. Commit clean baseline across ServerContext, CombatRuntimeState, and QuasarGateway
    ctx.active_scanner_probes.write().unwrap().insert(
        700_001,
        ActiveScannerProbe {
            probe_id: 700_001,
            probe_type_id: 30_013,
            owner_char_id: cid,
            ship_id,
            launcher_module_id: 700_010,
            solar_system_id: sys_id,
            position: [0.0; 3],
            destination: [0.0; 3],
            range_step: 2,
            scan_range_m: 1_000.0,
            base_sensor_strength: 40.0,
            max_flight_time_s: 3_600.0,
            launched_at_unix: 100,
            state: 1,
            warp_end_ft: None,
        },
    );
    set_char_safety_level(&ctx, cid, SafetyLevel::Yellow);
    set_char_drone_settings(
        &ctx,
        cid,
        DroneBehaviorSettings {
            aggressive: false,
            focus_fire: true,
        },
    );
    db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .pending_bounty_ticks
            .insert((sys_id, cid), 10_000.0);
        snap.combat_crimewatch_state
            .disapproval_flags
            .insert(cid, 555_555);
        snap.combat_crimewatch_state
            .sec_status_transactions
            .insert(
                cid,
                vec![eve_db::PersistedSecStatusTransaction {
                    timestamp_unix: 1_700_000_001,
                    old_sec: 1.0,
                    new_sec: 1.2,
                    delta: 0.2,
                    reason: "NPC Pirate Bounty".into(),
                    solar_system_id: sys_id,
                    event_type_id: 378,
                    other_owner_id: None,
                    other_type_id: None,
                }],
            );
    });
    with_mut_combat_ephemeral(&ctx, |st| {
        st.clone_soldier_cooldown_until.insert(sys_id, 555_555);
    });
    ctx.quasar_gateway
        .set_server_hijack_mode(false, None, None);
    let q_sess = make_quasar_session(cid, 2);
    assert_eq!(
        ctx.quasar_gateway
            .handle_public_request(
                &q_sess,
                &make_public_req("eve.tag.api.RegisterRequest", Vec::new()),
                None,
                false,
                1_700_000_000,
            )
            .status_code,
        200
    );
    ctx.sync_runtime_to_db();

    let baseline_commit_count = db.tx_commit_count();

    // 2. Dirty RAM-only caches AND simulate a failing CoW transaction that restores pre-tx working copy
    ctx.active_scanner_probes.write().unwrap().insert(
        700_999,
        ActiveScannerProbe {
            probe_id: 700_999,
            probe_type_id: 30_013,
            owner_char_id: cid,
            ship_id,
            launcher_module_id: 700_010,
            solar_system_id: sys_id,
            position: [0.0; 3],
            destination: [0.0; 3],
            range_step: 6,
            scan_range_m: 9_999.0,
            base_sensor_strength: 40.0,
            max_flight_time_s: 3_600.0,
            launched_at_unix: 200,
            state: 1,
            warp_end_ft: None,
        },
    );
    *ctx.combat_ephemeral.write().unwrap() = {
        let mut guard = CombatRuntimeState::default();
        guard.synced_db_epoch = db.combat_state_epoch();
        guard.clone_soldier_cooldown_until.insert(sys_id, 999_999);
        guard
    };
    *ctx.quasar_gateway.server_hijack_state.write().unwrap() =
        crate::quasar_gateway::QuasarHijackState {
            enabled: true,
            error_code: Some(500),
            latency_sec: Some(9.0),
        };
    assert!(ctx.active_scanner_probes.read().unwrap().contains_key(&700_999));

    // Force an error in try_with_mut_snapshot after mutating snapshot blobs -> CoW restores pre-tx snapshot!
    let res: Result<(), String> = db.try_with_mut_snapshot(|snap| {
        snap.service_state_blobs
            .insert("server_runtime_state".into(), "{}".into());
        snap.cluster_and_runtime.server_runtime_json = None;
        snap.quasar_state_blobs
            .insert("quasar_community_state".into(), "{}".into());
        snap.combat_crimewatch_state
            .safety_levels
            .insert(cid, SafetyLevel::Red as i32);
        snap.combat_crimewatch_state
            .drone_settings
            .insert(cid, (true, false));
        snap.combat_crimewatch_state.npc_timers.insert(cid, 999_999);
        snap.combat_crimewatch_state
            .disapproval_flags
            .insert(cid, 999_999);
        snap.combat_crimewatch_state
            .sec_status_transactions
            .clear();
        snap.combat_crimewatch_state
            .pending_bounty_ticks
            .insert((sys_id, cid), 999_999.0);
        snap.combat_crimewatch_state.extended_runtime_json.clear();
        Err("forced_tx_abort".into())
    });
    assert!(res.is_err());
    assert_eq!(
        db.tx_commit_count(),
        baseline_commit_count,
        "aborted tx must not advance WorldRepository::tx_commit_count"
    );

    // 3. Explicit rehydration restores persisted DB state across CombatRuntimeState, PublicGatewayRouter & ServerContext
    ctx.quasar_gateway.rehydrate_all_from_db();
    ctx.rehydrate_runtime_from_db();
    assert_eq!(char_safety_level(&ctx, cid), Some(SafetyLevel::Yellow));
    assert_eq!(
        char_drone_settings(&ctx, cid),
        DroneBehaviorSettings {
            aggressive: false,
            focus_fire: true,
        }
    );
    db.with_snapshot(|snap| {
        assert_eq!(
            snap.combat_crimewatch_state
                .pending_bounty_ticks
                .get(&(sys_id, cid)),
            Some(&10_000.0)
        );
        assert!(!snap.combat_crimewatch_state.npc_timers.contains_key(&cid));
        assert_eq!(
            snap.combat_crimewatch_state
                .disapproval_flags
                .get(&cid)
                .copied(),
            Some(555_555)
        );
        assert_eq!(
            snap.combat_crimewatch_state
                .sec_status_transactions
                .get(&cid)
                .map(Vec::len),
            Some(1)
        );
    });
    with_combat_ephemeral(&ctx, |st| {
        assert_eq!(
            st.clone_soldier_cooldown_until.get(&sys_id).copied(),
            Some(555_555)
        );
    });
    let list_after_abort = ctx.quasar_gateway.handle_public_request(
        &q_sess,
        &make_public_req("eve.tag.api.ListRequest", Vec::new()),
        None,
        false,
        1_700_000_050,
    );
    assert_eq!(list_after_abort.status_code, 200);
    assert!(!ctx.quasar_gateway.server_hijack_state.read().unwrap().enabled);
    assert!(list_after_abort
        .payload
        .unwrap()
        .value
        .windows("fleet_doctrine".len())
        .any(|w| w == b"fleet_doctrine"));

    let probes = ctx.active_scanner_probes.read().unwrap();
    assert!(
        !probes.contains_key(&700_999),
        "dirty unpersisted probe must be wiped on rehydration"
    );
    assert_eq!(probes.get(&700_001).map(|p| p.range_step), Some(2));
}

#[test]
fn many_scanner_probes_persist_without_blowing_service_state_blob_cap() {
    let dir = TempTestDir::new("probes_blob_cap");
    let db_path = dir.path().join("probes_cap.sqlite");
    let sde = shared_sde();

    {
        let db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
        let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db));
        for i in 0..12i64 {
            let pid = 750_000_000 + i;
            ctx.active_scanner_probes.write().unwrap().insert(
                pid,
                ActiveScannerProbe {
                    probe_id: pid,
                    probe_type_id: 30_013,
                    owner_char_id: 90_000_777,
                    ship_id: 100_000_777,
                    launcher_module_id: 100_000_888,
                    solar_system_id: 30_000_142,
                    position: [1_000.0 * i as f64, 2_000.0, 3_000.0],
                    destination: [1_500.0 * i as f64, 2_000.0, 3_000.0],
                    range_step: 4,
                    scan_range_m: 16.0 * 149_597_870_700.0,
                    base_sensor_strength: 40.0,
                    max_flight_time_s: 3_600.0,
                    launched_at_unix: 1_700_000_000,
                    state: 1,
                    warp_end_ft: Some(133_500_000_000_000_000),
                },
            );
        }
        crate::services::space_probe_scanning::sync_scanner_probes_to_db(&ctx);
        // Must not leave an oversized `server_runtime_state` blob in SQLite.
        let blob_len = db.with_snapshot(|s| {
            s.service_state_blobs
                .get("server_runtime_state")
                .map(|v| v.len())
                .unwrap_or(0)
        });
        assert!(
            blob_len <= eve_db::service_domains::MAX_SERVICE_STATE_BLOB_VALUE_BYTES,
            "server_runtime_state blob must stay within MAX_SERVICE_STATE_BLOB_VALUE_BYTES, got {blob_len}"
        );
    }

    let db2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db2));
    let probes = ctx2.active_scanner_probes.read().unwrap();
    assert_eq!(probes.len(), 12);
    for i in 0..12i64 {
        let p = &probes[&(750_000_000 + i)];
        assert_eq!(p.state, 0);
        assert_eq!(p.warp_end_ft, None);
    }
}

#[test]
fn combat_runtime_rehydrates_on_mut_snapshot_without_clobbering_unflushed_npcs() {
    let sde = shared_sde();
    let dir = TempTestDir::new("combat_mut_snap");
    let db_path = dir.path().join("world.sqlite");
    let db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
    let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db));

    // Seed clean baseline sync epochs on `ctx.combat_ephemeral`.
    with_combat_ephemeral(&ctx, |_| {});
    let sys_id = 30_000_142u32;
    let hunter_id = 90_000_311i64;

    // Simulate an unflushed RAM-only extended runtime mutation alongside an unrelated
    // `with_mut_snapshot` update (e.g., `next_entity_id`), then a direct snapshot mutation
    // updating `bounty_hunter_claimed` (as occurs during whole-snapshot reload / clone).
    ctx.combat_ephemeral
        .write()
        .unwrap()
        .clone_soldier_cooldown_until
        .insert(sys_id, 250);

    db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
    });
    with_combat_ephemeral(&ctx, |st| {
        assert_eq!(st.clone_soldier_cooldown_until.get(&sys_id), Some(&250));
    });

    db.with_mut_snapshot(|snap| {
        let cw = snap.combat_crimewatch_state.as_inner_mut();
        cw.bounty_hunter_claimed.insert(hunter_id, 1_250_000.0);
        cw.disapproval_flags.insert(hunter_id, 1_700_000_500);
        cw.modified_system_securities.insert(sys_id, 0.4);
    });
    assert_eq!(
        crate::services::combat_crimewatch::char_disapproval_flag_until(&ctx, hunter_id),
        Some(1_700_000_500)
    );
    with_combat_ephemeral(&ctx, |st| {
        // Core snapshot sync must not clobber unflushed RAM extended state when `extended_runtime_json` is unchanged.
        assert_eq!(st.clone_soldier_cooldown_until.get(&sys_id), Some(&250));
    });
    with_mut_combat_ephemeral(&ctx, |st| {
        st.clone_soldier_cooldown_until.insert(sys_id, 300);
    });
    flush_combat_runtime_to_db(&ctx);
    db.with_snapshot(|snap| {
        assert_eq!(
            snap.combat_crimewatch_state
                .modified_system_securities
                .get(&sys_id),
            Some(&0.4)
        );
    });
    db.with_snapshot(|snap| {
        assert_eq!(
            snap.combat_crimewatch_state
                .bounty_hunter_claimed
                .get(&hunter_id),
            Some(&1_250_000.0)
        );
    });

    // When `extended_runtime_json` itself changes in `with_mut_snapshot`, full rehydration runs.
    let mut replacement = CombatRuntimeState::default();
    replacement.clone_soldier_cooldown_until.insert(sys_id, 750);
    let replacement_persisted = replacement.to_persisted();
    db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state.extended_runtime_json =
            replacement_persisted.extended_runtime_json.clone();
    });
    with_combat_ephemeral(&ctx, |st| {
        assert_eq!(st.clone_soldier_cooldown_until.get(&sys_id), Some(&750));
    });
}

#[tokio::test]
async fn w4_mutant_killers_context_runtime_is_empty_ewar_probes_legacy_and_sync() {
    use crate::router::context::{ActiveMoonSurveyProbe, ModuleCooldownEntry};
    use crate::services::cluster_macho_misc_services::{
        LandingPadRecord, LauncherRecord, StarterLauncherRecord,
    };
    use crate::services::fleet_social::fleet_social_fleets::FleetAdvertState;
    use crate::services::mining_cycle::{ActiveMiningCycle, MiningKind};
    use crate::services::space_destiny::SessionChangeTimerState;
    use crate::services::space_mining_harvest::PhasedAsteroidFieldState;
    use crate::services::tactical_module_mechanics::{ActiveInterdictionProbe, LauncherFireStamp};
    use crate::services::tactical_modules::{ActiveEntosisLinkState, ProjectedRemoteAssistance};
    use crate::services::tactical_modules_probes::{ActiveWubbleDebuff, PendingWubbleProbe};
    use eve_db::{
        FleetSetupRecord, HorizonSiegePhase, OfferedLoadSlot, OwnerNoteRecord,
        PendingZarzakhTollQuote,
    };
    use eve_dogma::{DogmaItemState, ProjectedEwarEffect};
    use eve_proto::PyRep;
    use glam::DVec3;
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::sync::atomic::Ordering;

    let dir = TempTestDir::new("w4_runtime_persistence_killers");
    let db_path = dir.path().join("w4_runtime.sqlite");
    let sde = shared_sde();

    // Helper: fresh empty DB + context where server_runtime_json starts None
    let fresh_ctx = |name: &str| {
        let p = dir.path().join(name);
        let db = Arc::new(WorldRepository::open_sqlite(&p).expect("open sqlite"));
        let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db));
        ctx.active_moon_probes.write().unwrap().clear();
        ctx.scanned_signatures.write().unwrap().clear();
        ctx.character_scan_progress.write().unwrap().clear();
        fighter_test_helpers::clear_active_fighters(&ctx);
        fighter_test_helpers::clear_all_fighter_cooldowns(&ctx);
        fighter_test_helpers::clear_all_fighter_charges(&ctx);
        fighter_test_helpers::clear_all_fighter_tubes(&ctx);
        ctx.clear_all_reactivation_delays();
        ctx.mining_cycles.write().unwrap().clear();
        ctx.phased_asteroid_fields.write().unwrap().clear();
        crate::router::session_transition::clear_all_session_change_timers(&ctx);
        ctx.systems.clear_all_ewar_state_sync();
        ctx.map_landmarks.write().unwrap().clear();
        ctx.global_config_values.write().unwrap().clear();
        ctx.fw_idle_last_step_unix.write().unwrap().clear();
        ctx.combat_rng_seq.store(0, Ordering::Relaxed);
        ctx.cluster_tick_last_unix.store(0, Ordering::Relaxed);
        ctx.cluster_tick_persisted_unix.store(0, Ordering::Relaxed);
        *ctx.tactical_state.write().unwrap() = Default::default();
        *ctx.fleet_extras.lock().unwrap() = Default::default();
        *ctx.social_extras.lock().unwrap() = Default::default();
        *ctx.cluster_misc_state.lock().unwrap() = Default::default();
        *ctx.cluster_state.write().unwrap() = Default::default();
        *ctx.alert_telemetry.lock().unwrap() = Default::default();
        ctx.persist_server_runtime_to_db();
        (db, ctx)
    };

    // 1. Crash-lost `active_ewar_starts` is NOT persisted into `system_checkpoint` and clears on hydrate
    {
        let (db, ctx) = fresh_ctx("ewar_kinds.sqlite");
        assert!(db.with_snapshot(|s| s.cluster_and_runtime.server_runtime_json.is_none()));
        let _ = ctx.systems.get_or_spawn(30_000_142);
        ctx.record_active_ewar_start_for_test(
            Some(30_000_142),
            101,
            202,
            303,
            "warpScramblerMWD",
        );
        ctx.persist_server_runtime_to_db();
        assert!(db.with_snapshot(|s| s.cluster_and_runtime.server_runtime_json.is_none()));
        assert!(!db
            .with_sqlite_conn(|conn| eve_db::sqlite_store::system_checkpoint::has_any_system_checkpoints(conn))
            .expect("check checkpoints empty"));
        assert!(!db
            .with_sqlite_conn(|conn| eve_db::sqlite_store::system_checkpoint::has_any_system_checkpoints(conn))
            .expect("check checkpoints"));
        ctx.hydrate_server_runtime_from_db();
        assert_eq!(ctx.ewar_starts_count_for_test(), 0);
    }

    // 2. Durable vs Crash-lost fields of `TacticalModulesState` (foreign tag ignore + roundtrip)
    {
        let (db, ctx) = fresh_ctx("tactical_fields.sqlite");
        let dk = crate::services::tactical_modules::db_ptr_key(&ctx.db);
        let inst = ctx.instance_id as usize;
        // Foreign tag must be ignored
        ctx.tactical_state
            .write()
            .unwrap()
            .lance_caster_lock_until
            .insert((999_888_777, 1), 100);
        ctx.persist_server_runtime_to_db();
        assert!(
            !db.with_sqlite_conn(|conn| eve_db::sqlite_store::system_checkpoint::has_any_system_checkpoints(conn))
                .expect("check checkpoints"),
            "foreign tag must not persist"
        );
        ctx.tactical_state.write().unwrap().lance_caster_lock_until.clear();

        let assert_tac_persists = |label: &str, set: &dyn Fn(&mut crate::services::tactical_modules::TacticalModulesState, usize)| {
            {
                let mut ts = ctx.tactical_state.write().unwrap();
                set(&mut ts, dk);
            }
            ctx.persist_server_runtime_to_db();
            assert!(
                db.with_sqlite_conn(|conn| eve_db::sqlite_store::system_checkpoint::has_any_system_checkpoints(conn))
                    .expect("check checkpoints"),
                "tactical field `{label}` must persist into system_checkpoint"
            );
            let ctx2 = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db));
            ctx2.hydrate_server_runtime_from_db();
            *ctx.tactical_state.write().unwrap() = Default::default();
            ctx.persist_server_runtime_to_db();
            assert!(
                !db.with_sqlite_conn(|conn| eve_db::sqlite_store::system_checkpoint::has_any_system_checkpoints(conn))
                    .expect("check checkpoints"),
                "clearing `{label}` must clear system_checkpoint rows"
            );
        };

        assert_tac_persists("lance_caster_lock_until_inst", &|ts, _| {
            ts.lance_caster_lock_until.insert((inst, 11), 500);
        });
        assert_tac_persists("lance_victim_debuff_until", &|ts, k| {
            ts.lance_victim_debuff_until.insert((k, 12), 600);
        });
        assert_tac_persists("tsb_self_penalty_until", &|ts, k| {
            ts.tsb_self_penalty_until.insert((k, 18), 700);
        });
        assert_tac_persists("clone_vat_bay_activated_ships", &|ts, k| {
            ts.clone_vat_bay_activated_ships.insert((k, 19));
        });
        assert_tac_persists("system_sim_time_s", &|ts, k| {
            ts.system_sim_time_s.insert((k, 30_000_142), 12.5);
        });

        let assert_tac_crash_lost = |label: &str, set: &dyn Fn(&mut crate::services::tactical_modules::TacticalModulesState, usize)| {
            {
                let mut ts = ctx.tactical_state.write().unwrap();
                set(&mut ts, dk);
            }
            ctx.persist_server_runtime_to_db();
            assert!(
                !db.with_sqlite_conn(|conn| eve_db::sqlite_store::system_checkpoint::has_any_system_checkpoints(conn))
                    .expect("check checkpoints"),
                "crash-lost tactical field `{label}` must NOT persist into system_checkpoint"
            );
            *ctx.tactical_state.write().unwrap() = Default::default();
        };
        assert_tac_crash_lost("remote_assistance", &|ts, k| {
            ts.remote_assistance.insert(
                (k, 10),
                vec![ProjectedRemoteAssistance {
                    source_ship_id: 1,
                    module_id: 2,
                    module_type_id: 3,
                    max_target_range_bonus_pct: 5.0,
                    scan_resolution_bonus_pct: 5.0,
                    sensor_strength_bonus_pct: 5.0,
                    optimal_range_bonus_pct: 5.0,
                    falloff_bonus_pct: 5.0,
                    tracking_speed_bonus_pct: 5.0,
                }],
            );
        });
        assert_tac_crash_lost("entosis_active_links", &|ts, k| {
            ts.entosis_active_links.insert(
                (k, 13),
                ActiveEntosisLinkState {
                    ship_id: 13,
                    char_id: 1,
                    side_id: 2,
                    module_id: 3,
                    target_id: 4,
                    cycle_index: 1,
                    cycle_duration_s: 300.0,
                    started_unix: 100,
                    expires_unix: 400,
                    is_capital: false,
                },
            );
        });
        assert_tac_crash_lost("entosis_target_cycles", &|ts, k| {
            ts.entosis_target_cycles.insert((k, 14, 15), 2);
        });
        assert_tac_crash_lost("pending_wubble_probes", &|ts, k| {
            ts.pending_wubble_probes.push((
                k,
                PendingWubbleProbe {
                    probe_id: 16,
                    solar_system_id: 30_000_142,
                    position: DVec3::ZERO,
                    radius_m: 15_000.0,
                    remaining_arming_s: 1.0,
                    duration_s: 30.0,
                    velocity_multiplier: 0.7,
                    detonated: false,
                },
            ));
        });
        assert_tac_crash_lost("active_wubble_debuffs", &|ts, k| {
            ts.active_wubble_debuffs.insert(
                (k, 17),
                ActiveWubbleDebuff {
                    remaining_s: 12.0,
                    velocity_multiplier: 0.7,
                },
            );
        });
        assert_tac_crash_lost("active_interdiction_probes", &|ts, k| {
            ts.active_interdiction_probes.insert(
                (k, 20),
                ActiveInterdictionProbe {
                    probe_id: 20,
                    probe_type_id: 22778,
                    solar_system_id: 30_000_142,
                    remaining_s: 60.0,
                    is_wubble: false,
                },
            );
        });
        assert_tac_crash_lost("launcher_last_fire", &|ts, k| {
            ts.launcher_last_fire.insert(
                (k, 21),
                LauncherFireStamp {
                    fired_unix: 100,
                    fired_tick: 5,
                    fired_sim_s: 5.0,
                },
            );
        });
    }

    // 3. Every Checkpointed vs CrashLost field in `collect_checkpoint_batch_from_ctx`
    {
        let (db, ctx) = fresh_ctx("runtime_is_empty.sqlite");
        let check_one = |label: &str, set: &dyn Fn(&ServerContext), clear: &dyn Fn(&ServerContext)| {
            set(&ctx);
            ctx.persist_server_runtime_to_db();
            assert!(
                db.with_sqlite_conn(|conn| eve_db::sqlite_store::system_checkpoint::has_any_system_checkpoints(conn))
                    .expect("check checkpoints"),
                "field `{label}` must write rows into system_checkpoint"
            );
            clear(&ctx);
            ctx.persist_server_runtime_to_db();
            assert!(
                !db.with_sqlite_conn(|conn| eve_db::sqlite_store::system_checkpoint::has_any_system_checkpoints(conn))
                    .expect("check checkpoints"),
                "clearing `{label}` must leave system_checkpoint empty"
            );
        };
        let check_crash_lost = |label: &str, set: &dyn Fn(&ServerContext), clear: &dyn Fn(&ServerContext)| {
            set(&ctx);
            ctx.persist_server_runtime_to_db();
            assert!(
                !db.with_sqlite_conn(|conn| eve_db::sqlite_store::system_checkpoint::has_any_system_checkpoints(conn))
                    .expect("check checkpoints"),
                "crash-lost field `{label}` must NOT write rows into system_checkpoint"
            );
            clear(&ctx);
        };

        check_one(
            "active_moon_probes",
            &|c| {
                c.active_moon_probes.write().unwrap().push(ActiveMoonSurveyProbe {
                    probe_id: 1,
                    probe_type_id: 22778,
                    owner_char_id: 90_000_001,
                    solar_system_id: 30_000_142,
                    hit_moon_id: None,
                    arrival_unix_secs: 1000,
                });
            },
            &|c| c.active_moon_probes.write().unwrap().clear(),
        );
        check_one(
            "scanned_signatures",
            &|c| {
                c.scanned_signatures.write().unwrap().insert((1, 30_000_142, 2));
            },
            &|c| c.scanned_signatures.write().unwrap().clear(),
        );
        check_one(
            "character_scan_progress",
            &|c| {
                c.character_scan_progress
                    .write()
                    .unwrap()
                    .insert((1, 30_000_142, "ABC-123".into()), 0.5);
            },
            &|c| c.character_scan_progress.write().unwrap().clear(),
        );
        let _ = ctx.systems.get_or_spawn(30_000_142);
        check_one(
            "module_reactivation_cooldowns",
            &|c| {
                c.seed_module_cooldown_entry_for_test(
                    1,
                    2,
                    0,
                    ModuleCooldownEntry {
                        recorded_unix_f64: 100.0,
                        delay_s: 10.0,
                        solar_system_id: Some(30_000_142),
                        recorded_sys_sim_s: 5.0,
                    },
                );
            },
            &|c| c.clear_all_reactivation_delays(),
        );
        check_crash_lost(
            "mining_cycles",
            &|c| {
                c.mining_cycles.write().unwrap().insert(
                    1,
                    ActiveMiningCycle {
                        char_id: 1,
                        ship_id: 2,
                        module_id: 1,
                        target_id: 3,
                        solar_system_id: 30_000_142,
                        kind: MiningKind::Asteroid,
                        effect_name: "miningLaser".into(),
                        cycle_start_ms: 100,
                        duration_ms: 60_000,
                    },
                );
            },
            &|c| c.mining_cycles.write().unwrap().clear(),
        );
        check_one(
            "phased_asteroid_fields",
            &|c| {
                c.phased_asteroid_fields.write().unwrap().insert(
                    30_000_142,
                    PhasedAsteroidFieldState::new("PH-1".into(), 30_000_142),
                );
            },
            &|c| c.phased_asteroid_fields.write().unwrap().clear(),
        );
        // `session_change_timers` now persists via `DOMAIN_SP6_LIFECYCLE` (`domain_rows`),
        // NOT via `system_checkpoint` / `persist_server_runtime_to_db`.
        check_crash_lost(
            "session_change_timers",
            &|c| {
                crate::router::session_transition::seed_session_change_timer(
                    c,
                    1,
                    SessionChangeTimerState {
                        expires_unix: 1000,
                        system_id: 30_000_142,
                        started_tick: 1,
                        reason: "dock".into(),
                    },
                );
            },
            &|c| crate::router::session_transition::clear_all_session_change_timers(c),
        );
        check_crash_lost(
            "projected_ewar",
            &|c| {
                if let Some(h) = c.systems.get_handle(30_000_142) {
                    let _ = h.upsert_projected_ewar_sync(
                        20,
                        1,
                        ProjectedEwarEffect {
                            source_module_id: 10,
                            source_module_type_id: 527,
                            charge_type_id: None,
                            distance: 1000.0,
                            overloaded: false,
                            effective_module: DogmaItemState::from_sde(10, 587, &c.sde),
                        },
                    );
                }
            },
            &|c| c.systems.clear_all_ewar_state_sync(),
        );
        check_crash_lost(
            "projected_ewar_sources",
            &|c| {
                c.seed_ewar_source_for_test(Some(30_000_142), 10, 20);
            },
            &|c| c.systems.clear_all_ewar_state_sync(),
        );
        check_crash_lost(
            "active_ewar_starts",
            &|c| {
                c.record_active_ewar_start_for_test(Some(30_000_142), 1, 2, 3, "ecm");
            },
            &|c| c.systems.clear_all_ewar_state_sync(),
        );
        check_one(
            "map_landmarks",
            &|c| {
                c.map_landmarks
                    .write()
                    .unwrap()
                    .push((30_000_142, "LM".into(), 1.0, 2.0, 3.0));
            },
            &|c| c.map_landmarks.write().unwrap().clear(),
        );
        // Fleet extras sub-fields
        check_one(
            "fleet_extras.fleet_names",
            &|c| {
                c.fleet_extras.lock().unwrap().fleet_names.insert(1, "F".into());
            },
            &|c| {
                c.fleet_extras.lock().unwrap().fleet_names.clear();
            },
        );
        check_one(
            "fleet_extras.adverts",
            &|c| {
                c.fleet_extras.lock().unwrap().adverts.insert(
                    1,
                    FleetAdvertState {
                        fleet_id: 1,
                        leader_id: 2,
                        fleet_name: "Ad".into(),
                        description: "".into(),
                        solar_system_id: 30_000_142,
                        invite_scope: 1,
                        auto_join: true,
                        min_standing: 0.0,
                        min_security: -10.0,
                        allowed_entities: HashSet::new(),
                        _created_unix: 100,
                    },
                );
            },
            &|c| {
                c.fleet_extras.lock().unwrap().adverts.clear();
            },
        );
        check_one(
            "fleet_extras.saved_setups",
            &|c| {
                c.fleet_extras.lock().unwrap().saved_setups.insert(
                    1,
                    HashMap::from([(
                        "Std".into(),
                        FleetSetupRecord {
                            setup_name: "Std".into(),
                            motd: "".into(),
                            is_free_move: false,
                            max_size: 256,
                            default_squad_id: None,
                            wings: Vec::new(),
                        },
                    )]),
                );
            },
            &|c| {
                c.fleet_extras.lock().unwrap().saved_setups.clear();
            },
        );

        // Social extras persisted sub-fields
        check_one(
            "social_extras.delayed_local_speakers",
            &|c| {
                c.social_extras
                    .lock()
                    .unwrap()
                    .delayed_local_speakers
                    .insert("31000001".into(), HashSet::from([1]));
            },
            &|c| {
                c.social_extras.lock().unwrap().delayed_local_speakers.clear();
            },
        );

        // 4. Alert telemetry, ephemeral runtime fields (`calls_received`, `bean_counts`, `fw_idle_last_step_unix`,
        //    `combat_rng_seq`, `cluster_tick_last_unix`), and separately-persisted domains (`global_config_values`,
        //    `cluster_misc_state`) remain excluded from `server_runtime_json`
        ctx.cluster_state.write().unwrap().calls_received.insert("svc".into(), 3);
        ctx.alert_telemetry.lock().unwrap().bean_counts.insert(1, 5);
        ctx.global_config_values.write().unwrap().insert("k".into(), PyRep::Integer(1));
        ctx.fw_idle_last_step_unix.write().unwrap().insert(1, 500);
        ctx.combat_rng_seq.store(7, Ordering::Relaxed);
        ctx.cluster_tick_last_unix.store(1_700_000_123, Ordering::Relaxed);
        {
            let mut cm = ctx.cluster_misc_state.lock().unwrap();
            cm.char_settings
                .insert(1, BTreeMap::from([("ui".into(), PyRep::Bool(true))]));
            cm.landing_pads.push(LandingPadRecord {
                item_id: 1,
                type_id: 30_000,
                built_type_id: 0,
                solar_system_id: 30_000_142,
                faction_id: 500_001,
                is_built: true,
                link_timestamp: 100,
                is_disrupted: false,
                position: DVec3::ZERO,
                required_materials: BTreeMap::new(),
                contributed_materials: BTreeMap::new(),
            });
            cm.launchers.push(LauncherRecord {
                item_id: 2,
                type_id: 30_001,
                solar_system_id: 30_000_142,
                faction_id: 500_001,
                linked_pad_ids: Vec::new(),
                position: DVec3::ZERO,
            });
            cm.starter_launchers.push(StarterLauncherRecord {
                item_id: 3,
                type_id: 30_002,
                solar_system_id: 30_000_142,
                faction_id: 500_001,
                staging_system_id: 30_000_142,
                staging_station_id: 60_003_760,
                position: DVec3::ZERO,
            });
            cm.offered_load_slots.insert(
                1,
                OfferedLoadSlot {
                    slot_key: "s1".into(),
                    original_system_id: 30_000_142,
                    offered_system_ids: vec![30_000_144],
                    expires_unix: 1000,
                },
            );
            cm.zarzakh_toll_quotes.insert(
                1,
                PendingZarzakhTollQuote {
                    from_gate_id: 1,
                    to_gate_id: 2,
                    ship_id: 3,
                    quoted_toll_cents: eve_db::IskCents(10_000),
                    expires_unix: 9999,
                },
            );
            cm.zarzakh_horizon_siege.phase = HorizonSiegePhase::Warning;
        }
        ctx.persist_server_runtime_to_db();
        assert!(db.with_snapshot(|s| s.cluster_and_runtime.server_runtime_json.is_none()));
        assert!(!db
            .with_sqlite_conn(|conn| eve_db::sqlite_store::system_checkpoint::has_any_system_checkpoints(conn))
            .expect("check checkpoints empty"));
        *ctx.cluster_state.write().unwrap() = Default::default();
        *ctx.alert_telemetry.lock().unwrap() = Default::default();
        ctx.global_config_values.write().unwrap().clear();
        ctx.fw_idle_last_step_unix.write().unwrap().clear();
        ctx.combat_rng_seq.store(0, Ordering::Relaxed);
        ctx.cluster_tick_last_unix.store(0, Ordering::Relaxed);
        *ctx.cluster_misc_state.lock().unwrap() = Default::default();
        ctx.persist_server_runtime_to_db();
    }

    // 5. `hydrate_server_runtime_from_db`: scanner probes (`state < 0` dropped, `0` kept, `> 0` reset to `0`),
    //    `owner_notes` (`character_id == -79`), legacy blob cleanup, and `probes_differ`!
    {
        let db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
        let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db));
        ctx.active_moon_probes.write().unwrap().clear();
        ctx.active_scanner_probes.write().unwrap().clear();

        let make_probe = |pid: i64, state: i32| ActiveScannerProbe {
            probe_id: pid,
            probe_type_id: 30_013,
            owner_char_id: 90_000_001,
            ship_id: 100_000_001,
            launcher_module_id: 100_000_002,
            solar_system_id: 30_000_142,
            position: [0.0; 3],
            destination: [0.0; 3],
            range_step: 4,
            scan_range_m: 1000.0,
            base_sensor_strength: 40.0,
            max_flight_time_s: 3600.0,
            launched_at_unix: 100,
            state,
            warp_end_ft: None,
        };
        db.with_mut_snapshot(|snap| {
            snap.sp6_lifecycle.active_scanner_probes.clear();
            snap.sp6_lifecycle.active_scanner_probes.insert(800_001, make_probe(800_001, -1));
            snap.sp6_lifecycle.active_scanner_probes.insert(800_002, make_probe(800_002, 0));
            snap.sp6_lifecycle.active_scanner_probes.insert(800_003, make_probe(800_003, 2));
            snap.service_state_blobs
                .insert("server_runtime_state".into(), "{\"stale\":1}".into());
            snap.owner_notes.insert(
                1,
                OwnerNoteRecord {
                    note_id: 1,
                    character_id: -79,
                    item_id: 5001,
                    label: "Landmark-Sys".into(),
                    note: "1.0,2.0,3.0".into(),
                    timestamp_unix: 100,
                },
            );
            snap.owner_notes.insert(
                2,
                OwnerNoteRecord {
                    note_id: 2,
                    character_id: 90_000_001,
                    item_id: 5002,
                    label: "PersonalNote".into(),
                    note: "hello".into(),
                    timestamp_unix: 100,
                },
            );
        });

        ctx.hydrate_server_runtime_from_db();
        {
            let probes = ctx.active_scanner_probes.read().unwrap();
            assert!(!probes.contains_key(&800_001), "state < 0 probe must be dropped");
            assert_eq!(probes.get(&800_002).map(|p| p.state), Some(0));
            assert_eq!(probes.get(&800_003).map(|p| p.state), Some(0), "active probe must recover to state=0");
        }

        // Persist clears legacy `server_runtime_state` blob and detects probe content diff (`probes_differ`)
        ctx.persist_server_runtime_to_db();
        assert!(db.with_snapshot(|s| !s.service_state_blobs.contains_key("server_runtime_state")));
        ctx.active_scanner_probes
            .write()
            .unwrap()
            .get_mut(&800_002)
            .unwrap()
            .scan_range_m = 2000.0;
        ctx.sync_runtime_to_db();
        let saved_range = db.with_snapshot(|s| {
            s.sp6_lifecycle
                .active_scanner_probes
                .get(&800_002)
                .map(|p| p.scan_range_m)
        });
        assert_eq!(saved_range, Some(2000.0));

        // Second persist without change hits `already_matches` and still clears any re-injected legacy blob
        db.with_mut_snapshot(|snap| {
            snap.service_state_blobs
                .insert("server_runtime_state".into(), "{}".into());
        });
        ctx.persist_server_runtime_to_db();
        assert!(db.with_snapshot(|s| !s.service_state_blobs.contains_key("server_runtime_state")));
        assert!(db.with_snapshot(|s| s.owner_notes.contains_key(&2)));
    }
}

#[test]
fn from_config_with_static_data_gates_store_catalog_seeding_on_seed_demo() {
    let sde = shared_sde();
    let descriptors = eve_proto::quasar::QuasarDescriptorRegistry::open_default_or_empty();

    let mut prod_cfg = crate::config::ServerConfig::default();
    prod_cfg.storage.backend = "memory".into();
    prod_cfg.feature_flags.seed_demo = false;
    let prod_ctx = ServerContext::from_config_with_static_data(
        &prod_cfg,
        Arc::clone(&sde),
        Arc::clone(&descriptors),
    )
    .expect("from_config prod");
    assert!(
        prod_ctx
            .db
            .with_snapshot(|s| s.quasar_commerce.nes_store_catalog.is_none()),
        "non-demo ServerContext::from_config_with_static_data must not synthesize NES offers at boot"
    );

    let mut demo_cfg = crate::config::ServerConfig::default();
    demo_cfg.storage.backend = "memory".into();
    demo_cfg.feature_flags.seed_demo = true;
    let demo_ctx = ServerContext::from_config_with_static_data(
        &demo_cfg,
        Arc::clone(&sde),
        descriptors,
    )
    .expect("from_config demo");
    assert!(
        demo_ctx
            .db
            .with_snapshot(|s| s.quasar_commerce.nes_store_catalog.is_some()),
        "seed_demo = true should seed NES demo catalog on boot"
    );
}

#[test]
fn legacy_runtime_snapshot_with_location_change_invuln_fields_loads_cleanly() {
    let dir = TempTestDir::new("legacy_invuln_snap");
    let sde = shared_sde();

    // 1. Pre-Step-2B monolith JSON containing both `location_change_invuln` and
    //    `session_change_timers.*.location_change_invul_until` loads cleanly.
    {
        let db_path = dir.path().join("legacy_monolith.sqlite");
        let db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open legacy sqlite"));
        let legacy_json = r#"{
            "session_change_timers": {
                "90000001": {
                    "expires_unix": 1800000010,
                    "system_id": 30000142,
                    "started_tick": 42,
                    "reason": "stargate_jump",
                    "location_change_invul_until": 1800000060
                }
            },
            "location_change_invuln": {
                "90000001": 1800000060
            },
            "map_landmarks": [[1, "Gate-Ref", 10.0, 20.0, 30.0]]
        }"#;
        db.with_mut_snapshot(|snap| {
            snap.cluster_and_runtime.server_runtime_json = Some(legacy_json.to_string());
        });

        let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db));
        ctx.hydrate_server_runtime_from_db();

        {
            let st = crate::router::session_transition::get_session_change_timer(&ctx, 90_000_001)
                .expect("legacy session_change_timers entry must still load cleanly");
            assert_eq!(st.expires_unix, 1_800_000_010);
            assert_eq!(st.system_id, 30_000_142);
            assert_eq!(st.started_tick, 42);
            assert_eq!(st.reason, "stargate_jump");
        }
        assert_eq!(ctx.map_landmarks.read().unwrap().len(), 1);
    }

    // 2. Pre-Step-2B SQLite `system_checkpoint` rows containing legacy `kind = "location_change_invuln"`
    //    and `kind = "session_change_timers"` payloads with `location_change_invul_until` migrate cleanly
    //    into `snap.sp6_lifecycle.session_change_timers` on DB open, while newer `domain_rows` win on collision.
    {
        let db_path = dir.path().join("legacy_checkpoints.sqlite");
        {
            let db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open cp sqlite"));
            db.with_sqlite_conn(|conn| {
                eve_db::sqlite_store::system_checkpoint::ensure_system_checkpoint_schema(conn)?;
                conn.execute(
                    "INSERT INTO domain_rows (domain, key, value) VALUES (?1, ?2, ?3)",
                    rusqlite::params![
                        eve_db::sqlite_store::domain_rows::DOMAIN_SP6_LIFECYCLE,
                        "session_change_timers/90000003",
                        r#"{"expires_unix":1900000999,"system_id":30000142,"started_tick":99,"reason":"domain_wins"}"#,
                    ],
                )
                .map_err(|e| e.to_string())?;
                conn.execute(
                    "INSERT INTO system_checkpoint (system_id, kind, key, value, updated_unix)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        0_u32,
                        "location_change_invuln",
                        "90000001",
                        "1800000060",
                        1_800_000_000_u64,
                    ],
                )
                .map_err(|e| e.to_string())?;
                conn.execute(
                    "INSERT INTO system_checkpoint (system_id, kind, key, value, updated_unix)
                     VALUES (?1, ?2, ?3, ?4, ?5), (?1, ?2, ?6, ?7, ?5)",
                    rusqlite::params![
                        0_u32,
                        "session_change_timers",
                        "90000002",
                        r#"{"expires_unix":1800000020,"system_id":30000144,"started_tick":7,"reason":"jump_drive","location_change_invul_until":1800000075}"#,
                        1_800_000_000_u64,
                        "90000003",
                        r#"{"expires_unix":1800000005,"system_id":30000144,"started_tick":1,"reason":"stale_cp"}"#,
                    ],
                )
                .map_err(|e| e.to_string())?;
                Ok::<(), String>(())
            })
            .expect("seed legacy system_checkpoint rows");
        }

        let db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen migrated cp sqlite"));
        let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db));
        ctx.hydrate_server_runtime_from_db();

        let st = crate::router::session_transition::get_session_change_timer(&ctx, 90_000_002)
            .expect("session_change_timers checkpoint row with legacy field must migrate cleanly");
        assert_eq!(st.expires_unix, 1_800_000_020);
        assert_eq!(st.system_id, 30_000_144);
        assert_eq!(st.reason, "jump_drive");

        let winner = crate::router::session_transition::get_session_change_timer(&ctx, 90_000_003)
            .expect("colliding domain_rows timer must be retained");
        assert_eq!(winner.expires_unix, 1_900_000_999);
        assert_eq!(winner.reason, "domain_wins");

        let remaining_cp: i64 = db
            .with_sqlite_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM system_checkpoint WHERE kind = 'session_change_timers'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())
            })
            .expect("count remaining legacy checkpoint timers");
        assert_eq!(remaining_cp, 0);
    }

    // 3. Recorded session_change_timer survives full DB reload via domain_rows WITHOUT calling
    //    `persist_server_runtime_to_db` and writes zero rows to `system_checkpoint`.
    {
        let db_path = dir.path().join("domain_rows_no_persist_runtime.sqlite");
        let cid = 90_000_777_i64;
        {
            let db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
            let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db));
            record_session_change_timer(&ctx, cid, 1_800_000_500, 30_000_142, 21, "stargate_jump");
            let has_cp = db
                .with_sqlite_conn(|conn| {
                    eve_db::sqlite_store::system_checkpoint::has_any_system_checkpoints(conn)
                })
                .expect("check system_checkpoint empty");
            assert!(
                !has_cp,
                "record_session_change_timer must NOT write to system_checkpoint"
            );
        }

        let db2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
        let ctx2 = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db2));
        let reloaded = crate::router::session_transition::get_session_change_timer(&ctx2, cid)
            .expect("session_change_timer must survive full DB reload without persist_server_runtime_to_db");
        assert_eq!(reloaded.expires_unix, 1_800_000_507);
        assert_eq!(reloaded.system_id, 30_000_142);
        assert_eq!(reloaded.started_tick, 21);
        assert_eq!(reloaded.reason, "stargate_jump");
    }
}

fn f2_open_ctx(sde: &Arc<eve_sde::SdeRegistry>, dir: &TempTestDir, name: &str, seed_fn: impl FnOnce(&mut eve_db::WorldStateSnapshot)) -> (Arc<WorldRepository>, ServerContext) {
    let db = Arc::new(WorldRepository::open_sqlite(&dir.path().join(name)).expect("open fighter test sqlite"));
    db.with_mut_snapshot(seed_fn);
    let ctx = ServerContext::with_sde_and_repository(Arc::clone(sde), Arc::clone(&db));
    (db, ctx)
}

fn f2_sq(ball_id: i64, item_id: i64, ship_id: i64, tube_index: usize, sz: u32, is_abandoned: bool, mwd_active: bool) -> ActiveFighterSquadron {
    ActiveFighterSquadron { ball_id, item_id, type_id: 23_057, owner_char_id: 90_000_001, parent_ship_id: ship_id, tube_index, squadron_size: sz, target_id: None, is_abandoned, mwd_active }
}

fn f2_cluster_count(db: &WorldRepository) -> usize {
    use eve_db::sqlite_store::system_checkpoint::{load_all_checkpoints_for_kind, CLUSTER_SYSTEM_ID, KIND_FIGHTERS};
    db.with_sqlite_conn(|c| Ok::<usize, String>(load_all_checkpoints_for_kind::<serde_json::Value>(c, KIND_FIGHTERS)?.into_iter().filter(|(sid, _, _)| *sid == CLUSTER_SYSTEM_ID).count())).unwrap()
}

fn f2_hold_db_writer(db: &Arc<WorldRepository>) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let ((htx, hrx), (rtx, rrx), d) = (std::sync::mpsc::channel(), std::sync::mpsc::channel(), Arc::clone(db));
    let h = std::thread::spawn(move || d.with_mut_snapshot(|_| { htx.send(()).unwrap(); let _ = rrx.recv(); }));
    hrx.recv().unwrap();
    (rtx, h)
}

async fn f2_spawn_unhydrated(db: &Arc<WorldRepository>, ctx: &ServerContext, sys: u32) -> crate::system_actor::SystemHandle {
    let held = f2_hold_db_writer(db);
    let h = tokio::task::spawn_blocking({ let r = Arc::clone(&ctx.systems); move || r.get_or_spawn(sys) }).await.unwrap();
    let _ = held.0.send(());
    held.1.join().unwrap();
    h
}

fn f2_break_cp_table(db: &WorldRepository, broken: bool) {
    let sql = if broken { "ALTER TABLE system_checkpoint RENAME TO system_checkpoint_bak; CREATE TABLE system_checkpoint(broken INT);" }
        else { "DROP TABLE system_checkpoint; ALTER TABLE system_checkpoint_bak RENAME TO system_checkpoint;" };
    db.with_sqlite_conn(|c| c.execute_batch(sql).map_err(|e| e.to_string())).unwrap();
}

fn f2_make_hook_gate() -> Arc<(std::sync::Mutex<u8>, std::sync::Condvar)> { Arc::new((std::sync::Mutex::new(0u8), std::sync::Condvar::new())) }
fn f2_hold_gate(g: &(std::sync::Mutex<u8>, std::sync::Condvar), secs: u64) {
    let mut st = g.0.lock().unwrap();
    *st = 1;
    g.1.notify_all();
    while *st < 2 {
        let (n, r) = g.1.wait_timeout(st, std::time::Duration::from_secs(secs)).unwrap();
        st = n;
        if r.timed_out() { break; }
    }
}
fn f2_install_hold(gate: &Arc<(std::sync::Mutex<u8>, std::sync::Condvar)>, target: u32) -> Arc<dyn Fn(u32, u64) + Send + Sync> {
    let g = Arc::clone(gate);
    Arc::new(move |sid, _| if sid == target { f2_hold_gate(&g, 5); })
}
fn f2_wait_entered(gate: &Arc<(std::sync::Mutex<u8>, std::sync::Condvar)>) {
    let mut st = gate.0.lock().unwrap();
    while *st == 0 { let (n, r) = gate.1.wait_timeout(st, std::time::Duration::from_secs(5)).unwrap(); st = n; assert!(!r.timed_out(), "timed out waiting for hook entry"); }
}
fn f2_release_gate(gate: &Arc<(std::sync::Mutex<u8>, std::sync::Condvar)>) { *gate.0.lock().unwrap() = 2; gate.1.notify_all(); }
fn f2_seed_cp(db: &WorldRepository, sys: u32, rows: &[(String, serde_json::Value)], ts: u64) {
    db.with_sqlite_conn(|c| eve_db::sqlite_store::system_checkpoint::replace_system_checkpoints_for_kind(c, sys, eve_db::sqlite_store::system_checkpoint::KIND_FIGHTERS, rows, ts)).unwrap();
}
fn f2_commit(ship_id: i64, tube_index: usize, fighter_item_id: i64, sde: &Arc<eve_sde::SdeRegistry>) -> crate::system_actor::FighterTubeOp {
    crate::system_actor::FighterTubeOp::CommitLoad { ship_id, tube_index, fighter_item_id, carrier_type_id: 23_915, fighter_type_id: 23_057, sde: Arc::clone(sde) }
}
async fn f2_shutdown(h: &crate::system_actor::SystemHandle) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    h.cmd_tx.send(crate::system_actor::SystemCommand::ShutdownActor { reply: tx }).await.unwrap();
    let _ = rx.await;
    h.cmd_tx.closed().await;
}

#[tokio::test]
async fn system_actor_fighters_persist_per_system_and_migrate_legacy_one_way() {
    use eve_db::sqlite_store::system_checkpoint::{load_all_checkpoints_for_kind, CLUSTER_SYSTEM_ID, KIND_FIGHTERS};
    let (sde, dir, (sys_jita, ship_id, fid)) = (shared_sde(), TempTestDir::new("fighters_actor_persist"), (30_000_142_u32, 880_001_001_i64, 880_001_002_i64));
    {
        let (db, ctx) = f2_open_ctx(&sde, &dir, "actor_roundtrip.sqlite", |s| { s.system_states.insert(sys_jita, SystemDynamicState { solar_system_id: sys_jita, ..Default::default() }); });
        let sys_h = ctx.systems.get_or_spawn(sys_jita);
        fighter_test_helpers::insert_active_fighter_in_system(&ctx, sys_jita, fid, f2_sq(fid, fid, ship_id, 1, 7, false, true));
        fighter_test_helpers::set_fighter_tube_load_in_system(&ctx, sys_jita, ship_id, 1, fid);
        fighter_test_helpers::set_fighter_ability_cooldown_in_system(&ctx, sys_jita, fid, 0, 4242);
        fighter_test_helpers::set_fighter_ability_charges_used_in_system(&ctx, sys_jita, fid, 2, 3);
        fighter_test_helpers::set_fighter_unit_damage_accum_in_system(&ctx, sys_jita, fid, 18.5);
        fighter_test_helpers::set_fighter_tube_rearm_until_in_system(&ctx, sys_jita, ship_id, 1, 9_999);
        assert!(sys_h.flush_fighter_checkpoint().await.expect("flush fighter cp") >= 6);
        flush_combat_runtime_to_db(&ctx);
        let cp_systems: Vec<u32> = db.with_sqlite_conn(|c| Ok::<Vec<u32>, String>(load_all_checkpoints_for_kind::<serde_json::Value>(c, KIND_FIGHTERS)?.into_iter().map(|(sid, _, _)| sid).collect())).unwrap();
        assert!(!cp_systems.is_empty() && cp_systems.iter().all(|&sid| sid == sys_jita));
        let ext_json = db.with_snapshot(|s| s.combat_crimewatch_state.extended_runtime_json.clone());
        assert!(!ext_json.contains("fighter_unit_damage_accum") && !ext_json.contains("fighter_tube_rearm_until"));
    }
    {
        let db2 = Arc::new(WorldRepository::open_sqlite(&dir.path().join("actor_roundtrip.sqlite")).expect("reopen sqlite"));
        let ctx2 = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db2));
        let fg = &ctx2.systems.get_or_spawn(sys_jita).view().fighters;
        assert_eq!((fg.active_fighters.get(&fid).map(|s| s.squadron_size), fg.tube_loads.get(&(ship_id, 1)).map(|&(id, _)| id), fg.cooldown_state.get(&(fid, 0)), fg.charge_state.get(&(fid, 2)), fg.unit_damage_accum.get(&fid), fg.tube_rearm_until.get(&(ship_id, 1))), (Some(7), Some(fid), Some(&4242), Some(&3), Some(&18.5), Some(&9_999)));
        fighter_test_helpers::clear_all_fighter_state(&ctx2);
        assert_eq!(db2.with_sqlite_conn(|c| Ok::<usize, String>(load_all_checkpoints_for_kind::<serde_json::Value>(c, KIND_FIGHTERS)?.len())).unwrap(), 0);
    }
    {
        let (db, ctx) = f2_open_ctx(&sde, &dir, "legacy_fallback.sqlite", |s| {
            seed_fighter_system_snapshot(s, sys_jita, ship_id, &[(fid, ship_id, eve_db::InventoryFlag::FighterBay, true)]);
            s.combat_crimewatch_state.extended_runtime_json = format!(r#"{{"fighter_unit_damage_accum":{{"{fid}":27.25}},"fighter_tube_rearm_until":[[[{ship_id},0],55555]]}}"#);
        });
        f2_seed_cp(&db, CLUSTER_SYSTEM_ID, &[(format!("sq|{fid}"), serde_json::json!({ "Squadron": f2_sq(fid, fid, ship_id, 0, 8, false, false) })), (format!("tl|{ship_id}|0"), serde_json::json!({ "TubeLoad": [[ship_id, 0], fid] }))], 100);
        let sys_h = ctx.systems.get_or_spawn(sys_jita);
        let _ = sys_h.wait_for_post_unlock_barrier().await;
        let fg = &sys_h.view().fighters;
        assert_eq!((fg.active_fighters.get(&fid).map(|s| s.squadron_size), fg.tube_loads.get(&(ship_id, 0)).map(|&(id, _)| id), fg.unit_damage_accum.get(&fid), fg.tube_rearm_until.get(&(ship_id, 0))), (Some(8), Some(fid), Some(&27.25), Some(&55_555)));
        assert_eq!(f2_cluster_count(&db), 0);
        flush_combat_runtime_to_db(&ctx);
        let new_ext = db.with_snapshot(|s| s.combat_crimewatch_state.extended_runtime_json.clone());
        assert!(!new_ext.contains("fighter_unit_damage_accum") && !new_ext.contains("fighter_tube_rearm_until"));
    }
}

fn seed_fighter_system_snapshot(s: &mut eve_db::WorldStateSnapshot, sys_id: u32, ship_id: i64, fighters: &[(i64, i64, eve_db::InventoryFlag, bool)]) {
    s.system_states.insert(sys_id, SystemDynamicState { solar_system_id: sys_id, ..Default::default() });
    s.items.insert(ship_id, eve_db::InventoryItemRecord { item_id: ship_id, type_id: 23_915, owner_id: 90_000_001, location_id: sys_id as i64, flag: eve_db::InventoryFlag::None, quantity: 1, singleton: true, ..Default::default() });
    for &(iid, loc_id, flag, singleton) in fighters {
        s.items.insert(iid, eve_db::InventoryItemRecord { item_id: iid, type_id: 23_057, owner_id: 90_000_001, location_id: loc_id, flag, quantity: 9, singleton, ..Default::default() });
    }
}

#[tokio::test]
async fn r2_f04_cluster_migration_ball_id_ne_item_id_and_ext_json_pruning() {
    use eve_db::sqlite_store::system_checkpoint::CLUSTER_SYSTEM_ID;
    let (sde, dir, (sys_jita, ship_id, item_id, ball_id)) = (shared_sde(), TempTestDir::new("f04_cluster_mig"), (30_000_142_u32, 880_002_001_i64, 880_002_101_i64, 990_002_101_i64));
    {
        let (db, ctx) = f2_open_ctx(&sde, &dir, "legacy_f04.sqlite", |s| {
            seed_fighter_system_snapshot(s, sys_jita, ship_id, &[(item_id, sys_jita as i64, eve_db::InventoryFlag::None, true)]);
            s.combat_crimewatch_state.extended_runtime_json = format!(r#"{{"fighter_unit_damage_accum":{{"{ball_id}":19.25}},"fighter_tube_rearm_until":[[[{ship_id},1],77777]]}}"#);
        });
        f2_seed_cp(&db, CLUSTER_SYSTEM_ID, &[
            (format!("cd|{ball_id}|0"), serde_json::json!({ "Cooldown": [[ball_id, 0], 5_555] })),
            (format!("ch|{ball_id}|2"), serde_json::json!({ "Charge": [[ball_id, 2], 4] })),
            (format!("sq|{ball_id}"), serde_json::json!({ "Squadron": f2_sq(ball_id, item_id, ship_id, 0, 7, false, true) })),
            (format!("tl|{ship_id}|0"), serde_json::json!({ "TubeLoad": [[ship_id, 0], item_id] })),
        ], 100);
        let sys_h = ctx.systems.get_or_spawn(sys_jita);
        let _ = sys_h.wait_for_post_unlock_barrier().await;
        let fg = &sys_h.view().fighters;
        assert_eq!((fg.active_fighters.get(&ball_id).map(|s| s.item_id), fg.cooldown_state.get(&(ball_id, 0)), fg.charge_state.get(&(ball_id, 2)), fg.tube_loads.get(&(ship_id, 0)), fg.unit_damage_accum.get(&ball_id), fg.tube_rearm_until.get(&(ship_id, 1))), (Some(item_id), Some(&5_555), Some(&4), Some(&(item_id, 23_057)), Some(&19.25), Some(&77_777)));
        assert_eq!(f2_cluster_count(&db), 0);
        assert!(db.with_snapshot(|s| s.combat_crimewatch_state.extended_runtime_json.is_empty()));
        fighter_test_helpers::clear_all_fighter_state(&ctx);
    }
    let db2 = Arc::new(WorldRepository::open_sqlite(&dir.path().join("legacy_f04.sqlite")).expect("reopen f04 sqlite"));
    let ctx2 = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db2));
    let fg2 = &ctx2.systems.get_or_spawn(sys_jita).view().fighters;
    assert!(fg2.unit_damage_accum.is_empty() && fg2.tube_rearm_until.is_empty() && fg2.cooldown_state.is_empty() && fg2.charge_state.is_empty());
}

#[tokio::test]
async fn r2_f05_precheck_and_failed_cold_tube_op_preserve_state() {
    use crate::system_actor::FighterTubeOp;
    let (sde, dir, (sys_jita, ship_id, f1_id, f2_id)) = (shared_sde(), TempTestDir::new("f05_cold_tube"), (30_000_142_u32, 880_003_001_i64, 880_003_101_i64, 880_003_102_i64));
    let (_, ctx) = f2_open_ctx(&sde, &dir, "cold_f05.sqlite", |s| seed_fighter_system_snapshot(s, sys_jita, ship_id, &[(f1_id, ship_id, eve_db::InventoryFlag::FighterBay, false), (f2_id, ship_id, eve_db::InventoryFlag::FighterBay, false)]));
    assert!(ctx.systems.get_handle(sys_jita).is_none());
    ctx.systems.load_or_unload_fighter_tube(sys_jita, f2_commit(ship_id, 0, f1_id, &sde)).await.unwrap();
    let (pre_res, commit_res) = tokio::join!(
        ctx.systems.load_or_unload_fighter_tube(sys_jita, FighterTubeOp::PreCheckLoad { ship_id, tube_index: 2, fighter_item_id: f2_id }),
        ctx.systems.load_or_unload_fighter_tube(sys_jita, f2_commit(ship_id, 1, f2_id, &sde)),
    );
    assert!(matches!(pre_res, Ok(None)) || matches!(&pre_res, Err(e) if e.code == "FighterAlreadyLoaded"));
    assert!(commit_res.is_ok());
    let fail = ctx.systems.load_or_unload_fighter_tube(sys_jita, f2_commit(ship_id, 2, f1_id, &sde)).await.unwrap_err();
    assert_eq!(fail.code, "FighterAlreadyLoadedInTube");
    let snap = ctx.systems.snapshot_fighters_for_system(sys_jita);
    assert_eq!((snap.tube_loads.get(&(ship_id, 0)), snap.tube_loads.get(&(ship_id, 1))), (Some(&(f1_id, 23_057)), Some(&(f2_id, 23_057))));
    assert!(ctx.systems.get_handle(sys_jita).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r3_f2_01_cold_guard_cleans_preempted_loading_and_dead_live_and_transfer_waits_stopping() {
    use crate::system_actor::{SystemLifecycleState, SystemRegistry};
    let (sde, dir, (sys_a, sys_b, ship_id, f1_id, f2_id)) = (shared_sde(), TempTestDir::new("r3_f2_01"), (30_000_142_u32, 30_002_187_u32, 880_004_001_i64, 880_004_101_i64, 880_004_102_i64));
    let (_, ctx) = f2_open_ctx(&sde, &dir, "r3_f2_01.sqlite", |s| {
        seed_fighter_system_snapshot(s, sys_a, ship_id, &[(f1_id, ship_id, eve_db::InventoryFlag::FighterBay, false), (f2_id, ship_id, eve_db::InventoryFlag::FighterBay, false)]);
        s.system_states.insert(sys_b, SystemDynamicState { solar_system_id: sys_b, ..Default::default() });
    });

    // 1. PreemptedLoading clears phase immediately so Cold ops succeed WHILE wake_a is still in flight.
    let load_gate = f2_make_hook_gate();
    ctx.systems.set_before_publish_hook_for_test(Some(f2_install_hold(&load_gate, sys_a)));
    let wake_a = tokio::task::spawn_blocking({ let r = Arc::clone(&ctx.systems); move || r.wake_for_session_arrival(sys_a, crate::system_actor::SessionArrivalWakeReason::LoginInSpace) });
    f2_wait_entered(&load_gate);
    assert!(ctx.systems.stop_system(sys_a).await);
    ctx.systems.load_or_unload_fighter_tube(sys_a, f2_commit(ship_id, 0, f1_id, &sde)).await.expect("PreemptedLoading allows Cold ops");
    ctx.systems.abandon_fighters_in_cold_db(sys_a, 90_000_001, ship_id, &std::collections::HashSet::new()).expect("sync abandon succeeds");
    ctx.systems.set_before_publish_hook_for_test(None);
    f2_release_gate(&load_gate);
    assert!(wake_a.await.unwrap().is_err());
    assert_eq!(ctx.systems.snapshot_fighters_for_system(sys_a).tube_loads.get(&(ship_id, 0)), Some(&(f1_id, 23_057)));

    // 2. stop_dead_live_to_cold clears live slot state so Cold ops proceed.
    let h_a = ctx.systems.get_or_spawn(sys_a);
    f2_shutdown(&h_a).await;
    assert!(!ctx.systems.stop_system(sys_a).await);
    ctx.systems.load_or_unload_fighter_tube(sys_a, f2_commit(ship_id, 1, f2_id, &sde)).await.expect("stop_dead_live_to_cold cleans slot");

    let _ = ctx.systems.get_or_spawn(sys_b);
    let stop_gate = f2_make_hook_gate();
    ctx.systems.set_before_stop_finish_hook_for_test(Some(f2_install_hold(&stop_gate, sys_b)));
    let stop_b = tokio::spawn({ let r = Arc::clone(&ctx.systems); async move { r.stop_system(sys_b).await } });
    f2_wait_entered(&stop_gate);
    assert_eq!(ctx.systems.system_lifecycle_state(sys_b), SystemLifecycleState::Stopping);
    let xfer = tokio::task::spawn_blocking({ let r = Arc::clone(&ctx.systems); move || r.transfer_ship_fighter_tubes(sys_a, sys_b, ship_id) });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    ctx.systems.set_before_stop_finish_hook_for_test(None);
    f2_release_gate(&stop_gate);
    assert!(stop_b.await.unwrap());
    xfer.await.unwrap().expect("transfer across Stopping succeeds");
    let dest_fg = ctx.systems.snapshot_fighters_for_system(sys_b);
    assert_eq!((dest_fg.tube_loads.get(&(ship_id, 0)), dest_fg.tube_loads.get(&(ship_id, 1))), (Some(&(f1_id, 23_057)), Some(&(f2_id, 23_057))));
    assert!(ctx.systems.snapshot_fighters_for_system(sys_a).tube_loads.is_empty());

    let reg_nodb = Arc::new(SystemRegistry::new());
    let ha_nodb = tokio::task::spawn_blocking({ let r = Arc::clone(&reg_nodb); move || r.get_or_spawn(sys_a) }).await.unwrap();
    ha_nodb.load_or_unload_fighter_tube(f2_commit(ship_id, 0, f1_id, &sde)).await.unwrap();
    assert!(reg_nodb.transfer_ship_fighter_tubes(sys_a, sys_b, ship_id).is_err() && ha_nodb.view().fighters.tube_loads.get(&(ship_id, 0)) == Some(&(f1_id, 23_057)));
    drop(ha_nodb);
    let _ = reg_nodb.stop_system(sys_a).await;
}

#[tokio::test]
async fn r3_f2_02_cold_unload_and_transfer_delete_migrated_cluster_rows() {
    use crate::system_actor::FighterTubeOp;
    use eve_db::sqlite_store::system_checkpoint::CLUSTER_SYSTEM_ID;
    let (sde, dir, (sys_jita, sys_amarr, ship_id, f1_id, f2_id)) = (shared_sde(), TempTestDir::new("r3_f2_02"), (30_000_142_u32, 30_002_187_u32, 880_005_001_i64, 880_005_101_i64, 880_005_102_i64));
    let (db, ctx) = f2_open_ctx(&sde, &dir, "r3_f2_02.sqlite", |s| {
        seed_fighter_system_snapshot(s, sys_jita, ship_id, &[(f1_id, ship_id, eve_db::InventoryFlag::FighterBay, false), (f2_id, ship_id, eve_db::InventoryFlag::FighterBay, false)]);
        s.system_states.insert(sys_amarr, SystemDynamicState { solar_system_id: sys_amarr, ..Default::default() });
    });
    f2_seed_cp(&db, CLUSTER_SYSTEM_ID, &[(format!("tl|{ship_id}|0"), serde_json::json!({ "TubeLoad": [[ship_id, 0], f1_id] })), (format!("tl|{ship_id}|1"), serde_json::json!({ "TubeLoad": [[ship_id, 1], f2_id] }))], 100);
    assert_eq!(ctx.systems.load_or_unload_fighter_tube(sys_jita, FighterTubeOp::Unload { ship_id, tube_index: 0 }).await.unwrap(), Some(f1_id));
    assert!(ctx.systems.get_handle(sys_jita).is_none() && f2_cluster_count(&db) == 0);
    let snap = ctx.systems.snapshot_fighters_for_system(sys_jita);
    assert!(snap.tube_loads.get(&(ship_id, 0)).is_none() && snap.tube_loads.get(&(ship_id, 1)) == Some(&(f2_id, 23_057)));

    f2_seed_cp(&db, CLUSTER_SYSTEM_ID, &[(format!("tl|{ship_id}|0"), serde_json::json!({ "TubeLoad": [[ship_id, 0], f1_id] }))], 101);
    ctx.systems.transfer_ship_fighter_tubes(sys_jita, sys_amarr, ship_id).expect("cold transfer");
    let amarr_fg = ctx.systems.snapshot_fighters_for_system(sys_amarr);
    assert!(f2_cluster_count(&db) == 0 && (amarr_fg.tube_loads.get(&(ship_id, 0)), amarr_fg.tube_loads.get(&(ship_id, 1))) == (Some(&(f1_id, 23_057)), Some(&(f2_id, 23_057))));

    ctx.systems.load_or_unload_fighter_tube(sys_jita, f2_commit(ship_id, 0, f1_id, &sde)).await.unwrap();
    db.with_mut_snapshot(|s| s.combat_crimewatch_state.extended_runtime_json = format!(r#"{{"fighter_tube_rearm_until":[[[{ship_id},0],9876]]}}"#));
    f2_seed_cp(&db, CLUSTER_SYSTEM_ID, &[(format!("tl|{ship_id}|1"), serde_json::json!({ "TubeLoad": [[ship_id, 1], f2_id] }))], 102);
    db.with_sqlite_conn(|c| c.execute_batch("CREATE TRIGGER fail_cluster_del BEFORE DELETE ON system_checkpoint WHEN OLD.system_id = 0 BEGIN SELECT RAISE(ABORT, 'injected_cluster_del_fail'); END;").map_err(|e| e.to_string())).unwrap();
    assert!(ctx.systems.load_or_unload_fighter_tube(sys_jita, FighterTubeOp::Unload { ship_id, tube_index: 0 }).await.is_err());
    let jita_rb = ctx.systems.snapshot_fighters_for_system(sys_jita);
    assert!((jita_rb.tube_loads.get(&(ship_id, 0)), jita_rb.tube_loads.get(&(ship_id, 1))) == (Some(&(f1_id, 23_057)), Some(&(f2_id, 23_057))) && !db.with_snapshot(|s| s.combat_crimewatch_state.extended_runtime_json.is_empty()));
    db.with_sqlite_conn(|c| c.execute_batch("DROP TRIGGER fail_cluster_del;").map_err(|e| e.to_string())).unwrap();
    assert_eq!(ctx.systems.load_or_unload_fighter_tube(sys_jita, FighterTubeOp::Unload { ship_id, tube_index: 0 }).await.unwrap(), Some(f1_id));
    assert!(f2_cluster_count(&db) == 0 && db.with_snapshot(|s| s.combat_crimewatch_state.extended_runtime_json.is_empty()));
    assert_eq!(ctx.systems.get_or_spawn(sys_jita).view().fighters.tube_loads.iter().map(|(&k, &v)| (k, v)).collect::<Vec<_>>(), vec![((ship_id, 1), (f2_id, 23_057))]);
}

#[tokio::test]
async fn r4_f2_02_serialized_rev_and_retired_gen_rejects_stale_and_post_stop_flush() {
    use crate::system_actor::{persist_live_fighter_snapshot_sync, FighterTubeOp};
    let (sde, dir, (sys_a, ship_id, f1_id, f2_id)) = (shared_sde(), TempTestDir::new("r4_f2_02"), (30_000_142_u32, 880_006_001_i64, 880_006_101_i64, 880_006_102_i64));
    let (db, ctx) = f2_open_ctx(&sde, &dir, "r4_f2_02.sqlite", |s| seed_fighter_system_snapshot(s, sys_a, ship_id, &[(f1_id, ship_id, eve_db::InventoryFlag::FighterBay, false), (f2_id, ship_id, eve_db::InventoryFlag::FighterBay, false)]));
    let h_a = ctx.systems.get_or_spawn(sys_a);
    fighter_test_helpers::set_fighter_tube_load_in_system(&ctx, sys_a, ship_id, 0, f1_id);
    let (snap1, coord) = h_a
        .test_with_actor_state_async(|st| {
            (
                st.take_fighter_checkpoint_snapshot().unwrap(),
                Arc::clone(&st.fighters_persist),
            )
        })
        .await;
    fighter_test_helpers::set_fighter_tube_load_in_system(&ctx, sys_a, ship_id, 1, f2_id);
    let snap2 = h_a
        .test_with_actor_state_async(|st| st.take_fighter_checkpoint_snapshot().unwrap())
        .await;
    assert!(persist_live_fighter_snapshot_sync(&db, sys_a, &coord, &snap2, &h_a.fighter_ctx.dirty_stamp, Some(&sde), false).unwrap() >= 2);
    assert!(!h_a.test_with_actor_state_async(|st| st.is_fighters_dirty()).await);
    assert_eq!(persist_live_fighter_snapshot_sync(&db, sys_a, &coord, &snap1, &h_a.fighter_ctx.dirty_stamp, Some(&sde), false).unwrap(), 0);
    assert_eq!(crate::system_actor::load_cold_fighters_for_system(&db, Some(&sde), sys_a).unwrap().tube_loads.len(), 2);

    fighter_test_helpers::set_fighter_unit_damage_accum_in_system(&ctx, sys_a, f1_id, 55.0);
    let stale_after_stop = h_a
        .test_with_actor_state_async(|st| st.take_fighter_checkpoint_snapshot().unwrap())
        .await;
    drop(h_a);
    assert!(ctx.systems.stop_system(sys_a).await);
    assert_eq!(ctx.systems.load_or_unload_fighter_tube(sys_a, FighterTubeOp::Unload { ship_id, tube_index: 1 }).await.unwrap(), Some(f2_id));
    let detached = std::sync::atomic::AtomicU64::new(0);
    assert_eq!(persist_live_fighter_snapshot_sync(&db, sys_a, &coord, &stale_after_stop, &detached, Some(&sde), false).unwrap(), 0);
    assert!(ctx.systems.snapshot_fighters_for_system(sys_a).tube_loads.get(&(ship_id, 1)).is_none());
}

#[tokio::test]
async fn r4_f2_03_legacy_monolith_launched_fighter_ball_ne_item_preserves_ext_damage() {
    let (sde, dir, (sys_jita, ship_id)) = (shared_sde(), TempTestDir::new("r4_f2_03"), (30_000_142_u32, 880_007_001_i64));
    let (item_a, ball_a, item_b, ball_b, item_c, ball_c) = (880_007_101_i64, 990_007_101_i64, 880_007_102_i64, 990_007_102_i64, 880_007_103_i64, 990_007_103_i64);
    let (db, ctx) = f2_open_ctx(&sde, &dir, "r4_f2_03.sqlite", |s| {
        seed_fighter_system_snapshot(s, sys_jita, ship_id, &[(item_a, sys_jita as i64, eve_db::InventoryFlag::None, true), (item_b, sys_jita as i64, eve_db::InventoryFlag::None, true), (item_c, sys_jita as i64, eve_db::InventoryFlag::None, true)]);
        for (iid, bid) in [(item_a, ball_a), (item_b, ball_b)] { s.sp6_lifecycle.space_object_meta.insert(iid, eve_db::PersistedSpaceObjectMeta { drone_ball_id: Some(bid), ..Default::default() }); }
        s.combat_crimewatch_state.extended_runtime_json = format!(r#"{{"fighter_unit_damage_accum":{{"{ball_a}":42.5,"{ball_b}":13.75,"{ball_c}":9.5}},"fighter_tube_rearm_until":[[[{ship_id},0],77777]]}}"#);
    });
    let mut legacy = std::collections::HashMap::new();
    for (bid, iid) in [(ball_a, item_a), (ball_c, item_c)] { legacy.insert(bid, f2_sq(bid, iid, ship_id, 0, 7, false, false)); }
    crate::system_actor::migrate_legacy_monolith_fighters_one_way(&db, Some(&sde), &ctx.systems, legacy, vec![], vec![], vec![]);
    assert!(db.with_snapshot(|s| s.combat_crimewatch_state.extended_runtime_json.is_empty()));
    let fg = &ctx.systems.get_or_spawn(sys_jita).view().fighters;
    assert_eq!((fg.unit_damage_accum.get(&ball_a), fg.unit_damage_accum.get(&ball_b), fg.unit_damage_accum.get(&ball_c), fg.tube_rearm_until.get(&(ship_id, 0))), (Some(&42.5), Some(&13.75), Some(&9.5), Some(&77_777)));
}

#[tokio::test(flavor = "current_thread")]
async fn r4_f2_04_current_thread_preempted_loading_allows_sync_and_async_cold_ops() {
    let (sde, dir, (sys_a, sys_b, ship_id, f1_id)) = (shared_sde(), TempTestDir::new("r4_f2_04"), (30_000_142_u32, 30_002_187_u32, 880_008_001_i64, 880_008_101_i64));
    let (_, ctx) = f2_open_ctx(&sde, &dir, "r4_f2_04.sqlite", |s| {
        seed_fighter_system_snapshot(s, sys_a, ship_id, &[(f1_id, ship_id, eve_db::InventoryFlag::FighterBay, false)]);
        s.system_states.insert(sys_b, SystemDynamicState { solar_system_id: sys_b, ..Default::default() });
    });
    let gate = f2_make_hook_gate();
    ctx.systems.set_before_publish_hook_for_test(Some(f2_install_hold(&gate, sys_a)));
    let (rt, reg_wake) = (tokio::runtime::Handle::current(), Arc::clone(&ctx.systems));
    let wake_thread = std::thread::spawn(move || { let _g = rt.enter(); reg_wake.wake_for_session_arrival(sys_a, crate::system_actor::SessionArrivalWakeReason::LoginInSpace) });
    f2_wait_entered(&gate);
    assert!(ctx.systems.stop_system(sys_a).await);
    ctx.systems.load_or_unload_fighter_tube(sys_a, f2_commit(ship_id, 0, f1_id, &sde)).await.unwrap();
    ctx.systems.transfer_ship_fighter_tubes(sys_a, sys_b, ship_id).unwrap();
    ctx.systems.abandon_fighters_in_cold_db(sys_b, 90_000_001, ship_id, &std::collections::HashSet::new()).unwrap();
    ctx.systems.set_before_publish_hook_for_test(None);
    f2_release_gate(&gate);
    assert!(wake_thread.join().unwrap().is_err());
    assert_eq!(ctx.systems.snapshot_fighters_for_system(sys_b).tube_loads.get(&(ship_id, 0)), Some(&(f1_id, 23_057)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r5_f2_01_writer_contention_hydrates_lazily_without_deadlock_or_sqlite_wipe() {
    let (sde, (sys_a, ship_id, f1_id)) = (shared_sde(), (30_000_142_u32, 880_009_001_i64, 880_009_101_i64));
    let db_mem = Arc::new(WorldRepository::with_demo_seed());
    db_mem.with_mut_snapshot(|s| seed_fighter_system_snapshot(s, sys_a, ship_id, &[(f1_id, ship_id, eve_db::InventoryFlag::FighterBay, false)]));
    let rows = vec![(format!("tl|{ship_id}|0"), serde_json::json!({ "TubeLoadTyped": [[ship_id, 0], f1_id, 23_057] })), (format!("re|{ship_id}|0"), serde_json::json!({ "TubeRearmUntil": [[ship_id, 0], 88_888] }))];
    f2_seed_cp(&db_mem, sys_a, &rows, 100);
    let ctx_mem = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db_mem));

    let held = f2_hold_db_writer(&db_mem);
    let sys_h = tokio::task::spawn_blocking({ let r = Arc::clone(&ctx_mem.systems); move || r.get_or_spawn(sys_a) }).await.unwrap();
    assert!(sys_h.test_with_actor_state_async(|st| st.fighters_needs_sqlite_hydration).await);
    fighter_test_helpers::set_fighter_tube_rearm_until_in_system(&ctx_mem, sys_a, ship_id, 1, 77_777);
    assert_eq!(sys_h.flush_fighter_checkpoint().await.unwrap(), 0, "unhydrated actor must not wipe SQLite");
    let _ = held.0.send(());
    held.1.join().unwrap();
    assert_eq!(crate::system_actor::load_cold_fighters_for_system(&db_mem, Some(&sde), sys_a).unwrap().tube_loads.get(&(ship_id, 0)), Some(&(f1_id, 23_057)));

    assert!(ctx_mem.systems.flush_system_fighter_checkpoint_sync(sys_a) >= 3);
    assert!(!sys_h.test_with_actor_state_async(|st| st.fighters_needs_sqlite_hydration).await);
    let cold_after = crate::system_actor::load_cold_fighters_for_system(&db_mem, Some(&sde), sys_a).unwrap();
    assert_eq!((cold_after.tube_loads.get(&(ship_id, 0)), cold_after.tube_rearm_until.get(&(ship_id, 0)), cold_after.tube_rearm_until.get(&(ship_id, 1))), (Some(&(f1_id, 23_057)), Some(&88_888), Some(&77_777)));

    let dir = TempTestDir::new("r5_f2_01_wal");
    let (db_wal, ctx_wal) = f2_open_ctx(&sde, &dir, "wal.sqlite", |s| seed_fighter_system_snapshot(s, sys_a, ship_id, &[(f1_id, ship_id, eve_db::InventoryFlag::FighterBay, false)]));
    f2_seed_cp(&db_wal, sys_a, &rows, 100);
    let held_wal = f2_hold_db_writer(&db_wal);
    let h_wal = tokio::task::spawn_blocking({ let r = Arc::clone(&ctx_wal.systems); move || r.get_or_spawn(sys_a) }).await.unwrap();
    assert!(!h_wal.test_with_actor_state_async(|st| st.fighters_needs_sqlite_hydration).await && h_wal.view().fighters.tube_loads.get(&(ship_id, 0)) == Some(&(f1_id, 23_057)));
    let _ = held_wal.0.send(());
    held_wal.1.join().unwrap();
}

#[tokio::test]
async fn r5_f2_03_live_legacy_migration_preserves_live_mutations_and_rejects_stale_snapshot() {
    use crate::system_actor::persist_live_fighter_snapshot_sync;
    let (sde, dir, (sys_jita, ship_id, f1_id, f2_id)) = (shared_sde(), TempTestDir::new("r5_f2_03"), (30_000_142_u32, 880_010_001_i64, 880_010_101_i64, 880_010_102_i64));
    let (db, ctx) = f2_open_ctx(&sde, &dir, "r5_f2_03.sqlite", |s| seed_fighter_system_snapshot(s, sys_jita, ship_id, &[(f1_id, sys_jita as i64, eve_db::InventoryFlag::None, true), (f2_id, ship_id, eve_db::InventoryFlag::FighterBay, false)]));
    let h = ctx.systems.get_or_spawn(sys_jita);
    fighter_test_helpers::set_fighter_tube_load_in_system(&ctx, sys_jita, ship_id, 1, f2_id);
    let (stale_snap, coord) = h
        .test_with_actor_state_async(|st| {
            (
                st.take_fighter_checkpoint_snapshot().unwrap(),
                Arc::clone(&st.fighters_persist),
            )
        })
        .await;
    fighter_test_helpers::set_fighter_unit_damage_accum_in_system(&ctx, sys_jita, f2_id, 99.0);

    let mut legacy = std::collections::HashMap::new();
    legacy.insert(f1_id, f2_sq(f1_id, f1_id, ship_id, 0, 6, false, false));
    crate::system_actor::migrate_legacy_monolith_fighters_one_way(&db, Some(&sde), &ctx.systems, legacy, vec![], vec![], vec![((ship_id, 0), f1_id)]);
    let fg = &h.view().fighters;
    assert_eq!((fg.tube_loads.get(&(ship_id, 0)), fg.tube_loads.get(&(ship_id, 1)), fg.unit_damage_accum.get(&f2_id), fg.active_fighters.get(&f1_id).map(|s| s.squadron_size)), (Some(&(f1_id, 23_057)), Some(&(f2_id, 23_057)), Some(&99.0), Some(6)));
    assert_eq!(persist_live_fighter_snapshot_sync(&db, sys_jita, &coord, &stale_snap, &h.fighter_ctx.dirty_stamp, Some(&sde), false).unwrap(), 0);
    let persisted = crate::system_actor::load_cold_fighters_for_system(&db, Some(&sde), sys_jita).unwrap();
    assert_eq!((persisted.tube_loads.get(&(ship_id, 0)), persisted.unit_damage_accum.get(&f2_id)), (Some(&(f1_id, 23_057)), Some(&99.0)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r5_f2_04_cold_op_racing_finish_stopping_slot_completes_without_exhaustion() {
    use crate::system_actor::FighterTubeOp;
    let (sde, dir, (sys_a, ship_id, f1_id)) = (shared_sde(), TempTestDir::new("r5_f2_04"), (30_000_142_u32, 880_011_001_i64, 880_011_101_i64));
    let (_, ctx) = f2_open_ctx(&sde, &dir, "r5_f2_04.sqlite", |s| seed_fighter_system_snapshot(s, sys_a, ship_id, &[(f1_id, ship_id, eve_db::InventoryFlag::FighterBay, false)]));
    for i in 0..4_u64 {
        let _ = ctx.systems.get_or_spawn(sys_a);
        let gate = f2_make_hook_gate();
        ctx.systems.set_before_stop_finish_hook_for_test(Some(f2_install_hold(&gate, sys_a)));
        let stop = tokio::spawn({ let r = Arc::clone(&ctx.systems); async move { r.stop_system(sys_a).await } });
        f2_wait_entered(&gate);
        let cold_sync = tokio::task::spawn_blocking({ let r = Arc::clone(&ctx.systems); move || r.abandon_fighters_in_cold_db(sys_a, 90_000_001, ship_id, &std::collections::HashSet::new()) });
        let cold_async = tokio::spawn({ let (r, s) = (Arc::clone(&ctx.systems), Arc::clone(&sde)); async move {
            let op = if i % 2 == 0 { f2_commit(ship_id, 0, f1_id, &s) } else { FighterTubeOp::Unload { ship_id, tube_index: 0 } };
            r.load_or_unload_fighter_tube(sys_a, op).await
        } });
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        ctx.systems.set_before_stop_finish_hook_for_test(None);
        f2_release_gate(&gate);
        assert!(stop.await.unwrap());
        cold_sync.await.unwrap().expect("cold_sync abandon ok");
        cold_async.await.unwrap().expect("cold_async tube op ok");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r6_f2_01_concurrent_and_late_hydration_noop_prevents_resurrection() {
    use crate::system_actor::{FighterTubeOp, SystemFighterState};
    let (sde, (sys_a, ship_id, f1_id, f2_id)) = (shared_sde(), (30_000_142_u32, 880_012_001_i64, 880_012_101_i64, 880_012_102_i64));
    let db = Arc::new(WorldRepository::with_demo_seed());
    db.with_mut_snapshot(|s| seed_fighter_system_snapshot(s, sys_a, ship_id, &[(f1_id, ship_id, eve_db::InventoryFlag::FighterBay, false), (f2_id, ship_id, eve_db::InventoryFlag::FighterBay, false)]));
    let rows = vec![(format!("tl:{ship_id}:0"), serde_json::json!({ "TubeLoadTyped": [[ship_id, 0], f1_id, 23_057] })), (format!("tl:{ship_id}:1"), serde_json::json!({ "TubeLoadTyped": [[ship_id, 1], f2_id, 23_057] }))];
    f2_seed_cp(&db, sys_a, &rows, 100);
    let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db));

    let h = f2_spawn_unhydrated(&db, &ctx, sys_a).await;
    assert!(h.test_with_actor_state_async(|st| st.fighters_needs_sqlite_hydration).await);
    // Error during HydrateFlight must release flight slot cleanly so waiters retry and hydrate.
    f2_break_cp_table(&db, true);
    assert!(ctx.systems.ensure_live_fighters_hydrated_async(sys_a, &h).await.is_err() && h.test_with_actor_state_async(|st| st.fighters_needs_sqlite_hydration).await);
    f2_break_cp_table(&db, false);

    let bar = Arc::new(tokio::sync::Barrier::new(3));
    let a1 = tokio::spawn({ let (r, hh, b) = (Arc::clone(&ctx.systems), h.clone(), Arc::clone(&bar)); async move { b.wait().await; r.ensure_live_fighters_hydrated_async(sys_a, &hh).await } });
    let a2 = tokio::spawn({ let (r, hh, b) = (Arc::clone(&ctx.systems), h.clone(), Arc::clone(&bar)); async move { b.wait().await; r.ensure_live_fighters_hydrated_async(sys_a, &hh).await } });
    let s1 = tokio::task::spawn_blocking({ let (r, hh, b) = (Arc::clone(&ctx.systems), h.clone(), Arc::clone(&bar)); move || { tokio::runtime::Handle::current().block_on(b.wait()); r.ensure_live_fighters_hydrated_sync(sys_a, &hh) } });
    assert!(a1.await.unwrap().is_ok() && a2.await.unwrap().is_ok() && s1.await.unwrap().is_ok());
    assert!(!h.test_with_actor_state_async(|st| st.fighters_needs_sqlite_hydration).await && h.view().fighters.tube_loads.get(&(ship_id, 0)) == Some(&(f1_id, 23_057)));

    // Unload tube 0, verify stale hydration MergeFighterState does not resurrect it, and PreCheckLoad stays clean.
    assert_eq!(ctx.systems.load_or_unload_fighter_tube(sys_a, FighterTubeOp::Unload { ship_id, tube_index: 0 }).await.unwrap(), Some(f1_id));
    let mut stale = SystemFighterState::default();
    stale.tube_loads.insert((ship_id, 0), (f1_id, 23_057));
    let _ = h.merge_fighter_state_async(stale, false, true).await.unwrap();
    assert!(h.view().fighters.tube_loads.get(&(ship_id, 0)).is_none());
    h.flush_fighter_checkpoint().await.unwrap();
    let _ = ctx.systems.load_or_unload_fighter_tube(sys_a, FighterTubeOp::PreCheckLoad { ship_id, tube_index: 1, fighter_item_id: f2_id }).await;
    assert!(!h.test_with_actor_state_async(|st| st.is_fighters_dirty()).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r6_f2_03_unhydrated_eviction_and_shutdown_preserve_mutations_and_live_paths_hydrate() {
    use crate::system_actor::load_cold_fighters_for_system;
    let (sde, (sys_a, ship_id, f1_id, f_ab, f_kill)) = (shared_sde(), (30_000_142_u32, 880_013_001_i64, 880_013_101_i64, 880_013_102_i64, 880_013_103_i64));
    let spawn_unhydrated = |rows: Vec<(String, serde_json::Value)>| {
        let db = Arc::new(WorldRepository::with_demo_seed());
        db.with_mut_snapshot(|s| seed_fighter_system_snapshot(s, sys_a, ship_id, &[(f1_id, ship_id, eve_db::InventoryFlag::FighterBay, false), (f_ab, sys_a as i64, eve_db::InventoryFlag::None, false), (f_kill, sys_a as i64, eve_db::InventoryFlag::None, false)]));
        f2_seed_cp(&db, sys_a, &rows, 100);
        let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db));
        (db, ctx)
    };
    let base_rows = vec![
        (format!("tl:{ship_id}:0"), serde_json::json!({ "TubeLoadTyped": [[ship_id, 0], f1_id, 23_057] })),
        (format!("sq:{f_ab}"), serde_json::json!({ "Squadron": f2_sq(f_ab, f_ab, ship_id, 1, 7, false, false) })),
        (format!("sq:{f_kill}"), serde_json::json!({ "Squadron": f2_sq(f_kill, f_kill, ship_id, 2, 7, false, false) })),
    ];

    // 1. Eviction: refuses eviction when dirty & hydration fails; succeeds once DB healthy preserving SQLite + dirty mutation.
    {
        let sys_ev = 30_002_510_u32;
        let db = Arc::new(WorldRepository::with_demo_seed());
        db.with_mut_snapshot(|s| {
            s.items.retain(|_, i| i.location_id != sys_ev as i64);
            s.structures.retain(|_, st| st.solar_system_id != sys_ev);
            s.system_states.entry(sys_ev).or_insert_with(|| SystemDynamicState { solar_system_id: sys_ev, ..Default::default() });
        });
        f2_seed_cp(&db, sys_ev, &[(format!("tl:{ship_id}:0"), serde_json::json!({ "TubeLoadTyped": [[ship_id, 0], f1_id, 23_057] }))], 100);
        let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db));
        let h = f2_spawn_unhydrated(&db, &ctx, sys_ev).await;
        fighter_test_helpers::set_fighter_tube_rearm_until_in_system(&ctx, sys_ev, ship_id, 1, 77_777);
        let _ = h.step_with_output(1.0).await;
        assert!(h.is_hibernated());
        drop(h);
        f2_break_cp_table(&db, true);
        assert!(ctx.systems.evict_idle_systems_with_min_ticks(0, 0).await.is_empty());
        f2_break_cp_table(&db, false);
        assert_eq!(ctx.systems.evict_idle_systems_with_min_ticks(0, 0).await, vec![sys_ev]);
        let cold = load_cold_fighters_for_system(&db, Some(&sde), sys_ev).unwrap();
        assert_eq!((cold.tube_loads.get(&(ship_id, 0)), cold.tube_rearm_until.get(&(ship_id, 1))), (Some(&(f1_id, 23_057)), Some(&77_777)));
    }
    // 2. Shutdown flush hydrates dirty unhydrated actor without wiping SQLite entries.
    {
        let (db, ctx) = spawn_unhydrated(base_rows.clone());
        let h = f2_spawn_unhydrated(&db, &ctx, sys_a).await;
        fighter_test_helpers::set_fighter_tube_rearm_until_in_system(&ctx, sys_a, ship_id, 2, 66_666);
        f2_shutdown(&h).await;
        let cold = load_cold_fighters_for_system(&db, Some(&sde), sys_a).unwrap();
        assert_eq!((cold.tube_loads.get(&(ship_id, 0)), cold.tube_rearm_until.get(&(ship_id, 2))), (Some(&(f1_id, 23_057)), Some(&66_666)));
    }
    // 3. Live callers (snapshot_loaded_tube_items, abandon_drones_and_fighters_for_ship_sync, cleanup_destroyed_non_ship_ball_sync, resolve_runtime_killer_char_id) hydrate.
    for case in 0..4 {
        let (db, ctx) = spawn_unhydrated(base_rows.clone());
        let h = f2_spawn_unhydrated(&db, &ctx, sys_a).await;
        assert!(h.test_with_actor_state_async(|st| st.fighters_needs_sqlite_hydration).await);
        match case {
            0 => assert!(crate::services::inventory_stacking::inventory_stacking_ops::snapshot_loaded_tube_items(&ctx).contains(&f1_id)),
            1 => {
                crate::sp6_lifecycle::drone_return::abandon_drones_and_fighters_for_ship_sync(&ctx, 90_000_001, ship_id, sys_a);
                assert!(h.view().fighters.active_fighters.get(&f_ab).is_some_and(|s| s.is_abandoned));
            }
            2 => {
                crate::combat::death_pipeline::cleanup_destroyed_non_ship_ball_sync(&ctx, sys_a, f_kill);
                assert!(!h.view().fighters.active_fighters.contains_key(&f_kill) && h.view().fighters.tube_loads.get(&(ship_id, 0)) == Some(&(f1_id, 23_057)));
            }
            _ => assert_eq!(crate::services::agents_missions::agents_standings_arcs::resolve_runtime_killer_char_id(&ctx, f_ab), 90_000_001),
        }
        assert!(!h.test_with_actor_state_async(|st| st.fighters_needs_sqlite_hydration).await);
    }
    // 4. BF-01 & F2-R7-01: Pre-hydration ball removal (`remove_ball` & `try_remove_destroyed_ball`) marks dirty, blocks resurrection across hydration/eviction/shutdown, and persists deletion.
    {
        let (db, ctx) = spawn_unhydrated(base_rows.clone());
        let h = f2_spawn_unhydrated(&db, &ctx, sys_a).await;
        let _ = h.remove_ball(f_ab).await;
        h.try_remove_destroyed_ball(f_kill);
        assert!(
            h.test_with_actor_state_async(|st| {
                st.fighters_needs_sqlite_hydration && st.is_fighters_dirty()
            })
            .await
        );
        let _ = h.step_with_output(1.0).await;
        assert!(h.is_hibernated());
        drop(h);
        f2_break_cp_table(&db, true);
        assert!(ctx.systems.evict_idle_systems_with_min_ticks(0, 0).await.is_empty());
        f2_break_cp_table(&db, false);
        assert_eq!(ctx.systems.evict_idle_systems_with_min_ticks(0, 0).await, vec![sys_a]);
        let cold = load_cold_fighters_for_system(&db, Some(&sde), sys_a).unwrap();
        assert!(!cold.active_fighters.contains_key(&f_ab) && !cold.active_fighters.contains_key(&f_kill) && cold.tube_loads.get(&(ship_id, 0)) == Some(&(f1_id, 23_057)));

        let (db2, ctx2) = spawn_unhydrated(base_rows);
        let h2 = f2_spawn_unhydrated(&db2, &ctx2, sys_a).await;
        let _ = h2.remove_ball(f_ab).await;
        let _ = h2.cleanup_destroyed_fighter_sync(f_kill);
        f2_shutdown(&h2).await;
        let cold2 = load_cold_fighters_for_system(&db2, Some(&sde), sys_a).unwrap();
        assert!(!cold2.active_fighters.contains_key(&f_ab) && !cold2.active_fighters.contains_key(&f_kill) && cold2.tube_loads.get(&(ship_id, 0)) == Some(&(f1_id, 23_057)));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn r7_f2_03_transfer_restores_origin_on_dest_live_failure_and_retries_stopping() {
    use crate::system_actor::load_cold_fighters_for_system;
    let dir = TempTestDir::new("evomu-r7-f2-03");
    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let (sys_a, sys_b, ship_id, f1_id, f_ab) = (30_000_142_u32, 30_002_187_u32, 200_000_001_i64, 530_901_i64, 630_902_i64);
    let (db, ctx) = f2_open_ctx(&sde, &dir, "transfer.sqlite", |s| {
        s.system_states.entry(sys_a).or_insert_with(|| SystemDynamicState { solar_system_id: sys_a, ..Default::default() });
        seed_fighter_system_snapshot(s, sys_b, ship_id, &[(f_ab, sys_b as i64, eve_db::InventoryFlag::None, true)]);
    });

    // 1. Live actor dispatch Err propagation + cold fallback in abandon_drones_and_fighters_for_ship_sync.
    let h_dead = ctx.systems.get_or_spawn(sys_b);
    fighter_test_helpers::insert_active_fighter_in_system(&ctx, sys_b, f_ab, f2_sq(f_ab, f_ab, ship_id, 1, 5, false, false));
    h_dead.flush_fighter_checkpoint().await.unwrap();
    f2_shutdown(&h_dead).await;
    assert!(h_dead.abandon_ship_fighters_sync(90_000_001, ship_id, std::collections::HashSet::from([f_ab])).is_err() && h_dead.extract_ship_fighter_tubes_sync(ship_id).is_err());
    assert!(h_dead.insert_ship_fighter_tubes_sync(ship_id, crate::system_actor::ShipFighterTubeTransfer { tube_loads: vec![(0, (f1_id, 23_057))], tube_rearm_until: vec![], cooldowns: vec![], charges: vec![] }).is_err() && h_dead.seed_abandoned_fighters_sync(vec![f2_sq(f_ab, f_ab, 0, 0, 5, true, false)]).is_err());
    drop(h_dead);
    crate::sp6_lifecycle::drone_return::abandon_drones_and_fighters_for_ship_sync(&ctx, 90_000_001, ship_id, sys_b);
    assert!(load_cold_fighters_for_system(&db, Some(&sde), sys_b).unwrap().active_fighters.get(&f_ab).is_some_and(|sq| sq.is_abandoned));

    // 2. Destination live failure (same_live == true timeout on stalled actor) returns Err and restores origin tubes + rearm_until.
    let (_db2, ctx2) = f2_open_ctx(&sde, &dir, "transfer_timeout.sqlite", |s| {
        seed_fighter_system_snapshot(s, sys_a, ship_id, &[(f1_id, ship_id, eve_db::InventoryFlag::FighterBay, false)]);
        s.system_states.entry(sys_b).or_insert_with(|| SystemDynamicState { solar_system_id: sys_b, ..Default::default() });
    });
    let (h_a, h_b) = (ctx2.systems.get_or_spawn(sys_a), ctx2.systems.get_or_spawn(sys_b));
    h_a.load_or_unload_fighter_tube(f2_commit(ship_id, 0, f1_id, &sde)).await.unwrap();
    fighter_test_helpers::set_fighter_tube_rearm_until_in_system(&ctx2, sys_a, ship_id, 1, 88_888);
    let block_gate = f2_make_hook_gate();
    let blocker = tokio::spawn({
        let (hb, bg) = (h_b.clone(), Arc::clone(&block_gate));
        async move {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let op = crate::system_actor::TestAccessOp::new(move |_, _| f2_hold_gate(&bg, 7));
            hb.cmd_tx.send(crate::system_actor::SystemCommand::TestAccess { op, mutated: false, reply: tx }).await.unwrap();
            let _ = rx.await;
        }
    });
    f2_wait_entered(&block_gate);
    let mut fill_rxs = Vec::new();
    loop {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if h_b.cmd_tx.try_send(crate::system_actor::SystemCommand::CurrentTick { reply: tx }).is_err() { break; }
        fill_rxs.push(rx);
    }
    let res = tokio::task::spawn_blocking({ let r = Arc::clone(&ctx2.systems); move || r.transfer_ship_fighter_tubes(sys_a, sys_b, ship_id) }).await.unwrap();
    f2_release_gate(&block_gate);
    blocker.await.unwrap();
    drop(fill_rxs);
    assert!(res.is_err() && h_a.view().fighters.tube_loads.get(&(ship_id, 0)) == Some(&(f1_id, 23_057)));
    assert!(h_a.view().fighters.tube_rearm_until.get(&(ship_id, 1)) == Some(&88_888) && h_b.view().fighters.tube_loads.is_empty());

    // 3. Live -> Stopping -> ColdReady retry lands transferred tubes + rearm_until cleanly on cold destination.
    let stop_gate = f2_make_hook_gate();
    ctx2.systems.set_before_stop_finish_hook_for_test(Some(f2_install_hold(&stop_gate, sys_b)));
    let stopper = tokio::spawn({ let r = Arc::clone(&ctx2.systems); async move { r.stop_system(sys_b).await } });
    f2_wait_entered(&stop_gate);
    let moved = tokio::task::spawn_blocking({ let r = Arc::clone(&ctx2.systems); move || r.transfer_ship_fighter_tubes(sys_a, sys_b, ship_id) });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    f2_release_gate(&stop_gate);
    assert!(stopper.await.unwrap());
    ctx2.systems.set_before_stop_finish_hook_for_test(None);
    assert!(moved.await.unwrap().is_ok() && h_a.view().fighters.tube_loads.is_empty() && h_a.view().fighters.tube_rearm_until.is_empty());
    let cold_b = load_cold_fighters_for_system(ctx2.db.as_ref(), Some(&sde), sys_b).unwrap();
    assert_eq!((cold_b.tube_loads.get(&(ship_id, 0)), cold_b.tube_rearm_until.get(&(ship_id, 1))), (Some(&(f1_id, 23_057)), Some(&88_888)));
}

