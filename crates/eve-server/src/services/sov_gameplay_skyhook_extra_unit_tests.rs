//! Deep branch-coverage and mutation-killing unit tests for `sov_gameplay_skyhook` (`Q-8`, `SO-5`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::sov_gameplay::sov_gameplay_skyhook::*;
use crate::services::sov_services::{
    TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB, TYPE_SUPERIONIC_ICE,
};
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord, WorldStateSnapshot};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "sov_skyhook_components_and_ticks_unit_tests.rs"]
mod sov_skyhook_components_and_ticks_unit_tests;

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn test_ctx() -> ServerContext {
    let ctx = if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    };
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        1_800_000_000,
    )));
    ctx
}

pub(super) fn find_null_system_with_reagent_planet(ctx: &ServerContext) -> (u32, i64, [f64; 3]) {
    ctx.sde
        .solar_systems
        .values()
        .filter(|sys| {
            crate::services::sov_gameplay::is_claimable_nullsec_system(
                &ctx.sde,
                sys.solar_system_id,
            )
        })
        .flat_map(|sys| {
            let sid = sys.solar_system_id;
            ctx.sde
                .celestials_in_system(sid)
                .iter()
                .filter(|p| p.group_id == 7 && planet_reagent_type(p.type_id).is_some())
                .map(move |p| (sid, p.celestial_id, [p.position.x, p.position.y, p.position.z]))
                .collect::<Vec<_>>()
        })
        .min_by_key(|&(sid, cid, _)| (sid, cid))
        .unwrap_or((30_000_142, 40_009_077, [1.0e9, 0.0, 0.0]))
}

#[test]
fn sub_unit_remainder_conservation_inactive_skyhook_and_extract_extra_var_edges() {
    assert_eq!(planet_reagent_type(12), Some(TYPE_SUPERIONIC_ICE));
    assert_eq!(planet_reagent_type(2014), Some(TYPE_SUPERIONIC_ICE));
    assert_eq!(planet_reagent_type(2015), Some(TYPE_MAGMATIC_GAS));
    assert_eq!(planet_reagent_type(13), None);
    assert_eq!(planet_reagent_type(2016), None);

    assert_eq!(extract_extra_var(Some(&PyRep::Integer(5)), "planetID"), None);
    let dict_wrong = PyRep::Dict(vec![(
        PyRep::String("other".into()),
        PyRep::Integer(10),
    )]);
    assert_eq!(extract_extra_var(Some(&dict_wrong), "planetID"), None);

    // Missing skyhook_id returns None
    let ctx = test_ctx();
    let mut snap = WorldStateSnapshot::default();
    assert!(advance_skyhook_reagents_in_snap(&mut snap, &ctx.sde, 999, 1000).is_none());

    // Inactive skyhook does NOT harvest hourly yield, but still matures existing insecure_qty
    // and 864 steps of 100s mature the exact same 240 units as a single 86,400s step!
    let sh_id = 5001_i64;
    let t0 = 1_700_000_000_u64;
    snap.sov_runtime.skyhook_active.insert(sh_id, false);
    snap.sov_runtime
        .skyhook_reagent_bays
        .insert(sh_id, (TYPE_SUPERIONIC_ICE, 10, 240, 0)); // last_ts = 0 -> initialized to t0 on first call

    let (_, sec0, insec0) = advance_skyhook_reagents_in_snap(&mut snap, &ctx.sde, sh_id, t0).unwrap();
    assert_eq!((sec0, insec0), (10, 240));

    for step in 1..=864 {
        let _ = advance_skyhook_reagents_in_snap(&mut snap, &ctx.sde, sh_id, t0 + step * 100).unwrap();
    }
    let (_, final_sec, final_insec) =
        advance_skyhook_reagents_in_snap(&mut snap, &ctx.sde, sh_id, t0 + 86_400).unwrap();
    assert_eq!(final_sec, 250);
    assert_eq!(final_insec, 0);
    assert_eq!(snap.sov_runtime.skyhook_maturity_rem.get(&sh_id), Some(&(0, 0)));
}

