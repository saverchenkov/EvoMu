//! Real server-side emission tests for Notify Batch 7 (Part 4: Abyssal Deadspace,
//! Void Space, Agent Missions, Faction Warfare, Sovereignty Dev Index, PVP Filaments & Tournaments).

use crate::notify_real_emitters_batch7::{
    drain_all, find_notif, login_in_space, make_item, new_ctx, rpc, PILOT_A, PILOT_B, SYS_JITA,
};
use eve_db::{FwSystemOccupationRecord, InventoryFlag};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;

const FILAMENT_TYPE_ID: u32 = 47_763;

// parity: notify:OnAbyssalLocationEntered, notify:OnAbyssalContentFinished
// parity: notify:OnVoidSpaceEntered, notify:OnVoidSpaceRelog, notify:OnVoidSpaceContentFinished
// parity: notify:OnAgentMissionObjectivesCompleted, notify:OnAgentMissionObjectivesFailed
// parity: notify:OnFacWarDungeonCompleted, notify:OnSolarSystemDevIndexChanged
// parity: notify:OnPVPFilamentsCharacterStatistics, notify:OnPVPFilamentsLeaderboard
// parity: notify:OnTournamentPerformBan
#[tokio::test]
async fn notify_batch7_abyssal_void_missions_fw_sov_pvp_and_tourney() {
    eve_testkit::attest(&[
        "notify:OnAbyssalLocationEntered",
        "notify:OnAbyssalContentFinished",
        "notify:OnVoidSpaceEntered",
        "notify:OnVoidSpaceRelog",
        "notify:OnVoidSpaceContentFinished",
        "notify:OnAgentMissionObjectivesCompleted",
        "notify:OnAgentMissionObjectivesFailed",
        "notify:OnFacWarDungeonCompleted",
        "notify:OnSolarSystemDevIndexChanged",
        "notify:OnPVPFilamentsCharacterStatistics",
        "notify:OnPVPFilamentsLeaderboard",
        "notify:OnTournamentPerformBan",
    ]);

    let ctx = new_ctx();
    let (ship_a, ship_b, fil_id) = (540_000_001_i64, 540_000_002_i64, 740_000_010_i64);
    let mut sess_a = login_in_space(&ctx, 7_401, "demo", "demo-secret", PILOT_A, ship_a, 621, SYS_JITA, DVec3::new(0.0, 2.0e9, 0.0)).await;
    let mut sess_b = login_in_space(&ctx, 7_402, "other", "other-secret", PILOT_B, ship_b, 587, SYS_JITA, DVec3::new(1_000.0, 2.0e9, 0.0)).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.clone_grade = 2;
        }
        snap.items.insert(fil_id, make_item(fil_id, FILAMENT_TYPE_ID, PILOT_A, ship_a, InventoryFlag::Cargo, 4, false));
    });

    // 1-2. OnAbyssalLocationEntered (1-arg KeyVal) & OnAbyssalContentFinished (2-arg)
    ctx.advance_secs(12);
    let deploy_rep = rpc(&ctx, &mut sess_a, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(fil_id)])
        .await
        .expect("AbyssalEntranceDeployment");
    let trace_id = deploy_rep.dict_get("traceBallID").and_then(|v| v.as_i64()).expect("traceBallID");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    let enter_rep = rpc(&ctx, &mut sess_a, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_id)])
        .await
        .expect("AbyssalEntranceGateActivation");
    let pocket_id = enter_rep.dict_get("pocketID").and_then(|v| v.as_i64()).expect("pocketID");
    let abyss_in = drain_all(&ctx, &mut sess_a, PILOT_A);
    let abyss_enter_args = find_notif(&abyss_in, "OnAbyssalLocationEntered");
    assert_eq!(abyss_enter_args.len(), 1);
    assert_eq!(abyss_enter_args[0].dict_get("content_id").and_then(|v| v.as_i64()), Some(pocket_id));

    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, PILOT_A, 0).await;
    rpc(&ctx, &mut sess_a, "abyssalMgr", "AbyssalGateActivation", &[PyRep::Integer(1_895_000_000 + pocket_id * 10 + 1)])
        .await
        .expect("Abyssal R1->R2");
    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, PILOT_A, 1).await;
    rpc(&ctx, &mut sess_a, "abyssalMgr", "AbyssalGateActivation", &[PyRep::Integer(1_895_000_000 + pocket_id * 10 + 2)])
        .await
        .expect("Abyssal R2->R3");
    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, PILOT_A, 2).await;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    rpc(&ctx, &mut sess_a, "abyssalMgr", "AbyssalEndGateActivation", &[PyRep::Integer(1_895_000_000 + pocket_id * 10 + 3)])
        .await
        .expect("AbyssalEndGateActivation");
    let abyss_out = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&abyss_out, "OnAbyssalContentFinished"),
        &[PyRep::Integer(pocket_id), PyRep::Integer(1)]
    );

    // 3-5. OnVoidSpaceEntered (1-arg), OnVoidSpaceRelog (1-arg), OnVoidSpaceContentFinished (2-arg)
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess_a, "voidSpaceMgr", "VoidSpacePlayerJump", &[PyRep::Integer(fil_id), PyRep::Integer(777_001)])
        .await
        .expect("VoidSpacePlayerJump");
    let void_in = drain_all(&ctx, &mut sess_a, PILOT_A);
    let void_enter_args = find_notif(&void_in, "OnVoidSpaceEntered");
    assert_eq!(void_enter_args.len(), 1);
    assert_eq!(void_enter_args[0].dict_get("content_id").and_then(|v| v.as_i64()), Some(fil_id));

    rpc(&ctx, &mut sess_a, "voidSpaceMgr", "ClientIsReady", &[])
        .await
        .expect("voidSpaceMgr.ClientIsReady");
    let void_relog = drain_all(&ctx, &mut sess_a, PILOT_A);
    let relog_args = find_notif(&void_relog, "OnVoidSpaceRelog");
    assert_eq!(relog_args.len(), 1);
    assert_eq!(relog_args[0].dict_get("content_id").and_then(|v| v.as_i64()), Some(fil_id));

    ctx.advance_secs(12);
    rpc(&ctx, &mut sess_a, "voidSpaceMgr", "VoidSpaceEndGateActivation", &[PyRep::Integer(888_001)])
        .await
        .expect("VoidSpaceEndGateActivation");
    let void_out = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&void_out, "OnVoidSpaceContentFinished"),
        &[PyRep::Integer(fil_id), PyRep::Integer(1)]
    );

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
    for _ in 0..4 {
        ctx.advance_secs(150);
        tick_background_cluster(&ctx).await;
    }
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

    // 10-11. OnPVPFilamentsCharacterStatistics (1-arg KeyVal) & OnPVPFilamentsLeaderboard (1-arg KeyVal)
    rpc(&ctx, &mut sess_a, "pvpFilamentMgr", "GetCharacterStatistics", &[PyRep::Integer(171)])
        .await
        .expect("GetCharacterStatistics");
    rpc(&ctx, &mut sess_a, "pvpFilamentMgr", "GetLeaderboard", &[PyRep::Integer(171)])
        .await
        .expect("GetLeaderboard");
    let pvp_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let pvp_stats_args = find_notif(&pvp_notifs, "OnPVPFilamentsCharacterStatistics");
    assert_eq!(pvp_stats_args.len(), 1);
    assert_eq!(pvp_stats_args[0].dict_get("eventID").and_then(|v| v.as_i64()), Some(171));
    let pvp_lb_args = find_notif(&pvp_notifs, "OnPVPFilamentsLeaderboard");
    assert_eq!(pvp_lb_args.len(), 1);
    assert_eq!(pvp_lb_args[0].dict_get("eventID").and_then(|v| v.as_i64()), Some(171));

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
