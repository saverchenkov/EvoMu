//! Unit tests for Sansha Incursion lifecycle, role assignment, system DBuffs,
//! wave reinforcements, withdrawal/respawn cadence, and world event tales (`pve_incursion_lifecycle`).

use super::*;
use crate::router::ServerContext;
use crate::services::pve_incursion_sites::{
    seed_incursion_for_constellation, IncursionCompletionResult,
};
use crate::services::pve_metaliminal_storms::RoamingStormRecord;
use crate::services::pve_special_sites::{with_pve_special_reg, AbyssalWeatherKind};
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed()))
        .clone()
}

fn build_test_ctx() -> Arc<ServerContext> {
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    Arc::new(ServerContext::with_sde_and_repository(shared_sde(), db))
}

#[test]
fn test_eligible_constellations_and_role_assignment() {
    let ctx = build_test_ctx();
    let eligible = eligible_incursion_constellations(&ctx);
    assert!(eligible.len() >= 3);
    // Trade hub Jita constellation (20_000_020) is excluded
    assert!(!eligible.iter().any(|&(cid, _)| cid == 20_000_020));

    let (first_cid, avg_sec) = eligible[0];
    assert!(first_cid < 21_000_000);
    assert!((-1.0..=0.85).contains(&avg_sec));

    // Empty system slice fallback
    let empty_roles = assign_constellation_system_roles(&ctx.sde, first_cid, &[]);
    assert_eq!(
        empty_roles,
        (30_000_140, vec![30_000_140], vec![30_000_140], 30_000_140)
    );

    // 3-system slice
    let small_roles =
        assign_constellation_system_roles(&ctx.sde, first_cid, &[30_000_140, 30_000_141, 30_000_142]);
    assert_eq!(small_roles.1.len(), 1);
    assert_eq!(small_roles.2.len(), 1);

    // Real constellation role assignment
    let mut sys_list: Vec<SolarSystemId> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.constellation_id == first_cid)
        .map(|s| s.solar_system_id)
        .collect();
    sys_list.sort_unstable();
    assert!(sys_list.len() >= 6);
    let (stg, vg, as_s, hq) = assign_constellation_system_roles(&ctx.sde, first_cid, &sys_list);
    assert_ne!(stg, hq);
    assert!(!vg.is_empty());
    assert!(!as_s.is_empty());
    assert!(!vg.contains(&stg) && !vg.contains(&hq));
    assert!(!as_s.contains(&stg) && !as_s.contains(&hq));
    for v in &vg {
        assert!(!as_s.contains(v));
    }
}

#[test]
fn test_validate_site_role_and_spawn_in_role_system() {
    let ctx = build_test_ctx();
    let cid = eligible_incursion_constellations(&ctx)[0].0;

    // Missing incursion -> IncursionNotFound
    let err = spawn_incursion_site_in_role_system(
        &ctx,
        cid,
        30_000_142,
        "Vanguard",
        5165,
        "Nation Mining Colony",
    )
    .unwrap_err();
    assert_eq!(err.code, "IncursionNotFound");

    let inc = with_pve_special_reg(&ctx, |reg| {
        seed_incursion_for_constellation(&ctx, reg, cid);
        reg.incursions.get(&cid).cloned().unwrap()
    });

    assert!(validate_site_role_for_system(&inc, inc.staging_solar_system_id, "Staging").is_ok());
    assert!(validate_site_role_for_system(&inc, inc.vanguard_solar_system_ids[0], "Vanguard").is_ok());
    assert!(validate_site_role_for_system(&inc, inc.assault_solar_system_ids[0], "Assault").is_ok());
    assert!(validate_site_role_for_system(&inc, inc.hq_solar_system_id, "Headquarters").is_ok());
    assert!(validate_site_role_for_system(&inc, inc.hq_solar_system_id, "Mothership").is_ok());
    assert!(validate_site_role_for_system(&inc, inc.hq_solar_system_id, "Boss").is_ok());
    assert_eq!(
        validate_site_role_for_system(&inc, inc.staging_solar_system_id, "Headquarters")
            .unwrap_err()
            .code,
        "InvalidIncursionSiteSystem"
    );
    assert_eq!(
        validate_site_role_for_system(&inc, inc.hq_solar_system_id, "")
            .unwrap_err()
            .code,
        "InvalidIncursionSiteSystem"
    );

    assert_eq!(
        spawn_incursion_site_in_role_system(
            &ctx,
            cid,
            inc.staging_solar_system_id,
            "",
            9999,
            "Invalid"
        )
        .unwrap_err()
        .code,
        "InvalidIncursionSiteClass"
    );
    assert_eq!(
        spawn_incursion_site_in_role_system(
            &ctx,
            cid,
            inc.staging_solar_system_id,
            "Headquarters",
            5175,
            "Wrong Sys"
        )
        .unwrap_err()
        .code,
        "InvalidIncursionSiteSystem"
    );

    let spawned_hq = spawn_incursion_site_in_role_system(
        &ctx,
        cid,
        inc.hq_solar_system_id,
        "Headquarters",
        5199,
        "Test HQ",
    )
    .unwrap();
    assert_eq!(spawned_hq.influence_delta, 0.25);
    assert!(!spawned_hq.locked_until_low_influence);

    let spawned_ms = spawn_incursion_site_in_role_system(
        &ctx,
        cid,
        inc.hq_solar_system_id,
        "Mothership",
        5198,
        "Test MS",
    )
    .unwrap();
    assert_eq!(spawned_ms.influence_delta, 1.0);
    assert!(spawned_ms.locked_until_low_influence);

    with_pve_special_reg(&ctx, |reg| {
        assert_eq!(reg.incursion_site_waves[&spawned_hq.site_id].total_waves, 3);
        assert_eq!(reg.incursion_site_waves[&spawned_ms.site_id].total_waves, 1);
    });
}

