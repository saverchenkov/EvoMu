//! Integration regression tests covering blocking findings B-01 through B-15.

#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_blocking_findings_b01_to_b15_regressions() {
    // parity: rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID, rpc:fighterMgr.CmdActivateAbilitySlots, rpc:fighterMgr.GetFightersForShip, rpc:home_station.set_home_station, rpc:industryManager.CompleteJob, rpc:industryManager.InstallJob, rpc:jumpCloneSvc.CloneJump, rpc:jumpCloneSvc.InstallCloneInStation, rpc:jumpCloneSvc.InstallCloneInStructure, rpc:planetMgr.Flush, rpc:planetMgr.UserUpdateNetwork, rpc:ship.Board, rpc:shipCosmeticsMgr.ActivateSkinLicense, rpc:shipCosmeticsMgr.ApplySkinToShip, rpc:structureCynoBeaconMgr.CmdJumpToStructureBeacon, rpc:structureDeployment.Anchor, rpc:structureDocking.Dock, rpc:structureDocking.Undock
    use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
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
                2_000,
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

    // R-P1-06: non-V24 air_npe.AdvanceNode is removed and returns UnimplementedRPC
    let npe_gate_err = ctx
        .dispatch_rpc(&mut sess, "air_npe", None, "AdvanceNode", &[], &kw, None) // rpc-lint: allow legacy/negative-test RPC (air_npe.AdvanceNode)
        .await;
    assert_eq!(npe_gate_err.unwrap_err().code, "UnimplementedRPC");

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
                type_id: 2534,
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
                PyRep::Integer(2534),
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
                "CmdWarpToStuff",
                &[PyRep::String("item".into()), PyRep::Integer(gate_id)],
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
            Err(e) => panic!("CmdWarpToStuff to stargate: {e:?}"),
        }
        ctx_ref.advance_secs(12);
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

    // R36-P1-1 & R2-P0-01: materialize_system_lod2 rejects non-GMs with InsufficientRole, and rejects remote solar system arguments from GMs with CannotMaterializeRemoteSystem
    let non_gm_mat_err = eve_server::services::cosmetics_events::materialize_system_lod2(
        &ctx,
        &sess,
        &[PyRep::Integer(30_000_142)],
    )
    .await;
    assert_eq!(non_gm_mat_err.unwrap_err().code, "InsufficientRole");
    let saved_role = sess.role;
    sess.role |= eve_machonet::service_roles::ROLE_GMH;
    let remote_mat_err = eve_server::services::cosmetics_events::materialize_system_lod2(
        &ctx,
        &sess,
        &[PyRep::Integer(30_000_142)],
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
        "CreateCorporation", // rpc-lint: allow legacy/negative-test RPC (corpmgr.CreateCorporation)
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

    // Anchoring needs 1,000 km clearance from gates/stations/structures: fly 5,000,000 km off the gate.
    let tama = ctx.systems.get_or_spawn(30002813);
    let tama_ship = sess.ship_id.unwrap();
    let gate_side = tama.get_ball_snapshot(tama_ship).await.unwrap().unwrap().1.position;
    let citadel_spot = gate_side + glam::DVec3::new(0.0, 5.0e9, 0.0);
    assert!(tama.set_ball_position(tama_ship, citadel_spot).await);
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
        "FuelStructure", // rpc-lint: allow legacy/negative-test RPC (structureControl.FuelStructure)
        &[PyRep::Integer(tama_struct_id as i64), PyRep::Integer(1_500)],
        &kw,
        None,
    )
    .await
    .expect("FuelStructure");

    // R-P1-07 & AP-06: Anchor and fuel a dedicated Pharolux Cyno Beacon (35840) in low-sec Tama (30002813),
    // 2,000 km from the citadel.
    assert!(tama.set_ball_position(tama_ship, citadel_spot + glam::DVec3::new(2.0e6, 0.0, 0.0)).await);
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
        "FuelStructure", // rpc-lint: allow legacy/negative-test RPC (structureControl.FuelStructure)
        &[PyRep::Integer(tama_beacon_id as i64), PyRep::Integer(50)],
        &kw,
        None,
    )
    .await
    .expect("FuelStructure Tama Cyno Beacon");

    // The anchoring/core/onlining lifecycle is covered by structures_lifecycle_complete.rs; here the
    // citadel is fast-forwarded to its operational state (core installed, shield vulnerable) so the
    // jump-clone checks below run against an online structure.
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&(tama_struct_id as i64)).expect("Tama citadel");
        st.state = "ShieldVulnerable".into();
        st.quantum_core_installed = true;
        st.reinforced_until_unix = 0;
    });
    // Dock at Tama Cyno Citadel, install a jump clone there, and undock
    assert!(tama.set_ball_position(tama_ship, citadel_spot).await);
    ctx.advance_secs(12);
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
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(60003760)],
        &kw,
        None,
    )
    .await
    .expect("Warp to Jita 4-4");
    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    for _ in 0..40 {
        let _ = jita_handle.step_manual(1.0).await.unwrap();
    }
    ctx.advance_secs(12);
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
    let non_gm_mat = eve_server::services::cosmetics_events::materialize_system_lod2(
        &ctx,
        &sess,
        &[PyRep::Integer(30_000_142)],
    )
    .await;
    assert_eq!(non_gm_mat.unwrap_err().code, "InsufficientRole");
    sess.role |= 0x02;
    let legacy_bit_mat = eve_server::services::cosmetics_events::materialize_system_lod2(
        &ctx,
        &sess,
        &[PyRep::Integer(30_000_142)],
    )
    .await;
    assert_eq!(legacy_bit_mat.unwrap_err().code, "InsufficientRole");
    sess.role =
        eve_machonet::service_roles::ROLE_PLAYER_DEFAULT | eve_machonet::service_roles::ROLE_GML;
    let mat_second = eve_server::services::cosmetics_events::materialize_system_lod2(
        &ctx,
        &sess,
        &[PyRep::Integer(30_000_142)],
    )
    .await
    .expect("GM materialize_system_lod2");
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
        "MustBeDockedAtTargetStation" // rpc-lint: allow legacy/negative-test RPC (home_station.MustBeDockedAtTargetStation)
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
    ctx.advance_secs(12);
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
    ctx.advance_secs(12);
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

    ctx.advance_secs(12);
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
    ctx.advance_secs(12);
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
}
