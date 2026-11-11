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

#[tokio::test]
async fn r6_t1_p1_helpers_extra_edge_branches_and_mutations() {
    use eve_db::clock::{ServerClock, VirtualServerClock};
    use eve_db::{InventoryFlag, InventoryItemRecord, SystemDynamicState};
    use std::collections::HashMap;

    let mut sde_mut = (*shared_sde()).clone();
    let template_t = sde_mut.types.values().next().unwrap().clone();

    // Fighter 99_911: ability_id=9, charges=None, cooldown_seconds=None, attr 2157=500ms (<1s clamp -> 1s), attr 2124=150%
    let mut t1 = template_t.clone();
    t1.type_id = 99_911;
    t1.default_attributes = HashMap::from([(2157, 500.0), (2124, 150.0)]);
    sde_mut.types.insert(99_911, t1);
    sde_mut.fighter_abilities_by_type.insert(
        99_911,
        vec![FighterAbilitySlot {
            slot_index: 2,
            ability_id: 9,
            cooldown_seconds: None,
            charges: None,
        }],
    );

    // Fighter 99_912: ability_id=13, charges=None, cooldown_seconds=None, no attr 2157 (fallback 20s), attr 2224=250%
    let mut t2 = template_t.clone();
    t2.type_id = 99_912;
    t2.default_attributes = HashMap::from([(2224, 250.0)]);
    sde_mut.types.insert(99_912, t2);
    sde_mut.fighter_abilities_by_type.insert(
        99_912,
        vec![FighterAbilitySlot {
            slot_index: 0,
            ability_id: 13,
            cooldown_seconds: None,
            charges: None,
        }],
    );

    // Fighter 99_913: not present in fighter_abilities_by_type -> UnsupportedFighterAbility
    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde_mut),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_700_000_000));
    ctx.set_clock(clock.clone());

    let sys_id = 30_000_142_u32;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let char_id = 98_600_001_i64;
    let fid_abandoned = 98_600_101_i64;
    let fid_no_sde = 98_600_102_i64;
    let fid_attr2124 = 98_600_103_i64;
    let fid_attr2224 = 98_600_104_i64;

    {
        let mut fighters = ctx.active_fighters.write().unwrap();
        fighters.insert(
            fid_abandoned,
            ActiveFighterSquadron {
                ball_id: fid_abandoned,
                item_id: fid_abandoned,
                type_id: 99_911,
                owner_char_id: char_id,
                parent_ship_id: 98_600_999,
                tube_index: 0,
                squadron_size: 9,
                target_id: None,
                is_abandoned: true,
                mwd_active: false,
            },
        );
        fighters.insert(
            fid_no_sde,
            ActiveFighterSquadron {
                ball_id: fid_no_sde,
                item_id: fid_no_sde,
                type_id: 99_913,
                owner_char_id: char_id,
                parent_ship_id: 98_600_999,
                tube_index: 0,
                squadron_size: 9,
                target_id: None,
                is_abandoned: false,
                mwd_active: false,
            },
        );
        fighters.insert(
            fid_attr2124,
            ActiveFighterSquadron {
                ball_id: fid_attr2124,
                item_id: fid_attr2124,
                type_id: 99_911,
                owner_char_id: char_id,
                parent_ship_id: 98_600_999,
                tube_index: 1,
                squadron_size: 9,
                target_id: None,
                is_abandoned: false,
                mwd_active: false,
            },
        );
        fighters.insert(
            fid_attr2224,
            ActiveFighterSquadron {
                ball_id: fid_attr2224,
                item_id: fid_attr2224,
                type_id: 99_912,
                owner_char_id: char_id,
                parent_ship_id: 98_600_999,
                tube_index: 2,
                squadron_size: 9,
                target_id: None,
                is_abandoned: false,
                mwd_active: true, // was_mwd == true -> should NOT boost velocity again
            },
        );
    }

    assert_eq!(
        validate_and_activate_fighter_mwd(&ctx, char_id, &sys_handle, &[fid_abandoned])
            .await
            .unwrap_err()
            .code,
        "FighterNotOwned"
    );
    assert_eq!(
        validate_and_activate_fighter_mwd(&ctx, char_id, &sys_handle, &[fid_no_sde])
            .await
            .unwrap_err()
            .code,
        "UnsupportedFighterAbility"
    );

    for (fid, tid) in [(fid_attr2124, 99_911), (fid_attr2224, 99_912)] {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: fid,
                type_id: tid,
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
    }

    // fid_attr2124 gets +150% speed -> 2500.0, cooldown clamped to 1s; fid_attr2224 skips velocity multiply because was_mwd=true and gets 20s fallback cd
    validate_and_activate_fighter_mwd(&ctx, char_id, &sys_handle, &[fid_attr2124, fid_attr2224])
        .await
        .expect("multi-fighter MWD ok");
    let (_, b_2124) = sys_handle.get_ball_snapshot(fid_attr2124).await.unwrap().unwrap();
    let (_, b_2224) = sys_handle.get_ball_snapshot(fid_attr2224).await.unwrap().unwrap();
    assert!((b_2124.max_velocity - 2_500.0).abs() < 1e-6);
    assert!((b_2224.max_velocity - 1_000.0).abs() < 1e-6);
    assert_eq!(
        ctx.fighter_cooldown_state.read().unwrap().get(&(fid_attr2124, 2)).copied(),
        Some(clock.now_unix_secs() + 1)
    );
    assert_eq!(
        ctx.fighter_cooldown_state.read().unwrap().get(&(fid_attr2224, 0)).copied(),
        Some(clock.now_unix_secs() + 20)
    );
    // At +1s cooldown expires on fid_attr2124 cleanly without charge exhaustion
    clock.advance_secs(1);
    validate_and_activate_fighter_mwd(&ctx, char_id, &sys_handle, &[fid_attr2124])
        .await
        .expect("re-activation ok after 1s clamp");
    // No velocity double-multiply on second activation
    let (_, b_2124_after) = sys_handle.get_ball_snapshot(fid_attr2124).await.unwrap().unwrap();
    assert!((b_2124_after.max_velocity - 2_500.0).abs() < 1e-6);

    // 2. is_target_invulnerable_or_tethered: every flag & location_change_invuln boundary (`<` vs `<=`)
    let now = clock.now_unix_secs();
    let inv_id = 98_600_201_i64;
    let base_cs = CombatState::default();
    assert!(!is_target_invulnerable_or_tethered(&ctx, inv_id, &base_cs, now));
    for mutated in [
        {
            let mut c = base_cs.clone();
            c.is_cloaked = true;
            c
        },
        {
            let mut c = base_cs.clone();
            c.gate_cloak_remaining_ticks = 1;
            c
        },
        {
            let mut c = base_cs.clone();
            c.undock_invulnerable_remaining_ticks = 1;
            c
        },
        {
            let mut c = base_cs.clone();
            c.warp_exit_invulnerable_remaining_ticks = 1;
            c
        },
    ] {
        assert!(is_target_invulnerable_or_tethered(&ctx, inv_id, &mutated, now));
    }
    ctx.location_change_invuln.lock().unwrap().insert(inv_id, now + 5);
    assert!(is_target_invulnerable_or_tethered(&ctx, inv_id, &base_cs, now + 4));
    assert!(!is_target_invulnerable_or_tethered(&ctx, inv_id, &base_cs, now + 5));

    // 3. step_active_breacher_dots:
    // - missing target skipped safely
    // - invulnerable target with remaining_s > dt retains dot with decremented remaining_s
    // - target with max_shield_hp == 0 and max_armor_hp == 0 exercises 0.0 layer fallbacks & expiring clamped step_s
    // - fatal target (`destroyed_by_dot == true`) removes dot, hits character/item owner lookup & killmail/destruction handler
    // - source_char_id == 0 skips killmail/combat log
    let t_missing = 98_600_300_i64;
    let t_invuln_keep = 98_600_301_i64;
    let t_hull_only = 98_600_302_i64;
    let t_fatal = 98_600_303_i64;
    let t_no_src = 98_600_304_i64;
    let src_ship = 98_600_399_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.active_ship_id = t_fatal;
        }
        snap.items.insert(
            src_ship,
            InventoryItemRecord {
                item_id: src_ship,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: sys_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            t_hull_only,
            InventoryItemRecord {
                item_id: t_hull_only,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: sys_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });

    let _ = sys_handle
        .set_combat_state(
            t_invuln_keep,
            CombatState {
                is_cloaked: true,
                max_shield_hp: 100.0,
                shield_hp: 100.0,
                ..Default::default()
            },
        )
        .await;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: t_hull_only,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                max_shield_hp: 0.0,
                shield_hp: 0.0,
                max_armor_hp: 0.0,
                armor_hp: 0.0,
                max_structure_hp: 1_000.0,
                structure_hp: 1_000.0,
                ..Default::default()
            },
        )
        .await;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: t_fatal,
                type_id: 587,
                position: DVec3::new(100.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                max_shield_hp: 10.0,
                shield_hp: 0.0,
                max_armor_hp: 10.0,
                armor_hp: 0.0,
                max_structure_hp: 10.0,
                structure_hp: 0.5,
                ..Default::default()
            },
        )
        .await;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: t_no_src,
                type_id: 587,
                position: DVec3::new(200.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                max_shield_hp: 100.0,
                shield_hp: 100.0,
                max_armor_hp: 100.0,
                armor_hp: 100.0,
                max_structure_hp: 100.0,
                structure_hp: 100.0,
                ..Default::default()
            },
        )
        .await;

    let mk_dot = |tid: i64, src_char: i64, rem_s: f64| ActiveBreacherDot {
        target_ball_id: tid,
        source_char_id: src_char,
        source_ship_id: src_ship,
        weapon_type_id: 2873,
        charge_type_id: 192,
        flat_cap_dps: 50.0,
        pct_cap_fraction: 0.1,
        remaining_s: rem_s,
        tick_damage: 0.0,
    };
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_breacher_dots.insert(t_missing, mk_dot(t_missing, 90_000_001, 2.0));
        rt.active_breacher_dots.insert(t_invuln_keep, mk_dot(t_invuln_keep, 90_000_001, 3.0));
        rt.active_breacher_dots.insert(t_hull_only, mk_dot(t_hull_only, 90_000_001, 0.4));
        rt.active_breacher_dots.insert(t_fatal, mk_dot(t_fatal, 90_000_001, 5.0));
        rt.active_breacher_dots.insert(t_no_src, mk_dot(t_no_src, 0, 5.0));
    });

    step_active_breacher_dots(&ctx, &sys_handle, sys_id, 1.0).await;
    with_combat_runtime(&ctx, |rt| {
        assert!((rt.active_breacher_dots[&t_invuln_keep].remaining_s - 2.0).abs() < 1e-6);
        assert!(!rt.active_breacher_dots.contains_key(&t_hull_only));
        assert!(!rt.active_breacher_dots.contains_key(&t_fatal));
        assert!(rt.active_breacher_dots.contains_key(&t_no_src));
    });
    assert!(sys_handle.get_combat_state(t_hull_only).await.unwrap().unwrap().structure_hp < 1_000.0);

    // 4. apply_preserved_combat_state: None no-op + clamping when max == 0 / hp > max + preserving breacher/entropic fields
    let mut dest_none = CombatState::default();
    dest_none.shield_hp = 42.0;
    apply_preserved_combat_state(&mut dest_none, None);
    assert_eq!(dest_none.shield_hp, 42.0);

    let mut prev_clamped = CombatState::default();
    prev_clamped.max_shield_hp = 0.0;
    prev_clamped.shield_hp = 50.0;
    prev_clamped.max_armor_hp = 100.0;
    prev_clamped.armor_hp = 200.0;
    prev_clamped.active_breacher_dot = Some((25.0, 5.0, 2));
    prev_clamped.entropic_spool_cycles = 7;
    let mut dest_clamped = CombatState::default();
    dest_clamped.max_shield_hp = 120.0;
    dest_clamped.max_armor_hp = 300.0;
    apply_preserved_combat_state(&mut dest_clamped, Some(prev_clamped));
    assert_eq!(dest_clamped.shield_hp, 120.0);
    assert_eq!(dest_clamped.armor_hp, 300.0);
    assert_eq!(dest_clamped.active_breacher_dot, Some((25.0, 5.0, 2)));
    assert_eq!(dest_clamped.entropic_spool_cycles, 7);

    // 5. spawn_distinct_exit_wormhole populates reverse system_state link and preserves existing link
    let wh_dest = 31_000_555_u32;
    let wh_handle = ctx.systems.get_or_spawn(wh_dest);
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(wh_dest, SystemDynamicState::default());
    });
    let sig_pos = DVec3::new(11_000.0, 22_000.0, 33_000.0);
    let _wh1 = spawn_distinct_exit_wormhole(&ctx, &wh_handle, sys_id, wh_dest, 98_600_777, sig_pos, DVec3::ZERO).await;
    ctx.db.with_snapshot(|snap| {
        let st = &snap.system_states[&wh_dest];
        assert_eq!(st.wormhole_destination_system_id, Some(sys_id));
        assert!(st.wormhole_remaining_mass_kg >= 1_000_000_000.0);
        assert_eq!(st.signature_position, Some([11_000.0, 22_000.0, 33_000.0]));
    });
    // Second spawn must NOT overwrite existing reverse link
    let _wh2 = spawn_distinct_exit_wormhole(&ctx, &wh_handle, 30_002_187, wh_dest, 98_600_778, DVec3::new(1.0, 2.0, 3.0), DVec3::ZERO).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.system_states[&wh_dest].wormhole_destination_system_id, Some(sys_id));
    });

    // 6. parse_fighter_point_target_args error & Tuple branch coverage
    assert_eq!(
        parse_fighter_point_target_args(&[PyRep::List(vec![PyRep::Integer(0), PyRep::Integer(-1)])])
            .unwrap_err()
            .code,
        "FighterNotFound"
    );
    assert_eq!(
        parse_fighter_point_target_args(&[PyRep::Integer(-5)])
            .unwrap_err()
            .code,
        "FighterNotFound"
    );
    assert_eq!(
        parse_fighter_point_target_args(&[PyRep::Integer(11)])
            .unwrap_err()
            .code,
        "InvalidFighterAbilitySlot"
    );
    assert_eq!(
        parse_fighter_point_target_args(&[PyRep::Integer(11), PyRep::Integer(2)])
            .unwrap_err()
            .code,
        "InvalidTargetPoint"
    );
    let (tuple_ids, tuple_slot, tuple_pt) = parse_fighter_point_target_args(&[
        PyRep::Tuple(vec![PyRep::Integer(15), PyRep::Integer(16), PyRep::Integer(0)]),
        PyRep::Integer(3),
        PyRep::List(vec![PyRep::Real(7.0), PyRep::Real(8.0), PyRep::Real(9.0)]),
    ])
    .expect("tuple fighter IDs");
    assert_eq!(tuple_ids, vec![15, 16]);
    assert_eq!(tuple_slot, 3);
    assert_eq!(tuple_pt, DVec3::new(7.0, 8.0, 9.0));
}

