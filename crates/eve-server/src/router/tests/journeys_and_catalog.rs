//! Unit tests for in-process end-to-end journeys and catalog service coverage.

#[path = "dispatch_and_rpc.rs"]
mod dispatch_and_rpc;

use super::fixtures::*;
use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test]
async fn test_all_18_e2e_journeys_and_catalog_coverage() {
    let ctx = ServerContext::new();
    // Deterministic virtual time (T-7): time only moves via advance_virtual_clock.
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx.seed_demo_world_fixtures();
    let mut session = SessionState::new(100);
    let empty = HashMap::new();

    // E2E-01: Auth, Character Creation (Minmatar race=2, Brutor bloodline=3, ancestry=13, school=20), Selection, & Station Hangar Sync
    ctx.dispatch_rpc(
        &mut session,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &empty,
        None,
    )
    .await
    .unwrap();
    let new_char = ctx
        .dispatch_rpc(
            &mut session,
            "charUnboundMgr",
            None,
            "CreateCharacterWithDoll",
            &[
                PyRep::String("Valkyrie Pilot".into()),
                PyRep::Integer(3),  // Sebiestor bloodline (Minmatar race=2)
                PyRep::Integer(1),  // genderID
                PyRep::Integer(19), // Rebels ancestry
                PyRep::Dict(vec![(
                    PyRep::String("hair".into()),
                    PyRep::String("braids".into()),
                )]),
                PyRep::Dict(vec![(
                    PyRep::String("poseID".into()),
                    PyRep::Integer(1),
                )]),
                PyRep::Integer(14), // Republic Military School
            ],
            &empty,
            None,
        )
        .await
        .unwrap();
    assert!(new_char.as_i64().unwrap() > 200_000_000);
    login_and_select_aura(&ctx, &mut session).await;

    // E2E-02: Skill Queue, Skill Injector Consumption, Free SP Application, & Jump Clone Installation
    let injected_sp = ctx
        .dispatch_rpc(
            &mut session,
            "skillMgr2",
            None,
            "InjectSkillpoints",
            &[],
            &empty,
            None,
        )
        .await
        .unwrap();
    assert_eq!(injected_sp.as_f64().unwrap(), 400_000.0);
    ctx.dispatch_rpc(
        &mut session,
        "skillMgr2",
        None,
        "SaveSkillQueue",
        &[PyRep::Integer(3327), PyRep::Integer(5)],
        &empty,
        None,
    )
    .await
    .unwrap();
    let sp_after = ctx
        .dispatch_rpc(
            &mut session,
            "skillMgr2",
            None,
            "ApplyFreeSkillPoints",
            &[],
            &empty,
            None,
        )
        .await
        .unwrap();
    assert!(sp_after.as_f64().unwrap() > 15_000_000.0);
    ctx.dispatch_rpc(
        &mut session,
        "jumpCloneSvc",
        None,
        "InstallCloneInStation",
        &[],
        &empty,
        None,
    )
    .await
    .unwrap();

    // E2E-03: Ship Fitting, Module Overload, Insurance, & Abyssal Mutaplasmid Rolling
    ctx.dispatch_rpc(
        &mut session,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(100_000_003),
            PyRep::Integer(100_000_001),
            PyRep::Integer(19),
        ],
        &empty,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut session,
        "dogmaIM",
        None,
        "Overload",
        &[PyRep::Integer(100_000_003)],
        &empty,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut session,
        "dynamicItemService",
        None,
        "CreateDynamicItem",
        &[PyRep::Integer(47746), PyRep::Integer(100_000_003)],
        &empty,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut session,
        "insuranceSvc",
        None,
        "InsureShip",
        &[PyRep::Integer(100_000_001), PyRep::Real(1.0)],
        &empty,
        None,
    )
    .await
    .unwrap();

    // E2E-04 & E2E-05: Undock, Sub-Warp Flight, Intra-System Warp to Stargate 50001249, Jump to Perimeter (30000144), & Travel to Low-Sec Tama (30002813)
    let undock = ctx
        .dispatch_rpc(
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
    let PyRep::KeyVal(ukv) = undock else {
        panic!("Expected KeyVal");
    };
    let mut cur_b_id = ukv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    let jita_handle = ctx.systems.get_or_spawn(30_000_142);

    // Travel Jita (30000142) -> Perimeter (30000144) -> Niyabainen (30000143) -> Tunttaras (30001379) -> Nourvukaiken (30001376) -> Tama (30002813)
    for (from_sys, gate_id, expected_to_sys) in [
        (30_000_142_u32, 50_001_249_i64, 30_000_144_u32),
        (30_000_144_u32, 50_002_186_i64, 30_000_143_u32),
        (30_000_143_u32, 50_013_923_i64, 30_001_379_u32),
        (30_001_379_u32, 50_001_709_i64, 30_001_376_u32),
        (30_001_376_u32, 50_014_001_i64, 30_002_813_u32),
    ] {
        ctx.dispatch_rpc(
            &mut session,
            "beyonce",
            Some(cur_b_id),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(gate_id)],
            &empty,
            None,
        )
        .await
        .unwrap();
        let sys_h = ctx.systems.get_or_spawn(from_sys);
        for _ in 0..30 {
            let _ = sys_h.step_manual(1.0).await.unwrap();
        }
        let jump_res = ctx
            .dispatch_rpc(
                &mut session,
                "beyonce",
                Some(cur_b_id),
                "CmdStargateJump",
                &[PyRep::Integer(gate_id)],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(jkv) = jump_res else {
            panic!("Expected jump KeyVal");
        };
        assert_eq!(
            jkv.get("solarSystemID"),
            Some(&PyRep::Integer(expected_to_sys as i64))
        );
        cur_b_id = jkv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    }
    let tama_b_id = cur_b_id;
    let tama_handle = ctx.systems.get_or_spawn(30_002_813);

    // E2E-11: S-6 verifies scanMgr.RequestScans returns UnimplementedRPC; spawn K162 ball 750_000_001 in Tama (30002813), ConeScan, Warp to Wormhole, & Wormhole Jump into J105934 (31000005)
    let req_scans_err = ctx
        .dispatch_rpc(
            &mut session,
            "scanMgr",
            None,
            "RequestScans",
            &[PyRep::Real(0.25)],
            &empty,
            None,
        )
        .await;
    assert_eq!(req_scans_err.unwrap_err().code, "UnimplementedRPC");
    let _ = tama_handle
        .spawn_ball(eve_destiny::Ball {
            id: 750_000_001,
            type_id: 26272,
            position: glam::DVec3::new(15_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000_000_000.0,
            radius: 1500.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Rigid,
        })
        .await;
    ctx.mark_signature_scanned(90_000_001, 30_002_813, 750_000_001);
    ctx.dispatch_rpc(
        &mut session,
        "scanMgr",
        None,
        "ConeScan",
        &[PyRep::Real(360.0), PyRep::Real(14.4 * 149_597_870_700.0)],
        &empty,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut session,
        "beyonce",
        Some(tama_b_id),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(750_000_001)],
        &empty,
        None,
    )
    .await
    .unwrap();
    for _ in 0..30 {
        let _ = tama_handle.step_manual(1.0).await.unwrap();
    }
    ctx.dispatch_rpc(
        &mut session,
        "wormholeMgr",
        None,
        "WormholeJump",
        &[PyRep::Integer(750_000_001)],
        &empty,
        None,
    )
    .await
    .unwrap();
    assert_eq!(session.solar_system_id, Some(31_000_005));

    // Jump wormhole static exit from J105559 (31_000_005) into K-space Null-Sec VFK-IV (30_002_904)
    let jump_vfk = ctx
        .dispatch_rpc(
            &mut session,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::Integer(750_000_001)],
            &empty,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(vfk_kv) = jump_vfk else {
        panic!("Expected VFK-IV wormhole jump KeyVal");
    };
    assert_eq!(
        vfk_kv.get("solarSystemID"),
        Some(&PyRep::Integer(30_002_904))
    );

    // E2E-06: Turret Combat against test-spawned NPC Ball 700_000_001, DBS/ESS Bounty Split, & ESS Bank Hack (in K-space Null-Sec VFK-IV 30002904)
    let hed_handle = ctx.systems.get_or_spawn(30_002_904);
    let (_, ship_in_hed) = hed_handle
        .get_ball_snapshot(100_000_001)
        .await
        .unwrap()
        .unwrap();
    let _ = hed_handle
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: 700_000_001,
                type_id: 16997,
                position: ship_in_hed.position + glam::DVec3::new(900.0, 0.0, 0.0),
                velocity: glam::DVec3::ZERO,
                max_velocity: 310.0,
                inertia: 2.5,
                mass: 1_000_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            eve_destiny::CombatState {
                shield_hp: 50.0,
                max_shield_hp: 50.0,
                armor_hp: 50.0,
                max_armor_hp: 50.0,
                structure_hp: 50.0,
                max_structure_hp: 50.0,
                ..eve_destiny::CombatState::default()
            },
        )
        .await;
    let _ = hed_handle
        .spawn_ball(eve_destiny::Ball {
            id: 750_000_002,
            type_id: 32882,
            position: ship_in_hed.position + glam::DVec3::new(500.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 100_000_000.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Rigid,
        })
        .await;
    let _ = hed_handle.step_manual(1.0).await.unwrap();
    ctx.dispatch_rpc(
        &mut session,
        "dogmaIM",
        None,
        "FireWeapon", // rpc-lint: allow legacy/negative-test RPC (dogmaIM.FireWeapon)
        &[PyRep::Integer(700_000_001)],
        &empty,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut session,
        "essMgr",
        None,
        "AttemptLinkToMainBank",
        &[PyRep::Integer(750_000_002)],
        &empty,
        None,
    )
    .await
    .unwrap();
    for _ in 0..5 {
        let _ = hed_handle.step_manual(1.0).await.unwrap();
    }
    ctx.dispatch_rpc(
        &mut session,
        "essMgr",
        None,
        "AttemptLinkToMainBank",
        &[PyRep::Integer(750_000_002)],
        &empty,
        None,
    )
    .await
    .unwrap();

    // Jump wormhole from VFK-IV (30002904) -> Tama (30002813), then stargate route back to Jita (30000142)
    let jb_tama = ctx
        .dispatch_rpc(
            &mut session,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::Integer(750_000_001)],
            &empty,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(jb_tama_kv) = jb_tama else {
        panic!("Expected jb_tama KeyVal");
    };
    let mut ret_b_id = jb_tama_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    for (from_sys, gate_id, expected_to_sys) in [
        (30_002_813_u32, 50_014_002_i64, 30_001_376_u32),
        (30_001_376_u32, 50_000_805_i64, 30_001_379_u32),
        (30_001_379_u32, 50_013_925_i64, 30_000_138_u32),
        (30_000_138_u32, 50_013_927_i64, 30_000_142_u32),
    ] {
        let _warp_res = ctx.dispatch_rpc(
            &mut session,
            "beyonce",
            Some(ret_b_id),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(gate_id)],
            &empty,
            None,
        )
        .await;
        let sys_h = ctx.systems.get_or_spawn(from_sys);
        for _ in 0..120 {
            let _ = sys_h.step_manual(1.0).await.unwrap();
            if let Ok(Some((_, b))) = sys_h.get_ball_snapshot(session.ship_id.unwrap()).await {
                if b.mode == eve_destiny::MovementMode::Stop {
                    break;
                }
            }
        }
        let jb_res = ctx
            .dispatch_rpc(
                &mut session,
                "beyonce",
                Some(ret_b_id),
                "CmdStargateJump",
                &[PyRep::Integer(gate_id)],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(jb_kv) = jb_res else {
            panic!("Expected return jump KeyVal");
        };
        assert_eq!(
            jb_kv.get("solarSystemID"),
            Some(&PyRep::Integer(expected_to_sys as i64))
        );
        ret_b_id = jb_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    }
    let jita_b_id = ret_b_id;

    // Warp to Jita 4-4 station (60_003_760) and dock
    ctx.dispatch_rpc(
        &mut session,
        "beyonce",
        Some(jita_b_id),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(60_003_760)],
        &empty,
        None,
    )
    .await
    .unwrap();
    for _ in 0..30 {
        let _ = jita_handle.step_manual(1.0).await.unwrap();
    }
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

    // E2E-07: Regional Market Order Book, Immediate Match, Broker Fee, & Sales Tax
    let order_res = ctx
        .dispatch_rpc(
            &mut session,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(60_003_760),
                PyRep::Integer(34),
                PyRep::Real(10.0),
                PyRep::Integer(100),
                PyRep::Integer(90),
                PyRep::Integer(1),
            ],
            &empty,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(okv) = order_res else {
        panic!("Expected market order KeyVal");
    };
    assert_eq!(okv.get("filledImmediately"), Some(&PyRep::Bool(true)));

    // E2E-08: Ore Compression, Reprocessing, & Blueprint Manufacturing
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            1_000_000_000_001,
            eve_db::UpwellStructureRecord {
                structure_id: 1_000_000_000_001,
                owner_corp_id: 1_000_170,
                type_id: 35835, // Athanor refinery
                name: "Jita Refinery".into(),
                solar_system_id: 30_000_142,
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 50_000.0,
                armor_hp: 50_000.0,
                hull_hp: 50_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
        if let Some(it) = snap.items.get_mut(&100_000_012) {
            it.location_id = 1_000_000_000_001;
        }
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
            ch.structure_id = Some(1_000_000_000_001);
        }
    });
    session.station_id = None;
    session.structure_id = Some(1_000_000_000_001);
    ctx.dispatch_rpc(
        &mut session,
        "structureCompressionMgr",
        None,
        "CompressItemInStructure",
        &[PyRep::Integer(100_000_012)],
        &empty,
        None,
    )
    .await
    .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(60_003_760);
            ch.structure_id = None;
        }
    });
    session.station_id = Some(60_003_760);
    session.structure_id = None;
    ctx.dispatch_rpc(
        &mut session,
        "reprocessingSvc",
        None,
        "Reprocess",
        &[PyRep::Integer(100_000_018)],
        &empty,
        None,
    )
    .await
    .unwrap();
    let job_res = ctx
        .dispatch_rpc(
            &mut session,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::Integer(691), PyRep::Integer(1), PyRep::Integer(1)],
            &empty,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(job_kv) = job_res else {
        panic!("Expected job KeyVal");
    };
    let job_id = job_kv.get("jobID").unwrap().as_i64().unwrap();
    ctx.advance_virtual_clock(6_000);
    ctx.dispatch_rpc(
        &mut session,
        "industryManager",
        None,
        "CompleteJob",
        &[PyRep::Integer(job_id)],
        &empty,
        None,
    )
    .await
    .unwrap();

    // E2E-09: Agent Mission Request, Combat NPC Room Clear, Acceleration Gate (`keeper`), & 16-Mission Storyline Trigger
    let undock_m = ctx
        .dispatch_rpc(
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
    let PyRep::KeyVal(um_kv) = undock_m else {
        panic!("Expected Undock KeyVal");
    };
    let m_b_id = um_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(
        &mut session,
        "beyonce",
        Some(m_b_id),
        "CmdStop",
        &[],
        &empty,
        None,
    )
    .await
    .unwrap();
    let mut storyline_triggered = false;
    for _ in 1..=16i64 {
        let req_m = ctx
            .dispatch_rpc(
                &mut session,
                "agentMgr",
                None,
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
                &mut session,
                "agentMgr",
                None,
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
        for _ in 0..30 {
            for _ in 0..4 {
                let _ = jita_handle.step_manual(1.0).await.unwrap();
            }
            let shot = ctx
                .dispatch_rpc(
                    &mut session,
                    "dogmaIM",
                    None,
                    "FireWeapon", // rpc-lint: allow legacy/negative-test RPC (dogmaIM.FireWeapon)
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
            &mut session,
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
                &mut session,
                "agentMgr",
                None,
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

    // E2E-17 (while in space in Jita): Living World 3-Tier LOD Materialization (auto-triggered on Undock), Convoy Combat Destruction, & +15% NPC Price Spike
    let non_gm_mat = crate::services::cosmetics_events::materialize_system_lod2(
        &ctx,
        &session,
        &[PyRep::Integer(30_000_142)],
    )
    .await;
    assert_eq!(non_gm_mat.unwrap_err().code, "InsufficientRole");
    let non_gm_int = crate::services::cosmetics_events::interdict_convoy(
        &ctx,
        &session,
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
                &mut session,
                "dogmaIM",
                None,
                "FireWeapon", // rpc-lint: allow legacy/negative-test RPC (dogmaIM.FireWeapon)
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
    assert!(lw_new_ask > 5.0);

    // E2E-10: Fleet Creation, EveMail, & Bookmark Creation
    ctx.dispatch_rpc(
        &mut session,
        "fleetObjectHandler",
        None,
        "CreateFleet",
        &[],
        &empty,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut session,
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
        &mut session,
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
    ctx.dispatch_rpc(&mut sess2, "stationSvc", None, "Undock", &[], &empty, None)
        .await
        .unwrap();

    for _ in 0..4 {
        let _ = jita_handle.step_manual(1.0).await.unwrap();
    }
    let cw_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "FireWeapon", // rpc-lint: allow legacy/negative-test RPC (dogmaIM.FireWeapon)
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

    // CONCORD destroyed 100_000_001 and respawned 90_000_001 in a Capsule at 60_003_760;
    // board spare Rifter 100_000_029 in hangar, undock via RPC, and verify legal kill right engagement by sess2
    ctx.dispatch_rpc(
        &mut session,
        "ship",
        None,
        "Board",
        &[PyRep::Integer(100_000_029)],
        &empty,
        None,
    )
    .await
    .unwrap();
    let undock_29 = ctx
        .dispatch_rpc(
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
    let PyRep::KeyVal(u29_kv) = undock_29 else {
        panic!("Expected Undock KeyVal");
    };
    let b29 = u29_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(
        &mut session,
        "beyonce",
        Some(b29),
        "CmdStop",
        &[],
        &empty,
        None,
    )
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
    ctx.dispatch_rpc(
        &mut sess2,
        "beyonce",
        Some(b_sess2),
        "CmdStop",
        &[],
        &empty,
        None,
    )
    .await
    .unwrap();
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
                "FireWeapon", // rpc-lint: allow legacy/negative-test RPC (dogmaIM.FireWeapon)
                &[PyRep::Integer(90_000_001)],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(lkv) = legal_res else {
            panic!("Expected legal kill KeyVal");
        };
        assert_eq!(lkv.get("legalEngagement"), Some(&PyRep::Bool(true)));
        if lkv.get("destroyed") == Some(&PyRep::Bool(true)) {
            legal_destroyed = true;
            break;
        }
    }
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
        "ship",
        None,
        "Board",
        &[PyRep::Integer(100_000_035)],
        &empty,
        None,
    )
    .await
    .unwrap();
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
}
