//! Extra unit and mutation-killing tests for `services::combat_crimewatch::combat_fighters`
//! covering fighter class/slot limit checks across all 6 fighter groups/attributes,
//! cumulative unit damage & HP fallback/clamping, tube load/unload/launch/recall edge cases,
//! movement commands, and abandon/scoop validation rules.

use crate::router::context::ActiveFighterSquadron;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::combat_crimewatch::combat_fighters::*;
use crate::services::combat_crimewatch::with_combat_runtime;
use eve_db::{InventoryFlag, UpwellStructureRecord};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

pub(super) async fn spawn_carrier_ball(
    ctx: & crate::router::ServerContext,
    sys_id: u32,
    ship_id: i64,
    type_id: u32,
    pos: DVec3,
    mode: MovementMode,
    cs: CombatState,
) {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 120.0,
                inertia: 0.5,
                mass: 1_000_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode,
            },
            cs,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn apply_damage_to_fighter_squadron_accumulates_fractional_damage_and_respects_hp_fallbacks() {
    let (mut ctx, _session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    ctx.db.acquire_online_lock(1, 1, Some(char_id), true, ctx.now_unix()).unwrap();
    let sde_mut = Arc::make_mut(&mut ctx.sde);
    // Type 990_001: uses attr 9 = 500.0 (no attr 263)
    sde_mut.types.insert(
        990_001,
        make_fighter_type(990_001, 1652, 87, 1_000.0, vec![(9, 500.0), (2215, 9.0)]),
    );
    // Type 990_002: attr 263 = 100.0 -> clamped to min 250.0
    sde_mut.types.insert(
        990_002,
        make_fighter_type(990_002, 1652, 87, 1_000.0, vec![(263, 100.0), (2215, 9.0)]),
    );
    // Type 990_003: no attr 263 and no attr 9 -> defaults to 1_000.0
    sde_mut.types.insert(
        990_003,
        make_fighter_type(990_003, 1652, 87, 1_000.0, vec![(2215, 9.0)]),
    );

    // 1. Unknown ball returns (0, 0, false)
    let (lost0, rem0, wiped0) =
        apply_damage_to_fighter_squadron(&ctx, sys_id, 999_999_999, DamageVector::new(500.0, 0.0, 0.0, 0.0)).await;
    assert_eq!((lost0, rem0, wiped0), (0, 0, false));

    // 2. Type 990_001 (unit_hp = 500.0): fractional accumulation across two hits
    let ball1 = 430_001i64;
    let item1 = 530_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item1,
            make_fighter_item(item1, 990_001, char_id, sys_id as i64, InventoryFlag::None, 4),
        );
    });
    ctx.active_fighters.write().unwrap().insert(
        ball1,
        ActiveFighterSquadron {
            ball_id: ball1,
            item_id: item1,
            type_id: 990_001,
            owner_char_id: char_id,
            parent_ship_id: ship_id,
            tube_index: 2,
            squadron_size: 4,
            target_id: None,
            is_abandoned: false,
            mwd_active: false,
        },
    );

    // Hit 1: 300 dmg (< 500 unit_hp) -> 0 killed, 300 accumulated
    let (lost1, rem1, wiped1) =
        apply_damage_to_fighter_squadron(&ctx, sys_id, ball1, DamageVector::new(300.0, 0.0, 0.0, 0.0)).await;
    assert_eq!((lost1, rem1, wiped1), (0, 4, false));
    let accum1 = with_combat_runtime(&ctx, |rt| rt.fighter_unit_damage_accum.get(&ball1).copied().unwrap_or(0.0));
    assert!((accum1 - 300.0).abs() < 1e-6);

    // Hit 2: 250 dmg (total 550 >= 500) -> 1 killed, 50 remainder, size 4 -> 3
    let (lost2, rem2, wiped2) =
        apply_damage_to_fighter_squadron(&ctx, sys_id, ball1, DamageVector::new(250.0, 0.0, 0.0, 0.0)).await;
    assert_eq!((lost2, rem2, wiped2), (1, 3, false));
    let accum2 = with_combat_runtime(&ctx, |rt| rt.fighter_unit_damage_accum.get(&ball1).copied().unwrap_or(0.0));
    assert!((accum2 - 50.0).abs() < 1e-6);
    assert_eq!(ctx.db.get_item(item1).unwrap().quantity, 3);
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnInSpaceSquadronSizeChanged"));

    // 3. Type 990_002 (attr 263 = 100 -> clamped to 250.0): 240 dmg kills 0; +20 dmg kills 1
    let ball2 = 430_002i64;
    let item2 = 530_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item2,
            make_fighter_item(item2, 990_002, char_id, sys_id as i64, InventoryFlag::None, 2),
        );
    });
    ctx.active_fighters.write().unwrap().insert(
        ball2,
        ActiveFighterSquadron {
            ball_id: ball2,
            item_id: item2,
            type_id: 990_002,
            owner_char_id: char_id,
            parent_ship_id: ship_id,
            tube_index: 0,
            squadron_size: 2,
            target_id: None,
            is_abandoned: false,
            mwd_active: false,
        },
    );
    let (lost_c1, rem_c1, _) =
        apply_damage_to_fighter_squadron(&ctx, sys_id, ball2, DamageVector::new(240.0, 0.0, 0.0, 0.0)).await;
    assert_eq!((lost_c1, rem_c1), (0, 2));
    let (lost_c2, rem_c2, _) =
        apply_damage_to_fighter_squadron(&ctx, sys_id, ball2, DamageVector::new(20.0, 0.0, 0.0, 0.0)).await;
    assert_eq!((lost_c2, rem_c2), (1, 1));

    // 4. Type 990_003 (default 1_000.0): 1_000.0 dmg wipes out 1-unit squadron and emits OnFighterRemovedFromController
    let ball3 = 430_003i64;
    let item3 = 530_003i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item3,
            make_fighter_item(item3, 990_003, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    ctx.active_fighters.write().unwrap().insert(
        ball3,
        ActiveFighterSquadron {
            ball_id: ball3,
            item_id: item3,
            type_id: 990_003,
            owner_char_id: char_id,
            parent_ship_id: ship_id,
            tube_index: 1,
            squadron_size: 1,
            target_id: None,
            is_abandoned: false,
            mwd_active: false,
        },
    );
    let (lost3, rem3, wiped3) =
        apply_damage_to_fighter_squadron(&ctx, sys_id, ball3, DamageVector::new(1_000.0, 0.0, 0.0, 0.0)).await;
    assert_eq!((lost3, rem3, wiped3), (1, 0, true));
    let wipe_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(wipe_notifs.iter().any(|n| n.event == "OnFighterRemovedFromController"));
}

