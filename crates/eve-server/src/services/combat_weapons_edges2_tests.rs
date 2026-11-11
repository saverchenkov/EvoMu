//! Additional edge-case and branch-coverage unit tests for `combat_weapons.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::combat_crimewatch::combat_weapons::handle_weapons;
use crate::services::combat_crimewatch::{with_mut_combat_runtime, SafetyLevel};
use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, MovementMode, TargetLock};
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

async fn spawn_ball(
    ctx: &ServerContext,
    sys_id: u32,
    id: i64,
    type_id: u32,
    pos: DVec3,
    hp: f64,
    cap: f64,
) {
    ctx.systems
        .get_or_spawn(sys_id)
        .spawn_ball_with_combat(
            Ball {
                id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: hp,
                max_shield_hp: hp.max(100.0),
                armor_hp: hp,
                max_armor_hp: hp.max(100.0),
                structure_hp: hp,
                max_structure_hp: hp.max(100.0),
                capacitor: cap,
                max_capacitor: cap.max(100.0),
                signature_radius: 100.0,
                base_signature_radius: Some(100.0),
                ..CombatState::default()
            },
        )
        .await
        .unwrap();
}

async fn lock(ctx: &ServerContext, sys_id: u32, ship_id: i64, target_id: i64) {
    ctx.systems
        .get_or_spawn(sys_id)
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(250_000.0),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn ewar_target_painter_npc_overload_tether_and_non_weapon_dispatch() {
    let (mut ctx, mut sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();

    // Seed Target Painter (group 379), non-weapon module (group 60 -> Damage Control), Salvager (1122), Defender (512)
    let mut sde = (*ctx.sde).clone();
    sde.types.insert(
        56_001,
        make_fighter_type(56_001, 379, 7, 5.0, vec![(6, 5.0), (54, 40_000.0), (554, 30.0)]),
    );
    sde.types
        .insert(56_002, make_fighter_type(56_002, 60, 7, 5.0, vec![]));
    sde.types.insert(
        56_003,
        make_fighter_type(56_003, 1122, 7, 5.0, vec![(54, 10_000.0)]),
    );
    sde.types.insert(
        56_004,
        make_fighter_type(56_004, 512, 7, 5.0, vec![(54, 10_000.0)]),
    );
    ctx.sde = Arc::new(sde);

    spawn_ball(&ctx, sys_id, ship_id, 23_915, DVec3::ZERO, 1_000.0, 500.0).await;
    let npc_id = 940_001_i64;
    spawn_ball(&ctx, sys_id, npc_id, 23_915, DVec3::new(5_000.0, 0.0, 0.0), 1_000.0, 0.0).await;

    let tp_id = 840_001_i64;
    let non_wpn_id = 840_002_i64;
    let salv_id = 840_003_i64;
    let def_id = 840_004_i64;
    let missing_type_mod = 840_005_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(tp_id, make_fighter_item(tp_id, 56_001, char_id, ship_id, InventoryFlag::MedSlot0, 1));
        snap.items.insert(non_wpn_id, make_fighter_item(non_wpn_id, 56_002, char_id, ship_id, InventoryFlag::LoSlot0, 1));
        snap.items.insert(salv_id, make_fighter_item(salv_id, 56_003, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(def_id, make_fighter_item(def_id, 56_004, char_id, ship_id, InventoryFlag::HiSlot1, 1));
        snap.items.insert(missing_type_mod, make_fighter_item(missing_type_mod, 999_991, char_id, ship_id, InventoryFlag::HiSlot2, 1));
    });
    ctx.invalidate_ship_fitting(ship_id);

    // Missing module type in SDE -> ModuleNotFound
    let missing_err = handle_weapons(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(missing_type_mod)], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(missing_err.code, "ModuleNotFound");

    // Non-weapon module group returns None
    assert!(handle_weapons(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(non_wpn_id)], &kw)
        .await
        .is_none());

    // Tethered ship cannot activate weapon -> CantActivateWhileTethered
    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .try_update_combat_state(ship_id, |cs| {
            cs.is_tethered = true;
        });
    let tether_err = handle_weapons(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(tp_id), PyRep::Integer(npc_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(tether_err.code, "CantActivateWhileTethered");
    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .try_update_combat_state(ship_id, |cs| {
            cs.is_tethered = false;
        });

    // EWAR TargetRequired & TargetNotLocked
    assert_eq!(
        handle_weapons(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(tp_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TargetRequired"
    );
    assert_eq!(
        handle_weapons(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(tp_id), PyRep::Integer(npc_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TargetNotLocked"
    );

    // Lock NPC -> TargetPainter blooms signature radius via base_signature_radius
    lock(&ctx, sys_id, ship_id, npc_id).await;
    let tp_ok = handle_weapons(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Overload",
        &[PyRep::Integer(tp_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(tp_ok.dict_get("ewarAssistRecorded"), Some(&PyRep::Bool(true)));
    let npc_sig = ctx
        .systems
        .get_or_spawn(sys_id)
        .ballpark
        .read()
        .unwrap()
        .combat_states[&npc_id]
        .signature_radius;
    assert!(npc_sig > 100.0);

    // Salvager & Defender dispatch delegation (return Some(Err(...)) rather than None)
    assert!(handle_weapons(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(salv_id)], &kw).await.is_some());
    assert!(handle_weapons(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(def_id)], &kw).await.is_some());
}

#[tokio::test]
async fn weapon_range_cap_fof_entropic_spool_and_player_ewar_safety_branches() {
    let (mut ctx, mut sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    ctx.set_clock(Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let victim_id = 90_000_002_i64;
    let victim_ship_id = 941_001_i64;
    let kw = HashMap::new();

    // Seed Hybrid gun (74, needs cap), Entropic Disintegrator (1986), Launcher (507) + FoF missile (395), Webifier EWAR (65)
    let mut sde = (*ctx.sde).clone();
    sde.types.insert(
        56_101,
        make_fighter_type(56_101, 74, 7, 5.0, vec![(6, 50.0), (54, 1_000.0), (158, 1_000.0), (160, 10.0), (604, 114.0)]),
    );
    sde.types.insert(
        56_106,
        make_fighter_type(56_106, 114, 8, 1.0, vec![(117, 10.0), (118, 10.0)]),
    );
    sde.types.insert(
        56_102,
        make_fighter_type(56_102, 1986, 7, 5.0, vec![(6, 5.0), (54, 50_000.0), (160, 10.0), (114, 10.0), (604, 1987.0), (2699, 0.25), (2700, 1.5)]),
    );
    sde.types.insert(
        56_107,
        make_fighter_type(56_107, 1987, 8, 1.0, vec![(114, 10.0), (117, 10.0), (118, 10.0)]),
    );
    sde.types.insert(
        56_103,
        make_fighter_type(56_103, 507, 7, 5.0, vec![(6, 0.0), (54, 50_000.0), (604, 114.0), (605, 395.0)]),
    );
    sde.types.insert(
        56_104,
        make_fighter_type(56_104, 395, 8, 1.0, vec![(114, 50.0), (37, 250.0), (281, 200.0)]),
    );
    sde.types.insert(
        56_105,
        make_fighter_type(56_105, 65, 7, 5.0, vec![(6, 5.0), (54, 20_000.0)]),
    );
    ctx.sde = Arc::new(sde);

    // Give ship only 10 cap (not enough for hybrid's 50)
    spawn_ball(&ctx, sys_id, ship_id, 23_915, DVec3::ZERO, 1_000.0, 10.0).await;
    spawn_ball(&ctx, sys_id, victim_ship_id, 23_915, DVec3::new(10_000.0, 0.0, 0.0), 5_000.0, 100.0).await;

    let hybrid_id = 841_001_i64;
    let entropic_id = 841_002_i64;
    let launcher_id = 841_003_i64;
    let fof_ammo_id = 841_004_i64;
    let web_id = 841_005_i64;
    let not_in_space_target = 941_999_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(vc) = snap.characters.get_mut(&victim_id) {
            vc.active_ship_id = victim_ship_id;
            vc.solar_system_id = sys_id;
            vc.station_id = None;
            vc.structure_id = None;
        }
        snap.items.insert(victim_ship_id, make_fighter_item(victim_ship_id, 23_915, victim_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(hybrid_id, make_fighter_item(hybrid_id, 56_101, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(841_099, make_fighter_item(841_099, 56_106, char_id, ship_id, InventoryFlag::HiSlot0, 5));
        snap.items.insert(entropic_id, make_fighter_item(entropic_id, 56_102, char_id, ship_id, InventoryFlag::HiSlot1, 1));
        snap.items.insert(841_098, make_fighter_item(841_098, 56_107, char_id, ship_id, InventoryFlag::HiSlot1, 5));
        snap.items.insert(launcher_id, make_fighter_item(launcher_id, 56_103, char_id, ship_id, InventoryFlag::HiSlot2, 1));
        snap.items.insert(fof_ammo_id, make_fighter_item(fof_ammo_id, 56_104, char_id, ship_id, InventoryFlag::HiSlot2, 5));
        snap.items.insert(web_id, make_fighter_item(web_id, 56_105, char_id, ship_id, InventoryFlag::MedSlot0, 1));
    });
    ctx.invalidate_ship_fitting(ship_id);

    // FoF auto-targeting when target arg omitted hits victim_ship_id automatically without lock!
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Red);
    });
    let fof_res = handle_weapons(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(launcher_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fof_res.dict_get("targetID"), Some(&PyRep::Integer(victim_ship_id)));

    // EWAR against non-locked / safety-blocked player target
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Green);
        rt.limited_engagements.clear();
    });
    lock(&ctx, sys_id, ship_id, victim_ship_id).await;
    assert_eq!(
        handle_weapons(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(web_id), PyRep::Integer(victim_ship_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CantDoInCurrentSafetyLevel"
    );

    // Red safety + OutOfRange (victim at 10km, hybrid max=1km+2*1km=3km)
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Red);
    });
    assert_eq!(
        handle_weapons(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(hybrid_id), PyRep::Integer(victim_ship_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TargetOutOfRange"
    );

    // Close target (500m) for NotEnoughCapacitor on hybrid, and Entropic Disintegrator AlreadyActive on overload/second cycle
    let close_npc = 941_002_i64;
    spawn_ball(&ctx, sys_id, close_npc, 23_915, DVec3::new(500.0, 0.0, 0.0), 5_000.0, 0.0).await;
    lock(&ctx, sys_id, ship_id, close_npc).await;
    assert_eq!(
        handle_weapons(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(hybrid_id), PyRep::Integer(close_npc)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotEnoughCapacitor"
    );

    // Entropic Disintegrator Overload fires once (returns spoolMultiplier), second Overload on same target -> AlreadyActive
    let ent1 = handle_weapons(&ctx, &mut sess, "dogmaIM", "Overload", &[PyRep::Integer(entropic_id), PyRep::Integer(close_npc)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(ent1.dict_get("spoolMultiplier").is_some());
    assert_eq!(
        handle_weapons(&ctx, &mut sess, "dogmaIM", "Overload", &[PyRep::Integer(entropic_id), PyRep::Integer(close_npc)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "AlreadyActive"
    );

    // TargetNotInSpace when target is locked but no ball exists in ballpark
    lock(&ctx, sys_id, ship_id, not_in_space_target).await;
    assert_eq!(
        handle_weapons(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(entropic_id), PyRep::Integer(not_in_space_target)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TargetNotInSpace"
    );

    // TargetNotLocked resets entropic spool and active states
    assert_eq!(
        handle_weapons(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(entropic_id), PyRep::Integer(999_777)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TargetNotLocked"
    );
}
