use super::*;
use crate::router::ServerContext;
use crate::services::pve_special_sites::{handle_pve_special_sites, read_pve_special_reg};
use crate::system_actor::{SystemContentProviderMetadata, SystemLifecycleState, SystemRegistry};
use eve_db::clock::VirtualServerClock;
use eve_db::{SeasonalEventSiteRecord, WorldRepository};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
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

    for sys_id in [jita, trig_sys, drifter_sys] {
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

    assert!(read_pve_special_reg(&ctx, |reg| {
        !reg.triglavian_minor_victory_systems.is_empty()
            && reg.triglavian_sites.get(&trig_sys).is_some_and(|v| !v.is_empty())
            && reg.drifter_hives.contains_key(&drifter_sys)
    }));
    ctx.db.with_snapshot(|snap| {
        let has_mining = snap
            .cosmic_anomalies
            .get(&jita)
            .is_some_and(|v| v.iter().any(|a| a.anomaly_type == "OreAnomaly"))
            || snap
                .cosmic_signatures
                .get(&jita)
                .is_some_and(|v| !v.is_empty());
        assert!(has_mining, "ResourceSitesContentProvider must seed Jita ore anomaly/signature");
    });

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
