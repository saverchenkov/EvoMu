//! Second half of end-to-end journey helpers (`E2E-09` through `E2E-17`).

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;

pub(super) async fn run_e2e_09_to_12(ctx: &ServerContext, session: &mut SessionState) {
    let empty = HashMap::new();
    let jita_handle = ctx.systems.get_or_spawn(30_000_142);

    let undock_m = ctx
        .dispatch_rpc(session, "stationSvc", None, "Undock", &[], &empty, None)
        .await
        .unwrap();
    let PyRep::KeyVal(um_kv) = undock_m else {
        panic!("Expected Undock KeyVal");
    };
    let mut cur_b_id = um_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(session, "beyonce", Some(cur_b_id), "CmdStop", &[], &empty, None)
        .await
        .unwrap();
    if let (Some(ship_id), Ok(Some((_, convoy_ball)))) = (
        session.ship_id,
        jita_handle.get_ball_snapshot(800_000_001).await,
    ) {
        jita_handle
            .set_ball_position(ship_id, convoy_ball.position + glam::DVec3::new(1_000.0, 0.0, 0.0))
            .await;
    }

    // E2E-17 (while in space in Jita): Living World 3-Tier LOD Materialization (auto-triggered on Undock), Convoy Combat Destruction, & +15% NPC Price Spike
    let non_gm_mat = crate::services::cosmetics_events::materialize_system_lod2(
        ctx,
        session,
        &[PyRep::Integer(30_000_142)],
    )
    .await;
    assert_eq!(non_gm_mat.unwrap_err().code, "InsufficientRole");
    let non_gm_int = crate::services::cosmetics_events::interdict_convoy(
        ctx,
        session,
        &[PyRep::Integer(800_000_001)],
    )
    .await;
    assert_eq!(non_gm_int.unwrap_err().code, "InsufficientRole");
    let mut lw_destroyed = false;
    let mut lw_new_ask = 0.0;
    for _ in 0..10 {
        for _ in 0..4 {
            let _ = jita_handle.step_manual(1.0).await.unwrap();
        }
        let lw_res = ctx
            .dispatch_rpc(
                session,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(800_000_001)],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(lw_kv) = lw_res else {
            panic!("Expected Living World combat destruction KeyVal");
        };
        if lw_kv.get("destroyed") == Some(&PyRep::Bool(true)) {
            lw_destroyed = true;
            lw_new_ask = lw_kv.get("newDestinationAskISK").unwrap().as_f64().unwrap();
            break;
        }
    }
    assert!(lw_destroyed);
    assert!((lw_new_ask - 5.0).abs() < 1e-6);

    // E2E-09: Jump Jita (30000142) -> Perimeter (30000144) for L1 Caldari Navy Security BasicAgent 3012018 (`Ogyura Unao`),
    // run Combat NPC Room Clear, Acceleration Gate (`keeper`), & 16-Mission Storyline Trigger, then return to Jita 4-4.
    ctx.dispatch_rpc(
        session,
        "beyonce",
        Some(cur_b_id),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(50_001_249)],
        &empty,
        None,
    )
    .await
    .unwrap();
    for _ in 0..200 {
        let _ = jita_handle.step_manual(1.0).await.unwrap();
    }
    ctx.advance_secs(30);
    let jump_perim = ctx
        .dispatch_rpc(
            session,
            "beyonce",
            Some(cur_b_id),
            "CmdStargateJump",
            &[PyRep::Integer(50_001_249)],
            &empty,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(jp_kv) = jump_perim else {
        panic!("Expected Perimeter jump KeyVal");
    };
    cur_b_id = jp_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    let perim_handle = ctx.systems.get_or_spawn(30_000_144);
    ctx.dispatch_rpc(session, "beyonce", Some(cur_b_id), "CmdStop", &[], &empty, None)
        .await
        .unwrap();
    let perim_agent_oid = ctx.bind_object(session, "agentMgr", 3_012_018).unwrap();

    let mut storyline_triggered = false;
    for _ in 1..=16i64 {
        let req_m = ctx
            .dispatch_rpc(
                session,
                "agentMgr",
                Some(perim_agent_oid),
                "DoAction",
                &[PyRep::Integer(2)],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(req_mkv) = req_m else {
            panic!("Expected DoAction(RequestMission) KeyVal");
        };
        let mid = req_mkv.get("missionID").unwrap().as_i64().unwrap();
        let acc_m = ctx
            .dispatch_rpc(
                session,
                "agentMgr",
                Some(perim_agent_oid),
                "DoAction",
                &[PyRep::Integer(3), PyRep::Integer(mid)],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(acc_mkv) = acc_m else {
            panic!("Expected DoAction(AcceptMission) KeyVal");
        };
        let npc_ball_id = acc_mkv.get("spawnedNpcBallID").unwrap().as_i64().unwrap();
        ctx.set_invention_roll_override(Some(0.001));
        let mut destroyed = false;
        for _ in 0..200 {
            for _ in 0..4 {
                let _ = perim_handle.step_manual(1.0).await.unwrap();
            }
            let shot = ctx
                .dispatch_rpc(
                    session,
                    "dogmaIM",
                    None,
                    "Activate",
                    &[PyRep::Integer(npc_ball_id)],
                    &empty,
                    None,
                )
                .await
                .unwrap();
            if let PyRep::KeyVal(skv) = shot {
                if skv.get("destroyed") == Some(&PyRep::Bool(true)) {
                    destroyed = true;
                    break;
                }
            }
        }
        ctx.set_invention_roll_override(None);
        assert!(destroyed, "NPC should be destroyed during combat sequence");
        ctx.dispatch_rpc(
            session,
            "keeper",
            None,
            "ActivateAccelerationGate",
            &[PyRep::Integer(mid)],
            &empty,
            None,
        )
        .await
        .unwrap();
        let mres = ctx
            .dispatch_rpc(
                session,
                "agentMgr",
                Some(perim_agent_oid),
                "DoAction",
                &[PyRep::Integer(6), PyRep::Integer(mid)],
                &empty,
                None,
            )
            .await
            .unwrap();
        if let PyRep::KeyVal(mkv) = mres {
            if mkv.get("storylineTriggered") == Some(&PyRep::Bool(true)) {
                storyline_triggered = true;
            }
        }
    }
    assert!(
        storyline_triggered,
        "16th mission must trigger storyline offer"
    );

    // Return Perimeter (30000144, gate 50002185) -> Jita (30000142) and warp back to Jita 4-4 (60003760)
    ctx.advance_secs(65);
    let jump_jita = ctx
        .dispatch_rpc(
            session,
            "beyonce",
            Some(cur_b_id),
            "CmdStargateJump",
            &[PyRep::Integer(50_002_185)],
            &empty,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(jj_kv) = jump_jita else {
        panic!("Expected Jita return jump KeyVal");
    };
    cur_b_id = jj_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(
        session,
        "beyonce",
        Some(cur_b_id),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(60_003_760)],
        &empty,
        None,
    )
    .await
    .unwrap();
    for _ in 0..200 {
        let _ = jita_handle.step_manual(1.0).await.unwrap();
    }
    ctx.advance_secs(30);
    ctx.dispatch_rpc(session, "beyonce", Some(cur_b_id), "CmdStop", &[], &empty, None)
        .await
        .unwrap();

    // E2E-10: Fleet Creation, EveMail, & Bookmark Creation
    ctx.dispatch_rpc(session, "fleetObjectHandler", None, "CreateFleet", &[], &empty, None)
        .await
        .unwrap();
    ctx.dispatch_rpc(
        session,
        "bookmark",
        None,
        "BookmarkLocation",
        &[PyRep::String("Wormhole Entry".into())],
        &empty,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        session,
        "mailMgr",
        None,
        "SendMail",
        &[
            PyRep::Integer(90_000_002),
            PyRep::String("o7".into()),
            PyRep::String("Fly safe!".into()),
        ],
        &empty,
        None,
    )
    .await
    .unwrap();

    // E2E-12: CrimeWatch Illegal High-Sec Attack (with rival capsuleer 90_000_002 undocked on grid in Jita 1.0), CONCORD Response, & Kill Right Activation
    let mut sess2 = SessionState::new(200);
    ctx.dispatch_rpc(
        &mut sess2,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("other".into()),
            PyRep::String("other-secret".into()),
        ],
        &empty,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut sess2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
        &empty,
        None,
    )
    .await
    .unwrap();
    ctx.advance_secs(60);
    ctx.dispatch_rpc(&mut sess2, "stationSvc", None, "Undock", &[], &empty, None)
        .await
        .unwrap();
    if let (Some(s1_ship), Some(s2_ship)) = (session.ship_id, sess2.ship_id) {
        if let Ok(Some((_, s1_ball))) = jita_handle.get_ball_snapshot(s1_ship).await {
            jita_handle
                .set_ball_position(s2_ship, s1_ball.position + glam::DVec3::new(1_000.0, 0.0, 0.0))
                .await;
        }
    }

    for _ in 0..4 {
        let _ = jita_handle.step_manual(1.0).await.unwrap();
    }
    let cw_res = ctx
        .dispatch_rpc(
            session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(90_000_002)],
            &empty,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(cw_kv) = cw_res else {
        panic!("Expected CrimeWatch KeyVal");
    };
    assert_ne!(session.ship_id, Some(100_000_001));
    assert!(ctx.db.get_item(100_000_001).is_none());
    let kr_id = cw_kv.get("killRightID").unwrap().as_i64().unwrap();

    // CONCORD destroyed 100_000_001 and ejected 90_000_001 into an in-space Capsule;
    // dock at Jita 4-4 (60_003_760), board spare Rifter 100_000_029 in hangar, undock via RPC, and verify legal kill right engagement by sess2
    ctx.advance_secs(905);
    ctx.dispatch_rpc(session, "stationSvc", None, "Dock", &[PyRep::Integer(60_003_760)], &empty, None).await.unwrap();
    ctx.advance_secs(12);
    ctx.dispatch_rpc(
        session,
        "ship",
        None,
        "Board",
        &[PyRep::Integer(100_000_029)],
        &empty,
        None,
    )
    .await
    .unwrap();
    ctx.advance_secs(12);
    let undock_29 = ctx
        .dispatch_rpc(session, "stationSvc", None, "Undock", &[], &empty, None)
        .await
        .unwrap();
    let PyRep::KeyVal(u29_kv) = undock_29 else {
        panic!("Expected Undock KeyVal");
    };
    let b29 = u29_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(session, "beyonce", Some(b29), "CmdStop", &[], &empty, None)
        .await
        .unwrap();
    ctx.dispatch_rpc(
        &mut sess2,
        "killRightMgr",
        None,
        "ActivateKillRight",
        &[PyRep::Integer(kr_id)],
        &empty,
        None,
    )
    .await
    .unwrap();
    let b_sess2 = ctx.bind_object(&mut sess2, "beyonce", 30_000_142).unwrap();
    ctx.dispatch_rpc(&mut sess2, "beyonce", Some(b_sess2), "CmdStop", &[], &empty, None)
        .await
        .unwrap();
    ctx.sync_session(session);
    ctx.sync_session(&mut sess2);
    if let (Some(s1_ship), Some(s2_ship)) = (session.ship_id, sess2.ship_id) {
        if let Ok(Some((_, s2_ball))) = jita_handle.get_ball_snapshot(s2_ship).await {
            jita_handle
                .set_ball_position(s1_ship, s2_ball.position + glam::DVec3::new(1_000.0, 0.0, 0.0))
                .await;
        }
        ctx.db.with_mut_snapshot(|snap| {
            let kr_active = snap
                .kill_rights
                .get(&kr_id)
                .map(|kr| kr.is_active_suspect)
                .unwrap_or(false);
            assert!(kr_active, "ActivateKillRight must activate suspect state");
            if let Some(ch1) = snap.characters.get_mut(&90_000_001) {
                ch1.security_status = -5.0;
            }
            if let Some(ch) = snap.characters.get_mut(&90_000_002) {
                ch.clone_grade = 2;
            }
            let s1_skills = snap.skills.get(&90_000_001).cloned().unwrap_or_default();
            let skills = snap.skills.entry(90_000_002).or_default();
            skills.extend(s1_skills);
            for (skill_id, level, sp) in [
                (3300_u32, 5_u8, 256_000.0_f64),
                (3311, 5, 256_000.0),
                (3318, 5, 256_000.0),
                (3388, 5, 256_000.0),
                (3413, 5, 256_000.0),
                (3418, 5, 256_000.0),
                (3426, 5, 256_000.0),
                (21611, 5, 256_000.0),
            ] {
                skills.insert(
                    skill_id,
                    eve_db::TrainedSkillRecord {
                        type_id: skill_id,
                        level,
                        skill_points: sp,
                    },
                );
            }
            let hi_ids: Vec<i64> = snap
                .items
                .values()
                .filter(|it| {
                    it.location_id == s2_ship && matches!(it.flag, eve_db::InventoryFlag::HiSlot0)
                })
                .map(|it| it.item_id)
                .collect();
            for wid in hi_ids {
                snap.module_online_states.insert(wid, true);
            }
        });
        ctx.invalidate_ship_fitting(s2_ship);
        let _ = ctx.get_or_rebuild_ship_fitting(90_000_002, s2_ship);
    }
    let mut legal_destroyed = false;
    for _ in 0..120 {
        for _ in 0..4 {
            let _ = jita_handle.step_manual(1.0).await.unwrap();
        }
        let legal_res = ctx
            .dispatch_rpc(
                &mut sess2,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(90_000_001)],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(lkv) = legal_res else {
            panic!("Expected legal kill KeyVal");
        };
        if lkv.get("destroyed") == Some(&PyRep::Bool(true)) {
            assert_eq!(lkv.get("legalEngagement"), Some(&PyRep::Bool(true)));
            legal_destroyed = true;
            break;
        }
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch1) = snap.characters.get_mut(&90_000_001) {
            ch1.security_status = -1.0;
        }
    });
    assert!(
        legal_destroyed,
        "Target ship should be destroyed across volley"
    );
    assert!(
        ctx.db.get_item(100_000_029).is_none() && ctx.db.get_item(100_000_030).is_none(),
        "PvP ship destruction must remove destroyed ship and fitted modules from DB"
    );
    // After 100_000_029 was destroyed in PvP, session is in an ejected Capsule in space; dock at 60_003_760, board 100_000_035, and undock
    ctx.dispatch_rpc(
        session,
        "stationSvc",
        None,
        "Dock",
        &[PyRep::Integer(60_003_760)],
        &empty,
        None,
    )
    .await
    .unwrap();
    ctx.advance_secs(60);
    ctx.dispatch_rpc(
        session,
        "ship",
        None,
        "Board",
        &[PyRep::Integer(100_000_035)],
        &empty,
        None,
    )
    .await
    .unwrap();
    ctx.advance_secs(60);
    ctx.dispatch_rpc(session, "stationSvc", None, "Undock", &[], &empty, None)
        .await
        .unwrap();
}
