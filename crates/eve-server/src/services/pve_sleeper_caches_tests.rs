//! Unit tests for `pve_sleeper_caches`.

use super::*;
use crate::router::ServerContext;
use crate::services::pve_exploration::{with_pve_exploration_reg, PveSiteInstance};
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn sleeper_cache_tier_and_passage_validation() {
    assert_eq!(sleeper_cache_tier_for_dungeon(5604), Some(SleeperCacheTier::Limited));
    assert_eq!(sleeper_cache_tier_for_dungeon(5609), Some(SleeperCacheTier::Standard));
    assert_eq!(sleeper_cache_tier_for_dungeon(5632), Some(SleeperCacheTier::Superior));
    assert_eq!(sleeper_cache_tier_for_dungeon(9999), None);

    let err_rift = validate_sleeper_cache_passage(SleeperCacheTier::Limited, false, 25, 1_000_000.0)
        .expect_err("unstable rift blocks passage");
    assert_eq!(err_rift.code, "RiftPassageUnstable");

    validate_sleeper_cache_passage(SleeperCacheTier::Limited, true, 25, 1_500_000.0)
        .expect("frigate passes limited rift");
    assert_eq!(
        validate_sleeper_cache_passage(SleeperCacheTier::Limited, true, 26, 4_000_000.0)
            .expect_err("cruiser rejected in limited")
            .code,
        "ShipSizeRestricted"
    );
    assert_eq!(
        validate_sleeper_cache_passage(SleeperCacheTier::Limited, true, 25, 6_000_000.0)
            .expect_err("overweight hull rejected")
            .code,
        "ShipSizeRestricted"
    );

    validate_sleeper_cache_passage(SleeperCacheTier::Standard, true, 26, 12_000_000.0)
        .expect("cruiser passes standard rift");
    assert_eq!(
        validate_sleeper_cache_passage(SleeperCacheTier::Standard, true, 27, 15_000_000.0)
            .expect_err("battleship rejected in standard")
            .code,
        "ShipSizeRestricted"
    );
    validate_sleeper_cache_passage(SleeperCacheTier::Superior, true, 27, 100_000_000.0)
        .expect("battleship passes superior rift");
    assert_eq!(
        validate_sleeper_cache_passage(SleeperCacheTier::Superior, true, 547, 110_000_000.0)
            .expect_err("carrier rejected in superior")
            .code,
        "ShipSizeRestricted"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sleeper_hazard_damage_and_ghost_site_countdown() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_040_001;
    let ship_id: i64 = 1_040_000_001;
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_300_000)));

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1_200_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 1_000.0,
                max_shield_hp: 1_000.0,
                armor_hp: 1_000.0,
                max_armor_hp: 1_000.0,
                structure_hp: 1_000.0,
                max_structure_hp: 1_000.0,
                ..CombatState::default()
            },
        )
        .await;

    let mut session = SessionState::new(401);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);

    let mut safe_site = PveSiteInstance {
        sleeper_gas_cloud_active: false,
        sleeper_sentry_disabled: true,
        ..Default::default()
    };
    apply_sleeper_hazard_damage_if_active(&ctx, &session, sys_id, &safe_site).await;

    session.ship_id = Some(ship_id);
    safe_site.sleeper_gas_cloud_active = true;
    safe_site.sleeper_sentry_disabled = false;
    apply_sleeper_hazard_damage_if_active(&ctx, &session, sys_id, &safe_site).await;

    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(
            sys_id,
            vec![PveSiteInstance {
                site_id: "GHOST-1".into(),
                solar_system_id: sys_id,
                site_category: "Ghost".into(),
                ghost_detonate_after_sec: 120,
                ..Default::default()
            }],
        );
        assert!(!is_ghost_site_timer_expired(reg, sys_id, "GHOST-1"));
        assert!(activate_ghost_site_timer(reg, sys_id, "GHOST-1", 90));
        assert!(!activate_ghost_site_timer(reg, sys_id, "GHOST-1", 30));
        assert!(!is_ghost_site_timer_expired(reg, sys_id, "GHOST-1"));
        reg.sim_clock_sec = 1_775_300_100;
        assert!(is_ghost_site_timer_expired(reg, sys_id, "GHOST-1"));
    });
}

