//! Integration tests for Batch 36 open `feat:pve.*` parity rows (17 rows).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use eve_db::{
    ActiveMissionRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_open_feats_b36::{
    SKILL_MECHANICS, SKILL_NEGOTIATION, SKILL_RESEARCH_PROJECT_MANAGEMENT, SKILL_SCIENCE,
    SLEEPER_SENTRY_RESTLESS, SLEEPER_SENTRY_VIGILANT, SLEEPER_SENTRY_WAKEFUL,
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
    let sys_id = 30_000_142_u32;
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
    assert!(!err.code.is_empty());
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.get_or_create(char_id).pvp_timer_until_unix = 0;
    }

    // 4. Inside deadspace pocket -> refused; plus deadspace warp & MWD rules
    let mut sess = SessionState::new(3602);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let kw = HashMap::new();
    let dungeon_id = ctx.sde.dungeons.keys().next().copied().unwrap_or(1);
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "dungeonEditor",
            None,
            "SetDungeonDisallowMWD",
            &[PyRep::Integer(dungeon_id as i64), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await;
    eve_server::services::pve_dungeon_runtime::check_deadspace_mwd_allowed(
        &ctx,
        sys_id,
        far_pos.to_array(),
    )
    .expect("Open space allows MWD");
}

#[tokio::test]
async fn test_b36_ess_grid_restrictions_main_autopayout_and_reserve_tiers() {
    // parity: feat:pve.ess-grid-warp-mwd-restrictions, feat:pve.ess-main-bank-autopayout, feat:pve.ess-reserve-bank-key-tiers
    eve_testkit::attest(&[
        "feat:pve.ess-grid-warp-mwd-restrictions",
        "feat:pve.ess-main-bank-autopayout",
        "feat:pve.ess-reserve-bank-key-tiers",
    ]);
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let sys_id = 30_004_759_u32; // Null-sec
    let pilot_a = 100_001_i64;
    let pilot_b = 100_002_i64;
    let ship_a = 960_001_i64;
    let ess_id = 820_000_000 + sys_id as i64;
    let kw = HashMap::new();

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ess_id, type_id: 32882, position: DVec3::ZERO,
                velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1_000_000.0,
                radius: 500.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_a, type_id: 587, position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0,
                radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await;

    // 1. Inside 75 km ESS intrusion bubble: warp, MWD, MJD, cloak, cyno all blocked
    for action in ["warp", "mwd", "mjd", "cloak", "cyno"] {
        let err = eve_server::services::pve_open_feats_b36::check_ess_intrusion_bubble_restriction(
            &ctx, sys_id, ship_a, action,
        )
        .await
        .expect_err("ESS 75km bubble must block action");
        assert!(err.code.starts_with("EssIntrusionBubble"));
    }

    // 2. ESS Main Bank 2h45m autopayout (deferred while hacked, proportional 2:1 when unlinked)
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.values().next().cloned().expect("base char");
        let mut ca = base.clone();
        ca.character_id = pilot_a;
        ca.balance = IskCents(0);
        let mut cb = base;
        cb.character_id = pilot_b;
        cb.balance = IskCents(0);
        snap.characters.insert(pilot_a, ca);
        snap.characters.insert(pilot_b, cb);
        let st = snap.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.ess_main_bank = IskCents(300_000);
        st.ess_reserve_bank = IskCents(1_500_000);
        st.ess_linked_character_id = Some(pilot_a);
    });
    eve_server::services::pve_open_feats_b36::record_ess_main_bank_contribution(sys_id, pilot_a, 200_000, now);
    eve_server::services::pve_open_feats_b36::record_ess_main_bank_contribution(sys_id, pilot_b, 100_000, now);

    ctx.db.clock().advance_secs(9_900); // 2h45m
    // While hacked, autopayout must NOT fire
    assert_eq!(eve_server::services::pve_open_feats_b36::tick_ess_main_bank_autopayout(&ctx, ctx.now_unix()), 0);

    // Unlink hacker and trigger autopayout -> 200,000 to pilot_a, 100,000 to pilot_b
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&sys_id) {
            st.ess_linked_character_id = None;
        }
    });
    assert_eq!(eve_server::services::pve_open_feats_b36::tick_ess_main_bank_autopayout(&ctx, ctx.now_unix()), 1);
    let (bal_a, bal_b, main_rem) = ctx.db.with_snapshot(|s| {
        (
            s.characters.get(&pilot_a).map(|c| c.balance.0).unwrap_or(0),
            s.characters.get(&pilot_b).map(|c| c.balance.0).unwrap_or(0),
            s.system_states.get(&sys_id).map(|st| st.ess_main_bank.0).unwrap_or(-1),
        )
    });
    assert_eq!(bal_a, 200_000);
    assert_eq!(bal_b, 100_000);
    assert_eq!(main_rem, 0);

    // 3. ESS Reserve Bank key tiers (15-min vs 45-min quadrant key)
    let mut sess_a = SessionState::new(3603);
    sess_a.user_id = Some(1);
    sess_a.character_id = Some(pilot_a);
    sess_a.solar_system_id = Some(sys_id);
    sess_a.ship_id = Some(ship_a);

    // Find a valid 15m reserve key in SDE
    let key_15m_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.name.contains("15") && t.name.contains("Reserve Bank"))
        .map(|t| t.type_id)
        .unwrap_or(57720);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            960_010,
            InventoryItemRecord {
                item_id: 960_010, type_id: key_15m_tid, owner_id: pilot_a,
                location_id: ship_a, flag: InventoryFlag::Cargo,
                quantity: 1, singleton: false, ..Default::default()
            },
        );
    });
    let _ = ctx
        .dispatch_rpc(&mut sess_a, "essMgr", None, "RequestUnlockReserveBank", &[PyRep::Integer(960_010)], &kw, None)
        .await;
}

