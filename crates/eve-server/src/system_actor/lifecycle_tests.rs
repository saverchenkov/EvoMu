//! Unit tests for `#1542` Track 1a solar-system lifecycle types, wake reasons,
//! `SystemActivationError` formatting, `SystemContentProvider` registration/defaults,
//! and live-repo `system_lifecycle_ratchet` enforcement.

use super::*;
use eve_db::lock_rank::{RankedMutex, RankedRwLock};
use eve_testkit::system_lifecycle_ratchet::{
    SystemLifecycleRatchet, RULE_GET_OR_SPAWN, RULE_SEED_IF_MISSING,
};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::sync::Mutex;

const JITA_SYSTEM_ID: SolarSystemId = 30_000_142;

fn dummy_system_handle(solar_system_id: SolarSystemId) -> SystemHandle {
    let (cmd_tx, _cmd_rx) = mpsc::channel::<SystemCommand>(1);
    let (event_tx, _) = broadcast::channel::<TickBroadcast>(8);
    let (view_tx, view_rx) = tokio::sync::watch::channel(Arc::new(SystemView::default()));
    let view_tx = Arc::new(view_tx);
    let tidi_factor_bits = Arc::new(AtomicU64::new(1.0_f64.to_bits()));
    SystemHandle {
        solar_system_id,
        cmd_tx,
        event_tx,
        ball_positions: Arc::new(RankedRwLock::new(HashMap::new())),
        scrambled_balls: Arc::new(RankedRwLock::new(HashSet::new())),
        tethered_balls: Arc::new(RankedRwLock::new(HashSet::new())),
        committed_warp_balls: Arc::new(RankedRwLock::new(HashMap::new())),
        warp_stopped_at: Arc::new(RankedRwLock::new(HashMap::new())),
        active_module_ships: Arc::new(RankedRwLock::new(HashSet::new())),
        ballpark: Arc::new(BallparkHandle::new(
            Ballpark::new(solar_system_id),
            Arc::clone(&view_tx),
            Arc::clone(&tidi_factor_bits),
        )),
        actor_state: Arc::new(RankedRwLock::new(SystemActorState::default())),
        view_tx,
        view_rx,
        last_auto_stepped_unix: Arc::new(AtomicU64::new(0)),
        tidi_factor_bits,
        sim_elapsed_secs_bits: Arc::new(AtomicU64::new(0.0_f64.to_bits())),
        last_visible_stamp: Arc::new(AtomicU64::new(0)),
        last_fx_stamp: Arc::new(AtomicU64::new(0)),
        hibernated: Arc::new(AtomicBool::new(true)),
        hibernated_since_unix: Arc::new(AtomicU64::new(0)),
        idle_ticks: Arc::new(AtomicU64::new(0)),
        dynamic_ball_count: Arc::new(AtomicUsize::new(0)),
        command_panics: Arc::new(AtomicU64::new(0)),
        pending_destroyed_balls: PendingDestroyedOutbox::default(),
        panic_next_command_for_test: Arc::new(AtomicBool::new(false)),
        panic_on_command_countdown_for_test: Arc::new(AtomicUsize::new(0)),
        panic_after_destruction_for_test: Arc::new(AtomicBool::new(false)),
        before_step_reply_hook_for_test: Arc::new(RankedMutex::new(None)),
        npc_death_hook: Arc::new(RankedRwLock::new(None)),
        module_deactivation_hook: Arc::new(RankedRwLock::new(None)),
        target_lock_completed_hook: Arc::new(RankedRwLock::new(None)),
        system_step_hook: Arc::new(RankedRwLock::new(None)),
        post_unlock_actions: Arc::new(RankedMutex::new(Vec::new())),
    }
}

#[test]
fn lifecycle_state_is_loaded_matches_cold_vs_active_states() {
    assert!(!SystemLifecycleState::Cold.is_loaded());
    assert!(SystemLifecycleState::Loading.is_loaded());
    assert!(SystemLifecycleState::Live.is_loaded());
    assert!(SystemLifecycleState::Hibernating.is_loaded());
    assert!(!SystemLifecycleState::Stopping.is_loaded());

    let distinct: HashSet<SystemLifecycleState> = [
        SystemLifecycleState::Cold,
        SystemLifecycleState::Loading,
        SystemLifecycleState::Live,
        SystemLifecycleState::Hibernating,
        SystemLifecycleState::Stopping,
    ]
    .into_iter()
    .collect();
    assert_eq!(distinct.len(), 5);
}

#[test]
fn wake_reason_variants_and_activation_error_display_messages() {
    let reasons: HashSet<SystemWakeReason> = [
        SystemWakeReason::SessionArrival(SessionArrivalWakeReason::LoginInSpace),
        SystemWakeReason::SessionArrival(SessionArrivalWakeReason::Undock),
        SystemWakeReason::SessionArrival(SessionArrivalWakeReason::Jump),
        SystemWakeReason::SessionArrival(SessionArrivalWakeReason::CloneJump),
        SystemWakeReason::ClusterTick(ClusterTickWakeReason::SelfDestructDue),
        SystemWakeReason::ClusterTick(ClusterTickWakeReason::DisconnectedShip),
        SystemWakeReason::ClusterTick(ClusterTickWakeReason::SafeLogoffCompleted),
        SystemWakeReason::ClusterTick(ClusterTickWakeReason::MobileWarpDisruptorActivation),
        SystemWakeReason::ClusterTick(ClusterTickWakeReason::ScheduledWork),
        SystemWakeReason::GmCommand("/wake jita".to_string()),
        SystemWakeReason::LegacyGetOrSpawn,
    ]
    .into_iter()
    .collect();
    assert_eq!(reasons.len(), 11);

    let errors = [
        SystemActivationError::NoTokioRuntime {
            solar_system_id: JITA_SYSTEM_ID,
        },
        SystemActivationError::ReentrantLoading {
            solar_system_id: JITA_SYSTEM_ID,
        },
        SystemActivationError::ConcurrentActivationAborted {
            solar_system_id: JITA_SYSTEM_ID,
        },
        SystemActivationError::ContentProviderFailed {
            solar_system_id: JITA_SYSTEM_ID,
            provider: "pve_sites".to_string(),
            message: "bad seed".to_string(),
        },
        SystemActivationError::InvalidGmCommand {
            solar_system_id: JITA_SYSTEM_ID,
        },
        SystemActivationError::InvalidContentProvider {
            provider: "".to_string(),
            message: "empty".to_string(),
        },
        SystemActivationError::WaitRejectedWhileHoldingLocks {
            solar_system_id: JITA_SYSTEM_ID,
        },
        SystemActivationError::PoisonedLock {
            solar_system_id: Some(JITA_SYSTEM_ID),
            lock: "content_providers",
        },
        SystemActivationError::PoisonedLock {
            solar_system_id: None,
            lock: "content_providers",
        },
    ];
    for err in &errors {
        let msg = err.to_string();
        assert!(!msg.trim().is_empty(), "empty Display for {err:?}");
        let dyn_err: &dyn std::error::Error = err;
        assert!(dyn_err.source().is_none());
    }
    assert!(errors[3].to_string().contains("pve_sites"));
    assert!(errors[7].to_string().contains("30000142"));
    assert!(errors[8].to_string().contains("content_providers"));
}