#[tokio::test]
async fn anchor_skyhook_error_branches_and_weapon_damage_reinforcement_transitions() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let (null_sys, planet_id, planet_pos) = find_null_system_with_reagent_planet(&ctx);
    let ship_id = 1_071_000_001_i64;
    let bad_owner_item = 1_071_000_002_i64;
    let hangar_item = 1_071_000_003_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);

    let mut sess = SessionState::new(9501);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(null_sys);
    sess.ship_id = Some(ship_id);

    // 1. Docked -> ShipNotInSpace
    sess.station_id = Some(60_003_760);
    assert_eq!(
        handle_anchor_skyhook_rpc(&ctx, &mut sess, &[])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );
    sess.station_id = None;

    // 2. InsufficientCorpRoles when corp_role == 0 and not CEO
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .entry(corp_id)
            .or_default()
            .ceo_id = char_id + 999;
    });
    assert_eq!(
        handle_anchor_skyhook_rpc(&ctx, &mut sess, &[])
            .await
            .unwrap_err()
            .code,
        "InsufficientCorpRoles"
    );
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

    // 3. InvalidPlanet
    assert_eq!(
        handle_anchor_skyhook_rpc(&ctx, &mut sess, &[PyRep::Integer(999_999)])
            .await
            .unwrap_err()
            .code,
        "InvalidPlanet"
    );

    // 4. ShipNotInSpace when ship ball is not spawned
    assert_eq!(
        handle_anchor_skyhook_rpc(&ctx, &mut sess, &[PyRep::Integer(planet_id)])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // 5. Spawn ship ball > 500km from planet -> TooFarFromPlanet
    let sys_handle = ctx.systems.get_or_spawn(null_sys);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 648,
            position: DVec3::from_array(planet_pos) + DVec3::new(2_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        handle_anchor_skyhook_rpc(&ctx, &mut sess, &[PyRep::Integer(planet_id)])
            .await
            .unwrap_err()
            .code,
        "TooFarFromPlanet"
    );

    // 6. Move ship within 500km, no sov_alliance in system -> CannotAnchorSkyhookInEmpireSpace
    let _ = sys_handle
        .set_ball_position(
            ship_id,
            DVec3::from_array(planet_pos) + DVec3::new(150_000.0, 0.0, 0.0),
        )
        .await;
    assert_eq!(
        handle_anchor_skyhook_rpc(&ctx, &mut sess, &[PyRep::Integer(planet_id)])
            .await
            .unwrap_err()
            .code,
        "CannotAnchorSkyhookInEmpireSpace"
    );

    // 7. Set sov_alliance to a different alliance -> NotSovereigntyHolder
    let ally_id = 99_000_077_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ally_id);
        snap.corporations.entry(corp_id).or_default().alliance_id = Some(ally_id + 1);
    });
    assert_eq!(
        handle_anchor_skyhook_rpc(&ctx, &mut sess, &[PyRep::Integer(planet_id)])
            .await
            .unwrap_err()
            .code,
        "NotSovereigntyHolder"
    );

    // 8. Match alliance, test item validation errors: MissingSkyhookItem, ItemOwnershipViolation, ItemNotInCargo
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.entry(corp_id).or_default().alliance_id = Some(ally_id);
        snap.items.insert(
            bad_owner_item,
            InventoryItemRecord {
                item_id: bad_owner_item,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_id: char_id + 500,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            hangar_item,
            InventoryItemRecord {
                item_id: hangar_item,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_anchor_skyhook_rpc(&ctx, &mut sess, &[PyRep::Integer(planet_id)])
            .await
            .unwrap_err()
            .code,
        "MissingSkyhookItem"
    );
    assert_eq!(
        handle_anchor_skyhook_rpc(
            &ctx,
            &mut sess,
            &[PyRep::Integer(bad_owner_item), PyRep::Integer(planet_id)]
        )
        .await
        .unwrap_err()
        .code,
        "ItemOwnershipViolation"
    );
    assert_eq!(
        handle_anchor_skyhook_rpc(
            &ctx,
            &mut sess,
            &[PyRep::Integer(hangar_item), PyRep::Integer(planet_id)]
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotInCargo"
    );

    // 9. Weapon damage branches: StructureNotFound, StructureDestroyed, outside-window 0 HP -> ArmorReinforced -> StructureInvulnerable
    assert_eq!(
        apply_skyhook_weapon_damage(&ctx, 999_888_777, 100.0)
            .unwrap_err()
            .code,
        "StructureNotFound"
    );
    let sh_id = 1_071_000_099_i64;
    ctx.set_clock(std::sync::Arc::new(
        eve_db::VirtualServerClock::with_fixed_base(1_800_000_000),
    ));
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            sh_id,
            UpwellStructureRecord {
                structure_id: sh_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: null_sys,
                name: "Skyhook".into(),
                position: [0.0, 0.0, 0.0],
                state: "Destroyed".into(),
                shield_hp: SKYHOOK_BASE_SHIELD_HP,
                ..Default::default()
            },
        );
        // Outside theft window
        snap.sov_runtime
            .skyhook_theft_windows
            .insert(sh_id, (now + 3_600, now + 10_800));
    });
    assert_eq!(
        apply_skyhook_weapon_damage(&ctx, sh_id, 100.0)
            .unwrap_err()
            .code,
        "StructureDestroyed"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&sh_id).unwrap().state = "ShieldVulnerable".into();
    });
    let reinf = apply_skyhook_weapon_damage(&ctx, sh_id, 13_000.0).unwrap();
    assert_eq!(
        reinf.dict_get("state").and_then(PyRep::as_str),
        Some("ArmorReinforced")
    );
    assert_eq!(reinf.dict_get("hackable").and_then(PyRep::as_bool), Some(false));
    assert_eq!(
        apply_skyhook_weapon_damage(&ctx, sh_id, 100.0)
            .unwrap_err()
            .code,
        "StructureInvulnerable"
    );
    let _ = TYPE_SOVEREIGNTY_HUB;
}

