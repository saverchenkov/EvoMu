//! Deep branch-coverage and mutation-killing unit tests for `services::space_destiny::space_nav_travel`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::space_destiny::space_nav_travel::*;
use eve_db::types::UpwellStructureRecord;
use eve_db::{
    BookmarkRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository,
    WorldStateSnapshot,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn clone_type_with(
    sde: &mut Arc<SdeRegistry>,
    base_tid: u32,
    new_tid: u32,
    group_id: u32,
    category_id: u32,
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg
        .types
        .get(&base_tid)
        .cloned()
        .or_else(|| reg.types.values().next().cloned())
        .expect("base sde type");
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("NavExtraType_{new_tid}");
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

fn put_item(
    snap: &mut WorldStateSnapshot,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) {
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity,
            singleton: quantity == 1,
            version: 1,
            ..Default::default()
        },
    );
}

fn bound_sys(sys_id: u32, char_id: i64) -> BoundObjectEntry {
    BoundObjectEntry {
        bound_id: 1,
        session_id: 745,
        user_id: 50,
        character_id: char_id,
        service: "beyonce".to_string(),
        object_id: sys_id as i64,
        oid_str: format!("N=1:{sys_id}"),
        ref_id: 1,
    }
}

fn ship_ball(ship_id: i64, pos: DVec3) -> Ball {
    Ball {
        id: ship_id,
        type_id: 588,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_200_000.0,
        radius: 40.0,
        speed_fraction: 0.5,
        mode: MovementMode::Stop,
    }
}

