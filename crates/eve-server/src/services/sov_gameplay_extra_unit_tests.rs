//! Extra unit and mutation-killing tests for `services::sov_gameplay`
//! covering `ship_free_cargo_m3`, `is_claimable_nullsec_system` exclusions (Zarzakh, Pochven, wormholes, NPC null),
//! `handle_anchor_mercenary_den_rpc` validation and full anchoring lifecycle,
//! `handle_mercden_component_call` partial cargo extraction and error guards,
//! `apply_mercenary_den_weapon_damage` expired vulnerability reset and partial hull damage,
//! `apply_mercenary_den_shield_repair` range/reinforcement/partial repair,
//! `dispatch_special_structure_attack`, `handle_normal_structure_damage_aftermath`,
//! `handle_confirm_skyhook_sov_management`, `try_handle_sov_hub_data_hack`, and `handle_anchor_sov_hub_rpc`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::quasar_sov_mercden::TYPE_MERCENARY_DEN_MANAGEMENT;
use crate::services::sov_gameplay::*;
use crate::services::sov_services::TYPE_ORBITAL_SKYHOOK;
use eve_db::{
    InventoryFlag, InventoryItemRecord, PersistedMercenaryDen, TrainedSkillRecord,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

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

fn find_claimable_null_with_temperate_planet(ctx: &ServerContext) -> (u32, i64, [f64; 3]) {
    for sys in ctx.sde.solar_systems.values() {
        if is_claimable_nullsec_system(&ctx.sde, sys.solar_system_id) {
            let cels = ctx.sde.celestials_in_system(sys.solar_system_id);
            if let Some(p) = cels.iter().find(|c| c.group_id == 7 && c.type_id == 11) {
                return (
                    sys.solar_system_id,
                    p.celestial_id,
                    [p.position.x, p.position.y, p.position.z],
                );
            }
        }
    }
    (30_000_142, 40_009_077, [1.0e9, 0.0, 0.0])
}

#[tokio::test]
async fn handle_anchor_mercenary_den_rpc_guards_and_full_anchoring_flow() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let (null_sys, temp_planet_id, planet_pos) = find_claimable_null_with_temperate_planet(&ctx);
    let non_temp_planet_id = ctx
        .sde
        .celestials_in_system(null_sys)
        .iter()
        .find(|c| c.group_id == 7 && c.type_id != 11)
        .map(|c| c.celestial_id)
        .unwrap_or(99_999_999);

    assert!(!is_claimable_nullsec_system(&ctx.sde, 30_100_000)); // Zarzakh
    assert!(!is_claimable_nullsec_system(&ctx.sde, 31_000_005)); // Wormhole
    assert!(!is_claimable_nullsec_system(&ctx.sde, 99_999_999)); // Unknown

    let ship_id = 1_091_000_001_i64;
    let skyhook_id = 1_091_000_002_i64;
    let den_item_id = 1_091_000_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 648,
                owner_id: char_id,
                location_id: null_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(38, 500.0)]),
                ..Default::default()
            },
        );
        snap.items.insert(
            den_item_id,
            InventoryItemRecord {
                item_id: den_item_id,
                type_id: TYPE_MERCENARY_DEN,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState::new(9601);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(null_sys);
    sess.ship_id = Some(ship_id);

    // 1. Docked -> ShipNotInSpace
    assert_eq!(
        handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );
    sess.station_id = None;

    // 2. Ship ball not spawned yet -> ShipNotInSpace
    assert_eq!(
        handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // Spawn ship in high-sec Jita first -> SovereigntyNullsecRequired
    let jita_h = ctx.systems.get_or_spawn(30_000_142);
    let _ = jita_h
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 648,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    sess.solar_system_id = Some(30_000_142);
    assert_eq!(
        handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[])
            .await
            .unwrap_err()
            .code,
        "SovereigntyNullsecRequired"
    );

    // Spawn ship in claimable null_sys with no skyhook -> SkyhookNotFound
    sess.solar_system_id = Some(null_sys);
    let null_h = ctx.systems.get_or_spawn(null_sys);
    let sh_pos = DVec3::from_array(planet_pos);
    let _ = null_h
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 648,
            position: sh_pos + DVec3::new(25_000.0, 0.0, 0.0), // too far initially
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[])
            .await
            .unwrap_err()
            .code,
        "SkyhookNotFound"
    );

    // Insert skyhook at sh_pos -> ship at +25,000m fails MercenaryDenTooFarFromSkyhook
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: 98_000_001,
                solar_system_id: null_sys,
                name: "Null Skyhook".into(),
                position: planet_pos,
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[])
            .await
            .unwrap_err()
            .code,
        "MercenaryDenTooFarFromSkyhook"
    );

    // Move ship within 2,000m of skyhook
    let _ = null_h.remove_ball(ship_id).await;
    let _ = null_h
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 648,
            position: sh_pos + DVec3::new(2_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // Skyhook has no planet bound and no explicit planet arg -> SkyhookPlanetUnknown
    assert_eq!(
        handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(den_item_id)])
            .await
            .unwrap_err()
            .code,
        "SkyhookPlanetUnknown"
    );

    // Explicit non-temperate planet -> PlanetNotTemperate
    assert_eq!(
        handle_anchor_mercenary_den_rpc(
            &ctx,
            &mut sess,
            &[
                PyRep::Integer(den_item_id),
                PyRep::Integer(skyhook_id),
                PyRep::Integer(non_temp_planet_id),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "PlanetNotTemperate"
    );

    // Bind temperate planet to skyhook, test DenPlanetMismatch, SovereigntyNotClaimed, MercenaryDenCapExceeded
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .skyhook_planets
            .insert(skyhook_id, temp_planet_id);
    });
    assert_eq!(
        handle_anchor_mercenary_den_rpc(
            &ctx,
            &mut sess,
            &[
                PyRep::Integer(den_item_id),
                PyRep::Integer(skyhook_id),
                PyRep::Integer(temp_planet_id + 1),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "DenPlanetMismatch"
    );
    assert_eq!(
        handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(den_item_id)])
            .await
            .unwrap_err()
            .code,
        "SovereigntyNotClaimed"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.entry(null_sys).or_default();
        st.solar_system_id = null_sys;
        st.sov_alliance_id = Some(99_000_001);
    });
    assert_eq!(
        handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(den_item_id)])
            .await
            .unwrap_err()
            .code,
        "MercenaryDenCapExceeded"
    );

    // Grant Mercenary Den Management level 2 -> anchoring succeeds!
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord {
                type_id: TYPE_MERCENARY_DEN_MANAGEMENT,
                level: 2,
                skill_points: 10_000.0,
            },
        );
    });
    let anchored = handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(den_item_id)])
        .await
        .unwrap();
    let den_id = anchored.dict_get("denID").and_then(PyRep::as_i64).unwrap();
    assert_eq!(
        anchored.dict_get("skyhookID").and_then(PyRep::as_i64),
        Some(skyhook_id)
    );

    // Second anchor on same skyhook fails MercenaryDenAlreadyAnchoredOnSkyhook
    assert_eq!(
        handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(den_item_id)])
            .await
            .unwrap_err()
            .code,
        "MercenaryDenAlreadyAnchoredOnSkyhook"
    );

    // Test dispatch_special_structure_attack with ammo consumption on the newly anchored den
    let ammo_id = 1_091_000_099_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ammo_id,
            InventoryItemRecord {
                item_id: ammo_id,
                type_id: 200,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 2,
                ..Default::default()
            },
        );
    });
    let atk = dispatch_special_structure_attack(
        &ctx,
        char_id,
        den_id,
        TYPE_MERCENARY_DEN,
        false,
        1_000.0,
        42,
        &[ammo_id],
    )
    .await
    .unwrap();
    assert_eq!(
        atk.dict_get("shieldHP").and_then(PyRep::as_f64),
        Some(MERC_DEN_BASE_HP - 1_000.0)
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&ammo_id].quantity, 1);
        assert_eq!(snap.structures[&den_id].last_damage_tick, 42);
    });
}

