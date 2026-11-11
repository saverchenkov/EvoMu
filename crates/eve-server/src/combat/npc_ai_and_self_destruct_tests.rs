//! Semantic unit tests split from `salvage_ai_death_tests`.

use super::salvage_ai_death_tests::*;
use super::crimewatch::{
    credit_specialized_npc_bounty_and_sec_status, ConcordExecutionReport, CrimeWatchLock,
};
use super::death_pipeline::{execute_death_pipeline, execute_death_pipeline_sync};
use super::drones_salvage_tractor::execute_wreck_salvage_cycle;
use super::npc_ai::{NpcAiState, NpcSpawnManager};
use eve_db::{
    InventoryFlag, PersistedInsurgencyCampaign, TrainedSkillRecord,
};
use eve_destiny::{Ball, MovementMode};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

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

#[tokio::test]
async fn npc_ai_lowsec_mordu_clone_soldier_officer_fallback_and_drifter() {
    use super::npc_ai::NpcSpawnManager;
    use crate::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime};

    let mut ctx = test_ctx();
    // Pick real solar systems that already contain Asteroid Belts (`group_id == 9`) and Stargates (`group_id == 10`) in SQLite SDE
    let mut rich_sys_ids: Vec<u32> = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .filter(|&sid| sid < 31_000_000)
        .filter(|&sid| {
            let cs = ctx.sde.celestials_in_system(sid);
            let belts = cs.iter().filter(|c| c.group_id == 9).count();
            (1..=2).contains(&belts) && cs.iter().any(|c| c.group_id == 10)
        })
        .take(6)
        .collect();
    rich_sys_ids.sort_unstable();
    assert!(rich_sys_ids.len() >= 5, "expected at least 5 real systems with belts and gates");

    {
        let sde = std::sync::Arc::make_mut(&mut ctx.sde);
        let sample_cid = sde.solar_systems[&rich_sys_ids[0]].constellation_id;
        if let Some(c) = sde.constellations.get(&sample_cid).cloned() {
            let mut c_known = c.clone();
            c_known.constellation_id = 29_099_101;
            c_known.faction_id = Some(500_007);
            sde.constellations.insert(29_099_101, c_known);

            let mut c_unknown = c;
            c_unknown.constellation_id = 29_099_102;
            c_unknown.faction_id = Some(500_999);
            sde.constellations.insert(29_099_102, c_unknown);
        }

        let overrides = [
            (10_000_020u32, 29_099_102u32, 0.35f64),  // Blood Raiders region fallback, Mordu >=0.30 & Clone Soldier
            (10_000_001u32, 29_099_102u32, 0.20f64),  // Sansha region fallback, Mordu >=0.15
            (10_000_027u32, 29_099_102u32, 0.10f64),  // Serpentis region fallback, Mordu <0.15
            (10_000_005u32, 29_099_102u32, -0.50f64), // Angel region fallback, Null-sec Officer + Commander
            (10_000_999u32, 29_099_101u32, 0.25f64),  // Constellation 500_007 fallback
            (10_000_999u32, 29_099_102u32, 0.60f64),  // Unknown region default (500_010)
        ];
        for (&sid, &(rid, cid, sec)) in rich_sys_ids.iter().zip(overrides.iter()) {
            if let Some(sys) = sde.solar_systems.get_mut(&sid) {
                sys.region_id = rid;
                sys.constellation_id = cid;
                sys.security_status = sec;
            }
        }
    }

    let mut mgr = NpcSpawnManager::default();
    let counts = [12usize, 12, 12, 20, 2, 2];
    for (&sid, &iters) in rich_sys_ids.iter().zip(counts.iter()) {
        for _ in 0..iters {
            mgr.entities.clear();
            mgr.respawn_timers.clear();
            mgr.clone_soldier_respawn_timers.clear();
            if with_combat_runtime(&ctx, |rt| rt.clone_soldier_cooldown_until.contains_key(&sid)) {
                with_mut_combat_runtime(&ctx, |rt| rt.clone_soldier_cooldown_until.remove(&sid));
            }
            let spawned = mgr.spawn_belt_rats_for_system(&ctx, sid).await.unwrap();
            assert!(!spawned.is_empty());
        }
    }

    // Active Clone Soldier cooldown skips duplicate Clone Soldier spawn (`npc_ai.rs` cooldown branch)
    with_mut_combat_runtime(&ctx, |rt| {
        rt.clone_soldier_cooldown_until.insert(rich_sys_ids[0], u64::MAX / 2);
    });
    mgr.entities.clear();
    mgr.clone_soldier_respawn_timers.clear();
    let _ = mgr.spawn_belt_rats_for_system(&ctx, rich_sys_ids[0]).await.unwrap();
    assert!(with_combat_runtime(&ctx, |rt| {
        rt.clone_soldier_cooldown_until.get(&rich_sys_ids[0]).copied().unwrap_or(0) > 0
    }));

    // Empty faction-matched npc_spawn_groups + missing clone soldier groups exercise fallback branches
    let mut fallback_ctx = test_ctx();
    {
        let sde = std::sync::Arc::make_mut(&mut fallback_ctx.sde);
        if let Some(sys) = sde.solar_systems.get_mut(&rich_sys_ids[0]) {
            sys.region_id = 10_000_020;
            sys.constellation_id = 29_099_102;
            sys.security_status = 0.30;
        }
        sde.npc_spawn_groups.retain(|_, g| g.faction_id == 0);
        sde.types.retain(|_, t| !matches!(t.group_id, 1174 | 1285 | 1286 | 1287));
    }
    let mut fb_mgr = NpcSpawnManager::default();
    for _ in 0..10 {
        fb_mgr.entities.clear();
        fb_mgr.respawn_timers.clear();
        fb_mgr.clone_soldier_respawn_timers.clear();
        if with_combat_runtime(&fallback_ctx, |rt| {
            !rt.clone_soldier_cooldown_until.is_empty()
        }) {
            with_mut_combat_runtime(&fallback_ctx, |rt| rt.clone_soldier_cooldown_until.clear());
        }
        let _ = fb_mgr.spawn_belt_rats_for_system(&fallback_ctx, rich_sys_ids[0]).await;
    }

    // Drifter wormhole BS spawn branches (group 1530 in C5/C6 vs group 1310 in Thera/other WH)
    let mut wh_mgr = NpcSpawnManager::default();
    for wh_sid in [31_000_005u32, 31_000_099u32] {
        for _ in 0..20 {
            wh_mgr.entities.clear();
            let ids = wh_mgr.spawn_belt_rats_for_system(&ctx, wh_sid).await.unwrap();
            if !ids.is_empty() {
                break;
            }
        }
    }
}

