#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{ExtendedNpcEntity, with_mut_combat_runtime};
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::DamageVector;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn make_ext_npc(ball_id: i64, sys_id: u32, bounty_isk: f64, archetype: &str) -> ExtendedNpcEntity {
    ExtendedNpcEntity {
        ball_id,
        type_id: 587,
        faction_id: 500_010,
        archetype: archetype.into(),
        solar_system_id: sys_id,
        spawn_group_id: 0,
        target_id: None,
        aggression_threat: HashMap::new(),
        threat_table: HashMap::new(),
        bounty_isk,
        optimal_range: 30_000.0,
        orbit_range: 10_000.0,
        damage_vector: DamageVector {
            em: 5.0,
            thermal: 5.0,
            kinetic: 5.0,
            explosive: 5.0,
        },
        entropic_cycles: 0,
        drifter_overshield_hp: 0.0,
        drifter_doomsday_fired: false,
        web_speed_factor: 0.0,
        web_range: 0.0,
        scram_strength: 0.0,
        scram_range: 0.0,
        neut_amount: 0.0,
        ecm_jammed: false,
        remote_shield_rep: 0.0,
        remote_armor_rep: 0.0,
        remote_rep_range: 0.0,
        tag_drop_type_id: None,
        spawn_position: DVec3::ZERO,
        sleeper_wh_class: 0,
        custom_cargo_loot: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_destroyed_dedup_wh_zero_bounty_and_wave_advancement_without_npc() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let sys_id = 30_000_142_u32;

    // 1. Missing npc_entities entry still steps encounter waves without panicking
    let spawned_balls = create_custom_encounter_site(
        &ctx,
        88_001,
        sys_id,
        vec![vec![587]],
        DVec3::new(1_000.0, 0.0, 0.0),
    )
    .await;
    if let Some(&bid) = spawned_balls.first() {
        with_mut_combat_runtime(&ctx, |rt| {
            rt.npc_entities.remove(&bid);
        });
        handle_npc_destroyed_in_space(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            &ctx.combat_ephemeral,
            sys_id,
            bid,
            Some(cid),
        )
        .await;
    }

    // 2. Duplicate bounty_credited_balls guard, wormhole (>=31_000_000) skip, bounty_isk <= 0 skip, killer == 0 skip
    let wh_sys = 31_000_005_u32;
    let (wh_npc, zero_bounty_npc, zero_killer_npc, dup_npc) =
        (9_800_001_i64, 9_800_002_i64, 9_800_003_i64, 9_800_004_i64);
    with_mut_combat_runtime(&ctx, |rt| {
        rt.bounty_credited_balls.insert(dup_npc);
        for (bid, s, bounty) in [
            (wh_npc, wh_sys, 100_000.0),
            (zero_bounty_npc, sys_id, 0.0),
            (zero_killer_npc, sys_id, 100_000.0),
            (dup_npc, sys_id, 100_000.0),
        ] {
            rt.npc_entities.insert(bid, make_ext_npc(bid, s, bounty, "Pirate"));
        }
    });

    for (s, bid, killer) in [
        (wh_sys, wh_npc, Some(cid)),
        (sys_id, zero_bounty_npc, Some(cid)),
        (sys_id, zero_killer_npc, Some(0)),
        (sys_id, dup_npc, Some(cid)),
    ] {
        handle_npc_destroyed_in_space(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            &ctx.combat_ephemeral,
            s,
            bid,
            killer,
        )
        .await;
    }

    let has_pending = crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.contains_key(&(sys_id, cid))
            || rt.pending_bounty_ticks.contains_key(&(wh_sys, cid))
    });
    assert!(!has_pending);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bounty_tick_tax_branches_overdue_guard_and_wreck_fallbacks() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let player_corp = 98_880_001_i64;
    let npc_corp = 1_000_035_i64;
    let unknown_sys = 39_999_999_u32;

    // 1. tick_overdue_npc_bounty_payouts without started_unix uses now_unix fallback so delta=0 -> does not flush
    with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_tick_started_unix.clear();
        rt.pending_bounty_ticks.insert((unknown_sys, cid), 50_000.0);
    });
    tick_overdue_npc_bounty_payouts(&ctx, ctx.now_unix() + 10_000);
    let still_pending = crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.contains_key(&(unknown_sys, cid))
    });
    assert!(still_pending);

    // 2. Player corp 0% tax rate -> no corp tax; direct flush_npc_bounty_tick
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            player_corp,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = player_corp; __corp.name = "ZeroTax".into(); __corp.ticker = "ZTX".into(); __corp.ceo_id = cid; __corp.tax_rate = 0.0; __corp.members = HashSet::from([cid]); let __mw = IskCents(0); if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); } __corp },
        );
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = player_corp;
            ch.balance = IskCents(0);
        }
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((unknown_sys, cid), 100_000.0);
    });
    let payout_unk = flush_npc_bounty_tick(&ctx, unknown_sys, cid, 0.0);
    assert!((payout_unk.pilot_isk - 100_000.0).abs() < 1e-3);
    assert_eq!(payout_unk.main_ess_isk, 0.0);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.corporations[&player_corp].master_wallet_balance(), IskCents(0));
    });

    // NPC corp with >= 100k bounty applies 11% tax without player corp wallet credit
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = npc_corp;
            ch.balance = IskCents(0);
        }
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((unknown_sys, cid), 200_000.0);
    });
    let payout_npc = flush_npc_bounty_tick(&ctx, unknown_sys, cid, 0.11);
    assert!((payout_npc.pilot_isk - 178_000.0).abs() < 1e-3);
    assert_eq!(payout_npc.main_ess_isk, 0.0);

    // 3. populate_npc_wreck_on_turret_kill with wreck_id == 0 skips insertion; destroyed_npc_cache fallback
    let cached_ball = 9_810_001_i64;
    let wreck_id = 9_810_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wreck_id,
            InventoryItemRecord {
                item_id: wreck_id,
                type_id: 26_557,
                owner_id: cid,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    with_mut_combat_runtime(&ctx, |rt| {
        let mut ent = make_ext_npc(cached_ball, 30_000_142, 0.0, "Hauler");
        ent.custom_cargo_loot = Some((34, 25));
        rt.destroyed_npc_cache.insert(cached_ball, ent);
    });
    let mut dropped = Vec::new();
    let _ = populate_npc_wreck_on_turret_kill(
        &ctx,
        30_000_142,
        cached_ball,
        587,
        DVec3::ZERO,
        0,
        cid,
        &mut dropped,
    )
    .await;
    assert!(dropped.is_empty());

    with_mut_combat_runtime(&ctx, |rt| {
        let mut ent = make_ext_npc(cached_ball, 30_000_142, 0.0, "Hauler");
        ent.custom_cargo_loot = Some((34, 25));
        rt.destroyed_npc_cache.insert(cached_ball, ent);
    });
    let _ = populate_npc_wreck_on_turret_kill(
        &ctx,
        30_000_142,
        cached_ball,
        587,
        DVec3::ZERO,
        wreck_id,
        cid,
        &mut dropped,
    )
    .await;
    ctx.db.with_snapshot(|snap| {
        assert!(snap.items.values().any(|i| i.location_id == wreck_id && i.type_id == 34));
    });

    assert!(resolve_officer_or_commander_sde_drop(&ctx, 587).is_none());
    finalize_cycle_destroyed_npcs(&ctx, 30_000_144).await;
}
