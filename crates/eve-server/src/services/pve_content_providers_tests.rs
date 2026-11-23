use super::*;
use crate::router::ServerContext;
use crate::services::pve_special_sites::{
    complete_special_dungeon_instance, handle_abyssal_entrance_deployment, handle_pve_special_sites,
    load_triglavian_destroyed_escorts, read_pve_special_reg,
};
use crate::services::space_mining_harvest::{WORMHOLE_GUARD_TYPE_IDS, WORMHOLE_ORE_SITES};
use crate::system_actor::{
    SessionArrivalWakeReason, SystemActivationError, SystemContentProviderMetadata,
    SystemLifecycleState, SystemRegistry,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{
    AsteroidRecord, CosmicAnomalyRecord, InventoryFlag, InventoryItemRecord,
    SeasonalEventSiteRecord, WorldRepository,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn first_pochven_system(sde: &SdeRegistry) -> SolarSystemId {
    sde.solar_systems
        .values()
        .find(|s| s.region_id == 10_000_070)
        .map(|s| s.solar_system_id)
        .expect("Pochven solar system in SDE")
}

fn new_ctx(base_unix: u64) -> ServerContext {
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), Arc::new(WorldRepository::new_empty()));
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));
    ctx
}

struct TestSeedFailProvider {
    target_sys: SolarSystemId,
    should_fail: Arc<AtomicBool>,
}

impl SystemContentProvider for TestSeedFailProvider {
    fn name(&self) -> &'static str {
        "test_seed_fail_provider"
    }

    fn seed(&self, seed_ctx: &mut SystemContentSeedContext<'_>) -> Result<(), String> {
        if seed_ctx.solar_system_id == self.target_sys && self.should_fail.load(Ordering::Acquire) {
            return Err(format!("injected wake failure for {}", self.target_sys));
        }
        Ok(())
    }
}

fn provision_space_gm_session(
    ctx: &ServerContext,
    account_name: &str,
    char_id: i64,
    sys_id: SolarSystemId,
) -> SessionState {
    ctx.db.provision_gm_account(account_name, "pw", char_id, account_name);
    let (user_id, ship_id) = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).expect("provisioned char");
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        let uid = ch.user_id;
        let ship_id = ch.active_ship_id;
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = i64::from(sys_id);
        }
        (uid, ship_id)
    });
    let mut sess = SessionState::new(711);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.role = service_roles::ROLE_GM_MASK;
    sess
}

