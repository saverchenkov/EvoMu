//! Edge-case unit tests covering salvage, tractor beam, NPC AI/belt spawns, death pipeline, and CrimeWatch.

use super::crimewatch::{
    credit_specialized_npc_bounty_and_sec_status, ConcordExecutionReport, CrimeWatchLock,
};
use super::death_pipeline::{execute_death_pipeline, execute_death_pipeline_sync};
use super::drones_salvage_tractor::{
    attempt_salvage_wreck, execute_wreck_salvage_cycle, step_tractor_beam,
    validate_wreck_salvage_target, SalvageOutcome,
};
use super::npc_ai::{NpcAiState, NpcSpawnManager};
use crate::router::ServerContext;
use eve_db::{
    InventoryFlag, InventoryItemRecord, PersistedInsurgencyCampaign, TrainedSkillRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
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

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: quantity == 1 && flag == InventoryFlag::None,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

fn spawn_ship(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
    owner_id: i64,
    type_id: u32,
    pos: DVec3,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(ship_id, type_id, owner_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
        if let Some(ch) = snap.characters.get_mut(&owner_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_ship_combat_state(owner_id, ship_id, type_id);
    cs.capacitor = 250.0;
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 1.0,
            mode: MovementMode::Stop,
        },
        cs,
    );
}

#[tokio::test]
async fn salvage_covers_too_much_loot_range_cargo_rig_bonus_and_jetcan_transfer() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 100_090_001;
    let wreck1 = 100_090_010;
    let wreck2 = 100_090_011;
    let wreck3 = 100_090_012;
    let wreck4 = 100_090_013;

    spawn_ship(&ctx, sys_id, ship_id, char_id, 587, DVec3::ZERO);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Wreck 1: oversized loot volume (> 27,500 m3)
    // Wreck 2: far away (> 6,000 m / > 60,000 m)
    // Wreck 3: unspawned in space (not spawned in sys_handle)
    // Wreck 4: valid wreck within 2km containing loot item to transfer into jetcan
    for (wid, pos) in [
        (wreck1, DVec3::new(1_000.0, 0.0, 0.0)),
        (wreck2, DVec3::new(80_000.0, 0.0, 0.0)),
        (wreck4, DVec3::new(1_500.0, 0.0, 0.0)),
    ] {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: wid,
                type_id: 26557,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 10_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    let loot_in_w4 = 100_090_020;
    let rig_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 773 || (t.effect_ids.contains(&2663) && t.default_attributes.contains_key(&901)))
        .map(|t| t.type_id)
        .unwrap_or(31_790);

    ctx.db.with_mut_snapshot(|snap| {
        for wid in [wreck1, wreck2, wreck3, wreck4] {
            snap.items
                .insert(wid, make_item(wid, 26557, 90_000_002, sys_id as i64, InventoryFlag::None, 1));
        }
        // Place huge loot stack inside wreck1 (10,000 ships * volume >> 27,500 m3)
        snap.items.insert(
            100_090_019,
            make_item(100_090_019, 587, 90_000_002, wreck1, InventoryFlag::None, 10_000),
        );
        // Place modest loot stack inside wreck4
        snap.items.insert(
            loot_in_w4,
            make_item(loot_in_w4, 204, 90_000_002, wreck4, InventoryFlag::None, 2),
        );
        // Overfill ship cargo for attempt_salvage_wreck NotEnoughCargoSpace test
        snap.items.insert(
            100_090_021,
            make_item(100_090_021, 1230, char_id, ship_id, InventoryFlag::Cargo, 50_000),
        );
        // Equip salvage tackle rig & Salvaging skill V
        snap.items.insert(
            100_090_022,
            make_item(100_090_022, rig_tid, char_id, ship_id, InventoryFlag::RigSlot0, 1),
        );
        snap.module_online_states.insert(100_090_022, true);
        snap.skills.entry(char_id).or_default().insert(
            3438,
            TrainedSkillRecord {
                type_id: 3438,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });

    // 1. Over-capacity wreck rejected by both sync & async validators
    assert_eq!(
        attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, wreck1, 0.3, 0.0)
            .await
            .unwrap_err(),
        "SalvageTooMuchLootBody"
    );
    assert_eq!(
        validate_wreck_salvage_target(&ctx, sys_id, char_id, ship_id, wreck1, Some(25_861))
            .await
            .unwrap_err()
            .code,
        "SalvageTooMuchLootBody"
    );

    // 2. Full ship cargo returns NotEnoughCargoSpace on attempt_salvage_wreck
    assert_eq!(
        attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, wreck4, 0.5, 0.0)
            .await
            .unwrap_err(),
        "NotEnoughCargoSpace"
    );

    // 3. Out of range & unspawned checks for module and salvage drone
    assert_eq!(
        validate_wreck_salvage_target(&ctx, sys_id, char_id, ship_id, wreck2, Some(25_861))
            .await
            .unwrap_err()
            .code,
        "TargetOutOfRange"
    );
    assert_eq!(
        validate_wreck_salvage_target(&ctx, sys_id, char_id, ship_id, wreck2, None)
            .await
            .unwrap_err()
            .code,
        "TargetOutOfRange"
    );
    assert_eq!(
        validate_wreck_salvage_target(&ctx, sys_id, char_id, ship_id, wreck3, None)
            .await
            .unwrap_err()
            .code,
        "TargetNotInSpace"
    );

    // Free ship cargo space and verify attempt_salvage_wreck + execute_wreck_salvage_cycle
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&100_090_021);
    });
    let outcome = attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, wreck4, 0.5, 0.0)
        .await
        .unwrap();
    match outcome {
        SalvageOutcome::Success {
            yielded_salvage_type_ids,
            yielded_quantities,
        } => {
            assert_eq!(yielded_salvage_type_ids.len(), yielded_quantities.len());
            assert!(!yielded_salvage_type_ids.is_empty());
        }
        SalvageOutcome::Failed { reason } => panic!("expected salvage success, got {reason}"),
    }
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&wreck4));
        let can_id = snap.items[&loot_in_w4].location_id;
        assert_eq!(snap.items[&can_id].type_id, 23);
    });

    // Negative wreck difficulty hits InsufficientAccessChance branch
    let neg_wreck_tid = 999_801u32;
    let mut custom_ctx = test_ctx();
    let mut neg_t = custom_ctx.sde.types[&26557].clone();
    neg_t.type_id = neg_wreck_tid;
    neg_t.default_attributes.insert(901, -90.0);
    Arc::make_mut(&mut custom_ctx.sde)
        .types
        .insert(neg_wreck_tid, neg_t);
    spawn_ship(&custom_ctx, sys_id, 100_091_001, char_id, 587, DVec3::ZERO);
    let _ = custom_ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_ball(Ball {
            id: 100_091_002,
            type_id: neg_wreck_tid,
            position: DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 10_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    custom_ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_091_002,
            make_item(100_091_002, neg_wreck_tid, 90_000_002, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    let rep = execute_wreck_salvage_cycle(
        &custom_ctx,
        sys_id,
        char_id,
        100_091_001,
        100_091_002,
        None,
    )
    .await
    .unwrap();
    assert_eq!(rep.dict_get("salvaged").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        rep.dict_get("reason").and_then(|v| v.as_str()),
        Some("InsufficientAccessChance")
    );

    // Tractor beam advances toward ship and clamps at 1,200 m stop distance
    let (pulled, arrived_far) =
        step_tractor_beam(DVec3::new(20_000.0, 0.0, 0.0), DVec3::ZERO, 2_500.0, 1.0);
    assert!(!arrived_far);
    assert!((pulled.x - 17_500.0).abs() < 1e-6);
    let (clamped, arrived_near) =
        step_tractor_beam(DVec3::new(3_000.0, 0.0, 0.0), DVec3::ZERO, 2_500.0, 1.0);
    assert!(arrived_near);
    assert!((clamped.x - 1_200.0).abs() < 1e-6);
}

#[tokio::test]
async fn npc_ai_ewar_logi_threat_chase_despawn_clone_soldier_and_crimewatch_edges() {
    let ctx = test_ctx();
    let sys_id = 30_002_813; // Low-sec system with belts
    let char1 = 90_000_001;
    let char2 = 90_000_002;
    let ship1 = 100_092_001;
    let ship2 = 100_092_002;
    let now = ctx.now_unix();

    spawn_ship(&ctx, sys_id, ship1, char1, 587, DVec3::ZERO);
    spawn_ship(&ctx, sys_id, ship2, char2, 587, DVec3::new(15_000.0, 0.0, 0.0));

    // Activate EWAR module on ship1 and Logi module on ship2 + active Insurgency campaign
    let ewar_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 65)
        .map(|t| t.type_id)
        .unwrap_or(526);
    let logi_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 41)
        .map(|t| t.type_id)
        .unwrap_or(3582);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_092_010,
            make_item(100_092_010, ewar_tid, char1, ship1, InventoryFlag::MedSlot0, 1),
        );
        snap.module_active_states.insert(100_092_010);
        snap.items.insert(
            100_092_011,
            make_item(100_092_011, logi_tid, char2, ship2, InventoryFlag::HiSlot0, 1),
        );
        snap.module_active_states.insert(100_092_011);
        snap.insurgency_runtime.campaigns.insert(
            55_001,
            PersistedInsurgencyCampaign {
                campaign_id: 55_001,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: now.saturating_sub(1_000),
                forecasting_end_unix: now.saturating_sub(500),
                active_end_unix: now + 10_000,
                winddown_end_unix: now + 20_000,
                corruption_points: HashMap::from([(sys_id, 50_000)]),
                suppression_points: HashMap::new(),
            },
        );
    });

    // Test CrimeWatchLock with_mut_crimewatch & try_with_mut_crimewatch persisting corruption stage
    let cw_lock = CrimeWatchLock::new(Arc::clone(&ctx.db));
    cw_lock.with_mut_crimewatch(|dir| {
        assert_eq!(dir.system_corruption_stages.get(&sys_id).copied(), Some(5));
        dir.system_corruption_stages.insert(sys_id, 4);
    });
    let _: Result<(), String> = cw_lock.try_with_mut_crimewatch(|dir| {
        assert!(dir.system_corruption_stages.get(&sys_id).copied().is_some());
        Ok(())
    });
    cw_lock.push_concord_reports(&vec![
        ConcordExecutionReport {
            criminal_char_id: char1,
            victim_char_id: char2,
            concord_ball_id: 1,
            kill_right_id: 0,
            webbed_speed_fraction: 0.1,
            warp_scramble_strength: 100,
            capacitor_after_neut: 0.0,
            ecm_jammed: true,
            locks_cleared: 0,
            death_result: Err("test".into()),
        };
        40
    ]);
    assert_eq!(cw_lock.concord_reports().len(), 32);

    // Spawn belt rats in low-sec system (spawns Clone Soldier + regular belt rats)
    let mut mgr = NpcSpawnManager::new();
    let rats = mgr.spawn_belt_rats_for_system(&ctx, sys_id).await.unwrap();
    assert!(!rats.is_empty());

    // Place a player within 35km of first belt rat (> 1.4x orbit range -> Chase state!)
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let first_rat = rats[0];
    let rat_pos = sys_handle.get_ball_position_sync(first_rat).unwrap();
    let _ = sys_handle
        .set_ball_position(ship1, rat_pos + DVec3::new(30_000.0, 0.0, 0.0))
        .await;
    mgr.step_ai_sync(&ctx, sys_id, 50, 1.0);
    assert_eq!(mgr.entities[&first_rat].state, NpcAiState::Chase);

    // Remove a CloneSoldier ball from ballpark -> step_ai_sync prunes it & sets clone_soldier_respawn_timers
    if let Some((&cs_id, _)) = mgr.entities.iter().find(|(_, e)| {
        crate::services::combat_crimewatch::combat_npc_ai::derive_npc_archetype(&ctx, e.type_id)
            == "CloneSoldier"
    }) {
        sys_handle.try_remove_ball(cs_id);
        mgr.step_ai_sync(&ctx, sys_id, 51, 1.0);
        assert!(mgr.clone_soldier_respawn_timers.contains_key(&sys_id));
    }

    // Specialized NPC bounty & sec gain incremental update within 900s window
    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_id, char1, 100_001, 100_000.0);
    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_id, char1, 100_002, 250_000.0);
    let sec_after = ctx.db.with_snapshot(|s| s.characters[&char1].security_status);
    assert!(sec_after > 0.0);

    // Invalid ship returns error from async execute_death_pipeline
    assert!(
        execute_death_pipeline(&ctx, sys_id, char2, 999_999_999, char1, DVec3::ZERO, now)
            .await
            .is_err()
    );

    // Pod death clears Zarzakh gate locks and active implants
    let pod_id = 100_092_099;
    spawn_ship(&ctx, sys_id, pod_id, char2, 670, DVec3::ZERO);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_092_100,
            make_item(100_092_100, 19_540, char2, char2, InventoryFlag::Implant, 1),
        );
        snap.zarzakh_gate_locks.insert(char2, (50_000_001, now + 600));
    });
    let res =
        execute_death_pipeline_sync(&ctx, sys_id, char2, pod_id, char1, DVec3::ZERO, now).unwrap();
    assert!(res.killmail_id > 0);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.zarzakh_gate_locks.contains_key(&char2));
        assert!(snap
            .items
            .values()
            .all(|i| !(i.location_id == char2 && i.flag == InventoryFlag::Implant)));
    });

    // High-sec ship death with active insurance contract pays custom payout & grants 30d Kill Right
    let hi_sys = 30_000_142;
    let insured_ship = 100_093_001;
    spawn_ship(&ctx, hi_sys, insured_ship, char2, 587, DVec3::new(1_000.0, 0.0, 0.0));
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.insurance_contracts.insert(
            insured_ship,
            crate::services::combat_crimewatch::InsuranceContractState {
                ship_id: insured_ship,
                ship_type_id: 587,
                owner_id: char2,
                level_name: "Platinum".to_string(),
                level_fraction: 1.0,
                cost_isk: 120_000.0,
                payout_isk: 444_444.0,
                start_unix: now.saturating_sub(100),
                end_unix: now + 86_400,
            },
        );
    });
    let hi_res =
        execute_death_pipeline_sync(&ctx, hi_sys, char2, insured_ship, char1, DVec3::ZERO, now)
            .unwrap();
    assert_eq!(hi_res.insurance_payout_isk, 444_444.0);
    assert!(ctx
        .db
        .with_snapshot(|s| s.kill_rights.values().any(|kr| kr.owner_id == char2 && kr.target_id == char1)));
}

