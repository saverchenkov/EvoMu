//! Part 5 of FW & Insurgency closure tests:
//! - Published per-stage Corruption (1–5) & Suppression (1–5) mechanics, `securityMgr.get_modified_systems`
//!   (LAWLESS Stage 5 + manual overrides), documented insurgency sites, roaming Pirate/Enforcer NPCs,
//!   Counter-Insurgency Sentry Guns (`79507`), non-retroactive Stage 5 corruption cap, and post-campaign persistence
//!   (`feat:fw.corruption-suppression-effects`).
//! - Battlefield capture speed scaling by Advantage, combat-capable wave NPCs, Social skill standing scaling (`0` vs `5`),
//!   time-on-grid ranked 30-pilot standing cap, and VP/Advantage shifts (`feat:fw.battlefield-capture-rules`).

#![forbid(unsafe_code)]

use crate::fw_warzone_insurgency_closure::setup_warzone_chain;
use crate::sovereignty_equinox_gameplay_closure::{make_ball, register_online_pilot, rpc_call};
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{step_sentry_gun_grid_aggression, with_combat_runtime};
use eve_server::services::combat_npc_police::{
    is_concord_suppressed_in_system, is_faction_police_suppressed, is_sentry_disabled_by_corruption,
};
use eve_server::services::fw_gameplay::{
    BATTLEFIELD_MAX_FULL_PAYOUT_PILOTS, BATTLEFIELD_MAX_PILOT_LP, BATTLEFIELD_ZONE_CENTERS,
};
use eve_server::services::insurgency_services::{
    add_system_corruption, add_system_suppression, FORECASTING_DURATION_SECS, ACTIVE_DURATION_SECS,
    WINDDOWN_DURATION_SECS,
};
use eve_server::services::security_mgr::{
    insurgency_pvp_loot_drop_threshold, insurgency_suppression_bounty_and_sec_multiplier,
    insurgency_suppression_empire_lp_multiplier, insurgency_suppression_web_scram_range_multiplier,
};
use eve_server::services::space_destiny::{compute_pilot_warp_speed_au_s, compute_ship_warp_speed_au_s};
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
    let (enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

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
    let (camp_id, fob_sys) = ctx.db.with_snapshot(|s| {
        let (&cid, c) = s.insurgency_runtime.campaigns.iter().next().unwrap();
        (cid, c.fob_solar_system_id)
    });
    fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    assert!(active_sites_in_sys(&ctx, front_sys).contains(&"CorporateOutpostRaid".into()));
    assert!(active_sites_in_sys(&ctx, fob_sys).contains(&"PirateLaunderingFacility".into()));
    assert!(active_sites_in_sys(&ctx, 30_045_324).contains(&"CounterInsurgencyOutpost".into()), "War HQ Onnamon has CounterInsurgencyOutpost");
    let mod0 = rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap();
    assert_eq!(lookup_modified_sec(&mod0, front_sys), None, "Stage 0 system has no security modification");

    // 1. Corruption Stages 1..=5: PvP loot drop bonus (+7.5%/stage), Faction Police suppression (2+),
    //    documented sites, roaming Pirate NPCs (2+), +2 AU/s pirate warp speed (4+), and Stage 5 LAWLESS.
    let base_pir_warp = compute_ship_warp_speed_au_s(&ctx, 621);
    for stage in 1u8..=5 {
        let (_pts, cur_st) = add_system_corruption(&ctx, camp_id, front_sys, 200);
        assert_eq!(cur_st, stage);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

        let mod_rep = rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap();
        assert_eq!(
            lookup_modified_sec(&mod_rep, front_sys),
            if stage >= 5 { Some(0.0) } else { None },
            "Only Corruption Stage 5 overrides system security to 0.0 LAWLESS"
        );

        let drop_p = insurgency_pvp_loot_drop_threshold(&ctx, front_sys, pir_cid, emp_cid, false);
        assert!((drop_p - (0.50 + 0.075 * f64::from(stage))).abs() < 1e-6);
        assert!((insurgency_pvp_loot_drop_threshold(&ctx, front_sys, emp_cid, pir_cid, false) - 0.50).abs() < 1e-6);

        assert_eq!(is_faction_police_suppressed(&ctx, front_sys), stage >= 2);
        assert_eq!(is_sentry_disabled_by_corruption(&ctx, front_sys), stage >= 5);
        assert_eq!(is_concord_suppressed_in_system(&ctx, front_sys), stage >= 5);

        let sites = active_sites_in_sys(&ctx, front_sys);
        assert!(sites.contains(&"CorporateOutpostRaid".into()));
        assert!(sites.contains(&"MiningOpAmbush".into()));
        assert_eq!(sites.contains(&"IceRefineryHeist".into()), (3..=4).contains(&stage));

        let pirate_roamer_tid = with_combat_runtime(&ctx, |rt| {
            rt.npc_entities.values().find_map(|n| {
                let is_pir = ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 4637);
                (n.solar_system_id == front_sys && is_pir).then_some(n.type_id)
            })
        });
        assert_eq!(pirate_roamer_tid.is_some(), stage >= 2, "Roaming pirates spawn at Corruption 2+");

        let expected_warp = if stage >= 4 { base_pir_warp + 2.0 } else { base_pir_warp };
        assert!((compute_pilot_warp_speed_au_s(&ctx, pir_cid, front_sys, 621) - expected_warp).abs() < 1e-6);
        assert!((compute_pilot_warp_speed_au_s(&ctx, emp_cid, front_sys, 621) - base_pir_warp).abs() < 1e-6);
    }

    // Stage 5 lowsec sentry disabling verified on grid aggression:
    let sys_h = ctx.systems.get_or_spawn(front_sys);
    let _ = sys_h
        .spawn_ball_with_combat(make_ball(pir_ship, 621, DVec3::ZERO), ctx.build_type_combat_state(621))
        .await;
    assert_eq!(
        step_sentry_gun_grid_aggression(&ctx, front_sys, pir_cid, pir_ship, DVec3::ZERO, true).await,
        (false, 0.0),
        "Low-sec sentry guns cease firing at Corruption Stage 5"
    );
    let _ = sys_h.remove_ball(pir_ship).await;

    // 2. Dynamic pruning & restoration when Corruption drops:
    let (_pts_drop3, st_drop3) = add_system_corruption(&ctx, camp_id, front_sys, -500);
    assert_eq!(st_drop3, 3);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(active_sites_in_sys(&ctx, front_sys).contains(&"IceRefineryHeist".into()), "Stage 3 restores IceRefineryHeist");
    let mod_drop3 = rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap();
    assert_eq!(lookup_modified_sec(&mod_drop3, front_sys), None, "Dropping below Stage 5 restores normal security");

    let (_pts_drop0, st_drop0) = add_system_corruption(&ctx, camp_id, front_sys, -500);
    assert_eq!(st_drop0, 0);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let sites_drop0 = active_sites_in_sys(&ctx, front_sys);
    assert!(sites_drop0.contains(&"CorporateOutpostRaid".into()));
    assert!(!sites_drop0.contains(&"MiningOpAmbush".into()));
    assert!(!sites_drop0.contains(&"IceRefineryHeist".into()));

    // Measure baseline Stage-0 sentry damage for comparison with Counter-Insurgency Sentry Guns:
    let base_probe_ship = 100_000_058i64;
    let _ = sys_h
        .spawn_ball_with_combat(make_ball(base_probe_ship, 621, DVec3::ZERO), ctx.build_type_combat_state(621))
        .await;
    let (eng_s0, dmg_s0) = step_sentry_gun_grid_aggression(&ctx, front_sys, pir_cid, base_probe_ship, DVec3::ZERO, true).await;
    assert!(eng_s0 && dmg_s0 > 0.0, "Baseline Stage 0 sentry guns engage in low-sec");
    let _ = sys_h.remove_ball(base_probe_ship).await;

    // 3. Suppression Stages 1..=5: +5%/stage NPC bounty & sec gain, +5%/stage Empire LP (2+),
    //    -30% corruption gain & roaming Enforcers (3+), +10% web/scram range (4+), and Counter-Insurgency Sentries (5).
    for stage in 1u8..=5 {
        let (_pts, cur_st) = add_system_suppression(&ctx, camp_id, front_sys, 200);
        assert_eq!(cur_st, stage);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

        let mod_rep = rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap();
        assert_eq!(lookup_modified_sec(&mod_rep, front_sys), None, "Suppression does not alter securityMgr.get_modified_systems");

        assert!((insurgency_suppression_bounty_and_sec_multiplier(&ctx, front_sys) - (1.0 + 0.05 * f64::from(stage))).abs() < 1e-6);
        let expected_lp_mult = if stage >= 2 { 1.0 + 0.05 * f64::from(stage - 1) } else { 1.0 };
        assert!((insurgency_suppression_empire_lp_multiplier(&ctx, front_sys, 500_001) - expected_lp_mult).abs() < 1e-6);
        assert!((insurgency_suppression_empire_lp_multiplier(&ctx, front_sys, 500_010) - 1.0).abs() < 1e-6);

        let enforcer_exists = with_combat_runtime(&ctx, |rt| {
            rt.npc_entities.values().any(|n| {
                n.solar_system_id == front_sys && ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 4638)
            })
        });
        assert_eq!(enforcer_exists, stage >= 3, "Roaming Enforcer NPCs spawn at Suppression 3+");

        let expected_range_mult = if stage >= 4 { 1.10 } else { 1.0 };
        assert!((insurgency_suppression_web_scram_range_multiplier(&ctx, emp_cid, front_sys) - expected_range_mult).abs() < 1e-6);
        assert!((insurgency_suppression_web_scram_range_multiplier(&ctx, pir_cid, front_sys) - 1.0).abs() < 1e-6);

        if stage == 3 {
            let (c_pts_reduced, _) = add_system_corruption(&ctx, camp_id, front_sys, 100);
            assert_eq!(c_pts_reduced, 70, "Suppression Stage 3 reduces corruption gain by 30%");
            let _ = add_system_corruption(&ctx, camp_id, front_sys, -100);
        }
    }

    // At Suppression Stage 5:
    // (a) Corruption below Stage 5 is capped at 800 pts (Stage 4), while Counter-Insurgency Sentry Guns (79507)
    //     stay active, deal higher volley damage, target paint (+50% sig), and ECM jam!
    let (capped_c_pts, capped_c_stage) = add_system_corruption(&ctx, camp_id, front_sys, 2_000);
    assert_eq!((capped_c_pts, capped_c_stage), (800, 4), "Suppression Stage 5 caps corruption at Stage 4 (800 pts)");
    assert!(!is_sentry_disabled_by_corruption(&ctx, front_sys), "Counter-Insurgency Sentries stay online at Suppression Stage 5");
    let ci_probe_ship = 100_000_060i64;
    let base_cs = ctx.build_type_combat_state(621);
    let base_sig = base_cs.signature_radius;
    let _ = sys_h
        .spawn_ball_with_combat(make_ball(ci_probe_ship, 621, DVec3::new(10_000.0, 0.0, 0.0)), base_cs)
        .await;
    let (eng_s5, dmg_s5) = step_sentry_gun_grid_aggression(&ctx, front_sys, pir_cid, ci_probe_ship, DVec3::ZERO, true).await;
    assert!(eng_s5 && dmg_s5 > dmg_s0, "Counter-Insurgency Sentry Guns fire at higher SDE volley damage ({dmg_s5} > {dmg_s0})");
    let painted_cs = sys_h.get_combat_state(ci_probe_ship).await.unwrap().unwrap();
    assert!((painted_cs.signature_radius - base_sig * 1.50).abs() < 1e-3, "Counter-Insurgency Sentry Guns target paint (+50% sig)");
    assert!((painted_cs.ecm_jammed_remaining_s - 5.0).abs() < 1e-6, "Counter-Insurgency Sentry Guns apply 5.0s ECM jam within 75 km");

    // (b) Non-retroactive Stage 5 corruption cap: if `enemy_sys` is ALREADY at Corruption Stage 5 (`1,000` pts)
    //     before reaching Suppression Stage 5, it is NOT clamped down to 800!
    let (_e_cpts, e_cstage) = add_system_corruption(&ctx, camp_id, enemy_sys, 1_000);
    assert_eq!((_e_cpts, e_cstage), (1_000, 5));
    let (_e_spts, e_sstage) = add_system_suppression(&ctx, camp_id, enemy_sys, 1_000);
    assert_eq!((_e_spts, e_sstage), (1_000, 5));
    let (e_after_pts, e_after_stage) = add_system_corruption(&ctx, camp_id, enemy_sys, 100);
    assert_eq!(e_after_stage, 5, "Suppression Stage 5 must not retroactively demote an existing Stage 5 system");
    assert!(e_after_pts >= 1_000);

    // (c) When the Insurgency campaign ends, roaming NPCs and sites are cleaned up while Counter-Insurgency
    //     Sentry Guns (`79507`) persist in space until destroyed!
    fast_forward_secs(&ctx, ACTIVE_DURATION_SECS + WINDDOWN_DURATION_SECS + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let (ci_sentry_persists, roamers_left) = with_combat_runtime(&ctx, |rt| {
        let ci = rt.npc_entities.values().any(|n| n.solar_system_id == front_sys && n.type_id == 79_507);
        let r = rt.npc_entities.values().any(|n| {
            n.solar_system_id == front_sys && ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 4637 || t.group_id == 4638)
        });
        (ci, r)
    });
    assert!(ci_sentry_persists, "Counter-Insurgency Sentry Guns remain after insurgency ends until destroyed");
    assert!(!roamers_left, "Roaming Pirate/Enforcer NPCs despawn when insurgency ends");
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
    let vet_cid = 99_999_999i64;
    let vet_ship = 109_999_999i64;
    let mut s_gal = register_online_pilot(
        &ctx, 86_001, "demo", "demo-secret", gal_cid, 98_000_061, None,
        front_sys, gal_ship, 621, DVec3::from_array(BATTLEFIELD_ZONE_CENTERS[0]), 0,
    ).await;
    rpc_call(&ctx, &mut s_gal, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_004)])
        .await.expect("enlist Gallente");

    // Seed Battlefield instance in front_sys and spawn high-ID veteran (`99_999_999`) from tick 1.
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
    ctx.db.with_mut_snapshot(|s| {
        let mut vch = s.characters[&gal_cid].clone();
        vch.character_id = vet_cid;
        vch.name = "Veteran High-ID Pilot".into();
        vch.active_ship_id = vet_ship;
        vch.war_faction_id = Some(500_004);
        vch.loyalty_points = Default::default();
        vch.standings = Default::default();
        s.characters.insert(vet_cid, vch);
        s.items.insert(
            vet_ship,
            InventoryItemRecord {
                item_id: vet_ship,
                type_id: 621,
                owner_id: vet_cid,
                location_id: front_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let sys_h = ctx.systems.get_or_spawn(front_sys);
    let _ = sys_h
        .spawn_ball(make_ball(vet_ship, 621, DVec3::from_array(BATTLEFIELD_ZONE_CENTERS[0]) + DVec3::new(300.0, 0.0, 0.0)))
        .await;

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
    let npc_cs = sys_h.get_combat_state(wave_npcs[0]).await.unwrap().expect("wave NPC CombatState");
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

    // 2. Social skill standing scaling (`0` vs `5`), contribution-ranked 30-pilot standing cap (32 pilots on grid),
    //    LP scaling, & VP/Advantage shifts:
    // - `gal_cid` (`450s` on grid) has Social V (type_id 3355, level 5).
    // - `vet_cid` (`99_999_999`, `450s` on grid, highest character_id!) has Social 0.
    // - 30 latecomer wingmen (`90_200_000..90_200_030`, `270s` on grid) arrive on tick 3, including Social V on #28 & #29.
    //   Because standing recipients are ranked by cumulative time contributed on grid (`450s` > `270s`), `vet_cid`
    //   (`99_999_999`) MUST receive standing while the last 2 latecomers (`90_200_028` & `90_200_029`) are excluded!
    let mut all_cids = vec![gal_cid, vet_cid];
    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(gal_cid).or_default().insert(3355, TrainedSkillRecord {
            type_id: 3355,
            level: 5,
            skill_points: 256_000.0,
        });
        let base_ch = s.characters[&gal_cid].clone();
        for idx in 0..30i64 {
            let wcid = 90_200_000 + idx;
            let wship = 100_200_000 + idx;
            all_cids.push(wcid);
            let mut wch = base_ch.clone();
            wch.character_id = wcid;
            wch.name = format!("BF Latecomer {idx}");
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
            if idx >= 28 {
                s.skills.entry(wcid).or_default().insert(3355, TrainedSkillRecord {
                    type_id: 3355,
                    level: 5,
                    skill_points: 256_000.0,
                });
            }
        }
    });
    for idx in 0..30i64 {
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
    let (gal_fac_st, gal_corp_st, vet_fac_st, vet_corp_st, w28_fac_st, w29_fac_st, st_recipients, vp_after, adv_after) =
        ctx.db.with_snapshot(|s| {
            let gch = &s.characters[&gal_cid];
            let vch = &s.characters[&vet_cid];
            let w28 = &s.characters[&90_200_028];
            let w29 = &s.characters[&90_200_029];
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
                vch.standings.get(&500_004).copied().unwrap_or(0.0),
                vch.standings.get(&1_000_181).copied().unwrap_or(0.0),
                w28.standings.get(&500_004).copied().unwrap_or(0.0),
                w29.standings.get(&500_004).copied().unwrap_or(0.0),
                recipients,
                s.fw_system_occupations[&front_sys].victory_points,
                s.fw_runtime.system_advantage[&front_sys],
            )
        });

    assert_eq!(st_recipients, 30, "Standing payout is strictly capped at 30 winning pilots");
    assert!((gal_fac_st - 0.01875).abs() < 1e-6, "Social 5 veteran receives +25% scaled faction standing (got {gal_fac_st})");
    assert!((gal_corp_st - 0.01875).abs() < 1e-6, "Social 5 veteran receives +25% scaled corp standing (got {gal_corp_st})");
    assert!((vet_fac_st - 0.015).abs() < 1e-6, "High-ID Social 0 veteran (450s on grid) receives base standing (got {vet_fac_st})");
    assert!((vet_corp_st - 0.015).abs() < 1e-6, "High-ID Social 0 veteran (450s on grid) receives base corp standing (got {vet_corp_st})");
    assert_eq!(w28_fac_st, 0.0, "31st pilot by contribution receives no standing gain beyond 30-pilot cap");
    assert_eq!(w29_fac_st, 0.0, "32nd pilot by contribution receives no standing gain beyond 30-pilot cap");
    assert_eq!(vp_after, 2_000, "Attacker Battlefield victory shifts contested VP by +2,000");
    assert_eq!(adv_after, (0.0, 56.0), "Attacker Battlefield victory shifts system Advantage by +6.0%");
}

#[tokio::test]
async fn test_insurgency_live_gameplay_and_highsec_stage5() {
    // parity: feat:fw.corruption-suppression-effects
    eve_testkit::attest(&["feat:fw.corruption-suppression-effects"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let emp_cid = 90_000_071i64;
    let emp_ship = 100_000_071i64;
    let pir_cid = 90_000_072i64;
    let pir_ship = 100_000_072i64;
    let vic_cid = 90_000_073i64;
    let vic_ship = 100_000_073i64;
    let hs_sys = 30_045_324u32; // Onnamon (0.62 Highsec)
    assert!(ctx.sde.solar_systems.get(&hs_sys).unwrap().security_status >= 0.45);
    let helper_cid = 90_000_077i64;
    let helper_ship = 100_000_077i64;
    let crim_cid = 90_000_078i64;
    let crim_ship = 100_000_078i64;

    let mut s_emp = register_online_pilot(
        &ctx, 87_001, "demo", "demo-secret", emp_cid, 98_000_071, None,
        front_sys, emp_ship, 639, DVec3::ZERO, 0,
    ).await;
    let mut s_pir = register_online_pilot(
        &ctx, 87_002, "demo2", "demo2-secret", pir_cid, 98_000_072, None,
        front_sys, pir_ship, 621, DVec3::new(1_000.0, 0.0, 0.0), 0,
    ).await;
    ctx.db.with_mut_snapshot(|s| {
        let base_ch = s.characters[&emp_cid].clone();
        for (cid, sid, sys, name) in [
            (vic_cid, vic_ship, front_sys, "Insurgency Victim"),
            (helper_cid, helper_ship, hs_sys, "Highsec Helper"),
            (crim_cid, crim_ship, hs_sys, "Highsec Criminal"),
        ] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.name = name.into();
            ch.solar_system_id = sys;
            ch.active_ship_id = sid;
            ch.war_faction_id = None;
            s.characters.insert(cid, ch);
            s.items.insert(sid, InventoryItemRecord {
                item_id: sid, type_id: 621, owner_id: cid, location_id: sys as i64,
                flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
            });
        }
    });
    let sys_h = ctx.systems.get_or_spawn(front_sys);
    let _ = sys_h.spawn_ball(make_ball(vic_ship, 621, DVec3::new(8_000.0, 0.0, 0.0))).await;

    rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)])
        .await.expect("enlist Caldari");
    rpc_call(&ctx, &mut s_pir, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)])
        .await.expect("enlist Guristas");

    // Seed campaign and place it into Active phase covering `front_sys` and `hs_sys`.
    let _ = rpc_call(&ctx, &mut s_emp, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let now_u = ctx.now_unix();
    let camp_id = ctx.db.with_mut_snapshot(|s| {
        let (&cid, c) = s.insurgency_runtime.campaigns.iter_mut().next().unwrap();
        c.forecasting_end_unix = now_u.saturating_sub(1);
        if !c.systems.contains(&hs_sys) {
            c.systems.push(hs_sys);
        }
        cid
    });

    // 1. Live PvP loot drop via `execute_death_pipeline_sync`:
    //    For `iid = 77_000_001` and `rng_seed = (iid as u64) ^ 2`, deterministic roll is `~0.690` (`0.50 < 0.690 < 0.875`).
    //    At Corruption Stage 0, `iid` is destroyed; at Corruption Stage 5 (`0.875`), `iid` drops into the wreck!
    let loot_iid = 77_000_001i64;
    let rng_seed = (loot_iid as u64) ^ 2;
    let stage0_ship = 100_000_074i64;
    let stage5_ship = 100_000_075i64;
    ctx.db.with_mut_snapshot(|s| {
        for (sid, iid) in [(stage0_ship, loot_iid), (stage5_ship, loot_iid)] {
            s.items.insert(sid, InventoryItemRecord {
                item_id: sid, type_id: 621, owner_id: vic_cid, location_id: front_sys as i64,
                flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
            });
            if sid == stage0_ship {
                s.items.insert(iid, InventoryItemRecord {
                    item_id: iid, type_id: 34, owner_id: vic_cid, location_id: sid,
                    flag: InventoryFlag::Cargo, quantity: 10, singleton: false, ..Default::default()
                });
            }
        }
    });
    let res_s0 = eve_server::combat::death_pipeline::execute_death_pipeline_sync(
        &ctx, front_sys, vic_cid, stage0_ship, pir_cid, DVec3::ZERO, rng_seed,
    ).expect("stage0 death pipeline");
    assert!(res_s0.loot_dropped_item_ids.is_empty(), "Roll 0.690 > 0.50 destroys cargo at Corruption Stage 0");

    let (_cpts, cstage) = add_system_corruption(&ctx, camp_id, front_sys, 1_000);
    assert_eq!(cstage, 5);
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(loot_iid, InventoryItemRecord {
            item_id: loot_iid, type_id: 34, owner_id: vic_cid, location_id: stage5_ship,
            flag: InventoryFlag::Cargo, quantity: 10, singleton: false, ..Default::default()
        });
    });
    let res_s5 = eve_server::combat::death_pipeline::execute_death_pipeline_sync(
        &ctx, front_sys, vic_cid, stage5_ship, pir_cid, DVec3::ZERO, rng_seed,
    ).expect("stage5 death pipeline");
    assert_eq!(res_s5.loot_dropped_item_ids, vec![loot_iid], "Roll 0.690 < 0.875 drops cargo at Corruption Stage 5");

    // 2. Live `+2.0 AU/s` Pirate warp speed via `beyonce.CmdWarpToStuff` at Corruption Stage 4+:
    let base_pir_warp = compute_ship_warp_speed_au_s(&ctx, 621);
    let warp_dest_id = ctx
        .sde
        .celestials_in_system(front_sys)
        .into_iter()
        .find(|c| c.position.distance(DVec3::new(1_000.0, 0.0, 0.0)) >= 200_000.0)
        .expect("celestial >= 200 km from pirate ship")
        .celestial_id;
    let _ = rpc_call(
        &ctx,
        &mut s_pir,
        "beyonce",
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(warp_dest_id), PyRep::Real(0.0)],
    )
    .await
    .expect("CmdWarpToStuff in Stage 5 system");
    let (_, pir_ball_after) = sys_h.get_ball_snapshot(pir_ship).await.unwrap().unwrap();
    match pir_ball_after.mode {
        eve_destiny::MovementMode::Warp { warp_speed_au_s, .. } => {
            assert!((warp_speed_au_s - (base_pir_warp + 2.0)).abs() < 1e-6, "Pirate warp speed +2.0 AU/s (got {warp_speed_au_s})");
        }
        other => panic!("expected MovementMode::Warp, got {other:?}"),
    }

    // 3. Live Highsec Stage 5 Criminal Assist (`Corruption_and_Suppression.md:55`):
    //    Direct ship attack in Highsec Stage 5 suppresses CONCORD, whereas assisting a Criminal still triggers CONCORD!
    let (_hs_pts, hs_stage) = add_system_corruption(&ctx, camp_id, hs_sys, 1_000);
    assert_eq!(hs_stage, 5);
    assert!(is_concord_suppressed_in_system(&ctx, hs_sys), "Highsec Stage 5 suppresses CONCORD for direct ship attacks");
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.get_or_create(crim_cid).criminal_flag_until_unix = now_u + 900;
    }
    eve_server::services::combat_crimewatch::evaluate_and_record_remote_assistance(
        &ctx, helper_cid, helper_ship, hs_sys, crim_cid,
    ).expect("remote assist evaluation");
    let (helper_is_crim, concord_scheduled) = {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.get_or_create(helper_cid);
        (st.is_criminal(now_u), st.pending_concord_arrival.is_some())
    };
    assert!(helper_is_crim, "Assisting a criminal in Highsec Stage 5 grants Criminal flag");
    assert!(concord_scheduled, "Assisting a criminal in Highsec Stage 5 still triggers CONCORD response");

    // 4. Live Suppression Stage 4 `+10%` Warp Scrambler optimal range (`7500m -> 8250m`) via `dogmaIM.Activate`:
    let scram_id = 60_000_071i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(scram_id, InventoryItemRecord {
            item_id: scram_id, type_id: 447, owner_id: emp_cid, location_id: emp_ship,
            flag: InventoryFlag::MedSlot0, quantity: 1, singleton: true, ..Default::default()
        });
    });
    ctx.invalidate_ship_fitting(emp_ship);
    let _ = sys_h
        .add_target_lock(
            emp_ship,
            eve_destiny::TargetLock {
                target_id: vic_ship,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await;
    let (_s3_pts, s3_stage) = add_system_suppression(&ctx, camp_id, front_sys, 600);
    assert_eq!(s3_stage, 3);
    let err_s3 = rpc_call(
        &ctx, &mut s_emp, "dogmaIM", "Activate",
        &[PyRep::Integer(scram_id), PyRep::String("warpScrambler".into()), PyRep::Integer(vic_ship)],
    ).await.unwrap_err();
    assert_eq!(err_s3.code, "TargetOutOfRange", "8,000 m exceeds 7,500 m Warp Scrambler range at Suppression Stage 3");

    let (_s4_pts, s4_stage) = add_system_suppression(&ctx, camp_id, front_sys, 200);
    assert_eq!(s4_stage, 4);
    let ok_s4 = rpc_call(
        &ctx, &mut s_emp, "dogmaIM", "Activate",
        &[PyRep::Integer(scram_id), PyRep::String("warpScrambler".into()), PyRep::Integer(vic_ship)],
    ).await;
    assert!(ok_s4.is_ok(), "8,000 m succeeds within +10% (8,250 m) Warp Scrambler range at Suppression Stage 4: {ok_s4:?}");

    // 5. Live Suppression Stage 5 Empire FW LP (`1.20x`) & NPC Bounty/Security Status (`1.25x`):
    let (_s5_pts, s5_stage) = add_system_suppression(&ctx, camp_id, front_sys, 200);
    assert_eq!(s5_stage, 5);
    let emp_kill_lp = eve_server::services::fw_services::award_fw_kill_lp(&ctx, emp_cid, front_sys, 100_000_000.0);
    let pir_kill_lp = eve_server::services::fw_services::award_fw_kill_lp(&ctx, pir_cid, front_sys, 100_000_000.0);
    assert_eq!(emp_kill_lp, ((pir_kill_lp as f64) * 1.20).round() as i64, "Empire militia receives +20% FW LP at Suppression Stage 5");

    let (bounty_npc_tid, base_npc_bounty) = ctx
        .sde
        .types
        .values()
        .find_map(|t| {
            t.default_attributes
                .get(&eve_sde::attr_ids::ENTITY_KILL_BOUNTY)
                .copied()
                .filter(|&b| b > 0.0)
                .map(|b| (t.type_id, b))
        })
        .expect("SDE NPC type with ENTITY_KILL_BOUNTY > 0");
    let npc_ball_id = 88_000_071i64;
    let npc_ball = make_ball(npc_ball_id, bounty_npc_tid, DVec3::new(2_000.0, 0.0, 0.0));
    let _ = sys_h.spawn_ball_with_combat(npc_ball.clone(), ctx.build_type_combat_state(bounty_npc_tid)).await;
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&emp_cid).unwrap().security_status = 0.0;
    });
    let _ = eve_server::services::combat_crimewatch::combat_weapons_spool::handle_ship_or_npc_destruction(
        &ctx, &sys_h, emp_cid, emp_ship, 639, 0, npc_ball_id, &npc_ball, None, front_sys, 100.0, ctx.now_unix(),
    ).await;
    let pending_bounty = with_combat_runtime(&ctx, |rt| rt.pending_bounty_ticks.get(&(front_sys, emp_cid)).copied().unwrap_or(0.0));
    let new_sec = ctx.db.with_snapshot(|s| s.characters[&emp_cid].security_status);
    let expected_sec = eve_server::combat::npc_ai::compute_sec_status_gain(0.0, base_npc_bounty) * 1.25;
    assert!((pending_bounty - base_npc_bounty * 1.25).abs() < 1e-3, "Suppression Stage 5 scales NPC bounty by +25% (got {pending_bounty})");
    assert!((new_sec - expected_sec).abs() < 1e-6, "Suppression Stage 5 scales NPC security status gain by +25% (got {new_sec})");
}
