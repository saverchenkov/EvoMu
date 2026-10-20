//! `crates/eve-server/tests/space_navigation_cyno.rs`
//!
//! Track 6: Space & Navigation Polish (SP-2, SP-3, SP-5, S-2, S-3)
//!
//! Comprehensive integration and specification tests for:
//! - `SP-2`: Spatial grid bubbles: interdiction spheres (bubbles), mobile warp disruptors, bubble transition and trapping warp exits within bubble radius.
//! - `SP-3`: Beyonce command set: velocity, agility, approach, orbit, keep at range, align, warp to bookmark / celestial.
//! - `SP-5`: Dock and undock: approach station docking perimeter radius, dock delay, undock along station undock vector.
//! - `S-2`: Fighter manager (`fighterMgr`): load tubes, launch, recall, movement, fighter abilities.
//! - `S-3`: Pharolux cyno beacon: `structureCynoBeaconMgr.CmdJumpToStructureBeacon/CmdBridgeToStructureBeacon/CmdGroupJumpToStructureBeacon` jumping to a beacon structure with range and fuel checks.
//!
//! // parity: plan:SP-2, plan:SP-3, plan:SP-5, plan:S-2, plan:S-3
//! // parity: feat:modules.interdiction-sphere, feat:modules.mobile-warp-disruptor, feat:modules.interdiction-nullifier, feat:dogma.velocity-agility, feat:modules.afterburner, feat:modules.microwarpdrive, feat:modules.micro-jump-drive, feat:navigation.approach, feat:ships.board-ship, feat:navigation.undock, feat:navigation.dock-station, feat:navigation.dock-structure, feat:drones.fighter-load-tubes, feat:drones.fighter-launch-recall, feat:drones.fighter-movement, feat:drones.fighter-abilities, feat:drones.fighter-abandon, feat:navigation.pharolux-beacon

#![forbid(unsafe_code)]

mod common;

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_dogma::{eval_propulsion_module, DogmaItemState};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