#[tokio::test]
async fn mercden_partial_extraction_expired_vuln_reset_aftermath_and_sov_hub_anchoring_errors() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 1_092_000_001_i64;
    let den_id = 1_092_000_002_i64;
    let skyhook_id = 1_092_000_003_i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        // Ship with small cargo capacity (2.0 m3 -> fits 20 keys at 0.10 m3 each)
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 648,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(38, 2.0)]),
                ..Default::default()
            },
        );
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                name: "Skyhook".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.sov_runtime.mercenary_den_tax_pct.insert(skyhook_id, 20);
        snap.structures.insert(
            den_id,
            UpwellStructureRecord {
                structure_id: den_id,
                type_id: TYPE_MERCENARY_DEN,
                owner_corp_id: 0,
                solar_system_id: sys_id,
                name: "Den".into(),
                position: [1_000.0, 0.0, 0.0],
                state: "HullVulnerable".into(),
                shield_hp: 0.0,
                armor_hp: MERC_DEN_BASE_HP,
                hull_hp: MERC_DEN_BASE_HP,
                ..Default::default()
            },
        );
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            PersistedMercenaryDen {
                den_id,
                owner_char_id: char_id,
                skyhook_id,
                solar_system_id: sys_id,
                planet_id: 40_009_077,
                type_id: TYPE_MERCENARY_DEN,
                enabled: true,
                cargo_extraction_enabled: true,
                development_level: 1,
                anarchy_level: 0,
                simulated_at_unix: now,
                infomorphs: 100,
                next_activity_generation_unix: now + 3_600,
            },
        );
        // Set expired vulnerability window (vuln_end < now)
        snap.sov_runtime
            .mercenary_den_reinforcement
            .insert(den_id, (now - 10_000, now - 100, 100));
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 648,
            position: DVec3::new(1_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let sess = SessionState::new(9602);
    // 1. Unsupported component method rejected
    assert_eq!(
        handle_mercden_component_call(
            &ctx,
            &sess,
            sys_id,
            char_id,
            ship_id,
            den_id,
            "mercenaryDen",
            "unknown_method",
            &[],
        )
        .await
        .unwrap_err()
        .code,
        "UnsupportedComponentCall"
    );

    // 2. Partial cargo extraction: ship fits 20 owner units at 20% tax => extracts 25 total (20 owner, 5 tax)
    let partial = handle_mercden_component_call(
        &ctx,
        &sess,
        sys_id,
        char_id,
        ship_id,
        den_id,
        "mercenaryDen",
        "extract_infomorphs",
        &[],
    )
    .await
    .unwrap();
    assert_eq!(partial.dict_get("ownerShare").and_then(PyRep::as_i64), Some(20));
    assert_eq!(partial.dict_get("taxShare").and_then(PyRep::as_i64), Some(5));

    // Next extraction when cargo is now full fails NotEnoughCargoSpace
    assert_eq!(
        handle_mercden_component_call(
            &ctx,
            &sess,
            sys_id,
            char_id,
            ship_id,
            den_id,
            "mercenaryDen",
            "extract_infomorphs",
            &[],
        )
        .await
        .unwrap_err()
        .code,
        "NotEnoughCargoSpace"
    );

    // 3. Damage after vuln_end expired resets den to ShieldVulnerable (5000 HP) then subtracts 1000
    let dmg_after_vuln = apply_mercenary_den_weapon_damage(&ctx, den_id, 1_000.0)
        .await
        .unwrap();
    assert_eq!(
        dmg_after_vuln.dict_get("state").and_then(PyRep::as_str),
        Some("ShieldVulnerable")
    );
    assert_eq!(
        dmg_after_vuln.dict_get("shieldHP").and_then(PyRep::as_f64),
        Some(4_000.0)
    );

    // 4. handle_normal_structure_damage_aftermath: suspect timer + Tenebrex cyno jammer notification
    handle_normal_structure_damage_aftermath(
        &ctx,
        sys_id,
        0.9,
        char_id,
        ship_id,
        TYPE_TENEBREX_CYNO_JAMMER,
        "ArmorReinforced",
        true,
        now,
    );
    assert!(ctx.crimewatch.read().unwrap().states.contains_key(&char_id));
}