#[test]
fn content_provider_trait_defaults_registration_order_and_duplicate_rejection() {
    struct DefaultMetaProvider {
        calls: Arc<Mutex<Vec<&'static str>>>,
    }
    impl SystemContentProvider for DefaultMetaProvider {
        fn name(&self) -> &'static str {
            "alpha"
        }

        fn seed(&self, ctx: &mut SystemContentSeedContext<'_>) -> Result<(), String> {
            assert_eq!(ctx.solar_system_id, JITA_SYSTEM_ID);
            assert!(ctx.db_read_safe);
            assert_eq!(ctx.belt_respawn_interval_secs, 3600);
            assert_eq!(ctx.downtime_utc_secs_of_day, 39_600);
            let _ = (
                ctx.wake_reason,
                ctx.registry.active_systems_count(),
                &ctx.ballpark.solar_system_id,
                &ctx.actor_state.sim_elapsed_secs,
                ctx.sde,
                ctx.db,
                ctx.mechanic_rng,
            );
            self.calls.lock().unwrap().push("alpha");
            Ok(())
        }
    }

    struct TickAndExpireProvider {
        provider_name: &'static str,
        has_tick: bool,
        has_expire: bool,
    }
    impl SystemContentProvider for TickAndExpireProvider {
        fn name(&self) -> &'static str {
            self.provider_name
        }

        fn metadata(&self) -> SystemContentProviderMetadata {
            SystemContentProviderMetadata {
                name: self.provider_name,
                has_tick: self.has_tick,
                has_expire: self.has_expire,
            }
        }

        fn seed(&self, _ctx: &mut SystemContentSeedContext<'_>) -> Result<(), String> {
            Ok(())
        }
    }

    let reg = SystemRegistry::new();
    let calls = Arc::new(Mutex::new(Vec::new()));

    assert_eq!(
        reg.register_content_provider(Arc::new(DefaultMetaProvider {
            calls: Arc::clone(&calls),
        })),
        Ok(true)
    );
    assert_eq!(
        reg.register_content_provider(Arc::new(TickAndExpireProvider {
            provider_name: "beta",
            has_tick: true,
            has_expire: false,
        })),
        Ok(true)
    );
    assert_eq!(
        reg.register_content_provider(Arc::new(TickAndExpireProvider {
            provider_name: "gamma",
            has_tick: false,
            has_expire: true,
        })),
        Ok(true)
    );
    // Duplicate provider names are ignored idempotently without reordering or replacing existing entries.
    assert_eq!(
        reg.register_content_provider(Arc::new(TickAndExpireProvider {
            provider_name: "alpha",
            has_tick: true,
            has_expire: true,
        })),
        Ok(false)
    );

    assert_eq!(
        reg.registered_content_providers(),
        Ok(vec![
            SystemContentProviderMetadata {
                name: "alpha",
                has_tick: false,
                has_expire: false,
            },
            SystemContentProviderMetadata {
                name: "beta",
                has_tick: true,
                has_expire: false,
            },
            SystemContentProviderMetadata {
                name: "gamma",
                has_tick: false,
                has_expire: true,
            },
        ])
    );

    let snapshot = reg
        .content_providers
        .snapshot_for_test()
        .expect("snapshot providers");
    assert_eq!(snapshot.len(), 3);

    let mut ballpark = Ballpark::new(JITA_SYSTEM_ID);
    let mut actor_state = SystemActorState::default();
    let wake_reason = SystemWakeReason::SessionArrival(SessionArrivalWakeReason::LoginInSpace);
    let rng = Arc::new(crate::rng::MechanicRng::os());
    let mut seed_ctx = SystemContentSeedContext {
        solar_system_id: JITA_SYSTEM_ID,
        wake_reason: &wake_reason,
        registry: &reg,
        ballpark: &mut ballpark,
        actor_state: &mut actor_state,
        sde: None,
        db: None,
        db_read_safe: true,
        belt_respawn_interval_secs: 3600,
        downtime_utc_secs_of_day: 39_600,
        mechanic_rng: &rng,
    };
    assert_eq!(snapshot[0].seed(&mut seed_ctx), Ok(()));
    assert_eq!(*calls.lock().unwrap(), vec!["alpha"]);

    let handle = dummy_system_handle(JITA_SYSTEM_ID);
    let maint_ctx = SystemContentMaintenanceContext {
        solar_system_id: JITA_SYSTEM_ID,
        registry: &reg,
        handle: &handle,
        sde: None,
        db: None,
        now_unix_secs: 1_700_000_000,
    };
    assert_eq!(snapshot[0].tick(&maint_ctx), Ok(()));
    assert_eq!(snapshot[0].expire(&maint_ctx), Ok(()));
    assert!(!reg.is_system_loaded(JITA_SYSTEM_ID));
}

