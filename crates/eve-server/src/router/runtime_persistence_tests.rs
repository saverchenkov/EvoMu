//! Targeted persistence, cold-restart, and rollback rehydration tests for `#592`:
//! `ServerContext`, `CombatRuntimeState`, and `PublicGatewayRouter` (`QuasarGateway`)
//! runtime state backed by durable SQLite snapshots (`service_state_blobs`,
//! `combat_crimewatch_state`, `quasar_state_blobs`).

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use eve_db::{SystemDynamicState, WorldRepository};
use eve_machonet::{service_roles::ROLE_PLAYER, SessionState};
use eve_proto::quasar::{EvePublicRequest, GoogleAny, TYPE_GOOGLEAPIS_PREFIX};
use eve_sde::SdeRegistry;

use super::context::{ActiveFighterSquadron, ActiveScannerProbe};
use super::{BoundObjectEntry, ServerContext};
use crate::services::combat_crimewatch::{
    flush_combat_runtime_to_db, with_combat_ephemeral, with_mut_combat_ephemeral,
    CombatRuntimeState, InsuranceContractState, SafetyLevel, SelfDestructCountdown,
};
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
        ctx.active_fighters.write().unwrap().insert(
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
        ctx.fighter_cooldown_state
            .write()
            .unwrap()
            .insert((fighter_id, 1), 1_700_000_500);
        ctx.fighter_charge_state
            .write()
            .unwrap()
            .insert((fighter_id, 2), 6);
        ctx.fighter_tube_loads
            .write()
            .unwrap()
            .insert((ship_id, 0), fighter_id);
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
        ctx.active_warps.lock().unwrap().insert(ship_id, 60_003_760);
        ctx.weapon_cycle_state
            .write()
            .unwrap()
            .insert(mod_id, (Some(99_999), 3, Some(88_888)));
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

        // 2) CombatRuntimeState (#544 & #592)
        with_mut_combat_ephemeral(&ctx, |st| {
            st.pending_bounty_ticks.insert((sys_id, cid), 125_000.0);
            st.bounty_tick_next_flush_unix
                .insert((sys_id, cid), 1_700_001_200);
            st.safety_levels.insert(cid, SafetyLevel::Red);
            st.player_bounty_pool.insert(cid, 50_000_000.0);
            st.modified_system_securities.insert(sys_id, 0.85);
            st.crimewatch_timer_states
                .insert(cid, (1, 0, 1, 0, 0));
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
            st.insurance_contracts.insert(
                ship_id,
                InsuranceContractState {
                    ship_id,
                    ship_type_id: 587,
                    owner_id: cid,
                    level_name: "Platinum".into(),
                    level_fraction: 1.0,
                    cost_isk: 300_000.0,
                    payout_isk: 1_000_000.0,
                    start_unix: 1_700_000_000,
                    end_unix: 1_707_257_600,
                },
            );
        });
        flush_combat_runtime_to_db(&ctx);

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
        let probes = ctx2.active_scanner_probes.read().unwrap();
        assert_eq!(probes.get(&probe_id).map(|p| p.range_step), Some(4));
        assert_eq!(probes.get(&probe_id).map(|p| p.probe_type_id), Some(30_013));
        assert_eq!(probes.get(&probe_id).map(|p| p.state), Some(0));
        assert_eq!(probes.get(&probe_id).and_then(|p| p.warp_end_ft), None);
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
        let fighters = ctx2.active_fighters.read().unwrap();
        assert_eq!(fighters.get(&fighter_id).map(|f| f.squadron_size), Some(9));
        assert_eq!(
            ctx2.fighter_cooldown_state
                .read()
                .unwrap()
                .get(&(fighter_id, 1)),
            Some(&1_700_000_500)
        );
        assert_eq!(
            ctx2.fighter_charge_state
                .read()
                .unwrap()
                .get(&(fighter_id, 2)),
            Some(&6)
        );
        assert_eq!(
            ctx2.fighter_tube_loads.read().unwrap().get(&(ship_id, 0)),
            Some(&fighter_id)
        );
        assert!(ctx2
            .module_reactivation_cooldowns
            .read()
            .unwrap()
            .keys()
            .any(|&(s, m, _)| s == ship_id && m == mod_id));
        assert_eq!(
            ctx2.mining_cycles
                .read()
                .unwrap()
                .get(&mod_id)
                .map(|m| m.duration_ms),
            Some(60_000)
        );
        assert!(ctx2.session_change_timers.lock().unwrap().contains_key(&cid));
        assert_eq!(
            ctx2.active_warps.lock().unwrap().get(&ship_id),
            Some(&60_003_760)
        );
        assert_eq!(
            ctx2.weapon_cycle_state.read().unwrap().get(&mod_id),
            Some(&(Some(99_999), 3, Some(88_888)))
        );
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

        // 2) CombatRuntimeState persisted & restored
        with_combat_ephemeral(&ctx2, |st| {
            assert_eq!(
                st.pending_bounty_ticks.get(&(sys_id, cid)),
                Some(&125_000.0)
            );
            assert_eq!(
                st.bounty_tick_next_flush_unix.get(&(sys_id, cid)),
                Some(&1_700_001_200)
            );
            assert_eq!(st.safety_levels.get(&cid), Some(&SafetyLevel::Red));
            assert_eq!(st.player_bounty_pool.get(&cid), Some(&50_000_000.0));
            assert_eq!(st.modified_system_securities.get(&sys_id), Some(&0.85));
            assert_eq!(
                st.crimewatch_timer_states.get(&cid),
                Some(&(1, 0, 1, 0, 0))
            );
            assert_eq!(
                st.self_destruct_countdowns
                    .get(&ship_id)
                    .map(|s| s.detonate_at_unix),
                Some(1_700_000_120)
            );
            assert_eq!(
                st.insurance_contracts
                    .get(&ship_id)
                    .map(|c| c.level_name.as_str()),
                Some("Platinum")
            );
        });

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
    with_mut_combat_ephemeral(&ctx, |st| {
        st.safety_levels.insert(cid, SafetyLevel::Yellow);
        st.pending_bounty_ticks.insert((sys_id, cid), 10_000.0);
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

    let baseline_rollback_epoch = db.rollback_epoch();

    // 2. Dirty RAM-only caches AND simulate a failing transactional mutation that rolls back SQLite
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
        guard.synced_db_epoch = db.rollback_epoch();
        guard.safety_levels.insert(cid, SafetyLevel::Red);
        guard.pending_bounty_ticks.insert((sys_id, cid), 999_999.0);
        guard
    };
    *ctx.quasar_gateway.server_hijack_state.write().unwrap() =
        crate::quasar_gateway::QuasarHijackState {
            enabled: true,
            error_code: Some(500),
            latency_sec: Some(9.0),
        };
    assert!(ctx.active_scanner_probes.read().unwrap().contains_key(&700_999));

    // Force an error in try_with_mut_snapshot after mutating snapshot blobs -> triggers rollback_dirty_from_sqlite!
    let res: Result<(), String> = db.try_with_mut_snapshot(|snap| {
        snap.service_state_blobs
            .insert("server_runtime_state".into(), "{}".into());
        snap.cluster_and_runtime.server_runtime_json = None;
        snap.quasar_state_blobs
            .insert("quasar_community_state".into(), "{}".into());
        snap.combat_crimewatch_state.extended_runtime_json.clear();
        Err("forced_tx_abort".into())
    });
    assert!(res.is_err());
    assert!(
        db.rollback_epoch() > baseline_rollback_epoch,
        "rollback must bump WorldRepository::rollback_epoch"
    );

    // 3. Verify lazy epoch auto-rehydrate restores CombatRuntimeState & PublicGatewayRouter immediately
    with_combat_ephemeral(&ctx, |st| {
        assert_eq!(st.safety_levels.get(&cid), Some(&SafetyLevel::Yellow));
        assert_eq!(st.pending_bounty_ticks.get(&(sys_id, cid)), Some(&10_000.0));
    });
    let list_after_rollback = ctx.quasar_gateway.handle_public_request(
        &q_sess,
        &make_public_req("eve.tag.api.ListRequest", Vec::new()),
        None,
        false,
        1_700_000_050,
    );
    assert_eq!(list_after_rollback.status_code, 200);
    assert!(!ctx.quasar_gateway.server_hijack_state.read().unwrap().enabled);
    assert!(list_after_rollback
        .payload
        .unwrap()
        .value
        .windows("fleet_doctrine".len())
        .any(|w| w == b"fleet_doctrine"));

    // 4. Full ServerContext rollback rehydration (what dispatch_rpc WAL rollback calls)
    ctx.rehydrate_runtime_from_db();
    let probes = ctx.active_scanner_probes.read().unwrap();
    assert!(
        !probes.contains_key(&700_999),
        "dirty unpersisted probe must be wiped on rollback rehydration"
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
        .modified_system_securities
        .insert(sys_id, 0.25);

    db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
    });
    with_combat_ephemeral(&ctx, |st| {
        assert_eq!(st.modified_system_securities.get(&sys_id), Some(&0.25));
    });

    db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .bounty_hunter_claimed
            .insert(hunter_id, 1_250_000.0);
    });
    with_combat_ephemeral(&ctx, |st| {
        assert_eq!(
            st.bounty_hunter_claimed.get(&hunter_id),
            Some(&1_250_000.0)
        );
        // Core snapshot sync must not clobber unflushed RAM extended state when `extended_runtime_json` is unchanged.
        assert_eq!(st.modified_system_securities.get(&sys_id), Some(&0.25));
    });

    // When `extended_runtime_json` itself changes in `with_mut_snapshot`, full rehydration runs.
    let mut replacement = CombatRuntimeState::default();
    replacement.modified_system_securities.insert(sys_id, 0.75);
    let replacement_persisted = replacement.to_persisted();
    db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state.extended_runtime_json =
            replacement_persisted.extended_runtime_json.clone();
    });
    with_combat_ephemeral(&ctx, |st| {
        assert_eq!(st.modified_system_securities.get(&sys_id), Some(&0.75));
    });
}

