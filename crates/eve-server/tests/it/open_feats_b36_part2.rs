//! Integration tests for Batch 36 open `feat:pve.*` parity rows (part 2: Storyline offer counter).

#![forbid(unsafe_code)]

use std::collections::{HashMap, VecDeque};

use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
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

fn dock_at_station(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    sys_id: u32,
    station_id: Option<i64>,
) {
    sess.solar_system_id = Some(sys_id);
    sess.station_id = station_id;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = station_id;
        }
    });
}

async fn query_journal_missions(
    ctx: &ServerContext,
    s: &mut SessionState,
) -> Vec<HashMap<String, PyRep>> {
    let kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(s, "agentMgr", None, "GetMyJournalDetails", &[], &kw, None)
        .await
        .expect("GetMyJournalDetails");
    let PyRep::Tuple(tup) = rep else {
        panic!("Expected Tuple from GetMyJournalDetails");
    };
    let PyRep::List(missions) = &tup[0] else {
        panic!("Expected missions list");
    };
    missions.iter().map(kv_map).collect()
}

async fn complete_live_courier(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    agent: &eve_sde::AgentInfo,
) {
    let kw = HashMap::new();
    dock_at_station(
        ctx,
        sess,
        char_id,
        agent.solar_system_id.unwrap(),
        agent.station_id,
    );
    let offer = kv_map(
        &ctx.dispatch_rpc(
            sess,
            "agentMgr",
            None,
            "DoAction",
            &[PyRep::Integer(2), PyRep::Integer(agent.agent_id)],
            &kw,
            None,
        )
        .await
        .expect("Request courier mission"),
    );
    let mid = offer["missionID"].as_i64().unwrap();
    let dest_sta = offer["destinationStationID"].as_i64().unwrap();
    ctx.dispatch_rpc(
        sess,
        "agentMgr",
        None,
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(mid)],
        &kw,
        None,
    )
    .await
    .expect("Accept courier mission");

    let tag = format!("courier_cargo:{char_id}:{mid}");
    ctx.db.with_mut_snapshot(|snap| {
        let cid = snap
            .item_custom_names
            .iter()
            .find(|(_, v)| *v == &tag)
            .map(|(&k, _)| k)
            .expect("granted courier cargo item");
        if let Some(it) = snap.items.get_mut(&cid) {
            it.location_id = ship_id;
            it.flag = InventoryFlag::Cargo;
        }
    });
    let dest_sys = ctx.sde.stations[&dest_sta].solar_system_id;
    dock_at_station(ctx, sess, char_id, dest_sys, Some(dest_sta));
    ctx.dispatch_rpc(
        sess,
        "agentMgr",
        None,
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(mid)],
        &kw,
        None,
    )
    .await
    .expect("Complete courier mission");
}

async fn complete_live_fw_mission(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    fw_agent: &eve_sde::AgentInfo,
) {
    let kw = HashMap::new();
    let sys_id = fw_agent.solar_system_id.unwrap();
    dock_at_station(ctx, sess, char_id, sys_id, fw_agent.station_id);
    let offer = kv_map(
        &ctx.dispatch_rpc(
            sess,
            "agentMgr",
            None,
            "DoAction",
            &[PyRep::Integer(2), PyRep::Integer(fw_agent.agent_id)],
            &kw,
            None,
        )
        .await
        .expect("Request FW mission"),
    );
    let mid = offer["missionID"].as_i64().unwrap();
    ctx.dispatch_rpc(
        sess,
        "agentMgr",
        None,
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(mid)],
        &kw,
        None,
    )
    .await
    .expect("Accept FW mission");
    let (m_sys_id, npc_ids) = ctx.db.with_snapshot(|s| {
        let m = &s.active_missions[&(char_id, mid as u32)];
        (m.solar_system_id, m.spawned_npc_ball_ids.clone())
    });

    dock_at_station(ctx, sess, char_id, m_sys_id, None);
    let sys_h = ctx.systems.get_or_spawn(m_sys_id);
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 639,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 100_000_000.0,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(639),
        )
        .await;
    for nid in npc_ids {
        let _ = sys_h.set_ball_position(nid, DVec3::new(5_000.0, 0.0, 0.0)).await;
        ctx.dispatch_rpc(sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(nid)], &kw, None)
            .await
            .expect("AddTarget FW mission NPC");
        for _ in 0..30 {
            if sys_h.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == nid && l.locked) {
                break;
            }
            let _ = sys_h.step_manual(1.0).await;
        }
        ctx.dispatch_rpc(
            sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(820_002), PyRep::Integer(nid)],
            &kw,
            None,
        )
        .await
        .expect("Fire weapon at FW mission NPC");
        for _ in 0..180 {
            if sys_h.get_ball_snapshot(nid).await.unwrap().is_none() {
                break;
            }
            let _ = sys_h.step_manual(1.0).await;
        }
        assert!(
            sys_h.get_ball_snapshot(nid).await.unwrap().is_none(),
            "FW mission NPC {nid} must be destroyed"
        );
    }
    dock_at_station(ctx, sess, char_id, sys_id, fw_agent.station_id);
    ctx.dispatch_rpc(
        sess,
        "agentMgr",
        None,
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(mid)],
        &kw,
        None,
    )
    .await
    .expect("Complete FW mission");
}

