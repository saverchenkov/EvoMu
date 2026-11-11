//! Comprehensive unit and mutation-killing tests for `combat_npc_encounters`.
//!
//! Mounted inside `combat_npc_encounters` (`use super::*;`) so both module instantiations
//! (`services::combat_npc_encounters` and `services::combat_crimewatch::combat_npc_encounters`)
//! execute multi-wave spawning/advancement, space-death bounty/suppression/sec-status/Drifter hooks,
//! 20m bounty tick K-space nullsec DBS/ESS/tax splits, Officer/Commander/Mordu drops, convoy/spawn
//! group wreck population, cycle-destroyed wreck finalization, and kill progression hooks.

#![forbid(unsafe_code)]

use super::*;
use crate::combat::npc_ai::{NpcAiState, NpcCombatEntity, NpcSpawnManager};
use crate::living_world::{LivingWorldConvoy, NpcArchetype};
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::combat_crimewatch::combat_npc_ai::combat_npc_ai_tests::make_test_npc_entity;
use eve_db::durable_domain_types::PersistedInsurgencyCampaign;
use eve_db::{CorporationRecord, InventoryFlag, IskCents, SystemDynamicState, VirtualServerClock};
use eve_destiny::DamageVector;
use eve_sde::{NpcSpawnGroupInfo, SolarSystem};
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
async fn wave_creation_spawn_groups_trigger_and_empty_wave_advancement_and_space_death() {
    let (mut ctx, _sess, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let now = 1_755_000_000_u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now)));

    let mut sde = (*ctx.sde).clone();
    // Custom NPC types: 941_001 with full damage/range/vel/bounty attrs, 941_002 with zero dmg attrs
    let mut t_full = make_fighter_type(941_001, 550, 11, 500.0, vec![]);
    t_full.faction_id = Some(500_010);
    t_full.default_attributes.insert(481, 80_000.0);
    t_full.default_attributes.insert(37, 320.0);
    t_full.default_attributes.insert(54, 12_000.0);
    t_full.default_attributes.insert(64, 3.0);
    t_full.default_attributes.insert(114, 10.0);
    t_full.default_attributes.insert(116, 15.0);
    t_full.default_attributes.insert(117, 20.0);
    t_full.default_attributes.insert(118, 25.0);
    sde.types.insert(941_001, t_full);
    sde.types
        .insert(941_002, make_fighter_type(941_002, 550, 11, 500.0, vec![]));

    sde.npc_spawn_groups.insert(
        1,
        NpcSpawnGroupInfo {
            spawn_group_id: 1,
            faction_id: 500_010,
            ship_class: 1,
            group_id: 550,
            group_name: "TestRats".into(),
            npc_type_ids: vec![941_001, 941_002],
        },
    );
    ctx.sde = Arc::new(sde);

    // 1. Empty custom site & max_waves=0 clamping + 3-wave SDE spawn group derivation
    assert!(create_custom_encounter_site(&ctx, 1, sys_id, vec![], DVec3::ZERO).await.is_empty());
    assert_eq!(create_encounter_wave_site(&ctx, 2, sys_id, 941_001, 0, DVec3::ZERO).await.len(), 2);

    let w1 = create_encounter_wave_site(&ctx, 3, sys_id, 941_001, 3, DVec3::new(2_000.0, 0.0, 0.0)).await;
    assert_eq!(w1.len(), 2);
    // Non-trigger kill leaves trigger alive -> still wave 1, not completed
    step_encounter_wave_on_kill(&ctx, sys_id, w1[1], DVec3::ZERO).await;
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.encounter_waves[&3].current_wave, 1);
        assert!(!rt.encounter_waves[&3].completed);
    });

    // Shorten wave_npc_types to 2 entries while max_waves = 3 to test both explicit wave 2 and fallback wave 3!
    with_mut_combat_runtime(&ctx, |rt| {
        rt.encounter_waves.get_mut(&3).unwrap().wave_npc_types.truncate(2);
    });
    step_encounter_wave_on_kill(&ctx, sys_id, w1[0], DVec3::ZERO).await;
    let w2_trigger = with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.encounter_waves[&3].current_wave, 2);
        rt.encounter_waves[&3].trigger_ball_id
    });
    step_encounter_wave_on_kill(&ctx, sys_id, w2_trigger, DVec3::ZERO).await;
    let w3_balls = with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.encounter_waves[&3].current_wave, 3);
        assert!(!rt.encounter_waves[&3].completed);
        rt.encounter_waves[&3].active_wave_ball_ids.clone()
    });
    // Final wave: killing trigger while other balls remain does NOT complete until all balls are dead
    assert!(w3_balls.len() >= 2);
    step_encounter_wave_on_kill(&ctx, sys_id, w3_balls[0], DVec3::ZERO).await;
    with_combat_runtime(&ctx, |rt| assert!(!rt.encounter_waves[&3].completed));
    for &b in &w3_balls[1..] {
        step_encounter_wave_on_kill(&ctx, sys_id, b, DVec3::ZERO).await;
    }
    with_combat_runtime(&ctx, |rt| assert!(rt.encounter_waves[&3].completed));

    // 2. handle_npc_destroyed_in_space: active Stage-4 suppression (+20% bounty), explicit & zero-dmg wave spawns,
    //    final-wave completion, Drifter sleeper-core drop, sec_status < 5.0 vs >= 5.0, Sleeper & duplicate guards!
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().security_status = 2.0;
        snap.insurgency_runtime.campaigns.insert(
            77,
            PersistedInsurgencyCampaign {
                campaign_id: 77,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: now - 500,
                forecasting_end_unix: now - 100,
                active_end_unix: now + 5_000,
                winddown_end_unix: now + 9_000,
                corruption_points: HashMap::new(),
                suppression_points: HashMap::from([(sys_id, 800)]), // Stage 4 -> 1.20x bounty!
            },
        );
    });

    let space_w1 = create_custom_encounter_site(
        &ctx,
        4,
        sys_id,
        vec![vec![941_001], vec![941_002]],
        DVec3::new(4_000.0, 0.0, 0.0),
    )
    .await;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.encounter_waves.get_mut(&4).unwrap().max_waves = 3; // wave 3 uses fallback 941_001 (non-zero dmg)
        let mut drifter = make_test_npc_entity(space_w1[0], 941_001, sys_id, "Drifter");
        drifter.bounty_isk = 100_000.0;
        drifter.custom_cargo_loot = Some((30_744, 2));
        drifter.sleeper_wh_class = 5;
        rt.npc_entities.insert(space_w1[0], drifter);
    });

    handle_npc_destroyed_in_space(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        sys_id,
        space_w1[0],
        Some(char_id),
    )
    .await;

    // Stage 4 suppression gives 100,000 * 1.20 = 120,000 pending bounty + sec status gain!
    let space_w2_trigger = with_combat_runtime(&ctx, |rt| {
        let pending = rt.pending_bounty_ticks[&(sys_id, char_id)];
        assert!((pending - 120_000.0).abs() < 1e-3);
        assert!(!rt.sec_status_transactions[&char_id].is_empty());
        rt.encounter_waves[&4].trigger_ball_id
    });

    // Set security_status = 5.0 -> kills wave 2 trigger (spawns fallback wave 3 with 941_001 damage vector), skips sec status gain
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().security_status = 5.0;
    });
    with_mut_combat_runtime(&ctx, |rt| {
        if let Some(n) = rt.npc_entities.get_mut(&space_w2_trigger) {
            n.bounty_isk = 50_000.0;
        }
    });
    handle_npc_destroyed_in_space(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        sys_id,
        space_w2_trigger,
        Some(char_id),
    )
    .await;

    // Kill all wave 3 balls with killer_char_id = None -> marks encounter 4 completed without crediting bounty;
    // also verify Sleeper archetype does not credit pending_bounty_ticks!
    let w3_ids = with_mut_combat_runtime(&ctx, |rt| {
        let mut sl = make_test_npc_entity(888_901, 941_001, sys_id, "Sleeper");
        sl.bounty_isk = 999_999.0;
        rt.npc_entities.insert(888_901, sl);
        rt.encounter_waves[&4].active_wave_ball_ids.clone()
    });
    let bounty_before = with_combat_runtime(&ctx, |rt| rt.pending_bounty_ticks[&(sys_id, char_id)]);
    handle_npc_destroyed_in_space(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        sys_id,
        888_901,
        Some(char_id),
    )
    .await;
    for bid in w3_ids {
        handle_npc_destroyed_in_space(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            &ctx.combat_ephemeral,
            sys_id,
            bid,
            None,
        )
        .await;
    }
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.pending_bounty_ticks[&(sys_id, char_id)], bounty_before);
        assert!(rt.encounter_waves[&4].completed);
    });
}

