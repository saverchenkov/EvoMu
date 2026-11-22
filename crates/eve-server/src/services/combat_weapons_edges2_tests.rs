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
    super::combat_weapons_tests::enable_test_overload(&mut sde, &ctx, char_id, &[56_001]);
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
    super::combat_weapons_tests::enable_test_overload(&mut sde, &ctx, char_id, &[56_102]);
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

fn assert_no_charges_user_error(
    err: &eve_proto::BridgeRpcError,
    expected_module_id: i64,
    expected_launcher_type_id: u32,
    expected_charge_group: Option<u32>,
) {
    assert_eq!(err.code, "NoAmmoLoaded");
    let expected_type_id = i64::from(expected_launcher_type_id);
    let expected_facts = eve_proto::RejectionFacts::NoAmmoLoaded {
        module_id: Some(expected_module_id),
        launcher_type_id: Some(expected_type_id),
        required_charge_group: expected_charge_group,
    };
    assert_eq!(err.rejection_facts(), Some(&expected_facts));

    let launcher_py = PyRep::Tuple(vec![PyRep::Integer(4), PyRep::Integer(expected_type_id)]);
    let client_err = err.to_client_error("dogmaIM", "Activate");
    assert_eq!(
        client_err,
        eve_proto::ClientError::Rejected {
            key: eve_proto::UserErrorKey::NoCharges,
            args: vec![("launcher".to_string(), launcher_py.clone())],
            facts: expected_facts,
        }
    );

    let body = client_err.to_macho_error_body(6);
    let PyRep::Tuple(parts) = &body else {
        panic!("expected ErrorResponse tuple");
    };
    assert_eq!(
        parts[1],
        PyRep::Integer(eve_proto::MACHONETERR_WRAPPEDEXCEPTION)
    );
    let PyRep::Tuple(inner) = &parts[2] else {
        panic!("expected inner payload tuple");
    };
    let PyRep::Bytes(raw) = &inner[0] else {
        panic!("expected marshaled UserError bytes");
    };
    let PyRep::ObjectEx { header, .. } =
        eve_proto::marshal::decode(raw).expect("decode marshaled UserError")
    else {
        panic!("expected UserError ObjectEx");
    };
    let PyRep::Tuple(hdr) = *header else {
        panic!("expected UserError header tuple");
    };
    assert_eq!(hdr[0], PyRep::Token("eveexceptions.UserError".into()));
    let PyRep::Tuple(u_args) = &hdr[1] else {
        panic!("expected UserError args tuple");
    };
    assert_eq!(u_args[0].as_str(), Some("NoCharges"));
    assert_eq!(
        u_args[1],
        PyRep::Dict(vec![(PyRep::ascii_bytes("launcher"), launcher_py)])
    );
}

