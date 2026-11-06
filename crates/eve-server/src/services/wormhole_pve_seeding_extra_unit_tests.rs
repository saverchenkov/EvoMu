//! Deep branch-coverage and mutation-killing unit tests for `wormhole_pve_seeding`.

#![forbid(unsafe_code)]

use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use crate::services::pve_exploration::with_pve_exploration_reg;
use crate::services::wormhole_pve_seeding::*;
use glam::DVec3;

#[test]
fn sentry_types_capital_groups_and_sleeper_sentry_entity_builder() {
    assert_eq!(sleeper_sentry_type_for_class(0), 30_462);
    assert_eq!(sleeper_sentry_type_for_class(1), 30_460);
    assert_eq!(sleeper_sentry_type_for_class(2), 30_460);
    assert_eq!(sleeper_sentry_type_for_class(3), 30_461);
    assert_eq!(sleeper_sentry_type_for_class(4), 30_461);
    assert_eq!(sleeper_sentry_type_for_class(5), 30_462);
    assert_eq!(sleeper_sentry_type_for_class(6), 30_462);
    assert_eq!(sleeper_sentry_type_for_class(13), 30_462);

    assert!(!is_sleeper_sentry_type(30_459));
    assert!(is_sleeper_sentry_type(30_460));
    assert!(is_sleeper_sentry_type(30_461));
    assert!(is_sleeper_sentry_type(30_462));
    assert!(!is_sleeper_sentry_type(30_463));

    assert_eq!(capital_escalation_class_for_group(485), Some(true));
    assert_eq!(capital_escalation_class_for_group(4594), Some(true));
    assert_eq!(capital_escalation_class_for_group(547), Some(false));
    assert_eq!(capital_escalation_class_for_group(1538), Some(false));
    assert_eq!(capital_escalation_class_for_group(30), None);
    assert_eq!(capital_escalation_class_for_group(659), None);

    for cap_grp in [485_u32, 4594, 547, 1538, 30, 659] {
        assert!(is_any_capital_group(cap_grp));
    }
    assert!(!is_any_capital_group(26));

    let (ctx, _, _, _, _) = make_fighter_ctx_and_session();
    let pos = DVec3::new(1_000.0, 2_000.0, 3_000.0);
    for (tid, cls) in [(30_460_u32, 1_u8), (30_461, 3), (30_462, 5), (999_999_999, 6)] {
        let (ball, cs, ent) =
            build_sleeper_sentry_entity(&ctx.sde, 31_000_005, 800_100, tid, pos, cls);
        assert_eq!(ball.id, 800_100);
        assert_eq!(ball.type_id, tid);
        assert_eq!(ball.position, pos);
        assert_eq!(ball.max_velocity, 0.0);
        assert_eq!(ent.faction_id, 500_005);
        assert_eq!(ent.archetype, "SleeperSentry");
        assert_eq!(ent.sleeper_wh_class, cls);
        assert_eq!(ent.spawn_position, pos);
        if tid == 999_999_999 {
            assert_eq!(ent.optimal_range, 0.0);
            assert_eq!(ent.damage_vector.total(), 0.0);
            assert_eq!(cs.signature_radius, 0.0);
        }
    }
}

