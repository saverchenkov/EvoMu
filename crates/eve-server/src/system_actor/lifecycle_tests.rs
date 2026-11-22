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

    let distinct: HashSet<SystemLifecycleState> = [
        SystemLifecycleState::Cold,
        SystemLifecycleState::Loading,
        SystemLifecycleState::Live,
        SystemLifecycleState::Hibernating,
    ]
    .into_iter()
    .collect();
    assert_eq!(distinct.len(), 4);
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
        report.rules[RULE_GET_OR_SPAWN]
            .observed
            .get("system_actor.rs")
            .copied()
            .unwrap_or(0)
            <= 2
    );
}
