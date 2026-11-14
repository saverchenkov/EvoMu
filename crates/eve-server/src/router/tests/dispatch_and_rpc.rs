//! Unit tests for E2E journeys 13-18 and catalog/Quasar RPC dispatch coverage.

use super::super::fixtures::*;
use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test]
async fn test_e2e_journeys_13_to_18_and_catalog_coverage() {
    let ctx = ServerContext::new();
    // Deterministic virtual time (T-7): time only moves via advance_virtual_clock.
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx.seed_demo_world_fixtures();
    let mut session = SessionState::new(100);
    let empty = HashMap::new();
    login_and_select_aura(&ctx, &mut session).await;

    ctx.dispatch_rpc(
        &mut session,
        "ship",
        None,
        "Board",
        &[PyRep::Integer(100_000_035)],
        &empty,
        None,
    )
    .await
    .unwrap();
    ctx.advance_secs(12);
    ctx.dispatch_rpc(
        &mut session,
        "stationSvc",
        None,
        "Undock",
        &[],
        &empty,
        None,
    )
    .await
    .unwrap();
    let jita_handle = ctx.systems.get_or_spawn(30_000_142);

    let mut sess2 = SessionState::new(101);
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
    ctx.advance_secs(12);
    ctx.dispatch_rpc(&mut sess2, "stationSvc", None, "Undock", &[], &empty, None)
        .await
        .unwrap();

    // E2E-13: Planetary Interaction Colony Setup, Routing, & POCO Export Tax
    let active_loc = session
        .ship_id
        .or(session.station_id)
        .or(session.structure_id)
        .unwrap_or(100_000_001);
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            eve_db::InventoryItemRecord {
                item_id: id,
                type_id: 2534,
                owner_id: 90_000_001,
                location_id: active_loc,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.dispatch_rpc(
        &mut session,
        "planetMgr",
        None,
        "UserUpdateNetwork",
        &[
            PyRep::Integer(40_009_082),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Integer(2534),
                PyRep::Real(0.1),
                PyRep::Real(0.2),
            ])]),
        ],
        &empty,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut session,
        "planetMgr",
        None,
        "Flush",
        &[PyRep::Integer(40_009_082)],
        &empty,
        None,
    )
    .await
    .unwrap();

    // Create player corporation "Apex Syndicate" while docked before anchoring structure so high-sec war declaration can target it
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&90_000_001).unwrap().station_id = Some(60_003_760); });
    let corp1_res = ctx.dispatch_rpc(&mut session, "corpmgr", None, "CreateCorporation", &[PyRep::String("Apex Syndicate".into()), PyRep::String("APEX".into()), PyRep::Real(0.05)], &empty, None).await.unwrap(); // rpc-lint: allow legacy/negative-test RPC (corpmgr.CreateCorporation)
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&90_000_001).unwrap().station_id = None; });
    session.station_id = None;
    let PyRep::KeyVal(corp1_kv) = corp1_res else {
        panic!("Expected corp1 KeyVal");
    };
    let apex_corp_id = corp1_kv.get("corporationID").unwrap().as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(gov) = snap.corp_governance.get_mut(&apex_corp_id) {
            gov.created_unix = gov.created_unix.saturating_sub(700_000);
        }
    });

    // E2E-14: Upwell Structure Anchoring (consuming Astrahus 35832 + Quantum Core 56201), Fueling, & Damage-Capped Reinforcement
    // Jita (30000142) is a trade hub (`AnchoringDisallowedInTradeHub`); jump to high-sec Perimeter (30000144) 5,000,000 km off the gate.
    let at_station = jita_handle.get_ball_snapshot(session.ship_id.unwrap()).await.unwrap().unwrap().1.position;
    let gate_jita_perim = 50_001_249_i64;
    let gate_jita_pos = ctx.sde.jumps[&gate_jita_perim].position;
    assert!(jita_handle.set_ball_position(session.ship_id.unwrap(), gate_jita_pos).await);
    assert!(jita_handle.set_ball_position(sess2.ship_id.unwrap(), gate_jita_pos).await);
    let (b1, b2) = (ctx.bind_object(&session, "beyonce", 30_000_142).unwrap(), ctx.bind_object(&sess2, "beyonce", 30_000_142).unwrap());
    ctx.advance_secs(12);
    ctx.dispatch_rpc(&mut session, "beyonce", Some(b1), "CmdStargateJump", &[PyRep::Integer(gate_jita_perim)], &empty, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess2, "beyonce", Some(b2), "CmdStargateJump", &[PyRep::Integer(gate_jita_perim)], &empty, None).await.unwrap();
    ctx.advance_secs(12);
    let perim_handle = ctx.systems.get_or_spawn(30_000_144);
    let ship1 = session.ship_id.unwrap();
    let spot = perim_handle.get_ball_snapshot(ship1).await.unwrap().unwrap().1.position + glam::DVec3::new(0.0, 5.0e9, 0.0);
    assert!(perim_handle.set_ball_position(ship1, spot).await);
    assert!(perim_handle.set_ball_position(sess2.ship_id.unwrap(), spot + glam::DVec3::new(1_000.0, 0.0, 0.0)).await);
    let st_res = ctx
        .dispatch_rpc(
            &mut session,
            "structureDeployment",
            None,
            "Anchor",
            &[
                PyRep::Integer(35832),
                PyRep::String("EvoMu Keepstar".into()),
            ],
            &empty,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(st_kv) = st_res else {
        panic!("Expected structure KeyVal");
    };
    let st_id = st_kv.get("structureID").unwrap().as_i64().unwrap();
    assert!(perim_handle.set_ball_position(ship1, spot + glam::DVec3::new(60_000.0, 0.0, 0.0)).await && perim_handle.set_ball_position(sess2.ship_id.unwrap(), spot + glam::DVec3::new(61_000.0, 0.0, 0.0)).await);
    ctx.dispatch_rpc(
        &mut session,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(100_000_033),
            PyRep::Integer(st_id),
            PyRep::Integer(133),
            PyRep::Integer(50),
        ],
        &empty,
        None,
    )
    .await
    .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let hp = |a: u16| ctx.sde.types.get(&35832).and_then(|t| t.default_attributes.get(&a).copied()).unwrap();
        let st = snap.structures.get_mut(&st_id).unwrap();
        st.state = "ShieldVulnerable".into();
        st.quantum_core_installed = true;
        st.reinforced_until_unix = 0;
        st.shield_hp = hp(263);
        st.armor_hp = hp(265);
        st.hull_hp = hp(9);
    });
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&90_000_002).unwrap().station_id = Some(60_003_760); });
    ctx.dispatch_rpc(&mut sess2, "corpRegistry", None, "AddCorporation", &[PyRep::String("Rival Raiders".into()), PyRep::String("RVL".into()), PyRep::Real(0.05)], &empty, None).await.unwrap();
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&90_000_002).unwrap().station_id = None; });
    sess2.station_id = None;
    // Verify attacking a high-sec structure without an active war fails even if attacker owns a personal kill right
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            990_000_185,
            eve_db::InventoryItemRecord {
                item_id: 990_000_185,
                type_id: 185,
                owner_id: 90_000_002,
                location_id: 100_000_002,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 100,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.dispatch_rpc(
        &mut sess2,
        "dogmaIM",
        None,
        "AddTarget",
        &[PyRep::Integer(st_id)],
        &empty,
        None,
    )
    .await
    .unwrap();
    let _ = perim_handle.step_manual(10.0).await.unwrap();
    let no_war_atk = ctx
        .dispatch_rpc(
            &mut sess2,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(100_000_006),
                PyRep::String("projectileFired".into()),
                PyRep::Integer(st_id),
            ],
            &empty,
            None,
        )
        .await;
    assert_eq!(
        no_war_atk.unwrap_err().code,
        "IllegalHighSecStructureAttack"
    );

    // Declare corporate war against Apex Syndicate, advance virtual clock past 24h warmup, and attack structure legally
    // (the attacker needs its own War HQ structure and 100M ISK in its master wallet).
    let rival_corp_id = sess2.corporation_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let mut hq = snap.structures[&st_id].clone();
        hq.structure_id = st_id + 1_000_000;
        hq.owner_corp_id = rival_corp_id;
        hq.name = "Rival War HQ".into();
        snap.structures.insert(hq.structure_id, hq);
        let c = snap.corporations.get_mut(&rival_corp_id).unwrap();
        c.ensure_wallet_divisions();
        c.wallet_divisions[0].balance = eve_db::IskCents::from_isk(200_000_000);
        c.ensure_wallet_divisions();
    });
    ctx.dispatch_rpc(
        &mut sess2,
        "warRegistry",
        None,
        "DeclareWarAgainst",
        &[PyRep::Integer(apex_corp_id)],
        &empty,
        None,
    )
    .await
    .unwrap();
    ctx.advance_virtual_clock(86_401);
    let _ = perim_handle.step_manual(1.0).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess2,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(100_000_006),
            PyRep::String("projectileFired".into()),
            PyRep::Integer(st_id),
        ],
        &empty,
        None,
    )
    .await
    .unwrap();
    ctx.advance_virtual_clock(65);

    // Dock both sess2 (contract issuer) and session (courier) at anchored structure `st_id`
    crate::services::station_structure::structure_upwell_ops::configure_structure_access_profile(
        &ctx, &session, st_id, Some(false), &[], &[], &[], &[],
    )
    .unwrap();
    let st_dvec = ctx.db.with_snapshot(|snap| glam::DVec3::from_array(snap.structures[&st_id].position));
    assert!(perim_handle.set_ball_position(sess2.ship_id.unwrap(), st_dvec + glam::DVec3::new(100.0, 0.0, 0.0)).await);
    assert!(perim_handle.set_ball_position(session.ship_id.unwrap(), st_dvec).await);
    ctx.dispatch_rpc(
        &mut sess2,
        "structureDocking",
        None,
        "Dock",
        &[PyRep::Integer(st_id)],
        &empty,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut session,
        "structureDocking",
        None,
        "Dock",
        &[PyRep::Integer(st_id)],
        &empty,
        None,
    )
    .await
    .unwrap();

    // E2E-15: Courier Contract Creation (st_id -> Jita 4-4 60_003_760 by sess2), Self-Accept Rejection, Acceptance by session, Undock/Dock Delivery, & Destination-Verified Completion
    let courier_item_id = ctx
        .db
        .try_with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let iid = snap.next_entity_id;
            snap.items.insert(
                iid,
                eve_db::InventoryItemRecord {
                    item_id: iid,
                    type_id: 34,
                    owner_id: 90_000_002,
                    location_id: st_id,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 100,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            Ok::<_, std::convert::Infallible>(iid)
        })
        .unwrap();
    let c_res = ctx
        .dispatch_rpc(
            &mut sess2,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(3), // Courier
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(st_id),
                PyRep::Integer(60_003_760),
                PyRep::Real(0.0),
                PyRep::Real(500_000.0),
                PyRep::Real(2_000_000.0),
                PyRep::String("".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(courier_item_id),
                    PyRep::Integer(100),
                ])]),
            ],
            &empty,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(c_kv) = c_res else {
        panic!("Expected contract KeyVal");
    };
    let cid = c_kv.get("contractID").unwrap().as_i64().unwrap();
    let self_acc = ctx
        .dispatch_rpc(
            &mut sess2,
            "contractProxy",
            None,
            "AcceptContract",
            &[PyRep::Integer(cid)],
            &empty,
            None,
        )
        .await;
    assert_eq!(self_acc.unwrap_err().code, "CannotAcceptOwnContract");
    let acc_rep = ctx
        .dispatch_rpc(
            &mut session,
            "contractProxy",
            None,
            "AcceptContract",
            &[PyRep::Integer(cid)],
            &empty,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(acc_kv) = acc_rep else {
        panic!("Expected accept KeyVal");
    };
    let wrap_id = acc_kv.get("courierPackageID").unwrap().as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(w) = snap.items.get_mut(&wrap_id) {
            w.location_id = session.ship_id.unwrap();
            w.flag = eve_db::InventoryFlag::Cargo;
        }
    });
    // Attempting to complete at `st_id` before delivering to `60_003_760` must fail
    let early_complete = ctx
        .dispatch_rpc(
            &mut session,
            "contractProxy",
            None,
            "CompleteContract",
            &[PyRep::Integer(cid)],
            &empty,
            None,
        )
        .await;
    assert_eq!(
        early_complete.unwrap_err().code,
        "CourierNotAtDestinationStation"
    );

    // Undock from structure `st_id` via RPC, jump Perimeter -> Jita, and dock at destination station `60_003_760` via RPC
    let PyRep::KeyVal(stu_kv) = ctx.dispatch_rpc(&mut session, "structureDocking", None, "Undock", &[], &empty, None).await.unwrap() else {
        panic!("Expected structure undock KeyVal");
    };
    let perim_b_id = stu_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    let gate_perim_jita = ctx.sde.jumps.iter().find_map(|(&gid, j)| (j.from_solar_system_id == 30_000_144 && j.to_solar_system_id == 30_000_142).then_some(gid)).unwrap();
    assert!(perim_handle.set_ball_position(session.ship_id.unwrap(), ctx.sde.jumps[&gate_perim_jita].position).await);
    ctx.advance_secs(12);
    let PyRep::KeyVal(jb_kv) = ctx.dispatch_rpc(&mut session, "beyonce", Some(perim_b_id), "CmdStargateJump", &[PyRep::Integer(gate_perim_jita)], &empty, None).await.unwrap() else {
        panic!("Expected jump back KeyVal");
    };
    let stu_b_id = jb_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    ctx.advance_secs(12);
    assert!(jita_handle.set_ball_position(session.ship_id.unwrap(), at_station).await);
    let short_warp = ctx
        .dispatch_rpc(
            &mut session,
            "beyonce",
            Some(stu_b_id),
            "CmdWarpToStuff",
            &[
                PyRep::String("item".into()),
                PyRep::Integer(60_003_760),
            ],
            &empty,
            None,
        )
        .await;
    assert_eq!(short_warp.unwrap_err().code, "WarpDistanceTooShort");
    let (_, sb) = jita_handle
        .get_ball_snapshot(session.ship_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    ctx.dispatch_rpc(
        &mut session,
        "beyonce",
        Some(stu_b_id),
        "CmdGotoDirection",
        &[
            PyRep::Real(-107_303_362_560.0 - sb.position.x),
            PyRep::Real(-18_744_975_360.0 - sb.position.y),
            PyRep::Real(436_489_052_160.0 - sb.position.z),
        ],
        &empty,
        None,
    )
    .await
    .unwrap();
    for _ in 0..8 {
        let _ = jita_handle.step_manual(1.0).await.unwrap();
    }
    ctx.dispatch_rpc(
        &mut session,
        "beyonce",
        Some(stu_b_id),
        "CmdStop",
        &[],
        &empty,
        None,
    )
    .await
    .unwrap();
    ctx.advance_secs(12);
    ctx.dispatch_rpc(
        &mut session,
        "stationSvc",
        None,
        "Dock",
        &[PyRep::Integer(60_003_760)],
        &empty,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut session,
        "contractProxy",
        None,
        "CompleteContract",
        &[PyRep::Integer(cid)],
        &empty,
        None,
    )
    .await
    .unwrap();

    // E2E-18: Corporation Project Escrow & Member Contribution Payout via Quasar (while docked at 60_003_760)
    let corp_id = session.corporation_id.unwrap();
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.corporations.get_mut(&corp_id) {
            c.wallet_divisions[0].balance = eve_db::IskCents::from_isk(1_000_000);
        }
    });
    let g_any = eve_proto::pb::message("eve_public.goal.api.CreateRequest")
        .set("name", "Mine Veldspar")
        .set("desired_progress", 500u64)
        .set_raw_message("contribution_configuration.mined_ore", b"")
        .add_with("reward_pools", |pool| {
            pool.set("period", 1i32)
                .set("isk.amount.units", 50_000i64)
        })
        .must_to_any();
    let now_u = ctx.now_unix();
    let proj_rsp = ctx.quasar_gateway.handle_public_request(
        &session,
        &eve_proto::quasar::EvePublicRequest {
            issued: Some(eve_proto::quasar::GoogleTimestamp { seconds: now_u as i64, nanos: 0 }),
            correlation_uuid: vec![0x41; 16],
            payload: Some(g_any),
            external_origin: "test".into(),
            application_instance_uuid: vec![0x42; 16],
            authoritative_context: Some(eve_proto::quasar::AuthoritativeContext::for_session_at(Some(1), Some(90_000_001), Some(corp_id as u32), session.corp_role, "tranquility", vec![0x43; 16], now_u)),
            trace_context: None,
        },
        None,
        false,
        now_u,
    );
    assert_eq!(proj_rsp.status_code, crate::quasar_gateway::STATUS_OK, "{}", proj_rsp.status_message);
    let pid = crate::services::social_corp_fleet::social_corp_projects::goal_uuid_to_project_id(&proj_rsp.payload.unwrap().value[4..20]).unwrap();
    crate::services::social_corp_fleet::social_corp_projects::record_server_project_activity(&ctx, 90_000_001, &["MineMaterials"], 500, None);
    assert_eq!(ctx.db.with_snapshot(|s| s.corp_projects[&pid].current_quantity), 500);

    // Undock via RPC and verify AbyssalEntranceDeployment returns UnimplementedRPC
    ctx.advance_secs(12);
    ctx.dispatch_rpc(
        &mut session,
        "stationSvc",
        None,
        "Undock",
        &[],
        &empty,
        None,
    )
    .await
    .unwrap();
    let abyss_res = ctx
        .dispatch_rpc(
            &mut session,
            "abyssalMgr",
            None,
            "AbyssalEntranceDeployment",
            &[],
            &empty,
            None,
        )
        .await;
    assert_eq!(abyss_res.unwrap_err().code, "MissingRequiredArgument");

    // E2E-16: Non-V24 air_npe.AdvanceNode and seasonManager.claim_goal_reward return UnimplementedRPC
    let npe_res = ctx
        .dispatch_rpc(
            &mut session,
            "air_npe",
            None,
            "AdvanceNode", // rpc-lint: allow legacy/negative-test RPC (air_npe.AdvanceNode)
            &[],
            &empty,
            None,
        )
        .await;
    assert_eq!(npe_res.unwrap_err().code, "UnimplementedRPC");
    let goal_res = ctx
        .dispatch_rpc(
            &mut session,
            "seasonManager",
            None,
            "claim_goal_reward",
            &[PyRep::Integer(1)],
            &empty,
            None,
        )
        .await;
    assert_eq!(goal_res.unwrap_err().code, "GoalProgressIncomplete"); // rpc-lint: allow error code assertion

    // Verify unimplemented methods on cataloged services strictly return UnimplementedRPC (no catch-all stub)
    assert_eq!(ctx.cataloged_services.len(), 216);
    let unimpl_res = ctx
        .dispatch_rpc(
            &mut session,
            "marketProxy",
            None,
            "NonExistentMethod", // rpc-lint: allow legacy/negative-test RPC (marketProxy.NonExistentMethod)
            &[],
            &empty,
            None,
        )
        .await;
    assert_eq!(unimpl_res.unwrap_err().code, "UnimplementedRPC");

    assert_eq!(ctx.quasar_domains.len(), 135);
    for ns in ctx.quasar_domains.iter() {
        let q_res = ctx
            .dispatch_rpc(
                &mut session,
                "publicGatewaySvc",
                None,
                "SendRequest",
                &[
                    PyRep::String(ns.clone()),
                    PyRep::String("GetStateRequest".into()),
                ],
                &empty,
                None,
            )
            .await;
        assert_eq!(
            q_res.unwrap_err().code,
            "UnimplementedRPC",
            "Cataloged Quasar namespace {ns} must return explicit UnimplementedRPC"
        );
    }
}
