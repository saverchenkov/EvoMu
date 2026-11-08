//! Extra branch-coverage and mutation-killing unit tests for `combat_npc_encounters`.

#![forbid(unsafe_code)]

use std::sync::Arc;

use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::combat_crimewatch::combat_npc_ai::combat_npc_ai_tests::make_test_npc_entity;
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime};
use crate::services::combat_npc_encounters::{
    create_encounter_wave_site, finalize_cycle_destroyed_npcs, flush_npc_bounty_tick,
    handle_npc_destroyed_in_space, populate_npc_wreck_on_turret_kill, record_npc_kill_progression,
    resolve_officer_or_commander_sde_drop, step_encounter_wave_on_kill,
};
use eve_db::{CorporationRecord, InventoryFlag, IskCents, SystemDynamicState};
use eve_sde::SolarSystem;
use glam::DVec3;

#[tokio::test]
async fn test_handle_npc_destroyed_bounty_sec_status_drifter_and_wave_fallback() {
    let (mut ctx, _session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();

    // Add custom NPC types: one with explicit damage attrs, one with 0 damage attrs
    let mut sde = (*ctx.sde).clone();
    let mut npc_with_dmg = make_fighter_type(931_001, 550, 11, 500.0, vec![]);
    npc_with_dmg.default_attributes.insert(114, 15.0);
    npc_with_dmg.default_attributes.insert(116, 20.0);
    npc_with_dmg.default_attributes.insert(481, 50_000.0);
    sde.types.insert(931_001, npc_with_dmg);

    let npc_zero_dmg = make_fighter_type(931_002, 550, 11, 500.0, vec![]);
    sde.types.insert(931_002, npc_zero_dmg);
    ctx.sde = Arc::new(sde);

    // 1. create_encounter_wave_site with max_waves = 0 clamps to 1 wave
    let w1 = create_encounter_wave_site(&ctx, 501, sys_id, 931_001, 0, DVec3::ZERO).await;
    assert_eq!(w1.len(), 2);

    // Non-trigger kill while trigger is still in active_wave_ball_ids does not complete or advance
    step_encounter_wave_on_kill(&ctx, sys_id, w1[1], DVec3::ZERO).await;
    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.encounter_waves[&501].completed);
    });

    // 2. Seed an encounter where wave_npc_types is shorter than max_waves to hit fallback in handle_npc_destroyed_in_space
    let wave_balls = crate::services::combat_npc_encounters::create_custom_encounter_site(
        &ctx,
        502,
        sys_id,
        vec![vec![931_001], vec![931_002]],
        DVec3::new(2_000.0, 0.0, 0.0),
    )
    .await;
    with_mut_combat_runtime(&ctx, |rt| {
        if let Some(enc) = rt.encounter_waves.get_mut(&502) {
            enc.max_waves = 3; // wave 3 is not in wave_npc_types -> falls back to vec![npc_type_id, npc_type_id]
        }
        // Register trigger ball as a Drifter with bounty and custom_cargo_loot
        let mut drifter = make_test_npc_entity(wave_balls[0], 931_001, sys_id, "Drifter");
        drifter.bounty_isk = 100_000.0;
        drifter.custom_cargo_loot = Some((30_744, 2));
        drifter.sleeper_wh_class = 6;
        rt.npc_entities.insert(wave_balls[0], drifter);
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().security_status = 1.0;
    });

    // Kill wave 1 trigger -> spawns wave 2 (931_002 with zero dmg default), credits bounty + sec status + drops drifter wreck
    handle_npc_destroyed_in_space(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        sys_id,
        wave_balls[0],
        Some(char_id),
    )
    .await;

    let wave2_trigger = with_combat_runtime(&ctx, |rt| {
        assert!(rt.pending_bounty_ticks.get(&(sys_id, char_id)).copied().unwrap_or(0.0) >= 100_000.0);
        assert!(!rt.sec_status_transactions.get(&char_id).unwrap().is_empty());
        rt.encounter_waves[&502].trigger_ball_id
    });
    assert!(wave2_trigger > 0);

    // Kill wave 2 trigger -> advances to wave 3 using fallback vec![npc_type_id, npc_type_id] (931_001 with non-zero dmg)
    // Also set character security_status = 5.0 so the `old_sec < 5.0` false branch is covered
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().security_status = 5.0;
    });
    handle_npc_destroyed_in_space(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        sys_id,
        wave2_trigger,
        Some(char_id),
    )
    .await;
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.encounter_waves[&502].current_wave, 3);
    });
}

