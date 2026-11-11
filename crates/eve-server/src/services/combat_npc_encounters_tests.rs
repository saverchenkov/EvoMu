//! Unit and mutation-killing tests for `combat_npc_encounters`.

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use eve_db::InventoryFlag;
use glam::DVec3;

#[tokio::test]
async fn multi_wave_encounter_site_spawns_progresses_and_completes_on_trigger_kills() {
    let (ctx, _session, _char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();

    // Empty wave list returns empty
    let empty = create_custom_encounter_site(&ctx, 1, sys_id, Vec::new(), DVec3::ZERO).await;
    assert!(empty.is_empty());

    // 2-wave site
    let wave1 = create_custom_encounter_site(
        &ctx,
        77,
        sys_id,
        vec![vec![10_001, 10_001], vec![10_002]],
        DVec3::new(1_000.0, 0.0, 0.0),
    )
    .await;
    assert_eq!(wave1.len(), 2);

    // Killing trigger ball (wave1[0]) spawns wave 2 and advances current_wave to 2
    step_encounter_wave_on_kill(&ctx, sys_id, wave1[0], DVec3::ZERO).await;
    let remaining_balls = with_combat_runtime(&ctx, |rt| {
        let enc = rt.encounter_waves.get(&77).unwrap();
        assert_eq!(enc.current_wave, 2);
        assert!(!enc.completed);
        enc.active_wave_ball_ids.clone()
    });
    assert_eq!(remaining_balls.len(), 2); // wave1[1] + wave2[0]

    // Killing remaining balls completes the encounter site
    for bid in remaining_balls {
        step_encounter_wave_on_kill(&ctx, sys_id, bid, DVec3::ZERO).await;
    }
    with_combat_runtime(&ctx, |rt| {
        let enc = rt.encounter_waves.get(&77).unwrap();
        assert!(enc.completed);
    });

    // Also test create_encounter_wave_site and handle_npc_destroyed_in_space
    let auto_wave1 =
        create_encounter_wave_site(&ctx, 88, sys_id, 10_001, 2, DVec3::ZERO).await;
    assert_eq!(auto_wave1.len(), 2);
    handle_npc_destroyed_in_space(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        sys_id,
        auto_wave1[0],
        Some(90_000_001),
    )
    .await;
}

#[test]
fn bounty_tick_threshold_tax_and_overdue_flush_mechanics() {
    let (ctx, _session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let now = 1_750_000_000_u64;

    // 1. Empty tick returns zeroed payout
    let empty = flush_npc_bounty_tick(&ctx, sys_id, char_id, 0.0);
    assert_eq!((empty.pilot_isk, empty.main_ess_isk, empty.reserve_ess_isk), (0.0, 0.0, 0.0));

    // 2. Below 100k gross with default tax threshold -> 0% tax applied
    with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((sys_id, char_id), 50_000.0);
        rt.pending_bounty_tick_started_unix
            .insert((sys_id, char_id), now);
    });
    // Not overdue at now + 1199
    tick_overdue_npc_bounty_payouts(&ctx, now + 1_199);
    with_combat_runtime(&ctx, |rt| {
        assert!(rt.pending_bounty_ticks.contains_key(&(sys_id, char_id)));
    });

    // Overdue at now + 1200 -> flushes full 50,000 ISK (0% tax under 100k threshold)
    let bal_before = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).unwrap().balance.to_wire_f64());
    tick_overdue_npc_bounty_payouts(&ctx, now + 1_200);
    let bal_after_small = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).unwrap().balance.to_wire_f64());
    assert!((bal_after_small - bal_before - 50_000.0).abs() < 1e-2);

    // 3. >= 100k gross with 10% explicit tax -> 90% to pilot, 10% tax
    with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((sys_id, char_id), 200_000.0);
    });
    let payout = flush_npc_bounty_tick(&ctx, sys_id, char_id, 0.10);
    assert!((payout.pilot_isk - 180_000.0).abs() < 1e-2);
    assert_eq!(payout.main_ess_isk, 0.0);
    assert_eq!(payout.reserve_ess_isk, 0.0);
}

