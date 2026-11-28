//! Unit tests for `#1542` Track 1a solar-system lifecycle types, wake reasons,
//! `SystemActivationError` formatting, `SystemContentProvider` registration/defaults,
//! and live-repo `system_lifecycle_ratchet` enforcement.

use super::*;
use eve_db::lock_rank::{RankedMutex, RankedRwLock};
use eve_testkit::system_lifecycle_ratchet::{
    SystemLifecycleRatchet, RULE_GET_OR_SPAWN, RULE_SEED_IF_MISSING,
};
use std::collections::HashSet;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::sync::Mutex;

const JITA_SYSTEM_ID: SolarSystemId = 30_000_142;

fn dummy_system_handle(solar_system_id: SolarSystemId) -> SystemHandle {
    let (cmd_tx, _cmd_rx) = mpsc::channel::<SystemCommand>(1);
    let (event_tx, _) = broadcast::channel::<TickBroadcast>(8);
    let (view_tx, view_rx) = tokio::sync::watch::channel(Arc::new(SystemView::default()));
    let view_tx = Arc::new(view_tx);
    let tidi_factor_bits = Arc::new(AtomicU64::new(1.0_f64.to_bits()));
    let tethered_balls = Arc::new(RankedRwLock::new(HashSet::new()));
    let committed_warp_balls = Arc::new(RankedRwLock::new(HashMap::new()));
    SystemHandle {
        solar_system_id,
        cmd_tx,
        post_unlock_tx: mpsc::unbounded_channel().0,
        worker_cb_tx: mpsc::unbounded_channel().0,
        ct_driver: None,
        actor_rt_handle: None,
        post_unlock_worker_thread_id: Arc::new(std::sync::OnceLock::new()),
        post_unlock_in_flight: Arc::new(AtomicUsize::new(0)),
        completed_post_unlock_seq: Arc::new(AtomicU64::new(0)),
        fighters_needs_hydration: Arc::new(AtomicBool::new(false)),
        fighter_ctx: Arc::new(Default::default()),
        event_tx,
        ball_positions: Arc::new(celestials::BallPositionSyncCache::new(
            Arc::clone(&committed_warp_balls),
            Arc::clone(&tethered_balls),
        )),
        scrambled_balls: Arc::new(RankedRwLock::new(HashSet::new())),
        tethered_balls,
        committed_warp_balls,
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
        panic_after_command_guards_for_test: Arc::new(AtomicBool::new(false)),
        before_step_reply_hook_for_test: Arc::new(RankedMutex::new(None)),
        npc_death_hook: Arc::new(RankedRwLock::new(None)),
        module_deactivation_hook: Arc::new(RankedRwLock::new(None)),
        module_deactivation_post_unlock_hook: Arc::new(RankedRwLock::new(None)),
        target_lock_completed_hook: Arc::new(RankedRwLock::new(None)),
        system_step_hook: Arc::new(RankedRwLock::new(None)),
        post_unlock_actions: Arc::new(RankedMutex::new(Vec::new())),
        actor_flush_thread: Arc::new(std::sync::Mutex::new(None)),
        published_live: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        cooldown_persist_gate: Arc::new(super::CooldownPersistGate::default()),
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
    let mut seed_post_unlock_ops = super::post_unlock::PostUnlockOps::default();
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
        staged_special_site_balls: std::collections::HashSet::new(),
        staged_wh_ore_anomalies: std::collections::HashSet::new(),
        staged_post_unlock_ops: &mut seed_post_unlock_ops,
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
    let db_bp = super::celestials::db_ballpark_entry_for_test(&db, JITA_SYSTEM_ID).unwrap();
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
    let db_bp = super::celestials::db_ballpark_entry_for_test(&db, JITA_SYSTEM_ID).unwrap();
    assert!(Arc::ptr_eq(&db_bp.upgrade().unwrap(), &h2.ballpark));

    drop(h2);
    assert!(reg.stop_system(JITA_SYSTEM_ID).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf01_real_publish_contention_and_higher_rank_lock_fail_closed_without_wedging() {
    const OTHER_SYS_ID: SolarSystemId = 30_000_144;
    const COLD_LEAF_SYS_ID: SolarSystemId = 30_000_145;
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let reg = new_test_registry_with_db(Arc::clone(&db));
    let other_handle = reg
        .wake_for_session_arrival(OTHER_SYS_ID, SessionArrivalWakeReason::LoginInSpace)
        .expect("other system wakes");

    // Part A: Calling wake on a Cold system while holding >= RANK_SYSTEM_REGISTRY
    // (RANK_BALLPARK=20, RANK_WORLD_INNER=40, RANK_LEAF=60) fails closed with
    // WaitRejectedWhileHoldingLocks even under simultaneous systems.read() contention,
    // without panicking on telemetry/cluster_nodes locks and without wedging the slot.
    {
        let _sys_read = reg.systems.read().expect("registry read guard");
        let _bp_read = other_handle.ballpark.read().expect("ballpark read guard");
        assert_eq!(
            reg.wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Jump)
                .err(),
            Some(SystemActivationError::WaitRejectedWhileHoldingLocks {
                solar_system_id: JITA_SYSTEM_ID,
            })
        );
        assert_eq!(
            reg.system_lifecycle_state(JITA_SYSTEM_ID),
            SystemLifecycleState::Cold
        );
    }
    {
        let res = db.tx("bf01_world_inner", |_wtx| {
            assert_eq!(
                reg.wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Jump)
                    .err(),
                Some(SystemActivationError::WaitRejectedWhileHoldingLocks {
                    solar_system_id: JITA_SYSTEM_ID,
                })
            );
            Ok::<_, eve_db::TxError>(())
        });
        assert!(res.is_ok());
        assert_eq!(
            reg.system_lifecycle_state(JITA_SYSTEM_ID),
            SystemLifecycleState::Cold
        );
    }
    {
        let _leaf_read = reg.lifecycle_telemetry.read().expect("telemetry read guard");
        assert_eq!(
            reg.wake_for_session_arrival(COLD_LEAF_SYS_ID, SessionArrivalWakeReason::Jump)
                .err(),
            Some(SystemActivationError::WaitRejectedWhileHoldingLocks {
                solar_system_id: COLD_LEAF_SYS_ID,
            })
        );
        assert_eq!(
            reg.system_lifecycle_state(COLD_LEAF_SYS_ID),
            SystemLifecycleState::Cold
        );
    }

    // Part B: Real systems.read() contention during publish_loading_slot blocks cleanly on
    // ranked systems.write() without failing spuriously or wedging Loading.
    let pub_gate = TestGate::new();
    let pub_gate_hook = Arc::clone(&pub_gate);
    reg.set_before_publish_hook_for_test(Some(Arc::new(move |sys_id, _| {
        if sys_id == JITA_SYSTEM_ID {
            pub_gate_hook.enter_and_wait();
        }
    })));

    let reg_wake = Arc::clone(&reg);
    let wake_task = tokio::task::spawn_blocking(move || {
        reg_wake.wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Undock)
    });
    let pub_wait = Arc::clone(&pub_gate);
    tokio::task::spawn_blocking(move || pub_wait.wait_until_entered())
        .await
        .unwrap();

    let reader_entered = TestGate::new();
    let reader_entered_bg = Arc::clone(&reader_entered);
    let reg_reader = Arc::clone(&reg);
    let reader_task = tokio::task::spawn_blocking(move || {
        let _guard = reg_reader.systems.read().expect("real reader contention");
        reader_entered_bg.enter_and_wait();
    });
    let reader_wait = Arc::clone(&reader_entered);
    tokio::task::spawn_blocking(move || reader_wait.wait_until_entered())
        .await
        .unwrap();

    // Release owner publish while reader still holds systems.read(), then release reader.
    pub_gate.release();
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    reader_entered.release();
    reader_task.await.unwrap();

    let jita_h = wake_task
        .await
        .unwrap()
        .expect("publish waits for systems.read() cleanly without WouldBlock");
    assert_eq!(
        reg.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Hibernating
    );

    reg.set_before_publish_hook_for_test(None);
    drop((jita_h, other_handle));
    assert!(reg.stop_system(JITA_SYSTEM_ID).await);
    assert!(reg.stop_system(OTHER_SYS_ID).await);
}

