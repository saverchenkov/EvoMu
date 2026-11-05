//! Unit and integration tests for `pve_exploration_seeding`.

use super::*;
use crate::router::ServerContext;
use crate::services::pve_exploration::with_pve_exploration_reg;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_sde::{SdeRegistry, SolarSystemId};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn test_faction_and_site_catalog_tables_across_security_bands() {
    let sde = shared_sde();

    // Sample systems across highsec, lowsec, nullsec, and C1..C6 wormholes
    for sec in [0.9, 0.3, -0.5] {
        let anoms = anomaly_dungeons_for_system(&sde, 30_000_142, sec);
        assert_eq!(anoms.len(), 2);
        let (gid, gname) = ghost_site_dungeon_for_system(&sde, 30_000_142, sec, None);
        assert!(gid > 0 && !gname.is_empty());
        let (sid, sname) = sleeper_cache_dungeon_for_system(&sde, sec, None);
        assert!(sid > 0 && !sname.is_empty());
    }

    // Wormhole anomalies, ghost sites, and sleeper caches across C1..=C6
    for wh_class in [1_u8, 3_u8, 5_u8] {
        let wh_sys = sde
            .solar_systems
            .values()
            .find(|s| {
                crate::services::pve_exploration::wormhole_class_for_system(s.solar_system_id)
                    == Some(wh_class)
            })
            .map(|s| s.solar_system_id)
            .expect("wormhole system in SDE");
        let wh_anoms = anomaly_dungeons_for_system(&sde, wh_sys, -1.0);
        assert_eq!(wh_anoms.len(), 2);
        assert_eq!(wh_anoms[0].4, Some(wh_class));
        let (g_id, g_name) = ghost_site_dungeon_for_system(&sde, wh_sys, -1.0, Some(wh_class));
        assert!(g_id > 0 && g_name.contains("Superior"));
        let (s_id, s_name) = sleeper_cache_dungeon_for_system(&sde, -1.0, Some(wh_class));
        assert!(s_id > 0 && s_name.contains("Superior"));
    }

    // Exercise faction-distinct systems from SDE regions (Amarr, Minmatar, Gallente, Caldari)
    let mut seen_factions = std::collections::HashSet::new();
    for sys in sde.solar_systems.values() {
        if sys.solar_system_id >= 30_100_000 || sys.security_status < -1.0 {
            continue;
        }
        let fac = pirate_faction_for_system(&sde, sys.solar_system_id);
        let tag = match fac {
            PirateFaction::Guristas => "G",
            PirateFaction::BloodRaiders => "B",
            PirateFaction::Sansha => "S",
            PirateFaction::Serpentis => "Se",
            PirateFaction::AngelCartel => "A",
        };
        if seen_factions.insert(tag) {
            for r in [1_u8, 2, 4, 6, 8, 10] {
                let (did, dname) = ded_complex_for_rating(&sde, sys.solar_system_id, r);
                assert!(did > 0 && !dname.is_empty());
            }
            for sec in [0.9, 0.3, -0.5] {
                assert!(!anomaly_dungeons_for_system(&sde, sys.solar_system_id, sec).is_empty());
                let (gid, gname) =
                    ghost_site_dungeon_for_system(&sde, sys.solar_system_id, sec, None);
                assert!(gid > 0 && !gname.is_empty());
            }
            assert!(pirate_npc_type_for_system(&sde, sys.solar_system_id) > 0);
            let (loot_tid, faction_name) =
                pirate_commander_loot_for_system(&sde, sys.solar_system_id);
            assert!(loot_tid > 0 && !faction_name.is_empty());
        }
        if seen_factions.len() == 5 {
            break;
        }
    }
    assert_eq!(seen_factions.len(), 5);

    // Wave NPC ball builder & combat state builder
    let k_balls = build_wave_npc_balls(&sde, 30_000_142, 2134, 2, [0.0, 0.0, 0.0], None);
    assert_eq!(k_balls.len(), 2);
    assert!(!build_wave_npc_balls(&sde, 31_000_005, 3707, 1, [0.0, 0.0, 0.0], Some(1)).is_empty());
    assert!(!build_wave_npc_balls(&sde, 31_000_500, 3710, 3, [0.0, 0.0, 0.0], Some(3)).is_empty());
    assert!(!build_wave_npc_balls(&sde, 31_001_800, 3704, 4, [0.0, 0.0, 0.0], Some(5)).is_empty());

    let cs_real = build_type_combat_state_from_sde(&sde, 587);
    assert!(cs_real.max_shield_hp > 0.0);
    let cs_fallback = build_type_combat_state_from_sde(&sde, 999_999_999);
    assert!(cs_fallback.max_shield_hp >= 0.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_ensure_system_pve_sites_seeded_highsec_lowsec_nullsec_and_wh() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_200_000)));
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db);

    let lowsec_sys = sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id < 30_100_000 && s.security_status > 0.0 && s.security_status < 0.45)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_000_142);
    let nullsec_sys = sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id < 30_100_000 && s.security_status <= 0.0)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_000_142);

    let test_systems: Vec<SolarSystemId> = vec![30_000_142, lowsec_sys, nullsec_sys, 31_000_005];
    for sys in &test_systems {
        ensure_system_pve_sites_seeded(&ctx, *sys).await;
        // Calling a second time is idempotent
        ensure_system_pve_sites_seeded(&ctx, *sys).await;
    }

    with_pve_exploration_reg(&ctx, |reg| {
        for sys in &test_systems {
            assert!(reg.seeded_systems.get(sys).copied().unwrap_or(false));
            let sites = reg.sites_by_system.get(sys).expect("seeded sites");
            assert!(!sites.is_empty());
        }
    });
}
