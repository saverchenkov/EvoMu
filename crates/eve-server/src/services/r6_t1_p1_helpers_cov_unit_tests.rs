//! Unit tests covering `r6_t1_p1_helpers` (`validate_and_activate_fighter_mwd`,
//! `is_target_invulnerable_or_tethered`, `step_active_breacher_dots`,
//! `apply_preserved_combat_state`, `spawn_distinct_exit_wormhole`,
//! `parse_fighter_point_target_args`).

use super::*;
use crate::router::{ActiveFighterSquadron, ServerContext};
use crate::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime, ActiveBreacherDot};
use eve_db::WorldRepository;
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_proto::PyRep;
use eve_sde::{FighterAbilityCharges, FighterAbilitySlot, SdeRegistry};
use glam::DVec3;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[tokio::test]
async fn fighter_mwd_validation_cooldown_charges_and_speed_boost_work() {
    let mut sde_cloned = (*shared_sde()).clone();
    let custom_fighter_type = 99_901u32;
    let no_mwd_type = 99_902u32;
    sde_cloned.fighter_abilities_by_type.insert(
        custom_fighter_type,
        vec![FighterAbilitySlot {
            slot_index: 1,
            ability_id: 4, // MicroWarpdrive
            cooldown_seconds: Some(15.0),
            charges: Some(FighterAbilityCharges {
                charge_count: 1,
                rearm_time_seconds: 10.0,
            }),
        }],
    );
    sde_cloned.fighter_abilities_by_type.insert(
        no_mwd_type,
        vec![FighterAbilitySlot {
            slot_index: 0,
            ability_id: 1,
            cooldown_seconds: Some(5.0),
            charges: None,
        }],
    );

    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde_cloned),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let sys_id = 30_000_142u32;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let char_id = 98_400_001i64;
    let fid1 = 98_400_101i64;
    let fid2 = 98_400_102i64;
    let fid3 = 98_400_103i64;

    // Empty slice rejected
    assert_eq!(
        validate_and_activate_fighter_mwd(&ctx, char_id, &sys_handle, &[])
            .await
            .expect_err("empty")
            .code,
        "NoFightersSelected"
    );

    // Missing fighter rejected
    assert_eq!(
        validate_and_activate_fighter_mwd(&ctx, char_id, &sys_handle, &[fid1])
            .await
            .expect_err("missing")
            .code,
        "FighterNotFound"
    );

    {
        let mut fighters = ctx.active_fighters.write().unwrap();
        fighters.insert(
            fid1,
            ActiveFighterSquadron {
                ball_id: fid1,
                item_id: fid1,
                type_id: custom_fighter_type,
                owner_char_id: char_id,
                parent_ship_id: 98_400_999,
                tube_index: 0,
                squadron_size: 9,
                target_id: None,
                is_abandoned: false,
                mwd_active: false,
            },
        );
        fighters.insert(
            fid2,
            ActiveFighterSquadron {
                ball_id: fid2,
                item_id: fid2,
                type_id: custom_fighter_type,
                owner_char_id: char_id + 1,
                parent_ship_id: 98_400_998,
                tube_index: 0,
                squadron_size: 9,
                target_id: None,
                is_abandoned: false,
                mwd_active: false,
            },
        );
        fighters.insert(
            fid3,
            ActiveFighterSquadron {
                ball_id: fid3,
                item_id: fid3,
                type_id: no_mwd_type,
                owner_char_id: char_id,
                parent_ship_id: 98_400_999,
                tube_index: 1,
                squadron_size: 9,
                target_id: None,
                is_abandoned: false,
                mwd_active: false,
            },
        );
    }

    // Wrong owner / unsupported ability rejected
    assert_eq!(
        validate_and_activate_fighter_mwd(&ctx, char_id, &sys_handle, &[fid2])
            .await
            .expect_err("not owned")
            .code,
        "FighterNotOwned"
    );
    assert_eq!(
        validate_and_activate_fighter_mwd(&ctx, char_id, &sys_handle, &[fid3])
            .await
            .expect_err("no mwd ability")
            .code,
        "UnsupportedFighterAbility"
    );

    let _ = sys_handle
        .spawn_ball(Ball {
            id: fid1,
            type_id: custom_fighter_type,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 1_000.0,
            inertia: 1.0,
            mass: 1_000.0,
            radius: 10.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // First activation succeeds and boosts velocity + marks mwd_active
    validate_and_activate_fighter_mwd(&ctx, char_id, &sys_handle, &[fid1])
        .await
        .expect("mwd activation ok");
    assert!(ctx.active_fighters.read().unwrap().get(&fid1).unwrap().mwd_active);
    let (_, b_after) = sys_handle.get_ball_snapshot(fid1).await.unwrap().unwrap();
    assert!(b_after.max_velocity > 1_000.0);

    // Immediate second call rejected due to cooldown
    assert_eq!(
        validate_and_activate_fighter_mwd(&ctx, char_id, &sys_handle, &[fid1])
            .await
            .expect_err("cooldown")
            .code,
        "AbilityOnCooldown"
    );

    // Clear cooldown so single charge exhaustion is tested
    ctx.fighter_cooldown_state.write().unwrap().clear();
    assert_eq!(
        validate_and_activate_fighter_mwd(&ctx, char_id, &sys_handle, &[fid1])
            .await
            .expect_err("out of charges")
            .code,
        "OutOfCharges"
    );
}

#[tokio::test]
async fn breacher_dots_invulnerability_jump_state_wormhole_and_point_parsing() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let sys_id = 30_000_142u32;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let target_id = 98_500_101i64;
    let tethered_id = 98_500_102i64;
    let now_unix = ctx.now_unix();

    let mut cs_vuln = CombatState::default();
    cs_vuln.max_shield_hp = 500.0;
    cs_vuln.shield_hp = 500.0;
    cs_vuln.max_armor_hp = 500.0;
    cs_vuln.armor_hp = 500.0;
    cs_vuln.max_structure_hp = 500.0;
    cs_vuln.structure_hp = 500.0;
    let _ = sys_handle.set_combat_state(target_id, cs_vuln.clone()).await;

    let mut cs_teth = cs_vuln.clone();
    cs_teth.is_tethered = true;
    let _ = sys_handle.set_combat_state(tethered_id, cs_teth.clone()).await;

    assert!(!is_target_invulnerable_or_tethered(&ctx, target_id, &cs_vuln, now_unix));
    assert!(is_target_invulnerable_or_tethered(&ctx, tethered_id, &cs_teth, now_unix));

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_breacher_dots.insert(
            target_id,
            ActiveBreacherDot {
                target_ball_id: target_id,
                source_char_id: 90_000_001,
                source_ship_id: 98_500_999,
                weapon_type_id: 2873,
                charge_type_id: 192,
                flat_cap_dps: 50.0,
                pct_cap_fraction: 0.05,
                remaining_s: 2.0,
                tick_damage: 0.0,
            },
        );
        rt.active_breacher_dots.insert(
            tethered_id,
            ActiveBreacherDot {
                target_ball_id: tethered_id,
                source_char_id: 90_000_001,
                source_ship_id: 98_500_999,
                weapon_type_id: 2873,
                charge_type_id: 192,
                flat_cap_dps: 50.0,
                pct_cap_fraction: 0.05,
                remaining_s: 0.5,
                tick_damage: 0.0,
            },
        );
    });

    step_active_breacher_dots(&ctx, &sys_handle, sys_id, 1.0).await;
    let after_cs = sys_handle.get_combat_state(target_id).await.unwrap().unwrap();
    assert!(after_cs.shield_hp < 500.0);
    // Tethered dot expired after dt = 1.0
    assert!(with_combat_runtime(&ctx, |rt| !rt.active_breacher_dots.contains_key(&tethered_id)));

    // apply_preserved_combat_state
    let mut prev = CombatState::default();
    prev.max_shield_hp = 100.0;
    prev.shield_hp = 50.0;
    prev.max_armor_hp = 200.0;
    prev.armor_hp = 100.0;
    prev.max_structure_hp = 300.0;
    prev.structure_hp = 150.0;
    prev.max_capacitor = 400.0;
    prev.capacitor = 200.0;
    let mut dest = CombatState::default();
    dest.max_shield_hp = 200.0;
    dest.max_armor_hp = 400.0;
    dest.max_structure_hp = 600.0;
    dest.max_capacitor = 800.0;
    apply_preserved_combat_state(&mut dest, Some(prev));
    assert_eq!(dest.shield_hp, 100.0);
    assert_eq!(dest.armor_hp, 200.0);
    assert_eq!(dest.structure_hp, 300.0);
    assert_eq!(dest.capacitor, 400.0);

    // spawn_distinct_exit_wormhole
    let exit_wh = spawn_distinct_exit_wormhole(
        &ctx,
        &sys_handle,
        30_002_187,
        sys_id,
        98_500_777,
        DVec3::ZERO,
        DVec3::new(1_000.0, 2_000.0, 3_000.0),
    )
    .await;
    assert_ne!(exit_wh, 98_500_777);

    // parse_fighter_point_target_args (tuple coords, flat coords, non-finite error)
    let (ids1, slot1, pt1) = parse_fighter_point_target_args(&[
        PyRep::List(vec![PyRep::Integer(11)]),
        PyRep::Integer(2),
        PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(2.0), PyRep::Real(3.0)]),
    ])
    .expect("tuple point");
    assert_eq!(ids1, vec![11]);
    assert_eq!(slot1, 2);
    assert_eq!(pt1, DVec3::new(1.0, 2.0, 3.0));

    let (ids2, slot2, pt2) = parse_fighter_point_target_args(&[
        PyRep::Integer(12),
        PyRep::Integer(1),
        PyRep::Real(4.0),
        PyRep::Real(5.0),
        PyRep::Real(6.0),
    ])
    .expect("flat point");
    assert_eq!(ids2, vec![12]);
    assert_eq!(slot2, 1);
    assert_eq!(pt2, DVec3::new(4.0, 5.0, 6.0));

    assert_eq!(
        parse_fighter_point_target_args(&[
            PyRep::Integer(12),
            PyRep::Integer(1),
            PyRep::Real(f64::NAN),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
        ])
        .expect_err("non-finite rejected")
        .code,
        "InvalidTargetPoint"
    );
}