#[test]
fn bf02_tokio_spawn_worker_reuse_on_single_worker_during_stopping() {
    let actor_rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("actor runtime builds");
    let worker_rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("single-worker runtime builds");

    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let reg = new_test_registry_with_db(db);

    let wait_until_stopping = |reg: Arc<SystemRegistry>| async move {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        // Check the slot map directly without cloning `SystemHandle`, which would temporarily
        // bump `ballpark` strong_count above 3 and cause `evict_idle_systems` to abort eviction.
        while !matches!(
            reg.systems
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&JITA_SYSTEM_ID),
            Some(lifecycle_sm::SystemLifecycleSlot::Stopping { .. })
        ) {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for Stopping state"
            );
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    };

    // Part A: Eviction -> Stopping on single worker thread -> concurrent tokio::spawn sync + async
    // wakes reuse that exact worker thread without owner_thread false abort or deadlock.
    let h1 = actor_rt.block_on(async {
        reg.wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::LoginInSpace)
            .expect("gen 1")
    });
    let h1_cmd_tx = h1.cmd_tx.clone();
    let h1_step_hook = Arc::clone(&h1.before_step_reply_hook_for_test);
    drop(h1);

    let step_gate1 = TestGate::new();
    let step_gate1_hook = Arc::clone(&step_gate1);
    reg.set_before_evict_commit_hook_for_test(Some(Arc::new(move |sys_id, lifecycle_gen| {
        if sys_id == JITA_SYSTEM_ID && lifecycle_gen == 1 {
            let pause_gate = Arc::clone(&step_gate1_hook);
            *h1_step_hook.lock().unwrap() = Some(Box::new(move || {
                pause_gate.enter_and_wait();
            }));
            let (tx1, _rx1) = tokio::sync::oneshot::channel();
            assert!(h1_cmd_tx
                .try_send(SystemCommand::StepManual { dt: 0.1, reply: tx1 })
                .is_ok());
            step_gate1_hook.wait_until_entered();
        }
    })));

    worker_rt.block_on(async {
        let worker_tid = tokio::spawn(async { std::thread::current().id() })
            .await
            .unwrap();
        let reg_evict = Arc::clone(&reg);
        let evict_task = tokio::spawn(async move {
            assert_eq!(std::thread::current().id(), worker_tid);
            reg_evict.evict_idle_systems(0).await
        });
        wait_until_stopping(Arc::clone(&reg)).await;

        let reg_async = Arc::clone(&reg);
        let async_wake = tokio::spawn(async move {
            reg_async
                .wake_for_session_arrival_async(JITA_SYSTEM_ID, SessionArrivalWakeReason::Jump)
                .await
        });
        let reg_sync = Arc::clone(&reg);
        let sync_wake = tokio::spawn(async move {
            reg_sync.wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Undock)
        });

        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert_eq!(
            reg.system_lifecycle_state(JITA_SYSTEM_ID),
            SystemLifecycleState::Stopping
        );
        step_gate1.release();

        assert_eq!(evict_task.await.unwrap(), vec![JITA_SYSTEM_ID]);
        let h2_a = async_wake
            .await
            .unwrap()
            .expect("async wake during eviction succeeds");
        let h2_b = sync_wake
            .await
            .unwrap()
            .expect("sync block_in_place wake during eviction succeeds");
        assert!(Arc::ptr_eq(&h2_a.ballpark, &h2_b.ballpark));
        assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), Some(2));
        drop((h2_a, h2_b));
    });
    reg.set_before_evict_commit_hook_for_test(None);

    // Part B: stop_system -> Stopping on single worker thread -> concurrent tokio::spawn sync + async wakes.
    // Re-wake gen 2 on actor_rt first so its actor stays paused on actor_rt during stop_system on worker_rt.
    worker_rt.block_on(async {
        assert!(reg.stop_system(JITA_SYSTEM_ID).await);
    });
    let h2 = actor_rt.block_on(async {
        reg.wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::LoginInSpace)
            .expect("gen 3")
    });
    let step_gate2 = TestGate::new();
    let step_gate2_hook = Arc::clone(&step_gate2);
    *h2.before_step_reply_hook_for_test.lock().unwrap() = Some(Box::new(move || {
        step_gate2_hook.enter_and_wait();
    }));
    let (tx2, _rx2) = tokio::sync::oneshot::channel();
    assert!(h2
        .cmd_tx
        .try_send(SystemCommand::StepManual { dt: 0.1, reply: tx2 })
        .is_ok());
    step_gate2.wait_until_entered();
    drop(h2);

    worker_rt.block_on(async {
        let reg_stop = Arc::clone(&reg);
        let stop_task = tokio::spawn(async move { reg_stop.stop_system(JITA_SYSTEM_ID).await });
        wait_until_stopping(Arc::clone(&reg)).await;

        let reg_async2 = Arc::clone(&reg);
        let async_wake2 = tokio::spawn(async move {
            reg_async2
                .wake_for_session_arrival_async(
                    JITA_SYSTEM_ID,
                    SessionArrivalWakeReason::CloneJump,
                )
                .await
        });
        let reg_sync2 = Arc::clone(&reg);
        let sync_wake2 = tokio::spawn(async move {
            reg_sync2.wake_for_session_arrival(
                JITA_SYSTEM_ID,
                SessionArrivalWakeReason::LoginInSpace,
            )
        });

        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert_eq!(
            reg.system_lifecycle_state(JITA_SYSTEM_ID),
            SystemLifecycleState::Stopping
        );
        step_gate2.release();

        assert!(stop_task.await.unwrap());
        let h4_a = async_wake2
            .await
            .unwrap()
            .expect("async wake during stop succeeds");
        let h4_b = sync_wake2
            .await
            .unwrap()
            .expect("sync wake during stop succeeds");
        assert!(Arc::ptr_eq(&h4_a.ballpark, &h4_b.ballpark));
        assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), Some(4));
        drop((h4_a, h4_b));
        assert!(reg.stop_system(JITA_SYSTEM_ID).await);
    });
}

