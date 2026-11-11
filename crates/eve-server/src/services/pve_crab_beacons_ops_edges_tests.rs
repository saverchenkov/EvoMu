#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::test_ctx;
use crate::services::pve_crab_beacons::{TYPE_CARRIER_CRAB_BEACON, TYPE_CRAB_BEACON};
use eve_db::clock::VirtualServerClock;
use eve_db::ServerClock;
use std::sync::Arc;

#[test]
fn crab_waves_rewards_and_ceq_cost_branches() {
    let ctx = test_ctx();

    // 1. Canonical SDE waves: carrier vs standard across waves 1, 2, 3
    let std_w1 = resolve_crab_wave_drone_types(&ctx.sde, TYPE_CRAB_BEACON, 1);
    let std_w2 = resolve_crab_wave_drone_types(&ctx.sde, TYPE_CRAB_BEACON, 2);
    let std_w3 = resolve_crab_wave_drone_types(&ctx.sde, TYPE_CRAB_BEACON, 3);
    assert_eq!(std_w1.len(), 3);
    assert_eq!(std_w2.len(), 4);
    assert_eq!(std_w3.len(), 3);

    let car_w1 = resolve_crab_wave_drone_types(&ctx.sde, TYPE_CARRIER_CRAB_BEACON, 1);
    let car_w2 = resolve_crab_wave_drone_types(&ctx.sde, TYPE_CARRIER_CRAB_BEACON, 2);
    let car_w3 = resolve_crab_wave_drone_types(&ctx.sde, TYPE_CARRIER_CRAB_BEACON, 3);
    assert_eq!(car_w1.len(), 4);
    assert_eq!(car_w2.len(), 5);
    assert_eq!(car_w3.len(), 4);

    // 2. Empty SDE fallback: hardcoded drones [18_072, ...] and hardcoded mutaplasmids [60_476, 60_477]
    let empty_sde = SdeRegistry::default();
    let empty_w1 = resolve_crab_wave_drone_types(&empty_sde, TYPE_CRAB_BEACON, 1);
    assert_eq!(empty_w1, vec![18_072, 18_074, 18_084]);
    let empty_rewards = resolve_crab_completion_rewards(&empty_sde, TYPE_CRAB_BEACON);
    assert_eq!(empty_rewards, vec![(60_459, 15), (60_476, 1), (60_477, 1)]);

    // 3. Partial SDE fallback: <10 primary (4037) drones -> uses fallback groups 801..=805/844;
    //    and non-preferred group 1964 mutaplasmids -> exercises chosen_mutas.len() < 2 branch
    let mut partial_sde = SdeRegistry::default();
    for t in ctx
        .sde
        .types
        .values()
        .filter(|t| t.published && matches!(t.group_id, 801..=805 | 844) && !t.name.contains("DO NOT TRANSLATE"))
        .take(3)
    {
        partial_sde.types.insert(t.type_id, t.clone());
    }
    for t in ctx
        .sde
        .types
        .values()
        .filter(|t| {
            t.published
                && t.group_id == 1964
                && !matches!(t.type_id, 60_476 | 60_477)
                && !t.name.contains("DO NOT TRANSLATE")
        })
        .take(3)
    {
        partial_sde.types.insert(t.type_id, t.clone());
    }
    if let Some(chip) = ctx.sde.types.get(&33_619) {
        partial_sde.types.insert(33_619, chip.clone());
    }
    let partial_w2 = resolve_crab_wave_drone_types(&partial_sde, TYPE_CRAB_BEACON, 2);
    assert_eq!(partial_w2.len(), 4);
    let partial_rewards = resolve_crab_completion_rewards(&partial_sde, TYPE_CARRIER_CRAB_BEACON);
    assert_eq!(partial_rewards[0], (33_619, 10));
    assert_eq!(partial_rewards.len(), 3);

    // 4. Canonical completion rewards for carrier vs standard
    let std_rew = resolve_crab_completion_rewards(&ctx.sde, TYPE_CRAB_BEACON);
    assert_eq!(std_rew[0].1, 15);
    let car_rew = resolve_crab_completion_rewards(&ctx.sde, TYPE_CARRIER_CRAB_BEACON);
    assert_eq!(car_rew[0].1, 10);

    // 5. effective_crab_ceq_cost clamping & scaling
    assert_eq!(effective_crab_ceq_cost(30, -0.5), (30, 1.0));
    assert_eq!(effective_crab_ceq_cost(30, 0.0), (30, 1.0));
    assert_eq!(effective_crab_ceq_cost(30, 0.5), (32, 1.05));
    assert_eq!(effective_crab_ceq_cost(30, 1.5), (33, 1.10));
}

