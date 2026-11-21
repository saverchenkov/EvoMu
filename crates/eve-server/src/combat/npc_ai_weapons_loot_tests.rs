//! Unit tests for the unified SDE NPC weapon/profile factory and atomic wreck/loot helpers.

use std::sync::{Arc, OnceLock};

use eve_db::InventoryFlag;
use eve_sde::SdeRegistry;
use glam::DVec3;

use crate::router::ServerContext;

use super::npc_ai_weapons_loot::{
    build_npc_ball_from_sde, fallback_log_registry, inspect_npc_sde_profile,
    resolve_npc_combat_profile, resolve_npc_weapon_ammo_loot, resolve_npc_wreck_type_id,
    spawn_npc_wreck_and_loot_in_snap, NpcSizeClass, NpcWeaponKind,
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
