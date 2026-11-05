//! End-to-end parity closure tests (Part 1) for Equinox Sovereignty Hub anchoring,
//! sovereignty claim/loss notifications, earned Development Indices, live ADM Entosis
//! scaling, Power, Workforce, Fuel, Upgrades, Reagents & Upgrade Sites.

#![forbid(unsafe_code)]

use crate::quasar_sov_equinox_hub::structure;
use crate::sovereignty_equinox_gameplay::{
    drain_notif_events, fit_subcap_blasters, lock_target_via_rpc, make_ball, open_sqlite_ctx,
    place_ship_ball, register_online_pilot, rpc_call, set_pilot_location, step_weapon_ticks,
    temp_sqlite_path, ALLIANCE, CORP, PILOT, PILOT_SHIP,
};
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::sov_gameplay::{
    ENTOSIS_HUB_BASE_CAPTURE_SECS, ENTOSIS_NODE_BASE_CAPTURE_SECS,
};
use eve_server::services::sov_services::{
    calculate_adm, claim_system_sovereignty, TYPE_SOVEREIGNTY_HUB,
};
use eve_server::services::station_structure::structure_lifecycle as life;
use glam::DVec3;

fn err_code<T: std::fmt::Debug>(res: Result<T, eve_proto::BridgeRpcError>) -> String {
    match res {
        Err(e) => e.code,
        Ok(v) => panic!("expected BridgeRpcError, got Ok({v:?})"),
    }
}

fn find_two_claimable_nullsec(ctx: &ServerContext) -> (u32, u32) {
    let mut ids: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| {
            s.security_status <= 0.0
                && s.solar_system_id < 31_000_000
                && s.solar_system_id != 30_100_000
                && s.region_id != 10_000_070
                && ctx.sde.constellations.get(&s.constellation_id).and_then(|c| c.faction_id).is_none()
                && ctx.sde.regions.get(&s.region_id).and_then(|r| r.faction_id).is_none()
        })
        .map(|s| s.solar_system_id)
        .collect();
    ids.sort_unstable();
    (ids[0], ids[1])
}