#[test]
fn sleeper_cache_all_dungeon_tiers_and_exact_mass_group_boundaries() {
    for id in [5604, 3546, 2355] {
        assert_eq!(sleeper_cache_tier_for_dungeon(id), Some(SleeperCacheTier::Limited));
    }
    for id in [5609, 3547, 2356] {
        assert_eq!(sleeper_cache_tier_for_dungeon(id), Some(SleeperCacheTier::Standard));
    }
    for id in [5632, 3548, 2357] {
        assert_eq!(sleeper_cache_tier_for_dungeon(id), Some(SleeperCacheTier::Superior));
    }
    for id in [0, 5603, 5605, 3545, 2358, u32::MAX] {
        assert_eq!(sleeper_cache_tier_for_dungeon(id), None);
    }

    let (lim_mass, lim_label, lim_groups) = sleeper_cache_ship_restrictions(SleeperCacheTier::Limited);
    assert_eq!(lim_mass, 5_000_000.0);
    assert!(lim_label.contains("Limited Sleeper Cache"));
    assert_eq!(lim_groups, &[25, 420, 830, 831, 893, 1283, 1534]);
    for &grp in lim_groups {
        validate_sleeper_cache_passage(SleeperCacheTier::Limited, true, grp, 5_000_000.0)
            .expect("limited allowed group at exact 5,000,000 kg");
    }
    let over_lim = validate_sleeper_cache_passage(SleeperCacheTier::Limited, true, 25, 5_000_000.1)
        .expect_err("over 5M kg rejected");
    assert_eq!(over_lim.code, "ShipSizeRestricted");
    assert!(over_lim.message.contains("group 25"));
    assert!(over_lim.message.contains("Limited Sleeper Cache"));

    let (std_mass, std_label, std_groups) = sleeper_cache_ship_restrictions(SleeperCacheTier::Standard);
    assert_eq!(std_mass, 20_000_000.0);
    assert!(std_label.contains("Standard Sleeper Cache"));
    assert!(std_groups.contains(&963) && std_groups.contains(&419));
    for &grp in std_groups {
        validate_sleeper_cache_passage(SleeperCacheTier::Standard, true, grp, 20_000_000.0)
            .expect("standard allowed group at exact 20,000,000 kg");
    }
    assert_eq!(
        validate_sleeper_cache_passage(SleeperCacheTier::Standard, true, 963, 20_000_000.1)
            .expect_err("over 20M kg rejected")
            .code,
        "ShipSizeRestricted"
    );
    assert_eq!(
        validate_sleeper_cache_passage(SleeperCacheTier::Standard, false, 26, 10_000_000.0)
            .expect_err("unstable rift rejects standard cruiser")
            .code,
        "RiftPassageUnstable"
    );

    let (sup_mass, sup_label, sup_groups) = sleeper_cache_ship_restrictions(SleeperCacheTier::Superior);
    assert_eq!(sup_mass, 120_000_000.0);
    assert!(sup_label.contains("Superior Sleeper Cache"));
    for &grp in sup_groups {
        validate_sleeper_cache_passage(SleeperCacheTier::Superior, true, grp, 120_000_000.0)
            .expect("superior allowed group at exact 120,000,000 kg");
    }
    assert_eq!(
        validate_sleeper_cache_passage(SleeperCacheTier::Superior, true, 27, 120_000_000.1)
            .expect_err("over 120M kg rejected")
            .code,
        "ShipSizeRestricted"
    );
    assert_eq!(
        validate_sleeper_cache_passage(SleeperCacheTier::Superior, true, 485, 100_000_000.0)
            .expect_err("dreadnought rejected in superior")
            .code,
        "ShipSizeRestricted"
    );
    assert_eq!(
        validate_sleeper_cache_passage(SleeperCacheTier::Superior, false, 27, 90_000_000.0)
            .expect_err("unstable rift rejects superior battleship")
            .code,
        "RiftPassageUnstable"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sleeper_hazard_damage_differentiates_thermal_gas_vs_kinetic_sentry_and_no_ship() {
    let sys_id: SolarSystemId = 30_000_142;
    let ship_id: i64 = 1_040_000_101;
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1_200_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 1_000.0,
                max_shield_hp: 1_000.0,
                armor_hp: 1_000.0,
                max_armor_hp: 1_000.0,
                structure_hp: 1_000.0,
                max_structure_hp: 1_000.0,
                // Asymmetric thermal (0.5) vs kinetic (0.25) proves gas applies thermal and sentry applies kinetic.
                shield_resonance: [1.0, 0.5, 0.25, 1.0],
                ..CombatState::default()
            },
        )
        .await;

    let mut session = SessionState::new(402);
    session.character_id = Some(90_040_002);
    session.solar_system_id = Some(sys_id);

    // No ship bound -> safe and hazardous sites both leave ship untouched.
    let active_both = PveSiteInstance {
        sleeper_gas_cloud_active: true,
        sleeper_sentry_disabled: false,
        ..Default::default()
    };
    apply_sleeper_hazard_damage_if_active(&ctx, &session, sys_id, &active_both).await;
    assert_eq!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp, 1_000.0);

    session.ship_id = Some(ship_id);

    // Safe chamber -> no damage.
    let safe = PveSiteInstance {
        sleeper_gas_cloud_active: false,
        sleeper_sentry_disabled: true,
        ..Default::default()
    };
    apply_sleeper_hazard_damage_if_active(&ctx, &session, sys_id, &safe).await;
    assert_eq!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp, 1_000.0);

    // Gas cloud only -> 200.0 thermal * 0.5 = 100.0 damage.
    let gas_only = PveSiteInstance {
        sleeper_gas_cloud_active: true,
        sleeper_sentry_disabled: true,
        ..Default::default()
    };
    apply_sleeper_hazard_damage_if_active(&ctx, &session, sys_id, &gas_only).await;
    let after_gas = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp;
    assert!((after_gas - 900.0).abs() < 1e-6, "expected 900.0 after thermal gas, got {after_gas}");

    // Active sentry only -> 200.0 kinetic * 0.25 = 50.0 damage.
    let sentry_only = PveSiteInstance {
        sleeper_gas_cloud_active: false,
        sleeper_sentry_disabled: false,
        ..Default::default()
    };
    apply_sleeper_hazard_damage_if_active(&ctx, &session, sys_id, &sentry_only).await;
    let after_sentry = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp;
    assert!((after_sentry - 850.0).abs() < 1e-6, "expected 850.0 after kinetic sentry, got {after_sentry}");

    // Both active -> 100.0 thermal + 50.0 kinetic = 150.0 damage.
    apply_sleeper_hazard_damage_if_active(&ctx, &session, sys_id, &active_both).await;
    let after_both = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp;
    assert!((after_both - 700.0).abs() < 1e-6, "expected 700.0 after combined hazard, got {after_both}");
}

