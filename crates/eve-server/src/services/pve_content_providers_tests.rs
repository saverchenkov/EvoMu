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
    let sde = shared_sde();
    let jita: SolarSystemId = 30_000_142;
    let trig_sys: SolarSystemId = 30_000_012;
    let drifter_sys: SolarSystemId = 31_000_001;
    let char_id: i64 = 90_542_001;

    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_000_000)));
    let mut sess = provision_space_gm_session(&ctx, "t2a_cold_probe", char_id, jita);
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
        assert_eq!(
            ctx.systems.system_lifecycle_state(sys),
            SystemLifecycleState::Cold,
            "cold read must not wake {sys}"
        );
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
    let sde = shared_sde();
    let jita: SolarSystemId = 30_000_142;
    let trig_sys: SolarSystemId = 30_000_012;
    let drifter_sys: SolarSystemId = 31_000_001;
    let char_id: i64 = 90_542_002;

    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    let t0: u64 = 1_776_000_000;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(t0)));
    let mut sess = provision_space_gm_session(&ctx, "t2a_seed_verify", char_id, jita);
    let kw = HashMap::new();

    // Activating a plain high-sec system (Jita) must stay read-pure (0 DB commits, no unsolicited
    // mining/seasonal creation) while persisted non-belt asteroids still rehydrate on reactivation.
    let epoch_pre_jita = ctx.db.state_epoch();
    let commits_pre_jita = ctx.db.tx_commit_count();
    let jita_handle = ctx.systems.get_or_spawn(jita);
    assert_eq!(ctx.db.state_epoch(), epoch_pre_jita);
    assert_eq!(ctx.db.tx_commit_count(), commits_pre_jita);
    assert_eq!(
        jita_handle
            .dynamic_ball_count
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    assert_eq!(
        ctx.systems.system_lifecycle_state(jita),
        SystemLifecycleState::Hibernating
    );

    // Explicit mining seeding creates Jita ore/gas anomalies & signatures; stop -> reactivate
    // rehydrates persisted non-belt asteroids into the fresh Ballpark without extra DB writes.
    crate::services::mining_sites::seed_system_mining_sites_from_sde(&ctx, jita).await;
    let persisted_ast_id: i64 = ctx.db.with_snapshot(|snap| {
        let has_mining = snap
            .cosmic_anomalies
            .get(&jita)
            .is_some_and(|v| v.iter().any(|a| a.anomaly_type == "OreAnomaly"))
            || snap
                .cosmic_signatures
                .get(&jita)
                .is_some_and(|v| !v.is_empty());
        assert!(
            has_mining,
            "explicit mining seeding must populate Jita ore anomaly/signature"
        );
        snap.asteroids
            .values()
            .find(|a| a.solar_system_id == jita && a.belt_id == 0 && a.quantity > 0)
            .map(|a| a.asteroid_id)
            .expect("persisted non-belt asteroid in Jita")
    });
    assert!(ctx.systems.stop_system(jita).await);
    let epoch_rehydrate = ctx.db.state_epoch();
    let commits_rehydrate = ctx.db.tx_commit_count();
    let jita_rehydrated = ctx.systems.get_or_spawn(jita);
    assert_eq!(ctx.db.state_epoch(), epoch_rehydrate);
    assert_eq!(ctx.db.tx_commit_count(), commits_rehydrate);
    assert!(
        jita_rehydrated
            .view()
            .balls
            .contains_key(&persisted_ast_id),
        "re-activation must rehydrate persisted non-belt asteroid into Ballpark"
    );

    for sys_id in [trig_sys, drifter_sys] {
        let handle = ctx.systems.get_or_spawn(sys_id);
        assert!(ctx.systems.get_handle(sys_id).is_some());
        let state = ctx.systems.system_lifecycle_state(sys_id);
        if handle.dynamic_ball_count.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            assert_eq!(state, SystemLifecycleState::Hibernating);
        } else {
            assert_eq!(state, SystemLifecycleState::Live);
        }
        let transitions = ctx.systems.recent_lifecycle_transitions();
        let cold_to_loading = transitions.iter().position(|r| {
            r.solar_system_id == sys_id
                && r.from == SystemLifecycleState::Cold
                && r.to == SystemLifecycleState::Loading
        });
        let loading_to_live = transitions.iter().position(|r| {
            r.solar_system_id == sys_id
                && r.from == SystemLifecycleState::Loading
                && r.to == SystemLifecycleState::Live
        });
        assert!(
            matches!((cold_to_loading, loading_to_live), (Some(a), Some(b)) if a < b),
            "system {sys_id} must transition Cold -> Loading -> Live"
        );
    }

    // F-03: activating 31_000_001 seeds ONLY 31_000_001 and leaves sibling Drifter Hives cold.
    assert!(read_pve_special_reg(&ctx, |reg| {
        !reg.triglavian_minor_victory_systems.is_empty()
            && reg.triglavian_sites.get(&trig_sys).is_some_and(|v| !v.is_empty())
            && reg.drifter_hives.contains_key(&drifter_sys)
            && !(31_000_002..=31_000_005).any(|sid| reg.drifter_hives.contains_key(&sid))
    }));
    for sibling in 31_000_002..=31_000_005 {
        assert_eq!(
            ctx.systems.system_lifecycle_state(sibling),
            SystemLifecycleState::Cold
        );
    }

    // Querying seeded special sites reads persisted state with zero DB writes.
    let epoch_seeded = ctx.db.state_epoch();
    let commits_seeded = ctx.db.tx_commit_count();
    let trig_rep = handle_pve_special_sites(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[PyRep::Integer(i64::from(trig_sys))],
        &kw,
    )
    .await
    .expect("handled")
    .expect("seeded triglavian dict");
    let PyRep::Dict(trig_outer) = trig_rep else {
        panic!("expected triglavian outer dict, got {trig_rep:?}");
    };
    let (_, PyRep::List(trig_entries)) = &trig_outer[0] else {
        panic!("expected triglavian site list, got {trig_outer:?}");
    };
    assert_eq!(trig_entries.len(), 2, "activated Triglavian MV system must serve 2 seeded sites");
    assert_eq!(ctx.db.state_epoch(), epoch_seeded);
    assert_eq!(ctx.db.tx_commit_count(), commits_seeded);

    // Idle maintenance ticks with nothing expiring or respawning must perform zero DB writes.
    let ticked = ctx
        .systems
        .run_content_maintenance_for_active_systems(t0 + 60)
        .expect("idle tick 1");
    assert_eq!(ticked, 3);
    let ticked2 = ctx
        .systems
        .run_content_maintenance_for_active_systems(t0 + 120)
        .expect("idle tick 2");
    assert_eq!(ticked2, 3);
    assert_eq!(ctx.db.state_epoch(), epoch_seeded);
    assert_eq!(ctx.db.tx_commit_count(), commits_seeded);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn maintenance_expire_and_stop_reactivation_preserve_pruned_state() {
    let sde = shared_sde();
    let trig_sys: SolarSystemId = 30_000_012;
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    let t0: u64 = 1_776_100_000;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(t0)));

    let _h1 = ctx.systems.get_or_spawn(trig_sys);

    // Insert an expired seasonal event site record whose window_end_ft is in the past.
    let expired_inst_id: i64 = 99_542_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.pve_special_sites
            .seasonal_event_sites
            .entry(trig_sys)
            .or_default()
            .push(SeasonalEventSiteRecord {
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

    // Advancing clock invokes SpecialSitesContentProvider::expire and prunes the stale record.
    ctx.advance_secs(30);

    ctx.db.with_snapshot(|snap| {
        let still_present = snap
            .pve_special_sites
            .seasonal_event_sites
            .get(&trig_sys)
            .is_some_and(|list| list.iter().any(|s| s.instance_id == expired_inst_id));
        assert!(
            !still_present,
            "expired seasonal event site must be pruned by SpecialSitesContentProvider::expire"
        );
    });

    // Stopping and re-activating returns Cold -> Live cleanly without resurrecting pruned records.
    assert!(ctx.systems.stop_system(trig_sys).await);
    assert_eq!(ctx.systems.system_lifecycle_state(trig_sys), SystemLifecycleState::Cold);

    let _h2 = ctx.systems.get_or_spawn(trig_sys);
    assert_eq!(ctx.systems.system_lifecycle_state(trig_sys), SystemLifecycleState::Live);
    ctx.db.with_snapshot(|snap| {
        assert!(snap
            .pve_special_sites
            .seasonal_event_sites
            .get(&trig_sys)
            .is_none_or(|list| !list.iter().any(|s| s.instance_id == expired_inst_id)));
        assert!(snap
            .pve_special_sites
            .triglavian_sites
            .get(&trig_sys)
            .is_some_and(|list| !list.is_empty()));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_tick_seasonal_rehydration_publishes_view_and_registers_ai_f01() {
    let sde = shared_sde();
    let jita: SolarSystemId = 30_000_142;
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    let t0: u64 = 1_776_200_000;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(t0)));

    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        let mut cfg = crate::services::season_fsd::load_season_config(snap);
        cfg.start_time_ft = now_ft - 3_600 * 10_000_000;
        cfg.end_time_ft = now_ft + 3_600 * 10_000_000;
        snap.seasons_and_campaigns.season_configuration = Some(cfg);
    });

    let sites = crate::services::pve_special_sites::ensure_seasonal_event_sites_for_system(&ctx, jita).await;
    assert_eq!(sites.len(), 1);
    let boss_bid = sites[0].boss_npc_ball_id;
    let escort_bids = sites[0].escort_ball_ids.clone();

    let handle = ctx.systems.get_handle(jita).expect("live jita handle");
    handle.try_remove_ball(boss_bid);
    for &ebid in &escort_bids {
        handle.try_remove_ball(ebid);
    }
    {
        let mut rt = ctx.combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
        rt.npc_entities.remove(&boss_bid);
        for &ebid in &escort_bids {
            rt.npc_entities.remove(&ebid);
        }
    }
    assert!(!handle.view().balls.contains_key(&boss_bid));

    let ticked = ctx
        .systems
        .run_content_maintenance_for_active_systems(t0 + 60)
        .expect("maintenance tick");
    assert_eq!(ticked, 1);

    let view = handle.view();
    assert!(view.balls.contains_key(&boss_bid), "F-01: SystemView must reflect spawned seasonal boss");
    for &ebid in &escort_bids {
        assert!(view.balls.contains_key(&ebid), "F-01: SystemView must reflect spawned seasonal escort");
    }
    let rt = ctx.combat_ephemeral.read().unwrap_or_else(|e| e.into_inner());
    assert!(rt.npc_entities.contains_key(&boss_bid), "F-01: live tick must re-register seasonal boss AI");
    for &ebid in &escort_bids {
        assert!(rt.npc_entities.contains_key(&ebid), "F-01: live tick must re-register seasonal escort AI");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abyssal_cold_fail_closed_f04_and_seasonal_cold_fail_closed_f05() {
    let sde = shared_sde();
    let jita: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_542_010;
    let filament_id: i64 = 99_542_010;
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_300_000)));
    let mut sess = provision_space_gm_session(&ctx, "t2a_f04_f05", char_id, jita);
    let ship_id = sess.ship_id.expect("active ship");

    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            filament_id,
            InventoryItemRecord {
                item_id: filament_id,
                type_id: 56131,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        let mut cfg = crate::services::season_fsd::load_season_config(snap);
        cfg.start_time_ft = now_ft - 3_600 * 10_000_000;
        cfg.end_time_ft = now_ft + 3_600 * 10_000_000;
        snap.seasons_and_campaigns.season_configuration = Some(cfg);
    });
    assert!(ctx.systems.get_handle(jita).is_none());

    // F-04: Abyssal deployment on cold system must fail closed without consuming filament or trace.
    let err = handle_abyssal_entrance_deployment(&ctx, &mut sess, &[PyRep::Integer(filament_id)])
        .await
        .expect_err("cold abyssal filament deployment must fail");
    assert_eq!(err.code, "ShipNotInSpace");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&filament_id).map(|i| i.quantity), Some(1));
    });
    assert!(crate::services::pve_special_sites_abyssal::read_abyssal_state(
        &ctx,
        |st| st.traces.is_empty(),
    ));

    // F-05: live-only helpers & nonexistent wake target must fail closed with 0 orphan DB commits.
    let ep0 = ctx.db.state_epoch();
    let cm0 = ctx.db.tx_commit_count();
    let cold_sites =
        crate::services::pve_special_sites::ensure_seasonal_event_sites_if_live_for_system(
            &ctx, jita,
        )
        .await;
    assert!(cold_sites.is_empty());
    crate::services::pve_special_sites::ensure_special_sites_spawned_if_live_for_system(&ctx, jita)
        .await;
    assert_eq!(ctx.db.state_epoch(), ep0);
    assert_eq!(ctx.db.tx_commit_count(), cm0);
    assert!(ctx.systems.get_handle(jita).is_none());

    struct FailWakeForSystemProvider {
        target_sys: SolarSystemId,
    }
    impl SystemContentProvider for FailWakeForSystemProvider {
        fn name(&self) -> &'static str {
            "test_fail_wake_for_system"
        }

        fn seed(&self, seed_ctx: &mut SystemContentSeedContext<'_>) -> Result<(), String> {
            if seed_ctx.solar_system_id == self.target_sys {
                return Err(format!("injected wake failure for {}", self.target_sys));
            }
            Ok(())
        }
    }

    let missing_sys: SolarSystemId = 99_999_999;
    ctx.systems
        .register_content_provider(Arc::new(FailWakeForSystemProvider {
            target_sys: missing_sys,
        }))
        .expect("register injected wake failure provider");
    let failed_sites =
        crate::services::pve_special_sites::ensure_seasonal_event_sites_for_system(
            &ctx,
            missing_sys,
        )
        .await;
    assert!(failed_sites.is_empty());
    assert_eq!(ctx.db.tx_commit_count(), cm0);
    assert!(ctx.systems.get_handle(missing_sys).is_none());
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.pve_special_sites.seasonal_event_sites.contains_key(&missing_sys));
        assert!(!snap.cosmic_anomalies.contains_key(&missing_sys));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pochven_reactivation_prunes_stale_hypnosian_ai_f06_and_dead_live_stop_clears_markers_f02() {
    let sde = shared_sde();
    let pochven_sys = first_pochven_system(&sde);
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    let t0: u64 = 1_776_400_000;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(t0)));

    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies
            .entry(pochven_sys)
            .or_default()
            .push(CosmicAnomalyRecord {
                anomaly_id: "DNG-12345-900001".into(),
                solar_system_id: pochven_sys,
                name: "Pochven Test Ore Site".into(),
                anomaly_type: "OreAnomaly".into(),
                ore_types: Vec::new(),
                signal_strength: 1.0,
                position: [15_000.0, 0.0, 0.0],
                cleared: false,
                remaining_volume: 1_000.0,
                created_unix: t0,
                lifetime_seconds: 86_400,
            });
        snap.asteroids.insert(
            99_542_100,
            AsteroidRecord {
                asteroid_id: 99_542_100,
                solar_system_id: pochven_sys,
                belt_id: 0,
                type_id: 1228,
                quantity: 100,
                initial_quantity: 100,
                position: (15_000.0, 0.0, 0.0),
                radius: 200.0,
            },
        );
    });

    let _h1 = ctx.systems.get_or_spawn(pochven_sys);
    let first_ai_ids: Vec<i64> = {
        let rt = ctx.combat_ephemeral.read().unwrap_or_else(|e| e.into_inner());
        rt.npc_entities
            .iter()
            .filter(|(_, n)| {
                n.solar_system_id == pochven_sys && n.archetype == "PochvenHypnosianSleeper"
            })
            .map(|(&bid, _)| bid)
            .collect()
    };
    assert!(!first_ai_ids.is_empty(), "Pochven OreAnomaly must spawn Hypnosian sleepers");

    // Stop system and simulate a stale Hypnosian AI entry lingering across eviction.
    assert!(ctx.systems.stop_system(pochven_sys).await);
    let stale_bid: i64 = 1_899_542_999;
    {
        let mut rt = ctx.combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
        if let Some(sample) = rt.npc_entities.values().next().cloned() {
            let mut stale = sample;
            stale.ball_id = stale_bid;
            stale.solar_system_id = pochven_sys;
            stale.archetype = "PochvenHypnosianSleeper".to_string();
            rt.npc_entities.insert(stale_bid, stale);
        }
    }

    let h2 = ctx.systems.get_or_spawn(pochven_sys);
    let rt = ctx.combat_ephemeral.read().unwrap_or_else(|e| e.into_inner());
    assert!(
        !rt.npc_entities.contains_key(&stale_bid),
        "F-06: reactivation must prune stale Hypnosian AI entries"
    );
    let live_ids: Vec<i64> = rt
        .npc_entities
        .iter()
        .filter(|(_, n)| {
            n.solar_system_id == pochven_sys && n.archetype == "PochvenHypnosianSleeper"
        })
        .map(|(&bid, _)| bid)
        .collect();
    assert!(!live_ids.is_empty());
    for bid in live_ids {
        assert!(h2.view().balls.contains_key(&bid));
    }
    drop(rt);

    // F-02: dead-Live handle stop clears seed markers and re-seeds Triglavian balls on reactivation.
    let any_trig_bid = read_pve_special_reg(&ctx, |reg| {
        reg.triglavian_sites
            .get(&pochven_sys)
            .and_then(|v| v.first())
            .map(|s| s.boss_npc_ball_id)
    })
    .expect("triglavian site boss ball");
    assert!(h2.view().balls.contains_key(&any_trig_bid));
    let (tx, rx) = tokio::sync::oneshot::channel();
    h2.cmd_tx
        .send(crate::system_actor::SystemCommand::ShutdownActor { reply: tx })
        .await
        .expect("send shutdown directly to live actor");
    let _ = rx.await;
    h2.cmd_tx.closed().await;
    assert!(h2.cmd_tx.is_closed(), "actor task must exit to become dead-Live");
    assert!(!ctx.systems.stop_system(pochven_sys).await);
    assert!(!ctx.systems.is_special_site_ball_spawned(pochven_sys, any_trig_bid));
    let h3 = ctx.systems.get_or_spawn(pochven_sys);
    assert!(
        h3.view().balls.contains_key(&any_trig_bid),
        "F-02: clearing seed markers on dead-Live stop must re-seed Triglavian ball"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf01_failed_late_seed_does_not_leak_markers_and_subsequent_wake_spawns() {
    let sde = shared_sde();
    let pochven_sys = first_pochven_system(&sde);
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    let t0: u64 = 1_776_410_000;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(t0)));
    let wh_anom_id = "WH-BF01-ORE-1".to_string();
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies
            .entry(pochven_sys)
            .or_default()
            .push(CosmicAnomalyRecord {
                anomaly_id: wh_anom_id.clone(),
                solar_system_id: pochven_sys,
                name: WORMHOLE_ORE_SITES[0].dungeon_name.into(),
                anomaly_type: "OreAnomaly".into(),
                ore_types: Vec::new(),
                signal_strength: 1.0,
                position: [25_000.0, 0.0, 0.0],
                cleared: false,
                remaining_volume: 500.0,
                created_unix: t0,
                lifetime_seconds: 86_400,
            });
    });

    struct LateSeedFailProvider {
        target_sys: SolarSystemId,
        should_fail: Arc<AtomicBool>,
    }
    impl SystemContentProvider for LateSeedFailProvider {
        fn name(&self) -> &'static str {
            "test_bf01_late_seed_fail"
        }
        fn seed(&self, seed_ctx: &mut SystemContentSeedContext<'_>) -> Result<(), String> {
            if seed_ctx.solar_system_id == self.target_sys && self.should_fail.load(Ordering::Acquire) {
                return Err("injected late provider seed failure".into());
            }
            Ok(())
        }
    }

    let should_fail = Arc::new(AtomicBool::new(true));
    ctx.systems
        .register_content_provider(Arc::new(LateSeedFailProvider {
            target_sys: pochven_sys,
            should_fail: Arc::clone(&should_fail),
        }))
        .expect("register late seed failure provider");

    let err = match ctx
        .systems
        .wake_for_session_arrival_async(pochven_sys, SessionArrivalWakeReason::Jump)
        .await
    {
        Ok(_) => panic!("late provider error must abort activation"),
        Err(e) => e,
    };
    assert!(matches!(
        err,
        SystemActivationError::ContentProviderFailed { ref provider, .. }
            if provider == "test_bf01_late_seed_fail"
    ));
    assert!(ctx.systems.get_handle(pochven_sys).is_none());
    let trig_bid = read_pve_special_reg(&ctx, |reg| {
        reg.triglavian_sites
            .get(&pochven_sys)
            .and_then(|v| v.first())
            .map(|s| s.boss_npc_ball_id)
    })
    .expect("triglavian site persisted");
    assert!(!ctx.systems.is_special_site_ball_spawned(pochven_sys, trig_bid));
    assert!(!ctx.systems.is_wh_ore_anomaly_hydrated(pochven_sys, &wh_anom_id));

    should_fail.store(false, Ordering::Release);
    let h = ctx.systems.get_or_spawn(pochven_sys);
    assert!(h.view().balls.contains_key(&trig_bid));
    assert!(ctx.systems.is_special_site_ball_spawned(pochven_sys, trig_bid));
    assert!(ctx.systems.is_wh_ore_anomaly_hydrated(pochven_sys, &wh_anom_id));
    assert!(h
        .view()
        .balls
        .values()
        .any(|b| WORMHOLE_GUARD_TYPE_IDS.contains(&b.type_id)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf02_partial_triglavian_escort_kill_survives_stop_reactivation_and_completes() {
    let sde = shared_sde();
    let pochven_sys = first_pochven_system(&sde);
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_420_000)));
    let mut sess = provision_space_gm_session(&ctx, "bf02_pilot", 90_542_091, pochven_sys);
    let ship_id = sess.ship_id.expect("ship_id");
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(ship_id, [10_000.0, 0.0, 0.0]);
    });

    let h1 = ctx.systems.get_or_spawn(pochven_sys);
    let trig = read_pve_special_reg(&ctx, |reg| {
        reg.triglavian_sites
            .get(&pochven_sys)
            .and_then(|v| v.first())
            .cloned()
    })
    .expect("triglavian site");
    assert!(!trig.escort_ball_ids.is_empty());
    let killed_escort = trig.escort_ball_ids[0];
    let _ = h1.remove_ball(killed_escort).await;
    let _ = crate::services::pve_special_sites_combat::on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        pochven_sys,
        killed_escort,
        Some(90_542_091),
    )
    .await;
    assert!(ctx
        .db
        .with_snapshot(load_triglavian_destroyed_escorts)
        .contains(&killed_escort));

    assert!(ctx.systems.stop_system(pochven_sys).await);
    let h2 = ctx.systems.get_or_spawn(pochven_sys);
    assert!(!h2.view().balls.contains_key(&killed_escort));
    assert!(h2.view().balls.contains_key(&trig.boss_npc_ball_id));

    let _ = h2.remove_ball(trig.boss_npc_ball_id).await;
    for &ebid in &trig.escort_ball_ids[1..] {
        let _ = h2.remove_ball(ebid).await;
    }
    let res = complete_special_dungeon_instance(&ctx, &mut sess, trig.instance_id, 1)
        .await
        .expect("completion after stop/reactivate must succeed");
    assert_eq!(res.dict_get("completed").and_then(|v| v.as_bool()), Some(true));
    ctx.db.with_snapshot(|snap| {
        let destroyed = load_triglavian_destroyed_escorts(snap);
        for ebid in &trig.escort_ball_ids {
            assert!(!destroyed.contains(ebid), "escort {ebid} must be pruned");
        }
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf03_cold_pochven_mining_seed_stays_cold_and_spawns_single_hypnosian_wave() {
    let sde = shared_sde();
    let pochven_sys = first_pochven_system(&sde);
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_430_000)));

    assert_eq!(ctx.systems.system_lifecycle_state(pochven_sys), SystemLifecycleState::Cold);
    crate::services::mining_sites::seed_system_mining_sites_from_sde(&ctx, pochven_sys).await;
    assert!(ctx.systems.get_handle(pochven_sys).is_none());
    assert_eq!(ctx.systems.system_lifecycle_state(pochven_sys), SystemLifecycleState::Cold);

    let ore_site_count = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies
            .get(&pochven_sys)
            .map(|list| list.iter().filter(|a| a.anomaly_type == "OreAnomaly").count())
            .unwrap_or(0)
    });
    assert!(ore_site_count > 0);

    let h = ctx.systems.get_or_spawn(pochven_sys);
    let rt = ctx.combat_ephemeral.read().unwrap_or_else(|e| e.into_inner());
    let ai_count = rt
        .npc_entities
        .values()
        .filter(|n| n.solar_system_id == pochven_sys && n.archetype == "PochvenHypnosianSleeper")
        .count();
    assert_eq!(ai_count, ore_site_count * 3);
    for (bid, n) in &rt.npc_entities {
        if n.solar_system_id == pochven_sys && n.archetype == "PochvenHypnosianSleeper" {
            assert!(h.view().balls.contains_key(bid));
        }
    }
}