#[test]
fn register_content_provider_rejects_empty_blank_and_mismatched_names() {
    struct InvalidProvider {
        name: &'static str,
        meta_name: &'static str,
    }
    impl SystemContentProvider for InvalidProvider {
        fn name(&self) -> &'static str {
            self.name
        }

        fn metadata(&self) -> SystemContentProviderMetadata {
            SystemContentProviderMetadata {
                name: self.meta_name,
                has_tick: false,
                has_expire: false,
            }
        }

        fn seed(&self, _ctx: &mut SystemContentSeedContext<'_>) -> Result<(), String> {
            Ok(())
        }
    }

    let reg = SystemRegistry::new();
    for (name, meta_name) in [("", ""), ("   ", "   "), ("alpha", "other")] {
        let err = reg
            .register_content_provider(Arc::new(InvalidProvider { name, meta_name }))
            .expect_err("invalid provider must fail registration");
        assert!(matches!(
            err,
            SystemActivationError::InvalidContentProvider { .. }
        ));
    }
    assert_eq!(reg.registered_content_providers(), Ok(Vec::new()));
}

#[test]
fn poisoned_provider_lock_fails_closed_for_register_and_list() {
    struct ProbeProvider;
    impl SystemContentProvider for ProbeProvider {
        fn name(&self) -> &'static str {
            "probe"
        }

        fn seed(&self, _ctx: &mut SystemContentSeedContext<'_>) -> Result<(), String> {
            Ok(())
        }
    }

    let reg = SystemRegistry::new();
    reg.content_providers.poison_for_test();

    assert_eq!(
        reg.register_content_provider(Arc::new(ProbeProvider)),
        Err(SystemActivationError::PoisonedLock {
            solar_system_id: None,
            lock: "content_providers",
        })
    );
    assert_eq!(
        reg.registered_content_providers(),
        Err(SystemActivationError::PoisonedLock {
            solar_system_id: None,
            lock: "content_providers",
        })
    );
}

#[test]
fn system_lifecycle_ratchet_passes_against_live_repo() {
    let ratchet =
        SystemLifecycleRatchet::load_default().expect("load system_lifecycle_ratchet.json");
    let report = ratchet
        .verify_default()
        .expect("live crates/eve-server/src satisfies system_lifecycle_ratchet.json");
    assert!(report.rules[RULE_GET_OR_SPAWN].observed_total <= report.rules[RULE_GET_OR_SPAWN].baseline_total);
    assert!(
        report.rules[RULE_SEED_IF_MISSING].observed_total
            <= report.rules[RULE_SEED_IF_MISSING].baseline_total
    );
    assert!(
        !report.rules[RULE_GET_OR_SPAWN]
            .observed
            .contains_key("system_actor/lifecycle.rs")
    );
    assert!(
        !report.rules[RULE_GET_OR_SPAWN]
            .observed
            .contains_key("system_actor/lifecycle_sm.rs")
    );
    assert_eq!(
        report.rules[RULE_GET_OR_SPAWN]
            .observed
            .get("system_actor.rs")
            .copied()
            .unwrap_or(0),
        0
    );
}

#[test]
fn content_provider_registry_uses_leaf_lock_rank_and_drops_guard_before_trait_calls() {
    use eve_db::lock_rank::{held_lock_ranks, RANK_LEAF, RANK_SYSTEM_REGISTRY};

    struct LockProbingProvider {
        inner_leaf: RankedMutex<usize, RANK_LEAF>,
        observed_outer_ranks: Arc<Mutex<Vec<Vec<u16>>>>,
    }

    impl SystemContentProvider for LockProbingProvider {
        fn name(&self) -> &'static str {
            let ranks = held_lock_ranks();
            assert!(
                !ranks.contains(&RANK_LEAF),
                "provider.name() must not run while content_providers RANK_LEAF lock is held: {ranks:?}"
            );
            self.observed_outer_ranks.lock().unwrap().push(ranks);
            let mut n = self.inner_leaf.lock().unwrap();
            *n += 1;
            "rank-probe"
        }

        fn metadata(&self) -> SystemContentProviderMetadata {
            let ranks = held_lock_ranks();
            assert!(
                !ranks.contains(&RANK_LEAF),
                "provider.metadata() must not run while content_providers RANK_LEAF lock is held: {ranks:?}"
            );
            self.observed_outer_ranks.lock().unwrap().push(ranks);
            let mut n = self.inner_leaf.lock().unwrap();
            *n += 1;
            SystemContentProviderMetadata {
                name: "rank-probe",
                has_tick: true,
                has_expire: false,
            }
        }

        fn seed(&self, _ctx: &mut SystemContentSeedContext<'_>) -> Result<(), String> {
            Ok(())
        }
    }

    let reg = SystemRegistry::new();
    assert!(reg
        .content_providers
        .read_held_ranks_for_test()
        .contains(&RANK_LEAF));
    assert!(!held_lock_ranks().contains(&RANK_LEAF));

    let observed_outer_ranks = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(LockProbingProvider {
        inner_leaf: RankedMutex::new(0),
        observed_outer_ranks: Arc::clone(&observed_outer_ranks),
    });

    assert_eq!(reg.register_content_provider(Arc::clone(&provider) as _), Ok(true));
    assert_eq!(
        reg.register_content_provider(Arc::clone(&provider) as _),
        Ok(false)
    );
    assert_eq!(
        reg.registered_content_providers(),
        Ok(vec![SystemContentProviderMetadata {
            name: "rank-probe",
            has_tick: true,
            has_expire: false,
        }])
    );

    // Registering and listing under outer RANK_SYSTEM_REGISTRY leaves RANK_LEAF unheld during callbacks.
    {
        let _sys_guard = reg.systems.read().expect("systems RANK_SYSTEM_REGISTRY read guard");
        assert_eq!(
            reg.registered_content_providers(),
            Ok(vec![SystemContentProviderMetadata {
                name: "rank-probe",
                has_tick: true,
                has_expire: false,
            }])
        );
    }

    let calls = observed_outer_ranks.lock().unwrap();
    // 2x register (name + metadata each) + 2x registered_content_providers (metadata each) = 6 checks.
    assert_eq!(calls.len(), 6);
    assert_eq!(calls[0..5], vec![Vec::<u16>::new(); 5]);
    assert_eq!(calls[5], vec![RANK_SYSTEM_REGISTRY]);
    assert_eq!(*provider.inner_leaf.lock().unwrap(), 6);
}