#[tokio::test]
async fn sentry_cooldown_loot_drop_and_special_wormhole_seeding_matrix() {
    let (ctx, _, char_id, _, sys_id) = make_fighter_ctx_and_session();

    let sbid = spawn_sleeper_sentry_tower(&ctx, sys_id, 2, DVec3::new(500.0, 0.0, 0.0)).await;
    assert!(can_sentry_fire_and_record(&ctx, sbid, 15.0, 50.0));
    assert!(!can_sentry_fire_and_record(&ctx, sbid, 15.0, 64.9));
    // Exact sim-time boundary (65.0 - 50.0 == 15.0)
    assert!(can_sentry_fire_and_record(&ctx, sbid, 15.0, 65.0));

    // Drop loot into space and verify container (type 3293) and contents
    let loot_iid =
        drop_site_loot_into_space(&ctx, sys_id, char_id, [10.0, 20.0, 30.0], 30_460, 7).await;
    ctx.db.with_snapshot(|snap| {
        let loot = &snap.items[&loot_iid];
        assert_eq!(loot.type_id, 30_460);
        assert_eq!(loot.quantity, 7);
        assert_eq!(loot.owner_id, char_id);
        let container = &snap.items[&loot.location_id];
        assert_eq!(container.type_id, 3293);
        assert!(container.singleton);
        assert_eq!(snap.item_positions.get(&container.item_id), Some(&[10.0, 20.0, 30.0]));
    });

    // 1. Drifter system (31_000_002 -> class 15): seeded = true, 0 sites
    assert!(seed_special_wormhole_pve_sites(&ctx, 31_000_002).await);
    with_pve_exploration_reg(&ctx, |reg| {
        assert_eq!(reg.seeded_systems.get(&31_000_002), Some(&true));
        assert!(reg.sites_by_system[&31_000_002].is_empty());
    });

    // 2. C13 small-ship shattered system (31_002_586 -> class 13): 5 combat + 4 data/relic = 9 sites, 14_400s lifetime, no debris/ice
    assert!(seed_special_wormhole_pve_sites(&ctx, 31_002_586).await);
    with_pve_exploration_reg(&ctx, |reg| {
        let sites = &reg.sites_by_system[&31_002_586];
        assert_eq!(sites.len(), 9);
        assert!(sites.iter().all(|s| s.lifetime_seconds == 14_400));
        assert!(!sites.iter().any(|s| s.is_shattered_resource_site));
    });

    // 3. Standard shattered system (find a C4/C5/C6 shattered system in SDE to cover higher classes)
    let high_shattered_sys = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| {
            let (cls, _, is_shat, _) =
                crate::services::wormhole_lifecycle_ops::system_wormhole_profile_sde(&ctx.sde, sid);
            (4..=6).contains(&cls) && is_shat
        })
        .unwrap_or(31_002_510);
    assert!(seed_special_wormhole_pve_sites(&ctx, high_shattered_sys).await);
    // Second call is idempotent on cosmic_anomalies and cosmic_signatures
    let (anom_cnt_1, sig_cnt_1) = ctx.db.with_snapshot(|snap| {
        (
            snap.cosmic_anomalies[&high_shattered_sys].len(),
            snap.cosmic_signatures[&high_shattered_sys].len(),
        )
    });
    assert!(seed_special_wormhole_pve_sites(&ctx, high_shattered_sys).await);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.cosmic_anomalies[&high_shattered_sys].len(), anom_cnt_1);
        assert_eq!(snap.cosmic_signatures[&high_shattered_sys].len(), sig_cnt_1);
    });
}

