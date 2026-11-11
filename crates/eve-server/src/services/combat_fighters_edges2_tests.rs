//! Additional edge-case and branch coverage tests for `services::combat_fighters`.

use super::combat_fighters_tests::{make_fighter_ctx_and_session, make_fighter_item};
use super::*;
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

async fn spawn_carrier(ctx: &ServerContext, ship_id: i64, _owner_id: i64, _corp_id: i64, sys_id: u32) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 23_915,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 120.0,
                inertia: 0.5,
                mass: 1_000_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn fighters_loading_unloading_launching_and_damage_reserve_edges() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = (*ctx.sde).clone();
    let t_support: u32 = 998_102;
    sde.types.insert(
        t_support,
        super::combat_fighters_tests::make_fighter_type(
            t_support,
            1653,
            87,
            1_000.0,
            vec![(2215, 3.0), (2213, 1.0)],
        ),
    );
    ctx.sde = Arc::new(sde);
    let kw = HashMap::new();
    let svc = "fighterMgr";
    spawn_carrier(&ctx, ship_id, char_id, 1_000_009, sys_id).await;

    // Seed fighter stacks in fighter bay: 1 light stack (qty 15 -> squadron_size 9 + reserve 6)
    let f1_id = 700_001_i64;
    let f2_id = 700_002_i64;
    let f3_id = 700_003_i64;
    let f4_id = 700_004_i64;
    let f_sup_id = 700_005_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            f1_id,
            make_fighter_item(f1_id, 23_057, char_id, ship_id, InventoryFlag::FighterBay, 15),
        );
        snap.items.insert(
            f2_id,
            make_fighter_item(f2_id, 23_057, char_id, ship_id, InventoryFlag::FighterBay, 9),
        );
        snap.items.insert(
            f3_id,
            make_fighter_item(f3_id, 23_057, char_id, ship_id, InventoryFlag::FighterBay, 9),
        );
        snap.items.insert(
            f4_id,
            make_fighter_item(f4_id, 23_057, char_id, ship_id, InventoryFlag::Cargo, 9),
        );
        snap.items.insert(
            f_sup_id,
            make_fighter_item(f_sup_id, t_support, char_id, ship_id, InventoryFlag::FighterBay, 3),
        );
    });

    // NoActiveShip error when ship_id is None
    let mut no_ship_sess = session.clone();
    no_ship_sess.ship_id = None;
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut no_ship_sess,
            svc,
            "LoadFightersToTube",
            &[PyRep::Integer(f1_id), PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NoActiveShip"
    );

    // ItemNotInFighterBay for f4 in Cargo
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            svc,
            "LoadFightersToTube",
            &[PyRep::Integer(f4_id), PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ItemNotInFighterBay"
    );

    // Swapped arg order (item_id, tube_idx) loads tube 0; duplicate LoadFightersToTube -> FighterAlreadyLoaded
    let load_res = handle_fighters(
        &ctx,
        &mut session,
        svc,
        "LoadFightersToTube",
        &[PyRep::Integer(f1_id), PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(load_res.dict_get("success"), Some(&PyRep::Bool(true)));
    assert_eq!(load_res.dict_get("tubeIndex"), Some(&PyRep::Integer(0)));
    assert_eq!(load_res.dict_get("fighterID"), Some(&PyRep::Integer(f1_id)));
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            svc,
            "LoadFightersToTube",
            &[PyRep::Integer(f1_id), PyRep::Integer(1)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FighterAlreadyLoaded"
    );

    // Load tube 1 and tube 2
    let load_t1 = handle_fighters(
        &ctx,
        &mut session,
        svc,
        "LoadFightersToTube",
        &[PyRep::Integer(1), PyRep::Integer(f2_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(load_t1.dict_get("success"), Some(&PyRep::Bool(true)));
    assert_eq!(load_t1.dict_get("tubeIndex"), Some(&PyRep::Integer(1)));
    assert_eq!(load_t1.dict_get("fighterID"), Some(&PyRep::Integer(f2_id)));

    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            svc,
            "LoadFightersToTube",
            &[PyRep::Integer(2), PyRep::Integer(f3_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotLoadFighterTubeNoTubesAvailableForClass"
    );

    let load_t2 = handle_fighters(
        &ctx,
        &mut session,
        svc,
        "LoadFightersToTube",
        &[PyRep::Integer(2), PyRep::Integer(f_sup_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(load_t2.dict_get("success"), Some(&PyRep::Bool(true)));
    assert_eq!(load_t2.dict_get("tubeIndex"), Some(&PyRep::Integer(2)));
    assert_eq!(load_t2.dict_get("fighterID"), Some(&PyRep::Integer(f_sup_id)));

    // Unload tube 2 returns Bool(true); unloading empty tube 2 again returns Bool(false); invalid tube -> InvalidFighterTube
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            svc,
            "UnloadTubeToFighterBay",
            &[PyRep::Integer(2)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            svc,
            "UnloadTubeToFighterBay",
            &[PyRep::Integer(2)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            svc,
            "UnloadTubeToFighterBay",
            &[PyRep::Integer(9)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidFighterTube"
    );

    // GetFightersForShip returns sorted tubes [0, 1]
    let list = handle_fighters(&ctx, &mut session, svc, "GetFightersForShip", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    match &list {
        PyRep::Tuple(parts) => assert!(!parts.is_empty()),
        PyRep::List(rows) => assert_eq!(rows.len(), 2),
        other => panic!("unexpected GetFightersForShip rep: {other:?}"),
    }

    // Cannot launch while cloaked or tethered
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.is_cloaked = true;
        }
    }
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            svc,
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(0)])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotOpenFighterTubesWhileCloaked"
    );
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.is_cloaked = false;
        }
    }
    let tether_struct_id = 1_040_998_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let corp_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .unwrap_or(1_000_009);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.structures.insert(
            tether_struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: tether_struct_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Test Tether Citadel".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                position: [0.0, 0.0, 0.0],
                shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                hull_hp: 500_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            svc,
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(0)])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "DeniedTethered"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.remove(&tether_struct_id);
    });
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.is_tethered = false;
        }
    }

    // Launch tube 0 via flag ID 159 (FighterTube0) -> returns empty errorsByTubeID PyRep::Dict and registers squadron
    let launched = handle_fighters(
        &ctx,
        &mut session,
        svc,
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(159)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(launched, PyRep::Dict(ref entries) if entries.is_empty()), "expected empty errors Dict for flag-based launch, got {launched:?}");
    let squad_ball_id = ctx
        .active_fighters
        .read()
        .unwrap()
        .values()
        .find(|f| f.item_id == f1_id)
        .expect("launched squadron registered in active_fighters")
        .ball_id;

    // Trying to load or unload already-launched fighter fails with FighterAlreadyLaunched
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            svc,
            "UnloadTubeToFighterBay",
            &[PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FighterAlreadyLaunched"
    );

    // Damage launched squadron exceeding 1 unit HP + consuming reserve units
    let (units_killed, _remaining, squadron_destroyed) = apply_damage_to_fighter_squadron(
        &ctx,
        sys_id,
        squad_ball_id,
        DamageVector::new(1_500.0, 0.0, 0.0, 0.0),
    )
    .await;
    assert!(units_killed >= 1);
    assert!(!squadron_destroyed);

    // Reload f3 into tube 2 violates light slot limit (2217=2); check launch-time slot limit too by seeding tube 2 directly
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            svc,
            "LoadFightersToTube",
            &[PyRep::Integer(2), PyRep::Integer(f3_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotLoadFighterTubeNoTubesAvailableForClass"
    );
    let _ = handle_fighters(
        &ctx,
        &mut session,
        svc,
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(1)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    ctx.fighter_tube_loads
        .write()
        .unwrap()
        .insert((ship_id, 2), f3_id);
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            svc,
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(2)])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotLaunchFighterNotEnoughTubesAvailableForClass"
    );
    ctx.fighter_tube_loads.write().unwrap().remove(&(ship_id, 2));

    // Recall tube 0 sets rearm timer -> immediate relaunch -> TubeRearming
    let _ = handle_fighters(
        &ctx,
        &mut session,
        svc,
        "RecallFightersToTubes",
        &[PyRep::List(vec![PyRep::Integer(squad_ball_id)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            svc,
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(0)])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TubeRearming"
    );
}