#[tokio::test]
async fn officer_commander_mordu_sde_drops_and_finalize_cycle_destroyed_npcs() {
    let (mut ctx, _session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = (*ctx.sde).clone();

    // Mordu groups 1285, 1286, 1287
    sde.types
        .insert(930_001, make_fighter_type(930_001, 1_285, 11, 1_000.0, vec![]));
    sde.types
        .insert(930_002, make_fighter_type(930_002, 1_286, 11, 1_000.0, vec![]));
    sde.types
        .insert(930_003, make_fighter_type(930_003, 1_287, 11, 1_000.0, vec![]));

    // Officer NPC (group 553) + matching Officer module (cat 7, meta_group_id 5)
    let mut off_npc = make_fighter_type(930_004, 553, 11, 5_000.0, vec![]);
    off_npc.name = "Cormack Vaaja".into();
    sde.types.insert(930_004, off_npc);
    let mut off_mod = make_fighter_type(930_005, 60, 7, 5.0, vec![]);
    off_mod.name = "Cormack's Modified Damage Control".into();
    off_mod.meta_group_id = Some(5);
    sde.types.insert(930_005, off_mod);

    // Faction commander NPC + matching Faction module (cat 7, meta_group_id 4)
    let mut fac_npc = make_fighter_type(930_006, 550, 11, 2_000.0, vec![]);
    fac_npc.name = "Dread Guristas Invader".into();
    sde.types.insert(930_006, fac_npc);
    let mut fac_mod = make_fighter_type(930_007, 39, 7, 5.0, vec![]);
    fac_mod.name = "Dread Guristas Multispectrum Shield Hardener".into();
    fac_mod.meta_group_id = Some(4);
    sde.types.insert(930_007, fac_mod);

    ctx.sde = std::sync::Arc::new(sde);

    assert_eq!(
        resolve_officer_or_commander_sde_drop(&ctx, 930_001),
        Some((33_817, true))
    );
    assert_eq!(
        resolve_officer_or_commander_sde_drop(&ctx, 930_002),
        Some((33_819, true))
    );
    assert_eq!(
        resolve_officer_or_commander_sde_drop(&ctx, 930_003),
        Some((33_821, true))
    );
    let (off_drop, off_bpc) = resolve_officer_or_commander_sde_drop(&ctx, 930_004).unwrap();
    assert!(!off_bpc && off_drop > 0);
    let (fac_drop, fac_bpc) = resolve_officer_or_commander_sde_drop(&ctx, 930_006).unwrap();
    assert!(!fac_bpc && fac_drop > 0);

    // Populate wreck on turret kill & finalize_cycle_destroyed_npcs
    let wreck_id = 940_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wreck_id,
            make_fighter_item(wreck_id, 26_557, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    let mut dropped = Vec::new();
    let arch = populate_npc_wreck_on_turret_kill(
        &ctx,
        sys_id,
        950_001,
        930_001,
        DVec3::ZERO,
        wreck_id,
        char_id,
        &mut dropped,
    )
    .await;
    assert_eq!(arch, "MorduCommander");
    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0].type_id, 33_817);
    assert!(dropped[0].singleton);

    // Queue a destroyed NPC in destroyed_npc_cache and finalize_cycle_destroyed_npcs
    with_mut_combat_runtime(&ctx, |rt| {
        let mut ent = crate::services::combat_crimewatch::combat_npc_ai::combat_npc_ai_tests::make_test_npc_entity(
            950_002,
            930_002,
            sys_id,
            "Sleeper",
        );
        ent.threat_table.insert(ship_id, 500.0);
        rt.destroyed_npc_cache.insert(950_002, ent);
    });
    finalize_cycle_destroyed_npcs(&ctx, sys_id).await;
    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.destroyed_npc_cache.contains_key(&950_002));
    });

    record_npc_kill_progression(&ctx, char_id, Some(930_001));
}