#[test]
fn build_specialized_npc_components_archetype_faction_damage_and_ewar_matrix() {
    let (ctx, _, _, _, k_sys) = make_fighter_ctx_and_session();
    let wh_sys = 31_000_005_u32;
    let pos = DVec3::new(5_000.0, 0.0, 0.0);

    // Find Clone Soldier Trainer, Recruiter, Transporter, Negotiator in SDE
    let find_clone_soldier = |kw: &str| -> u32 {
        ctx.sde
            .types
            .values()
            .find(|t| t.name.contains("Clone Soldier") && t.name.contains(kw))
            .map(|t| t.type_id)
            .unwrap_or(34)
    };
    for kw in ["Trainer", "Recruiter", "Transporter", "Negotiator"] {
        let cs_tid = find_clone_soldier(kw);
        let (_, _, ent) = build_specialized_npc_components(
            &ctx,
            k_sys,
            881_001,
            cs_tid,
            "CloneSoldier",
            500_001,
            pos,
        );
        assert!(
            ent.tag_drop_type_id.is_some(),
            "expected Clone Soldier {kw} tag drop type"
        );
        if kw == "Negotiator" {
            assert_eq!(ent.web_speed_factor, 0.0);
            assert!(ent.scram_strength >= 2.0);
        }
    }

    // Pochven Hypnosian Sleeper with aroused type ID 56680 -> shield_hp = 5_000.0
    let (_, cs_hyp, ent_hyp) = build_specialized_npc_components(
        &ctx,
        99_999_999,
        881_002,
        56_680,
        "PochvenHypnosianSleeper",
        0,
        pos,
    );
    assert_eq!(cs_hyp.max_shield_hp, 5_000.0);
    assert_eq!(ent_hyp.faction_id, 500_021);
    assert_eq!(ent_hyp.sleeper_wh_class, 0);

    // Sleeper in wormhole system resolves non-zero sleeper_wh_class
    let (_, _, ent_wh) = build_specialized_npc_components(
        &ctx,
        wh_sys,
        881_003,
        999_999_999,
        "Sleeper",
        0,
        pos,
    );
    assert!(ent_wh.sleeper_wh_class > 0);
    assert_eq!(ent_wh.bounty_isk, 0.0);

    // Unknown type ID (no SDE attributes) exercises all fallback faction & damage branches
    for (arch, sp_grp, exp_faction, exp_dmg_total) in [
        ("Navy", 500_004_u32, 500_004_u32, 100.0),
        ("Police", 999_u32, 500_001_u32, 100.0),
        ("RogueDrone", 0, 500_008, 100.0),
        ("EDENCOM", 0, 500_027, 120.0),
        ("Triglavian", 0, 500_026, 150.0),
        ("Drifter", 0, 500_024, 200.0),
        ("IncursionSansha", 500_019, 500_019, 200.0),
        ("TransmissionRelay", 500_005, 500_005, 0.0),
        ("CustomUnknown", 500_010, 500_010, 100.0),
    ] {
        let (ball, cs, ent) = build_specialized_npc_components(
            &ctx,
            k_sys,
            881_010,
            999_999_999,
            arch,
            sp_grp,
            pos,
        );
        assert_eq!(ent.faction_id, exp_faction);
        assert!((ent.damage_vector.total() - exp_dmg_total).abs() < 1e-6);
        assert_eq!(cs.max_structure_hp, 350.0);
        if arch == "TransmissionRelay" {
            assert_eq!(ball.max_velocity, 0.0);
            assert_eq!(ent.optimal_range, 0.0);
            assert_eq!(ent.orbit_range, 0.0);
            assert_eq!(ent.neut_amount, 0.0);
        } else if arch == "Drifter" {
            assert_eq!(ent.drifter_overshield_hp, 4_000.0);
        }
    }

    // npc_type_id == 79_507 zeroes web and scram
    let (_, _, ent_no_ewar) = build_specialized_npc_components(
        &ctx,
        k_sys,
        881_020,
        79_507,
        "Pirate",
        500_010,
        pos,
    );
    assert_eq!(ent_no_ewar.web_speed_factor, 0.0);
    assert_eq!(ent_no_ewar.scram_strength, 0.0);
}