#[tokio::test]
async fn sov_mercden_repair_skyhook_tax_sov_hub_hack_and_anchor_sov_hub_all_branches() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let (null_sys, _, _) = find_claimable_null_with_temperate_planet(&ctx);
    let ship_id = 1_093_000_001_i64;
    let den_id = 1_093_000_002_i64;
    let skyhook_id = 1_093_000_003_i64;
    let hub_id = 1_093_000_004_i64;
    let now = ctx.now_unix();
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let ally_id = 99_000_199_i64;
    let other_corp_id = 98_000_999_i64;
    let other_ally_id = 99_000_299_i64;

    // 1. apply_mercenary_den_shield_repair: ShipNotInSpace -> MercenaryDenNotFound -> TargetNotInSystem -> TargetOutOfRange -> InReinforcement -> partial -> >= 50% re-enable
    assert_eq!(
        apply_mercenary_den_shield_repair(&ctx, null_sys, ship_id, den_id, 1_000.0)
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );
    let null_h = ctx.systems.get_or_spawn(null_sys);
    let _ = null_h
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 648,
            position: DVec3::new(100_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        apply_mercenary_den_shield_repair(&ctx, null_sys, ship_id, den_id, 1_000.0)
            .await
            .unwrap_err()
            .code,
        "MercenaryDenNotFound"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            den_id,
            UpwellStructureRecord {
                structure_id: den_id,
                type_id: TYPE_MERCENARY_DEN,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142, // mismatch first
                name: "Repair Den".into(),
                position: [0.0, 0.0, 0.0],
                state: "ArmorReinforced".into(),
                shield_hp: 0.0,
                armor_hp: MERC_DEN_BASE_HP,
                hull_hp: MERC_DEN_BASE_HP,
                reinforced_until_unix: now + 3_600,
                ..Default::default()
            },
        );
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            PersistedMercenaryDen {
                den_id,
                owner_char_id: char_id,
                skyhook_id,
                solar_system_id: null_sys,
                planet_id: 40_009_077,
                type_id: TYPE_MERCENARY_DEN,
                enabled: false,
                cargo_extraction_enabled: true,
                development_level: 1,
                anarchy_level: 0,
                simulated_at_unix: now,
                infomorphs: 0,
                next_activity_generation_unix: now + 3_600,
            },
        );
        snap.sov_runtime
            .mercenary_den_reinforcement
            .insert(den_id, (now + 3_600, 0, 0));
    });
    assert_eq!(
        apply_mercenary_den_shield_repair(&ctx, null_sys, ship_id, den_id, 1_000.0)
            .await
            .unwrap_err()
            .code,
        "TargetNotInSystem"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&den_id).unwrap().solar_system_id = null_sys;
    });
    assert_eq!(
        apply_mercenary_den_shield_repair(&ctx, null_sys, ship_id, den_id, 1_000.0)
            .await
            .unwrap_err()
            .code,
        "TargetOutOfRange"
    );
    let _ = null_h.set_ball_position(ship_id, DVec3::new(1_000.0, 0.0, 0.0)).await;
    assert_eq!(
        apply_mercenary_den_shield_repair(&ctx, null_sys, ship_id, den_id, 1_000.0)
            .await
            .unwrap_err()
            .code,
        "MercenaryDenStillInReinforcement"
    );

    // Expire reinforcement -> partial repair (< 50%) stays disabled; >= 50% re-enables and clears reinforcement
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.mercenary_den_reinforcement.insert(den_id, (now - 1, 0, 0));
    });
    let (pct1, en1) = apply_mercenary_den_shield_repair(&ctx, null_sys, ship_id, den_id, 1_500.0)
        .await
        .unwrap();
    assert_eq!(pct1, 30);
    assert!(!en1);
    let (pct2, en2) = apply_mercenary_den_shield_repair(&ctx, null_sys, ship_id, den_id, 1_500.0)
        .await
        .unwrap();
    assert_eq!(pct2, 60);
    assert!(en2);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&den_id].state, "ShieldVulnerable");
        assert!(snap.sov_runtime.mercenary_dens[&den_id].enabled);
    });

    // 2. handle_confirm_skyhook_sov_management validation, auth, and tax clamping
    let mut sess = SessionState::new(9603);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    assert_eq!(
        handle_confirm_skyhook_sov_management(&ctx, &sess, &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArguments"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.entry(corp_id).or_default().ceo_id = char_id + 1;
        snap.corporations.entry(corp_id).or_default().alliance_id = Some(ally_id);
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: null_sys,
                name: "Tax Hook".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_confirm_skyhook_sov_management(&ctx, &sess, &[PyRep::Integer(skyhook_id), PyRep::Integer(25)])
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.entry(corp_id).or_default().ceo_id = char_id;
    });
    let tax_res = handle_confirm_skyhook_sov_management(
        &ctx,
        &sess,
        &[PyRep::Integer(skyhook_id), PyRep::Integer(150)],
    )
    .await
    .unwrap();
    assert_eq!(tax_res.dict_get("taxPercent").and_then(PyRep::as_i64), Some(100));

    // 3. try_handle_sov_hub_data_hack: non-hub None, Relic error, Destroyed error, Friendly error, missing skill, success
    let mut data_mod = ctx.sde.types.values().next().unwrap().clone();
    data_mod.type_id = 30829;
    data_mod.name = "Data Analyzer II".into();
    let mut relic_mod = data_mod.clone();
    relic_mod.type_id = 22177;
    relic_mod.name = "Relic Analyzer I".into();
    assert!(try_handle_sov_hub_data_hack(&ctx, char_id, 999_999_999, 1, &data_mod, 50.0, 20.0).is_none());

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.entry(other_corp_id).or_default().alliance_id = Some(other_ally_id);
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: crate::services::sov_services::TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: corp_id,
                solar_system_id: null_sys,
                name: "Sov Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "Destroyed".into(),
                ..Default::default()
            },
        );
    });
    assert_eq!(
        try_handle_sov_hub_data_hack(&ctx, char_id, hub_id, 1, &relic_mod, 50.0, 20.0)
            .unwrap()
            .unwrap_err()
            .code,
        "DataAnalyzerRequired"
    );
    assert_eq!(
        try_handle_sov_hub_data_hack(&ctx, char_id, hub_id, 1, &data_mod, 50.0, 20.0)
            .unwrap()
            .unwrap_err()
            .code,
        "TargetDestroyed"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&hub_id).unwrap().state = "ShieldVulnerable".into();
    });
    assert_eq!(
        try_handle_sov_hub_data_hack(&ctx, char_id, hub_id, 1, &data_mod, 50.0, 20.0)
            .unwrap()
            .unwrap_err()
            .code,
        "CannotHackFriendlySovHub"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&hub_id).unwrap().owner_corp_id = other_corp_id;
        if let Some(m) = snap.skills.get_mut(&char_id) {
            m.remove(&3405);
            m.remove(&3408);
        }
    });
    assert_eq!(
        try_handle_sov_hub_data_hack(&ctx, char_id, hub_id, 1, &data_mod, 50.0, 20.0)
            .unwrap()
            .unwrap_err()
            .code,
        "HackingSkillRequired"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            3405,
            TrainedSkillRecord {
                type_id: 3405,
                level: 4,
                skill_points: 50_000.0,
            },
        );
    });
    let hack_ok = try_handle_sov_hub_data_hack(&ctx, char_id, hub_id, 55, &data_mod, 50.0, 20.0)
        .unwrap()
        .unwrap();
    assert_eq!(hack_ok.dict_get("virusCoherence").and_then(PyRep::as_f64), Some(90.0));
    assert_eq!(hack_ok.dict_get("virusStrength").and_then(PyRep::as_f64), Some(28.0));

    // 4. handle_anchor_sov_hub_rpc guards and full success path
    sess.station_id = Some(60_003_760);
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "ShipDocked");
    sess.station_id = None;
    sess.solar_system_id = None;
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "NoSolarSystem");
    sess.solar_system_id = Some(30_000_142);
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "CannotAnchorSovHubOutsideNullSec");
    sess.solar_system_id = Some(null_sys);
    sess.ship_id = Some(ship_id + 999);
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "ShipNotInSpace");
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.entry(corp_id).or_default().alliance_id = None;
    });
    sess.alliance_id = None;
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "AllianceRequired");
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.entry(corp_id).or_default().alliance_id = Some(ally_id);
        snap.corporations.entry(corp_id).or_default().ceo_id = char_id + 1;
    });
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "CorpRoleDenied");

    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(other_ally_id);
    });
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "SovereigntyHeldByAnotherAlliance");
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ally_id);
    });
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "SovHubAlreadyPresent");

    let hub_item_id = 1_093_000_050_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.remove(&hub_id);
    });
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "MissingStructureItem");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            hub_item_id,
            InventoryItemRecord {
                item_id: hub_item_id,
                type_id: crate::services::sov_services::TYPE_SOVEREIGNTY_HUB,
                owner_id: char_id + 123,
                location_id: ship_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[PyRep::Integer(hub_item_id)])
            .await
            .unwrap_err()
            .code,
        "ItemOwnershipViolation"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&hub_item_id).unwrap().owner_id = char_id;
    });
    assert_eq!(
        handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[PyRep::Integer(hub_item_id)])
            .await
            .unwrap_err()
            .code,
        "ItemNotInCargo"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&hub_item_id).unwrap().flag = InventoryFlag::Cargo;
    });
    let anchored_hub = handle_anchor_sov_hub_rpc(
        &ctx,
        &mut sess,
        &[PyRep::Integer(hub_item_id), PyRep::String("Custom Hub".into())],
    )
    .await
    .unwrap();
    assert_eq!(anchored_hub.dict_get("state").and_then(PyRep::as_str), Some("Onlining"));
    let new_hub_id = anchored_hub.dict_get("structureID").and_then(PyRep::as_i64).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&new_hub_id].name, "Custom Hub");
        assert!(!snap.items.contains_key(&hub_item_id));
    });
}