#[test]
fn test_flush_npc_bounty_tick_nullsec_ess_dbs_and_player_corp_tax() {
    let (mut ctx, _session, char_id, _ship_id, _highsec_id) = make_fighter_ctx_and_session();
    let null_sys_id = 30_004_759_u32;
    let player_corp_id = 98_500_001_i64;

    let mut sde = (*ctx.sde).clone();
    sde.solar_systems.insert(
        null_sys_id,
        SolarSystem {
            solar_system_id: null_sys_id,
            region_id: 10_000_060,
            constellation_id: 20_000_700,
            name: "NullSys".into(),
            security_status: -0.8,
            radius: 1.0e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: Vec::new(),
            stations: Vec::new(),
            wormhole_class_id: None,
        },
    );
    ctx.sde = Arc::new(sde);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = player_corp_id;
        }
        snap.corporations.insert(
            player_corp_id,
            CorporationRecord {
                corporation_id: player_corp_id,
                name: "NullRatters".into(),
                ticker: "NRAT".into(),
                ceo_id: char_id,
                tax_rate: 0.15, // 15% corp tax
                master_wallet: IskCents::ZERO,
                ..Default::default()
            },
        );
        snap.system_states.insert(
            null_sys_id,
            SystemDynamicState {
                solar_system_id: null_sys_id,
                dbs_multiplier: 1.20,
                ..Default::default()
            },
        );
    });

    with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((null_sys_id, char_id), 1_000_000.0);
    });

    // Pass corp_tax_rate = 0.0 with gross >= 100k -> uses player corp's resolved_tax_rate (0.15)
    let (pilot_isk, main_ess, reserve_ess) = flush_npc_bounty_tick(&ctx, null_sys_id, char_id, 0.0);
    assert!(pilot_isk > 0.0);
    assert!(main_ess > 0.0);
    assert!(reserve_ess >= 0.0);

    ctx.db.with_snapshot(|snap| {
        let corp = snap.corporations.get(&player_corp_id).unwrap();
        assert!(corp.master_wallet.raw_cents() > 0);
        assert_eq!(corp.wallet_divisions[0].balance, corp.master_wallet);
        let st = snap.system_states.get(&null_sys_id).unwrap();
        assert!(st.ess_main_bank.raw_cents() > 0);
        assert!(snap
            .wallet_journal
            .iter()
            .any(|j| j.character_id == char_id && j.entry_type == "bounty_prize_corporation_tax"));
    });
}