#[tokio::test]
async fn test_sov_hub_anchor_claim_and_loss_notifications_parity() {
    let db_path = temp_sqlite_path("sov_anchor_claim_loss");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, alt_null_sys) = find_two_claimable_nullsec(&ctx);
    let pochven_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.region_id == 10_000_070)
        .map(|s| s.solar_system_id)
        .expect("Pochven system");
    let npc_sov_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.security_status <= 0.0
                && (ctx.sde.constellations.get(&s.constellation_id).and_then(|c| c.faction_id).is_some()
                    || ctx.sde.regions.get(&s.region_id).and_then(|r| r.faction_id).is_some())
        })
        .map(|s| s.solar_system_id)
        .expect("NPC-sov nullsec system");
    let wh_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id >= 31_000_000)
        .map(|s| s.solar_system_id)
        .expect("wormhole system");

    let hub_pos = DVec3::new(150_000.0, 0.0, 0.0);
    let mut s_dir = register_online_pilot(
        &ctx,
        81_001,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        30_000_142,
        PILOT_SHIP,
        639,
        hub_pos,
        corp_roles::ROLE_DIRECTOR,
    )
    .await;

    // 1. Negative space checks: high-sec, low-sec, wormhole, Pochven, Zarzakh, and NPC-sov nullsec all fail closed.
    let _ = ctx.systems.get_or_spawn(30_000_142).remove_ball(PILOT_SHIP).await;
    for blocked_sys in [30_000_142u32, 30_000_144, wh_sys, pochven_sys, 30_100_000, npc_sov_sys] {
        set_pilot_location(&ctx, &mut s_dir, blocked_sys, None);
        place_ship_ball(&ctx, blocked_sys, PILOT, PILOT_SHIP, 639, hub_pos).await;
        assert_eq!(
            err_code(
                rpc_call(
                    &ctx,
                    &mut s_dir,
                    "structureDeployment",
                    "Anchor",
                    &[PyRep::Integer(TYPE_SOVEREIGNTY_HUB as i64), PyRep::String("Blocked Hub".into())],
                )
                .await
            ),
            "CannotAnchorSovHubOutsideNullSec"
        );
        let _ = ctx.systems.get_or_spawn(blocked_sys).remove_ball(PILOT_SHIP).await;
    }

    // 2. Negative alliance, role, and missing cargo item checks in claimable null-sec.
    set_pilot_location(&ctx, &mut s_dir, null_sys, None);
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 639, hub_pos).await;

    let mut s_no_ally = register_online_pilot(
        &ctx,
        81_002,
        "noally",
        "sec-1",
        90_000_111,
        98_000_111,
        None,
        null_sys,
        100_000_111,
        639,
        hub_pos + DVec3::new(2_000.0, 0.0, 0.0),
        corp_roles::ROLE_DIRECTOR,
    )
    .await;
    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_no_ally,
                "structureDeployment",
                "Anchor",
                &[PyRep::Integer(TYPE_SOVEREIGNTY_HUB as i64)],
            )
            .await
        ),
        "AllianceRequired"
    );

    let mut s_grunt = register_online_pilot(
        &ctx,
        81_003,
        "grunt",
        "sec-2",
        90_000_112,
        CORP,
        Some(ALLIANCE),
        null_sys,
        100_000_112,
        639,
        hub_pos + DVec3::new(3_000.0, 0.0, 0.0),
        0,
    )
    .await;
    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_grunt,
                "structureDeployment",
                "Anchor",
                &[PyRep::Integer(TYPE_SOVEREIGNTY_HUB as i64)],
            )
            .await
        ),
        "CorpRoleDenied"
    );

    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_dir,
                "structureDeployment",
                "Anchor",
                &[PyRep::Integer(TYPE_SOVEREIGNTY_HUB as i64)],
            )
            .await
        ),
        "MissingStructureItem"
    );

    // 3. Seed Sovereignty Hub item (32458) in cargo, anchor via structureDeployment.Anchor, complete onlining -> claims sov & emits both notifications!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            980_001,
            InventoryItemRecord {
                item_id: 980_001,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let anchor_rep = rpc_call(
        &ctx,
        &mut s_dir,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(980_001), PyRep::String("Parity Sov Hub".into())],
    )
    .await
    .expect("anchor Sovereignty Hub from cargo");
    let hub_id = anchor_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(anchor_rep.dict_get("state").and_then(|v| v.as_str()), Some(life::STATE_ONLINING));
    assert_eq!(ctx.db.with_snapshot(|s| s.items.contains_key(&980_001)), false);
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states.get(&null_sys).and_then(|st| st.sov_alliance_id)), None);

    // Duplicate hub anchor in the same system fails with SovHubAlreadyPresent; hostile-claimed system fails with SovereigntyHeldByAnotherAlliance.
    let mut s_hostile = register_online_pilot(
        &ctx,
        81_004,
        "demo2",
        "demo2-secret",
        90_000_113,
        98_000_113,
        Some(99_000_113),
        null_sys,
        100_000_113,
        621,
        hub_pos + DVec3::new(4_000.0, 0.0, 0.0),
        corp_roles::ROLE_DIRECTOR,
    )
    .await;
    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_hostile,
                "structureDeployment",
                "Anchor",
                &[PyRep::Integer(TYPE_SOVEREIGNTY_HUB as i64)],
            )
            .await
        ),
        "SovHubAlreadyPresent"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(alt_null_sys).or_default().sov_alliance_id = Some(ALLIANCE);
    });
    let _ = ctx.systems.get_or_spawn(null_sys).remove_ball(100_000_113).await;
    set_pilot_location(&ctx, &mut s_hostile, alt_null_sys, None);
    place_ship_ball(&ctx, alt_null_sys, 90_000_113, 100_000_113, 621, hub_pos).await;
    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_hostile,
                "structureDeployment",
                "Anchor",
                &[PyRep::Integer(TYPE_SOVEREIGNTY_HUB as i64)],
            )
            .await
        ),
        "SovereigntyHeldByAnotherAlliance"
    );
    let _ = ctx.systems.get_or_spawn(alt_null_sys).remove_ball(100_000_113).await;

    // Complete 15-minute onlining timer -> system claimed + OnSovereigntyChanged & OnSolarSystemSovStructuresUpdated broadcast!
    let _ = drain_notif_events(&ctx, PILOT);
    ctx.advance_secs(905);
    life::sync_all_structures(&ctx);
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&hub_id].state.clone()), life::STATE_SHIELD_VULNERABLE);
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&null_sys].sov_alliance_id), Some(ALLIANCE));
    let claim_notifs = ctx.drain_notifications_for_char(PILOT);
    let sov_claim = claim_notifs.iter().find(|n| n.event == "OnSovereigntyChanged").expect("claim OnSovereigntyChanged");
    assert_eq!(sov_claim.args.len(), 2);
    assert_eq!(sov_claim.args[0].as_i64(), Some(null_sys as i64));
    assert_eq!(sov_claim.args[1].as_i64(), Some(ALLIANCE));
    let sov_struct_claim = claim_notifs
        .iter()
        .find(|n| n.event == "OnSolarSystemSovStructuresUpdated")
        .expect("claim OnSolarSystemSovStructuresUpdated");
    assert_eq!(sov_struct_claim.args.len(), 3);
    assert_eq!(sov_struct_claim.args[0].as_i64(), Some(null_sys as i64));
    let PyRep::List(claim_rows) = &sov_struct_claim.args[1] else { panic!("expected List of sov structures") };
    assert_eq!(claim_rows.len(), 1);
    assert_eq!(claim_rows[0].dict_get("itemID").and_then(|v| v.as_i64()), Some(hub_id));
    assert_eq!(claim_rows[0].dict_get("typeID").and_then(|v| v.as_i64()), Some(i64::from(TYPE_SOVEREIGNTY_HUB)));
    let PyRep::Dict(claim_changes) = &sov_struct_claim.args[2] else { panic!("expected Dict of sov structure changes") };
    assert!(claim_changes.iter().any(|(k, _)| k.as_i64() == Some(hub_id)));

    // 4. Hostile Entosis capture of Sovereignty Hub + Command Nodes destroys Hub, clears sov, and emits BOTH loss notifications!
    set_pilot_location(&ctx, &mut s_hostile, null_sys, None);
    place_ship_ball(&ctx, null_sys, 90_000_113, 100_000_113, 621, hub_pos + DVec3::new(4_000.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_113).or_default().insert(
            3_426,
            TrainedSkillRecord { type_id: 3_426, level: 5, skill_points: 256_000.0 },
        );
        snap.items.insert(
            980_010,
            InventoryItemRecord {
                item_id: 980_010,
                type_id: 34_593,
                owner_id: 90_000_113,
                location_id: 100_000_113,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            980_011,
            InventoryItemRecord {
                item_id: 980_011,
                type_id: 16_275,
                owner_id: 90_000_113,
                location_id: 100_000_113,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(980_010, true);
    });
    ctx.invalidate_ship_fitting(100_000_113);
    rpc_call(&ctx, &mut s_dir, "structureVulnerability", "UpdateReinforceTimesFromClient", &[PyRep::Integer(hub_id), PyRep::Integer(12), PyRep::Integer(0)])
        .await
        .expect("set vulnerability window");
    let cur_sec = ctx.now_unix() % 86_400;
    ctx.advance_secs((86_400 + 12 * 3_600 - cur_sec) % 86_400);
    lock_target_via_rpc(&ctx, &mut s_hostile, null_sys, 100_000_113, hub_id).await;
    rpc_call(&ctx, &mut s_hostile, "dogmaIM", "Activate", &[PyRep::Integer(980_010), PyRep::String("default".into()), PyRep::Integer(hub_id)])
        .await
        .expect("activate Entosis Link on Sov Hub");
    ctx.advance_secs(300 + ENTOSIS_HUB_BASE_CAPTURE_SECS as u64 + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&hub_id].state.clone()), "HullReinforced");
    ctx.advance_secs(48 * 3_600 + 1);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let _ = drain_notif_events(&ctx, PILOT);
    for _ in 0..9 {
        let (nid, nsys, npos) = ctx.db.with_snapshot(|s| {
            let (&nid, &(_, nsys, _)) = s.sov_runtime.command_nodes.iter().find(|(_, &(hid, _, _))| hid == hub_id).unwrap();
            (nid, nsys, s.item_positions[&nid])
        });
        let prev = s_hostile.solar_system_id.unwrap();
        if prev != nsys {
            let _ = ctx.systems.get_or_spawn(prev).remove_ball(100_000_113).await;
        }
        set_pilot_location(&ctx, &mut s_hostile, nsys, None);
        place_ship_ball(&ctx, nsys, 90_000_113, 100_000_113, 621, DVec3::from_array(npos) + DVec3::new(3_000.0, 0.0, 0.0)).await;
        lock_target_via_rpc(&ctx, &mut s_hostile, nsys, 100_000_113, nid).await;
        rpc_call(&ctx, &mut s_hostile, "dogmaIM", "Activate", &[PyRep::Integer(980_010), PyRep::String("default".into()), PyRep::Integer(nid)])
            .await
            .expect("entosis Command Node");
        ctx.advance_secs(300 + ENTOSIS_NODE_BASE_CAPTURE_SECS as u64 + 10);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    }

    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&hub_id].state.clone()), "Destroyed");
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&null_sys].sov_alliance_id), None);
    let loss_notifs = ctx.drain_notifications_for_char(PILOT);
    let sov_loss = loss_notifs.iter().find(|n| n.event == "OnSovereigntyChanged").expect("loss OnSovereigntyChanged");
    assert_eq!(sov_loss.args.len(), 2);
    assert_eq!(sov_loss.args[0].as_i64(), Some(null_sys as i64));
    assert_eq!(sov_loss.args[1], PyRep::None);
    let sov_struct_loss = loss_notifs
        .iter()
        .find(|n| n.event == "OnSolarSystemSovStructuresUpdated")
        .expect("loss OnSolarSystemSovStructuresUpdated");
    assert_eq!(sov_struct_loss.args.len(), 3);
    assert_eq!(sov_struct_loss.args[0].as_i64(), Some(null_sys as i64));
    let PyRep::List(loss_rows) = &sov_struct_loss.args[1] else { panic!("expected List of sov structures") };
    assert!(loss_rows.is_empty(), "destroyed Hub must be excluded from active sov structures");
    let PyRep::Dict(loss_changes) = &sov_struct_loss.args[2] else { panic!("expected Dict of sov structure changes") };
    assert!(loss_changes.iter().any(|(k, _)| k.as_i64() == Some(hub_id)));

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_sov_dev_indices_and_adm_live_entosis_scaling_parity() {
    // parity: notify:OnSolarSystemDevIndexChanged
    eve_testkit::attest(&[
        "notify:OnSolarSystemDevIndexChanged",
    ]);

    let db_path = temp_sqlite_path("sov_adm_dev_indices");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (sys_adm1, sys_high) = find_two_claimable_nullsec(&ctx);
    let (hub_adm1, hub_high) = (1_080_000_001i64, 1_080_000_002i64);
    let pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_def = register_online_pilot(
        &ctx,
        82_001,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        sys_high,
        PILOT_SHIP,
        639,
        pos + DVec3::new(2_500.0, 0.0, 0.0),
        corp_roles::ROLE_DIRECTOR,
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(sys_adm1).or_default().solar_system_id = sys_adm1;
        snap.system_states.entry(sys_high).or_default().solar_system_id = sys_high;
        snap.structures.insert(hub_adm1, structure(hub_adm1, CORP, sys_adm1, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(hub_high, structure(hub_high, CORP, sys_high, TYPE_SOVEREIGNTY_HUB));
    });
    let _ = ctx.systems.get_or_spawn(sys_adm1).spawn_ball(make_ball(hub_adm1, TYPE_SOVEREIGNTY_HUB, pos)).await;
    let _ = ctx.systems.get_or_spawn(sys_high).spawn_ball(make_ball(hub_high, TYPE_SOVEREIGNTY_HUB, pos)).await;
    claim_system_sovereignty(&ctx, sys_high, ALLIANCE, CORP, hub_high).expect("claim sys_high");

    // 1. Earn Strategic Index on sys_high (7d held -> level 1), then claim sys_adm1 at ADM 1.0, then earn Military (2) & Industrial (1) on sys_high.
    let _ = drain_notif_events(&ctx, PILOT);
    ctx.advance_secs(7 * 86_400 + 60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&sys_high].strategic_index), 1);
    assert_eq!(
        ctx.drain_notifications_for_char(PILOT).iter().filter(|n| n.event == "OnSolarSystemDevIndexChanged").count(),
        1
    );
    claim_system_sovereignty(&ctx, sys_adm1, ALLIANCE, CORP, hub_adm1).expect("claim sys_adm1 at ADM 1.0");

    fit_subcap_blasters(&ctx, PILOT, PILOT_SHIP, 981_000);
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT).or_default().insert(3_386, TrainedSkillRecord { type_id: 3_386, level: 5, skill_points: 256_000.0 });
        for (i, flg) in [InventoryFlag::LoSlot3, InventoryFlag::LoSlot4, InventoryFlag::LoSlot5].into_iter().enumerate() {
            let iid = 981_060 + i as i64;
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 1_319,
                    owner_id: PILOT,
                    location_id: PILOT_SHIP,
                    flag: flg,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(iid, true);
        }
        for (i, flg) in [InventoryFlag::RigSlot0, InventoryFlag::RigSlot1, InventoryFlag::RigSlot2].into_iter().enumerate() {
            let iid = 981_070 + i as i64;
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 31_125,
                    owner_id: PILOT,
                    location_id: PILOT_SHIP,
                    flag: flg,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(iid, true);
        }
    });
    ctx.invalidate_ship_fitting(PILOT_SHIP);
    place_ship_ball(&ctx, sys_high, PILOT, PILOT_SHIP, 16_229, pos + DVec3::new(2_500.0, 0.0, 0.0)).await;
    ctx.set_invention_roll_override(Some(0.005));
    let _ = drain_notif_events(&ctx, PILOT);

    for idx in 0..15i64 {
        let rat_id = 890_001 + idx;
        let rat_cs = ctx.build_type_combat_state(17_005);
        let _ = ctx
            .systems
            .get_or_spawn(sys_high)
            .spawn_ball_with_combat(make_ball(rat_id, 17_005, pos + DVec3::new(3_500.0 + (idx as f64) * 50.0, 0.0, 0.0)), rat_cs)
            .await;
        lock_target_via_rpc(&ctx, &mut s_def, sys_high, PILOT_SHIP, rat_id).await;
        let mut killed = false;
        for round in 0..35 {
            for gun_id in (0..6).map(|g| 981_000 + g * 2) {
                // A weapon that is still cycling keeps firing; re-activating it is rejected (V24 rule).
                let res = match rpc_call(&ctx, &mut s_def, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(rat_id)]).await {
                    Ok(r) => r,
                    Err(e) if e.code == "ModuleAlreadyActive" => PyRep::None,
                    Err(e) => panic!("kill Guristas rat: {e:?}"),
                };
                if res.dict_get("destroyed") == Some(&PyRep::Bool(true))
                    || ctx.systems.get_or_spawn(sys_high).get_ball_snapshot(rat_id).await.ok().flatten().is_none()
                {
                    killed = true;
                    break;
                }
            }
            if killed {
                break;
            }
            // Cycling blasters deal their damage as the system actor steps.
            step_weapon_ticks(&ctx, sys_high).await;
            if ctx.systems.get_or_spawn(sys_high).get_ball_snapshot(rat_id).await.ok().flatten().is_none() {
                killed = true;
                break;
            }
            let _ = round;
        }
        assert!(killed, "Guristas rat {rat_id} must be destroyed by subcap blasters");
        if idx == 3 {
            assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&sys_high].military_index), 0);
        } else if idx == 4 {
            assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&sys_high].military_index), 1);
            let n = ctx.drain_notifications_for_char(PILOT);
            let ev = n.iter().find(|x| x.event == "OnSolarSystemDevIndexChanged").expect("mil 1 notify");
            assert_eq!(ev.args, vec![PyRep::Integer(sys_high as i64)]);
        }
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&sys_high].military_index), 2);
    assert_eq!(
        ctx.drain_notifications_for_char(PILOT).iter().filter(|n| n.event == "OnSolarSystemDevIndexChanged").count(),
        1
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_it) = snap.items.get_mut(&PILOT_SHIP) {
            ship_it.type_id = 19_722;
        }
        for id in 981_000..=981_011 {
            snap.items.remove(&id);
            snap.module_online_states.remove(&id);
        }
        snap.items.insert(
            981_050,
            InventoryItemRecord {
                item_id: 981_050,
                type_id: 483,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(981_050, true);
    });
    ctx.invalidate_ship_fitting(PILOT_SHIP);
    place_ship_ball(&ctx, sys_high, PILOT, PILOT_SHIP, 19_722, pos + DVec3::new(2_500.0, 0.0, 0.0)).await;

    let rock_id = 890_050i64;
    ctx.systems
        .get_or_spawn(sys_high)
        .spawn_asteroid(
            eve_server::system_actor::AsteroidState {
                asteroid_id: rock_id,
                belt_id: 0,
                type_id: 1_228,
                initial_units: 50_000,
                remaining_units: 50_000,
                unit_volume: 0.15,
                depleted: false,
                respawn_at_tick: None,
            },
            make_ball(rock_id, 1_228, pos + DVec3::new(3_000.0, 0.0, 0.0)),
        )
        .await
        .expect("spawn Scordite");
    lock_target_via_rpc(&ctx, &mut s_def, sys_high, PILOT_SHIP, rock_id).await;
    rpc_call(&ctx, &mut s_def, "dogmaIM", "Activate", &[PyRep::Integer(981_050), PyRep::String("miningLaser".into()), PyRep::Integer(rock_id)])
        .await
        .expect("activate Miner I");
    ctx.advance_secs(26 * 60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let _ = rpc_call(&ctx, &mut s_def, "dogmaIM", "Deactivate", &[PyRep::Integer(981_050), PyRep::String("miningLaser".into())]).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&sys_high].industrial_index), 1);
    assert_eq!(
        ctx.drain_notifications_for_char(PILOT).iter().filter(|n| n.event == "OnSolarSystemDevIndexChanged").count(),
        1
    );

    let expected_adm = calculate_adm(2, 1, 1);
    assert!((expected_adm - 3.2).abs() < 1e-6);
    let dev_sys = rpc_call(&ctx, &mut s_def, "devIndexManager", "GetDevelopmentIndicesForSystem", &[PyRep::Integer(sys_high as i64)])
        .await
        .expect("GetDevelopmentIndicesForSystem");
    let PyRep::Dict(sys_entries) = &dev_sys else { panic!("expected Dict from GetDevelopmentIndicesForSystem") };
    let idx_lvl = |attr_id: i64| {
        sys_entries
            .iter()
            .find(|(k, _)| k.as_i64() == Some(attr_id))
            .and_then(|(_, v)| v.dict_get("level").and_then(|l| l.as_i64()))
    };
    assert_eq!(idx_lvl(1583), Some(2));
    assert_eq!(idx_lvl(1584), Some(1));
    assert_eq!(idx_lvl(1615), Some(1));

    let all_idx = rpc_call(&ctx, &mut s_def, "devIndexManager", "GetAllDevelopmentIndices", &[]).await.expect("GetAllDevelopmentIndices");
    let PyRep::Dict(entries) = all_idx else { panic!("expected Dict") };
    let adm1_entry = entries.iter().find(|(k, _)| k.as_i64() == Some(sys_adm1 as i64)).expect("sys_adm1 entry");
    assert!((adm1_entry.1.dict_get("adm").and_then(|v| v.as_f64()).unwrap() - 1.0).abs() < 1e-5);

    // 2. Live Entosis timing comparison: ADM 1.0 hub reinforces at 300 + 600s, while ADM 3.2 hub stays ShieldVulnerable until 300 + 1920s!
    for hid in [hub_adm1, hub_high] {
        rpc_call(&ctx, &mut s_def, "structureVulnerability", "UpdateReinforceTimesFromClient", &[PyRep::Integer(hid), PyRep::Integer(14), PyRep::Integer(0)])
            .await
            .expect("set vuln window");
    }
    let cur_sec = ctx.now_unix() % 86_400;
    ctx.advance_secs((86_400 + 14 * 3_600 - cur_sec) % 86_400);

    let mut s_atk1 = register_online_pilot(&ctx, 82_010, "atk1", "sec-1", 90_000_201, 98_000_201, Some(99_000_201), sys_adm1, 100_000_201, 621, pos + DVec3::new(4_000.0, 0.0, 0.0), 0).await;
    let mut s_atk2 = register_online_pilot(&ctx, 82_011, "atk2", "sec-2", 90_000_202, 98_000_202, Some(99_000_202), sys_high, 100_000_202, 621, pos + DVec3::new(4_000.0, 0.0, 0.0), 0).await;
    ctx.db.with_mut_snapshot(|snap| {
        for (cid, sid, mid, stid) in [(90_000_201i64, 100_000_201i64, 981_201i64, 981_202i64), (90_000_202, 100_000_202, 981_203, 981_204)] {
            snap.skills.entry(cid).or_default().insert(3_426, TrainedSkillRecord { type_id: 3_426, level: 5, skill_points: 256_000.0 });
            snap.items.insert(mid, InventoryItemRecord { item_id: mid, type_id: 34_593, owner_id: cid, location_id: sid, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default() });
            snap.items.insert(stid, InventoryItemRecord { item_id: stid, type_id: 16_275, owner_id: cid, location_id: sid, flag: InventoryFlag::Cargo, quantity: 100, ..Default::default() });
            snap.module_online_states.insert(mid, true);
        }
    });
    ctx.invalidate_ship_fitting(100_000_201);
    ctx.invalidate_ship_fitting(100_000_202);
    lock_target_via_rpc(&ctx, &mut s_atk1, sys_adm1, 100_000_201, hub_adm1).await;
    lock_target_via_rpc(&ctx, &mut s_atk2, sys_high, 100_000_202, hub_high).await;

    rpc_call(&ctx, &mut s_atk1, "dogmaIM", "Activate", &[PyRep::Integer(981_201), PyRep::String("default".into()), PyRep::Integer(hub_adm1)]).await.expect("entosis hub_adm1");
    rpc_call(&ctx, &mut s_atk2, "dogmaIM", "Activate", &[PyRep::Integer(981_203), PyRep::String("default".into()), PyRep::Integer(hub_high)]).await.expect("entosis hub_high");

    ctx.advance_secs(905);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&hub_adm1].state.clone()), "HullReinforced");
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&hub_high].state.clone()), "ShieldVulnerable", "ADM 3.2 hub must not reinforce at ADM 1.0 timer");

    ctx.advance_secs(1_325);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&hub_high].state.clone()), "HullReinforced", "ADM 3.2 hub reinforces at 300 + 600*3.2 = 2220s");

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

