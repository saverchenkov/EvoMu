//! Shared helpers for Abyssal Deadspace room combat: clears room NPCs through the production
//! in-space damage pipeline (`SystemHandle::apply_damage_with_attacker` -> ballpark removal + death hook)
//! and verifies the room's Triglavian Bioadaptive Cache and its persisted loot.

use std::collections::HashMap;
use eve_db::InventoryFlag;
use eve_destiny::{DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{combat_npc_ai::step_extended_npc_ai_sync, with_combat_runtime};
use eve_server::services::pve_special_sites::{
    with_pve_special_reg, ABYSSAL_ROOM_NPC_TYPES, BIOADAPTIVE_CACHE_TYPE_ID,
    BIOADAPTIVE_CACHE_WRECK_TYPE_ID, TRIGLAVIAN_SURVEY_DATABASE_TYPE_ID,
};
use crate::common;

/// Ticks Abyssal room NPC AI (verifying Triglavian room NPCs target and damage the pilot's ship and have
/// difficulty-scaled HP), then destroys every NPC of `room_idx` (0-based) through the damage pipeline.
pub async fn destroy_room_npcs_via_damage(ctx: &ServerContext, char_id: i64, room_idx: usize) {
    let (sys, ship_id, diff_mult, npcs) = with_pve_special_reg(ctx, |reg| {
        let p = reg.abyssal_pockets.get(&char_id).expect("pocket");
        (p.abyssal_solar_system_id, p.ship_id, p.npc_difficulty_mult, p.rooms[room_idx].npc_ball_ids.clone())
    });
    assert!(!npcs.is_empty(), "room {} must have NPCs", room_idx + 1);
    let h = ctx.systems.get_or_spawn(sys);
    if let Ok(Some(mut cs)) = h.get_combat_state(ship_id).await {
        if cs.max_shield_hp < 50_000.0 {
            cs.max_shield_hp = 50_000.0;
            cs.shield_hp = 50_000.0;
            let _ = h.set_combat_state(ship_id, cs).await;
        }
    }
    let ship_before = h.get_combat_state(ship_id).await.unwrap().expect("pilot ship has CombatState");
    let hp_before = ship_before.shield_hp + ship_before.armor_hp + ship_before.structure_hp;
    step_extended_npc_ai_sync(ctx, sys);
    let ship_after = h.get_combat_state(ship_id).await.unwrap().expect("pilot ship CombatState after AI tick");
    let hp_after = ship_after.shield_hp + ship_after.armor_hp + ship_after.structure_hp;
    assert!(hp_after < hp_before, "Triglavian room NPC AI tick must damage pilot ship ({hp_after} < {hp_before})");
    let _ = h.heal_combat_state(ship_id).await;
    for bid in npcs {
        let (_, ball) = h.get_ball_snapshot(bid).await.unwrap().expect("room NPC ball spawned");
        assert!(ABYSSAL_ROOM_NPC_TYPES.contains(&ball.type_id), "NPC {bid} has real Abyssal type, got {}", ball.type_id);
        let target = with_combat_runtime(ctx, |rt| rt.npc_entities.get(&bid).and_then(|n| n.target_id));
        assert_eq!(target, Some(ship_id), "NPC {bid} must target pilot ship {ship_id}");
        let cs = h.get_combat_state(bid).await.unwrap().expect("room NPC has CombatState");
        let base_cs = ctx.build_type_combat_state(ball.type_id);
        assert!((cs.max_structure_hp - base_cs.max_structure_hp * diff_mult.max(1.0)).abs() < 1e-3, "NPC {bid} HP scaled by difficulty");
        let dmg = DamageVector { em: 1.0e7, thermal: 1.0e7, kinetic: 1.0e7, explosive: 1.0e7 };
        let res = h.apply_damage_with_attacker(bid, dmg, Some(char_id)).await;
        assert!(matches!(res, Ok(Some((_, true, _)))), "NPC {bid} must be destroyed by damage: {res:?}");
        assert!(h.get_ball_snapshot(bid).await.unwrap().is_none(), "destroyed NPC {bid} removed from ballpark");
    }
    // RF-04: Verify conduit proactively unlocked immediately on final NPC death BEFORE any AbyssalGateActivation RPC
    let (cleared, unlocked) = with_pve_special_reg(ctx, |reg| {
        let r = &reg.abyssal_pockets.get(&char_id).expect("pocket").rooms[room_idx];
        (r.cleared, r.gate_unlocked_at_sec.is_some())
    });
    assert!(cleared && unlocked, "room {} conduit proactively unlocked on final NPC death", room_idx + 1);
    // Approach room transfer/exit gate within proximity (<= 2500m)
    let (pid, curr_room) = with_pve_special_reg(ctx, |reg| {
        let p = reg.abyssal_pockets.get(&char_id).expect("pocket");
        (p.pocket_id, p.current_room)
    });
    let gate_id = 1_895_000_000 + pid * 10 + (curr_room as i64);
    if let Ok(Some((_, gb))) = h.get_ball_snapshot(gate_id).await {
        let _ = h.set_ball_position(ship_id, gb.position + glam::DVec3::new(100.0, 0.0, 0.0)).await;
    }
}

/// Verifies the room's Bioadaptive Cache in space, destroys it via combat damage, and verifies its loot transfers to the wreck.
pub async fn assert_room_cache_loot(ctx: &ServerContext, char_id: i64, room_idx: usize, survey_qty: i32, filament_type: u32) {
    let (sys, cache_id) = with_pve_special_reg(ctx, |reg| {
        let p = reg.abyssal_pockets.get(&char_id).expect("pocket");
        (p.abyssal_solar_system_id, p.rooms[room_idx].cache_item_id)
    });
    let h = ctx.systems.get_or_spawn(sys);
    let (_, ball) = h.get_ball_snapshot(cache_id).await.unwrap().expect("Bioadaptive Cache ball in space");
    assert_eq!(ball.type_id, BIOADAPTIVE_CACHE_TYPE_ID);
    assert!(h.get_combat_state(cache_id).await.unwrap().is_some(), "cache is a destroyable entity");
    let (cache_rec, mut loot) = ctx.db.with_snapshot(|s| {
        let rec = s.items.get(&cache_id).map(|i| (i.type_id, i.location_id));
        let loot: Vec<(u32, i32)> = s.items.values().filter(|i| i.location_id == cache_id).map(|i| (i.type_id, i.quantity)).collect();
        (rec, loot)
    });
    assert_eq!(cache_rec, Some((BIOADAPTIVE_CACHE_TYPE_ID, sys as i64)), "cache persisted in snap.items");
    loot.sort_unstable();
    assert!(
        loot.contains(&(TRIGLAVIAN_SURVEY_DATABASE_TYPE_ID, survey_qty)),
        "room {} cache loot contains survey database: {:?}",
        room_idx + 1,
        loot
    );
    assert!(
        loot.contains(&(filament_type, 1)),
        "room {} cache loot contains filament: {:?}",
        room_idx + 1,
        loot
    );
    assert!(
        loot.iter().any(|&(tid, _)| matches!(tid, 47_699 | 47_700 | 47_701)),
        "room {} cache loot contains tier-appropriate mutaplasmid: {:?}",
        room_idx + 1,
        loot
    );
    assert!(
        loot.iter().any(|&(tid, _)| tid == 48_112),
        "room {} cache loot contains zero-point condensate: {:?}",
        room_idx + 1,
        loot
    );
    let dmg = DamageVector { em: 1.0e6, thermal: 1.0e6, kinetic: 1.0e6, explosive: 1.0e6 };
    let res = h.apply_damage_with_attacker(cache_id, dmg, Some(char_id)).await;
    assert!(matches!(res, Ok(Some((_, true, _)))), "Bioadaptive Cache {cache_id} destroyed by damage: {res:?}");
    let (wreck_id, mut wreck_loot) = ctx.db.with_snapshot(|s| {
        let wid = s.items.values().filter(|i| i.type_id == BIOADAPTIVE_CACHE_WRECK_TYPE_ID && i.location_id == sys as i64).max_by_key(|i| i.item_id).map(|i| i.item_id).expect("cache wreck item");
        let wl: Vec<(u32, i32)> = s.items.values().filter(|i| i.location_id == wid).map(|i| (i.type_id, i.quantity)).collect();
        (wid, wl)
    });
    assert!(h.get_ball_snapshot(wreck_id).await.unwrap().is_some(), "cache wreck ball spawned in space");
    wreck_loot.sort_unstable();
    assert_eq!(wreck_loot, loot, "room {} cache loot moved into wreck {wreck_id}", room_idx + 1);
}

#[tokio::test]
async fn test_abyssal_deadspace_mechanics_and_collapse_lifecycle() {
    // parity: feat:abyssal.gate-unlock-on-clear, feat:abyssal.timer-collapse-death
    // parity: feat:navigation.abyssal-trace-collapse, feat:navigation.filament-abyssal
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(882_101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001_i64;
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

    let origin_sys = 30_000_002_u32; // Lashesih (0.8 high-sec)
    let ship_id = 100_000_015_i64;
    let filament_id = 100_000_016_i64;
    let fil_type = 47_765_u32; // Calm Electrical Filament

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = origin_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 621, // Caracal (Cruiser)
                owner_id: char_id,
                location_id: origin_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            filament_id,
            eve_db::InventoryItemRecord {
                item_id: filament_id,
                type_id: fil_type,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
    });

    sess.solar_system_id = Some(origin_sys);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    let origin_handle = ctx.systems.get_or_spawn(origin_sys);
    let ship_ball = eve_destiny::Ball {
        id: ship_id,
        type_id: 621,
        position: glam::DVec3::new(10_000.0, 0.0, 0.0),
        velocity: glam::DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let cs = ctx.build_ship_combat_state(char_id, ship_id, 621);
    let _ = origin_handle.spawn_ball_with_combat(ship_ball, cs).await;

    // 1. Deploy filament in space (spawns Abyssal Trace ball 47956)
    let deploy_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEntranceDeployment",
            &[PyRep::Integer(filament_id)],
            &kw,
            None,
        )
        .await
        .expect("AbyssalEntranceDeployment");
    let trace_id = deploy_rep.dict_get("traceBallID").and_then(|v| v.as_i64()).expect("traceBallID");
    assert_eq!(deploy_rep.dict_get("maxShips").and_then(|v| v.as_i64()), Some(1));
    let trace_ball = origin_handle.get_ball_snapshot(trace_id).await.unwrap().expect("Abyssal trace ball");
    assert_eq!(trace_ball.1.type_id, 47_458, "Trace ball is Abyssal Trace type");

    // Verify trace is scannable via DirectionalScan on scanMgr
    let scans_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            None,
            "DirectionalScan",
            &[PyRep::Real(360.0), PyRep::Real(14.3 * 1.495978707e11)],
            &kw,
            None,
        )
        .await
        .expect("scanMgr.DirectionalScan");
    let PyRep::Rowset { rows, .. } = scans_rep else {
        panic!("expected PyRep::Rowset from scanMgr.DirectionalScan");
    };
    let has_trace = rows.iter().any(|r| {
        r.first().and_then(|v| v.as_i64()) == Some(trace_id)
    });
    assert!(has_trace, "Abyssal Trace appears in directional scanner results");

    // 2. Activate entrance gate into Abyssal Deadspace pocket
    ctx.set_mechanic_rng_seed(Some(4));
    let enter_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEntranceGateActivation",
            &[PyRep::Integer(trace_id)],
            &kw,
            None,
        )
        .await
        .expect("AbyssalEntranceGateActivation");
    let abyssal_sys = enter_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    let pocket_id = enter_rep.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(sess.solar_system_id, Some(abyssal_sys));
    // RF-04: Concurrent second pilot entering an Abyssal pocket in 32000001 gets an isolated >= 10,000 km grid_offset
    let (char2, ship2, fil2) = (90_000_002_i64, 100_000_095_i64, 100_000_096_i64);
    let mut sess2 = SessionState::new(882_109);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char2)], &kw, None).await.expect("SelectCharacterID 2");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char2) { ch.solar_system_id = origin_sys; ch.station_id = None; ch.structure_id = None; ch.active_ship_id = ship2; }
        snap.items.insert(ship2, eve_db::InventoryItemRecord { item_id: ship2, type_id: 621, owner_id: char2, location_id: origin_sys as i64, flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default() });
        snap.items.insert(fil2, eve_db::InventoryItemRecord { item_id: fil2, type_id: fil_type, owner_id: char2, location_id: ship2, flag: InventoryFlag::Cargo, quantity: 1, singleton: false, ..Default::default() });
    });
    sess2.solar_system_id = Some(origin_sys); sess2.station_id = None; sess2.structure_id = None; sess2.ship_id = Some(ship2);
    let _ = origin_handle.spawn_ball_with_combat(eve_destiny::Ball { id: ship2, type_id: 621, position: glam::DVec3::new(2_000_000.0, 0.0, 0.0), velocity: glam::DVec3::ZERO, max_velocity: 250.0, inertia: 0.5, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx.build_ship_combat_state(char2, ship2, 621)).await;
    let dep2 = ctx.dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fil2)], &kw, None).await.expect("Deploy 2");
    let tr2 = dep2.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
    ctx.dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr2)], &kw, None).await.expect("Enter 2");
    let sep_m = with_pve_special_reg(&ctx, |reg| {
        glam::DVec3::from_array(reg.abyssal_pockets[&char2].grid_offset)
            .distance(glam::DVec3::from_array(reg.abyssal_pockets[&char_id].grid_offset))
    });
    assert!(sep_m >= 10_000_000.0, "Concurrent Abyssal pockets must be separated by >= 10,000 km (got {sep_m})");
    let abyss_handle = ctx.systems.get_or_spawn(abyssal_sys);
    let pilot_cs = abyss_handle.get_combat_state(ship_id).await.unwrap().expect("abyssal pilot CombatState");
    assert!(pilot_cs.shield_resonance[0] > 0.5, "Electrical storm reduces EM shield resistance");
    assert!(pilot_cs.armor_resonance[0] > 0.5 && pilot_cs.structure_resonance[0] > 0.67, "V24 SDE Electrical storm reduces EM armor and hull resistance");

    // 3. Verify spatial hazard clouds status effects
    // Move ship into Tachyon cloud boundary (at (15000, 0, 0))
    let mut ship_b = abyss_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    ship_b.position = glam::DVec3::new(15_000.0, 0.0, 0.0);
    abyss_handle.spawn_ball(ship_b.clone()).await.unwrap();

    let cloud_effect = eve_server::services::pve_special_sites_abyssal::evaluate_abyssal_spatial_clouds_for_ship(
        &ctx,
        abyssal_sys,
        ship_id,
    )
    .await;
    assert_eq!(cloud_effect, Some("TachyonCloud"), "Status effect applied inside Tachyon cloud boundary");

    // Move ship away from cloud boundary
    ship_b.position = glam::DVec3::new(0.0, 0.0, 0.0);
    abyss_handle.spawn_ball(ship_b).await.unwrap();
    let cloud_effect_outside = eve_server::services::pve_special_sites_abyssal::evaluate_abyssal_spatial_clouds_for_ship(
        &ctx,
        abyssal_sys,
        ship_id,
    )
    .await;
    assert!(cloud_effect_outside.is_none(), "Cloud effect removed outside boundary");

    // 4. Verify automated pylons
    let (_damaged_count, _buffed_count) = eve_server::services::pve_special_sites_abyssal::pulse_abyssal_automated_pylons(
        &ctx,
        abyssal_sys,
    )
    .await;
    let pylon_to_destroy = 1_899_000_000 + pocket_id * 100 + 1 * 10 + 1; // Small Suppressor in room 1
    let pylon_cs_before = abyss_handle.get_combat_state(pylon_to_destroy).await.unwrap().expect("pylon before damage");
    assert!(pylon_cs_before.structure_hp > 0.0);
    let dmg = DamageVector { em: 5.0e5, thermal: 5.0e5, kinetic: 5.0e5, explosive: 5.0e5 };
    let pylon_dmg_res = abyss_handle.apply_damage_with_attacker(pylon_to_destroy, dmg, Some(char_id)).await;
    assert!(matches!(pylon_dmg_res, Ok(Some((_, true, _)))), "Pylon destroyed by damage");
    assert!(abyss_handle.get_ball_snapshot(pylon_to_destroy).await.unwrap().is_none(), "Destroyed pylon removed");

    // 5. Verify room 1 Bioadaptive Cache & subnode
    assert_room_cache_loot(&ctx, char_id, 0, 2, fil_type).await;

    // Verify conduit gate is initially locked and invulnerable
    let gate1_id = 1_895_000_000 + pocket_id * 10 + 1;
    let gate1_cs = abyss_handle.get_combat_state(gate1_id).await.unwrap().expect("gate1 CombatState");
    assert!(gate1_cs.undock_invulnerable_remaining_ticks > 0, "Triglavian Transfer Conduit spawns in invulnerable state");

    let locked_err = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalGateActivation",
            &[PyRep::Integer(gate1_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Gate activation blocked while NPCs alive");
    assert_eq!(locked_err.code, "GateLocked");

    // Clear Room 1 NPCs -> unlocks gate
    destroy_room_npcs_via_damage(&ctx, char_id, 0).await;
    let gate1_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalGateActivation",
            &[PyRep::Integer(gate1_id)],
            &kw,
            None,
        )
        .await
        .expect("AbyssalGateActivation Room 1 -> Room 2");
    assert_eq!(gate1_rep.dict_get("room").and_then(|v| v.as_i64()), Some(2));

    // Clear Room 2 NPCs and transition to Room 3
    assert_room_cache_loot(&ctx, char_id, 1, 2, fil_type).await;
    destroy_room_npcs_via_damage(&ctx, char_id, 1).await;
    let gate2_id = 1_895_000_000 + pocket_id * 10 + 2;
    let gate2_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalGateActivation",
            &[PyRep::Integer(gate2_id)],
            &kw,
            None,
        )
        .await
        .expect("AbyssalGateActivation Room 2 -> Room 3");
    assert_eq!(gate2_rep.dict_get("room").and_then(|v| v.as_i64()), Some(3));

    // 6. Complete Room 3 and exit through origin gate
    assert_room_cache_loot(&ctx, char_id, 2, 2, fil_type).await;
    destroy_room_npcs_via_damage(&ctx, char_id, 2).await;
    let gate3_id = 1_895_000_000 + pocket_id * 10 + 3;

    let gate3_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEndGateActivation",
            &[PyRep::Integer(gate3_id)],
            &kw,
            None,
        )
        .await
        .expect("AbyssalEndGateActivation Room 3 -> Origin");
    assert_eq!(gate3_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(origin_sys as i64));
    assert_eq!(gate3_rep.dict_get("exitedAbyssalSpace"), Some(&PyRep::Bool(true)));
    let final_sys = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().solar_system_id);
    assert_eq!(final_sys, origin_sys, "Pilot returned to origin system after completing Abyssal run");

    // Origin trace ball despawned from Jita ballpark when the run finishes!
    let trace_after = origin_handle.get_ball_snapshot(trace_id).await.unwrap();
    assert!(trace_after.is_none(), "Origin Abyssal Trace ball despawned when run finishes");

    // Abyssal session cleared on successful exit: subsequent time advances do NOT collapse or kill pilot
    ctx.advance_secs(2000);
    let post_tick_sys = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().solar_system_id);
    assert_eq!(post_tick_sys, origin_sys, "Pilot remains alive in origin system after successful exit");
}

