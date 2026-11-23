//! Unit tests for the unified SDE NPC weapon/profile factory and atomic wreck/loot helpers.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use eve_db::InventoryFlag;
use eve_destiny::{Ball, DestinyEvent, MovementMode, WarpPhase};
use eve_sde::SdeRegistry;
use glam::DVec3;

use crate::router::ServerContext;

use super::npc_ai::spawn_npc_wreck_and_loot_in_snap;
use super::npc_ai_weapons_loot::{
    build_npc_ball_from_sde, clear_extended_npc_weapon_runtime,
    execute_extended_npc_attack_in_bp, fallback_log_registry, inspect_npc_sde_profile,
    is_edencom_arc_combatant_type, resolve_npc_combat_profile, resolve_npc_weapon_ammo_loot,
    resolve_npc_wreck_type_id, store_extended_npc_weapon_runtime,
    take_or_init_extended_npc_weapon_runtime, with_extended_npc_weapon_runtime,
    EdencomHostilityMeta, NpcSizeClass, NpcWeaponKind,
};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

#[test]
fn npc_sde_factory_unifies_profiles_and_forbids_invented_abilities() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;

    // 1. Guristas Arrogator (2382): Frigate Hybrid, exact SDE numbers, zero EWAR.
    let (prof_2382, fb_2382) = inspect_npc_sde_profile(&ctx.sde, 2_382, None, 500_010);
    assert!(!prof_2382.is_synthetic_fallback);
    assert!(
        fb_2382.is_empty(),
        "Guristas Arrogator 2382 must resolve cleanly without SDE fallbacks, got {fb_2382:?}"
    );
    let turret_2382 = prof_2382.turret.as_ref().expect("2382 Hybrid turret");
    assert_eq!(turret_2382.kind, NpcWeaponKind::Hybrid);
    assert_eq!(turret_2382.fx_guid, "effects.HybridFired");
    assert!((turret_2382.cycle_duration_s - 2.5).abs() < 1e-9);
    assert!((turret_2382.optimal_range - 500.0).abs() < 1e-9);
    assert!((turret_2382.falloff_range - 3_000.0).abs() < 1e-9);
    assert!((turret_2382.tracking_speed - 0.365).abs() < 1e-9);
    assert!((turret_2382.shot_damage.kinetic - 1.25).abs() < 1e-9);
    assert!((turret_2382.shot_damage.thermal - 1.25).abs() < 1e-9);
    assert!((prof_2382.total_nominal_dps - 1.0).abs() < 1e-9);
    assert!((prof_2382.max_velocity - 1_760.0).abs() < 1e-9);
    assert!((prof_2382.cruise_velocity - 220.0).abs() < 1e-9);

    let ball_2382 = build_npc_ball_from_sde(
        &ctx.sde,
        9_900_001,
        2_382,
        DVec3::new(1_000.0, 0.0, 0.0),
        prof_2382.max_velocity,
        0.5,
    );
    assert!((ball_2382.max_velocity - 1_760.0).abs() < 1e-9);
    assert_eq!(ball_2382.type_id, 2_382);

    for &(pirate_tid, id_offset) in &[
        (2_382_u32, 0_i64),
        (2_381, 1),
        (10_281, 2),
        (17_641, 3),
        (2_386, 4),
    ] {
        let (_b, _cs, pirate) = crate::services::wormhole_pve_seeding::build_specialized_npc_components(
            &ctx,
            sys_id,
            9_900_010 + id_offset,
            pirate_tid,
            "Pirate",
            1,
            DVec3::ZERO,
        );
        assert_eq!(
            (
                pirate.neut_amount,
                pirate.web_speed_factor,
                pirate.web_range,
                pirate.scram_strength,
                pirate.scram_range,
                pirate.remote_shield_rep,
                pirate.remote_armor_rep,
                pirate.remote_rep_range,
            ),
            (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0),
            "Pirate NPC {pirate_tid} must never receive invented EWAR or remote rep"
        );
    }

    // 2. Arch Angel Rogue (13032): Projectile turret + explicit SDE web/scram, 0.0 neut.
    let (prof_13032, fb_13032) = inspect_npc_sde_profile(&ctx.sde, 13_032, None, 500_011);
    assert!(!prof_13032.is_synthetic_fallback && fb_13032.is_empty());
    let turret_13032 = prof_13032.turret.as_ref().expect("13032 Projectile turret");
    assert_eq!(turret_13032.kind, NpcWeaponKind::Projectile);
    assert_eq!(turret_13032.fx_guid, "effects.ProjectileFired");
    assert!((prof_13032.total_nominal_dps - 15.0).abs() < 1e-6);
    let (_b, _cs, rogue) = crate::services::wormhole_pve_seeding::build_specialized_npc_components(
        &ctx,
        sys_id,
        9_900_020,
        13_032,
        "Pirate",
        1,
        DVec3::ZERO,
    );
    assert_eq!(rogue.neut_amount, 0.0);
    assert_eq!(rogue.web_speed_factor, -50.0);
    assert_eq!(rogue.web_range, 10_000.0);
    assert_eq!(rogue.scram_strength, 1.0);
    assert_eq!(rogue.scram_range, 20_000.0);

    // 3. Awakened Watchman (30201): Sleeper dual Laser + Missile profile and explicit 60.0 neut.
    let (prof_30201, fb_30201) = inspect_npc_sde_profile(&ctx.sde, 30_201, None, 500_021);
    assert!(!prof_30201.is_synthetic_fallback);
    assert!(prof_30201.turret.is_some() && prof_30201.missile.is_some(), "fb={fb_30201:?}");
    let (_b, _cs, watchman) = crate::services::wormhole_pve_seeding::build_specialized_npc_components(
        &ctx,
        sys_id,
        9_900_021,
        30_201,
        "Sleeper",
        1,
        DVec3::ZERO,
    );
    assert_eq!(watchman.neut_amount, 60.0);
    assert_eq!(watchman.web_speed_factor, -50.0);
    assert_eq!(watchman.scram_strength, 2.0);

    // 4. Harrowing Damavik (52206) TriglavianBeam and Skybreaker Disparu Troop (56188) Vorton.
    let (prof_52206, _) = inspect_npc_sde_profile(&ctx.sde, 52_206, None, 500_026);
    assert_eq!(prof_52206.turret.as_ref().expect("52206 beam").kind, NpcWeaponKind::TriglavianBeam);
    assert!((prof_52206.total_nominal_dps - 28.2).abs() < 1e-6);
    let (prof_56188, _) = inspect_npc_sde_profile(&ctx.sde, 56_188, None, 500_027);
    assert_eq!(prof_56188.turret.as_ref().expect("56188 vorton").kind, NpcWeaponKind::Vorton);

    // 5. Unarmed ORE Rorqual (79589): zero DPS, no weapons, zero orbit range.
    let (prof_79589, fb_79589) = inspect_npc_sde_profile(&ctx.sde, 79_589, None, 500_014);
    assert!(!prof_79589.is_synthetic_fallback && fb_79589.is_empty());
    assert!(prof_79589.turret.is_none() && prof_79589.missile.is_none());
    assert_eq!(prof_79589.total_nominal_dps, 0.0);
    assert_eq!(prof_79589.orbit_range, 0.0);
    assert_eq!(prof_79589.engagement_optimal_range, 0.0);

    // 6. Missing SDE type triggers synthetic fallback and bounded once-per-type registry.
    let missing_tid = 999_777_139_u32;
    {
        let mut reg = fallback_log_registry().lock().unwrap_or_else(|e| e.into_inner());
        reg.remove(&missing_tid);
    }
    let (prof_missing, fb_missing) = inspect_npc_sde_profile(&ctx.sde, missing_tid, Some(12_345.0), 500_010);
    assert!(prof_missing.is_synthetic_fallback);
    assert!(!fb_missing.is_empty());
    assert!(prof_missing.total_nominal_dps > 0.0);
    let _ = resolve_npc_combat_profile(&ctx.sde, missing_tid, Some(12_345.0), 500_010);
    {
        let mut reg = fallback_log_registry().lock().unwrap_or_else(|e| e.into_inner());
        assert!(reg.contains(&missing_tid));
        assert!(reg.len() <= 16_384, "fallback registry must stay bounded");
        reg.remove(&missing_tid);
    }
}