#[test]
fn w4_mutant_killers_context_runtime_is_empty_ewar_probes_legacy_and_sync() {
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
        ctx.active_fighters.write().unwrap().clear();
        ctx.fighter_cooldown_state.write().unwrap().clear();
        ctx.fighter_charge_state.write().unwrap().clear();
        ctx.fighter_tube_loads.write().unwrap().clear();
        ctx.module_reactivation_cooldowns.write().unwrap().clear();
        ctx.mining_cycles.write().unwrap().clear();
        ctx.phased_asteroid_fields.write().unwrap().clear();
        ctx.session_change_timers.lock().unwrap().clear();
        ctx.active_warps.lock().unwrap().clear();
        ctx.location_change_invuln.lock().unwrap().clear();
        ctx.weapon_cycle_state.write().unwrap().clear();
        ctx.projected_ewar.write().unwrap().clear();
        ctx.projected_ewar_sources.write().unwrap().clear();
        ctx.active_ewar_starts.write().unwrap().clear();
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

    // 1. All 11 `intern_ewar_kind` strings + unknown fallback -> `"electronic"`
    {
        let (db, ctx) = fresh_ctx("ewar_kinds.sqlite");
        assert!(db.with_snapshot(|s| s.cluster_and_runtime.server_runtime_json.is_none()));
        ctx.active_ewar_starts
            .write()
            .unwrap()
            .insert((101, 202), vec![(303, "warpScramblerMWD")]);
        ctx.persist_server_runtime_to_db();
        let base_json = db
            .with_snapshot(|s| s.cluster_and_runtime.server_runtime_json.clone())
            .expect("base json");
        for (input_kind, expected_static) in [
            ("warpScramblerMWD", "warpScramblerMWD"),
            ("warpScrambler", "warpScrambler"),
            ("focusedWarpScrambler", "focusedWarpScrambler"),
            ("webify", "webify"),
            ("ewTargetPaint", "ewTargetPaint"),
            ("ewRemoteSensorDamp", "ewRemoteSensorDamp"),
            ("ewTrackingDisrupt", "ewTrackingDisrupt"),
            ("ewGuidanceDisrupt", "ewGuidanceDisrupt"),
            ("electronic", "electronic"),
            ("ewEnergyNeut", "ewEnergyNeut"),
            ("ewEnergyVampire", "ewEnergyVampire"),
            ("custom_unknown_ewar", "electronic"),
        ] {
            let mutated_json = base_json.replace("\"warpScramblerMWD\"", &format!("\"{input_kind}\""));
            db.with_mut_snapshot(|s| {
                s.cluster_and_runtime.server_runtime_json = Some(mutated_json);
            });
            ctx.active_ewar_starts.write().unwrap().clear();
            ctx.hydrate_server_runtime_from_db();
            let got = ctx.active_ewar_starts.read().unwrap()[&(101, 202)][0].1;
            assert_eq!(got, expected_static, "ewar kind {input_kind}");
        }
    }

    // 2. Every field of `PersistedTacticalModulesPayload` (all 12 fields + foreign tag ignore + roundtrip)
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
            db.with_snapshot(|s| s.cluster_and_runtime.server_runtime_json.is_none()),
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
                db.with_snapshot(|s| s.cluster_and_runtime.server_runtime_json.is_some()),
                "tactical field `{label}` must make payload non-empty"
            );
            // Verify hydration into fresh context tagged with db_ptr_key or inst
            let ctx2 = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db));
            ctx2.hydrate_server_runtime_from_db();
            *ctx.tactical_state.write().unwrap() = Default::default();
            ctx.persist_server_runtime_to_db();
            assert!(
                db.with_snapshot(|s| s.cluster_and_runtime.server_runtime_json.is_none()),
                "clearing `{label}` must clear server_runtime_json"
            );
        };

        assert_tac_persists("remote_assistance", &|ts, k| {
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
        assert_tac_persists("lance_caster_lock_until_inst", &|ts, _| {
            ts.lance_caster_lock_until.insert((inst, 11), 500);
        });
        assert_tac_persists("lance_victim_debuff_until", &|ts, k| {
            ts.lance_victim_debuff_until.insert((k, 12), 600);
        });
        assert_tac_persists("entosis_active_links", &|ts, k| {
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
        assert_tac_persists("entosis_target_cycles", &|ts, k| {
            ts.entosis_target_cycles.insert((k, 14, 15), 2);
        });
        assert_tac_persists("pending_wubble_probes", &|ts, k| {
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
        assert_tac_persists("active_wubble_debuffs", &|ts, k| {
            ts.active_wubble_debuffs.insert(
                (k, 17),
                ActiveWubbleDebuff {
                    remaining_s: 12.0,
                    velocity_multiplier: 0.7,
                },
            );
        });
        assert_tac_persists("tsb_self_penalty_until", &|ts, k| {
            ts.tsb_self_penalty_until.insert((k, 18), 700);
        });
        assert_tac_persists("clone_vat_bay_activated_ships", &|ts, k| {
            ts.clone_vat_bay_activated_ships.insert((k, 19));
        });
        assert_tac_persists("active_interdiction_probes", &|ts, k| {
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
        assert_tac_persists("launcher_last_fire", &|ts, k| {
            ts.launcher_last_fire.insert(
                (k, 21),
                LauncherFireStamp {
                    fired_unix: 100,
                    fired_tick: 5,
                    fired_sim_s: 5.0,
                },
            );
        });
        assert_tac_persists("system_sim_time_s", &|ts, k| {
            ts.system_sim_time_s.insert((k, 30_000_142), 12.5);
        });
    }

    // 3. Every clause in `PersistedServerRuntimeState::is_empty()` (`&& -> ||` killers)
    {
        let (db, ctx) = fresh_ctx("runtime_is_empty.sqlite");
        let check_one = |label: &str, set: &dyn Fn(&ServerContext), clear: &dyn Fn(&ServerContext)| {
            set(&ctx);
            ctx.persist_server_runtime_to_db();
            assert!(
                db.with_snapshot(|s| s.cluster_and_runtime.server_runtime_json.is_some()),
                "field `{label}` must persist server_runtime_json = Some"
            );
            clear(&ctx);
            ctx.persist_server_runtime_to_db();
            assert!(
                db.with_snapshot(|s| s.cluster_and_runtime.server_runtime_json.is_none()),
                "clearing `{label}` must clear server_runtime_json = None"
            );
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
        check_one(
            "active_fighters",
            &|c| {
                c.active_fighters.write().unwrap().insert(
                    1,
                    ActiveFighterSquadron {
                        ball_id: 1,
                        item_id: 1,
                        type_id: 23_057,
                        owner_char_id: 1,
                        parent_ship_id: 2,
                        tube_index: 0,
                        squadron_size: 9,
                        target_id: None,
                        is_abandoned: false,
                        mwd_active: false,
                    },
                );
            },
            &|c| c.active_fighters.write().unwrap().clear(),
        );
        check_one(
            "fighter_cooldown_state",
            &|c| {
                c.fighter_cooldown_state.write().unwrap().insert((1, 0), 100);
            },
            &|c| c.fighter_cooldown_state.write().unwrap().clear(),
        );
        check_one(
            "fighter_charge_state",
            &|c| {
                c.fighter_charge_state.write().unwrap().insert((1, 2), 6);
            },
            &|c| c.fighter_charge_state.write().unwrap().clear(),
        );
        check_one(
            "fighter_tube_loads",
            &|c| {
                c.fighter_tube_loads.write().unwrap().insert((2, 0), 1);
            },
            &|c| c.fighter_tube_loads.write().unwrap().clear(),
        );
        check_one(
            "module_reactivation_cooldowns",
            &|c| {
                c.module_reactivation_cooldowns.write().unwrap().insert(
                    (1, 2, 0),
                    ModuleCooldownEntry {
                        recorded_unix_f64: 100.0,
                        delay_s: 10.0,
                        solar_system_id: Some(30_000_142),
                        recorded_sys_sim_s: 5.0,
                    },
                );
            },
            &|c| c.module_reactivation_cooldowns.write().unwrap().clear(),
        );
        check_one(
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
        check_one(
            "session_change_timers",
            &|c| {
                c.session_change_timers.lock().unwrap().insert(
                    1,
                    SessionChangeTimerState {
                        expires_unix: 1000,
                        system_id: 30_000_142,
                        started_tick: 1,
                        reason: "dock".into(),
                        location_change_invul_until: 0,
                    },
                );
            },
            &|c| c.session_change_timers.lock().unwrap().clear(),
        );
        check_one(
            "active_warps",
            &|c| {
                c.active_warps.lock().unwrap().insert(1, 2);
            },
            &|c| c.active_warps.lock().unwrap().clear(),
        );
        check_one(
            "location_change_invuln",
            &|c| {
                c.location_change_invuln.lock().unwrap().insert(1, 500);
            },
            &|c| c.location_change_invuln.lock().unwrap().clear(),
        );
        check_one(
            "weapon_cycle_state",
            &|c| {
                c.weapon_cycle_state
                    .write()
                    .unwrap()
                    .insert(1, (Some(2), 1, None));
            },
            &|c| c.weapon_cycle_state.write().unwrap().clear(),
        );
        check_one(
            "projected_ewar",
            &|c| {
                c.projected_ewar.write().unwrap().insert(
                    1,
                    vec![ProjectedEwarEffect {
                        source_module_id: 10,
                        source_module_type_id: 527,
                        charge_type_id: None,
                        distance: 1000.0,
                        overloaded: false,
                        effective_module: DogmaItemState::from_sde(10, 587, &c.sde),
                    }],
                );
            },
            &|c| c.projected_ewar.write().unwrap().clear(),
        );
        check_one(
            "projected_ewar_sources",
            &|c| {
                c.projected_ewar_sources.write().unwrap().insert(10, 20);
            },
            &|c| c.projected_ewar_sources.write().unwrap().clear(),
        );
        check_one(
            "active_ewar_starts",
            &|c| {
                c.active_ewar_starts
                    .write()
                    .unwrap()
                    .insert((1, 2), vec![(3, "ecm")]);
            },
            &|c| c.active_ewar_starts.write().unwrap().clear(),
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

