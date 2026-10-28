//! Integration tests for `SP-2` (Grid Bubble Splitting, Interdiction Spheres, WDFG, Mobile Warp Disruptors, & Interdiction Nullifiers).

use crate::common;

use eve_db::types::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

pub(crate) fn make_item(
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

pub(crate) fn grant_all_test_skills(ctx: &ServerContext, char_id: i64) {
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
    // parity: feat:navigation.bubbles-grid, feat:modules.interdiction-sphere, feat:modules.mobile-warp-disruptor, feat:modules.interdiction-nullifier, feat:modules.mobile-warp-disruptor-anchoring-skill, rpc:beyonce.AdminGetBubbleID, rpc:charUnboundMgr.SelectCharacterID, rpc:posMgr.AnchorStructure
    eve_testkit::attest(&["plan:SP-2", "feat:navigation.bubbles-grid", "feat:modules.interdiction-sphere", "feat:modules.mobile-warp-disruptor", "feat:modules.interdiction-nullifier", "feat:modules.mobile-warp-disruptor-anchoring-skill"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(2001);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
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

    // 1c. Wormhole space (J100001 `31_000_005`) allows Interdiction Sphere Probe launch (`feat:modules.interdiction-sphere`)
    let wh_sys_id = 31_000_005_u32;
    let wh_handle = ctx.systems.get_or_spawn(wh_sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = wh_sys_id;
        snap.items.get_mut(&ship_id).unwrap().location_id = wh_sys_id as i64;
    });
    sess.solar_system_id = Some(wh_sys_id);
    wh_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 22456,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 380.0,
            inertia: 2.8,
            mass: 1_350_000.0,
            radius: 58.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    let wh_launch = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(launcher_item_id)], &kw, None)
        .await
        .expect("Launch interdiction probe in wormhole space");
    let PyRep::KeyVal(wh_lkv) = wh_launch else { panic!() };
    assert_eq!(wh_lkv["bubbleRadius"], PyRep::Real(20_000.0));
    wh_handle.remove_warp_bubble(wh_lkv["probeID"].as_i64().unwrap()).await.unwrap();

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
    let mut first_probe_bubble_id = 0_i64;
    for (idx, probe_type_id) in [22778_u32, 34260, 41402].into_iter().enumerate() {
        ctx.db.with_mut_snapshot(|snap| {
            let c = snap.items.get_mut(&charge_item_id).unwrap();
            c.type_id = probe_type_id;
            c.quantity = 5;
        });
        let launch_res = ctx
            .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(launcher_item_id)], &kw, None)
            .await
            .expect("Launch interdiction probe variant");
        let PyRep::KeyVal(lkv) = launch_res else { panic!() };
        let b_id = lkv.get("probeID").unwrap().as_i64().unwrap();
        let b_rad = lkv.get("bubbleRadius").unwrap().as_f64().unwrap();
        assert!(b_rad >= 10_000.0);
        if idx == 0 {
            first_probe_bubble_id = b_id;
            assert_eq!(b_rad, 20_000.0);
        } else {
            null_handle.remove_warp_bubble(b_id).await.unwrap();
        }
    }

    // 2b. Activate Warp Disruption Field Generator I (`28654`) on Heavy Interdiction Cruiser (Onyx `11995`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 11995; // Onyx (HIC, group 894)
        snap.items.insert(
            wdfg_item_id,
            make_item(wdfg_item_id, 28654, char_id, ship_id, InventoryFlag::HiSlot1, 1, true),
        );
    });
    ctx.invalidate_ship_fitting(ship_id);
    null_handle.set_ball_position(ship_id, DVec3::new(30_000_000.0, 0.0, 0.0)).await;
    let wdfg_res = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(wdfg_item_id)], &kw, None)
        .await
        .expect("Activate WDFG in null-sec");
    let PyRep::KeyVal(wkv) = wdfg_res else { panic!() };
    assert_eq!(wkv.get("bubbleRadius").unwrap().as_f64().unwrap(), 16_000.0);
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(wdfg_item_id)], &kw, None)
        .await
        .expect("Deactivate WDFG");

    // 2c. Mobile Warp Disruptor Anchoring Skill & Deploy Timer (`feat:modules.mobile-warp-disruptor`, `feat:modules.mobile-warp-disruptor-anchoring-skill`)
    let mwd_pos = DVec3::new(35_000_000.0, 0.0, 0.0);
    null_handle.set_ball_position(ship_id, mwd_pos).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sk) = snap.skills.get_mut(&char_id) {
            sk.remove(&11584);
        }
    });
    let no_skill_err = ctx
        .dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mobile_disruptor_item_id)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(no_skill_err.code, "SkillMissing");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            11584,
            TrainedSkillRecord { type_id: 11584, level: 2, skill_points: 1_415.0 },
        );
        snap.items.get_mut(&mobile_disruptor_item_id).unwrap().type_id = 26890; // Mobile Medium Warp Disruptor II (req Anchoring IV)
    });
    let t2_skill_err = ctx
        .dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mobile_disruptor_item_id)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(t2_skill_err.code, "SkillMissing");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.get_mut(&char_id).unwrap().get_mut(&11584).unwrap().level = 4;
        snap.items.get_mut(&mobile_disruptor_item_id).unwrap().type_id = 12198;
    });
    let anchor_res = ctx
        .dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mobile_disruptor_item_id)], &kw, None)
        .await
        .expect("Anchor Mobile Small Warp Disruptor I in null-sec");
    let PyRep::KeyVal(akv) = anchor_res else { panic!() };
    let mobile_bubble_id = akv.get("itemID").unwrap().as_i64().unwrap();
    assert_eq!(akv["state"], PyRep::String("Anchoring".into()));
    assert_eq!(akv["anchoringDelaySeconds"], PyRep::Real(120.0));
    assert_eq!(akv["bubbleRadius"], PyRep::Real(5_000.0));
    // Step 120s deploy timer so the Mobile Small Warp Disruptor I bubble activates in space
    null_handle.step_manual(120.0).await.expect("Complete 120s deploy timer");

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
        (caught_dest.x - 24_980_000.0).abs() < 1.0,
        "Un-nullified warp must be stopped/dragged to 20km sphere bubble perimeter at 24,980 km: got {}",
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

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(nullifier_item_id)], &kw, None)
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
