//! Integration tests closing canonical plan tasks:
//! - `plan:DG-6` & `notify:OnChargeBeingLoadedToModule` (ammo loading/unloading, reload timers, auto-reload, LinkWeapons groups)
//! - `plan:ESI-3` (Universe, Status, Routes, Killmails, Dogma, Insurance)
//! - `plan:ESI-4` (Character, Clones, Skills, Location, Fittings, Assets, Bookmarks, Wallet, Loyalty, Calendar, Contacts, Mail)
//! - `plan:SO-2` (Fleet hierarchy, MOTD, invites, roles, FinishMove, broadcasts, warp, boss handover, lifecycle)

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, KillmailRecord, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

const JITA_STATION_ID: i64 = 60_003_760;

#[tokio::test]
async fn test_plan_ch2_skill_queue_and_brain_injection() {
    // parity: plan:CH-2
    eve_testkit::attest(&["plan:CH-2"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(9201);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;

    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("SelectCharacterID");

    // 1. GetSkills, GetSkillQueueAndFreePoints, and SaveSkillQueue (replacing old queue with new ordered list)
    let skills_rep = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetSkills", &[], &kw, None).await.expect("GetSkills");
    assert!(matches!(skills_rep, PyRep::Rowset { .. }));

    let q_init = [PyRep::List(vec![PyRep::List(vec![PyRep::Integer(3386), PyRep::Integer(1)])])];
    ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "SaveSkillQueue", &q_init, &kw, None).await.expect("SaveSkillQueue initial");
    assert_eq!(ctx.db.with_snapshot(|s| s.skill_queues[&char_id].len()), 1);

    let q_args = [PyRep::List(vec![
        PyRep::List(vec![PyRep::Integer(3300), PyRep::Integer(5)]),
        PyRep::List(vec![PyRep::Integer(3386), PyRep::Integer(4)]),
        PyRep::List(vec![PyRep::Integer(3392), PyRep::Integer(2)]),
    ])];
    let save_rep = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "SaveSkillQueue", &q_args, &kw, None).await.expect("SaveSkillQueue replace");
    assert_eq!(save_rep.dict_get("typeID"), Some(&PyRep::Integer(3392)));
    assert_eq!(save_rep.dict_get("targetLevel"), Some(&PyRep::Integer(2)));

    let (queue_len, expected_secs) = ctx.db.with_snapshot(|s| {
        let q = &s.skill_queues[&char_id];
        (q.len(), q.iter().map(|e| e.duration_seconds).sum::<f64>())
    });
    assert_eq!(queue_len, 3, "SaveSkillQueue must replace the queue with the 3 new entries");

    // 2. GetSkillQueue, GetEndOfTraining (active, partial progress, and paused via AbortTraining)
    let get_q = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetSkillQueue", &[], &kw, None).await.expect("GetSkillQueue");
    let PyRep::Rowset { rows, .. } = get_q else { panic!("expected Rowset from GetSkillQueue") };
    assert_eq!(rows.len(), 3, "GetSkillQueue must return 3 rows");
    assert_eq!(rows[0][0], PyRep::Integer(3300));
    assert_eq!(rows[1][0], PyRep::Integer(3386));
    assert_eq!(rows[2][0], PyRep::Integer(3392));

    let end_train = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetEndOfTraining", &[], &kw, None).await.expect("GetEndOfTraining");
    let PyRep::Integer(end_ft) = end_train else { panic!("expected Integer from GetEndOfTraining") };
    let expected_end_ft = ctx.now_blueos_filetime_ticks() + (expected_secs * 10_000_000.0) as i64;
    assert_eq!(end_ft, expected_end_ft, "GetEndOfTraining must match queue total duration");

    ctx.advance_secs(120);
    let end_after_advance = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetEndOfTraining", &[], &kw, None).await.expect("GetEndOfTraining after advance");
    let PyRep::Integer(end_ft_2) = end_after_advance else { panic!("expected Integer from GetEndOfTraining") };
    assert!((end_ft_2 - expected_end_ft).abs() <= 20_000_000, "GetEndOfTraining must account for head progress");

    ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "AbortTraining", &[], &kw, None).await.expect("AbortTraining");
    let paused_end = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetEndOfTraining", &[], &kw, None).await.expect("GetEndOfTraining paused");
    assert_eq!(paused_end, PyRep::None, "Paused skill queue must return None from GetEndOfTraining");

    // 3. Post-Ascension InjectSkillIntoBrain succeeds without prerequisites while SaveSkillQueue enforces prerequisites
    let book_item_id = 991_000_901;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().remove(&3386);
        snap.items.insert(
            book_item_id,
            InventoryItemRecord {
                item_id: book_item_id, type_id: 3410, owner_id: char_id, location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar, quantity: 1, singleton: false, version: 1, ..Default::default()
            },
        );
    });
    let inject_rep = ctx
        .dispatch_rpc(&mut sess, "skillMgr2", None, "InjectSkillIntoBrain", &[PyRep::Integer(book_item_id)], &kw, None)
        .await
        .expect("InjectSkillIntoBrain succeeds without prerequisites trained");
    assert_eq!(inject_rep.dict_get("typeID"), Some(&PyRep::Integer(3410)));
    assert_eq!(inject_rep.dict_get("injected"), Some(&PyRep::Bool(true)));

    let (has_book, has_skill) = ctx.db.with_snapshot(|snap| {
        (snap.items.contains_key(&book_item_id), snap.skills[&char_id].contains_key(&3410))
    });
    assert!(!has_book, "Skillbook item must be consumed from inventory");
    assert!(has_skill, "Injected skill must be registered in character skills");

    let q_no_prereq = ctx
        .dispatch_rpc(&mut sess, "skillMgr2", None, "SaveSkillQueue", &[PyRep::List(vec![PyRep::List(vec![PyRep::Integer(3410), PyRep::Integer(1)])])], &kw, None)
        .await
        .expect_err("SaveSkillQueue without Mining IV must fail with SkillPrerequisiteNotMet");
    assert_eq!(q_no_prereq.code, "SkillPrerequisiteNotMet");

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().free_skill_points = 5_000.0;
    });
    let apply_no_prereq = ctx
        .dispatch_rpc(&mut sess, "skillMgr2", None, "ApplyFreeSkillPoints", &[PyRep::Integer(3410), PyRep::Integer(450)], &kw, None)
        .await
        .expect_err("ApplyFreeSkillPoints without prerequisites must fail with SkillPrerequisiteNotMet");
    assert_eq!(apply_no_prereq.code, "SkillPrerequisiteNotMet");

    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(3386, TrainedSkillRecord { type_id: 3386, level: 4, skill_points: 45_255.0 });
        sk.insert(3402, TrainedSkillRecord { type_id: 3402, level: 4, skill_points: 45_255.0 });
    });
    let q_with_prereq = ctx
        .dispatch_rpc(&mut sess, "skillMgr2", None, "SaveSkillQueue", &[PyRep::List(vec![PyRep::List(vec![PyRep::Integer(3410), PyRep::Integer(1)])])], &kw, None)
        .await
        .expect("Queueing Astrogeology with prerequisites trained must succeed");
    assert_eq!(q_with_prereq.dict_get("typeID"), Some(&PyRep::Integer(3410)));

    let hist = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetSkillHistory", &[], &kw, None).await.expect("GetSkillHistory");
    let PyRep::Rowset { rows: hist_rows, .. } = hist else { panic!("expected Rowset from GetSkillHistory") };
    assert!(hist_rows.iter().any(|r| r[2] == PyRep::Integer(3410)), "GetSkillHistory must include injected skill 3410");

    // 4. GetRespecInfo and neural remap RespecCharacter with cooldown
    let respec_info = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetRespecInfo", &[], &kw, None).await.expect("GetRespecInfo");
    assert!(respec_info.dict_get("bonusRemaps").is_some());

    let remap_args = [PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(19)];
    let remap_rep = ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &remap_args, &kw, None).await.expect("RespecCharacter timed");
    assert_eq!(remap_rep.dict_get("charisma"), Some(&PyRep::Integer(20)));
    let bonus_left = remap_rep.dict_get("bonusRemaps").and_then(|v| v.as_i64()).unwrap_or(0);
    let cooldown_until = remap_rep.dict_get("remapCooldownUntilUnix").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(cooldown_until > ctx.now_unix() as i64, "Timed remap must set yearly cooldown");

    for _ in 0..bonus_left {
        ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &remap_args, &kw, None).await.expect("RespecCharacter bonus");
    }
    let remap_err = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &remap_args, &kw, None)
        .await
        .expect_err("Remap inside cooldown with 0 bonus remaps must be rejected");
    assert_eq!(remap_err.code, "RemapFailed");
    assert!(remap_err.message.contains("cooldown"), "Error message must indicate yearly cooldown");

    // 5. ApplyFreeSkillPoints(typeID, points) and GetSkillQueueAndFreePoints
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().free_skill_points = 5_000.0;
    });
    let apply_rep = ctx
        .dispatch_rpc(&mut sess, "skillMgr2", None, "ApplyFreeSkillPoints", &[PyRep::Integer(3410), PyRep::Integer(450)], &kw, None)
        .await
        .expect("ApplyFreeSkillPoints");
    assert_eq!(apply_rep, PyRep::Real(450.0));
    let q_and_free = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetSkillQueueAndFreePoints", &[], &kw, None).await.expect("GetSkillQueueAndFreePoints");
    let PyRep::Tuple(qf_parts) = q_and_free else { panic!("expected Tuple from GetSkillQueueAndFreePoints") };
    assert_eq!(qf_parts[1], PyRep::Real(4_550.0));
    let sp_after = ctx.db.with_snapshot(|snap| snap.skills[&char_id][&3410].skill_points);
    assert_eq!(sp_after as i64, 450, "Allocated free SP must be credited to Astrogeology");
}