#[derive(Default)]
struct TestGate {
    state: Mutex<(bool, bool)>,
    cvar: std::sync::Condvar,
}

impl TestGate {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn enter_and_wait(&self) {
        let mut g = self.state.lock().unwrap();
        g.0 = true;
        self.cvar.notify_all();
        while !g.1 {
            g = self.cvar.wait(g).unwrap();
        }
    }

    fn wait_until_entered(&self) {
        let mut g = self.state.lock().unwrap();
        while !g.0 {
            g = self.cvar.wait(g).unwrap();
        }
    }

    fn release(&self) {
        let mut g = self.state.lock().unwrap();
        g.1 = true;
        self.cvar.notify_all();
    }
}

fn new_test_registry_with_db(db: Arc<eve_db::WorldRepository>) -> Arc<SystemRegistry> {
    Arc::new(SystemRegistry::with_sde_and_db(
        Arc::new(eve_sde::SdeRegistry::with_canonical_seed()),
        db,
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_provider_seed_order_reentrancy_gm_validation_and_dynamic_hibernation() {
    struct OrderingSeedProvider {
        provider_name: &'static str,
        order: Arc<Mutex<Vec<&'static str>>>,
        check_reentrant: bool,
    }

    impl SystemContentProvider for OrderingSeedProvider {
        fn name(&self) -> &'static str {
            self.provider_name
        }

        fn seed(&self, ctx: &mut SystemContentSeedContext<'_>) -> Result<(), String> {
            assert_eq!(
                ctx.registry.system_lifecycle_state(ctx.solar_system_id),
                SystemLifecycleState::Loading
            );
            if self.check_reentrant {
                let reentrant = ctx.registry.wake_for_session_arrival(
                    ctx.solar_system_id,
                    SessionArrivalWakeReason::Jump,
                );
                assert_eq!(
                    reentrant.err(),
                    Some(SystemActivationError::ReentrantLoading {
                        solar_system_id: ctx.solar_system_id,
                    })
                );
            }
            self.order.lock().unwrap().push(self.provider_name);
            Ok(())
        }
    }

    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let reg = new_test_registry_with_db(db);
    let order = Arc::new(Mutex::new(Vec::new()));
    assert_eq!(
        reg.register_content_provider(Arc::new(OrderingSeedProvider {
            provider_name: "first",
            order: Arc::clone(&order),
            check_reentrant: true,
        })),
        Ok(true)
    );
    assert_eq!(
        reg.register_content_provider(Arc::new(OrderingSeedProvider {
            provider_name: "second",
            order: Arc::clone(&order),
            check_reentrant: false,
        })),
        Ok(true)
    );

    assert_eq!(
        reg.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Cold
    );
    assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), None);
    assert_eq!(reg.last_wake_reason(JITA_SYSTEM_ID), None);

    assert_eq!(
        reg.wake_for_gm_command(JITA_SYSTEM_ID, "   \t  ").err(),
        Some(SystemActivationError::InvalidGmCommand {
            solar_system_id: JITA_SYSTEM_ID,
        })
    );
    assert_eq!(
        reg.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Cold
    );

    let handle = reg
        .wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::LoginInSpace)
        .expect("wake from Cold succeeds");
    assert_eq!(*order.lock().unwrap(), vec!["first", "second"]);
    assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), Some(1));
    assert_eq!(
        reg.last_wake_reason_with_generation(JITA_SYSTEM_ID),
        Some((
            1,
            SystemWakeReason::SessionArrival(SessionArrivalWakeReason::LoginInSpace),
        ))
    );
    assert_eq!(
        reg.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Hibernating
    );

    // Hibernation is queried live without mutating the registry slot.
    let sub = handle.event_tx.subscribe();
    assert_eq!(
        reg.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Live
    );
    assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), Some(1));
    drop(sub);
    assert_eq!(
        reg.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Hibernating
    );

    drop(handle);
    assert!(reg.stop_system(JITA_SYSTEM_ID).await);
    assert_eq!(
        reg.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Cold
    );
    assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), None);
    assert_eq!(reg.last_wake_reason(JITA_SYSTEM_ID), None);
    let states: Vec<SystemLifecycleState> = reg
        .recent_lifecycle_transitions()
        .into_iter()
        .map(|rec| rec.to)
        .collect();
    assert!(states.contains(&SystemLifecycleState::Loading));
    assert!(states.contains(&SystemLifecycleState::Live));
    assert!(states.contains(&SystemLifecycleState::Stopping));
    assert!(states.contains(&SystemLifecycleState::Cold));
}

