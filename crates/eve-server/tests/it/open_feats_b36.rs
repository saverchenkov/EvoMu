//! Integration tests for Batch 36 open `feat:pve.*` parity rows (17 rows).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_db::{
    ActiveMissionRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_open_feats_b36::{
    SKILL_MECHANICS, SKILL_NEGOTIATION, SKILL_RESEARCH_PROJECT_MANAGEMENT, SKILL_SCIENCE,
};
use glam::DVec3;

fn kv_map(rep: &PyRep) -> HashMap<String, PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.clone(),
        PyRep::Dict(entries) => entries
            .iter()
            .filter_map(|(k, v)| match k {
                PyRep::String(s) | PyRep::Token(s) => Some((s.clone(), v.clone())),
                _ => None,
            })
            .collect(),
        _ => HashMap::new(),
    }
}

fn set_skill(ctx: &ServerContext, char_id: i64, type_id: u32, level: u8) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            type_id,
            TrainedSkillRecord { type_id, level, skill_points: 256_000.0 },
        );
    });
}

async fn do_start(ctx: &ServerContext, s: &mut SessionState, aid: i64, field_skill: u32) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(s, "agentMgr", None, "DoAction", &[PyRep::Integer(12), PyRep::Integer(aid), PyRep::Integer(field_skill as i64)], &kw, None).await
}

fn dock_at_agent(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, agent: &eve_sde::AgentInfo) {
    sess.solar_system_id = agent.solar_system_id;
    sess.station_id = agent.station_id;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = agent.station_id;
            ch.solar_system_id = agent.solar_system_id.unwrap_or(30_000_142);
        }
    });
}