#[tokio::test]
async fn test_plan_dg6_ammo_and_weapon_grouping() {
    // parity: plan:DG-6
    // parity: notify:OnChargeBeingLoadedToModule
    eve_testkit::attest(&["plan:DG-6", "notify:OnChargeBeingLoadedToModule"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(9202);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;

    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("SelectCharacterID");

    let t1_id = 100_000_005i64;
    let t2_id = 100_000_006i64;
    let t3_id = 100_000_007i64;
    let mismatch_id = 100_000_008i64;
    let bad_ammo_id = 100_000_835i64;

    let ammo_id = 100_000_834i64;
    ctx.db.with_mut_snapshot(|snap| {
        for (id, tid, flag, qty, sing) in [
            (t2_id, 2889, InventoryFlag::HiSlot1, 1, true),
            (t3_id, 2889, InventoryFlag::HiSlot2, 1, true),
            (mismatch_id, 491, InventoryFlag::HiSlot3, 1, true),
            (bad_ammo_id, 193, InventoryFlag::Cargo, 100, false),
            (ammo_id, 183, InventoryFlag::Cargo, 400, false),
        ] {
            snap.items.insert(id, InventoryItemRecord {
                item_id: id, type_id: tid, owner_id: char_id, location_id: ship_id,
                flag, quantity: qty, singleton: sing, version: 1, ..Default::default()
            });
        }
        snap.skills.entry(char_id).or_default().insert(
            12201,
            TrainedSkillRecord { type_id: 12201, level: 5, skill_points: 256_000.0 },
        );
    });

    // 1. LinkWeapons rejects mismatched weapon types (InvalidWeaponGroup) and links matching weapons
    let bad_link = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "LinkWeapons", &[PyRep::Integer(ship_id), PyRep::Integer(t1_id), PyRep::Integer(mismatch_id)], &kw, None)
        .await
        .expect_err("LinkWeapons must reject different weapon types");
    assert_eq!(bad_link.code, "InvalidWeaponGroup");

    let link_rep = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "LinkWeapons", &[PyRep::Integer(ship_id), PyRep::Integer(t1_id), PyRep::Integer(t2_id)], &kw, None)
        .await
        .expect("LinkWeapons");
    assert_eq!(
        link_rep,
        PyRep::Dict(vec![(
            PyRep::Integer(t1_id),
            PyRep::List(vec![PyRep::Integer(t1_id), PyRep::Integer(t2_id)]),
        )]),
    );

    let bank_size = ctx.db.with_snapshot(|s| s.weapon_banks.get(&ship_id).and_then(|b| b.get(&t1_id)).map(Vec::len));
    assert_eq!(bank_size, Some(2), "Linked weapon bank must contain 2 modules");

    // 2. LoadAmmo rejects incompatible charge size (IncompatibleAmmo)
    let bad_load = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(t1_id)]), PyRep::List(vec![PyRep::Integer(bad_ammo_id)]), PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect_err("LoadAmmo must reject incompatible charge size");
    assert_eq!(bad_load.code, "IncompatibleAmmo");

    // 2. Undock into space, verify LoadAmmo emits OnChargeBeingLoadedToModule and enforces reloadTime (attr 1795)
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let (_, ship_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().expect("ship ball after undock");
    let npc_id = 700_099_001i64;
    sys.spawn_ball_with_combat(
        eve_destiny::Ball {
            id: npc_id, type_id: 587, position: ship_ball.position + glam::DVec3::new(2_500.0, 0.0, 0.0), velocity: glam::DVec3::ZERO,
            max_velocity: 300.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
        },
        ctx.build_type_combat_state(587),
    )
    .await
    .expect("spawn target ball");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(npc_id)], &kw, None).await.expect("AddTarget");
    for _ in 0..15 {
        sys.try_step_full_tick(1.0);
    }

    let _ = ctx.drain_notifications_for_char(char_id);
    let load_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "LoadAmmo",
            &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(t1_id)]), PyRep::List(vec![PyRep::Integer(ammo_id)]), PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect("LoadAmmo");

    let sde_reload_ms = ctx.sde.types[&2889].default_attributes[&1795];
    assert_eq!(sde_reload_ms, 10_000.0);
    assert_eq!(load_rep.dict_get("reloadTimeMs"), Some(&PyRep::Real(sde_reload_ms)), "Reload timer must match SDE attribute 1795");

    let load_notifs = ctx.drain_notifications_for_char(char_id);
    let charge_load_notif = load_notifs.iter().find(|n| n.event == "OnChargeBeingLoadedToModule").expect("OnChargeBeingLoadedToModule notification");
    assert_eq!(
        charge_load_notif.args,
        vec![PyRep::List(vec![PyRep::Integer(t1_id), PyRep::Integer(t2_id)]), PyRep::Integer(183), PyRep::Real(10_000.0)],
    );

    let (slot0_before, slot1_before) = ctx.db.with_snapshot(|snap| {
        let s0 = snap.items.values().find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.type_id == 183).map(|i| i.quantity).unwrap_or(0);
        let s1 = snap.items.values().find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot1 && i.type_id == 183).map(|i| i.quantity).unwrap_or(0);
        (s0, s1)
    });
    assert!(slot0_before > 0 && slot1_before == slot0_before, "Both grouped turrets must be loaded with ammo");

    // 3. Activating while reloading fails with ModuleIsReloading; advancing 11s allows firing and consumes 1 round per linked turret
    let reloading_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(t1_id), PyRep::String("projectileFired".into()), PyRep::Integer(npc_id)], &kw, None)
        .await
        .expect_err("Activate during 10s reload window must fail with ModuleIsReloading");
    assert_eq!(reloading_err.code, "ModuleIsReloading");

    ctx.advance_secs(11);
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(t1_id), PyRep::String("projectileFired".into()), PyRep::Integer(npc_id)], &kw, None)
        .await
        .expect("Activate grouped weapon bank after reload timer");

    let (slot0_after_fire, slot1_after_fire) = ctx.db.with_snapshot(|snap| {
        let s0 = snap.items.values().find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.type_id == 183).map(|i| i.quantity).unwrap_or(0);
        let s1 = snap.items.values().find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot1 && i.type_id == 183).map(|i| i.quantity).unwrap_or(0);
        (s0, s1)
    });
    assert_eq!(slot0_after_fire, slot0_before - 1, "Master turret in bank must consume 1 charge on cycle");
    assert_eq!(slot1_after_fire, slot1_before - 1, "Slave turret in bank must consume 1 charge on cycle");

    // 4. Charge depletion auto-reload: set loaded charges to 1 round each, step cycle to depletion, verify auto-reload from cargo + reload timer
    ctx.db.with_mut_snapshot(|snap| {
        for it in snap.items.values_mut() {
            if it.location_id == ship_id && matches!(it.flag, InventoryFlag::HiSlot0 | InventoryFlag::HiSlot1) && it.type_id == 183 {
                it.quantity = 1;
            }
        }
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    for _ in 0..6 {
        sys.try_step_full_tick(1.0);
    }
    let auto_reload_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(auto_reload_notifs.iter().any(|n| n.event == "OnChargeBeingLoadedToModule"), "Depleting loaded charges must emit OnChargeBeingLoadedToModule");
    let (slot0_reloaded, slot1_reloaded) = ctx.db.with_snapshot(|snap| {
        let s0 = snap.items.values().find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.type_id == 183).map(|i| i.quantity).unwrap_or(0);
        let s1 = snap.items.values().find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot1 && i.type_id == 183).map(|i| i.quantity).unwrap_or(0);
        (s0, s1)
    });
    assert_eq!((slot0_reloaded, slot1_reloaded), (80, 80), "Remaining 160 cargo rounds must auto-reload 80 into each grouped turret");
    let auto_reload_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(t1_id), PyRep::String("projectileFired".into()), PyRep::Integer(npc_id)], &kw, None)
        .await
        .expect_err("Activate during auto-reload timer must fail with ModuleIsReloading");
    assert_eq!(auto_reload_err.code, "ModuleIsReloading");
    ctx.advance_secs(11);

    // 5. UnloadAmmo moves remaining charges out of both turret slots back to cargo (80 + 80 = 160)
    let unload_rep = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "UnloadAmmo", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(t1_id)]), PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect("UnloadAmmo");
    assert_eq!(unload_rep, PyRep::None);

    let (cargo_qty_after_unload, still_in_slot) = ctx.db.with_snapshot(|snap| {
        let cq = snap.items.values().filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo && i.type_id == 183).map(|i| i.quantity as i64).sum::<i64>();
        let slot_charge = snap.items.values().any(|i| i.location_id == ship_id && matches!(i.flag, InventoryFlag::HiSlot0 | InventoryFlag::HiSlot1) && i.type_id == 183);
        (cq, slot_charge)
    });
    assert_eq!(cargo_qty_after_unload, 160, "Cargo ammo quantity after auto-reload and unload must be 160");
    assert!(!still_in_slot, "Charges must be removed from turret slots on unload");

    // 5. PeelAndLink, MergeModuleGroups, and UnlinkAllModules
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "PeelAndLink", &[PyRep::Integer(ship_id), PyRep::Integer(t3_id), PyRep::Integer(t1_id)], &kw, None)
        .await
        .expect("PeelAndLink");
    assert_eq!(ctx.db.with_snapshot(|s| s.weapon_banks[&ship_id][&t1_id].len()), 3);

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "UnlinkModule", &[PyRep::Integer(ship_id), PyRep::Integer(t3_id)], &kw, None)
        .await
        .expect("UnlinkModule");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "MergeModuleGroups", &[PyRep::Integer(ship_id), PyRep::Integer(t1_id), PyRep::Integer(t3_id)], &kw, None)
        .await
        .expect("MergeModuleGroups");
    assert_eq!(ctx.db.with_snapshot(|s| s.weapon_banks[&ship_id][&t1_id].len()), 3);

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "UnlinkAllModules", &[PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect("UnlinkAllModules");
    assert!(ctx.db.with_snapshot(|s| !s.weapon_banks.contains_key(&ship_id)));
}

