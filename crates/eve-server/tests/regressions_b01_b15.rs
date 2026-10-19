//! Integration regression tests covering blocking findings B-01 through B-15.

#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_blocking_findings_b01_to_b15_regressions() {
    use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};

    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let mut sess = SessionState::new(1);
    let kw = HashMap::new();

    // B-07 & AP-05: Production WorldRepository::with_demo_seed() has 0 GM accounts, only starter items 100_000_001..=100_000_012, and 0 items in 100_000_013..=100_000_028
    for starter_id in 100_000_001..=100_000_012 {
        assert!(
            ctx.db.get_item(starter_id).is_some(),
            "Expected starter item {starter_id} in with_demo_seed()"
        );
    }
    for test_only_item_id in 100_000_013..=100_000_028 {
        assert!(
            ctx.db.get_item(test_only_item_id).is_none(),
            "Production seed must not contain test-only fixture item {test_only_item_id}"
        );
    }

    // Seed expansion test fixtures explicitly inside the test harness
    ctx.db.with_mut_snapshot(|snap| {
        let extra_items = [
            (
                100_000_015,
                35832,
                100_000_001,
                InventoryFlag::Cargo,
                5,
                false,
            ),
            (
                100_000_016,
                56201,
                100_000_001,
                InventoryFlag::Cargo,
                5,
                false,
            ),
            (
                100_000_020,
                20424,
                60_003_760,
                InventoryFlag::Hangar,
                10,
                false,
            ),
            (
                100_000_028,
                20172,
                60_003_760,
                InventoryFlag::Hangar,
                10,
                false,
            ),
            (
                100_000_023,
                23055,
                100_000_024,
                InventoryFlag::FighterBay,
                9,
                false,
            ),
            (
                100_000_024,
                24483,
                60_003_760,
                InventoryFlag::Hangar,
                1,
                true,
            ),
            (
                100_000_025,
                4247,
                100_000_024,
                InventoryFlag::Cargo,
                500,
                false,
            ),
            (
                100_000_026,
                4247,
                100_000_001,
                InventoryFlag::Cargo,
                500,
                false,
            ),
            (
                100_000_027,
                34748,
                60_003_760,
                InventoryFlag::Hangar,
                1,
                false,
            ),
            (
                100_000_036,
                35840,
                100_000_001,
                InventoryFlag::Cargo,
                2,
                false,
            ),
        ];
        for (item_id, type_id, location_id, flag, quantity, singleton) in extra_items {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id: 90_000_001,
                    location_id,
                    flag,
                    quantity,
                    singleton,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
        let char_skills = snap.skills.entry(90_000_001).or_default();
        for (skill_id, level, sp) in [
            (3388, 4, 45_255.0),
            (11448, 4, 226_275.0),
            (11452, 4, 226_275.0),
            (21791, 4, 226_275.0),
            (24242, 4, 45_255.0),
            (33407, 4, 90_510.0),
            (21611, 2, 12_699.0),
        ] {
            char_skills.insert(
                skill_id,
                TrainedSkillRecord {
                    type_id: skill_id,
                    level,
                    skill_points: sp,
                },
            );
        }
    });

    let gm_fail = ctx
        .dispatch_rpc(
            &mut sess,
            "authentication",
            None,
            "Login",
            &[
                PyRep::String("admin".into()),
                PyRep::String("admin-secret".into()),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(gm_fail.unwrap_err().code, "AuthenticationFailed");

    // Login as demo capsuleer (90_000_001)
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login demo");
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

    // R-P1-06: air_npe.AdvanceNode does not mutate career_goal_progress and gates node 3 (DestroySeekers) on actual NPC kill
    for _ in 0..2 {
        ctx.dispatch_rpc(&mut sess, "air_npe", None, "AdvanceNode", &[], &kw, None)
            .await
            .expect("AdvanceNode to node 3");
    }
    let npe_gate_err = ctx
        .dispatch_rpc(&mut sess, "air_npe", None, "AdvanceNode", &[], &kw, None)
        .await;
    assert_eq!(npe_gate_err.unwrap_err().code, "NodeObjectiveNotMet");

    // ApplySkinToShip rejects unlicensed SKIN before ActivateSkinLicense, and ActivateSkinLicense consumes item 100_000_027
    let unlicensed = ctx
        .dispatch_rpc(
            &mut sess,
            "shipCosmeticsMgr",
            None,
            "ApplySkinToShip",
            &[PyRep::Integer(587), PyRep::Integer(34748)],
            &kw,
            None,
        )
        .await;
    assert_eq!(unlicensed.unwrap_err().code, "SkinNotLicensed");

    ctx.dispatch_rpc(
        &mut sess,
        "shipCosmeticsMgr",
        None,
        "ActivateSkinLicense",
        &[PyRep::Integer(34748)],
        &kw,
        None,
    )
    .await
    .expect("ActivateSkinLicense");
    let no_second_license = ctx
        .dispatch_rpc(
            &mut sess,
            "shipCosmeticsMgr",
            None,
            "ActivateSkinLicense",
            &[PyRep::Integer(34748)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        no_second_license.unwrap_err().code,
        "SkinLicenseItemNotFound"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "shipCosmeticsMgr",
        None,
        "ApplySkinToShip",
        &[PyRep::Integer(587), PyRep::Integer(34748)],
        &kw,
        None,
    )
    .await
    .expect("ApplySkinToShip after license activation");

    // B-04: Installing T2 Invention (activity 8) before copying any BPC from BPO 691 must fail with InventionRequiresBPC
    let bpo_inv_err = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::Integer(691), PyRep::Integer(8), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await;
    assert_eq!(bpo_inv_err.unwrap_err().code, "InventionRequiresBPC");

    // Copy Rifter BPO (691) twice (producing 2 BPCs with is_blueprint_copy=true), then test deterministic invention failure and success via server-side override
    let copy1 = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::Integer(691), PyRep::Integer(5), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("Copy BPO 1");
    let PyRep::KeyVal(copy1_kv) = copy1 else {
        panic!("Expected copy1 KeyVal");
    };
    let copy1_id = copy1_kv.get("jobID").unwrap().as_i64().unwrap() as u64;
    ctx.advance_virtual_clock(10_000);
    ctx.dispatch_rpc(
        &mut sess,
        "industryManager",
        None,
        "CompleteJob",
        &[PyRep::Integer(copy1_id as i64)],
        &kw,
        None,
    )
    .await
    .expect("Complete Copy 1");

    let copy2 = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::Integer(691), PyRep::Integer(5), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("Copy BPO 2");
    let PyRep::KeyVal(copy2_kv) = copy2 else {
        panic!("Expected copy2 KeyVal");
    };
    let copy2_id = copy2_kv.get("jobID").unwrap().as_i64().unwrap() as u64;
    ctx.advance_virtual_clock(10_000);
    ctx.dispatch_rpc(
        &mut sess,
        "industryManager",
        None,
        "CompleteJob",
        &[PyRep::Integer(copy2_id as i64)],
        &kw,
        None,
    )
    .await
    .expect("Complete Copy 2");

    // Install invention 1 and test failure roll (server-side override = 0.95 >= 0.4100)
    let inv1 = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::Integer(691), PyRep::Integer(8), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("Install invention 1");
    let PyRep::KeyVal(inv1_kv) = inv1 else {
        panic!("Expected inv1 KeyVal");
    };
    let inv1_job_id = inv1_kv.get("jobID").unwrap().as_i64().unwrap() as u64;
    ctx.advance_virtual_clock(70_000);
    ctx.set_invention_roll_override(Some(0.95));
    let inv1_fail = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "CompleteJob",
            &[PyRep::Integer(inv1_job_id as i64)],
            &kw,
            None,
        )
        .await;
    assert_eq!(inv1_fail.unwrap_err().code, "InventionAttemptFailed");

    // Install invention 2 and test success roll (server-side override = 0.10 < 0.4100)
    let inv2 = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::Integer(691), PyRep::Integer(8), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("Install invention 2");
    let PyRep::KeyVal(inv2_kv) = inv2 else {
        panic!("Expected inv2 KeyVal");
    };
    let inv2_job_id = inv2_kv.get("jobID").unwrap().as_i64().unwrap() as u64;
    ctx.advance_virtual_clock(70_000);
    ctx.set_invention_roll_override(Some(0.10));
    let inv2_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "CompleteJob",
            &[PyRep::Integer(inv2_job_id as i64)],
            &kw,
            None,
        )
        .await
        .expect("Complete invention 2");
    ctx.set_invention_roll_override(None);
    let PyRep::KeyVal(inv2_ok_kv) = inv2_ok else {
        panic!("Expected inv2_ok KeyVal");
    };
    assert_eq!(
        inv2_ok_kv.get("productTypeID").and_then(|v| v.as_i64()),
        Some(11372)
    );

    // planetMgr.UserUpdateNetwork + planetMgr.Flush materializes 200x Water (type_id 3645) into station hangar
    let ship_id = sess.ship_id.unwrap_or(100_000_001);
    let prev_st = sess.station_id;
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
        }
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            eve_db::InventoryItemRecord {
                item_id: id,
                type_id: 2254,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.dispatch_rpc(
        &mut sess,
        "planetMgr",
        None,
        "UserUpdateNetwork",
        &[
            PyRep::Integer(40009082),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Integer(2254),
                PyRep::Real(0.1),
                PyRep::Real(0.2),
            ])]),
        ],
        &kw,
        None,
    )
    .await
    .expect("planetMgr.UserUpdateNetwork");
    sess.station_id = prev_st;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = prev_st;
        }
    });
    let flush = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "Flush",
            &[PyRep::Integer(40009082)],
            &kw,
            None,
        )
        .await
        .expect("planetMgr.Flush");
    let PyRep::KeyVal(flush_kv) = flush else {
        panic!("Expected flush KeyVal");
    };
    let delivered_water_id = flush_kv.get("deliveredItemID").unwrap().as_i64().unwrap() as u64;
    assert!(delivered_water_id > 0);

    async fn warp_and_jump_gate(
        ctx_ref: &ServerContext,
        sess_ref: &mut SessionState,
        kw_ref: &HashMap<String, PyRep>,
        gate_id: i64,
        _pos: [f64; 3],
    ) {
        let sys_id = sess_ref.solar_system_id.unwrap();
        let gate_pos = ctx_ref.sde.jumps[&gate_id].position;
        let pos = [gate_pos.x, gate_pos.y, gate_pos.z];
        let bound = ctx_ref
            .bind_object(sess_ref, "beyonce", sys_id as i64)
            .expect("bind beyonce");
        let handle = ctx_ref.systems.get_or_spawn(sys_id);
        let warp_res = ctx_ref
            .dispatch_rpc(
                sess_ref,
                "beyonce",
                Some(bound),
                "CmdWarpToPoint",
                &[
                    PyRep::Real(pos[0]),
                    PyRep::Real(pos[1]),
                    PyRep::Real(pos[2]),
                ],
                kw_ref,
                None,
            )
            .await;
        match warp_res {
            Ok(_) => {
                for _ in 0..40 {
                    let _ = handle.step_manual(1.0).await.unwrap();
                }
            }
            Err(e) if e.code == "WarpDistanceTooShort" => {
                let ship_id = sess_ref.ship_id.unwrap();
                let (_, ball) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
                let dir = [
                    pos[0] - ball.position.x,
                    pos[1] - ball.position.y,
                    pos[2] - ball.position.z,
                ];
                ctx_ref
                    .dispatch_rpc(
                        sess_ref,
                        "beyonce",
                        Some(bound),
                        "CmdGotoDirection",
                        &[
                            PyRep::Real(dir[0]),
                            PyRep::Real(dir[1]),
                            PyRep::Real(dir[2]),
                        ],
                        kw_ref,
                        None,
                    )
                    .await
                    .expect("CmdGotoDirection to nearby stargate");
                for _ in 0..9 {
                    let _ = handle.step_manual(1.0).await.unwrap();
                }
                ctx_ref
                    .dispatch_rpc(
                        sess_ref,
                        "beyonce",
                        Some(bound),
                        "CmdStop",
                        &[],
                        kw_ref,
                        None,
                    )
                    .await
                    .expect("CmdStop at stargate");
            }
            Err(e) => panic!("CmdWarpToPoint to stargate: {e:?}"),
        }
        ctx_ref
            .dispatch_rpc(
                sess_ref,
                "beyonce",
                Some(bound),
                "CmdStargateJump",
                &[PyRep::Integer(gate_id)],
                kw_ref,
                None,
            )
            .await
            .expect("CmdStargateJump");
    }

    // Undock in Rifter (100_000_001), travel Jita -> Ikuchi -> Tunttaras -> Nourvukaiken -> Tama (30002813) to anchor & fuel Tama Cyno Citadel
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock Rifter");
    for gate_id in [50_013_928_i64, 50_013_926, 50_001_709, 50_014_001] {
        warp_and_jump_gate(&ctx, &mut sess, &kw, gate_id, [0.0, 0.0, 0.0]).await;
    }
    assert_eq!(sess.solar_system_id, Some(30002813));

    // R36-P1-1 & R2-P0-01: MaterializeSystemLOD2 rejects non-GMs with InsufficientRole, and rejects remote solar system arguments from GMs with CannotMaterializeRemoteSystem
    let non_gm_mat_err = ctx
        .dispatch_rpc(
            &mut sess,
            "taleMgr",
            None,
            "MaterializeSystemLOD2",
            &[PyRep::Integer(30_000_142)],
            &kw,
            None,
        )
        .await;
    assert_eq!(non_gm_mat_err.unwrap_err().code, "InsufficientRole");
    let saved_role = sess.role;
    sess.role |= eve_machonet::service_roles::ROLE_GMH;
    let remote_mat_err = ctx
        .dispatch_rpc(
            &mut sess,
            "taleMgr",
            None,
            "MaterializeSystemLOD2",
            &[PyRep::Integer(30_000_142)],
            &kw,
            None,
        )
        .await;
    sess.role = saved_role;
    assert_eq!(
        remote_mat_err.unwrap_err().code,
        "CannotMaterializeRemoteSystem"
    );

    // R2-P1-03: Anchoring an Upwell structure while in starter NPC corporation (1_000_172) is rejected with NpcCorpCannotAnchorStructure
    let npc_anchor_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDeployment",
            None,
            "Anchor",
            &[
                PyRep::Integer(35832),
                PyRep::String("Tama Cyno Citadel".into()),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        npc_anchor_err.unwrap_err().code,
        "NpcCorpCannotAnchorStructure"
    );

    // Create a player corporation on sess (90_000_001) before anchoring structures
    ctx.dispatch_rpc(
        &mut sess,
        "corpmgr",
        None,
        "CreateCorporation",
        &[
            PyRep::String("Tama Vanguard Corp".into()),
            PyRep::String("TVC".into()),
            PyRep::Real(0.05),
        ],
        &kw,
        None,
    )
    .await
    .expect("CreateCorporation TVC");

    // Anchor Upwell Astrahus in Tama (30002813), verify fuelBlocks == 0, and fuel it from Rifter cargo
    let anchor_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDeployment",
            None,
            "Anchor",
            &[
                PyRep::Integer(35832),
                PyRep::String("Tama Cyno Citadel".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("Anchor Tama Cyno Citadel");
    let PyRep::KeyVal(anchor_kv) = anchor_res else {
        panic!("Expected anchor KeyVal");
    };
    assert_eq!(
        anchor_kv.get("fuelBlocks").and_then(|v| v.as_i64()),
        Some(0)
    );
    let tama_struct_id = anchor_kv.get("structureID").unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(
        &mut sess,
        "structureControl",
        None,
        "FuelStructure",
        &[PyRep::Integer(tama_struct_id as i64), PyRep::Integer(50)],
        &kw,
        None,
    )
    .await
    .expect("FuelStructure");

    // R-P1-07 & AP-06: Anchor and fuel a dedicated Pharolux Cyno Beacon (35840) in low-sec Tama (30002813)
    let anchor_beacon_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDeployment",
            None,
            "Anchor",
            &[
                PyRep::Integer(35840),
                PyRep::String("Tama Cyno Beacon".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("Anchor Tama Cyno Beacon");
    let PyRep::KeyVal(beacon_kv) = anchor_beacon_res else {
        panic!("Expected beacon KeyVal");
    };
    let tama_beacon_id = beacon_kv.get("structureID").unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(
        &mut sess,
        "structureControl",
        None,
        "FuelStructure",
        &[PyRep::Integer(tama_beacon_id as i64), PyRep::Integer(50)],
        &kw,
        None,
    )
    .await
    .expect("FuelStructure Tama Cyno Beacon");

    // Dock at Tama Cyno Citadel, install a jump clone there, and undock
    ctx.dispatch_rpc(
        &mut sess,
        "structureDocking",
        None,
        "Dock",
        &[PyRep::Integer(tama_struct_id as i64)],
        &kw,
        None,
    )
    .await
    .expect("Dock at Tama Cyno Citadel");
    // Fit and online Standup Cloning Center I (35894) in ServiceSlot0 to provide medical clone services
    let clone_bay_item_id = 100_000_980_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            clone_bay_item_id,
            InventoryItemRecord {
                item_id: clone_bay_item_id,
                type_id: 35894,
                owner_id: 90_000_001,
                location_id: tama_struct_id as i64,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.module_online_states.insert(clone_bay_item_id, true);
    });

    // S-4: InstallCloneInStation rejects when docked in a structure; InstallCloneInStructure succeeds
    // parity: plan:S-4
    let wrong_clone_rpc = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(wrong_clone_rpc.unwrap_err().code, "MustBeDocked");
    let clone_tama = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStructure",
            &[],
            &kw,
            None,
        )
        .await
        .expect("InstallCloneInStructure at Tama Cyno Citadel");
    let PyRep::KeyVal(ct_kv) = clone_tama else {
        panic!("Expected clone_tama KeyVal");
    };
    let tama_clone_id = ct_kv.get("jumpCloneID").unwrap().as_i64().unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "structureDocking",
        None,
        "Undock",
        &[],
        &kw,
        None,
    )
    .await
    .expect("Undock from Tama Cyno Citadel");

    // Return Rifter to Jita (60003760) via stargates 50014002 -> 50000805 -> 50013925 -> 50013927
    for gate_id in [50_014_002_i64, 50_000_805, 50_013_925, 50_013_927] {
        warp_and_jump_gate(&ctx, &mut sess, &kw, gate_id, [0.0, 0.0, 0.0]).await;
    }
    let jita_bound = ctx.bind_object(&mut sess, "beyonce", 30_000_142).unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(jita_bound),
        "CmdWarpToPoint",
        &[
            PyRep::Real(-107_303_362_560.0),
            PyRep::Real(-18_744_975_360.0),
            PyRep::Real(436_489_052_160.0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Warp to Jita 4-4");
    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    for _ in 0..40 {
        let _ = jita_handle.step_manual(1.0).await.unwrap();
    }
    ctx.dispatch_rpc(
        &mut sess,
        "stationSvc",
        None,
        "Dock",
        &[PyRep::Integer(60003760)],
        &kw,
        None,
    )
    .await
    .expect("Dock Rifter at Jita");

    // S-9: MaterializeSystemLOD2 rejects non-GM roles (including legacy 0x02) and requires 64-bit V24.01 serviceConst GM role bitmask
    assert_eq!(
        ctx.living_world
            .get_convoy(800_000_001)
            .map(|c| c.lod_level),
        Some(2)
    );
    let non_gm_mat = ctx
        .dispatch_rpc(
            &mut sess,
            "taleMgr",
            None,
            "MaterializeSystemLOD2",
            &[PyRep::Integer(30_000_142)],
            &kw,
            None,
        )
        .await;
    assert_eq!(non_gm_mat.unwrap_err().code, "InsufficientRole");
    sess.role |= 0x02;
    let legacy_bit_mat = ctx
        .dispatch_rpc(
            &mut sess,
            "taleMgr",
            None,
            "MaterializeSystemLOD2",
            &[PyRep::Integer(30_000_142)],
            &kw,
            None,
        )
        .await;
    assert_eq!(legacy_bit_mat.unwrap_err().code, "InsufficientRole");
    sess.role =
        eve_machonet::service_roles::ROLE_PLAYER_DEFAULT | eve_machonet::service_roles::ROLE_GML;
    let mat_second = ctx
        .dispatch_rpc(
            &mut sess,
            "taleMgr",
            None,
            "MaterializeSystemLOD2",
            &[PyRep::Integer(30_000_142)],
            &kw,
            None,
        )
        .await
        .expect("GM MaterializeSystemLOD2");
    assert_eq!(mat_second.as_i64(), Some(0));
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;

    // N-P1-5: home_station.set_home_station rejects remote station (60008494 while docked at 60003760) with MustBeDockedAtTargetStation
    let remote_home_err = ctx
        .dispatch_rpc(
            &mut sess,
            "home_station",
            None,
            "set_home_station",
            &[PyRep::Integer(60_008_494)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        remote_home_err.unwrap_err().code,
        "MustBeDockedAtTargetStation"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "home_station",
        None,
        "set_home_station",
        &[PyRep::Integer(60_003_760)],
        &kw,
        None,
    )
    .await
    .expect("set_home_station while docked at 60003760");

    // Test cross-location CloneJump on sess (Jita 60003760 -> Tama Cyno Citadel -> back to Jita 60003760)
    ctx.dispatch_rpc(
        &mut sess,
        "jumpCloneSvc",
        None,
        "CloneJump",
        &[PyRep::Integer(tama_clone_id)],
        &kw,
        None,
    )
    .await
    .expect("CloneJump to Tama Cyno Citadel");
    assert_eq!(sess.structure_id, Some(tama_struct_id as i64));
    assert_eq!(sess.solar_system_id, Some(30002813));
    assert_ne!(sess.ship_id, Some(100_000_001));

    ctx.advance_virtual_clock(90_000);
    ctx.dispatch_rpc(
        &mut sess,
        "jumpCloneSvc",
        None,
        "CloneJump",
        &[PyRep::Integer(tama_clone_id)],
        &kw,
        None,
    )
    .await
    .expect("CloneJump back to Jita");
    assert_eq!(sess.station_id, Some(60003760));
    assert_eq!(sess.solar_system_id, Some(30000142));

    // Board Archon Carrier (100_000_024, type_id = 24483), undock, and verify S-2 & S-3 return UnimplementedRPC
    ctx.dispatch_rpc(
        &mut sess,
        "ship",
        None,
        "Board",
        &[PyRep::Integer(100_000_024)],
        &kw,
        None,
    )
    .await
    .expect("Board Archon Carrier");
    assert_eq!(sess.ship_id, Some(100_000_024));

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock Archon");
    ctx.dispatch_rpc(
        &mut sess,
        "fighterMgr",
        None,
        "GetFightersForShip",
        &[],
        &kw,
        None,
    )
    .await
    .expect("GetFightersForShip from Archon");
    // parity: plan:S-2
    let ftr_err = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[PyRep::Integer(700_000_077), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await;
    assert!(matches!(
        ftr_err.unwrap_err().code.as_str(),
        "UnimplementedRPC" | "FighterNotFound"
    ));

    // parity: plan:S-3
    let cyno_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "CmdJumpToStructureBeacon",
            &[PyRep::Integer(30002813)],
            &kw,
            None,
        )
        .await;
    assert!(matches!(
        cyno_err.unwrap_err().code.as_str(),
        "UnimplementedRPC" | "ShipDocked" | "BeaconNotFound"
    ));

    // Move Archon Carrier (100_000_024) into low-sec Tama (30002813) for subsequent tests
    let _ = jita_handle.remove_ball(100_000_024).await;
    ctx.db
        .set_character_solar_system(90_000_001, 30_002_813, &ctx.sde)
        .expect("Move Archon to Tama");
    sess.solar_system_id = Some(30_002_813);

    // Spawn second player (90_000_002 on account "other"), jump to Tama, create hostile corp, and siege structure across all 3 phases
    let mut sess2 = SessionState::new(2);
    ctx.dispatch_rpc(
        &mut sess2,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("other".into()),
            PyRep::String("other-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login sess2");
    ctx.dispatch_rpc(
        &mut sess2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID 90_000_002");

    // Verify untrained character cannot install jump clone without Infomorph Psychology
    let no_infomorph = ctx
        .dispatch_rpc(
            &mut sess2,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[PyRep::Integer(60008494)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        no_infomorph.unwrap_err().code,
        "InfomorphPsychologyRequired"
    );

    ctx.dispatch_rpc(&mut sess2, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock 90_000_002");
    // Travel via stargates Jita -> Ikuchi -> Tunttaras -> Nourvukaiken -> Tama (30002813)
    for gate_id in [50_013_928_i64, 50_013_926, 50_001_709, 50_014_001] {
        warp_and_jump_gate(&ctx, &mut sess2, &kw, gate_id, [0.0, 0.0, 0.0]).await;
    }
    assert_eq!(sess2.solar_system_id, Some(30002813));
    let corp_hsc = ctx
        .dispatch_rpc(
            &mut sess2,
            "corpmgr",
            None,
            "CreateCorporation",
            &[
                PyRep::String("Hostile Siege Corp".into()),
                PyRep::String("HSC".into()),
                PyRep::Real(0.05),
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateCorporation HSC");
    let PyRep::KeyVal(hsc_kv) = corp_hsc else {
        panic!("Expected hsc KeyVal");
    };
    // B-06: CreateCorporation does NOT auto-create an alliance; allianceRegistry.CreateAlliance creates it explicitly
    assert_eq!(hsc_kv.get("allianceID"), Some(&PyRep::None));
    let al_res = ctx
        .dispatch_rpc(
            &mut sess2,
            "allianceRegistry",
            None,
            "CreateAlliance",
            &[
                PyRep::String("Hostile Siege Alliance".into()),
                PyRep::String("HSA".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateAlliance HSA");
    let PyRep::KeyVal(al_kv) = al_res else {
        panic!("Expected al_kv KeyVal");
    };
    assert!(
        al_kv
            .get("allianceID")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            > 99_000_000
    );

    let tama_handle = ctx.systems.get_or_spawn(30_002_813);
    for _ in 0..4 {
        let _ = tama_handle.step_manual(1.0).await.unwrap();
    }

    // R8-P1-3, R9-P1-2 & R18-P1-1: Seed Republic Fleet Phased Plasma S (21924: 2.3 kinetic + 11.5 thermal = 13.8 base damage) in Rifter 100_000_002 Cargo,
    // verify AttackStructure rejects with NoAmmoLoaded while ammo is only in Cargo, load into HiSlot0 (flag 27) via dogmaIM.LoadAmmo,
    // and verify a single frigate weapon cycle (200mm AutoCannon II 2889: 13.8 * 3.465 = 47.817 volley / 3.125s = 15.30144 DPS) deals strictly one volley and consumes 1 round
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_000_940,
            eve_db::InventoryItemRecord {
                item_id: 100_000_940,
                type_id: 21924,
                owner_id: 90_000_002,
                location_id: 100_000_002,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 2_000,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let unl_frigate_poke = ctx
        .dispatch_rpc(
            &mut sess2,
            "structureControl",
            None,
            "AttackStructure",
            &[PyRep::Integer(tama_struct_id as i64)],
            &kw,
            None,
        )
        .await;
    assert_eq!(unl_frigate_poke.unwrap_err().code, "NoAmmoLoaded");
    ctx.dispatch_rpc(
        &mut sess2,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(100_000_002),
            PyRep::Integer(27),
            PyRep::Integer(100_000_940),
        ],
        &kw,
        None,
    )
    .await
    .expect("LoadAmmo 21924 into Rifter 100_000_002 HiSlot0");
    let frigate_poke = ctx
        .dispatch_rpc(
            &mut sess2,
            "structureControl",
            None,
            "AttackStructure",
            &[PyRep::Integer(tama_struct_id as i64)],
            &kw,
            None,
        )
        .await
        .expect("AttackStructure single frigate cycle");
    let PyRep::KeyVal(poke_kv) = frigate_poke else {
        panic!("Expected poke_kv KeyVal");
    };
    assert_eq!(
        poke_kv.get("state").and_then(|v| v.as_str()),
        Some("ShieldVulnerable")
    );
    let poke_shield_hp = poke_kv.get("shieldHP").and_then(|v| v.as_f64()).unwrap();
    assert!((poke_shield_hp - 3_599_952.183).abs() < 1e-4);
    assert_eq!(ctx.db.get_item(100_000_940).unwrap().quantity, 1_880);
    let loaded_after_poke = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .find(|i| {
                i.location_id == 100_000_002
                    && i.flag == eve_db::InventoryFlag::HiSlot0
                    && i.type_id == 21924
            })
            .map(|i| i.quantity)
            .unwrap_or(0)
    });
    assert_eq!(loaded_after_poke, 119);

    // R12-P1-1 / R13-P0-1 / R16-P1-1 / R17-P1-1 / R18-P1-1: Dock at tama_struct_id, board a Raven battleship (type_id 638: 11,000 MW PG, 750 tf CPU, 7 high slots, 6 launcher hardpoints),
    // fit 6x Torpedo Launcher II (type_id 2420: 1,375 MW PG, 66 tf CPU, 10.5s cycle, 15.0 m3 capacity) loaded with Inferno Torpedo (type_id 2510: 0.05 m3 -> 300 torpedoes per launcher, 450.0 thermal damage each -> 2,700.0 volley / 257.14 DPS total)
    // via invbroker.MoveItem & dogmaIM.LoadAmmo into HiSlot0..=HiSlot5 (flags 27..=32), undock, and cycle 1,334 volleys (1,334 * 2,700 = 3,601,800 >= 3,599,952.183) to strip the remaining shield HP
    ctx.dispatch_rpc(
        &mut sess2,
        "structureDocking",
        None,
        "Dock",
        &[PyRep::Integer(tama_struct_id as i64)],
        &kw,
        None,
    )
    .await
    .expect("Dock sess2 at tama_struct_id to refit");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_000_950,
            eve_db::InventoryItemRecord {
                item_id: 100_000_950,
                type_id: 638, // Raven battleship
                owner_id: 90_000_002,
                location_id: tama_struct_id as i64,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        // Seed 1 Heavy Entropic Disintegrator II (47918) to verify Raven (638) rejects Triglavian cruiser weapons,
        // plus 6 canonical Torpedo Launcher II (2420) for 90_000_002
        snap.items.insert(
            100_000_959,
            eve_db::InventoryItemRecord {
                item_id: 100_000_959,
                type_id: 47918, // Heavy Entropic Disintegrator II (restricted to Cruiser/HAC)
                owner_id: 90_000_002,
                location_id: tama_struct_id as i64,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.clone_grade = 2; // Omega clone so Torpedo Specialization IV can be queued and trained
            ch2.free_skill_points = 1_682_039.0; // 256,000 (3319 V) + 16,000 (3321 III) + 24,000 (3324 III) + 1,024,000 (3325 V) + 362,039 (20212 IV)
        }
        for idx in 0..6_i64 {
            let mod_id = 100_000_951 + idx;
            snap.items.insert(
                mod_id,
                eve_db::InventoryItemRecord {
                    item_id: mod_id,
                    type_id: 2420, // Torpedo Launcher II
                    owner_id: 90_000_002,
                    location_id: tama_struct_id as i64,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
    });
    let skill_items = vec![
        PyRep::Tuple(vec![PyRep::Integer(3319), PyRep::Integer(5)]),
        PyRep::Tuple(vec![PyRep::Integer(3321), PyRep::Integer(3)]),
        PyRep::Tuple(vec![PyRep::Integer(3324), PyRep::Integer(3)]),
        PyRep::Tuple(vec![PyRep::Integer(3325), PyRep::Integer(5)]),
        PyRep::Tuple(vec![PyRep::Integer(20213), PyRep::Integer(4)]),
    ];
    ctx.dispatch_rpc(
        &mut sess2,
        "skillMgr2",
        None,
        "SaveSkillQueue",
        &[PyRep::List(skill_items)],
        &kw,
        None,
    )
    .await
    .expect("Queue missile skill for Raven");
    ctx.dispatch_rpc(
        &mut sess2,
        "skillMgr2",
        None,
        "ApplyFreeSkillPoints",
        &[],
        &kw,
        None,
    )
    .await
    .expect("ApplyFreeSkillPoints to complete missile skill queue for Raven");
    ctx.db.with_snapshot(|snap| {
        let skills = snap.skills.get(&90_000_002).expect("90_000_002 skills");
        assert_eq!(skills.get(&3319).map(|s| s.level), Some(5));
        assert_eq!(skills.get(&3325).map(|s| s.level), Some(5));
        assert_eq!(skills.get(&20213).map(|s| s.level), Some(4));
        assert_eq!(snap.skill_queues.get(&90_000_002).map(|q| q.len()), Some(0));
    });
    ctx.dispatch_rpc(
        &mut sess2,
        "ship",
        None,
        "Board",
        &[PyRep::Integer(100_000_950)],
        &kw,
        None,
    )
    .await
    .expect("Board Raven 100_000_950 at tama_struct_id");
    // Verify Heavy Entropic Disintegrator II (47936) is rejected on Raven (638)
    let bad_trig_fit = ctx
        .dispatch_rpc(
            &mut sess2,
            "invbroker",
            None,
            "MoveItem",
            &[
                PyRep::Integer(100_000_959),
                PyRep::Integer(100_000_950),
                PyRep::Integer(27),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(bad_trig_fit.unwrap_err().code, "FittingValidationFailed");
    for idx in 0..6_i64 {
        let mod_id = 100_000_951 + idx;
        let flag_id = 27 + idx;
        ctx.dispatch_rpc(
            &mut sess2,
            "invbroker",
            None,
            "MoveItem",
            &[
                PyRep::Integer(mod_id),
                PyRep::Integer(100_000_950),
                PyRep::Integer(flag_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Fit Torpedo Launcher II via MoveItem");
    }
    let undock_res = ctx
        .dispatch_rpc(
            &mut sess2,
            "structureDocking",
            None,
            "Undock",
            &[],
            &kw,
            None,
        )
        .await
        .expect("Undock Raven from tama_struct_id");
    let PyRep::KeyVal(u_kv) = undock_res else {
        panic!("Expected undock KeyVal");
    };
    let raven_b_id = u_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(
        &mut sess2,
        "beyonce",
        Some(raven_b_id),
        "CmdStop",
        &[],
        &kw,
        None,
    )
    .await
    .expect("Stop Raven on grid at tama_struct_id");

    // R17-P1-1: Verify unloaded Torpedo Launcher II (2420) cannot fire without torpedo charges
    for _ in 0..15 {
        let _ = tama_handle.step_manual(1.0).await.unwrap();
    }
    let unloaded_atk = ctx
        .dispatch_rpc(
            &mut sess2,
            "structureControl",
            None,
            "AttackStructure",
            &[PyRep::Integer(tama_struct_id as i64)],
            &kw,
            None,
        )
        .await;
    assert_eq!(unloaded_atk.unwrap_err().code, "NoAmmoLoaded");

    // Seed canonical Inferno Torpedo charges (type_id 2510: group 89, 0.05 m3, 450.0 thermal damage) in Raven 100_000_950 Cargo
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_000_960,
            eve_db::InventoryItemRecord {
                item_id: 100_000_960,
                type_id: 2510, // Inferno Torpedo
                owner_id: 90_000_002,
                location_id: 100_000_950,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 20_000,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    // R18-P1-1: Having Inferno Torpedoes in Cargo alone is NOT sufficient; AttackStructure must reject with NoAmmoLoaded until charges are loaded into weapon slots
    let cargo_only_atk = ctx
        .dispatch_rpc(
            &mut sess2,
            "structureControl",
            None,
            "AttackStructure",
            &[PyRep::Integer(tama_struct_id as i64)],
            &kw,
            None,
        )
        .await;
    assert_eq!(cargo_only_atk.unwrap_err().code, "NoAmmoLoaded");

    let raven_slots = PyRep::List((27_i64..=32).map(PyRep::Integer).collect());
    let load_torps = ctx
        .dispatch_rpc(
            &mut sess2,
            "dogmaIM",
            None,
            "LoadAmmo",
            &[
                PyRep::Integer(100_000_950),
                raven_slots.clone(),
                PyRep::Integer(100_000_960),
            ],
            &kw,
            None,
        )
        .await
        .expect("LoadAmmo 6x Torpedo Launcher II");
    let PyRep::KeyVal(lt_kv) = load_torps else {
        panic!("Expected LoadAmmo KeyVal");
    };
    assert_eq!(lt_kv.get("totalLoaded"), Some(&PyRep::Integer(240)));
    assert_eq!(ctx.db.get_item(100_000_960).unwrap().quantity, 19_760);

    let mut atk1_state = String::new();
    for volley_idx in 0..1340 {
        for _ in 0..15 {
            let _ = tama_handle.step_manual(1.0).await.unwrap();
        }
        let atk1 = match ctx
            .dispatch_rpc(
                &mut sess2,
                "structureControl",
                None,
                "AttackStructure",
                &[PyRep::Integer(tama_struct_id as i64)],
                &kw,
                None,
            )
            .await
        {
            Ok(res) => res,
            Err(e) if e.code == "NoAmmoLoaded" => {
                ctx.dispatch_rpc(
                    &mut sess2,
                    "dogmaIM",
                    None,
                    "LoadAmmo",
                    &[
                        PyRep::Integer(100_000_950),
                        raven_slots.clone(),
                        PyRep::Integer(100_000_960),
                    ],
                    &kw,
                    None,
                )
                .await
                .expect("Reload Torpedo Launcher II in Shield siege");
                ctx.dispatch_rpc(
                    &mut sess2,
                    "structureControl",
                    None,
                    "AttackStructure",
                    &[PyRep::Integer(tama_struct_id as i64)],
                    &kw,
                    None,
                )
                .await
                .expect("AttackStructure Shield volley after reload")
            }
            Err(e) => panic!("Unexpected AttackStructure Shield error: {e:?}"),
        };
        if volley_idx == 0 {
            for flag_u16 in 27_u16..=32 {
                let slot_qty = ctx.db.with_snapshot(|s| {
                    s.items
                        .values()
                        .find(|i| {
                            i.location_id == 100_000_950
                                && (i.flag as u16) == flag_u16
                                && i.type_id == 2510
                        })
                        .map(|i| i.quantity)
                        .unwrap_or(0)
                });
                assert_eq!(slot_qty, 39);
            }
        }
        let PyRep::KeyVal(atk1_kv) = atk1 else {
            panic!("Expected atk1 KeyVal");
        };
        atk1_state = atk1_kv
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if atk1_state == "ArmorReinforced" {
            break;
        }
    }
    assert_eq!(atk1_state, "ArmorReinforced");

    // Structure remains invulnerable even after 901s because ArmorReinforced timer is 24h (86,400s)
    ctx.advance_virtual_clock(901);
    for _ in 0..15 {
        let _ = tama_handle.step_manual(1.0).await.unwrap();
    }
    let invuln_err = ctx
        .dispatch_rpc(
            &mut sess2,
            "structureControl",
            None,
            "AttackStructure",
            &[PyRep::Integer(tama_struct_id as i64)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        invuln_err.unwrap_err().code,
        "StructureReinforcedInvulnerable"
    );

    // Advance another 86,400s past the 24h ArmorReinforced timer and cycle 667 volleys (667 * 2,700 = 1,800,900 >= 1,800,000) to strip Armor
    ctx.advance_virtual_clock(86_400);
    let mut atk2_state = String::new();
    for _ in 0..670 {
        for _ in 0..15 {
            let _ = tama_handle.step_manual(1.0).await.unwrap();
        }
        let atk2 = match ctx
            .dispatch_rpc(
                &mut sess2,
                "structureControl",
                None,
                "AttackStructure",
                &[PyRep::Integer(tama_struct_id as i64)],
                &kw,
                None,
            )
            .await
        {
            Ok(res) => res,
            Err(e) if e.code == "NoAmmoLoaded" => {
                ctx.dispatch_rpc(
                    &mut sess2,
                    "dogmaIM",
                    None,
                    "LoadAmmo",
                    &[
                        PyRep::Integer(100_000_950),
                        raven_slots.clone(),
                        PyRep::Integer(100_000_960),
                    ],
                    &kw,
                    None,
                )
                .await
                .expect("Reload Torpedo Launcher II in Armor siege");
                ctx.dispatch_rpc(
                    &mut sess2,
                    "structureControl",
                    None,
                    "AttackStructure",
                    &[PyRep::Integer(tama_struct_id as i64)],
                    &kw,
                    None,
                )
                .await
                .expect("AttackStructure Armor volley after reload")
            }
            Err(e) => panic!("Unexpected AttackStructure Armor error: {e:?}"),
        };
        let PyRep::KeyVal(atk2_kv) = atk2 else {
            panic!("Expected atk2 KeyVal");
        };
        atk2_state = atk2_kv
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if atk2_state == "HullReinforced" {
            break;
        }
    }
    assert_eq!(atk2_state, "HullReinforced");

    // Advance past the 48h (172,800s) HullReinforced timer and cycle 667 volleys (667 * 2,700 = 1,800,900 >= 1,800,000) to destroy Hull
    ctx.advance_virtual_clock(172_800 + 1);
    let mut atk3_state = String::new();
    let mut dropped_core = 0_i64;
    for _ in 0..670 {
        for _ in 0..15 {
            let _ = tama_handle.step_manual(1.0).await.unwrap();
        }
        let atk3 = match ctx
            .dispatch_rpc(
                &mut sess2,
                "structureControl",
                None,
                "AttackStructure",
                &[PyRep::Integer(tama_struct_id as i64)],
                &kw,
                None,
            )
            .await
        {
            Ok(res) => res,
            Err(e) if e.code == "NoAmmoLoaded" => {
                ctx.dispatch_rpc(
                    &mut sess2,
                    "dogmaIM",
                    None,
                    "LoadAmmo",
                    &[
                        PyRep::Integer(100_000_950),
                        raven_slots.clone(),
                        PyRep::Integer(100_000_960),
                    ],
                    &kw,
                    None,
                )
                .await
                .expect("Reload Torpedo Launcher II in Hull siege");
                ctx.dispatch_rpc(
                    &mut sess2,
                    "structureControl",
                    None,
                    "AttackStructure",
                    &[PyRep::Integer(tama_struct_id as i64)],
                    &kw,
                    None,
                )
                .await
                .expect("AttackStructure Hull volley after reload")
            }
            Err(e) => panic!("Unexpected AttackStructure Hull error: {e:?}"),
        };
        let PyRep::KeyVal(atk3_kv) = atk3 else {
            panic!("Expected atk3 KeyVal");
        };
        atk3_state = atk3_kv
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        dropped_core = atk3_kv
            .get("droppedQuantumCoreItemID")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if atk3_state == "Destroyed" {
            break;
        }
    }
    assert_eq!(atk3_state, "Destroyed");
    assert!(dropped_core > 0);

    // B-09 & R-P1-03: Faction Warfare CaptureComplex in low-sec Tama (30002813, complex beacon at [250_000.0, 0.0, -150_000.0], 600s duration, starts at 270s / 45.0%)
    ctx.dispatch_rpc(
        &mut sess2,
        "fwCharacterEnlistmentMgr",
        None,
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500001)],
        &kw,
        None,
    )
    .await
    .expect("Enlist FW");

    // Attempting to capture at stargate/structure position (outside 30km of [250_000.0, 0.0, -150_000.0]) is rejected
    let out_of_radius = ctx
        .dispatch_rpc(
            &mut sess2,
            "facWarMgr",
            None,
            "CaptureComplex",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        out_of_radius.unwrap_err().code,
        "OutsideComplexCaptureRadius"
    );

    // Warp to complex beacon [250_000.0, 0.0, -150_000.0]
    let tama_b2 = ctx.bind_object(&mut sess2, "beyonce", 30_002_813).unwrap();
    ctx.dispatch_rpc(
        &mut sess2,
        "beyonce",
        Some(tama_b2),
        "CmdWarpToPoint",
        &[
            PyRep::Real(250_000.0),
            PyRep::Real(0.0),
            PyRep::Real(-150_000.0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Warp sess2 to Tama FW complex beacon");
    for _ in 0..60 {
        let _ = tama_handle.step_manual(1.0).await.unwrap();
    }

    // First 2 capture ticks only advance elapsed timer from 270s -> 271s -> 272s out of 600s (0 LP awarded)
    for expected_elapsed in [271_i64, 272_i64] {
        let _ = tama_handle.step_manual(1.0).await.unwrap();
        let fw_tick = ctx
            .dispatch_rpc(
                &mut sess2,
                "facWarMgr",
                None,
                "CaptureComplex",
                &[],
                &kw,
                None,
            )
            .await
            .expect("CaptureComplex tick");
        let PyRep::KeyVal(fw_kv) = fw_tick else {
            panic!("Expected fw_tick KeyVal");
        };
        assert_eq!(
            fw_kv.get("captureSecondsElapsed").and_then(|v| v.as_i64()),
            Some(expected_elapsed)
        );
        assert_eq!(fw_kv.get("lpAwarded").and_then(|v| v.as_i64()), Some(0));
    }

    // R-P1-03 & AP-07: CaptureComplex clamps each pulse to <= 30 ticks while within 30km; step 11 bounded 30-tick intervals (11 * 30 >= 328s) to reach 600s / 100.0% and award 15,000 LP
    let mut fw_final_kv = None;
    for _ in 0..11 {
        for _ in 0..30 {
            let _ = tama_handle.step_manual(1.0).await.unwrap();
        }
        let fw_step = ctx
            .dispatch_rpc(
                &mut sess2,
                "facWarMgr",
                None,
                "CaptureComplex",
                &[],
                &kw,
                None,
            )
            .await
            .expect("CaptureComplex bounded 30-tick step");
        let PyRep::KeyVal(step_kv) = fw_step else {
            panic!("Expected fw_step KeyVal");
        };
        fw_final_kv = Some(step_kv);
    }
    let fw_final_kv = fw_final_kv.expect("Expected 11th CaptureComplex result");
    assert_eq!(
        fw_final_kv.get("contestedPct").and_then(|v| v.as_f64()),
        Some(100.0)
    );
    assert_eq!(
        fw_final_kv.get("lpAwarded").and_then(|v| v.as_i64()),
        Some(15_000)
    );
}
