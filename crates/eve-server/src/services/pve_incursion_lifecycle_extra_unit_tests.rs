//! Extra unit and mutation-killing tests for `services::pve_incursion_lifecycle`
//! covering NPC combat state attribute overrides, constellation eligibility filters and sorting,
//! stargate graph role assignment edge cases, wave reinforcement IDs and staging wave progression,
//! snapshot/owner-note system effect calculations, influence regeneration & mothership despawn
//! boundaries, respawn cadence gates, and world-event tale collection across all 6 sources.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_type;
use crate::services::pve_incursion_lifecycle::*;
use crate::services::pve_incursion_sites::{
    seed_incursion_for_constellation, IncursionCompletionResult,
};
use crate::services::pve_metaliminal_storms::RoamingStormRecord;
use crate::services::pve_special_sites::{
    with_pve_special_reg, AbyssalWeatherKind, PveSpecialSitesRegistry, TriglavianSiteRecord,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_proto::PyRep;
use eve_sde::{
    attr_ids, BloodlineInfo, ConstellationInfo, SchoolInfo, SdeRegistry, SolarSystem,
    StargateJump, StationInfo,
};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())).clone()
}

fn make_ctx_with_sde(sde: Arc<SdeRegistry>, base_unix: u64) -> Arc<ServerContext> {
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));
    Arc::new(ServerContext::with_sde_and_repository(sde, db))
}

fn make_ctx() -> Arc<ServerContext> {
    make_ctx_with_sde(shared_sde(), 1_800_000_000)
}

fn make_test_system(sys_id: u32, const_id: u32, sec: f64) -> SolarSystem {
    SolarSystem {
        solar_system_id: sys_id,
        region_id: 10_000_001,
        constellation_id: const_id,
        name: format!("Sys-{sys_id}"),
        security_status: sec,
        radius: 1.0e12,
        galactic_pos_ly: [0.0, 0.0, 0.0],
        planet_ids: Vec::new(),
        stations: Vec::new(),
        wormhole_class_id: None,
    }
}

#[test]
fn wave_meta_serde_defaults_and_npc_combat_state_recharge_overrides() {
    let default_meta: IncursionSiteWaveMeta = serde_json::from_str("{}").unwrap();
    assert_eq!((default_meta.current_wave, default_meta.total_waves), (1, 3));

    let default_lc: IncursionLifecycleMeta = serde_json::from_str("{}").unwrap();
    assert_eq!((default_lc.spawned_at_sec, default_lc.last_site_completed_sec, default_lc.last_regen_sec), (0, 0, 0));
    assert!(default_lc.withdraw_until_sec.is_none() && default_lc.ended_at_sec.is_none());

    let mut sde = SdeRegistry::default();
    sde.types.insert(
        900_001,
        make_fighter_type(
            900_001,
            550,
            11,
            1_000_000.0,
            vec![
                (attr_ids::SHIELD_CAPACITY, 5_000.0),
                (attr_ids::ARMOR_HP, 4_000.0),
                (attr_ids::HP_STRUCT, 3_000.0),
                (attr_ids::SHIELD_RECHARGE_RATE, 250_000.0),
                (attr_ids::RECHARGE_RATE, 180_000.0),
            ],
        ),
    );
    sde.types.insert(
        900_002,
        make_fighter_type(
            900_002,
            550,
            11,
            1_000_000.0,
            vec![(attr_ids::SHIELD_RECHARGE_RATE, 0.0), (attr_ids::RECHARGE_RATE, -10.0)],
        ),
    );

    let cs_pos = build_incursion_npc_combat_state(&sde, 900_001);
    assert_eq!((cs_pos.shield_recharge_time_ms, cs_pos.capacitor_recharge_time_ms), (250_000.0, 180_000.0));

    let cs_zero = build_incursion_npc_combat_state(&sde, 900_002);
    assert_eq!((cs_zero.shield_recharge_time_ms, cs_zero.capacitor_recharge_time_ms), (0.0, -10.0));

    let cs_unknown = build_incursion_npc_combat_state(&sde, 999_999);
    assert!(cs_unknown.max_shield_hp > 0.0);
}