#[tokio::test]
async fn skyhook_damage_nan_in_window_hackable_and_authorisation_branches() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let (null_sys, _, _) = find_null_system_with_reagent_planet(&ctx);
    let sh_id = 1_073_000_001_i64;
    let ship_id = 1_073_000_002_i64;
    ctx.set_clock(std::sync::Arc::new(
        eve_db::VirtualServerClock::with_fixed_base(1_800_000_000),
    ));
    let now = ctx.now_unix();
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);

    // 1. Shield resonances bounded 0..=1 and NaN/Inf damage rejected
    let res = skyhook_shield_resonances(&ctx.sde);
    assert!(res.iter().all(|r| (0.0..=1.0).contains(r)));
    assert_eq!(apply_skyhook_weapon_damage(&ctx, sh_id, f64::NAN).unwrap_err().code, "InvalidDamage");
    assert_eq!(
        apply_skyhook_weapon_damage_typed(&ctx, sh_id, 10.0, f64::INFINITY, 0.0, 0.0)
            .unwrap_err()
            .code,
        "InvalidDamage"
    );

    // 2. Inside theft window: dropping shield <= 10% sets hackable=true & keeps ShieldVulnerable even at 0 HP
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            sh_id,
            UpwellStructureRecord {
                structure_id: sh_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: null_sys,
                name: "InWindow Hook".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                shield_hp: SKYHOOK_BASE_SHIELD_HP,
                ..Default::default()
            },
        );
        snap.sov_runtime
            .skyhook_theft_windows
            .insert(sh_id, (now.saturating_sub(60), now + 3_600));
    });
    let neg = apply_skyhook_weapon_damage(&ctx, sh_id, -500.0).unwrap();
    assert_eq!(neg.dict_get("appliedDamage").and_then(PyRep::as_f64), Some(0.0));
    assert_eq!(neg.dict_get("hackable").and_then(PyRep::as_bool), Some(false));

    let low = apply_skyhook_weapon_damage(&ctx, sh_id, 25_000.0).unwrap();
    assert_eq!(low.dict_get("state").and_then(PyRep::as_str), Some("ShieldVulnerable"));
    assert_eq!(low.dict_get("hackable").and_then(PyRep::as_bool), Some(true));

    // 3. ShieldTooHigh & orbitalSkyhook.is_character_authorised + ExtractReagentsToShip errors & success
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&sh_id).unwrap().shield_hp = 2_000.0; // > 10%
        snap.sov_runtime
            .skyhook_reagent_bays
            .insert(sh_id, (TYPE_SUPERIONIC_ICE, 25, 10, now));
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 620,
                owner_id: char_id,
                location_id: i64::from(null_sys),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                dynamic_attributes: std::collections::HashMap::from([(38, 1_000.0)]),
                ..Default::default()
            },
        );
    });
    let sys_h = ctx.systems.get_or_spawn(null_sys);
    let _ = sys_h
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 620,
            position: DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let mut sess = SessionState::new(9503);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id + 1); // wrong corp -> unauthorized
    sess.solar_system_id = Some(null_sys);
    sess.ship_id = Some(ship_id);

    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &sess,
            null_sys,
            char_id,
            ship_id,
            sh_id,
            "linkWithShip",
            "InitiateLink",
            &[]
        )
        .await
        .unwrap_err()
        .code,
        "SkyhookSiloShieldTooHigh"
    );
    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &sess,
            null_sys,
            char_id,
            ship_id,
            sh_id,
            "orbitalSkyhook",
            "is_character_authorised",
            &[]
        )
        .await
        .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &sess,
            null_sys,
            char_id,
            ship_id,
            sh_id,
            "orbitalSkyhook",
            "ExtractReagentsToShip",
            &[]
        )
        .await
        .unwrap_err()
        .code,
        "NotAuthorisedForSkyhookExtraction"
    );

    sess.corporation_id = Some(corp_id);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &sess,
            null_sys,
            char_id,
            ship_id,
            sh_id,
            "orbitalSkyhook",
            "is_character_authorised",
            &[]
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );
    let ext = handle_skyhook_component_call(
        &ctx,
        &sess,
        null_sys,
        char_id,
        ship_id,
        sh_id,
        "orbitalSkyhook",
        "ExtractReagentsToShip",
        &[PyRep::None, PyRep::None, PyRep::None, PyRep::None, PyRep::Integer(10)],
    )
    .await
    .unwrap();
    assert_eq!(ext.dict_get("extractedQuantity").and_then(PyRep::as_i64), Some(10));
    assert_eq!(ext.dict_get("remainingSecureQuantity").and_then(PyRep::as_i64), Some(15));
}