#[test]
fn npc_atomic_wreck_and_weapon_matched_loot_builder() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let owner_id = 90_000_001_i64;

    assert_eq!(resolve_npc_wreck_type_id(&ctx.sde, Some(2_382), None), 26_573);
    assert_eq!(resolve_npc_wreck_type_id(&ctx.sde, Some(13_032), None), 26_561);
    assert_eq!(resolve_npc_wreck_type_id(&ctx.sde, Some(2_381), None), 26_586);
    assert_eq!(resolve_npc_wreck_type_id(&ctx.sde, Some(10_281), None), 26_568);
    assert_eq!(resolve_npc_wreck_type_id(&ctx.sde, Some(17_641), None), 26_563);
    assert_eq!(resolve_npc_wreck_type_id(&ctx.sde, Some(30_201), Some("Sleeper")), 30_492);
    assert_eq!(resolve_npc_wreck_type_id(&ctx.sde, Some(52_206), None), 52_414);
    assert_eq!(resolve_npc_wreck_type_id(&ctx.sde, Some(79_589), None), 79_586);
    // Size-class Cruiser fallback when SDE omits wreck_type_id (56188 sig=192m -> Cruiser Wreck 26558).
    assert_eq!(resolve_npc_wreck_type_id(&ctx.sde, Some(56_188), Some("EDENCOM")), 26_558);
    assert_eq!(resolve_npc_wreck_type_id(&ctx.sde, Some(999_777_140), Some("Sleeper")), 30_495);
    assert_eq!(resolve_npc_wreck_type_id(&ctx.sde, Some(999_777_140), Some("Drifter")), 37_531);

    for &(tid, fid, expected_ammo) in &[
        (2_382_u32, 500_010_u32, Some((222_u32, 25_i32))),
        (13_032, 500_011, Some((183, 25))),
        (2_381, 500_020, Some((230, 60))),
        (10_281, 500_012, Some((254, 1))),
        (17_641, 500_011, Some((199, 120))),
        (2_386, 500_010, Some((266, 25))),
        // Sleeper 30201 carries unpublished missile 30428; turret ammo (254) is dropped instead.
        (30_201, 500_021, Some((254, 1))),
        (79_589, 500_014, None),
    ] {
        let (prof, _) = inspect_npc_sde_profile(&ctx.sde, tid, None, fid);
        assert_eq!(
            resolve_npc_weapon_ammo_loot(&ctx.sde, Some(tid), &prof, Some(fid)),
            expected_ammo,
            "unexpected weapon-matched ammo for NPC {tid}"
        );
    }

    let wreck_tid = resolve_npc_wreck_type_id(&ctx.sde, Some(17_641), None);
    let (prof_17641, _) = inspect_npc_sde_profile(&ctx.sde, 17_641, None, 500_011);
    let (ammo_tid, ammo_qty) = resolve_npc_weapon_ammo_loot(&ctx.sde, Some(17_641), &prof_17641, Some(500_011))
        .expect("17641 ammo drop");
    let scrap_qty = NpcSizeClass::from_sde(&ctx.sde, Some(17_641)).scrap_quantity();

    let (items_before, wreck_id) = ctx.db.with_mut_snapshot(|snap| {
        let before = snap.items.len();
        let wid = spawn_npc_wreck_and_loot_in_snap(
            snap,
            sys_id,
            owner_id,
            DVec3::new(100.0, 200.0, 300.0),
            1_700_000_000,
            wreck_tid,
            &[(15_331, scrap_qty, false), (ammo_tid, ammo_qty, false)],
        );
        (before, wid)
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.len(), items_before + 3);
        let wreck = snap.items.get(&wreck_id).expect("atomic wreck item");
        assert_eq!(wreck.type_id, 26_563);
        assert_eq!(wreck.location_id, sys_id as i64);
        assert_eq!(wreck.owner_id, owner_id);
        assert!(wreck.singleton);

        let loot: Vec<_> = snap.items.values().filter(|i| i.location_id == wreck_id).collect();
        assert_eq!(loot.len(), 2);
        assert!(loot.iter().all(|i| i.flag == InventoryFlag::Cargo && i.owner_id == owner_id));
        assert!(loot.iter().any(|i| i.type_id == 15_331 && i.quantity == 10));
        assert!(loot.iter().any(|i| i.type_id == 199 && i.quantity == 120));
    });
}