#[test]
fn eligible_constellations_filters_starter_trade_size_pochven_and_security_bounds() {
    let mut sde = SdeRegistry::default();
    let add_const = |sde: &mut SdeRegistry, cid: u32, sys_ids: &[u32], sec: f64| {
        sde.constellations.insert(
            cid,
            ConstellationInfo {
                constellation_id: cid,
                region_id: 10_000_001,
                name: format!("Const-{cid}"),
                center: [0.0; 3],
                faction_id: None,
                wormhole_class_id: None,
            },
        );
        for &sid in sys_ids {
            sde.solar_systems.insert(sid, make_test_system(sid, cid, sec));
        }
    };

    // Excluded constellations: trade hub (20_000_020), W-space (>=21_000_000), <6 systems, >=30_100_000 system,
    // school starter system, bloodline station starter system, avg_sec > 0.85, avg_sec < -1.0
    add_const(&mut sde, 20_000_020, &[30_000_001, 30_000_002, 30_000_003, 30_000_004, 30_000_005, 30_000_006], 0.5);
    add_const(&mut sde, 21_000_000, &[30_000_011, 30_000_012, 30_000_013, 30_000_014, 30_000_015, 30_000_016], 0.5);
    add_const(&mut sde, 20_000_100, &[30_000_021, 30_000_022, 30_000_023, 30_000_024, 30_000_025], 0.5);
    add_const(&mut sde, 20_000_101, &[30_000_031, 30_000_032, 30_000_033, 30_000_034, 30_000_035, 30_100_000], 0.5);
    add_const(&mut sde, 20_000_102, &[30_000_041, 30_000_042, 30_000_043, 30_000_044, 30_000_045, 30_000_046], 0.5);
    sde.schools.insert(
        1,
        SchoolInfo {
            school_id: 1, name: "School".into(), corporation_id: 1_000_044,
            station_id: 60_000_000, solar_system_id: 30_000_041, race_id: 1,
        },
    );
    add_const(&mut sde, 20_000_103, &[30_000_051, 30_000_052, 30_000_053, 30_000_054, 30_000_055, 30_000_056], 0.5);
    sde.bloodlines.insert(
        1,
        BloodlineInfo {
            bloodline_id: 1, name: "Bloodline".into(), race_id: 1, corporation_id: 1_000_006,
            ship_type_id: Some(606), charisma: 20, intelligence: 20, memory: 20,
            perception: 20, willpower: 20, description: String::new(),
        },
    );
    sde.stations.insert(
        60_000_001,
        StationInfo {
            station_id: 60_000_001, solar_system_id: 30_000_051, constellation_id: 20_000_103,
            region_id: 10_000_001, station_type_id: 1529, owner_corporation_id: 1_000_006,
            owner_faction_id: None, operation_id: 0, is_conquerable: false, orbit_id: None,
            has_clone_bay: true, name: "Station".into(), position: glam::DVec3::ZERO,
            reprocessing_efficiency: 0.5, reprocessing_stations_take: 0.05,
        },
    );
    add_const(&mut sde, 20_000_104, &[30_000_061, 30_000_062, 30_000_063, 30_000_064, 30_000_065, 30_000_066], 0.86);
    add_const(&mut sde, 20_000_105, &[30_000_071, 30_000_072, 30_000_073, 30_000_074, 30_000_075, 30_000_076], -1.01);

    // Eligible constellations testing exact boundary (-1.0 and 0.85) and empire priority (0.1..=0.8)
    add_const(&mut sde, 20_000_201, &[30_000_101, 30_000_102, 30_000_103, 30_000_104, 30_000_105, 30_000_106], 0.85);
    add_const(&mut sde, 20_000_202, &[30_000_111, 30_000_112, 30_000_113, 30_000_114, 30_000_115, 30_000_116], -1.0);
    add_const(&mut sde, 20_000_204, &[30_000_121, 30_000_122, 30_000_123, 30_000_124, 30_000_125, 30_000_126], 0.50);
    add_const(&mut sde, 20_000_203, &[30_000_131, 30_000_132, 30_000_133, 30_000_134, 30_000_135, 30_000_136], 0.50);
    add_const(&mut sde, 20_000_205, &[30_000_141, 30_000_142, 30_000_143, 30_000_144, 30_000_145, 30_000_146], 0.80);

    let ctx = make_ctx_with_sde(Arc::new(sde), 1_800_000_000);
    let cids: Vec<u32> = eligible_incursion_constellations(&ctx).iter().map(|&(cid, _)| cid).collect();
    assert_eq!(cids, vec![20_000_205, 20_000_203, 20_000_204, 20_000_201, 20_000_202]);
}

