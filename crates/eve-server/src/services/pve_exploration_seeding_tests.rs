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

#[tokio::test]
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