#[test]
fn bounty_tick_thresholds_npc_vs_player_corp_wspace_and_nullsec_dbs_ess_splits() {
    let (mut ctx, _sess, char_id, _ship_id, hs_sys) = make_fighter_ctx_and_session();
    let null_sys = 30_004_759_u32;
    let wh_sys = 31_000_005_u32;
    let now = 1_755_100_000_u64;

    let mut sde = (*ctx.sde).clone();
    for (sid, sec) in [(null_sys, -0.7), (wh_sys, -1.0)] {
        sde.solar_systems.insert(
            sid,
            SolarSystem {
                solar_system_id: sid,
                region_id: 10_000_060,
                constellation_id: 20_000_700,
                name: format!("S_{sid}"),
                security_status: sec,
                radius: 1.0e12,
                galactic_pos_ly: [0.0; 3],
                planet_ids: Vec::new(),
                stations: Vec::new(),
                wormhole_class_id: None,
            },
        );
    }
    ctx.sde = Arc::new(sde);

    // 1. Empty / non-positive pending tick returns zeroed payout
    let empty_tick = flush_npc_bounty_tick(&ctx, hs_sys, char_id, 0.0);
    assert_eq!((empty_tick.pilot_isk, empty_tick.main_ess_isk, empty_tick.reserve_ess_isk), (0.0, 0.0, 0.0));

    // 2. Overdue boundary: 1,199s not flushed; 1,200s flushed at 0% tax (< 100k with default threshold)
    with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((hs_sys, char_id), 99_999.0);
        rt.pending_bounty_tick_started_unix.insert((hs_sys, char_id), now);
    });
    tick_overdue_npc_bounty_payouts(&ctx, now + 1_199);
    assert!(with_combat_runtime(&ctx, |rt| rt.pending_bounty_ticks.contains_key(&(hs_sys, char_id))));
    tick_overdue_npc_bounty_payouts(&ctx, now + 1_200);
    assert!(with_combat_runtime(&ctx, |rt| !rt.pending_bounty_ticks.contains_key(&(hs_sys, char_id))));

    // 3. Explicit non-0.11 tax rate (0.20) applies even below 100k gross;
    //    >= 100k in NPC corp applies 11% NPC corp tax!
    with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((hs_sys, char_id), 50_000.0);
    });
    let small_taxed = flush_npc_bounty_tick(&ctx, hs_sys, char_id, 0.20);
    assert!((small_taxed.pilot_isk - 40_000.0).abs() < 1e-2 && small_taxed.main_ess_isk == 0.0 && small_taxed.reserve_ess_isk == 0.0);

    with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((hs_sys, char_id), 200_000.0);
    });
    let p_npc_tax = flush_npc_bounty_tick(&ctx, hs_sys, char_id, 0.0).pilot_isk;
    assert!((p_npc_tax - 178_000.0).abs() < 1e-2);

    // 4. Wormhole system (>= 31_000_000, sec -1.0) is NOT K-space nullsec -> dbs_mult = 1.0, main_ess = 0.0!
    with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((wh_sys, char_id), 200_000.0);
    });
    let wh_payout = flush_npc_bounty_tick(&ctx, wh_sys, char_id, 0.0);
    assert_eq!((wh_payout.main_ess_isk, wh_payout.reserve_ess_isk), (0.0, 0.0));

    // 5. K-space nullsec with player corp: first with dbs_multiplier = 0.0 (falls back to 1.35x),
    //    then with custom dbs_multiplier = 1.25x, crediting master_wallet, division 1, and ESS main/reserve banks!
    let pcorp = 98_600_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().corporation_id = pcorp;
        snap.corporations.insert(
            pcorp,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = pcorp;
                __corp.name = "NullCorp".into();
                __corp.ticker = "NCRP".into();
                __corp.ceo_id = char_id;
                __corp.tax_rate = 0.10;
                __corp.set_division_balance(1, IskCents::ZERO);
                __corp },
        );
        snap.system_states.insert(
            null_sys,
            SystemDynamicState {
                solar_system_id: null_sys,
                dbs_multiplier: 0.0, // exercises fallback 1.35x!
                ..Default::default()
            },
        );
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((null_sys, char_id), 1_000_000.0);
    });
    let null_payout = flush_npc_bounty_tick(&ctx, null_sys, char_id, 0.0);
    assert!(null_payout.pilot_isk > 0.0 && null_payout.main_ess_isk > 0.0 && null_payout.reserve_ess_isk >= 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        let c = &snap.corporations[&pcorp];
        assert!(c.master_wallet_balance().raw_cents() > 0);
        assert_eq!(c.wallet_divisions[0].balance, c.master_wallet_balance());
        assert!(snap.system_states[&null_sys].ess_main_bank.raw_cents() > 0);
        snap.system_states.get_mut(&null_sys).unwrap().dbs_multiplier = 1.25;
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((null_sys, char_id), 500_000.0);
    });
    let null_pilot2 = flush_npc_bounty_tick(&ctx, null_sys, char_id, 0.11).pilot_isk;
    assert!(null_pilot2 > 0.0);
}