#[tokio::test]
async fn test_plan_esi3_universe_status_routes_killmails() {
    // parity: plan:ESI-3
    eve_testkit::attest(&["plan:ESI-3"]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let kill_id = 990_001i64;
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(
            kill_id,
            KillmailRecord {
                kill_id,
                solar_system_id: 30_000_142,
                victim_character_id: 90_000_002,
                attacker_character_id: 90_000_001,
                ship_type_id: 587,
                total_value_isk: 500_000.0,
                killed_unix: now,
                ..Default::default()
            },
        );
    });
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let km_tok = crate::esi_routes::issue_token_with_scopes(&ctx, 90_000_001, vec!["esi-killmails.read_killmails.v1".into()]);

    let status_val = crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", "/v1/status/"), 200);
    assert!(status_val.get("server_version").is_some());

    let route_val = crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", "/v1/route/30000142/30000144/"), 200);
    assert!(route_val.as_array().expect("route array").contains(&json!(30000144)));

    let sys_val = crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", "/v4/universe/systems/30000142/"), 200);
    assert_eq!(sys_val.get("name").and_then(|v| v.as_str()), Some("Jita"));

    let type_val = crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", "/v3/universe/types/587/"), 200);
    assert_eq!(type_val.get("type_id").and_then(|v| v.as_i64()), Some(587));

    let st_val = crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", "/v2/universe/stations/60003760/"), 200);
    assert_eq!(st_val.get("station_id").and_then(|v| v.as_i64()), Some(60_003_760));

    let ins_val = crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", "/v1/insurance/prices/"), 200);
    assert!(ins_val.as_array().is_some());

    let dogma_val = crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", "/v1/dogma/attributes/"), 200);
    assert!(dogma_val.as_array().is_some());

    let eff_val = crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", "/v1/dogma/effects/"), 200);
    assert!(eff_val.as_array().is_some());

    let rec_kms = crate::esi_routes::validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/killmails/recent/").with_bearer_token(&km_tok),
        200,
    );
    let kill_hash = rec_kms
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["killmail_id"] == json!(kill_id))
        .unwrap()["killmail_hash"]
        .as_str()
        .unwrap();
    let km_val = crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", &format!("/v1/killmails/{kill_id}/{kill_hash}/")), 200);
    assert_eq!(km_val.get("killmail_id").and_then(|v| v.as_i64()), Some(kill_id));
}