#[tokio::test]
async fn test_b36_rnd_agent_access_rp_formula_and_datacore_purchase() {
    // parity: feat:pve.research-agent-access-requirements, feat:pve.research-agent-rp-formula, feat:pve.datacore-purchase
    eve_testkit::attest(&[
        "feat:pve.research-agent-access-requirements",
        "feat:pve.research-agent-rp-formula",
        "feat:pve.datacore-purchase",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;

    // Pick a level-4 R&D agent and a second R&D agent from SDE
    let mut rnd_agents: Vec<_> = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 4 && a.station_id.is_some() && a.solar_system_id.is_some())
        .collect();
    rnd_agents.sort_by_key(|a| a.agent_id);
    let l4_agent = rnd_agents
        .iter()
        .find(|a| a.level == 4)
        .copied()
        .expect("SDE level 4 R&D agent");
    let l1_agent = rnd_agents
        .iter()
        .find(|a| a.level == 1 && a.agent_id != l4_agent.agent_id)
        .copied()
        .expect("SDE level 1 R&D agent");

    let mut sess = SessionState::new(3601);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = l4_agent.solar_system_id;
    sess.station_id = None; // Undocked initially

    let field_skill: u32 = 11448; // Minmatar Starship Engineering

    // 1. Undocked refusal
    let err = do_start(&ctx, &mut sess, l4_agent.agent_id, field_skill).await.expect_err("Undocked R&D start must fail");
    assert_eq!(err.code, "AgentNotCoLocated");

    // Dock in agent's station
    dock_at_agent(&ctx, &mut sess, char_id, l4_agent);

    // 2. Missing Science V
    let err = do_start(&ctx, &mut sess, l4_agent.agent_id, field_skill).await.expect_err("Missing Science V must fail");
    assert_eq!(err.code, "ErrorNoRelevantResearchSkills");

    set_skill(&ctx, char_id, SKILL_SCIENCE, 5);
    // 3. Missing Mechanics/PG/CPU V
    let err = do_start(&ctx, &mut sess, l4_agent.agent_id, field_skill).await.expect_err("Missing engineering V must fail");
    assert_eq!(err.code, "ErrorNoRelevantResearchSkills");

    set_skill(&ctx, char_id, SKILL_MECHANICS, 5);
    set_skill(&ctx, char_id, field_skill, 5);
    set_skill(&ctx, char_id, SKILL_NEGOTIATION, 5);
    set_skill(&ctx, char_id, SKILL_RESEARCH_PROJECT_MANAGEMENT, 0);

    // 4. Faction standing 10.0 with corp standing 0.0 must be refused for level 4 agent
    let corp_id = l4_agent.corporation_id;
    let fac_id = ctx
        .sde
        .npc_corporations
        .get(&corp_id)
        .and_then(|c| c.faction_id)
        .unwrap_or(500_001);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(fac_id as i64, 10.0);
            ch.standings.insert(corp_id, 0.0);
            ch.standings.insert(l4_agent.agent_id, 10.0);
        }
    });
    let err = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(12), PyRep::Integer(l4_agent.agent_id), PyRep::Integer(field_skill as i64)], &kw, None)
        .await
        .expect_err("Faction 10 with Corp 0 must fail for L4 R&D agent");
    assert_eq!(err.code, "InsufficientCorporationStanding");

    // 5. Mark mission completed for agent so Connections applies, set corp standing >= 3.0, verify 125.55 RP/day
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(corp_id, 3.0);
        }
        eve_server::services::pve_open_feats_b36::record_agent_mission_completed(
            snap,
            char_id,
            l4_agent.agent_id,
        );
    });
    let res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(12), PyRep::Integer(l4_agent.agent_id), PyRep::Integer(field_skill as i64)], &kw, None)
        .await
        .expect("StartResearch should succeed");
    let kv = kv_map(&res);
    let ppd = kv.get("pointsPerDay").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((ppd - 125.55).abs() < 1e-6, "Expected 125.55 RP/day, got {ppd}");

    // 6. Concurrent agent cap: with RPM 0, max is 1; starting 2nd agent fails until RPM 1
    dock_at_agent(&ctx, &mut sess, char_id, l1_agent);
    let err = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(12), PyRep::Integer(l1_agent.agent_id), PyRep::Integer(field_skill as i64)], &kw, None)
        .await
        .expect_err("2nd R&D agent with RPM 0 must fail");
    assert_eq!(err.code, "MaxResearchAgentsReached");

    set_skill(&ctx, char_id, SKILL_RESEARCH_PROJECT_MANAGEMENT, 1);
    ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(12), PyRep::Integer(l1_agent.agent_id), PyRep::Integer(field_skill as i64)], &kw, None)
        .await
        .expect("2nd R&D agent with RPM 1 should succeed");

    // 7. Advance clock by 2 days -> 251.10 RP accrued -> affords 2 datacores (100 RP + 10,000 ISK each), 3 refused
    dock_at_agent(&ctx, &mut sess, char_id, l4_agent);
    ctx.db.clock().advance_secs(2 * 86_400);
    let journal_rep = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "GetMyJournalDetails", &[], &kw, None)
        .await
        .expect("GetMyJournalDetails");
    let PyRep::Tuple(j_tup) = journal_rep else {
        panic!("Expected Tuple from GetMyJournalDetails");
    };
    let PyRep::List(res_list) = &j_tup[1] else {
        panic!("Expected List of research entries");
    };
    let l4_entry = res_list
        .iter()
        .map(kv_map)
        .find(|m| m.get("agentID").and_then(|v| v.as_i64()) == Some(l4_agent.agent_id))
        .expect("L4 research entry");
    let pts = l4_entry.get("points").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((pts - 251.10).abs() < 1e-4, "Expected 251.10 RP, got {pts}");

    let err_3 = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(14), PyRep::Integer(l4_agent.agent_id), PyRep::Integer(3)], &kw, None)
        .await
        .expect_err("Buying 3 datacores with 251.10 RP must fail");
    assert_eq!(err_3.code, "InsufficientResearchPoints");

    let isk_before = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.balance.0).unwrap_or(0));
    let buy_rep = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(14), PyRep::Integer(l4_agent.agent_id), PyRep::Integer(2)], &kw, None)
        .await
        .expect("Buying 2 datacores should succeed");
    let buy_kv = kv_map(&buy_rep);
    let rem_rp = buy_kv.get("remainingResearchPoints").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((rem_rp - 51.10).abs() < 1e-4);
    let dc_item_id = buy_kv.get("datacoreItemID").and_then(|v| v.as_i64()).unwrap_or(0);
    let (isk_after, dc_qty, dc_loc) = ctx.db.with_snapshot(|s| {
        let bal = s.characters.get(&char_id).map(|c| c.balance.0).unwrap_or(0);
        let it = s.items.get(&dc_item_id).cloned().expect("datacore item");
        (bal, it.quantity, it.location_id)
    });
    assert_eq!(isk_before - isk_after, 2_000_000, "20,000 ISK = 2,000,000 cents");
    assert_eq!(dc_qty, 2);
    assert_eq!(Some(dc_loc), l4_agent.station_id);
}

