//! Integration tests closing `plan:C-3`, `plan:C-4`, and their 8 combat/lifecycle/NPC parity rows.
//!
// parity: plan:C-3, plan:C-4
// parity: feat:lifecycle.death-session-change, feat:lifecycle.wreck-owner-victim
// parity: feat:lifecycle.pod-death-pipeline-corpse, feat:lifecycle.wreck-abandon
// parity: feat:lifecycle.jetcan-empty, feat:lifecycle.courier-package-destroyed
// parity: feat:npc.npc-target-switching, feat:npc.clone-soldier-spawns

#![forbid(unsafe_code)]

use std::collections::HashMap;
use eve_db::{ContractRecord, InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::death_pipeline::{execute_death_pipeline, execute_death_pipeline_sync};
use eve_server::combat::npc_ai::{
    clone_soldier_type_for_security, clone_soldier_type_for_security_and_faction, regional_pirate_faction_id,
};
use eve_server::router::{BoundObjectEntry, ServerContext};
use eve_server::services::combat_crimewatch::combat_npc_ai::{
    destroy_specialized_npc_and_queue_bounty, record_npc_threat_event, spawn_specialized_npc_entity, step_extended_npc_ai_sync,
};
use eve_server::services::combat_crimewatch::combat_weapons_spool::handle_ship_or_npc_destruction;
use eve_server::services::combat_crimewatch::{
    step_combat_system, with_combat_runtime, with_mut_combat_runtime, InFlightMissileState,
};
use glam::DVec3;

fn new_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
}

fn new_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball { id, type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.5, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }
}

#[tokio::test]
async fn test_c3_ship_death_session_change_timer_and_wreck_owner_victim() {
    // parity: feat:lifecycle.death-session-change, feat:lifecycle.wreck-owner-victim, plan:C-3
    eve_testkit::attest(&[
        "feat:lifecycle.death-session-change",
        "feat:lifecycle.wreck-owner-victim",
        "plan:C-3",
    ]);

    let ctx = ServerContext::new();
    let sys_id = 30_000_142u32;
    let (victim_id, attacker_id, looter_id) = (90_000_001i64, 90_000_002i64, 90_000_003i64);
    let (victim_ship_id, looter_ship_id) = (610_001_001i64, 610_001_003i64);

    ctx.db.with_mut_snapshot(|snap| {
        for &(cid, sid, corp_id) in &[
            (victim_id, victim_ship_id, 2_000_101i64),
            (attacker_id, 610_001_002i64, 2_000_102i64),
            (looter_id, looter_ship_id, 2_000_103i64),
        ] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = sid;
                ch.corporation_id = corp_id;
            }
            snap.items.insert(sid, new_item(sid, 587, cid, sys_id as i64, InventoryFlag::None, 1, true));
        }
        for i in 0..20 {
            let iid = 610_002_000 + i;
            snap.items.insert(iid, new_item(iid, 34, victim_id, victim_ship_id, InventoryFlag::Cargo, 100, false));
        }
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for &sid in &[victim_ship_id, looter_ship_id] {
        let _ = sys_handle
            .spawn_ball_with_combat(new_ball(sid, 587, DVec3::new(1_000.0, 0.0, 0.0)), ctx.build_type_combat_state(587))
            .await;
    }

    let mut victim_sess = SessionState::default();
    victim_sess.session_id = 1;
    victim_sess.user_id = Some(1);
    victim_sess.character_id = Some(victim_id);
    victim_sess.solar_system_id = Some(sys_id);
    victim_sess.ship_id = Some(victim_ship_id);

    let mut notify_rx2 = ctx.notify_tx.subscribe();
    let now_unix = ctx.now_unix();
    let _ = ctx.db.acquire_online_lock(1, 1, Some(victim_id), true, now_unix);
    let _ = ctx.db.acquire_online_lock(2, 1, Some(victim_id), true, now_unix);
    let outcome = execute_death_pipeline(
        &ctx,
        sys_id,
        victim_id,
        victim_ship_id,
        attacker_id,
        DVec3::new(1_000.0, 0.0, 0.0),
        now_unix,
    )
    .await
    .expect("ship death pipeline must succeed");

    let capsule_id = outcome.ejected_capsule_id.expect("Rifter destruction must eject a capsule");
    assert_ne!(capsule_id, victim_ship_id);

    // 1. OnSessionChanged notification must reflect shipid -> capsule_id across both session streams
    let notifs = ctx.drain_notifications_for_char(victim_id);
    let saw_session_change = notifs.iter().any(|n| {
        n.event == "OnSessionChanged"
            && format!("{:?}", n.args).contains("shipid")
            && format!("{:?}", n.args).contains(&capsule_id.to_string())
    });
    assert!(saw_session_change, "OnSessionChanged must be emitted for ship death -> capsule");
    let mut saw_rx2 = false;
    for _ in 0..64 {
        let Ok((cid, n)) = notify_rx2.try_recv() else { break };
        if cid == victim_id && n.event == "OnSessionChanged" && format!("{:?}", n.args).contains(&capsule_id.to_string()) {
            saw_rx2 = true;
        }
    }
    assert!(saw_rx2, "Second session stream must also receive OnSessionChanged");

    // 2. Session change timer (10s) must block immediate CmdDock even if session still has old ship_id
    let bid = ctx.bind_object(&mut victim_sess, "beyonce", sys_id as i64).expect("bind beyonce");
    let dock_err = ctx
        .dispatch_rpc(&mut victim_sess, "beyonce", Some(bid), "CmdDock", &[PyRep::Integer(60_003_760), PyRep::Integer(capsule_id)], &HashMap::new(), None)
        .await
        .expect_err("CmdDock during 10s death sessionChangeTimer must fail");
    assert_eq!(dock_err.code, "SessionChangeInProgress");

    // 3. Wreck owner must be the VICTIM (`feat:lifecycle.wreck-owner-victim`), not the attacker
    let (wreck_owner, dropped_owners_all_victim) = ctx.db.with_snapshot(|snap| {
        let w = snap.items.get(&outcome.wreck_item_id).expect("wreck item exists");
        let children_ok = snap.items.values().filter(|i| i.location_id == outcome.wreck_item_id).all(|i| i.owner_id == victim_id);
        (w.owner_id, children_ok)
    });
    assert_eq!(wreck_owner, victim_id, "Player ship wreck must be owned by victim");
    assert!(dropped_owners_all_victim, "Dropped items in victim wreck must be owned by victim");

    let slim_owner = sys_handle
        .ballpark
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .ball_slim_meta
        .get(&outcome.wreck_item_id)
        .expect("wreck slim meta present")
        .owner_id;
    assert_eq!(slim_owner, victim_id, "Wreck SlimItem owner_id must equal victim_id");

    // 4. Third-party looter taking from victim's wreck triggers Crimewatch Suspect flag
    let mut looter_sess = SessionState::default();
    looter_sess.user_id = Some(3);
    looter_sess.character_id = Some(looter_id);
    looter_sess.solar_system_id = Some(sys_id);
    looter_sess.ship_id = Some(looter_ship_id);

    let loot_res = ctx
        .dispatch_rpc(&mut looter_sess, "invbroker", None, "LootAll", &[PyRep::Integer(outcome.wreck_item_id)], &HashMap::new(), None)
        .await
        .expect("LootAll by third party with Red safety must succeed");
    assert_eq!(
        loot_res.dict_get("suspectFlagTriggered").and_then(|v| v.as_bool()),
        Some(true),
        "Looting a victim-owned player wreck must trigger Suspect flag"
    );
    let is_suspect = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner()).get_or_create(looter_id).is_suspect(now_unix);
    assert!(is_suspect, "Looter must hold an active Crimewatch Suspect timer");

    // 5. Verify inline handle_ship_or_npc_destruction also sets wreck owner to victim (`P1-3`)
    //    and non-ship balls (wreck/drone) do NOT trigger capsuleer death pipeline (`P0-1`)
    let bal_before = ctx.db.with_snapshot(|s| s.characters.get(&victim_id).unwrap().balance);
    assert!(execute_death_pipeline_sync(&ctx, sys_id, victim_id, outcome.wreck_item_id, attacker_id, DVec3::ZERO, now_unix).is_err());
    let (_, wreck_ball) = sys_handle.get_ball_snapshot(outcome.wreck_item_id).await.unwrap().unwrap();
    let (_, inline_w_id, _, _) = handle_ship_or_npc_destruction(&ctx, &sys_handle, attacker_id, 610_001_002, 587, 2905, outcome.wreck_item_id, &wreck_ball, Some((victim_id, outcome.wreck_item_id)), sys_id, 500.0, now_unix).await;
    assert_eq!(inline_w_id, 0, "Destroying a wreck must not spawn another wreck or pod the owner");
    assert_eq!(ctx.db.with_snapshot(|s| (s.characters.get(&victim_id).unwrap().active_ship_id, s.characters.get(&victim_id).unwrap().balance)), (capsule_id, bal_before));
    let ship2_id = 610_001_099i64;
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&victim_id).unwrap().active_ship_id = ship2_id;
        s.items.insert(ship2_id, new_item(ship2_id, 587, victim_id, sys_id as i64, InventoryFlag::None, 1, true));
    });
    let ship2_ball = new_ball(ship2_id, 587, DVec3::new(1_200.0, 0.0, 0.0));
    let _ = sys_handle.spawn_ball_with_combat(ship2_ball.clone(), ctx.build_type_combat_state(587)).await;
    let (_, inline_ship_wreck_id, _, _) = handle_ship_or_npc_destruction(&ctx, &sys_handle, attacker_id, 610_001_002, 587, 2905, ship2_id, &ship2_ball, Some((victim_id, ship2_id)), sys_id, 1000.0, now_unix).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&inline_ship_wreck_id).unwrap().owner_id), victim_id);
}

#[tokio::test]
async fn test_c3_pod_death_corpse_implants_and_home_station_respawn() {
    // parity: feat:lifecycle.pod-death-pipeline-corpse, feat:lifecycle.death-session-change, plan:C-3
    eve_testkit::attest(&[
        "feat:lifecycle.pod-death-pipeline-corpse",
        "feat:lifecycle.death-session-change",
        "plan:C-3",
    ]);

    let ctx = ServerContext::new();
    let sys_id = 30_000_142u32;
    let home_station_id = 60_003_760i64;
    let (victim_id, attacker_id) = (90_000_001i64, 90_000_002i64);
    let (pod_id, implant_item_id) = (610_010_001i64, 610_010_099i64);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&victim_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = pod_id;
            ch.home_station_id = home_station_id;
        }
        snap.items.insert(pod_id, new_item(pod_id, 670, victim_id, sys_id as i64, InventoryFlag::None, 1, true));
        snap.active_implants.insert(victim_id, vec![9899]);
        snap.items.insert(implant_item_id, new_item(implant_item_id, 10208, victim_id, victim_id, InventoryFlag::Implant, 1, true));
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(new_ball(pod_id, 670, DVec3::new(2_500.0, 500.0, -250.0)), ctx.build_type_combat_state(670))
        .await;

    {
        let mut bo = ctx.bound_objects.write().unwrap_or_else(|e| e.into_inner());
        bo.insert(
            777_001,
            BoundObjectEntry { bound_id: 777_001, session_id: 1, user_id: 1, character_id: victim_id, service: "beyonce".into(), object_id: sys_id as i64, oid_str: "N=888444:777001".into(), ref_id: 133_000_000_000_000_000 },
        );
        bo.insert(
            777_002,
            BoundObjectEntry { bound_id: 777_002, session_id: 1, user_id: 1, character_id: victim_id, service: "scanMgr".into(), object_id: sys_id as i64, oid_str: "N=888444:777002".into(), ref_id: 133_000_000_000_000_000 },
        );
    }

    let now_unix = ctx.now_unix();
    let _ = ctx.db.acquire_online_lock(1, 1, Some(victim_id), true, now_unix);
    let outcome = execute_death_pipeline(&ctx, sys_id, victim_id, pod_id, attacker_id, DVec3::new(2_500.0, 500.0, -250.0), now_unix)
        .await
        .expect("pod death pipeline must succeed");

    assert!(outcome.ejected_capsule_id.is_none() && outcome.wreck_type_id == 25);

    // 1. Corpse (type_id 25) spawned in space and Ballpark; NO 26557 wreck spawned
    let (corpse_rec, any_26557_wreck, implants_cleared) = ctx.db.with_snapshot(|snap| {
        let corpse = snap.items.get(&outcome.wreck_item_id).cloned().expect("corpse item exists");
        let has_wreck = snap.items.values().any(|i| i.item_id == outcome.wreck_item_id && i.type_id == 26557);
        let imp_cleared = !snap.active_implants.contains_key(&victim_id) && !snap.items.contains_key(&implant_item_id);
        (corpse, has_wreck, imp_cleared)
    });
    assert_eq!(corpse_rec.type_id, 25, "Pod death must spawn a Corpse (type_id 25)");
    assert_eq!(corpse_rec.flag, InventoryFlag::None);
    assert_eq!(corpse_rec.location_id, sys_id as i64);
    assert!(!any_26557_wreck, "Pod death must not spawn a 26557 ship wreck");
    assert!(implants_cleared, "Pod death must destroy all plugged-in implants");

    let dkm = with_combat_runtime(&ctx, |rt| rt.detailed_killmails.get(&outcome.killmail_id).cloned())
        .expect("detailed killmail recorded");
    assert!(
        dkm.destroyed_items.iter().any(|i| i.type_id == 9899)
            && dkm.destroyed_items.iter().any(|i| i.type_id == 10208),
        "Pod killmail must list destroyed implants"
    );

    let (_, corpse_ball) = sys_handle.get_ball_snapshot(outcome.wreck_item_id).await.unwrap().expect("corpse ball in ballpark");
    assert_eq!(corpse_ball.type_id, 25);
    let corpse_slim = sys_handle
        .ballpark
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .ball_slim_meta
        .get(&outcome.wreck_item_id)
        .cloned()
        .expect("corpse slim meta");
    assert_eq!((corpse_slim.group_id, corpse_slim.category_id), (14, 2));

    // 2. Victim respawned at home_station_id in a fresh Capsule (670) and space bound objects released
    let (station_after, respawn_ship_id, respawn_ship_type) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&victim_id).expect("victim exists");
        let stype = snap.items.get(&ch.active_ship_id).map(|i| i.type_id).unwrap_or(0);
        (ch.station_id, ch.active_ship_id, stype)
    });
    assert_eq!(station_after, Some(home_station_id));
    assert_eq!(respawn_ship_type, 670);
    {
        let bo = ctx.bound_objects.read().unwrap_or_else(|e| e.into_inner());
        assert!(!bo.contains_key(&777_001) && !bo.contains_key(&777_002));
    }

    let notifs = ctx.drain_notifications_for_char(victim_id);
    assert!(
        notifs.iter().any(|n| n.event == "OnMachoObjectDisconnect" && n.args.len() == 3 && n.args[1] == PyRep::Integer(1_000_000_000_001))
            && notifs.iter().any(|n| {
                let s = format!("{:?}", n.args);
                n.event == "OnSessionChanged" && s.contains("stationid") && s.contains(&home_station_id.to_string()) && s.contains("solarsystemid2") && s.contains(&respawn_ship_id.to_string())
            }),
        "Pod death must emit 3-arg OnMachoObjectDisconnect and OnSessionChanged with stationid/solarsystemid2/shipid"
    );
}