#[tokio::test]
async fn shattered_resource_fields_dungeon_variants_and_sde_npc_attribute_branches() {
    let (ctx, _, _, _, k_sys) = make_fighter_ctx_and_session();
    let pos = DVec3::new(12_000.0, -4_000.0, 8_000.0);

    // Non-special systems (K-space and normal non-shattered WH) return false from seed_special_wormhole_pve_sites
    assert!(!seed_special_wormhole_pve_sites(&ctx, k_sys).await);
    assert!(!seed_special_wormhole_pve_sites(&ctx, 31_000_005).await);

    // Find a C5 shattered system so combat_classes = [4, 5, 6] exercises wh_class 4, 5 (variant 1 -> 3716), and 6,
    // and also find a C6 shattered system so combat_classes = [5, 6] exercises wh_class 5 (variant 0 -> 3704).
    let mut c5_shat = None;
    let mut c6_shat = None;
    let mut c2_shat = None;
    for &sid in ctx.sde.solar_systems.keys() {
        let (cls, _, is_shat, _) =
            crate::services::wormhole_lifecycle_ops::system_wormhole_profile_sde(&ctx.sde, sid);
        if is_shat {
            match cls {
                2 if c2_shat.is_none() => c2_shat = Some(sid),
                5 if c5_shat.is_none() => c5_shat = Some(sid),
                6 if c6_shat.is_none() => c6_shat = Some(sid),
                _ => {}
            }
        }
    }
    for shat_sys in [c2_shat, c5_shat, c6_shat].into_iter().flatten() {
        assert!(seed_special_wormhole_pve_sites(&ctx, shat_sys).await);
        with_pve_exploration_reg(&ctx, |reg| {
            let sites = &reg.sites_by_system[&shat_sys];
            let debris = sites
                .iter()
                .find(|s| s.site_category == "ShatteredDebris")
                .expect("expected ShatteredDebris site");
            assert_eq!(debris.dungeon_id, 3790);
            assert!(debris.is_shattered_resource_site);
            assert!(!debris.delayed_wave_triggered);
            assert_eq!(debris.current_wave, 0);
            assert_eq!(debris.total_waves, 1);

            let ice = sites
                .iter()
                .find(|s| s.site_category == "ShatteredIce")
                .expect("expected ShatteredIce site");
            assert_eq!(ice.dungeon_id, 3791);
            assert!(ice.is_shattered_resource_site);
            assert_eq!(ice.lifetime_seconds, 86_400);
        });
        ctx.db.with_snapshot(|snap| {
            let sys_asts: Vec<_> = snap
                .asteroids
                .values()
                .filter(|a| a.solar_system_id == shat_sys)
                .collect();
            assert_eq!(
                sys_asts.len(),
                SHATTERED_DEBRIS_ORE_TYPES.len() + SHATTERED_ICE_TYPES.len()
            );
            for &ore_tid in &SHATTERED_DEBRIS_ORE_TYPES {
                assert!(sys_asts.iter().any(|a| a.type_id == ore_tid && a.quantity == 20_000));
            }
            for &ice_tid in &SHATTERED_ICE_TYPES {
                assert!(sys_asts.iter().any(|a| a.type_id == ice_tid && a.quantity == 500));
            }
        });
    }

    // Wall-clock elapsed branch in can_sentry_fire_and_record
    let sbid = 889_900_i64;
    let now_u = ctx.now_unix_secs() as f64;
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sentry_last_fire_unix.insert(sbid, now_u - 20.0);
        reg.sentry_last_fire_sim_s.insert(sbid, 100.0);
    });
    // Sim delta is only 1.0s (< 15.0s), but unix delta is 20.0s (>= 15.0s) -> ready = true
    assert!(can_sentry_fire_and_record(&ctx, sbid, 15.0, 101.0));

    // SleeperSentry archetype in build_specialized_npc_components (unknown type fallback)
    let (ball_ss, cs_ss, ent_ss) = build_specialized_npc_components(
        &ctx,
        31_000_005,
        882_001,
        999_999_999,
        "SleeperSentry",
        0,
        pos,
    );
    assert_eq!(ball_ss.max_velocity, 0.0);
    assert_eq!(ent_ss.optimal_range, 250_000.0);
    assert_eq!(ent_ss.orbit_range, 0.0);
    assert_eq!(ent_ss.bounty_isk, 0.0);
    assert_eq!(ent_ss.neut_amount, 0.0);
    assert_eq!(ent_ss.web_speed_factor, 0.0);
    assert_eq!(ent_ss.scram_strength, 0.0);
    assert_eq!(cs_ss.signature_radius, 50.0);

    // PochvenHypnosianSleeper range 56319..=56327 vs non-aroused fallback
    let (_, cs_hyp_range, _) = build_specialized_npc_components(
        &ctx,
        99_999_999,
        882_002,
        56_320,
        "PochvenHypnosianSleeper",
        0,
        pos,
    );
    assert_eq!(cs_hyp_range.max_shield_hp, 5_000.0);
    let (_, cs_hyp_plain, _) = build_specialized_npc_components(
        &ctx,
        99_999_999,
        882_003,
        999_999_999,
        "PochvenHypnosianSleeper",
        0,
        pos,
    );
    assert_ne!(cs_hyp_plain.max_shield_hp, 5_000.0);

    // Drifter with group_id == 1956 (no overshield) vs normal Drifter
    if let Some(drifter_1956) = ctx.sde.types.values().find(|t| t.group_id == 1956) {
        let (_, _, ent_d1956) = build_specialized_npc_components(
            &ctx,
            k_sys,
            882_004,
            drifter_1956.type_id,
            "Drifter",
            0,
            pos,
        );
        assert_eq!(ent_d1956.drifter_overshield_hp, 0.0);
    }

    // Pirate / BeltRat / Officer fallback damage vector & regional pirate faction fallback (spawn_group_id == 0)
    for arch in ["Pirate", "BeltRat", "Officer"] {
        let (_, _, ent_p) = build_specialized_npc_components(
            &ctx,
            k_sys,
            882_005,
            999_999_999,
            arch,
            0,
            pos,
        );
        assert!(ent_p.faction_id >= 500_001);
        assert!((ent_p.damage_vector.total() - 100.0).abs() < 1e-6);
    }
}
