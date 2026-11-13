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
            let npc_tid = pirate_npc_type_for_system(&sde, sys.solar_system_id);
            assert!(!(10_959..=10_963).contains(&npc_tid), "legacy non-existent NPC type_id {npc_tid} (#991)");
            assert!(
                sde.types.get(&npc_tid).is_some_and(|t| t.category_id == 11),
                "pirate NPC type_id {npc_tid} must exist in SDE with category_id == 11 (#991)"
            );
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signal_tracker_register_seeds_offgrid_anoms_warp_works_and_cleared_sites_stay_cleared() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let sys_id: SolarSystemId = 30_000_142;
    let char_id = 90_000_001_i64;
    let ship_id = 995_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.cosmic_anomalies.remove(&sys_id);
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.try_spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        ctx.build_ship_combat_state(char_id, ship_id, 587),
        ctx.build_ship_slim_meta(char_id, 587),
    );

    let mut sess = eve_machonet::SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let kw = std::collections::HashMap::new();

    // SignalTrackerRegister triggers seeding for the active system (#730)
    let _ = crate::services::fleet_social::handle(
        &ctx,
        &mut sess,
        "scanMgr",
        "SignalTrackerRegister",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("SignalTrackerRegister");

    let anoms = ctx.db.with_snapshot(|s| {
        s.cosmic_anomalies
            .get(&sys_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.anomaly_id.starts_with("PVE-ANOM-"))
            .collect::<Vec<_>>()
    });
    assert!(!anoms.is_empty(), "SignalTrackerRegister must seed PVE-ANOM sites");
    for a in &anoms {
        let dist = DVec3::from_array(a.position).length();
        assert!(
            dist >= 10_000_000_000.0,
            "PVE-ANOM {} must be off-grid (>= 10e9 m), got {dist}",
            a.anomaly_id
        );
    }

    // CmdWarpToStuff("ep_instance", anom_id) succeeds (#730)
    let target_anom_id = anoms[0].anomaly_id.clone();
    let warp_rep = crate::services::space_warp_jump::handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdWarpToStuff",
        &[
            eve_proto::PyRep::String("ep_instance".into()),
            eve_proto::PyRep::String(target_anom_id.clone()),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("CmdWarpToStuff ep_instance");
    assert!(warp_rep.as_i64().is_some());

    // Cleared anomaly stays cleared on re-seed, and legacy near-Sun anomaly gets moved off-grid
    let second_anom_id = anoms.get(1).map(|a| a.anomaly_id.clone());
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(list) = snap.cosmic_anomalies.get_mut(&sys_id) {
            if let Some(first) = list.iter_mut().find(|a| a.anomaly_id == target_anom_id) {
                first.cleared = true;
            }
            if let Some(sec_id) = &second_anom_id {
                if let Some(sec) = list.iter_mut().find(|a| a.anomaly_id == *sec_id) {
                    sec.position = [150_000.0, 0.0, 0.0];
                }
            }
        }
    });
    with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.remove(&sys_id);
        reg.sites_by_system.remove(&sys_id);
    });
    ensure_system_pve_sites_seeded(&ctx, sys_id).await;

    with_pve_exploration_reg(&ctx, |reg| {
        let sites = reg.sites_by_system.get(&sys_id).expect("re-seeded sites");
        assert!(
            !sites.iter().any(|s| s.site_id == target_anom_id),
            "cleared anomaly {target_anom_id} must not respawn"
        );
    });
    if let Some(sec_id) = second_anom_id {
        let upgraded_pos = ctx.db.with_snapshot(|s| {
            s.cosmic_anomalies
                .get(&sys_id)
                .and_then(|l| l.iter().find(|a| a.anomaly_id == sec_id))
                .map(|a| DVec3::from_array(a.position).length())
                .unwrap_or(0.0)
        });
        assert!(
            upgraded_pos >= 10_000_000_000.0,
            "legacy near-Sun anomaly must be upgraded off-grid, got {upgraded_pos}"
        );
    }
}