#[tokio::test]
async fn test_plan_esi4_character_skills_clones_assets_mail() {
    // parity: plan:ESI-4
    eve_testkit::attest(&["plan:ESI-4"]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let token = crate::esi_routes::issue_token_with_scopes(
        &ctx,
        90_000_001,
        vec![
            "esi-assets.read_assets.v1".into(),
            "esi-bookmarks.read_character_bookmarks.v1".into(),
            "esi-calendar.read_calendar_events.v1".into(),
            "esi-characters.read_contacts.v1".into(),
            "esi-characters.read_loyalty.v1".into(),
            "esi-clones.read_clones.v1".into(),
            "esi-fittings.read_fittings.v1".into(),
            "esi-location.read_location.v1".into(),
            "esi-location.read_ship_type.v1".into(),
            "esi-mail.read_mail.v1".into(),
            "esi-mail.send_mail.v1".into(),
            "esi-skills.read_skills.v1".into(),
            "esi-skills.read_skillqueue.v1".into(),
            "esi-wallet.read_character_wallet.v1".into(),
        ],
    );
    let get_auth = |path: &str| {
        crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", path).with_bearer_token(&token), 200)
    };

    // Scope enforcement: missing token returns 401; insufficient scope returns 403
    let unauth_resp = router.handle_request(&EsiHttpRequest::new("GET", "/v4/characters/90000001/skills/"));
    assert_eq!(unauth_resp.status_code, 401);
    let narrow_tok = crate::esi_routes::issue_token_with_scopes(&ctx, 90_000_001, vec!["esi-wallet.read_character_wallet.v1".into()]);
    let forbid_resp = router.handle_request(&EsiHttpRequest::new("GET", "/v4/characters/90000001/skills/").with_bearer_token(&narrow_tok));
    assert_eq!(forbid_resp.status_code, 403);

    // 1. Character
    let char_val = crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", "/v5/characters/90000001/"), 200);
    assert_eq!(char_val.get("name").and_then(|v| v.as_str()), Some("Aura Capsuleer"));
    // 2. Clones
    assert!(get_auth("/v3/characters/90000001/clones/").get("jump_clones").is_some());
    // 3. Skills & Skillqueue
    assert!(get_auth("/v4/characters/90000001/skills/").get("total_sp").is_some());
    assert!(get_auth("/v2/characters/90000001/skillqueue/").as_array().is_some());
    // 4. Location & Ship
    assert_eq!(get_auth("/v2/characters/90000001/location/").get("solar_system_id").and_then(|v| v.as_i64()), Some(30_000_142));
    assert_eq!(get_auth("/v1/characters/90000001/ship/").get("ship_type_id").and_then(|v| v.as_i64()), Some(587));
    // 5. Fittings
    assert!(get_auth("/v2/characters/90000001/fittings/").as_array().is_some());
    // 6. Assets & Bookmarks
    assert!(!get_auth("/v5/characters/90000001/assets/").as_array().expect("assets").is_empty());
    assert!(get_auth("/v2/characters/90000001/bookmarks/").as_array().is_some());
    // 7. Wallet
    assert!(get_auth("/v1/characters/90000001/wallet/").as_f64().is_some());
    // 8. Loyalty (corporation store offers + character loyalty points)
    assert!(crate::esi_routes::validate_and_json(&router, &spec, &EsiHttpRequest::new("GET", "/v1/loyalty/stores/1000125/offers/"), 200).as_array().is_some());
    assert!(get_auth("/v1/characters/90000001/loyalty/points/").as_array().is_some());
    // 9. Calendar
    assert!(get_auth("/v1/characters/90000001/calendar/").as_array().is_some());
    // 10. Contacts
    assert!(get_auth("/v2/characters/90000001/contacts/").as_array().is_some());
    // 11. Mail (POST send + GET list)
    let mail_body = serde_json::to_vec(&json!({
        "approved_cost": 0,
        "body": "Fleet op tonight",
        "recipients": [{"recipient_id": 90_000_002, "recipient_type": "character"}],
        "subject": "Op Notification"
    }))
    .unwrap();
    let sent_id = crate::esi_routes::validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", "/v1/characters/90000001/mail/").with_bearer_token(&token).with_body(mail_body),
        201,
    );
    assert!(sent_id.as_i64().unwrap_or(0) > 0);
    assert!(get_auth("/v1/characters/90000001/mail/").as_array().is_some());
}

