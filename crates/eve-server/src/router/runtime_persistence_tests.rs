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
