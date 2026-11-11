//! Behavioral edge-case unit tests for `services::insurgency_complex_ops_ext`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::insurgency_complex_ops::{
    OutpostRaidSize, TYPE_ANGEL_LAUNDERING_FACILITY, TYPE_FEDERATION_CI_OUTPOST_HUB,
    TYPE_GURISTAS_LAUNDERING_FACILITY, TYPE_IMPERIAL_CI_OUTPOST_HUB, TYPE_REPUBLIC_CI_OUTPOST_HUB,
    TYPE_STATE_CI_OUTPOST_HUB,
};
use crate::services::insurgency_services::{FACTION_ANGEL_CARTEL, FACTION_GURISTAS};
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_dogma::ShipFittingEngine;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn make_sde_type(
    type_id: u32,
    group_id: u32,
    faction_id: Option<u32>,
    published: bool,
    shield_cap: f64,
) -> InvType {
    let mut default_attributes = HashMap::new();
    if shield_cap > 0.0 {
        default_attributes.insert(eve_sde::attr_ids::SHIELD_CAPACITY, shield_cap);
    }
    InvType {
        type_id,
        group_id,
        category_id: 11,
        name: format!("Type {type_id}"),
        mass: 1_000_000.0,
        volume: 1_000.0,
        capacity: 0.0,
        radius: 100.0,
        base_price: 0.0,
        published,
        portion_size: 1,
        market_group_id: None,
        race_id: None,
        faction_id,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: None,
        default_effect_id: None,
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes,
    }
}