#[test]
fn npc_extended_weapon_runtime_is_context_scoped_and_pruned() {
    let ctx_a = test_ctx();
    let ctx_b = test_ctx();
    let sys_id = 30_000_142_u32;
    let ball_id = 9_910_001_i64;
    let (_ball, _cs, npc) = crate::services::wormhole_pve_seeding::build_specialized_npc_components(
        &ctx_a,
        sys_id,
        ball_id,
        52_645,
        "Triglavian",
        500_026,
        DVec3::ZERO,
    );

    let mut rt_a = take_or_init_extended_npc_weapon_runtime(&ctx_a, sys_id, &npc);
    assert_eq!(rt_a.turret_cooldown_s, 0.0);
    rt_a.turret_cooldown_s = 7.5;
    store_extended_npc_weapon_runtime(&ctx_a, sys_id, ball_id, rt_a);

    let rt_b = take_or_init_extended_npc_weapon_runtime(&ctx_b, sys_id, &npc);
    assert_eq!(
        rt_b.turret_cooldown_s, 0.0,
        "weapon runtime must be isolated per ServerContext"
    );

    let rt_a_reloaded = take_or_init_extended_npc_weapon_runtime(&ctx_a, sys_id, &npc);
    assert!((rt_a_reloaded.turret_cooldown_s - 7.5).abs() < 1e-9);
    store_extended_npc_weapon_runtime(&ctx_a, sys_id, ball_id, rt_a_reloaded);

    clear_extended_npc_weapon_runtime(&ctx_a, sys_id, &[ball_id]);
    let has_entry =
        with_extended_npc_weapon_runtime(&ctx_a, |wrt| wrt.contains_key(&(sys_id, ball_id)));
    assert!(!has_entry);
    let rt_a_fresh = take_or_init_extended_npc_weapon_runtime(&ctx_a, sys_id, &npc);
    assert_eq!(rt_a_fresh.turret_cooldown_s, 0.0);
}