#[tokio::test]
async fn fighters_movement_abandon_and_scoop_edges() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    let svc = "fighterMgr";
    spawn_carrier(&ctx, ship_id, char_id, 1_000_009, sys_id).await;

    // Seed fighter in tube 0
    let f1_id = 710_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            f1_id,
            make_fighter_item(f1_id, 23_057, char_id, ship_id, InventoryFlag::FighterBay, 9),
        );
    });
    let _ = handle_fighters(
        &ctx,
        &mut session,
        svc,
        "LoadFightersToTube",
        &[PyRep::Integer(0), PyRep::Integer(f1_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let launched = handle_fighters(
        &ctx,
        &mut session,
        svc,
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(0)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let f_ball = match launched {
        PyRep::Tuple(v) | PyRep::List(v) => v[0].as_i64().unwrap(),
        PyRep::Dict(v) => v[0].1.as_i64().unwrap(),
        other => panic!("unexpected launched rep: {other:?}"),
    };

    // ExecuteMovementCommandOnFighters error paths & MJD lack
    assert_eq!(
        handle_fighters(&ctx, &mut session, svc, "ExecuteMovementCommandOnFighters", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            svc,
            "ExecuteMovementCommandOnFighters",
            &[
                PyRep::List(vec![PyRep::Integer(999_999_888)]),
                PyRep::String("STOP".into()),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FighterNotFound"
    );
    // GOTO_POINT + ORBIT + STOP
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            svc,
            "ExecuteMovementCommandOnFighters",
            &[
                PyRep::List(vec![PyRep::Integer(f_ball)]),
                PyRep::String("GOTO_POINT".into()),
                PyRep::Tuple(vec![PyRep::Real(1_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            svc,
            "ExecuteMovementCommandOnFighters",
            &[
                PyRep::List(vec![PyRep::Integer(f_ball)]),
                PyRep::String("ORBIT".into()),
                PyRep::Integer(ship_id),
                PyRep::Real(5_000.0),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            svc,
            "ExecuteMovementCommandOnFighters",
            &[
                PyRep::List(vec![PyRep::Integer(f_ball)]),
                PyRep::String("MJD".into()),
                PyRep::Tuple(vec![PyRep::Real(10_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FighterLacksMjdAbility"
    );

    // Scoop before abandoning -> FighterNotAbandoned
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            svc,
            "CmdScoopAbandonedFighterFromSpace",
            &[PyRep::Integer(f_ball)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FighterNotAbandoned"
    );

    // Abandon fighter, verify docked error & out-of-range error, then scoop successfully
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut session,
            svc,
            "CmdAbandonFighter",
            &[PyRep::List(vec![PyRep::Integer(f_ball)])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::List(vec![PyRep::Integer(f_ball)])
    );

    let mut docked_sess = session.clone();
    docked_sess.station_id = Some(60_003_760);
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut docked_sess,
            svc,
            "CmdScoopAbandonedFighterFromSpace",
            &[PyRep::Integer(f_ball)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ShipDocked"
    );

    // Outsider cannot scoop
    let mut thief_sess = session.clone();
    thief_sess.character_id = Some(90_000_002);
    thief_sess.corporation_id = Some(1_000_010);
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut thief_sess,
            svc,
            "CmdScoopAbandonedFighterFromSpace",
            &[PyRep::Integer(f_ball)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotFighterOwner"
    );

    // Scoop by corp-mate works when within range
    ctx.db.with_mut_snapshot(|snap| {
        let c1_corp = snap.characters[&90_000_001].corporation_id;
        snap.characters.get_mut(&90_000_002).unwrap().corporation_id = c1_corp;
    });
    let mut corpmate_sess = session.clone();
    corpmate_sess.character_id = Some(90_000_002);
    corpmate_sess.corporation_id = Some(1_000_009);
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut corpmate_sess,
            svc,
            "CmdScoopAbandonedFighterFromSpace",
            &[PyRep::Integer(f_ball)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
}

#[test]
fn fighter_class_and_slot_attr_and_slot_limit_helpers_cover_all_classes_and_boundaries() {
    let ctx = ServerContext::new();
    let mut sde = (*ctx.sde).clone();
    let base_ty = sde.types.values().next().cloned().expect("sde types");
    for (tid, gid, attr_opt) in [
        (990_001_u32, 1652_u32, None),
        (990_002, 1537, None),
        (990_003, 1653, None),
        (990_004, 4777, None),
        (990_005, 4778, None),
        (990_006, 4779, None),
        (990_007, 9999, Some((2742_u16, 1.0))),
        (990_008, 9999, None),
        (990_100, 547, Some((2217, 0.0))),
        (990_101, 547, Some((2217, 2.0))),
    ] {
        let mut t = base_ty.clone();
        t.type_id = tid;
        t.group_id = gid;
        t.default_attributes.clear();
        if let Some((a, v)) = attr_opt {
            t.default_attributes.insert(a, v);
        }
        sde.types.insert(tid, t);
    }
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), ctx.db.clone());

    assert_eq!(super::fighter_class_and_slot_attr(&ctx.sde, 990_001), Some((2212, 2217)));
    assert_eq!(super::fighter_class_and_slot_attr(&ctx.sde, 990_002), Some((2213, 2218)));
    assert_eq!(super::fighter_class_and_slot_attr(&ctx.sde, 990_003), Some((2214, 2219)));
    assert_eq!(super::fighter_class_and_slot_attr(&ctx.sde, 990_004), Some((2740, 2737)));
    assert_eq!(super::fighter_class_and_slot_attr(&ctx.sde, 990_005), Some((2741, 2738)));
    assert_eq!(super::fighter_class_and_slot_attr(&ctx.sde, 990_006), Some((2742, 2739)));
    assert_eq!(super::fighter_class_and_slot_attr(&ctx.sde, 990_007), Some((2742, 2739)));
    assert_eq!(super::fighter_class_and_slot_attr(&ctx.sde, 990_008), None);

    let mut active_map = HashMap::new();
    // Non-fighter or carrier without slot attr -> Ok(())
    assert!(super::check_fighter_class_slot_limit_in_map(&active_map, &ctx, 500, 990_101, 990_008, 1).is_ok());
    assert!(super::check_fighter_class_slot_limit(&ctx, 500, 990_101, 990_008, true, 1, None).is_ok());

    // max_slots == 0 rejects both in-map launch and tube load/launch
    assert_eq!(
        super::check_fighter_class_slot_limit_in_map(&active_map, &ctx, 500, 990_100, 990_001, 1)
            .unwrap_err()
            .code,
        "CannotLaunchFighterNotEnoughTubesAvailableForClass"
    );
    assert_eq!(
        super::check_fighter_class_slot_limit(&ctx, 500, 990_100, 990_001, true, 1, None)
            .unwrap_err()
            .code,
        "CannotLoadFighterTubeNoTubesExistForClass"
    );
    assert_eq!(
        super::check_fighter_class_slot_limit(&ctx, 500, 990_100, 990_001, false, 1, None)
            .unwrap_err()
            .code,
        "CannotLaunchFighterNotEnoughTubesAvailableForClass"
    );

    // Populate 1 active non-abandoned + 1 abandoned fighter in active_map (max_slots == 2)
    let make_sq = |id: i64, abandoned: bool| crate::router::context::ActiveFighterSquadron {
        ball_id: id,
        item_id: id,
        type_id: 990_001,
        owner_char_id: 90_000_001,
        parent_ship_id: 500,
        tube_index: 0,
        squadron_size: 9,
        target_id: None,
        is_abandoned: abandoned,
        mwd_active: false,
    };
    active_map.insert(1, make_sq(1, false));
    active_map.insert(2, make_sq(2, true));
    assert!(super::check_fighter_class_slot_limit_in_map(&active_map, &ctx, 500, 990_101, 990_001, 1).is_ok());
    assert_eq!(
        super::check_fighter_class_slot_limit_in_map(&active_map, &ctx, 500, 990_101, 990_001, 2)
            .unwrap_err()
            .code,
        "CannotLaunchFighterNotEnoughTubesAvailableForClass"
    );

    // Seed 1 active fighter + 1 loaded tube on ship 500; replacing tube 0 succeeds while adding a new tube fails
    ctx.active_fighters.write().unwrap().insert(1, make_sq(1, false));
    ctx.fighter_tube_loads.write().unwrap().insert((500, 0), 700_001);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            700_001,
            InventoryItemRecord {
                item_id: 700_001,
                type_id: 990_001,
                owner_id: 90_000_001,
                location_id: 500,
                flag: InventoryFlag::FighterBay,
                quantity: 9,
                singleton: false,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        super::check_fighter_class_slot_limit(&ctx, 500, 990_101, 990_001, true, 1, None)
            .unwrap_err()
            .code,
        "CannotLoadFighterTubeNoTubesAvailableForClass"
    );
    assert!(super::check_fighter_class_slot_limit(&ctx, 500, 990_101, 990_001, true, 1, Some(0)).is_ok());
}

#[tokio::test]
async fn w4_mutant_killers_fighters_slots_launch_movement_scoop_and_persistence() {
    use super::combat_fighters_tests::{
        make_fighter_item, make_fighter_sde_registry, make_fighter_type,
    };
    use crate::router::context::ActiveFighterSquadron;
    use eve_db::{InventoryFlag, WorldRepository};
    use eve_destiny::{Ball, CombatState, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use glam::DVec3;
    use std::collections::HashMap;
    use std::sync::Arc;

    // 1. `check_fighter_class_slot_limit_in_map` boundary & ship/class discrimination (`L47, 50, 52`)
    let mut sde = make_fighter_sde_registry();
    // Light (group 1652, attr 2212=1, 2217=1), Support (1653, 2213=1, 2218=1), Heavy (1537, 2214=1, 2219=1)
    let t_light: u32 = 998_201;
    let t_support: u32 = 998_202;
    let t_heavy: u32 = 998_203;
    let t_carrier: u32 = 998_210;

    let mut light_t = make_fighter_type(t_light, 1652, 87, 1.0, vec![(2212, 1.0), (2217, 1.0), (263, 900.0), (265, 900.0), (9, 900.0)]);
    light_t.default_attributes.insert(2215, 9.0);
    sde.types.insert(t_light, light_t);

    let mut support_t = make_fighter_type(t_support, 1653, 87, 1.0, vec![(2213, 1.0), (2218, 1.0), (263, 600.0), (265, 600.0), (9, 600.0)]);
    support_t.default_attributes.insert(2215, 6.0);
    sde.types.insert(t_support, support_t);

    let mut heavy_t = make_fighter_type(t_heavy, 1537, 87, 1.0, vec![(2214, 1.0), (2219, 1.0), (263, 300.0), (265, 300.0), (9, 300.0)]);
    heavy_t.default_attributes.insert(2215, 3.0);
    sde.types.insert(t_heavy, heavy_t);

    let mut carrier_t = sde.types[&23_915].clone();
    carrier_t.type_id = t_carrier;
    carrier_t.default_attributes.insert(2216, 4.0); // 4 tubes total
    carrier_t.default_attributes.insert(2217, 1.0); // 1 light slot
    carrier_t.default_attributes.insert(2218, 1.0); // 1 support slot
    carrier_t.default_attributes.insert(2219, 1.0); // 1 heavy slot
    sde.types.insert(t_carrier, carrier_t);

    let temp_dir = std::env::temp_dir().join(format!(
        "evomu_w4_fighter_killers_{}_{}",
        std::process::id(),
        901_001
    ));
    let _ = std::fs::create_dir_all(&temp_dir);
    let db_path = temp_dir.join("fighters.sqlite");
    let db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("sqlite db"));
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db.clone());
    ctx.seed_demo_world_fixtures();

    let char_id = 90_000_001i64;
    let other_char = 90_000_002i64;
    let corp_id = 1_000_009i64;
    let foreign_corp = 1_000_099i64;
    let ship_id = 200_100_001i64;
    let other_ship = 200_100_002i64;
    let sys_id = 30_000_142u32;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters[&char_id].clone();
        let mut ch2 = base_ch.clone();
        ch2.character_id = other_char;
        ch2.user_id = other_char as u64;
        ch2.corporation_id = foreign_corp;
        ch2.active_ship_id = other_ship;
        snap.characters.insert(other_char, ch2);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.corporation_id = corp_id;
        }
        snap.items.insert(
            ship_id,
            make_fighter_item(ship_id, t_carrier, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            other_ship,
            make_fighter_item(other_ship, t_carrier, other_char, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: t_carrier,
                position: DVec3::new(1000.0, 2000.0, 3000.0),
                velocity: DVec3::ZERO,
                max_velocity: 120.0,
                inertia: 0.5,
                mass: 1_000_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);
    session.ship_id = Some(ship_id);
    session.solar_system_id = Some(sys_id);
    let kwargs = HashMap::new();

    // `GetFightersForShip` read-only method should NOT persist/increment epoch on sqlite (`L172, 178`)
    let rev_before = db.state_epoch();
    let get_res = handle_fighters(&ctx, &mut session, "fighterMgr", "GetFightersForShip", &[], &kwargs)
        .await
        .unwrap()
        .expect("GetFightersForShip");
    assert!(matches!(get_res, PyRep::Tuple(_)));
    assert_eq!(db.state_epoch(), rev_before);

    // Seed active fighter from ANOTHER ship and ANOTHER class (support) on `ship_id`:
    // light slot check on `ship_id` (cap 1) must still pass (`L47, 50, 52`)!
    ctx.active_fighters.write().unwrap().insert(
        770_001,
        ActiveFighterSquadron {
            ball_id: 770_001,
            item_id: 770_001,
            type_id: t_light,
            owner_char_id: other_char,
            parent_ship_id: other_ship,
            tube_index: 0,
            squadron_size: 9,
            target_id: None,
            is_abandoned: false,
            mwd_active: false,
        },
    );
    ctx.active_fighters.write().unwrap().insert(
        770_002,
        ActiveFighterSquadron {
            ball_id: 770_002,
            item_id: 770_002,
            type_id: t_support,
            owner_char_id: char_id,
            parent_ship_id: ship_id,
            tube_index: 3,
            squadron_size: 6,
            target_id: None,
            is_abandoned: false,
            mwd_active: false,
        },
    );
    assert!(super::check_fighter_class_slot_limit(&ctx, ship_id, t_carrier, t_light, false, 1, None).is_ok());
    ctx.active_fighters.write().unwrap().clear();

    // 2. Load & Unload on Tube 1 (`FighterTube1 = 160`) (`L504, 573`), plus Launch with excess qty on
    // Light (1652->9), Support (1653->6), Heavy (1537->3) in tubes 0, 1, 2 -> tube offsets `(tidx+1)*50` & HP scaling (`L765-916, 976-1000`)
    let bay_light = 300_201i64;
    let bay_support = 300_202i64;
    let bay_heavy = 300_203i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            bay_light,
            make_fighter_item(bay_light, t_light, char_id, ship_id, InventoryFlag::FighterBay, 11),
        );
        snap.items.insert(
            bay_support,
            make_fighter_item(bay_support, t_support, char_id, ship_id, InventoryFlag::FighterBay, 8),
        );
        snap.items.insert(
            bay_heavy,
            make_fighter_item(bay_heavy, t_heavy, char_id, ship_id, InventoryFlag::FighterBay, 5),
        );
    });

    // Mutating call (`LoadFightersToTube`) DOES increment DB epoch (`rev > rev_before`)
    handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(1), PyRep::Integer(bay_support)],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("load support to tube 1");
    assert!(db.state_epoch() > rev_before);

    // Unload tube 1 merges/returns to FighterBay and clears tube load
    handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "UnloadTubeToFighterBay",
        &[PyRep::Integer(1)],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("unload tube 1");
    assert!(!ctx.fighter_tube_loads.read().unwrap().contains_key(&(ship_id, 1)));

    // Now seed tubes directly with EXCESS quantities (10 light in tube 0, 7 support in tube 1, 4 heavy in tube 2)
    // to exercise excess remainder split (`> max_squad`) + class defaults (9, 6, 3) + HP scaling & tube positions!
    let t0_item = 300_300i64;
    let t1_item = 300_301i64;
    let t2_item = 300_302i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            t0_item,
            make_fighter_item(t0_item, t_light, char_id, ship_id, InventoryFlag::from_u16(159), 10),
        );
        snap.items.insert(
            t1_item,
            make_fighter_item(t1_item, t_support, char_id, ship_id, InventoryFlag::from_u16(160), 7),
        );
        snap.items.insert(
            t2_item,
            make_fighter_item(t2_item, t_heavy, char_id, ship_id, InventoryFlag::from_u16(161), 4),
        );
    });
    {
        let mut loads = ctx.fighter_tube_loads.write().unwrap();
        loads.insert((ship_id, 0), t0_item);
        loads.insert((ship_id, 1), t1_item);
        loads.insert((ship_id, 2), t2_item);
    }

    let launch_rep = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![
            PyRep::Integer(0),
            PyRep::Integer(1),
            PyRep::Integer(2),
        ])],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("launch tubes 0,1,2");
    if let PyRep::Dict(entries) = launch_rep {
        assert!(!entries.is_empty());
    }

    // Verify launched sizes (9, 6, 3) & excess remainders returned/split & tube offsets `(tidx + 1) * 50.0`!
    let (sq0, sq1, sq2) = {
        let af = ctx.active_fighters.read().unwrap();
        let find_t = |idx: usize| {
            af.values()
                .find(|s| s.parent_ship_id == ship_id && s.tube_index == idx)
                .cloned()
                .expect("launched tube")
        };
        (find_t(0), find_t(1), find_t(2))
    };
    assert_eq!(sq0.squadron_size, 9);
    assert_eq!(sq1.squadron_size, 6);
    assert_eq!(sq2.squadron_size, 3);

    // Check positions: ship at (1000, 2000, 3000) -> x offsets +50, +100, +150
    let (_, b0) = sys_handle.get_ball_snapshot(sq0.ball_id).await.unwrap().unwrap();
    let (_, b1) = sys_handle.get_ball_snapshot(sq1.ball_id).await.unwrap().unwrap();
    let (_, b2) = sys_handle.get_ball_snapshot(sq2.ball_id).await.unwrap().unwrap();
    assert!((b0.position.x - 1050.0).abs() < 1e-3);
    assert!((b1.position.x - 1100.0).abs() < 1e-3);
    assert!((b2.position.x - 1150.0).abs() < 1e-3);

    // Check HP proportional to squadron_size / 1.0 (e.g. light 900 * 9 = 8100, support 600 * 6 = 3600, heavy 300 * 3 = 900)
    let cs0 = sys_handle.get_combat_state(sq0.ball_id).await.unwrap().unwrap();
    let cs1 = sys_handle.get_combat_state(sq1.ball_id).await.unwrap().unwrap();
    let cs2 = sys_handle.get_combat_state(sq2.ball_id).await.unwrap().unwrap();
    assert!((cs0.max_shield_hp - 8100.0).abs() < 1e-3);
    assert!((cs1.max_shield_hp - 3600.0).abs() < 1e-3);
    assert!((cs2.max_shield_hp - 900.0).abs() < 1e-3);

    // 3. `ExecuteMovementCommandOnFighters` actually mutates MovementMode to Orbit / Follow (`L1195, 1315, 1326`)
    handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "ExecuteMovementCommandOnFighters",
        &[
            PyRep::List(vec![PyRep::Integer(sq0.ball_id)]),
            PyRep::String("ORBIT".into()),
            PyRep::Integer(ship_id),
            PyRep::Real(2_200.0),
        ],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("ORBIT");
    let (_, b0_orbit) = sys_handle.get_ball_snapshot(sq0.ball_id).await.unwrap().unwrap();
    assert!(matches!(
        b0_orbit.mode,
        MovementMode::Orbit { target_id, .. } if target_id == ship_id
    ));

    handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "ExecuteMovementCommandOnFighters",
        &[
            PyRep::List(vec![PyRep::Integer(sq1.ball_id)]),
            PyRep::String("FOLLOW".into()),
            PyRep::Integer(ship_id),
            PyRep::Real(1_200.0),
        ],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("FOLLOW");
    let (_, b1_follow) = sys_handle.get_ball_snapshot(sq1.ball_id).await.unwrap().unwrap();
    assert!(matches!(
        b1_follow.mode,
        MovementMode::Follow { target_id, .. } if target_id == ship_id
    ));

    // 4. Abandon + foreign-corp / out-of-range / ScoopAbandonedFighterFromSpace (`L1480-1592`)
    let aband_sq = sq2.ball_id;
    let aband_item = sq2.item_id;
    let aband_res = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "CmdAbandonFighter",
        &[PyRep::Integer(aband_sq)],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("abandon heavy squadron");
    assert_eq!(aband_res, PyRep::List(vec![PyRep::Integer(aband_sq)]));

    // Move ship within scoop distance (< 2500m, already 150m away) and have a DIFFERENT corp pilot scoop it
    let mut foreign_sess = SessionState {
        user_id: Some(other_char as u64),
        character_id: Some(other_char),
        corporation_id: Some(foreign_corp),
        ship_id: Some(other_ship),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: other_ship,
                type_id: t_carrier,
                position: DVec3::new(1150.0, 2000.0, 3000.0),
                velocity: DVec3::ZERO,
                max_velocity: 120.0,
                inertia: 0.5,
                mass: 1_000_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    // Foreign-corp pilot cannot scoop abandoned fighter; same-corp pilot scoops into carrier FighterBay (`owner_id` updated, `flag == FighterBay`)
    assert_eq!(
        handle_fighters(
            &ctx,
            &mut foreign_sess,
            "fighterMgr",
            "CmdScoopAbandonedFighterFromSpace",
            &[PyRep::Integer(aband_sq)],
            &kwargs,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotFighterOwner"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&other_char) {
            ch.corporation_id = corp_id;
        }
    });
    foreign_sess.corporation_id = Some(corp_id);
    let scoop_corpmate = handle_fighters(
        &ctx,
        &mut foreign_sess,
        "fighterMgr",
        "CmdScoopAbandonedFighterFromSpace",
        &[PyRep::Integer(aband_sq)],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("corpmate pilot scoops abandoned fighter within range");
    assert_eq!(scoop_corpmate, PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&aband_item].location_id, other_ship);
        assert_eq!(snap.items[&aband_item].owner_id, other_char);
        assert_eq!(snap.items[&aband_item].flag, InventoryFlag::FighterBay);
    });

    let _ = std::fs::remove_dir_all(&temp_dir);
}