#[tokio::test]
async fn test_officer_fallback_faction_prefixes_archetypes_and_abyssal_finalize() {
    let (mut ctx, _session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = (*ctx.sde).clone();

    // Officer NPC whose first name doesn't match any Officer module -> hits fallback to any meta_group 5 module
    let mut unmatched_officer = make_fighter_type(932_001, 559, 11, 5_000.0, vec![]);
    unmatched_officer.name = "ZzzUnmatchedOfficer".into();
    sde.types.insert(932_001, unmatched_officer);
    let mut generic_off_mod = make_fighter_type(932_002, 60, 7, 5.0, vec![]);
    generic_off_mod.name = "Estamel's Modified Adaptive Invulnerability Field".into();
    generic_off_mod.meta_group_id = Some(5);
    sde.types.insert(932_002, generic_off_mod);

    // Remaining 4 faction commander prefixes: Shadow Serpentis, Dark Blood, True Sansha, Domination
    for (idx, prefix) in ["Shadow Serpentis", "Dark Blood", "True Sansha", "Domination"]
        .into_iter()
        .enumerate()
    {
        let npc_tid = 932_010 + (idx as u32) * 2;
        let mod_tid = npc_tid + 1;
        let mut f_npc = make_fighter_type(npc_tid, 550, 11, 2_000.0, vec![]);
        f_npc.name = format!("{prefix} Commander");
        sde.types.insert(npc_tid, f_npc);

        let mut f_mod = make_fighter_type(mod_tid, 60, 7, 5.0, vec![]);
        f_mod.name = format!("{prefix} Armor Plates");
        f_mod.meta_group_id = Some(4);
        sde.types.insert(mod_tid, f_mod);
    }

    // PochvenHypnosianSleeper loot item ("Interstellar Navigation Logs")
    let mut nav_logs = make_fighter_type(932_050, 300, 4, 0.1, vec![]);
    nav_logs.name = "Interstellar Navigation Logs".into();
    sde.types.insert(932_050, nav_logs);

    ctx.sde = Arc::new(sde);

    // Unknown NPC returns None
    assert_eq!(resolve_officer_or_commander_sde_drop(&ctx, 999_999_999), None);
    // Officer fallback returns Some((_, false))
    assert!(resolve_officer_or_commander_sde_drop(&ctx, 932_001).is_some());
    // All 4 faction commander prefixes return their matching faction module
    for idx in 0..4 {
        let npc_tid = 932_010 + (idx as u32) * 2;
        assert!(resolve_officer_or_commander_sde_drop(&ctx, npc_tid).is_some());
    }

    // Test populate_npc_wreck_on_turret_kill for CloneSoldier, Sleeper, PochvenHypnosianSleeper, Triglavian
    let wreck_id = 942_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wreck_id,
            make_fighter_item(wreck_id, 26_557, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });

    for (ball_id, arch) in [
        (952_001_i64, "CloneSoldier"),
        (952_002_i64, "Sleeper"),
        (952_003_i64, "PochvenHypnosianSleeper"),
        (952_004_i64, "Triglavian"),
    ] {
        with_mut_combat_runtime(&ctx, |rt| {
            let mut ent = make_test_npc_entity(ball_id, 932_010, sys_id, arch);
            ent.tag_drop_type_id = Some(28_999);
            ent.custom_cargo_loot = Some((30_001, 3));
            rt.npc_entities.insert(ball_id, ent);
        });
        let mut dropped = Vec::new();
        let ret_arch = populate_npc_wreck_on_turret_kill(
            &ctx,
            sys_id,
            ball_id,
            932_010,
            DVec3::ZERO,
            wreck_id,
            char_id,
            &mut dropped,
        )
        .await;
        assert_eq!(ret_arch, arch);
        assert!(!dropped.is_empty());
    }
    with_combat_runtime(&ctx, |rt| {
        assert!(rt.clone_soldier_cooldown_until.contains_key(&sys_id));
    });

    // Test finalize_cycle_destroyed_npcs:
    // 1) Empty cache early return
    finalize_cycle_destroyed_npcs(&ctx, sys_id).await;
    // 2) Abyssal solar system (32_000_001) branch & Drifter wreck type (37531) in K-space
    let ab_sys = 32_000_001_u32;
    with_mut_combat_runtime(&ctx, |rt| {
        let mut ab_ent = make_test_npc_entity(952_010, 932_010, ab_sys, "Triglavian");
        ab_ent.target_id = Some(char_id);
        rt.destroyed_npc_cache.insert(952_010, ab_ent);

        let mut drifter_ent = make_test_npc_entity(952_011, 932_010, sys_id, "Drifter");
        drifter_ent.threat_table.insert(char_id, 100.0);
        rt.destroyed_npc_cache.insert(952_011, drifter_ent);
    });
    finalize_cycle_destroyed_npcs(&ctx, ab_sys).await;
    finalize_cycle_destroyed_npcs(&ctx, sys_id).await;
    with_combat_runtime(&ctx, |rt| {
        assert!(rt.destroyed_npc_cache.is_empty());
    });

    // record_npc_kill_progression with killer_char_id <= 0 is a safe no-op
    record_npc_kill_progression(&ctx, 0, None);
    record_npc_kill_progression(&ctx, -1, Some(932_010));
}