#[test]
fn jump_drive_skills_pochven_multistack_fuel_fatigue_decay_and_timer_errors() {
    let mut ctx = test_ctx();
    let char_id = 96_801_i64;
    let ship_id = 78_801_i64;
    let iso_tid = 17_887_u32;
    let cap_tid = 99_850_u32;
    let mult_tid = 99_851_u32;

    let can_jump = attr_id_by_name(&ctx, "canJump").unwrap_or(866);
    let jd_range = attr_id_by_name(&ctx, "jumpDriveRange").unwrap_or(868);
    let jd_amount = attr_id_by_name(&ctx, "jumpDriveConsumptionAmount").unwrap_or(869);
    let jd_type = attr_id_by_name(&ctx, "jumpDriveConsumptionType").unwrap_or(867);
    let jf_mult = attr_id_by_name(&ctx, "jumpFatigueMultiplier").unwrap_or(1971);

    assert!(attr_id_by_name(&ctx, "nonExistentDogmaAttr").is_none());
    assert!(type_attr(&ctx, 999_999_999, "canJump").is_none());

    // Find two low-sec systems and a Pochven system (region 10_000_070)
    let mut low_sys: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status < 0.45 && s.solar_system_id < 31_000_000 && s.region_id != 10_000_070)
        .map(|s| s.solar_system_id)
        .collect();
    low_sys.sort_unstable();
    let cur_sys = low_sys[0];
    let dst_sys = low_sys
        .iter()
        .copied()
        .find(|&sid| sid != cur_sys && distance_ly(&ctx, cur_sys, sid) > 1.0 && distance_ly(&ctx, cur_sys, sid) <= 8.0)
        .unwrap();
    let dist = distance_ly(&ctx, cur_sys, dst_sys);

    // Configure cap_tid with base range = dist * 0.6 (so level 0 fails JumpDistanceOutOfRange, level 5 = 1.2 * dist succeeds!)
    clone_type_with(
        &mut ctx.sde,
        23757,
        cap_tid,
        547,
        6,
        &[
            (can_jump, 1.0),
            (jd_range, dist * 0.6),
            (jd_amount, 100.0),
            (jd_type, f64::from(iso_tid)),
        ],
    );
    clone_type_with(
        &mut ctx.sde,
        23757,
        mult_tid,
        547,
        6,
        &[(jf_mult, 0.20)], // 1.0 - 0.20 = 0.80 reduction bonus
    );
    assert!((ship_fatigue_reduction_bonus(&ctx, mult_tid) - 0.80).abs() < 1e-6);

    // Group ID fallbacks for ship_fatigue_reduction_bonus
    for (idx, grp) in [883, 28, 380, 1202, 513, 29, 31, 941].into_iter().enumerate() {
        let tid = 99_860 + idx as u32;
        clone_type_with(&mut ctx.sde, 588, tid, grp, 6, &[]);
        assert!((ship_fatigue_reduction_bonus(&ctx, tid) - 0.90).abs() < 1e-6);
    }

    // Wormhole source and Zarzakh source checks
    assert_eq!(
        plan_jump_drive(&ctx, char_id, cap_tid, 31_000_005, dst_sys)
            .unwrap_err()
            .code,
        "JumpDriveForbiddenInWormholeSpace"
    );
    assert_eq!(
        plan_jump_drive(&ctx, char_id, cap_tid, 30_100_000, dst_sys)
            .unwrap_err()
            .code,
        "JumpDriveForbiddenInZarzakh"
    );

    // Pochven destination check
    if let Some(poch_sys) = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.region_id == 10_000_070)
        .map(|s| s.solar_system_id)
    {
        assert_eq!(
            plan_jump_drive(&ctx, char_id, cap_tid, cur_sys, poch_sys)
                .unwrap_err()
                .code,
            "CannotJumpToPochven"
        );
    }

    // Without Jump Drive Calibration skill -> JumpDistanceOutOfRange
    assert_eq!(
        plan_jump_drive(&ctx, char_id, cap_tid, cur_sys, dst_sys)
            .unwrap_err()
            .code,
        "JumpDistanceOutOfRange"
    );

    // Grant Jump Drive Calibration V and Jump Fuel Conservation V
    let jdc_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.name == "Jump Drive Calibration")
        .map(|t| t.type_id)
        .unwrap();
    let jfc_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.name == "Jump Fuel Conservation")
        .map(|t| t.type_id)
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().unwrap();
        ch.character_id = char_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        let smap = snap.skills.entry(char_id).or_default();
        smap.insert(
            jdc_id,
            TrainedSkillRecord {
                type_id: jdc_id,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    let plan_no_jfc = plan_jump_drive(&ctx, char_id, cap_tid, cur_sys, dst_sys).expect("jdc V ok");
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            jfc_id,
            TrainedSkillRecord {
                type_id: jfc_id,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    let plan_with_jfc = plan_jump_drive(&ctx, char_id, cap_tid, cur_sys, dst_sys).expect("jfc V ok");
    assert!(plan_with_jfc.fuel_units < plan_no_jfc.fuel_units);

    // InsufficientFuel when fuel bay is empty, then multi-stack consumption across FuelBay + Cargo
    assert_eq!(
        consume_jump_fuel(&ctx, ship_id, &plan_with_jfc)
            .unwrap_err()
            .code,
        "InsufficientFuel"
    );
    let half = (plan_with_jfc.fuel_units / 2).max(1);
    let rest = plan_with_jfc.fuel_units - half + 15;
    ctx.db.with_mut_snapshot(|snap| {
        put_item(
            snap,
            78_810,
            iso_tid,
            char_id,
            ship_id,
            InventoryFlag::SpecializedFuelBay,
            half,
        );
        put_item(
            snap,
            78_811,
            iso_tid,
            char_id,
            ship_id,
            InventoryFlag::Cargo,
            rest,
        );
    });
    consume_jump_fuel(&ctx, ship_id, &plan_with_jfc).expect("multi-stack fuel ok");
    ctx.db.with_snapshot(|snap| {
        let remaining: Vec<i32> = [78_810, 78_811]
            .into_iter()
            .filter_map(|id| snap.items.get(&id).map(|i| i.quantity))
            .collect();
        assert_eq!(remaining, vec![15]);
    });

    // current_fatigue_min decay & handle_get_jump_timers error branches
    let now = ctx.now_unix();
    let (_, fat) = apply_jump_fatigue(&ctx, char_id, dist);
    let decayed = current_fatigue_min(&ctx, char_id, now + 120);
    assert!((decayed - (fat - 2.0).max(0.0)).abs() < 1e-6);

    let mut sess = SessionState::new(745);
    sess.user_id = Some(50);
    sess.character_id = Some(char_id);
    assert!(handle_get_jump_timers(&ctx, &sess, &[]).is_ok());
    assert_eq!(
        handle_get_jump_timers(&ctx, &sess, &[PyRep::String("bad".into())])
            .unwrap_err()
            .code,
        "InvalidArguments"
    );
    assert_eq!(
        handle_get_jump_timers(&ctx, &sess, &[PyRep::Integer(char_id + 1)])
            .unwrap_err()
            .code,
        "CharacterNotInSession"
    );
}

#[tokio::test]
async fn resolve_target_position_and_destiny_command_validation_branches() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 96_810_i64;
    let ship_id = 78_820_i64;
    let target_id = 78_821_i64;
    let st_id = 1_049_820_001_i64;
    let bm_id = 6_001_i64;

    // 1. resolve_system_target_position across station, stargate, celestial, structure, and <= 0
    assert!(resolve_system_target_position(&ctx, sys_id, char_id, 0).await.is_none());
    assert!(resolve_system_target_position(&ctx, sys_id, char_id, 999_999_999).await.is_none());

    if let Some(st) = ctx.sde.stations.values().find(|s| s.solar_system_id == sys_id) {
        assert_eq!(
            resolve_system_target_position(&ctx, sys_id, char_id, st.station_id).await,
            Some(st.position)
        );
    }
    if let Some(gate) = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == sys_id) {
        assert_eq!(
            resolve_system_target_position(&ctx, sys_id, char_id, gate.stargate_id).await,
            Some(gate.position)
        );
    }
    if let Some(cel) = ctx.sde.celestials_in_system(sys_id).into_iter().next() {
        assert_eq!(
            resolve_system_target_position(&ctx, sys_id, char_id, cel.celestial_id).await,
            Some(cel.position)
        );
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                name: "Nav Struct".into(),
                state: "ShieldVulnerable".into(),
                position: [25_000.0, -10_000.0, 5_000.0],
                shield_hp: 100.0,
                armor_hp: 100.0,
                hull_hp: 100.0,
                quantum_core_installed: true,
                fuel_blocks: 100,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.bookmarks.insert(
            bm_id,
            BookmarkRecord {
                bookmark_id: bm_id,
                owner_id: char_id,
                label: "BM".into(),
                solar_system_id: sys_id,
                x: 50_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });
    assert_eq!(
        resolve_system_target_position(&ctx, sys_id, char_id, st_id).await,
        Some(DVec3::new(25_000.0, -10_000.0, 5_000.0))
    );

    let bound = bound_sys(sys_id, char_id);
    let mut sess = SessionState::new(745);
    sess.user_id = Some(50);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    // 2. Docked session rejected on GotoDirection, Stop, SetSpeedFraction, Orbit, AlignTo
    let mut docked = sess.clone();
    docked.station_id = Some(60_003_760);
    assert_eq!(
        handle_cmd_goto_direction(&ctx, &docked, Some(&bound), &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)])
            .await
            .unwrap_err()
            .code,
        "NotInSpace"
    );
    assert_eq!(
        handle_cmd_stop(&ctx, &docked, Some(&bound)).await.unwrap_err().code,
        "NotInSpace"
    );
    assert_eq!(
        handle_cmd_set_speed_fraction(&ctx, &docked, Some(&bound), &[PyRep::Real(0.5)])
            .await
            .unwrap_err()
            .code,
        "NotInSpace"
    );
    assert_eq!(
        handle_cmd_orbit_or_follow(&ctx, &docked, Some(&bound), "CmdOrbit", &[PyRep::Integer(target_id)])
            .await
            .unwrap_err()
            .code,
        "NotInSpace"
    );
    assert_eq!(
        handle_cmd_align_to(&ctx, &docked, Some(&bound), &[PyRep::Integer(target_id)], &HashMap::new())
            .await
            .unwrap_err()
            .code,
        "NotInSpace"
    );

    // 3. ShipNotInSpace before ball is spawned
    assert_eq!(
        handle_cmd_goto_direction(&ctx, &sess, Some(&bound), &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h.spawn_ball(ship_ball(ship_id, DVec3::ZERO)).await;
    let _ = sys_h
        .spawn_ball(ship_ball(target_id, DVec3::new(20_000.0, 0.0, 0.0)))
        .await;

    // 4. Invalid arguments across movement commands
    assert_eq!(
        handle_cmd_goto_point(&ctx, &sess, Some(&bound), &[PyRep::Real(f64::NAN), PyRep::Real(0.0), PyRep::Real(0.0)])
            .await
            .unwrap_err()
            .code,
        "InvalidArguments"
    );
    assert_eq!(
        handle_cmd_goto_direction(&ctx, &sess, Some(&bound), &[PyRep::Real(f64::INFINITY), PyRep::Real(0.0), PyRep::Real(0.0)])
            .await
            .unwrap_err()
            .code,
        "InvalidArguments"
    );
    assert_eq!(
        handle_cmd_set_speed_fraction(&ctx, &sess, Some(&bound), &[PyRep::Real(-0.5)])
            .await
            .unwrap_err()
            .code,
        "InvalidSpeedFraction"
    );
    assert_eq!(
        handle_cmd_orbit_or_follow(&ctx, &sess, Some(&bound), "CmdOrbit", &[PyRep::Integer(0)])
            .await
            .unwrap_err()
            .code,
        "InvalidTarget"
    );
    assert_eq!(
        handle_cmd_orbit_or_follow(&ctx, &sess, Some(&bound), "CmdOrbit", &[PyRep::Integer(ship_id)])
            .await
            .unwrap_err()
            .code,
        "InvalidTarget"
    );
    assert_eq!(
        handle_cmd_orbit_or_follow(&ctx, &sess, Some(&bound), "CmdOrbit", &[PyRep::Integer(999_999_999)])
            .await
            .unwrap_err()
            .code,
        "TargetNotFound"
    );
    assert_eq!(
        handle_cmd_orbit_or_follow(&ctx, &sess, Some(&bound), "CmdOrbit", &[PyRep::Integer(target_id), PyRep::Real(-10.0)])
            .await
            .unwrap_err()
            .code,
        "InvalidRange"
    );

    // 5. AlignTo validation: <= 0, own ship, Abyssal bookmark, Zarzakh bookmark, and valid bookmarkID kwarg
    assert_eq!(
        handle_cmd_align_to(&ctx, &sess, Some(&bound), &[PyRep::Integer(0)], &HashMap::new())
            .await
            .unwrap_err()
            .code,
        "InvalidTarget"
    );
    assert_eq!(
        handle_cmd_align_to(&ctx, &sess, Some(&bound), &[PyRep::Integer(ship_id)], &HashMap::new())
            .await
            .unwrap_err()
            .code,
        "InvalidTarget"
    );
    let mut kw_bm = HashMap::new();
    kw_bm.insert("bookmarkID".to_string(), PyRep::Integer(bm_id));
    assert!(handle_cmd_align_to(&ctx, &sess, Some(&bound), &[], &kw_bm).await.is_ok());

    let abyss_sys = 32_000_005_u32;
    let abyss_h = ctx.systems.get_or_spawn(abyss_sys);
    let _ = abyss_h.spawn_ball(ship_ball(ship_id, DVec3::ZERO)).await;
    let abyss_bound = bound_sys(abyss_sys, char_id);
    assert_eq!(
        handle_cmd_align_to(&ctx, &sess, Some(&abyss_bound), &[], &kw_bm).await.unwrap_err().code,
        "CannotWarpInAbyssalSpace"
    );

    let zarz_sys = 30_100_000_u32;
    let zarz_h = ctx.systems.get_or_spawn(zarz_sys);
    let _ = zarz_h.spawn_ball(ship_ball(ship_id, DVec3::ZERO)).await;
    let zarz_bound = bound_sys(zarz_sys, char_id);
    assert_eq!(
        handle_cmd_align_to(&ctx, &sess, Some(&zarz_bound), &[], &kw_bm).await.unwrap_err().code,
        "DunBlacklistCannotWarp"
    );

    let _ = sys_h
        .set_combat_state(ship_id, CombatState { mjfg_spool_remaining_s: 5.0, ..Default::default() })
        .await;
    assert_eq!(handle_cmd_stop(&ctx, &sess, Some(&bound)).await.unwrap_err().code, "CantMoveWhileModuleActive");
}

#[tokio::test]
async fn sync_tether_disallow_conditions_and_decloak_velocity_restoration() {
    let mut ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let corp_id = 98_444_001_i64;
    let char_id = 96_850_i64;
    let ship_id = 78_850_i64;
    let struct_id = 1_040_850_001_i64;
    let struct_tid = 35_832_u32;
    let proto_cloak_tid = 99_870_u32;

    if let Some(tid) = attr_id_by_name(&ctx, "tetheringRange") {
        clone_type_with(&mut ctx.sde, struct_tid, struct_tid, 1657, 65, &[(tid, 15_000.0)]);
    }
    let vel_attr = crate::services::space_navigation_advanced::max_velocity_cloak_attr_id(&ctx.sde).unwrap_or(564);
    clone_type_with(&mut ctx.sde, 11370, proto_cloak_tid, 330, 7, &[(vel_attr, 0.25)]);

    let cloak_item_id = 78_851_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().unwrap();
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        put_item(snap, ship_id, 588, char_id, sys_id as i64, InventoryFlag::Hangar, 1);
        put_item(snap, cloak_item_id, proto_cloak_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1);
        snap.module_active_states.insert(cloak_item_id);
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: struct_tid,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Tether Citadel".into(),
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

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(ship_ball(ship_id, DVec3::new(2_000.0, 0.0, 0.0)))
        .await;

    // 1. Activate non-covert cloak: cloak_original_max_velocity = 300 / 0.25 = 1200.0 and ship is tethered
    sync_ship_cloak_and_mjfg_from_fitting(&ctx, char_id, sys_id, ship_id).await;
    let cs_cloaked = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_cloaked.is_cloaked);
    assert!(!cs_cloaked.is_covert_cloak);
    assert!(cs_cloaked.is_tethered);
    assert_eq!(cs_cloaked.cloak_original_max_velocity, Some(1200.0));

    // 2. Deactivate cloak: original max_velocity (1200.0) is restored
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cloak_item_id);
    });
    sync_ship_cloak_and_mjfg_from_fitting(&ctx, char_id, sys_id, ship_id).await;
    let (_, b_uncloaked) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let cs_uncloaked = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_uncloaked.is_cloaked);
    assert!((b_uncloaked.max_velocity - 1200.0).abs() < 1e-6);

    // 3. Lance disruption breaks tether
    let mut cs_lance = cs_uncloaked;
    cs_lance.lance_disrupted_remaining_s = 10.0;
    let _ = sys_h.set_combat_state(ship_id, cs_lance).await;
    sync_tether(&ctx, char_id, sys_id, ship_id).await;
    let cs_after = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_after.is_tethered);
}
