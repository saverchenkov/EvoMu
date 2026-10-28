//! Integration tests for `plan:SP-7` (`Background cluster scheduler, unobserved systems & ballpark reload`),
//! `feat:lifecycle.world-unobserved-system-sim`, `feat:lifecycle.world-restart-space-objects`,
//! `feat:lifecycle.wreck-restart`, `feat:lifecycle.structure-restart`,
//! `feat:lifecycle.mobile_warp_disruptor-lifecycle`, `feat:lifecycle.drones-in-space-persist-restart`,
//! `feat:lifecycle.wreck-downtime`, and `feat:lifecycle.mission-downtime-respawn`.

use crate::common::*;
use crate::sp6_logoff_and_server_tick::{
    persist_and_reopen_sqlite, setup_pilot_in_space, sp6_item,
};
use eve_db::{
    ActiveMissionRecord, CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag,
    SkillQueueEntry, TrainedSkillRecord, VirtualServerClock,
};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode, SlimItemMeta};
use eve_proto::PyRep;
use eve_server::combat::death_pipeline::execute_death_pipeline;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_combat_runtime;
use eve_server::sp6_lifecycle::{
    handle_character_logoff_or_disconnect, record_wreck_or_container_meta_in_snap,
    tick_background_cluster,
};
use eve_server::system_actor::ActiveModuleCycle;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
async fn test_sp7_unobserved_system_simulation_and_background_cluster_tick() {
    // parity: plan:SP-7, feat:lifecycle.world-unobserved-system-sim
    eve_testkit::attest(&["plan:SP-7", "feat:lifecycle.world-unobserved-system-sim"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let empty = HashMap::new();
    // Put pilot 90_000_001 in Amarr (30_002_187) at [90_000, 0, 0] with high shield HP
    let (mut sess, old_sys, ship_id) = setup_pilot_in_space(&ctx, 90_000_001).await;
    let _ = ctx.systems.get_or_spawn(old_sys).remove_ball(ship_id).await;
    let sys_id = 30_002_187_u32;
    let drone_id = 880_100_001_i64;
    let cargo_id = 880_100_002_i64;
    let wreck_id = 880_100_003_i64;
    let now0 = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = sys_id;
        let sh = snap.items.get_mut(&ship_id).unwrap();
        sh.location_id = sys_id as i64;
        sh.type_id = 626; // Vexor
        snap.item_positions.insert(ship_id, [90_000.0, 0.0, 0.0]);
        snap.items.insert(
            drone_id,
            sp6_item(drone_id, 2_454, 90_000_001, ship_id, InventoryFlag::DroneBay, HashMap::new()),
        );
        snap.items.insert(
            cargo_id,
            sp6_item(cargo_id, 34, 90_000_001, ship_id, InventoryFlag::Cargo, HashMap::new()),
        );
        snap.items.insert(
            wreck_id,
            sp6_item(wreck_id, 26_557, 90_000_001, sys_id as i64, InventoryFlag::Cargo, HashMap::new()),
        );
        snap.item_positions.insert(wreck_id, [90_500.0, 0.0, 0.0]);
        record_wreck_or_container_meta_in_snap(snap, wreck_id, now0, false);
    });
    ctx.invalidate_ship_fitting(ship_id);
    sess.solar_system_id = Some(sys_id);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let tank_cs = CombatState {
        shield_hp: 50_000.0, max_shield_hp: 50_000.0, armor_hp: 50_000.0, max_armor_hp: 50_000.0,
        structure_hp: 50_000.0, max_structure_hp: 50_000.0, capacitor: 5_000.0, max_capacitor: 5_000.0,
        ..CombatState::default()
    };
    sys_handle
        .spawn_ball_with_meta(
            Ball {
                id: ship_id, type_id: 626, position: DVec3::new(90_000.0, 0.0, 0.0),
                velocity: DVec3::new(150.0, 0.0, 0.0), max_velocity: 300.0, inertia: 2.0,
                mass: 1_000_000.0, radius: 40.0, speed_fraction: 1.0,
                mode: MovementMode::GotoDirection { direction: DVec3::X },
            },
            tank_cs,
            SlimItemMeta {
                group_id: 26, category_id: 6, owner_id: 90_000_001, char_id: Some(90_000_001),
                corp_id: 1_000_009, alliance_id: -1, security_status: 0.0, is_cloaked: false,
            },
        )
        .await
        .expect("spawn player ship");
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: wreck_id, type_id: 26_557, position: DVec3::new(90_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 10_000.0,
                radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .expect("spawn wreck ball");
    eve_server::services::combat_crimewatch::combat_wrecks_killmails::register_container_lifetime(
        &ctx, wreck_id, sys_id, 26_557, 90_000_001, now0, false,
    );

    // Launch a drone and jettison a jetcan (3,600s lifetime)
    ctx.dispatch_rpc(&mut sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(drone_id)])], &empty, None)
        .await
        .expect("LaunchDrones");
    let jet_res = ctx
        .dispatch_rpc(&mut sess, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(cargo_id)])], &empty, None)
        .await
        .expect("Jettison");
    let PyRep::Tuple(jet_tup) = jet_res else { panic!("expected Tuple from Jettison") };
    let jetcan_id = match &jet_tup[0] { PyRep::List(l) => l[0].as_i64().unwrap(), _ => panic!("expected List") };

    // Spawn a Pirate NPC at [92_000, 0, 0] and run 1 AI tick so it warp-scrambles and aggresses the ship (5m = 300s NPC timer)
    let npc_id = eve_server::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity(
        &ctx, sys_id, 10_019, "Pirate", 1, DVec3::new(92_000.0, 0.0, 0.0),
    )
    .await;
    assert!(npc_id > 0);
    eve_server::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai(&ctx, sys_id).await;
    sys_handle.set_warp_scramble_strength(ship_id, 2).await.expect("NPC warp scramble");

    // Also add an active weapon cycle on the NPC so both module cycles and NPC AI fire in the unobserved system
    sys_handle
        .add_module_cycle(ActiveModuleCycle {
            ship_id: npc_id, module_id: 999_111, module_type_id: 3_001,
            effect_name: "targetAttack".into(), target_id: Some(ship_id), repeat: 1_000,
            duration_s: 2.0, damage_vector: DamageVector::new(25.0, 25.0, 0.0, 0.0),
            ..Default::default()
        })
        .await
        .expect("add_module_cycle");

    // Seed an expiring anomaly/signature (5s), a 1s skill queue item, and a pending CONCORD arrival at T+5s
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies.entry(sys_id).or_default().push(CosmicAnomalyRecord {
            anomaly_id: "SP7-ANOM-1".into(), solar_system_id: sys_id, name: "Temp Anomaly".into(),
            anomaly_type: "Combat".into(), position: [0.0, 0.0, 0.0], signal_strength: 1.0,
            ore_types: vec![], remaining_volume: 0.0, created_unix: now0, lifetime_seconds: 5, cleared: false,
        });
        snap.cosmic_signatures.entry(sys_id).or_default().push(CosmicSignatureRecord {
            signature_id: "SP7-SIG-1".into(), solar_system_id: sys_id, sig_group: "Relic".into(),
            type_name: "Temp Sig".into(), position: [0.0, 0.0, 0.0], signal_strength: 1.0,
            wormhole_type_id: None, destination_system_id: None, created_unix: now0, lifetime_seconds: 5,
            max_mass_kg: 0.0, remaining_mass_kg: 0.0, max_jump_mass_kg: 0.0, collapsed: false,
        });
        snap.characters.get_mut(&90_000_001).unwrap().clone_grade = 2;
        snap.skills.entry(90_000_001).or_default().remove(&3_300);
        snap.skill_queues.insert(90_000_001, vec![SkillQueueEntry {
            type_id: 3_300, target_level: 1, start_sp: 249.9, end_sp: 250.0,
            duration_seconds: 1.0, started_at_unix: Some(now0),
        }]);
    });
    ctx.crimewatch.write().unwrap().states.entry(90_000_002).or_default().pending_concord_arrival =
        Some((sys_id, now0 + 5, 100_000_002, 90_000_001));

    // Pilot disconnects — leaving the system with 0 subscribed sessions, an abandoned drone, a logged-off ship with a 300s NPC timer, an attacking NPC, a jetcan, and a wreck
    handle_character_logoff_or_disconnect(&ctx, 90_000_001).await;

    // Advance 10 seconds with 0 subscribers and run `tick_background_cluster`
    for _ in 0..10 {
        ctx.advance_secs(1);
        tick_background_cluster(&ctx).await;
    }

    let (_, moved_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().expect("disconnected ship still in unobserved system at T+10s");
    assert!(moved_ball.position.x > 90_100.0, "Unobserved system must advance ship physics (pos.x = {})", moved_ball.position.x);
    let target_cs = sys_handle.get_combat_state(ship_id).await.unwrap().expect("disconnected ship combat state");
    assert!(target_cs.shield_hp < 50_000.0, "Disconnected ship being shot by NPCs in unobserved system must lose HP (shield_hp = {})", target_cs.shield_hp);
    ctx.db.with_snapshot(|snap| {
        assert!(snap.cosmic_anomalies[&sys_id].is_empty());
        assert!(snap.cosmic_signatures[&sys_id].is_empty());
        assert_eq!(snap.skills[&90_000_001][&3_300].level, 1);
        assert!(snap.skill_queues[&90_000_001].is_empty());
        assert!(snap.kill_rights.values().any(|kr| kr.owner_id == 90_000_001 && kr.target_id == 90_000_002));
    });

    // Advance to T+300s: the disconnected ship's 5m (300s) NPC aggression logoff timer expires on schedule
    ctx.advance_secs(290);
    tick_background_cluster(&ctx).await;
    assert!(sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_none(), "Disconnected ship must be removed when its 300s NPC logoff timer expires in unobserved system");

    // Advance to T+3601s: the 1h (3,600s) jetcan despawn timer expires while the 2h (7,200s) wreck remains
    ctx.advance_secs(3_301);
    tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.items.contains_key(&jetcan_id)), false);
    assert_eq!(ctx.db.with_snapshot(|s| s.items.contains_key(&wreck_id)), true);

    // Advance to T+7201s: the 2h (7,200s) wreck despawn timer expires on schedule
    ctx.advance_secs(3_600);
    tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.items.contains_key(&wreck_id)), false);
}