#[tokio::test]
async fn test_abyssal_hull_rules_and_timer_collapse() {
    // parity: feat:abyssal.timer-collapse-death, feat:navigation.filament-abyssal
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(882_202);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001_i64;
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

    let origin_sys = 30_000_002_u32; // Lashesih (0.8 high-sec)
    let fil_type = 47761_u32; // Calm Exotic Filament
    let fil_id1 = 100_000_201_i64;
    let fil_id2 = 100_000_202_i64;
    let fil_id3 = 100_000_203_i64;
    let fil_id4 = 100_000_204_i64;
    let t3c_ship = 100_000_210_i64;
    let frig_ship = 100_000_211_i64;
    let flag_ship = 100_000_212_i64;
    let cruiser_ship = 100_000_213_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = origin_sys;
            ch.active_ship_id = t3c_ship;
            ch.station_id = None;
            ch.structure_id = None;
            ch.balance = eve_db::IskCents::from_isk(100_000_000);
        }
        // Strategic Cruiser (T3C Loki, type 29990, group 963)
        snap.items.insert(
            t3c_ship,
            eve_db::InventoryItemRecord {
                item_id: t3c_ship,
                type_id: 29990,
                owner_id: char_id,
                location_id: origin_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Frigate (Rifter, type 587, group 25)
        snap.items.insert(
            frig_ship,
            eve_db::InventoryItemRecord {
                item_id: frig_ship,
                type_id: 587,
                owner_id: char_id,
                location_id: origin_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Flag Cruiser (Monitor, type 45534, group 1972)
        snap.items.insert(
            flag_ship,
            eve_db::InventoryItemRecord {
                item_id: flag_ship,
                type_id: 45534,
                owner_id: char_id,
                location_id: origin_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Cruiser (Rupture, type 621, group 26)
        snap.items.insert(
            cruiser_ship,
            eve_db::InventoryItemRecord {
                item_id: cruiser_ship,
                type_id: 621,
                owner_id: char_id,
                location_id: origin_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Filaments in cargo of ships
        snap.items.insert(
            fil_id1,
            eve_db::InventoryItemRecord {
                item_id: fil_id1,
                type_id: fil_type,
                owner_id: char_id,
                location_id: t3c_ship,
                flag: InventoryFlag::Cargo,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            fil_id2,
            eve_db::InventoryItemRecord {
                item_id: fil_id2,
                type_id: fil_type,
                owner_id: char_id,
                location_id: frig_ship,
                flag: InventoryFlag::Cargo,
                quantity: 2, // only 2 filaments: insufficient for frigate (requires 3)
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            fil_id3,
            eve_db::InventoryItemRecord {
                item_id: fil_id3,
                type_id: fil_type,
                owner_id: char_id,
                location_id: flag_ship,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            fil_id4,
            eve_db::InventoryItemRecord {
                item_id: fil_id4,
                type_id: fil_type,
                owner_id: char_id,
                location_id: cruiser_ship,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });

    sess.solar_system_id = Some(origin_sys);
    sess.ship_id = Some(t3c_ship);
    let origin_handle = ctx.systems.get_or_spawn(origin_sys);
    for (sid, tid, x) in [(t3c_ship, 29990, 100.0), (frig_ship, 587, 100.0), (flag_ship, 45534, 2_000_000.0), (cruiser_ship, 621, 4_000_000.0)] {
        let b = eve_destiny::Ball {
            id: sid,
            type_id: tid,
            position: glam::DVec3::new(x, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 250.0,
            inertia: 0.5,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        let _ = origin_handle.spawn_ball(b).await;
    }

    // 1. Strategic Cruiser (T3C) entry restriction: deployment rejected
    let t3c_err = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fil_id1)], &kw, None)
        .await
        .expect_err("T3C deployment must be disallowed");
    assert_eq!(t3c_err.code, "DisallowedShipType");

    // 2. Frigate requires 3 filaments: 2 filaments is insufficient
    sess.ship_id = Some(frig_ship);
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().active_ship_id = frig_ship);
    let ins_err = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fil_id2)], &kw, None)
        .await
        .expect_err("Frigate with 2 filaments must be rejected");
    assert_eq!(ins_err.code, "InsufficientFilamentQuantity");

    // Give 3 filaments to frigate: succeeds and consumes all 3 filaments, setting max_ships = 3
    ctx.db.with_mut_snapshot(|s| s.items.get_mut(&fil_id2).unwrap().quantity = 3);
    let frig_rep = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fil_id2)], &kw, None)
        .await
        .expect("Frigate deployment with 3 filaments");
    assert_eq!(frig_rep.dict_get("maxShips").and_then(|v| v.as_i64()), Some(3));
    let frig_trace_id = frig_rep.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
    let remaining_qty = ctx.db.with_snapshot(|s| s.items.get(&fil_id2).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(remaining_qty, 0, "All 3 filaments consumed for frigate trace");

    // 3. Cruiser cannot enter a 3-ship frigate trace (capacity class mismatch)
    sess.ship_id = Some(cruiser_ship);
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().active_ship_id = cruiser_ship);
    let _ = origin_handle.set_ball_position(cruiser_ship, glam::DVec3::new(600.0, 0.0, 0.0)).await;
    let mismatch_err = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(frig_trace_id)], &kw, None)
        .await
        .expect_err("Cruiser entering frigate trace must be rejected");
    assert_eq!(mismatch_err.code, "DisallowedShipType");
    let _ = origin_handle.set_ball_position(cruiser_ship, glam::DVec3::new(4_000_000.0, 0.0, 0.0)).await;
    // 4. Flag Cruiser (group 1972) classified as Cruiser: solo run (1 ship, 1 filament)
    sess.ship_id = Some(flag_ship);
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().active_ship_id = flag_ship);
    let flag_rep = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fil_id3)], &kw, None)
        .await
        .expect("Flag cruiser deployment with 1 filament");
    assert_eq!(flag_rep.dict_get("maxShips").and_then(|v| v.as_i64()), Some(1));
    // 5. Cruiser deploys and enters pocket; test timer collapse with zero insurance payout
    sess.ship_id = Some(cruiser_ship);
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().active_ship_id = cruiser_ship);
    let cr_deploy = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fil_id4)], &kw, None)
        .await
        .expect("Cruiser deployment");
    let cr_trace_id = cr_deploy.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
    let enter_rep = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(cr_trace_id)], &kw, None)
        .await
        .expect("Cruiser enters pocket");
    let abyssal_sys = enter_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;

    let wallet_before = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);

    // Advance 1100s (< 1200s): Cluster tick does NOT collapse pocket before expiration
    ctx.advance_secs(1100);
    let cur_sys_mid = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().solar_system_id);
    assert_eq!(cur_sys_mid, abyssal_sys, "Pocket not collapsed before 1200s expiry");

    // Advance to 1205s (expiry elapsed): Cluster tick via ctx.advance_secs executes collapse!
    ctx.advance_secs(105);

    // Verify pilot ship destroyed, pilot podded and returned to home station in corvette
    let (pilot_sys, active_ship) = ctx.db.with_snapshot(|s| {
        let ch = s.characters.get(&char_id).unwrap();
        (ch.solar_system_id, ch.active_ship_id)
    });
    assert_ne!(pilot_sys, abyssal_sys, "Pilot removed from collapsed Abyssal space");
    assert_ne!(active_ship, cruiser_ship, "Old ship destroyed, replaced with starter corvette");

    // Zero insurance payout on Abyssal collapse death
    let wallet_after = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
    assert_eq!(wallet_after, wallet_before, "No insurance payout on Abyssal collapse death");

    // Verify killmails logged to Abyssal Environment (corp 1000298)
    let kms = ctx.db.with_snapshot(|s| {
        s.killmails.values().filter(|k| k.victim_character_id == char_id).cloned().collect::<Vec<_>>()
    });
    assert_eq!(kms.len(), 2, "Expected 2 killmails (ship and capsule)");
    assert!(kms.iter().all(|k| k.attacker_character_id == 0 && k.attacker_corp_id == 1_000_298), "Death logged to Abyssal Environment");
    assert!(kms.iter().any(|k| k.ship_type_id == 670), "Capsule destroyed");

    // Trace despawned on collapse
    let trace_after = origin_handle.get_ball_snapshot(cr_trace_id).await.unwrap();
    assert!(trace_after.is_none(), "Origin Abyssal Trace ball despawned on collapse");
}

