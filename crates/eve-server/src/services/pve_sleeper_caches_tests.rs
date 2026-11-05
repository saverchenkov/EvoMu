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