#[tokio::test]
async fn npc_edencom_vorton_secondary_arc_reaches_beyond_optimal_and_skips_protected_targets() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let npc_id = 9_920_001_i64;
    let (npc_ball, npc_cs, mut npc) =
        crate::services::wormhole_pve_seeding::build_specialized_npc_components(
            &ctx,
            sys_id,
            npc_id,
            56_188,
            "EDENCOM",
            500_027,
            DVec3::ZERO,
        );

    let primary_id = 9_920_002_i64;
    let secondary_beyond_opt_id = 9_920_003_i64;
    let cloaked_id = 9_920_004_i64;
    let warping_id = 9_920_005_i64;
    let invuln_id = 9_920_006_i64;
    let dead_id = 9_920_007_i64;
    let loc_invuln_id = 9_920_008_i64;
    let broken_jump_cloak_id = 9_920_009_i64;
    let loc_char_id = 90_000_099_i64;

    for tid in [
        primary_id,
        secondary_beyond_opt_id,
        cloaked_id,
        warping_id,
        invuln_id,
        dead_id,
        loc_invuln_id,
        broken_jump_cloak_id,
    ] {
        npc.threat_table.insert(tid, 100.0);
    }

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut ch) = snap.characters.values().next().cloned() {
            ch.character_id = loc_char_id;
            ch.active_ship_id = loc_invuln_id;
            ch.standings.insert(500_027, -5.0);
            snap.characters.insert(loc_char_id, ch);
        }
    });
    let edencom_meta = EdencomHostilityMeta::collect(
        &ctx,
        sys_id,
        &[
            primary_id,
            secondary_beyond_opt_id,
            cloaked_id,
            warping_id,
            invuln_id,
            dead_id,
            loc_invuln_id,
            broken_jump_cloak_id,
        ],
    );

    let mut weapon_rt = take_or_init_extended_npc_weapon_runtime(&ctx, sys_id, &npc);
    let opt = weapon_rt.profile.turret.as_ref().expect("56188 vorton").optimal_range;
    assert!(opt > 10_000.0 && opt < 25_000.0);

    let mk_target_ball = |id: i64, x: f64, mode: MovementMode| Ball {
        id,
        type_id: 587,
        position: DVec3::new(x, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode,
    };
    let mk_target_cs = || {
        let mut cs = ctx.build_type_combat_state(587);
        cs.max_shield_hp = 5_000.0;
        cs.shield_hp = 5_000.0;
        cs.max_armor_hp = 5_000.0;
        cs.armor_hp = 5_000.0;
        cs.max_structure_hp = 5_000.0;
        cs.structure_hp = 5_000.0;
        cs
    };

    let sys = ctx.systems.get_or_spawn(sys_id);
    let (pri_after, sec_after, cloak_after, warp_after, invuln_after, loc_invuln_after, broken_after, dead_after) = {
        let mut bp = sys.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.balls.insert(npc_id, npc_ball);
        bp.combat_states.insert(npc_id, npc_cs);

        // Primary within shooter optimal (18 km < 19.2 km); secondary 6 km from primary (<= 10 km)
        // yet 24 km from shooter (> 19.2 km optimal).
        bp.balls
            .insert(primary_id, mk_target_ball(primary_id, 18_000.0, MovementMode::Stop));
        bp.combat_states.insert(primary_id, mk_target_cs());

        bp.balls.insert(
            secondary_beyond_opt_id,
            mk_target_ball(secondary_beyond_opt_id, 24_000.0, MovementMode::Stop),
        );
        bp.combat_states
            .insert(secondary_beyond_opt_id, mk_target_cs());

        bp.balls
            .insert(cloaked_id, mk_target_ball(cloaked_id, 18_100.0, MovementMode::Stop));
        let mut cs_cloak = mk_target_cs();
        cs_cloak.is_cloaked = true;
        bp.combat_states.insert(cloaked_id, cs_cloak);

        bp.balls.insert(
            warping_id,
            mk_target_ball(
                warping_id,
                18_200.0,
                MovementMode::Warp {
                    origin: DVec3::ZERO,
                    destination: DVec3::new(100_000.0, 0.0, 0.0),
                    warp_speed_au_s: 3.0,
                    total_distance: 100_000.0,
                    phase: WarpPhase::Cruising,
                    elapsed_warp_time: 1.0,
                },
            ),
        );
        bp.combat_states.insert(warping_id, mk_target_cs());

        bp.balls
            .insert(invuln_id, mk_target_ball(invuln_id, 18_300.0, MovementMode::Stop));
        let mut cs_invuln = mk_target_cs();
        cs_invuln.undock_invulnerable_remaining_ticks = 10;
        bp.combat_states.insert(invuln_id, cs_invuln);

        bp.balls
            .insert(dead_id, mk_target_ball(dead_id, 18_400.0, MovementMode::Stop));
        let mut cs_dead = mk_target_cs();
        cs_dead.shield_hp = 0.0;
        cs_dead.armor_hp = 0.0;
        cs_dead.structure_hp = 0.0;
        bp.combat_states.insert(dead_id, cs_dead);

        bp.balls.insert(
            loc_invuln_id,
            mk_target_ball(loc_invuln_id, 18_350.0, MovementMode::Stop),
        );
        let mut cs_loc_invuln = mk_target_cs();
        cs_loc_invuln.apply_jump_cloak_and_invuln_timers(60);
        cs_loc_invuln.is_cloaked = false;
        bp.combat_states.insert(loc_invuln_id, cs_loc_invuln);

        bp.balls.insert(
            broken_jump_cloak_id,
            mk_target_ball(broken_jump_cloak_id, 18_150.0, MovementMode::Stop),
        );
        let mut cs_broken_cloak = mk_target_cs();
        cs_broken_cloak.apply_jump_cloak_and_invuln_timers(60);
        cs_broken_cloak.undock_invulnerable_remaining_ticks = 0;
        bp.combat_states.insert(broken_jump_cloak_id, cs_broken_cloak);

        let sigs = HashMap::new();
        let destroyed_in_call = HashSet::new();
        let mut destroyed_in_tick = Vec::new();
        let mut homefront_events = Vec::new();
        let fired = execute_extended_npc_attack_in_bp(
            &ctx,
            &mut bp,
            &mut npc,
            &mut weapon_rt,
            primary_id,
            &sigs,
            false,
            false,
            Some(&edencom_meta),
            &destroyed_in_call,
            &mut destroyed_in_tick,
            &mut homefront_events,
        );
        assert!(fired);
        (
            bp.combat_states[&primary_id].shield_hp,
            bp.combat_states[&secondary_beyond_opt_id].shield_hp,
            bp.combat_states[&cloaked_id].shield_hp,
            bp.combat_states[&warping_id].shield_hp,
            bp.combat_states[&invuln_id].shield_hp,
            bp.combat_states[&loc_invuln_id].shield_hp,
            bp.combat_states[&broken_jump_cloak_id].shield_hp,
            bp.combat_states[&dead_id].shield_hp,
        )
    };

    assert!(pri_after < 5_000.0, "primary target must take vorton damage");
    assert!(
        sec_after < 5_000.0,
        "secondary arc within 10 km of primary must hit even beyond shooter optimal"
    );
    assert_eq!(cloak_after, 5_000.0, "cloaked target must not be hit by arc");
    assert_eq!(warp_after, 5_000.0, "warping target must not be hit by arc");
    assert_eq!(invuln_after, 5_000.0, "invulnerable target must not be hit by arc");
    assert_eq!(
        loc_invuln_after, 5_000.0,
        "location-change invulnerable target must not be hit by arc"
    );
    assert!(
        broken_after < 5_000.0,
        "post-action broken jump cloak (raw is_cloaked stale) must be hit via has_any_cloak()"
    );
    assert_eq!(dead_after, 0.0, "dead target must not be targeted by arc");
}

