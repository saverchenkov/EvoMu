//! Edge-case unit tests for `pve_exploration_seeding`: faction fallbacks,
//! DED ratings 1..=10 across all factions, SDE dungeon name fallbacks, special wormhole
//! early returns, pre-existing anomaly/sig deduplication, and Expedition site retention.

use super::*;
use crate::router::ServerContext;
use crate::services::pve_exploration::with_pve_exploration_reg;
use eve_db::clock::VirtualServerClock;
use eve_db::{CosmicAnomalyRecord, CosmicSignatureRecord, WorldRepository};
use eve_sde::{ConstellationInfo, DungeonTemplate, RegionInfo, SdeRegistry, SolarSystem};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_synth_sde() -> SdeRegistry {
    let mut sde = SdeRegistry::default();
    let region_defs = [
        (10_000_001, "Derelik", Some(500_003)),
        (10_000_002, "Amarr Region", Some(500_003)),
        (10_000_003, "The Forge", None),
        (10_000_004, "Sinq Laison", None),
        (10_000_005, "Domain", None),
        (10_000_006, "Heimatar", None),
        (10_000_007, "Gallente Space", Some(500_004)),
        (10_000_008, "Minmatar Space", Some(500_002)),
    ];
    for (rid, rname, rfac) in region_defs {
        sde.regions.insert(
            rid,
            RegionInfo {
                region_id: rid,
                name: rname.into(),
                center: [0.0; 3],
                faction_id: rfac,
                wormhole_class_id: None,
            },
        );
        sde.constellations.insert(
            rid + 1000,
            ConstellationInfo {
                constellation_id: rid + 1000,
                region_id: rid,
                name: format!("Con-{rname}"),
                center: [0.0; 3],
                faction_id: None,
                wormhole_class_id: None,
            },
        );
        sde.solar_systems.insert(
            30_900_000 + rid,
            SolarSystem {
                solar_system_id: 30_900_000 + rid,
                constellation_id: rid + 1000,
                region_id: rid,
                name: format!("Sys-{rname}"),
                security_status: 0.5,
                radius: 1e12,
                galactic_pos_ly: [0.0; 3],
                planet_ids: vec![],
                stations: vec![],
                wormhole_class_id: None,
            },
        );
    }
    sde
}

#[test]
fn pirate_faction_all_region_and_faction_branches_plus_unknown_system() {
    let sde = make_synth_sde();

    // Unknown system falls back to Guristas
    assert_eq!(pirate_faction_for_system(&sde, 999_999_999), PirateFaction::Guristas);

    // 500003 in Derelik -> Sansha; 500003 in non-Sansha region -> BloodRaiders
    assert_eq!(pirate_faction_for_system(&sde, 30_900_000 + 10_000_001), PirateFaction::Sansha);
    assert_eq!(pirate_faction_for_system(&sde, 30_900_000 + 10_000_002), PirateFaction::BloodRaiders);

    // Region name fallbacks when faction_id is None
    assert_eq!(pirate_faction_for_system(&sde, 30_900_000 + 10_000_003), PirateFaction::Guristas);
    assert_eq!(pirate_faction_for_system(&sde, 30_900_000 + 10_000_004), PirateFaction::Serpentis);
    assert_eq!(pirate_faction_for_system(&sde, 30_900_000 + 10_000_005), PirateFaction::BloodRaiders);
    assert_eq!(pirate_faction_for_system(&sde, 30_900_000 + 10_000_006), PirateFaction::AngelCartel);

    // Explicit faction IDs 500004 -> Serpentis, 500002 -> AngelCartel
    assert_eq!(pirate_faction_for_system(&sde, 30_900_000 + 10_000_007), PirateFaction::Serpentis);
    assert_eq!(pirate_faction_for_system(&sde, 30_900_000 + 10_000_008), PirateFaction::AngelCartel);
}

