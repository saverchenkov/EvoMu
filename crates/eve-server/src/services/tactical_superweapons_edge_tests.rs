//! Comprehensive edge, error, and happy-path unit tests for `tactical_modules_superweapon_ops`.

#![forbid(unsafe_code)]

use super::*;
use crate::system_actor::ActiveModuleCycle;
use eve_db::{InventoryFlag, UpwellStructureRecord};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode, SlimItemMeta, TargetLock};
use glam::DVec3;

async fn spawn_ball_with_state(
    ctx: &ServerContext,
    sys_id: u32,
    ball_id: i64,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    owner_id: i64,
    pos: DVec3,
    cs: CombatState,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ball_id,
        type_id,
        position: pos,
        velocity: DVec3::new(10.0, 0.0, 0.0),
        max_velocity: 1_000.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 200.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let meta = SlimItemMeta {
        group_id,
        category_id,
        owner_id,
        char_id: Some(owner_id),
        corp_id: 98_000_001,
        alliance_id: 0,
        security_status: 5.0,
        is_cloaked: cs.is_cloaked,
        ..Default::default()
    };
    sys.spawn_ball_with_meta(ball, cs, meta)
        .await
        .expect("spawn ball");
}

#[tokio::test]
async fn superweapon_ship_sec_module_tether_and_fuel_split_errors() {
    let ctx = test_ctx_with_custom_types(|sde| {
        if let Some(mut sys) = sde.solar_systems.get(&30_000_142).cloned() {
            sys.solar_system_id = 30_002_601;
            sys.security_status = 0.25; // Low-sec
            sde.solar_systems.insert(30_002_601, sys.clone());
            sys.solar_system_id = 30_002_602;
            sys.security_status = -0.6; // Null-sec
            sde.solar_systems.insert(30_002_602, sys);
        }
        sde.types.insert(
            999_301,
            custom_inv_type(sde, 999_301, 30, 6, "Edge Titan", &[]),
        );
        sde.types.insert(
            999_302,
            custom_inv_type(sde, 999_302, 4594, 6, "Edge Lancer", &[(5418, 20.0), (5417, 10.0)]),
        );
        sde.types.insert(
            999_303,
            custom_inv_type(
                sde,
                999_303,
                588,
                7,
                "Edge Doomsday",
                &[
                    (6, 5_000.0),
                    (54, 150_000.0),
                    (114, 10_000.0),
                    (713, 16274.0),
                    (714, 100.0),
                    (2262, 10_000.0),
                    (2263, 2_500.0),
                    (2264, 10_000.0),
                    (2265, 1_000.0),
                    (2427, 300_000.0),
                ],
            ),
        );
        sde.types.insert(
            77399,
            custom_inv_type(
                sde,
                77399,
                588,
                7,
                "Edge Lance",
                &[
                    (6, 10_000.0),
                    (54, 100_000.0),
                    (73, 300_000.0),
                    (114, 1_000.0),
                    (713, 16274.0),
                    (714, 100.0),
                    (2259, 10_000.0),
                    (2263, 2_500.0),
                    (2264, 5_000.0),
                    (2265, 1_000.0),
                    (2260, 2_000.0),
                ],
            ),
        );
        sde.types.insert(
            999_304,
            custom_inv_type(sde, 999_304, 53, 7, "Not A Superweapon", &[]),
        );
        sde.types.insert(
            999_305,
            custom_inv_type(sde, 999_305, 485, 6, "Target Dread", &[]),
        );
    });

    let char_id = 95_100_001;
    let vic_char = 95_100_002;
    let low_sys = 30_002_601;
    let null_sys = 30_002_602;
    let frig_id = 1_000_500_001;
    let titan_id = 1_000_500_002;
    let lancer_id = 1_000_500_003;
    let dd_mod = 1_000_500_010;
    let bad_mod = 1_000_500_011;
    let lance_mod = 1_000_500_012;
    let siege_mod = 1_000_500_013;
    let dread_id = 1_000_500_020;

    ensure_char(&ctx, char_id, 98_000_001, null_sys, titan_id, 2);
    ensure_char(&ctx, vic_char, 98_000_002, null_sys, dread_id, 1);

    insert_item(&ctx, frig_id, 587, char_id, null_sys as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, titan_id, 999_301, char_id, null_sys as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, lancer_id, 999_302, char_id, low_sys as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, dd_mod, 999_303, char_id, titan_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, bad_mod, 999_304, char_id, titan_id, InventoryFlag::HiSlot1, 1, true);
    insert_item(&ctx, lance_mod, 77399, char_id, lancer_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, siege_mod, 20280, char_id, lancer_id, InventoryFlag::HiSlot1, 1, true);
    insert_item(&ctx, dread_id, 999_305, vic_char, null_sys as i64, InventoryFlag::None, 1, true);

    // 1. InvalidShipGroup when non-Titan/non-Lancer uses non-lance module
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, frig_id, null_sys, dd_mod, "Activate", Some(dread_id))
            .await
            .unwrap_err()
            .code,
        "InvalidShipGroup"
    );

    // 2. Titan in low-sec -> CannotActivateLanceInLowSec
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, low_sys, dd_mod, "Activate", Some(dread_id))
            .await
            .unwrap_err()
            .code,
        "CannotActivateLanceInLowSec"
    );

    // 3. ModuleNotFound when module type is absent from SDE, and InvalidModuleGroup when missing/wrong group
    insert_item(&ctx, 88_000_099, 999_888_777, char_id, titan_id, InventoryFlag::HiSlot2, 1, true);
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, 88_000_099, "Activate", Some(dread_id))
            .await
            .unwrap_err()
            .code,
        "ModuleNotFound"
    );
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, 9_999_999, "Activate", Some(dread_id))
            .await
            .unwrap_err()
            .code,
        "InvalidModuleGroup"
    );

    // 4. InvalidModuleGroup when module group != 588
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, bad_mod, "Activate", Some(dread_id))
            .await
            .unwrap_err()
            .code,
        "InvalidModuleGroup"
    );

    // 5. ShipNotInSpace for Titan & Lancer before ball spawn
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, dd_mod, "Activate", Some(dread_id))
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(siege_mod);
    });
    grant_skill(&ctx, char_id, 77739, 5);
    grant_skill(&ctx, char_id, 76079, 4); // fallback lancer hull skill 76079!
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, lancer_id, low_sys, lance_mod, "ActivateSlashModule", None)
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // 6. Tether guard: tethered Titan & check_superweapon_tether_guard
    let mut tethered_cs = CombatState::default();
    tethered_cs.capacitor = 100_000.0;
    tethered_cs.max_capacitor = 100_000.0;
    tethered_cs.is_tethered = true;
    spawn_ball_with_state(&ctx, null_sys, titan_id, 999_301, 30, 6, char_id, DVec3::ZERO, tethered_cs.clone()).await;
    assert_eq!(
        check_superweapon_tether_guard(&ctx, null_sys, titan_id).await.unwrap_err().code,
        "CantActivateWhileTethered"
    );
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, dd_mod, "Activate", Some(dread_id))
            .await
            .unwrap_err()
            .code,
        "CantActivateWhileTethered"
    );

    // Lancer tethered in low-sec -> CantActivateWhileTethered, then untethered with low cap -> NotEnoughCapacitor
    spawn_ball_with_state(&ctx, low_sys, lancer_id, 999_302, 4594, 6, char_id, DVec3::ZERO, tethered_cs).await;
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, lancer_id, low_sys, lance_mod, "ActivateSlashModule", None)
            .await
            .unwrap_err()
            .code,
        "CantActivateWhileTethered"
    );
    let low_sys_h = ctx.systems.get_or_spawn(low_sys);
    let mut untethered_low_cap = CombatState::default();
    untethered_low_cap.capacitor = 100.0;
    untethered_low_cap.max_capacitor = 100_000.0;
    low_sys_h.set_combat_state(lancer_id, untethered_low_cap).await.unwrap();
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, lancer_id, low_sys, lance_mod, "ActivateSlashModule", None)
            .await
            .unwrap_err()
            .code,
        "NotEnoughCapacitor"
    );

    // Lancer in low-sec with sufficient cap (reduced by skill 77739 lvl 5: 10_000 * 0.75 = 7_500 GJ) + split fuel (Locked ignored, FuelBay prioritized over Cargo)
    low_sys_h.set_capacitor(lancer_id, 8_000.0).await.expect("set_capacitor");
    insert_item(&ctx, 1_000_500_050, 16274, char_id, lancer_id, InventoryFlag::Locked, 500, false);
    insert_item(&ctx, 1_000_500_051, 16274, char_id, lancer_id, InventoryFlag::SpecializedFuelBay, 40, false);
    insert_item(&ctx, 1_000_500_052, 16274, char_id, lancer_id, InventoryFlag::Cargo, 30, false);
    // Partial fuel available (70 < 100) -> "Insufficient lance fuel ... need 100, have 70"
    let err_partial = activate_superweapon_or_lance(&ctx, char_id, lancer_id, low_sys, lance_mod, "ActivateSlashModule", None)
        .await
        .unwrap_err();
    assert_eq!(err_partial.code, "InsufficientFuel");
    assert!(err_partial.message.contains("Insufficient lance fuel"));
    assert!(err_partial.message.contains("have 70"));

    // Top up Cargo stack to 80 (total 40 FuelBay + 80 Cargo = 120) -> consumes all 40 from FuelBay first + 60 from Cargo!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&1_000_500_052).unwrap().quantity = 80;
    });
    let lance_ok = activate_superweapon_or_lance(&ctx, char_id, lancer_id, low_sys, lance_mod, "ActivateSlashModule", None)
        .await
        .expect("lancer allowed in low-sec with skill cap reduction");
    assert_eq!(lance_ok.dict_get("capacitorCost").and_then(|v| v.as_f64()), Some(7_500.0));
    assert_eq!(lance_ok.dict_get("fuelConsumed").and_then(|v| v.as_i64()), Some(100));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&1_000_500_051), "depleted FuelBay stack removed");
        assert_eq!(snap.items.get(&1_000_500_052).map(|i| i.quantity), Some(20), "60 taken from Cargo");
        assert_eq!(snap.items.get(&1_000_500_050).map(|i| i.quantity), Some(500), "Locked stack untouched");
    });
}

