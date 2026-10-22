//! Parity integration tests for Milestones (`milestoneMgr`), Monolith Task Tracker (`monolithTaskTrackerMgr`),
//! Fighters (`fighterMgr`), Rewards (`rewardMgr`), Colony Skyhooks (`colonyResourcesMgr`),
//! Dev Indices (`devIndexManager`), Epic Arcs (`epicArcStatus`), Client Telemetry (`infoGatheringMgr`, `clientStatLogger`),
//! and External Queue (`externalQueueMgr`).

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::{ActiveFighterSquadron, ServerContext};
use eve_server::services::sov_services::TYPE_ORBITAL_SKYHOOK;

use crate::common;

fn is_keyval(rep: &PyRep) -> bool {
    matches!(rep, PyRep::KeyVal(_))
}

fn make_test_ball(id: i64, type_id: u32, pos: glam::DVec3) -> eve_destiny::Ball {
    eve_destiny::Ball {
        id,
        type_id,
        position: pos,
        velocity: glam::DVec3::ZERO,
        max_velocity: 100.0,
        inertia: 1.0,
        mass: 1e6,
        radius: 100.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    }
}

#[tokio::test]
// parity: rpc:monolithTaskTrackerMgr.HasEverWarped
// parity: rpc:monolithTaskTrackerMgr.UpdateClientAchievmentsAndCounters
// parity: rpc:fighterMgr.CmdScoopAbandonedFighterFromSpace
async fn test_milestones_task_tracker_and_fighters() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(29_101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let char_id = sess.character_id.unwrap();
    let ship_id = 100_000_001i64;
    let sys_id = 30_000_142u32;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess.structure_id = None;

    // 1. milestoneMgr returns UnimplementedRPC
    let claim_err = ctx
        .dispatch_rpc(&mut sess, "milestoneMgr", None, "ClaimRewards", &[PyRep::Integer(1)], &kw, None)
        .await
        .expect_err("ClaimRewards must return UnimplementedRPC");
    assert_eq!(claim_err.code, "UnimplementedRPC");

    let logon_err = ctx
        .dispatch_rpc(&mut sess, "milestoneMgr", None, "ProcessCharacterLogon", &[], &kw, None)
        .await
        .expect_err("ProcessCharacterLogon must return UnimplementedRPC");
    assert_eq!(logon_err.code, "UnimplementedRPC");

    // 2. monolithTaskTrackerMgr.HasEverWarped
    // Fresh character has not warped
    let warped_rep1 = ctx
        .dispatch_rpc(&mut sess, "monolithTaskTrackerMgr", None, "HasEverWarped", &[], &kw, None)
        .await
        .expect("HasEverWarped fresh");
    assert_eq!(warped_rep1, PyRep::Bool(false));

    // Pilot records warp
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = eve_server::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap);
        if !ext.characters_warped.contains(&char_id) {
            ext.characters_warped.push(char_id);
            eve_server::services::cosmetics_ui_ops::save_cosmetics_ui_state(snap, &ext);
        }
    });

    let warped_rep2 = ctx
        .dispatch_rpc(&mut sess, "monolithTaskTrackerMgr", None, "HasEverWarped", &[], &kw, None)
        .await
        .expect("HasEverWarped after warp");
    assert_eq!(warped_rep2, PyRep::Bool(true));

    // 3. monolithTaskTrackerMgr.UpdateClientAchievmentsAndCounters
    let update_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "monolithTaskTrackerMgr",
            None,
            "UpdateClientAchievmentsAndCounters",
            &[
                PyRep::List(vec![PyRep::Integer(101)]),
                PyRep::Dict(vec![(PyRep::String("jumps".into()), PyRep::Integer(10))]),
            ],
            &kw,
            None,
        )
        .await
        .expect("UpdateClientAchievmentsAndCounters");
    assert_eq!(update_rep, PyRep::None);

    // Verify achievement state was persisted
    let achievements_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "monolithTaskTrackerMgr",
            None,
            "GetCompletedAchievementsAndClientEventCount",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetCompletedAchievementsAndClientEventCount");
    let completed_dict = achievements_rep.dict_get("completedDict").expect("completedDict");
    let has_101 = match completed_dict {
        PyRep::Dict(entries) => entries.iter().any(|(k, v)| k.as_i64() == Some(101) && v == &PyRep::Bool(true)),
        _ => false,
    };
    assert!(has_101, "Achievement 101 should be completed in completedDict");
    let event_dict = achievements_rep.dict_get("eventDict").expect("eventDict");
    assert_eq!(event_dict.dict_get("jumps"), Some(&PyRep::Integer(10)));

    // 4. fighterMgr.CmdScoopAbandonedFighterFromSpace
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 23757; // Archon carrier hull with fighter bay
        } else {
            snap.items.insert(
                ship_id,
                eve_db::InventoryItemRecord {
                    item_id: ship_id,
                    type_id: 23757, // Archon carrier hull
                    owner_id: char_id,
                    location_id: sys_id as i64,
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    ..Default::default()
                },
            );
        }
    });
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let fighter_ball_id = 88_001i64;
    let fighter_item_id = 99_001i64;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.spawn_ball(make_test_ball(ship_id, 23757, glam::DVec3::ZERO)).await.unwrap();
    sys_handle.spawn_ball(make_test_ball(fighter_ball_id, 40_555, glam::DVec3::new(100.0, 0.0, 0.0))).await.unwrap();

    // Negative 1: invalid ID <= 0
    let err_invalid = ctx
        .dispatch_rpc(&mut sess, "fighterMgr", None, "CmdScoopAbandonedFighterFromSpace", &[PyRep::Integer(0)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_invalid.code, "InvalidFighterID");

    // Negative 2: fighter not found
    let err_not_found = ctx
        .dispatch_rpc(&mut sess, "fighterMgr", None, "CmdScoopAbandonedFighterFromSpace", &[PyRep::Integer(999_999)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_not_found.code, "FighterNotFound");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fighter_item_id,
            eve_db::InventoryItemRecord {
                item_id: fighter_item_id,
                type_id: 40_555,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    ctx.active_fighters.write().unwrap().insert(
        fighter_ball_id,
        ActiveFighterSquadron {
            ball_id: fighter_ball_id,
            item_id: fighter_item_id,
            type_id: 40_555,
            owner_char_id: char_id,
            parent_ship_id: ship_id,
            tube_index: 0,
            squadron_size: 9,
            target_id: None,
            is_abandoned: false,
            mwd_active: false,
        },
    );

    // Negative 3: fighter is not abandoned
    let err_not_abandoned = ctx
        .dispatch_rpc(&mut sess, "fighterMgr", None, "CmdScoopAbandonedFighterFromSpace", &[PyRep::Integer(fighter_ball_id)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_not_abandoned.code, "FighterNotAbandoned");

    // Negative 4: fighter owned by another pilot in a different corporation
    ctx.active_fighters.write().unwrap().get_mut(&fighter_ball_id).unwrap().is_abandoned = true;
    ctx.active_fighters.write().unwrap().get_mut(&fighter_ball_id).unwrap().owner_char_id = 90_000_999;
    let err_not_owner = ctx
        .dispatch_rpc(&mut sess, "fighterMgr", None, "CmdScoopAbandonedFighterFromSpace", &[PyRep::Integer(fighter_ball_id)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_not_owner.code, "NotFighterOwner");

    // Negative 5: cannot scoop while docked
    ctx.active_fighters.write().unwrap().get_mut(&fighter_ball_id).unwrap().owner_char_id = char_id;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_003_760);
        }
    });
    let err_docked = ctx
        .dispatch_rpc(&mut sess, "fighterMgr", None, "CmdScoopAbandonedFighterFromSpace", &[PyRep::Integer(fighter_ball_id)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_docked.code, "ShipDocked");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
        }
    });
    sess.station_id = None;

    // Negative 6: fighter out of range (> 2500m)
    let far_fighter_ball_id = 88_002i64;
    sys_handle.spawn_ball(make_test_ball(far_fighter_ball_id, 40_555, glam::DVec3::new(5000.0, 0.0, 0.0))).await.unwrap();
    ctx.active_fighters.write().unwrap().insert(
        far_fighter_ball_id,
        ActiveFighterSquadron {
            ball_id: far_fighter_ball_id,
            item_id: fighter_item_id,
            type_id: 40_555,
            owner_char_id: char_id,
            parent_ship_id: ship_id,
            tube_index: 1,
            squadron_size: 9,
            target_id: None,
            is_abandoned: true,
            mwd_active: false,
        },
    );
    let err_range = ctx
        .dispatch_rpc(&mut sess, "fighterMgr", None, "CmdScoopAbandonedFighterFromSpace", &[PyRep::Integer(far_fighter_ball_id)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_range.code, "FighterOutOfRange");

    // Negative 7: ship with no fighter bay capacity
    let no_bay_ship_id = 100_000_099i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(no_bay_ship_id, eve_db::InventoryItemRecord {
            item_id: no_bay_ship_id,
            type_id: 606, // Corvette with no fighter bay
            owner_id: char_id,
            location_id: sys_id as i64,
            flag: eve_db::InventoryFlag::None,
            quantity: 1,
            ..Default::default()
        });
    });
    sys_handle.spawn_ball(make_test_ball(no_bay_ship_id, 606, glam::DVec3::ZERO)).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = no_bay_ship_id;
        }
    });
    sess.ship_id = Some(no_bay_ship_id);
    let err_no_bay = ctx
        .dispatch_rpc(&mut sess, "fighterMgr", None, "CmdScoopAbandonedFighterFromSpace", &[PyRep::Integer(fighter_ball_id)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_no_bay.code, "NoFighterBay");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
        }
    });
    sess.ship_id = Some(ship_id);

    // Positive: successfully scoop abandoned fighter
    let scoop_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdScoopAbandonedFighterFromSpace",
            &[PyRep::Integer(fighter_ball_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdScoopAbandonedFighterFromSpace");
    assert_eq!(scoop_rep, PyRep::Bool(true));

    // Verify fighter item moved into ship's FighterBay
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&fighter_item_id).expect("fighter item in db");
        assert_eq!(it.location_id, ship_id);
        assert_eq!(it.flag, eve_db::InventoryFlag::FighterBay);
    });
    // Verify fighter removed from active_fighters
    assert!(!ctx.active_fighters.read().unwrap().contains_key(&fighter_ball_id));
}

