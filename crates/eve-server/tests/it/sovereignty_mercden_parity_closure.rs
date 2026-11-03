//! End-to-end parity closure tests (Part 1) for Equinox Sovereignty Hub anchoring,
//! sovereignty claim/loss notifications, earned Development Indices, live ADM Entosis
//! scaling, Power, Workforce, Fuel, Upgrades, Reagents & Upgrade Sites.

#![forbid(unsafe_code)]

use crate::quasar_sov_equinox_hub::structure;
use crate::sovereignty_equinox_gameplay_closure::{
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
    // parity: feat:sovereignty.sov-hub-anchor
    // parity: feat:sovereignty.sov-changed-notifications
    eve_testkit::attest(&[
        "feat:sovereignty.sov-hub-anchor",
        "feat:sovereignty.sov-changed-notifications",
    ]);

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
    // parity: feat:sovereignty.sov-dev-indices
    // parity: feat:sovereignty.sov-adm
    // parity: notify:OnSolarSystemDevIndexChanged
    eve_testkit::attest(&[
        "feat:sovereignty.sov-dev-indices",
        "feat:sovereignty.sov-adm",
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
                let res = rpc_call(&ctx, &mut s_def, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(rat_id)])
                    .await
                    .expect("kill Guristas rat");
                if res.dict_get("destroyed") == Some(&PyRep::Bool(true)) {
                    killed = true;
                    break;
                }
            }
            if killed {
                break;
            }
            if round % 15 == 14 {
                step_weapon_ticks(&ctx, sys_high).await;
            }
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