#[tokio::test]
async fn test_b36_storyline_offer_counter() {
    // parity: feat:pve.storyline-offer-counter
    eve_testkit::attest(&["feat:pve.storyline-offer-counter"]);
    let ctx = ServerContext::new();
    let char_id = 90_000_001_i64;

    // 1. InterBus and militia corps never generate storyline offers
    let interbus_agent = eve_sde::AgentInfo {
        agent_id: 9_999_001,
        name: "InterBus Agent".into(),
        corporation_id: 1000127,
        division_id: 24,
        level: 2,
        agent_type_id: 2,
        is_locator: false,
        station_id: Some(60_003_760),
        solar_system_id: Some(30_000_142),
    };
    ctx.db.with_mut_snapshot(|snap| {
        for _ in 0..16 {
            let res = eve_server::services::pve_open_feats_b36::record_mission_completion_and_resolve_storyline(
                &ctx, snap, char_id, &interbus_agent,
            );
            assert!(res.is_none(), "InterBus must never trigger storyline");
        }
    });

    // 2. Caldari State (faction 500001) high-sec agent triggers nearest high-sec storyline agent on 16th completion
    let caldari_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| {
            a.agent_type_id == 2
                && a.level == 2
                && !eve_server::services::pve_open_feats_b36::is_interbus_or_militia_corp(a.corporation_id)
                && ctx.sde.npc_corporations.get(&a.corporation_id).and_then(|c| c.faction_id) == Some(500_001)
                && a.solar_system_id.and_then(|sid| ctx.sde.solar_systems.get(&sid)).is_some_and(|s| s.security_status >= 0.45)
        })
        .cloned()
        .expect("Caldari L2 highsec agent");

    let first_sl = ctx.db.with_mut_snapshot(|snap| {
        for _ in 0..15 {
            assert!(eve_server::services::pve_open_feats_b36::record_mission_completion_and_resolve_storyline(
                &ctx, snap, char_id, &caldari_agent,
            ).is_none());
        }
        eve_server::services::pve_open_feats_b36::record_mission_completion_and_resolve_storyline(
            &ctx, snap, char_id, &caldari_agent,
        )
        .expect("16th completion must trigger storyline agent")
    });
    let first_sl_info = ctx.sde.agents.get(&first_sl).expect("first storyline agent");
    let first_sl_sec = first_sl_info
        .solar_system_id
        .and_then(|sid| ctx.sde.solar_systems.get(&sid))
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    assert!(first_sl_sec >= 0.45, "Storyline agent must be in high-sec when 16th mission was in high-sec");

    // 3. When nearest storyline agent already has a pending offer, 2nd 16th completion picks second-nearest
    let second_sl = ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, 999_888),
            ActiveMissionRecord {
                mission_id: 999_888, character_id: char_id, agent_id: first_sl,
                solar_system_id: first_sl_info.solar_system_id.unwrap_or(30_000_142),
                state: "Offered".into(), current_room_id: 1, npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
            },
        );
        for _ in 0..15 {
            let _ = eve_server::services::pve_open_feats_b36::record_mission_completion_and_resolve_storyline(
                &ctx, snap, char_id, &caldari_agent,
            );
        }
        eve_server::services::pve_open_feats_b36::record_mission_completion_and_resolve_storyline(
            &ctx, snap, char_id, &caldari_agent,
        )
        .expect("2nd 16th completion must trigger second-nearest storyline agent")
    });
    assert_ne!(first_sl, second_sl, "Must select second-nearest storyline agent when nearest has pending offer");
}