#[tokio::test]
async fn single_target_doomsday_invulnerability_range_cap_structure_and_destruction() {
    let ctx = test_ctx_with_custom_types(|sde| {
        if let Some(mut sys) = sde.solar_systems.get(&30_000_142).cloned() {
            sys.solar_system_id = 30_002_603;
            sys.security_status = -0.7;
            sde.solar_systems.insert(30_002_603, sys);
        }
        sde.types.insert(999_311, custom_inv_type(sde, 999_311, 30, 6, "DD Titan", &[]));
        sde.types.insert(
            999_312,
            custom_inv_type(
                sde,
                999_312,
                588,
                7,
                "Targeted Doomsday",
                &[
                    (6, 5_000.0),
                    (54, 100_000.0),
                    (114, 25_000.0),
                    (713, 16274.0),
                    (714, 100.0),
                    (2427, 300_000.0),
                ],
            ),
        );
        sde.types.insert(999_313, custom_inv_type(sde, 999_313, 485, 6, "Target Cap", &[]));
    });

    let char_id = 95_100_010;
    let vic_char = 95_100_011;
    let null_sys = 30_002_603;
    let titan_id = 1_000_500_101;
    let dd_mod = 1_000_500_102;
    let dd_mod2 = 1_000_500_103;
    let subcap_id = 1_000_500_110;
    let cap_id = 1_000_500_111;
    let invuln_struct_id = 1_000_500_112;

    ensure_char(&ctx, char_id, 98_000_001, null_sys, titan_id, 2);
    ensure_char(&ctx, vic_char, 98_000_002, null_sys, cap_id, 1);

    insert_item(&ctx, titan_id, 999_311, char_id, null_sys as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, dd_mod, 999_312, char_id, titan_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, dd_mod2, 999_312, char_id, titan_id, InventoryFlag::HiSlot1, 1, true);
    insert_item(&ctx, subcap_id, 587, vic_char, null_sys as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, cap_id, 999_313, vic_char, null_sys as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, 1_000_500_150, 16274, char_id, titan_id, InventoryFlag::Cargo, 500, false);

    let mut titan_cs = CombatState::default();
    titan_cs.capacitor = 2_000.0; // below 5_000.0 DD cost
    titan_cs.max_capacitor = 100_000.0;
    spawn_ball_with_state(&ctx, null_sys, titan_id, 999_311, 30, 6, char_id, DVec3::ZERO, titan_cs).await;

    let mut cloaked_cs = CombatState::default();
    cloaked_cs.is_cloaked = true;
    cloaked_cs.shield_hp = 5_000.0;
    cloaked_cs.armor_hp = 5_000.0;
    cloaked_cs.structure_hp = 5_000.0;
    spawn_ball_with_state(&ctx, null_sys, cap_id, 999_313, 485, 6, vic_char, DVec3::new(10_000.0, 0.0, 0.0), cloaked_cs).await;
    spawn_ball_with_state(&ctx, null_sys, subcap_id, 587, 25, 6, vic_char, DVec3::new(5_000.0, 0.0, 0.0), CombatState::default()).await;

    let sys = ctx.systems.get_or_spawn(null_sys);
    // 1. Cloaked target -> CannotTargetCloaked
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, dd_mod, "Activate", Some(cap_id))
            .await
            .unwrap_err()
            .code,
        "CannotTargetCloaked"
    );

    // 2. Tethered / gate_cloak / undock_invuln -> TargetInvulnerable
    let mut tethered_target_cs = CombatState::default();
    tethered_target_cs.shield_hp = 5_000.0;
    tethered_target_cs.armor_hp = 5_000.0;
    tethered_target_cs.structure_hp = 5_000.0;
    tethered_target_cs.is_tethered = true;
    sys.set_combat_state(cap_id, tethered_target_cs.clone()).await.unwrap();
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, dd_mod, "Activate", Some(cap_id))
            .await
            .unwrap_err()
            .code,
        "TargetInvulnerable"
    );
    tethered_target_cs.is_tethered = false;
    sys.set_combat_state(cap_id, tethered_target_cs).await.unwrap();

    // 3. TargetNotLocked
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, dd_mod, "Activate", Some(cap_id))
            .await
            .unwrap_err()
            .code,
        "TargetNotLocked"
    );

    // Lock both subcap & cap
    for &tid in &[subcap_id, cap_id, invuln_struct_id] {
        let _ = sys
            .add_target_lock(
                titan_id,
                TargetLock {
                    target_id: tid,
                    lock_time_ms: 100.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: Some(500_000.0),
                },
            )
            .await;
    }

    // 4. Non-capital subcap target -> InvalidTarget
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, dd_mod, "Activate", Some(subcap_id))
            .await
            .unwrap_err()
            .code,
        "InvalidTarget"
    );

    // 5. Out-of-range capital -> TargetOutOfRange
    sys.set_ball_position(cap_id, DVec3::new(300_000.0, 0.0, 0.0)).await;
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, dd_mod, "Activate", Some(cap_id))
            .await
            .unwrap_err()
            .code,
        "TargetOutOfRange"
    );
    sys.set_ball_position(cap_id, DVec3::new(10_000.0, 0.0, 0.0)).await;

    // 6. Location-change invulnerable target character -> TargetInvulnerable
    crate::services::space_destiny::grant_location_change_invulnerability(&ctx, vic_char, ctx.now_unix_secs() + 30);
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, dd_mod, "Activate", Some(cap_id))
            .await
            .unwrap_err()
            .code,
        "TargetInvulnerable"
    );
    crate::services::space_destiny::break_location_change_invulnerability(&ctx, vic_char);

    // 7. Invulnerable structure (Sov IHub type 32458 in ShieldVulnerable) -> StructureInvulnerable
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            invuln_struct_id,
            UpwellStructureRecord {
                structure_id: invuln_struct_id,
                type_id: 32458,
                owner_corp_id: 98_000_002,
                solar_system_id: null_sys,
                name: "Sov Hub".into(),
                position: [15_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                fuel_blocks: 100,
                ..Default::default()
            },
        );
    });
    spawn_ball_with_state(&ctx, null_sys, invuln_struct_id, 32458, 4645, 65, 98_000_002, DVec3::new(15_000.0, 0.0, 0.0), CombatState::default()).await;
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, dd_mod, "Activate", Some(invuln_struct_id))
            .await
            .unwrap_err()
            .code,
        "StructureInvulnerable"
    );

    // 8. Dynamic attr 2110 <= 0.0 on target -> TargetInvulnerable
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&cap_id).unwrap().dynamic_attributes.insert(2110, 0.0);
    });
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, dd_mod, "Activate", Some(cap_id))
            .await
            .unwrap_err()
            .code,
        "TargetInvulnerable"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&cap_id).unwrap().dynamic_attributes.remove(&2110);
    });

    // 9. NotEnoughCapacitor on single-target DD when titan_cs.capacitor < 5000
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, dd_mod, "Activate", Some(cap_id))
            .await
            .unwrap_err()
            .code,
        "NotEnoughCapacitor"
    );

    // 10. Happy path with lethal damage (cap has low HP -> destroyed == true) + notifications
    sys.set_capacitor(titan_id, 50_000.0).await.expect("set_capacitor");
    let mut fragile_cs = CombatState::default();
    fragile_cs.shield_hp = 500.0;
    fragile_cs.max_shield_hp = 500.0;
    fragile_cs.armor_hp = 500.0;
    fragile_cs.max_armor_hp = 500.0;
    fragile_cs.structure_hp = 500.0;
    fragile_cs.max_structure_hp = 500.0;
    sys.set_combat_state(cap_id, fragile_cs).await.unwrap();

    let _ = ctx.drain_notifications_for_char(char_id);
    let dd_res = activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, dd_mod, "Activate", Some(cap_id))
        .await
        .expect("lethal single-target doomsday");
    assert_eq!(dd_res.dict_get("totalDamageApplied").and_then(|v| v.as_f64()), Some(25_000.0));
    assert_eq!(dd_res.dict_get("immobilizeSeconds").and_then(|v| v.as_f64()), Some(30.0));
    let notes = ctx.drain_notifications_for_char(char_id);
    assert!(notes.iter().any(|n| n.event == "OnGodmaShipEffect"));
    assert!(notes.iter().any(|n| n.event == "OnDamageMessage"));
    assert!(notes.iter().any(|n| n.event == "OnDamageMessages"));

    // 11. Slash mode with non-difference direction fallback (p1 == p0 -> uses p0), hits nearby combatant only, no OnDamageMessage
    let args_same = vec![
        PyRep::Integer(dd_mod2),
        PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
    ];
    let hit_ship = 1_000_500_120;
    let miss_ship = 1_000_500_121;
    spawn_ball_with_state(&ctx, null_sys, hit_ship, 587, 25, 6, vic_char, DVec3::new(20_000.0, 500.0, 0.0), CombatState::default()).await;
    spawn_ball_with_state(&ctx, null_sys, miss_ship, 587, 25, 6, vic_char, DVec3::new(20_000.0, 10_000.0, 0.0), CombatState::default()).await;
    let slash_ok = activate_superweapon_with_args(&ctx, char_id, titan_id, null_sys, dd_mod2, "ActivateSlashModule", None, &args_same)
        .await
        .expect("slash mode with equal endpoints");
    let hit_list = slash_ok
        .dict_get("hitTargetIDs")
        .and_then(|v| match v {
            PyRep::List(items) => Some(items.iter().filter_map(|i| i.as_i64()).collect::<Vec<_>>()),
            _ => None,
        })
        .unwrap_or_default();
    assert!(hit_list.contains(&hit_ship));
    assert!(!hit_list.contains(&miss_ship));
    let slash_notes = ctx.drain_notifications_for_char(char_id);
    assert!(slash_notes.iter().any(|n| n.event == "OnGodmaShipEffect"));
    assert!(!slash_notes.iter().any(|n| n.event == "OnDamageMessage"));
}