#[tokio::test]
async fn test_c3_wreck_abandon_and_empty_jetcan_despawn() {
    // parity: feat:lifecycle.wreck-abandon, feat:lifecycle.jetcan-empty, plan:C-3
    eve_testkit::attest(&[
        "feat:lifecycle.wreck-abandon",
        "feat:lifecycle.jetcan-empty",
        "plan:C-3",
    ]);

    let ctx = ServerContext::new();
    let sys_id = 30_000_142u32;
    let (owner_id, other_id) = (90_000_001i64, 90_000_002i64);
    let (owner_ship_id, other_ship_id) = (610_020_001i64, 610_020_002i64);
    let (wreck_id, wreck_loot_id) = (610_020_100i64, 610_020_101i64);
    let (jetcan_a_item, jetcan_b_item) = (610_020_201i64, 610_020_202i64);

    ctx.db.with_mut_snapshot(|snap| {
        for &(cid, sid, corp_id) in &[(owner_id, owner_ship_id, 2_000_201i64), (other_id, other_ship_id, 2_000_202i64)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = sid;
                ch.corporation_id = corp_id;
            }
            snap.items.insert(sid, new_item(sid, 587, cid, sys_id as i64, InventoryFlag::None, 1, true));
        }
        snap.items.insert(wreck_id, new_item(wreck_id, 26557, owner_id, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(wreck_loot_id, new_item(wreck_loot_id, 34, owner_id, wreck_id, InventoryFlag::Cargo, 50, false));
        snap.items.insert(jetcan_a_item, new_item(jetcan_a_item, 35, owner_id, owner_ship_id, InventoryFlag::Cargo, 10, false));
        snap.items.insert(jetcan_b_item, new_item(jetcan_b_item, 35, owner_id, owner_ship_id, InventoryFlag::Cargo, 20, false));
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for &(bid, tid) in &[(owner_ship_id, 587u32), (other_ship_id, 587u32), (wreck_id, 26557u32)] {
        let _ = sys_handle.spawn_ball_with_combat(new_ball(bid, tid, DVec3::ZERO), ctx.build_type_combat_state(tid)).await;
    }

    let _ = ctx.db.acquire_online_lock(1, 1, Some(owner_id), true, ctx.now_unix());
    let mut owner_sess = SessionState::default();
    owner_sess.session_id = 1;
    owner_sess.user_id = Some(1);
    owner_sess.character_id = Some(owner_id);
    owner_sess.solar_system_id = Some(sys_id);
    owner_sess.ship_id = Some(owner_ship_id);

    let mut other_sess = SessionState::default();
    other_sess.user_id = Some(2);
    other_sess.character_id = Some(other_id);
    other_sess.solar_system_id = Some(sys_id);
    other_sess.ship_id = Some(other_ship_id);

    // 1. Non-owner cannot abandon wreck
    let non_owner_err = ctx
        .dispatch_rpc(&mut other_sess, "beyonce", None, "CmdAbandonLoot", &[PyRep::List(vec![PyRep::Integer(wreck_id)])], &HashMap::new(), None)
        .await
        .expect_err("non-owner CmdAbandonLoot must fail");
    assert_eq!(non_owner_err.code, "AccessDenied");

    // 2. Owner abandons wreck via beyonce.CmdAbandonLoot (list argument) -> OnSlimItemChange & no Suspect on loot
    ctx.dispatch_rpc(&mut owner_sess, "beyonce", None, "CmdAbandonLoot", &[PyRep::List(vec![PyRep::Integer(wreck_id)])], &HashMap::new(), None)
        .await
        .expect("owner CmdAbandonLoot must succeed");

    let notifs = ctx.drain_notifications_for_char(owner_id);
    let slim_notif = notifs.iter().find(|n| n.event == "OnSlimItemChange").expect("CmdAbandonLoot must emit OnSlimItemChange");
    assert_eq!(slim_notif.args.len(), 2, "OnSlimItemChange must have (oldSlim, newSlim)");
    assert_eq!(slim_notif.args[0].dict_get("ownerID"), Some(&PyRep::Integer(owner_id)));
    assert_eq!(slim_notif.args[1].dict_get("ownerID"), Some(&PyRep::Integer(0)));
    assert_eq!(slim_notif.args[1].dict_get("typeID"), Some(&PyRep::Integer(26557)));

    // Other player loots abandoned wreck via invbroker.Add without Suspect penalty
    let add_res = ctx
        .dispatch_rpc(
            &mut other_sess,
            "invbroker",
            None,
            "Add",
            &[PyRep::Integer(wreck_loot_id), PyRep::Integer(other_ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("looting abandoned wreck via invbroker.Add must succeed");
    assert_eq!(add_res.dict_get("suspectFlagged").and_then(|v| v.as_bool()), Some(false));

    // 3. Jettison two items into a jetcan, partial loot keeps can, emptying last item despawns can immediately
    let jet_res = ctx
        .dispatch_rpc(&mut owner_sess, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(jetcan_a_item), PyRep::Integer(jetcan_b_item)])], &HashMap::new(), None)
        .await
        .expect("ship.Jettison must succeed");
    let can_id = match &jet_res {
        PyRep::Tuple(t) => match t.first() {
            Some(PyRep::List(l)) => l.first().and_then(|v| v.as_i64()).expect("jetcan id in list"),
            _ => panic!("unexpected Jettison tuple: {jet_res:?}"),
        },
        _ => panic!("unexpected Jettison response: {jet_res:?}"),
    };

    assert!(sys_handle.get_ball_snapshot(can_id).await.unwrap().is_some());
    for _ in 0..3 {
        let _ = sys_handle.step_manual(1.0).await;
    }

    // Move first item out -> jetcan still has 1 item, so it must remain
    ctx.dispatch_rpc(&mut owner_sess, "invbroker", None, "Add", &[PyRep::Integer(jetcan_a_item), PyRep::Integer(owner_ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &HashMap::new(), None)
        .await
        .expect("partial loot from jetcan must succeed");
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&can_id)));
    let (expected_tick, _) = sys_handle.get_ball_snapshot(can_id).await.unwrap().unwrap();
    assert!(expected_tick >= 3);
    let _ = ctx.drain_notifications_for_char(owner_id);

    // Move last item out -> jetcan is now empty and must despawn immediately from DB and Ballpark with real tick stamp
    ctx.dispatch_rpc(&mut owner_sess, "invbroker", None, "Add", &[PyRep::Integer(jetcan_b_item), PyRep::Integer(owner_ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &HashMap::new(), None)
        .await
        .expect("looting final item from jetcan must succeed");

    assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&can_id)));
    assert!(sys_handle.get_ball_snapshot(can_id).await.unwrap().is_none());
    let jet_notifs = ctx.drain_notifications_for_char(owner_id);
    assert!(jet_notifs.iter().any(|n| n.event == "DoDestinyUpdate" && format!("{:?}", n.args).contains("RemoveBall") && format!("{:?}", n.args).contains(&format!("Integer({expected_tick})"))));
}

#[tokio::test]
async fn test_c3_courier_package_destroyed_and_missile_death_pipeline() {
    // parity: feat:lifecycle.courier-package-destroyed, plan:C-3
    eve_testkit::attest(&[
        "feat:lifecycle.courier-package-destroyed",
        "plan:C-3",
    ]);

    let ctx = ServerContext::new();
    let sys_id = 30_000_142u32;
    let start_station_id = 60_003_760i64;
    let (issuer_id, hauler_id, attacker_id) = (90_000_001i64, 90_000_002i64, 90_000_003i64);
    let (hauler_ship_id, wrap_item_id, wrapped_cargo_id) = (610_030_001i64, 610_030_100i64, 610_030_101i64);
    let (contract_id, zero_collat_contract_id, zero_collat_item_id) = (710_001i64, 710_002i64, 610_030_200i64);
    let (jet_can_id, jet_wrap_id, jet_cargo_id, jet_contract_id) = (610_030_300i64, 610_030_301i64, 610_030_302i64, 710_003i64);

    let issuer_initial_bal = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&hauler_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = hauler_ship_id;
        }
        let ibal = snap.characters.get(&issuer_id).map(|c| c.balance).unwrap_or(IskCents(0));
        snap.items.insert(hauler_ship_id, new_item(hauler_ship_id, 587, hauler_id, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(wrap_item_id, new_item(wrap_item_id, 3468, hauler_id, hauler_ship_id, InventoryFlag::Cargo, 1, true));
        snap.items.insert(wrapped_cargo_id, new_item(wrapped_cargo_id, 34, issuer_id, wrap_item_id, InventoryFlag::Locked, 500, false));
        snap.contracts.insert(contract_id, ContractRecord {
            contract_id, issuer_id, acceptor_id: Some(hauler_id), contract_type: "Courier".to_string(), status: "InProgress".to_string(),
            price: IskCents(0), reward: IskCents::from_f64_isk(100_000.0), collateral: IskCents::from_f64_isk(5_000_000.0),
            start_station_id, end_station_id: 60_004_588, item_ids: vec![wrapped_cargo_id], expiration_unix: 1_700_086_400, num_days: 1,
            ..Default::default()
        });
        snap.items.insert(jet_can_id, new_item(jet_can_id, 23, hauler_id, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(jet_wrap_id, new_item(jet_wrap_id, 3468, hauler_id, jet_can_id, InventoryFlag::Cargo, 1, true));
        snap.items.insert(jet_cargo_id, new_item(jet_cargo_id, 34, issuer_id, jet_wrap_id, InventoryFlag::Locked, 100, false));
        snap.contracts.insert(jet_contract_id, ContractRecord {
            contract_id: jet_contract_id, issuer_id, acceptor_id: Some(hauler_id), contract_type: "Courier".to_string(), status: "InProgress".to_string(),
            price: IskCents(0), reward: IskCents::from_f64_isk(50_000.0), collateral: IskCents::from_f64_isk(1_000_000.0),
            start_station_id, end_station_id: 60_004_588, item_ids: vec![jet_cargo_id], expiration_unix: 1_700_086_400, num_days: 1,
            ..Default::default()
        });
        snap.items.insert(zero_collat_item_id, new_item(zero_collat_item_id, 35, issuer_id, 60_008_494, InventoryFlag::Locked, 250, false));
        snap.contracts.insert(zero_collat_contract_id, ContractRecord {
            contract_id: zero_collat_contract_id, issuer_id, acceptor_id: Some(hauler_id), contract_type: "Courier".to_string(), status: "InProgress".to_string(),
            price: IskCents(0), reward: IskCents::from_f64_isk(50_000.0), collateral: IskCents(0),
            start_station_id, end_station_id: 60_004_588, item_ids: vec![zero_collat_item_id], expiration_unix: 1_700_086_400, num_days: 1,
            ..Default::default()
        });
        ibal
    });

    // 1. Test missile impact destroying hauler's ship, a jettisoned courier container, a drone, and an NPC
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut low_hp_cs = ctx.build_type_combat_state(587);
    low_hp_cs.shield_hp = 0.0;
    low_hp_cs.armor_hp = 0.0;
    low_hp_cs.structure_hp = 10.0;
    let _ = sys_handle.spawn_ball_with_combat(new_ball(hauler_ship_id, 587, DVec3::new(500.0, 0.0, 0.0)), low_hp_cs.clone()).await;
    let _ = sys_handle.spawn_ball_with_combat(new_ball(jet_can_id, 23, DVec3::new(700.0, 0.0, 0.0)), low_hp_cs.clone()).await;
    let npc_target_id = spawn_specialized_npc_entity(&ctx, sys_id, 33132, "CloneSoldier", 500_010, DVec3::new(600.0, 0.0, 0.0)).await;
    let _ = sys_handle.set_combat_state(npc_target_id, low_hp_cs.clone()).await;

    let (missile_id, drone_id, drone_missile_id, can_missile_id, npc_missile_id) = (699_001_001i64, 610_030_900i64, 699_001_002i64, 699_001_003i64, 699_001_004i64);
    ctx.db.with_mut_snapshot(|s| { s.items.insert(drone_id, new_item(drone_id, 2454, issuer_id, sys_id as i64, InventoryFlag::None, 1, true)); });
    let _ = sys_handle.spawn_ball_with_combat(new_ball(drone_id, 2454, DVec3::new(900.0, 0.0, 0.0)), low_hp_cs).await;
    for (mid, pos) in [(missile_id, 400.0), (drone_missile_id, 850.0), (can_missile_id, 650.0), (npc_missile_id, 550.0)] {
        let _ = sys_handle.spawn_ball_with_combat(new_ball(mid, 204, DVec3::new(pos, 0.0, 0.0)), CombatState::default()).await;
    }
    with_mut_combat_runtime(&ctx, |rt| {
        for (mid, tid) in [(missile_id, hauler_ship_id), (drone_missile_id, drone_id), (can_missile_id, jet_can_id), (npc_missile_id, npc_target_id)] {
            rt.in_flight_missiles.insert(mid, InFlightMissileState {
                missile_ball_id: mid, solar_system_id: sys_id, attacker_char_id: attacker_id, attacker_ship_id: 610_030_003,
                launcher_module_id: 610_030_004, launcher_type_id: 2410, missile_type_id: 204, target_ball_id: tid,
                max_velocity: 1_000.0, flight_time_s: 5.0, remaining_flight_s: 5.0, hp: 70.0, max_hp: 70.0,
                base_damage: DamageVector::new(100.0, 100.0, 100.0, 100.0), explosion_radius: 40.0, explosion_velocity: 150.0,
                drf: 0.6, is_fof: false, is_breacher_pod: false, breacher_dot_duration_s: 0.0, breacher_flat_cap: 0.0,
                breacher_pct_cap: 0.0, destroyed: false, expired: false, impacted: false, applied_damage: 0.0,
            });
        }
    });

    let issuer_ship_before = ctx.db.with_snapshot(|s| s.characters.get(&issuer_id).unwrap().active_ship_id);
    step_combat_system(&ctx, sys_id, 1.0).await;
    assert!(sys_handle.get_ball_snapshot(drone_id).await.unwrap().is_none(), "Destroyed drone ball must be removed");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&issuer_id).unwrap().active_ship_id), issuer_ship_before, "Destroying a player's drone must not pod the player");
    let sde_33132_bounty = ctx.sde.types.get(&33132).and_then(|t| t.default_attributes.get(&481).copied()).unwrap();
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.pending_bounty_ticks.get(&(sys_id, attacker_id)).copied()), Some(sde_33132_bounty), "Missile kill of NPC must queue exact 1x SDE bounty");
    let turret_npc_id = spawn_specialized_npc_entity(&ctx, sys_id, 33132, "CloneSoldier", 500_010, DVec3::new(620.0, 0.0, 0.0)).await;
    let (_, _, turret_npc_ball) = sys_handle.apply_damage_with_attacker(turret_npc_id, DamageVector::new(50_000.0, 50_000.0, 50_000.0, 50_000.0), Some(attacker_id)).await.unwrap().unwrap();
    let _ = handle_ship_or_npc_destruction(&ctx, &sys_handle, attacker_id, 610_030_003, 587, 2905, turret_npc_id, &turret_npc_ball, None, sys_id, 50_000.0, ctx.now_unix()).await;
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.pending_bounty_ticks.get(&(sys_id, attacker_id)).copied()), Some(sde_33132_bounty * 2.0), "Turret kill of NPC must queue exact 1x SDE bounty without doubling");

    // Verify Contract 1 & jettisoned Contract 3 failed, collateral paid to issuer, and no orphaned items remain
    ctx.db.with_mut_snapshot(|snap| {
        assert_eq!(snap.contracts.get(&contract_id).unwrap().status, "Failed", "Destroyed courier package must fail the contract");
        assert_eq!(snap.contracts.get(&jet_contract_id).unwrap().status, "Failed", "Destroyed jettisoned courier package must fail the contract");
        assert!(!snap.items.contains_key(&jet_wrap_id) && !snap.items.contains_key(&jet_cargo_id), "Destroyed jetcan must remove nested wrap and inner courier cargo without orphans");
        let issuer_after = snap.characters.get(&issuer_id).expect("issuer exists").balance;
        assert_eq!(
            issuer_after,
            issuer_initial_bal.checked_add(IskCents::from_f64_isk(6_150_000.0)).unwrap(),
            "Issuer must receive collateral payout + escrowed reward refund for both destroyed courier packages"
        );
        assert!(!snap.items.contains_key(&wrap_item_id), "Destroyed Plastic Wrap container must be removed");
        let hauler_wreck_id = snap.items.values().find(|i| i.owner_id == hauler_id && i.location_id == sys_id as i64 && i.type_id != 670).map(|i| i.item_id).expect("hauler wreck exists");
        let cargo_valid = match snap.items.get(&wrapped_cargo_id) {
            Some(cargo) => cargo.location_id == hauler_wreck_id && cargo.location_id != start_station_id,
            None => true,
        };
        assert!(cargo_valid, "Wrapped cargo must be either dropped in wreck or destroyed, never at start_station_id");

        // 2. Verify zero-collateral courier deadline expiry unwraps in-place for acceptor without teleporting to start_station_id
        snap.resolve_expired_contract(zero_collat_contract_id, 1_700_200_000)
            .expect("resolve_expired_contract must succeed");
        let c2 = snap.contracts.get(&zero_collat_contract_id).expect("contract 2 exists");
        assert_eq!(c2.status, "Failed");
        let item2 = snap.items.get(&zero_collat_item_id).expect("zero-collat item exists");
        assert_eq!(item2.owner_id, hauler_id, "Unwrapped courier item transfers to acceptor");
        assert_eq!(item2.flag, InventoryFlag::Hangar);
        assert_eq!(
            item2.location_id, 60_008_494,
            "Zero-collateral failed courier item must stay in-place, never teleport to start_station_id"
        );
    });
}

