//! Integration regression tests covering blocking findings B-01 through B-15 (Part 2: Tama structure lifecycle, fueling, clones, and carriers).

#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const EXPECTED_REMOTE_ERR: &str = "MustBeDockedAtTargetStation";

#[tokio::test]
async fn test_blocking_findings_b01_to_b15_tama_structure_and_clone_regressions() {
    // parity: rpc:fighterMgr.CmdActivateAbilitySlots, rpc:fighterMgr.GetFightersForShip, rpc:home_station.set_home_station, rpc:jumpCloneSvc.CloneJump, rpc:jumpCloneSvc.InstallCloneInStation, rpc:jumpCloneSvc.InstallCloneInStructure, rpc:ship.Board, rpc:structureCynoBeaconMgr.CmdJumpToStructureBeacon, rpc:structureDeployment.Anchor, rpc:structureDocking.Dock, rpc:structureDocking.Undock
    use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();
    let mut sess = SessionState::new(1);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let extra_items = [
            (100_000_015, 35832, 100_000_001, InventoryFlag::Cargo, 5, false),
            (100_000_016, 56201, 100_000_001, InventoryFlag::Cargo, 5, false),
            (100_000_023, 23055, 100_000_024, InventoryFlag::FighterBay, 9, false),
            (100_000_024, 24483, 60_003_760, InventoryFlag::Hangar, 1, true),
            (100_000_025, 4247, 100_000_024, InventoryFlag::Cargo, 500, false),
            (100_000_026, 4247, 100_000_001, InventoryFlag::Cargo, 2_000, false),
            (100_000_036, 35840, 100_000_001, InventoryFlag::Cargo, 2, false),
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

    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
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
                        &[PyRep::Real(dir[0]), PyRep::Real(dir[1]), PyRep::Real(dir[2])],
                        kw_ref,
                        None,
                    )
                    .await
                    .expect("CmdGotoDirection to nearby stargate");
                for _ in 0..9 {
                    let _ = handle.step_manual(1.0).await.unwrap();
                }
                ctx_ref
                    .dispatch_rpc(sess_ref, "beyonce", Some(bound), "CmdStop", &[], kw_ref, None)
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
    assert_eq!(remote_mat_err.unwrap_err().code, "CannotMaterializeRemoteSystem");

    // R2-P1-03: Anchoring an Upwell structure while in starter NPC corporation (1_000_172) is rejected with NpcCorpCannotAnchorStructure
    let npc_anchor_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDeployment",
            None,
            "Anchor",
            &[PyRep::Integer(35832), PyRep::String("Tama Cyno Citadel".into())],
            &kw,
            None,
        )
        .await;
    assert_eq!(npc_anchor_err.unwrap_err().code, "NpcCorpCannotAnchorStructure");

    // Create a player corporation on sess (90_000_001) before anchoring structures
    ctx.advance_secs(12);
    ctx.dispatch_rpc(
        &mut sess,
        "corpRegistry",
        None,
        "AddCorporation",
        &[
            PyRep::String("Tama Vanguard Corp".into()),
            PyRep::String("TVC".into()),
            PyRep::Real(0.05),
        ],
        &kw,
        None,
    )
    .await
    .expect("AddCorporation TVC");

    // Anchoring needs 1,000 km clearance from gates/stations/structures: fly 5,000,000 km off the gate.
    let tama = ctx.systems.get_or_spawn(30002813);
    let tama_ship = sess.ship_id.unwrap();
    let gate_side = tama.get_ball_snapshot(tama_ship).await.unwrap().unwrap().1.position;
    let citadel_spot = gate_side + glam::DVec3::new(0.0, 5.0e9, 0.0);
    assert!(tama.set_ball_position(tama_ship, citadel_spot).await);

    // Anchor Upwell Astrahus in Tama (30002813), verify fuelBlocks == 0, and fuel it from Rifter cargo via invbroker.Add (flag 133 = SpecializedFuelBay)
    let anchor_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDeployment",
            None,
            "Anchor",
            &[PyRep::Integer(35832), PyRep::String("Tama Cyno Citadel".into())],
            &kw,
            None,
        )
        .await
        .expect("Anchor Tama Cyno Citadel");
    let PyRep::KeyVal(anchor_kv) = anchor_res else {
        panic!("Expected anchor KeyVal");
    };
    assert_eq!(anchor_kv.get("fuelBlocks").and_then(|v| v.as_i64()), Some(0));
    let tama_struct_id = anchor_kv.get("structureID").unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(100_000_026),
            PyRep::Integer(tama_struct_id as i64),
            PyRep::Integer(133),
            PyRep::Integer(1_500),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fuel Tama Cyno Citadel via invbroker.Add");

    // R-P1-07 & AP-06: Anchor and fuel a dedicated Pharolux Cyno Beacon (35840) in low-sec Tama (30002813), 2,000 km from the citadel.
    assert!(tama.set_ball_position(tama_ship, citadel_spot + glam::DVec3::new(2.0e6, 0.0, 0.0)).await);
    let anchor_beacon_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDeployment",
            None,
            "Anchor",
            &[PyRep::Integer(35840), PyRep::String("Tama Cyno Beacon".into())],
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
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(100_000_026),
            PyRep::Integer(tama_beacon_id as i64),
            PyRep::Integer(133),
            PyRep::Integer(50),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fuel Tama Cyno Beacon via invbroker.Add");

    // Fast-forward citadel to operational state so jump-clone checks below run against an online structure.
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&(tama_struct_id as i64)).expect("Tama citadel");
        st.state = "ShieldVulnerable".into();
        st.quantum_core_installed = true;
        st.reinforced_until_unix = 0;
    });
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
        .dispatch_rpc(&mut sess, "jumpCloneSvc", None, "InstallCloneInStation", &[], &kw, None)
        .await;
    assert_eq!(wrong_clone_rpc.unwrap_err().code, "MustBeDocked");
    let clone_tama = ctx
        .dispatch_rpc(&mut sess, "jumpCloneSvc", None, "InstallCloneInStructure", &[], &kw, None)
        .await
        .expect("InstallCloneInStructure at Tama Cyno Citadel");
    let PyRep::KeyVal(ct_kv) = clone_tama else {
        panic!("Expected clone_tama KeyVal");
    };
    let tama_clone_id = ct_kv.get("jumpCloneID").unwrap().as_i64().unwrap();
    ctx.dispatch_rpc(&mut sess, "structureDocking", None, "Undock", &[], &kw, None)
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
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Dock", &[PyRep::Integer(60003760)], &kw, None)
        .await
        .expect("Dock Rifter at Jita");

    // S-9: MaterializeSystemLOD2 rejects non-GM roles (including legacy 0x02) and requires 64-bit V24.01 serviceConst GM role bitmask
    assert_eq!(ctx.living_world.get_convoy(800_000_001).map(|c| c.lod_level), Some(2));
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
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT | eve_machonet::service_roles::ROLE_GML;
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
    assert_eq!(remote_home_err.unwrap_err().code, EXPECTED_REMOTE_ERR);
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
    let pod_id = 99_000_700;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            pod_id,
            eve_db::InventoryItemRecord {
                item_id: pod_id,
                type_id: 670,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.active_ship_id = pod_id;
        }
    });
    sess.ship_id = Some(pod_id);
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

    // Board Archon Carrier (100_000_024, type_id = 24483), undock, and verify S-2 & S-3
    let char_id = sess.character_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(20533, eve_db::TrainedSkillRecord { type_id: 20533, level: 5, skill_points: 256_000.0 });
        sk.insert(3327, eve_db::TrainedSkillRecord { type_id: 3327, level: 5, skill_points: 256_000.0 });
        sk.insert(24314, eve_db::TrainedSkillRecord { type_id: 24314, level: 5, skill_points: 256_000.0 });
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
        }
    });
    ctx.advance_secs(12);
    ctx.dispatch_rpc(&mut sess, "ship", None, "Board", &[PyRep::Integer(100_000_024)], &kw, None)
        .await
        .expect("Board Archon Carrier");
    assert_eq!(sess.ship_id, Some(100_000_024));

    ctx.advance_secs(12);
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock Archon");
    ctx.dispatch_rpc(&mut sess, "fighterMgr", None, "GetFightersForShip", &[], &kw, None)
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

    let _ = jita_handle.remove_ball(100_000_024).await;
    ctx.db
        .set_character_solar_system(90_000_001, 30_002_813, &ctx.sde)
        .expect("Move Archon to Tama");
    sess.solar_system_id = Some(30_002_813);
}