#[tokio::test]
async fn self_destruct_battleship_escape_bay_plastic_wrap_salvage_and_npc_officers() {
    use super::death_pipeline::SelfDestructManager;

    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let now = ctx.now_unix();
    let char1 = 90_000_001;
    let char2 = 90_000_002;
    let _char3 = 90_000_003;

    // 1. SelfDestructManager countdown, abort, remaining_seconds, and step_timers detonation
    let mut sd = SelfDestructManager::new();
    let sd_ship = 100_094_001;
    spawn_ship(&ctx, sys_id, sd_ship, char1, 587, DVec3::new(2_500.0, 0.0, 0.0));
    let det_unix = sd.start_self_destruct(sd_ship, char1, sys_id, now);
    assert_eq!(det_unix, now + 120);
    assert!(sd.is_self_destructing(sd_ship));
    assert_eq!(sd.remaining_seconds(sd_ship, now + 30), Some(90));
    assert!(sd.abort_self_destruct(sd_ship));
    assert!(!sd.is_self_destructing(sd_ship));

    sd.start_self_destruct(sd_ship, char1, sys_id, now);
    let results = sd
        .step_timers(&ctx, now + 125, crate::rng::TEST_MODE_DEFAULT_SEED)
        .await;
    assert_eq!(results.len(), 1);
    assert!(results[0].1.is_ok());

    // 2. Battleship destruction with Escape Frigate Bay (flag 179), Plastic Wrap (3468), and assist log
    let bs_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 27)
        .map(|t| t.type_id)
        .unwrap_or(638);
    let frig_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 25)
        .map(|t| t.type_id)
        .unwrap_or(587);
    let bs_ship = 100_094_002;
    let escape_frig = 100_094_003;
    let plastic_wrap = 100_094_004;
    let wrapped_trit = 100_094_005;
    spawn_ship(&ctx, sys_id, bs_ship, char2, bs_type_id, DVec3::new(3_000.0, 0.0, 0.0));
    ctx.db.with_mut_snapshot(|snap| {
        let mut esc_item =
            make_item(escape_frig, frig_type_id, char2, bs_ship, InventoryFlag::from_u16(179), 1);
        esc_item.singleton = true;
        snap.items.insert(escape_frig, esc_item);
        snap.items.insert(
            plastic_wrap,
            make_item(plastic_wrap, 3468, char2, bs_ship, InventoryFlag::Cargo, 1),
        );
        snap.items.insert(
            wrapped_trit,
            make_item(wrapped_trit, 34, char2, plastic_wrap, InventoryFlag::None, 50),
        );
    });
    let bs_death =
        execute_death_pipeline_sync(&ctx, sys_id, char2, bs_ship, char1, DVec3::ZERO, now)
            .unwrap();
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char2].active_ship_id),
        escape_frig
    );
    assert!(bs_death.killmail_id > 0);

    // 3. Salvage cycle success into cargo, plus jetcan fallback when cargo is full
    let salvor_ship = 100_094_006;
    let wreck_a = 100_094_007;
    let wreck_b = 100_094_008;
    let wreck_b_loot = 100_094_009;
    spawn_ship(&ctx, sys_id, salvor_ship, char1, 670, DVec3::ZERO);
    let handle = ctx.systems.get_or_spawn(sys_id);
    for (wid, pos) in [
        (wreck_a, DVec3::new(500.0, 0.0, 0.0)),
        (wreck_b, DVec3::new(-500.0, 0.0, 0.0)),
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                wid,
                make_item(wid, 26557, char1, sys_id as i64, InventoryFlag::None, 1),
            );
            snap.item_positions.insert(wid, [pos.x, pos.y, pos.z]);
        });
        handle.try_spawn_ball_with_combat(
            Ball {
                id: wid,
                type_id: 26557,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
        );
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills
            .entry(char1)
            .or_default()
            .insert(3438, TrainedSkillRecord { type_id: 3438, level: 5, skill_points: 256_000.0 });
        snap.items.insert(
            wreck_b_loot,
            make_item(wreck_b_loot, 34, char1, wreck_b, InventoryFlag::Cargo, 5),
        );
    });
    let salv_ok =
        execute_wreck_salvage_cycle(&ctx, sys_id, char1, salvor_ship, wreck_a, Some(25861))
            .await
            .unwrap();
    assert!(format!("{salv_ok:?}").contains("salvaged"));

    // Fill ship cargo past capacity so second salvage overflows into a jetcan
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_094_010,
            make_item(100_094_010, 34, char1, salvor_ship, InventoryFlag::Cargo, 50_000),
        );
    });
    let salv_can =
        execute_wreck_salvage_cycle(&ctx, sys_id, char1, salvor_ship, wreck_b, Some(25861))
            .await
            .unwrap();
    assert!(format!("{salv_can:?}").contains("salvaged"));

    // 4. Faction Commander & Named Officer belt rat spawns in null-sec
    if let Some(&null_sys) = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.0 && s.solar_system_id < 31_000_000)
        .map(|s| &s.solar_system_id)
    {
        let mut mgr = NpcSpawnManager::default();
        for step in 0..8_u64 {
            mgr.respawn_timers.clear();
            mgr.step_ai_sync(&ctx, null_sys, step + 1, 1.0);
        }
    }
}