#[tokio::test]
async fn test_c4_npc_target_switching_and_clone_soldier_lowsec_spawns() {
    // parity: feat:npc.npc-target-switching, feat:npc.clone-soldier-spawns, plan:C-4
    eve_testkit::attest(&[
        "feat:npc.npc-target-switching",
        "feat:npc.clone-soldier-spawns",
        "plan:C-4",
    ]);

    let ctx = ServerContext::new();

    // 1. Verify Clone Soldier security band gating (0.1..=0.4 low-sec only; never in high-sec or null-sec)
    for (sec, exp) in [(0.9, None), (0.5, None), (0.4, Some(33132)), (0.3, Some(33142)), (0.2, Some(33163)), (0.1, Some(33175)), (0.0, None), (-0.5, None)] {
        assert_eq!(clone_soldier_type_for_security(sec), exp);
    }

    let lowsec_sys_id = ctx.sde.solar_systems.values().find(|s| (0.10..0.45).contains(&(s.security_status as f64))).map(|s| s.solar_system_id).unwrap_or(30_002_537);
    let expected_faction = regional_pirate_faction_id(&ctx, lowsec_sys_id);
    assert_eq!(regional_pirate_faction_id(&ctx, 30_000_001), 500_019, "Derelik system must resolve to Sansha's Nation via SDE faction_id");
    assert!(clone_soldier_type_for_security_and_faction(&ctx.sde, 0.3, expected_faction).is_some());

    // Drive belt, gate, and Clone Soldier spawning through the real SP-7 production scheduler (`tick_background_cluster`)
    let lowsec_handle = ctx.systems.get_or_spawn(lowsec_sys_id);
    eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
    assert!(ctx.npc_spawns.read().unwrap().get(&lowsec_sys_id).is_some_and(|m| !m.entities.is_empty()), "SP-7 tick must populate ctx.npc_spawns belt/gate waves");

    let (cs_ball_id, cs_npc) = with_combat_runtime(&ctx, |rt| {
        rt.npc_entities.iter().find(|(_, n)| n.solar_system_id == lowsec_sys_id && n.archetype == "CloneSoldier").map(|(&id, n)| (id, n.clone()))
    }).expect("SP-7 tick must spawn a Clone Soldier in low-sec");
    assert_eq!((cs_npc.archetype.as_str(), cs_npc.faction_id), ("CloneSoldier", expected_faction));
    let tag_tid = cs_npc.tag_drop_type_id.expect("Clone Soldier must have a security tag drop");
    assert!(matches!(tag_tid, 33138..=33141) && ctx.sde.types.get(&tag_tid).map(|t| t.group_id) == Some(1206));
    assert!(matches!(ctx.sde.types.get(&cs_npc.type_id).map(|t| t.group_id), Some(551 | 552 | 556 | 566 | 574)));

    let cs_combat = lowsec_handle.get_combat_state(cs_ball_id).await.unwrap().expect("Clone Soldier CombatState present");
    let sde_combat = ctx.build_type_combat_state(cs_npc.type_id);
    assert_eq!((cs_combat.shield_resonance, cs_combat.armor_resonance), (sde_combat.shield_resonance, sde_combat.armor_resonance));

    let killer_id = 90_000_001i64;
    let (cs_wreck_id, bounty, dropped_types) = destroy_specialized_npc_and_queue_bounty(&ctx, lowsec_sys_id, cs_ball_id, killer_id).await;
    assert_eq!(bounty, ctx.sde.types.get(&cs_npc.type_id).and_then(|t| t.default_attributes.get(&481).copied()).unwrap_or(0.0));
    assert!(dropped_types.contains(&tag_tid) && ctx.db.with_snapshot(|snap| snap.items.values().any(|i| i.location_id == cs_wreck_id && i.type_id == tag_tid)));
    eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
    assert!(with_combat_runtime(&ctx, |rt| !rt.npc_entities.values().any(|n| n.solar_system_id == lowsec_sys_id && n.archetype == "CloneSoldier")), "Clone Soldier must not respawn during 1200s cooldown");
    ctx.npc_spawns.write().unwrap().get_mut(&lowsec_sys_id).unwrap().clone_soldier_respawn_timers.insert(lowsec_sys_id, 0);
    with_mut_combat_runtime(&ctx, |rt| { rt.clone_soldier_cooldown_until.insert(lowsec_sys_id, 0); });
    eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
    assert!(with_combat_runtime(&ctx, |rt| rt.npc_entities.values().any(|n| n.solar_system_id == lowsec_sys_id && n.archetype == "CloneSoldier")), "Clone Soldier must respawn via SP-7 tick after cooldown expires");

    // 2. Test NpcSpawnManager::step_ai (Idle -> Chase -> Orbit/Attack), SP-7 timer scheduling, and target switching
    let sys_id = 30_000_144u32;
    let (ship_a, ship_b, drone_a, cap_c) = (610_040_001i64, 610_040_002i64, 610_040_003i64, 610_040_004i64);
    let (char_a, char_b, char_c) = (90_000_001i64, 90_000_002i64, 90_000_003i64);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut c3) = snap.characters.get(&char_a).cloned() { c3.character_id = char_c; snap.characters.insert(char_c, c3); }
        for &(cid, sid, tid) in &[(char_a, ship_a, 587u32), (char_b, ship_b, 587u32), (char_c, cap_c, 19720u32)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = sys_id; ch.station_id = None; ch.active_ship_id = sid; ch.standings.insert(500_010, -5.0);
            }
            snap.items.insert(sid, new_item(sid, tid, cid, sys_id as i64, InventoryFlag::None, 1, true));
        }
        snap.items.insert(drone_a, new_item(drone_a, 2454, char_a, sys_id as i64, InventoryFlag::DroneBay, 1, true));
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for &(bid, tid) in &[(ship_a, 587u32), (ship_b, 587u32), (drone_a, 2454u32)] {
        let _ = sys_handle.spawn_ball_with_combat(new_ball(bid, tid, DVec3::new(5_000.0, 0.0, 0.0)), ctx.build_type_combat_state(tid)).await;
    }
    eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
    let first_belt_rat = *ctx.npc_spawns.read().unwrap()[&sys_id].entities.keys().next().unwrap();
    let _ = ctx.npc_spawns.write().unwrap().get_mut(&sys_id).unwrap().handle_npc_destroyed(&ctx, sys_id, first_belt_rat, char_a, ctx.now_unix()).unwrap();
    assert!(!ctx.npc_spawns.read().unwrap()[&sys_id].respawn_timers.is_empty());

    let transporter_id = spawn_specialized_npc_entity(&ctx, sys_id, 33163, "CloneSoldier", 500_010, DVec3::new(900_000.0, 0.0, 0.0)).await;
    let npc_id = spawn_specialized_npc_entity(&ctx, sys_id, 33175, "CloneSoldier", 500_010, DVec3::ZERO).await;
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.npc_entities.get(&transporter_id).unwrap().web_range, 10_000.0, "Transporter webs at 10 km");
        assert_eq!(rt.npc_entities.get(&npc_id).unwrap().scram_range, 24_000.0, "Negotiator scrams at 24 km");
    });

    record_npc_threat_event(&ctx, npc_id, ship_a, 200.0, false, false);
    step_extended_npc_ai_sync(&ctx, sys_id);
    assert_eq!(
        with_combat_runtime(&ctx, |rt| rt.npc_entities.get(&npc_id).and_then(|n| n.target_id)),
        Some(ship_a),
        "NPC must initially target Ship A"
    );

    record_npc_threat_event(&ctx, npc_id, ship_b, 300.0, true, false);
    step_extended_npc_ai_sync(&ctx, sys_id);
    assert_eq!(
        with_combat_runtime(&ctx, |rt| rt.npc_entities.get(&npc_id).and_then(|n| n.target_id)),
        Some(ship_b),
        "NPC must switch target to higher-threat EWAR+DPS Ship B"
    );

    {
        let mut bp = sys_handle.ballpark.write().unwrap_or_else(|e| e.into_inner());
        if let Some(cs) = bp.combat_states.get_mut(&ship_b) {
            cs.is_cloaked = true;
        }
    }
    step_extended_npc_ai_sync(&ctx, sys_id);
    assert_eq!(
        with_combat_runtime(&ctx, |rt| rt.npc_entities.get(&npc_id).and_then(|n| n.target_id)),
        Some(ship_a),
        "NPC must drop cloaked Ship B (loss of lock) and switch back to Ship A"
    );

    {
        let mut bp = sys_handle.ballpark.write().unwrap_or_else(|e| e.into_inner());
        if let Some(b) = bp.balls.get_mut(&ship_a) {
            b.mode = MovementMode::new_warp(DVec3::ZERO, DVec3::new(1_000_000_000.0, 0.0, 0.0), 3.0);
        }
    }
    step_extended_npc_ai_sync(&ctx, sys_id);
    assert_eq!(
        with_combat_runtime(&ctx, |rt| rt.npc_entities.get(&npc_id).and_then(|n| n.target_id)),
        Some(drone_a),
        "NPC must drop warping Ship A and switch to remaining hostile player drone on grid"
    );

    // Target destruction & new capital arrival on grid -> switches from destroyed targets to newly arrived cap_c
    for bid in [ship_a, drone_a] { let _ = sys_handle.remove_ball(bid).await; }
    let _ = sys_handle.spawn_ball_with_combat(new_ball(cap_c, 19720, DVec3::new(6_000.0, 0.0, 0.0)), ctx.build_type_combat_state(19720)).await;
    step_extended_npc_ai_sync(&ctx, sys_id);
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.npc_entities.get(&npc_id).and_then(|n| n.target_id)), Some(cap_c));
}

pub(crate) mod fighter_tube_duplicate_load_launch {
    // Round-4 Track 1 Regression Tests (#153 through #163):
    // - #153: Fighter tube duplicate load prevention, launch ownership/location/flag check, recall validation
    // - #154: Self-destruct docked rejection, mandatory 120s countdown, PvP killmail/bounty attribution
    // - #155: Smartbomb/bomb SafetyLevel enforcement, invulnerability skip, lowsec flags
    // - #156: Breacher pod DoT lethal tick triggers death pipeline and wreck creation
    // - #157: Security tag turn-in requires local station hangar / active ship cargo and not contract-locked
    // - #158: Wormhole bidirectional polarization and K162 reverse jump
    // - #159: Probe recovery preserves mixed probe types and respects ship cargo capacity
    // - #160: Hacking minigame minimum difficulty floor, utility slot cap, and targeted SecondaryVector
    // - #161: CmdAlignToBookmark expiry, folder ACL, criminal warp restriction, and decloak
    // - #162: Directional scan (ConeScan) docked rejection and convoy range/cone filtering
    // - #163: Salvage ship cargo capacity enforcement and 6,000 m Salvager module range check

    use eve_db::{
        AccountRecord, BookmarkFolderRecord, BookmarkRecord, CharacterRecord, ContractRecord,
        InventoryFlag, InventoryItemRecord, IskCents, NeuralAttributes, TrainedSkillRecord,
    };
    use eve_destiny::{Ball, CombatState, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::combat::drones_salvage_tractor::execute_wreck_salvage_cycle;
    use eve_server::router::ServerContext;
    use eve_server::services::combat_crimewatch::{
        record_killmail_assist_action, step_combat_system, with_combat_runtime,
        with_mut_combat_runtime, ActiveBreacherDot, SafetyLevel,
    };
    use glam::DVec3;
    use std::collections::HashMap;

    fn item_rec(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
        InventoryItemRecord {
            item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None, insured_payout_isk: 0.0,
            is_blueprint_copy: false, blueprint_runs_remaining: None,
        }
    }

    async fn setup_pilot_in_space(
        ctx: &ServerContext, client_id: u64, char_id: i64, corp_id: i64, ship_id: i64, ship_type_id: u32, sys_id: u32, pos: DVec3,
    ) -> SessionState {
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.insert(char_id, CharacterRecord {
                character_id: char_id, user_id: client_id, name: format!("Pilot {char_id}"), corporation_id: corp_id,
                alliance_id: None, war_faction_id: None, station_id: None, structure_id: None, solar_system_id: sys_id,
                constellation_id: 20_000_020, region_id: 10_000_002, active_ship_id: ship_id, balance: IskCents::from_isk(100_000_000),
                plex_balance: 0, loyalty_points: HashMap::new(), standings: HashMap::new(), security_status: 0.0,
                clone_grade: 2, home_station_id: 60_003_760, race_id: 1, bloodline_id: 1, ancestry_id: 1, school_id: 1,
                neural: NeuralAttributes::default(), skill_points: 5_000_000.0, activity_status: None, free_skill_points: 0.0,
                last_clone_jump_unix: 0, jump_fatigue_minutes: 0.0, jump_cooldown_until_unix: 0, jump_fatigue_until_unix: 0,
                description: String::new(), title: String::new(), gender: 1, create_date_unix: 1_700_000_000,
                fleet_id: None, autopilot_waypoints: Vec::new(), version: 1, system_epoch: 0,
            });
            if !snap.accounts.values().any(|a| a.user_id == client_id) {
                snap.accounts.insert(format!("user_{client_id}"), AccountRecord {
                    user_id: client_id, username: format!("user_{client_id}"), password_token: String::new(),
                    role: 0, banned_until_unix: None, ban_reason: None,
                });
            }
            snap.items.insert(ship_id, item_rec(ship_id, ship_type_id, char_id, sys_id as i64, InventoryFlag::None, 1, true));
        });
        let _ = ctx.db.acquire_online_lock(client_id, 7_000 + client_id, Some(char_id), true, ctx.now_unix());
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle.spawn_ball_with_combat(
            Ball { id: ship_id, type_id: ship_type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 2.5, mass: 10_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
            ctx.build_type_combat_state(ship_type_id),
        ).await;
        let mut sess = SessionState::new(7_000 + client_id);
        sess.user_id = Some(client_id);
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(corp_id);
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(sys_id);
        sess
    }

    #[tokio::test]
    async fn test_r4_153_fighter_tube_duplicate_load_and_launch_ownership_validation() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let sys_id = 30_000_142u32;
        let (char_id, other_char_id, ship_id) = (97_153_001i64, 97_153_002i64, 87_153_100i64);
        // Archon carrier (23757) with FighterBay
        let mut sess =
            setup_pilot_in_space(&ctx, 71531, char_id, 1_000_125, ship_id, 23757, sys_id, DVec3::ZERO)
                .await;