#[allow(dead_code, unused_imports)]
pub(crate) mod mercenary_dens_development {
    use super::*;

//  End-to-end parity closure tests (Part 2) for Tenebrex Cyno Jammer upgrade gating,
//  Sovereignty Hub hacking & `OnLocalHackProgressUpdated`, Mercenary Den Development/Anarchy
//  evolution & workforce disruption, and all 7 Quasar Sovereignty protobuf APIs & notices.


use crate::quasar_sov_equinox_hub::{field, fields, id_req, idm, rpc, structure, subscribe, varint};
use crate::sovereignty_equinox_gameplay::{
    drain_quasar, find_nullsec_planets, fit_subcap_blasters, lock_target_via_rpc, make_ball,
    open_sqlite_ctx, place_ship_ball, register_online_pilot, rpc_call, step_weapon_ticks,
    temp_sqlite_path, ALLIANCE, CORP, PILOT, PILOT_SHIP,
};
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::quasar::{encode_bytes_field, encode_varint_field};
use eve_proto::PyRep;
use eve_server::services::quasar_sov_hub::compute_hub_resources;
use eve_server::services::quasar_sov_mercden::TYPE_MERCENARY_DEN_MANAGEMENT;
use eve_server::services::sov_gameplay::{
    TENEBREX_SPOOL_UP_SECS, TYPE_MERCENARY_DEN, TYPE_TENEBREX_CYNO_JAMMER,
    UPGRADE_CYNOSURAL_SUPPRESSION,
};
use eve_server::services::sov_services::{
    claim_system_sovereignty, is_hub_hacked, TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK,
    TYPE_SOVEREIGNTY_HUB, TYPE_SUPERIONIC_ICE,
};
use eve_server::services::station_structure::structure_lifecycle as life;
use glam::DVec3;

#[tokio::test]
async fn test_sov_cyno_jammer_upgrade_and_hub_hacking_parity() {
    // parity: feat:sovereignty.sov-cyno-jammer-upgrade
    // parity: feat:sovereignty.sov-hub-hacking
    // parity: notify:OnLocalHackProgressUpdated
    eve_testkit::attest(&[
        "feat:sovereignty.sov-cyno-jammer-upgrade",
        "feat:sovereignty.sov-hub-hacking",
        "notify:OnLocalHackProgressUpdated",
    ]);

    let db_path = temp_sqlite_path("sov_cyno_hack");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, _, _, _, _) = find_nullsec_planets(&ctx);
    let hub_id = 1_100_000_001i64;
    let hub_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_dir = register_online_pilot(
        &ctx,
        84_001,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        null_sys,
        PILOT_SHIP,
        11_957,
        hub_pos + DVec3::new(35_000.0, 0.0, 0.0),
        corp_roles::ROLE_DIRECTOR,
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().solar_system_id = null_sys;
        snap.structures.insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        for (idx, cel) in ctx.sde.celestials_in_system(null_sys).into_iter().filter(|c| c.group_id == 7).enumerate() {
            let sh_id = 1_100_000_100 + idx as i64;
            snap.structures.insert(sh_id, structure(sh_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
            snap.sov_runtime.skyhook_planets.insert(sh_id, cel.celestial_id);
        }
        for (iid, tid, loc, flg, qty) in [
            (984_001, TYPE_MAGMATIC_GAS, hub_id, InventoryFlag::Cargo, 5_000),
            (984_002, TYPE_SUPERIONIC_ICE, hub_id, InventoryFlag::Cargo, 5_000),
            (984_004, 88_228, hub_id, InventoryFlag::Cargo, 1),
            (984_005, UPGRADE_CYNOSURAL_SUPPRESSION, hub_id, InventoryFlag::Cargo, 1),
            (984_010, TYPE_TENEBREX_CYNO_JAMMER, PILOT_SHIP, InventoryFlag::Cargo, 1),
            (984_011, 4_247, PILOT_SHIP, InventoryFlag::Cargo, 500),
            (984_020, 21_096, PILOT_SHIP, InventoryFlag::HiSlot0, 1),
            (984_021, 16_273, PILOT_SHIP, InventoryFlag::Cargo, 2_000),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: PILOT,
                    location_id: loc,
                    flag: flg,
                    quantity: qty,
                    singleton: qty == 1,
                    ..Default::default()
                },
            );
        }
        snap.module_online_states.insert(984_020, true);
        let sk = snap.skills.entry(PILOT).or_default();
        for sid in [21_603u32, 3_335, 12_092, 22_761] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
    });
    ctx.invalidate_ship_fitting(PILOT_SHIP);
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(hub_id, TYPE_SOVEREIGNTY_HUB, hub_pos)).await;
    claim_system_sovereignty(&ctx, null_sys, ALLIANCE, CORP, hub_id).expect("claim null_sys");

    // 1. Anchoring Tenebrex fails closed until Cynosural Suppression (81619) is installed & onlined.
    let err_offline = rpc_call(
        &ctx,
        &mut s_dir,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(984_010), PyRep::String("Tenebrex".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err_offline.code, "RequiredSovUpgradeOffline");

    for (fuel_iid, amt) in [(984_001i64, 2_000u64), (984_002i64, 2_000u64)] {
        let mut add = id_req(1, fuel_iid as u64);
        encode_varint_field(2, amt, &mut add);
        encode_bytes_field(3, &idm(hub_id as u64), &mut add);
        assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.fuel.api.AddRequest", add).await.status_code, 200);
    }
    let mut proc = Vec::new();
    for iid in [984_004u64, 984_005] {
        encode_bytes_field(1, &idm(iid), &mut proc);
    }
    for tid in [88_228u64, UPGRADE_CYNOSURAL_SUPPRESSION as u64] {
        let mut c = Vec::new();
        encode_bytes_field(1, &idm(tid), &mut c);
        encode_varint_field(2, 1, &mut c);
        encode_bytes_field(2, &c, &mut proc);
    }
    encode_bytes_field(3, &idm(hub_id as u64), &mut proc);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc).await.status_code, 200);

    // Anchor Tenebrex, fuel it, advance through anchoring + onlining + 300s spool-up -> system jammed!
    let deep = DVec3::new(40_000_000.0, 40_000_000.0, 40_000_000.0);
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 11_957, deep).await;
    let tenebrex_id = rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(984_010), PyRep::String("Tenebrex".into())])
        .await.unwrap().dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 11_957, deep + DVec3::new(60_000.0, 0.0, 0.0)).await;
    rpc_call(&ctx, &mut s_dir, "invbroker", "Add", &[PyRep::Integer(984_011), PyRep::Integer(tenebrex_id), PyRep::Integer(133), PyRep::Integer(500)])
        .await.expect("fuel Tenebrex");
    ctx.advance_secs(15 * 60 + 24 * 3_600 + 15 * 60 + 10);
    life::sync_all_structures(&ctx);
    ctx.advance_secs(TENEBREX_SPOOL_UP_SECS + 5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let jam_on = rpc_call(&ctx, &mut s_dir, "beyonce", "GetCynoJammerState", &[]).await.expect("GetCynoJammerState");
    assert_eq!(jam_on.dict_get("isJammed"), Some(&PyRep::Bool(true)));
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(5001, eve_db::FleetRecord { fleet_id: 5001, boss_id: PILOT, members: vec![PILOT], ..Default::default() });
    });
    let cyno_err = rpc_call(&ctx, &mut s_dir, "dogmaIM", "Activate", &[PyRep::Integer(984_020), PyRep::String("cynosuralGeneration".into())])
        .await.unwrap_err();
    assert_eq!(cyno_err.code, "CynoJammed");

    // 2. Hostile Data Analyzer I hacking minigame on Sov Hub emits OnLocalHackProgressUpdated + OnSovHubHacked and disables jammer!
    let hacker_cid = 90_000_301i64;
    let hacker_ship = 100_000_301i64;
    let mut s_hacker = register_online_pilot(
        &ctx,
        84_010,
        "hacker",
        "sec-h",
        hacker_cid,
        98_000_301,
        Some(99_000_301),
        null_sys,
        hacker_ship,
        621,
        hub_pos + DVec3::new(2_500.0, 0.0, 0.0),
        0,
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(hacker_cid).or_default();
        for sid in [3_405u32, 21_718] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        snap.items.insert(
            984_100,
            InventoryItemRecord {
                item_id: 984_100,
                type_id: 22_175,
                owner_id: hacker_cid,
                location_id: hacker_ship,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(984_100, true);
    });
    ctx.invalidate_ship_fitting(hacker_ship);
    lock_target_via_rpc(&ctx, &mut s_hacker, null_sys, hacker_ship, hub_id).await;
    let _ = ctx.drain_notifications_for_char(PILOT);
    let _ = ctx.drain_notifications_for_char(hacker_cid);

    rpc_call(&ctx, &mut s_hacker, "dogmaIM", "Activate", &[PyRep::Integer(984_100), PyRep::String("dataHacking".into()), PyRep::Integer(hub_id)])
        .await.expect("start Data Analyzer on Sov Hub");
    rpc_call(&ctx, &mut s_hacker, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(hub_id)])
        .await.expect("start hacking board");

    let mut progress_values = Vec::new();
    let mut won = false;
    for _pass in 0..8 {
        for tile in 1..24i64 {
            for _ in 0..5 {
                if let Ok(step) = rpc_call(&ctx, &mut s_hacker, "hackingMgr", "ClickedOnTile", &[PyRep::Integer(hub_id), PyRep::Integer(tile)]).await {
                    for n in ctx.drain_notifications_for_char(PILOT) {
                        if n.event == "OnLocalHackProgressUpdated" {
                            assert_eq!(n.args[0].as_i64(), Some(hub_id));
                            if let Some(p) = n.args[1].as_f64() {
                                progress_values.push(p);
                            }
                        } else if n.event == "OnSovHubHacked" {
                            assert_eq!(n.args[0].as_i64(), Some(hub_id));
                        }
                    }
                    if step.dict_get("status").and_then(|v| v.as_str()) == Some("Won") {
                        won = true;
                        break;
                    }
                } else {
                    break;
                }
            }
            if won {
                break;
            }
        }
        if won {
            break;
        }
    }
    assert_eq!(won, true);
    assert_eq!(is_hub_hacked(&ctx, hub_id), true);
    assert_eq!(progress_values.iter().any(|&p| p > 0.0 && p < 1.0), true, "intermediate OnLocalHackProgressUpdated values emitted");
    assert_eq!(progress_values.last().copied(), Some(1.0), "final OnLocalHackProgressUpdated reaches 1.0");

    // While hacked, Cynosural Suppression upgrade is disabled so Tenebrex immediately stops jamming!
    let jam_hacked = rpc_call(&ctx, &mut s_dir, "beyonce", "GetCynoJammerState", &[]).await.expect("GetCynoJammerState while hacked");
    assert_eq!(jam_hacked.dict_get("isJammed"), Some(&PyRep::Bool(false)));
    let cyno_ok = rpc_call(&ctx, &mut s_dir, "dogmaIM", "Activate", &[PyRep::Integer(984_020), PyRep::String("cynosuralGeneration".into())]).await;
    assert_eq!(cyno_ok.unwrap().dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(null_sys as i64));

    // Once the 1h hack expires, Cynosural Suppression recovers and Tenebrex re-spools.
    ctx.advance_secs(3_605);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(is_hub_hacked(&ctx, hub_id), false);
    ctx.advance_secs(TENEBREX_SPOOL_UP_SECS + 5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(rpc_call(&ctx, &mut s_dir, "beyonce", "GetCynoJammerState", &[]).await.unwrap().dict_get("isJammed"), Some(&PyRep::Bool(true)));

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_mercenary_den_lifecycle_evolution_and_anarchy_parity() {
    // DATA GAP: feat:sovereignty.mercenary-den / mercenary-den-development-anarchy / mercenary-den-anarchy-evolution — synthetic yield/capacity/anarchy values pending V24.01 client/SDE tables (docs/BLOCKERS.md §10)

    let db_path = temp_sqlite_path("sov_mercden_evo");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, temp_planet, _, _, _) = find_nullsec_planets(&ctx);
    let (hub_id, skyhook_id) = (1_110_000_001i64, 1_110_000_002i64);
    let sh_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_owner = register_online_pilot(
        &ctx,
        85_001,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        null_sys,
        PILOT_SHIP,
        19_722,
        sh_pos + DVec3::new(3_000.0, 0.0, 0.0),
        corp_roles::ROLE_DIRECTOR,
    )
    .await;
    let mut rx = subscribe(&ctx, &mut s_owner).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ALLIANCE);
        snap.structures.insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(skyhook_id, structure(skyhook_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
        snap.skills.entry(PILOT).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord { type_id: TYPE_MERCENARY_DEN_MANAGEMENT, level: 2, skill_points: 50_000.0 },
        );
        snap.items.insert(
            985_001,
            InventoryItemRecord {
                item_id: 985_001,
                type_id: TYPE_MERCENARY_DEN,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(skyhook_id, TYPE_ORBITAL_SKYHOOK, sh_pos)).await;

    let base_wf = ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available);
    let den_id = rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)])
        .await.unwrap().dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    let _ = drain_quasar(&mut rx);

    // Stage 1 (0..19): 65 IDK/hr, 0% workforce disruption.
    ctx.advance_secs(2 * 3_600);
    let d_2h = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await.payload.unwrap().value;
    let info_2h = field(&field(&d_2h, 4).unwrap(), 2).unwrap();
    assert_eq!(varint(&info_2h, 1), Some(130), "2h at stage 1 yields 2 * 65 = 130 IDK");
    assert_eq!(ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available), base_wf);

    // Advance to Day 4 (Dev 20, Anarchy 20 -> Stage 2): 68 IDK/hr, still 0% workforce disruption.
    ctx.advance_secs(4 * 86_400 - 2 * 3_600);
    let d_day4 = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await.payload.unwrap().value;
    let evo4 = field(&d_day4, 3).unwrap();
    let dev_sim4 = field(&field(&evo4, 2).unwrap(), 3).unwrap();
    assert_eq!(varint(&dev_sim4, 1), Some(20));
    assert_eq!(varint(&dev_sim4, 2), Some(2));
    assert_eq!(ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available), base_wf);

    // Advance to Day 8 (Dev 40, Anarchy 40 -> Stage 3): -10% of Temperate Skyhook workforce (150 of 1,500)!
    ctx.advance_secs(4 * 86_400);
    let _ = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await;
    assert_eq!(ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available) + 150, base_wf);

    // Advance to Day 14 (Dev 70, Anarchy 70 -> Stage 4): -20% of Temperate Skyhook workforce (300 of 1,500)!
    ctx.advance_secs(6 * 86_400);
    let _ = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await;
    assert_eq!(ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available) + 300, base_wf);

    // Advance to Day 20 (Dev 100, Anarchy 100 -> Stage 5): 78 IDK/hr, -40% of Temperate Skyhook workforce (600 of 1,500)!
    ctx.advance_secs(6 * 86_400);
    let _ = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await;
    assert_eq!(ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available) + 600, base_wf);

    // Extract stored infomorphs with 25% sov tax, then advance 1h at Stage 5 to verify 78 IDK/hr rate!
    rpc_call(&ctx, &mut s_owner, "sovMgr", "ConfirmSkyhookSovManagement", &[PyRep::Integer(skyhook_id), PyRep::Integer(25)])
        .await.expect("set 25% Mercenary Den tax");
    let den_pos = ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(den_id).await.unwrap().unwrap().1.position;
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 16_229, den_pos + DVec3::new(2_000.0, 0.0, 0.0)).await;
    let ext = rpc_call(&ctx, &mut s_owner, "beyonce", "CallComponentFromClient", &[PyRep::Integer(den_id), PyRep::String("mercenaryDen".into()), PyRep::String("extract_infomorphs_to_ship".into())])
        .await.expect("extract infomorphs");
    assert_eq!(ext.dict_get("ownerShare").and_then(|v| v.as_i64()), Some(2_250));
    assert_eq!(ext.dict_get("taxShare").and_then(|v| v.as_i64()), Some(750));

    ctx.advance_secs(3_600);
    let d_s5 = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await.payload.unwrap().value;
    let info_s5 = field(&field(&d_s5, 4).unwrap(), 2).unwrap();
    assert_eq!(varint(&info_s5, 1), Some(78), "1h at stage 5 yields 78 IDK");

    // Completing an MTO at max Development (100) grants +350 bonus infomorphs (78 -> 428) and lowers Anarchy 100 -> 90 (stage 4: -300 wf)!
    let acts = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest", id_req(1, den_id as u64)).await.payload.unwrap().value;
    let first_act = fields(&acts, 1).into_iter().next().unwrap();
    let mut start_req = Vec::new();
    encode_bytes_field(1, &field(&first_act, 1).unwrap(), &mut start_req);
    assert_eq!(rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", start_req).await.status_code, 200);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let mto_ball = ctx.db.with_snapshot(|s| s.sov_runtime.mto_site_balls.keys().copied().next().unwrap());
    let mto_pos = ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(mto_ball).await.unwrap().unwrap().1.position;
    fit_subcap_blasters(&ctx, PILOT, PILOT_SHIP, 985_200);
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 16_229, mto_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;
    lock_target_via_rpc(&ctx, &mut s_owner, null_sys, PILOT_SHIP, mto_ball).await;
    for _round in 0..25 {
        for gun in (0..6).map(|g| 985_200i64 + g * 2) {
            ctx.set_invention_roll_override(Some(0.45));
            let shot = match rpc_call(&ctx, &mut s_owner, "dogmaIM", "Activate", &[PyRep::Integer(gun), PyRep::Integer(mto_ball)]).await {
                Ok(r) => r,
                Err(e) if e.code == "ModuleAlreadyActive" => PyRep::None,
                Err(e) => panic!("dogmaIM.Activate turret on MTO ball: {e:?}"),
            };
            if shot.dict_get("destroyed") == Some(&PyRep::Bool(true))
                || ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&mto_ball))
            {
                break;
            }
        }
        if ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&mto_ball)) {
            break;
        }
        step_weapon_ticks(&ctx, null_sys).await;
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.mercenary_dens[&den_id].infomorphs), 428);
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.mercenary_dens[&den_id].anarchy_level), 90);
    assert_eq!(ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available) + 300, base_wf);

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_quasar_sovereignty_all_seven_protos_parity() {
    // DATA GAP: feat:quasar.sovereignty — server-only celestial resource tables (docs/BLOCKERS.md §9)
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/notices_pb2.pyj
    // DATA GAP: quasar:eveProto/generated/eve_public/sovereignty/resource/planet/api/requests_pb2.pyj — server-only planet resource table (docs/BLOCKERS.md §9)
    // DATA GAP: quasar:eveProto/generated/eve_public/sovereignty/resource/star/api/requests_pb2.pyj — server-only star power table (docs/BLOCKERS.md §9)
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/sovereignty/hub/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/notices_pb2.pyj",
    ]);

    let db_path = temp_sqlite_path("quasar_sov_protos");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, temp_planet, _, _, _) = find_nullsec_planets(&ctx);
    let (hub_id, skyhook_id) = (1_120_000_001i64, 1_120_000_002i64);
    let sh_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_owner = register_online_pilot(
        &ctx,
        86_001,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        null_sys,
        PILOT_SHIP,
        639,
        sh_pos + DVec3::new(2_500.0, 0.0, 0.0),
        corp_roles::ROLE_DIRECTOR,
    )
    .await;
    let mut rx = subscribe(&ctx, &mut s_owner).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ALLIANCE);
        snap.structures.insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(skyhook_id, structure(skyhook_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
        snap.skills.entry(PILOT).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord { type_id: TYPE_MERCENARY_DEN_MANAGEMENT, level: 4, skill_points: 180_000.0 },
        );
        snap.items.insert(
            986_001,
            InventoryItemRecord {
                item_id: 986_001,
                type_id: TYPE_MERCENARY_DEN,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(skyhook_id, TYPE_ORBITAL_SKYHOOK, sh_pos)).await;

    // 1. resource.planet.api (GetAllDefinitions, GetDefinitionsVersion, GetAllConfigurations) + invalid version negative check.
    let all_defs = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest", Vec::new()).await;
    let defs_list = fields(&all_defs.payload.unwrap().value, 1);
    assert_eq!(defs_list.len(), 8);
    assert_eq!(defs_list.iter().filter(|d| field(d, 4).is_some()).count(), 3);
    let lava_def = defs_list.iter().find(|d| varint(&field(d, 1).unwrap(), 1) == Some(2015)).expect("Lava planet definition");
    let lava_reagent_wrapper = field(lava_def, 4).expect("Lava reagent_definition");
    let lava_reagent_inner = field(&lava_reagent_wrapper, 2).expect("Lava inner ReagentDefinition");
    assert_eq!(varint(&lava_reagent_inner, 1), Some(50));
    assert_eq!(varint(&lava_reagent_inner, 2), Some(3_600));
    assert_eq!(varint(&lava_reagent_inner, 3), Some(50));
    assert_eq!(varint(&lava_reagent_inner, 4), Some(25_000));
    assert_eq!(varint(&lava_reagent_inner, 5), Some(25_000));
    assert_eq!(varint(&field(&lava_reagent_wrapper, 3).unwrap(), 1), Some(TYPE_MAGMATIC_GAS as u64));

    let ver_defs = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest", Vec::new()).await;
    assert_eq!(fields(&ver_defs.payload.unwrap().value, 1).len(), 8);
    let mut bad_ver = Vec::new();
    let mut spec = Vec::new();
    encode_varint_field(1, 99, &mut spec);
    encode_bytes_field(1, &spec, &mut bad_ver);
    assert_eq!(rpc(&ctx, &mut s_owner, "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest", bad_ver).await.status_code, 404);

    let planet_cfgs = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.resource.planet.api.GetAllConfigurationsRequest", Vec::new()).await;
    let cfg_list = fields(&planet_cfgs.payload.unwrap().value, 1);
    let expected_planets = ctx.sde.celestials_in_system(null_sys).into_iter().filter(|c| c.group_id == 7).count();
    assert_eq!(cfg_list.len(), expected_planets);
    let reagent_cfgs: Vec<Vec<u8>> = cfg_list.iter().filter_map(|c| field(c, 4)).collect();
    assert!(reagent_cfgs.len() >= 2);
    let gas_cfg = reagent_cfgs.iter().find(|c| varint(&field(c, 1).unwrap(), 1) == Some(TYPE_MAGMATIC_GAS as u64)).expect("Magmatic Gas config");
    assert_eq!(varint(&field(gas_cfg, 2).unwrap(), 4), Some(25_000));
    assert_eq!(varint(&field(gas_cfg, 3).unwrap(), 2), Some(86_400));
    let ice_cfg = reagent_cfgs.iter().find(|c| varint(&field(c, 1).unwrap(), 1) == Some(TYPE_SUPERIONIC_ICE as u64)).expect("Superionic Ice config");
    assert_eq!(varint(&field(ice_cfg, 2).unwrap(), 4), Some(25_000));
    assert_eq!(varint(&field(ice_cfg, 3).unwrap(), 2), Some(86_400));

    // 2. resource.star.api (GetAllConfigurationsRequest)
    let star_cfgs = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest", Vec::new()).await;
    let stars = fields(&star_cfgs.payload.unwrap().value, 1);
    assert_eq!(stars.is_empty(), false);
    assert_eq!(varint(&stars[0], 2).unwrap_or(0) >= 200, true);

    // 3. hub.api (GetResourcesRequest + non-owner forbidden check)
    let hub_res = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_id as u64)).await.payload.unwrap().value;
    assert_eq!(varint(&field(&hub_res, 1).unwrap(), 1).unwrap_or(0) > 0, true);
    assert_eq!(varint(&field(&hub_res, 2).unwrap(), 1).unwrap_or(0) > 0, true);

    let mut s_other = register_online_pilot(&ctx, 86_002, "other2", "sec-o", 90_000_310, 98_000_310, Some(99_000_310), null_sys, 100_000_310, 639, sh_pos, 0).await;
    assert_eq!(rpc(&ctx, &mut s_other, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_id as u64)).await.status_code, 403);

    // 4. mercenaryden.api + notices + mercenaryden.activity.api + notices
    let den_id = rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)])
        .await.unwrap().dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    let anchor_notices = drain_quasar(&mut rx);
    assert_eq!(anchor_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.api.DefinitionsUpdatedNotice"), true);
    assert_eq!(anchor_notices.iter().filter(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.AddedNotice").count(), 3);

    let max_rsp = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest", Vec::new()).await.payload.unwrap().value;
    assert_eq!(varint(&max_rsp, 1), Some(4));
    assert_eq!(varint(&max_rsp, 2), Some(5));

    let owned_rsp = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAllOwnedRequest", Vec::new()).await.payload.unwrap().value;
    assert_eq!(fields(&owned_rsp, 1).len(), 1);

    let owner_rsp = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await.payload.unwrap().value;
    let owner_attrs = field(&owner_rsp, 1).unwrap();
    assert_eq!(varint(&field(&owner_attrs, 1).unwrap(), 1), Some(PILOT as u64));
    assert_eq!(varint(&field(&owner_attrs, 2).unwrap(), 1), Some(skyhook_id as u64));
    assert_eq!(rpc(&ctx, &mut s_other, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await.status_code, 403);

    let cap_rsp = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityRequest", Vec::new()).await.payload.unwrap().value;
    assert_eq!(varint(&cap_rsp, 1), Some(3));
    let all_acts = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetAllRequest", Vec::new()).await.payload.unwrap().value;
    assert_eq!(fields(&all_acts, 1).len(), 3);
    let den_acts = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest", id_req(1, den_id as u64)).await.payload.unwrap().value;
    let act_list = fields(&den_acts, 1);
    assert_eq!(act_list.len(), 3);

    let pre_mto_bal = ctx.db.with_snapshot(|s| s.characters[&PILOT].balance.0);
    let mut start_req = Vec::new();
    encode_bytes_field(1, &field(&act_list[0], 1).unwrap(), &mut start_req);
    assert_eq!(rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", start_req).await.status_code, 200);
    let start_notices = drain_quasar(&mut rx);
    assert_eq!(start_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.StartedNotice"), true);
    assert_eq!(start_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.ExpiryChangedNotice"), true);

    // Spawn the live MTO target ball via cluster tick and destroy it through real `dogmaIM.AddTarget` + `dogmaIM.Activate` weapon fire!
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let site_ball = ctx.db.with_snapshot(|s| s.sov_runtime.mto_site_balls.keys().copied().next().unwrap());
    let site_pos = ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(site_ball).await.unwrap().unwrap().1.position;
    fit_subcap_blasters(&ctx, PILOT, PILOT_SHIP, 986_200);
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 16_229, site_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;
    lock_target_via_rpc(&ctx, &mut s_owner, null_sys, PILOT_SHIP, site_ball).await;
    for _round in 0..25 {
        for gun_id in (0..6).map(|g| 986_200i64 + g * 2) {
            ctx.set_invention_roll_override(Some(0.45));
            let shot = match rpc_call(&ctx, &mut s_owner, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(site_ball)]).await {
                Ok(r) => r,
                Err(e) if e.code == "ModuleAlreadyActive" => PyRep::None,
                Err(e) => panic!("dogmaIM.Activate turret on site ball: {e:?}"),
            };
            if shot.dict_get("destroyed") == Some(&PyRep::Bool(true))
                || ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&site_ball))
            {
                break;
            }
        }
        if ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&site_ball)) {
            break;
        }
        step_weapon_ticks(&ctx, null_sys).await;
    }
    assert!(ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&site_ball)));
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&PILOT].balance.0), pre_mto_bal + 7_500_000 * 100, "MTO completion awards 7.5M ISK");
    let comp_notices = drain_quasar(&mut rx);
    assert_eq!(comp_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.CompletedNotice"), true);
    assert_eq!(comp_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.api.EvolutionStateChangedNotice"), true);

    // Advance 4 days past unstarted activity expiry: `GetAllRequest` is strictly read-only (filters expired activities,
    // emits 0 notices, and does NOT mutate `mercenary_activities` or `simulated_at_unix` until tick/refresh runs).
    ctx.advance_secs(4 * 86_400);
    let snap_before_get_all = ctx.db.with_snapshot(|s| (s.sov_runtime.mercenary_activities.len(), s.sov_runtime.mercenary_dens[&den_id].simulated_at_unix));
    let get_all_expired = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetAllRequest", Vec::new()).await.payload.unwrap().value;
    assert!(fields(&get_all_expired, 1).is_empty(), "GetAllRequest filters out expired activities without mutating state");
    assert!(drain_quasar(&mut rx).is_empty(), "GetAllRequest must not emit notices");
    assert_eq!(
        ctx.db.with_snapshot(|s| (s.sov_runtime.mercenary_activities.len(), s.sov_runtime.mercenary_dens[&den_id].simulated_at_unix)),
        snap_before_get_all,
        "GetAllRequest must not mutate persisted mercenary_activities or simulated_at_unix"
    );

    let _ = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest", id_req(1, den_id as u64)).await;
    let exp_notices = drain_quasar(&mut rx);
    assert_eq!(exp_notices.iter().filter(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.RemovedNotice").count(), 2);

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod mercenary_dens_anarchy {
    use super::*;

//  End-to-end parity closure tests (Part 3) for Sovereignty Hub Power, Workforce,
//  Fuel, Upgrades, Skyhook Reagents & Sovereignty Upgrade Sites.


use crate::quasar_sov_equinox_hub::{field, fields, id_req, idm, rpc, structure, subscribe, varint};
use crate::sovereignty_equinox_gameplay::{
    drain_quasar, find_nullsec_planets, fit_subcap_blasters, lock_target_via_rpc, make_ball,
    open_sqlite_ctx, place_ship_ball, register_online_pilot, rpc_call, step_weapon_ticks,
    temp_sqlite_path, ALLIANCE, CORP, PILOT, PILOT_SHIP,
};
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord};
use eve_proto::quasar::{encode_bytes_field, encode_varint_field};
use eve_proto::PyRep;
use eve_server::services::sov_gameplay::{UPGRADE_MAJOR_THREAT_DETECTION, UPGRADE_PROSPECTING_ARRAY};
use eve_server::services::sov_services::{
    claim_system_sovereignty, TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB,
    TYPE_SUPERIONIC_ICE,
};
use glam::DVec3;

#[tokio::test]
async fn test_sov_power_workforce_fuel_upgrades_reagents_and_sites_parity() {
    // DATA GAP: feat:sovereignty.sov-hub-upgrades / sov-hub-fuel — 44 wiki-derived (build 3538132) upgrade definitions & 0 startup-cost placeholder pending V24.01 SDE import (docs/BLOCKERS.md §9)
    // DATA GAP: feat:sovereignty.sov-power — server-only celestial power table (docs/BLOCKERS.md §9)
    // DATA GAP: feat:sovereignty.sov-workforce — server-only celestial workforce table (docs/BLOCKERS.md §9)
    // DATA GAP: feat:sovereignty.sov-reagents — server-only planetary reagent yield table (docs/BLOCKERS.md §9)

    let db_path = temp_sqlite_path("sov_power_wf_sites");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, _, lava_planet, ice_planet, _) = find_nullsec_planets(&ctx);
    let mut adj_null: Vec<u32> = ctx
        .sde
        .jumps
        .values()
        .filter(|j| {
            j.from_solar_system_id == null_sys
                && ctx.sde.solar_systems.get(&j.to_solar_system_id).is_some_and(|s| {
                    s.security_status <= 0.0
                        && ctx.sde.constellations.get(&s.constellation_id).and_then(|c| c.faction_id).is_none()
                        && ctx.sde.regions.get(&s.region_id).and_then(|r| r.faction_id).is_none()
                })
        })
        .map(|j| j.to_solar_system_id)
        .filter(|&sys| {
            ctx.sde
                .celestials_in_system(sys)
                .into_iter()
                .filter(|c| c.group_id == 7)
                .map(|c| eve_server::services::quasar_sov_hub::planet_yield(c.type_id).1)
                .sum::<u64>()
                >= 5_000
        })
        .collect();
    adj_null.sort_unstable();
    let neighbor_sys = adj_null.into_iter().next().expect("adjacent null-sec system with >= 5000 workforce");
    let cels = ctx.sde.celestials_in_system(null_sys);
    let lava_pos = cels.iter().find(|c| c.celestial_id == lava_planet).unwrap().position;
    let ice_pos = cels.iter().find(|c| c.celestial_id == ice_planet).unwrap().position;
    let (hub_a, hub_b) = (1_090_000_001i64, 1_090_000_002i64);

    let mut s_dir = register_online_pilot(
        &ctx,
        83_001,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        null_sys,
        PILOT_SHIP,
        19_722,
        lava_pos,
        corp_roles::ROLE_DIRECTOR,
    )
    .await;
    let mut rx = subscribe(&ctx, &mut s_dir).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().solar_system_id = null_sys;
        snap.system_states.entry(neighbor_sys).or_default().solar_system_id = neighbor_sys;
        snap.structures.insert(hub_a, structure(hub_a, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(hub_b, structure(hub_b, CORP, neighbor_sys, TYPE_SOVEREIGNTY_HUB));
        // Anchor active Skyhooks on non-Lava/non-Ice planets in null_sys (leaving Lava & Ice free for live anchoring below)
        // and on all planets in neighbor_sys so planetary Power & Workforce are harvested solely from active Skyhooks.
        for (idx, cel) in ctx.sde.celestials_in_system(null_sys).into_iter().filter(|c| c.group_id == 7 && c.celestial_id != lava_planet && c.celestial_id != ice_planet).enumerate() {
            let sh_id = 1_090_000_100 + idx as i64;
            snap.structures.insert(sh_id, structure(sh_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
            snap.sov_runtime.skyhook_planets.insert(sh_id, cel.celestial_id);
        }
        for (idx, cel) in ctx.sde.celestials_in_system(neighbor_sys).into_iter().filter(|c| c.group_id == 7).enumerate() {
            let sh_id = 1_090_000_200 + idx as i64;
            snap.structures.insert(sh_id, structure(sh_id, CORP, neighbor_sys, TYPE_ORBITAL_SKYHOOK));
            snap.sov_runtime.skyhook_planets.insert(sh_id, cel.celestial_id);
        }
        for (iid, tid, loc, qty) in [
            (982_001, TYPE_MAGMATIC_GAS, hub_a, 3_000),
            (982_002, TYPE_SUPERIONIC_ICE, hub_a, 3_000),
            (982_004, 87_710, hub_a, 1), // Workforce Mecha-Tooling 1 (200 p, +5,000 wf, 8 Ice/hr)
            (982_005, UPGRADE_MAJOR_THREAT_DETECTION, hub_a, 1), // 400 p, 2,700 wf, 0 fuel
            (982_006, UPGRADE_PROSPECTING_ARRAY, hub_a, 1), // 450 p, 6,400 wf, 0 fuel
            (982_007, UPGRADE_MAJOR_THREAT_DETECTION, hub_a, 1),
            (982_008, 87_703, hub_a, 1), // Power Monitoring Division 1 ("ResourceManagement" group)
            (982_010, TYPE_ORBITAL_SKYHOOK, PILOT_SHIP, 2),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: PILOT,
                    location_id: loc,
                    flag: InventoryFlag::Cargo,
                    quantity: qty,
                    ..Default::default()
                },
            );
        }
    });
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(hub_a, TYPE_SOVEREIGNTY_HUB, DVec3::new(200_000.0, 0.0, 0.0))).await;
    claim_system_sovereignty(&ctx, null_sys, ALLIANCE, CORP, hub_a).expect("claim null_sys");
    claim_system_sovereignty(&ctx, neighbor_sys, ALLIANCE, CORP, hub_b).expect("claim neighbor_sys");

    // 1. Upgrade definitions from SDE (`GetDefinitionsRequest`) & Fuel deposit (`fuel.api.AddRequest` / `GetRequest`).
    let defs_rsp = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.GetDefinitionsRequest", Vec::new()).await;
    let defs = fields(&defs_rsp.payload.unwrap().value, 1);
    let mt1_def = defs.iter().find(|d| varint(&field(d, 1).unwrap(), 1) == Some(UPGRADE_MAJOR_THREAT_DETECTION as u64)).expect("Major Threat 1");
    assert_eq!((varint(mt1_def, 2), varint(mt1_def, 3), field(mt1_def, 4)), (Some(400), Some(2_700), None));
    let pa1_def = defs.iter().find(|d| varint(&field(d, 1).unwrap(), 1) == Some(UPGRADE_PROSPECTING_ARRAY as u64)).expect("Prospecting Array 1");
    assert_eq!((varint(pa1_def, 2), varint(pa1_def, 3), field(pa1_def, 4)), (Some(450), Some(6_400), None));
    let cyno_def = defs.iter().find(|d| varint(&field(d, 1).unwrap(), 1) == Some(81_619)).expect("Cynosural Suppression");
    assert_eq!((varint(cyno_def, 2), varint(cyno_def, 3)), (Some(250), Some(4_500)));
    assert_eq!(field(cyno_def, 4).and_then(|i| varint(&i, 1)), Some(TYPE_MAGMATIC_GAS as u64));
    assert_eq!(varint(cyno_def, 5), Some(205));

    for (fuel_iid, amt) in [(982_001i64, 1_000u64), (982_002i64, 1_000u64)] {
        let mut add = id_req(1, fuel_iid as u64);
        encode_varint_field(2, amt, &mut add);
        encode_bytes_field(3, &idm(hub_a as u64), &mut add);
        let r = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.fuel.api.AddRequest", add).await;
        assert_eq!(r.status_code, 200);
    }
    let _ = drain_quasar(&mut rx);
    let fuel_get = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.fuel.api.GetRequest", id_req(1, hub_a as u64)).await.payload.unwrap().value;
    let fuel_entries = fields(&fuel_get, 1);
    assert_eq!(fuel_entries.len(), 2);
    assert_eq!(fuel_entries.iter().all(|f| varint(f, 2) == Some(1_000)), true);
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&982_001].quantity), 2_000);

    // 2. Cross-system Workforce transfer (`GetNetworkableHubsRequest` + `ConfigureRequest`).
    let net_rsp = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.workforce.api.GetNetworkableHubsRequest", id_req(1, hub_a as u64)).await.payload.unwrap().value;
    assert_eq!(fields(&net_rsp, 1).iter().any(|b| field(b, 1).and_then(|h| varint(&h, 1)) == Some(hub_b as u64)), true);
    let wf0 = varint(&field(&rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_a as u64)).await.payload.unwrap().value, 2).unwrap(), 1).unwrap();
    let (mut exp, mut exp_cfg, mut cfg_b) = (Vec::new(), Vec::new(), id_req(1, hub_b as u64));
    encode_bytes_field(3, &idm(null_sys as u64), &mut exp);
    encode_varint_field(2, 3_500, &mut exp);
    encode_bytes_field(3, &exp, &mut exp_cfg);
    encode_bytes_field(2, &exp_cfg, &mut cfg_b);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_b).await.status_code, 200);
    let (mut src, mut imp, mut imp_cfg, mut cfg_a) = (Vec::new(), Vec::new(), Vec::new(), id_req(1, hub_a as u64));
    encode_bytes_field(1, &idm(neighbor_sys as u64), &mut src);
    encode_bytes_field(1, &src, &mut imp);
    encode_bytes_field(2, &imp, &mut imp_cfg);
    encode_bytes_field(2, &imp_cfg, &mut cfg_a);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_a).await.status_code, 200);
    let wf1 = varint(&field(&rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_a as u64)).await.payload.unwrap().value, 2).unwrap(), 1).unwrap();
    assert_eq!(wf1, wf0 + 3_500);

    // 3. Exclusive group conflict check + priority-slot power/workforce overload (`POWER_STATE_LOW`) + valid priority reorder:
    //    - Duplicate Major Threat Detection Array fails (`400`).
    //    - Conflicting `"ResourceManagement"` upgrades (`87710` + `87703`) fail (`400`).
    //    - Placing Prospecting Array 1 (`6,400` wf) in slot 0 BEFORE Workforce Mecha-Tooling 1 (`+5,000` wf) in slot 1 succeeds (`200`)
    //      and puts slot 0 into `POWER_STATE_LOW` (`3`) because base+imported wf is `5,000 < 6,400`.
    //    - Reordering Workforce Mecha-Tooling 1 (`87710`) into slot 0 brings all 3 upgrades to `POWER_STATE_ONLINE` (`2`)!
    let build_cfg = |new_iids: &[u64], tids: &[u64]| {
        let mut req = Vec::new();
        for &iid in new_iids {
            encode_bytes_field(1, &idm(iid), &mut req);
        }
        for &tid in tids {
            let mut c = Vec::new();
            encode_bytes_field(1, &idm(tid), &mut c);
            encode_varint_field(2, 1, &mut c);
            encode_bytes_field(2, &c, &mut req);
        }
        encode_bytes_field(3, &idm(hub_a as u64), &mut req);
        req
    };
    let bad_dup = build_cfg(&[982_004, 982_005, 982_007], &[87_710, UPGRADE_MAJOR_THREAT_DETECTION as u64]);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", bad_dup).await.status_code, 400);
    let bad_excl = build_cfg(&[982_004, 982_008], &[87_710, 87_703]);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", bad_excl).await.status_code, 400);

    let bad_prio = build_cfg(&[982_006, 982_004, 982_005], &[UPGRADE_PROSPECTING_ARRAY as u64, 87_710, UPGRADE_MAJOR_THREAT_DETECTION as u64]);
    let prio_rsp = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", bad_prio).await;
    assert_eq!(prio_rsp.status_code, 200);
    let prio_ups = fields(&field(&prio_rsp.payload.unwrap().value, 1).unwrap(), 2);
    assert_eq!(field(&prio_ups[0], 2).and_then(|a| varint(&a, 2)), Some(3), "slot 0 Prospecting Array enters POWER_STATE_LOW");

    let proc = build_cfg(&[], &[87_710, UPGRADE_MAJOR_THREAT_DETECTION as u64, UPGRADE_PROSPECTING_ARRAY as u64]);
    let ok_rsp = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc).await;
    assert_eq!(ok_rsp.status_code, 200);
    let ok_ups = fields(&field(&ok_rsp.payload.unwrap().value, 1).unwrap(), 2);
    assert!(ok_ups.iter().all(|u| field(u, 2).and_then(|a| varint(&a, 2)) == Some(2)), "reordered upgrades all POWER_STATE_ONLINE");

    // 4. Spawn upgrade sites (`tick_background_cluster`), verify mineable asteroid & killable NPC rat balls in space!
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let balls = ctx.systems.get_or_spawn(null_sys).list_balls().await.unwrap();
    let ore_ball = balls
        .iter()
        .find(|b| matches!(b.type_id, 18..=22 | 1_223..=1_232))
        .expect("spawned asteroid ball");
    let npc_balls: Vec<_> = balls.iter().filter(|b| b.type_id == 37_453).cloned().collect();
    // Major Threat Detection Array I: 3 wiki combat sites (Haven/Sanctum band) x 3 NPCs each, plus 1 ore site.
    assert_eq!(npc_balls.len(), 9);
    let kinds: Vec<String> = ctx.db.with_snapshot(|s| s.cosmic_anomalies[&null_sys].iter().map(|a| a.anomaly_type.clone()).collect());
    assert_eq!((kinds.iter().filter(|k| *k == "Combat").count(), kinds.iter().filter(|k| *k == "Ore").count()), (3, 1));

    // Kill every spawned upgrade-site NPC rat via live subcap blaster fire -> credits Military Index, marks site cleared, and blocks instant respawn!
    fit_subcap_blasters(&ctx, PILOT, PILOT_SHIP, 982_200);
    let mut last_shot = PyRep::None;
    for (idx, npc_ball) in npc_balls.iter().enumerate() {
        place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 16_229, npc_ball.position + DVec3::new(2_500.0, 0.0, 0.0)).await;
        lock_target_via_rpc(&ctx, &mut s_dir, null_sys, PILOT_SHIP, npc_ball.id).await;
        for _round in 0..8 {
            for gun_id in (0..6).map(|g| 982_200i64 + g * 2) {
                ctx.set_invention_roll_override(Some(0.05));
                last_shot = rpc_call(&ctx, &mut s_dir, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(npc_ball.id)])
                    .await
                    .expect("activate turret on upgrade-site NPC");
                if last_shot.dict_get("destroyed") == Some(&PyRep::Bool(true))
                    || ctx.db.with_snapshot(|s| s.sov_runtime.sov_npc_kills.get(&null_sys).copied().unwrap_or(0)) > idx as u64
                {
                    break;
                }
            }
            if ctx.db.with_snapshot(|s| s.sov_runtime.sov_npc_kills.get(&null_sys).copied().unwrap_or(0)) > idx as u64 {
                break;
            }
            step_weapon_ticks(&ctx, null_sys).await;
        }
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.sov_npc_kills.get(&null_sys).copied().unwrap_or(0)), npc_balls.len() as u64, "last_shot={last_shot:?}");
    assert!(ctx.db.with_snapshot(|s| s.cosmic_anomalies[&null_sys].iter().any(|a| a.anomaly_type == "Combat" && a.cleared)));
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(!ctx.systems.get_or_spawn(null_sys).list_balls().await.unwrap().iter().any(|b| b.type_id == 37_453), "cleared Threat Detection site must not immediately respawn");
    assert_eq!(matches!(ore_ball.type_id, 18..=22 | 1_223..=1_232), true);

    // 5. Skyhook reagents on Lava (81143 Magmatic Gas) & Ice (81144 Superionic Ice) + extraction to ship cargo.
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, lava_pos).await;
    let lava_sh = rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(lava_planet)])
        .await.unwrap().dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, ice_pos).await;
    let ice_sh = rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(ice_planet)])
        .await.unwrap().dict_get("structureID").and_then(|v| v.as_i64()).unwrap();

    // Advance 1h to harvest initial insecure batch, then 24h to mature secured reagents, then extract Magmatic Gas to ship cargo.
    ctx.advance_secs(3_600);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    ctx.advance_secs(24 * 3_600);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let (lt, lsec, _, _) = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays[&lava_sh]);
    let (it, isec, _, _) = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays[&ice_sh]);
    assert_eq!(lt, TYPE_MAGMATIC_GAS);
    assert_eq!(it, TYPE_SUPERIONIC_ICE);
    assert_eq!(lsec > 0 && isec > 0, true);

    let lava_sh_pos = ctx.db.with_snapshot(|s| DVec3::from_array(s.structures[&lava_sh].position));
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, lava_sh_pos + DVec3::new(2_500.0, 0.0, 0.0)).await;
    let ext = rpc_call(&ctx, &mut s_dir, "beyonce", "CallComponentFromClient", &[PyRep::Integer(lava_sh), PyRep::String("orbitalSkyhook".into()), PyRep::String("ExtractReagentsToShip".into())])
        .await.expect("extract Magmatic Gas");
    assert_eq!(ext.dict_get("extractedQuantity").and_then(|v| v.as_i64()), Some(lsec as i64));

    // Advance 200 more hours: Workforce Mecha-Tooling 1 (`87710`, 8 Ice/hr) exhausts the 1,000 Superionic Ice bay,
    // dropping `87710` and cascaded `Prospecting Array 1` (`6,400 > 5,000` remaining wf) to `POWER_STATE_LOW`, while
    // zero-fuel `Major Threat Detection Array 1` (`400` power, `2,700 <= 5,000` wf) stays online!
    ctx.advance_secs(200 * 3_600);
    let res_post = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_a as u64)).await.payload.unwrap().value;
    assert_eq!(varint(&field(&res_post, 1).unwrap(), 2), Some(400), "Mecha-Tooling + cascaded Prospecting drop to low power while zero-fuel Major Threat 1 stays online at 400p");

    // Decommissioning an upgrade destroys it without minting a hangar item (`Sovereignty.md:37`).
    let _ = drain_quasar(&mut rx);
    let (mut un_ident, mut un_req) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &idm(hub_a as u64), &mut un_ident);
    encode_bytes_field(2, &idm(87_710), &mut un_ident);
    encode_bytes_field(1, &un_ident, &mut un_req);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.UninstallRequest", un_req).await.status_code, 200);
    assert!(drain_quasar(&mut rx).iter().any(|(t, _)| t == "eve_public.sovereignty.hub.upgrade.api.UninstalledNotice"));
    assert!(ctx.db.with_snapshot(|s| !s.items.values().any(|i| i.type_id == 87_710 && i.location_id == hub_a)));

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_skyhook_harvest_gating_anarchy_and_persisted_npc_durability() {
    let db_path = temp_sqlite_path("sov_harvest_npc_durability");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, temp_planet, _, _, _) = find_nullsec_planets(&ctx);
    let hub_id = 1_130_000_001i64;
    let temp_sh_id = 1_130_000_010i64;
    let hub_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_dir = register_online_pilot(
        &ctx, 88_001, "demo", "demo-secret", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 19_722, hub_pos + DVec3::new(2_500.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;
    let mut rx = subscribe(&ctx, &mut s_dir).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ALLIANCE);
        snap.structures.insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        snap.items.insert(
            988_001,
            InventoryItemRecord {
                item_id: 988_001,
                type_id: eve_server::services::sov_gameplay::UPGRADE_MINOR_THREAT_DETECTION,
                owner_id: PILOT,
                location_id: hub_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    claim_system_sovereignty(&ctx, null_sys, ALLIANCE, CORP, hub_id).expect("claim null_sys");

    // 1. With 0 Orbital Skyhooks, planetary Power & Workforce are 0 (only stellar power = 300).
    let r0 = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_id as u64)).await.payload.unwrap().value;
    assert_eq!(varint(&field(&r0, 1).unwrap(), 1), Some(300));
    assert_eq!(varint(&field(&r0, 2).unwrap(), 1).unwrap_or(0), 0);

    // Anchor active Skyhook on non-first Temperate planet -> harvests +1,500 workforce; Anarchy stage 5 siphons 600 -> 900 wf.
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(temp_sh_id, structure(temp_sh_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(temp_sh_id, temp_planet);
        snap.sov_runtime.mercenary_dens.insert(
            1_130_000_099,
            eve_db::PersistedMercenaryDen {
                den_id: 1_130_000_099,
                owner_char_id: PILOT,
                skyhook_id: temp_sh_id,
                solar_system_id: null_sys,
                planet_id: temp_planet,
                type_id: eve_server::services::sov_gameplay::TYPE_MERCENARY_DEN,
                enabled: true,
                cargo_extraction_enabled: true,
                development_level: 100,
                anarchy_level: 100,
                simulated_at_unix: ctx.now_unix(),
                infomorphs: 0,
                next_activity_generation_unix: ctx.now_unix() + 86_400,
            },
        );
    });
    let r_active = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_id as u64)).await.payload.unwrap().value;
    assert_eq!(varint(&field(&r_active, 2).unwrap(), 1), Some(900), "1,500 harvested - 40% Anarchy siphon = 900 wf");

    // Deactivating the Skyhook drops harvested workforce to 0 (Anarchy siphons 0 when unharvested) and emits exact bound `temp_planet` in ActivationNotice!
    let _ = drain_quasar(&mut rx);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.skyhook.api.DeactivateRequest", id_req(1, temp_sh_id as u64)).await.status_code, 200);
    let deact_n = drain_quasar(&mut rx);
    let act_notice = deact_n.iter().find(|(t, _)| t == "eve_public.sovereignty.skyhook.api.ActivationNotice").expect("ActivationNotice");
    assert_eq!(field(&act_notice.1, 2).and_then(|i| varint(&i, 1)), Some(temp_planet as u64), "ActivationNotice carries bound non-first planet_id");
    let r_deact = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_id as u64)).await.payload.unwrap().value;
    assert_eq!(varint(&field(&r_deact, 2).unwrap(), 1).unwrap_or(0), 0);

    // Re-activating restores harvest; destroying the Skyhook removes harvest again.
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.skyhook.api.ActivateRequest", id_req(1, temp_sh_id as u64)).await.status_code, 200);
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.mercenary_dens.clear();
        snap.structures.get_mut(&temp_sh_id).unwrap().state = "Destroyed".into();
    });
    let r_dest = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_id as u64)).await.payload.unwrap().value;
    assert_eq!(varint(&field(&r_dest, 2).unwrap(), 1).unwrap_or(0), 0);

    // 2. Restore Temperate + neighbor workforce, online Minor Threat Detection Array 1 (200p, 1,800wf, 0 fuel), spawn Threat Detection NPCs,
    //    and verify `sov_upgrade_combat_npcs` isolation, rollback safety, and SQLite reload durability!
    let neigh = ctx.sde.jumps.values().find(|j| {
        j.from_solar_system_id == null_sys
            && eve_server::services::sov_gameplay::is_claimable_nullsec_system(&ctx.sde, j.to_solar_system_id)
            && ctx.sde.celestials_in_system(j.to_solar_system_id).into_iter().any(|c| matches!(c.type_id, 11 | 2014 | 2016))
    }).unwrap().to_solar_system_id;
    let hb = 1_130_000_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&temp_sh_id).unwrap().state = "ShieldVulnerable".into();
        let sh2 = 1_130_000_011i64;
        snap.system_states.entry(neigh).or_default().solar_system_id = neigh;
        snap.structures.insert(hb, structure(hb, CORP, neigh, TYPE_SOVEREIGNTY_HUB));
        let np = ctx.sde.celestials_in_system(neigh).into_iter().find(|c| matches!(c.type_id, 11 | 2014 | 2016)).unwrap().celestial_id;
        snap.structures.insert(sh2, structure(sh2, CORP, neigh, TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(sh2, np);
    });
    claim_system_sovereignty(&ctx, neigh, ALLIANCE, CORP, hb).expect("claim neigh");
    let (mut exp, mut exp_cfg, mut cfg_hb) = (Vec::new(), Vec::new(), id_req(1, hb as u64));
    encode_bytes_field(3, &idm(null_sys as u64), &mut exp);
    encode_varint_field(2, 500, &mut exp);
    encode_bytes_field(3, &exp, &mut exp_cfg);
    encode_bytes_field(2, &exp_cfg, &mut cfg_hb);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_hb).await.status_code, 200);
    let (mut src, mut imp, mut imp_cfg, mut cfg_h) = (Vec::new(), Vec::new(), Vec::new(), id_req(1, hub_id as u64));
    encode_bytes_field(1, &idm(neigh as u64), &mut src);
    encode_bytes_field(1, &src, &mut imp);
    encode_bytes_field(2, &imp, &mut imp_cfg);
    encode_bytes_field(2, &imp_cfg, &mut cfg_h);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_h).await.status_code, 200);
    let mut proc = id_req(1, 988_001);
    let mut cfg = Vec::new();
    encode_bytes_field(1, &idm(eve_server::services::sov_gameplay::UPGRADE_MINOR_THREAT_DETECTION as u64), &mut cfg);
    encode_varint_field(2, 1, &mut cfg);
    encode_bytes_field(2, &cfg, &mut proc);
    encode_bytes_field(3, &idm(hub_id as u64), &mut proc);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc).await.status_code, 200);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let tracked_nid = ctx.db.with_snapshot(|s| *s.sov_runtime.sov_upgrade_combat_npcs.iter().next().expect("persisted Threat Detection NPC ID"));
    let other_ctx = eve_server::router::ServerContext::new();
    assert!(
        other_ctx.db.with_snapshot(|s| !s.sov_runtime.sov_upgrade_combat_npcs.contains(&tracked_nid)),
        "separate ServerContext must not share persisted Threat Detection NPC set"
    );
    let _: Result<(), eve_proto::BridgeRpcError> = ctx.db.try_with_mut_snapshot(|snap| {
        snap.sov_runtime.sov_upgrade_combat_npcs.insert(777_888_999);
        Err(eve_proto::BridgeRpcError { code: "Abort".into(), message: "rollback test".into() })
    });
    assert!(ctx.db.with_snapshot(|s| !s.sov_runtime.sov_upgrade_combat_npcs.contains(&777_888_999)), "failed transaction rolls back sov_upgrade_combat_npcs");

    drop(ctx);
    let reloaded = open_sqlite_ctx(&db_path, false);
    assert!(
        reloaded.db.with_snapshot(|s| s.sov_runtime.sov_upgrade_combat_npcs.contains(&tracked_nid)),
        "sov_upgrade_combat_npcs survives SQLite reopen"
    );
    eve_server::sp6_lifecycle::tick_background_cluster(&reloaded).await;
    let npc_pos = reloaded.systems.get_or_spawn(null_sys).get_ball_snapshot(tracked_nid).await.unwrap().unwrap().1.position;
    let mut s_rel = register_online_pilot(
        &reloaded, 88_002, "demo", "demo-secret", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 16_229, npc_pos + DVec3::new(2_500.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;
    fit_subcap_blasters(&reloaded, PILOT, PILOT_SHIP, 988_300);
    lock_target_via_rpc(&reloaded, &mut s_rel, null_sys, PILOT_SHIP, tracked_nid).await;
    for _round in 0..8 {
        for gun_id in (0..6).map(|g| 988_300i64 + g * 2) {
            reloaded.set_invention_roll_override(Some(0.05));
            let res = rpc_call(&reloaded, &mut s_rel, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(tracked_nid)]).await.unwrap();
            if res.dict_get("destroyed") == Some(&PyRep::Bool(true)) {
                break;
            }
        }
        if reloaded.db.with_snapshot(|s| !s.sov_runtime.sov_upgrade_combat_npcs.contains(&tracked_nid)) {
            break;
        }
        step_weapon_ticks(&reloaded, null_sys).await;
    }
    assert_eq!(reloaded.db.with_snapshot(|s| s.sov_runtime.sov_npc_kills.get(&null_sys).copied().unwrap_or(0)), 1, "post-reload Threat Detection rat kill credits sov_npc_kills");
    assert!(reloaded.db.with_snapshot(|s| !s.sov_runtime.sov_upgrade_combat_npcs.contains(&tracked_nid)));

    drop(reloaded);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_sov_anchoring_item_locks_and_posmgr_mercenary_den_guards() {
    let db_path = temp_sqlite_path("sov_anchor_locks_posmgr");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, temp_planet, lava_planet, _, _) = find_nullsec_planets(&ctx);
    let cels = ctx.sde.celestials_in_system(null_sys);
    let temp_pos = cels.iter().find(|c| c.celestial_id == temp_planet).unwrap().position;
    let lava_pos = cels.iter().find(|c| c.celestial_id == lava_planet).unwrap().position;

    let mut s_dir = register_online_pilot(
        &ctx, 89_001, "demo", "demo-secret", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 19_722, lava_pos, corp_roles::ROLE_DIRECTOR,
    ).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT).or_default().insert(
            85_233,
            eve_db::TrainedSkillRecord { type_id: 85_233, level: 3, skill_points: 100_000.0 },
        );
        for (iid, tid, owner, flg) in [
            (989_001, TYPE_SOVEREIGNTY_HUB, PILOT, InventoryFlag::Cargo),  // contract-locked
            (989_002, TYPE_SOVEREIGNTY_HUB, PILOT, InventoryFlag::Hangar), // non-cargo
            (989_003, TYPE_SOVEREIGNTY_HUB, 90_999_888, InventoryFlag::Cargo), // unowned
            (989_011, TYPE_ORBITAL_SKYHOOK, PILOT, InventoryFlag::Cargo),  // contract-locked
            (989_012, TYPE_ORBITAL_SKYHOOK, PILOT, InventoryFlag::Hangar), // non-cargo
            (989_013, TYPE_ORBITAL_SKYHOOK, 90_999_888, InventoryFlag::Cargo), // unowned
            (989_021, eve_server::services::sov_gameplay::TYPE_MERCENARY_DEN, PILOT, InventoryFlag::Cargo), // contract-locked
            (989_022, eve_server::services::sov_gameplay::TYPE_MERCENARY_DEN, PILOT, InventoryFlag::Hangar), // non-cargo
            (989_023, eve_server::services::sov_gameplay::TYPE_MERCENARY_DEN, 90_999_888, InventoryFlag::Cargo), // unowned
            (989_024, eve_server::services::sov_gameplay::TYPE_MERCENARY_DEN, PILOT, InventoryFlag::Cargo), // valid #1
            (989_025, eve_server::services::sov_gameplay::TYPE_MERCENARY_DEN, PILOT, InventoryFlag::Cargo), // valid #2
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: owner,
                    location_id: PILOT_SHIP,
                    flag: flg,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.contracts.insert(
            700_001,
            eve_db::ContractRecord {
                contract_id: 700_001,
                issuer_id: PILOT,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                item_ids: vec![989_001, 989_011, 989_021],
                ..Default::default()
            },
        );
    });

    // 1. Sov Hub anchoring rejects contract-locked, non-cargo, and unowned items (both by item_id and by type_id).
    assert_eq!(rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(989_001)]).await.unwrap_err().code, "ItemLockedInContractOrJob");
    assert_eq!(rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(989_002)]).await.unwrap_err().code, "ItemNotInCargo");
    assert_eq!(rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(989_003)]).await.unwrap_err().code, "ItemOwnershipViolation");
    assert_eq!(rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_SOVEREIGNTY_HUB as i64)]).await.unwrap_err().code, "MissingStructureItem");

    // 2. Seed Sov Hub + claim sovereignty; Skyhook anchoring rejects contract-locked, non-cargo, and unowned items.
    let hub_id = 1_140_000_001i64;
    let lava_sh = 1_140_000_010i64;
    let temp_sh = 1_140_000_011i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().solar_system_id = null_sys;
        snap.structures.insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        let mut lsh = structure(lava_sh, CORP, null_sys, TYPE_ORBITAL_SKYHOOK);
        lsh.position = lava_pos.to_array();
        snap.structures.insert(lava_sh, lsh);
        snap.sov_runtime.skyhook_planets.insert(lava_sh, lava_planet);
        let mut tsh = structure(temp_sh, CORP, null_sys, TYPE_ORBITAL_SKYHOOK);
        tsh.position = temp_pos.to_array();
        snap.structures.insert(temp_sh, tsh);
        snap.sov_runtime.skyhook_planets.insert(temp_sh, temp_planet);
    });
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(lava_sh, TYPE_ORBITAL_SKYHOOK, lava_pos)).await;
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(temp_sh, TYPE_ORBITAL_SKYHOOK, temp_pos)).await;
    claim_system_sovereignty(&ctx, null_sys, ALLIANCE, CORP, hub_id).expect("claim null_sys");

    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, temp_pos).await;
    let (_, _, _, ice_planet, _) = find_nullsec_planets(&ctx);
    let ice_pos = cels.iter().find(|c| c.celestial_id == ice_planet).unwrap().position;
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, ice_pos).await;
    assert_eq!(rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(989_011), PyRep::Integer(ice_planet)]).await.unwrap_err().code, "ItemLockedInContractOrJob");
    assert_eq!(rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(989_012), PyRep::Integer(ice_planet)]).await.unwrap_err().code, "ItemNotInCargo");
    assert_eq!(rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(989_013), PyRep::Integer(ice_planet)]).await.unwrap_err().code, "ItemOwnershipViolation");
    assert_eq!(rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(ice_planet)]).await.unwrap_err().code, "MissingSkyhookItem");

    // 3. `posMgr.AnchorStructure` for Mercenary Den (`85230`):
    //    - Rejects non-Temperate Lava Skyhook (`PlanetNotTemperate`), >10 km distance (`MercenaryDenTooFarFromSkyhook`),
    //      unclaimed nullsec (`SovereigntyNotClaimed`), Zarzakh (`SovereigntyNullsecRequired`), and locked/non-cargo/unowned items!
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, lava_pos + DVec3::new(2_000.0, 0.0, 0.0)).await;
    let _ = ctx.drain_notifications_for_char(PILOT);
    assert_eq!(rpc_call(&ctx, &mut s_dir, "posMgr", "AnchorStructure", &[PyRep::Integer(989_024)]).await.unwrap_err().code, "PlanetNotTemperate");
    let inv_n = ctx.drain_notifications_for_char(PILOT).into_iter().find(|n| n.event == "OnMercenaryDenRegistrationInvalid").expect("OnMercenaryDenRegistrationInvalid");
    assert_eq!(inv_n.args[1].dict_get("itemID"), Some(&PyRep::Integer(989_024)));
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, temp_pos + DVec3::new(25_000.0, 0.0, 0.0)).await;
    assert_eq!(rpc_call(&ctx, &mut s_dir, "posMgr", "AnchorStructure", &[PyRep::Integer(989_024)]).await.unwrap_err().code, "MercenaryDenTooFarFromSkyhook");
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, temp_pos + DVec3::new(2_500.0, 0.0, 0.0)).await;
    assert_eq!(rpc_call(&ctx, &mut s_dir, "posMgr", "AnchorStructure", &[PyRep::Integer(989_021)]).await.unwrap_err().code, "ItemLockedInContractOrJob");
    assert_eq!(rpc_call(&ctx, &mut s_dir, "posMgr", "AnchorStructure", &[PyRep::Integer(989_022)]).await.unwrap_err().code, "ItemNotInCargo");
    assert_eq!(rpc_call(&ctx, &mut s_dir, "posMgr", "AnchorStructure", &[PyRep::Integer(989_023)]).await.unwrap_err().code, "ItemOwnershipViolation");

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&null_sys).unwrap().sov_alliance_id = None;
    });
    assert_eq!(rpc_call(&ctx, &mut s_dir, "posMgr", "AnchorStructure", &[PyRep::Integer(989_024)]).await.unwrap_err().code, "SovereigntyNotClaimed");
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&null_sys).unwrap().sov_alliance_id = Some(ALLIANCE);
    });

    // Valid 10-arg V24 `structureDeployment.Anchor` within 10 km of Temperate Skyhook in claimed nullsec succeeds, and a second den on the same Skyhook fails!
    let _ = ctx.drain_notifications_for_char(PILOT);
    let extra_cfg = PyRep::Dict(vec![
        (PyRep::String("skyhookID".into()), PyRep::Integer(temp_sh)),
        (PyRep::String("planetID".into()), PyRep::Integer(temp_planet)),
    ]);
    let ok_den = rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[
        PyRep::Integer(989_024),
        PyRep::Real(temp_pos.x + 2_500.0),
        PyRep::Real(temp_pos.z),
        PyRep::Real(0.0),
        PyRep::Integer(1),
        PyRep::String("Custom Den".into()),
        PyRep::String(String::new()),
        PyRep::Integer(0),
        PyRep::Integer(0),
        extra_cfg,
    ]).await.expect("10-arg structureDeployment.Anchor Mercenary Den");
    let ok_den_id = ok_den.dict_get("structureID").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(ok_den_id > 0);
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&ok_den_id].name.clone()), "Custom Den");
    assert!(ctx.drain_notifications_for_char(PILOT).iter().any(|n| n.event == "OnMercenaryDenRegistrationSuccess"));
    assert_eq!(rpc_call(&ctx, &mut s_dir, "posMgr", "AnchorStructure", &[PyRep::Integer(989_025)]).await.unwrap_err().code, "MercenaryDenAlreadyAnchoredOnSkyhook");

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

}