#[tokio::test]
async fn drops_convoys_spawn_groups_cycle_finalize_and_kill_progression_all_branches() {
    let (mut ctx, _sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = (*ctx.sde).clone();
    sde.types.retain(|_, t| !matches!(t.meta_group_id, Some(4 | 5)));

    // Mordu (1285, 1286, 1287), Officer matched (553) & unmatched fallback (4795), 5 Faction Commanders, plain rat, and Pochven logs
    sde.types.insert(945_001, make_fighter_type(945_001, 1_285, 11, 500.0, vec![]));
    sde.types.insert(945_002, make_fighter_type(945_002, 1_286, 11, 500.0, vec![]));
    sde.types.insert(945_003, make_fighter_type(945_003, 1_287, 11, 500.0, vec![]));

    let mut off_match = make_fighter_type(945_004, 553, 11, 1_000.0, vec![]);
    off_match.name = "Tobias Kruzhor".into();
    sde.types.insert(945_004, off_match);
    let mut off_unmatched = make_fighter_type(945_005, 4_795, 11, 1_000.0, vec![]);
    off_unmatched.name = "QqqUnmatchedBoss".into();
    sde.types.insert(945_005, off_unmatched);
    let mut off_mod = make_fighter_type(945_006, 60, 7, 5.0, vec![]);
    off_mod.name = "Tobias' Modified Webifier".into();
    off_mod.meta_group_id = Some(5);
    sde.types.insert(945_006, off_mod);

    for (i, pfx) in [
        "Dread Guristas",
        "Shadow Serpentis",
        "Dark Blood",
        "True Sansha",
        "Domination",
    ]
    .into_iter()
    .enumerate()
    {
        let ntid = 945_010 + (i as u32) * 2;
        let mtid = ntid + 1;
        let mut fn_t = make_fighter_type(ntid, 550, 11, 500.0, vec![]);
        fn_t.name = format!("{pfx} Ace");
        sde.types.insert(ntid, fn_t);
        let mut fm_t = make_fighter_type(mtid, 60, 7, 5.0, vec![]);
        fm_t.name = format!("{pfx} Point");
        fm_t.meta_group_id = Some(4);
        sde.types.insert(mtid, fm_t);
    }
    let mut plain_rat = make_fighter_type(945_030, 550, 11, 500.0, vec![]);
    plain_rat.name = "Guristas Pithi".into();
    sde.types.insert(945_030, plain_rat);
    sde.wrecks.insert(945_030, 26_559);

    let mut nav_log = make_fighter_type(945_040, 300, 4, 0.1, vec![]);
    nav_log.name = "Interstellar Navigation Logs".into();
    sde.types.insert(945_040, nav_log);
    ctx.sde = Arc::new(sde);

    assert_eq!(resolve_officer_or_commander_sde_drop(&ctx, 404_404_404), None);
    assert_eq!(resolve_officer_or_commander_sde_drop(&ctx, 945_030), None);
    assert_eq!(resolve_officer_or_commander_sde_drop(&ctx, 945_001), Some((33_817, true)));
    assert_eq!(resolve_officer_or_commander_sde_drop(&ctx, 945_002), Some((33_819, true)));
    assert_eq!(resolve_officer_or_commander_sde_drop(&ctx, 945_003), Some((33_821, true)));
    assert_eq!(resolve_officer_or_commander_sde_drop(&ctx, 945_004), Some((945_006, false)));
    assert_eq!(resolve_officer_or_commander_sde_drop(&ctx, 945_005), Some((945_006, false)));
    for i in 0..5 {
        let ntid = 945_010 + (i as u32) * 2;
        assert_eq!(resolve_officer_or_commander_sde_drop(&ctx, ntid), Some((ntid + 1, false)));
    }

    // 1. NpcSpawnManager respawn timer: 2 entities in group 9 -> 1st kill keeps timer empty, 2nd kill sets +1200s!
    {
        let mut spawns = ctx.npc_spawns.write().unwrap();
        let mgr = spawns.entry(sys_id).or_insert_with(NpcSpawnManager::new);
        for bid in [960_001_i64, 960_002] {
            mgr.entities.insert(
                bid,
                NpcCombatEntity {
                    ball_id: bid,
                    type_id: 945_030,
                    faction_id: 500_010,
                    state: NpcAiState::Idle,
                    spawn_group_id: 9,
                    spawn_pos: DVec3::ZERO,
                    target_id: None,
                    bounty_isk: 1_000.0,
                    optimal_range: 5_000.0,
                    orbit_range: 2_500.0,
                    base_dps: 10.0,
                    damage_vector: DamageVector::new(5.0, 5.0, 5.0, 5.0),
                    last_attack_tick: 0,
                    has_shield_rep: false,
                    has_armor_rep: false,
                },
            );
        }
    }
    let wreck_id = 970_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wreck_id,
            make_fighter_item(wreck_id, 26_557, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    let mut dropped = Vec::new();
    populate_npc_wreck_on_turret_kill(&ctx, sys_id, 960_001, 945_030, DVec3::ZERO, wreck_id, char_id, &mut dropped).await;
    assert!(!ctx.npc_spawns.read().unwrap()[&sys_id].respawn_timers.contains_key(&(sys_id, 9)));
    populate_npc_wreck_on_turret_kill(&ctx, sys_id, 960_002, 945_030, DVec3::ZERO, wreck_id, char_id, &mut dropped).await;
    assert!(ctx.npc_spawns.read().unwrap()[&sys_id].respawn_timers.contains_key(&(sys_id, 9)));

    // 2. LivingWorldConvoy kills: wreck_id > 0 (drops convoy cargo & defaults to "Hauler") vs wreck_id == 0 (interdicts)!
    for (cid, wid) in [(960_010_i64, wreck_id), (960_011, 0)] {
        ctx.living_world.register_convoy(LivingWorldConvoy {
            convoy_id: cid,
            archetype: NpcArchetype::HaulerConvoy,
            ship_type_id: 945_030,
            origin_system_id: sys_id,
            current_system_id: sys_id,
            destination_system_id: sys_id,
            destination_station_id: 60_003_760,
            cargo_type_id: 34,
            cargo_quantity: 500,
            lod_level: 2,
            destroyed: false,
        });
        let mut c_drop = Vec::new();
        let arch = populate_npc_wreck_on_turret_kill(
            &ctx,
            sys_id,
            cid,
            945_030,
            DVec3::ZERO,
            wid,
            char_id,
            &mut c_drop,
        )
        .await;
        assert_eq!(arch, "Hauler");
        if wid > 0 {
            assert!(c_drop.iter().any(|d| d.type_id == 34 && d.qty_dropped == 500));
        }
        assert!(ctx.living_world.get_convoy(cid).unwrap().destroyed);
    }

    // 3. Archetype loot & CloneSoldier cooldown
    for (bid, ntid, arch) in [
        (960_020_i64, 945_001_u32, "CloneSoldier"),
        (960_021, 945_030, "Sleeper"),
        (960_022, 945_030, "PochvenHypnosianSleeper"),
        (960_023, 945_030, "Triglavian"),
    ] {
        with_mut_combat_runtime(&ctx, |rt| {
            let mut e = make_test_npc_entity(bid, ntid, sys_id, arch);
            e.tag_drop_type_id = Some(28_001);
            e.custom_cargo_loot = Some((35, 4));
            rt.npc_entities.insert(bid, e);
        });
        let mut d = Vec::new();
        assert_eq!(
            populate_npc_wreck_on_turret_kill(&ctx, sys_id, bid, ntid, DVec3::ZERO, wreck_id, char_id, &mut d).await,
            arch
        );
        assert!(d.len() >= 2);
    }
    assert!(with_combat_runtime(&ctx, |rt| rt.clone_soldier_cooldown_until.contains_key(&sys_id)));

    // 4. finalize_cycle_destroyed_npcs: empty no-op, Abyssal 32_000_001, and K-space killer resolution
    //    (char_id, ship_id -> owner_id, target_id, unknown -> 0) + SDE/Drifter/Sleeper/default wreck types!
    finalize_cycle_destroyed_npcs(&ctx, sys_id).await;
    let ab_sys = 32_000_001_u32;
    with_mut_combat_runtime(&ctx, |rt| {
        let mut ab = make_test_npc_entity(960_030, 945_030, ab_sys, "Triglavian");
        ab.target_id = Some(char_id);
        rt.destroyed_npc_cache.insert(960_030, ab);

        let mut e_sde_wreck = make_test_npc_entity(960_031, 945_030, sys_id, "BeltRat");
        e_sde_wreck.aggression_threat.insert(ship_id, 250.0); // resolves ship_id -> char_id!
        rt.destroyed_npc_cache.insert(960_031, e_sde_wreck);

        let mut e_drifter = make_test_npc_entity(960_032, 945_001, sys_id, "Drifter");
        e_drifter.threat_table.insert(char_id, 100.0);
        rt.destroyed_npc_cache.insert(960_032, e_drifter);

        let mut e_sleeper = make_test_npc_entity(960_033, 945_002, sys_id, "Sleeper");
        e_sleeper.target_id = Some(999_999_999); // unknown -> 0
        rt.destroyed_npc_cache.insert(960_033, e_sleeper);

        let e_default = make_test_npc_entity(960_034, 945_003, sys_id, "BeltRat");
        rt.destroyed_npc_cache.insert(960_034, e_default);
    });
    finalize_cycle_destroyed_npcs(&ctx, ab_sys).await;
    finalize_cycle_destroyed_npcs(&ctx, sys_id).await;
    assert!(with_combat_runtime(&ctx, |rt| rt.destroyed_npc_cache.is_empty()));

    // 5. record_npc_kill_progression: <= 0 guard + Some(tid) & None
    record_npc_kill_progression(&ctx, 0, Some(945_030));
    record_npc_kill_progression(&ctx, -5, None);
    record_npc_kill_progression(&ctx, char_id, Some(945_030));
    record_npc_kill_progression(&ctx, char_id, None);
}