#[test]
fn bare_registry_starts_empty_and_server_context_registers_t2a_providers_in_order() {
    let bare = SystemRegistry::new();
    assert_eq!(bare.registered_content_providers(), Ok(Vec::new()));

    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::new_empty()),
    );
    let expected = vec![
        SystemContentProviderMetadata {
            name: PROVIDER_SPECIAL_SITES,
            has_tick: true,
            has_expire: true,
        },
        SystemContentProviderMetadata {
            name: PROVIDER_RESOURCE_SITES,
            has_tick: true,
            has_expire: false,
        },
    ];
    assert_eq!(
        ctx.systems.registered_content_providers(),
        Ok(expected.clone())
    );

    // Repeated registration is idempotent and preserves deterministic ordering.
    register_default_pve_content_providers(
        &ctx.systems,
        Arc::clone(&ctx.sde),
        Arc::clone(&ctx.db),
        Arc::clone(&ctx.combat_ephemeral),
        Arc::clone(&ctx.mechanic_rng),
        Arc::clone(&ctx.phased_asteroid_fields),
    )
    .expect("idempotent default provider registration");
    assert_eq!(ctx.systems.registered_content_providers(), Ok(expected));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_read_rpcs_do_not_wake_systems_seed_content_or_write_db() {
    let (jita, trig_sys, drifter_sys): (SolarSystemId, SolarSystemId, SolarSystemId) =
        (30_000_142, 30_000_012, 31_000_001);
    let ctx = new_ctx(1_776_000_000);
    let mut sess = provision_space_gm_session(&ctx, "t2a_cold_probe", 90_542_001, jita);
    let kw = HashMap::new();

    for sys in [jita, trig_sys, drifter_sys] {
        assert_eq!(ctx.systems.system_lifecycle_state(sys), SystemLifecycleState::Cold);
        assert!(ctx.systems.get_handle(sys).is_none());
        assert_eq!(ctx.systems.current_system_generation(sys), None);
    }

    let epoch0 = ctx.db.state_epoch();
    let commits0 = ctx.db.tx_commit_count();
    for (service, method, args) in [
        ("dungeonInstanceCacheMgr", "GetTriglavianSiteInstances", vec![PyRep::Integer(i64::from(trig_sys))]),
        ("dungeonInstanceCacheMgr", "GetSignatureInstances", vec![PyRep::Integer(i64::from(drifter_sys))]),
        ("map", "GetAllRoamingWeatherSystems", Vec::new()),
    ] {
        let rep = handle_pve_special_sites(&ctx, &mut sess, service, method, &args, &kw)
            .await
            .expect("special_sites handled")
            .expect("special_sites read ok");
        assert!(!matches!(rep, PyRep::None), "{service}.{method} cold response");
    }

    assert_eq!(ctx.db.state_epoch(), epoch0, "cold reads must not bump state_epoch");
    assert_eq!(ctx.db.tx_commit_count(), commits0, "cold reads must not write DB");
    for sys in [jita, trig_sys, drifter_sys] {
        assert_eq!(ctx.systems.system_lifecycle_state(sys), SystemLifecycleState::Cold, "cold read must not wake {sys}");
        assert!(ctx.systems.get_handle(sys).is_none());
        assert_eq!(ctx.systems.current_system_generation(sys), None);
    }
    ctx.db.with_snapshot(|snap| {
        assert!(snap.cosmic_anomalies.is_empty());
        assert!(snap.cosmic_signatures.is_empty());
        assert!(snap.pve_special_sites.triglavian_sites.is_empty());
        assert!(snap.pve_special_sites.drifter_hives.is_empty());
        assert!(snap.pve_special_sites.roaming_storms.is_empty());
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activation_seeds_t2a_providers_deterministically_and_idle_maintenance_skips_db_writes() {
    let (jita, trig_sys, drifter_sys): (SolarSystemId, SolarSystemId, SolarSystemId) =
        (30_000_142, 30_000_012, 31_000_001);
    let t0: u64 = 1_776_000_000;
    let ctx = new_ctx(t0);
    let mut sess = provision_space_gm_session(&ctx, "t2a_seed_verify", 90_542_002, jita);
    let kw = HashMap::new();

    // Activating Jita stays read-pure; stop -> reactivate rehydrates persisted non-belt asteroids.
    let (epoch_pre_jita, commits_pre_jita) = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    let jita_handle = ctx.systems.get_or_spawn(jita);
    assert_eq!(ctx.db.state_epoch(), epoch_pre_jita);
    assert_eq!(ctx.db.tx_commit_count(), commits_pre_jita);
    assert_eq!(jita_handle.dynamic_ball_count.load(Ordering::Relaxed), 0);
    assert_eq!(ctx.systems.system_lifecycle_state(jita), SystemLifecycleState::Hibernating);

    crate::services::mining_sites::seed_system_mining_sites_from_sde(&ctx, jita).await;
    let persisted_ast_id: i64 = ctx.db.with_snapshot(|snap| {
        assert!(
            snap.cosmic_anomalies.get(&jita).is_some_and(|v| v.iter().any(|a| a.anomaly_type == "OreAnomaly"))
                || snap.cosmic_signatures.get(&jita).is_some_and(|v| !v.is_empty())
        );
        snap.asteroids
            .values()
            .find(|a| a.solar_system_id == jita && a.belt_id == 0 && a.quantity > 0)
            .map(|a| a.asteroid_id)
            .expect("persisted non-belt asteroid in Jita")
    });
    assert!(ctx.systems.stop_system(jita).await);
    let (epoch_rehydrate, commits_rehydrate) = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    let jita_rehydrated = ctx.systems.get_or_spawn(jita);
    assert_eq!(ctx.db.state_epoch(), epoch_rehydrate);
    assert_eq!(ctx.db.tx_commit_count(), commits_rehydrate);
    assert!(jita_rehydrated.view().balls.contains_key(&persisted_ast_id));

    for sys_id in [trig_sys, drifter_sys] {
        let handle = ctx.systems.get_or_spawn(sys_id);
        assert!(ctx.systems.get_handle(sys_id).is_some());
        let expected_state = if handle.dynamic_ball_count.load(Ordering::Relaxed) == 0 {
            SystemLifecycleState::Hibernating
        } else {
            SystemLifecycleState::Live
        };
        assert_eq!(ctx.systems.system_lifecycle_state(sys_id), expected_state);
        let transitions = ctx.systems.recent_lifecycle_transitions();
        let cold_to_loading = transitions.iter().position(|r| {
            r.solar_system_id == sys_id && r.from == SystemLifecycleState::Cold && r.to == SystemLifecycleState::Loading
        });
        let loading_to_live = transitions.iter().position(|r| {
            r.solar_system_id == sys_id && r.from == SystemLifecycleState::Loading && r.to == SystemLifecycleState::Live
        });
        assert!(matches!((cold_to_loading, loading_to_live), (Some(a), Some(b)) if a < b));
    }

    // F-03: activating 31_000_001 seeds ONLY 31_000_001 and leaves sibling Drifter Hives cold.
    assert!(read_pve_special_reg(&ctx, |reg| {
        !reg.triglavian_minor_victory_systems.is_empty()
            && reg.triglavian_sites.get(&trig_sys).is_some_and(|v| !v.is_empty())
            && reg.drifter_hives.contains_key(&drifter_sys)
            && !(31_000_002..=31_000_005).any(|sid| reg.drifter_hives.contains_key(&sid))
    }));
    for sibling in 31_000_002..=31_000_005 {
        assert_eq!(ctx.systems.system_lifecycle_state(sibling), SystemLifecycleState::Cold);
    }

    let (epoch_seeded, commits_seeded) = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    let trig_rep = handle_pve_special_sites(
        &ctx, &mut sess, "dungeonInstanceCacheMgr", "GetTriglavianSiteInstances", &[PyRep::Integer(i64::from(trig_sys))], &kw,
    )
    .await
    .expect("handled")
    .expect("seeded triglavian dict");
    let PyRep::Dict(trig_outer) = trig_rep else { panic!("expected dict") };
    let (_, PyRep::List(trig_entries)) = &trig_outer[0] else { panic!("expected list") };
    assert_eq!(trig_entries.len(), 2);
    assert_eq!(ctx.db.state_epoch(), epoch_seeded);
    assert_eq!(ctx.db.tx_commit_count(), commits_seeded);
    assert_eq!(ctx.systems.run_content_maintenance_for_active_systems(t0 + 60), Ok(3));
    assert_eq!(ctx.systems.run_content_maintenance_for_active_systems(t0 + 120), Ok(3));
    assert_eq!(ctx.db.state_epoch(), epoch_seeded);
    assert_eq!(ctx.db.tx_commit_count(), commits_seeded);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn maintenance_expire_and_stop_reactivation_preserve_pruned_state() {
    let trig_sys: SolarSystemId = 30_000_012;
    let ctx = new_ctx(1_776_100_000);
    let _h1 = ctx.systems.get_or_spawn(trig_sys);

    let expired_inst_id: i64 = 99_542_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.pve_special_sites.seasonal_event_sites.entry(trig_sys).or_default().push(SeasonalEventSiteRecord {
            instance_id: expired_inst_id,
            season_id: 77_001,
            dungeon_id: 999_542,
            dungeon_name: "Expired Seasonal Probe".into(),
            solar_system_id: trig_sys,
            window_start_ft: 1,
            window_end_ft: 100,
            completed: false,
            ..Default::default()
        });
    });

    ctx.advance_secs(30);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.pve_special_sites.seasonal_event_sites.get(&trig_sys).is_some_and(|l| l.iter().any(|s| s.instance_id == expired_inst_id)));
    });

    assert!(ctx.systems.stop_system(trig_sys).await);
    assert_eq!(ctx.systems.system_lifecycle_state(trig_sys), SystemLifecycleState::Cold);
    let _h2 = ctx.systems.get_or_spawn(trig_sys);
    assert_eq!(ctx.systems.system_lifecycle_state(trig_sys), SystemLifecycleState::Live);
    ctx.db.with_snapshot(|snap| {
        assert!(snap.pve_special_sites.seasonal_event_sites.get(&trig_sys).is_none_or(|l| !l.iter().any(|s| s.instance_id == expired_inst_id)));
        assert!(snap.pve_special_sites.triglavian_sites.get(&trig_sys).is_some_and(|l| !l.is_empty()));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_tick_seasonal_rehydration_publishes_view_and_registers_ai_f01() {
    let jita: SolarSystemId = 30_000_142;
    let t0: u64 = 1_776_200_000;
    let ctx = new_ctx(t0);
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        let mut cfg = crate::services::season_fsd::load_season_config(snap);
        cfg.start_time_ft = now_ft - 3_600 * 10_000_000;
        cfg.end_time_ft = now_ft + 3_600 * 10_000_000;
        snap.seasons_and_campaigns.season_configuration = Some(cfg);
    });

    let sites = crate::services::pve_special_sites::ensure_seasonal_event_sites_for_system(&ctx, jita).await;
    assert_eq!(sites.len(), 1);
    let (boss_bid, escort_bids) = (sites[0].boss_npc_ball_id, sites[0].escort_ball_ids.clone());
    let handle = ctx.systems.get_handle(jita).expect("live jita handle");
    handle.try_remove_ball(boss_bid);
    for &ebid in &escort_bids { handle.try_remove_ball(ebid); }
    {
        let mut rt = ctx.combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
        rt.npc_entities.remove(&boss_bid);
        for &ebid in &escort_bids { rt.npc_entities.remove(&ebid); }
    }
    assert!(!handle.view().balls.contains_key(&boss_bid));

    assert_eq!(ctx.systems.run_content_maintenance_for_active_systems(t0 + 60), Ok(1));
    let view = handle.view();
    assert!(view.balls.contains_key(&boss_bid));
    for &ebid in &escort_bids { assert!(view.balls.contains_key(&ebid)); }
    let rt = ctx.combat_ephemeral.read().unwrap_or_else(|e| e.into_inner());
    assert!(rt.npc_entities.contains_key(&boss_bid));
    for &ebid in &escort_bids { assert!(rt.npc_entities.contains_key(&ebid)); }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abyssal_cold_fail_closed_f04_and_seasonal_cold_fail_closed_f05() {
    let jita: SolarSystemId = 30_000_142;
    let (char_id, filament_id): (i64, i64) = (90_542_010, 99_542_010);
    let ctx = new_ctx(1_776_300_000);
    let mut sess = provision_space_gm_session(&ctx, "t2a_f04_f05", char_id, jita);
    let ship_id = sess.ship_id.expect("active ship");
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(filament_id, InventoryItemRecord {
            item_id: filament_id, type_id: 56131, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::Cargo, quantity: 1, singleton: false, version: 1, ..Default::default()
        });
        let mut cfg = crate::services::season_fsd::load_season_config(snap);
        cfg.start_time_ft = now_ft - 3_600 * 10_000_000;
        cfg.end_time_ft = now_ft + 3_600 * 10_000_000;
        snap.seasons_and_campaigns.season_configuration = Some(cfg);
    });
    assert!(ctx.systems.get_handle(jita).is_none());

    let err = handle_abyssal_entrance_deployment(&ctx, &mut sess, &[PyRep::Integer(filament_id)])
        .await
        .expect_err("cold abyssal filament deployment must fail");
    assert_eq!(err.code, "ShipNotInSpace");
    ctx.db.with_snapshot(|snap| assert_eq!(snap.items.get(&filament_id).map(|i| i.quantity), Some(1)));
    assert!(crate::services::pve_special_sites_abyssal::read_abyssal_state(&ctx, |st| st.traces.is_empty()));

    let (ep0, cm0) = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    assert!(crate::services::pve_special_sites::ensure_seasonal_event_sites_if_live_for_system(&ctx, jita).await.is_empty());
    crate::services::pve_special_sites::ensure_special_sites_spawned_if_live_for_system(&ctx, jita).await;
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), (ep0, cm0));
    assert!(ctx.systems.get_handle(jita).is_none());

    let missing_sys: SolarSystemId = 99_999_999;
    ctx.systems
        .register_content_provider(Arc::new(TestSeedFailProvider { target_sys: missing_sys, should_fail: Arc::new(AtomicBool::new(true)) }))
        .expect("register wake failure provider");
    assert!(crate::services::pve_special_sites::ensure_seasonal_event_sites_for_system(&ctx, missing_sys).await.is_empty());
    assert_eq!(ctx.db.tx_commit_count(), cm0);
    assert!(ctx.systems.get_handle(missing_sys).is_none());
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.pve_special_sites.seasonal_event_sites.contains_key(&missing_sys));
        assert!(!snap.cosmic_anomalies.contains_key(&missing_sys));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pochven_reactivation_prunes_stale_hypnosian_ai_f06_and_dead_live_stop_clears_markers_f02() {
    let ctx = new_ctx(1_776_400_000);
    let pochven_sys = first_pochven_system(&ctx.sde);
    let t0 = ctx.now_unix_secs();

    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies.entry(pochven_sys).or_default().push(CosmicAnomalyRecord {
            anomaly_id: "DNG-12345-900001".into(), solar_system_id: pochven_sys,
            name: "Pochven Test Ore Site".into(), anomaly_type: "OreAnomaly".into(),
            ore_types: Vec::new(), signal_strength: 1.0, position: [15_000.0, 0.0, 0.0],
            cleared: false, remaining_volume: 1_000.0, created_unix: t0, lifetime_seconds: 86_400,
        });
        snap.asteroids.insert(99_542_100, AsteroidRecord {
            asteroid_id: 99_542_100, solar_system_id: pochven_sys, belt_id: 0, type_id: 1228,
            quantity: 100, initial_quantity: 100, position: (15_000.0, 0.0, 0.0), radius: 200.0,
        });
    });

    let _h1 = ctx.systems.get_or_spawn(pochven_sys);
    assert!(ctx.combat_ephemeral.read().unwrap_or_else(|e| e.into_inner()).npc_entities.values().any(|n| n.solar_system_id == pochven_sys && n.archetype == "PochvenHypnosianSleeper"));
    assert!(ctx.systems.stop_system(pochven_sys).await);
    let stale_bid: i64 = 1_899_542_999;
    {
        let mut rt = ctx.combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
        if let Some(mut stale) = rt.npc_entities.values().next().cloned() {
            stale.ball_id = stale_bid;
            stale.solar_system_id = pochven_sys;
            stale.archetype = "PochvenHypnosianSleeper".to_string();
            rt.npc_entities.insert(stale_bid, stale);
        }
    }

    let h2 = ctx.systems.get_or_spawn(pochven_sys);
    {
        let rt = ctx.combat_ephemeral.read().unwrap_or_else(|e| e.into_inner());
        assert!(!rt.npc_entities.contains_key(&stale_bid), "F-06: reactivation must prune stale Hypnosian AI");
        let live_ids: Vec<i64> = rt.npc_entities.iter().filter(|(_, n)| n.solar_system_id == pochven_sys && n.archetype == "PochvenHypnosianSleeper").map(|(&bid, _)| bid).collect();
        assert!(!live_ids.is_empty());
        for bid in live_ids { assert!(h2.view().balls.contains_key(&bid)); }
    }

    let any_trig_bid = read_pve_special_reg(&ctx, |reg| reg.triglavian_sites.get(&pochven_sys).and_then(|v| v.first()).map(|s| s.boss_npc_ball_id)).expect("triglavian site boss ball");
    assert!(h2.view().balls.contains_key(&any_trig_bid));
    let (tx, rx) = tokio::sync::oneshot::channel();
    h2.cmd_tx.send(crate::system_actor::SystemCommand::ShutdownActor { reply: tx }).await.expect("shutdown actor");
    let _ = rx.await;
    h2.cmd_tx.closed().await;
    assert!(!ctx.systems.stop_system(pochven_sys).await);
    assert!(!ctx.systems.is_special_site_ball_spawned(pochven_sys, any_trig_bid));
    let h3 = ctx.systems.get_or_spawn(pochven_sys);
    assert!(h3.view().balls.contains_key(&any_trig_bid));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf01_failed_late_seed_does_not_leak_markers_and_subsequent_wake_spawns() {
    let t0: u64 = 1_776_410_000;
    let ctx = new_ctx(t0);
    let pochven_sys = first_pochven_system(&ctx.sde);
    let wh_anom_id = "WH-BF01-ORE-1".to_string();
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies.entry(pochven_sys).or_default().push(CosmicAnomalyRecord {
            anomaly_id: wh_anom_id.clone(), solar_system_id: pochven_sys,
            name: WORMHOLE_ORE_SITES[0].dungeon_name.into(), anomaly_type: "OreAnomaly".into(),
            ore_types: Vec::new(), signal_strength: 1.0, position: [25_000.0, 0.0, 0.0],
            cleared: false, remaining_volume: 500.0, created_unix: t0, lifetime_seconds: 86_400,
        });
    });
    let should_fail = Arc::new(AtomicBool::new(true));
    ctx.systems.register_content_provider(Arc::new(TestSeedFailProvider { target_sys: pochven_sys, should_fail: Arc::clone(&should_fail) })).expect("register fail provider");

    let err = ctx.systems.wake_for_session_arrival_async(pochven_sys, SessionArrivalWakeReason::Jump).await.err().expect("must abort");
    assert!(matches!(err, SystemActivationError::ContentProviderFailed { ref provider, .. } if provider == "test_seed_fail_provider"));
    assert!(ctx.systems.get_handle(pochven_sys).is_none());
    let trig_bid = read_pve_special_reg(&ctx, |reg| reg.triglavian_sites.get(&pochven_sys).and_then(|v| v.first()).map(|s| s.boss_npc_ball_id)).expect("triglavian site");
    assert!(!ctx.systems.is_special_site_ball_spawned(pochven_sys, trig_bid));
    assert!(!ctx.systems.is_wh_ore_anomaly_hydrated(pochven_sys, &wh_anom_id));

    should_fail.store(false, Ordering::Release);
    let h = ctx.systems.get_or_spawn(pochven_sys);
    assert!(h.view().balls.contains_key(&trig_bid));
    assert!(ctx.systems.is_special_site_ball_spawned(pochven_sys, trig_bid));
    assert!(ctx.systems.is_wh_ore_anomaly_hydrated(pochven_sys, &wh_anom_id));
    assert!(h.view().balls.values().any(|b| WORMHOLE_GUARD_TYPE_IDS.contains(&b.type_id)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf02_partial_triglavian_escort_kill_survives_stop_reactivation_and_completes() {
    let ctx = new_ctx(1_776_420_000);
    let pochven_sys = first_pochven_system(&ctx.sde);
    let mut sess = provision_space_gm_session(&ctx, "bf02_pilot", 90_542_091, pochven_sys);
    let ship_id = sess.ship_id.expect("ship_id");
    ctx.db.with_mut_snapshot(|snap| { snap.item_positions.insert(ship_id, [10_000.0, 0.0, 0.0]); });

    let h1 = ctx.systems.get_or_spawn(pochven_sys);
    let trig = read_pve_special_reg(&ctx, |reg| reg.triglavian_sites.get(&pochven_sys).and_then(|v| v.first()).cloned()).expect("triglavian site");
    let killed_escort = trig.escort_ball_ids[0];
    let _ = h1.remove_ball(killed_escort).await;
    let _ = crate::services::pve_special_sites_combat::on_special_site_npc_destroyed_core(&ctx.systems, &ctx.db, &ctx.sde, pochven_sys, killed_escort, Some(90_542_091)).await;
    assert!(ctx.db.with_snapshot(load_triglavian_destroyed_escorts).contains(&killed_escort));

    assert!(ctx.systems.stop_system(pochven_sys).await);
    let h2 = ctx.systems.get_or_spawn(pochven_sys);
    assert!(!h2.view().balls.contains_key(&killed_escort));
    assert!(h2.view().balls.contains_key(&trig.boss_npc_ball_id));

    let _ = h2.remove_ball(trig.boss_npc_ball_id).await;
    for &ebid in &trig.escort_ball_ids[1..] { let _ = h2.remove_ball(ebid).await; }
    let res = complete_special_dungeon_instance(&ctx, &mut sess, trig.instance_id, 1).await.expect("completion succeeds");
    assert_eq!(res.dict_get("completed").and_then(|v| v.as_bool()), Some(true));
    ctx.db.with_snapshot(|snap| {
        let destroyed = load_triglavian_destroyed_escorts(snap);
        for ebid in &trig.escort_ball_ids { assert!(!destroyed.contains(ebid)); }
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf03_cold_pochven_mining_seed_stays_cold_and_spawns_single_hypnosian_wave() {
    let ctx = new_ctx(1_776_430_000);
    let pochven_sys = first_pochven_system(&ctx.sde);
    assert_eq!(ctx.systems.system_lifecycle_state(pochven_sys), SystemLifecycleState::Cold);
    crate::services::mining_sites::seed_system_mining_sites_from_sde(&ctx, pochven_sys).await;
    assert!(ctx.systems.get_handle(pochven_sys).is_none());
    let ore_site_count = ctx.db.with_snapshot(|snap| snap.cosmic_anomalies.get(&pochven_sys).map(|l| l.iter().filter(|a| a.anomaly_type == "OreAnomaly").count()).unwrap_or(0));
    assert!(ore_site_count > 0);
    let h = ctx.systems.get_or_spawn(pochven_sys);
    let rt = ctx.combat_ephemeral.read().unwrap_or_else(|e| e.into_inner());
    let hyp: Vec<i64> = rt.npc_entities.iter().filter(|(_, n)| n.solar_system_id == pochven_sys && n.archetype == "PochvenHypnosianSleeper").map(|(&b, _)| b).collect();
    assert_eq!(hyp.len(), ore_site_count * 3);
    for bid in hyp { assert!(h.view().balls.contains_key(&bid)); }
}

async fn setup_abyssal_cruiser(ctx: &ServerContext, acct: &str, char_id: i64, item_id: i64, item_tid: u32, qty: i32) -> (SessionState, SolarSystemId) {
    let jita: SolarSystemId = 30_000_142;
    let sess = provision_space_gm_session(ctx, acct, char_id, jita);
    let ship_id = sess.ship_id.expect("ship_id");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) { ship.type_id = 621; }
        snap.items.insert(item_id, InventoryItemRecord {
            item_id, type_id: item_tid, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::Cargo, quantity: qty, singleton: false, version: 1, ..Default::default()
        });
    });
    let h = ctx.systems.get_or_spawn(jita);
    h.spawn_ball(crate::services::pve_incursion_sites::make_site_npc_ball(ship_id, 621, glam::DVec3::new(500_000_000.0, 0.0, 0.0))).await.expect("ship ball");
    (sess, jita)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p0_01_abyssal_wake_failure_preserves_filament_trace_and_char_system() {
    let abyss_sys = crate::services::abyssal_void_mgr::ABYSSAL_SPACE_SYSTEM_ID;
    let (char_id, fil_id): (i64, i64) = (90_542_095, 99_542_095);
    let ctx = new_ctx(1_776_440_000);
    let (mut sess, jita) = setup_abyssal_cruiser(&ctx, "p0_01_abyss", char_id, fil_id, 56131, 2).await;

    let should_fail = Arc::new(AtomicBool::new(true));
    ctx.systems.register_content_provider(Arc::new(TestSeedFailProvider { target_sys: abyss_sys, should_fail: Arc::clone(&should_fail) })).expect("register fail provider");
    let kw = HashMap::new();
    let fil_err = crate::services::abyssal_void_mgr::handle(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(fil_id)], &kw)
        .await.expect("handled").expect_err("must fail when 32_000_001 wake fails");
    assert_eq!(fil_err.code, "ShipNotInSpace");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&fil_id).map(|i| i.quantity), Some(2));
        assert_eq!(snap.characters[&char_id].solar_system_id, jita);
    });
    assert!(crate::services::pve_special_sites_abyssal::read_abyssal_state(&ctx, |st| st.traces.is_empty()));
    assert!(ctx.systems.get_handle(abyss_sys).is_none());

    let deployed = handle_abyssal_entrance_deployment(&ctx, &mut sess, &[PyRep::Integer(fil_id)]).await.expect("deploy trace");
    let trace_id = deployed.dict_get("traceBallID").and_then(|v| v.as_i64()).expect("traceBallID");
    let tr_err = crate::services::abyssal_void_mgr::handle(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_id)], &kw)
        .await.expect("handled").expect_err("trace activate must fail when 32_000_001 wake fails");
    assert_eq!(tr_err.code, "ShipNotInSpace");
    ctx.db.with_snapshot(|snap| assert_eq!(snap.characters[&char_id].solar_system_id, jita));
    assert!(crate::services::pve_special_sites_abyssal::read_abyssal_state(&ctx, |st| st.traces[&trace_id].entered_char_ids.is_empty()));
    assert!(ctx.systems.get_handle(abyss_sys).is_none());

    should_fail.store(false, Ordering::Release);
    let ok = crate::services::abyssal_void_mgr::handle(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_id)], &kw)
        .await.expect("handled").expect("succeeds after abyssal wake recovers");
    assert!(ok.dict_get("pocketID").and_then(|v| v.as_i64()).is_some_and(|p| p > 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r4_p1_01_rejected_filament_keeps_abyssal_space_cold() {
    let (char_id, bad_item_id): (i64, i64) = (90_542_096, 99_542_096);
    let ctx = new_ctx(1_776_460_000);
    let (mut sess, _) = setup_abyssal_cruiser(&ctx, "r4_p1_01", char_id, bad_item_id, 34, 1).await;
    let kw = HashMap::new();
    for arg in [888_888_888_i64, bad_item_id] {
        let err = crate::services::abyssal_void_mgr::handle(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(arg)], &kw)
            .await.expect("handled").expect_err("invalid filament must fail");
        assert!(matches!(err.code.as_str(), "ItemNotFound" | "InvalidAbyssalFilament"));
        assert!(ctx.systems.get_handle(crate::services::abyssal_void_mgr::ABYSSAL_SPACE_SYSTEM_ID).is_none());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r4_p1_02_homefront_get_instances_then_warp_materializes_outer_entry() {
    let ctx = new_ctx(1_776_470_000);
    let hs_sys = crate::services::pve_homefront_sites::HOMEFRONT_SYSTEMS
        .iter().copied().find(|s| ctx.sde.solar_systems.contains_key(s)).expect("homefront system");
    let mut sess = provision_space_gm_session(&ctx, "r4_p1_02", 90_542_097, hs_sys);
    let res = handle_pve_special_sites(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetHomefrontSiteInstances", &[], &HashMap::new())
        .await.expect("handled").expect("homefront dict");
    let PyRep::Dict(outer) = res else { panic!("expected dict"); };
    let PyRep::List(entries) = &outer[0].1 else { panic!("expected list"); };
    let inst_id = entries[0].dict_get("instanceID").and_then(|v| v.as_i64()).expect("instanceID");
    read_pve_special_reg(&ctx, |reg| assert!(!reg.initialized && reg.homefront_sites.is_empty()));

    crate::services::pve_special_sites::on_warp_to_homefront_site(&ctx, hs_sys, inst_id).await;
    let hf_rt = crate::services::pve_homefront_sites::get_homefront_runtime_record(&ctx, inst_id).expect("homefront runtime");
    let h = ctx.systems.get_handle(hs_sys).expect("live homefront system");
    assert!(h.get_ball_snapshot(hf_rt.outer_gate_ball_id).await.ok().flatten().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r4_p1_03_abyssal_client_is_ready_wakes_cold_pocket_and_spawns_room_balls() {
    let abyss_sys = crate::services::abyssal_void_mgr::ABYSSAL_SPACE_SYSTEM_ID;
    let (char_id, fil_id): (i64, i64) = (90_542_098, 99_542_098);
    let ctx = new_ctx(1_776_480_000);
    let (mut sess, _) = setup_abyssal_cruiser(&ctx, "r4_p1_03", char_id, fil_id, 56131, 1).await;
    let kw = HashMap::new();
    let entered = crate::services::abyssal_void_mgr::handle(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(fil_id)], &kw)
        .await.expect("handled").expect("entered abyss");
    let pocket_id = entered.dict_get("pocketID").and_then(|v| v.as_i64()).expect("pocketID");
    assert!(ctx.systems.stop_system(abyss_sys).await);
    assert!(ctx.systems.get_handle(abyss_sys).is_none());

    let should_fail = Arc::new(AtomicBool::new(true));
    ctx.systems.register_content_provider(Arc::new(TestSeedFailProvider { target_sys: abyss_sys, should_fail: Arc::clone(&should_fail) })).expect("register fail provider");
    let wake_err = crate::services::abyssal_void_mgr::handle(&ctx, &mut sess, "abyssalMgr", "ClientIsReady", &[], &kw)
        .await.expect("handled").expect_err("must fail closed if wake fails");
    assert_eq!(wake_err.code, "ShipNotInSpace");

    should_fail.store(false, Ordering::Release);
    let ready = crate::services::abyssal_void_mgr::handle(&ctx, &mut sess, "abyssalMgr", "ClientIsReady", &[], &kw)
        .await.expect("handled").expect("ready after wake");
    assert_eq!(ready.dict_get("ready").and_then(|v| v.as_bool()), Some(true));
    let h = ctx.systems.get_handle(abyss_sys).expect("woken abyssal handle");
    let marker_id = 1_895_000_000 + pocket_id * 10 + 1;
    assert!(h.get_ball_snapshot(marker_id).await.ok().flatten().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p1_02_seasonal_maintenance_clock_override_respects_season_boundary() {
    let jita: SolarSystemId = 30_000_142;
    let t0: u64 = 1_776_450_000;
    let ctx = new_ctx(t0);
    let t0_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        let mut cfg = crate::services::season_fsd::load_season_config(snap);
        cfg.start_time_ft = t0_ft - 3_600 * 10_000_000;
        cfg.end_time_ft = t0_ft + 600 * 10_000_000;
        snap.seasons_and_campaigns.season_configuration = Some(cfg);
    });
    let sites = crate::services::pve_special_sites::ensure_seasonal_event_sites_for_system(&ctx, jita).await;
    let (boss_bid, escort_bids) = (sites[0].boss_npc_ball_id, sites[0].escort_ball_ids.clone());
    let handle = ctx.systems.get_handle(jita).expect("live jita");
    handle.try_remove_ball(boss_bid);
    for &ebid in &escort_bids { handle.try_remove_ball(ebid); }
    {
        let mut rt = ctx.combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
        rt.npc_entities.remove(&boss_bid);
        for &ebid in &escort_bids { rt.npc_entities.remove(&ebid); }
    }
    // Advance db.clock() past season end_time_ft, while passing active maintenance timestamp (t0 + 60).
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(t0 + 1_200)));
    assert_eq!(ctx.systems.run_content_maintenance_for_active_systems(t0 + 60), Ok(1));
    assert!(handle.view().balls.contains_key(&boss_bid), "P1-02: post-spawn reconcile must use maintenance timestamp");
    assert!(ctx.combat_ephemeral.read().unwrap_or_else(|e| e.into_inner()).npc_entities.contains_key(&boss_bid));
}