#[tokio::test]
async fn fire_lance_warmup_in_ballpark_edge_cases() {
    let ctx = test_ctx_with_custom_types(|sde| {
        sde.types.insert(
            77398,
            custom_inv_type(sde, 77398, 588, 7, "Warmup Lance", &[(54, 50_000.0), (552, 1_000.0), (2263, 2_500.0), (2427, 300_000.0)]),
        );
    });

    let mut bp = eve_destiny::Ballpark::new(30_002_604);
    let cyc_missing = ActiveModuleCycle {
        ship_id: 999,
        module_id: 1,
        module_type_id: 77398,
        owner_char_id: Some(1),
        damage_vector: DamageVector::new(10_000.0, 0.0, 0.0, 0.0),
        lance_beam_dir: Some(DVec3::X),
        ..Default::default()
    };
    let mut pending = Vec::new();
    let mut destroyed = Vec::new();
    fire_lance_warmup_in_ballpark(
        &mut bp,
        &cyc_missing,
        &Some(ctx.sde.clone()),
        &Some(ctx.db.clone()),
        &ctx.tactical_state,
        &mut pending,
        &mut destroyed,
    );
    assert!(pending.is_empty());

    // Add caster + 3 targets: cloaked (skipped), behind caster (skipped), fragile small-sig (scaled + destroyed!)
    let caster_id = 2_000_001;
    let cloaked_id = 2_000_002;
    let behind_id = 2_000_003;
    let frag_id = 2_000_004;
    for (id, pos) in [
        (caster_id, DVec3::ZERO),
        (cloaked_id, DVec3::new(5_000.0, 0.0, 0.0)),
        (behind_id, DVec3::new(-5_000.0, 0.0, 0.0)),
        (frag_id, DVec3::new(10_000.0, 0.0, 0.0)),
    ] {
        bp.add_ball(Ball {
            id,
            type_id: 587,
            position: pos,
            velocity: DVec3::new(50.0, 0.0, 0.0),
            max_velocity: 500.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 50.0,
            speed_fraction: 0.5,
            mode: MovementMode::Stop,
        });
        bp.ball_slim_meta.insert(id, SlimItemMeta { group_id: 25, category_id: 6, owner_id: 10, ..Default::default() });
    }
    bp.combat_states.insert(caster_id, CombatState { is_tethered: true, ..Default::default() });
    bp.combat_states.insert(cloaked_id, CombatState { is_cloaked: true, shield_hp: 1_000.0, max_shield_hp: 1_000.0, ..Default::default() });
    bp.combat_states.insert(behind_id, CombatState { shield_hp: 1_000.0, max_shield_hp: 1_000.0, ..Default::default() });
    bp.combat_states.insert(frag_id, CombatState {
        signature_radius: 100.0, // 100/1000 = 0.1x sig scale -> 10_000 * 0.1 = 1_000 damage
        shield_hp: 100.0,
        max_shield_hp: 100.0,
        armor_hp: 100.0,
        max_armor_hp: 100.0,
        structure_hp: 100.0,
        max_structure_hp: 100.0,
        ..Default::default()
    });

    let cyc = ActiveModuleCycle {
        ship_id: caster_id,
        module_id: 77,
        module_type_id: 77398,
        owner_char_id: None, // verifies no PendingCycleAction pushed when owner_char_id is None
        damage_vector: DamageVector::new(10_000.0, 0.0, 0.0, 0.0),
        lance_beam_dir: Some(DVec3::X),
        ..Default::default()
    };
    fire_lance_warmup_in_ballpark(
        &mut bp,
        &cyc,
        &Some(ctx.sde.clone()),
        &Some(ctx.db.clone()),
        &ctx.tactical_state,
        &mut pending,
        &mut destroyed,
    );
    assert!(pending.is_empty());
    assert_eq!(destroyed, vec![(frag_id, None)]);
    assert_eq!(bp.combat_states[&cloaked_id].shield_hp, 1_000.0);
    assert_eq!(bp.combat_states[&behind_id].shield_hp, 1_000.0);
    assert_eq!(bp.balls[&caster_id].velocity, DVec3::ZERO);
    assert_eq!(bp.balls[&caster_id].speed_fraction, 0.0);
    assert!((bp.combat_states[&caster_id].lance_disrupted_remaining_s - 285.0).abs() < 1e-6);
}