#[tokio::test]
async fn npc_attack_skips_missile_fx_and_cooldown_after_turret_destroys_target() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let npc_id = 9_930_001_i64;
    // Sleepless Patroller (30188) carries both a Hybrid/Laser turret and Phantasmata Missile (30426).
    let (npc_ball, npc_cs, mut npc) =
        crate::services::wormhole_pve_seeding::build_specialized_npc_components(
            &ctx,
            sys_id,
            npc_id,
            30_188,
            "Sleeper",
            500_021,
            DVec3::ZERO,
        );
    let mut weapon_rt = take_or_init_extended_npc_weapon_runtime(&ctx, sys_id, &npc);
    assert!(weapon_rt.profile.turret.is_some());
    assert!(weapon_rt.profile.missile.is_some());

    let target_id = 9_930_002_i64;
    let target_ball = Ball {
        id: target_id,
        type_id: 587,
        position: DVec3::new(1_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut target_cs = ctx.build_type_combat_state(587);
    target_cs.shield_hp = 0.0;
    target_cs.armor_hp = 0.0;
    target_cs.structure_hp = 1.0;

    let sys = ctx.systems.get_or_spawn(sys_id);
    let (fired, target_still_exists, fx_count, missile_fx_count) = {
        let mut bp = sys.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.balls.insert(npc_id, npc_ball);
        bp.combat_states.insert(npc_id, npc_cs);
        bp.balls.insert(target_id, target_ball);
        bp.combat_states.insert(target_id, target_cs);
        bp.pending_events.clear();

        let sigs = HashMap::new();
        let destroyed_in_call = HashSet::new();
        let mut destroyed_in_tick = Vec::new();
        let mut homefront_events = Vec::new();
        let fired = execute_extended_npc_attack_in_bp(
            &ctx,
            &mut bp,
            &mut npc,
            &mut weapon_rt,
            target_id,
            &sigs,
            false,
            false,
            None,
            &destroyed_in_call,
            &mut destroyed_in_tick,
            &mut homefront_events,
        );
        let fx_events: Vec<&String> = bp
            .pending_events
            .iter()
            .filter_map(|ev| match ev {
                DestinyEvent::OnSpecialFX { guid, .. } => Some(guid),
                _ => None,
            })
            .collect();
        let m_fx = fx_events.iter().filter(|g| g.contains("Missile")).count();
        (fired, bp.balls.contains_key(&target_id), fx_events.len(), m_fx)
    };

    assert!(fired);
    assert!(!target_still_exists, "turret shot must destroy fragile target");
    assert_eq!(fx_count, 1, "only turret SpecialFX may emit before target dies");
    assert_eq!(missile_fx_count, 0, "missile FX must not emit on dead target");
    assert_eq!(
        weapon_rt.missile_cooldown_s, 0.0,
        "missile cooldown must not be burned after turret destroys target"
    );
}

#[tokio::test]
async fn npc_edencom_vorton_arc_never_hits_player_wreck_jetcan_or_deployable() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let npc_id = 9_940_001_i64;
    let (npc_ball, npc_cs, mut npc) =
        crate::services::wormhole_pve_seeding::build_specialized_npc_components(
            &ctx,
            sys_id,
            npc_id,
            56_188,
            "EDENCOM",
            500_027,
            DVec3::ZERO,
        );

    let drone_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 18 && is_edencom_arc_combatant_type(&ctx.sde, t.type_id))
        .map(|t| t.type_id)
        .expect("SDE combatant drone type");
    assert!(!is_edencom_arc_combatant_type(&ctx.sde, 26_557), "wreck must never be an arc target");
    assert!(!is_edencom_arc_combatant_type(&ctx.sde, 23), "jetcan must never be an arc target");
    assert!(!is_edencom_arc_combatant_type(&ctx.sde, 33_474), "deployable must never be an arc target");

    let char_id = 90_000_141_i64;
    let primary_id = 9_940_002_i64;
    let wreck_id = 9_940_003_i64;
    let jetcan_id = 9_940_004_i64;
    let depot_id = 9_940_005_i64;
    let drone_id = 9_940_006_i64;
    npc.threat_table.insert(primary_id, 100.0);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut ch) = snap.characters.values().next().cloned() {
            ch.character_id = char_id;
            ch.active_ship_id = primary_id;
            ch.standings.insert(500_027, -5.0);
            snap.characters.insert(char_id, ch);
        }
        let template = snap.items.values().next().cloned().expect("seed item");
        for &(id, tid) in &[
            (primary_id, 587_u32),
            (wreck_id, 26_557),
            (jetcan_id, 23),
            (depot_id, 33_474),
            (drone_id, drone_tid),
        ] {
            let mut item = template.clone();
            item.item_id = id;
            item.type_id = tid;
            item.owner_id = char_id;
            item.location_id = sys_id as i64;
            item.flag = InventoryFlag::None;
            snap.items.insert(id, item);
        }
    });

    let meta = EdencomHostilityMeta::collect(
        &ctx,
        sys_id,
        &[primary_id, wreck_id, jetcan_id, depot_id, drone_id],
    );
    let mut weapon_rt = take_or_init_extended_npc_weapon_runtime(&ctx, sys_id, &npc);
    let mk_ball = |id: i64, tid: u32, x: f64| Ball {
        id,
        type_id: tid,
        position: DVec3::new(x, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mk_cs = || {
        let mut cs = ctx.build_type_combat_state(587);
        cs.max_shield_hp = 5_000.0;
        cs.shield_hp = 5_000.0;
        cs.max_armor_hp = 5_000.0;
        cs.armor_hp = 5_000.0;
        cs.max_structure_hp = 5_000.0;
        cs.structure_hp = 5_000.0;
        cs
    };

    let sys = ctx.systems.get_or_spawn(sys_id);
    let (pri_s, wreck_s, jetcan_s, depot_s, drone_s) = {
        let mut bp = sys.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.balls.insert(npc_id, npc_ball);
        bp.combat_states.insert(npc_id, npc_cs);
        for &(id, tid, x) in &[
            (primary_id, 587_u32, 10_000.0),
            (wreck_id, 26_557, 11_000.0),
            (jetcan_id, 23, 11_500.0),
            (depot_id, 33_474, 12_000.0),
            (drone_id, drone_tid, 12_500.0),
        ] {
            bp.balls.insert(id, mk_ball(id, tid, x));
            bp.combat_states.insert(id, mk_cs());
        }
        let sigs = HashMap::new();
        let destroyed_in_call = HashSet::new();
        let mut destroyed_in_tick = Vec::new();
        let mut homefront_events = Vec::new();
        let fired = execute_extended_npc_attack_in_bp(
            &ctx,
            &mut bp,
            &mut npc,
            &mut weapon_rt,
            primary_id,
            &sigs,
            false,
            false,
            Some(&meta),
            &destroyed_in_call,
            &mut destroyed_in_tick,
            &mut homefront_events,
        );
        assert!(fired);
        (
            bp.combat_states[&primary_id].shield_hp,
            bp.combat_states[&wreck_id].shield_hp,
            bp.combat_states[&jetcan_id].shield_hp,
            bp.combat_states[&depot_id].shield_hp,
            bp.combat_states[&drone_id].shield_hp,
        )
    };
    assert!(pri_s < 5_000.0, "primary target must take Vorton damage");
    assert!(drone_s < 5_000.0, "hostile combatant drone within 10km must take Vorton arc damage");
    assert_eq!(wreck_s, 5_000.0, "player wreck must never be damaged by Vorton arc");
    assert_eq!(jetcan_s, 5_000.0, "jetcan must never be damaged by Vorton arc");
    assert_eq!(depot_s, 5_000.0, "deployable must never be damaged by Vorton arc");
}
