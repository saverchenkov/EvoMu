//! Part 5 of FW & Insurgency closure tests:
//! - Per-stage Corruption (1–5) & Suppression (1–5) security modifications (`securityMgr.get_modified_systems`),
//!   stage-gated site spawning/pruning, NPC/sentry effects, and dynamic removal on stage drop (`feat:fw.corruption-suppression-effects`).
//! - Battlefield capture speed scaling by Advantage, combat-capable wave NPCs, Social skill standing scaling (`0` vs `5`),
//!   30-pilot standing cap, and VP/Advantage shifts (`feat:fw.battlefield-capture-rules`).

#![forbid(unsafe_code)]

use crate::fw_warzone_insurgency_closure::setup_warzone_chain;
use crate::sovereignty_equinox_gameplay_closure::{make_ball, register_online_pilot, rpc_call};
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_npc_police::{
    is_concord_suppressed_in_system, is_faction_police_suppressed, is_sentry_disabled_by_corruption,
};
use eve_server::services::combat_crimewatch::step_sentry_gun_grid_aggression;
use eve_server::services::fw_gameplay::{
    BATTLEFIELD_MAX_FULL_PAYOUT_PILOTS, BATTLEFIELD_MAX_PILOT_LP, BATTLEFIELD_ZONE_CENTERS,
};
use eve_server::services::insurgency_services::{
    add_system_corruption, add_system_suppression, FORECASTING_DURATION_SECS,
};
use glam::DVec3;

fn lookup_modified_sec(rep: &PyRep, sys_id: u32) -> Option<f64> {
    let PyRep::Dict(entries) = rep else {
        panic!("expected Dict from securityMgr.get_modified_systems, got {rep:?}");
    };
    entries.iter().find_map(|(k, v)| match (k, v) {
        (PyRep::Integer(id), PyRep::Real(sec)) if *id == i64::from(sys_id) => Some(*sec),
        _ => None,
    })
}

fn active_sites_in_sys(ctx: &ServerContext, sys_id: u32) -> Vec<String> {
    ctx.db.with_snapshot(|s| {
        s.insurgency_runtime
            .insurgency_instances
            .iter()
            .filter(|i| i.solar_system_id == sys_id && !i.completed)
            .map(|i| i.site_type.clone())
            .collect()
    })
}

fn expected_sec(base_sec: f64, delta: f64) -> f64 {
    (((base_sec + delta).clamp(0.0, 1.0) * 10_000.0).round()) / 10_000.0
}

fn fast_forward_secs(ctx: &ServerContext, secs: u64) {
    ctx.clock().advance_secs(secs);
    let cur = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert("__server_now_unix".into(), cur.to_string());
    });
}