#[test]
fn ghost_site_timer_boundary_non_ghost_and_zero_duration_preserve() {
    let sys_id: SolarSystemId = 30_000_142;
    let missing_sys: SolarSystemId = 30_000_144;
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_100_000)));

    with_pve_exploration_reg(&ctx, |reg| {
        // Unknown system returns false.
        assert!(!activate_ghost_site_timer(reg, missing_sys, "GHOST-0", 60));
        assert!(!is_ghost_site_timer_expired(reg, missing_sys, "GHOST-0"));

        reg.sites_by_system.insert(
            sys_id,
            vec![
                PveSiteInstance {
                    site_id: "RELIC-1".into(),
                    solar_system_id: sys_id,
                    site_category: "Relic".into(),
                    ghost_timer_started_at: Some(1_775_000_000),
                    ghost_detonate_after_sec: 1,
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "GHOST-KEEP".into(),
                    solar_system_id: sys_id,
                    site_category: "Ghost".into(),
                    ghost_detonate_after_sec: 45,
                    ..Default::default()
                },
            ],
        );

        // Non-ghost site never activates or expires even with started_at in the past.
        assert!(!activate_ghost_site_timer(reg, sys_id, "RELIC-1", 60));
        assert!(!is_ghost_site_timer_expired(reg, sys_id, "RELIC-1"));
        assert!(!activate_ghost_site_timer(reg, sys_id, "UNKNOWN-SITE", 60));

        // timer_duration_sec == 0 preserves preconfigured ghost_detonate_after_sec (45s).
        reg.sim_clock_sec = 1_775_100_000;
        assert!(activate_ghost_site_timer(reg, sys_id, "GHOST-KEEP", 0));
        // Second activation fails and leaves original started_at & duration intact.
        assert!(!activate_ghost_site_timer(reg, sys_id, "GHOST-KEEP", 5));

        reg.sim_clock_sec = 1_775_100_044;
        assert!(!is_ghost_site_timer_expired(reg, sys_id, "GHOST-KEEP"));
        reg.sim_clock_sec = 1_775_100_045;
        assert!(is_ghost_site_timer_expired(reg, sys_id, "GHOST-KEEP"));
    });
}