#[test]
fn no_tokio_runtime_fails_closed_without_partial_state() {
    let reg = SystemRegistry::new();
    let err = reg
        .wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Undock)
        .err();
    assert_eq!(
        err,
        Some(SystemActivationError::NoTokioRuntime {
            solar_system_id: JITA_SYSTEM_ID,
        })
    );
    assert!(matches!(
        reg.try_wake_if_runtime(JITA_SYSTEM_ID, SystemWakeReason::LegacyGetOrSpawn),
        Ok(None)
    ));
    assert_eq!(
        reg.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Cold
    );
    assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r1_01_eviction_race_vs_rewake_prevents_orphaned_or_evicted_live_handle() {
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let reg = new_test_registry_with_db(db);

    // Part A: concurrent re-wake acquires a strong handle before eviction commits -> eviction skips.
    let h1 = reg
        .wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::LoginInSpace)
        .expect("wake gen 1");
    drop(h1);

    let evict_gate = TestGate::new();
    let evict_gate_hook = Arc::clone(&evict_gate);
    reg.set_before_evict_commit_hook_for_test(Some(Arc::new(move |sys_id, lifecycle_gen| {
        if sys_id == JITA_SYSTEM_ID && lifecycle_gen == 1 {
            evict_gate_hook.enter_and_wait();
        }
    })));

    let reg_evict = Arc::clone(&reg);
    let evict_task = tokio::spawn(async move { reg_evict.evict_idle_systems(0).await });
    let evict_wait_gate = Arc::clone(&evict_gate);
    tokio::task::spawn_blocking(move || evict_wait_gate.wait_until_entered())
        .await
        .unwrap();

    let rewake_h1 = reg
        .wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Jump)
        .expect("concurrent rewake acquires handle");
    evict_gate.release();

    let evicted = evict_task.await.unwrap();
    assert!(evicted.is_empty(), "eviction must abort when handle refcount rose above 3");
    assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), Some(1));
    assert!(Arc::ptr_eq(
        &reg.get_handle(JITA_SYSTEM_ID).unwrap().ballpark,
        &rewake_h1.ballpark
    ));

    // Part B: re-wake arrives after eviction transitioned Live -> Stopping -> waits for Stopping -> Cold, then wakes gen 2.
    reg.set_before_evict_commit_hook_for_test(None);
    let old_bp_weak = Arc::downgrade(&rewake_h1.ballpark);
    drop(rewake_h1);

    let stop_gate = TestGate::new();
    let stop_gate_hook = Arc::clone(&stop_gate);
    reg.set_before_stop_finish_hook_for_test(Some(Arc::new(move |sys_id, lifecycle_gen| {
        if sys_id == JITA_SYSTEM_ID && lifecycle_gen == 1 {
            stop_gate_hook.enter_and_wait();
        }
    })));

    let reg_evict2 = Arc::clone(&reg);
    let evict_task2 = tokio::spawn(async move { reg_evict2.evict_idle_systems(0).await });
    let stop_wait_gate = Arc::clone(&stop_gate);
    tokio::task::spawn_blocking(move || stop_wait_gate.wait_until_entered())
        .await
        .unwrap();
    assert_eq!(
        reg.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Stopping
    );

    let reg_rewake = Arc::clone(&reg);
    let rewake_task = tokio::task::spawn_blocking(move || {
        reg_rewake.wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Jump)
    });
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert_eq!(
        reg.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Stopping
    );

    stop_gate.release();
    assert_eq!(evict_task2.await.unwrap(), vec![JITA_SYSTEM_ID]);
    assert!(
        old_bp_weak.upgrade().is_none(),
        "evicted gen-1 ballpark must be fully dropped once Stopping finishes"
    );
    let h2 = rewake_task
        .await
        .unwrap()
        .expect("rewake after Stopping -> Cold must succeed");
    assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), Some(2));
    assert_eq!(
        reg.last_wake_reason_with_generation(JITA_SYSTEM_ID),
        Some((
            2,
            SystemWakeReason::SessionArrival(SessionArrivalWakeReason::Jump),
        ))
    );

    drop(h2);
    assert!(reg.stop_system(JITA_SYSTEM_ID).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r1_02_loading_to_live_window_never_exposes_cold_or_unregistered_live() {
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let reg = new_test_registry_with_db(Arc::clone(&db));

    let pub_gate = TestGate::new();
    let pub_gate_hook = Arc::clone(&pub_gate);
    reg.set_before_publish_hook_for_test(Some(Arc::new(move |sys_id, _| {
        if sys_id == JITA_SYSTEM_ID {
            pub_gate_hook.enter_and_wait();
        }
    })));

    let reg_w1 = Arc::clone(&reg);
    let w1 = tokio::task::spawn_blocking(move || {
        reg_w1.wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Undock)
    });

    let pub_wait_gate = Arc::clone(&pub_gate);
    tokio::task::spawn_blocking(move || pub_wait_gate.wait_until_entered())
        .await
        .unwrap();

    assert_eq!(
        reg.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Loading
    );
    assert!(reg.get_handle(JITA_SYSTEM_ID).is_none());
    assert!(super::celestials::db_ballpark_entry_for_test(&db, JITA_SYSTEM_ID).is_none());

    let reg_w2 = Arc::clone(&reg);
    let w2 = tokio::task::spawn_blocking(move || {
        reg_w2.wake_for_cluster_tick(
            JITA_SYSTEM_ID,
            ClusterTickWakeReason::SelfDestructDue,
        )
    });

    let stop_polling = Arc::new(AtomicBool::new(false));
    let stop_polling_obs = Arc::clone(&stop_polling);
    let reg_obs = Arc::clone(&reg);
    let db_obs = Arc::clone(&db);
    let observer = tokio::task::spawn_blocking(move || {
        while !stop_polling_obs.load(std::sync::atomic::Ordering::Acquire) {
            let state = reg_obs.system_lifecycle_state(JITA_SYSTEM_ID);
            assert_ne!(
                state,
                SystemLifecycleState::Cold,
                "Loading -> Live window must never expose Cold"
            );
            if reg_obs.get_handle(JITA_SYSTEM_ID).is_some() {
                assert!(
                    super::celestials::db_ballpark_entry_for_test(&db_obs, JITA_SYSTEM_ID)
                        .is_some(),
                    "Live handle must never be visible before DB ballpark registration"
                );
            }
            std::thread::yield_now();
        }
    });

    pub_gate.release();
    let h1 = w1.await.unwrap().expect("primary wake succeeds");
    let h2 = w2.await.unwrap().expect("waiter wake joins cleanly");
    stop_polling.store(true, std::sync::atomic::Ordering::Release);
    observer.await.unwrap();

    assert!(Arc::ptr_eq(&h1.ballpark, &h2.ballpark));
    let (db_gen, db_bp) =
        super::celestials::db_ballpark_entry_for_test(&db, JITA_SYSTEM_ID).unwrap();
    assert_eq!(db_gen, 1);
    assert!(Arc::ptr_eq(&db_bp.upgrade().unwrap(), &h1.ballpark));

    drop((h1, h2));
    assert!(reg.stop_system(JITA_SYSTEM_ID).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r1_03_no_gate_wait_while_holding_db_registry_or_ballpark_locks() {
    const OTHER_SYS_ID: SolarSystemId = 30_000_144;
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let reg = new_test_registry_with_db(Arc::clone(&db));
    let other_handle = reg
        .wake_for_session_arrival(OTHER_SYS_ID, SessionArrivalWakeReason::LoginInSpace)
        .expect("other system wakes cleanly");

    let pub_gate = TestGate::new();
    let pub_gate_hook = Arc::clone(&pub_gate);
    reg.set_before_publish_hook_for_test(Some(Arc::new(move |sys_id, _| {
        if sys_id == JITA_SYSTEM_ID {
            pub_gate_hook.enter_and_wait();
        }
    })));

    let reg_w1 = Arc::clone(&reg);
    let w1 = tokio::task::spawn_blocking(move || {
        reg_w1.wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::LoginInSpace)
    });
    let pub_wait_gate = Arc::clone(&pub_gate);
    tokio::task::spawn_blocking(move || pub_wait_gate.wait_until_entered())
        .await
        .unwrap();

    // 1. Holding RANK_SYSTEM_REGISTRY must immediately reject gate wait (and Cold claim).
    {
        let _sys_guard = reg.systems.read().expect("registry read lock");
        let err = reg
            .wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Jump)
            .err();
        assert_eq!(
            err,
            Some(SystemActivationError::WaitRejectedWhileHoldingLocks {
                solar_system_id: JITA_SYSTEM_ID,
            })
        );
        let cold_err = reg
            .wake_for_session_arrival(30_000_145, SessionArrivalWakeReason::Jump)
            .err();
        assert_eq!(
            cold_err,
            Some(SystemActivationError::WaitRejectedWhileHoldingLocks {
                solar_system_id: 30_000_145,
            })
        );
        assert_eq!(
            reg.system_lifecycle_state(30_000_145),
            SystemLifecycleState::Cold
        );
    }

    // 2. Holding RANK_BALLPARK must immediately reject gate wait.
    {
        let _bp_guard = other_handle.ballpark.read().expect("ballpark read lock");
        let err = reg
            .wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Jump)
            .err();
        assert_eq!(
            err,
            Some(SystemActivationError::WaitRejectedWhileHoldingLocks {
                solar_system_id: JITA_SYSTEM_ID,
            })
        );
    }

    // 3. Holding DB write lock (RANK_WORLD_INNER) must immediately reject gate wait.
    {
        let res = db.tx("r1_03_hold_db", |_wtx| {
            let err = reg
                .wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Jump)
                .err();
            assert_eq!(
                err,
                Some(SystemActivationError::WaitRejectedWhileHoldingLocks {
                    solar_system_id: JITA_SYSTEM_ID,
                })
            );
            Ok::<_, eve_db::TxError>(())
        });
        assert!(res.is_ok());
    }

    pub_gate.release();
    let h = w1.await.unwrap().expect("owner completes cleanly");
    drop((h, other_handle));
    assert!(reg.stop_system(JITA_SYSTEM_ID).await);
    assert!(reg.stop_system(OTHER_SYS_ID).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r2_01_wouldblock_timeout_and_provider_failure_leave_zero_orphans() {
    struct FailingProvider;
    impl SystemContentProvider for FailingProvider {
        fn name(&self) -> &'static str {
            "failing_provider"
        }

        fn seed(&self, _ctx: &mut SystemContentSeedContext<'_>) -> Result<(), String> {
            Err("simulated provider seed failure".to_string())
        }
    }

    // Case A: ContentProvider seed failure fails before spawning and rolls back Loading -> Cold.
    let db_a = Arc::new(eve_db::WorldRepository::new_empty());
    let reg_a = new_test_registry_with_db(Arc::clone(&db_a));
    assert_eq!(
        reg_a.register_content_provider(Arc::new(FailingProvider)),
        Ok(true)
    );
    let err_a = reg_a
        .wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Undock)
        .err();
    assert_eq!(
        err_a,
        Some(SystemActivationError::ContentProviderFailed {
            solar_system_id: JITA_SYSTEM_ID,
            provider: "failing_provider".to_string(),
            message: "simulated provider seed failure".to_string(),
        })
    );
    assert_eq!(
        reg_a.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Cold
    );
    assert_eq!(reg_a.active_systems_count(), 0);
    assert!(reg_a.last_wake_reason(JITA_SYSTEM_ID).is_none());
    assert!(super::celestials::db_ballpark_entry_for_test(&db_a, JITA_SYSTEM_ID).is_none());

    // Case B: Publish WouldBlock / contention shuts down unregistered handle and rolls back cleanly.
    let db_b = Arc::new(eve_db::WorldRepository::new_empty());
    let reg_b = new_test_registry_with_db(Arc::clone(&db_b));
    reg_b.set_force_publish_wouldblock_for_test(true);
    let err_b = reg_b
        .wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Undock)
        .err();
    assert_eq!(
        err_b,
        Some(SystemActivationError::ConcurrentActivationAborted {
            solar_system_id: JITA_SYSTEM_ID,
        })
    );
    assert_eq!(
        reg_b.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Cold
    );
    assert!(reg_b.get_handle(JITA_SYSTEM_ID).is_none());
    assert_eq!(reg_b.active_systems_count(), 0);
    assert!(reg_b.last_wake_reason(JITA_SYSTEM_ID).is_none());
    assert!(super::celestials::db_ballpark_entry_for_test(&db_b, JITA_SYSTEM_ID).is_none());

    // Subsequent normal wake succeeds cleanly after rollback.
    let h_after = reg_b
        .wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Undock)
        .expect("wake after publish rollback succeeds");
    assert_eq!(reg_b.current_system_generation(JITA_SYSTEM_ID), Some(2));
    drop(h_after);
    assert!(reg_b.stop_system(JITA_SYSTEM_ID).await);

    // Case C: Waiter gate timeout fails closed without disrupting owner activation.
    let pub_gate = TestGate::new();
    let pub_gate_hook = Arc::clone(&pub_gate);
    reg_b.set_before_publish_hook_for_test(Some(Arc::new(move |sys_id, _| {
        if sys_id == JITA_SYSTEM_ID {
            pub_gate_hook.enter_and_wait();
        }
    })));
    reg_b.set_gate_wait_timeout_for_test(Some(std::time::Duration::from_millis(40)));

    let reg_owner = Arc::clone(&reg_b);
    let owner_task = tokio::task::spawn_blocking(move || {
        reg_owner.wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Jump)
    });
    let wait_gate = Arc::clone(&pub_gate);
    tokio::task::spawn_blocking(move || wait_gate.wait_until_entered())
        .await
        .unwrap();

    let reg_waiter = Arc::clone(&reg_b);
    let waiter_res = tokio::task::spawn_blocking(move || {
        reg_waiter.wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::LoginInSpace)
    })
    .await
    .unwrap();
    assert_eq!(
        waiter_res.err(),
        Some(SystemActivationError::ConcurrentActivationAborted {
            solar_system_id: JITA_SYSTEM_ID,
        })
    );

    pub_gate.release();
    let owner_h = owner_task.await.unwrap().expect("owner still finishes cleanly");
    assert_eq!(reg_b.current_system_generation(JITA_SYSTEM_ID), Some(3));
    drop(owner_h);
    assert!(reg_b.stop_system(JITA_SYSTEM_ID).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r2_02_publish_and_stop_keep_wake_reason_generation_consistent() {
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let reg = new_test_registry_with_db(db);

    let h1 = reg
        .wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::LoginInSpace)
        .expect("gen 1");
    assert_eq!(
        reg.last_wake_reason_with_generation(JITA_SYSTEM_ID),
        Some((
            1,
            SystemWakeReason::SessionArrival(SessionArrivalWakeReason::LoginInSpace),
        ))
    );
    drop(h1);
    assert!(reg.stop_system(JITA_SYSTEM_ID).await);
    assert_eq!(reg.last_wake_reason(JITA_SYSTEM_ID), None);

    let h2 = reg
        .wake_for_cluster_tick(JITA_SYSTEM_ID, ClusterTickWakeReason::SafeLogoffCompleted)
        .expect("gen 2");
    assert_eq!(
        reg.last_wake_reason_with_generation(JITA_SYSTEM_ID),
        Some((
            2,
            SystemWakeReason::ClusterTick(ClusterTickWakeReason::SafeLogoffCompleted),
        ))
    );
    drop(h2);
    assert!(reg.stop_system(JITA_SYSTEM_ID).await);
    assert_eq!(reg.last_wake_reason(JITA_SYSTEM_ID), None);

    // Preempted gen 3 cannot overwrite cleared/newer gen 4 wake reason on late publish.
    let gate = TestGate::new();
    let gate_hook = Arc::clone(&gate);
    reg.set_before_publish_hook_for_test(Some(Arc::new(move |sys_id, lifecycle_gen| {
        if sys_id == JITA_SYSTEM_ID && lifecycle_gen == 3 {
            gate_hook.enter_and_wait();
        }
    })));
    let reg_g3 = Arc::clone(&reg);
    let g3_task = tokio::task::spawn_blocking(move || {
        reg_g3.wake_for_gm_command(JITA_SYSTEM_ID, "/wake jita")
    });
    let wait_g3 = Arc::clone(&gate);
    tokio::task::spawn_blocking(move || wait_g3.wait_until_entered())
        .await
        .unwrap();

    assert!(reg.stop_system(JITA_SYSTEM_ID).await);
    assert_eq!(reg.last_wake_reason(JITA_SYSTEM_ID), None);

    reg.set_before_publish_hook_for_test(None);
    let h4 = reg
        .wake_for_cluster_tick(JITA_SYSTEM_ID, ClusterTickWakeReason::DisconnectedShip)
        .expect("gen 4");
    gate.release();
    assert_eq!(
        g3_task.await.unwrap().err(),
        Some(SystemActivationError::ConcurrentActivationAborted {
            solar_system_id: JITA_SYSTEM_ID,
        })
    );
    assert_eq!(
        reg.last_wake_reason_with_generation(JITA_SYSTEM_ID),
        Some((
            4,
            SystemWakeReason::ClusterTick(ClusterTickWakeReason::DisconnectedShip),
        ))
    );
    drop(h4);
    assert!(reg.stop_system(JITA_SYSTEM_ID).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r3_01_already_live_handoff_does_not_drop_concurrent_stop() {
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let reg = new_test_registry_with_db(db);

    let h1 = reg
        .wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::LoginInSpace)
        .expect("gen 1");
    let h1_bp = Arc::clone(&h1.ballpark);

    let stop_gate = TestGate::new();
    let stop_gate_hook = Arc::clone(&stop_gate);
    reg.set_before_stop_finish_hook_for_test(Some(Arc::new(move |sys_id, lifecycle_gen| {
        if sys_id == JITA_SYSTEM_ID && lifecycle_gen == 1 {
            stop_gate_hook.enter_and_wait();
        }
    })));

    let reg_stop = Arc::clone(&reg);
    let stop_task = tokio::spawn(async move { reg_stop.stop_system(JITA_SYSTEM_ID).await });
    let wait_stop = Arc::clone(&stop_gate);
    tokio::task::spawn_blocking(move || wait_stop.wait_until_entered())
        .await
        .unwrap();
    assert_eq!(
        reg.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Stopping
    );

    // Concurrent wake while stopping re-reads the slot rather than handing back the closing handle.
    let reg_wake = Arc::clone(&reg);
    let wake_task = tokio::task::spawn_blocking(move || {
        reg_wake.wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::CloneJump)
    });
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    stop_gate.release();

    assert!(stop_task.await.unwrap());
    assert!(h1.cmd_tx.is_closed());
    let h2 = wake_task
        .await
        .unwrap()
        .expect("wake resumes after Stopping -> Cold");
    assert!(!Arc::ptr_eq(&h2.ballpark, &h1_bp));
    assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), Some(2));
    drop((h1, h2));
    assert!(reg.stop_system(JITA_SYSTEM_ID).await);
}

