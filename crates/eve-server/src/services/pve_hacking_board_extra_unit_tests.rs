//! Extra branch-coverage and mutation-killing unit tests for `pve_hacking_board.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_exploration::{
    with_pve_exploration_reg, HackingGameSession, HackingNodeKind, HackingTileState,
    HackingUtilityKind, PveSiteInstance, SpewedLootCan,
};
use crate::services::pve_hacking_board::{
    build_hacking_grid_procedural, build_hacking_grid_procedural_with_sde, detonate_ghost_site,
    detonate_ghost_site_with_can, hacking_tile_distance_indicator,
    handle_second_hack_failure_explosion, resolve_fitted_analyzer_virus_stats,
    resolve_hacking_node_stats, resolve_hacking_turn_combat,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_sde::{SdeRegistry, SolarSystem, SolarSystemId};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

const CUSTOM_INTEGRATED_182_183: u32 = 990_001;
const CUSTOM_INTEGRATED_1330_1331: u32 = 990_002;
const CUSTOM_QA_ANALYZER: u32 = 990_003;
const CUSTOM_RELIC_1331: u32 = 990_004;
const CUSTOM_DATA_MINIMAL: u32 = 990_005;
const CUSTOM_SUBSYSTEM_BONUS: u32 = 990_006;
const CUSTOM_IMPLANT_ARCH: u32 = 990_007;
const CUSTOM_BOOSTER_HACK: u32 = 990_008;
const CUSTOM_IMPLANT_BOTH: u32 = 990_009;
const CUSTOM_STORM_GROUP_920: u32 = 990_010;
const CUSTOM_CONTAINER_TIER_3: u32 = 990_011;
const CUSTOM_CONTAINER_TIER_0: u32 = 990_012;

fn custom_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        let base_t = sde.types.values().next().cloned().expect("canonical seed");

        let mut int1 = base_t.clone();
        int1.type_id = CUSTOM_INTEGRATED_182_183;
        int1.group_id = 538;
        int1.name = "Dual Skill Probe".into();
        int1.default_attributes =
            HashMap::from([(182, 21718.0), (183, 13278.0), (1909, 50.0), (1910, 25.0)]);
        sde.types.insert(int1.type_id, int1);

        let mut int2 = base_t.clone();
        int2.type_id = CUSTOM_INTEGRATED_1330_1331;
        int2.group_id = 538;
        int2.name = "Combo Decoder".into();
        int2.default_attributes =
            HashMap::from([(1330, 1.0), (1331, 1.0), (1909, 60.0), (1910, 30.0)]);
        sde.types.insert(int2.type_id, int2);

        let mut qa = base_t.clone();
        qa.type_id = CUSTOM_QA_ANALYZER;
        qa.group_id = 538;
        qa.name = "QA Multi Analyzer".into();
        qa.default_attributes = HashMap::from([(1909, 100.0), (1910, 50.0), (1911, 5.0), (54, 8000.0)]);
        sde.types.insert(qa.type_id, qa);

        let mut rel = base_t.clone();
        rel.type_id = CUSTOM_RELIC_1331;
        rel.group_id = 538;
        rel.name = "Ancient Scanner".into();
        rel.default_attributes = HashMap::from([(1331, 1.0), (1909, 45.0), (1910, 20.0)]);
        sde.types.insert(rel.type_id, rel);

        // Minimal data analyzer with low stats to test floor clamping (.max(10), .max(5), .max(1.0), .max(1000.0))
        let mut min_an = base_t.clone();
        min_an.type_id = CUSTOM_DATA_MINIMAL;
        min_an.group_id = 538;
        min_an.name = "Weak Data Scanner".into();
        min_an.default_attributes =
            HashMap::from([(1909, 2.0), (1910, 1.0), (1911, 0.0), (54, 100.0)]);
        sde.types.insert(min_an.type_id, min_an);

        let mut sub = base_t.clone();
        sub.type_id = CUSTOM_SUBSYSTEM_BONUS;
        sub.group_id = 954;
        sub.name = "Covert Subsystem".into();
        sub.default_attributes = HashMap::from([(1915, 15.0), (1918, 5.0)]);
        sde.types.insert(sub.type_id, sub);

        let mut imp_arch = base_t.clone();
        imp_arch.type_id = CUSTOM_IMPLANT_ARCH;
        imp_arch.group_id = 300;
        imp_arch.name = "Poteque 'Prospector' Archaeology AR-806".into();
        imp_arch.default_attributes = HashMap::from([(1915, 12.0), (1918, 4.0)]);
        sde.types.insert(imp_arch.type_id, imp_arch);

        let mut boost_hack = base_t.clone();
        boost_hack.type_id = CUSTOM_BOOSTER_HACK;
        boost_hack.group_id = 303;
        boost_hack.name = "Neural Hacking Enhancer".into();
        boost_hack.default_attributes = HashMap::from([(1915, 8.0), (1918, 3.0)]);
        sde.types.insert(boost_hack.type_id, boost_hack);

        let mut imp_both = base_t.clone();
        imp_both.type_id = CUSTOM_IMPLANT_BOTH;
        imp_both.group_id = 300;
        imp_both.name = "Blackglass Net Intrusion 900".into();
        imp_both.default_attributes = HashMap::from([(1915, -20.0), (1918, 20.0)]);
        sde.types.insert(imp_both.type_id, imp_both);

        let mut storm = base_t.clone();
        storm.type_id = CUSTOM_STORM_GROUP_920;
        storm.group_id = 920;
        storm.name = "Custom Weather Effect".into();
        storm.default_attributes = HashMap::from([(1915, 18.0)]);
        sde.types.insert(storm.type_id, storm);

        let mut c3 = base_t.clone();
        c3.type_id = CUSTOM_CONTAINER_TIER_3;
        c3.group_id = 1207;
        c3.default_attributes = HashMap::from([(1919, 3.0), (1927, 1.0)]);
        sde.types.insert(c3.type_id, c3);

        let mut c0 = base_t;
        c0.type_id = CUSTOM_CONTAINER_TIER_0;
        c0.group_id = 1207;
        c0.default_attributes = HashMap::from([(1919, 0.5)]);
        sde.types.insert(c0.type_id, c0);

        Arc::new(sde)
    }))
}