#[tokio::test]
async fn test_plan_so2_fleets_lifecycle_and_warp() {
    // parity: plan:SO-2
    eve_testkit::attest(&["plan:SO-2"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(9205);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap();

    let mut sess2 = SessionState::new(9206);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap();

    // 1. Create fleet and set MOTD
    let create_rep = ctx
        .dispatch_rpc(&mut sess1, "fleetMgr", None, "CreateFleet", &[PyRep::String("Vanguard Fleet".into()), PyRep::String("Initial MOTD".into())], &kw, None)
        .await
        .expect("CreateFleet");
    let fleet_id = create_rep.dict_get("fleetID").and_then(|v| v.as_i64()).expect("fleetID");
    assert_eq!(sess1.fleet_id, Some(fleet_id));

    let motd_rep = ctx
        .dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "SetMotdEx", &[PyRep::String("Updated fleet MOTD".into())], &kw, None)
        .await
        .expect("SetMotdEx");
    assert_eq!(motd_rep.dict_get("motd"), Some(&PyRep::String("Updated fleet MOTD".into())));

    // 2. Create wing and squad
    let wing_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "CreateWing", &[PyRep::String("Wing 1".into())], &kw, None).await.expect("CreateWing");
    let wing_id = wing_rep.dict_get("wingID").and_then(|v| v.as_i64()).unwrap();

    let squad_rep = ctx
        .dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "CreateSquad", &[PyRep::Integer(wing_id), PyRep::String("Squad 1".into())], &kw, None)
        .await
        .expect("CreateSquad");
    let squad_id = squad_rep.dict_get("squadID").and_then(|v| v.as_i64()).unwrap();

    // 3. Invite member, accept invite, assign role via MoveMember, and verify FinishMove does not demote caller
    ctx.dispatch_rpc(
        &mut sess1,
        "fleetObjectHandler",
        None,
        "Invite",
        &[PyRep::Integer(90_000_002), PyRep::Integer(wing_id), PyRep::Integer(squad_id), PyRep::Integer(4)],
        &kw,
        None,
    )
    .await
    .expect("Invite");

    eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_002);
    let accept_rep = ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "AcceptInvite", &[], &kw, None).await.expect("AcceptInvite");
    assert_eq!(accept_rep.dict_get("fleetID"), Some(&PyRep::Integer(fleet_id)));
    assert_eq!(sess2.fleet_id, Some(fleet_id));

    ctx.advance_secs(12);
    let move_rep = ctx
        .dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "MoveMember", &[PyRep::Integer(90_000_002), PyRep::Integer(wing_id), PyRep::Integer(squad_id), PyRep::Integer(3)], &kw, None)
        .await
        .expect("MoveMember role=3 SquadCommander");
    assert_eq!(move_rep.dict_get("role"), Some(&PyRep::Integer(3)));

    let fm_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "FinishMove", &[], &kw, None).await.expect("FinishMove");
    assert_eq!(fm_rep, PyRep::None);
    let boss_role = ctx.db.with_snapshot(|s| s.fleets[&fleet_id].member_details[&90_000_001].role);
    assert_eq!(boss_role, eve_db::FleetRole::FleetCommander, "FinishMove must not demote FleetCommander");

    // 4. SendBroadcast and verify OnFleetBroadcast notification delivery
    let _ = ctx.drain_notifications_for_char(90_000_002);
    let bc_rep = ctx
        .dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "SendBroadcast", &[PyRep::String("BroadcastTravelTo".into()), PyRep::Integer(30_000_142)], &kw, None)
        .await
        .expect("SendBroadcast");
    assert_eq!(bc_rep.dict_get("broadcast"), Some(&PyRep::String("BroadcastTravelTo".into())));
    assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnFleetBroadcast"), "Pilot 2 must receive OnFleetBroadcast");

    // 5. fleetObjectHandler.Init initializes existing fleet without duplicating; one fleet per character enforced
    let fleets_before = ctx.db.with_snapshot(|s| s.fleets.len());
    let init_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "Init", &[PyRep::Integer(587)], &kw, None).await.expect("fleetObjectHandler.Init");
    assert_eq!(init_rep, PyRep::None);
    assert_eq!(ctx.db.with_snapshot(|s| s.fleets.len()), fleets_before, "Init must not create a second fleet");

    let dup_err = ctx.dispatch_rpc(&mut sess1, "fleetMgr", None, "CreateFleet", &[PyRep::String("Second".into())], &kw, None).await.expect_err("Cannot belong to multiple fleets");
    assert!(dup_err.code.contains("Fleet"), "Must reject duplicate fleet creation");

    // 6. FleetWarp: Undock both ships, SetTakesFleetWarp, and initiate FleetWarp
    ctx.dispatch_rpc(&mut sess1, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock Pilot 1");
    ctx.dispatch_rpc(&mut sess2, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock Pilot 2");
    ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "SetTakesFleetWarp", &[PyRep::Bool(true)], &kw, None).await.expect("SetTakesFleetWarp");

    let ship1_id = sess1.ship_id.expect("Pilot 1 ship_id");
    let ship2_id = sess2.ship_id.expect("Pilot 2 ship_id");
    let sys = ctx.systems.get_or_spawn(30_000_142);

    let fw_dest = glam::DVec3::new(1_000_000_000.0, 0.0, 0.0);
    let fw_rep = ctx
        .dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "FleetWarp", &[PyRep::Real(fw_dest.x), PyRep::Real(fw_dest.y), PyRep::Real(fw_dest.z), PyRep::Real(0.0)], &kw, None)
        .await
        .expect("FleetWarp");
    assert_eq!(fw_rep.dict_get("warpCount"), Some(&PyRep::Integer(2)));

    let (_, b1_fw) = sys.get_ball_snapshot(ship1_id).await.unwrap().unwrap();
    let (_, b2_fw) = sys.get_ball_snapshot(ship2_id).await.unwrap().unwrap();
    assert!(matches!(b1_fw.mode, eve_destiny::MovementMode::Warp { destination, .. } if destination.distance(fw_dest) < 1.0));
    assert!(matches!(b2_fw.mode, eve_destiny::MovementMode::Warp { destination, .. } if destination.distance(fw_dest) < 1.0));

    // 7. Kick member: notification delivery and session cleanup
    let kick_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "KickMember", &[PyRep::Integer(90_000_002)], &kw, None).await.expect("KickMember");
    assert_eq!(kick_rep.dict_get("kickedCharacterID"), Some(&PyRep::Integer(90_000_002)));
    assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnFleetLeave"));
    let p2_fleet_err = ctx.dispatch_rpc(&mut sess2, "fleetMgr", None, "GetInitState", &[], &kw, None).await.expect_err("Kicked member must not be in fleet");
    assert_eq!(p2_fleet_err.code, "NotInFleet");
    assert_eq!(sess2.fleet_id, None, "Kicked member session.fleet_id must be None");

    // 8. Re-invite Pilot 2, logout Boss Pilot 1 -> boss handover to Pilot 2, then Pilot 2 leaves fleet (empty cleanup)
    ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "Invite", &[PyRep::Integer(90_000_002), PyRep::Integer(wing_id), PyRep::Integer(squad_id), PyRep::Integer(3)], &kw, None)
        .await
        .expect("Re-invite Pilot 2");
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_002);
    ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "AcceptInvite", &[], &kw, None).await.expect("AcceptInvite 2");
    let _ = ctx.drain_notifications_for_char(90_000_002);

    ctx.dispatch_rpc(&mut sess1, "authentication", None, "Logout", &[], &kw, None).await.expect("Logout boss");
    assert_eq!(sess1.fleet_id, None);
    assert_eq!(ctx.db.with_snapshot(|s| s.fleets[&fleet_id].boss_id), 90_000_002, "Logout must hand over fleet boss to Pilot 2");
    assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnFleetMemberChanged"));

    let leave_rep = ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "LeaveFleet", &[], &kw, None).await.expect("LeaveFleet");
    assert_eq!(leave_rep.dict_get("left"), Some(&PyRep::Bool(true)));
    assert_eq!(sess2.fleet_id, None, "session.fleet_id must be None after LeaveFleet");
    assert!(!ctx.db.with_snapshot(|s| s.fleets.contains_key(&fleet_id)), "Empty fleet must be cleaned up");
}