        let fighter_item_id = 87_153_201i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                fighter_item_id,
                item_rec(
                    fighter_item_id,
                    23055, // Templar I light fighter
                    char_id,
                    ship_id,
                    InventoryFlag::FighterBay,
                    9,
                    false,
                ),
            );
        });

        // Loading into tube 0 succeeds
        ctx.dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LoadFightersToTube",
            &[PyRep::Integer(0), PyRep::Integer(fighter_item_id)],
            &kw,
            None,
        )
        .await
        .expect("Load fighter into tube 0");

        // Loading the same fighter item into sibling tube 1 must be rejected with FighterAlreadyLoaded
        let dup_err = ctx
            .dispatch_rpc(
                &mut sess,
                "fighterMgr",
                None,
                "LoadFightersToTube",
                &[PyRep::Integer(1), PyRep::Integer(fighter_item_id)],
                &kw,
                None,
            )
            .await
            .expect_err("Same fighter stack must not load into two tubes");
        assert_eq!(dup_err.code, "FighterAlreadyLoaded");

        // If the fighter item is moved out of FighterBay or transferred to another owner before launch, LaunchFightersFromTubes must reject
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&fighter_item_id).unwrap().owner_id = other_char_id;
        });
        let launch_err = ctx
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
            .expect_err("Launching fighter no longer owned by pilot in FighterBay must fail");
        assert_eq!(launch_err.code, "ItemNotInFighterBay");
    }

    #[tokio::test]
    async fn test_r4_154_self_destruct_docked_rejection_countdown_and_pvp_assist_attribution() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let sys_id = 30_002_813u32; // Tama
        let (vic_id, atk_id, vic_ship_id, atk_ship_id) =
            (97_154_001i64, 97_154_002i64, 87_154_101i64, 87_154_102i64);
        let mut vic_sess =
            setup_pilot_in_space(&ctx, 71541, vic_id, 1_000_125, vic_ship_id, 587, sys_id, DVec3::ZERO)
                .await;
        let _atk_sess = setup_pilot_in_space(
            &ctx,
            71542,
            atk_id,
            1_000_126,
            atk_ship_id,
            587,
            sys_id,
            DVec3::new(2_000.0, 0.0, 0.0),
        )
        .await;

        // 1. Docked session cannot self-destruct
        let mut docked_sess = vic_sess.clone();
        docked_sess.station_id = Some(60_003_760);
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&vic_id).unwrap().station_id = Some(60_003_760);
        });
        let docked_err = ctx
            .dispatch_rpc(&mut docked_sess, "ship", None, "SelfDestruct", &[], &kw, None)
            .await
            .expect_err("Docked ship must not self-destruct");
        assert_eq!(docked_err.code, "CannotSelfDestructWhileDocked");
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&vic_id).unwrap().station_id = None;
        });

        // 2. Empty args SelfDestruct starts the 120s countdown instead of bypassing it
        let start_rep = ctx
            .dispatch_rpc(&mut vic_sess, "ship", None, "SelfDestruct", &[], &kw, None)
            .await
            .expect("Start countdown with empty args");
        assert_eq!(
            start_rep.dict_get("countdownStarted").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            start_rep.dict_get("detonated").and_then(|v| v.as_bool()),
            Some(false)
        );

        // 3. Record PvP aggression from atk_id and advance countdown to detonation
        record_killmail_assist_action(
            &ctx,
            vic_ship_id,
            atk_id,
            587,
            486,
            250.0,
            false,
            ctx.now_unix(),
        );
        with_mut_combat_runtime(&ctx, |rt| {
            if let Some(cd) = rt.self_destruct_countdowns.get_mut(&vic_ship_id) {
                cd.detonate_at_unix = ctx.now_unix();
            }
        });
        step_combat_system(&ctx, sys_id, 1.0).await;

        let km_attacker = ctx.db.with_snapshot(|snap| {
            snap.killmails
                .values()
                .find(|km| km.victim_character_id == vic_id)
                .map(|km| km.attacker_character_id)
        });
        assert_eq!(
            km_attacker,
            Some(atk_id),
            "Self-destruct within 15m assist window must attribute killmail to PvP attacker"
        );
        let dkm_has_attacker = with_combat_runtime(&ctx, |rt| {
            rt.detailed_killmails
                .values()
                .any(|dkm| dkm.victim_character_id == vic_id && dkm.attackers.iter().any(|a| a.character_id == atk_id))
        });
        assert!(dkm_has_attacker, "Detailed killmail must preserve PvP attacker");
    }

    #[tokio::test]
    async fn test_r4_155_156_smartbomb_safety_invulnerability_and_breacher_dot_lethal_death() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let jita_sys = 30_000_142u32;
        let (atk_id, vic_id, atk_ship_id, vic_ship_id, sb_mod_id) = (
            97_155_001i64,
            97_155_002i64,
            87_155_101i64,
            87_155_102i64,
            87_155_201i64,
        );
        let mut atk_sess =
            setup_pilot_in_space(&ctx, 71551, atk_id, 1_000_125, atk_ship_id, 645, jita_sys, DVec3::ZERO)
                .await;
        let _vic_sess = setup_pilot_in_space(
            &ctx,
            71552,
            vic_id,
            1_000_126,
            vic_ship_id,
            587,
            jita_sys,
            DVec3::new(1_000.0, 0.0, 0.0),
        )
        .await;

        let sb_type_id = ctx
            .sde
            .types
            .values()
            .find(|t| t.group_id == 72)
            .map(|t| t.type_id)
            .expect("Smartbomb type in SDE");
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                sb_mod_id,
                item_rec(
                    sb_mod_id,
                    sb_type_id,
                    atk_id,
                    atk_ship_id,
                    InventoryFlag::HiSlot0,
                    1,
                    true,
                ),
            );
            snap.module_online_states.insert(sb_mod_id, true);
        });
        ctx.invalidate_ship_fitting(atk_ship_id);

        // 1. With SafetyLevel::Green in Highsec, smartbomb activation is blocked before capacitor drain
        with_mut_combat_runtime(&ctx, |rt| {
            rt.safety_levels.insert(atk_id, SafetyLevel::Green);
        });
        let sb_err = eve_server::services::combat_crimewatch::dispatch_combat_rpc(
            &ctx,
            &mut atk_sess,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(sb_mod_id)],
            &kw,
        )
        .await
        .expect_err("Smartbomb in highsec with Green safety must be blocked");
        assert_eq!(sb_err.code, "CantDoInCurrentSafetyLevel");

        // 2. Lethal Breacher Pod DoT tick triggers death pipeline and removes destroyed ship ball
        let sys_handle = ctx.systems.get_or_spawn(jita_sys);
        let _ = sys_handle
            .set_combat_state(
                vic_ship_id,
                CombatState {
                    shield_hp: 0.0,
                    max_shield_hp: 100.0,
                    armor_hp: 0.0,
                    max_armor_hp: 100.0,
                    structure_hp: 10.0,
                    max_structure_hp: 100.0,
                    ..CombatState::default()
                },
            )
            .await;
        with_mut_combat_runtime(&ctx, |rt| {
            rt.active_breacher_dots.insert(
                vic_ship_id,
                ActiveBreacherDot {
                    target_ball_id: vic_ship_id,
                    source_char_id: atk_id,
                    source_ship_id: atk_ship_id,
                    weapon_type_id: 83482,
                    charge_type_id: 83485,
                    flat_cap_dps: 500.0,
                    pct_cap_fraction: 0.5,
                    remaining_s: 10.0,
                    tick_damage: 50.0,
                },
            );
        });
        step_combat_system(&ctx, jita_sys, 1.0).await;
        assert!(
            sys_handle
                .get_ball_snapshot(vic_ship_id)
                .await
                .unwrap()
                .is_none(),
            "Lethal breacher DoT must destroy target ship ball via death pipeline"
        );
    }

    #[tokio::test]
    async fn test_r4_157_sec_tag_turn_in_requires_local_hangar_and_not_contract_locked() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let char_id = 97_157_001i64;
        let ship_id = 87_157_101i64;
        let sec_office_svc = ctx.sde.station_services.values().find(|s| s.name == "Security Office").map(|s| s.service_id).unwrap_or(26);
        let (_lowsec_stn, lowsec_sys) = ctx.sde.stations.values().find(|st| {
            (matches!(st.owner_corporation_id, 1_000_125 | 1_000_137) || st.owner_faction_id == Some(500_006))
                && ctx.sde.solar_systems.get(&st.solar_system_id).is_some_and(|s| s.security_status > 0.0 && s.security_status < 0.45)
                && ctx.sde.station_operations.get(&st.operation_id).is_some_and(|op| op.services.contains(&sec_office_svc))
        }).map(|st| (st.station_id, st.solar_system_id)).expect("lowsec CONCORD Security Office station");
        let mut sess = setup_pilot_in_space(
            &ctx,
            71571,
            char_id,
            1_000_125,
            ship_id,
            587,
            lowsec_sys,
            DVec3::ZERO,
        )
        .await;
        let sec_office_st = ctx
            .sde
            .stations
            .values()
            .find_map(|st| {
                let sec = ctx.sde.solar_systems.get(&st.solar_system_id)?.security_status;
                let has_office = ctx
                    .sde
                    .station_operations
                    .get(&st.operation_id)
                    .is_some_and(|op| {
                        op.services.iter().any(|sid| {
                            ctx.sde.station_services.get(sid).is_some_and(|svc| {
                                svc.name.eq_ignore_ascii_case("Security Office")
                                    || svc.name.eq_ignore_ascii_case("securityOffice")
                            })
                        })
                    });
                (0.0 < sec && sec < 0.45 && has_office).then_some(st.station_id)
            })
            .unwrap_or(60_014_861);
        sess.station_id = Some(sec_office_st);

        let remote_tag_id = 87_157_201i64;
        let locked_tag_id = 87_157_202i64;
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.station_id = Some(sec_office_st);
            ch.security_status = -8.5;
            // Tag in remote station 60_008_494
            snap.items.insert(
                remote_tag_id,
                item_rec(
                    remote_tag_id,
                    33138,
                    char_id,
                    60_008_494,
                    InventoryFlag::Hangar,
                    1,
                    false,
                ),
            );
            // Tag in local station but locked in an Outstanding contract
            snap.items.insert(
                locked_tag_id,
                item_rec(
                    locked_tag_id,
                    33138,
                    char_id,
                    sec_office_st,
                    InventoryFlag::Hangar,
                    1,
                    false,
                ),
            );
            snap.contracts.insert(77_157_001, ContractRecord {
                contract_id: 77_157_001, issuer_id: char_id, issuer_corp_id: 1_000_125, assignee_id: 0, acceptor_id: None,
                contract_type: "ItemExchange".into(), status: "Outstanding".into(), title: "Locked Tag".into(), for_corp: false,
                availability: "Public".into(), start_station_id: 60_003_760, end_station_id: 60_003_760,
                price: IskCents::from_isk(1), reward: IskCents(0), collateral: IskCents(0), item_ids: vec![locked_tag_id],
                expiration_unix: 1_700_086_400, buyout: IskCents(0), current_bid: IskCents(0), current_bidder_id: None,
                requested_items: Vec::new(), bid_history: Vec::new(), date_issued_unix: 1_700_000_000, num_days: 1,
            });
        });

        let err = ctx
            .dispatch_rpc(
                &mut sess,
                "crimewatch",
                None,
                "ApplyTagsForSecStatus",
                &[PyRep::Real(-8.0), PyRep::Dict(vec![(PyRep::Integer(33138), PyRep::Integer(1))])],
                &kw,
                None,
            )
            .await
            .expect_err("Remote and contract-locked tags must not be consumed");
        assert_eq!(err.code, "InsufficientSecurityTags");
    }

    #[tokio::test]
    async fn test_r4_158_159_160_161_162_163_exploration_wormhole_dscan_and_salvage() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let sys_id = 30_000_142u32;
        let (char_id, ship_id) = (97_158_001i64, 87_158_101i64);
        let mut sess =
            setup_pilot_in_space(&ctx, 71581, char_id, 1_000_125, ship_id, 587, sys_id, DVec3::ZERO)
                .await;

        // #162: DirectionalScan / ConeScan while docked must fail with CannotDirectionalScanWhileDocked
        let mut docked_sess = sess.clone();
        docked_sess.station_id = Some(60_003_760);
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().station_id = Some(60_003_760);
        });
        let dscan_err = ctx
            .dispatch_rpc(
                &mut docked_sess,
                "scanMgr",
                None,
                "ConeScan",
                &[PyRep::Real(360.0), PyRep::Real(1_000_000.0)],
                &kw,
                None,
            )
            .await
            .expect_err("ConeScan while docked must fail");
        assert_eq!(dscan_err.code, "CannotDirectionalScanWhileDocked");
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().station_id = None;
        });

        // #160: Hacking minigame minimum difficulty floor ignores client difficulty=1 spoof
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                87_160_801,
                item_rec(
                    87_160_801,
                    22177, // Data Analyzer I
                    char_id,
                    ship_id,
                    InventoryFlag::MedSlot0,
                    1,
                    true,
                ),
            );
            snap.items.insert(
                87_160_802,
                item_rec(
                    87_160_802,
                    22175, // Relic Analyzer I
                    char_id,
                    ship_id,
                    InventoryFlag::MedSlot1,
                    1,
                    true,
                ),
            );
            snap.module_online_states.insert(87_160_801, true);
            snap.module_online_states.insert(87_160_802, true);
        });
        ctx.invalidate_ship_fitting(ship_id);
        eve_server::services::pve_exploration::ensure_system_pve_sites_seeded(&ctx, sys_id).await;
        let relic_can_id = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
            let sites = reg.sites_by_system.get_mut(&sys_id).unwrap();
            let relic_site = sites
                .iter_mut()
                .find(|s| s.site_category == "Relic" || s.site_category == "Ghost")
                .unwrap();
            relic_site.position = [0.0, 0.0, 0.0];
            relic_site.hackable_can_ids[0]
        });
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle
            .spawn_ball(Ball {
                id: relic_can_id,
                type_id: 26473,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 10.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        let _hack_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "hackingMgr",
                None,
                "StartNewGameInstance",
                &[
                    PyRep::Integer(relic_can_id),
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                    PyRep::Integer(1), // client difficulty=1 spoof
                ],
                &kw,
                None,
            )
            .await
            .expect("StartNewGameInstance");
        let (utility_cap, core_coherence) =
            eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
                let g = reg.hacking_games.get(&(char_id, relic_can_id)).unwrap();
                let core_coh = g
                    .tiles
                    .iter()
                    .find(|t| t.kind == eve_server::services::pve_exploration::HackingNodeKind::SystemCore)
                    .map(|t| t.coherence)
                    .unwrap_or(0);
                (g.utilities.len(), core_coh)
            });
        assert_eq!(utility_cap, 3, "Relic Analyzer I has 3 virusElementSlots");
        assert!(
            core_coherence >= 50,
            "Client difficulty=1 argument must not bypass container minimum difficulty floor (got core coherence {core_coherence})"
        );

        // #161: Expired bookmark and restricted ACL folder bookmark must be rejected in CmdAlignToBookmark
        let exp_bm_id = 87_161_001i64;
        let acl_bm_id = 87_161_002i64;
        let folder_id = 87_161_010i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.bookmarks.insert(exp_bm_id, BookmarkRecord {
                bookmark_id: exp_bm_id, owner_id: char_id, folder_id: None, is_shared: false,
                solar_system_id: sys_id, item_id: None, label: "Expired".into(), memo: String::new(), x: 200_000.0, y: 0.0, z: 0.0,
            });
            snap.bookmark_expiries.insert(exp_bm_id, ctx.now_unix().saturating_sub(10));
            snap.bookmark_folders.insert(folder_id, BookmarkFolderRecord {
                folder_id, owner_id: 99_999_999, folder_name: "Private Shared Folder".into(), is_shared: true,
                corporation_id: None, description: String::new(), admin_group_id: Some(99_999_999),
                manage_group_id: Some(99_999_999), use_group_id: Some(99_999_999), view_group_id: Some(99_999_999),
            });
            snap.bookmarks.insert(acl_bm_id, BookmarkRecord {
                bookmark_id: acl_bm_id, owner_id: 99_999_999, folder_id: Some(folder_id), is_shared: true,
                solar_system_id: sys_id, item_id: None, label: "ACL Restricted".into(), memo: String::new(), x: 200_000.0, y: 0.0, z: 0.0,
            });
        });
        let exp_err = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                None,
                "CmdGotoBookmark",
                &[PyRep::Integer(exp_bm_id)],
                &kw,
                None,
            )
            .await
            .expect_err("Expired bookmark must be rejected");
        assert_eq!(exp_err.code, "BookmarkExpired");

        let acl_err = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                None,
                "CmdGotoBookmark",
                &[PyRep::Integer(acl_bm_id)],
                &kw,
                None,
            )
            .await
            .expect_err("Bookmark in folder without ACL access must be rejected");
        assert_eq!(acl_err.code, "AccessDenied");

        // #163: Salvager module range check (wreck at 12 km > 6 km max range) and cargo capacity check
        let far_wreck_id = 87_163_001i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(char_id).or_default().insert(
                3438,
                TrainedSkillRecord {
                    type_id: 3438,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
            snap.items.insert(
                far_wreck_id,
                item_rec(
                    far_wreck_id,
                    26473,
                    char_id,
                    sys_id as i64,
                    InventoryFlag::None,
                    1,
                    true,
                ),
            );
        });
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle
            .spawn_ball(Ball {
                id: far_wreck_id,
                type_id: 26473,
                position: DVec3::new(12_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 10_000.0,
                radius: 25.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        let range_err =
            execute_wreck_salvage_cycle(&ctx, sys_id, char_id, ship_id, far_wreck_id, Some(25861))
                .await
                .expect_err("Salvager I at 12 km must fail with TargetOutOfRange");
        assert_eq!(range_err.code, "TargetOutOfRange");
    }
}

pub(crate) mod fighter_point_targeted_charges_death {
    // Round-5 Track 1 Regression Tests (#199 through #211):
    // - #199: ActivateFighterAbilitySlotsOnPoint requires pointTargeted ability with charges, range/safety checks
    // - #200: ActivateFighterAbilitySlotsOnEntities triggers death pipeline & crimewatch on lethal hits
    // - #201: Death pipeline consumes insurance contract once, enforces end_unix, and blocks self-destruct bounty farming
    // - #202: Weapon activation with zero locked targets fails closed with TargetNotLocked and resets spool
    // - #203: Lancer/Doomsday checks reactivation cooldown & consumes isotopes before applying beam damage
    // - #204: ActivateRandomJumpFilament enforces scramble/warp/crimewatch on fleetmates and preserves HP
    // - #205: GetFightersForShip enforces ownership and RecallFightersToTubes checks owner/lock & preserves loaded tube
    // - #206: WormholeJump preserves pre-jump HP/capacitor and allows first return jump before polarizing
    // - #207: Vorton Projector secondary arcs exclude wrecks/invulnerable/safety-protected capsuleers
    // - #208: Breacher pod DoT ticks only on its own solar system step and applies on missile impact
    // - #209: Smartbombs and Bombs spare wrecks and record killmail assist / LE / sec status loss
    // - #210: RecoverProbes preserves in-space probes when cargo is full; DestroyProbe spares cargo probes
    // - #211: ClickedOnTile enforces cloak/range checks; QuitHackingAttempt detonates active Ghost Site

    use eve_db::{
        AccountRecord, CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents, NeuralAttributes,
    };
    use eve_destiny::{Ball, DamageVector, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::combat::death_pipeline::execute_death_pipeline_sync_inner;
    use eve_server::router::{ActiveScannerProbe, ServerContext};
    use eve_server::services::combat_crimewatch::{
        record_killmail_assist_action, step_combat_system, with_combat_runtime,
        with_mut_combat_runtime, ActiveBreacherDot, EntropicSpoolState, InsuranceContractState,
        SafetyLevel,
    };
    use eve_server::services::pve_exploration::{
        with_pve_exploration_reg, HackingGameSession, HackingNodeKind, HackingTileState, PveSiteInstance,
    };
    use glam::DVec3;
    use std::collections::HashMap;

    fn item_rec(
        item_id: i64, type_id: u32, owner_id: i64, location_id: i64,
        flag: InventoryFlag, quantity: i32, singleton: bool,
    ) -> InventoryItemRecord {
        InventoryItemRecord {
            item_id, type_id, owner_id, location_id, flag, quantity, singleton,
            version: 1, dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        }
    }

    async fn setup_pilot_in_space(
        ctx: &ServerContext, client_id: u64, char_id: i64, corp_id: i64,
        ship_id: i64, ship_type_id: u32, sys_id: u32, pos: DVec3,
    ) -> SessionState {
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.insert(
                char_id,
                CharacterRecord {
                    character_id: char_id, user_id: client_id, name: format!("Pilot {char_id}"),
                    corporation_id: corp_id, alliance_id: None, war_faction_id: None,
                    station_id: None, structure_id: None, solar_system_id: sys_id,
                    constellation_id: 20_000_020, region_id: 10_000_002, active_ship_id: ship_id,
                    balance: IskCents::from_isk(100_000_000), plex_balance: 0,
                    loyalty_points: HashMap::new(), standings: HashMap::new(),
                    security_status: 2.0, clone_grade: 2, home_station_id: 60_003_760,
                    race_id: 1, bloodline_id: 1, ancestry_id: 1, school_id: 1,
                    neural: NeuralAttributes::default(), skill_points: 5_000_000.0,
                    activity_status: None, free_skill_points: 0.0, last_clone_jump_unix: 0,
                    jump_fatigue_minutes: 0.0, jump_cooldown_until_unix: 0, jump_fatigue_until_unix: 0,
                    description: String::new(), title: String::new(), gender: 1,
                    create_date_unix: 1_700_000_000, fleet_id: None, autopilot_waypoints: Vec::new(),
                    version: 1, system_epoch: 0,
                },
            );
            snap.accounts.insert(
                format!("user_{client_id}"),
                AccountRecord {
                    user_id: client_id, username: format!("user_{client_id}"),
                    password_token: String::new(), role: 0, banned_until_unix: None, ban_reason: None,
                },
            );
            snap.items.insert(
                ship_id,
                item_rec(ship_id, ship_type_id, char_id, sys_id as i64, InventoryFlag::None, 1, true),
            );
        });
        let _ = ctx.db.acquire_online_lock(client_id, 8_000 + client_id, Some(char_id), true, ctx.now_unix());
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: ship_id, type_id: ship_type_id, position: pos, velocity: DVec3::ZERO,
                    max_velocity: 300.0, inertia: 2.5, mass: 10_000_000.0, radius: 50.0,
                    speed_fraction: 0.0, mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(ship_type_id),
            )
            .await;
        let mut sess = SessionState::new(8_000 + client_id);
        sess.user_id = Some(client_id);
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(corp_id);
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(sys_id);
        sess
    }

    #[tokio::test]
    async fn test_r5_199_200_205_fighter_point_targeted_charges_death_pipeline_and_recall_acl() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let sys_id = 30_002_813u32;
        let (char_id, other_id, ship_id, other_ship_id) =
            (98_199_001i64, 98_199_002i64, 88_199_101i64, 88_199_102i64);
        let mut sess = setup_pilot_in_space(&ctx, 81991, char_id, 1_000_125, ship_id, 23757, sys_id, DVec3::ZERO).await;
        let mut other_sess = setup_pilot_in_space(&ctx, 81992, other_id, 1_000_126, other_ship_id, 587, sys_id, DVec3::new(1_000.0, 0.0, 0.0)).await;

        let acl_err = ctx
            .dispatch_rpc(&mut other_sess, "fighterMgr", None, "GetFightersForShip", &[PyRep::Integer(ship_id)], &kw, None)
            .await
            .expect_err("Unrelated pilot must not inspect carrier fighter bay/tubes");
        assert_eq!(acl_err.code, "AccessDenied");

        let fighter_id = 88_199_201i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(fighter_id, item_rec(fighter_id, 23055, char_id, ship_id, InventoryFlag::FighterBay, 9, false));
        });
        ctx.dispatch_rpc(&mut sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(fighter_id)], &kw, None).await.unwrap();
        ctx.dispatch_rpc(&mut sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None).await.unwrap();
        let f_ball_id = *ctx.active_fighters.read().unwrap().keys().next().expect("Launched fighter ball ID");

        let pt_err = ctx
            .dispatch_rpc(
                &mut sess, "fighterMgr", None, "CmdActivateAbilitySlots",
                &[PyRep::Integer(f_ball_id), PyRep::Integer(0), PyRep::Tuple(vec![PyRep::Real(500.0), PyRep::Real(0.0), PyRep::Real(0.0)])],
                &kw, None,
            )
            .await
            .expect_err("Non-pointTargeted ability slot must be rejected on point activation");
        assert_eq!(pt_err.code, "InvalidTargetMode");

        let npc_ball_id = 1_895_199_001i64;
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let mut npc_cs = ctx.build_type_combat_state(587);
        npc_cs.shield_hp = 0.0;
        npc_cs.armor_hp = 0.0;
        npc_cs.structure_hp = 1.0;
        let _ = sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: npc_ball_id, type_id: 587, position: DVec3::new(500.0, 0.0, 0.0), velocity: DVec3::ZERO,
                    max_velocity: 200.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0,
                    speed_fraction: 0.0, mode: MovementMode::Stop,
                },
                npc_cs,
            )
            .await;
        let _ = sys_handle
            .add_target_lock(
                ship_id,
                eve_destiny::TargetLock {
                    target_id: npc_ball_id,
                    locked: true,
                    remaining_lock_ms: 0.0,
                    lock_time_ms: 0.0,
                    max_range_m: Some(100_000.0),
                },
            )
            .await;
        ctx.dispatch_rpc(
            &mut sess, "fighterMgr", None, "CmdActivateAbilitySlots",
            &[PyRep::List(vec![PyRep::Integer(f_ball_id)]), PyRep::Integer(0), PyRep::Integer(npc_ball_id)],
            &kw, None,
        )
        .await
        .expect("Lethal fighter ability on NPC");
        assert!(
            sys_handle.get_ball_snapshot(npc_ball_id).await.unwrap().is_none(),
            "Destroyed NPC ball must be removed by fighter death pipeline"
        );
    }

    #[tokio::test]
    async fn test_r5_201_insurance_single_use_consumption_and_self_destruct_bounty_block() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let now = ctx.now_unix();
        let sys_id = 30_002_813u32;
        let (vic_id, atk_id, ship_id, atk_ship_id) =
            (98_201_001i64, 98_201_002i64, 88_201_101i64, 88_201_102i64);
        let _vic_sess = setup_pilot_in_space(&ctx, 82011, vic_id, 1_000_125, ship_id, 587, sys_id, DVec3::ZERO).await;
        let _atk_sess = setup_pilot_in_space(&ctx, 82012, atk_id, 1_000_126, atk_ship_id, 587, sys_id, DVec3::new(1_000.0, 0.0, 0.0)).await;

        with_mut_combat_runtime(&ctx, |rt| {
            rt.insurance_contracts.insert(
                ship_id,
                InsuranceContractState {
                    ship_id, ship_type_id: 587, owner_id: vic_id, level_name: "Platinum".into(),
                    level_fraction: 1.0, cost_isk: 1_000_000.0, payout_isk: 2_000_000.0,
                    start_unix: now - 100, end_unix: now + 86_400,
                },
            );
            rt.player_bounty_pool.insert(vic_id, 50_000_000.0);
        });
        record_killmail_assist_action(&ctx, ship_id, atk_id, 587, 486, 100.0, false, now);

        let atk_bal_before = ctx.db.with_snapshot(|s| s.characters[&atk_id].balance.to_wire_f64());
        let _res = execute_death_pipeline_sync_inner(&ctx, sys_id, vic_id, ship_id, 587, DVec3::ZERO, atk_id as u64, true)
            .expect("Self-destruct death pipeline");
        let atk_bal_after = ctx.db.with_snapshot(|s| s.characters[&atk_id].balance.to_wire_f64());
        assert!((atk_bal_after - atk_bal_before).abs() < 1e-2, "Attacker must not receive bounty payout on victim self-destruct");
        let contract_remaining = with_combat_runtime(&ctx, |rt| rt.insurance_contracts.contains_key(&ship_id));
        assert!(!contract_remaining, "Insurance contract must be consumed upon ship destruction");
    }

    #[tokio::test]
    async fn test_r5_202_207_weapon_zero_lock_deactivation_and_vorton_secondary_arc_safety() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let sys_id = 30_000_142u32;
        let (char_id, ship_id, gun_id, ammo_id) =
            (98_202_001i64, 88_202_101i64, 88_202_201i64, 88_202_202i64);
        let mut sess = setup_pilot_in_space(&ctx, 82021, char_id, 1_000_125, ship_id, 587, sys_id, DVec3::ZERO).await;

        let npc_ball_id = 1_895_202_001i64;
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: npc_ball_id, type_id: 587, position: DVec3::new(1_500.0, 0.0, 0.0), velocity: DVec3::ZERO,
                    max_velocity: 200.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0,
                    speed_fraction: 0.0, mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(587),
            )
            .await;

        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(gun_id, item_rec(gun_id, 486, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
            snap.items.insert(ammo_id, item_rec(ammo_id, 178, char_id, ship_id, InventoryFlag::HiSlot0, 100, false));
            snap.module_online_states.insert(gun_id, true);
            snap.module_active_states.insert(gun_id);
        });
        ctx.invalidate_ship_fitting(ship_id);
        with_mut_combat_runtime(&ctx, |rt| {
            rt.entropic_spools.insert(
                (ship_id, gun_id),
                EntropicSpoolState {
                    target_id: npc_ball_id, solar_system_id: sys_id, attacker_char_id: char_id,
                    attacker_ship_id: ship_id, module_id: gun_id, module_type_id: 486,
                    charge_item_id: Some(ammo_id), base_damage: DamageVector::new(10.0, 10.0, 10.0, 10.0),
                    cap_need: 1.0, duration_s: 3.0, cycles: 5, elapsed_in_cycle: 0.0,
                    per_cycle_bonus: 0.1, max_bonus: 1.5, current_multiplier: 1.5,
                },
            );
        });

        let err = eve_server::services::combat_crimewatch::dispatch_combat_rpc(
            &ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(npc_ball_id)], &kw,
        )
        .await
        .expect_err("Firing weapon with zero locked targets must fail");
        assert_eq!(err.code, "TargetNotLocked");
        let spool_exists = with_combat_runtime(&ctx, |rt| rt.entropic_spools.contains_key(&(ship_id, gun_id)));
        assert!(!spool_exists, "Entropic spool must reset when lock check fails");
    }

    #[tokio::test]
    async fn test_r5_203_superweapon_doomsday_fuel_before_damage_and_cooldown() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let sys_id = 30_002_813u32;
        let (char_id, vic_id, ship_id, vic_ship_id, lance_id) =
            (98_203_001i64, 98_203_002i64, 88_203_101i64, 88_203_102i64, 88_203_201i64);
        let mut sess = setup_pilot_in_space(&ctx, 82031, char_id, 1_000_125, ship_id, 77281, sys_id, DVec3::ZERO).await;
        let _vic_sess = setup_pilot_in_space(&ctx, 82032, vic_id, 1_000_126, vic_ship_id, 587, sys_id, DVec3::new(5_000.0, 0.0, 0.0)).await;

        let siege_id = 88_203_202i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(lance_id, item_rec(lance_id, 77398, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
            snap.items.insert(siege_id, item_rec(siege_id, 20280, char_id, ship_id, InventoryFlag::HiSlot1, 1, true));
            snap.module_active_states.insert(siege_id);
            let skills = snap.skills.entry(char_id).or_default();
            skills.insert(77738, eve_db::TrainedSkillRecord { type_id: 77738, level: 5, skill_points: 256_000.0 });
            skills.insert(77739, eve_db::TrainedSkillRecord { type_id: 77739, level: 5, skill_points: 256_000.0 });
        });

        let err = ctx
            .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(lance_id), PyRep::Integer(vic_ship_id)], &kw, None)
            .await
            .expect_err("Lancer Doomsday without isotopes must fail");
        assert_eq!(err.code, "InsufficientFuel");

        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let vic_cs = sys_handle.get_combat_state(vic_ship_id).await.unwrap().unwrap();
        assert!((vic_cs.shield_hp - vic_cs.max_shield_hp).abs() < 1e-3, "Victim in beam path must take zero damage when Lancer lacks isotopes");
    }

    #[tokio::test]
    async fn test_r5_204_206_filament_fleetmate_scramble_and_wormhole_hp_preservation() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let (sys_a, sys_b) = (30_002_813u32, 31_000_005u32);
        let (c1, c2, s1, s2, fil_id) =
            (98_204_001i64, 98_204_002i64, 88_204_101i64, 88_204_102i64, 88_204_201i64);
        let deep_pos = DVec3::new(500_000_000_000.0, 500_000_000_000.0, 0.0);
        let mut sess1 = setup_pilot_in_space(&ctx, 82041, c1, 1_000_125, s1, 587, sys_a, deep_pos).await;
        let _sess2 = setup_pilot_in_space(&ctx, 82042, c2, 1_000_125, s2, 587, sys_a, deep_pos + DVec3::new(500.0, 0.0, 0.0)).await;

        let fil_type_id = ctx
            .sde
            .types
            .iter()
            .find(|(_, t)| t.group_id == 4041 && t.default_attributes.contains_key(&3025))
            .map(|(&id, _)| id)
            .expect("Needlejack filament type in SDE");
        let fleet_rep = ctx.dispatch_rpc(&mut sess1, "fleetMgr", None, "CreateFleet", &[], &kw, None).await.unwrap();
        let fleet_id = fleet_rep.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap_or(1);
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&c1).unwrap().fleet_id = Some(fleet_id);
            snap.characters.get_mut(&c2).unwrap().fleet_id = Some(fleet_id);
            if let Some(f) = snap.fleets.get_mut(&fleet_id) {
                if !f.members.contains(&c2) {
                    f.members.push(c2);
                }
            }
            snap.items.insert(fil_id, item_rec(fil_id, fil_type_id, c1, s1, InventoryFlag::Cargo, 1, false));
        });
        let sys_handle_a = ctx.systems.get_or_spawn(sys_a);
        let mut cs2 = sys_handle_a.get_combat_state(s2).await.unwrap().unwrap();
        cs2.warp_scramble_strength = 2;
        let _ = sys_handle_a.set_combat_state(s2, cs2).await;

        // #204: Scrambled fleetmate s2 is excluded from jumpers when c1 activates a random jump filament
        ctx.dispatch_rpc(&mut sess1, "randomJumpMgr", None, "ActivateRandomJumpFilament", &[PyRep::Integer(fil_id)], &kw, None).await.unwrap();
        let c2_sys = ctx.db.with_snapshot(|s| s.characters[&c2].solar_system_id);
        assert_eq!(c2_sys, sys_a, "Scrambled fleetmate must not be pulled into filament jump");

        // #206: Return c1 to sys_a, damage s1 to 40% armor, jump through wormhole to sys_b, and verify HP is preserved AND return jump is allowed
        ctx.db.clock().advance_secs(15);
        let now = ctx.now_unix();
        sess1.solar_system_id = Some(sys_a);
        ctx.db.with_mut_snapshot(|s| {
            s.characters.get_mut(&c1).unwrap().solar_system_id = sys_a;
            s.cosmic_signatures.entry(sys_a).or_default().push(eve_db::CosmicSignatureRecord {
                signature_id: "WH-206".into(),
                solar_system_id: sys_a,
                sig_group: "Wormhole".into(),
                type_name: "Unstable Wormhole".into(),
                position: [0.0, 0.0, 0.0],
                signal_strength: 1.0,
                destination_system_id: Some(sys_b),
                remaining_mass_kg: 2_000_000_000.0,
                max_mass_kg: 2_000_000_000.0,
                max_jump_mass_kg: 300_000_000.0,
                lifetime_seconds: 86_400,
                created_unix: now,
                collapsed: false,
                wormhole_type_id: Some(26272),
            });
        });
        let mut cs1 = ctx.build_type_combat_state(587);
        cs1.shield_hp = 0.0;
        cs1.armor_hp = cs1.max_armor_hp * 0.4;
        let expected_armor = cs1.armor_hp;
        let _ = sys_handle_a
            .spawn_ball_with_combat(
                Ball {
                    id: s1, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO,
                    max_velocity: 300.0, inertia: 2.5, mass: 10_000_000.0, radius: 50.0,
                    speed_fraction: 0.0, mode: MovementMode::Stop,
                },
                cs1,
            )
            .await;

        let wh_rep = ctx
            .dispatch_rpc(&mut sess1, "wormholeMgr", None, "WormholeJump", &[PyRep::String("WH-206".into()), PyRep::Integer(sys_b as i64)], &kw, None)
            .await
            .expect("First wormhole transit A -> B");
        let sys_handle_b = ctx.systems.get_or_spawn(sys_b);
        let cs1_after = sys_handle_b.get_combat_state(s1).await.unwrap().unwrap();
        assert_eq!(cs1_after.shield_hp, 0.0, "Shield HP must not heal to 100% on wormhole jump");
        assert!((cs1_after.armor_hp - expected_armor).abs() < 1.0, "Armor HP must be preserved across wormhole jump");

        ctx.db.clock().advance_secs(15);
        let k162_id = wh_rep.dict_get("exitWormholeID").and_then(|v| v.as_i64()).unwrap();
        let exit_pos = sys_handle_b
            .get_ball_snapshot(k162_id)
            .await
            .unwrap()
            .map(|(_, b)| b.position)
            .unwrap_or(DVec3::ZERO);
        let _ = sys_handle_b
            .spawn_ball_with_combat(
                Ball {
                    id: s1, type_id: 587, position: exit_pos, velocity: DVec3::ZERO,
                    max_velocity: 300.0, inertia: 2.5, mass: 10_000_000.0, radius: 50.0,
                    speed_fraction: 0.0, mode: MovementMode::Stop,
                },
                cs1_after,
            )
            .await;
        ctx.dispatch_rpc(&mut sess1, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(k162_id), PyRep::Integer(sys_a as i64)], &kw, None)
            .await
            .expect("First return jump B -> A must not be prematurely polarized");
    }

    #[tokio::test]
    async fn test_r5_208_209_breacher_dot_impact_system_scoping_and_smartbomb_wreck_crimewatch() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let (sys_a, sys_b) = (30_002_813u32, 30_000_142u32);
        let (atk_id, vic_id, atk_ship, vic_ship, sb_id, wreck_id) =
            (98_208_001i64, 98_208_002i64, 88_208_101i64, 88_208_102i64, 88_208_201i64, 88_208_301i64);
        let mut atk_sess = setup_pilot_in_space(&ctx, 82081, atk_id, 1_000_125, atk_ship, 645, sys_a, DVec3::ZERO).await;
        let _vic_sess = setup_pilot_in_space(&ctx, 82082, vic_id, 1_000_126, vic_ship, 587, sys_a, DVec3::new(1_000.0, 0.0, 0.0)).await;

        with_mut_combat_runtime(&ctx, |rt| {
            rt.active_breacher_dots.insert(
                vic_ship,
                ActiveBreacherDot {
                    target_ball_id: vic_ship, source_char_id: atk_id, source_ship_id: atk_ship,
                    weapon_type_id: 486, charge_type_id: 178, flat_cap_dps: 100.0,
                    pct_cap_fraction: 0.01, remaining_s: 50.0, tick_damage: 100.0,
                },
            );
        });
        step_combat_system(&ctx, sys_b, 10.0).await;
        let rem_after_other_sys = with_combat_runtime(&ctx, |rt| rt.active_breacher_dots[&vic_ship].remaining_s);
        assert!((rem_after_other_sys - 50.0).abs() < 1e-6, "Stepping another solar system must not decrement Breacher DoT timer");

        let sys_handle = ctx.systems.get_or_spawn(sys_a);
        let _ = sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: wreck_id, type_id: 26561, position: DVec3::new(500.0, 0.0, 0.0), velocity: DVec3::ZERO,
                    max_velocity: 0.0, inertia: 1.0, mass: 10_000.0, radius: 25.0,
                    speed_fraction: 0.0, mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(26561),
            )
            .await;
        let sb_type_id = ctx.sde.types.values().find(|t| t.group_id == 72).map(|t| t.type_id).unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(sb_id, item_rec(sb_id, sb_type_id, atk_id, atk_ship, InventoryFlag::HiSlot0, 1, true));
            snap.module_online_states.insert(sb_id, true);
        });
        ctx.invalidate_ship_fitting(atk_ship);
        with_mut_combat_runtime(&ctx, |rt| {
            rt.safety_levels.insert(atk_id, SafetyLevel::Red);
        });
        let sec_before = ctx.db.with_snapshot(|s| s.characters[&atk_id].security_status);
        eve_server::services::combat_crimewatch::dispatch_combat_rpc(
            &ctx, &mut atk_sess, "dogmaIM", "Activate", &[PyRep::Integer(sb_id)], &kw,
        )
        .await
        .expect("Smartbomb activation in lowsec");

        assert!(sys_handle.get_ball_snapshot(wreck_id).await.unwrap().is_some(), "Smartbomb must not damage or destroy wrecks");
        let sec_after = ctx.db.with_snapshot(|s| s.characters[&atk_id].security_status);
        assert!(sec_after < sec_before, "Illegal lowsec smartbomb hit must reduce security status");
        let has_le = with_combat_runtime(&ctx, |rt| rt.has_limited_engagement_with(atk_id, vic_id, ctx.now_unix()));
        assert!(!has_le, "Illegal lowsec smartbomb hit must NOT create Limited Engagement");
        assert!(ctx.crimewatch.read().unwrap().states[&atk_id].is_suspect(ctx.now_unix()));
    }

    #[tokio::test]
    async fn test_r5_210_211_probe_full_cargo_preservation_and_hacking_cloak_ghost_quit() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let sys_id = 30_002_813u32;
        let (char_id, ship_id, probe_id, cargo_probe_id, filler_id, analyzer_id, can_id) =
            (98_210_001i64, 88_210_101i64, 88_210_201i64, 88_210_202i64, 88_210_203i64, 88_210_204i64, 88_210_301i64);
        let mut sess = setup_pilot_in_space(&ctx, 82101, char_id, 1_000_125, ship_id, 587, sys_id, DVec3::ZERO).await;

        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(filler_id, item_rec(filler_id, 34, char_id, ship_id, InventoryFlag::Cargo, 100_000, false));
            snap.items.insert(cargo_probe_id, item_rec(cargo_probe_id, 30013, char_id, ship_id, InventoryFlag::Cargo, 4, false));
            snap.items.insert(analyzer_id, item_rec(analyzer_id, 22175, char_id, ship_id, InventoryFlag::MedSlot0, 1, true));
        });
        {
            let mut map = ctx.active_scanner_probes.write().unwrap();
            map.insert(
                probe_id,
                ActiveScannerProbe {
                    probe_id, probe_type_id: 30013, owner_char_id: char_id, ship_id, launcher_module_id: 0,
                    solar_system_id: sys_id, position: [0.0, 0.0, 0.0], destination: [0.0, 0.0, 0.0],
                    range_step: 1, scan_range_m: 149_597_870_700.0, base_sensor_strength: 40.0,
                    max_flight_time_s: 3600.0, launched_at_unix: ctx.now_unix(), state: 1,
                    warp_end_ft: None,
                },
            );
            map.insert(
                probe_id + 1,
                ActiveScannerProbe {
                    probe_id: probe_id + 1, probe_type_id: 30013, owner_char_id: char_id, ship_id, launcher_module_id: 0,
                    solar_system_id: sys_id, position: [0.0, 0.0, 0.0], destination: [0.0, 0.0, 0.0],
                    range_step: 1, scan_range_m: 149_597_870_700.0, base_sensor_strength: 40.0,
                    max_flight_time_s: 3600.0, launched_at_unix: ctx.now_unix(), state: -1,
                    warp_end_ft: None,
                },
            );
        }

        let rec_rep = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RecoverProbes", &[], &kw, None).await.expect("RecoverProbes with full cargo");
        assert_eq!(rec_rep.dict_get("recoveredCount").and_then(|v| v.as_i64()), Some(0));
        assert!(ctx.active_scanner_probes.read().unwrap().contains_key(&probe_id), "Unrecovered probe must remain in active_scanner_probes");

        ctx.dispatch_rpc(&mut sess, "scanMgr", None, "DestroyProbe", &[PyRep::Integer(probe_id + 1)], &kw, None).await.expect("DestroyProbe on expired probe");
        let cargo_qty = ctx.db.with_snapshot(|s| s.items[&cargo_probe_id].quantity);
        assert_eq!(cargo_qty, 4, "DestroyProbe must not delete probes from ship cargo");

        with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system.entry(sys_id).or_default().push(PveSiteInstance {
                site_id: "GHOST-R5-211".into(),
                solar_system_id: sys_id,
                site_category: "Ghost".into(),
                position: [0.0, 0.0, 0.0],
                hackable_can_ids: vec![can_id],
                ghost_timer_started_at: Some(ctx.now_unix()),
                ghost_detonate_after_sec: 120,
                ..Default::default()
            });
            reg.hacking_games.insert(
                (char_id, can_id),
                HackingGameSession {
                    game_id: 500_211, character_id: char_id, solar_system_id: sys_id, container_id: can_id,
                    site_id: "GHOST-R5-211".into(), is_relic: false, is_ghost_site: true,
                    is_sleeper_cache_puzzle: false, virus_coherence: 60, max_virus_coherence: 60,
                    base_virus_strength: 25, polymorphic_shield_charges: 0, secondary_vector_target: None,
                    utilities: vec![None; 3],
                    tiles: vec![
                        HackingTileState { tile_index: 0, revealed: true, kind: HackingNodeKind::Empty, coherence: 0, strength: 0, utility_drop: None, blocked_by_defensive: false },
                        HackingTileState { tile_index: 1, revealed: false, kind: HackingNodeKind::Empty, coherence: 0, strength: 0, utility_drop: None, blocked_by_defensive: false },
                    ],
                    status: "InProgress".into(),
                },
            );
        });
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let mut cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        cs.is_cloaked = true;
        let _ = sys_handle.set_combat_state(ship_id, cs.clone()).await;
        let cloak_err = ctx
            .dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(can_id), PyRep::Integer(1)], &kw, None)
            .await
            .expect_err("ClickedOnTile while cloaked must fail");
        assert_eq!(cloak_err.code, "CannotHackWhileCloaked");
        cs.is_cloaked = false;
        let _ = sys_handle.set_combat_state(ship_id, cs).await;

        ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "QuitHackingAttempt", &[PyRep::Integer(can_id)], &kw, None).await.expect("QuitHackingAttempt on Ghost Site");
        let detonated = with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system[&sys_id].iter().find(|s| s.site_id == "GHOST-R5-211").unwrap().ghost_detonated
        });
        assert!(detonated, "Quitting an active Ghost Site hack must trigger Ghost Site detonation");
    }
}