#[tokio::test]
async fn npc_ai_ball_destructions_drone_disconnected_assist_and_sleeper_interval() {
    use super::npc_ai::{
        compute_sleeper_sentry_interval_s, handle_npc_ai_ball_destructions, NpcAiState,
        NpcCombatEntity, NpcSpawnManager,
    };
    use crate::services::combat_crimewatch::{
        with_combat_runtime, with_mut_combat_runtime, AssistLogEntry, DroneSpaceState,
        ExtendedNpcEntity,
    };
    use crate::sp6_lifecycle::{with_mut_sp6_state, DisconnectedShipEntry};

    let mut ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let char_on = 90_000_001i64;
    let char_att = 90_000_002i64;
    let char_disc = 90_000_003i64;
    let char_item_only = 90_094_888i64;
    let ship_on = 100_094_801i64;
    let ship_disc = 100_094_802i64;
    let ship_item = 100_094_803i64;
    let drone_id = 100_094_804i64;
    let ext_npc_id = 100_094_805i64;
    let live_kill_ship = 100_094_806i64;
    let npc_killer_id = 100_094_807i64;
    let now = ctx.now_unix();

    spawn_ship(&ctx, sys_id, ship_on, char_on, 587, DVec3::new(1_000.0, 0.0, 0.0));
    spawn_ship(&ctx, sys_id, ship_disc, char_disc, 587, DVec3::new(2_000.0, 0.0, 0.0));
    spawn_ship(&ctx, sys_id, ship_item, char_item_only, 587, DVec3::new(3_000.0, 0.0, 0.0));
    spawn_ship(&ctx, sys_id, live_kill_ship, char_att, 587, DVec3::new(5_000.0, 0.0, 0.0));

    let handle = ctx.systems.get_or_spawn(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        let mut tmpl = snap.characters[&char_on].clone();
        if let Some(ch_disc) = snap.characters.get_mut(&char_disc) {
            ch_disc.active_ship_id = 0;
        }
        snap.items.insert(
            drone_id,
            make_item(drone_id, 2454, char_on, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            ship_item,
            make_item(ship_item, 587, char_item_only, sys_id as i64, InventoryFlag::None, 1),
        );
        tmpl.character_id = char_item_only;
        tmpl.user_id = 94_888;
        tmpl.name = "ItemOwnerFallback".into();
        tmpl.solar_system_id = sys_id;
        tmpl.active_ship_id = 999_999_001;
        snap.characters.insert(char_item_only, tmpl);
    });
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.disconnected_ships.insert(
            char_disc,
            DisconnectedShipEntry {
                char_id: char_disc,
                ship_id: ship_disc,
                ship_type_id: 587,
                solar_system_id: sys_id,
                last_position: [2_000.0, 0.0, 0.0],
                emergency_warp_dest: None,
                pre_warp_position: None,
                disconnected_at_unix: now,
                remove_at_unix: now + 300,
                removed_from_space: false,
            },
        );
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            drone_id,
            DroneSpaceState {
                ball_id: drone_id,
                drone_item_id: drone_id,
                type_id: 2454,
                group_id: 100,
                owner_char_id: char_on,
                original_owner_char_id: char_on,
                parent_ship_id: ship_on,
                solar_system_id: sys_id,
                activity_state: 0,
                target_id: None,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 1_500.0,
                optimal_range: 5_000.0,
                damage_vector: eve_destiny::DamageVector::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 1.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
        rt.aggression_assist_log.insert(
            ship_on,
            vec![
                AssistLogEntry {
                    attacker_char_id: 0,
                    corporation_id: 1_000_009,
                    alliance_id: None,
                    ship_type_id: 0,
                    weapon_type_id: 0,
                    damage_done: 10.0,
                    is_ewar: false,
                    last_action_unix: now - 10,
                },
                AssistLogEntry {
                    attacker_char_id: char_att,
                    corporation_id: 1_000_009,
                    alliance_id: None,
                    ship_type_id: 587,
                    weapon_type_id: 0,
                    damage_done: 500.0,
                    is_ewar: false,
                    last_action_unix: now - 2,
                },
            ],
        );
        rt.npc_entities.insert(
            ext_npc_id,
            ExtendedNpcEntity {
                ball_id: ext_npc_id,
                type_id: 10_019,
                faction_id: 500_010,
                archetype: "Pirate".into(),
                solar_system_id: sys_id,
                spawn_group_id: 1,
                target_id: None,
                aggression_threat: HashMap::new(),
                threat_table: HashMap::new(),
                bounty_isk: 10_000.0,
                optimal_range: 10_000.0,
                orbit_range: 5_000.0,
                damage_vector: eve_destiny::DamageVector::default(),
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
                custom_cargo_loot: None,
                spawn_position: DVec3::ZERO,
                sleeper_wh_class: 0,
            },
        );
    });

    let make_dead_ball = |id: i64, type_id: u32, pos: DVec3| Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 100.0,
        inertia: 1.0,
        mass: 1_000.0,
        radius: 30.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };

    handle_npc_ai_ball_destructions(
        &ctx,
        sys_id,
        vec![
            (npc_killer_id, drone_id, make_dead_ball(drone_id, 2454, DVec3::ZERO)),
            (
                npc_killer_id,
                ship_on,
                make_dead_ball(ship_on, 587, DVec3::new(1_000.0, 0.0, 0.0)),
            ),
            (
                npc_killer_id,
                ship_disc,
                make_dead_ball(ship_disc, 587, DVec3::new(2_000.0, 0.0, 0.0)),
            ),
            (
                npc_killer_id,
                ship_item,
                make_dead_ball(ship_item, 587, DVec3::new(3_000.0, 0.0, 0.0)),
            ),
            (
                ship_on,
                ext_npc_id,
                make_dead_ball(ext_npc_id, 10_019, DVec3::new(4_000.0, 0.0, 0.0)),
            ),
        ],
    );

    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.active_drones_in_space.contains_key(&drone_id));
        assert!(!rt.npc_entities.contains_key(&ext_npc_id));
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&drone_id));
    });

    // Trigger live kill inside NpcSpawnManager::step_ai_sync (`L845` + `L924`)
    let mut low_hp_cs = eve_destiny::CombatState::default();
    low_hp_cs.max_shield_hp = 10.0;
    low_hp_cs.shield_hp = 0.0;
    low_hp_cs.max_armor_hp = 10.0;
    low_hp_cs.armor_hp = 0.0;
    low_hp_cs.max_structure_hp = 1.0;
    low_hp_cs.structure_hp = 1.0;
    let _ = handle.set_combat_state(live_kill_ship, low_hp_cs).await;
    handle.try_spawn_ball_with_combat(
        make_dead_ball(npc_killer_id, 10_019, DVec3::new(5_500.0, 0.0, 0.0)),
        eve_destiny::CombatState::default(),
    );
    let mut mgr = NpcSpawnManager::default();
    mgr.entities.insert(
        npc_killer_id,
        NpcCombatEntity {
            ball_id: npc_killer_id,
            type_id: 10_019,
            faction_id: 500_010,
            state: NpcAiState::Attack,
            spawn_group_id: 550,
            spawn_pos: DVec3::new(5_500.0, 0.0, 0.0),
            target_id: Some(live_kill_ship),
            bounty_isk: 5_000.0,
            optimal_range: 15_000.0,
            orbit_range: 5_000.0,
            base_dps: 400.0,
            damage_vector: eve_destiny::DamageVector::new(100.0, 100.0, 100.0, 100.0),
            last_attack_tick: 0,
            has_shield_rep: false,
            has_armor_rep: false,
        },
    );
    mgr.step_ai_sync(&ctx, sys_id, 10, 1.0);
    assert!(ctx.db.with_snapshot(|snap| !snap.items.contains_key(&live_kill_ship)));

    // Sleeper sentry interval helper coverage across default and attribute-overridden types
    let base_interval = compute_sleeper_sentry_interval_s(&ctx, 999_991);
    assert!(base_interval >= 1.0);
    {
        let sde = std::sync::Arc::make_mut(&mut ctx.sde);
        let mut t = sde.types[&587].clone();
        t.type_id = 999_992;
        t.default_attributes.insert(51, 6_000.0);
        sde.types.insert(999_992, t);
    }
    let custom_interval = compute_sleeper_sentry_interval_s(&ctx, 999_992);
    assert!((custom_interval - 6.0).abs() < 1e-6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn death_pipeline_self_destruct_guards_nested_fittings_and_escape_frigate() {
    use super::death_pipeline::{
        drain_nested_ship_fittings, execute_live_self_destruct_sync, is_item_descendant_of_in_snap,
    };
    use crate::services::combat_crimewatch::{with_mut_combat_runtime, AssistLogEntry};

    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let char_id = 90_000_001i64;
    let other_char = 90_000_002i64;
    let missing_ship = 100_094_900i64;
    let alien_ship = 100_094_901i64;
    let missing_char_ship = 100_094_902i64;
    let now = ctx.now_unix();

    // Self-destruct validation guard branches (`L1324`, `L1327`, `L1330`)
    assert!(execute_live_self_destruct_sync(&ctx, sys_id, char_id, missing_ship, DVec3::ZERO, now).unwrap().is_none());
    spawn_ship(&ctx, sys_id, alien_ship, other_char, 587, DVec3::ZERO);
    assert!(execute_live_self_destruct_sync(&ctx, sys_id, char_id, alien_ship, DVec3::ZERO, now).unwrap().is_none());
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            missing_char_ship,
            make_item(missing_char_ship, 587, 999_888_777, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    assert!(
        execute_live_self_destruct_sync(&ctx, sys_id, 999_888_777, missing_char_ship, DVec3::ZERO, now)
            .unwrap()
            .is_none()
    );

    // Battleship with Frigate Escape Bay (`group_id == 27`) + nested fitted ship + cycle/orphan checks
    let main_bs = 100_094_910i64;
    let esc_frig = 100_094_911i64;
    let esc_mod = 100_094_912i64;
    let nested_frig = 100_094_913i64;
    let nested_mod = 100_094_914i64;
    let main_mod = 100_094_915i64;
    let cyc_a = 100_094_916i64;
    let cyc_b = 100_094_917i64;
    let orphan = 100_094_918i64;

    spawn_ship(&ctx, sys_id, main_bs, char_id, 641, DVec3::ZERO);
    ctx.db.with_mut_snapshot(|snap| {
        let mut esc_it = make_item(
            esc_frig,
            587,
            char_id,
            main_bs,
            InventoryFlag::from_u16(177),
            1,
        );
        esc_it.singleton = true;
        snap.items.insert(esc_frig, esc_it);
        snap.items.insert(
            nested_frig,
            make_item(nested_frig, 587, char_id, main_bs, InventoryFlag::Cargo, 1),
        );
        snap.items.insert(
            orphan,
            make_item(orphan, 34, char_id, 0, InventoryFlag::None, 1),
        );
        snap.items.insert(
            cyc_a,
            make_item(cyc_a, 34, char_id, cyc_b, InventoryFlag::None, 1),
        );
        snap.items.insert(
            cyc_b,
            make_item(cyc_b, 34, char_id, cyc_a, InventoryFlag::None, 1),
        );
        assert!(!is_item_descendant_of_in_snap(snap, orphan, main_bs));
        assert!(!is_item_descendant_of_in_snap(snap, cyc_a, main_bs));
    });

    // Build real fitting engines via get_or_rebuild_ship_fitting and seed missing-in-snap FittedModules (`L543-567`, `L76-85`)
    let _ = ctx.get_or_rebuild_ship_fitting(char_id, main_bs);
    let _ = ctx.get_or_rebuild_ship_fitting(char_id, esc_frig);
    let _ = ctx.get_or_rebuild_ship_fitting(char_id, nested_frig);
    {
        let mut guard = ctx.ship_fittings.write().unwrap();
        let mk_mod = |item_id: i64| eve_dogma::FittedModule {
            item_id,
            type_id: 2048,
            flag_id: 11,
            state: eve_dogma::ModuleState::Online,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: None,
            charge_quantity: 0,
            dynamic_attribute_overrides: HashMap::new(),
        };
        if let Some(f) = guard.get_mut(&main_bs) {
            f.modules.insert(main_mod, mk_mod(main_mod));
        }
        if let Some(f) = guard.get_mut(&esc_frig) {
            f.modules.insert(esc_mod, mk_mod(esc_mod));
        }
        if let Some(f) = guard.get_mut(&nested_frig) {
            f.modules.insert(nested_mod, mk_mod(nested_mod));
        }
    }

    with_mut_combat_runtime(&ctx, |rt| {
        rt.aggression_assist_log.insert(
            main_bs,
            vec![AssistLogEntry {
                attacker_char_id: other_char,
                corporation_id: 1_000_009,
                alliance_id: None,
                ship_type_id: 587,
                weapon_type_id: 0,
                damage_done: 250.0,
                is_ewar: false,
                last_action_unix: now - 5,
            }],
        );
    });
    let res = execute_live_self_destruct_sync(&ctx, sys_id, char_id, main_bs, DVec3::ZERO, now)
        .unwrap()
        .expect("live self destruct fires");
    assert_eq!(res.ejected_capsule_id, Some(esc_frig));
    assert!(drain_nested_ship_fittings(&ctx, main_bs, false).is_empty());
}