fn grant_all_prereq_skills_for_types(ctx: &ServerContext, char_id: i64, type_ids: &[u32]) {
    let mut needed = Vec::new();
    let mut stack: Vec<u32> = type_ids.to_vec();
    while let Some(tid) = stack.pop() {
        if let Some(t) = ctx.sde.types.get(&tid) {
            for &skill_attr in &[182u16, 183, 184, 1285, 1289, 1291] {
                if let Some(&req_id) = t.default_attributes.get(&skill_attr) {
                    let stid = req_id as u32;
                    if stid > 0 && !needed.contains(&stid) {
                        needed.push(stid);
                        stack.push(stid);
                    }
                }
            }
        }
    }
    ctx.db.with_mut_snapshot(|snap| {
        let skill_map = snap.skills.entry(char_id).or_default();
        for stid in needed {
            skill_map.insert(
                stid,
                eve_db::TrainedSkillRecord {
                    type_id: stid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });
}

#[tokio::test]
async fn test_sp2_spatial_bubbles_interdiction_sphere_mobile_warp_disruptor_and_nullifier() {
    // parity: plan:SP-2
    // parity: feat:modules.interdiction-sphere, feat:modules.mobile-warp-disruptor, feat:modules.interdiction-nullifier
    eve_testkit::attest(&[
        "plan:SP-2",
        "feat:modules.interdiction-sphere",
        "feat:modules.mobile-warp-disruptor",
        "feat:modules.interdiction-nullifier",
    ]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(2001);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;

    // Grant skills for Sabre (22456), Interdiction Sphere Launcher I (22782), Warp Disrupt Probe (22778),
    // Mobile Small Warp Disruptor I (12198), Stiletto (11198), Interdiction Nullifier I (58919)
    grant_all_prereq_skills_for_types(
        &ctx,
        char_id,
        &[22456, 22782, 22778, 12198, 11198, 58919],
    );

    let launcher_item_id = 80_001_001;
    let probe_stack_item_id = 80_001_002;
    let mobile_disruptor_item_id = 80_001_003;
    let nullifier_item_id = 80_001_004;

    ctx.db.with_mut_snapshot(|snap| {
        for item in snap.items.values_mut() {
            if item.location_id == ship_id {
                item.flag = InventoryFlag::Cargo;
            }
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 22456; // Sabre (Interdictor, group 541)
        }
        // Interdiction Sphere Launcher I in ship cargo to fit via invbroker.Add
        snap.items.insert(
            launcher_item_id,
            InventoryItemRecord {
                item_id: launcher_item_id,
                type_id: 22782, // Interdiction Sphere Launcher I (group 589)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
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
        // Warp Disrupt Probe loaded in HiSlot0 (flag 27)
        snap.items.insert(
            probe_stack_item_id,
            InventoryItemRecord {
                item_id: probe_stack_item_id,
                type_id: 22778, // Warp Disrupt Probe (group 548, 20km radius)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 5,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        // Mobile Small Warp Disruptor I in ship cargo
        snap.items.insert(
            mobile_disruptor_item_id,
            InventoryItemRecord {
                item_id: mobile_disruptor_item_id,
                type_id: 12198, // Mobile Small Warp Disruptor I (group 361)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        // Interdiction Nullifier I in ship cargo (for Stiletto test in step 4)
        snap.items.insert(
            nullifier_item_id,
            InventoryItemRecord {
                item_id: nullifier_item_id,
                type_id: 58919, // Interdiction Nullifier I (group 4117)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
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
    });

    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // Fit Interdiction Sphere Launcher I to HiSlot0 (27) while docked in Jita
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(launcher_item_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::HiSlot0 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit Interdiction Sphere Launcher I");

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock in Jita");

    // 1a. High-sec rejection (Jita 30000142, security 0.95 > 0.0):
    // Both Interdiction Sphere Launcher activation and Mobile Warp Disruptor anchoring must be rejected!
    let hs_probe_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(launcher_item_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Interdiction Sphere Launcher must be rejected in high-sec");
    assert_eq!(hs_probe_err.code, "CannotLaunchInterdictionProbeInEmpire");

    let hs_anchor_err = ctx
        .dispatch_rpc(
            &mut sess,
            "posMgr",
            None,
            "AnchorStructure",
            &[PyRep::Integer(mobile_disruptor_item_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Mobile Warp Disruptor anchoring must be rejected in high-sec");
    assert_eq!(hs_anchor_err.code, "CantAnchorInEmpire");

    // 1b. Low-sec rejection (Asabona 30000012, security 0.32 > 0.0):
    let lowsec_sys = 30_000_012;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = lowsec_sys;
        snap.items.get_mut(&ship_id).unwrap().location_id = lowsec_sys as i64;
    });
    sess.solar_system_id = Some(lowsec_sys);
    let ls_probe_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(launcher_item_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Interdiction Sphere Launcher must be rejected in low-sec");
    assert_eq!(ls_probe_err.code, "CannotLaunchInterdictionProbeInEmpire");

    let ls_anchor_err = ctx
        .dispatch_rpc(
            &mut sess,
            "posMgr",
            None,
            "AnchorStructure",
            &[PyRep::Integer(mobile_disruptor_item_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Mobile Warp Disruptor anchoring must be rejected in low-sec");
    assert_eq!(ls_anchor_err.code, "CantAnchorInEmpire");

    // 2. Move pilot + Sabre to null-sec system 6VDT-H (30004608, security -0.70 <= 0.0)
    let null_sys_id = 30_004_608;
    let null_handle = ctx.systems.get_or_spawn(null_sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = null_sys_id;
        snap.items.get_mut(&ship_id).unwrap().location_id = null_sys_id as i64;
    });
    sess.solar_system_id = Some(null_sys_id);
    let null_beyonce_id = ctx
        .bind_object(&mut sess, "beyonce", null_sys_id as i64)
        .expect("bind beyonce in null-sec");

    let sphere_bubble_pos = DVec3::new(25_000_000.0, 0.0, 0.0);
    let sabre_ball = Ball {
        id: ship_id,
        type_id: 22456,
        position: sphere_bubble_pos,
        velocity: DVec3::ZERO,
        max_velocity: 380.0,
        inertia: 2.8,
        mass: 1_350_000.0,
        radius: 58.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    null_handle
        .spawn_ball_with_meta(
            sabre_ball,
            ctx.build_ship_combat_state(char_id, ship_id, 22456),
            ctx.build_ship_slim_meta(char_id, 22456),
        )
        .await
        .expect("spawn Sabre in null-sec");

    // 2a. Launch Interdiction Sphere Probe via dogmaIM.Activate (feat:modules.interdiction-sphere)
    let launch_res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(launcher_item_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate Interdiction Sphere Launcher I in null-sec");
    let PyRep::KeyVal(launch_kv) = launch_res else {
        panic!("Expected KeyVal from Interdiction Sphere Launcher Activate");
    };
    let probe_ball_id = launch_kv.get("probeID").unwrap().as_i64().unwrap();
    let probe_radius = launch_kv.get("bubbleRadius").unwrap().as_f64().unwrap();
    let rem_probes = launch_kv.get("remainingInLauncher").unwrap().as_i64().unwrap();
    assert_eq!(probe_radius, 20_000.0, "Warp Disrupt Probe radius must be 20 km from SDE");
    assert_eq!(rem_probes, 4, "Launching interdiction sphere must consume 1 probe");
    assert!(
        null_handle.is_ball_scrambled(probe_ball_id).await.unwrap(),
        "Launched interdiction sphere must scramble its 20 km bubble volume"
    );

    // 2b. Verify warp drag/stop catching a warping ship whose trajectory intersects the interdiction sphere
    let warping_victim_id = 100_000_050;
    let victim_ball = Ball {
        id: warping_victim_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::new(300.0, 0.0, 0.0),
        max_velocity: 365.0,
        inertia: 3.2,
        mass: 1_067_000.0,
        radius: 35.0,
        speed_fraction: 1.0,
        mode: MovementMode::new_warp(DVec3::ZERO, DVec3::new(50_000_000.0, 0.0, 0.0), 3.0),
    };
    null_handle
        .spawn_ball_with_combat(victim_ball, ctx.build_type_combat_state(587))
        .await
        .expect("spawn warping victim ship");
    for _ in 0..25 {
        null_handle.step_manual(1.0).await.expect("step nullsec ballpark for warp drag");
    }
    let (_, caught_ball) = null_handle
        .get_ball_snapshot(warping_victim_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        caught_ball.mode,
        MovementMode::Stop,
        "Warping ship intersecting interdiction sphere must be dragged out of warp and stopped"
    );
    assert!(
        caught_ball.position.distance(sphere_bubble_pos) <= probe_radius + 1.0,
        "Dragged ship must land at interdiction sphere boundary (dist={})",
        caught_ball.position.distance(sphere_bubble_pos)
    );
    assert!(
        null_handle.is_ball_scrambled(warping_victim_id).await.unwrap(),
        "Ship caught in interdiction sphere must be warp scrambled"
    );

    // 3. Deploy & Anchor Mobile Small Warp Disruptor I via posMgr.AnchorStructure in null-sec (feat:modules.mobile-warp-disruptor)
    let mwd_bubble_pos = DVec3::new(12_000_000.0, 0.0, 0.0);
    null_handle.set_ball_position(ship_id, mwd_bubble_pos).await;
    let anchor_res = ctx
        .dispatch_rpc(
            &mut sess,
            "posMgr",
            None,
            "AnchorStructure",
            &[
                PyRep::Integer(mobile_disruptor_item_id),
                PyRep::Tuple(vec![
                    PyRep::Real(mwd_bubble_pos.x),
                    PyRep::Real(mwd_bubble_pos.y),
                    PyRep::Real(mwd_bubble_pos.z),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .expect("AnchorStructure Mobile Small Warp Disruptor I in null-sec");
    let PyRep::KeyVal(anchor_kv) = anchor_res else {
        panic!("Expected KeyVal from posMgr.AnchorStructure");
    };
    assert_eq!(
        anchor_kv.get("state").unwrap().as_str().unwrap(),
        "Anchoring",
        "Mobile Warp Disruptor must enter Anchoring state first"
    );
    let anchor_delay_s = anchor_kv.get("anchoringDelaySeconds").unwrap().as_f64().unwrap();
    let mwd_bubble_radius = anchor_kv.get("bubbleRadius").unwrap().as_f64().unwrap();
    assert_eq!(anchor_delay_s, 120.0, "SDE anchoringDelay (attr 556) for 12198 is 120s");
    assert_eq!(mwd_bubble_radius, 5_000.0, "SDE warpScrambleRange (attr 103) for 12198 is 5,000m");

    // Before anchoring delay completes (1 tick elapsed out of 120s), bubble must NOT be active yet
    null_handle.step_manual(1.0).await.expect("step 1s during anchoring");
    assert!(
        !null_handle.is_ball_scrambled(ship_id).await.unwrap(),
        "Mobile Warp Disruptor must NOT scramble ships before its 120s anchoring delay completes"
    );

    // Advance remaining 119s so anchoring delay completes and bubble activates
    for _ in 0..120 {
        null_handle.step_manual(1.0).await.expect("step anchoring delay to completion");
    }
    assert!(
        null_handle.is_ball_scrambled(ship_id).await.unwrap(),
        "Mobile Warp Disruptor must scramble ships within 5 km once anchoring delay completes"
    );

    let warp_target_struct_id = 1_040_000_000_999_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            warp_target_struct_id,
            UpwellStructureRecord {
                structure_id: warp_target_struct_id,
                type_id: 35832,
                solar_system_id: null_sys_id,
                owner_corp_id: 1000001,
                name: "6VDT-H Deep Safe".into(),
                position: [50_000_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });

    let warp_in_bubble_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(null_beyonce_id),
            "CmdWarpToStuff",
            &[
                PyRep::String("item".into()),
                PyRep::Integer(warp_target_struct_id),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Un-nullified ship inside active Mobile Warp Disruptor bubble must not warp");
    assert_eq!(warp_in_bubble_err.code, "WarpScrambled");

    // 4. Interdiction Nullifier I (58919) on Stiletto Interceptor (11198) (feat:modules.interdiction-nullifier)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 11198; // Stiletto (Interceptor, group 831)
            ship.location_id = warp_target_struct_id;
        }
        // Unfit launcher so HiSlot0 is free for Interdiction Nullifier I
        if let Some(launcher) = snap.items.get_mut(&launcher_item_id) {
            launcher.flag = InventoryFlag::Cargo;
        }
        if let Some(probes) = snap.items.get_mut(&probe_stack_item_id) {
            probes.flag = InventoryFlag::Cargo;
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.structure_id = Some(warp_target_struct_id);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(nullifier_item_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::HiSlot0 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit Interdiction Nullifier I on Stiletto");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.structure_id = None;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = null_sys_id as i64;
        }
    });
    sess.structure_id = None;

    // Before activating Interdiction Nullifier I, ship is still scrambled by the bubble
    assert!(
        null_handle.is_ball_scrambled(ship_id).await.unwrap(),
        "Passive/unactivated Interdiction Nullifier I must not grant bubble immunity"
    );

    // Activate Interdiction Nullifier I via dogmaIM.Activate
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(nullifier_item_id)],
        &kw,
        None,
    )
    .await
    .expect("Activate Interdiction Nullifier I");

    null_handle.step_manual(1.0).await.expect("step ballpark to sync nullifier state");
    assert!(
        !null_handle.is_ball_scrambled(ship_id).await.unwrap(),
        "Active Interdiction Nullifier I must grant immunity to warp disruption bubbles"
    );

    // Initiate warp via beyonce.CmdWarpToStuff out of the Mobile Warp Disruptor bubble and through the Interdiction Sphere
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(null_beyonce_id),
        "CmdWarpToStuff",
        &[
            PyRep::String("item".into()),
            PyRep::Integer(warp_target_struct_id),
        ],
        &kw,
        None,
    )
    .await
    .expect("Nullified Stiletto must initiate warp out of bubble via CmdWarpToStuff");

    for _ in 0..12 {
        null_handle.step_manual(1.0).await.expect("step nullified Stiletto align & warp");
    }
    let (_, nullified_ball) = null_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        nullified_ball.position.x > 30_000_000.0,
        "Nullified ship must warp out of the origin bubble and pass right through the 25,000 km interdiction sphere without being dragged (x={})",
        nullified_ball.position.x
    );
}

#[tokio::test]
async fn test_sp3_beyonce_command_set_navigation_agility_propulsion_and_mjd() {
    // parity: plan:SP-3
    // parity: feat:dogma.velocity-agility, feat:modules.afterburner, feat:modules.microwarpdrive, feat:modules.micro-jump-drive, feat:navigation.approach
    eve_testkit::attest(&[
        "plan:SP-3",
        "feat:dogma.velocity-agility",
        "feat:modules.afterburner",
        "feat:modules.microwarpdrive",
        "feat:modules.micro-jump-drive",
        "feat:navigation.approach",
    ]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(2002);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;

    // Grant prerequisite skills for Rifter (587), 5MN MWD I (434), Apocalypse (642), Large MJD (4383)
    grant_all_prereq_skills_for_types(&ctx, char_id, &[587, 434, 438, 440, 642, 4383]);

    let mwd_item_id = 80_002_001;
    let mjd_item_id = 80_002_002;
    ctx.db.with_mut_snapshot(|snap| {
        for item in snap.items.values_mut() {
            if item.location_id == ship_id {
                item.flag = InventoryFlag::Cargo;
            }
        }
        snap.items.insert(
            mwd_item_id,
            InventoryItemRecord {
                item_id: mwd_item_id,
                type_id: 434, // 5MN Microwarpdrive I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
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
        snap.items.insert(
            mjd_item_id,
            InventoryItemRecord {
                item_id: mjd_item_id,
                type_id: 4383, // Large Micro Jump Drive (12s spool, 100km jump)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
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
    });
    ctx.invalidate_ship_fitting(ship_id);

    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // Fit 5MN Microwarpdrive I (434) while docked in station before undocking
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(mwd_item_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::MedSlot0 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit 5MN Microwarpdrive I to Rifter MedSlot0");

    let undock_res = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(ukv) = undock_res else {
        panic!("Expected KeyVal from Undock");
    };
    let cur_b_id = ukv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;

    let sys_id = 30_000_142;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // 1. Dogma Velocity & Agility calculations (feat:dogma.velocity-agility)
    let rifter = DogmaItemState::from_sde(ship_id, 587, &ctx.sde);
    let base_vel = rifter.get_attribute(eve_sde::attr_ids::MAX_VELOCITY, &ctx.sde);
    let base_mass = rifter.get_attribute(eve_sde::attr_ids::MASS, &ctx.sde);
    let base_inertia = rifter.get_attribute(eve_sde::attr_ids::AGILITY, &ctx.sde);
    let base_sig = rifter.get_attribute(eve_sde::attr_ids::SIGNATURE_RADIUS, &ctx.sde);
    let base_cap = rifter.get_attribute(eve_sde::attr_ids::CAPACITOR_CAPACITY, &ctx.sde);
    assert_eq!(base_vel, 365.0);
    assert_eq!(base_mass, 1_067_000.0);
    assert!((base_inertia - 3.2).abs() < 1e-4);

    let tau = (base_inertia * base_mass) / 1_000_000.0;
    let t_align = (-0.25_f64.ln()) * tau;
    assert!((tau - 3.414).abs() < 1e-2, "Align time constant tau must be ~3.41 s: {tau}");
    assert!((t_align - 4.733).abs() < 1e-2, "Align time must be ~4.73 s: {t_align}");

    // 2. Afterburner (feat:modules.afterburner)
    let ab2 = DogmaItemState::from_sde(2001, 438, &ctx.sde); // 1MN Afterburner II
    let ab_active = eval_propulsion_module(&rifter, &ab2, true, false, false, &ctx.sde);
    assert!((ab_active.max_velocity - base_vel * 2.35).abs() < 1e-4, "AB boosts speed by 135%");
    assert_eq!(ab_active.mass, base_mass + 500_000.0, "AB adds 500,000 kg mass");
    assert_eq!(ab_active.signature_radius, base_sig, "AB does NOT bloom signature");
    let ab_scram = eval_propulsion_module(&rifter, &ab2, true, false, true, &ctx.sde);
    assert!(ab_scram.can_activate, "AB must be able to activate under warp scramble");

    // 3. Microwarpdrive: fit and activate 5MN Microwarpdrive I (434) via dogmaIM (feat:modules.microwarpdrive)
    let mwd1 = DogmaItemState::from_sde(mwd_item_id, 434, &ctx.sde); // 5MN Microwarpdrive I
    let mwd1_eval = eval_propulsion_module(&rifter, &mwd1, true, false, false, &ctx.sde);
    assert!(
        (mwd1_eval.max_velocity - base_vel * 6.0).abs() < 1e-3,
        "Base 5MN MWD I speedFactor (500%) must boost Rifter max_velocity by 6x"
    );
    let mwd2 = DogmaItemState::from_sde(2002, 440, &ctx.sde); // 5MN Microwarpdrive II
    let mwd_scram = eval_propulsion_module(&rifter, &mwd2, true, false, true, &ctx.sde);
    assert!(!mwd_scram.can_activate, "MWD CANNOT activate under warp scramble");

    let (_, pre_mwd_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(mwd_item_id)],
        &kw,
        None,
    )
    .await
    .expect("Activate 5MN Microwarpdrive I via dogmaIM.Activate");

    let (_, mwd_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let mwd_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(
        (mwd_cs.max_capacitor - base_cap * 0.75).abs() < 1e-3,
        "Fitting/activating 5MN MWD I must apply 0.75 capacitorCapacityMultiplier penalty (got {}, expected {})",
        mwd_cs.max_capacitor,
        base_cap * 0.75
    );
    assert!(
        mwd_ball.max_velocity >= pre_mwd_ball.max_velocity * 6.0 - 1e-3,
        "Active 5MN MWD I must boost ship max_velocity by at least +500% (6x): got {} vs pre {}",
        mwd_ball.max_velocity,
        pre_mwd_ball.max_velocity
    );
    assert!(
        (mwd_ball.mass - (base_mass + 500_000.0)).abs() < 1.0,
        "Active 5MN MWD I must add 500,000 kg mass: got {}",
        mwd_ball.mass
    );
    assert!(
        (mwd_cs.signature_radius - base_sig * 6.0).abs() < 1e-3,
        "Active 5MN MWD I must bloom ship signature radius by +500% (6x): got {}",
        mwd_cs.signature_radius
    );

    // Deactivate MWD and finish its cycle before navigation & MJD tests
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[PyRep::Integer(mwd_item_id)],
        &kw,
        None,
    )
    .await
    .expect("Deactivate MWD");
    for _ in 0..10 {
        sys_handle.step_manual(1.0).await.expect("step to finish MWD cycle");
    }

    // 4. Canonical V24 Beyonce Command Set: CmdSetSpeedFraction, CmdFollowBall, CmdOrbit, CmdAlignTo (feat:navigation.approach)
    let (_, cur_ship_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let nav_target_id = 900_001;
    let nav_target_pos = cur_ship_ball.position + DVec3::new(4_000.0, 0.0, 0.0);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: nav_target_id,
                type_id: 587,
                position: nav_target_pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await
        .expect("spawn nav target ball");

    let spd_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(cur_b_id),
            "CmdSetSpeedFraction",
            &[PyRep::Real(1.0)],
            &kw,
            None,
        )
        .await
        .expect("CmdSetSpeedFraction");
    assert!(matches!(spd_res, PyRep::Integer(_)));

    // 4a. CmdFollowBall with range 0.0 (Approach target): step ballpark and assert ship moves toward target
    let dist_initial = cur_ship_ball.position.distance(nav_target_pos);
    let follow_zero_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(cur_b_id),
            "CmdFollowBall",
            &[PyRep::Integer(nav_target_id), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .expect("CmdFollowBall(target, 0.0)");
    assert!(matches!(follow_zero_res, PyRep::Integer(_)));
    for _ in 0..8 {
        sys_handle.step_manual(1.0).await.expect("step approach via CmdFollowBall");
    }
    let (_, approached_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let dist_after_approach = approached_ball.position.distance(nav_target_pos);
    assert!(
        dist_after_approach < dist_initial - 500.0,
        "CmdFollowBall(target, 0.0) must move ship toward target across ticks (before={dist_initial:.1}, after={dist_after_approach:.1})"
    );

    // 4b. CmdFollowBall with range 1_500.0 (Keep at Range): step ballpark until ship converges near 1,500m
    let follow_range_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(cur_b_id),
            "CmdFollowBall",
            &[PyRep::Integer(nav_target_id), PyRep::Real(1_500.0)],
            &kw,
            None,
        )
        .await
        .expect("CmdFollowBall(target, 1500.0)");
    assert!(matches!(follow_range_res, PyRep::Integer(_)));
    for _ in 0..20 {
        sys_handle.step_manual(1.0).await.expect("step keep-at-range via CmdFollowBall");
    }
    let (_, range_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let dist_at_range = range_ball.position.distance(nav_target_pos);
    assert!(
        (dist_at_range - 1_500.0).abs() < 350.0,
        "CmdFollowBall(target, 1500.0) must converge ship distance near 1,500m (got {dist_at_range:.1})"
    );

    // 4c. CmdOrbit and CmdAlignTo
    let orbit_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(cur_b_id),
            "CmdOrbit",
            &[PyRep::Integer(nav_target_id), PyRep::Real(2_500.0)],
            &kw,
            None,
        )
        .await
        .expect("CmdOrbit");
    assert!(matches!(orbit_res, PyRep::Integer(_)));

    let align_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(cur_b_id),
            "CmdAlignTo",
            &[PyRep::Integer(60003760)],
            &kw,
            None,
        )
        .await
        .expect("CmdAlignTo");
    assert!(matches!(align_res, PyRep::Integer(_)));

    // 5. Micro Jump Drive via module fitting + dogmaIM.Activate + 12s spool-up (feat:modules.micro-jump-drive)
    // Switch hull to Apocalypse (642, Battleship) so Large Micro Jump Drive (4383) fits cleanly
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 642; // Apocalypse
            ship.location_id = 60003760;
        }
        if let Some(mwd) = snap.items.get_mut(&mwd_item_id) {
            mwd.flag = InventoryFlag::Cargo;
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60003760);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(mjd_item_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::MedSlot0 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit Large Micro Jump Drive to MedSlot0");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = sys_id as i64;
        }
    });
    sess.station_id = None;

    sys_handle
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 642,
                position: DVec3::new(200_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 115.0,
                inertia: 0.68,
                mass: 97_100_000.0,
                radius: 210.0,
                speed_fraction: 0.0,
                mode: MovementMode::GotoDirection { direction: DVec3::X },
            },
            ctx.build_ship_combat_state(char_id, ship_id, 642),
            ctx.build_ship_slim_meta(char_id, 642),
        )
        .await
        .expect("respawn ship as Apocalypse for Large MJD");

    let pos_before = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .unwrap()
        .1
        .position;
    let apoc_base_sig = sys_handle
        .get_combat_state(ship_id)
        .await
        .unwrap()
        .unwrap()
        .signature_radius;

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(mjd_item_id)],
        &kw,
        None,
    )
    .await
    .expect("Activate Large Micro Jump Drive via dogmaIM.Activate");

    // During spool-up (5s out of 12s SDE duration), ship must NOT have jumped 100km yet, and sig blooms +150%
    let spool_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(
        (spool_cs.signature_radius - apoc_base_sig * 2.5).abs() < 1e-2,
        "Spooling MJD must bloom signature radius by +150% (2.5x): got {}",
        spool_cs.signature_radius
    );
    for _ in 0..5 {
        sys_handle.step_manual(1.0).await.expect("step 5s of MJD spool-up");
    }
    let pos_mid_spool = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .unwrap()
        .1
        .position;
    assert!(
        pos_mid_spool.distance(pos_before) < 2_000.0,
        "MJD must NOT jump before its 12s SDE spool-up duration completes (moved {}m at t=5s)",
        pos_mid_spool.distance(pos_before)
    );

    // Step the remaining 7s to complete the 12s MJD spool cycle -> ship jumps 100,000 m along heading
    for _ in 0..7 {
        sys_handle.step_manual(1.0).await.expect("step remaining 7s of MJD spool-up");
    }
    let pos_after = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .unwrap()
        .1
        .position;
    let jump_dist = pos_after.distance(pos_before);
    assert!(
        (jump_dist - 100_000.0).abs() < 250.0,
        "Completing 12s MJD spool-up must translate ship 100,000 m along heading vector: got {jump_dist}"
    );
}

#[tokio::test]
async fn test_sp5_docking_and_undocking_station_structure_and_boarding() {
    // parity: plan:SP-5
    // parity: feat:ships.board-ship, feat:navigation.undock, feat:navigation.dock-station, feat:navigation.dock-structure
    eve_testkit::attest(&[
        "plan:SP-5",
        "feat:ships.board-ship",
        "feat:navigation.undock",
        "feat:navigation.dock-station",
        "feat:navigation.dock-structure",
    ]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(2003);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let first_ship_id = 100_000_001;
    let second_ship_id = 100_000_002;
    let station_id = 60003760;

    // Seed a second assembled ship (Caracal, type 621) in the station hangar owned by the pilot
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            second_ship_id,
            InventoryItemRecord {
                item_id: second_ship_id,
                type_id: 621, // Caracal
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
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
    });

    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    assert_eq!(sess.station_id, Some(station_id));
    assert_eq!(sess.ship_id, Some(first_ship_id));
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. Board a DIFFERENT ship in station hangar (feat:ships.board-ship):
    // Verify active ship ID changes in session + DB and emits OnSessionChanged + ProcessActiveShipChanged
    let board_res = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "Board",
            &[PyRep::Integer(second_ship_id)],
            &kw,
            None,
        )
        .await
        .expect("Board second ship in station");
    let PyRep::KeyVal(bkv) = board_res else {
        panic!("Expected KeyVal from ship.Board");
    };
    assert_eq!(bkv.get("shipID").unwrap().as_i64().unwrap(), second_ship_id);
    assert_eq!(bkv.get("oldShipID").unwrap().as_i64().unwrap(), first_ship_id);
    assert_eq!(sess.ship_id, Some(second_ship_id), "Session active ship_id must switch to second ship");

    let board_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        board_notifs.iter().any(|n| n.event == "OnSessionChanged"),
        "Boarding a different ship must emit OnSessionChanged notification"
    );
    assert!(
        board_notifs
            .iter()
            .any(|n| n.event == "ProcessActiveShipChanged"),
        "Boarding a different ship must emit ProcessActiveShipChanged notification"
    );

    // Switch back to first_ship_id (100_000_001) before undocking
    ctx.dispatch_rpc(
        &mut sess,
        "ship",
        None,
        "Board",
        &[PyRep::Integer(first_ship_id)],
        &kw,
        None,
    )
    .await
    .expect("Board first ship in station");
    assert_eq!(sess.ship_id, Some(first_ship_id));

    // 2. Undock from station along celestial-derived undock vector + 30s invulnerability (feat:navigation.undock)
    let undock_res = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(ukv) = undock_res else {
        panic!("Expected KeyVal from Undock");
    };
    assert_eq!(ukv.get("shipID").unwrap().as_i64().unwrap(), first_ship_id);
    assert_eq!(
        ukv.get("undockInvulnerabilityTicks").unwrap().as_i64().unwrap(),
        30,
        "Undock must grant 30 ticks (30s) of undock invulnerability"
    );
    assert_eq!(sess.station_id, None, "Pilot must be undocked in space");

    let sys_id = 30_000_142;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, ball) = sys_handle
        .get_ball_snapshot(first_ship_id)
        .await
        .unwrap()
        .unwrap();

    let st_rec = &ctx.sde.stations[&station_id];
    let station_pos = st_rec.position;
    let orbit_id = st_rec.orbit_id.expect("Jita 4-4 has orbit_id");
    let orbit_cel = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.celestial_id == orbit_id)
        .expect("orbit celestial in Jita");
    let expected_dir = (station_pos - orbit_cel.position).normalize();
    let st_radius = ctx.sde.types[&st_rec.station_type_id].radius;
    let ship_radius = ctx.sde.types[&ball.type_id].radius;
    let expected_offset = (st_radius * 0.015 + ship_radius).clamp(500.0, 2_000.0);
    let expected_spawn = station_pos + expected_dir * expected_offset;
    assert!(
        ball.position.distance(expected_spawn) < 1.0,
        "Undock spawn position must be derived from station celestial/type geometry"
    );
    assert_eq!(
        ball.mode,
        MovementMode::GotoDirection {
            direction: expected_dir
        },
        "Undocking ship must head along station celestial undock vector"
    );

    let undock_cs = sys_handle
        .get_combat_state(first_ship_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        undock_cs.undock_invulnerable_remaining_ticks, 30,
        "Spawned ship CombatState must have 30s undock invulnerability timer active"
    );

    // 3. Docking perimeter check: ship at spawn is within 2,500m docking perimeter
    // Move ship far away (50 km) -> docking must reject with OutsideDockingPerimeter
    sys_handle
        .set_ball_position(first_ship_id, station_pos + DVec3::new(50_000.0, 0.0, 0.0))
        .await;
    let far_dock = ctx
        .dispatch_rpc(
            &mut sess,
            "stationSvc",
            None,
            "Dock",
            &[PyRep::Integer(station_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        far_dock.unwrap_err().code,
        "OutsideDockingPerimeter",
        "Docking from 50 km must fail outside docking perimeter"
    );

    // Approach station to within docking perimeter (1,000 m <= 2,500 m) -> Dock succeeds (feat:navigation.dock-station)
    sys_handle
        .set_ball_position(first_ship_id, station_pos + DVec3::new(1_000.0, 0.0, 0.0))
        .await;
    let dock_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "stationSvc",
            None,
            "Dock",
            &[PyRep::Integer(station_id)],
            &kw,
            None,
        )
        .await
        .expect("Dock at station within docking perimeter");
    assert!(matches!(dock_ok, PyRep::KeyVal(_)));
    assert_eq!(sess.station_id, Some(station_id));

    // 4. Upwell structure docking (feat:navigation.dock-structure)
    // Undock again, create an Upwell structure in system, and dock there
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock again");

    let struct_id = 1_040_000_000_100_i64;
    let struct_pos = DVec3::new(200_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832, // Astrahus
                solar_system_id: sys_id,
                owner_corp_id: 1000001,
                name: "Astrahus Staging".into(),
                position: struct_pos.to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });

    // Move within structure docking perimeter (1,500 m)
    sys_handle
        .set_ball_position(100_000_001, struct_pos + DVec3::new(1_500.0, 0.0, 0.0))
        .await;
    let struct_dock = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDocking",
            None,
            "Dock",
            &[PyRep::Integer(struct_id)],
            &kw,
            None,
        )
        .await
        .expect("Dock at Upwell structure");
    assert!(matches!(struct_dock, PyRep::KeyVal(_)));
    assert_eq!(sess.structure_id, Some(struct_id));
}

#[tokio::test]
async fn test_s2_fighter_manager_load_launch_recall_movement_abilities_abandon() {
    // parity: plan:S-2
    // parity: feat:drones.fighter-load-tubes, feat:drones.fighter-launch-recall, feat:drones.fighter-movement, feat:drones.fighter-abilities, feat:drones.fighter-abandon
    eve_testkit::attest(&[
        "plan:S-2",
        "feat:drones.fighter-load-tubes",
        "feat:drones.fighter-launch-recall",
        "feat:drones.fighter-movement",
        "feat:drones.fighter-abilities",
        "feat:drones.fighter-abandon",
    ]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(2004);
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

    let ship_id = 100_000_001;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 23911; // Thanatos (Carrier with 4 fighter tubes)
        }
    });

    // Seed fighters into ship's FighterBay (flag 158):
    // fighter1: Dragonfly I (type 23057, squadron max size 6), quantity 15 -> tests stack splitting / clamping
    // fighter2: Firbolg I (type 23059, squadron max size 6), quantity 6
    // fighter3: Ametat I (type 40362, heavy fighter with ability 7 disallowed in low/high sec), quantity 3
    let fighter1_item_id = 70_000_001;
    let fighter2_item_id = 70_000_002;
    let fighter3_item_id = 70_000_003;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fighter1_item_id,
            InventoryItemRecord {
                item_id: fighter1_item_id,
                type_id: 23057, // Dragonfly I (Light Fighter, max size 6)
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::FighterBay,
                quantity: 15,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            fighter2_item_id,
            InventoryItemRecord {
                item_id: fighter2_item_id,
                type_id: 23059, // Firbolg I (Light Fighter, max size 6)
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::FighterBay,
                quantity: 6,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            fighter3_item_id,
            InventoryItemRecord {
                item_id: fighter3_item_id,
                type_id: 40362, // Ametat I (Heavy Fighter with ability 7)
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::FighterBay,
                quantity: 3,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    // 1. GetFightersForShip: verify fighters in bay
    let list_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "GetFightersForShip",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect("GetFightersForShip");
    let PyRep::Rowset { columns, .. } = list_res else {
        panic!("Expected rowset from GetFightersForShip");
    };
    assert_eq!(columns, vec!["itemID", "typeID", "squadronSize"]);

    // 2. Load fighter tubes (feat:drones.fighter-load-tubes)
    // 2a. Reject invalid tube index (tube 99 on 4-tube carrier)
    let err_invalid_tube = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LoadFightersToTube",
            &[PyRep::Integer(99), PyRep::Integer(fighter1_item_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_invalid_tube.code, "InvalidFighterTube");

    // 2b. Load tube 0: clamps quantity 15 to max squadron size 6 and splits remaining 9 into bay
    let load_res1 = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LoadFightersToTube",
            &[PyRep::Integer(0), PyRep::Integer(fighter1_item_id)],
            &kw,
            None,
        )
        .await
        .expect("Load tube 0");
    let PyRep::KeyVal(kv1) = load_res1 else {
        panic!("Expected KeyVal from LoadFightersToTube");
    };
    assert_eq!(kv1.get("fighterID").unwrap().as_i64().unwrap(), fighter1_item_id);
    assert_eq!(
        ctx.db.with_snapshot(|snap| snap.items[&fighter1_item_id].quantity),
        6,
        "Must clamp loaded item quantity to max squadron size 6"
    );
    let remaining_in_bay: i32 = ctx.db.with_snapshot(|snap| {
        snap.items.values()
            .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::FighterBay && i.type_id == 23057 && i.item_id != fighter1_item_id)
            .map(|i| i.quantity)
            .sum()
    });
    assert_eq!(remaining_in_bay, 9, "Excess fighters must remain in bay as split stack");

    // 2c. Reject loading same item already loaded
    let err_already_loaded = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LoadFightersToTube",
            &[PyRep::Integer(1), PyRep::Integer(fighter1_item_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_already_loaded.code, "FighterAlreadyLoaded");

    // 2d. Load tube 1 and tube 2
    let load_res2 = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LoadFightersToTube",
            &[PyRep::Integer(1), PyRep::Integer(fighter2_item_id)],
            &kw,
            None,
        )
        .await
        .expect("Load tube 1");
    assert!(matches!(load_res2, PyRep::KeyVal(_)));

    let load_res3 = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LoadFightersToTube",
            &[PyRep::Integer(2), PyRep::Integer(fighter3_item_id)],
            &kw,
            None,
        )
        .await
        .expect("Load tube 2");
    assert!(matches!(load_res3, PyRep::KeyVal(_)));

    // 3. Undock and launch fighters into space (feat:drones.fighter-launch-recall)
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    // 3a. Reject launching empty tube 3
    let err_empty_tube = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(3)])],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_empty_tube.code, "EmptyFighterTube");

    // 3b. Launch loaded tubes 0, 1, 2
    let launch_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(0), PyRep::Integer(1), PyRep::Integer(2)])],
            &kw,
            None,
        )
        .await
        .expect("Launch fighters");
    let PyRep::List(launched_balls) = launch_res else {
        panic!("Expected List from LaunchFightersFromTubes");
    };
    assert_eq!(launched_balls.len(), 3, "All 3 loaded tubes must launch");
    let f1_ball_id = launched_balls[0].as_i64().unwrap();
    let f2_ball_id = launched_balls[1].as_i64().unwrap();
    let f3_ball_id = launched_balls[2].as_i64().unwrap();

    let sys_id = 30_000_142;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    assert!(
        sys_handle.get_ball_snapshot(f1_ball_id).await.unwrap().is_some(),
        "Fighter 1 must exist as ball in ballpark"
    );

    // 3c. Reject re-launching already launched tube
    let err_already_launched = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(0)])],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_already_launched.code, "FighterAlreadyLaunched");

    // 3d. Reject unloading a launched tube
    let err_unload_launched = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "UnloadTubeToFighterBay",
            &[PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_unload_launched.code, "FighterAlreadyLaunched");

    // 4. Fighter Movement (feat:drones.fighter-movement)
    let move_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "ExecuteMovementCommandOnFighters",
            &[
                PyRep::List(vec![PyRep::Integer(f1_ball_id)]),
                PyRep::String("GOTO_POINT".into()),
                PyRep::Real(10_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("ExecuteMovementCommandOnFighters GOTO_POINT");
    assert!(matches!(move_res, PyRep::Bool(true)));

    let orbit_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "ExecuteMovementCommandOnFighters",
            &[
                PyRep::List(vec![PyRep::Integer(f2_ball_id)]),
                PyRep::String("ORBIT".into()),
                PyRep::Integer(ship_id),
                PyRep::Real(2_000.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("ExecuteMovementCommandOnFighters ORBIT");
    assert!(matches!(orbit_res, PyRep::Bool(true)));

    // 5. Fighter Abilities (feat:drones.fighter-abilities)
    let (_, f1_ball) = sys_handle
        .get_ball_snapshot(f1_ball_id)
        .await
        .unwrap()
        .unwrap();
    let dummy_target_id = 99_001;
    let dummy_ball = Ball {
        id: dummy_target_id,
        type_id: 282,
        position: f1_ball.position + DVec3::new(1_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1_000.0,
        radius: 10.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    sys_handle
        .spawn_ball_with_combat(dummy_ball, ctx.build_type_combat_state(282))
        .await
        .expect("spawn dummy");
    sys_handle
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: dummy_target_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(100_000.0),
            },
        )
        .await
        .expect("lock dummy target");

    // 5a. Error: FighterNotOwned when called from unowning pilot
    let mut sess_other = SessionState::new(2005);
    sess_other.user_id = Some(1002);
    sess_other.character_id = Some(90_000_999);
    sess_other.solar_system_id = Some(sys_id);
    sess_other.ship_id = Some(100_000_999);
    let err_not_owned = ctx
        .dispatch_rpc(
            &mut sess_other,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[
                PyRep::List(vec![PyRep::Integer(f1_ball_id)]),
                PyRep::Integer(0),
                PyRep::Integer(dummy_target_id),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_not_owned.code, "FighterNotOwned");

    // 5b. Error: TargetOutOfRange when target is far beyond ability optimal + falloff
    let far_target_id = 99_002;
    let far_ball = Ball {
        id: far_target_id,
        type_id: 282,
        position: f1_ball.position + DVec3::new(200_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1_000.0,
        radius: 10.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    sys_handle
        .spawn_ball_with_combat(far_ball, ctx.build_type_combat_state(282))
        .await
        .expect("spawn far dummy");
    sys_handle
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: far_target_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(300_000.0),
            },
        )
        .await
        .expect("lock far dummy target");
    let err_out_of_range = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[
                PyRep::List(vec![PyRep::Integer(f1_ball_id)]),
                PyRep::Integer(0),
                PyRep::Integer(far_target_id),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_out_of_range.code, "TargetOutOfRange");

    // 5c. Primary attack deals damage
    let sec_before_npc = ctx.db.with_snapshot(|s| s.characters[&90_000_001].security_status);
    let cs_before = sys_handle.get_combat_state(dummy_target_id).await.unwrap().unwrap();
    let ability_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[
                PyRep::List(vec![PyRep::Integer(f1_ball_id)]),
                PyRep::Integer(0),
                PyRep::Integer(dummy_target_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("CmdActivateAbilitySlots Slot 0");
    assert!(matches!(ability_res, PyRep::KeyVal(_)));
    let cs_after = sys_handle.get_combat_state(dummy_target_id).await.unwrap().unwrap();
    assert!(
        cs_after.shield_hp < cs_before.shield_hp,
        "Fighter slot 0 attack must deal damage to target"
    );

    // Verify NPC attack in high-sec triggers weapon timer but not criminal/suspect/CONCORD or sec loss
    let now_npc = ctx.now_unix();
    {
        let cw = ctx.crimewatch.read().unwrap();
        let att_st = cw.states.get(&90_000_001).expect("attacker crimewatch state");
        assert!(att_st.is_weapon_timer_active(now_npc), "Attacker weapon timer must be active");
        assert!(!att_st.is_criminal(now_npc), "NPC attack must not flag criminal");
        assert!(!att_st.is_suspect(now_npc), "NPC attack must not flag suspect");
        assert!(att_st.pending_concord_arrival.is_none(), "NPC attack must not schedule CONCORD arrival");
    }
    let sec_after_npc = ctx.db.with_snapshot(|s| s.characters[&90_000_001].security_status);
    assert_eq!(sec_after_npc, sec_before_npc, "Attacking NPC dummy must not penalize security status");

    // 5d. Error: AbilityOnCooldown on rapid re-activation
    let err_cooldown = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[
                PyRep::List(vec![PyRep::Integer(f1_ball_id)]),
                PyRep::Integer(0),
                PyRep::Integer(dummy_target_id),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_cooldown.code, "AbilityOnCooldown");

    // 5e. Slot 1: MWD / Boost (untargeted propulsion dispatch)
    let mwd_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[PyRep::List(vec![PyRep::Integer(f1_ball_id)]), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("CmdActivateAbilitySlots Slot 1 MWD");
    assert!(matches!(mwd_res, PyRep::KeyVal(_)));
    assert!(
        ctx.active_fighters.read().unwrap()[&f1_ball_id].mwd_active,
        "Fighter 1 MWD must be marked active"
    );

    // 5f. Disallowed abilities in high-sec / low-sec:
    // Ametat I (f3_ball_id) slot 2 has ability 7 which has disallowInHighSec=true and disallowInLowSec=true
    let err_highsec = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[PyRep::List(vec![PyRep::Integer(f3_ball_id)]), PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_highsec.code, "AbilityDisallowedInHighSec");

    // Test in low-sec system (Asabona: 30000012, sec 0.32)
    let lowsec_sys = 30_000_012;
    let lowsec_handle = ctx.systems.get_or_spawn(lowsec_sys);
    let (_, f3_ball) = sys_handle
        .get_ball_snapshot(f3_ball_id)
        .await
        .unwrap()
        .unwrap();
    let _ = lowsec_handle.spawn_ball_with_combat(f3_ball, ctx.build_type_combat_state(40362)).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = lowsec_sys;
    });
    sess.solar_system_id = Some(lowsec_sys);
    let err_lowsec = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[PyRep::List(vec![PyRep::Integer(f3_ball_id)]), PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_lowsec.code, "AbilityDisallowedInLowSec");
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = sys_id;
    });
    sess.solar_system_id = Some(sys_id);

    // 5g. CrimeWatch weapon timer and aggression on fighter attack:
    // Seed a player victim character (90_000_002) and ship (100_000_002)
    let victim_char_id = 90_000_002;
    let victim_ship_id = 100_000_002;
    ctx.db.with_mut_snapshot(|snap| {
        let mut vic_ch = snap.characters[&90_000_001].clone();
        vic_ch.character_id = victim_char_id;
        vic_ch.name = "Victim Pilot".to_string();
        vic_ch.security_status = 5.0;
        vic_ch.active_ship_id = victim_ship_id;
        snap.characters.insert(victim_char_id, vic_ch);
        snap.items.insert(
            victim_ship_id,
            InventoryItemRecord {
                item_id: victim_ship_id,
                type_id: 587,
                owner_id: victim_char_id,
                location_id: 60003760,
                flag: InventoryFlag::Hangar,
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
    });
    let (_, f2_ball) = sys_handle
        .get_ball_snapshot(f2_ball_id)
        .await
        .unwrap()
        .unwrap();
    let victim_ball = Ball {
        id: victim_ship_id,
        type_id: 587,
        position: f2_ball.position + DVec3::new(1_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1_000.0,
        radius: 30.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    sys_handle
        .spawn_ball_with_combat(victim_ball, ctx.build_type_combat_state(587))
        .await
        .expect("spawn victim ship");
    sys_handle
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: victim_ship_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(100_000.0),
            },
        )
        .await
        .expect("lock victim ship");

    let sec_before = ctx.db.with_snapshot(|s| s.characters[&90_000_001].security_status);
    let pvp_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[
                PyRep::List(vec![PyRep::Integer(f2_ball_id)]),
                PyRep::Integer(0),
                PyRep::Integer(victim_ship_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("CmdActivateAbilitySlots Slot 0 PvP attack");
    assert!(matches!(pvp_res, PyRep::KeyVal(_)));

    let now = ctx.now_unix();
    {
        let cw = ctx.crimewatch.read().unwrap();
        let att_st = cw.states.get(&90_000_001).expect("attacker crimewatch state");
        assert!(att_st.is_weapon_timer_active(now), "Attacker weapon timer must be active");
        assert!(att_st.is_criminal(now), "Unprovoked high-sec attack must flag attacker criminal");
        assert!(att_st.pending_concord_arrival.is_some(), "CONCORD arrival must be scheduled");
        assert!(
            cw.aggression_mgr.is_aggressor_against(90_000_001, victim_char_id, now),
            "Aggression against victim must be recorded"
        );
    }
    let sec_after = ctx.db.with_snapshot(|s| s.characters[&90_000_001].security_status);
    assert!(sec_after < sec_before, "Attacker security status must be penalized");

    // 6. Abandon fighters (feat:drones.fighter-abandon)
    let abandon_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdAbandonFighter",
            &[PyRep::Integer(f1_ball_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdAbandonFighter");
    let PyRep::List(ab_list) = abandon_res else {
        panic!("Expected List from CmdAbandonFighter");
    };
    assert!(!ab_list.is_empty(), "Fighter must be abandoned in space");
    assert!(
        ctx.active_fighters.read().unwrap()[&f1_ball_id].is_abandoned,
        "Fighter 1 must be marked abandoned"
    );
    // Reloading an abandoned fighter must fail with ItemNotInFighterBay
    let err_load_abandoned = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LoadFightersToTube",
            &[PyRep::Integer(0), PyRep::Integer(fighter1_item_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_load_abandoned.code, "ItemNotInFighterBay");

    // 7. Recall fighters (feat:drones.fighter-launch-recall)
    // Recall active fighters (f2 and f3) back to ship tubes
    let recall_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "RecallFightersToTubes",
            &[PyRep::List(vec![PyRep::Integer(f2_ball_id), PyRep::Integer(f3_ball_id)])],
            &kw,
            None,
        )
        .await
        .expect("RecallFightersToTubes");
    let PyRep::List(rec_list) = recall_res else {
        panic!("Expected List from RecallFightersToTubes");
    };
    assert_eq!(rec_list.len(), 2, "Both active fighters must be recalled from space");
    assert!(
        sys_handle.get_ball_snapshot(f2_ball_id).await.unwrap().is_none(),
        "Recalled fighter ball must be removed from ballpark"
    );
    // Recalled fighters are restored in fighter_tube_loads
    assert!(
        ctx.fighter_tube_loads.read().unwrap().contains_key(&(ship_id, 1)),
        "Tube 1 must be re-loaded upon recall"
    );
    assert!(
        ctx.fighter_tube_loads.read().unwrap().contains_key(&(ship_id, 2)),
        "Tube 2 must be re-loaded upon recall"
    );
}

#[tokio::test]
async fn test_s3_pharolux_cyno_beacon_jump_bridge_and_group_jump() {
    // parity: plan:S-3
    // parity: feat:navigation.pharolux-beacon
    eve_testkit::attest(&[
        "plan:S-3",
        "feat:navigation.pharolux-beacon",
    ]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(2005);
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

    let ship_id = 100_000_001;
    let pilot_corp_id = sess.corporation_id.unwrap();

    // Seed Pharolux Cyno Beacon (type 35840) in low-sec Ohkunen (30_000_204, security 0.38, 4.80 LY from Jita)
    let dest_sys_id = 30_000_204;
    let beacon_struct_id = 1_040_000_000_200_i64;
    let beacon_pos = DVec3::new(450_000.0, 0.0, 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            beacon_struct_id,
            UpwellStructureRecord {
                structure_id: beacon_struct_id,
                type_id: 35840, // Pharolux Cyno Beacon
                solar_system_id: dest_sys_id,
                owner_corp_id: pilot_corp_id,
                name: "Ohkunen Pharolux Beacon".into(),
                position: beacon_pos.to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                shield_hp: 50_000.0,
                armor_hp: 50_000.0,
                hull_hp: 50_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );

        // Put capital jump fuel (Helium Isotopes) in ship's SpecializedFuelBay
        snap.items.insert(
            88_000_001,
            InventoryItemRecord {
                item_id: 88_000_001,
                type_id: 16274, // Helium Isotopes
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 10_000,
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

    // 1. structureCynoBeaconMgr.GetMyAccessibleOnlineCynoBeaconStructures: lists online beacons for character's corp
    let fleet_beacons = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "GetMyAccessibleOnlineCynoBeaconStructures",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetMyAccessibleOnlineCynoBeaconStructures");
    let PyRep::List(blist) = fleet_beacons else {
        panic!("Expected List from GetMyAccessibleOnlineCynoBeaconStructures");
    };
    assert!(
        blist.iter().any(|b| {
            if let PyRep::KeyVal(d) = b {
                d.get("structureID").and_then(|v| v.as_i64()) == Some(beacon_struct_id)
            } else {
                false
            }
        }),
        "GetMyAccessibleOnlineCynoBeaconStructures must contain the seeded Pharolux beacon"
    );

    // 2. Undock into space in origin system
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let _origin_sys = sess.solar_system_id.unwrap();

    // 3. Jump to Pharolux Cyno Beacon (feat:navigation.pharolux-beacon)
    let jump_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "CmdJumpToStructureBeacon",
            &[
                PyRep::Integer(dest_sys_id as i64),
                PyRep::Integer(beacon_struct_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("CmdJumpToStructureBeacon");
    let PyRep::KeyVal(jkv) = jump_res else {
        panic!("Expected KeyVal from CmdJumpToStructureBeacon");
    };
    assert_eq!(
        jkv.get("solarSystemID").unwrap().as_i64().unwrap(),
        dest_sys_id as i64,
        "Target solar system must match Ohkunen"
    );
    assert!(jkv.get("jumpFatigueMinutes").unwrap().as_f64().unwrap() > 0.0);
    assert!(jkv.get("jumpCooldownMinutes").unwrap().as_f64().unwrap() > 0.0);
    assert_eq!(sess.solar_system_id, Some(dest_sys_id));

    // Verify ship spawned in Ohkunen near beacon
    let dest_handle = ctx.systems.get_or_spawn(dest_sys_id);
    let (_, dest_ball) = dest_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("Ship spawned in destination system");
    let dist_to_beacon = dest_ball.position.distance(beacon_pos);
    assert!(
        dist_to_beacon <= 10_000.0,
        "Ship must land within 10 km of beacon: got {dist_to_beacon}"
    );

    // Verify fuel was consumed from ship
    let rem_fuel = ctx.db.with_snapshot(|snap| {
        snap.items.get(&88_000_001).map(|i| i.quantity).unwrap_or(0)
    });
    assert!(rem_fuel < 10_000, "Jump must consume isotope fuel");

    // 4. Bridge to structure beacon
    let bridge_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "CmdBridgeToStructureBeacon",
            &[
                PyRep::Integer(dest_sys_id as i64),
                PyRep::Integer(beacon_struct_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("CmdBridgeToStructureBeacon");
    let PyRep::KeyVal(b_kv) = bridge_res else {
        panic!("Expected KeyVal from CmdBridgeToStructureBeacon");
    };
    assert_eq!(b_kv.get("bridgeActive").unwrap(), &PyRep::Bool(true));

    // 5. Group jump to structure beacon
    let group_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "CmdGroupJumpToStructureBeacon",
            &[
                PyRep::Integer(dest_sys_id as i64),
                PyRep::Integer(beacon_struct_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("CmdGroupJumpToStructureBeacon");
    let PyRep::KeyVal(g_kv) = group_res else {
        panic!("Expected KeyVal from CmdGroupJumpToStructureBeacon");
    };
    assert_eq!(g_kv.get("groupJumpActive").unwrap(), &PyRep::Bool(true));

    // 6. Error validation: High-sec beacon jump rejected
    let highsec_beacon_id = 1_040_000_000_201_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            highsec_beacon_id,
            UpwellStructureRecord {
                structure_id: highsec_beacon_id,
                type_id: 35840,
                solar_system_id: 30000142, // Jita 0.95 high-sec
                owner_corp_id: pilot_corp_id,
                name: "Jita HighSec Beacon".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                shield_hp: 50_000.0,
                armor_hp: 50_000.0,
                hull_hp: 50_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });

    let hs_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "CmdJumpToStructureBeacon",
            &[
                PyRep::Integer(30000142),
                PyRep::Integer(highsec_beacon_id),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        hs_err.unwrap_err().code,
        "CannotJumpToHighSecBeacon",
        "Jumping to high-sec cyno beacon must be rejected"
    );
}