#[tokio::test]
async fn test_sp7_restart_reloads_structures_wrecks_jetcans_mwd_bubbles_and_drones() {
    // parity: feat:lifecycle.world-restart-space-objects, feat:lifecycle.wreck-restart, feat:lifecycle.structure-restart, feat:lifecycle.mobile_warp_disruptor-lifecycle, feat:lifecycle.drones-in-space-persist-restart
    eve_testkit::attest(&[
        "feat:lifecycle.world-restart-space-objects",
        "feat:lifecycle.wreck-restart",
        "feat:lifecycle.structure-restart",
        "feat:lifecycle.mobile_warp_disruptor-lifecycle",
        "feat:lifecycle.drones-in-space-persist-restart",
    ]);

    let (ctx2, sys_id, structure_id, wreck_id, dropped_loot_ids, jetcan_id, mwd_item_id, depot_item_id, drone_item_id, drone_ball_id, disc_ship_id) = {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let empty = HashMap::new();
        let (mut sess, old_sys_id, ship_id) = setup_pilot_in_space(&ctx, 90_000_001).await;
        let _ = ctx.systems.get_or_spawn(old_sys_id).remove_ball(ship_id).await;
        let (mut sess2, old_sys2, disc_ship_id) = setup_pilot_in_space(&ctx, 90_000_002).await;
        let _ = ctx.systems.get_or_spawn(old_sys2).remove_ball(disc_ship_id).await;

        // Move both pilots and ships to null-sec system 6VDT-H (30004608, security -0.70 <= 0.0)
        let sys_id = 30_004_608_u32;
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = sys_id;
            snap.characters.get_mut(&90_000_002).unwrap().solar_system_id = sys_id;
            let sh = snap.items.get_mut(&ship_id).unwrap();
            sh.location_id = sys_id as i64;
            sh.type_id = 626; // Vexor (75 Mbit/s drone bandwidth)
            snap.items.get_mut(&disc_ship_id).unwrap().location_id = sys_id as i64;
            snap.items.insert(100_000_042, sp6_item(100_000_042, 185, 90_000_002, disc_ship_id, InventoryFlag::HiSlot0, HashMap::new()));
        });
        ctx.invalidate_ship_fitting(ship_id); ctx.invalidate_ship_fitting(disc_ship_id);
        sess.solar_system_id = Some(sys_id);
        sess2.solar_system_id = Some(sys_id);
        ctx.advance_secs(12);
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        for (cid, sid, tid) in [(90_000_001, ship_id, 626), (90_000_002, disc_ship_id, 606)] {
            sys_handle
                .spawn_ball_with_meta(
                    Ball {
                        id: sid,
                        type_id: tid,
                        position: DVec3::ZERO,
                        velocity: DVec3::ZERO,
                        max_velocity: 300.0,
                        inertia: 2.0,
                        mass: 1_000_000.0,
                        radius: 40.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    },
                    ctx.build_ship_combat_state(cid, sid, tid),
                    ctx.build_ship_slim_meta(cid, tid),
                )
                .await
                .expect("spawn ship in null-sec");
        }

        // 1. Anchor an Upwell Astrahus structure (`35832`) via `structureDeployment.Anchor`
        ctx.dispatch_rpc(
            &mut sess,
            "corpmgr",
            None,
            "CreateCorporation", // rpc-lint: allow legacy/negative-test RPC (corpmgr.CreateCorporation)
            &[
                PyRep::String("SP7 Syndicate".into()),
                PyRep::String("SP7".into()),
                PyRep::Real(0.05),
            ],
            &empty,
            None,
        )
        .await
        .expect("CreateCorporation");
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                991_000_100,
                sp6_item(991_000_100, 35_832, 90_000_001, ship_id, InventoryFlag::Cargo, HashMap::new()),
            );
        });
        let _ = sys_handle
            .set_ball_position(ship_id, DVec3::new(120_000.0, 0.0, -80_000.0))
            .await;
        let anchor_res = ctx
            .dispatch_rpc(
                &mut sess,
                "structureDeployment",
                None,
                "Anchor",
                &[
                    PyRep::Integer(35_832),
                    PyRep::String("SP7 Restart Astrahus".into()),
                ],
                &empty,
                None,
            )
            .await
            .expect("structureDeployment.Anchor");
        let PyRep::KeyVal(anchor_kv) = anchor_res else {
            panic!("expected structure KeyVal");
        };
        let structure_id = anchor_kv.get("structureID").unwrap().as_i64().unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            let hp = |a: u16| ctx.sde.types.get(&35_832).and_then(|t| t.default_attributes.get(&a).copied()).unwrap();
            let st = snap.structures.get_mut(&structure_id).unwrap();
            st.state = "ShieldVulnerable".into();
            st.quantum_core_installed = true;
            st.reinforced_until_unix = 0;
            st.shield_hp = hp(263);
            st.armor_hp = hp(265);
            st.hull_hp = hp(9);
        });

        // 2. Create a wreck via `execute_death_pipeline` on a ship with cargo, and a jetcan via `ship.Jettison`
        let mwd_item_id = 991_000_103_i64;
        let depot_item_id = 991_000_104_i64;
        let drone_item_id = 991_000_105_i64;
        let jettison_cargo_id = 991_000_106_i64;
        let victim_ship_id = 991_000_200_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                victim_ship_id,
                sp6_item(victim_ship_id, 606, 90_000_001, sys_id as i64, InventoryFlag::None, HashMap::new()),
            );
            for cid in 991_000_201..=991_000_208 {
                snap.items.insert(
                    cid,
                    sp6_item(cid, 34, 90_000_001, victim_ship_id, InventoryFlag::Cargo, HashMap::new()),
                );
            }
            snap.items.insert(
                jettison_cargo_id,
                sp6_item(jettison_cargo_id, 34, 90_000_001, ship_id, InventoryFlag::Cargo, HashMap::new()),
            );
            // Mobile Small Warp Disruptor I (`12198`, group `361`) and Mobile Depot (`33474`, group `1246`)
            snap.items.insert(
                mwd_item_id,
                sp6_item(mwd_item_id, 12_198, 90_000_001, ship_id, InventoryFlag::Cargo, HashMap::new()),
            );
            snap.items.insert(
                depot_item_id,
                sp6_item(depot_item_id, 33_474, 90_000_001, ship_id, InventoryFlag::Cargo, HashMap::new()),
            );
            // Hobgoblin I (`2454`, category `18`) in drone bay
            snap.items.insert(
                drone_item_id,
                sp6_item(drone_item_id, 2_454, 90_000_001, ship_id, InventoryFlag::DroneBay, HashMap::new()),
            );
            for stid in [11_584_u32, 3_435_u32, 3_436_u32] {
                snap.skills.entry(90_000_001).or_default().insert(
                    stid,
                    TrainedSkillRecord {
                        type_id: stid,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
        });
        let death = execute_death_pipeline(
            &ctx,
            sys_id,
            90_000_001,
            victim_ship_id,
            90_000_002,
            DVec3::new(15_000.0, 2_000.0, -5_000.0),
            42,
        )
        .await
        .expect("execute_death_pipeline for wreck");
        let wreck_id = death.wreck_item_id;
        let dropped_loot_ids = death.loot_dropped_item_ids.clone();
        assert!(!dropped_loot_ids.is_empty(), "Wreck must contain dropped cargo");
        if let Some(cap_id) = death.ejected_capsule_id {
            let _ = sys_handle.remove_ball(cap_id).await;
            ctx.db.with_mut_snapshot(|snap| {
                snap.items.remove(&cap_id);
                snap.item_positions.remove(&cap_id);
                snap.characters.get_mut(&90_000_001).unwrap().active_ship_id = ship_id;
            });
        }

        let _ = sys_handle
            .set_ball_position(ship_id, DVec3::new(17_500.0, -1_000.0, 4_000.0))
            .await;
        let jet_res = ctx
            .dispatch_rpc(
                &mut sess,
                "ship",
                None,
                "Jettison",
                &[PyRep::List(vec![PyRep::Integer(jettison_cargo_id)])],
                &empty,
                None,
            )
            .await
            .expect("ship.Jettison");
        let PyRep::Tuple(jet_tup) = jet_res else {
            panic!("expected Jettison Tuple");
        };
        let jetcan_id = match &jet_tup[0] { PyRep::List(l) => l[0].as_i64().unwrap(), _ => panic!("expected List") };

        // 3. Anchor both `Mobile Depot` (`33474`, group `1246`) at `[0, 0, 0]`
        //    and `Mobile Small Warp Disruptor I` (`12198`, group `361`) at `[50_000, 0, 0]`
        let _ = sys_handle.set_ball_position(ship_id, DVec3::ZERO).await;
        ctx.dispatch_rpc(
            &mut sess,
            "posMgr",
            None,
            "AnchorStructure",
            &[
                PyRep::Integer(depot_item_id),
                PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
            ],
            &empty,
            None,
        )
        .await
        .expect("AnchorStructure Mobile Depot");

        let _ = sys_handle
            .set_ball_position(ship_id, DVec3::new(50_000.0, 0.0, 0.0))
            .await;
        ctx.dispatch_rpc(
            &mut sess,
            "posMgr",
            None,
            "AnchorStructure",
            &[
                PyRep::Integer(mwd_item_id),
                PyRep::Tuple(vec![
                    PyRep::Real(50_000.0),
                    PyRep::Real(0.0),
                    PyRep::Real(0.0),
                ]),
            ],
            &empty,
            None,
        )
        .await
        .expect("AnchorStructure MWD bubble");

        // Complete anchoring delay (120s) so the Mobile Warp Disruptor activates its warp bubble at [50_000, 0, 0]
        ctx.advance_secs(120);
        let _ = sys_handle.set_ball_position(ship_id, DVec3::ZERO).await;
        assert_eq!(sys_handle.is_ball_scrambled(ship_id).await.unwrap(), false);
        let _ = sys_handle
            .set_ball_position(ship_id, DVec3::new(50_000.0, 0.0, 0.0))
            .await;
        assert_eq!(sys_handle.is_ball_scrambled(ship_id).await.unwrap(), true);

        // 4. Launch drone (`ship.LaunchDrones`) so it is in space when the server restarts
        let launch_res = ctx
            .dispatch_rpc(&mut sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(drone_item_id)])], &empty, None)
            .await
            .expect("LaunchDrones");
        let PyRep::List(launched_ids) = launch_res else { panic!("expected list of launched drone ball IDs") };
        let drone_ball_id = launched_ids[0].as_i64().expect("drone ball id");

        // Advance 1,800s (30m): jetcan (1h = 3600s) has 30m left; wreck (2h = 7200s) has 90m left
        ctx.advance_secs(1_800);

        // 5. Disconnect pilot 90_000_002 in space at [25_000, 3_000, 1_000] with an active 900s PvP combat timer
        let _ = sys_handle.set_ball_position(disc_ship_id, DVec3::new(25_000.0, 3_000.0, 1_000.0)).await;
        ctx.crimewatch.write().unwrap().get_or_create(90_000_002).pvp_timer_until_unix = ctx.now_unix() + 900;
        let _ = handle_character_logoff_or_disconnect(&ctx, 90_000_002).await;

        let ctx2 = persist_and_reopen_sqlite(&ctx, "sp7_restart_objects");
        (ctx2, sys_id, structure_id, wreck_id, dropped_loot_ids, jetcan_id, mwd_item_id, depot_item_id, drone_item_id, drone_ball_id, disc_ship_id)
    };

    // Restart server from SQLite (`ctx2`) at `T0 + 1920s`
    ctx2.set_clock(Arc::new(VirtualServerClock::with_fixed_base(TEST_CLOCK_BASE_UNIX + 1_920)));
    let sys_handle2 = ctx2.systems.get_or_spawn(sys_id);

    // Verify all persisted space objects were re-materialized into the fresh Ballpark at exact coordinates
    let (_, st_ball) = sys_handle2.get_ball_snapshot(structure_id).await.unwrap().expect("Upwell structure reloaded");
    assert_eq!(st_ball.type_id, 35_832);
    assert!(st_ball.position.distance(DVec3::new(120_000.0, 0.0, -80_000.0)) < 1.0);

    let (_, wr_ball) = sys_handle2.get_ball_snapshot(wreck_id).await.unwrap().expect("Unexpired wreck reloaded");
    assert!(wr_ball.position.distance(DVec3::new(15_000.0, 2_000.0, -5_000.0)) < 1.0);

    let (_, jc_ball) = sys_handle2.get_ball_snapshot(jetcan_id).await.unwrap().expect("Unexpired jetcan reloaded");
    assert_eq!(jc_ball.type_id, 23);
    assert!(jc_ball.position.distance(DVec3::new(18_000.0, -1_000.0, 4_000.0)) < 1.0);

    let (_, dp_ball) = sys_handle2.get_ball_snapshot(depot_item_id).await.unwrap().expect("Mobile Depot reloaded");
    assert_eq!(dp_ball.type_id, 33_474);
    assert!(dp_ball.position.distance(DVec3::ZERO) < 1.0);

    let (_, mwd_ball) = sys_handle2.get_ball_snapshot(mwd_item_id).await.unwrap().expect("MWD bubble reloaded");
    assert_eq!(mwd_ball.type_id, 12_198);
    assert!(mwd_ball.position.distance(DVec3::new(50_000.0, 0.0, 0.0)) < 1.0);

    let (_, dr_ball) = sys_handle2.get_ball_snapshot(drone_ball_id).await.unwrap().expect("Abandoned drone reloaded");
    assert_eq!(dr_ball.type_id, 2_454);

    let (_, ds_ball) = sys_handle2.get_ball_snapshot(disc_ship_id).await.unwrap().expect("Disconnected ship reloaded");
    assert!(ds_ball.position.distance(DVec3::new(25_000.0, 3_000.0, 1_000.0)) < 1.0);

    // Verify abandoned `DroneSpaceState` was seeded into `active_drones_in_space` on SQLite restart
    let drone_state = with_combat_runtime(&ctx2, |rt| {
        rt.active_drones_in_space.get(&drone_ball_id).map(|d| (d.drone_item_id, d.is_abandoned))
    });
    assert_eq!(drone_state, Some((drone_item_id, true)));

    // Pilot 90_000_001 reconnects, loots the restarted wreck (`invbroker.LootAll`), scoops the restarted drone,
    // and targets + docks/undocks at the restarted Upwell structure
    let (mut sess_re, _, ship_id2) = setup_pilot_in_space(&ctx2, 90_000_001).await;
    let empty = HashMap::new();
    let _ = sys_handle2.set_ball_position(ship_id2, DVec3::new(15_000.0, 2_000.0, -5_000.0)).await;
    ctx2.dispatch_rpc(&mut sess_re, "invbroker", None, "LootAll", &[PyRep::Integer(wreck_id)], &empty, None)
        .await
        .expect("LootAll on restarted wreck");
    ctx2.db.with_snapshot(|snap| {
        for &lid in &dropped_loot_ids {
            assert_eq!(snap.items[&lid].location_id, ship_id2, "Looted wreck item {lid} must transfer to ship cargo");
        }
    });

    let _ = sys_handle2.set_ball_position(ship_id2, DVec3::new(50_000.0, 0.0, 0.0)).await;
    assert_eq!(sys_handle2.is_ball_scrambled(ship_id2).await.unwrap(), true);
    ctx2.dispatch_rpc(&mut sess_re, "ship", None, "ScoopToDroneBay", &[PyRep::Integer(drone_ball_id)], &empty, None)
        .await
        .expect("ScoopToDroneBay on restarted drone");
    assert_eq!(ctx2.db.with_snapshot(|s| s.items[&drone_item_id].location_id), ship_id2);
    // Target, dock, and undock at the restarted structure (`feat:lifecycle.structure-restart`)
    let _ = sys_handle2.set_ball_position(ship_id2, DVec3::new(120_000.0, 0.0, -80_000.0)).await;
    ctx2.dispatch_rpc(&mut sess_re, "dogmaIM", None, "AddTarget", &[PyRep::Integer(structure_id)], &empty, None).await.expect("AddTarget on restarted structure");
    ctx2.advance_secs(12);
    ctx2.dispatch_rpc(&mut sess_re, "structureDocking", None, "Dock", &[PyRep::Integer(structure_id)], &empty, None).await.expect("Dock at restarted structure");
    assert_eq!(sess_re.structure_id, Some(structure_id));
    ctx2.advance_secs(12);
    ctx2.dispatch_rpc(&mut sess_re, "structureDocking", None, "Undock", &[], &empty, None).await.expect("Undock from restarted structure");
    assert_eq!(sess_re.structure_id, None);
    // Rival pilot 90_000_002 reconnects, locks, and attacks the restarted structure (`dogmaIM.Activate`)
    let (mut sess2_re, _, ship2_id_re) = setup_pilot_in_space(&ctx2, 90_000_002).await;
    ctx2.db.with_mut_snapshot(|snap| {
        let mut ammo = sp6_item(990_007_185, 185, 90_000_002, ship2_id_re, InventoryFlag::HiSlot0, HashMap::new());
        ammo.quantity = 100;
        ammo.singleton = false;
        snap.items.insert(990_007_185, ammo);
    });
    if let Ok(mut bp) = sys_handle2.ballpark.write() { let _ = bp.set_mode(ship2_id_re, MovementMode::Stop); }
    let _ = sys_handle2.set_ball_position(ship2_id_re, DVec3::new(120_500.0, 0.0, -80_000.0)).await;
    ctx2.dispatch_rpc(&mut sess2_re, "dogmaIM", None, "AddTarget", &[PyRep::Integer(structure_id)], &empty, None).await.expect("AddTarget structure");
    let _ = sys_handle2.step_manual(10.0).await;
    let atk_res = ctx2.dispatch_rpc(&mut sess2_re, "dogmaIM", None, "Activate", &[PyRep::Integer(100_000_006), PyRep::String("projectileFired".into()), PyRep::Integer(structure_id)], &empty, None).await.expect("Activate weapon on restarted structure");
    let PyRep::KeyVal(atk_kv) = atk_res else { panic!("expected Activate KeyVal") };
    assert!(atk_kv.get("appliedDamage").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);

    // Advance past jetcan 1h lifetime (`T0 + 3601s`) -> jetcan decays while wreck (2h) remains
    let _ = sys_handle2.set_ball_position(ship_id2, DVec3::new(50_000.0, 0.0, 0.0)).await;
    ctx2.advance_secs(1_700);
    assert_eq!(sys_handle2.get_ball_snapshot(jetcan_id).await.unwrap().is_none(), true);
    assert_eq!(sys_handle2.get_ball_snapshot(wreck_id).await.unwrap().is_some(), true);

    // Advance past wreck 2h lifetime (`T0 + 7201s`) -> wreck decays; MWD bubble (48h) still present
    ctx2.advance_secs(3_600);
    assert_eq!(sys_handle2.get_ball_snapshot(wreck_id).await.unwrap().is_none(), true);
    assert_eq!(sys_handle2.get_ball_snapshot(mwd_item_id).await.unwrap().is_some(), true);

    // Advance past Mobile Warp Disruptor 2-day (`172,800s`) lifetime -> MWD ball and warp bubble decay
    ctx2.advance_secs(172_800);
    assert_eq!(sys_handle2.get_ball_snapshot(mwd_item_id).await.unwrap().is_none(), true);
    assert_eq!(sys_handle2.is_ball_scrambled(ship_id2).await.unwrap(), false);
}

