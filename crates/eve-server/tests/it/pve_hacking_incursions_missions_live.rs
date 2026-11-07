//! Live integration tests for PvE Hacking Node Types and Hacking Failure Outcomes.

use crate::common::{login_pilot, slash_cmd, LivePilot};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_exploration::{with_pve_exploration_reg, HackingNodeKind};
use eve_server::services::pve_exploration_seeding::ensure_system_pve_sites_seeded;
use eve_server::services::pve_hacking_board::build_hacking_grid_procedural_with_sde;
use eve_server::services::pve_hacking_sites::are_hacking_tiles_adjacent;
use glam::DVec3;
use std::collections::{HashMap, HashSet};

const JITA_SYS: u32 = 30_000_142;
const NULL_SYS: u32 = 30_004_759;
const TYPE_VELATOR: u32 = 606;
const TYPE_DATA_ANALYZER_I: u32 = 22_175;
const SKILL_HACKING: u32 = 21_718;
const SKILL_ARCHAEOLOGY: u32 = 13_278;

async fn gm_hacker_in_space(ctx: &ServerContext, sess_id: u64, char_id: i64) -> LivePilot {
    let kw = HashMap::new();
    let user = format!("live{char_id}");
    let pass = format!("{user}-secret");
    ctx.db
        .provision_gm_account(&user, &pass, char_id, &format!("Live Pilot {char_id}"));
    let mut sess = SessionState::new(sess_id);
    login_pilot(ctx, &mut sess, &user, &pass).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    slash_cmd(ctx, &mut sess, &format!("/giveskill {SKILL_HACKING} 5")).await;
    slash_cmd(ctx, &mut sess, &format!("/giveskill {SKILL_ARCHAEOLOGY} 5")).await;
    let required: Vec<u32> = ctx
        .sde
        .types
        .get(&TYPE_VELATOR)
        .map(|t| {
            [182_u16, 183, 184, 1285, 1289, 1290]
                .iter()
                .filter_map(|a| t.default_attributes.get(a))
                .map(|v| v.round() as u32)
                .filter(|&s| s > 0)
                .collect()
        })
        .unwrap_or_default();
    for skill_id in required {
        slash_cmd(ctx, &mut sess, &format!("/giveskill {skill_id} 5")).await;
    }
    let ship_rep = slash_cmd(ctx, &mut sess, &format!("/createitem {TYPE_VELATOR} 1")).await;
    let ship_id = ship_rep.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    let analyzer_rep =
        slash_cmd(ctx, &mut sess, &format!("/createitem {TYPE_DATA_ANALYZER_I} 1")).await;
    let analyzer_id = analyzer_rep.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(31);
    ctx.dispatch_rpc(&mut sess, "ship", None, "Board", &[PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect("ship.Board");
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(analyzer_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(19),
        ],
        &kw,
        None,
    )
    .await
    .expect("fit Data Analyzer I in MedSlot0");
    ctx.advance_secs(31);
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("stationSvc.Undock");
    sess.ship_id = Some(ship_id);
    let bind = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Integer(i64::from(JITA_SYS)), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("beyonce.MachoBindObject");
    let PyRep::Tuple(bound) = bind else { panic!() };
    let beyonce = bound[0].as_i64().unwrap() as u64;
    ctx.advance_secs(31);
    let sys0 = ctx.systems.get_or_spawn(JITA_SYS);
    for _ in 0..35 {
        sys0.step_manual(1.0).await.unwrap();
    }
    let _ = sess.drain_notifications();
    let _ = ctx.drain_notifications_for_char(char_id);
    LivePilot {
        sess,
        char_id,
        ship_id,
        beyonce,
    }
}

async fn move_pilot_to(ctx: &ServerContext, p: &mut LivePilot, pos: DVec3) {
    let sys_id = p.sess.solar_system_id.expect("in space");
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.set_ball_position(p.ship_id, pos).await;
    sys.step_manual(0.1).await.unwrap();
}

// ============================================================================
// 1. Hacking Node Types & Self-Repair Mechanics (feat:pve.hacking-node-types)
// ============================================================================
#[tokio::test]
async fn test_live_pve_hacking_node_types_and_self_repair_mechanics() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    // 1. Verify difficulty gating: diff 1 & 2 never spawn RestorationNode or VirusSuppressor;
    // diff 4 spawns all V24 node types and scales SystemCore coherence.
    let mut diff1_core_coh = 0i32;
    let mut diff4_core_coh = 0i32;
    let mut seen_kinds = HashSet::new();
    for seed in 1_u64..=15 {
        for diff in [1_u8, 2_u8] {
            let tiles = build_hacking_grid_procedural_with_sde(
                Some(&ctx.sde),
                500_001,
                None,
                false,
                diff,
                seed,
            );
            assert!(
                !tiles.iter().any(|t| matches!(
                    t.kind,
                    HackingNodeKind::RestorationNode | HackingNodeKind::VirusSuppressor
                )),
                "Difficulty {diff} must not spawn RestorationNode or VirusSuppressor"
            );
            if diff == 1 && seed == 1 {
                diff1_core_coh = tiles
                    .iter()
                    .find(|t| t.kind == HackingNodeKind::SystemCore)
                    .map(|t| t.coherence)
                    .unwrap_or(0);
            }
        }
        let tiles4 = build_hacking_grid_procedural_with_sde(
            Some(&ctx.sde),
            500_002,
            None,
            false,
            4,
            seed,
        );
        for t in &tiles4 {
            seen_kinds.insert(format!("{:?}", t.kind));
            if t.kind == HackingNodeKind::SystemCore && seed == 1 {
                diff4_core_coh = t.coherence;
            }
        }
    }
    assert!(
        diff4_core_coh > diff1_core_coh,
        "Difficulty 4 core coherence ({diff4_core_coh}) must exceed difficulty 1 ({diff1_core_coh})"
    );
    for expected in [
        "SystemCore",
        "Firewall",
        "Antivirus",
        "RestorationNode",
        "VirusSuppressor",
        "DataCache",
        "UtilitySubsystem",
        "Empty",
    ] {
        assert!(
            seen_kinds.contains(expected),
            "Difficulty 4 boards must include {expected}, saw {seen_kinds:?}"
        );
    }

    // 2. Live minigame in space: fit Data Analyzer I, fly to Data site container,
    // start difficulty-4 board, walk adjacent tiles, verify distanceIndicator 1..=5,
    // and exercise SelfRepair (+5..=+10/turn for 3 turns, uncapped above max_virus_coherence).
    let mut p = gm_hacker_in_space(&ctx, 9701, 97_001_001).await;
    ensure_system_pve_sites_seeded(&ctx, JITA_SYS).await;

    let (can_id, can_pos) = with_pve_exploration_reg(&ctx, |reg| {
        let site = reg.sites_by_system[&JITA_SYS]
            .iter()
            .find(|s| s.site_category == "Data" && s.ghost_detonate_after_sec == 0 && !s.hackable_can_ids.is_empty())
            .expect("seeded Data site in Jita");
        let cid = site.hackable_can_ids[0];
        let pos = ctx
            .db
            .with_snapshot(|s| s.item_positions.get(&cid).copied())
            .unwrap_or(site.position);
        (cid, DVec3::from_array(pos))
    });

    move_pilot_to(&ctx, &mut p, can_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;

    let start_rep = ctx
        .dispatch_rpc(
            &mut p.sess,
            "hackingMgr",
            None,
            "StartNewGameInstance",
            &[
                PyRep::Integer(can_id),
                PyRep::None,
                PyRep::None,
                PyRep::None,
                PyRep::Integer(4),
            ],
            &kw,
            None,
        )
        .await
        .expect("StartNewGameInstance");
    assert_eq!(start_rep.dict_get("status").and_then(|v| v.as_str()), Some("InProgress"));
    let grid_size = start_rep.dict_get("gridSize").and_then(|v| v.as_i64()).unwrap() as u32;
    let start_dist = start_rep.dict_get("distanceIndicator").and_then(|v| v.as_i64()).unwrap();
    assert!((1..=5).contains(&start_dist), "distanceIndicator must be in 1..=5, got {start_dist}");

    // Empty utility slot must reject UsedUtilityElementOnVirus
    let empty_util_err = ctx
        .dispatch_rpc(
            &mut p.sess,
            "hackingMgr",
            None,
            "UsedUtilityElementOnVirus",
            &[PyRep::Integer(can_id), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await;
    assert!(empty_util_err.is_err(), "Empty utility slot must error");

    // Walk adjacent tiles via ClickedOnTile, checking distanceIndicator 1..=5 and testing utilities
    let mut revealed: HashSet<u32> = HashSet::from([0]);
    let mut non_blocking: HashSet<u32> = HashSet::from([0]);
    let mut next_slot_to_try = 0usize;

    for _step in 0..grid_size {
        let Some(next_tile) = (0..grid_size).find(|&t| {
            !revealed.contains(&t)
                && non_blocking
                    .iter()
                    .any(|&adj| are_hacking_tiles_adjacent(adj, t))
        }) else {
            break;
        };

        let turn = ctx
            .dispatch_rpc(
                &mut p.sess,
                "hackingMgr",
                None,
                "ClickedOnTile",
                &[PyRep::Integer(can_id), PyRep::Integer(next_tile as i64)],
                &kw,
                None,
            )
            .await
            .expect("ClickedOnTile");
        revealed.insert(next_tile);

        let dist = turn.dict_get("distanceIndicator").and_then(|v| v.as_i64()).unwrap();
        assert!((1..=5).contains(&dist), "distanceIndicator must be 1..=5, got {dist}");

        let kind = turn.dict_get("nodeKind").and_then(|v| v.as_str()).unwrap_or("");
        let node_coh = turn.dict_get("nodeCoherence").and_then(|v| v.as_i64()).unwrap_or(0);
        let status = turn.dict_get("status").and_then(|v| v.as_str()).unwrap_or("");
        if status != "InProgress" {
            break;
        }
        let is_blocker = matches!(
            kind,
            "Firewall" | "Antivirus" | "RestorationNode" | "VirusSuppressor"
        ) && node_coh > 0;
        if !is_blocker {
            non_blocking.insert(next_tile);
        }

        if matches!(kind, "UtilitySubsystem" | "DataCache") && next_slot_to_try < 3 {
            let slot = next_slot_to_try;
            next_slot_to_try += 1;
            if let Ok(util_rep) = ctx
                .dispatch_rpc(
                    &mut p.sess,
                    "hackingMgr",
                    None,
                    "UsedUtilityElementOnVirus",
                    &[PyRep::Integer(can_id), PyRep::Integer(slot as i64)],
                    &kw,
                    None,
                )
                .await
            {
                let util_name = util_rep
                    .dict_get("utilityUsed")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if util_name == "SelfRepair" {
                    let mut prev_coh = turn.dict_get("virusCoherence").and_then(|v| v.as_i64()).unwrap();
                    for _ in 0..3 {
                        let Some(safe_t) = (0..grid_size).find(|&t| {
                            !revealed.contains(&t)
                                && non_blocking.iter().any(|&adj| are_hacking_tiles_adjacent(adj, t))
                        }) else {
                            break;
                        };
                        let t_rep = ctx
                            .dispatch_rpc(
                                &mut p.sess,
                                "hackingMgr",
                                None,
                                "ClickedOnTile",
                                &[PyRep::Integer(can_id), PyRep::Integer(safe_t as i64)],
                                &kw,
                                None,
                            )
                            .await
                            .expect("ClickedOnTile during SelfRepair");
                        revealed.insert(safe_t);
                        let t_kind = t_rep.dict_get("nodeKind").and_then(|v| v.as_str()).unwrap_or("");
                        let t_coh = t_rep.dict_get("nodeCoherence").and_then(|v| v.as_i64()).unwrap_or(0);
                        let t_status = t_rep.dict_get("status").and_then(|v| v.as_str()).unwrap_or("");
                        let new_coh = t_rep.dict_get("virusCoherence").and_then(|v| v.as_i64()).unwrap();
                        if !matches!(
                            t_kind,
                            "Firewall" | "Antivirus" | "RestorationNode" | "VirusSuppressor"
                        ) || t_coh <= 0
                        {
                            non_blocking.insert(safe_t);
                        }
                        if t_kind == "Empty" && t_status == "InProgress" {
                            let delta = new_coh - prev_coh;
                            assert!(
                                (5..=10).contains(&delta),
                                "SelfRepair must heal +5..=+10/turn uncapped, got delta={delta}"
                            );
                        }
                        prev_coh = new_coh;
                        if t_status != "InProgress" {
                            break;
                        }
                    }
                }
            }
        }
    }
}

// ============================================================================
// 2. Hacking Failure Outcomes & Ghost Detonation (feat:pve.hacking-failure-outcome)
// ============================================================================
#[tokio::test]
async fn test_live_pve_hacking_failure_outcomes_and_ghost_detonation() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut p = gm_hacker_in_space(&ctx, 9702, 97_001_002).await;
    ensure_system_pve_sites_seeded(&ctx, JITA_SYS).await;
    ensure_system_pve_sites_seeded(&ctx, NULL_SYS).await;

    let (can1_id, can1_pos, can2_id, can2_pos, ghost_can_id, ghost_pos) =
        with_pve_exploration_reg(&ctx, |reg| {
            let std_null_site = reg.sites_by_system[&NULL_SYS]
                .iter()
                .find(|s| s.site_category == "Data" && s.ghost_detonate_after_sec == 0 && !s.hackable_can_ids.is_empty())
                .expect("seeded Nullsec Data site with >=1 can");
            let c1 = std_null_site.hackable_can_ids[0];
            let std_jita_site = reg.sites_by_system[&JITA_SYS]
                .iter()
                .find(|s| s.site_category == "Data" && s.ghost_detonate_after_sec == 0 && !s.hackable_can_ids.is_empty())
                .expect("seeded Jita Data site with >=1 can");
            let c2 = std_jita_site.hackable_can_ids[0];
            let ghost_site = reg.sites_by_system[&JITA_SYS]
                .iter()
                .find(|s| s.ghost_detonate_after_sec > 0 && !s.hackable_can_ids.is_empty())
                .expect("seeded Ghost site");
            let gc = ghost_site.hackable_can_ids[0];
            let pos_of = |cid: i64, fallback: [f64; 3]| {
                DVec3::from_array(
                    ctx.db
                        .with_snapshot(|s| s.item_positions.get(&cid).copied())
                        .unwrap_or(fallback),
                )
            };
            (
                c1,
                pos_of(c1, std_null_site.position),
                c2,
                pos_of(c2, std_jita_site.position),
                gc,
                DVec3::from_array(ghost_site.position),
            )
        });

    async fn fail_current_hack(
        ctx: &ServerContext,
        p: &mut LivePilot,
        can_id: i64,
        grid_size: u32,
    ) -> PyRep {
        let kw = HashMap::new();
        let mut revealed: HashSet<u32> = HashSet::from([0]);
        let mut non_blocking: HashSet<u32> = HashSet::from([0]);
        loop {
            let target = (0..grid_size)
                .find(|&t| {
                    !revealed.contains(&t)
                        && non_blocking.iter().any(|&adj| are_hacking_tiles_adjacent(adj, t))
                })
                .expect("accessible tile to fail hack");
            let rep = ctx
                .dispatch_rpc(
                    &mut p.sess,
                    "hackingMgr",
                    None,
                    "ClickedOnTile",
                    &[PyRep::Integer(can_id), PyRep::Integer(target as i64)],
                    &kw,
                    None,
                )
                .await
                .expect("ClickedOnTile");
            revealed.insert(target);
            if rep.dict_get("status").and_then(|v| v.as_str()) == Some("Failed") {
                return rep;
            }
            let kind = rep.dict_get("nodeKind").and_then(|v| v.as_str()).unwrap_or("");
            let coh = rep.dict_get("nodeCoherence").and_then(|v| v.as_i64()).unwrap_or(0);
            let is_blocker = matches!(
                kind,
                "Firewall" | "Antivirus" | "RestorationNode" | "VirusSuppressor"
            ) && coh > 0;
            if is_blocker {
                loop {
                    let rep2 = ctx
                        .dispatch_rpc(
                            &mut p.sess,
                            "hackingMgr",
                            None,
                            "ClickedOnTile",
                            &[PyRep::Integer(can_id), PyRep::Integer(target as i64)],
                            &kw,
                            None,
                        )
                        .await
                        .expect("ClickedOnTile on blocker");
                    if rep2.dict_get("status").and_then(|v| v.as_str()) == Some("Failed") {
                        return rep2;
                    }
                    if rep2.dict_get("nodeCoherence").and_then(|v| v.as_i64()).unwrap_or(0) <= 0 {
                        non_blocking.insert(target);
                        break;
                    }
                }
            } else {
                non_blocking.insert(target);
            }
        }
    }

    // 1. Standard container: 1st failure leaves can alive; 2nd failure destroys can with 0 ship damage & locks out
    slash_cmd(&ctx, &mut p.sess, &format!("/giveskill {SKILL_HACKING} 0")).await;
    slash_cmd(&ctx, &mut p.sess, &format!("/tr {NULL_SYS}")).await;
    move_pilot_to(&ctx, &mut p, can1_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;
    let s1 = ctx
        .dispatch_rpc(
            &mut p.sess,
            "hackingMgr",
            None,
            "StartNewGameInstance",
            &[PyRep::Integer(can1_id), PyRep::None, PyRep::None, PyRep::None, PyRep::Integer(5)],
            &kw,
            None,
        )
        .await
        .expect("StartNewGameInstance 1");
    let gs1 = s1.dict_get("gridSize").and_then(|v| v.as_i64()).unwrap() as u32;
    let fail1 = fail_current_hack(&ctx, &mut p, can1_id, gs1).await;
    assert_eq!(fail1.dict_get("containerExploded"), Some(&PyRep::Bool(false)));
    assert_eq!(fail1.dict_get("explosionDamage").and_then(|v| v.as_f64()), Some(0.0));

    assert!(
        ctx.db.with_snapshot(|s| s.items.contains_key(&can1_id)),
        "Container item must survive 1st hack failure"
    );

    let s2 = ctx
        .dispatch_rpc(
            &mut p.sess,
            "hackingMgr",
            None,
            "StartNewGameInstance",
            &[PyRep::Integer(can1_id), PyRep::None, PyRep::None, PyRep::None, PyRep::Integer(5)],
            &kw,
            None,
        )
        .await
        .expect("StartNewGameInstance 2");
    let gs2 = s2.dict_get("gridSize").and_then(|v| v.as_i64()).unwrap() as u32;
    let fail2 = fail_current_hack(&ctx, &mut p, can1_id, gs2).await;
    assert_eq!(fail2.dict_get("containerExploded"), Some(&PyRep::Bool(true)));
    assert_eq!(fail2.dict_get("containerDestroyed"), Some(&PyRep::Bool(true)));
    assert_eq!(fail2.dict_get("explosionDamage").and_then(|v| v.as_f64()), Some(0.0));
    assert!(
        !ctx.db.with_snapshot(|s| s.items.contains_key(&can1_id)),
        "Container item must be deleted after 2nd hack failure"
    );
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let cs_after_std = sys.get_combat_state(p.ship_id).await.unwrap().unwrap();
    assert!(
        (cs_after_std.shield_hp - cs_after_std.max_shield_hp).abs() < 1e-3,
        "Standard container explosion must deal 0 ship damage"
    );
    let lockout_err = ctx
        .dispatch_rpc(
            &mut p.sess,
            "hackingMgr",
            None,
            "StartNewGameInstance",
            &[PyRep::Integer(can1_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(lockout_err.code, "HackingGameAlreadyFinished");

    // 2. Standard container Core Kill: unlocks & spews loot
    slash_cmd(&ctx, &mut p.sess, &format!("/tr {JITA_SYS}")).await;
    slash_cmd(&ctx, &mut p.sess, &format!("/giveskill {SKILL_HACKING} 5")).await;
    move_pilot_to(&ctx, &mut p, can2_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;
    let s_win = ctx
        .dispatch_rpc(
            &mut p.sess,
            "hackingMgr",
            None,
            "StartNewGameInstance",
            &[PyRep::Integer(can2_id), PyRep::None, PyRep::None, PyRep::None, PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("StartNewGameInstance win");
    let gs_win = s_win.dict_get("gridSize").and_then(|v| v.as_i64()).unwrap() as u32;
    let mut revealed_w: HashSet<u32> = HashSet::from([0]);
    let mut non_blocking_w: HashSet<u32> = HashSet::from([0]);
    let mut won_rep = None;
    for _ in 0..(gs_win * 3) {
        let Some(next_t) = (0..gs_win).find(|&t| {
            !revealed_w.contains(&t)
                && non_blocking_w.iter().any(|&adj| are_hacking_tiles_adjacent(adj, t))
        }) else {
            break;
        };
        let rep = ctx
            .dispatch_rpc(
                &mut p.sess,
                "hackingMgr",
                None,
                "ClickedOnTile",
                &[PyRep::Integer(can2_id), PyRep::Integer(next_t as i64)],
                &kw,
                None,
            )
            .await
            .expect("ClickedOnTile win");
        revealed_w.insert(next_t);
        if rep.dict_get("status").and_then(|v| v.as_str()) == Some("Won") {
            won_rep = Some(rep);
            break;
        }
        let kind = rep.dict_get("nodeKind").and_then(|v| v.as_str()).unwrap_or("");
        let coh = rep.dict_get("nodeCoherence").and_then(|v| v.as_i64()).unwrap_or(0);
        if kind == "SystemCore" && coh > 0 {
            loop {
                let rep2 = ctx
                    .dispatch_rpc(
                        &mut p.sess,
                        "hackingMgr",
                        None,
                        "ClickedOnTile",
                        &[PyRep::Integer(can2_id), PyRep::Integer(next_t as i64)],
                        &kw,
                        None,
                    )
                    .await
                    .expect("ClickedOnTile core");
                if rep2.dict_get("status").and_then(|v| v.as_str()) == Some("Won") {
                    won_rep = Some(rep2);
                    break;
                }
            }
            if won_rep.is_some() {
                break;
            }
        }
        let is_blocker = matches!(
            kind,
            "Firewall" | "Antivirus" | "RestorationNode" | "VirusSuppressor"
        ) && coh > 0;
        if !is_blocker {
            non_blocking_w.insert(next_t);
        }
    }
    let won_rep = won_rep.expect("Core kill must win");
    let PyRep::List(spewed) = won_rep.dict_get("spewedContainers").unwrap() else {
        panic!("spewedContainers list");
    };
    assert!(!spewed.is_empty(), "Core kill must spew loot containers");

    // 3. Ghost Site container failure: detonates with AoE explosive damage & spawns ambush NPCs
    let actual_ghost_pos = sys
        .get_ball_snapshot(ghost_can_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.position)
        .unwrap_or(ghost_pos);
    move_pilot_to(&ctx, &mut p, actual_ghost_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;
    let s_ghost = ctx
        .dispatch_rpc(
            &mut p.sess,
            "hackingMgr",
            None,
            "StartNewGameInstance",
            &[PyRep::Integer(ghost_can_id), PyRep::None, PyRep::None, PyRep::None, PyRep::Integer(5)],
            &kw,
            None,
        )
        .await
        .expect("StartNewGameInstance ghost");
    let gs_ghost = s_ghost.dict_get("gridSize").and_then(|v| v.as_i64()).unwrap() as u32;
    let ghost_fail = fail_current_hack(&ctx, &mut p, ghost_can_id, gs_ghost).await;
    assert_eq!(ghost_fail.dict_get("ghostDetonated"), Some(&PyRep::Bool(true)));
    let ghost_dmg = ghost_fail
        .dict_get("ghostExplosiveDamage")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert!(ghost_dmg >= 6_000.0, "High-Sec ghost detonation must deal >=6000 explosive dmg, got {ghost_dmg}");
    let PyRep::List(ambush_npcs) = ghost_fail.dict_get("ghostSpawnedNpcBallIDs").unwrap() else {
        panic!("ghostSpawnedNpcBallIDs list");
    };
    assert!(!ambush_npcs.is_empty(), "Ghost detonation must spawn ambush NPCs");
    assert!(
        sys.get_ball_snapshot(p.ship_id).await.unwrap().is_none(),
        "6000 explosive damage from Ghost detonation must destroy untanked frigate within 10km"
    );
}