fn build_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_778_000_000)));
    ServerContext::with_sde_and_repository(custom_sde(), db)
}

#[test]
fn resolve_fitted_analyzer_integrated_implants_boosters_storms_and_floors() {
    let ctx = build_ctx();
    let char_id = 910_001_i64;
    let ship_id = 920_001_i64;
    let sys_id: SolarSystemId = 30_000_142;

    ctx.db
        .provision_gm_account("hack_extra", "pw", char_id, "HackExtra");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
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
                singleton: true,
                ..Default::default()
            },
        );
        // Fit minimal data analyzer to test floor clamping when session.ship_id is None (uses character.active_ship_id)
        snap.items.insert(
            ship_id + 1,
            InventoryItemRecord {
                item_id: ship_id + 1,
                type_id: CUSTOM_DATA_MINIMAL,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // session.ship_id = None and session.solar_system_id = None -> falls back to snap.characters
    let mut sess = SessionState::default();
    sess.character_id = Some(char_id);
    let (coh, strg, slots, range) =
        resolve_fitted_analyzer_virus_stats(&ctx, &sess, char_id, false).unwrap();
    assert_eq!(coh, 10); // clamped from 2 to 10
    assert_eq!(strg, 5); // clamped from 1 to 5
    assert_eq!(slots, 1); // clamped from 0 to 1
    assert_eq!(range, 1_000.0); // clamped from 100 to 1000

    // Relic request fails when only data analyzer is fitted
    assert!(resolve_fitted_analyzer_virus_stats(&ctx, &sess, char_id, true).is_err());

    // Test Integrated analyzers (182+183, 1330+1331, QA), subsystem slot (flag 125), implants, boosters, and storm beacons
    for int_tid in [
        CUSTOM_INTEGRATED_182_183,
        CUSTOM_INTEGRATED_1330_1331,
        CUSTOM_QA_ANALYZER,
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&(ship_id + 1)).unwrap().type_id = int_tid;
        });
        assert!(resolve_fitted_analyzer_virus_stats(&ctx, &sess, char_id, false).is_ok());
        assert!(resolve_fitted_analyzer_virus_stats(&ctx, &sess, char_id, true).is_ok());
    }

    // Add subsystem (flag 125 = SubSystemSlot0), implants, boosters, and weak/custom storm beacons
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&(ship_id + 1)).unwrap().type_id = CUSTOM_QA_ANALYZER; // 100 coh, 50 str
        snap.items.insert(
            ship_id + 2,
            InventoryItemRecord {
                item_id: ship_id + 2,
                type_id: CUSTOM_SUBSYSTEM_BONUS, // +15 coh, +5 str
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SubSystemSlot0,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id + 3,
            InventoryItemRecord {
                item_id: ship_id + 3,
                type_id: CUSTOM_IMPLANT_ARCH, // +12 coh, +4 str (relic only)
                owner_id: char_id,
                location_id: char_id,
                flag: InventoryFlag::Implant,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id + 4,
            InventoryItemRecord {
                item_id: ship_id + 4,
                type_id: CUSTOM_BOOSTER_HACK, // +8 coh, +3 str (data only)
                owner_id: char_id,
                location_id: char_id,
                flag: InventoryFlag::Booster,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id + 5,
            InventoryItemRecord {
                item_id: ship_id + 5,
                type_id: 47381, // Weak storm +10
                owner_id: 1,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id + 6,
            InventoryItemRecord {
                item_id: ship_id + 6,
                type_id: CUSTOM_STORM_GROUP_920, // Custom storm +18 (beats +10)
                owner_id: 1,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                ..Default::default()
            },
        );
        let mut sk = HashMap::new();
        sk.insert(
            21718,
            TrainedSkillRecord {
                type_id: 21718,
                level: 3,
                skill_points: 8_000.0,
            },
        );
        snap.skills.insert(char_id, sk);
    });

    // Data: base 100 + skill(3*10=30) + sub(15) + booster_hack(8) + storm(18) = 171; str: 50 + 5 + 3 = 58
    let (d_coh, d_str, d_slots, d_range) =
        resolve_fitted_analyzer_virus_stats(&ctx, &sess, char_id, false).unwrap();
    assert_eq!(d_coh, 171);
    assert_eq!(d_str, 58);
    assert_eq!(d_slots, 5);
    assert_eq!(d_range, 8_000.0);

    // Relic: base 100 + skill(0) + sub(15) + implant_arch(12) + storm(18) = 145; str: 50 + 5 + 4 = 59
    let (r_coh, r_str, _, _) =
        resolve_fitted_analyzer_virus_stats(&ctx, &sess, char_id, true).unwrap();
    assert_eq!(r_coh, 145);
    assert_eq!(r_str, 59);
}

#[test]
fn resolve_hacking_node_stats_and_procedural_board_difficulty_scaling() {
    let sde = custom_sde();

    // difficulty == 0 with SDE tier 3 container -> resolves to 3
    let (d_sde, s_sde) =
        resolve_hacking_node_stats(Some(&sde), Some(CUSTOM_CONTAINER_TIER_3), false, 0);
    assert_eq!(d_sde, 3);
    assert_eq!(s_sde.core_coh, 50);
    assert_eq!(s_sde.core_str, 14);
    assert_eq!(s_sde.fw_coh, 32);
    assert_eq!(s_sde.fw_str, 16);
    assert_eq!(s_sde.av_coh, 20);
    assert_eq!(s_sde.av_str, 24);
    assert_eq!(s_sde.support_coh, 20);
    assert_eq!(s_sde.support_str, 10);

    // difficulty > 0 with higher SDE tier uses difficulty.max(st)
    let (d_max, _) =
        resolve_hacking_node_stats(Some(&sde), Some(CUSTOM_CONTAINER_TIER_3), false, 1);
    assert_eq!(d_max, 3);

    // Container with tier < 1.0 is ignored -> defaults to 2 when difficulty == 0
    let (d_low, _) =
        resolve_hacking_node_stats(Some(&sde), Some(CUSTOM_CONTAINER_TIER_0), false, 0);
    assert_eq!(d_low, 2);

    // Verify exact pool counts across difficulties 1, 2, 3, 4, 5
    for (diff, exp_fw, exp_av, exp_rest, exp_sup, exp_util) in [
        (1_u8, 1_usize, 0_usize, 0_usize, 0_usize, 1_usize),
        (2, 1, 0, 0, 0, 2),
        (3, 2, 1, 1, 1, 3),
        (4, 2, 2, 1, 1, 4),
        (5, 2, 2, 1, 1, 4),
    ] {
        let board = build_hacking_grid_procedural_with_sde(Some(&sde), 100, None, false, diff, 7);
        assert_eq!(board.len(), 24);
        assert_eq!(
            board
                .iter()
                .filter(|t| t.kind == HackingNodeKind::Firewall)
                .count(),
            exp_fw
        );
        assert_eq!(
            board
                .iter()
                .filter(|t| t.kind == HackingNodeKind::Antivirus)
                .count(),
            exp_av
        );
        assert_eq!(
            board
                .iter()
                .filter(|t| t.kind == HackingNodeKind::RestorationNode)
                .count(),
            exp_rest
        );
        assert_eq!(
            board
                .iter()
                .filter(|t| t.kind == HackingNodeKind::VirusSuppressor)
                .count(),
            exp_sup
        );
        assert_eq!(
            board
                .iter()
                .filter(|t| t.kind == HackingNodeKind::UtilitySubsystem)
                .count(),
            exp_util
        );
        let d_ind = hacking_tile_distance_indicator(0, &board);
        assert!((1..=5).contains(&d_ind));
    }
}

#[test]
fn resolve_hacking_turn_combat_one_shot_kill_suppressor_floor_and_no_heal_when_dead() {
    let tiles = build_hacking_grid_procedural(900, true, 3, 55);
    let mut game = HackingGameSession {
        game_id: 9,
        character_id: 910_001,
        solar_system_id: 30_000_142,
        container_id: 900,
        site_id: "S9".into(),
        is_relic: true,
        is_ghost_site: false,
        is_sleeper_cache_puzzle: false,
        virus_coherence: 50,
        max_virus_coherence: 50,
        base_virus_strength: 20, // with suppressor (-15) clamps to .max(10) = 10
        polymorphic_shield_charges: 1,
        secondary_vector_target: Some((1, 1)),
        utilities: vec![Some(HackingUtilityKind::SelfRepair)], // all slots full
        tiles,
        status: "active".into(),
    };

    // Set tile 1 as unrevealed Firewall (so SecondaryVector does NOT tick unrevealed tile!)
    game.tiles[1] = HackingTileState {
        tile_index: 1,
        revealed: false,
        blocked_by_defensive: false,
        kind: HackingNodeKind::Firewall,
        coherence: 10,
        strength: 25,
        utility_drop: None,
    };
    // Set tile 2 as revealed active VirusSuppressor
    game.tiles[2] = HackingTileState {
        tile_index: 2,
        revealed: true,
        blocked_by_defensive: true,
        kind: HackingNodeKind::VirusSuppressor,
        coherence: 30,
        strength: 10,
        utility_drop: None,
    };
    // Set tile 3 as revealed active RestorationNode
    game.tiles[3] = HackingTileState {
        tile_index: 3,
        revealed: true,
        blocked_by_defensive: true,
        kind: HackingNodeKind::RestorationNode,
        coherence: 20,
        strength: 10,
        utility_drop: None,
    };

    // Attack tile 1 (coherence 10) with eff_str = 10 -> one-shot kill!
    // Because coherence drops to 0, tile 1 does NOT retaliate and polymorphic_shield_charges stays 1!
    let (k1, rem1, eff1, _) = resolve_hacking_turn_combat(&mut game, 1);
    assert_eq!(k1, "Firewall");
    assert_eq!(rem1, 0);
    assert_eq!(eff1, 10);
    assert_eq!(game.polymorphic_shield_charges, 1);
    assert_eq!(game.virus_coherence, 50);
    assert!(game.secondary_vector_target.is_none());

    // Now virus dies when attacking high-strength node (after shield is 0): RestorationNode must NOT heal when virus_coherence == 0
    game.polymorphic_shield_charges = 0;
    game.virus_coherence = 10;
    game.tiles[2].coherence = 40;
    game.tiles[2].strength = 20;
    let rest_coh_before = game.tiles[3].coherence;
    let (_, rem2, _, _) = resolve_hacking_turn_combat(&mut game, 2);
    assert_eq!(rem2, 30);
    assert_eq!(game.virus_coherence, 0);
    assert_eq!(game.tiles[3].coherence, rest_coh_before);
}

#[tokio::test]
async fn detonate_ghost_site_security_bands_and_blast_radius_boundary() {
    let mut ctx = build_ctx();
    let lowsec_sys: SolarSystemId = 30_002_537;
    let nullsec_sys: SolarSystemId = 30_004_759;
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        let base_sys = sde_mut.solar_systems.values().next().cloned().unwrap();
        sde_mut.solar_systems.insert(
            lowsec_sys,
            SolarSystem {
                solar_system_id: lowsec_sys,
                security_status: 0.3,
                ..base_sys.clone()
            },
        );
        sde_mut.solar_systems.insert(
            nullsec_sys,
            SolarSystem {
                solar_system_id: nullsec_sys,
                security_status: -0.5,
                ..base_sys
            },
        );
    }

    // Verify spewed loot cans are cleaned up on second hack failure explosion
    with_pve_exploration_reg(&ctx, |reg| {
        reg.spewed_cans.insert(
            700_001,
            SpewedLootCan {
                can_item_id: 700_001,
                parent_container_id: 800_001,
                solar_system_id: lowsec_sys,
                position: [0.0; 3],
                loot_type_id: 34,
                loot_quantity: 1,
                spawned_at_sec: 100,
                despawn_at_sec: 999_999,
                scooped: false,
            },
        );
    });
    let (exploded, _, _, _) =
        handle_second_hack_failure_explosion(&ctx, lowsec_sys, "REL_1", 800_001, true).await;
    assert!(exploded);
    with_pve_exploration_reg(&ctx, |reg| {
        assert!(!reg.spewed_cans.contains_key(&700_001));
    });

    // Lowsec ghost site -> 8,000 explosive damage; ship at 10,000m hit, ship at 10,001m spared
    let near_ship = 950_001_i64;
    let far_ship = 950_002_i64;
    let sys_h = ctx.systems.get_or_spawn(lowsec_sys);
    for (id, x) in [(near_ship, 10_000.0), (far_ship, 10_001.0)] {
        let _ = sys_h
            .spawn_ball_with_combat(
                Ball {
                    id,
                    type_id: 587,
                    position: DVec3::new(x, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: 20_000.0,
                    max_shield_hp: 20_000.0,
                    armor_hp: 20_000.0,
                    max_armor_hp: 20_000.0,
                    structure_hp: 20_000.0,
                    max_structure_hp: 20_000.0,
                    ..CombatState::default()
                },
            )
            .await;
    }
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(
            lowsec_sys,
            vec![PveSiteInstance {
                site_id: "LOW_GHOST".into(),
                dungeon_id: 5534,
                solar_system_id: lowsec_sys,
                position: [0.0, 0.0, 0.0],
                hackable_can_ids: vec![],
                ..Default::default()
            }],
        );
    });
    let mut sess = SessionState::default();
    let (low_dmg, _) = detonate_ghost_site(&ctx, &mut sess, lowsec_sys, "LOW_GHOST").await;
    assert_eq!(low_dmg, 8_000.0);
    let near_cs = sys_h.get_combat_state(near_ship).await.unwrap().unwrap();
    let far_cs = sys_h.get_combat_state(far_ship).await.unwrap().unwrap();
    assert!(near_cs.shield_hp < 20_000.0);
    assert_eq!(far_cs.shield_hp, 20_000.0);

    // Nullsec ghost site -> 10,000 explosive damage
    let (null_dmg, _) =
        detonate_ghost_site_with_can(&ctx, &mut sess, nullsec_sys, "NULL_GHOST", Some(12345)).await;
    assert_eq!(null_dmg, 10_000.0);
}
