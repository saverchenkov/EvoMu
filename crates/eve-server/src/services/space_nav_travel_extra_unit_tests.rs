//! Deep branch-coverage and mutation-killing unit tests for `services::space_destiny::space_nav_travel`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::space_destiny::space_nav_travel::*;
use eve_db::{
    InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository,
    WorldStateSnapshot,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::sync::{Arc, OnceLock};

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn test_ctx() -> ServerContext {
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

pub(super) fn clone_type_with(
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

pub(super) fn put_item(
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

pub(super) fn bound_sys(sys_id: u32, char_id: i64) -> BoundObjectEntry {
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

pub(super) fn ship_ball(ship_id: i64, pos: DVec3) -> Ball {
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
