//! Integration tests for `SP-2` (Grid Bubble Splitting, Interdiction Spheres, WDFG, Mobile Warp Disruptors, & Interdiction Nullifiers)
//! and `SP-3` (Propulsion Modules, Agility, MJD/MJFG/Capital MJD, Cloaking, & Subwarp Navigation).

use crate::common;

use eve_db::types::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, Ballpark, MovementMode};
use eve_dogma::DogmaItemState;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
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
    }
}

fn grant_all_test_skills(ctx: &ServerContext, char_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        for &sid in &[
            3402_u32, 3413, 3426, 3435, 3449, 3450, 3454, 3455, 4385, 11446, 11579, 11584, 28609,
        ] {
            sk.insert(
                sid,
                TrainedSkillRecord {
                    type_id: sid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });
}

#[tokio::test]
async fn test_sp2_grid_bubble_splitting_interdiction_and_nullifier() {
    // parity: plan:SP-2
    // parity: feat:navigation.bubbles-grid, feat:modules.interdiction-sphere, feat:modules.warp-disruption-field, feat:modules.mobile-warp-disruptor, feat:modules.interdiction-nullifier, feat:modules.mobile-warp-disruptor-anchoring-skill, rpc:beyonce.AdminGetBubbleID, rpc:beyonce.CmdWarpToStuff, rpc:charUnboundMgr.SelectCharacterID, rpc:dogmaIM.Activate, rpc:dogmaIM.Deactivate, rpc:posMgr.AnchorStructure, rpc:stationSvc.Undock
    eve_testkit::attest(&[
        "plan:SP-2",
        "feat:navigation.bubbles-grid",
        "feat:modules.interdiction-sphere",
        "feat:modules.warp-disruption-field",
        "feat:modules.mobile-warp-disruptor",
        "feat:modules.interdiction-nullifier",
        "feat:modules.mobile-warp-disruptor-anchoring-skill",
    ]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(2001);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
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

    sess.role = u64::MAX;
    let ship_id = 100_000_001;
    let launcher_item_id = 60_000_101;
    let charge_item_id = 60_000_102;
    let wdfg_item_id = 60_000_103;
    let nullifier_item_id = 60_000_104;
    let mobile_disruptor_item_id = 60_000_105;

    grant_all_test_skills(&ctx, char_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_id);
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 22456; // Sabre (Interdictor)
        }
        for item in [
            make_item(launcher_item_id, 22782, char_id, ship_id, InventoryFlag::HiSlot0, 1, true),
            make_item(charge_item_id, 22778, char_id, ship_id, InventoryFlag::HiSlot0, 5, false),
            make_item(mobile_disruptor_item_id, 12198, char_id, ship_id, InventoryFlag::Cargo, 5, false),
        ] {
            snap.items.insert(item.item_id, item);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    // 1. Undock in Jita (30000142, high-sec 0.95): verify grid bubble partitioning (`feat:navigation.bubbles-grid`)
    let undock_res = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(ukv) = undock_res else {
        panic!("Expected KeyVal from Undock");
    };
    let jita_beyonce_id = ukv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    let jita_sys_id = 30_000_142;
    let jita_handle = ctx.systems.get_or_spawn(jita_sys_id);

    let near_npc_id = 88_001;
    let far_npc_id = 88_002;
    let (_, ship_ball) = jita_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    for (id, offset) in [(near_npc_id, 5_000.0), (far_npc_id, 50_000_000.0)] {
        jita_handle
            .spawn_ball_with_combat(
                Ball {
                    id,
                    type_id: 587,
                    position: ship_ball.position + DVec3::new(offset, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 3.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(587),
            )
            .await
            .expect("spawn npc");
    }

    let ship_bubble = ctx
        .dispatch_rpc(&mut sess, "beyonce", Some(jita_beyonce_id), "AdminGetBubbleID", &[PyRep::Integer(ship_id)], &kw, None)
        .await.expect("AdminGetBubbleID ship").as_i64().unwrap();
    let near_bubble = ctx
        .dispatch_rpc(&mut sess, "beyonce", Some(jita_beyonce_id), "AdminGetBubbleID", &[PyRep::Integer(near_npc_id)], &kw, None)
        .await.expect("AdminGetBubbleID near").as_i64().unwrap();
    assert_eq!(ship_bubble, near_bubble);

    let far_bubble_err = ctx
        .dispatch_rpc(&mut sess, "beyonce", Some(jita_beyonce_id), "AdminGetBubbleID", &[PyRep::Integer(far_npc_id)], &kw, None)
        .await.expect_err("AdminGetBubbleID must reject balls outside caller bubble");
    assert_eq!(far_bubble_err.code, "BallNotVisible");
    let far_bubble_actor = jita_handle.get_bubble_id_for_ball(far_npc_id).await.unwrap().unwrap() as i64;
    assert_ne!(ship_bubble, far_bubble_actor);

    let visible = jita_handle.list_balls_in_bubble(ship_id).await.unwrap();
    assert!(visible.iter().any(|b| b.id == near_npc_id));
    assert!(!visible.iter().any(|b| b.id == far_npc_id));

    // 1b. Security-class restrictions in High-Sec (Jita 0.95) and Low-Sec (Tama 30002813, 0.28)
    for (sec_sys_id, _) in [(jita_sys_id, "high-sec"), (30_002_813_u32, "low-sec")] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().solar_system_id = sec_sys_id;
            snap.items.get_mut(&ship_id).unwrap().location_id = sec_sys_id as i64;
        });
        sess.solar_system_id = Some(sec_sys_id);
        let probe_err = ctx
            .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(launcher_item_id)], &kw, None)
            .await.unwrap_err();
        assert_eq!(probe_err.code, "CannotLaunchInterdictionProbeInEmpire");
        let anchor_err = ctx
            .dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mobile_disruptor_item_id)], &kw, None)
            .await.unwrap_err();
        assert_eq!(anchor_err.code, "CantAnchorInEmpire");
    }

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
    null_handle
        .spawn_ball_with_meta(
            Ball {
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
            },
            ctx.build_ship_combat_state(char_id, ship_id, 22456),
            ctx.build_ship_slim_meta(char_id, 22456),
        )
        .await
        .expect("spawn Sabre in null-sec");

    // 2a. Launch Interdiction Sphere Probe Variants (`feat:modules.interdiction-sphere`, `feat:modules.interdiction-probe-variants`)
    // - 22778 Warp Disrupt Probe (20 km radius)
    // - 34260 Surgical Warp Disrupt Probe (10 km radius)
    // - 41402 Phobetor Warp Disrupt Probe (20 km radius)
    let mut first_probe_bubble_id = 0_i64;
    for (idx, probe_type_id) in [22778_u32, 34260, 41402].into_iter().enumerate() {
        ctx.db.with_mut_snapshot(|snap| {
            let c = snap.items.get_mut(&charge_item_id).unwrap();
            c.type_id = probe_type_id;
            c.quantity = 5;
        });
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
            .expect("Launch interdiction probe variant");
        let PyRep::KeyVal(lkv) = launch_res else {
            panic!("Expected KeyVal from Interdiction Sphere Launcher activation");
        };
        let b_id = lkv.get("probeID").unwrap().as_i64().unwrap();
        let b_rad = lkv.get("bubbleRadius").unwrap().as_f64().unwrap();
        assert!(b_rad >= 10_000.0, "Probe variant {probe_type_id} must spawn warp bubble (radius={b_rad})");
        if idx == 0 {
            first_probe_bubble_id = b_id;
            assert_eq!(b_rad, 20_000.0);
        } else {
            null_handle.remove_warp_bubble(b_id).await.unwrap();
        }
    }

    // 2b. Activate Warp Disruption Field Generator I (`28654`, `feat:modules.warp-disruption-field`) on Heavy Interdiction Cruiser (Onyx `11995`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 11995; // Onyx (HIC, group 894)
        snap.items.insert(
            wdfg_item_id,
            make_item(wdfg_item_id, 28654, char_id, ship_id, InventoryFlag::HiSlot1, 1, true),
        );
    });
    ctx.invalidate_ship_fitting(ship_id);
    let wdfg_pos = DVec3::new(30_000_000.0, 0.0, 0.0);
    null_handle.set_ball_position(ship_id, wdfg_pos).await;
    let wdfg_res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(wdfg_item_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate WDFG in null-sec");
    let PyRep::KeyVal(wkv) = wdfg_res else {
        panic!("Expected KeyVal from WDFG activation");
    };
    assert_eq!(wkv.get("bubbleRadius").unwrap().as_f64().unwrap(), 16_000.0);
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[PyRep::Integer(wdfg_item_id)],
        &kw,
        None,
    )
    .await
    .expect("Deactivate WDFG");

    // 2c. Mobile Warp Disruptor Anchoring Skill requirements (`feat:modules.mobile-warp-disruptor`, `feat:modules.mobile-warp-disruptor-anchoring-skill`)
    // Test Anchoring skill (11584):
    // - Level 0 -> rejected with SkillMissing
    // - Level 2 -> allows Small I (12198, req lvl 2), rejects Medium II (26890, req lvl 4)
    // - Level 4 -> allows all variants
    let mwd_pos = DVec3::new(35_000_000.0, 0.0, 0.0);
    null_handle.set_ball_position(ship_id, mwd_pos).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sk) = snap.skills.get_mut(&char_id) {
            sk.remove(&11584);
        }
    });
    let no_skill_err = ctx
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
        .unwrap_err();
    assert_eq!(no_skill_err.code, "SkillMissing");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            11584,
            TrainedSkillRecord {
                type_id: 11584,
                level: 2,
                skill_points: 1_415.0,
            },
        );
        snap.items.get_mut(&mobile_disruptor_item_id).unwrap().type_id = 26890; // Mobile Medium Warp Disruptor II (requires Anchoring IV)
    });
    let t2_skill_err = ctx
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
        .unwrap_err();
    assert_eq!(t2_skill_err.code, "SkillMissing");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.get_mut(&char_id).unwrap().get_mut(&11584).unwrap().level = 4;
        snap.items.get_mut(&mobile_disruptor_item_id).unwrap().type_id = 12198;
    });
    let anchor_res = ctx
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
        .expect("Anchor Mobile Small Warp Disruptor I in null-sec");
    let PyRep::KeyVal(akv) = anchor_res else {
        panic!("Expected KeyVal from AnchorStructure");
    };
    let mobile_bubble_id = akv.get("itemID").unwrap().as_i64().unwrap();
    assert_eq!(akv.get("bubbleRadius").unwrap().as_f64().unwrap(), 5_000.0);

    // 3. Verify non-nullified warp is dragged/stopped by bubble, and Interdiction Nullifier (`58919`, `feat:modules.interdiction-nullifier`) bypasses bubbles
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_id);
        snap.items.get_mut(&ship_id).unwrap().type_id = 11198; // Stiletto (Interceptor, group 831)
        snap.items.insert(
            nullifier_item_id,
            make_item(nullifier_item_id, 58919, char_id, ship_id, InventoryFlag::HiSlot0, 1, true),
        );
    });
    ctx.invalidate_ship_fitting(ship_id);
    let warp_start = DVec3::new(15_000_000.0, 0.0, 0.0);
    let warp_dest_cel_id = 88_999;
    let warp_dest_pos = DVec3::new(40_000_000.0, 0.0, 0.0);
    null_handle
        .spawn_ball(Ball {
            id: warp_dest_cel_id,
            type_id: 15,
            position: warp_dest_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e12,
            radius: 5000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await
        .expect("spawn rigid beacon at 40,000 km");
    null_handle.set_ball_position(ship_id, warp_start).await;

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(null_beyonce_id),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(warp_dest_cel_id)],
        &kw,
        None,
    )
    .await
    .expect("CmdWarpToStuff without nullifier");

    let (_, caught_ball) = null_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let MovementMode::Warp { destination: caught_dest, .. } = caught_ball.mode else {
        panic!("Expected ship in Warp mode");
    };
    assert!(
        caught_dest.x < 36_000_000.0,
        "Un-nullified warp must be intercepted by interdiction bubble before 40,000 km: got {}",
        caught_dest.x
    );

    // Reset ship inside bubble at 25,000 km -> cannot warp without nullifier, CAN warp once nullifier activated
    let (_, mut reset_ball) = null_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    reset_ball.position = sphere_bubble_pos;
    reset_ball.mode = MovementMode::Stop;
    null_handle.spawn_ball(reset_ball.clone()).await.unwrap();

    let trapped_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(null_beyonce_id),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(warp_dest_cel_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(trapped_err.code, "WarpScrambled");

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

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(null_beyonce_id),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(warp_dest_cel_id)],
        &kw,
        None,
    )
    .await
    .expect("Nullified ship must warp out of bubble and bypass in-line bubbles");

    let (_, nullified_ball) = null_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let MovementMode::Warp { destination: nullified_dest, .. } = nullified_ball.mode else {
        panic!("Expected ship in Warp mode");
    };
    assert!((nullified_dest.x - 40_000_000.0).abs() < 1.0);

    let _ = null_handle.remove_warp_bubble(first_probe_bubble_id).await;
    let _ = null_handle.remove_warp_bubble(mobile_bubble_id).await;
}