#[test]
fn test_incursion_system_effects_dbuffs_and_beacon_sync() {
    let ctx = build_test_ctx();
    let cid = eligible_incursion_constellations(&ctx)[0].0;
    let inc = with_pve_special_reg(&ctx, |reg| {
        seed_incursion_for_constellation(&ctx, reg, cid);
        reg.incursions.get(&cid).cloned().unwrap()
    });
    sync_incursion_system_effect_beacons(&ctx);

    let stg_eff = compute_incursion_system_effects(&ctx, inc.staging_solar_system_id).unwrap();
    assert_eq!(stg_eff.effect_type_id, INCURSION_EFFECT_BEACON_VANGUARD);
    assert!((stg_eff.resistance_penalty - 0.10).abs() < 1e-6);
    assert!(stg_eff.cyno_jammed);

    let vg_eff = compute_incursion_system_effects(&ctx, inc.vanguard_solar_system_ids[0]).unwrap();
    assert_eq!(vg_eff.effect_type_id, INCURSION_EFFECT_BEACON_VANGUARD);

    let as_eff = compute_incursion_system_effects(&ctx, inc.assault_solar_system_ids[0]).unwrap();
    assert_eq!(as_eff.effect_type_id, INCURSION_EFFECT_BEACON_ASSAULT);
    assert!((as_eff.damage_penalty - 0.25).abs() < 1e-6);

    let hq_eff = compute_incursion_system_effects(&ctx, inc.hq_solar_system_id).unwrap();
    assert_eq!(hq_eff.effect_type_id, INCURSION_EFFECT_BEACON_HQ);
    assert!((hq_eff.shield_penalty - 0.50).abs() < 1e-6);
    assert_eq!(
        get_incursion_system_wide_dbuffs(&ctx, inc.hq_solar_system_id),
        vec![(10, 50.0), (13, 50.0)]
    );

    // Non-incursion system has no effects
    assert!(compute_incursion_system_effects(&ctx, 30_000_142).is_none());
    assert!(get_incursion_system_wide_dbuffs(&ctx, 30_000_142).is_empty());

    // Verify beacons exist in snap.items
    let beacon_count = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                matches!(
                    i.type_id,
                    INCURSION_EFFECT_BEACON_VANGUARD
                        | INCURSION_EFFECT_BEACON_ASSAULT
                        | INCURSION_EFFECT_BEACON_HQ
                )
            })
            .count()
    });
    assert!(beacon_count >= 4);

    // Scale influence to 0.40 -> HQ dbuff becomes 20.0
    with_pve_special_reg(&ctx, |reg| {
        reg.incursions.get_mut(&cid).unwrap().influence = 0.40;
    });
    let dbuffs_scaled = get_incursion_system_wide_dbuffs(&ctx, inc.hq_solar_system_id);
    assert_eq!(dbuffs_scaled.len(), 2);
    assert!((dbuffs_scaled[0].1 - 20.0).abs() < 1e-6);

    // Low influence before Mothership unlocks still jams cynos
    with_pve_special_reg(&ctx, |reg| {
        let i = reg.incursions.get_mut(&cid).unwrap();
        i.influence = 0.05;
        i.state = "Established".into();
        i.mothership_spawned = false;
    });
    assert!(compute_incursion_system_effects(&ctx, inc.hq_solar_system_id).unwrap().cyno_jammed);

    // Mobilizing with slight regen (<= 0.10) keeps cyno jammer dropped; > 0.10 re-engages it
    with_pve_special_reg(&ctx, |reg| {
        let i = reg.incursions.get_mut(&cid).unwrap();
        i.influence = 0.02;
        i.state = "Mobilizing".into();
        i.mothership_spawned = true;
    });
    assert!(!compute_incursion_system_effects(&ctx, inc.hq_solar_system_id).unwrap().cyno_jammed);

    with_pve_special_reg(&ctx, |reg| {
        reg.incursions.get_mut(&cid).unwrap().influence = 0.12;
    });
    assert!(compute_incursion_system_effects(&ctx, inc.hq_solar_system_id).unwrap().cyno_jammed);

    // Withdrawing removes effects and beacons
    with_pve_special_reg(&ctx, |reg| {
        reg.incursions.get_mut(&cid).unwrap().state = "Withdrawing".into();
    });
    sync_incursion_system_effect_beacons(&ctx);
    assert!(compute_incursion_system_effects(&ctx, inc.hq_solar_system_id).is_none());
    assert!(get_incursion_system_wide_dbuffs(&ctx, inc.hq_solar_system_id).is_empty());
    let beacon_count_after = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                matches!(
                    i.type_id,
                    INCURSION_EFFECT_BEACON_VANGUARD
                        | INCURSION_EFFECT_BEACON_ASSAULT
                        | INCURSION_EFFECT_BEACON_HQ
                )
            })
            .count()
    });
    assert_eq!(beacon_count_after, 0);
}