#[tokio::test]
// parity: rpc:rewardMgr.GetRewardLPLogs
// parity: rpc:colonyResourcesMgr.GetMyCorporationSkyhooks
// parity: rpc:devIndexManager.GetAllDevelopmentIndices
// parity: rpc:epicArcStatus.AgentHasEpicMissionsForCharacter
async fn test_rewards_sov_and_epic_arc_status() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(29_102);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let char_id = sess.character_id.expect("char_id");
    let corp_id = 98_000_101i64;
    let sys_id = 30_000_142u32;
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);

    // 1. rewardMgr.GetRewardLPLogs
    let test_corp_id = 1000125i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.loyalty_points.insert(test_corp_id, 25_000);
        }
    });
    let lplogs_rep = ctx
        .dispatch_rpc(&mut sess, "rewardMgr", None, "GetRewardLPLogs", &[], &kw, None)
        .await
        .expect("GetRewardLPLogs");
    let PyRep::List(logs) = &lplogs_rep else { panic!("Expected List") };
    assert!(!logs.is_empty(), "LP logs should contain seeded corp LP");
    let found = logs.iter().any(|l| {
        l.dict_get("corpID") == Some(&PyRep::Integer(test_corp_id))
            && l.dict_get("amount") == Some(&PyRep::Integer(25_000))
    });
    assert!(found, "LP logs should contain seeded corp 1000125 LP entry");

    // 2. rewardMgr.GetDelayedRewardsByGroupIDs
    ctx.db.with_mut_snapshot(|snap| {
        eve_server::services::cosmetics_ui_ops::seed_delayed_reward(
            snap,
            char_id,
            eve_server::services::cosmetics_ui_ops::DelayedRewardEntry {
                reward_group_id: 501,
                reward_type_id: 1,
                quantity: 5000,
            },
        );
    });

    let delayed_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "rewardMgr",
            None,
            "GetDelayedRewardsByGroupIDs",
            &[PyRep::List(vec![PyRep::Integer(501), PyRep::Integer(999)])],
            &kw,
            None,
        )
        .await
        .expect("GetDelayedRewardsByGroupIDs");
    let PyRep::Dict(delayed_dict) = &delayed_rep else { panic!("Expected Dict") };
    let group_501 = delayed_dict.iter().find(|(k, _)| k == &PyRep::Integer(501)).expect("Group 501 delayed rewards found");
    let (_, PyRep::List(rew_501)) = group_501 else { panic!("Expected List") };
    assert_eq!(rew_501.len(), 1);
    assert_eq!(rew_501[0].dict_get("rewardGroupID"), Some(&PyRep::Integer(501)));
    assert_eq!(rew_501[0].dict_get("rewardQuantity"), Some(&PyRep::Integer(5000)));

    let group_999 = delayed_dict.iter().find(|(k, _)| k == &PyRep::Integer(999)).expect("Group 999 delayed rewards found");
    let (_, PyRep::List(rew_999)) = group_999 else { panic!("Expected List") };
    assert!(rew_999.is_empty(), "Unseeded group 999 should have empty reward list");

    // 3. colonyResourcesMgr.GetMyCorporationSkyhooks
    let skyhook_id = 60_000_501i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        snap.structures.insert(
            skyhook_id,
            eve_db::UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Test Corp Skyhook".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 5000.0,
                armor_hp: 5000.0,
                hull_hp: 5000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });

    let skyhooks_rep = ctx
        .dispatch_rpc(&mut sess, "colonyResourcesMgr", None, "GetMyCorporationSkyhooks", &[], &kw, None)
        .await
        .expect("GetMyCorporationSkyhooks");
    let PyRep::List(skyhook_list) = &skyhooks_rep else { panic!("Expected List") };
    assert!(!skyhook_list.is_empty(), "Skyhook list not empty");
    assert_eq!(skyhook_list[0].dict_get("skyhookID"), Some(&PyRep::Integer(skyhook_id)));
    assert_eq!(skyhook_list[0].dict_get("corporationID"), Some(&PyRep::Integer(corp_id)));
    assert_eq!(skyhook_list[0].dict_get("reagentTypeID"), Some(&PyRep::Integer(0)));
    assert_eq!(skyhook_list[0].dict_get("reagentQuantity"), Some(&PyRep::Integer(0)));

    // 4. devIndexManager.GetAllDevelopmentIndices
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.entry(sys_id).or_default();
        st.military_index = 3;
        st.industrial_index = 2;
        st.strategic_index = 1;
    });

    let indices_rep = ctx
        .dispatch_rpc(&mut sess, "devIndexManager", None, "GetAllDevelopmentIndices", &[], &kw, None)
        .await
        .expect("GetAllDevelopmentIndices");
    let PyRep::Dict(idx_dict) = &indices_rep else { panic!("Expected Dict") };
    let sys_indices = idx_dict.iter().find(|(k, _)| k == &PyRep::Integer(sys_id as i64));
    assert!(sys_indices.is_some(), "System indices found");
    let (_, idx_kv) = sys_indices.unwrap();
    assert_eq!(idx_kv.dict_get("military"), Some(&PyRep::Integer(3)));
    assert_eq!(idx_kv.dict_get("industrial"), Some(&PyRep::Integer(2)));

    // 5. epicArcStatus.AgentHasEpicMissionsForCharacter
    // Non-epic agent returns false
    let non_epic = ctx
        .dispatch_rpc(
            &mut sess,
            "epicArcStatus",
            None,
            "AgentHasEpicMissionsForCharacter",
            &[PyRep::Integer(999_999_999)],
            &kw,
            None,
        )
        .await
        .expect("AgentHasEpicMissionsForCharacter non-epic");
    assert_eq!(non_epic, PyRep::Bool(false));

    let agent_id = 3_019_356i64; // Blood-Stained Stars starter agent
    let epic_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "epicArcStatus",
            None,
            "AgentHasEpicMissionsForCharacter",
            &[PyRep::Integer(agent_id)],
            &kw,
            None,
        )
        .await
        .expect("AgentHasEpicMissionsForCharacter");
    assert_eq!(epic_rep, PyRep::Bool(true));
}

