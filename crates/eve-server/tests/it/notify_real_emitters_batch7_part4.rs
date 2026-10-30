//! Real server-side emission tests for Notify Batch 7 (Part 4: Agent Missions,
//! Faction Warfare, Sovereignty Dev Index, PVP Filaments & Tournaments).

use crate::notify_real_emitters_batch7::{
    drain_all, find_notif, login_in_space, make_item, new_ctx, rpc, PILOT_A, PILOT_B, SYS_JITA,
};
use eve_db::{FwSystemOccupationRecord, InventoryFlag};
use eve_destiny::{Ball, MovementMode, TargetLock};
use eve_proto::PyRep;
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;

// parity: notify:OnAgentMissionObjectivesCompleted, notify:OnAgentMissionObjectivesFailed
// parity: notify:OnFacWarDungeonCompleted, notify:OnSolarSystemDevIndexChanged
// parity: notify:OnPVPFilamentsCharacterStatistics, notify:OnPVPFilamentsLeaderboard
// parity: notify:OnTournamentPerformBan
// DATA GAP: OnAbyssalLocationEntered, OnAbyssalContentFinished, OnVoidSpaceEntered, OnVoidSpaceRelog, OnVoidSpaceContentFinished remain open (V24 room-index location_id & weather/tier nebula selection pending).
#[tokio::test]
async fn notify_batch7_abyssal_void_missions_fw_sov_pvp_and_tourney() {
    eve_testkit::attest(&[
        "notify:OnAgentMissionObjectivesCompleted",
        "notify:OnAgentMissionObjectivesFailed",
        "notify:OnFacWarDungeonCompleted",
        "notify:OnSolarSystemDevIndexChanged",
        "notify:OnPVPFilamentsCharacterStatistics",
        "notify:OnPVPFilamentsLeaderboard",
        "notify:OnTournamentPerformBan",
    ]);

    let ctx = new_ctx();
    let (ship_a, ship_b) = (540_000_001_i64, 540_000_002_i64);
    let mut sess_a = login_in_space(&ctx, 7_401, "demo", "demo-secret", PILOT_A, ship_a, 621, SYS_JITA, DVec3::new(0.0, 2.0e9, 0.0)).await;
    let mut sess_b = login_in_space(&ctx, 7_402, "other", "other-secret", PILOT_B, ship_b, 621, SYS_JITA, DVec3::new(1_000.0, 2.0e9, 0.0)).await;

    // 6-7. OnAgentMissionObjectivesCompleted (1-arg agentID) & OnAgentMissionObjectivesFailed (1-arg agentID)
    let (agent_id, corp_id, agent_station_id, agent_sys_id) = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id == 22 && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .map(|a| {
            let sid = a.station_id.unwrap() as i64;
            let sys = ctx.sde.stations.get(&sid).map(|st| st.solar_system_id).unwrap_or(SYS_JITA);
            (a.agent_id as i64, a.corporation_id, sid, sys)
        })
        .expect("Level 1 Distribution agent in SDE");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(agent_station_id);
            ch.solar_system_id = agent_sys_id;
            ch.standings.insert(corp_id, 3.0);
        }
        if let Some(sh) = snap.items.get_mut(&ship_a) {
            sh.location_id = agent_station_id;
            sh.flag = InventoryFlag::Hangar;
        }
    });
    sess_a.station_id = Some(agent_station_id);
    sess_a.solar_system_id = Some(agent_sys_id);

    let offer1 = rpc(&ctx, &mut sess_a, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(agent_id)])
        .await
        .expect("Offer courier mission 1");
    let mid1 = offer1.dict_get("missionID").and_then(|v| v.as_i64()).expect("mid1");
    rpc(&ctx, &mut sess_a, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid1)])
        .await
        .expect("Accept courier mission 1");
    let (dest_sid, req_tid) = ctx.db.with_snapshot(|snap| {
        let meta = eve_server::services::agents_missions::load_mission_meta(snap, PILOT_A, mid1 as u32)
            .expect("mission meta");
        (
            meta.destination_station_id.unwrap_or(agent_station_id),
            meta.required_delivery_type_id.unwrap_or(3683),
        )
    });
    let dest_sys = ctx.sde.stations.get(&dest_sid).map(|s| s.solar_system_id).unwrap_or(agent_sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(dest_sid);
            ch.solar_system_id = dest_sys;
        }
        if let Some(sh) = snap.items.get_mut(&ship_a) {
            sh.location_id = dest_sid;
        }
        for it in snap.items.values_mut() {
            if it.owner_id == PILOT_A && it.type_id == req_tid {
                it.location_id = ship_a;
                it.flag = InventoryFlag::Cargo;
            }
        }
    });
    sess_a.station_id = Some(dest_sid);
    sess_a.solar_system_id = Some(dest_sys);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    rpc(&ctx, &mut sess_a, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mid1)])
        .await
        .expect("Complete courier mission 1");
    let m_done_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&m_done_notifs, "OnAgentMissionObjectivesCompleted"),
        &[PyRep::Integer(agent_id)]
    );

    // Offer + accept second mission, then quit (actionID = 11) -> OnAgentMissionObjectivesFailed
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(agent_station_id);
            ch.solar_system_id = agent_sys_id;
        }
    });
    sess_a.station_id = Some(agent_station_id);
    sess_a.solar_system_id = Some(agent_sys_id);
    let offer2 = rpc(&ctx, &mut sess_a, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(agent_id)])
        .await
        .expect("Offer mission 2");
    let mid2 = offer2.dict_get("missionID").and_then(|v| v.as_i64()).expect("mid2");
    rpc(&ctx, &mut sess_a, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid2)])
        .await
        .expect("Accept mission 2");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(&ctx, &mut sess_a, "agentMgr", "DoAction", &[PyRep::Integer(11), PyRep::Integer(mid2)])
        .await
        .expect("Quit mission 2");
    let m_fail_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&m_fail_notifs, "OnAgentMissionObjectivesFailed"),
        &[PyRep::Integer(agent_id)]
    );

    // 8. OnFacWarDungeonCompleted (2-arg [was_conquered, faction_id])
    let (enemy_sys, front_sys) = {
        let lowsec: Vec<u32> = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| s.security_status > 0.0 && s.security_status < 0.45 && s.solar_system_id < 31_000_000)
            .map(|s| s.solar_system_id)
            .collect();
        let mut pair = None;
        'fw_pair: for &f in &lowsec {
            for &nb in ctx.sde.stargates().values().filter(|g| g.from_solar_system_id == f).map(|g| &g.to_solar_system_id) {
                if lowsec.contains(&nb) && nb != f {
                    pair = Some((nb, f));
                    break 'fw_pair;
                }
            }
        }
        pair.expect("adjacent lowsec pair in SDE")
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.clear();
        snap.fw_system_occupations.insert(
            enemy_sys,
            FwSystemOccupationRecord {
                solar_system_id: enemy_sys,
                owner_faction_id: 500_004,
                occupier_faction_id: 500_004,
                victory_points: 0,
                victory_points_threshold: 3_000,
            },
        );
        snap.fw_system_occupations.insert(
            front_sys,
            FwSystemOccupationRecord {
                solar_system_id: front_sys,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 0,
                victory_points_threshold: 3_000,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = None;
            ch.solar_system_id = front_sys;
            ch.standings.insert(500_004, 1.0);
        }
        if let Some(sh) = snap.items.get_mut(&ship_a) {
            sh.type_id = 593; // Tristan (T1 Frigate)
            sh.location_id = front_sys as i64;
            sh.flag = InventoryFlag::None;
        }
    });
    sess_a.station_id = None;
    sess_a.solar_system_id = Some(front_sys);
    rpc(&ctx, &mut sess_a, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_004)])
        .await
        .expect("Enlist Gallente Militia");
    let _ = rpc(&ctx, &mut sess_a, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[])
        .await
        .expect("Spawn FW complexes");
    let (novice_gate, novice_pos) = ctx.db.with_snapshot(|s| {
        s.fw_runtime
            .fw_complexes
            .iter()
            .find(|(_, (sys, tier, _, _, done))| *sys == front_sys && tier == "Novice" && !*done)
            .map(|(&gid, _)| (gid, DVec3::from_array(s.item_positions[&gid])))
            .expect("Novice complex in front_sys")
    });
    let front_h = ctx.systems.get_or_spawn(front_sys);
    let mut fw_cs = ctx.build_ship_combat_state(PILOT_A, ship_a, 593);
    fw_cs.is_cloaked = false;
    fw_cs.gate_cloak_remaining_ticks = 0;
    fw_cs.undock_invulnerable_remaining_ticks = 0;
    let _ = front_h
        .spawn_ball_with_meta(
            Ball {
                id: ship_a,
                type_id: 593,
                position: novice_pos + DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            fw_cs,
            ctx.build_ship_slim_meta(PILOT_A, 593),
        )
        .await;
    rpc(&ctx, &mut sess_a, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(novice_gate)])
        .await
        .expect("Activate FW complex gate");
    let _ = front_h.remove_ball(novice_gate + 500_000_000).await;
    ctx.db.with_mut_snapshot(|s| {
        s.fw_runtime.complex_guards_cleared.insert(novice_gate, true);
    });
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    ctx.advance_secs(1);
    tick_background_cluster(&ctx).await;
    ctx.advance_secs(600);
    tick_background_cluster(&ctx).await;
    let fw_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&fw_notifs, "OnFacWarDungeonCompleted"),
        &[PyRep::Bool(true), PyRep::Integer(500_004)]
    );

    // 9. OnSolarSystemDevIndexChanged (1-arg solarSystemID)
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= 0.0 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .expect("nullsec system in SDE");
    let sov_now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = null_sys;
            ch.station_id = None;
        }
        let st = snap.system_states.entry(null_sys).or_default();
        st.sov_alliance_id = Some(99_000_001);
        st.strategic_index = 0;
        snap.sov_runtime.sov_claimed_since_unix.insert(null_sys, sov_now);
    });
    sess_a.solar_system_id = Some(null_sys);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    ctx.advance_secs(8 * 86_400);
    tick_background_cluster(&ctx).await;
    let sov_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&sov_notifs, "OnSolarSystemDevIndexChanged"), &[PyRep::Integer(null_sys as i64)]);

    // 10-11. OnPVPFilamentsCharacterStatistics (1-arg Dict) & OnPVPFilamentsLeaderboard (1-arg Dict)
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = rpc(&ctx, &mut sess_a, "pvpFilamentMgr", "GetCharacterStatistics", &[PyRep::Integer(171)])
        .await
        .expect("GetCharacterStatistics");
    let _ = rpc(&ctx, &mut sess_a, "pvpFilamentMgr", "GetLeaderboard", &[PyRep::Integer(171)])
        .await
        .expect("GetLeaderboard");
    assert!(
        drain_all(&ctx, &mut sess_a, PILOT_A).is_empty(),
        "read-only GetCharacterStatistics / GetLeaderboard must not emit notifications"
    );

    ctx.advance_secs(30 * 86_400);
    let all_events = rpc(&ctx, &mut sess_a, "pvpFilamentMgr", "GetAllEvents", &[])
        .await
        .expect("GetAllEvents");
    let PyRep::Dict(ev_entries) = all_events else { panic!("expected Dict from GetAllEvents") };
    let (expected_event_id, expected_match_type_id, expected_schedule_id) = ev_entries
        .iter()
        .filter_map(|(k, v)| {
            let eid = k.as_i64()?;
            let has_72982 = match v.dict_get("typeIDs") {
                Some(PyRep::List(l)) => l.iter().any(|x| x.as_i64() == Some(72_982)),
                _ => false,
            };
            if !has_72982 {
                return None;
            }
            let mt = v.dict_get("matchTypeID")?.as_i64()?;
            let sc = v.dict_get("scheduleID")?.as_i64()?;
            Some((eid, mt, sc))
        })
        .max_by_key(|&(eid, _, _)| eid)
        .expect("event for 72982 in SDE");

    let (fil_a, fil_b, turret_id, ammo_id) = (740_000_011_i64, 740_000_012_i64, 740_000_013_i64, 740_000_014_i64);
    ctx.db.with_mut_snapshot(|snap| {
        for &(cid, sid, fid) in &[(PILOT_A, ship_a, fil_a), (PILOT_B, ship_b, fil_b)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = SYS_JITA;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = sid;
            }
            if let Some(sh) = snap.items.get_mut(&sid) {
                sh.type_id = 621; // Caracal
                sh.location_id = SYS_JITA as i64;
                sh.flag = InventoryFlag::None;
            }
            snap.items.insert(fid, make_item(fid, 72_982, cid, sid, InventoryFlag::Cargo, 1, false));
        }
        snap.items.insert(turret_id, make_item(turret_id, 10_678, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(ammo_id, make_item(ammo_id, 222, PILOT_A, ship_a, InventoryFlag::HiSlot0, 10, false));
    });
    sess_a.solar_system_id = Some(SYS_JITA);
    sess_a.station_id = None;
    sess_a.ship_id = Some(ship_a);
    sess_b.solar_system_id = Some(SYS_JITA);
    sess_b.station_id = None;
    sess_b.ship_id = Some(ship_b);
    ctx.invalidate_ship_fitting(ship_a);
    ctx.invalidate_ship_fitting(ship_b);

    let jita_h = ctx.systems.get_or_spawn(SYS_JITA);
    let _ = jita_h.remove_ball(ship_a).await;
    let _ = jita_h.remove_ball(ship_b).await;
    for &(cid, sid, pos) in &[
        (PILOT_A, ship_a, DVec3::new(0.0, 2.0e9, 0.0)),
        (PILOT_B, ship_b, DVec3::new(1_000.0, 2.0e9, 0.0)),
    ] {
        let mut cs = ctx.build_ship_combat_state(cid, sid, 621);
        cs.is_cloaked = false;
        cs.gate_cloak_remaining_ticks = 0;
        cs.undock_invulnerable_remaining_ticks = 0;
        let _ = jita_h
            .spawn_ball_with_meta(
                Ball {
                    id: sid,
                    type_id: 621,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_200_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
                ctx.build_ship_slim_meta(cid, 621),
            )
            .await;
    }

    let q_a = rpc(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(fil_a), PyRep::Integer(72_982)])
        .await
        .expect("PILOT_A JoinPVPQueue");
    assert_eq!(q_a, PyRep::Integer(30));
    let q_b = rpc(&ctx, &mut sess_b, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(fil_b), PyRep::Integer(72_982)])
        .await
        .expect("PILOT_B JoinPVPQueue");
    assert_eq!(q_b, PyRep::Integer(5));
    ctx.sync_session(&mut sess_a);
    ctx.sync_session(&mut sess_b);
    assert_eq!(sess_a.solar_system_id, Some(32_000_001));
    assert_eq!(sess_b.solar_system_id, Some(32_000_001));

    let arena_h = ctx.systems.get_or_spawn(32_000_001);
    let (_, ball_b) = arena_h.get_ball_snapshot(ship_b).await.expect("snap").expect("ship_b in arena");
    let _ = arena_h.set_ball_position(ship_a, ball_b.position + DVec3::new(-1_500.0, 0.0, 0.0)).await;
    let mut cs_a = arena_h.get_combat_state(ship_a).await.expect("cs_a").expect("ship_a combat");
    cs_a.is_cloaked = false;
    cs_a.gate_cloak_remaining_ticks = 0;
    cs_a.undock_invulnerable_remaining_ticks = 0;
    let _ = arena_h.set_combat_state(ship_a, cs_a).await;
    let mut cs_b = arena_h.get_combat_state(ship_b).await.expect("cs_b").expect("ship_b combat");
    cs_b.is_cloaked = false;
    cs_b.gate_cloak_remaining_ticks = 0;
    cs_b.undock_invulnerable_remaining_ticks = 0;
    (cs_b.shield_hp, cs_b.armor_hp, cs_b.structure_hp) = (0.0, 0.0, 1.0);
    let _ = arena_h.set_combat_state(ship_b, cs_b).await;
    let _ = arena_h
        .add_target_lock(
            ship_a,
            TargetLock {
                target_id: ship_b,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    ctx.set_invention_roll_override(Some(0.50));

    let fire_res = rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(turret_id), PyRep::Integer(ship_b)])
        .await
        .expect("Activate turret in Proving Grounds arena");
    assert_eq!(fire_res.dict_get("destroyed").and_then(PyRep::as_bool), Some(true));

    let pvp_notifs_a = drain_all(&ctx, &mut sess_a, PILOT_A);
    let pvp_stats_args = find_notif(&pvp_notifs_a, "OnPVPFilamentsCharacterStatistics");
    assert_eq!(pvp_stats_args.len(), 1);
    let PyRep::Dict(stat_entries) = &pvp_stats_args[0] else {
        panic!("expected PyRep::Dict for OnPVPFilamentsCharacterStatistics, got {:?}", pvp_stats_args[0]);
    };
    let stat_lookup = |key: &str| -> Option<&PyRep> {
        stat_entries.iter().find_map(|(k, v)| match k {
            PyRep::String(s) if s == key => Some(v),
            _ => None,
        })
    };
    assert_eq!(stat_lookup("eventID"), Some(&PyRep::Integer(expected_event_id)));
    assert_eq!(stat_lookup("matchTypeID"), Some(&PyRep::Integer(expected_match_type_id)));
    assert_eq!(stat_lookup("scheduleID"), Some(&PyRep::Integer(expected_schedule_id)));
    let Some(PyRep::Dict(inner_stats)) = stat_lookup("statistics") else {
        panic!("expected PyRep::Dict for statistics field, got {:?}", stat_lookup("statistics"));
    };
    let inner_lookup = |key: &str| -> Option<&PyRep> {
        inner_stats.iter().find_map(|(k, v)| match k {
            PyRep::String(s) if s == key => Some(v),
            _ => None,
        })
    };
    assert_eq!(inner_lookup("character_id"), Some(&PyRep::Integer(PILOT_A)));
    assert_eq!(inner_lookup("rank"), Some(&PyRep::Integer(1)));
    assert_eq!(inner_lookup("wins"), Some(&PyRep::Integer(1)));
    assert_eq!(inner_lookup("losses"), Some(&PyRep::Integer(0)));
    assert_eq!(inner_lookup("draws"), Some(&PyRep::Integer(0)));

    let pvp_lb_args = find_notif(&pvp_notifs_a, "OnPVPFilamentsLeaderboard");
    assert_eq!(pvp_lb_args.len(), 1);
    let PyRep::Dict(lb_entries) = &pvp_lb_args[0] else {
        panic!("expected PyRep::Dict for OnPVPFilamentsLeaderboard, got {:?}", pvp_lb_args[0]);
    };
    let lb_lookup = |key: &str| -> Option<&PyRep> {
        lb_entries.iter().find_map(|(k, v)| match k {
            PyRep::String(s) if s == key => Some(v),
            _ => None,
        })
    };
    assert_eq!(lb_lookup("eventID"), Some(&PyRep::Integer(expected_event_id)));
    assert_eq!(lb_lookup("matchTypeID"), Some(&PyRep::Integer(expected_match_type_id)));
    assert_eq!(lb_lookup("scheduleID"), Some(&PyRep::Integer(expected_schedule_id)));
    let Some(PyRep::List(rows)) = lb_lookup("entries") else {
        panic!("expected PyRep::List for entries field, got {:?}", lb_lookup("entries"));
    };
    assert_eq!(rows.len(), 2);
    let PyRep::Dict(row0) = &rows[0] else { panic!("expected PyRep::Dict for leaderboard row 0") };
    let row0_char = row0.iter().find_map(|(k, v)| match k {
        PyRep::String(s) if s == "character_id" => Some(v),
        _ => None,
    });
    let row0_rank = row0.iter().find_map(|(k, v)| match k {
        PyRep::String(s) if s == "rank" => Some(v),
        _ => None,
    });
    assert_eq!(row0_char, Some(&PyRep::Integer(PILOT_A)));
    assert_eq!(row0_rank, Some(&PyRep::Integer(1)));

    let pvp_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    let pvp_stats_b = find_notif(&pvp_notifs_b, "OnPVPFilamentsCharacterStatistics");
    assert_eq!(pvp_stats_b.len(), 1);
    let PyRep::Dict(stat_entries_b) = &pvp_stats_b[0] else {
        panic!("expected PyRep::Dict for PILOT_B OnPVPFilamentsCharacterStatistics");
    };
    let inner_b = stat_entries_b.iter().find_map(|(k, v)| match k {
        PyRep::String(s) if s == "statistics" => Some(v),
        _ => None,
    });
    let Some(PyRep::Dict(inner_stats_b)) = inner_b else {
        panic!("expected PyRep::Dict for PILOT_B statistics");
    };
    let b_char = inner_stats_b.iter().find_map(|(k, v)| match k {
        PyRep::String(s) if s == "character_id" => Some(v),
        _ => None,
    });
    let b_losses = inner_stats_b.iter().find_map(|(k, v)| match k {
        PyRep::String(s) if s == "losses" => Some(v),
        _ => None,
    });
    assert_eq!(b_char, Some(&PyRep::Integer(PILOT_B)));
    assert_eq!(b_losses, Some(&PyRep::Integer(1)));
    let pvp_lb_b = find_notif(&pvp_notifs_b, "OnPVPFilamentsLeaderboard");
    assert_eq!(pvp_lb_b.len(), 1);
    assert!(matches!(&pvp_lb_b[0], PyRep::Dict(_)));

    // 12. OnTournamentPerformBan (6-arg) via tourneyMgr.CreateMatch + StartAutobanning
    sess_a.role = eve_machonet::service_roles::ROLE_GMH;
    rpc(
        &ctx,
        &mut sess_a,
        "tourneyMgr",
        "CreateMatch",
        &[
            PyRep::Integer(1),
            PyRep::Tuple(vec![
                PyRep::Integer(1001),
                PyRep::Integer(98_000_001),
                PyRep::Integer(98_000_002),
                PyRep::Integer(PILOT_A),
                PyRep::Integer(PILOT_B),
            ]),
            PyRep::Integer(100),
            PyRep::Integer(600),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .expect("tourneyMgr.CreateMatch");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    rpc(&ctx, &mut sess_a, "tourneyMgr", "StartAutobanning", &[PyRep::Integer(1001), PyRep::Integer(2)])
        .await
        .expect("tourneyMgr.StartAutobanning");
    let ban_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
    let ban_args = find_notif(&ban_notifs, "OnTournamentPerformBan");
    assert_eq!(ban_args.len(), 6);
    assert_eq!(ban_args[0], PyRep::Integer(1001));
    assert_eq!(ban_args[1], PyRep::Integer(2));
    assert!(matches!(&ban_args[2], PyRep::List(l) if !l.is_empty()));
    assert!(ban_args[3].as_i64().unwrap_or(0) > 0);
    assert_eq!(ban_args[4], PyRep::Integer(1));
    assert_eq!(ban_args[5], PyRep::Integer(1));
}