#[tokio::test]
async fn test_sp3_propulsion_agility_mjd_mjfg_and_approach() {
    // parity: plan:SP-3
    // parity: feat:navigation.approach, feat:dogma.velocity-agility, feat:modules.afterburner, feat:modules.microwarpdrive, feat:modules.micro-jump-drive, feat:modules.micro-jump-field-generator, feat:modules.micro-jump-field-boosh-limit, feat:modules.mjfg-fleet-jump, feat:modules.capital-micro-jump-drive, feat:navigation.directional-jump-drive, rpc:beyonce.CmdFollowBall, rpc:beyonce.CmdGotoDirection, rpc:beyonce.CmdSetSpeedFraction, rpc:charUnboundMgr.SelectCharacterID, rpc:dogmaIM.Activate, rpc:dogmaIM.Deactivate, rpc:stationSvc.Undock
    eve_testkit::attest(&[
        "plan:SP-3",
        "feat:navigation.approach",
        "feat:dogma.velocity-agility",
        "feat:modules.afterburner",
        "feat:modules.microwarpdrive",
        "feat:modules.micro-jump-drive",
        "feat:modules.micro-jump-field-generator",
        "feat:modules.micro-jump-field-boosh-limit",
        "feat:modules.mjfg-fleet-jump",
        "feat:modules.capital-micro-jump-drive",
        "feat:navigation.directional-jump-drive",
    ]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(2002);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
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

    let ab_item_id = 60_000_201;
    let mwd_item_id = 60_000_202;
    let mjd_item_id = 60_000_203;
    grant_all_test_skills(&ctx, char_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_id);
        snap.items.get_mut(&ship_id).unwrap().type_id = 638; // Raven (Battleship, group 27)
        for item in [
            make_item(ab_item_id, 439, char_id, ship_id, InventoryFlag::MedSlot0, 1, true),
            make_item(mwd_item_id, 440, char_id, ship_id, InventoryFlag::MedSlot1, 1, true),
            make_item(mjd_item_id, 4383, char_id, ship_id, InventoryFlag::MedSlot2, 1, true),
        ] {
            snap.items.insert(item.item_id, item);
        }
        snap.module_online_states.insert(mwd_item_id, false);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let undock_res = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(ukv) = undock_res else {
        panic!("Expected KeyVal from Undock");
    };
    let beyonce_id = ukv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    let sys_id = 30_000_142;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // 1. Subwarp Velocity & Agility (`feat:dogma.velocity-agility`)
    let (_, base_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let base_vmax = base_ball.max_velocity;
    let base_mass = base_ball.mass;
    let base_inertia = base_ball.inertia;
    let base_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    let base_sig = base_cs.signature_radius;

    let fit_eval = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .unwrap()
        .evaluated_context(&ctx.sde);
    assert!(
        (base_vmax - fit_eval.ship.get_attribute(eve_sde::attr_ids::MAX_VELOCITY, &ctx.sde)).abs() < 1e-3
    );
    let _hull_dogma = DogmaItemState::from_sde(ship_id, base_ball.type_id, &ctx.sde);
    let align_time = (4.0_f64.ln() * base_inertia * base_mass) / 1_000_000.0;
    assert!(align_time > 1.0 && align_time < 30.0);

    // 2. Approach (`feat:navigation.approach`)
    let target_ball_id = 89_001;
    let (_, cur_ship) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let target_pos = cur_ship.position + DVec3::new(15_000.0, 0.0, 0.0);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: target_ball_id,
                type_id: 587,
                position: target_pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await
        .expect("spawn approach target");

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdFollowBall",
        &[PyRep::Integer(target_ball_id), PyRep::Real(0.0)],
        &kw,
        None,
    )
    .await
    .expect("CmdFollowBall");
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdSetSpeedFraction",
        &[PyRep::Real(1.0)],
        &kw,
        None,
    )
    .await
    .expect("CmdSetSpeedFraction");

    let dist_before = cur_ship.position.distance(target_pos);
    for _ in 0..5 {
        sys_handle.step_manual(1.0).await.expect("step_manual");
    }
    let (_, ship_after_approach) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(ship_after_approach.position.distance(target_pos) < dist_before);

    // 3. Afterburner (`feat:modules.afterburner`)
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(ab_item_id)],
        &kw,
        None,
    )
    .await
    .expect("Activate Afterburner");
    let (_, ab_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let ab_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(ab_ball.max_velocity > base_vmax);
    assert!((ab_cs.signature_radius - base_sig).abs() < 1e-3);

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[PyRep::Integer(ab_item_id)],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Afterburner");

    // 4. Microwarpdrive (`feat:modules.microwarpdrive`) — swap online state from AB to MWD (maxGroupOnline = 1)
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(ab_item_id, false);
        snap.module_online_states.insert(mwd_item_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);
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
    .expect("Activate MWD");
    let (_, mwd_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let mwd_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(
        mwd_ball.max_velocity > ab_ball.max_velocity
            && mwd_ball.mass > base_mass
            && mwd_cs.signature_radius > base_sig * 4.0
    );
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

    // Apply an active Warp Scrambler tackle cycle on ship_id (2 scramble points + MWD/MJD suppression)
    sys_handle
        .add_module_cycle(eve_server::system_actor::ActiveModuleCycle {
            ship_id: target_ball_id,
            module_id: 999_001,
            module_type_id: 447,
            effect_name: "warpScrambleBlockMWDWithNPCEffect".into(),
            target_id: Some(ship_id),
            repeat: 1,
            duration_s: 60.0,
            elapsed_s: 0.0,
            cap_need_per_cycle: 0.0,
            speed_boost_factor: 0.0,
            original_max_velocity: None,
            linked_turrets: Vec::new(),
            ammo_type_id: None,
            damage_vector: eve_destiny::DamageVector::new(0.0, 0.0, 0.0, 0.0),
            cap_drain_target: 0.0,
            cap_transfer_target: 0.0,
            shield_boost_amount: 0.0,
            armor_repair_amount: 0.0,
            is_overloaded: false,
            rack_idx: 1,
            heat_damage_per_cycle: 0.0,
            reactivation_delay_s: 0.0,
            is_mwd_or_mjd: false,
            warp_scramble_points: 2,
            disables_target_mwd_mjd: true,
            warp_core_stab_bonus: 0,
            hic_bubble_radius_m: None,
            hic_velocity_multiplier: None,
            hic_sig_multiplier: None,
            hic_mass_addition: None,
            original_signature_radius: None,
            original_mass: None,
            mjd_jump_distance_m: None,
            grants_interdiction_nullification: false,
            effective_max_capacitor: None,
            ..Default::default()
        })
        .await
        .unwrap();
    let mwd_scram_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(mwd_item_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(mwd_scram_err.code, "WarpScrambled");

    // 5. Micro Jump Drive (`feat:modules.micro-jump-drive`, `feat:navigation.directional-jump-drive`)
    let mjd_scram_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(mjd_item_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(mjd_scram_err.code, "WarpScrambled");

    sys_handle.remove_module_cycle(target_ball_id, 999_001).await.unwrap();

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdGotoDirection",
        &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
        &kw,
        None,
    )
    .await
    .expect("Set heading +X");
    sys_handle.step_manual(1.0).await.unwrap();
    let (_, ball_before_mjd) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let pos_before = ball_before_mjd.position;

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
    .expect("Activate MJD");

    for _ in 0..12 {
        sys_handle.step_manual(1.0).await.unwrap();
    }
    let (_, ball_after_mjd) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!((ball_after_mjd.position.distance(pos_before) - 100_000.0).abs() < 2_500.0);

    // 6. Micro Jump Field Generator, 25-ship Boosh Limit, Fleet Jump, & Capital MJD
    // (`feat:modules.micro-jump-field-generator`, `feat:modules.micro-jump-field-boosh-limit`, `feat:modules.mjfg-fleet-jump`, `feat:modules.capital-micro-jump-drive`)
    let mut park = Ballpark::new(30_004_608);
    let bifrost_id = 500_001;
    let origin = DVec3::new(1_000_000.0, 0.0, 0.0);
    park.add_ball(Ball {
        id: bifrost_id,
        type_id: 37480, // Bifrost (Command Destroyer)
        position: origin,
        velocity: DVec3::new(100.0, 0.0, 0.0),
        max_velocity: 300.0,
        inertia: 2.5,
        mass: 1_500_000.0,
        radius: 65.0,
        speed_fraction: 1.0,
        mode: MovementMode::GotoDirection { direction: DVec3::X },
    });

    // Spawn 30 eligible ships within 6 km, plus 1 scrambled ship and 1 cloaked ship
    for i in 0..30 {
        let bid = 501_000 + i;
        park.add_ball(Ball {
            id: bid,
            type_id: 587,
            position: origin + DVec3::new((i as f64) * 150.0, 100.0, 0.0), // 0..4,350 m (<= 6,000 m)
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        });
    }
    let scrammed_victim_id = 502_001;
    park.add_ball(Ball {
        id: scrammed_victim_id,
        type_id: 587,
        position: origin + DVec3::new(500.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    park.combat_states
        .get_mut(&scrammed_victim_id)
        .unwrap()
        .is_mwd_mjd_disabled = true;

    // Trigger Command Destroyer 9s MJFG spool-up via capacitor drop on Bifrost (37480)
    park.combat_states.get_mut(&bifrost_id).unwrap().last_step_capacitor = Some(250.0);
    park.combat_states.get_mut(&bifrost_id).unwrap().capacitor = 150.0;
    let base_bifrost_sig = park.combat_states[&bifrost_id].signature_radius;

    park.step(1.0);
    assert!(
        (park.combat_states[&bifrost_id].signature_radius - base_bifrost_sig * 2.5).abs() < 1e-3,
        "MJFG spool-up must apply +150% signature radius bloom (2.5x)"
    );
    for _ in 0..8 {
        park.step(1.0);
    }

    // Verify Bifrost + exactly 25 closest eligible ships booshed 100 km, while ships 25..30 and scrambled ship stayed behind
    assert!(
        park.balls[&bifrost_id].position.x > origin.x + 99_000.0,
        "Bifrost must translate 100 km along +X upon completing 9s MJFG spool-up"
    );
    let booshed_count = (0..30)
        .filter(|&i| park.balls[&(501_000 + i)].position.x > origin.x + 90_000.0)
        .count();
    assert_eq!(
        1 + booshed_count,
        25,
        "Command Destroyer MJFG must boosh at most 25 total ships within 6 km"
    );
    assert!(
        park.balls[&scrammed_victim_id].position.x < origin.x + 5_000.0,
        "Warp-scrambled ships must NOT be moved by MJFG boosh"
    );

    // Test Capital Micro Jump Drive (250 km jump for capital hull 19720 Revelation) & Capital MJFG fleet jump (250 km / 10 km / 50 ships)
    let dread_id = 503_001;
    let fleet_ally_id = 503_002;
    park.add_ball(Ball {
        id: dread_id,
        type_id: 19720, // Revelation (Dreadnought)
        position: origin,
        velocity: DVec3::new(50.0, 0.0, 0.0),
        max_velocity: 80.0,
        inertia: 0.1,
        mass: 1_200_000_000.0,
        radius: 1_200.0,
        speed_fraction: 1.0,
        mode: MovementMode::GotoDirection { direction: DVec3::X },
    });
    park.add_ball(Ball {
        id: fleet_ally_id,
        type_id: 621,
        position: origin + DVec3::new(8_500.0, 0.0, 0.0), // 8.5 km (within 10 km Capital MJFG radius)
        velocity: DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 2.0,
        mass: 10_000_000.0,
        radius: 120.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });

    // Capital MJD translates capital ship 250,000 m
    let cap_mjd_pos = park.micro_jump(dread_id, 100_000.0).expect("Capital MJD jump");
    assert!(
        (cap_mjd_pos.distance(origin) - 250_000.0).abs() < 1.0,
        "Capital Micro Jump Drive must jump 250,000 m along ship heading"
    );

    // Reset dread to origin and run Capital MJFG (250 km jump, 10 km field, up to 50 subcapitals)
    park.set_ball_position(dread_id, origin);
    let moved_fleet = park
        .micro_jump_field(dread_id, 250_000.0, 10_000.0, 50)
        .expect("Capital MJFG fleet jump");
    assert!(
        moved_fleet.contains(&dread_id) && moved_fleet.contains(&fleet_ally_id),
        "Capital MJFG must jump the capital ship and subcapitals within 10 km by 250 km"
    );
    assert!((park.balls[&fleet_ally_id].position.x - (origin.x + 8_500.0 + 250_000.0)).abs() < 1.0);
}

#[tokio::test]
async fn test_sp3_cloaking_prototype_covert_decloak_and_stabilization() {
    // parity: plan:SP-3
    // parity: feat:modules.cloak-prototype, feat:modules.cloak-decloak, feat:modules.covert-ops-cloak, feat:modules.cloak-stabilization-timer, rpc:beyonce.CmdWarpToStuff, rpc:beyonce.UpdateStateRequest, rpc:charUnboundMgr.SelectCharacterID, rpc:dogmaIM.Activate, rpc:stationSvc.Undock
    eve_testkit::attest(&[
        "plan:SP-3",
        "feat:modules.cloak-prototype",
        "feat:modules.cloak-decloak",
        "feat:modules.covert-ops-cloak",
        "feat:modules.cloak-stabilization-timer",
    ]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(2099);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
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

    let proto_cloak_id = 60_000_301;
    grant_all_test_skills(&ctx, char_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_id);
        snap.items.get_mut(&ship_id).unwrap().type_id = 11188; // Anathema (Covert Ops, group 830)
        snap.items.insert(
            proto_cloak_id,
            make_item(proto_cloak_id, 11370, char_id, ship_id, InventoryFlag::HiSlot0, 1, true),
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    let undock_res = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(ukv) = undock_res else {
        panic!("Expected KeyVal from Undock");
    };
    let beyonce_id = ukv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    let sys_id = 30_000_142;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Position ship in open space far from station (so station doesn't proximity-decloak us immediately)
    let open_pos = DVec3::new(15_000_000.0, 0.0, 0.0);
    let (_, mut start_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    start_ball.position = open_pos;
    start_ball.velocity = DVec3::ZERO;
    start_ball.speed_fraction = 0.0;
    start_ball.mode = MovementMode::Stop;
    sys_handle.spawn_ball(start_ball).await.unwrap();

    // 1. Prototype Cloaking Device I (`11370`, `feat:modules.cloak-prototype`, `feat:modules.cloak-stabilization-timer`)
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(proto_cloak_id)],
        &kw,
        None,
    )
    .await
    .expect("Activate Prototype Cloaking Device I");
    let (_, pre_sync_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let orig_vmax = pre_sync_ball.max_velocity;
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "UpdateStateRequest",
        &[],
        &kw,
        None,
    )
    .await
    .expect("Sync cloak state");

    let (_, proto_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let proto_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(proto_cs.is_cloaked && !proto_cs.is_covert_cloak);
    assert_eq!(
        proto_cs.cloak_stabilization_remaining_s, 900.0,
        "Cloak activation must start 15-minute (900s) Cloak Stabilization timer"
    );
    assert!(
        (proto_ball.max_velocity - orig_vmax * 0.25).abs() < 1e-3,
        "Prototype Cloak I must apply -75% max_velocity penalty (0.25x): got {} vs orig {}",
        proto_ball.max_velocity,
        orig_vmax
    );

    // Prototype cloak blocks warp initiation with CloakedCannotWarp
    let warp_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(60003760)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(warp_err.code, "CloakedCannotWarp");

    // 2. Proximity Decloak within 2,000m & Post-Decloak Targeting Delay (`feat:modules.cloak-decloak`)
    let hunter_id = 940_001;
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: hunter_id,
                type_id: 587,
                position: open_pos + DVec3::new(1_500.0, 0.0, 0.0), // within 2,000 m
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.5,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await
        .unwrap();

    sys_handle.step_manual(1.0).await.expect("step proximity decloak");
    let decloaked_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    let (_, decloaked_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(!decloaked_cs.is_cloaked, "Object within 2,000m must decloak the ship");
    assert!(
        decloaked_cs.decloak_targeting_delay_remaining_s >= 29.0,
        "Prototype Cloak decloak must apply 30s sensor recalibration targeting delay (got {})",
        decloaked_cs.decloak_targeting_delay_remaining_s
    );
    assert!(
        (decloaked_ball.max_velocity - orig_vmax).abs() < 1e-3,
        "Decloaking must restore original max_velocity"
    );

    // While decloak_targeting_delay_remaining_s > 0, ship cannot lock targets
    sys_handle
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: hunter_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 1_000.0,
                locked: false,
                max_range_m: Some(50_000.0),
            },
        )
        .await
        .unwrap();
    assert!(
        sys_handle.get_target_locks(ship_id).await.unwrap().is_empty(),
        "Post-decloak sensor recalibration delay must prevent target locking"
    );
    let _ = sys_handle.remove_ball(hunter_id).await;

    // 3. Covert Ops Cloaking Device II (`11578`, `feat:modules.covert-ops-cloak`) & 15-min Stabilization Expiry
    // Clear targeting delay and switch module to Covert Ops Cloaking Device II (11578)
    let mut clear_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    clear_cs.decloak_targeting_delay_remaining_s = 0.0;
    sys_handle.set_combat_state(ship_id, clear_cs).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&proto_cloak_id).unwrap().type_id = 11578; // Covert Ops Cloaking Device II
        snap.module_active_states.insert(proto_cloak_id);
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "UpdateStateRequest",
        &[],
        &kw,
        None,
    )
    .await
    .unwrap();

    let cov_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    let (_, cov_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(cov_cs.is_cloaked && cov_cs.is_covert_cloak);
    assert!((cov_ball.max_velocity - orig_vmax).abs() < 1e-3, "Covert Ops Cloak II has no velocity penalty");

    // Covert Ops Cloak allows initiating warp while cloaked!
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(60003760), PyRep::Real(50_000.0)],
        &kw,
        None,
    )
    .await
    .expect("Covert Ops Cloak II must allow warping while cloaked");
    let (_, cov_warping_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(cov_warping_ball.mode, MovementMode::Warp { .. }));

    // Test 15-minute (900s) Cloak Stabilization timer countdown to 0.0
    let mut exp_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    exp_cs.cloak_stabilization_remaining_s = 1.0;
    sys_handle.set_combat_state(ship_id, exp_cs).await.unwrap();
    sys_handle.step_manual(1.0).await.unwrap();
    let after_exp_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(
        after_exp_cs.cloak_stabilization_remaining_s, 0.0,
        "15-minute (900s) Cloak Stabilization timer must count down to 0.0"
    );
}

#[tokio::test]
async fn test_sp3_subwarp_navigation_orbit_keep_at_range_align_goto_stop() {
    // parity: plan:SP-3, plan:S-1
    // parity: feat:navigation.orbit, feat:navigation.keep-at-range, feat:navigation.align-to, feat:navigation.goto-point, feat:navigation.stop-ship, rpc:beyonce.CmdAlignTo, rpc:beyonce.CmdFollowBall, rpc:beyonce.CmdGotoDirection, rpc:beyonce.CmdOrbit, rpc:beyonce.CmdSetSpeedFraction, rpc:beyonce.CmdStop, rpc:charUnboundMgr.SelectCharacterID, rpc:stationSvc.Undock
    eve_testkit::attest(&[
        "plan:S-1",
        "feat:navigation.orbit",
        "feat:navigation.keep-at-range",
        "feat:navigation.align-to",
        "feat:navigation.goto-point",
        "feat:navigation.stop-ship",
    ]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(3001);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
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

    let undock_res = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(ukv) = undock_res else {
        panic!("Expected KeyVal from Undock");
    };
    let beyonce_id = ukv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    let sys_id = 30_000_142;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    let anchor_origin = DVec3::new(500_000.0, 0.0, 0.0);
    let (_, mut ship_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    ship_ball.position = anchor_origin;
    ship_ball.velocity = DVec3::ZERO;
    ship_ball.speed_fraction = 0.0;
    ship_ball.mode = MovementMode::Stop;
    let mover_radius = ship_ball.radius;
    sys_handle.spawn_ball(ship_ball).await.unwrap();

    let target_id = 910_001;
    let target_pos = anchor_origin + DVec3::new(3_500.0, 0.0, 0.0);
    let target_radius = 50.0;
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: target_id,
                type_id: 587,
                position: target_pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: target_radius,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await
        .unwrap();

    // 1. Keep at Range (`feat:navigation.keep-at-range`)
    let desired_range = 1_500.0;
    let expected_center_dist = target_radius + mover_radius + desired_range;
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdSetSpeedFraction",
        &[PyRep::Real(1.0)],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdFollowBall",
        &[PyRep::Integer(target_id), PyRep::Real(desired_range)],
        &kw,
        None,
    )
    .await
    .unwrap();

    for _ in 0..25 {
        sys_handle.step_manual(1.0).await.unwrap();
    }
    let (_, b_kar_far) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!((b_kar_far.position.distance(target_pos) - expected_center_dist).abs() < 250.0);

    let (_, mut close_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    close_ball.position = target_pos - DVec3::new(300.0, 0.0, 0.0);
    close_ball.velocity = DVec3::ZERO;
    close_ball.speed_fraction = 1.0;
    close_ball.mode = MovementMode::Follow {
        target_id,
        range: desired_range,
    };
    sys_handle.spawn_ball(close_ball).await.unwrap();
    for _ in 0..20 {
        sys_handle.step_manual(1.0).await.unwrap();
    }
    let (_, b_kar_close) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let dist_from_close = b_kar_close.position.distance(target_pos);
    assert!(dist_from_close > 900.0 && (dist_from_close - expected_center_dist).abs() < 300.0);

    // 2. Orbit (`feat:navigation.orbit`)
    let orbit_range = 2_000.0;
    let expected_orbit_radius = target_radius + mover_radius + orbit_range;
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdOrbit",
        &[PyRep::Integer(target_id), PyRep::Real(orbit_range)],
        &kw,
        None,
    )
    .await
    .unwrap();

    let (_, b_orbit_start) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let rel_start = (b_orbit_start.position - target_pos).normalize();
    for _ in 0..25 {
        sys_handle.step_manual(1.0).await.unwrap();
    }
    let (_, b_orbit_end) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!((b_orbit_end.position.distance(target_pos) - expected_orbit_radius).abs() < 350.0);
    let rel_end = (b_orbit_end.position - target_pos).normalize();
    let radial_speed = b_orbit_end.velocity.dot(rel_end).abs();
    let tangential_speed = (b_orbit_end.velocity - rel_end * b_orbit_end.velocity.dot(rel_end)).length();
    assert!(tangential_speed > radial_speed && tangential_speed > 100.0 && rel_start.dot(rel_end) < 0.98);

    // 3. Goto Direction (`feat:navigation.goto-point`)
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdGotoDirection",
        &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)],
        &kw,
        None,
    )
    .await
    .unwrap();
    for _ in 0..12 {
        sys_handle.step_manual(1.0).await.unwrap();
    }
    let (_, b_goto) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(b_goto.velocity.normalize().y > 0.90 && b_goto.velocity.length() > 200.0);

    // 4. Stop Ship (`feat:navigation.stop-ship`)
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdStop",
        &[],
        &kw,
        None,
    )
    .await
    .unwrap();
    for _ in 0..25 {
        sys_handle.step_manual(1.0).await.unwrap();
    }
    let (_, b_stopped) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(b_stopped.mode, MovementMode::Stop);
    assert!(b_stopped.velocity.length() < 1.0);

    // 5. Align To (`feat:navigation.align-to`)
    for &(hull_type_id, expected_align_ticks) in &[(587_u32, 5_usize), (24692_u32, 21_usize)] {
        let hull_dogma = DogmaItemState::from_sde(ship_id, hull_type_id, &ctx.sde);
        let vmax = hull_dogma.get_attribute(eve_sde::attr_ids::MAX_VELOCITY, &ctx.sde);
        let mass = hull_dogma.get_attribute(eve_sde::attr_ids::MASS, &ctx.sde);
        let inertia = hull_dogma.get_attribute(eve_sde::attr_ids::AGILITY, &ctx.sde);
        let tau = inertia * mass / 1_000_000.0;
        assert_eq!((4.0_f64.ln() * tau).ceil() as usize, expected_align_ticks);

        sys_handle
            .spawn_ball(Ball {
                id: ship_id,
                type_id: hull_type_id,
                position: anchor_origin,
                velocity: DVec3::ZERO,
                max_velocity: vmax,
                inertia,
                mass,
                radius: ctx.sde.types[&hull_type_id].radius,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await
            .unwrap();

        ctx.dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdAlignTo",
            &[PyRep::Integer(60003760)],
            &kw,
            None,
        )
        .await
        .unwrap();

        for t in 1..=expected_align_ticks {
            sys_handle.step_manual(1.0).await.unwrap();
            let (_, b_now) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
            if t < expected_align_ticks {
                assert!(b_now.velocity.length() < 0.75 * vmax);
            } else {
                assert!(b_now.velocity.length() >= 0.75 * vmax);
            }
        }
    }
}