#[test]
fn r3_02_late_db_ballpark_registration_never_overwrites_newer_generation() {
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let h1 = dummy_system_handle(JITA_SYSTEM_ID);
    let h2 = dummy_system_handle(JITA_SYSTEM_ID);
    let h3 = dummy_system_handle(JITA_SYSTEM_ID);

    assert!(super::celestials::register_db_system_ballpark_with_generation(
        &db,
        JITA_SYSTEM_ID,
        1,
        &h1
    ));
    assert!(super::celestials::register_db_system_ballpark_with_generation(
        &db,
        JITA_SYSTEM_ID,
        2,
        &h2
    ));

    // Late gen 1 and legacy gen 0 registrations must be rejected and preserve gen 2.
    assert!(!super::celestials::register_db_system_ballpark_with_generation(
        &db,
        JITA_SYSTEM_ID,
        1,
        &h1
    ));
    super::celestials::register_db_system_ballpark(&db, JITA_SYSTEM_ID, &h1);
    let (cur_gen, cur_weak) =
        super::celestials::db_ballpark_entry_for_test(&db, JITA_SYSTEM_ID).unwrap();
    assert_eq!(cur_gen, 2);
    assert!(Arc::ptr_eq(&cur_weak.upgrade().unwrap(), &h2.ballpark));

    // Retiring gen 2 removes the entry and blocks late gen 1 / gen 2 resurrection, while allowing gen 3.
    assert!(super::celestials::unregister_db_system_ballpark_if_generation(
        &db,
        JITA_SYSTEM_ID,
        2
    ));
    assert!(super::celestials::db_ballpark_entry_for_test(&db, JITA_SYSTEM_ID).is_none());
    assert!(!super::celestials::register_db_system_ballpark_with_generation(
        &db,
        JITA_SYSTEM_ID,
        2,
        &h2
    ));
    assert!(!super::celestials::register_db_system_ballpark_with_generation(
        &db,
        JITA_SYSTEM_ID,
        1,
        &h1
    ));
    assert!(super::celestials::register_db_system_ballpark_with_generation(
        &db,
        JITA_SYSTEM_ID,
        3,
        &h3
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generation_race_stop_and_rewake_discards_stale_completion_cleanly() {
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let reg = new_test_registry_with_db(Arc::clone(&db));

    let stale_gate = TestGate::new();
    let stale_gate_hook = Arc::clone(&stale_gate);
    reg.set_before_publish_hook_for_test(Some(Arc::new(move |sys_id, lifecycle_gen| {
        if sys_id == JITA_SYSTEM_ID && lifecycle_gen == 1 {
            stale_gate_hook.enter_and_wait();
        }
    })));

    let reg_gen1 = Arc::clone(&reg);
    let gen1_task = tokio::task::spawn_blocking(move || {
        reg_gen1.wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::LoginInSpace)
    });

    let wait_gen1 = Arc::clone(&stale_gate);
    tokio::task::spawn_blocking(move || wait_gen1.wait_until_entered())
        .await
        .unwrap();
    assert_eq!(
        reg.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Loading
    );
    assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), Some(1));

    // Stop generation 1 and immediately re-wake generation 2 while generation 1 is still finishing.
    reg.set_before_publish_hook_for_test(None);
    assert!(reg.stop_system(JITA_SYSTEM_ID).await);
    assert_eq!(
        reg.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Cold
    );

    let h2 = reg
        .wake_for_cluster_tick(
            JITA_SYSTEM_ID,
            ClusterTickWakeReason::MobileWarpDisruptorActivation,
        )
        .expect("rewake generation 2 succeeds immediately");
    assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), Some(2));

    // Allow stale generation 1 completion to attempt publish -> must be discarded cleanly.
    stale_gate.release();
    let gen1_res = gen1_task.await.unwrap();
    assert_eq!(
        gen1_res.err(),
        Some(SystemActivationError::ConcurrentActivationAborted {
            solar_system_id: JITA_SYSTEM_ID,
        })
    );

    assert_eq!(reg.discarded_stale_completions_for_test(), 1);
    assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), Some(2));
    assert!(Arc::ptr_eq(
        &reg.get_handle(JITA_SYSTEM_ID).unwrap().ballpark,
        &h2.ballpark
    ));
    assert_eq!(
        reg.last_wake_reason_with_generation(JITA_SYSTEM_ID),
        Some((
            2,
            SystemWakeReason::ClusterTick(ClusterTickWakeReason::MobileWarpDisruptorActivation),
        ))
    );
    let (db_gen, db_bp) =
        super::celestials::db_ballpark_entry_for_test(&db, JITA_SYSTEM_ID).unwrap();
    assert_eq!(db_gen, 2);
    assert!(Arc::ptr_eq(&db_bp.upgrade().unwrap(), &h2.ballpark));

    drop(h2);
    assert!(reg.stop_system(JITA_SYSTEM_ID).await);
}