#[tokio::test]
async fn test_b36_storyline_offer_counter() {
    // parity: feat:pve.storyline-offer-counter
    eve_testkit::attest(&["feat:pve.storyline-offer-counter"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let ship_id = 820_001_i64;
    let weapon_id = 820_002_i64;
    let ammo_id = 820_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
            ch.active_ship_id = ship_id;
            ch.standings.insert(500_001, 6.0);
            ch.standings.insert(1_000_148, 5.0);
            ch.standings.insert(1_000_182, 5.0);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 639,
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            weapon_id,
            InventoryItemRecord {
                item_id: weapon_id,
                type_id: 2961,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ammo_id,
            InventoryItemRecord {
                item_id: ammo_id,
                type_id: 21894,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 5_000,
                singleton: false,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(weapon_id, true);
        let sk = snap.skills.entry(char_id).or_default();
        for sid in [3300_u32, 3302, 3305, 3308, 3310, 3311, 3312, 3315, 3316, 3318, 3334, 3413, 3418, 3426, 20327, 21667] {
            sk.insert(
                sid,
                TrainedSkillRecord {
                    type_id: sid,
                    level: 5,
                    skill_points: 1_280_000.0,
                },
            );
        }
        for tid in [639_u32, 2961, 21894] {
            if let Some(t) = ctx.sde.types.get(&tid) {
                for sa in [182_u16, 183, 184] {
                    if let Some(&req_f) = t.default_attributes.get(&sa) {
                        let rsid = req_f as u32;
                        if rsid > 0 {
                            sk.insert(rsid, TrainedSkillRecord { type_id: rsid, level: 5, skill_points: 1_280_000.0 });
                        }
                    }
                }
            }
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    let mut sess = SessionState::new(3602);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);

    let count_storyline_offers = |missions: &[HashMap<String, PyRep>]| -> Vec<(i64, i64, i64)> {
        missions
            .iter()
            .filter(|m| {
                m.get("state").and_then(|v| v.as_str()) == Some("Offered")
                    && m.get("agentID")
                        .and_then(|v| v.as_i64())
                        .and_then(|aid| ctx.sde.agents.get(&aid))
                        .is_some_and(|a| matches!(a.agent_type_id, 6 | 7))
            })
            .map(|m| {
                (
                    m["agentID"].as_i64().unwrap(),
                    m["missionID"].as_i64().unwrap(),
                    m.get("level").and_then(|v| v.as_i64()).unwrap_or(0),
                )
            })
            .collect()
    };

    // Negative RPC check: completing an unaccepted mission is refused
    assert!(ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(6), PyRep::Integer(999_999)], &kw, None)
        .await
        .is_err());

    // 1. Real SDE InterBus agent (3015955, corp 1000148): 16 live completions -> 0 storyline offers
    let interbus_agent = ctx.sde.agents[&3_015_955].clone();
    for _ in 0..16 {
        complete_live_courier(&ctx, &mut sess, char_id, ship_id, &interbus_agent).await;
    }
    assert!(count_storyline_offers(&query_journal_missions(&ctx, &mut sess).await).is_empty());

    // 2. Real SDE FW militia agent (3019210, corp 1000182 Tribal Liberation Force): 16 live completions -> 0 storyline offers
    let fw_agent = ctx.sde.agents[&3_019_210].clone();
    dock_at_station(&ctx, &mut sess, char_id, fw_agent.solar_system_id.unwrap(), fw_agent.station_id);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(2), PyRep::Integer(fw_agent.agent_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "NotEnlistedInMilitia"
    );
    ctx.dispatch_rpc(&mut sess, "facWarMgr", None, "JoinFactionAsCharacter", &[PyRep::Integer(500_002)], &kw, None)
        .await
        .expect("Enlist in Minmatar militia");
    for _ in 0..16 {
        complete_live_fw_mission(&ctx, &mut sess, char_id, ship_id, &fw_agent).await;
    }
    assert!(count_storyline_offers(&query_journal_missions(&ctx, &mut sess).await).is_empty());

    // 3. Separate (faction, level) counters: 15 L3 + 1 L1 Caldari high-sec missions -> 0 offers; 16th L3 -> triggers L3 Storyline!
    let caldari_l3 = ctx.sde.agents[&3_008_423].clone();
    let caldari_l1 = ctx.sde.agents[&3_008_416].clone();
    for _ in 0..15 {
        complete_live_courier(&ctx, &mut sess, char_id, ship_id, &caldari_l3).await;
    }
    complete_live_courier(&ctx, &mut sess, char_id, ship_id, &caldari_l1).await;
    assert!(count_storyline_offers(&query_journal_missions(&ctx, &mut sess).await).is_empty());

    complete_live_courier(&ctx, &mut sess, char_id, ship_id, &caldari_l3).await;
    let sl_offers_1 = count_storyline_offers(&query_journal_missions(&ctx, &mut sess).await);
    assert_eq!(sl_offers_1.len(), 1);
    let (first_sl_aid, first_sl_mid, first_sl_lvl) = sl_offers_1[0];
    assert_eq!(first_sl_lvl, 3);

    let briefing_1 = kv_map(
        &ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetMissionBriefingInfo", &[PyRep::Integer(first_sl_mid)], &kw, None)
            .await
            .unwrap(),
    );
    assert_eq!(briefing_1["level"].as_i64(), Some(3));
    let sl_tmpl_diff = ctx
        .sde
        .missions
        .get(&(first_sl_mid as u32))
        .and_then(|t| t.dungeon_id)
        .and_then(|did| ctx.sde.dungeons.get(&did))
        .and_then(|d| d.difficulty);
    assert_eq!(sl_tmpl_diff, Some(3));

    let first_sl_info = &ctx.sde.agents[&first_sl_aid];
    assert!(ctx.sde.solar_systems[&first_sl_info.solar_system_id.unwrap()].security_status >= 0.45);

    let origin_sys = caldari_l3.solar_system_id.unwrap();
    let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
    for j in ctx.sde.jumps.values() {
        adj.entry(j.from_solar_system_id)
            .or_default()
            .push(j.to_solar_system_id);
    }
    let mut dists = HashMap::from([(origin_sys, 0_u32)]);
    let mut q = VecDeque::from([origin_sys]);
    while let Some(u) = q.pop_front() {
        let nd = dists[&u] + 1;
        if let Some(nbs) = adj.get(&u) {
            for &v in nbs {
                if !dists.contains_key(&v) {
                    dists.insert(v, nd);
                    q.push_back(v);
                }
            }
        }
    }
    let min_jumps = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            matches!(a.agent_type_id, 6 | 7)
                && ctx.sde.npc_corporations.get(&a.corporation_id).and_then(|c| c.faction_id) == Some(500_001)
                && a.solar_system_id.and_then(|s| ctx.sde.solar_systems.get(&s)).is_some_and(|s| s.security_status >= 0.45)
        })
        .filter_map(|a| dists.get(&a.solar_system_id?))
        .copied()
        .min()
        .unwrap();
    assert_eq!(dists[&first_sl_info.solar_system_id.unwrap()], min_jumps);

    // 4. With nearest storyline agent's offer still pending, 32nd L3 completion picks second-nearest high-sec Storyline agent!
    for _ in 0..16 {
        complete_live_courier(&ctx, &mut sess, char_id, ship_id, &caldari_l3).await;
    }
    let sl_offers_2 = count_storyline_offers(&query_journal_missions(&ctx, &mut sess).await);
    assert_eq!(sl_offers_2.len(), 2);
    let second_sl_aid = sl_offers_2
        .iter()
        .map(|&(aid, _, _)| aid)
        .find(|&aid| aid != first_sl_aid)
        .expect("second-nearest storyline agent");
    assert!(ctx.sde.solar_systems[&ctx.sde.agents[&second_sl_aid].solar_system_id.unwrap()].security_status >= 0.45);
}
