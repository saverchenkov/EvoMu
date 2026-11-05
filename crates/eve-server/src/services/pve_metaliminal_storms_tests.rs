//! Unit tests for `pve_metaliminal_storms`.

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_sde::{SdeRegistry, SolarSystemId};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed()))
        .clone()
}

fn find_nullsec_system(sde: &SdeRegistry) -> SolarSystemId {
    let mut cands: Vec<SolarSystemId> = sde
        .solar_systems
        .values()
        .filter(|s| is_nullsec_storm_system(sde, s.solar_system_id))
        .map(|s| s.solar_system_id)
        .collect();
    cands.sort_unstable();
    cands.into_iter().next().unwrap_or(30_000_208)
}

#[test]
fn storm_beacons_footprint_movement_and_anomalies() {
    let sde = shared_sde();
    let ns_sys = find_nullsec_system(&sde);
    assert!(is_nullsec_storm_system(&sde, ns_sys));
    assert!(!is_nullsec_storm_system(&sde, 31_000_001));
    assert!(!is_nullsec_storm_system(&sde, 99_999_999));

    for (w, expected) in [
        (AbyssalWeatherKind::Electrical, (47381, 47382)),
        (AbyssalWeatherKind::Exotic, (47384, 47385)),
        (AbyssalWeatherKind::Gamma, (47387, 47388)),
        (AbyssalWeatherKind::Firestorm, (47390, 47391)),
        (AbyssalWeatherKind::Dark, (47378, 47379)),
    ] {
        let pair = storm_weather_beacons(w);
        assert_eq!(pair, expected);
        assert!(is_storm_beacon_type(pair.0));
        assert!(is_storm_beacon_type(pair.1));
    }
    assert!(!is_storm_beacon_type(34));

    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_500_000)));

    let (core, periphery) = compute_storm_footprint(&ctx, ns_sys);
    assert!(core.contains(&ns_sys));
    assert!(periphery.iter().all(|p| !core.contains(p)));

    for step in 0..5u32 {
        let interval = storm_move_interval_secs(1, step);
        assert!((STORM_MOVE_MIN_SECS..=STORM_MOVE_MAX_SECS).contains(&interval));
    }

    with_pve_special_reg(&ctx, |reg| {
        init_roaming_storms(&ctx, reg);
        assert_eq!(reg.roaming_storms.len(), 4);
        for s in &mut reg.roaming_storms {
            s.next_move_at_sec = 1_775_500_000;
        }
    });
    sync_metaliminal_storm_effects_and_sites(&ctx);

    let initial_centers: Vec<SolarSystemId> = with_pve_special_reg(&ctx, |reg| {
        reg.roaming_storms.iter().map(|s| s.center_solar_system_id).collect()
    });
    ctx.db.with_snapshot(|snap| {
        let has_volatile_ice = snap
            .cosmic_anomalies
            .values()
            .flatten()
            .any(|a| a.anomaly_id.starts_with("DNG-9042-") && a.ore_types.contains(&56142));
        assert!(has_volatile_ice, "storm core spawns Volatile Ice anomaly");
    });

    with_pve_special_reg(&ctx, |reg| {
        reg.sim_clock_sec = 1_775_700_000;
        advance_roaming_storms(&ctx, reg, 200_000);
        for (idx, s) in reg.roaming_storms.iter().enumerate() {
            if !nullsec_neighbors(&ctx.sde, initial_centers[idx]).is_empty() {
                assert!(s.step_count >= 1);
            }
        }
    });
    sync_metaliminal_storm_effects_and_sites(&ctx);

    let list = get_all_roaming_weather_systems(&ctx);
    assert!(!list.is_empty());
}