#[tokio::test]
// parity: rpc:infoGatheringMgr.GetStateAndConfig
// parity: rpc:infoGatheringMgr.LogInfoEventsFromClient
// parity: rpc:clientStatLogger.LogString
async fn test_telemetry_logging_and_load_service() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(29_103);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // 1. infoGatheringMgr.GetStateAndConfig
    let cfg_rep = ctx
        .dispatch_rpc(&mut sess, "infoGatheringMgr", None, "GetStateAndConfig", &[], &kw, None)
        .await
        .expect("GetStateAndConfig");
    assert!(is_keyval(&cfg_rep));
    assert_eq!(cfg_rep.dict_get("isEnabled"), Some(&PyRep::Integer(0)));
    assert_eq!(cfg_rep.dict_get("clientWorkerInterval"), Some(&PyRep::Integer(600_000)));

    // 2. infoGatheringMgr.LogInfoEventsFromClient
    let log_info = ctx
        .dispatch_rpc(
            &mut sess,
            "infoGatheringMgr",
            None,
            "LogInfoEventsFromClient",
            &[PyRep::List(vec![PyRep::String("ui_open_window".into())])],
            &kw,
            None,
        )
        .await
        .expect("LogInfoEventsFromClient");
    assert_eq!(log_info, PyRep::None);

    // 3. clientStatLogger.LogString
    let log_str = ctx
        .dispatch_rpc(
            &mut sess,
            "clientStatLogger",
            None,
            "LogString",
            &[PyRep::String("render_fps: 60".into())],
            &kw,
            None,
        )
        .await
        .expect("LogString");
    assert_eq!(log_str, PyRep::None);

    // 4. externalQueueMgr.PublishProxyEvent
    let pub_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "externalQueueMgr",
            None,
            "PublishProxyEvent",
            &[PyRep::String("ClusterNodeHeartbeat".into()), PyRep::Integer(1001)],
            &kw,
            None,
        )
        .await
        .expect("PublishProxyEvent");
    assert_eq!(pub_rep, PyRep::Bool(true));
    {
        let q = ctx.proxy_event_queue.read().unwrap();
        let found = q.iter().any(|r| r.event_name == "ClusterNodeHeartbeat" && r.payload == PyRep::Integer(1001));
        assert!(found, "Event record should be queued in ctx.proxy_event_queue");
    }
}