pub(crate) mod cyno_lighting_rules_beacons_covert {
    // Dedicated end-to-end integration tests for open `feat:modules.*`, `feat:ships.*`, and `feat:combat.*` parity rows (Part 1).

    use crate::modules_advanced_tactical::{build_ctx, seed_pilot, NULL_SYS};
    use crate::nav_tactical_modules::{give_item, put_in_fleet, set_skill, spawn_ship_in_space, DEST_NULL_SYS};
    use eve_db::InventoryFlag;
    use eve_destiny::{Ball, DVec3, MovementMode};
    use eve_proto::PyRep;
    use eve_server::combat::death_pipeline::execute_death_pipeline;
    use eve_server::services::combat_crimewatch::with_combat_runtime;
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_cyno_lighting_rules_beacons_and_covert_cyno() {
        let ctx = build_ctx();
        let kw = HashMap::new();
        let jita_sys: u32 = 30_000_142; // 0.95 high-sec
        let pochven_sys: u32 = 30_000_021; // Kuharah (Pochven)

        let char_id = 981_001;
        let ship_id = 991_001;
        let mut sess = seed_pilot(&ctx, char_id, ship_id, 11957, jita_sys, false); // Falcon Force Recon (group 833)
        put_in_fleet(&ctx, 77_001, char_id, &[(char_id, true)]);
        set_skill(&ctx, char_id, 21603, 5); // Cynosural Field Theory V
        spawn_ship_in_space(&ctx, char_id, ship_id, 11957, jita_sys, DVec3::new(10_000.0, 0.0, 0.0)).await;

        let cyno_mod = 995_001;
        give_item(&ctx, cyno_mod, 21096, char_id, ship_id, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, 995_002, 16273, char_id, ship_id, InventoryFlag::Cargo, 2_000); // Liquid Ozone

        // 1. High-sec blocks Cynosural Field Generator I
        assert!(
            ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod)], &kw, None)
                .await
                .is_err(),
            "Cyno must be blocked in high-security space (Jita)"
        );

        // 2. Pochven (region 10000070) blocks Cynosural Field Generator I
        let _ = ctx.systems.get_or_spawn(jita_sys).remove_ball(ship_id).await;
        sess.solar_system_id = Some(pochven_sys);
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&char_id) {
                ch.solar_system_id = pochven_sys;
            }
        });
        spawn_ship_in_space(&ctx, char_id, ship_id, 11957, pochven_sys, DVec3::new(10_000.0, 0.0, 0.0)).await;
        assert_eq!(
            ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod)], &kw, None)
                .await
                .unwrap_err()
                .code,
            "CynoJammedInPochven",
            "Pochven region-wide cyno jammer must block cyno activation"
        );

        // 3. Null-sec (30004759): moving >= 500 m/s blocks cyno; stopping allows cyno, consumes Liquid Ozone, spawns beacon ball, and immobilizes ship!
        let _ = ctx.systems.get_or_spawn(pochven_sys).remove_ball(ship_id).await;
        sess.solar_system_id = Some(NULL_SYS);
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&char_id) {
                ch.solar_system_id = NULL_SYS;
            }
        });
        let sys_null = ctx.systems.get_or_spawn(NULL_SYS);
        let fast_ball = Ball {
            id: ship_id,
            type_id: 11957,
            position: DVec3::new(20_000.0, 0.0, 0.0),
            velocity: DVec3::new(650.0, 0.0, 0.0),
            max_velocity: 1_000.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 150.0,
            speed_fraction: 0.65,
            mode: MovementMode::GotoDirection { direction: DVec3::X },
        };
        let _ = sys_null
            .spawn_ball_with_combat(fast_ball, ctx.build_type_combat_state(11957))
            .await;
        assert_eq!(
            ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod)], &kw, None)
                .await
                .unwrap_err()
                .code,
            "ShipMovingTooFast",
            "Cannot light cyno while moving >= 500 m/s"
        );

        let _ = sys_null.remove_ball(ship_id).await;
        spawn_ship_in_space(&ctx, char_id, ship_id, 11957, NULL_SYS, DVec3::new(20_000.0, 0.0, 0.0)).await;
        let ozone_before = ctx.db.with_snapshot(|s| s.items.get(&995_002).unwrap().quantity);
        let cyno_res = ctx
            .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod)], &kw, None)
            .await
            .expect("Activate Cynosural Field Generator I in Null-sec");
        let ozone_after = ctx.db.with_snapshot(|s| s.items.get(&995_002).unwrap().quantity);
        assert!(ozone_after < ozone_before, "Lighting cyno must consume Liquid Ozone ({ozone_after} < {ozone_before})");
        let beacon_id = cyno_res
            .dict_get("beaconID")
            .and_then(|v| v.as_i64())
            .expect("Beacon ID must be spawned");
        assert!(
            sys_null.get_ball_snapshot(beacon_id).await.unwrap().is_some(),
            "Cynosural beacon ball must exist in space"
        );
        assert!(
            eve_server::services::inventory_fitting::fitting_cyno_portal::is_ship_cyno_active(&ctx, ship_id),
            "Ship cyno state must be active"
        );

        // 4. Covert Cyno (28646) on Prowler Blockade Runner (12735) in cyno-jammed system succeeds!
        let covert_char = 981_002;
        let prowler_ship = 991_002;
        let mut covert_sess = seed_pilot(&ctx, covert_char, prowler_ship, 12735, NULL_SYS, false);
        put_in_fleet(&ctx, 77_001, char_id, &[(char_id, true), (covert_char, true)]);
        give_item(&ctx, 995_099, 33700, char_id, NULL_SYS as i64, InventoryFlag::None, 1); // Mobile Cyno Inhibitor
        set_skill(&ctx, covert_char, 21603, 5);
        spawn_ship_in_space(&ctx, covert_char, prowler_ship, 12735, NULL_SYS, DVec3::new(35_000.0, 0.0, 0.0)).await;
        let covert_mod = 995_010;
        give_item(&ctx, covert_mod, 28646, covert_char, prowler_ship, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, 995_011, 16273, covert_char, prowler_ship, InventoryFlag::Cargo, 500);
        let cov_res = ctx
            .dispatch_rpc(&mut covert_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(covert_mod)], &kw, None)
            .await
            .expect("Covert Cyno on Prowler must bypass cyno inhibitor in Null-sec");
        let cov_beacon_id = cov_res.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap_or(0);
        assert!(cov_beacon_id > 0, "Covert cyno must spawn covert fleet beacon");
    }

    #[tokio::test]
    async fn test_jump_drive_fatigue_and_titan_jump_portal_bridge() {
        let ctx = build_ctx();
        let kw = HashMap::new();
        let origin_sys = NULL_SYS; // 30004759
        let dest_sys = DEST_NULL_SYS; // 30004760

        let cyno_char = 981_101;
        let cyno_ship = 991_101;
        let carrier_char = 981_102;
        let carrier_ship = 991_102;
        let titan_char = 981_103;
        let titan_ship = 991_103;
        let subcap_char = 981_104;
        let subcap_ship = 991_104;

        let mut cyno_sess = seed_pilot(&ctx, cyno_char, cyno_ship, 11957, dest_sys, false);
        let mut carrier_sess = seed_pilot(&ctx, carrier_char, carrier_ship, 23757, origin_sys, false);
        let mut titan_sess = seed_pilot(&ctx, titan_char, titan_ship, 11567, origin_sys, false);
        let mut subcap_sess = seed_pilot(&ctx, subcap_char, subcap_ship, 626, origin_sys, false);

        put_in_fleet(
            &ctx,
            77_002,
            titan_char,
            &[
                (cyno_char, true),
                (carrier_char, true),
                (titan_char, true),
                (subcap_char, true),
            ],
        );

        for &(cid, sk) in &[
            (cyno_char, 21603),
            (carrier_char, 21610),
            (carrier_char, 21611),
            (titan_char, 21610),
            (titan_char, 21611),
            (titan_char, 24562),
        ] {
            set_skill(&ctx, cid, sk, 5);
        }

        spawn_ship_in_space(&ctx, cyno_char, cyno_ship, 11957, dest_sys, DVec3::new(15_000.0, 0.0, 0.0)).await;
        spawn_ship_in_space(&ctx, carrier_char, carrier_ship, 23757, origin_sys, DVec3::new(10_000.0, 0.0, 0.0)).await;
        spawn_ship_in_space(&ctx, titan_char, titan_ship, 11567, origin_sys, DVec3::new(20_000.0, 0.0, 0.0)).await;
        spawn_ship_in_space(&ctx, subcap_char, subcap_ship, 626, origin_sys, DVec3::new(21_000.0, 0.0, 0.0)).await;

        // Light Cyno in dest_sys
        give_item(&ctx, 995_101, 21096, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, 995_102, 16273, cyno_char, cyno_ship, InventoryFlag::Cargo, 1_000);
        let cyno_res = ctx
            .dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(995_101)], &kw, None)
            .await
            .expect("Light Cyno in dest_sys");
        let beacon_id = cyno_res.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap_or(0);

        // 1. Archon Carrier jumps via CmdJumpToFleetModuleBeacon -> consumes Helium Isotopes (16274) and accumulates Jump Fatigue!
        give_item(&ctx, 995_103, 16274, carrier_char, carrier_ship, InventoryFlag::SpecializedFuelBay, 10_000);
        let fatigue_before = ctx.db.with_snapshot(|s| s.characters.get(&carrier_char).map(|c| c.jump_fatigue_until_unix).unwrap_or(0));
        assert_eq!(fatigue_before, 0);
        ctx.dispatch_rpc(
            &mut carrier_sess,
            "beyonce",
            None,
            "CmdJumpToFleetModuleBeacon",
            &[PyRep::Integer(cyno_char), PyRep::Integer(dest_sys as i64), PyRep::Integer(beacon_id)],
            &kw,
            None,
        )
        .await
        .expect("Carrier CmdJumpToFleetModuleBeacon");
        assert_eq!(carrier_sess.solar_system_id, Some(dest_sys), "Carrier must arrive in dest_sys");
        let fuel_left = ctx.db.with_snapshot(|s| s.items.get(&995_103).unwrap().quantity);
        assert!(fuel_left < 10_000, "Jump Drive must consume isotopes ({fuel_left} < 10000)");
        let (fat_until, cd_until, fat_min) = ctx.db.with_snapshot(|s| {
            let ch = s.characters.get(&carrier_char).unwrap();
            (ch.jump_fatigue_until_unix, ch.jump_cooldown_until_unix, ch.jump_fatigue_minutes)
        });
        let now_u = ctx.now_unix_secs();
        assert!(fat_min > 0.0 && fat_until > now_u + 300, "Jump fatigue timer must be set (> 5m, got {fat_min}m)");
        assert!(cd_until > now_u, "Jump reactivation (blue) timer must be set");

        // 2. Avatar Titan opens Jump Portal via CmdBridgeToFleetModuleBeacon & Subcap jumps through via CmdJumpThroughFleet!
        give_item(&ctx, 995_105, 23953, titan_char, titan_ship, InventoryFlag::HiSlot0, 1); // Jump Portal Generator I
        give_item(&ctx, 995_106, 16275, titan_char, titan_ship, InventoryFlag::SpecializedFuelBay, 5_000);
        give_item(&ctx, 995_107, 16274, titan_char, titan_ship, InventoryFlag::SpecializedFuelBay, 10_000);
        ctx.dispatch_rpc(
            &mut titan_sess,
            "beyonce",
            None,
            "CmdBridgeToFleetModuleBeacon",
            &[PyRep::Integer(cyno_char), PyRep::Integer(dest_sys as i64), PyRep::Integer(beacon_id)],
            &kw,
            None,
        )
        .await
        .expect("Titan CmdBridgeToFleetModuleBeacon");

        ctx.dispatch_rpc(
            &mut subcap_sess,
            "beyonce",
            None,
            "CmdJumpThroughFleet",
            &[PyRep::Integer(titan_char), PyRep::Integer(titan_ship)],
            &kw,
            None,
        )
        .await
        .expect("Fleetmate CmdJumpThroughFleet through Titan bridge");
        assert_eq!(subcap_sess.solar_system_id, Some(dest_sys), "Subcap must arrive in dest_sys via Titan Jump Portal");
        assert!(
            ctx.db.with_snapshot(|s| s.characters.get(&subcap_char).is_some_and(|c| c.jump_fatigue_until_unix > now_u)),
            "Jumping through Titan bridge must accumulate jump fatigue on passenger"
        );
    }

    #[tokio::test]
    async fn test_blockade_runner_scan_immunity_and_frigate_escape_bay() {
        let ctx = build_ctx();
        let kw = HashMap::new();
        let sys_id = NULL_SYS;

        let scanner_char = 981_201;
        let scanner_ship = 991_201;
        let br_char = 981_202;
        let br_ship = 991_202;
        let rifter_char = 981_203;
        let rifter_ship = 991_203;

        let mut scanner_sess = seed_pilot(&ctx, scanner_char, scanner_ship, 626, sys_id, false);
        let _br_sess = seed_pilot(&ctx, br_char, br_ship, 12735, sys_id, false);
        let _rifter_sess = seed_pilot(&ctx, rifter_char, rifter_ship, 587, sys_id, false);

        spawn_ship_in_space(&ctx, scanner_char, scanner_ship, 626, sys_id, DVec3::new(0.0, 0.0, 0.0)).await;
        spawn_ship_in_space(&ctx, br_char, br_ship, 12735, sys_id, DVec3::new(5_000.0, 0.0, 0.0)).await;
        spawn_ship_in_space(&ctx, rifter_char, rifter_ship, 587, sys_id, DVec3::new(6_000.0, 0.0, 0.0)).await;

        let cargo_scanner = 995_201;
        give_item(&ctx, cargo_scanner, 442, scanner_char, scanner_ship, InventoryFlag::MedSlot0, 1);
        give_item(&ctx, 995_202, 34, br_char, br_ship, InventoryFlag::Cargo, 5_000);
        give_item(&ctx, 995_203, 34, rifter_char, rifter_ship, InventoryFlag::Cargo, 1_234);

        // 1. Cargo Scanner on Blockade Runner (Prowler 12735) -> immune!
        let br_scan = ctx
            .dispatch_rpc(
                &mut scanner_sess,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(cargo_scanner), PyRep::Integer(br_ship)],
                &kw,
                None,
            )
            .await
            .expect("Cargo Scan Prowler");
        assert_eq!(br_scan.dict_get("cargoScanImmune").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(br_scan.dict_get("totalScannedQuantity").and_then(|v| v.as_i64()), Some(0));

        // Cargo Scanner on normal Rifter (587) -> reveals 1,234 Tritanium!
        ctx.db.with_mut_snapshot(|s| {
            s.module_active_states.remove(&cargo_scanner);
        });
        let normal_scan = ctx
            .dispatch_rpc(
                &mut scanner_sess,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(cargo_scanner), PyRep::Integer(rifter_ship)],
                &kw,
                None,
            )
            .await
            .expect("Cargo Scan Rifter");
        assert_eq!(normal_scan.dict_get("totalScannedQuantity").and_then(|v| v.as_i64()), Some(1_234));

        // 2. Frigate Escape Bay on Megathron (641): assembled Rifter in flag 179 ejects pilot into Rifter on Battleship destruction!
        let bs_char = 981_204;
        let bs_ship = 991_204;
        let feb_frigate = 995_210;
        let _bs_sess = seed_pilot(&ctx, bs_char, bs_ship, 641, sys_id, false);
        spawn_ship_in_space(&ctx, bs_char, bs_ship, 641, sys_id, DVec3::new(10_000.0, 0.0, 0.0)).await;
        give_item(&ctx, feb_frigate, 587, bs_char, bs_ship, InventoryFlag::from_u16(179), 1);
        give_item(&ctx, 995_211, 34, bs_char, bs_ship, InventoryFlag::Cargo, 500);

        let death_out = execute_death_pipeline(&ctx, sys_id, bs_char, bs_ship, scanner_char, DVec3::new(10_000.0, 0.0, 0.0), 42)
            .await
            .expect("Execute Megathron death pipeline");
        assert_eq!(death_out.ejected_capsule_id, Some(feb_frigate), "Pilot must eject into Frigate Escape Bay ship ({feb_frigate})");
        let feb_item = ctx.db.with_snapshot(|s| s.items.get(&feb_frigate).cloned()).expect("FEB frigate must survive");
        assert_eq!(feb_item.location_id, sys_id as i64, "FEB frigate must be in space in solar system");
        assert!(ctx.systems.get_or_spawn(sys_id).get_ball_snapshot(feb_frigate).await.unwrap().is_some(), "FEB frigate ball must be spawned in space");
    }

    #[tokio::test]
    async fn test_combat_insurance_void_and_expiry_and_pochven_loot_drop() {
        let ctx = build_ctx();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let jita_sys: u32 = 30_000_142;
        let pochven_sys: u32 = 30_000_021; // Kuharah

        let char_id = 981_301;
        let ship_id = 991_301;
        let killer_char = 981_302;
        let killer_ship = 991_302;
        let mut sess = seed_pilot(&ctx, char_id, ship_id, 587, jita_sys, true);
        let _killer_sess = seed_pilot(&ctx, killer_char, killer_ship, 626, pochven_sys, false);
        sess.station_id = Some(60_003_760);

        // 1. Insurance contract via insuranceSvc.InsureShip, voiding on repackage, and 12-week expiry fallback (40%)
        let base_price = ctx.sde.types.get(&587).map(|t| t.base_price).filter(|&p| p > 0.0).unwrap_or(230_000.0);
        let cost_isk = (base_price * 0.30).round();
        let _ins_res = ctx
            .dispatch_rpc(
                &mut sess,
                "insuranceSvc",
                None,
                "InsureShip",
                &[PyRep::Integer(ship_id), PyRep::Real(cost_isk), PyRep::Bool(false), PyRep::Bool(true)],
                &kw,
                None,
            )
            .await
            .expect("InsureShip RPC");
        assert!(with_combat_runtime(&ctx, |rt| rt.insurance_contracts.contains_key(&ship_id)));

        ctx.dispatch_rpc(
            &mut sess,
            "insuranceSvc",
            None,
            "UnInsureShip",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect("UnInsureShip voids active insurance contract");
        assert!(
            !with_combat_runtime(&ctx, |rt| rt.insurance_contracts.contains_key(&ship_id)),
            "Voiding/repackaging ship must remove active insurance contract"
        );

        // Re-assemble, re-insure, advance clock > 12 weeks -> expired contract pays 40% fallback + EVE-mail!
        ctx.db.with_mut_snapshot(|s| {
            if let Some(it) = s.items.get_mut(&ship_id) {
                it.singleton = true;
                it.location_id = pochven_sys as i64;
            }
            if let Some(ch) = s.characters.get_mut(&char_id) {
                ch.station_id = None;
                ch.solar_system_id = pochven_sys;
            }
        });
        sess.station_id = Some(60_003_760);
        ctx.dispatch_rpc(
            &mut sess,
            "insuranceSvc",
            None,
            "InsureShip",
            &[PyRep::Integer(ship_id), PyRep::Real(cost_isk), PyRep::Bool(false), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect("Re-InsureShip");
        sess.station_id = None;
        sess.solar_system_id = Some(pochven_sys);
        ctx.advance_time_seconds(12 * 7 * 86_400 + 30);

        // 2. Seed 20 cargo items on ship in Pochven (30000021) -> destroy ship -> ~60% loot drop (12/20) + expired 40% insurance payout + EVE-mail!
        for i in 0..20_i64 {
            give_item(&ctx, 995_300 + i, 34, char_id, ship_id, InventoryFlag::Cargo, 100);
        }
        let mails_before = ctx.db.with_snapshot(|s| s.mails.len());
        let poch_death = execute_death_pipeline(&ctx, pochven_sys, char_id, ship_id, killer_char, DVec3::ZERO, 42)
            .await
            .expect("Execute Pochven death pipeline");
        assert!(
            poch_death.loot_dropped_item_ids.len() > 10,
            "Pochven PvP loot drop (~60%) must drop more than 50% of 20 stacks (got {})",
            poch_death.loot_dropped_item_ids.len()
        );
        assert!(
            ctx.db.with_snapshot(|s| s.mails.len() > mails_before),
            "Ship loss must deliver Insurance Payout EVE-mail"
        );
    }
}

pub(crate) mod fighter_heavy_long_range_bomb {
    // Dedicated end-to-end integration tests for `feat:drones.*` fighter parity rows (Part 2).

    use eve_db::{InventoryFlag, IskCents};
    use eve_destiny::{Ball, DamageVector, DVec3, MovementMode};
    use eve_machonet::{service_roles, SessionState};
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services::combat_crimewatch::{
        apply_damage_to_fighter_squadron, dispatch_combat_rpc, with_combat_runtime,
    };
    use std::collections::HashMap;

    const ARCHON_CARRIER: u32 = 23757;
    const AMETAT_HEAVY_FIGHTER: u32 = 40362;
    const EQUITE_LIGHT_FIGHTER: u32 = 40358;
    const SIREN_SUPPORT_FIGHTER: u32 = 40559;
    const DROMI_SUPPORT_FIGHTER: u32 = 40561;
    const CENOBITE_SUPPORT_FIGHTER: u32 = 40560;
    const SCARAB_SUPPORT_FIGHTER: u32 = 40562;
    const SYS_ID: u32 = 30_004_759;

    fn make_sess(char_id: i64, ship_id: i64) -> SessionState {
        let mut s = SessionState::new(6001);
        s.user_id = Some(9001);
        s.character_id = Some(char_id);
        s.corporation_id = Some(1_000_009);
        s.solar_system_id = Some(SYS_ID);
        s.constellation_id = Some(20_000_020);
        s.region_id = Some(10_000_002);
        s.ship_id = Some(ship_id);
        s.role = service_roles::ROLE_PLAYER | service_roles::ROLE_LOGIN;
        s
    }

    async fn setup_carrier(ctx: &ServerContext, char_id: i64, ship_id: i64) {
        crate::nav_tactical_modules::give_item(ctx, ship_id, ARCHON_CARRIER, char_id, SYS_ID as i64, InventoryFlag::None, 1);
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&char_id) {
                ch.solar_system_id = SYS_ID;
                ch.active_ship_id = ship_id;
                ch.balance = IskCents::from_isk(500_000_000);
            }
        });
        let sys = ctx.systems.get_or_spawn(SYS_ID);
        let mut cs = ctx.build_type_combat_state(ARCHON_CARRIER);
        cs.capacitor = 100_000.0;
        cs.max_capacitor = 100_000.0;
        let _ = sys
            .spawn_ball_with_combat(
                Ball {
                    id: ship_id,
                    type_id: ARCHON_CARRIER,
                    position: DVec3::ZERO,
                    velocity: DVec3::ZERO,
                    max_velocity: 100.0,
                    inertia: 1.0,
                    mass: 100_000_000.0,
                    radius: 400.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
            )
            .await;
    }

    async fn setup_target_cruiser(ctx: &ServerContext, char_id: i64, ship_id: i64, pos: DVec3) {
        crate::nav_tactical_modules::give_item(ctx, ship_id, 626, char_id, SYS_ID as i64, InventoryFlag::None, 1);
        let sys = ctx.systems.get_or_spawn(SYS_ID);
        let mut cs = ctx.build_type_combat_state(626);
        if cs.max_shield_hp <= 0.0 {
            cs.max_shield_hp = 5_000.0;
            cs.shield_hp = 5_000.0;
            cs.max_armor_hp = 5_000.0;
            cs.armor_hp = 5_000.0;
            cs.max_structure_hp = 5_000.0;
            cs.structure_hp = 5_000.0;
        }
        cs.capacitor = 5_000.0;
        cs.max_capacitor = 5_000.0;
        let _ = sys
            .spawn_ball_with_combat(
                Ball {
                    id: ship_id,
                    type_id: 626,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 400.0,
                    inertia: 1.0,
                    mass: 10_000_000.0,
                    radius: 150.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
            )
            .await;
    }

    #[tokio::test]
    async fn test_fighter_heavy_long_range_bomb() {
        // parity: feat:drones.fighter-heavy-long-range-bomb
        eve_testkit::attest(&["feat:drones.fighter-heavy-long-range-bomb"]);

        let ctx = crate::modules_advanced_tactical::build_ctx();
        let kw = HashMap::new();
        let char_id = 90_000_001;
        let carrier_id = 104_000_001;
        let victim_near = 104_000_010;
        let victim_far = 104_000_011;

        setup_carrier(&ctx, char_id, carrier_id).await;
        setup_target_cruiser(&ctx, 90_000_002, victim_near, DVec3::new(26_000.0, 0.0, 0.0)).await;
        setup_target_cruiser(&ctx, 90_000_002, victim_far, DVec3::new(55_000.0, 0.0, 0.0)).await;
        let mut sess = make_sess(char_id, carrier_id);

        let item_id = 410_001;
        crate::nav_tactical_modules::give_item(&ctx, item_id, AMETAT_HEAVY_FIGHTER, char_id, carrier_id, InventoryFlag::FighterBay, 3);
        dispatch_combat_rpc(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(item_id)], &kw)
            .await
            .expect("LoadFightersToTube");
        let launched = dispatch_combat_rpc(
            &ctx,
            &mut sess,
            "fighterMgr",
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(0)])],
            &kw,
        )
        .await
        .expect("LaunchFightersFromTubes");
        let PyRep::List(balls) = launched else { panic!("Expected List"); };
        let fighter_ball = balls[0].as_i64().unwrap();

        let sys = ctx.systems.get_or_spawn(SYS_ID);
        let near_before = sys.get_combat_state(victim_near).await.unwrap().unwrap().shield_hp;
        let far_before = sys.get_combat_state(victim_far).await.unwrap().unwrap().shield_hp;

        let pt = PyRep::Tuple(vec![PyRep::Real(25_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]);
        let res = dispatch_combat_rpc(
            &ctx,
            &mut sess,
            "fighterMgr",
            "CmdActivateAbilitySlots",
            &[PyRep::Integer(fighter_ball), PyRep::Integer(2), pt],
            &kw,
        )
        .await
        .expect("Activate Heavy Fighter Bomb");
        assert_eq!(res.dict_get("success"), Some(&PyRep::Bool(true)));

        let near_after = sys.get_combat_state(victim_near).await.unwrap().unwrap().shield_hp;
        let far_after = sys.get_combat_state(victim_far).await.unwrap().unwrap().shield_hp;
        assert!(near_after < near_before, "Ship within bomb blast radius must take AoE damage");
        assert_eq!(far_after, far_before, "Ship 30km away must be outside bomb blast radius");

        let charges = res.dict_get("chargesRemaining").and_then(|v| v.as_i64()).unwrap_or(0);
        assert!(charges < 3, "Launching Bomb must consume charge ({charges} < 3)");
    }

    #[tokio::test]
    async fn test_fighter_refuel_rearm_repair() {
        // parity: feat:drones.fighter-refuel-rearm-repair
        eve_testkit::attest(&["feat:drones.fighter-refuel-rearm-repair"]);

        let ctx = crate::modules_advanced_tactical::build_ctx();
        let kw = HashMap::new();
        let char_id = 90_000_001;
        let carrier_id = 104_000_001;
        setup_carrier(&ctx, char_id, carrier_id).await;
        let mut sess = make_sess(char_id, carrier_id);

        let tube_item = 410_010;
        let bay_spare = 410_011;
        crate::nav_tactical_modules::give_item(&ctx, tube_item, EQUITE_LIGHT_FIGHTER, char_id, carrier_id, InventoryFlag::FighterBay, 9);
        crate::nav_tactical_modules::give_item(&ctx, bay_spare, EQUITE_LIGHT_FIGHTER, char_id, carrier_id, InventoryFlag::FighterBay, 6);

        dispatch_combat_rpc(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(tube_item)], &kw)
            .await
            .expect("LoadFightersToTube");
        let launched = dispatch_combat_rpc(
            &ctx,
            &mut sess,
            "fighterMgr",
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(0)])],
            &kw,
        )
        .await
        .expect("LaunchFightersFromTubes");
        let PyRep::List(balls) = launched else { panic!("Expected List"); };
        let fighter_ball = balls[0].as_i64().unwrap();

        // Destroy 2 fighter units via damage
        let (lost, remaining, wiped) = apply_damage_to_fighter_squadron(&ctx, SYS_ID, fighter_ball, DamageVector::new(2_500.0, 0.0, 0.0, 0.0)).await;
        assert!(!wiped && lost >= 1 && remaining < 9, "Damaged squadron must lose fighter units ({lost} lost, {remaining} remaining)");

        // Recall to tube -> triggers rearm/repair/replenish timer
        dispatch_combat_rpc(
            &ctx,
            &mut sess,
            "fighterMgr",
            "RecallFightersToTubes",
            &[PyRep::List(vec![PyRep::Integer(fighter_ball)])],
            &kw,
        )
        .await
        .expect("RecallFightersToTubes");

        let is_rearming = with_combat_runtime(&ctx, |rt| {
            rt.fighter_tube_rearm_until
                .get(&(carrier_id, 0))
                .map(|&exp| exp > ctx.now_unix())
                .unwrap_or(false)
        });
        assert!(is_rearming, "Recalled squadron must enter tube rearm/repair timer");
        assert!(
            dispatch_combat_rpc(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw)
                .await
                .is_err(),
            "Relaunching tube while rearm/repair timer is active must be rejected"
        );

        // Advance past rearm timer -> relaunch succeeds!
        ctx.advance_time_seconds(60);
        assert!(
            dispatch_combat_rpc(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw)
                .await
                .is_ok(),
            "Relaunch after rearm/repair completion must succeed"
        );
    }

    #[tokio::test]
    async fn test_fighter_support_abilities_and_squadron_scaling() {
        // parity: feat:drones.fighter-support-cenotaph-scarab
        // parity: feat:drones.support-fighter-abilities
        eve_testkit::attest(&[
            "feat:drones.fighter-support-cenotaph-scarab",
            "feat:drones.support-fighter-abilities",
        ]);

        let ctx = crate::modules_advanced_tactical::build_ctx();
        let kw = HashMap::new();
        let char_id = 90_000_001;
        let carrier_id = 104_000_001;
        let target_id = 104_000_020;

        setup_carrier(&ctx, char_id, carrier_id).await;
        setup_target_cruiser(&ctx, 90_000_002, target_id, DVec3::new(5_000.0, 0.0, 0.0)).await;
        let mut sess = make_sess(char_id, carrier_id);

        // Archon has class limit 2 for Support Fighters: test Siren + Cenobite, then recall & test Dromi + Scarab!
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(target_id)], &kw, None)
            .await
            .expect("AddTarget");
        let sys_t = ctx.systems.get_or_spawn(SYS_ID);
        for _ in 0..25 {
            ctx.advance_time_seconds(1);
            let _ = sys_t.try_step_full_tick(1.0);
        }

        for &(pair, base_id) in &[
            ([SIREN_SUPPORT_FIGHTER, CENOBITE_SUPPORT_FIGHTER], 410_020i64),
            ([DROMI_SUPPORT_FIGHTER, SCARAB_SUPPORT_FIGHTER], 410_030i64),
        ] {
            for (idx, tid) in pair.into_iter().enumerate() {
                let iid = base_id + idx as i64;
                crate::nav_tactical_modules::give_item(&ctx, iid, tid, char_id, carrier_id, InventoryFlag::FighterBay, 3);
                dispatch_combat_rpc(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(idx as i64), PyRep::Integer(iid)], &kw)
                    .await
                    .expect("Load support fighter");
            }
            let launched = dispatch_combat_rpc(
                &ctx,
                &mut sess,
                "fighterMgr",
                "LaunchFightersFromTubes",
                &[PyRep::List(vec![PyRep::Integer(0), PyRep::Integer(1)])],
                &kw,
            )
            .await
            .expect("Launch support squadrons");
            let PyRep::List(balls) = launched else { panic!("Expected List"); };
            assert_eq!(balls.len(), 2);

            let sys = ctx.systems.get_or_spawn(SYS_ID);

            for (idx, b) in balls.iter().enumerate() {
                let bid = b.as_i64().unwrap();
                let slot = if pair[idx] == CENOBITE_SUPPORT_FIGHTER { 0 } else { 2 };
                let res = dispatch_combat_rpc(
                    &ctx,
                    &mut sess,
                    "fighterMgr",
                    "CmdActivateAbilitySlots",
                    &[PyRep::Integer(bid), PyRep::Integer(slot), PyRep::Integer(target_id)],
                    &kw,
                )
                .await
                .expect("Activate support fighter EWAR ability");
                assert_eq!(res.dict_get("success"), Some(&PyRep::Bool(true)));
            }

            if pair[0] == SIREN_SUPPORT_FIGHTER {
                let scramble = sys.get_combat_state(target_id).await.unwrap().unwrap().warp_scramble_strength;
                assert!(scramble >= 3, "Siren support fighter squadron (3 units) must apply >= 3 warp disruption points (got {scramble})");
                let siren_ball = balls[0].as_i64().unwrap();
                let (lost, rem, _) = apply_damage_to_fighter_squadron(&ctx, SYS_ID, siren_ball, DamageVector::new(4_500.0, 0.0, 0.0, 0.0)).await;
                assert!(lost >= 1 && rem < 3, "Siren squadron must lose unit(s) under fire ({lost} lost, {rem} remaining)");
            }
            dispatch_combat_rpc(
                &ctx,
                &mut sess,
                "fighterMgr",
                "RecallFightersToTubes",
                &[PyRep::List(balls)],
                &kw,
            )
            .await
            .expect("Recall support squadrons");
            ctx.advance_time_seconds(60);
        }
    }

    #[tokio::test]
    async fn test_light_fighter_space_superiority() {
        // parity: feat:drones.light-fighter-space-superiority
        eve_testkit::attest(&["feat:drones.light-fighter-space-superiority"]);

        let ctx = crate::modules_advanced_tactical::build_ctx();
        let kw = HashMap::new();
        let char_id = 90_000_001;
        let carrier_id = 104_000_001;
        let enemy_carrier_id = 104_000_099;

        setup_carrier(&ctx, char_id, carrier_id).await;
        setup_target_cruiser(&ctx, 90_000_002, enemy_carrier_id, DVec3::new(15_000.0, 0.0, 0.0)).await;
        let mut sess = make_sess(char_id, carrier_id);

        let equite_id = 410_050;
        crate::nav_tactical_modules::give_item(&ctx, equite_id, EQUITE_LIGHT_FIGHTER, char_id, carrier_id, InventoryFlag::FighterBay, 9);
        dispatch_combat_rpc(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(equite_id)], &kw)
            .await
            .expect("Load Equite");
        let launched = dispatch_combat_rpc(
            &ctx,
            &mut sess,
            "fighterMgr",
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(0)])],
            &kw,
        )
        .await
        .expect("Launch Equite");
        let PyRep::List(balls) = launched else { panic!("Expected List"); };
        let equite_ball = balls[0].as_i64().unwrap();

        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(enemy_carrier_id)], &kw, None)
            .await
            .expect("AddTarget");
        let sys_t = ctx.systems.get_or_spawn(SYS_ID);
        for _ in 0..25 {
            ctx.advance_time_seconds(1);
            let _ = sys_t.try_step_full_tick(1.0);
        }

        // Activate Evasive Maneuvers / MWD (slot 1) and Anti-Fighter Missile Volley (slot 2)
        let evasive = dispatch_combat_rpc(
            &ctx,
            &mut sess,
            "fighterMgr",
            "CmdActivateAbilitySlots",
            &[PyRep::Integer(equite_ball), PyRep::Integer(1), PyRep::None],
            &kw,
        )
        .await
        .expect("Evasive Maneuvers");
        assert_eq!(evasive.dict_get("success"), Some(&PyRep::Bool(true)));

        let volley = dispatch_combat_rpc(
            &ctx,
            &mut sess,
            "fighterMgr",
            "CmdActivateAbilitySlots",
            &[PyRep::Integer(equite_ball), PyRep::Integer(2), PyRep::Integer(enemy_carrier_id)],
            &kw,
        )
        .await
        .expect("Anti-Fighter Missile Volley");
        assert_eq!(volley.dict_get("success"), Some(&PyRep::Bool(true)));

        // Immediate re-activation of Anti-Fighter Missile Volley must be rejected while on cooldown!
        assert!(
            dispatch_combat_rpc(
                &ctx,
                &mut sess,
                "fighterMgr",
                "CmdActivateAbilitySlots",
                &[PyRep::Integer(equite_ball), PyRep::Integer(2), PyRep::Integer(enemy_carrier_id)],
                &kw,
            )
            .await
            .is_err(),
            "Re-activating Equite anti-fighter missile volley while on cooldown must be rejected"
        );
    }
}
