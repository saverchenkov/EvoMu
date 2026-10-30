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
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
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
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(with_combat_runtime(&ctx, |rt| !rt.npc_entities.values().any(|n| n.solar_system_id == lowsec_sys_id && n.archetype == "CloneSoldier")), "Clone Soldier must not respawn during 1200s cooldown");
    ctx.npc_spawns.write().unwrap().get_mut(&lowsec_sys_id).unwrap().clone_soldier_respawn_timers.insert(lowsec_sys_id, 0);
    with_mut_combat_runtime(&ctx, |rt| { rt.clone_soldier_cooldown_until.insert(lowsec_sys_id, 0); });
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
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
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
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