#[test]
fn bf02_current_thread_runtime_async_wake_succeeds_and_sync_wait_fails_closed() {
    struct NotifyWaker(Arc<tokio::sync::Notify>);
    impl std::task::Wake for NotifyWaker {
        fn wake(self: Arc<Self>) {
            self.0.notify_one();
        }
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current_thread runtime builds");

    rt.block_on(async {
        let db = Arc::new(eve_db::WorldRepository::new_empty());
        let reg = new_test_registry_with_db(db);

        let h1 = reg
            .wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::LoginInSpace)
            .expect("cold wake on current_thread succeeds");
        drop(h1);

        let wake_notify = Arc::new(tokio::sync::Notify::new());
        let waker = std::task::Waker::from(Arc::new(NotifyWaker(Arc::clone(&wake_notify))));
        let mut stop_fut = std::pin::pin!(reg.stop_system(JITA_SYSTEM_ID));
        assert!(stop_fut
            .as_mut()
            .poll(&mut std::task::Context::from_waker(&waker))
            .is_pending());
        assert_eq!(
            reg.system_lifecycle_state(JITA_SYSTEM_ID),
            SystemLifecycleState::Stopping
        );

        // Synchronous wait on a Pending gate under current_thread must fail closed immediately
        // instead of blocking the sole executor thread.
        assert_eq!(
            reg.wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Jump)
                .err(),
            Some(SystemActivationError::ConcurrentActivationAborted {
                solar_system_id: JITA_SYSTEM_ID,
            })
        );

        // Async wake on the same current_thread runtime awaits SettledToCold and wakes gen 2 cleanly.
        let drive_stop = async {
            loop {
                let notified = wake_notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if let std::task::Poll::Ready(stopped) =
                    stop_fut.as_mut().poll(&mut std::task::Context::from_waker(&waker))
                {
                    return stopped;
                }
                notified.await;
            }
        };
        let (stopped, wake_res) = tokio::join!(
            drive_stop,
            reg.wake_for_session_arrival_async(JITA_SYSTEM_ID, SessionArrivalWakeReason::Jump)
        );
        assert!(stopped);
        let h2 = wake_res.expect("async wake on current_thread settles cleanly");
        assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), Some(2));
        drop(h2);
        assert!(reg.stop_system(JITA_SYSTEM_ID).await);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_world_repository_multi_registry_and_pointer_clear_isolation() {
    const OTHER_SYS_ID: SolarSystemId = 30_000_144;
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let reg1 = new_test_registry_with_db(Arc::clone(&db));

    let h_other = reg1
        .wake_for_session_arrival(OTHER_SYS_ID, SessionArrivalWakeReason::LoginInSpace)
        .expect("reg1 gen 1");
    let h_jita = reg1
        .wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Undock)
        .expect("reg1 gen 2");
    assert_eq!(reg1.current_system_generation(JITA_SYSTEM_ID), Some(2));
    drop(h_jita);
    assert!(reg1.stop_system(JITA_SYSTEM_ID).await);
    assert!(super::celestials::db_ballpark_entry_for_test(&db, JITA_SYSTEM_ID).is_none());

    // Second SystemRegistry sharing the same WorldRepository wakes cleanly and overwrites
    // the weak DB ballpark entry for OTHER_SYS_ID without collision.
    let reg2 = new_test_registry_with_db(Arc::clone(&db));
    let h2_other = reg2
        .wake_for_session_arrival(OTHER_SYS_ID, SessionArrivalWakeReason::Jump)
        .expect("reg2 replaces live DB ballpark cleanly");
    let h2_jita = reg2
        .wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Jump)
        .expect("reg2 wakes previously stopped system cleanly");
    assert_eq!(reg2.current_system_generation(OTHER_SYS_ID), Some(3));
    assert_eq!(reg2.current_system_generation(JITA_SYSTEM_ID), Some(4));

    let db_other = super::celestials::db_ballpark_entry_for_test(&db, OTHER_SYS_ID).unwrap();
    assert!(Arc::ptr_eq(&db_other.upgrade().unwrap(), &h2_other.ballpark));
    let db_jita = super::celestials::db_ballpark_entry_for_test(&db, JITA_SYSTEM_ID).unwrap();
    assert!(Arc::ptr_eq(&db_jita.upgrade().unwrap(), &h2_jita.ballpark));

    // Stopping reg1's older handle must NOT clear reg2's newer DB ballpark pointer.
    drop(h_other);
    assert!(reg1.stop_system(OTHER_SYS_ID).await);
    let db_other_after_reg1 =
        super::celestials::db_ballpark_entry_for_test(&db, OTHER_SYS_ID).unwrap();
    assert!(Arc::ptr_eq(
        &db_other_after_reg1.upgrade().unwrap(),
        &h2_other.ballpark
    ));

    // Stopping reg2's matching handles clears both entries cleanly.
    drop((h2_other, h2_jita));
    assert!(reg2.stop_system(OTHER_SYS_ID).await);
    assert!(reg2.stop_system(JITA_SYSTEM_ID).await);
    assert!(super::celestials::db_ballpark_entry_for_test(&db, OTHER_SYS_ID).is_none());
    assert!(super::celestials::db_ballpark_entry_for_test(&db, JITA_SYSTEM_ID).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p2_cancelled_stop_completes_and_closed_handle_generation_and_drop_order() {
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let reg = new_test_registry_with_db(Arc::clone(&db));

    // Part A: Closed Live handle reports Cold AND None generation consistently.
    let dead_handle = dummy_system_handle(JITA_SYSTEM_ID);
    assert!(dead_handle.cmd_tx.is_closed());
    reg.replace_live_handle_for_test(JITA_SYSTEM_ID, dead_handle);
    assert_eq!(
        reg.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Cold
    );
    assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), None);

    // Subsequent wake allocates generation 2 and replaces the terminated Live slot cleanly.
    let h1 = reg
        .wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Undock)
        .expect("wake over closed handle succeeds");
    assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), Some(2));

    // Part B1: Dropping a stop_system caller future while the actor mailbox is full does NOT cancel
    // or roll back Stopping -> Live; the spawned stop task keeps DB ballpark intact until ShutdownActor
    // sends, lets a concurrent stop_system wait on the gate, and drives teardown cleanly to Cold.
    let step_gate1 = TestGate::new();
    let step_gate1_hook = Arc::clone(&step_gate1);
    *h1.before_step_reply_hook_for_test.lock().unwrap() = Some(Box::new(move || {
        step_gate1_hook.enter_and_wait();
    }));
    let (tx1, _rx1) = tokio::sync::oneshot::channel();
    assert!(h1
        .cmd_tx
        .try_send(SystemCommand::StepManual { dt: 0.1, reply: tx1 })
        .is_ok());
    let wait_step1 = Arc::clone(&step_gate1);
    tokio::task::spawn_blocking(move || wait_step1.wait_until_entered())
        .await
        .unwrap();

    let mut drain_rxs = Vec::new();
    loop {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if h1
            .cmd_tx
            .try_send(SystemCommand::StepManual { dt: 0.0, reply: tx })
            .is_err()
        {
            break;
        }
        drain_rxs.push(rx);
    }

    let h1_weak = Arc::downgrade(&h1.ballpark);
    let h1_cmd_tx = h1.cmd_tx.clone();
    drop(h1);

    tokio::select! {
        _ = reg.stop_system(JITA_SYSTEM_ID) => panic!("stop_system should block on full mailbox"),
        _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {}
    }

    assert_eq!(
        reg.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Stopping
    );
    assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), Some(2));
    assert!(!h1_cmd_tx.is_closed());
    assert!(super::celestials::db_ballpark_entry_for_test(&db, JITA_SYSTEM_ID).is_some());

    let reg_stop2 = Arc::clone(&reg);
    let stop2 = tokio::spawn(async move { reg_stop2.stop_system(JITA_SYSTEM_ID).await });
    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    assert!(!stop2.is_finished());

    step_gate1.release();
    for rx in drain_rxs {
        let _ = rx.await;
    }

    assert!(stop2.await.unwrap());
    assert_eq!(
        reg.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Cold
    );
    assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), None);
    assert!(h1_cmd_tx.is_closed());
    assert!(
        h1_weak.upgrade().is_none(),
        "actor BallparkHandle Arc must be fully dropped when spawned stop settles"
    );
    assert!(super::celestials::db_ballpark_entry_for_test(&db, JITA_SYSTEM_ID).is_none());
    assert!(!reg.stop_system(JITA_SYSTEM_ID).await);

    // Part B2: Dropping stop_system AFTER ShutdownActor is sent completes to Cold without a second
    // stop_system call, clears DB ballpark once, and unblocks a concurrent wake for gen 4.
    let h2 = reg
        .wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::LoginInSpace)
        .expect("wake gen 3 succeeds");
    assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), Some(3));
    let step_gate2 = TestGate::new();
    let step_gate2_hook = Arc::clone(&step_gate2);
    *h2.before_step_reply_hook_for_test.lock().unwrap() = Some(Box::new(move || {
        step_gate2_hook.enter_and_wait();
    }));
    let (tx2, _rx2) = tokio::sync::oneshot::channel();
    assert!(h2
        .cmd_tx
        .try_send(SystemCommand::StepManual { dt: 0.1, reply: tx2 })
        .is_ok());
    let wait_step2 = Arc::clone(&step_gate2);
    tokio::task::spawn_blocking(move || wait_step2.wait_until_entered())
        .await
        .unwrap();

    let h2_weak = Arc::downgrade(&h2.ballpark);
    let h2_cmd_tx = h2.cmd_tx.clone();
    drop(h2);
    tokio::select! {
        _ = reg.stop_system(JITA_SYSTEM_ID) => panic!("stop_system should wait for paused actor shutdown reply"),
        _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {}
    }
    assert_eq!(
        reg.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Stopping
    );
    assert!(super::celestials::db_ballpark_entry_for_test(&db, JITA_SYSTEM_ID).is_none());

    let reg_wake = Arc::clone(&reg);
    let wake_task = tokio::task::spawn_blocking(move || {
        reg_wake.wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Jump)
    });
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert!(!wake_task.is_finished());

    step_gate2.release();
    let h3 = wake_task
        .await
        .unwrap()
        .expect("wake resumes once cancelled stop task settles to Cold");
    assert!(h2_cmd_tx.is_closed());
    assert!(
        h2_weak.upgrade().is_none(),
        "actor BallparkHandle Arc must be fully dropped before Stopping settles to Cold"
    );
    assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), Some(4));
    assert!(!reg
        .recent_lifecycle_transitions()
        .into_iter()
        .any(|rec| rec.from == SystemLifecycleState::Stopping && rec.to == SystemLifecycleState::Live));

    drop(h3);
    assert!(reg.stop_system(JITA_SYSTEM_ID).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf01_write_lock_cold_and_stopping_races_wait_cleanly() {
    const OTHER_SYS_ID: SolarSystemId = 30_000_144;
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let reg = new_test_registry_with_db(Arc::clone(&db));

    // 1. Write-lock `Loading` Pending wait: Caller 2 sees Cold in read probe, then enters
    //    `claim_loading_under_write_lock` after Caller 1 inserts Loading -> waits cleanly via `Wait`.
    let probe_gate1 = TestGate::new();
    let probe_hook1 = Arc::clone(&probe_gate1);
    let fired1 = Arc::new(AtomicBool::new(false));
    let fired1_hook = Arc::clone(&fired1);
    reg.set_after_read_probe_hook_for_test(Some(Arc::new(move |sys_id| {
        if sys_id == JITA_SYSTEM_ID
            && !fired1_hook.swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            probe_hook1.enter_and_wait();
        }
    })));
    let reg_c2 = Arc::clone(&reg);
    let c2_load = tokio::task::spawn_blocking(move || {
        reg_c2.wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Jump)
    });
    let wait_probe1 = Arc::clone(&probe_gate1);
    tokio::task::spawn_blocking(move || wait_probe1.wait_until_entered())
        .await
        .unwrap();
    reg.set_after_read_probe_hook_for_test(None);

    let pub_gate1 = TestGate::new();
    let pub_hook1 = Arc::clone(&pub_gate1);
    reg.set_before_publish_hook_for_test(Some(Arc::new(move |sys_id, _| {
        if sys_id == JITA_SYSTEM_ID {
            pub_hook1.enter_and_wait();
        }
    })));
    let reg_c1 = Arc::clone(&reg);
    let c1_load = tokio::task::spawn_blocking(move || {
        reg_c1.wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Undock)
    });
    let wait_pub1 = Arc::clone(&pub_gate1);
    tokio::task::spawn_blocking(move || wait_pub1.wait_until_entered())
        .await
        .unwrap();
    assert_eq!(
        reg.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Loading
    );

    probe_gate1.release();
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert!(!c2_load.is_finished());

    pub_gate1.release();
    let h1 = c1_load.await.unwrap().expect("owner completes gen 1");
    let h2 = c2_load
        .await
        .unwrap()
        .expect("write-lock racer over Loading Pending waits cleanly");
    assert!(Arc::ptr_eq(&h1.ballpark, &h2.ballpark));
    assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), Some(1));
    reg.set_before_publish_hook_for_test(None);
    drop((h1, h2));
    assert!(reg.stop_system(JITA_SYSTEM_ID).await);

    // 2. Write-lock `Stopping` Pending wait: Caller 2 sees Cold in read probe, then enters
    //    `claim_loading_under_write_lock` while Stopping (Pending) is active.
    let probe_gate2 = TestGate::new();
    let probe_hook2 = Arc::clone(&probe_gate2);
    let fired2 = Arc::new(AtomicBool::new(false));
    let fired2_hook = Arc::clone(&fired2);
    reg.set_after_read_probe_hook_for_test(Some(Arc::new(move |sys_id| {
        if sys_id == JITA_SYSTEM_ID
            && !fired2_hook.swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            probe_hook2.enter_and_wait();
        }
    })));
    let reg_c2_stop = Arc::clone(&reg);
    let c2_stop = tokio::task::spawn_blocking(move || {
        reg_c2_stop.wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Jump)
    });
    let wait_probe2 = Arc::clone(&probe_gate2);
    tokio::task::spawn_blocking(move || wait_probe2.wait_until_entered())
        .await
        .unwrap();
    reg.set_after_read_probe_hook_for_test(None);

    let h_tmp = reg
        .wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Undock)
        .expect("wake gen 2");
    drop(h_tmp);
    let stop_gate = TestGate::new();
    let stop_hook = Arc::clone(&stop_gate);
    reg.set_before_stop_finish_hook_for_test(Some(Arc::new(move |sys_id, _| {
        if sys_id == JITA_SYSTEM_ID {
            stop_hook.enter_and_wait();
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

    probe_gate2.release();
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert!(!c2_stop.is_finished());

    stop_gate.release();
    assert!(stop_task.await.unwrap());
    let h3 = c2_stop
        .await
        .unwrap()
        .expect("write-lock racer over Stopping Pending waits and wakes gen 3 cleanly");
    assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), Some(3));
    reg.set_before_stop_finish_hook_for_test(None);
    drop(h3);
    assert!(reg.stop_system(JITA_SYSTEM_ID).await);

    // 3. Write-lock `Stopping` race after caller future drops mid-drain still waits on Pending gate.
    let probe_gate3 = TestGate::new();
    let probe_hook3 = Arc::clone(&probe_gate3);
    let fired3 = Arc::new(AtomicBool::new(false));
    let fired3_hook = Arc::clone(&fired3);
    reg.set_after_read_probe_hook_for_test(Some(Arc::new(move |sys_id| {
        if sys_id == JITA_SYSTEM_ID
            && !fired3_hook.swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            probe_hook3.enter_and_wait();
        }
    })));
    let reg_c2_drain = Arc::clone(&reg);
    let c2_drain = tokio::task::spawn_blocking(move || {
        reg_c2_drain
            .wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::LoginInSpace)
    });
    let wait_probe3 = Arc::clone(&probe_gate3);
    tokio::task::spawn_blocking(move || wait_probe3.wait_until_entered())
        .await
        .unwrap();
    reg.set_after_read_probe_hook_for_test(None);

    let h4 = reg
        .wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Undock)
        .expect("wake gen 4");
    let step_gate = TestGate::new();
    let step_hook = Arc::clone(&step_gate);
    *h4.before_step_reply_hook_for_test.lock().unwrap() = Some(Box::new(move || {
        step_hook.enter_and_wait();
    }));
    let (tx4, _rx4) = tokio::sync::oneshot::channel();
    assert!(h4
        .cmd_tx
        .try_send(SystemCommand::StepManual { dt: 0.1, reply: tx4 })
        .is_ok());
    let wait_step4 = Arc::clone(&step_gate);
    tokio::task::spawn_blocking(move || wait_step4.wait_until_entered())
        .await
        .unwrap();
    drop(h4);
    tokio::select! {
        _ = reg.stop_system(JITA_SYSTEM_ID) => panic!("stop_system should pause awaiting ShutdownActor reply"),
        _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {}
    }

    probe_gate3.release();
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert!(!c2_drain.is_finished());

    step_gate.release();
    let h5 = c2_drain
        .await
        .unwrap()
        .expect("write-lock racer over cancelled draining Stopping waits cleanly via gate");
    assert_eq!(reg.current_system_generation(JITA_SYSTEM_ID), Some(5));
    drop(h5);
    assert!(reg.stop_system(JITA_SYSTEM_ID).await);

    // 4. Lower-ranked lock (`RANK_NPC_SPAWNS = 5 < RANK_SYSTEM_REGISTRY`) sampled before
    //    `systems.write()` may still claim Cold systems, but must reject Wait inside write lock.
    let probe_gate4 = TestGate::new();
    let probe_hook4 = Arc::clone(&probe_gate4);
    let fired4 = Arc::new(AtomicBool::new(false));
    let fired4_hook = Arc::clone(&fired4);
    reg.set_after_read_probe_hook_for_test(Some(Arc::new(move |sys_id| {
        if sys_id == JITA_SYSTEM_ID
            && !fired4_hook.swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            probe_hook4.enter_and_wait();
        }
    })));
    let reg_npc = Arc::clone(&reg);
    let npc_task = tokio::task::spawn_blocking(move || {
        let npc_lock: RankedMutex<(), { eve_db::lock_rank::RANK_NPC_SPAWNS }> =
            RankedMutex::new(());
        let _npc_guard = npc_lock.lock().unwrap();
        let other_h = reg_npc
            .wake_for_session_arrival(OTHER_SYS_ID, SessionArrivalWakeReason::Undock)
            .expect("Cold claim under RANK_NPC_SPAWNS < RANK_SYSTEM_REGISTRY succeeds");
        drop(other_h);
        reg_npc.wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Jump)
    });
    let wait_probe4 = Arc::clone(&probe_gate4);
    tokio::task::spawn_blocking(move || wait_probe4.wait_until_entered())
        .await
        .unwrap();
    reg.set_after_read_probe_hook_for_test(None);

    let pub_gate4 = TestGate::new();
    let pub_hook4 = Arc::clone(&pub_gate4);
    reg.set_before_publish_hook_for_test(Some(Arc::new(move |sys_id, _| {
        if sys_id == JITA_SYSTEM_ID {
            pub_hook4.enter_and_wait();
        }
    })));
    let reg_owner4 = Arc::clone(&reg);
    let owner4 = tokio::task::spawn_blocking(move || {
        reg_owner4.wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Undock)
    });
    let wait_pub4 = Arc::clone(&pub_gate4);
    tokio::task::spawn_blocking(move || wait_pub4.wait_until_entered())
        .await
        .unwrap();

    probe_gate4.release();
    assert_eq!(
        npc_task.await.unwrap().err(),
        Some(SystemActivationError::WaitRejectedWhileHoldingLocks {
            solar_system_id: JITA_SYSTEM_ID,
        })
    );

    pub_gate4.release();
    reg.set_before_publish_hook_for_test(None);
    drop(owner4.await.unwrap().unwrap());
    assert!(reg.stop_system(JITA_SYSTEM_ID).await);
    assert!(reg.stop_system(OTHER_SYS_ID).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf03_full_mailbox_unregistered_handle_shutdown() {
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let reg = new_test_registry_with_db(Arc::clone(&db));

    // `shutdown_unregistered_handle` spawns async delivery when the mailbox is full.
    let h2 = reg
        .wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Jump)
        .expect("wake gen 1");
    let step_gate = TestGate::new();
    let step_hook = Arc::clone(&step_gate);
    *h2.before_step_reply_hook_for_test.lock().unwrap() = Some(Box::new(move || {
        step_hook.enter_and_wait();
    }));
    let (tx_pause, _rx_pause) = tokio::sync::oneshot::channel();
    assert!(h2
        .cmd_tx
        .try_send(SystemCommand::StepManual {
            dt: 0.1,
            reply: tx_pause,
        })
        .is_ok());
    let wait_step = Arc::clone(&step_gate);
    tokio::task::spawn_blocking(move || wait_step.wait_until_entered())
        .await
        .unwrap();

    let mut drain_rxs = Vec::new();
    loop {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if h2
            .cmd_tx
            .try_send(SystemCommand::StepManual { dt: 0.0, reply: tx })
            .is_err()
        {
            break;
        }
        drain_rxs.push(rx);
    }

    let h2_weak = Arc::downgrade(&h2.ballpark);
    let h2_cmd_tx = h2.cmd_tx.clone();
    reg.replace_live_handle_for_test(JITA_SYSTEM_ID, dummy_system_handle(JITA_SYSTEM_ID));
    reg.shutdown_unregistered_handle(h2);
    assert!(!h2_cmd_tx.is_closed());

    step_gate.release();
    for rx in drain_rxs {
        let _ = rx.await;
    }
    tokio::time::timeout(std::time::Duration::from_secs(2), h2_cmd_tx.closed())
        .await
        .expect("spawned delivery task must drain full mailbox and close actor");
    assert!(h2_weak.upgrade().is_none());
    assert!(!reg.stop_system(JITA_SYSTEM_ID).await);
    assert_eq!(
        reg.system_lifecycle_state(JITA_SYSTEM_ID),
        SystemLifecycleState::Cold
    );
}