#[test]
fn assign_constellation_system_roles_handles_1_2_3_and_graph_topologies() {
    let mut sde = SdeRegistry::default();
    let cid = 20_000_500;
    for sid in 30_001_001..=30_001_007 {
        sde.solar_systems.insert(sid, make_test_system(sid, cid, 0.5));
    }
    sde.solar_systems.insert(30_002_001, make_test_system(30_002_001, 20_000_501, 0.5));

    let r1 = assign_constellation_system_roles(&sde, cid, &[30_001_001]);
    assert_eq!(r1, (30_001_001, vec![30_001_001], vec![30_001_001], 30_001_001));

    let r2 = assign_constellation_system_roles(&sde, cid, &[30_001_001, 30_001_002]);
    assert_eq!((r2.0, r2.3, r2.1, r2.2), (30_001_001, 30_001_002, vec![30_001_001], vec![30_001_002]));

    let r3 = assign_constellation_system_roles(&sde, cid, &[30_001_001, 30_001_002, 30_001_003]);
    assert_eq!(r3.1.len(), 1);
    assert_eq!(r3.1, r3.2);

    let edges = [
        (1, 30_001_001, 30_002_001),
        (2, 30_001_001, 30_001_002),
        (3, 30_001_002, 30_001_003),
        (4, 30_001_003, 30_001_004),
        (5, 30_001_004, 30_001_005),
        (6, 30_001_005, 30_001_006),
    ];
    for (jid, from_s, to_s) in edges {
        sde.jumps.insert(
            jid,
            StargateJump {
                stargate_id: jid, from_solar_system_id: from_s, to_solar_system_id: to_s,
                destination_stargate_id: jid + 100, position: glam::DVec3::ZERO,
            },
        );
    }
    let sys6 = vec![30_001_001, 30_001_002, 30_001_003, 30_001_004, 30_001_005, 30_001_006];
    let (stg, vg, as_s, hq) = assign_constellation_system_roles(&sde, cid, &sys6);
    assert_eq!((stg, hq), (30_001_001, 30_001_006));
    assert_eq!(vg, vec![30_001_002, 30_001_003]);
    assert_eq!(as_s, vec![30_001_004, 30_001_005]);
}