#[test]
fn ded_complex_all_ratings_and_ghost_sleeper_sde_fallbacks() {
    let mut sde = make_synth_sde();

    let faction_systems = [
        (30_900_000 + 10_000_003, PirateFaction::Guristas),
        (30_900_000 + 10_000_002, PirateFaction::BloodRaiders),
        (30_900_000 + 10_000_001, PirateFaction::Sansha),
        (30_900_000 + 10_000_007, PirateFaction::Serpentis),
        (30_900_000 + 10_000_008, PirateFaction::AngelCartel),
    ];

    for (sys_id, fac) in faction_systems {
        assert_eq!(pirate_faction_for_system(&sde, sys_id), fac);
        for rating in 1..=10u8 {
            let (did, name) = ded_complex_for_rating(&sde, sys_id, rating);
            assert!(did > 0 && !name.is_empty());
        }
        for sec in [0.9, 0.25, -0.5] {
            let anoms = anomaly_dungeons_for_system(&sde, sys_id, sec);
            assert_eq!(anoms.len(), 2);
        }
        assert!(pirate_npc_type_for_system(&sde, sys_id) > 0);
        let (loot_id, loot_name) = pirate_commander_loot_for_system(&sde, sys_id);
        assert!(loot_id > 0 && !loot_name.is_empty());
    }

    // Without dungeons in SDE, ghost & sleeper cache hit final (target_did, expected_name) fallback
    let (gdid, gname) = ghost_site_dungeon_for_system(&sde, 30_900_000 + 10_000_003, 0.8, None);
    assert_eq!(gdid, 5541);
    assert_eq!(gname, "Lesser Guristas Covert Research Facility");

    let (sdid, sname) = sleeper_cache_dungeon_for_system(&sde, 0.8, None);
    assert_eq!(sdid, 5604);
    assert_eq!(sname, "Limited Sleeper Cache");

    // With a dungeon matching by trimmed name under a different ID, name-lookup fallback hits
    sde.dungeons.insert(
        99_001,
        DungeonTemplate {
            dungeon_id: 99_001,
            name: "  Lesser Guristas Covert Research Facility  ".into(),
            archetype_id: None,
            faction_id: None,
            difficulty: None,
        },
    );
    sde.dungeons.insert(
        99_002,
        DungeonTemplate {
            dungeon_id: 99_002,
            name: "  Standard Sleeper Cache  ".into(),
            archetype_id: None,
            faction_id: None,
            difficulty: None,
        },
    );
    assert_eq!(
        ghost_site_dungeon_for_system(&sde, 30_900_000 + 10_000_003, 0.8, None),
        (99_001, "Lesser Guristas Covert Research Facility".into())
    );
    assert_eq!(
        sleeper_cache_dungeon_for_system(&sde, 0.2, None),
        (99_002, "Standard Sleeper Cache".into())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ensure_seeded_covers_all_factions_special_wh_unknown_sys_and_preexisting_records() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_200_000)));
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db);

    // Pick one K-space system for each of the 5 pirate factions
    let mut faction_systems = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for sys in sde.solar_systems.values() {
        if sys.solar_system_id >= 30_100_000 {
            continue;
        }
        let fac = pirate_faction_for_system(&sde, sys.solar_system_id);
        let key = format!("{fac:?}");
        if seen.insert(key) {
            faction_systems.push(sys.solar_system_id);
        }
        if seen.len() == 5 {
            break;
        }
    }
    assert_eq!(faction_systems.len(), 5);

    // Pre-populate an existing anomaly, signature, and Expedition site in the first system
    let first_sys = faction_systems[0];
    let first_anom_id = format!("PVE-ANOM-{first_sys}-1");
    let first_sig_id = format!("DAT-{first_sys}");
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies
            .entry(first_sys)
            .or_default()
            .push(CosmicAnomalyRecord {
                anomaly_id: first_anom_id.clone(),
                solar_system_id: first_sys,
                anomaly_type: "Combat".into(),
                name: "Preexisting".into(),
                position: [0.0; 3],
                signal_strength: 1.0,
                ore_types: vec![],
                remaining_volume: 0.0,
                created_unix: 1,
                lifetime_seconds: 86_400,
                cleared: false,
            });
        snap.cosmic_signatures
            .entry(first_sys)
            .or_default()
            .push(CosmicSignatureRecord {
                signature_id: first_sig_id.clone(),
                solar_system_id: first_sys,
                sig_group: "Data".into(),
                type_name: "Preexisting".into(),
                position: [0.0; 3],
                signal_strength: 0.0,
                wormhole_type_id: None,
                destination_system_id: None,
                created_unix: 1,
                lifetime_seconds: 86_400,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: false,
            });
    });
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.entry(first_sys).or_default().push(PveSiteInstance {
            site_id: "EXP-KEEP".into(),
            site_category: "Expedition".into(),
            solar_system_id: first_sys,
            ..Default::default()
        });
        reg.sites_by_system.entry(first_sys).or_default().push(PveSiteInstance {
            site_id: "OLD-NON-EXP".into(),
            site_category: "CombatAnomaly".into(),
            solar_system_id: first_sys,
            ..Default::default()
        });
    });

    for sys_id in faction_systems {
        ensure_system_pve_sites_seeded(&ctx, sys_id).await;
    }

    // Verify Expedition site kept and non-Expedition replaced; pre-existing anom/sig not duplicated
    with_pve_exploration_reg(&ctx, |reg| {
        let sites = &reg.sites_by_system[&first_sys];
        assert!(sites.iter().any(|s| s.site_id == "EXP-KEEP"));
        assert!(!sites.iter().any(|s| s.site_id == "OLD-NON-EXP"));
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.cosmic_anomalies[&first_sys]
                .iter()
                .filter(|a| a.anomaly_id == first_anom_id)
                .count(),
            1
        );
        assert_eq!(
            snap.cosmic_signatures[&first_sys]
                .iter()
                .filter(|s| s.signature_id == first_sig_id)
                .count(),
            1
        );
    });

    // Special wormhole system (e.g. C14-C18 Drifter WH or C13 Shattered) triggers early return
    if let Some(special_wh) = sde
        .solar_systems
        .values()
        .find(|s| matches!(wormhole_class_for_system(s.solar_system_id), Some(13..=18)))
        .map(|s| s.solar_system_id)
    {
        ensure_system_pve_sites_seeded(&ctx, special_wh).await;
        with_pve_exploration_reg(&ctx, |reg| {
            assert!(reg.seeded_systems.get(&special_wh).copied().unwrap_or(false));
        });
    }

    // Unknown solar system falls back to true_sec = 0.5
    ensure_system_pve_sites_seeded(&ctx, 30_999_999).await;
    with_pve_exploration_reg(&ctx, |reg| {
        assert!(reg.seeded_systems.get(&30_999_999).copied().unwrap_or(false));
    });
}
