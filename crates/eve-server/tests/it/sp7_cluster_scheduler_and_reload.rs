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
    CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag, SkillQueueEntry,
    TrainedSkillRecord, VirtualServerClock,
};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode, SlimItemMeta};
use eve_proto::PyRep;
use eve_server::combat::death_pipeline::execute_death_pipeline;
use eve_server::router::ServerContext;
use eve_server::services::mission_fixture::{self, MissionFixture};
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
                corp_id: 1_000_009, alliance_id: -1, security_status: 0.0, is_cloaked: false, ..Default::default()
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
        snap.skills.entry(90_000_001).or_default().insert(
            3_300,
            eve_db::TrainedSkillRecord {
                type_id: 3_300,
                level: 0,
                skill_points: 0.0,
            },
        );
        snap.skill_queues.insert(90_000_001, vec![SkillQueueEntry {
            type_id: 3_300, target_level: 1, start_sp: 249.9, end_sp: 250.0,
            duration_seconds: 1.0, started_at_unix: Some(now0),
        }]);
        if let Some(crim) = snap.characters.get_mut(&90_000_002) {
            crim.station_id = None;
            crim.structure_id = None;
            crim.solar_system_id = sys_id;
            crim.active_ship_id = 100_000_002;
        }
        snap.items.insert(
            100_000_002,
            sp6_item(100_000_002, 587, 90_000_002, sys_id as i64, InventoryFlag::None, HashMap::new()),
        );
    });
    let _ = sys_handle
        .spawn_ball_with_meta(
            Ball {
                id: 100_000_002, type_id: 587, position: DVec3::new(91_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 2.0,
                mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta {
                group_id: 25, category_id: 6, owner_id: 90_000_002, char_id: Some(90_000_002),
                corp_id: 1_000_009, alliance_id: -1, security_status: 0.0, is_cloaked: false, ..Default::default()
            },
        )
        .await;
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
        assert!(!snap.skill_queues.contains_key(&90_000_001));
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
        let prev_station = sess.station_id;
        let dock_st = prev_station.unwrap_or(60_003_760);
        sess.station_id = Some(dock_st);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.station_id = Some(dock_st);
            }
        });
        ctx.dispatch_rpc(
            &mut sess,
            "corpmgr",
            None,
            "createcorporation",
            &[
                PyRep::String("SP7 Syndicate".into()),
                PyRep::String("SP7".into()), // rpc-lint: allow SP7 ticker string in test corp creation
                PyRep::Real(0.05),
            ],
            &empty,
            None,
        )
        .await
        .expect("createcorporation");
        sess.station_id = prev_station;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.station_id = prev_station;
            }
        });
        ctx.db.with_mut_snapshot(|snap| {
            for g in snap.corp_governance.values_mut() {
                g.created_unix = 1;
            }
        });
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
    assert!(st_ball.position.distance(DVec3::new(180_000.0, 0.0, -80_000.0)) < 1.0);

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
    let drone_state = eve_server::services::drone_test_helpers::active_drone(&ctx2, drone_ball_id)
        .map(|d| (d.drone_item_id, d.is_abandoned));
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
    let _ = sys_handle2.set_ball_position(ship_id2, DVec3::new(180_000.0, 0.0, -80_000.0)).await;
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
    sys_handle2
        .test_with_ballpark_mut_async(move |bp| {
            let _ = bp.set_mode(ship2_id_re, MovementMode::Stop);
        })
        .await;
    let _ = sys_handle2.set_ball_position(ship2_id_re, DVec3::new(180_500.0, 0.0, -80_000.0)).await;
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
    // parity: feat:lifecycle.wreck-downtime, feat:lifecycle.mission-downtime-respawn
    eve_testkit::attest(&[
        "feat:lifecycle.wreck-downtime",
        "feat:lifecycle.mission-downtime-respawn",
    ]);

    let ctx = ServerContext::new();
    let day_start = TEST_CLOCK_BASE_UNIX - (TEST_CLOCK_BASE_UNIX % 86_400);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(
        day_start + 11 * 3_600 - 5,
    )));
    let (_sess, sys_id, _ship_id) = setup_pilot_in_space(&ctx, 90_000_001).await;
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
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(90_000_001, 500_001, 3_019_001, sys_id)
                .current_room_id(1)
                .npcs_remaining(1)
                .spawned_npc_ball_ids(vec![pre_dt_npc_id]),
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
        let mission = mission_fixture::get(snap, 90_000_001, 500_001).expect("mission preserved");
        assert_eq!(mission.npcs_remaining, 4);
        assert!(mission.spawned_npc_ball_ids.is_empty());
    });

    // Pilot reconnects after downtime and warps back to the mission pocket -> initial 4-NPC wave spawns on pocket entry
    ctx.advance_secs(905); // Advance past 11:00–11:15 UTC downtime window before warping
    tick_background_cluster(&ctx).await;
    let mut sess = eve_machonet::SessionState::default();
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID after downtime");
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
        mission_fixture::get(s, 90_000_001, 500_001)
            .expect("mission preserved")
            .spawned_npc_ball_ids
    });
    assert_eq!(mission_ball_ids.len(), 4);
    for bid in mission_ball_ids {
        assert!(
            respawned_balls.iter().any(|b| b.id == bid),
            "Respawned mission NPC ball {bid} must be present in Ballpark"
        );
        assert!(
            eve_server::combat::npc_ai::npc_test_helpers::has_npc_entity(&ctx, bid),
            "Respawned mission NPC ball {bid} must be registered in NPC runtime"
        );
    }
}