#[tokio::test]
async fn test_corruption_and_suppression_per_stage_effects() {
    // parity: feat:fw.corruption-suppression-effects
    eve_testkit::attest(&["feat:fw.corruption-suppression-effects"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let emp_cid = 90_000_051i64;
    let emp_ship = 100_000_051i64;
    let pir_cid = 90_000_052i64;
    let pir_ship = 100_000_052i64;

    let mut s_emp = register_online_pilot(
        &ctx, 85_001, "demo", "demo-secret", emp_cid, 98_000_051, None,
        front_sys, emp_ship, 639, DVec3::new(50_000.0, 0.0, 0.0), 0,
    ).await;
    let mut s_pir = register_online_pilot(
        &ctx, 85_002, "demo2", "demo2-secret", pir_cid, 98_000_052, None,
        front_sys, pir_ship, 621, DVec3::new(60_000.0, 0.0, 0.0), 0,
    ).await;

    rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)])
        .await.expect("enlist Caldari");
    rpc_call(&ctx, &mut s_pir, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)])
        .await.expect("enlist Guristas");

    // Seed campaign and advance past 48h Forecasting into 7d Active phase.
    let _ = rpc_call(&ctx, &mut s_emp, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let camp_id = ctx.db.with_snapshot(|s| *s.insurgency_runtime.campaigns.keys().next().unwrap());
    fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let base_sec = ctx.sde.solar_systems[&front_sys].security_status;
    let mod0 = rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap();
    assert_eq!(lookup_modified_sec(&mod0, front_sys), None, "Stage 0 system has no security modification");

    // 1. Corruption Stages 1..=5: security degradation, police/sentry/CONCORD suppression, and stage-gated sites.
    let corr_deltas = [(1u8, -0.02), (2, -0.05), (3, -0.10), (4, -0.15)];
    for (stage, delta) in corr_deltas {
        let (_pts, cur_st) = add_system_corruption(&ctx, camp_id, front_sys, 200);
        assert_eq!(cur_st, stage);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

        let mod_rep = rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap();
        assert_eq!(
            lookup_modified_sec(&mod_rep, front_sys),
            Some(expected_sec(base_sec, delta)),
            "Corruption stage {stage} applies {delta} security modification"
        );
        assert_eq!(is_faction_police_suppressed(&ctx, front_sys), stage >= 2);
        assert_eq!(is_sentry_disabled_by_corruption(&ctx, front_sys), stage >= 3);

        let sites = active_sites_in_sys(&ctx, front_sys);
        assert!(sites.contains(&"PirateMiningAmbush".into()));
        assert_eq!(sites.contains(&"IceHeist".into()), stage >= 2);
        assert_eq!(sites.contains(&"LawlessOutpost".into()), stage >= 3);
        assert_eq!(sites.contains(&"PirateRoamer".into()), stage >= 4);
    }

    // Stage 3+ sentry gun disabling verified on grid aggression:
    let sys_h = ctx.systems.get_or_spawn(front_sys);
    let _ = sys_h
        .spawn_ball_with_combat(make_ball(pir_ship, 621, DVec3::ZERO), ctx.build_type_combat_state(621))
        .await;
    assert_eq!(
        step_sentry_gun_grid_aggression(&ctx, front_sys, pir_cid, pir_ship, DVec3::ZERO, true).await,
        (false, 0.0),
        "Sentry guns remain offline in Corruption Stage 4 low-sec"
    );
    let _ = sys_h.remove_ball(pir_ship).await;

    // Corruption Stage 5: lawless 0.0 security override and CONCORD suppression:
    let (_pts5, st5) = add_system_corruption(&ctx, camp_id, front_sys, 200);
    assert_eq!(st5, 5);
    let mod_c5 = rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap();
    assert_eq!(lookup_modified_sec(&mod_c5, front_sys), Some(0.0), "Corruption stage 5 sets lawless 0.0 security");
    assert!(is_concord_suppressed_in_system(&ctx, front_sys), "Corruption stage 5 suppresses CONCORD");

    // 2. Dynamic pruning & security restoration when Corruption stages drop:
    let (_pts_drop3, st_drop3) = add_system_corruption(&ctx, camp_id, front_sys, -500);
    assert_eq!(st_drop3, 3);
    let sites_drop3 = active_sites_in_sys(&ctx, front_sys);
    assert!(!sites_drop3.contains(&"PirateRoamer".into()), "Dropping below Stage 4 prunes PirateRoamer");
    assert!(sites_drop3.contains(&"LawlessOutpost".into()), "Stage 3 retains LawlessOutpost");
    let mod_drop3 = rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap();
    assert_eq!(lookup_modified_sec(&mod_drop3, front_sys), Some(expected_sec(base_sec, -0.10)));

    let (_pts_drop0, st_drop0) = add_system_corruption(&ctx, camp_id, front_sys, -500);
    assert_eq!(st_drop0, 0);
    let sites_drop0 = active_sites_in_sys(&ctx, front_sys);
    assert!(!sites_drop0.contains(&"PirateMiningAmbush".into()));
    assert!(!sites_drop0.contains(&"IceHeist".into()));
    assert!(!sites_drop0.contains(&"LawlessOutpost".into()));
    let mod_drop0 = rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap();
    assert_eq!(lookup_modified_sec(&mod_drop0, front_sys), None, "Stage 0 removes system from modified_systems");

    // Measure baseline Stage-0 sentry damage for comparison with Suppression Stage 2+:
    let base_probe_ship = 100_000_058i64;
    let _ = sys_h
        .spawn_ball_with_combat(make_ball(base_probe_ship, 621, DVec3::ZERO), ctx.build_type_combat_state(621))
        .await;
    let (eng_s0, dmg_s0) = step_sentry_gun_grid_aggression(&ctx, front_sys, pir_cid, base_probe_ship, DVec3::ZERO, true).await;
    assert!(eng_s0 && dmg_s0 > 0.0, "Baseline Stage 0 sentry guns engage in low-sec");
    let _ = sys_h.remove_ball(base_probe_ship).await;

    // 3. Suppression Stages 1..=5: security reinforcement, enhanced sentry damage, 30% corruption reduction,
    //    Counter-Insurgency Sentry Guns, corruption cap below Stage 5, and stage-gated sites.
    let supp_deltas = [(1u8, 0.02), (2, 0.04), (3, 0.06), (4, 0.08), (5, 0.10)];
    for (stage, delta) in supp_deltas {
        let (_pts, cur_st) = add_system_suppression(&ctx, camp_id, front_sys, 200);
        assert_eq!(cur_st, stage);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

        let mod_rep = rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap();
        assert_eq!(
            lookup_modified_sec(&mod_rep, front_sys),
            Some(expected_sec(base_sec, delta)),
            "Suppression stage {stage} applies +{delta} security reinforcement"
        );

        let sites = active_sites_in_sys(&ctx, front_sys);
        assert!(sites.contains(&"LawEnforcementPatrol".into()));
        assert_eq!(sites.contains(&"CounterInsurgencyOutpost".into()), stage >= 2);
        assert_eq!(sites.contains(&"AntiPiratePatrol".into()), stage >= 3);
        assert_eq!(sites.contains(&"HeavyLawPatrol".into()), stage >= 4);
        assert_eq!(sites.contains(&"CounterInsurgencySentry".into()), stage >= 5);

        if stage == 2 {
            let probe_ship = 100_000_059i64;
            let _ = sys_h
                .spawn_ball_with_combat(make_ball(probe_ship, 621, DVec3::ZERO), ctx.build_type_combat_state(621))
                .await;
            let (eng_s2, dmg_s2) = step_sentry_gun_grid_aggression(&ctx, front_sys, pir_cid, probe_ship, DVec3::ZERO, true).await;
            assert!(eng_s2 && dmg_s2 > dmg_s0, "Suppression Stage 2 increases sentry gun damage ({dmg_s2} > {dmg_s0})");
            assert!(((dmg_s2 / dmg_s0) - (105.0 / 88.0)).abs() < 1e-6);
        }
        if stage == 3 {
            let (c_pts_reduced, _) = add_system_corruption(&ctx, camp_id, front_sys, 100);
            assert_eq!(c_pts_reduced, 70, "Suppression Stage 3 reduces corruption gain by 30%");
            let _ = add_system_corruption(&ctx, camp_id, front_sys, -100);
        }
    }

    // At Suppression Stage 5: corruption is capped below Stage 5 (800 pts) and Counter-Insurgency Sentry Guns
    // keep sentries active even when Corruption reaches Stage 4!
    let (capped_c_pts, capped_c_stage) = add_system_corruption(&ctx, camp_id, front_sys, 2_000);
    assert_eq!((capped_c_pts, capped_c_stage), (800, 4), "Suppression Stage 5 caps corruption at Stage 4 (800 pts)");
    assert!(!is_sentry_disabled_by_corruption(&ctx, front_sys), "Counter-Insurgency Sentries stay online at Suppression Stage 5");
    let ci_probe_ship = 100_000_060i64;
    let _ = sys_h
        .spawn_ball_with_combat(make_ball(ci_probe_ship, 621, DVec3::ZERO), ctx.build_type_combat_state(621))
        .await;
    let (eng_s5, dmg_s5) = step_sentry_gun_grid_aggression(&ctx, front_sys, pir_cid, ci_probe_ship, DVec3::ZERO, true).await;
    assert!(eng_s5 && dmg_s5 > dmg_s0, "Counter-Insurgency Sentry Guns fire at boosted damage");

    // 4. Dynamic pruning when Suppression drops:
    let _ = add_system_corruption(&ctx, camp_id, front_sys, -10_000);
    let (_s_drop2_pts, s_drop2) = add_system_suppression(&ctx, camp_id, front_sys, -600);
    assert_eq!(s_drop2, 2);
    let sites_s2 = active_sites_in_sys(&ctx, front_sys);
    assert!(!sites_s2.contains(&"AntiPiratePatrol".into()));
    assert!(!sites_s2.contains(&"HeavyLawPatrol".into()));
    assert!(!sites_s2.contains(&"CounterInsurgencySentry".into()));
    assert!(sites_s2.contains(&"LawEnforcementPatrol".into()));
    assert!(sites_s2.contains(&"CounterInsurgencyOutpost".into()));

    let (_s_drop0_pts, s_drop0) = add_system_suppression(&ctx, camp_id, front_sys, -400);
    assert_eq!(s_drop0, 0);
    let sites_s0 = active_sites_in_sys(&ctx, front_sys);
    assert!(!sites_s0.contains(&"LawEnforcementPatrol".into()));
    assert!(!sites_s0.contains(&"CounterInsurgencyOutpost".into()));
    let mod_s0 = rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap();
    assert_eq!(lookup_modified_sec(&mod_s0, front_sys), None, "Suppression Stage 0 removes system from modified_systems");
}