#[test]
fn skyhook_theft_window_math_boundaries_clamp_and_fast_forward_pin_mutants() {
    assert_eq!(SKYHOOK_MATURITY_CYCLE_SECS, 86_400);

    let prev = 1_728_000_000_u64; // exact multiple of 86_400 (00:00 UTC)
    // Collect skyhook_ids hitting all hash % 3 == 0/1/2 and (hash >> 8) % 2 == 0/1 arms
    let mut seen_days = [false; 2];
    let mut seen_jitter = [false; 3];
    for id in 1_i64..200 {
        let h = (id.unsigned_abs())
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        seen_days[((h >> 8) % 2) as usize] = true;
        seen_jitter[(h % 3) as usize] = true;

        let (w_start, w_end) = next_skyhook_theft_window(id, prev, Some(12), prev);
        assert_eq!(w_end, w_start + 7_200);
        let expected_days = if (h >> 8) % 2 == 0 { 3 } else { 4 };
        let jitter = match h % 3 {
            0 => -3_600_i64,
            1 => 0_i64,
            _ => 3_600_i64,
        };
        let target = ((prev + expected_days * 86_400) as i64 + 12 * 3_600 + jitter) as u64;
        let base_window = prev + expected_days * 86_400;
        let expected = if target >= prev + 259_200 && target <= prev + 345_600 {
            target
        } else {
            base_window
        };
        assert_eq!(w_start, expected);

        let rh0_prev = prev + 12 * 3_600;
        let (rw_start, rw_end) = next_skyhook_theft_window(id, rh0_prev, Some(0), rh0_prev);
        assert_eq!(rw_end, rw_start + 7_200);
        if expected_days == 4 {
            assert_eq!(
                rw_start,
                ((rh0_prev + 4 * 86_400 - 12 * 3_600) as i64 + jitter) as u64
            );
        }
    }
    assert!(seen_days.iter().all(|&b| b) && seen_jitter.iter().all(|&b| b));

    // Boundary on while condition: now == first_w_end - 1 stays at first window; now == first_w_end rolls forward!
    let (f0, f1) = next_skyhook_theft_window(7, prev, None, prev);
    assert_eq!(next_skyhook_theft_window(7, prev, None, f1 - 1), (f0, f1));
    let (s0, s1) = next_skyhook_theft_window(7, prev, None, f1);
    assert!(s0 > f0 && s1 == s0 + 7_200);

    // Clamp outside [start + 259_200, start + 345_600] keeps raw 3-day or 4-day slot
    // Find id with days == 3 and hash % 3 == 0 (-3600) starting at hour 20 -> day_base + 0h - 1h < prev + 259_200!
    let prev_20h = prev + 20 * 3_600;
    let id_d3_j0 = (1_i64..500)
        .find(|&id| {
            let h = (id.unsigned_abs())
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (h >> 8) % 2 == 0 && h % 3 == 0
        })
        .unwrap();
    let (clamped_low, _) = next_skyhook_theft_window(id_d3_j0, prev_20h, Some(0), prev_20h);
    assert_eq!(clamped_low, prev_20h + 3 * 86_400);

    // rh % 24 test: Some(25) behaves like Some(1)
    assert_eq!(
        next_skyhook_theft_window(7, prev, Some(25), prev),
        next_skyhook_theft_window(7, prev, Some(1), prev)
    );

    // Fast-forward formula when cycle > 10_000 (~3500+ days ahead)
    let far_now = prev + 12_000 * 345_600;
    let expected_ff = prev + ((far_now - prev) / 302_400 + 1) * 302_400;
    assert_eq!(
        next_skyhook_theft_window(7, prev, Some(10), far_now),
        (expected_ff, expected_ff + 7_200)
    );

    // advance_skyhook_theft_window_in_snap boundary & hackable clearing
    let ctx = test_ctx();
    let mut snap = WorldStateSnapshot::default();
    let sh_id = 55_001_i64;
    snap.structures.insert(
        sh_id,
        UpwellStructureRecord {
            structure_id: sh_id,
            type_id: TYPE_ORBITAL_SKYHOOK,
            owner_corp_id: 1_000_009,
            solar_system_id: 30_000_142,
            state: "ShieldVulnerable".into(),
            ..Default::default()
        },
    );
    snap.corp_governance.insert(
        1_000_009,
        eve_db::CorpGovState {
            reinforce_hour: 15,
            ..Default::default()
        },
    );
    let w = (prev + 10_000, prev + 17_200);
    snap.sov_runtime.skyhook_theft_windows.insert(sh_id, w);
    snap.sov_runtime.skyhook_hackable.insert(sh_id, true);

    assert_eq!(
        advance_skyhook_theft_window_in_snap(&mut snap, sh_id, w.1 - 1),
        Some(w)
    );
    assert_eq!(snap.sov_runtime.skyhook_hackable.get(&sh_id), Some(&true));

    let next_w = advance_skyhook_theft_window_in_snap(&mut snap, sh_id, w.1).unwrap();
    assert_eq!(next_w, next_skyhook_theft_window(sh_id, w.0, Some(15), w.1));
    assert_eq!(snap.sov_runtime.skyhook_hackable.get(&sh_id), Some(&false));

    // advance_skyhook_reagents_in_snap: bay_ts > 0 init + 3599/3600 boundary + destroyed structure
    let sh2 = 55_002_i64;
    let (null_sys, planet_id, _) = find_null_system_with_reagent_planet(&ctx);
    snap.sov_runtime.skyhook_planets.insert(sh2, planet_id);
    snap.structures.insert(
        sh2,
        UpwellStructureRecord {
            structure_id: sh2,
            type_id: TYPE_ORBITAL_SKYHOOK,
            owner_corp_id: 1_000_009,
            solar_system_id: null_sys,
            state: "ShieldVulnerable".into(),
            ..Default::default()
        },
    );
    let bay_t0 = 1_700_000_000_u64;
    snap.sov_runtime
        .skyhook_reagent_bays
        .insert(sh2, (TYPE_SUPERIONIC_ICE, 0, 0, bay_t0));
    let r_before = advance_skyhook_reagents_in_snap(&mut snap, &ctx.sde, sh2, bay_t0 + 3_599).unwrap();
    assert_eq!(r_before, (TYPE_SUPERIONIC_ICE, 0, 0));
    assert_eq!(snap.sov_runtime.skyhook_last_harvest_unix.get(&sh2), Some(&bay_t0));

    let r_at = advance_skyhook_reagents_in_snap(&mut snap, &ctx.sde, sh2, bay_t0 + 3_600).unwrap();
    assert!(r_at.1 + r_at.2 > 0);
    assert_eq!(r_at.1, (r_at.1 + r_at.2) / 2);

    // Destroyed state blocks harvest even when skyhook_active map has no entry
    snap.structures.get_mut(&sh2).unwrap().state = "Destroyed".into();
    let r_dead = advance_skyhook_reagents_in_snap(&mut snap, &ctx.sde, sh2, bay_t0 + 10_800).unwrap();
    assert_eq!(r_dead.1 + r_dead.2, r_at.1 + r_at.2);
}

