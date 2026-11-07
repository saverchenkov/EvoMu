//! Non-GM E2E integration tests for Sovereignty ADM, Dev Indices, Entosis, Command Nodes, Skyhooks & Warp Vector.
#![forbid(unsafe_code)]
#![allow(dead_code)]

use crate::parity_shard_3_closure_helpers::*;
use crate::parity_closure_sov_hub_and_upgrade_sites::{
    first_claimable_null_sys, setup_player_alliance, ENTOSIS_LINK_I_TYPE, HURRICANE_TYPE,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_proto::PyRep;
use eve_server::services::sov_gameplay::{
    advance_skyhook_reagents_in_snap, record_sov_system_activity, SKYHOOK_MATURITY_CYCLE_SECS,
};
use eve_server::services::sov_services::{
    calculate_adm, TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB,
    TYPE_SUPERIONIC_ICE,
};
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;

// parity: feat:sovereignty.sov-adm
// parity: feat:sovereignty.sov-dev-indices
// parity: feat:sovereignty.sov-entosis
// parity: feat:sovereignty.sov-command-nodes
// parity: feat:sovereignty.sov-changed-notifications
#[tokio::test]
async fn test_e2e_sov_adm_dev_indices_entosis_counter_and_command_nodes() {
    eve_testkit::attest(&[
        "feat:sovereignty.sov-adm",
        "feat:sovereignty.sov-dev-indices",
        "feat:sovereignty.sov-entosis",
        "feat:sovereignty.sov-command-nodes",
        "feat:sovereignty.sov-changed-notifications",
    ]);

    let (db_path, ctx) = new_sqlite_ctx("sov_entosis");
    crate::common::pin_clock(&ctx);

    let mut def = spawn_player_combat_pilot(
        &ctx,
        82_002,
        "sov_def_p1",
        "Sov Defender",
        HURRICANE_TYPE,
        true,
    )
    .await;
    let (_, def_alliance) =
        setup_player_alliance(&ctx, &mut def, "Def Corp", "DEFC", "Defenders Union", "DEFU").await;
    let mut atk = spawn_player_combat_pilot(
        &ctx,
        82_003,
        "sov_atk_p1",
        "Sov Attacker",
        HURRICANE_TYPE,
        true,
    )
    .await;
    let (_, atk_alliance) =
        setup_player_alliance(&ctx, &mut atk, "Atk Corp", "ATKC", "Invaders Horde", "INVH").await;
    ctx.advance_secs(8 * 86_400);

    let sys_id = first_claimable_null_sys(&ctx);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&def.char_id).unwrap().solar_system_id = sys_id;
        s.characters.get_mut(&atk.char_id).unwrap().solar_system_id = sys_id;
        s.next_entity_id += 1;
        let hid = s.next_entity_id;
        s.items.insert(
            hid,
            InventoryItemRecord {
                item_id: hid,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_id: def.char_id,
                location_id: def.ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    def.sess.solar_system_id = Some(sys_id);
    atk.sess.solar_system_id = Some(sys_id);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for (sid_ship, x) in [(def.ship_id, 100_000.0), (atk.ship_id, 105_000.0)] {
        let _ = sys_h
            .spawn_ball(eve_destiny::Ball {
                id: sid_ship,
                type_id: HURRICANE_TYPE,
                position: DVec3::new(x, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 3.0,
                mass: 1.5e7,
                radius: 200.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await;
    }

    let hub_item = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .find(|i| i.location_id == def.ship_id && i.type_id == TYPE_SOVEREIGNTY_HUB)
            .unwrap()
            .item_id
    });
    let anchor_res = call_rpc(
        &ctx,
        &mut def.sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(hub_item), PyRep::String("Defended Sov Hub".into())],
    )
    .await
    .expect("Anchor defended Hub");
    let hub_id = anchor_res.dict_get("structureID").and_then(PyRep::as_i64).expect("hub_id");
    ctx.advance_secs(87_400);
    tick_background_cluster(&ctx).await;

    // 1. Build Military & Industrial Dev Indices -> verify ADM & OnSolarSystemDevIndexChanged
    let _ = ctx.drain_notifications_for_char(def.char_id);
    record_sov_system_activity(&ctx, sys_id, 20, 6_000.0);
    let (mil_idx, ind_idx, strat_idx) = ctx.db.with_snapshot(|s| {
        let st = &s.system_states[&sys_id];
        (st.military_index, st.industrial_index, st.strategic_index)
    });
    let adm = calculate_adm(mil_idx, ind_idx, strat_idx);
    assert!(adm >= 2.0, "Military + Industrial dev indices must raise ADM >= 2.0 (got {adm})");
    let dev_notifs = ctx.drain_notifications_for_char(def.char_id);
    assert!(
        dev_notifs.iter().any(|n| n.event == "OnSolarSystemDevIndexChanged"),
        "Raising dev indices must emit OnSolarSystemDevIndexChanged (got {:?})",
        dev_notifs.iter().map(|n| n.event.as_str()).collect::<Vec<_>>()
    );

    // 2. Align clock into hub's vulnerability window, start attacker Entosis Link, and verify
    //    defender counter-entosis pauses capture progress!
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|s| {
        let cur_hour = (now / 3_600) % 24;
        let cur_day = (now / 86_400) % 7;
        s.structure_vulnerability_windows.insert(hub_id, (cur_hour, cur_day));
    });
    let atk_ent = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let stront_id = s.next_entity_id;
        s.items.insert(
            stront_id,
            InventoryItemRecord {
                item_id: stront_id,
                type_id: 16_275, // Strontium Clathrates consumed by Entosis Link cycles
                owner_id: atk.char_id,
                location_id: atk.ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2_000,
                ..Default::default()
            },
        );
        s.items
            .values()
            .find(|i| i.location_id == atk.ship_id && i.type_id == ENTOSIS_LINK_I_TYPE)
            .unwrap()
            .item_id
    });
    let def_ent = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .find(|i| i.location_id == def.ship_id && i.type_id == ENTOSIS_LINK_I_TYPE)
            .unwrap()
            .item_id
    });

    move_player_near_ball(&ctx, &mut atk.sess, sys_id, atk.ship_id, hub_id, 5_000.0).await;
    move_player_near_ball(&ctx, &mut def.sess, sys_id, def.ship_id, hub_id, 6_000.0).await;

    call_rpc(
        &ctx,
        &mut atk.sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(atk_ent), PyRep::String("default".into()), PyRep::Integer(hub_id)],
    )
    .await
    .expect("Attacker activate Entosis Link");
    ctx.advance_secs(390);
    tick_background_cluster(&ctx).await;
    let prog_uncontested = ctx
        .db
        .with_snapshot(|s| s.sov_runtime.entosis_capture_progress.get(&hub_id).map(|e| e.3))
        .unwrap_or(0.0);
    assert!(prog_uncontested > 0.0, "Attacker Entosis progress advances when uncontested");

    // Defender activates counter-entosis link -> progress pauses!
    call_rpc(
        &ctx,
        &mut def.sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(def_ent), PyRep::String("default".into()), PyRep::Integer(hub_id)],
    )
    .await
    .expect("Defender counter-entosis Activate");
    ctx.advance_secs(60);
    tick_background_cluster(&ctx).await;
    let prog_contested = ctx
        .db
        .with_snapshot(|s| s.sov_runtime.entosis_capture_progress.get(&hub_id).map(|e| e.3))
        .unwrap_or(0.0);
    assert_eq!(
        prog_contested, prog_uncontested,
        "Defender counter-entosis must pause attacker Hub progress"
    );

    // Deactivate defender link & complete capture -> Hub enters HullReinforced for 48h, command nodes spawn after 48h!
    call_rpc(
        &ctx,
        &mut def.sess,
        "dogmaIM",
        "Deactivate",
        &[PyRep::Integer(def_ent), PyRep::String("default".into())],
    )
    .await
    .expect("Deactivate defender link");
    for _ in 0..80 {
        ctx.advance_secs(60);
        tick_background_cluster(&ctx).await;
        if ctx.db.with_snapshot(|s| s.structures[&hub_id].state == "HullReinforced") {
            break;
        }
    }
    assert_eq!(
        ctx.db.with_snapshot(|s| s.structures[&hub_id].state.clone()),
        "HullReinforced",
        "Completed Hub Entosis enters 48h HullReinforced before command nodes"
    );
    let nodes_early = ctx.db.with_snapshot(|s| {
        s.sov_runtime
            .command_nodes
            .values()
            .filter(|&&(hid, _, _)| hid == hub_id)
            .count()
    });
    assert_eq!(nodes_early, 0, "Command nodes must not spawn before 48h reinforcement expires");

    ctx.advance_secs(48 * 3_600 + 60);
    let now2 = ctx.now_unix();
    ctx.db.with_mut_snapshot(|s| {
        let cur_hour = (now2 / 3_600) % 24;
        let cur_day = (now2 / 86_400) % 7;
        s.structure_vulnerability_windows.insert(hub_id, (cur_hour, cur_day));
    });
    tick_background_cluster(&ctx).await;

    // Capture Command Nodes with attacker Entosis Link until sovereignty flips!
    let advance_avoiding_downtime = |secs: u64| {
        ctx.advance_secs(secs);
        let dt_sec = ctx.systems.downtime_utc_secs_of_day();
        let since_dt = (86_400 + (ctx.now_unix() % 86_400) - dt_sec) % 86_400;
        if since_dt <= 1_000 {
            ctx.advance_secs(1_050 - since_dt);
        }
    };
    let _ = ctx.drain_notifications_for_char(def.char_id);
    let _ = ctx.drain_notifications_for_char(atk.char_id);
    for _ in 0..20 {
        if ctx.db.with_snapshot(|s| s.system_states[&sys_id].sov_alliance_id) == Some(atk_alliance) {
            break;
        }
        let next_node = ctx.db.with_snapshot(|s| {
            s.sov_runtime
                .command_nodes
                .iter()
                .find(|(_, &(hid, _, _))| hid == hub_id)
                .map(|(&nid, &(_, nsys, _))| (nid, nsys))
        });
        let Some((nid, nsys)) = next_node else {
            tick_background_cluster(&ctx).await;
            continue;
        };
        ctx.db.with_mut_snapshot(|s| {
            let ch = s.characters.get_mut(&atk.char_id).unwrap();
            ch.solar_system_id = nsys;
            ch.station_id = None;
            let ship = s.items.get_mut(&atk.ship_id).unwrap();
            ship.location_id = i64::from(nsys);
            ship.flag = InventoryFlag::Hangar;
        });
        atk.sess.solar_system_id = Some(nsys);
        atk.sess.station_id = None;
        let nsh = ctx.systems.get_or_spawn(nsys);
        let npos = nsh.get_ball_snapshot(nid).await.ok().flatten().map(|(_, b)| b.position).unwrap_or(DVec3::ZERO);
        if nsh.get_ball_snapshot(atk.ship_id).await.unwrap().is_none() {
            let _ = nsh
                .spawn_ball(eve_destiny::Ball {
                    id: atk.ship_id,
                    type_id: HURRICANE_TYPE,
                    position: npos + DVec3::new(3_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 200.0,
                    inertia: 3.0,
                    mass: 1.5e7,
                    radius: 200.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                })
                .await;
        } else {
            nsh.set_ball_position(atk.ship_id, npos + DVec3::new(3_000.0, 0.0, 0.0)).await;
        }
        advance_avoiding_downtime(0);
        call_rpc(
            &ctx,
            &mut atk.sess,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(atk_ent), PyRep::String("default".into()), PyRep::Integer(nid)],
        )
        .await
        .expect("Capture Command Node");
        for _ in 0..8 {
            advance_avoiding_downtime(300);
            tick_background_cluster(&ctx).await;
            if !ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.contains_key(&nid)) {
                break;
            }
        }
    }

    assert_ne!(def_alliance, atk_alliance, "Attacker and defender alliances are distinct");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.system_states[&sys_id].sov_alliance_id),
        None,
        "Winning command nodes destroys defender Sov Hub and clears system sovereignty"
    );
    let sov_notifs = ctx.drain_notifications_for_char(atk.char_id);
    assert!(
        sov_notifs.iter().any(|n| n.event == "OnSovereigntyChanged" || n.event == "OnSolarSystemSovStructuresUpdated"),
        "Sovereignty unclaim emits OnSovereigntyChanged / OnSolarSystemSovStructuresUpdated"
    );
    let atk_hub_item = ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&atk.char_id).unwrap().solar_system_id = sys_id;
        s.next_entity_id += 1;
        let hid = s.next_entity_id;
        s.items.insert(
            hid,
            InventoryItemRecord {
                item_id: hid,
                type_id: eve_server::services::sov_services::TYPE_SOVEREIGNTY_HUB,
                owner_id: atk.char_id,
                location_id: atk.ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
        hid
    });
    atk.sess.solar_system_id = Some(sys_id);
    let _ = sys_h
        .spawn_ball(eve_destiny::Ball {
            id: atk.ship_id,
            type_id: HURRICANE_TYPE,
            position: DVec3::new(150_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 3.0,
            mass: 1.5e7,
            radius: 200.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    call_rpc(
        &ctx,
        &mut atk.sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(atk_hub_item), PyRep::String("Invader Sov Hub".into())],
    )
    .await
    .expect("Invader anchors Sovereignty Hub in conquered system");
    ctx.advance_secs(86_400);
    tick_background_cluster(&ctx).await;
    assert_eq!(
        ctx.db.with_snapshot(|s| s.system_states[&sys_id].sov_alliance_id),
        Some(atk_alliance),
        "Anchoring Sovereignty Hub after winning command nodes transfers system sovereignty to invading alliance"
    );

    let ctx2 = reload_ctx_from_sqlite(&ctx, &db_path);
    assert_eq!(
        ctx2.db.with_snapshot(|s| s.system_states[&sys_id].sov_alliance_id),
        Some(atk_alliance),
        "Captured alliance ownership persists across SQLite reload"
    );
    let _ = bound_id_of;
    let _ = call_bound_rpc;
}

// parity: feat:sovereignty.skyhook-reagent-maturity
// parity: feat:navigation.warp-vector
#[tokio::test]
async fn test_e2e_skyhook_reagent_maturity_and_warp_vector() {
    eve_testkit::attest(&[
        "feat:sovereignty.skyhook-reagent-maturity",
        "feat:navigation.warp-vector",
    ]);

    let (db_path, ctx) = new_sqlite_ctx("skyhook_vector");
    crate::common::pin_clock(&ctx);

    let mut pilot = spawn_player_combat_pilot(
        &ctx,
        82_004,
        "sky_vec_p1",
        "Skyhook Corsair",
        HURRICANE_TYPE,
        false,
    )
    .await;
    let (_, alliance_id) =
        setup_player_alliance(&ctx, &mut pilot, "Reagent Raiders", "REAG", "Outer Syndicate", "OSYN").await;
    ctx.advance_secs(8 * 86_400);

    // Find a null-sec system with an Ice (12) or Lava (2015) planet
    let (sys_id, planet_id, planet_pos) = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| eve_server::services::sov_gameplay::is_claimable_nullsec_system(&ctx.sde, s.solar_system_id))
        .find_map(|s| {
            ctx.sde
                .celestials_in_system(s.solar_system_id)
                .into_iter()
                .find(|c| matches!(c.type_id, 12 | 2014 | 2015))
                .map(|c| (s.solar_system_id, c.celestial_id, c.position))
        })
        .expect("null-sec system with Ice/Lava/Oceanic planet");

    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&pilot.char_id).unwrap().solar_system_id = sys_id;
        let st = s.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.sov_alliance_id = Some(alliance_id);
        s.next_entity_id += 1;
        let sh_item = s.next_entity_id;
        s.items.insert(
            sh_item,
            InventoryItemRecord {
                item_id: sh_item,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_id: pilot.char_id,
                location_id: pilot.ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    pilot.sess.solar_system_id = Some(sys_id);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(eve_destiny::Ball {
            id: pilot.ship_id,
            type_id: HURRICANE_TYPE,
            position: planet_pos + DVec3::new(80_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 3.0,
            mass: 1.5e7,
            radius: 200.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

    let sh_item_id = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .find(|i| i.location_id == pilot.ship_id && i.type_id == TYPE_ORBITAL_SKYHOOK)
            .unwrap()
            .item_id
    });
    let anchor_sh = call_rpc(
        &ctx,
        &mut pilot.sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(sh_item_id), PyRep::Integer(planet_id)],
    )
    .await
    .expect("Anchor Orbital Skyhook");
    let skyhook_id = anchor_sh.dict_get("structureID").and_then(PyRep::as_i64).expect("skyhook_id");

    // Advance past anchoring, record 0h baseline right at online completion, then +2h (50% Secure/Surplus), then +24h maturity!
    ctx.advance_secs(86_400);
    tick_background_cluster(&ctx).await;
    let (_, sec_0h, insec_0h) = ctx
        .db
        .with_mut_snapshot(|s| advance_skyhook_reagents_in_snap(s, &ctx.sde, skyhook_id, ctx.now_unix()))
        .expect("reagent bay initialized at anchor completion");

    let after_2h = ctx.now_unix() + 2 * 3_600;
    let (reagent_tid_2h, sec_2h, insec_2h) = ctx
        .db
        .with_mut_snapshot(|s| advance_skyhook_reagents_in_snap(s, &ctx.sde, skyhook_id, after_2h))
        .expect("reagent bay after 2h");
    let harvested_2h = (sec_2h + insec_2h) - (sec_0h + insec_0h);
    let expected_hourly = eve_server::services::quasar_sov_skyhook::planet_hourly_reagent_yield(
        &ctx.sde,
        ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_planets.get(&skyhook_id).copied()),
        Some(sys_id),
        reagent_tid_2h,
    );
    assert_eq!(harvested_2h, 2 * expected_hourly);
    assert!(insec_2h > 0, "Surplus bay holds unmatured 50% split ({insec_2h})");

    let after_maturity = after_2h + SKYHOOK_MATURITY_CYCLE_SECS + 3_600;
    let (rt_tid, sec_full, insec_full) = ctx
        .db
        .with_mut_snapshot(|s| advance_skyhook_reagents_in_snap(s, &ctx.sde, skyhook_id, after_maturity))
        .expect("reagent maturity elapsed");
    assert!(rt_tid == TYPE_SUPERIONIC_ICE || rt_tid == TYPE_MAGMATIC_GAS);
    let expected_sec_full = sec_2h + insec_2h + (((sec_full + insec_full) - (sec_2h + insec_2h)) / 2);
    assert_eq!(sec_full, expected_sec_full, "Surplus reagents mature into Secure bay");

    let star_pos = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 6)
        .map(|c| c.position)
        .unwrap_or(DVec3::ZERO);
    sys_h.set_ball_position(pilot.ship_id, star_pos + DVec3::new(60_000.0, 0.0, 0.0)).await;
    let vector_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 4599)
        .map(|t| t.type_id)
        .unwrap_or(77_825);
    let vec_item_id = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let id = s.next_entity_id;
        s.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: vector_tid,
                owner_id: pilot.char_id,
                location_id: pilot.ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                ..Default::default()
            },
        );
        id
    });
    let pos_before_vec = sys_h.get_ball_snapshot(pilot.ship_id).await.unwrap().unwrap().1.position;
    let _vec_res = call_rpc(&ctx, &mut pilot.sess, "warpVectorMgr", "UseVector", &[PyRep::Integer(vec_item_id)])
        .await
        .expect("warpVectorMgr.UseVector near star");
    for _ in 0..120 {
        let _ = sys_h.step_manual(1.0).await;
        ctx.advance_secs(1);
        if sys_h.get_ball_snapshot(pilot.ship_id).await.ok().flatten().is_some_and(|(_, b)| pos_before_vec.distance(b.position) >= 1_000_000.0) {
            break;
        }
    }
    let pos_after_vec = sys_h.get_ball_snapshot(pilot.ship_id).await.unwrap().unwrap().1.position;
    assert!(
        pos_before_vec.distance(pos_after_vec) >= 150_000.0,
        "Warp Vector must propel ship >= 150 km via destiny Warp (moved {:.0} m)",
        pos_before_vec.distance(pos_after_vec)
    );

    let ctx2 = reload_ctx_from_sqlite(&ctx, &db_path);
    let persisted_sec = ctx2.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays.get(&skyhook_id).map(|b| b.1).unwrap_or(0));
    assert_eq!(persisted_sec, sec_full, "Matured Skyhook Secure bay persists across SQLite reload");
}