#[tokio::test]
async fn test_sp7_downtime_cleans_npc_wrecks_and_respawns_accepted_mission_waves() {
    // parity: feat:lifecycle.wreck-downtime, feat:lifecycle.mission-downtime-respawn, feat:serverops.downtime, feat:serverops.daily-jobs
    eve_testkit::attest(&[
        "feat:lifecycle.wreck-downtime",
        "feat:lifecycle.mission-downtime-respawn",
        "feat:serverops.downtime",
        "feat:serverops.daily-jobs",
    ]);

    let ctx = ServerContext::new();
    let day_start = TEST_CLOCK_BASE_UNIX - (TEST_CLOCK_BASE_UNIX % 86_400);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(
        day_start + 11 * 3_600 - 5,
    )));
    let (mut sess, sys_id, _ship_id) = setup_pilot_in_space(&ctx, 90_000_001).await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    let npc_wreck_id = 993_000_001_i64;
    let player_wreck_id = 993_000_002_i64;
    let pre_dt_npc_id = 888_111_001_i64;
    let now_unix = ctx.now_unix();
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: pre_dt_npc_id, type_id: 10_019, position: DVec3::new(30_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 2.0, mass: 1_000_000.0,
                radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            npc_wreck_id,
            sp6_item(npc_wreck_id, 26_557, 90_000_001, sys_id as i64, InventoryFlag::Cargo, HashMap::new()),
        );
        snap.item_positions.insert(npc_wreck_id, [10_000.0, 0.0, 0.0]);
        record_wreck_or_container_meta_in_snap(snap, npc_wreck_id, now_unix, true);

        snap.items.insert(
            player_wreck_id,
            sp6_item(player_wreck_id, 26_557, 90_000_001, sys_id as i64, InventoryFlag::Cargo, HashMap::new()),
        );
        snap.item_positions.insert(player_wreck_id, [20_000.0, 0.0, 0.0]);
        record_wreck_or_container_meta_in_snap(snap, player_wreck_id, now_unix, false);

        // Partially-cleared ("Accepted") mission with 1 of 4 NPCs remaining
        snap.active_missions.insert(
            (90_000_001, 3_019_001),
            ActiveMissionRecord {
                mission_id: 500_001,
                character_id: 90_000_001,
                agent_id: 3_019_001,
                solar_system_id: sys_id,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 1,
                spawned_npc_ball_ids: vec![pre_dt_npc_id],
            },
        );
    });

    // Advance across 11:00 UTC (`10:59:55 -> 11:00:05 UTC`) so downtime cleans NPC wrecks,
    // despawns surviving pre-downtime mission NPCs, and resets mission npcs_remaining to 4
    tick_background_cluster(&ctx).await;
    ctx.advance_secs(10);
    tick_background_cluster(&ctx).await;

    assert!(sys_handle.get_ball_snapshot(pre_dt_npc_id).await.unwrap().is_none());
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&npc_wreck_id));
        assert!(snap.items.contains_key(&player_wreck_id));
        let mission = &snap.active_missions[&(90_000_001, 3_019_001)];
        assert_eq!(mission.npcs_remaining, 4);
        assert!(mission.spawned_npc_ball_ids.is_empty());
    });

    // Pilot warps back to the mission pocket after downtime -> initial 4-NPC wave spawns on pocket entry
    let beyonce_oid = ctx.bind_object(&sess, "beyonce", sys_id as i64).expect("bind beyonce");
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_oid),
        "CmdWarpToStuff",
        &[PyRep::String("mission".into()), PyRep::Integer(500_001)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("CmdWarpToStuff into mission pocket after downtime");

    let respawned_balls = sys_handle.list_balls().await.unwrap();
    let mission_ball_ids = ctx.db.with_snapshot(|s| {
        s.active_missions[&(90_000_001, 3_019_001)]
            .spawned_npc_ball_ids
            .clone()
    });
    assert_eq!(mission_ball_ids.len(), 4);
    for bid in mission_ball_ids {
        assert!(
            respawned_balls.iter().any(|b| b.id == bid),
            "Respawned mission NPC ball {bid} must be present in Ballpark"
        );
        assert!(
            with_combat_runtime(&ctx, |rt| rt.npc_entities.contains_key(&bid)),
            "Respawned mission NPC ball {bid} must be registered in COMBAT_RUNTIME.npc_entities"
        );
    }
}