#[tokio::test]
async fn test_b36_abyssal_activation_and_deadspace_pocket_rules() {
    // parity: feat:pve.abyssal-activation-restrictions, feat:pve.deadspace-pocket-rules
    eve_testkit::attest(&[
        "feat:pve.abyssal-activation-restrictions",
        "feat:pve.deadspace-pocket-rules",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let sys_id = 30_002_813_u32; // Low-sec system
    let char_id = 90_000_001_i64;
    let ship_id = 950_001_i64;
    let filament_id = 950_002_i64;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let gate_pos = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_id)
        .map(|j| j.position)
        .unwrap_or(DVec3::ZERO);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id, type_id: 621, owner_id: char_id,
                location_id: sys_id as i64, flag: InventoryFlag::None,
                quantity: 1, singleton: true, ..Default::default()
            },
        );
        snap.items.insert(
            filament_id,
            InventoryItemRecord {
                item_id: filament_id, type_id: 47761, owner_id: char_id,
                location_id: ship_id, flag: InventoryFlag::Cargo,
                quantity: 5, singleton: false, ..Default::default()
            },
        );
    });

    // 1. Within 1,000 km of stargate -> refused
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id, type_id: 621, position: gate_pos + DVec3::new(100_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 10_000_000.0,
                radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await;
    let err = eve_server::services::pve_abyssal_hazards::validate_and_consume_abyssal_filaments(
        &ctx, char_id, ship_id, sys_id, filament_id,
    )
    .await
    .expect_err("Near stargate must refuse filament");
    assert_eq!(err.code, "ProximityRestriction");

    // Move ship far away (> 10,000 km) from all gates/stations
    let far_pos = DVec3::new(50_000_000_000.0, 50_000_000_000.0, 50_000_000_000.0);
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        if let Some(b) = bp.balls.get_mut(&ship_id) {
            b.position = far_pos;
        }
    }

    // 2. Post-warp invulnerability -> refused
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.warp_exit_invulnerable_remaining_ticks = 10;
        }
    }
    let err = eve_server::services::pve_abyssal_hazards::validate_and_consume_abyssal_filaments(
        &ctx, char_id, ship_id, sys_id, filament_id,
    )
    .await
    .expect_err("Post-warp invulnerable must refuse filament");
    assert_eq!(err.code, "ShipInvulnerable");
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.warp_exit_invulnerable_remaining_ticks = 0;
        }
    }

    // 3. Active capsuleer PvP/log-off timer -> refused
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.get_or_create(char_id).pvp_timer_until_unix = ctx.now_unix() + 600;
    }
    let err = eve_server::services::pve_abyssal_hazards::validate_and_consume_abyssal_filaments(
        &ctx, char_id, ship_id, sys_id, filament_id,
    )
    .await
    .expect_err("Active PvP logoff timer must refuse filament");
    assert_eq!(err.code, "PvpTimerActive");
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.get_or_create(char_id).pvp_timer_until_unix = 0;
    }

    // 4. Inside deadspace pocket -> refused with ShipInDungeon
    let err = eve_server::services::pve_abyssal_hazards::validate_and_consume_abyssal_filaments(
        &ctx, char_id, ship_id, 32_000_001, filament_id,
    )
    .await
    .expect_err("Inside deadspace pocket must refuse filament");
    assert_eq!(err.code, "ShipInDungeon");

    // 5. Within 1,000 km of a Mobile Depot -> refused with ProximityRestriction
    let depot_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| eve_server::services::mobile_deployables::is_mobile_structure(&ctx.sde, t.type_id))
        .map(|t| t.type_id)
        .unwrap_or(33_474);
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: 950_010, type_id: depot_type_id, position: far_pos + DVec3::new(50_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1_000.0,
                radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await;
    let err = eve_server::services::pve_abyssal_hazards::validate_and_consume_abyssal_filaments(
        &ctx, char_id, ship_id, sys_id, filament_id,
    )
    .await
    .expect_err("Near mobile depot must refuse filament");
    assert_eq!(err.code, "ProximityRestriction");
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        bp.balls.remove(&950_010);
    }

    // 6. Within 1,000 km of an Abyssal Trace -> refused with ProximityRestriction
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: 950_011, type_id: eve_server::services::pve_abyssal_hazards::ABYSSAL_TRACE_TYPE_ID,
                position: far_pos + DVec3::new(50_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1_000.0,
                radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await;
    let err = eve_server::services::pve_abyssal_hazards::validate_and_consume_abyssal_filaments(
        &ctx, char_id, ship_id, sys_id, filament_id,
    )
    .await
    .expect_err("Near Abyssal trace must refuse filament");
    assert_eq!(err.code, "ProximityRestriction");
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        bp.balls.remove(&950_011);
    }

    // 7. Deadspace pocket rules: MWD disallowed in deadspace pocket, allowed in open space;
    //    warping directly into gated rooms or between distinct pockets is blocked.
    eve_server::services::pve_dungeon_runtime::check_deadspace_mwd_allowed(
        &ctx,
        sys_id,
        far_pos.to_array(),
    )
    .expect("Open space allows MWD");

    let pocket_pos = [1_000_000.0, 0.0, 0.0];
    let room2_pos = [2_000_000.0, 0.0, 0.0];
    let no_mwd_dungeon_id = ctx
        .sde
        .dungeons
        .values()
        .find(|d| d.archetype_id == Some(51))
        .map(|d| d.dungeon_id)
        .unwrap_or(1);
    eve_server::services::pve_dungeon_runtime::with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(
            77_001,
            eve_server::services::pve_dungeon_runtime::DungeonRuntimeInstance {
                instance_id: 77_001,
                dungeon_id: no_mwd_dungeon_id,
                dungeon_name: "Deadspace Complex".into(),
                category: "Combat".into(),
                archetype: "Deadspace".into(),
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: pocket_pos,
                disallow_mwd: true,
                entry_gate_ball_id: None,
                entry_gate_pos: None,
                entry_gate_locked: false,
                current_room_index: 1,
                completed: false,
                despawned: false,
                created_unix: 0,
                rooms: vec![
                    eve_server::services::pve_dungeon_runtime::DungeonRuntimeRoom {
                        room_index: 1,
                        room_id: 1,
                        room_name: "Room 1".into(),
                        position: pocket_pos,
                        gate_destination_pos: room2_pos,
                        gated: false,
                        disallow_mwd: Some(true),
                        gate_ball_id: None,
                        gate_locked: false,
                        objects: Vec::new(),
                        npc_groups: Vec::new(),
                        triggers: Vec::new(),
                        spawned: true,
                        cleared: false,
                    },
                    eve_server::services::pve_dungeon_runtime::DungeonRuntimeRoom {
                        room_index: 2,
                        room_id: 2,
                        room_name: "Room 2 (Gated)".into(),
                        position: room2_pos,
                        gate_destination_pos: room2_pos,
                        gated: true,
                        disallow_mwd: None,
                        gate_ball_id: None,
                        gate_locked: true,
                        objects: Vec::new(),
                        npc_groups: Vec::new(),
                        triggers: Vec::new(),
                        spawned: false,
                        cleared: false,
                    },
                ],
            },
        );
    });

    let err_mwd = eve_server::services::pve_dungeon_runtime::check_deadspace_mwd_allowed(
        &ctx,
        sys_id,
        pocket_pos,
    )
    .expect_err("Deadspace pocket with disallow_mwd must refuse MWD");
    assert_eq!(err_mwd.code, "ModuleNotAllowedInDeadspace");

    let err_warp = eve_server::services::pve_dungeon_runtime::check_deadspace_warp_allowed(
        &ctx,
        sys_id,
        pocket_pos,
        room2_pos,
        room2_pos,
    )
    .expect_err("Warping directly into gated deadspace room must be blocked");
    assert_eq!(err_warp.code, "NaturalPhenomenaDisruptingWarp");
}