#[tokio::test]
async fn test_wave_reinforcements_regen_withdrawal_and_respawn_cadence() {
    let ctx = build_test_ctx();
    let eligible = eligible_incursion_constellations(&ctx);
    let cid0 = eligible[0].0;
    let cid1 = eligible[1].0;
    let cid2 = eligible[2].0;

    // Seed 3 active incursions (TARGET_ACTIVE_INCURSIONS = 3)
    with_pve_special_reg(&ctx, |reg| {
        reg.initialized = true;
        for cid in [cid0, cid1, cid2] {
            seed_incursion_for_constellation(&ctx, reg, cid);
        }
    });

    // Test build_wave_reinforcements across tiers
    assert_eq!(build_wave_reinforcements(1000, "Headquarters", 2).len(), 3);
    assert_eq!(build_wave_reinforcements(1000, "Assault", 2).len(), 2);
    assert_eq!(build_wave_reinforcements(1000, "Vanguard", 2).len(), 2);

    // Test advance_incursion_wave_on_escort_kill on Vanguard site (3 waves)
    let (vg_sys, vg_site_id, escorts_w1) = with_pve_special_reg(&ctx, |reg| {
        let s = reg.incursions[&cid0]
            .sites
            .iter()
            .find(|s| s.tier == "Vanguard")
            .unwrap();
        (s.solar_system_id, s.site_id.clone(), s.escort_spawns.clone())
    });

    // If not all escorts are destroyed, wave does not advance
    with_pve_special_reg(&ctx, |reg| {
        let s = reg
            .incursions
            .get_mut(&cid0)
            .unwrap()
            .sites
            .iter_mut()
            .find(|s| s.site_id == vg_site_id)
            .unwrap();
        s.escort_spawns[0].destroyed = true;
    });
    advance_incursion_wave_on_escort_kill(&ctx.systems, &ctx.db, &ctx.sde, vg_sys, &vg_site_id)
        .await;
    with_pve_special_reg(&ctx, |reg| {
        assert_eq!(reg.incursion_site_waves[&vg_site_id].current_wave, 1);
    });

    // Destroy all remaining wave-1 escorts -> advances to wave 2 + spawns 2 reinforcements
    with_pve_special_reg(&ctx, |reg| {
        let s = reg
            .incursions
            .get_mut(&cid0)
            .unwrap()
            .sites
            .iter_mut()
            .find(|s| s.site_id == vg_site_id)
            .unwrap();
        for e in &mut s.escort_spawns {
            e.destroyed = true;
        }
    });
    advance_incursion_wave_on_escort_kill(&ctx.systems, &ctx.db, &ctx.sde, vg_sys, &vg_site_id)
        .await;
    let escorts_w2 = with_pve_special_reg(&ctx, |reg| {
        assert_eq!(reg.incursion_site_waves[&vg_site_id].current_wave, 2);
        let s = reg.incursions[&cid0]
            .sites
            .iter()
            .find(|s| s.site_id == vg_site_id)
            .unwrap();
        assert_eq!(s.escort_spawns.len(), escorts_w1.len() + 2);
        s.escort_spawns.clone()
    });
    let sys_handle = ctx.systems.get_or_spawn(vg_sys);
    for e in escorts_w2.iter().skip(escorts_w1.len()) {
        assert!(sys_handle
            .get_ball_snapshot(e.npc_ball_id)
            .await
            .unwrap()
            .is_some());
    }

    // Destroy wave-2 escorts -> advances to wave 3 (final wave) + spawns boss ball!
    with_pve_special_reg(&ctx, |reg| {
        let s = reg
            .incursions
            .get_mut(&cid0)
            .unwrap()
            .sites
            .iter_mut()
            .find(|s| s.site_id == vg_site_id)
            .unwrap();
        for e in &mut s.escort_spawns {
            e.destroyed = true;
        }
    });
    advance_incursion_wave_on_escort_kill(&ctx.systems, &ctx.db, &ctx.sde, vg_sys, &vg_site_id)
        .await;
    let boss_bid = with_pve_special_reg(&ctx, |reg| {
        assert_eq!(reg.incursion_site_waves[&vg_site_id].current_wave, 3);
        reg.incursions[&cid0]
            .sites
            .iter()
            .find(|s| s.site_id == vg_site_id)
            .unwrap()
            .boss_npc_ball_id
    });
    assert!(sys_handle.get_ball_snapshot(boss_bid).await.unwrap().is_some());

    // Influence regeneration (+2% per hour) + mothership despawn when influence rises > 0.10
    with_pve_special_reg(&ctx, |reg| {
        let inc = reg.incursions.get_mut(&cid0).unwrap();
        inc.influence = 0.09;
        inc.mothership_spawned = true;
        inc.state = "Mobilizing".into();
    });
    ctx.advance_secs(3_600);
    with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&cid0];
        assert!((inc.influence - 0.11).abs() < 1e-6);
        assert!(!inc.mothership_spawned);
        assert_eq!(inc.state, "Established");
    });

    // Withdrawal timer (24h = 86,400s) and respawn cadence (12h = 43,200s)
    let now = ctx.now_unix();
    with_pve_special_reg(&ctx, |reg| {
        let inc = reg.incursions.get_mut(&cid1).unwrap();
        inc.state = "Withdrawing".into();
        inc.influence = 0.0;
        let meta = reg.incursion_lifecycle.get_mut(&cid1).unwrap();
        meta.withdraw_until_sec = Some(now + INCURSION_WITHDRAWAL_DURATION_SEC);
    });
    ctx.advance_secs(INCURSION_WITHDRAWAL_DURATION_SEC - 10);
    with_pve_special_reg(&ctx, |reg| {
        assert_eq!(reg.incursions[&cid1].state, "Withdrawing");
    });
    ctx.advance_secs(10);
    with_pve_special_reg(&ctx, |reg| {
        assert_eq!(reg.incursions[&cid1].state, "Ended");
        assert!(reg.incursion_lifecycle[&cid1].ended_at_sec.is_some());
        let active = reg.incursions.values().filter(|i| i.state != "Ended").count();
        assert_eq!(active, 2);
    });

    // Before respawn cadence (43,200s), active count stays 2
    ctx.advance_secs(INCURSION_RESPAWN_CADENCE_SEC - 100);
    with_pve_special_reg(&ctx, |reg| {
        let active = reg.incursions.values().filter(|i| i.state != "Ended").count();
        assert_eq!(active, 2);
    });

    // Once respawn cadence elapses, a 3rd active incursion spawns
    ctx.advance_secs(100);
    with_pve_special_reg(&ctx, |reg| {
        let active = reg.incursions.values().filter(|i| i.state != "Ended").count();
        assert_eq!(active, TARGET_ACTIVE_INCURSIONS);
    });
}

