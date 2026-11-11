use super::combat_fighters::combat_fighters_tests::{
    make_fighter_item, make_fighter_sde_registry, make_fighter_type,
};
use super::*;
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_sde::{FighterAbilityCharges, FighterAbilityInfo, FighterAbilitySlot};
use glam::DVec3;
use std::sync::Arc;

#[cfg(test)]
#[path = "combat_fighters_antifighter_ewar_tests.rs"]
mod combat_fighters_antifighter_ewar_tests;

#[cfg(test)]
#[path = "combat_fighters_bomb_tests.rs"]
mod combat_fighters_bomb_tests;
#[cfg(test)]
#[path = "combat_fighters_ewar_tests.rs"]
mod combat_fighters_ewar_tests;
#[cfg(test)]
#[path = "combat_fighters_abilities_edges_tests.rs"]
mod combat_fighters_abilities_edges_tests;

pub(super) fn py_key<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| matches!(k, PyRep::String(s) if s == key))
            .map(|(_, v)| v),
        PyRep::KeyVal(entries) => entries.get(key),
        _ => None,
    }
}

pub(super) fn make_ability_test_ctx() -> (ServerContext, SessionState, i64, i64, u32) {
    let mut sde = make_fighter_sde_registry();
    let ability_defs: &[(u32, &str, bool, bool)] = &[
        (2, "itemTargeted", false, false),
        (7, "itemTargeted", false, false),
        (10, "itemTargeted", false, false),
        (11, "itemTargeted", false, false),
        (12, "itemTargeted", false, false),
        (13, "untargeted", false, false),
        (14, "itemTargeted", false, false),
        (16, "itemTargeted", false, false),
        (17, "itemTargeted", false, false),
        (20, "itemTargeted", false, false),
        (34, "itemTargeted", false, false),
        (38, "itemTargeted", false, false),
        (44, "itemTargeted", false, false),
        (50, "pointTargeted", true, false),
        (98, "untargeted", false, true),
        (99, "untargeted", true, false),
    ];
    for &(aid, mode, no_hi, no_lo) in ability_defs {
        sde.fighter_abilities.insert(
            aid,
            FighterAbilityInfo {
                ability_id: aid,
                target_mode: mode.into(),
                disallow_in_high_sec: no_hi,
                disallow_in_low_sec: no_lo,
                icon_id: Some(aid),
                display_name_id: Some(aid + 1000),
                ability_name_id: Some(aid + 2000),
                tooltip_text_id: Some(aid + 3000),
                turret_graphic_id: Some(aid + 4000),
                raw_json: serde_json::Value::Null,
            },
        );
    }
    // Bomb charge item 41_549
    sde.types.insert(
        41_549,
        make_fighter_type(
            41_549,
            863,
            8,
            100.0,
            vec![(114, 150.0), (116, 100.0), (117, 50.0), (118, 25.0), (99, 12_000.0)],
        ),
    );
    // Support fighter (Siren/Dromi) type 23_059 with EWAR slots
    sde.types.insert(
        23_059,
        make_fighter_type(
            23_059,
            1537,
            87,
            1_000.0,
            vec![
                (2215, 3.0),
                (2213, 1.0),
                (2184, -30.0),
                (2205, 2.0),
                (2211, 80.0),
                (2246, 4.0),
                (2224, 250.0),
                (2225, -75.0),
                (2130, 2.0),
                (2131, 40.0),
                (2132, 30.0),
                (2133, 20.0),
                (2134, 10.0),
            ],
        ),
    );
    sde.fighter_abilities_by_type.insert(
        23_059,
        vec![
            FighterAbilitySlot { slot_index: 0, ability_id: 2, cooldown_seconds: Some(0.0), charges: None },
            FighterAbilitySlot { slot_index: 1, ability_id: 13, cooldown_seconds: Some(15.0), charges: None },
            FighterAbilitySlot { slot_index: 2, ability_id: 16, cooldown_seconds: Some(12.0), charges: None },
        ],
    );
    // Bomber fighter type 23_063 for point-targeted bomb ability 50
    sde.types.insert(
        23_063,
        make_fighter_type(23_063, 1653, 87, 1_000.0, vec![(2215, 6.0), (2214, 1.0), (2324, 41_549.0)]),
    );
    sde.fighter_abilities_by_type.insert(
        23_063,
        vec![
            FighterAbilitySlot { slot_index: 0, ability_id: 34, cooldown_seconds: Some(0.0), charges: None },
            FighterAbilitySlot { slot_index: 1, ability_id: 4, cooldown_seconds: Some(10.0), charges: None },
            FighterAbilitySlot {
                slot_index: 2,
                ability_id: 50,
                cooldown_seconds: Some(20.0),
                charges: Some(FighterAbilityCharges { charge_count: 2, rearm_time_seconds: 30.0 }),
            },
        ],
    );
    // Lowsec test system 30_000_200
    sde.solar_systems.insert(
        30_000_200,
        eve_sde::SolarSystem {
            solar_system_id: 30_000_200,
            region_id: 10_000_002,
            constellation_id: 20_000_020,
            name: "LowsecAbilityTest".into(),
            security_status: 0.25,
            radius: 1.0e12,
            galactic_pos_ly: [0.0, 0.0, 0.0],
            planet_ids: Vec::new(),
            stations: Vec::new(),
            wormhole_class_id: None,
        },
    );

    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db.clone());
    let char_id = 90_000_001i64;
    let ship_id = 200_000_001i64;
    let sys_id = 30_000_142u32;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_fighter_item(ship_id, 23_915, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    let mut session = SessionState::default();
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);
    session.user_id = Some(1);
    (ctx, session, char_id, ship_id, sys_id)
}

pub(super) fn squad_record(
    ball_id: i64,
    type_id: u32,
    owner_char_id: i64,
    parent_ship_id: i64,
    _sys_id: u32,
    size: i32,
) -> crate::router::context::ActiveFighterSquadron {
    crate::router::context::ActiveFighterSquadron {
        ball_id,
        item_id: ball_id,
        type_id,
        owner_char_id,
        parent_ship_id,
        tube_index: 0,
        squadron_size: size.max(1) as u32,
        target_id: None,
        is_abandoned: false,
        mwd_active: false,
    }
}

pub(super) async fn spawn_fighter_and_ship(
    ctx: &ServerContext,
    sys_id: u32,
    parent_ship_id: i64,
    fighter_id: i64,
    fighter_type: u32,
    owner_char_id: i64,
) {
    ctx.active_fighters.write().unwrap().insert(
        fighter_id,
        squad_record(fighter_id, fighter_type, owner_char_id, parent_ship_id, sys_id, 3),
    );
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: parent_ship_id,
                type_id: 23_915,
                position: DVec3::new(0.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 120.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 500.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await;
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: fighter_id,
                type_id: fighter_type,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 1_000.0,
                inertia: 1.0,
                mass: 5_000.0,
                radius: 25.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 500.0,
                max_shield_hp: 500.0,
                armor_hp: 200.0,
                max_armor_hp: 200.0,
                structure_hp: 100.0,
                max_structure_hp: 100.0,
                capacitor: 500.0,
                max_capacitor: 500.0,
                signature_radius: 50.0,
                ..CombatState::default()
            },
        )
        .await;
}
