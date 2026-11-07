//! Deep branch-coverage and mutation-killing unit tests for `sov_gameplay_skyhook` (`Q-8`, `SO-5`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::sov_gameplay::sov_gameplay_skyhook::*;
use crate::services::sov_services::{
    TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB, TYPE_SUPERIONIC_ICE,
};
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord, WorldStateSnapshot};
use eve_destiny::{Ball, CombatState, MovementMode};
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

pub(super) fn find_null_system_with_reagent_planet(ctx: &ServerContext) -> (u32, i64, [f64; 3]) {
    for sys in ctx.sde.solar_systems.values() {
        if crate::services::sov_gameplay::is_claimable_nullsec_system(&ctx.sde, sys.solar_system_id)
        {
            for p in ctx.sde.celestials_in_system(sys.solar_system_id) {
                if p.group_id == 7 && planet_reagent_type(p.type_id).is_some() {
                    return (
                        sys.solar_system_id,
                        p.celestial_id,
                        [p.position.x, p.position.y, p.position.z],
                    );
                }
            }
        }
    }
    (30_000_142, 40_009_077, [1.0e9, 0.0, 0.0])
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