#[test]
fn test_world_event_tales_collection_and_notifications() {
    let ctx = build_test_ctx();
    let cid = eligible_incursion_constellations(&ctx)[0].0;
    let inc = with_pve_special_reg(&ctx, |reg| {
        reg.initialized = true;
        seed_incursion_for_constellation(&ctx, reg, cid);
        reg.triglavian_minor_victory_systems.push(30_000_163);
        reg.edencom_fortress_systems.push(30_000_004);
        reg.edencom_minor_victory_systems.push(30_000_012);
        reg.roaming_storms.push(RoamingStormRecord {
            storm_id: 1,
            weather: AbyssalWeatherKind::Gamma,
            center_solar_system_id: 30_000_142,
            strong_core_systems: vec![30_000_142],
            weak_periphery_systems: Vec::new(),
            step_count: 0,
            next_move_at_sec: ctx.now_unix() + 7_200,
            recent_centers: vec![30_000_142],
        });
        reg.incursions[&cid].clone()
    });

    let custom_tid = register_world_event_tale(
        &ctx,
        WorldEventTaleRecord {
            tale_id: 0,
            template_id: 5999,
            template_class_id: "customCampaign".into(),
            solar_system_id: inc.staging_solar_system_id,
            constellation_id: cid,
            aggressor_faction_id: 500_019,
            state: "Active".into(),
            influence: 0.75,
            start_unix: ctx.now_unix(),
            end_unix: ctx.now_unix() + 3_600,
        },
    );
    assert!(custom_tid > 900_000);

    let all_tales = collect_all_world_event_tales(&ctx, None);
    assert!(all_tales.len() >= 5);
    for tale in &all_tales {
        assert!(
            tale.dict_get("templateNameID").and_then(|v| v.as_i64()).is_some(),
            "world event tale missing templateNameID (#639): {tale:?}"
        );
        assert!(
            tale.dict_get("aggressorFactionID").and_then(|v| v.as_i64()).is_some(),
            "world event tale missing aggressorFactionID (#639): {tale:?}"
        );
    }

    let inc_only =
        collect_all_world_event_tales(&ctx, Some(INCURSION_TALE_TEMPLATE_ID as i64));
    assert_eq!(inc_only.len(), 1);
    assert_eq!(
        inc_only[0].dict_get("taleID").and_then(|v| v.as_i64()),
        Some(inc.tale_id)
    );

    let trig_only = collect_all_world_event_tales(&ctx, Some(5201));
    assert_eq!(trig_only.len(), 1);

    let eden_only = collect_all_world_event_tales(&ctx, Some(5202));
    assert_eq!(eden_only.len(), 2);

    let storm_only = collect_all_world_event_tales(&ctx, Some(STORM_TALE_TEMPLATE_ID));
    assert_eq!(storm_only.len(), 1);

    let custom_only = collect_all_world_event_tales(&ctx, Some(5999));
    assert_eq!(custom_only.len(), 1);

    // Online character in staging system receives tale data & completion notifications
    let char_id = 91_000_055;
    ctx.db
        .provision_gm_account("tale_pilot", "pw", char_id, "TalePilot");
    let uid = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.user_id).unwrap_or(1));
    let _ = ctx.db.acquire_online_lock_with_computer_hash(
        uid,
        55,
        Some(char_id),
        None,
        true,
        ctx.now_unix(),
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = inc.staging_solar_system_id;
            ch.constellation_id = cid;
        }
    });

    emit_active_incursion_tale_data_for_system(&ctx, inc.staging_solar_system_id);
    let comp_res = IncursionCompletionResult {
        site_id: inc.sites[0].site_id.clone(),
        tier: "Mothership".into(),
        fleet_size: 1,
        fleet_scale_factor: 1.0,
        isk_per_pilot: 63_000_000,
        concord_lp_per_pilot: 14_000,
        influence: 0.0,
        state: "Withdrawing".into(),
        mothership_spawned: true,
        incursion_ended: true,
    };
    notify_incursion_site_completed(&ctx.quasar_gateway, &ctx.db, inc.hq_solar_system_id, &comp_res);
    emit_incursion_completion_macho_notifications(&ctx, &comp_res);

    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.len() >= 2);
    with_pve_special_reg(&ctx, |reg| {
        assert!(reg.incursions[&cid].tale_stop_published);
    });
}