#[tokio::test]
async fn issue_544_pending_npc_bounty_persists_sqlite_and_flushes_on_downtime() {
    let base_ctx = ServerContext::new();
    let sde = Arc::clone(&base_ctx.sde);
    let tmp_dir = std::env::temp_dir().join(format!(
        "evomu_issue_544_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let db_path = tmp_dir.join("world.sqlite");

    let repo1 = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
    let ctx1 = ServerContext::with_sde_and_repository(Arc::clone(&sde), repo1);
    let char_id = 90_000_001_i64;
    let mut base_char = base_ctx.db.with_snapshot(|s| s.characters[&char_id].clone());
    let player_corp_id = 98_544_001_i64;
    let null_sys_id = 30_004_759_u32;
    let hs_sys_id = 30_000_142_u32;

    let initial_bal = ctx1.db.with_mut_snapshot(|snap| {
        base_char.security_status = 5.0; // old_sec >= 5.0 previously skipped persistence in handle_npc_destroyed_in_space
        base_char.corporation_id = player_corp_id;
        let bal = base_char.balance;
        snap.characters.insert(char_id, base_char);
        snap.corporations.insert(
            player_corp_id,
            CorporationRecord {
                corporation_id: player_corp_id,
                name: "DowntimeRatters".into(),
                ticker: "DTR".into(),
                ceo_id: char_id,
                tax_rate: 0.10,
                master_wallet: IskCents::ZERO,
                ..Default::default()
            },
        );
        snap.system_states.insert(
            null_sys_id,
            SystemDynamicState {
                solar_system_id: null_sys_id,
                dbs_multiplier: 1.25,
                ..Default::default()
            },
        );
        bal
    });

    // Kill normal NPC via handle_npc_destroyed_in_space AND specialized NPC via credit_specialized_npc_bounty_and_sec_status
    let npc_ball_id = 954_001_i64;
    with_mut_combat_runtime(&ctx1, |rt| {
        let mut ent = make_test_npc_entity(npc_ball_id, 932_010, null_sys_id, "Pirate");
        ent.bounty_isk = 800_000.0;
        rt.npc_entities.insert(npc_ball_id, ent);
    });
    handle_npc_destroyed_in_space(
        &ctx1.systems,
        &ctx1.db,
        &ctx1.sde,
        &ctx1.combat_ephemeral,
        null_sys_id,
        npc_ball_id,
        Some(char_id),
    )
    .await;
    crate::combat::crimewatch::credit_specialized_npc_bounty_and_sec_status(
        &ctx1,
        hs_sys_id,
        954_002,
        char_id,
        250_000.0,
    );
    drop(ctx1);

    // Reopen fresh ServerContext from SQLite and verify pending bounty state survived restart
    let repo2 = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = ServerContext::with_sde_and_repository(Arc::clone(&sde), repo2);
    crate::services::combat_crimewatch::with_combat_ephemeral(&ctx2, |rt| {
        assert_eq!(rt.pending_bounty_ticks.get(&(null_sys_id, char_id)).copied(), Some(800_000.0));
        assert_eq!(rt.pending_bounty_ticks.get(&(hs_sys_id, char_id)).copied(), Some(250_000.0));
        assert!(rt.pending_bounty_tick_started_unix.contains_key(&(null_sys_id, char_id)));
        assert!(rt.pending_bounty_tick_started_unix.contains_key(&(hs_sys_id, char_id)));
        assert!(rt.bounty_tick_next_flush_unix.contains_key(&(null_sys_id, char_id)));
        assert!(rt.bounty_tick_next_flush_unix.contains_key(&(hs_sys_id, char_id)));
    });

    // Run daily downtime cleanup -> must flush all pending bounty ticks with corp tax & null-sec ESS split
    let _ = crate::sp6_lifecycle::cleanup_npc_wrecks_and_reset_missions_on_downtime(&ctx2);
    drop(ctx2);

    // Reopen once more to confirm flushed state & wallets persisted cleanly to SQLite
    let repo3 = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("reopen sqlite 3"));
    let ctx3 = ServerContext::with_sde_and_repository(sde, repo3);
    with_combat_runtime(&ctx3, |rt| {
        assert!(rt.pending_bounty_ticks.is_empty());
        assert!(rt.pending_bounty_tick_started_unix.is_empty());
        assert!(rt.bounty_tick_next_flush_unix.is_empty());
    });
    ctx3.db.with_snapshot(|snap| {
        assert!(snap.characters[&char_id].balance > initial_bal);
        let corp = snap.corporations.get(&player_corp_id).unwrap();
        assert!(corp.master_wallet.raw_cents() > 0);
        let sys_st = snap.system_states.get(&null_sys_id).unwrap();
        assert!(sys_st.ess_main_bank.raw_cents() > 0);
        assert!(snap
            .wallet_journal
            .iter()
            .any(|j| j.character_id == char_id && j.entry_type == "bounty_prize_corporation_tax"));
    });

    let _ = std::fs::remove_dir_all(&tmp_dir);
}