#[tokio::test]
async fn wave_reinforcements_exact_ids_and_staging_two_wave_progression() {
    let hq_w2 = build_wave_reinforcements(500, "Headquarters", 2);
    assert_eq!(hq_w2.len(), 3);
    assert_eq!((hq_w2[0].npc_ball_id, hq_w2[0].type_id), (500 * 10 + 2 * 20 + 1, 2845));
    assert_eq!((hq_w2[1].npc_ball_id, hq_w2[1].type_id), (500 * 10 + 2 * 20 + 2, 2933));
    assert_eq!((hq_w2[2].npc_ball_id, hq_w2[2].type_id), (500 * 10 + 2 * 20 + 3, 2936));

    let ms_w1 = build_wave_reinforcements(200, "Mothership", 1);
    assert_eq!((ms_w1.len(), ms_w1[0].npc_ball_id), (3, 200 * 10 + 20 + 1));

    let boss_w3 = build_wave_reinforcements(100, "Boss", 3);
    assert_eq!((boss_w3.len(), boss_w3[0].npc_ball_id), (3, 100 * 10 + 60 + 1));

    let as_w2 = build_wave_reinforcements(300, "Assault", 2);
    assert_eq!(as_w2.len(), 2);
    assert_eq!((as_w2[0].npc_ball_id, as_w2[0].type_id), (300 * 10 + 40 + 1, 2933));
    assert_eq!((as_w2[1].npc_ball_id, as_w2[1].type_id), (300 * 10 + 40 + 2, 2859));

    let vg_w1 = build_wave_reinforcements(400, "Staging", 1);
    assert_eq!(vg_w1.len(), 2);
    assert_eq!((vg_w1[0].npc_ball_id, vg_w1[0].type_id), (400 * 10 + 20 + 1, 2936));
    assert_eq!((vg_w1[1].npc_ball_id, vg_w1[1].type_id), (400 * 10 + 20 + 2, 2909));

    let ctx = make_ctx();
    let cid = eligible_incursion_constellations(&ctx)[0].0;
    let inc = with_pve_special_reg(&ctx, |reg| {
        reg.initialized = true;
        seed_incursion_for_constellation(&ctx, reg, cid);
        reg.incursions[&cid].clone()
    });

    let stg_site = spawn_incursion_site_in_role_system(&ctx, cid, inc.staging_solar_system_id, "Staging", 5160, "Staging Scout Site").unwrap();
    assert_eq!(stg_site.influence_delta, 0.05);
    assert!(!stg_site.locked_until_low_influence);

    let as_site = spawn_incursion_site_in_role_system(&ctx, cid, inc.assault_solar_system_ids[0], "Assault", 5170, "Assault Site").unwrap();
    assert_eq!(as_site.influence_delta, 0.20);

    // If boss_destroyed is true, advance_incursion_wave_on_escort_kill is a no-op
    with_pve_special_reg(&ctx, |reg| {
        reg.incursion_site_waves.remove(&stg_site.site_id);
        let s = reg.incursions.get_mut(&cid).unwrap().sites.iter_mut().find(|s| s.site_id == stg_site.site_id).unwrap();
        s.boss_destroyed = true;
        for e in &mut s.escort_spawns {
            e.destroyed = true;
        }
    });
    advance_incursion_wave_on_escort_kill(&ctx.systems, &ctx.db, &ctx.sde, inc.staging_solar_system_id, &stg_site.site_id).await;
    with_pve_special_reg(&ctx, |reg| assert!(!reg.incursion_site_waves.contains_key(&stg_site.site_id)));

    // Clear boss_destroyed -> Staging advances from wave 1 to wave 2 (final wave for Staging) and spawns boss
    with_pve_special_reg(&ctx, |reg| {
        reg.incursions.get_mut(&cid).unwrap().sites.iter_mut().find(|s| s.site_id == stg_site.site_id).unwrap().boss_destroyed = false;
    });
    advance_incursion_wave_on_escort_kill(&ctx.systems, &ctx.db, &ctx.sde, inc.staging_solar_system_id, &stg_site.site_id).await;
    with_pve_special_reg(&ctx, |reg| {
        let wm = &reg.incursion_site_waves[&stg_site.site_id];
        assert_eq!((wm.current_wave, wm.total_waves), (2, 2));
    });
    let stg_handle = ctx.systems.get_or_spawn(inc.staging_solar_system_id);
    assert!(stg_handle.get_ball_snapshot(stg_site.boss_npc_ball_id).await.unwrap().is_some());

    // Calling again at final wave is a no-op
    with_pve_special_reg(&ctx, |reg| {
        for e in &mut reg.incursions.get_mut(&cid).unwrap().sites.iter_mut().find(|s| s.site_id == stg_site.site_id).unwrap().escort_spawns {
            e.destroyed = true;
        }
    });
    advance_incursion_wave_on_escort_kill(&ctx.systems, &ctx.db, &ctx.sde, inc.staging_solar_system_id, &stg_site.site_id).await;
    with_pve_special_reg(&ctx, |reg| assert_eq!(reg.incursion_site_waves[&stg_site.site_id].current_wave, 2));
}