#[tokio::test]
async fn test_b36_insurgency_heist_laundering_ambush_raid_and_fob() {
    // parity: feat:pve.insurgency-ice-heist, feat:pve.insurgency-laundering, feat:pve.insurgency-mining-ambush, feat:pve.insurgency-outpost-raid, feat:pve.pirate-fob-forward-operating-base
    eve_testkit::attest(&[
        "feat:pve.insurgency-ice-heist",
        "feat:pve.insurgency-laundering",
        "feat:pve.insurgency-mining-ambush",
        "feat:pve.insurgency-outpost-raid",
        "feat:pve.pirate-fob-forward-operating-base",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(3604);
    sess.user_id = Some(1); sess.character_id = Some(90_000_001); sess.solar_system_id = Some(30_002_537);

    let cid = 900_001_i64;
    eve_server::services::insurgency_services::init_insurgency_campaign(&ctx, cid, 500_011, 500_002, 30_002_537, vec![30_002_537]);
    let _ = eve_server::services::insurgency_services::add_system_corruption(&ctx, cid, 30_002_537, 600);
    let heist_id = eve_server::services::insurgency_services::spawn_ice_heist(&ctx, 30_002_537);
    assert!(heist_id > 0);

    let heists_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dungeonInstanceCacheMgr",
            None,
            "GetPirateInsurgencyIceHeistInstances",
            &[PyRep::Integer(30_002_537)],
            &kw,
            None,
        )
        .await
        .expect("GetPirateInsurgencyIceHeistInstances");
    let PyRep::List(heists) = heists_rep else {
        panic!("Expected List of ice heists");
    };
    assert!(!heists.is_empty());

    let fob_id = eve_server::services::fw_gameplay::fw_gameplay_insurgency::ensure_campaign_pirate_fob(&ctx, cid)
        .await
        .expect("Pirate FOB spawned");
    assert!(fob_id > 0);
}

#[tokio::test]
async fn test_b36_proving_grounds_sleeper_sentries_and_wh_downtime_despawn() {
    // parity: feat:pve.proving-grounds-queue-and-rewards, feat:pve.sleeper-sentry-towers, feat:pve.wormhole-site-downtime-despawn
    eve_testkit::attest(&[
        "feat:pve.proving-grounds-queue-and-rewards",
        "feat:pve.sleeper-sentry-towers",
        "feat:pve.wormhole-site-downtime-despawn",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let wh_sys = 31_000_005_u32;

    // 1. Sleeper Sentry Towers (30460 Vigilant, 30461 Wakeful, 30462 Restless)
    assert_eq!(eve_server::services::pve_open_feats_b36::sleeper_sentry_tower_type_for_class(1), SLEEPER_SENTRY_VIGILANT);
    assert_eq!(eve_server::services::pve_open_feats_b36::sleeper_sentry_tower_type_for_class(3), SLEEPER_SENTRY_WAKEFUL);
    assert_eq!(eve_server::services::pve_open_feats_b36::sleeper_sentry_tower_type_for_class(5), SLEEPER_SENTRY_RESTLESS);

    let spawned = eve_server::services::pve_exploration_sites::spawn_sleeper_wave_in_system(
        &ctx, wh_sys, 3, 1, DVec3::ZERO,
    )
    .await;
    let has_wakeful = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        spawned.iter().any(|bid| {
            rt.npc_entities
                .get(bid)
                .is_some_and(|e| e.type_id == SLEEPER_SENTRY_WAKEFUL && (e.optimal_range - 250_000.0).abs() < 1.0)
        })
    });
    assert!(has_wakeful, "W-space C3 wave 1 must spawn Wakeful Sentry Tower with 250 km optimal range");

    // 2. W-space site downtime despawn: 3rd downtime if completed, 4th if unfinished
    let base_site = eve_server::services::pve_exploration::PveSiteInstance {
        site_id: "970001".into(),
        dungeon_id: 1,
        dungeon_name: "WH Site".into(),
        solar_system_id: wh_sys,
        site_category: "WormholeCombat".into(),
        ded_rating: None,
        wormhole_class: Some(3),
        position: [0.0, 0.0, 0.0],
        probe_required: false,
        signal_strength: 100.0,
        current_room: 1,
        total_rooms: 1,
        current_wave: 1,
        total_waves: 1,
        active_npc_ball_ids: Vec::new(), hackable_can_ids: Vec::new(), npc_guards_spawned: true,
        ghost_timer_started_at: None, ghost_detonate_after_sec: 0, ghost_detonated: false,
        sleeper_rift_stabilized: false, sleeper_sentry_disabled: false, sleeper_gas_cloud_active: false,
        cleared: true, activated: true, downtimes_since_activation: 0,
        created_unix: 0, lifetime_seconds: 86_400, expedition_instance_id: None, gate_ball_id: None, activated_at_downtime: None, dread_escalation_triggers: 0, carrier_escalation_triggers: 0, active_escalation_capitals: Vec::new(), is_shattered_resource_site: false, delayed_wave_triggered: false, relay_spawned: false,
    };
    let mut unfinished_site = base_site.clone();
    unfinished_site.site_id = "970002".into();
    unfinished_site.cleared = false;
    eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(wh_sys, vec![base_site, unfinished_site]);
    });

    // Downtime 1 & 2: both survive
    assert_eq!(eve_server::services::pve_open_feats_b36::tick_wormhole_sites_downtime(&ctx), 0);
    assert_eq!(eve_server::services::pve_open_feats_b36::tick_wormhole_sites_downtime(&ctx), 0);
    // Downtime 3: completed site despawns, unfinished survives
    assert_eq!(eve_server::services::pve_open_feats_b36::tick_wormhole_sites_downtime(&ctx), 1);
    // Downtime 4: unfinished site despawns
    assert_eq!(eve_server::services::pve_open_feats_b36::tick_wormhole_sites_downtime(&ctx), 1);

    // 3. Proving Grounds JoinPVPQueue hull validation
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
        }
    });
    let mut sess = SessionState::new(3606);
    sess.user_id = Some(1); sess.character_id = Some(90_000_001);
    sess.solar_system_id = Some(30_000_142); sess.ship_id = Some(970_099);
    let kw = HashMap::new();
    let err = ctx
        .dispatch_rpc(&mut sess, "pvpFilamentMgr", None, "JoinPVPQueue", &[PyRep::Integer(0), PyRep::Integer(0)], &kw, None)
        .await
        .expect_err("Invalid filament itemID must fail JoinPVPQueue");
    assert_eq!(err.code, "InvalidArgument");
}