#[test]
fn bf04_non_tokio_runtime_helpers_return_none_while_holding_locks() {
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let reg = new_test_registry_with_db(Arc::clone(&db));

    // Holding DB write lock (`RANK_WORLD_INNER`) on a non-Tokio thread returns None cleanly.
    let db_res = db.tx("bf04_non_tokio_db_write", |_wtx| {
        assert!(matches!(
            reg.try_wake_if_runtime(JITA_SYSTEM_ID, SystemWakeReason::LegacyGetOrSpawn),
            Ok(None)
        ));
        assert!(reg.get_or_spawn_if_runtime(JITA_SYSTEM_ID).is_none());
        Ok::<_, eve_db::TxError>(())
    });
    assert!(db_res.is_ok());

    // Holding `RANK_BALLPARK` on a non-Tokio thread returns None cleanly.
    let bp_lock: RankedRwLock<(), { eve_db::lock_rank::RANK_BALLPARK }> = RankedRwLock::new(());
    {
        let _bp_guard = bp_lock.read().unwrap();
        assert!(matches!(
            reg.try_wake_if_runtime(JITA_SYSTEM_ID, SystemWakeReason::LegacyGetOrSpawn),
            Ok(None)
        ));
        assert!(reg.get_or_spawn_if_runtime(JITA_SYSTEM_ID).is_none());
    }

    // Holding `RANK_SYSTEM_REGISTRY` on a non-Tokio thread returns None cleanly.
    {
        let _sys_guard = reg.systems.read().unwrap();
        assert!(matches!(
            reg.try_wake_if_runtime(JITA_SYSTEM_ID, SystemWakeReason::LegacyGetOrSpawn),
            Ok(None)
        ));
        assert!(reg.get_or_spawn_if_runtime(JITA_SYSTEM_ID).is_none());
    }

    // Already-Live system remains accessible on a non-Tokio thread even while holding DB/ballpark locks.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let live_handle = rt.block_on(async {
        reg.wake_for_session_arrival(JITA_SYSTEM_ID, SessionArrivalWakeReason::Undock)
            .expect("wake live handle")
    });
    {
        let _bp_guard = live_handle.ballpark.read().unwrap();
        let got = reg
            .get_or_spawn_if_runtime(JITA_SYSTEM_ID)
            .expect("live handle returned cleanly off-runtime");
        assert!(Arc::ptr_eq(&got.ballpark, &live_handle.ballpark));
        assert!(matches!(
            reg.try_wake_if_runtime(JITA_SYSTEM_ID, SystemWakeReason::LegacyGetOrSpawn),
            Ok(Some(_))
        ));
    }
    drop(live_handle);
    assert!(rt.block_on(reg.stop_system(JITA_SYSTEM_ID)));
}