#[tokio::test]
async fn step_active_breacher_dots_total_hp_damage_ratios_and_zero_gates_pin_mutants() {
    use eve_db::clock::VirtualServerClock;
    use eve_db::{InventoryFlag, InventoryItemRecord};

    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_700_010_000));
    ctx.set_clock(clock.clone());

    let sys_id = 30_000_142_u32;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let attacker_id = 90_000_001_i64;
    let victim_id = 90_000_002_i64;
    let src_ship = 98_601_001_i64;
    let t_layered = 98_601_101_i64;
    let t_partial = 98_601_102_i64;
    let t_no_src = 98_601_103_i64;

    ctx.db
        .acquire_online_lock(1, 9101, Some(attacker_id), true, ctx.now_unix())
        .expect("online lock for attacker");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut victim) = snap.characters.get(&attacker_id).cloned() {
            victim.character_id = victim_id;
            victim.user_id = 9102;
            victim.name = "BreacherVictim".into();
            snap.characters.insert(victim_id, victim);
        }
        snap.items.insert(
            src_ship,
            InventoryItemRecord {
                item_id: src_ship,
                type_id: 587,
                owner_id: attacker_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
        for (item_id, owner_id) in [(t_layered, victim_id), (t_partial, victim_id), (t_no_src, victim_id)] {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id: 587,
                    owner_id,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    ..Default::default()
                },
            );
        }
    });

    for (ball_id, cs) in [
        (
            t_layered,
            CombatState {
                max_shield_hp: 500.0,
                shield_hp: 500.0,
                max_armor_hp: 300.0,
                armor_hp: 300.0,
                max_structure_hp: 200.0,
                structure_hp: 200.0,
                ..Default::default()
            },
        ),
        (
            t_partial,
            CombatState {
                max_shield_hp: 100.0,
                shield_hp: 0.0,
                max_armor_hp: 300.0,
                armor_hp: 200.0,
                max_structure_hp: 200.0,
                structure_hp: 100.0,
                ..Default::default()
            },
        ),
        (
            t_no_src,
            CombatState {
                max_shield_hp: 500.0,
                shield_hp: 500.0,
                max_armor_hp: 300.0,
                armor_hp: 300.0,
                max_structure_hp: 200.0,
                structure_hp: 200.0,
                ..Default::default()
            },
        ),
    ] {
        let _ = sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: ball_id,
                    type_id: 587,
                    position: DVec3::new((ball_id - t_layered) as f64 * 100.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
            )
            .await;
    }

    // 1. dt == 0.0 with source_char_id > 0: applied_dmg == 0.0 -> NO combat log or damage state notifications
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_breacher_dots.insert(
            t_layered,
            ActiveBreacherDot {
                target_ball_id: t_layered,
                source_char_id: attacker_id,
                source_ship_id: src_ship,
                weapon_type_id: 2873,
                charge_type_id: 192,
                flat_cap_dps: 200.0,
                pct_cap_fraction: 0.05,
                remaining_s: 5.0,
                tick_damage: 0.0,
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(attacker_id);
    step_active_breacher_dots(&ctx, &sys_handle, sys_id, 0.0).await;
    let zero_dt_notifs = ctx.drain_notifications_for_char(attacker_id);
    assert!(
        !zero_dt_notifs.iter().any(|n| n.event == "OnDamageMessage" || n.event == "OnDamageStateChange"),
        "dt == 0 must not emit combat notifications: {zero_dt_notifs:?}"
    );
    assert!(
        (sys_handle.get_combat_state(t_layered).await.unwrap().unwrap().shield_hp - 500.0).abs() < 1e-6
    );

    // 2. source_char_id == 0 with positive damage: deals exact pct_cap_fraction * (500 + 300 + 200) = 50.0,
    //    but emits NO combat log / damage state notification for attacker_id
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_breacher_dots.remove(&t_layered);
        rt.active_breacher_dots.insert(
            t_no_src,
            ActiveBreacherDot {
                target_ball_id: t_no_src,
                source_char_id: 0,
                source_ship_id: src_ship,
                weapon_type_id: 2873,
                charge_type_id: 192,
                flat_cap_dps: 200.0,
                pct_cap_fraction: 0.05,
                remaining_s: 5.0,
                tick_damage: 0.0,
            },
        );
    });
    step_active_breacher_dots(&ctx, &sys_handle, sys_id, 1.0).await;
    let no_src_notifs = ctx.drain_notifications_for_char(attacker_id);
    assert!(
        !no_src_notifs.iter().any(|n| n.event == "OnDamageMessage" || n.event == "OnDamageStateChange"),
        "source_char_id == 0 must not emit combat notifications: {no_src_notifs:?}"
    );
    assert!(
        (sys_handle.get_combat_state(t_no_src).await.unwrap().unwrap().shield_hp - 450.0).abs() < 1e-6
    );

    // 3. Layered total_max_hp (500+300+200 = 1000 * 0.05 = 50.0) & partial armor/hull ratios (0.0, 0.5, 0.5)
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_breacher_dots.clear();
        rt.active_breacher_dots.insert(
            t_layered,
            ActiveBreacherDot {
                target_ball_id: t_layered,
                source_char_id: attacker_id,
                source_ship_id: src_ship,
                weapon_type_id: 2873,
                charge_type_id: 192,
                flat_cap_dps: 200.0,
                pct_cap_fraction: 0.05,
                remaining_s: 5.0,
                tick_damage: 0.0,
            },
        );
        rt.active_breacher_dots.insert(
            t_partial,
            ActiveBreacherDot {
                target_ball_id: t_partial,
                source_char_id: attacker_id,
                source_ship_id: src_ship,
                weapon_type_id: 2873,
                charge_type_id: 192,
                flat_cap_dps: 50.0,
                pct_cap_fraction: 0.25,
                remaining_s: 5.0,
                tick_damage: 0.0,
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(attacker_id);
    step_active_breacher_dots(&ctx, &sys_handle, sys_id, 1.0).await;

    let cs_layered = sys_handle.get_combat_state(t_layered).await.unwrap().unwrap();
    assert!((cs_layered.shield_hp - 450.0).abs() < 1e-6);
    assert!((cs_layered.armor_hp - 300.0).abs() < 1e-6);
    assert!((cs_layered.structure_hp - 200.0).abs() < 1e-6);

    let cs_partial = sys_handle.get_combat_state(t_partial).await.unwrap().unwrap();
    assert!((cs_partial.shield_hp - 0.0).abs() < 1e-6);
    assert!((cs_partial.armor_hp - 150.0).abs() < 1e-6);
    assert!((cs_partial.structure_hp - 100.0).abs() < 1e-6);

    let notifs = ctx.drain_notifications_for_char(attacker_id);
    let check_dmg_ratios = |ball_id: i64, expected_dmg: f64, exp_s: f64, exp_a: f64, exp_h: f64| {
        let has_msg = notifs.iter().any(|n| {
            let dict_opt = n.args.first().filter(|d| d.dict_get("target").is_some()).or_else(|| n.args.get(1));
            n.event == "OnDamageMessage"
                && dict_opt.and_then(|d| d.dict_get("target")) == Some(&PyRep::Integer(ball_id))
                && dict_opt
                    .and_then(|d| d.dict_get("damage"))
                    .and_then(|v| v.as_f64())
                    .is_some_and(|d| (d - expected_dmg).abs() < 1e-6)
        });
        assert!(has_msg, "expected OnDamageMessage({ball_id}, dmg={expected_dmg}) in {notifs:?}");

        let state_notif = notifs
            .iter()
            .find(|n| n.event == "OnDamageStateChange" && n.args.first() == Some(&PyRep::Integer(ball_id)))
            .unwrap_or_else(|| panic!("expected OnDamageStateChange for {ball_id}: {notifs:?}"));
        let PyRep::Tuple(parts) = &state_notif.args[1] else {
            panic!("expected tuple damage state, got {:?}", state_notif.args[1]);
        };
        let PyRep::Tuple(shield_parts) = &parts[0] else {
            panic!("expected inner shield tuple, got {:?}", parts[0]);
        };
        let s = shield_parts[0].as_f64().unwrap();
        let a = parts[1].as_f64().unwrap();
        let h = parts[2].as_f64().unwrap();
        assert!((s - exp_s).abs() < 1e-6, "shield ratio mismatch for {ball_id}: {s} vs {exp_s}");
        assert!((a - exp_a).abs() < 1e-6, "armor ratio mismatch for {ball_id}: {a} vs {exp_a}");
        assert!((h - exp_h).abs() < 1e-6, "hull ratio mismatch for {ball_id}: {h} vs {exp_h}");
    };
    check_dmg_ratios(t_layered, 50.0, 0.9, 1.0, 1.0);
    check_dmg_ratios(t_partial, 50.0, 0.0, 0.5, 0.5);
}