#[tokio::test]
async fn test_sqlite_restart_abandons_launched_engaged_drones_and_allows_reconnect_or_scoop() {
    struct TempSqliteCleanup(std::path::PathBuf);
    impl Drop for TempSqliteCleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            let _ = std::fs::remove_file(std::path::PathBuf::from(format!("{}-wal", self.0.display())));
            let _ = std::fs::remove_file(std::path::PathBuf::from(format!("{}-shm", self.0.display())));
        }
    }

    let db_path = std::env::temp_dir().join(format!(
        "evomu_sp7_sqlite_drone_restart_{}.sqlite",
        std::process::id()
    ));
    let _guard = TempSqliteCleanup(db_path.clone());
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(std::path::PathBuf::from(format!("{}-wal", db_path.display())));
    let _ = std::fs::remove_file(std::path::PathBuf::from(format!("{}-shm", db_path.display())));

    let sde = ServerContext::new().sde;
    let empty = HashMap::new();
    let d1_item = 880_950_001i64;
    let d2_item = 880_950_002i64;
    let target_npc_id = 990_950_001i64;
    let other_sys = 30_000_144u32;
    let d_other_ball = 880_950_099i64;

    let (sys_id, ship_id, d1_ball, d2_ball) = {
        let repo1 =
            Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("open_sqlite 1"));
        repo1.with_mut_snapshot(|snap| {
            *snap = eve_db::WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        });
        let ctx1 = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&repo1));
        pin_clock(&ctx1);
        let (mut sess, sys_id, ship_id) = setup_pilot_in_space(&ctx1, 90_000_001).await;
        ctx1.db.with_mut_snapshot(|snap| {
            if let Some(sh) = snap.items.get_mut(&ship_id) {
                sh.type_id = 626; // Vexor
            }
            for stid in [3_435_u32, 3_436_u32, 3_434_u32] {
                snap.skills.entry(90_000_001).or_default().insert(
                    stid,
                    TrainedSkillRecord {
                        type_id: stid,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
            snap.items.insert(
                d1_item,
                sp6_item(d1_item, 2_454, 90_000_001, ship_id, InventoryFlag::DroneBay, HashMap::new()),
            );
            snap.items.insert(
                d2_item,
                sp6_item(d2_item, 2_454, 90_000_001, ship_id, InventoryFlag::DroneBay, HashMap::new()),
            );
        });
        ctx1.invalidate_ship_fitting(ship_id);

        let sys_handle = ctx1.systems.get_or_spawn(sys_id);
        let ship_pos = sys_handle
            .get_ball_snapshot(ship_id)
            .await
            .unwrap()
            .expect("pilot ship ball")
            .1
            .position;
        let launch_rep = ctx1
            .dispatch_rpc(
                &mut sess,
                "ship",
                None,
                "LaunchDrones",
                &[PyRep::List(vec![PyRep::Integer(d1_item), PyRep::Integer(d2_item)])],
                &empty,
                None,
            )
            .await
            .expect("LaunchDrones");
        let PyRep::List(launched) = launch_rep else {
            panic!("expected List from LaunchDrones, got {launch_rep:?}");
        };
        let d1_ball = launched[0].as_i64().unwrap();
        let d2_ball = launched[1].as_i64().unwrap();

        let _ = sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: target_npc_id,
                    type_id: 587,
                    position: ship_pos + DVec3::new(1_500.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 100.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState::default(),
            )
            .await
            .unwrap();
        ctx1.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "AddTarget",
            &[PyRep::Integer(target_npc_id)],
            &empty,
            None,
        )
        .await
        .expect("AddTarget");
        for _ in 0..20 {
            if sys_handle
                .get_target_locks(ship_id)
                .await
                .unwrap()
                .iter()
                .any(|l| l.target_id == target_npc_id && l.locked)
            {
                break;
            }
            let _ = sys_handle.step_manual(1.0).await;
        }
        ctx1.dispatch_rpc(
            &mut sess,
            "entity",
            None,
            "CmdEngage",
            &[
                PyRep::List(vec![PyRep::Integer(d1_ball)]),
                PyRep::Integer(target_npc_id),
            ],
            &empty,
            None,
        )
        .await
        .expect("CmdEngage d1");

        let pre_d1 = eve_server::services::drone_test_helpers::active_drone(&ctx1, d1_ball)
            .expect("d1 active before restart");
        assert!(!pre_d1.is_abandoned);
        assert_eq!(pre_d1.owner_char_id, 90_000_001);
        assert_eq!(pre_d1.parent_ship_id, ship_id);
        assert_eq!(pre_d1.activity_state, 1);
        assert_eq!(pre_d1.target_id, Some(target_npc_id));

        // Also seed a controlled checkpointed drone with custom stats in another system without `snap.items`.
        let h_other = ctx1.systems.get_or_spawn(other_sys);
        h_other
            .upsert_active_drone(eve_server::services::combat_crimewatch::DroneSpaceState {
                ball_id: d_other_ball,
                drone_item_id: d_other_ball,
                type_id: 2_454,
                group_id: 100,
                owner_char_id: 90_000_001,
                original_owner_char_id: 90_000_001,
                parent_ship_id: ship_id,
                solar_system_id: other_sys,
                target_id: Some(target_npc_id),
                activity_state: 1,
                assist_char_id: Some(90_000_002),
                guard_char_id: Some(90_000_003),
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 1_500.0,
                optimal_range: 5_000.0,
                damage_vector: DamageVector {
                    em: 42.0,
                    explosive: 0.0,
                    kinetic: 0.0,
                    thermal: 0.0,
                },
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 1.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            })
            .await
            .unwrap();

        assert!(ctx1.systems.stop_system(sys_id).await);
        assert!(ctx1.systems.stop_system(other_sys).await);

        // Simulate crash-window stale `KIND_ACTIVE_DRONES` rows for a drone already scooped to bay
        // and a drone recorded as recently destroyed before shutdown.
        let d_scooped_stale = 880_950_097i64;
        let d_dead_stale = 880_950_098i64;
        ctx1.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                d_scooped_stale,
                sp6_item(
                    d_scooped_stale,
                    2_454,
                    90_000_001,
                    ship_id,
                    InventoryFlag::DroneBay,
                    HashMap::new(),
                ),
            );
            snap.combat_crimewatch_state
                .record_destroyed_item_id(d_dead_stale);
        });
        let now_stale = ctx1.db.clock().now_unix_secs();
        ctx1.db
            .with_sqlite_conn(|conn| {
                let mut rows = eve_db::sqlite_store::system_checkpoint::load_system_checkpoints::<
                    eve_server::services::combat_crimewatch::DroneSpaceState,
                >(
                    conn,
                    sys_id,
                    eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES,
                )?;
                rows.push((
                    d_scooped_stale.to_string(),
                    eve_server::services::combat_crimewatch::DroneSpaceState {
                        ball_id: d_scooped_stale,
                        drone_item_id: d_scooped_stale,
                        ..pre_d1.clone()
                    },
                ));
                rows.push((
                    d_dead_stale.to_string(),
                    eve_server::services::combat_crimewatch::DroneSpaceState {
                        ball_id: d_dead_stale,
                        drone_item_id: d_dead_stale,
                        ..pre_d1.clone()
                    },
                ));
                eve_db::sqlite_store::system_checkpoint::replace_system_checkpoints_for_kind(
                    conn,
                    sys_id,
                    eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES,
                    &rows,
                    now_stale,
                )
            })
            .expect("seed stale crash-window drone checkpoints");

        (sys_id, ship_id, d1_ball, d2_ball)
    };

    // Real SQLite server restart (`WorldRepository::open_sqlite` + `ServerContext::with_sde_and_repository`).
    let repo2 = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("open_sqlite 2"));
    let ctx2 = ServerContext::with_sde_and_repository(sde, Arc::clone(&repo2));
    pin_clock(&ctx2);

    assert!(
        eve_server::services::drone_test_helpers::active_drone(&ctx2, 880_950_097).is_none(),
        "stale crash-window checkpoint for DroneBay item must not resurrect"
    );
    assert!(
        eve_server::services::drone_test_helpers::active_drone(&ctx2, 880_950_098).is_none(),
        "stale crash-window checkpoint for destroyed drone must not resurrect"
    );

    // Launched + engaged drones from `sys_id` must restore abandoned (never controlled for offline pilot).
    for bid in [d1_ball, d2_ball] {
        let d = eve_server::services::drone_test_helpers::active_drone(&ctx2, bid)
            .unwrap_or_else(|| panic!("drone {bid} must restore across SQLite restart"));
        assert!(d.is_abandoned, "drone {bid} must restore abandoned");
        assert_eq!(d.owner_char_id, 0);
        assert_eq!(d.parent_ship_id, 0);
        assert_eq!(d.target_id, None);
        assert_eq!(d.activity_state, 0);
        assert_eq!(d.assist_char_id, None);
        assert_eq!(d.guard_char_id, None);
        assert_eq!(d.original_owner_char_id, 90_000_001);
    }

    // Checkpointed-only cold `other_sys` stays cold until entered, then restores abandoned preserving stats.
    assert!(
        ctx2.systems.get_handle(other_sys).is_none(),
        "cold checkpointed system {other_sys} must not wake prematurely at startup"
    );
    let h_other2 = ctx2.systems.get_or_spawn(other_sys);
    let d_other = h_other2
        .active_drone(d_other_ball)
        .expect("checkpointed drone in other_sys must restore when woken");
    assert!(d_other.is_abandoned);
    assert_eq!(d_other.owner_char_id, 0);
    assert_eq!(d_other.parent_ship_id, 0);
    assert_eq!(d_other.target_id, None);
    assert_eq!(d_other.activity_state, 0);
    assert_eq!(d_other.assist_char_id, None);
    assert_eq!(d_other.guard_char_id, None);
    assert_eq!(d_other.original_owner_char_id, 90_000_001);
    assert!((d_other.damage_vector.em - 42.0).abs() < 1e-6);

    // Pilot reconnects, regains control of `d1_ball`, and scoops `d2_ball`.
    let (mut sess2, _, ship_id2) = setup_pilot_in_space(&ctx2, 90_000_001).await;
    assert_eq!(ship_id2, ship_id);
    let sys2 = ctx2.systems.get_or_spawn(sys_id);
    let d1_pos = sys2
        .get_ball_snapshot(d1_ball)
        .await
        .unwrap()
        .expect("d1 ball after restart")
        .1
        .position;
    let _ = sys2.set_ball_position(ship_id2, d1_pos + DVec3::new(100.0, 0.0, 0.0)).await;

    ctx2.dispatch_rpc(
        &mut sess2,
        "entity",
        None,
        "CmdReconnectToDrones",
        &[PyRep::List(vec![PyRep::Integer(d1_ball)])],
        &empty,
        None,
    )
    .await
    .expect("CmdReconnectToDrones after restart");
    let reconnected_d1 = eve_server::services::drone_test_helpers::active_drone(&ctx2, d1_ball)
        .expect("reconnected d1");
    assert!(!reconnected_d1.is_abandoned);
    assert_eq!(reconnected_d1.owner_char_id, 90_000_001);
    assert_eq!(reconnected_d1.parent_ship_id, ship_id2);

    let d2_pos = sys2
        .get_ball_snapshot(d2_ball)
        .await
        .unwrap()
        .expect("d2 ball after restart")
        .1
        .position;
    let _ = sys2.set_ball_position(ship_id2, d2_pos + DVec3::new(100.0, 0.0, 0.0)).await;
    ctx2.dispatch_rpc(
        &mut sess2,
        "ship",
        None,
        "ScoopToDroneBay",
        &[PyRep::Integer(d2_ball)],
        &empty,
        None,
    )
    .await
    .expect("ScoopToDroneBay after restart");
    assert_eq!(
        ctx2.db.with_snapshot(|s| (s.items[&d2_item].location_id, s.items[&d2_item].flag)),
        (ship_id2, InventoryFlag::DroneBay)
    );
}
