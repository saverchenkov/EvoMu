//! Additional unit and mutation-killing tests for `combat_missiles_aoe`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::combat_crimewatch::*;
use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;
use std::sync::Arc;

#[cfg(test)]
#[path = "combat_smartbomb_and_defender_unit_tests.rs"]
mod combat_smartbomb_and_defender_unit_tests;

pub(super) fn py_val<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| matches!(k, PyRep::String(s) if s == key))
            .map(|(_, v)| v),
        PyRep::KeyVal(entries) => entries.get(key),
        _ => None,
    }
}

pub(super) async fn spawn_ball_with_hp(
    ctx: &ServerContext,
    sys_id: u32,
    id: i64,
    type_id: u32,
    pos: DVec3,
    shield: f64,
    armor: f64,
    hull: f64,
    cap: f64,
) {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: shield,
                max_shield_hp: shield.max(100.0),
                armor_hp: armor,
                max_armor_hp: armor.max(100.0),
                structure_hp: hull,
                max_structure_hp: hull.max(100.0),
                capacitor: cap,
                max_capacitor: cap.max(100.0),
                signature_radius: 120.0,
                base_signature_radius: Some(120.0),
                ..CombatState::default()
            },
        )
        .await
        .unwrap();
}

pub(super) fn setup_aoe_extra_ctx() -> (ServerContext, SessionState, i64, i64, u32) {
    let (mut ctx, session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = (*ctx.sde).clone();

    sde.types.insert(
        587,
        make_fighter_type(587, 25, 6, 10_000.0, vec![(263, 500.0), (265, 500.0), (9, 500.0)]),
    );
    sde.types.insert(
        670,
        make_fighter_type(670, 29, 6, 500.0, vec![(9, 400.0)]),
    );
    sde.types.insert(
        26_470,
        make_fighter_type(26_470, 186, 2, 100.0, vec![]),
    );
    // Category 11 NPC (950_001)
    sde.types.insert(
        950_001,
        make_fighter_type(950_001, 550, 11, 5_000.0, vec![(481, 10_000.0)]),
    );
    // Category 65 Upwell Structure (35832 Astrahus)
    sde.types.insert(
        35_832,
        make_fighter_type(35_832, 1657, 65, 100_000.0, vec![]),
    );
    // Light Missile Launcher 2410 (group 509) & Scourge Light Missile 209 (group 384)
    sde.types.insert(
        2_410,
        make_fighter_type(2_410, 509, 7, 5.0, vec![(51, 3_000.0)]),
    );
    sde.types.insert(
        209,
        make_fighter_type(
            209,
            384,
            8,
            0.015,
            vec![
                (37, 2_000.0),
                (281, 6_000.0),
                (9, 75.0),
                (653, 170.0),
                (654, 40.0),
                (1353, 0.604),
            ],
        ),
    );
    // Smartbomb 12219 (group 72: range 5000, 62.5 each = 250 total dmg, cap 30)
    sde.types.insert(
        12_219,
        make_fighter_type(
            12_219,
            72,
            7,
            5.0,
            vec![
                (99, 5_000.0),
                (114, 62.5),
                (116, 62.5),
                (117, 62.5),
                (118, 62.5),
                (6, 30.0),
            ],
        ),
    );
    // Bomb Launcher 27912 (group 862) & Damage Bomb 27916 (group 90)
    sde.types.insert(
        27_912,
        make_fighter_type(27_912, 862, 7, 10.0, vec![(281, 3_000.0), (37, 2_000.0)]),
    );
    sde.types.insert(
        27_916,
        make_fighter_type(
            27_916,
            90,
            8,
            75.0,
            vec![(281, 3_000.0), (37, 2_000.0), (99, 15_000.0), (654, 400.0), (118, 6_400.0)],
        ),
    );
    // Defender Launcher 1599 (group 512) & Defender Missile 267 (group 88)
    sde.types.insert(1_599, make_fighter_type(1_599, 512, 7, 5.0, vec![]));
    sde.types.insert(267, make_fighter_type(267, 88, 8, 0.015, vec![]));
    // Warrior I drone 2488 (cat 18, group 100)
    sde.types.insert(2_488, make_fighter_type(2_488, 100, 18, 5.0, vec![]));

    ctx.sde = Arc::new(sde);
    (ctx, session, char_id, ship_id, sys_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn combatant_predicates_structure_noop_capsuleer_and_cat11_npc_destruction() {
    let (ctx, _, char_id, _ship_id, sys_id) = setup_aoe_extra_ctx();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Boundary checks on NPC ball ID range (1_895_000_000..1_900_000_000) and categories
    assert!(!is_combatant_ship_or_entity_ball(&ctx, 1_894_999_999, 26_470));
    assert!(is_combatant_ship_or_entity_ball(&ctx, 1_895_000_000, 26_470));
    assert!(is_combatant_ship_or_entity_ball(&ctx, 1_899_999_999, 26_470));
    assert!(!is_combatant_ship_or_entity_ball(&ctx, 1_900_000_000, 26_470));
    assert!(is_combatant_ship_or_entity_ball(&ctx, 500_001, 950_001)); // cat 11
    assert!(is_combatant_ship_or_entity_ball(&ctx, 1_040_000_000_001, 35_832)); // structure cat 65

    // 1. Structure ball early-return in handle_destroyed_ball_from_missile_or_aoe
    let struct_id = 1_040_000_000_111_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                solar_system_id: sys_id,
                ..Default::default()
            },
        );
    });
    spawn_ball_with_hp(&ctx, sys_id, struct_id, 35_832, DVec3::ZERO, 1000.0, 1000.0, 1000.0, 0.0).await;
    handle_destroyed_ball_from_missile_or_aoe(
        &ctx,
        sys_id,
        char_id,
        struct_id,
        35_832,
        DVec3::ZERO,
        ctx.now_unix(),
    )
    .await;
    assert!(
        sys_handle.get_ball_snapshot(struct_id).await.unwrap().is_some(),
        "structure ball must not be removed by non-structure destruction handler"
    );

    // 2. Category 11 NPC destruction (outside 1_895M range, matched via cat_id == 11)
    let npc_ball = 550_000_001_i64;
    spawn_ball_with_hp(&ctx, sys_id, npc_ball, 950_001, DVec3::ZERO, 0.0, 0.0, 0.0, 0.0).await;
    handle_destroyed_ball_from_missile_or_aoe(
        &ctx,
        sys_id,
        char_id,
        npc_ball,
        950_001,
        DVec3::ZERO,
        ctx.now_unix(),
    )
    .await;
    assert!(sys_handle.get_ball_snapshot(npc_ball).await.unwrap().is_none());

    // 3. Capsuleer active ship destruction triggers full ship death pipeline & killmail
    let victim_char = 90_000_002_i64;
    let victim_ship = 220_000_111_i64;
    spawn_ball_with_hp(&ctx, sys_id, victim_ship, 587, DVec3::new(500.0, 0.0, 0.0), 0.0, 0.0, 0.0, 0.0).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            victim_ship,
            make_fighter_item(victim_ship, 587, victim_char, sys_id as i64, InventoryFlag::Hangar, 1),
        );
        if let Some(vc) = snap.characters.get_mut(&victim_char) {
            vc.solar_system_id = sys_id;
            vc.station_id = None;
            vc.active_ship_id = victim_ship;
        }
    });
    handle_destroyed_ball_from_missile_or_aoe(
        &ctx,
        sys_id,
        char_id,
        victim_ship,
        587,
        DVec3::new(500.0, 0.0, 0.0),
        ctx.now_unix(),
    )
    .await;
    ctx.db.with_snapshot(|snap| {
        assert_ne!(snap.characters[&victim_char].active_ship_id, victim_ship);
        assert!(snap.killmails.values().any(|km| km.victim_character_id == victim_char));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activate_missile_and_breacher_launcher_fof_defer_and_dot_application() {
    let (mut ctx, _, char_id, ship_id, sys_id) = setup_aoe_extra_ctx();
    let target_char = 90_000_002_i64;
    let target_ship = 220_000_333_i64;
    let launcher_id = 720_001_i64;
    let now_unix = ctx.now_unix();

    let mut sde = (*ctx.sde).clone();
    sde.types.insert(
        950_209,
        make_fighter_type(
            950_209,
            384,
            8,
            0.015,
            vec![
                (37, 2_000.0),
                (281, 6_000.0),
                (9, 75.0),
                (653, 170.0),
                (654, 40.0),
                (1353, 0.604),
                (5735, 30_000.0),
                (5736, 150.0),
                (5737, 1.0),
            ],
        ),
    );
    ctx.sde = Arc::new(sde);

    spawn_ball_with_hp(&ctx, sys_id, ship_id, 587, DVec3::ZERO, 500.0, 500.0, 500.0, 500.0).await;
    spawn_ball_with_hp(&ctx, sys_id, target_ship, 587, DVec3::new(500.0, 0.0, 0.0), 5_000.0, 5_000.0, 5_000.0, 500.0).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(tc) = snap.characters.get_mut(&target_char) {
            tc.solar_system_id = sys_id;
            tc.station_id = None;
            tc.active_ship_id = target_ship;
        }
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let (_, attacker_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    let fitted_mod = eve_dogma::FittedModule {
        item_id: launcher_id,
        type_id: 2_410,
        flag_id: 27,
        state: eve_dogma::ModuleState::Active,
        damage_hp: 0.0,
        max_hp: 40.0,
        charge_type_id: Some(209),
        charge_quantity: 10,
        dynamic_attribute_overrides: std::collections::HashMap::new(),
    };
    let charge_std = ctx.sde.types[&209].clone();
    let charge_breacher = ctx.sde.types[&950_209].clone();

    // 1. FoF missile with explicit target defers impact (`inFlight = true`)
    let fof_rep = activate_missile_or_breacher_launcher(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        now_unix,
        launcher_id,
        &fitted_mod,
        &fit,
        209,
        &charge_std,
        &attacker_ball,
        target_ship,
        eve_destiny::DamageVector::new(0.0, 0.0, 100.0, 0.0),
        true,
        false,
        true,
    )
    .await
    .unwrap();
    assert_eq!(py_val(&fof_rep, "inFlight"), Some(&PyRep::Bool(true)));
    let fof_ball = py_val(&fof_rep, "missileBallID").and_then(PyRep::as_i64).unwrap();
    assert!(with_combat_runtime(&ctx, |rt| rt.in_flight_missiles.contains_key(&fof_ball)));

    // 2. Non-deferred Breacher Pod impacts immediately and activates DoT
    let br_rep = activate_missile_or_breacher_launcher(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        now_unix,
        launcher_id,
        &fitted_mod,
        &fit,
        950_209,
        &charge_breacher,
        &attacker_ball,
        target_ship,
        eve_destiny::DamageVector::new(0.0, 0.0, 50.0, 0.0),
        false,
        true,
        true,
    )
    .await
    .unwrap();
    assert_eq!(py_val(&br_rep, "impacted"), Some(&PyRep::Bool(true)));
    assert_eq!(py_val(&br_rep, "isBreacherPod"), Some(&PyRep::Bool(true)));
    assert!(py_val(&br_rep, "breacherTickDPS").and_then(PyRep::as_f64).unwrap_or(0.0) > 0.0);
    assert!(with_combat_runtime(&ctx, |rt| rt.active_breacher_dots.contains_key(&target_ship)));
}