#[tokio::test]
async fn test_battlefield_capture_social_standing_and_advantage_rules() {
    // parity: feat:fw.battlefield-capture-rules
    eve_testkit::attest(&["feat:fw.battlefield-capture-rules"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let gal_cid = 90_000_061i64;
    let gal_ship = 100_000_061i64;
    let mut s_gal = register_online_pilot(
        &ctx, 86_001, "demo", "demo-secret", gal_cid, 98_000_061, None,
        front_sys, gal_ship, 621, DVec3::from_array(BATTLEFIELD_ZONE_CENTERS[0]), 0,
    ).await;
    rpc_call(&ctx, &mut s_gal, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_004)])
        .await.expect("enlist Gallente");

    // Seed Battlefield instance in front_sys.
    let _ = rpc_call(&ctx, &mut s_gal, "dungeonInstanceCacheMgr", "GetFactionWarfareBattlefieldInstances", &[])
        .await.unwrap();
    let bf_id = ctx.db.with_snapshot(|s| {
        s.fw_runtime
            .battlefield_instances
            .iter()
            .find(|b| b.solar_system_id == front_sys && !b.completed)
            .expect("frontline battlefield")
            .instance_id
    });

    // 1. Capture rate scales with warzone Advantage (`calculate_capture_speed_multiplier`) and spawns combat-capable wave NPCs:
    ctx.advance_secs(90);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let (prog_neutral, wave_npcs) = ctx.db.with_snapshot(|s| {
        let bf = s.fw_runtime.battlefield_instances.iter().find(|b| b.instance_id == bf_id).unwrap();
        (
            bf.capture_progress,
            s.fw_runtime.battlefield_npc_waves.get(&bf_id).cloned().unwrap_or_default(),
        )
    });
    assert!((prog_neutral - 10.0).abs() < 1e-6, "1 zone held for 90s at 0% advantage advances 10.0% (got {prog_neutral})");
    assert_eq!(wave_npcs.len(), 6, "Battlefield spawns 6 combat-capable wave NPCs");
    let npc_cs = ctx.systems.get_or_spawn(front_sys).get_combat_state(wave_npcs[0]).await.unwrap().expect("wave NPC CombatState");
    assert!(npc_cs.shield_hp > 0.0 && npc_cs.structure_hp > 0.0, "Wave NPCs are combat-capable");

    // Grant Gallente +50% Advantage (`speed_mult = 1.25`) -> 90s tick advances +12.5%!
    ctx.db.with_mut_snapshot(|s| {
        s.fw_runtime.system_advantage.insert(front_sys, (0.0, 50.0));
    });
    ctx.advance_secs(90);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let prog_adv = ctx.db.with_snapshot(|s| {
        s.fw_runtime.battlefield_instances.iter().find(|b| b.instance_id == bf_id).unwrap().capture_progress
    });
    let adv_delta = prog_adv - prog_neutral;
    assert!((adv_delta - 12.5).abs() < 1e-6, "50% Advantage scales capture speed by 1.25x to 12.5%/90s (got {adv_delta})");

    // 2. Social skill standing scaling (`0` vs `5`), 30-pilot standing cap (32 pilots on grid), LP scaling, & VP/Advantage shifts:
    // Give `gal_cid` (pilot #1) Social V (type_id 3355, level 5).
    // Spawn 31 wingmen (`90_200_000..90_200_031`):
    // - Wingman 0 (`90_200_000`, pilot #2) has Social 0.
    // - Wingmen 29 (`90_200_029`, pilot #31) & 30 (`90_200_030`, pilot #32) have Social 5, testing the 30-pilot cap!
    let mut all_cids = vec![gal_cid];
    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(gal_cid).or_default().insert(3355, TrainedSkillRecord {
            type_id: 3355,
            level: 5,
            skill_points: 256_000.0,
        });
        let base_ch = s.characters[&gal_cid].clone();
        for idx in 0..31i64 {
            let wcid = 90_200_000 + idx;
            let wship = 100_200_000 + idx;
            all_cids.push(wcid);
            let mut wch = base_ch.clone();
            wch.character_id = wcid;
            wch.name = format!("BF Pilot {idx}");
            wch.solar_system_id = front_sys;
            wch.active_ship_id = wship;
            wch.war_faction_id = Some(500_004);
            wch.loyalty_points = Default::default();
            wch.standings = Default::default();
            s.characters.insert(wcid, wch);
            s.items.insert(
                wship,
                InventoryItemRecord {
                    item_id: wship,
                    type_id: 621,
                    owner_id: wcid,
                    location_id: front_sys as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            if idx >= 29 {
                s.skills.entry(wcid).or_default().insert(3355, TrainedSkillRecord {
                    type_id: 3355,
                    level: 5,
                    skill_points: 256_000.0,
                });
            }
        }
    });
    let sys_h = ctx.systems.get_or_spawn(front_sys);
    for idx in 0..31i64 {
        let wship = 100_200_000 + idx;
        let zc = DVec3::from_array(BATTLEFIELD_ZONE_CENTERS[(idx as usize) % 3]);
        let _ = sys_h
            .spawn_ball(make_ball(wship, 621, zc + DVec3::new(400.0, 0.0, 0.0)))
            .await;
    }

    // Holding all 3 zones at 1.25x speed advances 37.5% per 90s; 3 ticks complete the remaining 77.5%!
    for _ in 0..3 {
        ctx.advance_secs(90);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    }

    let expected_lp = (BATTLEFIELD_MAX_PILOT_LP * (BATTLEFIELD_MAX_FULL_PAYOUT_PILOTS as i64)) / 32;
    let (gal_fac_st, gal_corp_st, w0_fac_st, w0_corp_st, w29_fac_st, w30_fac_st, st_recipients, vp_after, adv_after) =
        ctx.db.with_snapshot(|s| {
            let gch = &s.characters[&gal_cid];
            let w0 = &s.characters[&90_200_000];
            let w29 = &s.characters[&90_200_029];
            let w30 = &s.characters[&90_200_030];
            let recipients = all_cids
                .iter()
                .filter(|&&cid| s.characters[&cid].standings.get(&500_004).copied().unwrap_or(0.0) > 0.0)
                .count();
            for &cid in &all_cids {
                let lp = s.characters[&cid].loyalty_points.get(&1_000_181).copied().unwrap_or(0);
                assert_eq!(lp, expected_lp, "Every pilot in 32-pilot fleet receives scaled LP {expected_lp}");
            }
            (
                gch.standings.get(&500_004).copied().unwrap_or(0.0),
                gch.standings.get(&1_000_181).copied().unwrap_or(0.0),
                w0.standings.get(&500_004).copied().unwrap_or(0.0),
                w0.standings.get(&1_000_181).copied().unwrap_or(0.0),
                w29.standings.get(&500_004).copied().unwrap_or(0.0),
                w30.standings.get(&500_004).copied().unwrap_or(0.0),
                recipients,
                s.fw_system_occupations[&front_sys].victory_points,
                s.fw_runtime.system_advantage[&front_sys],
            )
        });

    assert_eq!(st_recipients, 30, "Standing payout is strictly capped at 30 winning pilots");
    assert!((gal_fac_st - 0.01875).abs() < 1e-6, "Social 5 pilot receives +25% scaled faction standing (got {gal_fac_st})");
    assert!((gal_corp_st - 0.01875).abs() < 1e-6, "Social 5 pilot receives +25% scaled corp standing (got {gal_corp_st})");
    assert!((w0_fac_st - 0.015).abs() < 1e-6, "Social 0 pilot receives base 0.15% faction standing (got {w0_fac_st})");
    assert!((w0_corp_st - 0.015).abs() < 1e-6, "Social 0 pilot receives base 0.15% corp standing (got {w0_corp_st})");
    assert_eq!(w29_fac_st, 0.0, "31st pilot receives no standing gain beyond 30-pilot cap");
    assert_eq!(w30_fac_st, 0.0, "32nd pilot receives no standing gain beyond 30-pilot cap");
    assert_eq!(vp_after, 2_000, "Attacker Battlefield victory shifts contested VP by +2,000");
    assert_eq!(adv_after, (0.0, 56.0), "Attacker Battlefield victory shifts system Advantage by +6.0%");
}