#[test]
fn system_effects_owner_note_fallback_influence_clamping_and_stale_beacon_cleanup() {
    let ctx = make_ctx();
    let cid = eligible_incursion_constellations(&ctx)[0].0;
    let inc = with_pve_special_reg(&ctx, |reg| {
        reg.initialized = true;
        seed_incursion_for_constellation(&ctx, reg, cid);
        reg.incursions.get_mut(&cid).unwrap().influence = 1.5; // Over 1.0 -> clamped to 1.0
        reg.incursions[&cid].clone()
    });

    // Typed pve_special_sites.incursions directly drives compute_incursion_system_effects & clamps influence > 1.0

    let hq_eff = compute_incursion_system_effects(&ctx, inc.hq_solar_system_id).unwrap();
    assert_eq!((hq_eff.influence, hq_eff.effect_type_id), (1.0, INCURSION_EFFECT_BEACON_HQ));
    for p in [
        hq_eff.shield_penalty, hq_eff.armor_penalty, hq_eff.capacitor_penalty,
        hq_eff.damage_penalty, hq_eff.resistance_penalty, hq_eff.bounty_penalty, hq_eff.lp_penalty,
    ] {
        assert!((p - 0.50).abs() < 1e-9);
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            999_001,
            InventoryItemRecord {
                item_id: 999_001, type_id: INCURSION_EFFECT_BEACON_VANGUARD, owner_id: 500_019,
                location_id: i64::from(inc.hq_solar_system_id), flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
            },
        );
        snap.items.insert(
            999_002,
            InventoryItemRecord {
                item_id: 999_002, type_id: INCURSION_EFFECT_BEACON_ASSAULT, owner_id: 500_019,
                location_id: 30_000_142, flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
            },
        );
    });
    sync_incursion_system_effect_beacons(&ctx);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&999_001) && !snap.items.contains_key(&999_002));
        let hq_beacon_id = 1_896_900_000 + i64::from(inc.hq_solar_system_id % 100_000);
        assert_eq!(snap.items[&hq_beacon_id].type_id, INCURSION_EFFECT_BEACON_HQ);
    });

    with_pve_special_reg(&ctx, |reg| reg.incursions.get_mut(&cid).unwrap().influence = 0.0);
    assert!(compute_incursion_system_effects(&ctx, inc.hq_solar_system_id).is_none());

    with_pve_special_reg(&ctx, |reg| {
        let i = reg.incursions.get_mut(&cid).unwrap();
        i.influence = 0.8;
        i.mothership_defeated = true;
    });
    assert!(compute_incursion_system_effects(&ctx, inc.hq_solar_system_id).is_none());
}

#[test]
fn lifecycle_mothership_despawn_boundary_and_respawn_cadence_spawned_gate() {
    let ctx = make_ctx();
    advance_incursion_lifecycles(&ctx);

    let eligible = eligible_incursion_constellations(&ctx);
    let (cid0, cid1, cid2) = (eligible[0].0, eligible[1].0, eligible[2].0);
    let now = ctx.now_unix();

    with_pve_special_reg(&ctx, |reg| {
        reg.initialized = true;
        for cid in [cid0, cid1, cid2] {
            seed_incursion_for_constellation(&ctx, reg, cid);
        }
        let inc0 = reg.incursions.get_mut(&cid0).unwrap();
        inc0.influence = 0.08;
        inc0.mothership_spawned = true;
        inc0.state = "Mobilizing".into();
        reg.incursion_lifecycle.insert(
            cid0,
            IncursionLifecycleMeta { spawned_at_sec: 0, last_site_completed_sec: now, last_regen_sec: now, withdraw_until_sec: None, ended_at_sec: None },
        );
        reg.incursions.get_mut(&cid1).unwrap().state = "Withdrawing".into();
        reg.incursion_lifecycle.insert(
            cid1,
            IncursionLifecycleMeta { spawned_at_sec: now, last_site_completed_sec: now, last_regen_sec: now, withdraw_until_sec: None, ended_at_sec: None },
        );
    });

    ctx.advance_secs(3_600);
    with_pve_special_reg(&ctx, |reg| {
        let inc0 = &reg.incursions[&cid0];
        assert!((inc0.influence - 0.10).abs() < 1e-6 && inc0.mothership_spawned);
        assert_eq!(reg.incursion_lifecycle[&cid0].spawned_at_sec, now + 3_600);
    });

    ctx.advance_secs(10);
    with_pve_special_reg(&ctx, |reg| {
        let inc0 = &reg.incursions[&cid0];
        assert!(inc0.influence > 0.10 && !inc0.mothership_spawned && inc0.state == "Established");
    });

    let cur = ctx.now_unix();
    with_pve_special_reg(&ctx, |reg| {
        reg.incursion_lifecycle.get_mut(&cid2).unwrap().spawned_at_sec = cur + 82_800 + 20_000;
    });
    ctx.advance_secs(82_800);
    with_pve_special_reg(&ctx, |reg| assert_eq!(reg.incursions[&cid1].state, "Ended"));

    ctx.advance_secs(INCURSION_RESPAWN_CADENCE_SEC);
    with_pve_special_reg(&ctx, |reg| {
        assert_eq!(reg.incursions.values().filter(|i| i.state != "Ended").count(), 2);
    });
    ctx.advance_secs(20_000);
    with_pve_special_reg(&ctx, |reg| {
        assert_eq!(reg.incursions.values().filter(|i| i.state != "Ended").count(), TARGET_ACTIVE_INCURSIONS);
    });
}