#[tokio::test]
async fn skyhook_anchor_filters_link_silo_initiation_and_tick_skyhooks_lifecycle() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let hacker_id = 90_000_002_i64;
    let (null_sys, planet_id, planet_pos) = find_null_system_with_reagent_planet(&ctx);
    let ship_id = 1_074_000_001_i64;
    let hacker_ship = 1_074_000_002_i64;
    let zero_qty_item = 1_074_000_010_i64;
    let locked_item = 1_074_000_011_i64;
    let valid_item = 1_074_000_012_i64;
    let sh_reinf = 1_074_000_101_i64;
    let sh_reinf_zero = 1_074_000_102_i64;
    let sh_hub = 1_074_000_103_i64;
    let sh_raid = 1_074_000_104_i64;
    ctx.set_clock(std::sync::Arc::new(
        eve_db::VirtualServerClock::with_fixed_base(1_800_000_000),
    ));
    let now = ctx.now_unix();
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let ally_id = 99_000_077_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ally_id);
        snap.corporations.entry(corp_id).or_default().alliance_id = Some(ally_id);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 620,
                owner_id: char_id,
                location_id: i64::from(null_sys),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            zero_qty_item,
            InventoryItemRecord {
                item_id: zero_qty_item,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 0,
                ..Default::default()
            },
        );
        snap.items.insert(
            locked_item,
            InventoryItemRecord {
                item_id: locked_item,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            96_740_001,
            eve_db::ContractRecord {
                contract_id: 96_740_001,
                issuer_id: char_id,
                acceptor_id: None,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                price: eve_db::IskCents(0),
                reward: eve_db::IskCents(0),
                collateral: eve_db::IskCents(0),
                item_ids: vec![locked_item],
                expiration_unix: now + 86_400,
                issuer_corp_id: corp_id,
                assignee_id: 0,
                availability: "Personal".into(),
                buyout: eve_db::IskCents(0),
                current_bid: eve_db::IskCents(0),
                current_bidder_id: None,
                requested_items: Vec::new(),
                bid_history: Vec::new(),
                date_issued_unix: now,
                title: "lock".into(),
                for_corp: false,
                num_days: 1,
            },
        );
    });

    let sys_h = ctx.systems.get_or_spawn(null_sys);
    let _ = sys_h
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 620,
            position: DVec3::new(planet_pos[0] + 10_000.0, planet_pos[1], planet_pos[2]),
            velocity: DVec3::ZERO,
            max_velocity: 2_000.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let mut sess = SessionState::new(9510);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(null_sys);
    sess.ship_id = Some(ship_id);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

    // 1. Anchoring skips quantity == 0 and contract-locked items -> MissingSkyhookItem
    assert_eq!(
        handle_anchor_skyhook_rpc(&ctx, &mut sess, &[PyRep::Integer(planet_id)])
            .await
            .unwrap_err()
            .code,
        "MissingSkyhookItem"
    );
    // Add valid item AFTER the zero-qty & locked items; it is selected & consumed cleanly
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            valid_item,
            InventoryItemRecord {
                item_id: valid_item,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let anchored = handle_anchor_skyhook_rpc(&ctx, &mut sess, &[PyRep::Integer(planet_id)])
        .await
        .expect("anchor succeeds with valid third item");
    assert!(anchored.dict_get("structureID").and_then(PyRep::as_i64).is_some());
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&valid_item));
        assert!(snap.items.contains_key(&locked_item));
        assert!(snap.items.contains_key(&zero_qty_item));
    });

    // 2. linkWithShip InitiateLink / InitiateLinkQuick branches + boundary
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            hacker_ship,
            InventoryItemRecord {
                item_id: hacker_ship,
                type_id: 587, // frigate -> ineligible first
                owner_id: hacker_id,
                location_id: i64::from(null_sys),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            sh_raid,
            UpwellStructureRecord {
                structure_id: sh_raid,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: null_sys,
                position: [planet_pos[0] + 10_000.0, planet_pos[1], planet_pos[2]],
                state: "ShieldVulnerable".into(),
                shield_hp: 1_000.0, // exactly 10%
                ..Default::default()
            },
        );
        snap.sov_runtime.skyhook_hackable.insert(sh_raid, false);
    });
    let _ = sys_h
        .spawn_ball(Ball {
            id: hacker_ship,
            type_id: 587,
            position: DVec3::new(planet_pos[0] + 12_000.0, planet_pos[1], planet_pos[2]),
            velocity: DVec3::ZERO,
            max_velocity: 2_000.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let mut h_sess = SessionState::new(9511);
    h_sess.user_id = Some(2);
    h_sess.character_id = Some(hacker_id);
    h_sess.corporation_id = Some(corp_id + 5);
    h_sess.solar_system_id = Some(null_sys);
    h_sess.ship_id = Some(hacker_ship);

    assert_eq!(
        handle_skyhook_component_call(&ctx, &h_sess, null_sys, hacker_id, hacker_ship, sh_raid, "linkWithShip", "InitiateLink", &[]).await.unwrap_err().code,
        "IneligibleShipForSkyhookLink"
    );
    // Switch to eligible cruiser (620)
    let _ = sys_h.remove_ball(hacker_ship).await;
    let _ = sys_h
        .spawn_ball(Ball {
            id: hacker_ship,
            type_id: 620,
            position: DVec3::new(planet_pos[0] + 12_000.0, planet_pos[1], planet_pos[2]),
            velocity: DVec3::ZERO,
            max_velocity: 2_000.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        handle_skyhook_component_call(&ctx, &h_sess, null_sys, hacker_id, hacker_ship, sh_raid, "linkWithShip", "InitiateLinkQuick", &[]).await.unwrap_err().code,
        "SkyhookNotVulnerableToTheft"
    );
    // Fallback skyhook_hackable == true (no theft window entry) passes vulnerability check, hits InsecureReagentBayEmpty
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_hackable.insert(sh_raid, true);
    });
    assert_eq!(
        handle_skyhook_component_call(&ctx, &h_sess, null_sys, hacker_id, hacker_ship, sh_raid, "linkWithShip", "InitiateLink", &[]).await.unwrap_err().code,
        "InsecureReagentBayEmpty"
    );
    // Populate insecure bay & add expired link -> Initiates link cleanly via InitiateLinkQuick!
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .skyhook_reagent_bays
            .insert(sh_raid, (TYPE_SUPERIONIC_ICE, 20, 35, now));
        snap.sov_runtime
            .skyhook_silo_links
            .insert(sh_raid, (hacker_id, hacker_ship, now, 2_000.0)); // expired: now < until is false!
    });
    let link_res = handle_skyhook_component_call(
        &ctx, &h_sess, null_sys, hacker_id, hacker_ship, sh_raid, "linkWithShip", "InitiateLinkQuick", &[]
    ).await.unwrap();
    assert_eq!(link_res.dict_get("skyhookID").and_then(PyRep::as_i64), Some(sh_raid));
    assert_eq!(link_res.dict_get("shipID").and_then(PyRep::as_i64), Some(hacker_ship));
    assert_eq!(link_res.dict_get("linked").and_then(PyRep::as_bool), Some(true));
    assert_eq!(link_res.dict_get("linkDurationSeconds").and_then(PyRep::as_i64), Some(SKYHOOK_SILO_LINK_DURATION_SECS as i64));
    assert_eq!(link_res.dict_get("completesAtUnix").and_then(PyRep::as_i64), Some((now + SKYHOOK_SILO_LINK_DURATION_SECS) as i64));
    assert_eq!(link_res.dict_get("maxVelocityCap").and_then(PyRep::as_f64), Some(SKYHOOK_SILO_LINK_MAX_SPEED_MS));

    // Duplicate while active -> SkyhookAlreadyLinked
    assert_eq!(
        handle_skyhook_component_call(&ctx, &h_sess, null_sys, hacker_id, hacker_ship, sh_raid, "linkWithShip", "InitiateLink", &[]).await.unwrap_err().code,
        "SkyhookAlreadyLinked"
    );

    // 3. tick_skyhooks: ArmorReinforced expiration, non-expired / zero / non-skyhook preservation, ongoing link cap, broken cloak & completed drop
    ctx.db.with_mut_snapshot(|snap| {
        // Purely in snap.structures (not in skyhook_reagent_bays or skyhook_active)
        snap.structures.insert(
            sh_reinf,
            UpwellStructureRecord {
                structure_id: sh_reinf,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: null_sys,
                state: "ArmorReinforced".into(),
                reinforced_until_unix: now,
                shield_hp: 500.0,
                ..Default::default()
            },
        );
        snap.structures.insert(
            sh_reinf_zero,
            UpwellStructureRecord {
                structure_id: sh_reinf_zero,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: null_sys,
                state: "ArmorReinforced".into(),
                reinforced_until_unix: 0,
                ..Default::default()
            },
        );
        snap.structures.insert(
            sh_hub,
            UpwellStructureRecord {
                structure_id: sh_hub,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: corp_id,
                solar_system_id: null_sys,
                state: "ArmorReinforced".into(),
                reinforced_until_unix: now.saturating_sub(10),
                ..Default::default()
            },
        );
        snap.sov_runtime
            .skyhook_theft_windows
            .insert(sh_raid, (now.saturating_sub(60), now + 3_600));
    });
    // Ongoing link tick clamps velocity if > 1000
    tick_skyhooks(&ctx, now + 10).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&sh_reinf].state, "ArmorVulnerable");
        assert_eq!(snap.structures[&sh_reinf].reinforced_until_unix, 0);
        assert_eq!(snap.structures[&sh_reinf_zero].state, "ArmorReinforced");
        assert_eq!(snap.structures[&sh_hub].state, "ArmorReinforced");
        assert!(snap.sov_runtime.skyhook_silo_links.contains_key(&sh_raid));
    });

    // Complete link at now + SKYHOOK_SILO_LINK_DURATION_SECS: drops container ball & clears insecure bay + maturity_rem
    tick_skyhooks(&ctx, now + SKYHOOK_SILO_LINK_DURATION_SECS).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.skyhook_silo_links.contains_key(&sh_raid));
        let bay = snap.sov_runtime.skyhook_reagent_bays[&sh_raid];
        assert_eq!(bay.2, 0);
        assert_eq!(snap.sov_runtime.skyhook_maturity_rem.get(&sh_raid), Some(&(0, 0)));
    });

    // Cloak break restores velocity when orig_vel > 0.0
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .skyhook_silo_links
            .insert(sh_raid, (hacker_id, hacker_ship, now + 9_999, 2_000.0));
    });
    let _ = sys_h
        .set_combat_state(hacker_ship, eve_destiny::CombatState { is_cloaked: true, ..Default::default() })
        .await;
    tick_skyhooks(&ctx, now + SKYHOOK_SILO_LINK_DURATION_SECS + 1).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.skyhook_silo_links.contains_key(&sh_raid));
    });
}