#[test]
fn crab_interference_activity_decay_bands_and_notifications() {
    let ctx = test_ctx();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000));
    ctx.set_clock(clock.clone());
    let sys_id = 30_000_208_u32;

    // Seed a docked character and a structure-docked character in sys_id so emit_interference_changed_clusterwide exercises both targets
    let uid2 = ctx.db.provision_player_account("crab_pilot_2", "pass123");
    let mut uid1 = 1_u64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&90_000_001) {
            uid1 = c.user_id;
            c.solar_system_id = sys_id;
            c.station_id = Some(60_003_760);
            c.structure_id = None;
        }
        if let Some(mut c2) = snap.characters.get(&90_000_001).cloned() {
            c2.character_id = 90_000_111;
            c2.user_id = uid2;
            c2.solar_system_id = sys_id;
            c2.station_id = None;
            c2.structure_id = Some(1_880_000_555);
            snap.characters.insert(90_000_111, c2);
        }
    });
    let _ = ctx.db.acquire_online_lock(uid1, 1, Some(90_000_001), true, ctx.now_unix());
    let _ = ctx.db.acquire_online_lock(uid2, 2, Some(90_000_111), true, ctx.now_unix());

    // Zero delta at t=0 -> no change (None)
    assert_eq!(apply_interference_activity(&ctx, sys_id, 0.0, 3_600.0), None);

    // Increase into band 1 -> Some
    let (lvl1, band1) = apply_interference_activity(&ctx, sys_id, 0.20, 3_600.0).unwrap();
    assert!((lvl1 - 0.20).abs() < 1e-6);
    assert_eq!(band1, 1);

    // Increase into band 2 (>= 0.35)
    let (lvl2, band2) = apply_interference_activity(&ctx, sys_id, 0.25, 3_600.0).unwrap();
    assert!((lvl2 - 0.45).abs() < 1e-6);
    assert_eq!(band2, 2);

    // Increase into band 3 (>= 0.70)
    let (lvl3, band3) = apply_interference_activity(&ctx, sys_id, 0.35, 3_600.0).unwrap();
    assert!((lvl3 - 0.80).abs() < 1e-6);
    assert_eq!(band3, 3);

    // Emit clusterwide notification
    emit_interference_changed_clusterwide(&ctx, sys_id, lvl3, band3);
    let pending = ctx.pending_notifications.read().unwrap();
    assert!(pending
        .get(&90_000_001)
        .is_some_and(|v| v.iter().any(|n| n.event == "OnSolarsystemInterferenceChanged")));
    assert!(pending
        .get(&90_000_111)
        .is_some_and(|v| v.iter().any(|n| n.event == "OnSolarsystemInterferenceChanged")));
    drop(pending);

    // Advance clock by half the decay window -> decays by 0.50 -> drops back to band 1 (0.30)
    clock.advance_secs(1_800);
    let (lvl4, band4) = apply_interference_activity(&ctx, sys_id, 0.0, 3_600.0).unwrap();
    assert!((lvl4 - 0.30).abs() < 1e-6);
    assert_eq!(band4, 1);

    // Advance full window -> decays to 0.0
    clock.advance_secs(3_600);
    let (lvl5, band5) = apply_interference_activity(&ctx, sys_id, 0.0, 3_600.0).unwrap();
    assert!((lvl5 - 0.0).abs() < 1e-9);
    assert_eq!(band5, 1);

    // Again at 0.0 with no activity -> None
    clock.advance_secs(60);
    assert_eq!(apply_interference_activity(&ctx, sys_id, 0.0, 3_600.0), None);
}