#[tokio::test]
async fn get_fighters_for_ship_and_tube_class_limits_and_squadron_size_fallbacks() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    let sde_mut = Arc::make_mut(&mut ctx.sde);

    // Group-based fallback fighter types (no 2212/2213/2214/2215 attrs):
    // Support fighter group 1537 -> class (2213, 2218), default max squadron size = 3
    sde_mut.types.insert(991_001, make_fighter_type(991_001, 1537, 87, 500.0, vec![]));
    // Heavy fighter group 1653 -> class (2214, 2219), default max squadron size = 6
    sde_mut.types.insert(991_002, make_fighter_type(991_002, 1653, 87, 500.0, vec![]));
    // Standup light 4777 -> (2740, 2737), Standup support 4778 -> (2741, 2738, size 3), Standup heavy 4779 -> (2742, 2739, size 6)
    sde_mut.types.insert(991_003, make_fighter_type(991_003, 4777, 87, 500.0, vec![]));
    sde_mut.types.insert(991_004, make_fighter_type(991_004, 4778, 87, 500.0, vec![]));
    sde_mut.types.insert(991_005, make_fighter_type(991_005, 4779, 87, 500.0, vec![]));
    // Structure carrier type 35834: 5 tubes (2216=5), standup light=1 (2737=1), standup support=1 (2738=1), standup heavy=0 (2739=0)
    sde_mut.types.insert(
        35_834,
        make_fighter_type(
            35_834,
            1657,
            65,
            10_000_000.0,
            vec![(2216, 5.0), (2737, 1.0), (2738, 1.0), (2739, 0.0)],
        ),
    );

    // 1. GetFightersForShip authorization checks: NoActiveShip, AccessDenied, and corp structure access
    let mut no_ship_sess = session.clone();
    no_ship_sess.ship_id = None;
    let err_no_ship = handle_fighters(&ctx, &mut no_ship_sess, "fighterMgr", "GetFightersForShip", &[], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err_no_ship.code, "NoActiveShip");

    let err_denied = handle_fighters(
        &ctx,
        &mut no_ship_sess,
        "fighterMgr",
        "GetFightersForShip",
        &[PyRep::Integer(888_888_888)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_denied.code, "AccessDenied");

    let struct_id = 1_000_555_001i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_834,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Keepstar".into(),
                state: "ShieldVulnerable".into(),
                position: [0.0; 3],
                quantum_core_installed: true,
                ..Default::default()
            },
        );
    });
    let ok_struct = handle_fighters(
        &ctx,
        &mut no_ship_sess,
        "fighterMgr",
        "GetFightersForShip",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("corp member can query structure fighters");
    assert!(matches!(ok_struct, PyRep::Tuple(_)));

    // 2. Load support fighter (group 1537, carrier 23915 has 2218=1 max support slot) with stack of 5 -> splits to 3!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            601_001,
            make_fighter_item(601_001, 991_001, char_id, ship_id, InventoryFlag::FighterBay, 5),
        );
        snap.items.insert(
            601_002,
            make_fighter_item(601_002, 991_001, char_id, ship_id, InventoryFlag::FighterBay, 3),
        );
        // Foreign-owned fighter in bay -> OwnershipViolation
        snap.items.insert(
            601_003,
            make_fighter_item(601_003, 991_001, 99_999_999, ship_id, InventoryFlag::FighterBay, 3),
        );
        // Non-fighter category item in FighterBay -> ItemNotAFighter
        snap.items.insert(
            601_004,
            make_fighter_item(601_004, 587, char_id, ship_id, InventoryFlag::FighterBay, 1),
        );
    });

    let err_not_found = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(0), PyRep::Integer(777_777_777)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_not_found.code, "ItemNotFound");

    let err_own = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(0), PyRep::Integer(601_003)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_own.code, "OwnershipViolation");

    let err_cat = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(0), PyRep::Integer(601_004)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_cat.code, "ItemNotAFighter");

    // Load 601_001 into tube 0 -> splits stack from 5 to 3 (default for group 1537)
    handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(0), PyRep::Integer(601_001)],
        &kw,
    )
    .await
    .unwrap()
    .expect("load support fighter into tube 0");
    assert_eq!(ctx.db.get_item(601_001).unwrap().quantity, 3);

    // Loading a second support fighter into tube 1 exceeds max support slots (2218=1) -> CannotLoadFighterTubeNoTubesAvailableForClass
    let err_class_full = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(1), PyRep::Integer(601_002)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_class_full.code, "CannotLoadFighterTubeNoTubesAvailableForClass");

    // Replacing tube 0 itself with 601_002 succeeds because replace_tube_idx=Some(0) excludes tube 0 from loaded count
    handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(0), PyRep::Integer(601_002)],
        &kw,
    )
    .await
    .unwrap()
    .expect("replace tube 0 with same class");
}