#[test]
fn synthetic_sde_covers_all_faction_fallbacks_ded_ratings_and_dungeon_name_lookup() {
    use eve_sde::{ConstellationInfo, DungeonTemplate, InvType, RegionInfo, SolarSystem};

    let mut sde = SdeRegistry::default();
    let cases = [
        (1u32, 101u32, "The Forge", Some(500_001u32), PirateFaction::Guristas),
        (2, 102, "Derelik", Some(500_003), PirateFaction::Sansha),
        (3, 103, "Kor-Azor", Some(500_003), PirateFaction::BloodRaiders),
        (4, 104, "Sinq Laison", Some(500_004), PirateFaction::Serpentis),
        (5, 105, "Heimatar", Some(500_002), PirateFaction::AngelCartel),
        (6, 106, "Lonetrek", None, PirateFaction::Guristas),
        (7, 107, "Everyshore", None, PirateFaction::Serpentis),
        (8, 108, "Bleak Lands", None, PirateFaction::BloodRaiders),
        (9, 109, "Scalding Pass", None, PirateFaction::AngelCartel),
    ];
    for (idx, (sys_id, reg_id, reg_name, fac, expected)) in cases.into_iter().enumerate() {
        let con_id = 200 + idx as u32;
        sde.regions.insert(
            reg_id,
            RegionInfo {
                region_id: reg_id,
                name: reg_name.into(),
                center: [0.0; 3],
                faction_id: fac,
                wormhole_class_id: None,
            },
        );
        sde.constellations.insert(
            con_id,
            ConstellationInfo {
                constellation_id: con_id,
                region_id: reg_id,
                name: format!("Con-{idx}"),
                center: [0.0; 3],
                faction_id: None,
                wormhole_class_id: None,
            },
        );
        sde.solar_systems.insert(
            sys_id,
            SolarSystem {
                solar_system_id: sys_id,
                region_id: reg_id,
                constellation_id: con_id,
                name: format!("Sys-{idx}"),
                security_status: 0.5,
                radius: 1.0e12,
                galactic_pos_ly: [0.0; 3],
                planet_ids: Vec::new(),
                stations: Vec::new(),
                wormhole_class_id: None,
            },
        );
        assert_eq!(pirate_faction_for_system(&sde, sys_id), expected);
    }
    // Unknown system falls back to Guristas
    assert_eq!(pirate_faction_for_system(&sde, 999_999_999), PirateFaction::Guristas);

    // Every DED rating arm across all 5 factions (sys 1..=5)
    for sys_id in 1..=5_u32 {
        for r in 1..=10_u8 {
            let (did, name) = ded_complex_for_rating(&sde, sys_id, r);
            assert!(did > 0 && !name.is_empty());
        }
        for sec in [0.8, 0.2, -0.4] {
            let anoms = anomaly_dungeons_for_system(&sde, sys_id, sec);
            assert_eq!(anoms.len(), 2);
        }
    }
    // Serpentis 2 vs 3 distinct, Angel 6..=10 distinct
    assert_ne!(ded_complex_for_rating(&sde, 4, 2).0, ded_complex_for_rating(&sde, 4, 3).0);
    assert_ne!(ded_complex_for_rating(&sde, 5, 6).0, ded_complex_for_rating(&sde, 5, 7).0);
    assert_ne!(ded_complex_for_rating(&sde, 5, 8).0, ded_complex_for_rating(&sde, 5, 9).0);
    assert_ne!(ded_complex_for_rating(&sde, 5, 9).0, ded_complex_for_rating(&sde, 5, 10).0);

    // Ghost site & Sleeper cache fallback when SDE lacks target ID (first no match, then name match)
    assert_eq!(
        ghost_site_dungeon_for_system(&sde, 1, 0.8, None),
        (5541, "Lesser Guristas Covert Research Facility".into())
    );
    assert_eq!(
        sleeper_cache_dungeon_for_system(&sde, 0.8, None),
        (5604, "Limited Sleeper Cache".into())
    );
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
            name: "Standard Sleeper Cache".into(),
            archetype_id: None,
            faction_id: None,
            difficulty: None,
        },
    );
    assert_eq!(ghost_site_dungeon_for_system(&sde, 1, 0.8, None).0, 99_001);
    assert_eq!(sleeper_cache_dungeon_for_system(&sde, 0.2, None).0, 99_002);

    // build_wave_npc_balls clamping and WH class 2/4/6
    assert_eq!(build_wave_npc_balls(&sde, 1, 100, 0, [0.0; 3], Some(2)).len(), 2);
    assert_eq!(build_wave_npc_balls(&sde, 1, 100, 9, [0.0; 3], Some(4)).len(), 4);
    assert_eq!(build_wave_npc_balls(&sde, 1, 100, 3, [0.0; 3], Some(6))[0].type_id, 30_198);

    // Type in SDE without combat attributes uses radius.max(25.0) & defaults
    sde.types.insert(
        77_001,
        InvType {
            type_id: 77_001,
            group_id: 25,
            category_id: 6,
            name: "TestType".into(),
            mass: 1_000.0,
            volume: 100.0,
            capacity: 10.0,
            radius: 10.0,
            base_price: 0.0,
            published: true,
            portion_size: 1,
            market_group_id: None,
            race_id: None,
            faction_id: None,
            variation_parent_type_id: None,
            description_id: None,
            icon_id: None,
            graphic_id: None,
            sound_id: None,
            meta_group_id: None,
            meta_level: None,
            tech_level: None,
            default_effect_id: None,
            description: String::new(),
            effect_ids: Vec::new(),
            default_attributes: std::collections::HashMap::new(),
        },
    );
    let cs = build_type_combat_state_from_sde(&sde, 77_001);
    assert_eq!(cs.max_shield_hp, 50.0);
    assert_eq!(cs.signature_radius, 25.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seeding_preserves_expeditions_emits_tracker_deltas_and_handles_special_wh() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_200_000)));
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    db.acquire_online_lock(1, 701, Some(char_id), true, 1_776_200_000)
        .expect("online lock");
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = sys_id;
        }
        snap.fleet_and_social_ops.signal_tracker_subs.insert(sys_id, vec![char_id]);
    });

    // Pre-insert an Expedition site; ensure_system_pve_sites_seeded must preserve it
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.entry(sys_id).or_default().push(PveSiteInstance {
            site_id: "EXP-KEEP-1".into(),
            solar_system_id: sys_id,
            site_category: "Expedition".into(),
            ..Default::default()
        });
    });

    ensure_system_pve_sites_seeded(&ctx, sys_id).await;

    with_pve_exploration_reg(&ctx, |reg| {
        let sites = &reg.sites_by_system[&sys_id];
        assert!(sites.iter().any(|s| s.site_id == "EXP-KEEP-1"));
        assert!(sites.iter().any(|s| s.site_category == "DED"));
        assert!(sites.iter().any(|s| s.site_category == "Deadspace"));
        assert!(sites.iter().any(|s| s.site_category == "Ghost"));
        assert!(sites.iter().any(|s| s.site_category == "SleeperCache"));
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.cosmic_anomalies[&sys_id].is_empty());
        assert!(snap.cosmic_signatures[&sys_id].len() >= 5);
    });
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnSignalTrackerAnomalyUpdate" || n.event.contains("SignalTracker")));

    // Special C14..C18 wormhole system hits early-return branch in seed_special_wormhole_pve_sites
    if let Some(c14_sys) = sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| (31_000_000..32_000_000).contains(&sid) && sid >= 31_002_600)
    {
        ensure_system_pve_sites_seeded(&ctx, c14_sys).await;
        with_pve_exploration_reg(&ctx, |reg| {
            assert!(reg.seeded_systems.get(&c14_sys).copied().unwrap_or(false));
        });
    }
}