#[tokio::test]
async fn unloaded_ship_weapons_return_typed_no_charges_user_error() {
    let (mut ctx, mut sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    ctx.set_clock(Arc::new(
        eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000),
    ));
    let kw = HashMap::new();

    let mut sde = (*ctx.sde).clone();
    if let Some(ship_t) = sde.types.get_mut(&23_915) {
        ship_t.default_attributes.insert(11, 1_000.0);
        ship_t.default_attributes.insert(14, 8.0);
        ship_t.default_attributes.insert(48, 1_000.0);
    }
    let hi_weapon_type = |tid, gid, attrs| {
        let mut t = make_fighter_type(tid, gid, 7, 5.0, attrs);
        t.effect_ids = vec![12]; // hiPower
        t
    };
    // Standard Projectile Turret (group 55, chargeGroup1 = 83)
    sde.types.insert(
        56_201,
        hi_weapon_type(
            56_201,
            55,
            vec![(6, 5.0), (54, 10_000.0), (158, 5_000.0), (160, 10.0), (604, 83.0)],
        ),
    );
    // Entropic Disintegrator (group 1986, chargeGroup1 = 1987)
    sde.types.insert(
        56_202,
        hi_weapon_type(
            56_202,
            1986,
            vec![
                (6, 5.0),
                (54, 50_000.0),
                (160, 10.0),
                (114, 10.0),
                (604, 1987.0),
                (2699, 0.25),
                (2700, 1.5),
            ],
        ),
    );
    // Missile Launcher (group 507, chargeGroup1 = 384)
    sde.types.insert(
        56_203,
        hi_weapon_type(
            56_203,
            507,
            vec![(6, 0.0), (54, 50_000.0), (604, 384.0)],
        ),
    );
    // Bomb Launcher (group 862, allowed groups [90, 863, 864])
    sde.types.insert(
        56_204,
        hi_weapon_type(56_204, 862, vec![(54, 30_000.0)]),
    );
    // Defender Launcher (group 512, allowed groups [88])
    sde.types.insert(
        56_205,
        hi_weapon_type(56_205, 512, vec![(54, 10_000.0)]),
    );
    // Category-11 NPC target
    sde.types
        .insert(56_999, make_fighter_type(56_999, 93, 11, 1_000.0, vec![]));
    ctx.sde = Arc::new(sde);

    spawn_ball(&ctx, sys_id, ship_id, 23_915, DVec3::ZERO, 1_000.0, 500.0).await;
    let npc_id = 942_001_i64;
    spawn_ball(
        &ctx,
        sys_id,
        npc_id,
        56_999,
        DVec3::new(1_000.0, 0.0, 0.0),
        5_000.0,
        0.0,
    )
    .await;
    lock(&ctx, sys_id, ship_id, npc_id).await;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Red);
        rt.modified_system_securities.insert(sys_id, 0.2);
    });

    let proj_id = 842_001_i64;
    let entropic_id = 842_002_i64;
    let missile_id = 842_003_i64;
    let bomb_id = 842_004_i64;
    let def_id = 842_005_i64;
    ctx.db.with_mut_snapshot(|snap| {
        for (mid, tid, flg) in [
            (proj_id, 56_201, InventoryFlag::HiSlot0),
            (entropic_id, 56_202, InventoryFlag::HiSlot1),
            (missile_id, 56_203, InventoryFlag::HiSlot2),
            (bomb_id, 56_204, InventoryFlag::HiSlot3),
            (def_id, 56_205, InventoryFlag::HiSlot4),
        ] {
            snap.items.insert(
                mid,
                make_fighter_item(mid, tid, char_id, ship_id, flg, 1),
            );
            snap.module_online_states.insert(mid, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    // 1. Standard turret via full dogmaIM.Activate dispatch (`fitting_ewar_capital::resolve_pending_weapon_autoload`)
    let std_turret_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(proj_id),
                PyRep::String("projectileFired".into()),
                PyRep::Integer(npc_id),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("unloaded standard turret Activate must fail");
    assert_no_charges_user_error(&std_turret_err, proj_id, 56_201, Some(83));

    // 2. Direct weapon handler (`combat_weapons::prepare_direct_weapon_shot`)
    let direct_err = handle_weapons(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(entropic_id), PyRep::Integer(npc_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("unloaded Entropic Disintegrator Activate must fail");
    assert_no_charges_user_error(&direct_err, entropic_id, 56_202, Some(1987));

    // 3. AOE Bomb & Defender launchers (`combat_missiles_aoe::resolve_and_consume_aoe_launcher_charge`)
    let bomb_err = handle_weapons(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(bomb_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("unloaded Bomb Launcher Activate must fail");
    assert_no_charges_user_error(&bomb_err, bomb_id, 56_204, Some(90));

    let def_err = handle_weapons(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(def_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("unloaded Defender Launcher Activate must fail");
    assert_no_charges_user_error(&def_err, def_id, 56_205, Some(88));

    // 4. Legacy 1-arg dogmaIM.Activate(targetID) (`combat_legacy_ess::handle_legacy_and_ess`)
    let legacy_err = crate::services::combat_crimewatch::combat_legacy_ess::handle_legacy_and_ess(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(npc_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("legacy 1-arg Activate with unloaded weapon must fail");
    assert_no_charges_user_error(&legacy_err, proj_id, 56_201, Some(83));

    // 5. Direct missile-shot defensive fallback when `charge_type_opt` or `charge_type_id_opt` is absent (`combat_weapons::activate_and_finalize_missile_shot`)
    let mut fit = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .expect("ship fitting must rebuild for direct missile shot fallback check");
    let missile_mod = fit
        .modules
        .get(&missile_id)
        .cloned()
        .expect("missile launcher module must be fitted");
    let missile_type = ctx
        .sde
        .types
        .get(&56_203)
        .expect("missile launcher type must exist in SDE");
    let attacker_ball = Ball {
        id: ship_id,
        type_id: 23_915,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    for (charge_item_id_opt, charge_type_id_opt) in
        [(Some(842_099), Some(999_888)), (None, None)]
    {
        let missile_err = super::activate_and_finalize_missile_shot(
            &ctx,
            char_id,
            ship_id,
            sys_id,
            1_800_000_000,
            missile_id,
            &missile_mod,
            missile_type,
            &mut fit,
            charge_item_id_opt,
            charge_type_id_opt,
            None,
            &attacker_ball,
            npc_id,
            eve_destiny::DamageVector::default(),
            false,
            false,
            false,
            true,
        )
        .await
        .expect("missile fallback must return Some(Err)")
        .expect_err("missile launcher without resolved SDE charge must fail");
        assert_no_charges_user_error(&missile_err, missile_id, 56_203, Some(384));
    }
}

#[tokio::test]
async fn unloaded_standup_weapons_return_typed_no_charges_user_error() {
    use crate::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
    use crate::services::structure_standup_weapons::{
        try_activate_standup_weapon, GROUP_STANDUP_FLAK_AMMO, GROUP_STANDUP_GUIDED_BOMB,
        GROUP_STANDUP_GUIDED_BOMB_LAUNCHER, GROUP_STANDUP_POINT_DEFENSE_BATTERY, TYPE_KEEPSTAR,
    };

    let (mut ctx, _sess, char_id, _ship_id, _hs_sys) = make_fighter_ctx_and_session();
    let ns_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.0)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759);

    let pdb_tid = 56_211_u32;
    let gbl_tid = 56_212_u32;
    let mut sde = (*ctx.sde).clone();
    sde.types.insert(
        pdb_tid,
        make_fighter_type(
            pdb_tid,
            GROUP_STANDUP_POINT_DEFENSE_BATTERY,
            66,
            500.0,
            vec![
                (6, 500.0),
                (54, 10_000.0),
                (158, 5_000.0),
                (64, 5.0),
                (56, 2.0),
                (73, 10_000.0),
                (604, f64::from(GROUP_STANDUP_FLAK_AMMO)),
                (1970, 1.0),
            ],
        ),
    );
    sde.types.insert(
        gbl_tid,
        make_fighter_type(
            gbl_tid,
            GROUP_STANDUP_GUIDED_BOMB_LAUNCHER,
            66,
            500.0,
            vec![(6, 500.0), (54, 250_000.0), (73, 10_000.0), (1970, 1.0)],
        ),
    );
    ctx.sde = Arc::new(sde);

    let sid = 10_000_970_001_i64;
    let st_corp = 98_701_i64;
    let pdb_mod_id = 842_101_i64;
    let gbl_mod_id = 842_102_i64;
    let target_id = 942_050_i64;
    let st_ball = Ball {
        id: sid,
        type_id: TYPE_KEEPSTAR,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1.0e12,
        radius: 5_000.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };

    let sys_ns = ctx.systems.get_or_spawn(ns_sys);
    spawn_ball(&ctx, ns_sys, sid, TYPE_KEEPSTAR, DVec3::ZERO, 1.0e6, 100_000.0).await;
    spawn_ball(
        &ctx,
        ns_sys,
        target_id,
        587,
        DVec3::new(50_000.0, 0.0, 0.0),
        5_000.0,
        1_000.0,
    )
    .await;
    lock(&ctx, ns_sys, sid, target_id).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            sid,
            eve_db::UpwellStructureRecord {
                structure_id: sid,
                type_id: TYPE_KEEPSTAR,
                owner_corp_id: st_corp,
                solar_system_id: ns_sys,
                name: "Test Keepstar".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1.0e6,
                armor_hp: 1.0e6,
                hull_hp: 1.0e6,
                ..Default::default()
            },
        );
        for (mid, tid, flg) in [
            (pdb_mod_id, pdb_tid, InventoryFlag::HiSlot0),
            (gbl_mod_id, gbl_tid, InventoryFlag::HiSlot1),
        ] {
            snap.items
                .insert(mid, make_fighter_item(mid, tid, st_corp, sid, flg, 1));
            snap.module_online_states.insert(mid, true);
        }
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(sid, char_id);
        save_ext_state(snap, &ext);
    });

    let pdb_err = try_activate_standup_weapon(
        &ctx,
        &sys_ns,
        char_id,
        sid,
        ns_sys,
        pdb_mod_id,
        None,
        &st_ball,
    )
    .await
    .unwrap()
    .expect_err("unloaded Standup Point Defense Battery must fail");
    assert_no_charges_user_error(
        &pdb_err,
        pdb_mod_id,
        pdb_tid,
        Some(GROUP_STANDUP_FLAK_AMMO),
    );

    let gbl_err = try_activate_standup_weapon(
        &ctx,
        &sys_ns,
        char_id,
        sid,
        ns_sys,
        gbl_mod_id,
        Some(target_id),
        &st_ball,
    )
    .await
    .unwrap()
    .expect_err("unloaded Standup Guided Bomb Launcher must fail");
    assert_no_charges_user_error(
        &gbl_err,
        gbl_mod_id,
        gbl_tid,
        Some(GROUP_STANDUP_GUIDED_BOMB),
    );
}