fn test_ctx_with_sde(sde: SdeRegistry) -> ServerContext {
    ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

#[test]
fn resolve_outpost_defender_and_laundering_crate_cover_all_sizes_and_fallbacks() {
    let mut sde = SdeRegistry::default();
    sde.types.clear();

    // Missing from SDE -> returns fallback
    assert_eq!(
        resolve_sde_outpost_defender_type(&sde, 500_010, OutpostRaidSize::Small, 99_999),
        99_999
    );
    // Crate missing from SDE -> falls back to 78_867
    assert_eq!(
        resolve_laundering_reward_crate_type(&sde, TYPE_ANGEL_LAUNDERING_FACILITY),
        78_867
    );

    // Unpublished or zero-shield types are skipped; lowest valid matching type_id wins
    sde.types.insert(10, make_sde_type(10, 551, Some(500_010), false, 500.0));
    sde.types.insert(11, make_sde_type(11, 551, Some(500_010), true, 0.0));
    sde.types.insert(100, make_sde_type(100, 551, Some(500_010), true, 500.0));
    sde.types.insert(101, make_sde_type(101, 551, Some(500_010), true, 500.0));
    sde.types.insert(200, make_sde_type(200, 553, Some(500_010), true, 800.0));
    sde.types.insert(300, make_sde_type(300, 554, Some(500_010), true, 1200.0));
    sde.types.insert(400, make_sde_type(400, 550, Some(500_010), true, 2500.0));

    assert_eq!(
        resolve_sde_outpost_defender_type(&sde, 500_010, OutpostRaidSize::Small, 999),
        100
    );
    assert_eq!(
        resolve_sde_outpost_defender_type(&sde, 500_010, OutpostRaidSize::Medium, 999),
        200
    );
    assert_eq!(
        resolve_sde_outpost_defender_type(&sde, 500_010, OutpostRaidSize::Moderate, 999),
        300
    );
    assert_eq!(
        resolve_sde_outpost_defender_type(&sde, 500_010, OutpostRaidSize::LargeAdv, 999),
        400
    );

    // Populate all crate types in SDE
    for crate_tid in [78_867_u32, 78_872, 78_873, 78_874, 79_117, 79_118] {
        sde.types
            .insert(crate_tid, make_sde_type(crate_tid, 18, None, true, 0.0));
    }
    assert_eq!(
        resolve_laundering_reward_crate_type(&sde, TYPE_GURISTAS_LAUNDERING_FACILITY),
        78_867
    );
    assert_eq!(
        resolve_laundering_reward_crate_type(&sde, TYPE_ANGEL_LAUNDERING_FACILITY),
        78_872
    );
    assert_eq!(
        resolve_laundering_reward_crate_type(&sde, TYPE_FEDERATION_CI_OUTPOST_HUB),
        78_873
    );
    assert_eq!(
        resolve_laundering_reward_crate_type(&sde, TYPE_IMPERIAL_CI_OUTPOST_HUB),
        78_874
    );
    assert_eq!(
        resolve_laundering_reward_crate_type(&sde, TYPE_STATE_CI_OUTPOST_HUB),
        79_117
    );
    assert_eq!(
        resolve_laundering_reward_crate_type(&sde, TYPE_REPUBLIC_CI_OUTPOST_HUB),
        79_118
    );
}

#[test]
fn notifications_rewards_and_fit_clearing_cover_bc_bonus_lp_and_empty_vs_occupied() {
    let ctx = test_ctx_with_sde(SdeRegistry::default());
    let char_id = ctx.db.with_snapshot(|s| *s.characters.keys().next().unwrap());
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx
        .db
        .acquire_online_lock(uid, 501, Some(char_id), false, ctx.now_unix());

    // 1. Non-battlecruiser group -> no notification
    notify_moderate_outpost_bc_bonus(&ctx, char_id, 500_001, 25, true);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // 2. Battlecruiser group with no trained skill -> defaults to level 1 (4.0%)
    notify_moderate_outpost_bc_bonus(&ctx, char_id, 500_001, 419, true);
    let notifs1 = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notifs1.len(), 1);
    assert_eq!(notifs1[0].event, "OnAttributes");

    // 3. Battlecruiser group with skill level 4 -> 16.0%
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            12_097,
            TrainedSkillRecord {
                type_id: 12_097,
                level: 4,
                skill_points: 45_255.0,
            },
        );
    });
    notify_moderate_outpost_bc_bonus(&ctx, char_id, 500_001, 419, true);
    let notifs2 = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notifs2.len(), 1);
    assert_eq!(notifs2[0].event, "OnAttributes");

    // 4. clear_moderate_outpost_fits: empty prev_occ does not clear fits; non-empty clears & notifies false
    {
        let mut fits = ctx.ship_fittings.write().unwrap();
        fits.insert(500_001, ShipFittingEngine::from_sde(500_001, 24698, &ctx.sde));
    }
    clear_moderate_outpost_fits(&ctx, &[], &[(char_id, 500_001, 419)]);
    assert!(ctx.ship_fittings.read().unwrap().contains_key(&500_001));
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    clear_moderate_outpost_fits(&ctx, &[(char_id, 500_001)], &[(char_id, 500_001, 419)]);
    assert!(!ctx.ship_fittings.read().unwrap().contains_key(&500_001));
    let clear_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(clear_notifs.len(), 1);
    assert_eq!(clear_notifs[0].event, "OnAttributes");

    // 5. reward_aligned_insurgency_pilots (both suppression_scaled=false and true) + notify_insurgency_laundering_reward
    reward_aligned_insurgency_pilots(
        &ctx,
        10,
        30_000_142,
        &[(char_id, 500_001, 500_001, 419, false)],
        1_000,
        15,
        false,
    );
    reward_aligned_insurgency_pilots(
        &ctx,
        10,
        30_000_142,
        &[(char_id, 500_001, 500_001, 419, false)],
        500,
        10,
        true,
    );
    notify_insurgency_laundering_reward(&ctx, char_id, 500_001, 30_000_142);
    let lp_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(lp_notifs.iter().any(|n| n.event == "OnLPChange"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collect_aligned_pilots_on_grid_filters_docked_capsule_cloaked_warping_range_and_detects_wcs() {
    let mut sde = SdeRegistry::default();
    sde.types.insert(587, make_sde_type(587, 25, None, true, 100.0)); // Frigate
    sde.types.insert(10998, make_sde_type(10998, 315, None, true, 0.0)); // Warp Core Stabilizer
    let ctx = test_ctx_with_sde(sde);
    let sys_id = 30_000_142_u32;
    let anchor = DVec3::new(1_000.0, 0.0, 0.0);

    let c_valid_pirate = 91_000_001_i64;
    let c_valid_empire = 91_000_002_i64;
    let c_cloaked = 91_000_003_i64;
    let c_warping = 91_000_004_i64;
    let c_far = 91_000_005_i64;
    let c_docked = 91_000_006_i64;
    let c_capsule = 91_000_007_i64;
    let c_unaligned = 91_000_008_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters.values().next().unwrap().clone();
        let make_char = |cid: i64, ship_id: i64, wf: Option<i64>, docked: bool| {
            let mut ch = base_char.clone();
            ch.character_id = cid;
            ch.solar_system_id = sys_id;
            ch.station_id = if docked { Some(60_003_760) } else { None };
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.war_faction_id = wf;
            ch
        };

        snap.characters.insert(
            c_valid_pirate,
            make_char(c_valid_pirate, 800_001, Some(FACTION_GURISTAS), false),
        );
        snap.characters.insert(
            c_valid_empire,
            make_char(c_valid_empire, 800_002, Some(500_001), false),
        );
        snap.characters.insert(
            c_cloaked,
            make_char(c_cloaked, 800_003, Some(FACTION_GURISTAS), false),
        );
        snap.characters.insert(
            c_warping,
            make_char(c_warping, 800_004, Some(FACTION_GURISTAS), false),
        );
        snap.characters.insert(
            c_far,
            make_char(c_far, 800_005, Some(FACTION_GURISTAS), false),
        );
        snap.characters.insert(
            c_docked,
            make_char(c_docked, 800_006, Some(FACTION_GURISTAS), true),
        );
        snap.characters.insert(
            c_capsule,
            make_char(c_capsule, 800_007, Some(FACTION_GURISTAS), false),
        );
        snap.characters.insert(
            c_unaligned,
            make_char(c_unaligned, 800_008, Some(500_002), false),
        );

        for (shid, owner, tid) in [
            (800_001, c_valid_pirate, 587_u32),
            (800_002, c_valid_empire, 587),
            (800_003, c_cloaked, 587),
            (800_004, c_warping, 587),
            (800_005, c_far, 587),
            (800_006, c_docked, 587),
            (800_007, c_capsule, 670), // capsule excluded
            (800_008, c_unaligned, 587),
        ] {
            snap.items.insert(
                shid,
                InventoryItemRecord {
                    item_id: shid,
                    type_id: tid,
                    owner_id: owner,
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        // Fit WCS (group 315) in low slot (flag 11) on c_valid_pirate's ship
        snap.items.insert(
            810_001,
            InventoryItemRecord {
                item_id: 810_001,
                type_id: 10998,
                owner_id: c_valid_pirate,
                location_id: 800_001,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    for (bid, pos, mode) in [
        (800_001, anchor + DVec3::new(1_000.0, 0.0, 0.0), MovementMode::Stop),
        (800_002, anchor + DVec3::new(2_000.0, 0.0, 0.0), MovementMode::Stop),
        (800_003, anchor + DVec3::new(500.0, 0.0, 0.0), MovementMode::Stop),
        (
            800_004,
            anchor + DVec3::new(500.0, 0.0, 0.0),
            MovementMode::new_warp(anchor, anchor + DVec3::new(100_000_000.0, 0.0, 0.0), 3.0),
        ),
        (800_005, anchor + DVec3::new(100_000.0, 0.0, 0.0), MovementMode::Stop),
        (800_007, anchor + DVec3::new(500.0, 0.0, 0.0), MovementMode::Stop),
        (800_008, anchor + DVec3::new(500.0, 0.0, 0.0), MovementMode::Stop),
    ] {
        let _ = sys
            .spawn_ball(Ball {
                id: bid,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode,
            })
            .await;
    }
    let _ = sys
        .set_combat_state(
            800_003,
            CombatState {
                is_cloaked: true,
                ..CombatState::default()
            },
        )
        .await;

    let pilots = collect_aligned_pilots_on_grid(
        &ctx,
        sys_id,
        anchor,
        30_000.0,
        Some(FACTION_GURISTAS),
        Some(FACTION_GURISTAS),
    )
    .await;
    assert_eq!(pilots.len(), 2);
    assert_eq!(pilots[0], (c_valid_pirate, 800_001, FACTION_GURISTAS, 25, true));
    assert_eq!(pilots[1], (c_valid_empire, 800_002, 500_001, 25, false));

    // Angel Cartel opposing empires are 500_002 | 500_003, so c_unaligned (500_002) matches
    let angel_pilots = collect_aligned_pilots_on_grid(
        &ctx,
        sys_id,
        anchor,
        30_000.0,
        Some(FACTION_ANGEL_CARTEL),
        Some(FACTION_ANGEL_CARTEL),
    )
    .await;
    assert_eq!(angel_pilots.len(), 1);
    assert_eq!(angel_pilots[0].0, c_unaligned);
}
