//! Unit and integration tests for `pve_exploration_sites`.

use super::*;
use crate::router::ServerContext;
use crate::services::combat_crimewatch::CombatRuntimeState;
use crate::services::pve_exploration::SpewedLootCan;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::sync::{Arc, OnceLock, RwLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn test_loot_generation_ded_restrictions_and_sleeper_tables() {
    let sde = shared_sde();

    // Highsec data & relic loot
    let hs_data = generate_exploration_container_loot(&sde, 30_000_142, false);
    assert!(!hs_data.is_empty());
    let hs_relic = generate_exploration_container_loot(&sde, 30_000_142, true);
    assert!(!hs_relic.is_empty());

    // Lowsec & wormhole deep-space loot includes ancient salvage on relic sites and higher quantities
    let ls_sys = sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id < 31_000_000 && s.security_status > 0.0 && s.security_status < 0.45)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_000_142);
    let ls_data = generate_exploration_container_loot(&sde, ls_sys, false);
    assert!(!ls_data.is_empty());
    let wh_relic = generate_exploration_container_loot(&sde, 31_000_005, true);
    assert!(wh_relic.len() > hs_relic.len());
    let wh_data = generate_exploration_container_loot(&sde, 31_000_005, false);
    assert!(!wh_data.is_empty());

    // DED ship restrictions
    let (m1, _, g1) = ded_ship_restriction_for_rating(2);
    assert_eq!(m1, 10_000_000.0);
    assert!(g1.contains(&25) && !g1.contains(&26));
    let (m5, _, g5) = ded_ship_restriction_for_rating(5);
    assert_eq!(m5, 75_000_000.0);
    assert!(g5.contains(&26));
    let (m9, _, g9) = ded_ship_restriction_for_rating(9);
    assert_eq!(m9, 250_000_000.0);
    assert!(g9.contains(&27));

    assert_eq!(
        check_ded_gate_ship_allowed(&sde, 2, 999_999_999, 1_000_000.0)
            .unwrap_err()
            .code,
        "ShipSizeRestricted"
    );
    // Frigate (type 587, group 25) allowed in 2/10 when under mass, rejected if over mass
    assert!(check_ded_gate_ship_allowed(&sde, 2, 587, 1_200_000.0).is_ok());
    assert!(check_ded_gate_ship_allowed(&sde, 2, 587, 25_000_000.0).is_err());

    // Overseer personal effects & Sleeper wave/blue loot tables
    let ope1 = overseer_effects_for_rating(&sde, 1);
    let ope10 = overseer_effects_for_rating(&sde, 10);
    assert!(ope1.is_some() && ope10.is_some());
    assert_ne!(ope1.unwrap().0, ope10.unwrap().0);

    for wh_class in 1..=6 {
        let (t1, e1) = sleeper_npc_types_for_class_and_wave(wh_class, 1);
        let (t2, e2) = sleeper_npc_types_for_class_and_wave(wh_class, 2);
        assert!(t1 > 0 && !e1.is_empty());
        assert!(t2 > 0 && !e2.is_empty());
        assert!(sleeper_blue_loot_for_class(&sde, wh_class).is_some());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_spewed_cans_ded_wrecks_and_sleeper_waves() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_100_000)));
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db);
    let sys_id: SolarSystemId = 31_000_005;
    let char_id: i64 = 90_100_001;

    // Empty slice early return
    materialize_spewed_cans_core(&ctx.db, &ctx.systems, char_id, &[]).await;

    let cans = vec![SpewedLootCan {
        can_item_id: 1_900_001,
        parent_container_id: 1_800_001,
        solar_system_id: sys_id,
        position: [1_000.0, 2_000.0, -500.0],
        loot_type_id: 34,
        loot_quantity: 250,
        spawned_at_sec: 1_776_100_000,
        despawn_at_sec: 1_776_103_600,
        scooped: false,
    }];
    materialize_spewed_cans_core(&ctx.db, &ctx.systems, char_id, &cans).await;
    ctx.db.with_snapshot(|snap| {
        assert!(snap.items.contains_key(&1_900_001));
        assert!(snap.items.values().any(|i| i.location_id == 1_900_001 && i.quantity == 250));
    });

    // DED overseer wrecks (low & high rating)
    let (wid_low, loot_low) = spawn_ded_overseer_wreck_core(
        &ctx.db,
        &ctx.systems,
        &ctx.sde,
        30_000_142,
        char_id,
        DVec3::ZERO,
        2,
    )
    .await;
    let (wid_hi, loot_hi) = spawn_ded_overseer_wreck_core(
        &ctx.db,
        &ctx.systems,
        &ctx.sde,
        30_000_142,
        char_id,
        DVec3::new(500.0, 0.0, 0.0),
        8,
    )
    .await;
    assert!(wid_low > 0 && wid_hi > wid_low);
    assert!(!loot_low.is_empty() && !loot_hi.is_empty());

    // Sleeper wave 1 (includes sentry) & wave 2 (no sentry) + sleeper wreck drop
    let w1_balls = spawn_sleeper_wave_in_system(&ctx, sys_id, 3, 1, DVec3::ZERO).await;
    let w2_balls = spawn_sleeper_wave_in_system(&ctx, sys_id, 3, 2, DVec3::new(1000.0, 0.0, 0.0)).await;
    assert_eq!(w1_balls.len(), 4);
    assert_eq!(w2_balls.len(), 3);

    let combat_rt = Arc::new(RwLock::new(CombatRuntimeState::default()));
    let w3_balls = spawn_sleeper_wave_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &combat_rt,
        sys_id,
        5,
        2,
        DVec3::ZERO,
    )
    .await;
    assert_eq!(w3_balls.len(), 3);
    assert_eq!(combat_rt.read().unwrap().npc_entities.len(), 3);

    let sw_id = drop_sleeper_wreck_core(
        &ctx.db,
        &ctx.systems,
        &ctx.sde,
        sys_id,
        char_id,
        DVec3::new(250.0, 0.0, 0.0),
        5,
    )
    .await;
    assert!(sw_id > 0);
}
