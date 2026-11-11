//! Additional edge-case and branch-coverage tests for `sov_gameplay_skyhook`.

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::sov_services::{TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SUPERIONIC_ICE};
use eve_db::{
    ContractRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord, WorldRepository,
    WorldStateSnapshot,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::sync::{Arc, OnceLock};

fn test_ctx() -> ServerContext {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    let sde = Arc::clone(SDE.get_or_init(|| {
        Arc::new(SdeRegistry::open(SdeRegistry::default_db_path()).expect("open default SDE"))
    }));
    ServerContext::with_sde_and_repository(sde, Arc::new(WorldRepository::with_demo_seed()))
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn find_null_system_with_reagent_planet(ctx: &ServerContext) -> (u32, i64, [f64; 3]) {
    for sys in ctx.sde.solar_systems.values() {
        if crate::services::sov_gameplay::is_claimable_nullsec_system(&ctx.sde, sys.solar_system_id) {
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
fn skyhook_helpers_hull_eligibility_reagent_harvest_and_damage_edges() {
    let ctx = test_ctx();

    // 1. extract_extra_var with KeyVal & case-insensitive Dict
    let kv = PyRep::keyval([("Other", PyRep::Integer(1)), ("PlanetID", PyRep::Integer(42))]);
    assert_eq!(extract_extra_var(Some(&kv), "planetID"), Some(42));
    assert_eq!(extract_extra_var(Some(&kv), "missing"), None);
    let dict = PyRep::Dict(vec![(PyRep::String("PLANET_ID".into()), PyRep::Integer(99))]);
    assert_eq!(extract_extra_var(Some(&dict), "planet_id"), Some(99));
    assert_eq!(extract_extra_var(None, "planetID"), None);

    // 2. is_eligible_skyhook_link_hull: unknown, non-ship, capsule/frigate/destroyer rejected; cruiser/industrial accepted.
    assert!(!is_eligible_skyhook_link_hull(&ctx, 999_999_999));
    assert!(!is_eligible_skyhook_link_hull(&ctx, TYPE_ORBITAL_SKYHOOK)); // structure, not category 6
    assert!(!is_eligible_skyhook_link_hull(&ctx, 670)); // Capsule (group 29)
    assert!(!is_eligible_skyhook_link_hull(&ctx, 587)); // Rifter Frigate (group 25)
    assert!(!is_eligible_skyhook_link_hull(&ctx, 16236)); // Coercer Destroyer (group 420)
    assert!(is_eligible_skyhook_link_hull(&ctx, 620)); // Osprey Cruiser (group 26)
    assert!(is_eligible_skyhook_link_hull(&ctx, 648)); // Badger Industrial (group 28)

    // 3. advance_skyhook_reagents_in_snap: active hourly harvest splits 50/50 into secure/insecure,
    // and structure state "Destroyed" suppresses hourly harvest while maturing existing insecure bay.
    let mut snap = WorldStateSnapshot::default();
    let sh_id = 8_001_i64;
    let t0 = 1_850_000_000_u64;
    snap.structures.insert(
        sh_id,
        UpwellStructureRecord {
            structure_id: sh_id,
            type_id: TYPE_ORBITAL_SKYHOOK,
            state: "ShieldVulnerable".into(),
            ..Default::default()
        },
    );
    snap.sov_runtime
        .skyhook_reagent_bays
        .insert(sh_id, (TYPE_SUPERIONIC_ICE, 0, 0, t0));
    snap.sov_runtime.skyhook_last_harvest_unix.insert(sh_id, t0);
    // Advance 2 hours (7200s): harvests 2 * hourly_yield, half to secure, half to insecure.
    let (_, sec1, insec1) = advance_skyhook_reagents_in_snap(&mut snap, &ctx.sde, sh_id, t0 + 7_200).unwrap();
    assert!(sec1 > 0 && insec1 > 0);
    assert!(sec1 == insec1 || sec1 + 1 == insec1);

    // Mark structure Destroyed -> no new harvest after another 2 hours, only maturity of existing insecure.
    snap.structures.get_mut(&sh_id).unwrap().state = "Destroyed".into();
    let total_before = sec1 + insec1;
    let (_, sec2, insec2) =
        advance_skyhook_reagents_in_snap(&mut snap, &ctx.sde, sh_id, t0 + 14_400).unwrap();
    assert_eq!(sec2 + insec2, total_before);
    assert!(sec2 > sec1);

    // 4. apply_skyhook_weapon_damage & typed damage error/edge paths
    assert_eq!(
        apply_skyhook_weapon_damage(&ctx, 1, f64::NAN).unwrap_err().code,
        "InvalidDamage"
    );
    assert_eq!(
        apply_skyhook_weapon_damage(&ctx, 1, f64::INFINITY).unwrap_err().code,
        "InvalidDamage"
    );
    assert_eq!(
        apply_skyhook_weapon_damage_typed(&ctx, 1, 1.0, f64::NAN, 0.0, 0.0)
            .unwrap_err()
            .code,
        "InvalidDamage"
    );

    // Inside theft window, dropping shield to 0 stays ShieldVulnerable (does NOT enter ArmorReinforced) and sets hackable=true;
    // Also expired ArmorReinforced (now >= reinforced_until_unix) allows damage again.
    let now = ctx.now_unix();
    let sh2 = 8_002_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.structures.insert(
            sh2,
            UpwellStructureRecord {
                structure_id: sh2,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_142,
                state: "ArmorReinforced".into(),
                shield_hp: 5_000.0,
                reinforced_until_unix: now.saturating_sub(10), // expired reinforcement!
                ..Default::default()
            },
        );
        s.sov_runtime
            .skyhook_theft_windows
            .insert(sh2, (now.saturating_sub(60), now + 3_600));
    });
    // Negative damage clamps to 0.0 applied.
    let neg_res = apply_skyhook_weapon_damage(&ctx, sh2, -500.0).unwrap();
    assert_eq!(neg_res.dict_get("appliedDamage").and_then(PyRep::as_f64), Some(0.0));
    // Heavy typed damage drops shield to 0 inside theft window -> stays ArmorReinforced/ShieldVulnerable (not re-reinforced), hackable=true.
    let zero_res = apply_skyhook_weapon_damage_typed(&ctx, sh2, 50_000.0, 0.0, 0.0, 0.0).unwrap();
    assert_eq!(zero_res.dict_get("shieldHP").and_then(PyRep::as_f64), Some(0.0));
    assert_eq!(zero_res.dict_get("hackable").and_then(PyRep::as_bool), Some(true));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anchor_skyhook_extra_paths_and_component_extraction_edges() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let (null_sys, planet_id, planet_pos) = find_null_system_with_reagent_planet(&ctx);
    let ship_id = 1_081_000_001_i64;
    let locked_item = 1_081_000_010_i64;
    let stack_item = 1_081_000_011_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let ally_id = 99_800_001_i64;

    let mut sess = SessionState::new(9601);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

    // 1. structure_id.is_some() -> ShipNotInSpace
    sess.structure_id = Some(1_000_001);
    assert_eq!(
        handle_anchor_skyhook_rpc(&ctx, &mut sess, &[])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );
    sess.structure_id = None;

    // 2. solar_system_id == None -> NoSolarSystem; ship_id == None -> ShipNotInSpace
    assert_eq!(
        handle_anchor_skyhook_rpc(&ctx, &mut sess, &[])
            .await
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );
    sess.solar_system_id = Some(null_sys);
    assert_eq!(
        handle_anchor_skyhook_rpc(&ctx, &mut sess, &[])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );
    sess.ship_id = Some(ship_id);

    // Spawn ship within 500km of planet and configure sov + items.
    let sys_handle = ctx.systems.get_or_spawn(null_sys);
    sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 620, // Cruiser (eligible)
            position: DVec3::from_array(planet_pos) + DVec3::new(150_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 2_000.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();

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
        // Locked skyhook item in open contract -> ItemLockedInContractOrJob
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
            555_001,
            ContractRecord {
                contract_id: 555_001,
                issuer_id: char_id,
                status: "Outstanding".into(),
                item_ids: vec![locked_item],
                ..Default::default()
            },
        );
        // Unlocked stack of 2 skyhook items -> decrementing quantity leaves 1!
        snap.items.insert(
            stack_item,
            InventoryItemRecord {
                item_id: stack_item,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                ..Default::default()
            },
        );
    });

    assert_eq!(
        handle_anchor_skyhook_rpc(
            &ctx,
            &mut sess,
            &[PyRep::Integer(locked_item), PyRep::Integer(planet_id)],
        )
        .await
        .unwrap_err()
        .code,
        "ItemLockedInContractOrJob"
    );

    // 3. Anchor via 7-element Tuple with extra_vars KeyVal {"planetID": planet_id}, using TYPE_ORBITAL_SKYHOOK as arg0
    // so it auto-selects the unlocked `stack_item` and decrements its quantity from 2 to 1!
    let tuple_args = vec![PyRep::Tuple(vec![
        PyRep::Integer(i64::from(TYPE_ORBITAL_SKYHOOK)),
        PyRep::Integer(0),
        PyRep::Real(0.0),
        PyRep::Real(0.0),
        PyRep::Real(0.0),
        PyRep::String("Hook".into()),
        PyRep::keyval([("planetID", PyRep::Integer(planet_id))]),
    ])];
    let anchored = handle_anchor_skyhook_rpc(&ctx, &mut sess, &tuple_args)
        .await
        .unwrap();
    let skyhook_id = anchored.dict_get("structureID").and_then(PyRep::as_i64).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&stack_item].quantity, 1);
    });

    // 4. Second anchor at same planet -> SkyhookAlreadyExistsAtPlanet
    assert_eq!(
        handle_anchor_skyhook_rpc(&ctx, &mut sess, &[PyRep::Integer(planet_id)])
            .await
            .unwrap_err()
            .code,
        "SkyhookAlreadyExistsAtPlanet"
    );

    // 5. Component calls:
    // - TargetNotInSameSystem
    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &sess,
            30_000_142,
            char_id,
            ship_id,
            skyhook_id,
            "orbitalSkyhook",
            "is_character_authorised",
            &[],
        )
        .await
        .unwrap_err()
        .code,
        "ShipNotInSpace" // ship not in 30_000_142; test TargetNotInSameSystem with ship in 30_000_142:
    );
    ctx.systems
        .get_or_spawn(30_000_142)
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 620,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 2_000.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &sess,
            30_000_142,
            char_id,
            ship_id,
            skyhook_id,
            "orbitalSkyhook",
            "is_character_authorised",
            &[],
        )
        .await
        .unwrap_err()
        .code,
        "TargetNotInSameSystem"
    );

    // - SkyhookSiloShieldTooHigh while shield_hp == 10_000.0 (> 10%)
    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &sess,
            null_sys,
            char_id,
            ship_id,
            skyhook_id,
            "linkWithShip",
            "InitiateLink",
            &[],
        )
        .await
        .unwrap_err()
        .code,
        "SkyhookSiloShieldTooHigh"
    );

    // - is_character_authorised true for owner director, false for other corp
    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &sess,
            null_sys,
            char_id,
            ship_id,
            skyhook_id,
            "orbitalSkyhook",
            "is_character_authorised",
            &[],
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );
    let mut other_sess = sess.clone();
    other_sess.corporation_id = Some(98_999_999);
    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &other_sess,
            null_sys,
            char_id,
            ship_id,
            skyhook_id,
            "orbitalSkyhook",
            "is_character_authorised",
            &[],
        )
        .await
        .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &other_sess,
            null_sys,
            char_id,
            ship_id,
            skyhook_id,
            "orbitalSkyhook",
            "ExtractReagentsToShip",
            &[],
        )
        .await
        .unwrap_err()
        .code,
        "NotAuthorisedForSkyhookExtraction"
    );

    // - ExtractReagentsToShip: NoSecuredReagentsAvailable, SkyhookReagentBayNotFound,
    // NotEnoughCargoSpace, and partial requested quantity (args[4]).
    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &sess,
            null_sys,
            char_id,
            ship_id,
            skyhook_id,
            "orbitalSkyhook",
            "ExtractReagentsToShip",
            &[],
        )
        .await
        .unwrap_err()
        .code,
        "NoSecuredReagentsAvailable"
    );

    let saved_bay = ctx.db.with_mut_snapshot(|s| s.sov_runtime.skyhook_reagent_bays.remove(&skyhook_id));
    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &sess,
            null_sys,
            char_id,
            ship_id,
            skyhook_id,
            "orbitalSkyhook",
            "ExtractReagentsToShip",
            &[],
        )
        .await
        .unwrap_err()
        .code,
        "SkyhookReagentBayNotFound"
    );

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|s| {
        if let Some(mut b) = saved_bay {
            b.1 = 100; // 100 secured
            b.3 = now;
            s.sov_runtime.skyhook_reagent_bays.insert(skyhook_id, b);
        } else {
            s.sov_runtime
                .skyhook_reagent_bays
                .insert(skyhook_id, (TYPE_MAGMATIC_GAS, 100, 0, now));
        }
        // Fill ship cargo to provoke NotEnoughCargoSpace (remove stack_item & insert giant item)
        s.items.remove(&stack_item);
        s.items.insert(
            1_081_000_099,
            InventoryItemRecord {
                item_id: 1_081_000_099,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_skyhook_component_call(
            &ctx,
            &sess,
            null_sys,
            char_id,
            ship_id,
            skyhook_id,
            "orbitalSkyhook",
            "ExtractReagentsToShip",
            &[],
        )
        .await
        .unwrap_err()
        .code,
        "NotEnoughCargoSpace"
    );

    // Clear giant cargo items and extract exact requested quantity (15 out of 100).
    ctx.db.with_mut_snapshot(|s| {
        s.items.remove(&1_081_000_099);
        s.items.remove(&locked_item);
    });
    let ext = handle_skyhook_component_call(
        &ctx,
        &sess,
        null_sys,
        char_id,
        ship_id,
        skyhook_id,
        "orbitalSkyhook",
        "ExtractReagentsToShip",
        &[
            PyRep::None,
            PyRep::None,
            PyRep::None,
            PyRep::None,
            PyRep::Integer(15),
        ],
    )
    .await
    .unwrap();
    assert_eq!(ext.dict_get("extractedQuantity").and_then(PyRep::as_i64), Some(15));
    assert_eq!(
        ext.dict_get("remainingSecureQuantity")
            .and_then(PyRep::as_i64),
        Some(85)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tick_skyhooks_empty_window_rollover_speed_cap_and_missing_ship_cleanup() {
    let ctx = test_ctx();
    let now = ctx.now_unix();

    // 1. Empty skyhooks & links -> fast no-op return.
    tick_skyhooks(&ctx, now).await;

    // 2. Expired theft window rolls forward by >= 302_400s, and low shield inside open window flips hackable -> true!
    let sys_id = 30_000_142_u32;
    let sh1 = 1_082_000_001_i64;
    let sh2 = 1_082_000_002_i64;
    let ship1 = 1_082_000_011_i64;
    let ship2 = 1_082_000_012_i64;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball(Ball {
            id: ship1,
            type_id: 620,
            position: DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 2_500.0, // > 1_000.0 -> tick_skyhooks caps speed while link active
            inertia: 1.0,
            mass: 1e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    sys_handle
        .set_combat_state(ship1, CombatState::default())
        .await
        .unwrap();

    ctx.db.with_mut_snapshot(|s| {
        // sh1: expired window far in the past, low shield, active link not yet completed
        s.structures.insert(
            sh1,
            UpwellStructureRecord {
                structure_id: sh1,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                state: "ShieldVulnerable".into(),
                shield_hp: 500.0,
                position: [0.0, 0.0, 0.0],
                ..Default::default()
            },
        );
        s.sov_runtime.skyhook_active.insert(sh1, true);
        s.sov_runtime
            .skyhook_theft_windows
            .insert(sh1, (now - 1_000_000, now - 992_800));
        s.sov_runtime
            .skyhook_silo_links
            .insert(sh1, (90_000_001, ship1, now + 300, 2_500.0));

        // sh2: Destroyed skyhook with an orphaned link -> removed cleanly
        s.structures.insert(
            sh2,
            UpwellStructureRecord {
                structure_id: sh2,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                state: "Destroyed".into(),
                position: [0.0, 0.0, 0.0],
                ..Default::default()
            },
        );
        s.sov_runtime
            .skyhook_silo_links
            .insert(sh2, (90_000_001, ship2, now + 300, 2_000.0));
    });

    tick_skyhooks(&ctx, now).await;

    ctx.db.with_snapshot(|s| {
        // Expired window rolled forward past `now`
        let (_, win_end) = s.sov_runtime.skyhook_theft_windows[&sh1];
        assert!(win_end > now);
        // Destroyed sh2 link was removed
        assert!(!s.sov_runtime.skyhook_silo_links.contains_key(&sh2));
        // Active sh1 link kept
        assert!(s.sov_runtime.skyhook_silo_links.contains_key(&sh1));
    });
    // ship1 max_velocity capped to SKYHOOK_SILO_LINK_MAX_SPEED_MS
    let (_, b1) = sys_handle.get_ball_snapshot(ship1).await.unwrap().unwrap();
    assert!((b1.max_velocity - SKYHOOK_SILO_LINK_MAX_SPEED_MS).abs() < 1e-6);

    // Complete sh1 link with 0 insecure reagents -> drop_res returns None, yet ship velocity is restored!
    ctx.db.with_mut_snapshot(|s| {
        s.sov_runtime
            .skyhook_reagent_bays
            .insert(sh1, (TYPE_SUPERIONIC_ICE, 10, 0, now + 400));
    });
    tick_skyhooks(&ctx, now + 400).await;
    let (_, b1_done) = sys_handle.get_ball_snapshot(ship1).await.unwrap().unwrap();
    assert!((b1_done.max_velocity - 2_500.0).abs() < 1e-6);
    assert!(!is_ship_skyhook_linked(&ctx, ship1, now + 400));
}