#[test]
fn world_event_tales_all_branches_and_idempotent_stop_notice() {
    let ctx = make_ctx();
    let cid = eligible_incursion_constellations(&ctx)[0].0;
    let inc = with_pve_special_reg(&ctx, |reg: &mut PveSpecialSitesRegistry| {
        reg.initialized = true;
        seed_incursion_for_constellation(&ctx, reg, cid);
        reg.triglavian_minor_victory_systems.push(30_000_163);
        reg.triglavian_sites.insert(
            30_000_163,
            vec![TriglavianSiteRecord {
                instance_id: 1, dungeon_id: 6001, dungeon_name: "Conduit".into(),
                solar_system_id: 30_000_163, faction_owner_id: 500_026, base_isk: 1_000_000,
                survey_db_drop: 10, completed: false, boss_npc_ball_id: 700_001, boss_type_id: 47_956,
                escort_ball_ids: vec![700_002], escort_type_ids: vec![47_951], beacon_ball_id: 700_000,
            }],
        );
        reg.roaming_storms.push(RoamingStormRecord {
            storm_id: 2, weather: AbyssalWeatherKind::Electrical, center_solar_system_id: 30_000_142,
            strong_core_systems: vec![30_000_142], weak_periphery_systems: Vec::new(),
            step_count: 0, next_move_at_sec: 0, recent_centers: vec![30_000_142],
        });
        reg.incursions[&cid].clone()
    });

    let explicit_id = register_world_event_tale(
        &ctx,
        WorldEventTaleRecord {
            tale_id: 777_888, template_id: 6001, template_class_id: "invasion".into(),
            solar_system_id: 30_000_142, constellation_id: 20_000_020, aggressor_faction_id: 500_026,
            state: "Ended".into(), influence: 0.0, start_unix: 1_800_000_000, end_unix: 1_800_003_600,
        },
    );
    assert_eq!(explicit_id, 777_888);
    assert!(collect_all_world_event_tales(&ctx, Some(6001)).is_empty());

    ctx.quasar_gateway.progression.extra.active_tales.write().unwrap().insert(42_001, (6002, 30_000_142, 1_800_000_000));
    let quasar_tales = collect_all_world_event_tales(&ctx, Some(6002));
    assert_eq!(quasar_tales.len(), 1);
    assert_eq!(quasar_tales[0].dict_get("taleID"), Some(&PyRep::Integer(42_001)));

    let trig_tales = collect_all_world_event_tales(&ctx, Some(5201));
    assert_eq!(trig_tales[0].dict_get("state"), Some(&PyRep::String("TriglavianInvasionActive".into())));

    let mut res = IncursionCompletionResult {
        site_id: inc.sites[0].site_id.clone(), tier: "Vanguard".into(), fleet_size: 1,
        fleet_scale_factor: 1.0, isk_per_pilot: 10_000_000, concord_lp_per_pilot: 1_400,
        influence: 0.95, state: "Established".into(), mothership_spawned: false, incursion_ended: false,
    };
    publish_incursion_tale_stopped_if_ended(&ctx.quasar_gateway, &ctx.db, &res);
    with_pve_special_reg(&ctx, |reg| assert!(!reg.incursions[&cid].tale_stop_published));
    res.incursion_ended = true;
    publish_incursion_tale_stopped_if_ended(&ctx.quasar_gateway, &ctx.db, &res);
    publish_incursion_tale_stopped_if_ended(&ctx.quasar_gateway, &ctx.db, &res);
    with_pve_special_reg(&ctx, |reg| assert!(reg.incursions[&cid].tale_stop_published));
}
