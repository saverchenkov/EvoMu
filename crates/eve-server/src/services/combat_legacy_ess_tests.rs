//! Unit and mutation-killing tests for `combat_legacy_ess`.

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_sde_registry, make_fighter_type,
};
use eve_db::{ActiveMissionRecord, InventoryFlag};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

#[test]
fn step_dynamic_bounty_multiplier_decay_boost_equilibrium_and_clamp() {
    let (ctx, _session, _char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();

    // Initial state starts at 1.35; 50M ratting decays by 0.10 -> 1.25
    let m1 = step_dynamic_bounty_multiplier(&ctx, sys_id, 50_000_000.0, 0.0);
    assert!((m1 - 1.25).abs() < 1e-6);

    // 25M PvP loss boosts by 0.15 -> 1.40
    let m2 = step_dynamic_bounty_multiplier(&ctx, sys_id, 0.0, 25_000_000.0);
    assert!((m2 - 1.40).abs() < 1e-6);

    // Zero ratting and zero PvP drifts 5% toward 1.35: 1.40 + (1.35 - 1.40)*0.05 = 1.3975
    let m3 = step_dynamic_bounty_multiplier(&ctx, sys_id, 0.0, 0.0);
    assert!((m3 - 1.3975).abs() < 1e-6);

    // Massive ratting clamps to 0.50 floor; massive PvP clamps to 2.00 ceiling
    let floor = step_dynamic_bounty_multiplier(&ctx, sys_id, 5_000_000_000.0, 0.0);
    assert!((floor - 0.50).abs() < 1e-6);
    let ceil = step_dynamic_bounty_multiplier(&ctx, sys_id, 0.0, 5_000_000_000.0);
    assert!((ceil - 2.00).abs() < 1e-6);
}

#[tokio::test]
async fn legacy_activate_validation_npc_kill_and_concord_highsec_enforcement() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kwargs = HashMap::new();

    // Deleted legacy RPCs return UnimplementedRPC
    // rpc-lint: allow testing Phase 4 deleted FireWeapon rejection
    let fw_err = handle_legacy_and_ess(&ctx, &mut session, "dogmaIM", "FireWeapon", &[], &kwargs)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(fw_err.code, "UnimplementedRPC");

    // Configure weapon (group 55 Projectile Turret, type 2905) + compatible ammo (group 83, type 192, size 1) + NPC (type 10001, cat 11)
    let mut sde = (*ctx.sde).clone();
    if let Some(ship_t) = sde.types.get_mut(&23_915) {
        ship_t.default_attributes.insert(14, 8.0);
        ship_t.default_attributes.insert(102, 8.0);
        ship_t.default_attributes.insert(11, 10_000.0);
        ship_t.default_attributes.insert(48, 10_000.0);
    }
    let mut wep_t = make_fighter_type(
        2_905,
        55,
        7,
        5.0,
        vec![
            (51, 1_000.0),   // speed 1000ms (1 tick)
            (54, 15_000.0),  // optimal range 15km
            (64, 2.0),       // damage multiplier 2x
            (128, 1.0),      // charge size 1
            (158, 10_000.0), // falloff 10km
            (160, 100.0),    // tracking speed
            (604, 83.0),     // usedWithChargeGroup 83
        ],
    );
    wep_t.effect_ids = vec![12];
    sde.types.insert(2_905, wep_t);
    sde.types.insert(
        192,
        make_fighter_type(
            192,
            83,
            8,
            0.01,
            vec![
                (128, 1.0),    // charge size 1
                (114, 500.0),  // EM damage
                (116, 500.0),  // Explosive damage
                (117, 500.0),  // Kinetic damage
                (118, 500.0),  // Thermal damage
            ],
        ),
    );
    sde.types.insert(
        10_001,
        make_fighter_type(
            10_001,
            550,
            11,
            1_000.0,
            vec![(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 100_000.0)],
        ),
    );
    ctx.sde = std::sync::Arc::new(sde);

    // 1. Docked rejection
    session.station_id = Some(60_003_760);
    let docked_err = handle_legacy_and_ess(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(900_001)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(docked_err.code, "ShipDocked");
    session.station_id = None;

    // 2. Self/invalid target rejection
    let self_err = handle_legacy_and_ess(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(ship_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(self_err.code, "InvalidTarget");

    // 3. Ship not in space rejection
    let not_in_space_err = handle_legacy_and_ess(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(900_001)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(not_in_space_err.code, "ShipNotInSpace");

    // Spawn attacker ship and target NPC in space
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let npc_ball_id = 900_001_i64;
    for (id, tid, pos, hp) in [
        (ship_id, 23_915_u32, DVec3::ZERO, 1_000.0),
        (npc_ball_id, 10_001_u32, DVec3::new(2_000.0, 0.0, 0.0), 100.0),
    ] {
        sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id,
                    type_id: tid,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 200.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: 0.0,
                    max_shield_hp: hp,
                    armor_hp: 0.0,
                    max_armor_hp: hp,
                    structure_hp: hp,
                    max_structure_hp: hp,
                    signature_radius: 50.0,
                    ..CombatState::default()
                },
            )
            .await
            .unwrap();
    }

    // 4. No weapon fitted rejection
    let no_wep_err = handle_legacy_and_ess(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(npc_ball_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(no_wep_err.code, "NoWeaponFitted");

    // Fit weapon in HiSlot0 (flag 27), verify NoAmmoLoaded before loading ammo
    let wep_id = 800_001_i64;
    let ammo_id = 800_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wep_id,
            make_fighter_item(wep_id, 2_905, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
    });
    ctx.invalidate_ship_fitting(ship_id);
    let no_ammo_err = handle_legacy_and_ess(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(npc_ball_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(no_ammo_err.code, "NoAmmoLoaded");

    // Load ammo and seed an active mission referencing `npc_ball_id`
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ammo_id,
            make_fighter_item(ammo_id, 192, char_id, ship_id, InventoryFlag::HiSlot0, 5),
        );
        snap.active_missions.insert(
            (char_id, 3_018_418),
            ActiveMissionRecord {
                character_id: char_id,
                agent_id: 3_018_418,
                mission_id: 1,
                state: "Accepted".into(),
                solar_system_id: sys_id,
                current_room_id: 0,
                npcs_remaining: 1,
                spawned_npc_ball_ids: vec![npc_ball_id],
                bookmark_id: None,
            },
        );
    });

    // 5. Destroy NPC via legacy Activate -> verify bounty payout, mission decrement, wreck & killmail
    let hit_res = handle_legacy_and_ess(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(npc_ball_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        hit_res.dict_get("destroyed"),
        Some(&PyRep::Bool(true))
    );
    assert_eq!(
        hit_res.dict_get("bountyPayoutISK"),
        Some(&PyRep::Real(90_000.0))
    );
    ctx.db.with_snapshot(|snap| {
        let m = snap.active_missions.get(&(char_id, 3_018_418)).unwrap();
        assert_eq!(m.npcs_remaining, 0);
        assert_eq!(snap.items.get(&ammo_id).unwrap().quantity, 4);
    });

    // 6. Illegal high-sec PvP attack triggers CONCORD response and kill right creation
    ctx.weapon_cycle_state.write().unwrap().clear();
    let victim_char_id = 90_000_002_i64;
    let victim_ship_id = 200_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut vic = snap.characters.get(&char_id).cloned().unwrap();
        vic.character_id = victim_char_id;
        vic.corporation_id = 2_000_002;
        vic.active_ship_id = victim_ship_id;
        vic.solar_system_id = sys_id;
        vic.security_status = 1.0;
        snap.characters.insert(victim_char_id, vic);
    });
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: victim_ship_id,
                type_id: 23_915,
                position: DVec3::new(1_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 1_000.0,
                max_shield_hp: 1_000.0,
                armor_hp: 1_000.0,
                max_armor_hp: 1_000.0,
                structure_hp: 1_000.0,
                max_structure_hp: 1_000.0,
                signature_radius: 50.0,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();

    let concord_res = handle_legacy_and_ess(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(victim_char_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        concord_res.dict_get("criminalCharacterID"),
        Some(&PyRep::Integer(char_id))
    );
    assert_eq!(
        concord_res.dict_get("victimCharacterID"),
        Some(&PyRep::Integer(victim_char_id))
    );
    assert_eq!(
        concord_res.dict_get("criminalTimerSeconds"),
        Some(&PyRep::Integer(900))
    );
}

#[tokio::test]
async fn legacy_activate_rejects_concord_props_zero_res_and_chains_vorton_across_zero_bounty_npcs() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kwargs = HashMap::new();

    // Configure ship, Vorton Projector (4060, 25km optimal, 5 secondary arcs), and Vorton charge (4061)
    let mut sde = (*ctx.sde).clone();
    if let Some(ship_t) = sde.types.get_mut(&23_915) {
        ship_t.default_attributes.insert(14, 8.0);
        ship_t.default_attributes.insert(102, 8.0);
        ship_t.default_attributes.insert(11, 50_000.0);
        ship_t.default_attributes.insert(48, 50_000.0);
    }
    let mut vorton_t = make_fighter_type(
        54_801,
        4060,
        7,
        10.0,
        vec![
            (51, 1_000.0),  // 1s cycle
            (54, 25_000.0), // 25km optimal
            (128, 1.0),     // charge size 1
            (604, 4061.0),  // Vorton ammo group
            (2104, 5.0),    // max 5 secondary arcs
            (1353, 0.40),   // DRF
        ],
    );
    vorton_t.effect_ids = vec![12];
    sde.types.insert(54_801, vorton_t);
    sde.types.insert(
        54_802,
        make_fighter_type(
            54_802,
            4061,
            8,
            0.1,
            vec![
                (128, 1.0),
                (114, 1_000.0),
                (653, 200.0),
                (654, 40.0),
            ],
        ),
    );

    // Invulnerable targets: CONCORD (300, 301, 182), non-combat props (306, 494, 922, 4073, 4576, 4771),
    // and explicit zero-resonance SDE type (group 1759 with all 12 resonance attrs = 0.0)
    let invuln_types = [
        (90_300_u32, 300_u32, "CONCORD Fixture Police"),
        (90_301, 301, "CONCORD Police Officer"),
        (90_182, 182, "CONCORD SWAT Officer"),
        (90_306, 306, "Spawn Container"),
        (90_494, 494, "Deadspace Overseer's Structure"),
        (90_922, 922, "Capture Point"),
        (94_073, 4073, "Temporary Collidable Structure"),
        (94_576, 4576, "Homefront Operations Structure"),
        (94_771, 4771, "Homefront Operations Noncombatant"),
    ];
    for (tid, gid, name) in invuln_types {
        let mut t = make_fighter_type(tid, gid, 11, 50.0, vec![]);
        t.name = name.into();
        sde.types.insert(tid, t);
    }
    let zero_res_attrs = vec![
        (eve_sde::attr_ids::SHIELD_EM_DAMAGE_RESONANCE, 0.0),
        (eve_sde::attr_ids::SHIELD_THERMAL_DAMAGE_RESONANCE, 0.0),
        (eve_sde::attr_ids::SHIELD_KINETIC_DAMAGE_RESONANCE, 0.0),
        (eve_sde::attr_ids::SHIELD_EXPLOSIVE_DAMAGE_RESONANCE, 0.0),
        (eve_sde::attr_ids::ARMOR_EM_DAMAGE_RESONANCE, 0.0),
        (eve_sde::attr_ids::ARMOR_THERMAL_DAMAGE_RESONANCE, 0.0),
        (eve_sde::attr_ids::ARMOR_KINETIC_DAMAGE_RESONANCE, 0.0),
        (eve_sde::attr_ids::ARMOR_EXPLOSIVE_DAMAGE_RESONANCE, 0.0),
        (eve_sde::attr_ids::EM_DAMAGE_RESONANCE, 0.0),
        (eve_sde::attr_ids::THERMAL_DAMAGE_RESONANCE, 0.0),
        (eve_sde::attr_ids::KINETIC_DAMAGE_RESONANCE, 0.0),
        (eve_sde::attr_ids::EXPLOSIVE_DAMAGE_RESONANCE, 0.0),
    ];
    sde.types.insert(
        91_759,
        make_fighter_type(91_759, 1759, 11, 50.0, zero_res_attrs),
    );
    // Attackable group 550 type (tested with both zero-resonance CombatState and live targets)
    // plus zero-bounty mission CONCORD (693), Empire Navy (665), and Sleeper (959) combat NPCs.
    for (tid, gid, name) in [
        (90_550_u32, 550_u32, "Zero-Res Custom NPC"),
        (90_693, 693, "SARO Frigate"),
        (90_665, 665, "Imperial Navy Detective"),
        (90_959, 959, "Sleepless Sentinel"),
    ] {
        let mut t = make_fighter_type(tid, gid, 11, 50.0, vec![]);
        t.name = name.into();
        sde.types.insert(tid, t);
    }
    ctx.sde = std::sync::Arc::new(sde);

    let ammo_id = 880_002_i64;
    let mod_id = 880_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            make_fighter_item(mod_id, 54_801, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            ammo_id,
            make_fighter_item(ammo_id, 54_802, char_id, ship_id, InventoryFlag::HiSlot0, 10),
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let spawn_npc = |ball_id: i64, type_id: u32, x: f64, hp: f64, zero_res: bool| {
        let sys_handle = sys_handle.clone();
        async move {
            let mut cs = CombatState {
                shield_hp: hp,
                max_shield_hp: hp,
                armor_hp: hp,
                max_armor_hp: hp,
                structure_hp: hp,
                max_structure_hp: hp,
                signature_radius: 50.0,
                ..CombatState::default()
            };
            if zero_res {
                cs.shield_resonance = [0.0; 4];
                cs.armor_resonance = [0.0; 4];
                cs.structure_resonance = [0.0; 4];
            }
            sys_handle
                .spawn_ball_with_combat(
                    Ball {
                        id: ball_id,
                        type_id,
                        position: DVec3::new(x, 0.0, 0.0),
                        velocity: DVec3::ZERO,
                        max_velocity: 100.0,
                        inertia: 1.0,
                        mass: 1_000_000.0,
                        radius: 50.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    },
                    cs,
                )
                .await
                .unwrap();
        }
    };

    // Ship at origin
    spawn_npc(ship_id, 23_915, 0.0, 5_000.0, false).await;

    // 1. Every invulnerable target (CONCORD 300/301/182, props 306/494/922/4073/4576/4771,
    //    SDE zero-res 91_759, CombatState zero-res 90_550) returns TargetInvulnerable without ammo loss
    let invuln_balls: Vec<(i64, u32, f64, bool)> = vec![
        (950_300, 90_300, 1_500.0, false),
        (950_301, 90_301, 1_600.0, false),
        (950_182, 90_182, 1_700.0, false),
        (950_306, 90_306, 1_800.0, false),
        (950_494, 90_494, 1_900.0, false),
        (950_922, 90_922, 2_000.0, false),
        (954_073, 94_073, 2_100.0, false),
        (954_576, 94_576, 2_200.0, false),
        (954_771, 94_771, 2_300.0, false),
        (951_759, 91_759, 2_400.0, false),
        (950_550, 90_550, 2_500.0, true),
    ];
    for &(bid, tid, x, zero_res) in &invuln_balls {
        spawn_npc(bid, tid, x, 10.0, zero_res).await;
    }
    for &(bid, _, _, _) in &invuln_balls {
        let err = handle_legacy_and_ess(
            &ctx,
            &mut session,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(bid)],
            &kwargs,
        )
        .await
        .expect("Activate handled")
        .expect_err("TargetInvulnerable expected");
        assert_eq!(err.code, "TargetInvulnerable", "ball {bid} must be rejected");
    }
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&ammo_id].quantity),
        10,
        "Rejected TargetInvulnerable shots must never consume loaded ammo"
    );

    // 2. Attack primary zero-bounty Mission CONCORD NPC (693) at 1_000m while zero-bounty Sleeper (959)
    //    and Empire Navy (665) sit within 10km alongside CONCORD (300/301) and non-combat props (306/4073).
    //    Vorton Projector must hit primary, chain ONLY onto Sleeper + Empire Navy (2 secondary hits),
    //    destroy Sleeper (10 HP) & decrement mission counter, and leave CONCORD & props completely untouched.
    let primary_bid = 960_693_i64;
    let sleeper_arc_bid = 960_959_i64;
    let navy_arc_bid = 960_665_i64;
    spawn_npc(primary_bid, 90_693, 1_000.0, 5_000.0, false).await;
    spawn_npc(sleeper_arc_bid, 90_959, 2_800.0, 10.0, false).await;
    spawn_npc(navy_arc_bid, 90_665, 3_000.0, 5_000.0, false).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, 3_018_418),
            ActiveMissionRecord {
                character_id: char_id,
                agent_id: 3_018_418,
                mission_id: 2,
                state: "Accepted".into(),
                solar_system_id: sys_id,
                current_room_id: 0,
                npcs_remaining: 2,
                spawned_npc_ball_ids: vec![primary_bid, sleeper_arc_bid],
                bookmark_id: None,
            },
        );
    });

    let res = handle_legacy_and_ess(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(primary_bid)],
        &kwargs,
    )
    .await
    .expect("Activate handled")
    .expect("Zero-bounty combat NPC attackable");
    assert_eq!(res.dict_get("weaponKind"), Some(&PyRep::String("VortonProjector".into())));
    assert_eq!(res.dict_get("destroyed"), Some(&PyRep::Bool(false)));
    assert_eq!(
        res.dict_get("secondaryTargetsHit"),
        Some(&PyRep::Integer(2)),
        "Vorton arc must chain to zero-bounty Sleeper + Empire Navy while excluding CONCORD & props"
    );
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&ammo_id].quantity), 9);

    let primary_cs = sys_handle.get_combat_state(primary_bid).await.unwrap().unwrap();
    assert!(primary_cs.shield_hp < 5_000.0, "Primary zero-bounty combat NPC took damage");
    let navy_cs = sys_handle.get_combat_state(navy_arc_bid).await.unwrap().unwrap();
    assert!(navy_cs.shield_hp < 5_000.0, "Secondary zero-bounty Empire Navy NPC took Vorton arc damage");
    assert!(
        sys_handle.get_ball_snapshot(sleeper_arc_bid).await.unwrap().is_none(),
        "Secondary zero-bounty Sleeper NPC destroyed by Vorton arc"
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.active_missions[&(char_id, 3_018_418)].npcs_remaining,
            1,
            "Secondary Vorton arc Sleeper kill decrements active mission counter"
        );
    });
    for &(bid, _, _, _) in &invuln_balls {
        let cs = sys_handle.get_combat_state(bid).await.unwrap().unwrap();
        assert_eq!(cs.shield_hp, 10.0, "Invulnerable ball {bid} must never be chained by Vorton arc");
    }
}

fn seed_standings_test_turret_sde(sde: &mut eve_sde::SdeRegistry) {
    if let Some(ship_t) = sde.types.get_mut(&23_915) {
        ship_t.default_attributes.insert(14, 8.0);
        ship_t.default_attributes.insert(102, 8.0);
        ship_t.default_attributes.insert(11, 50_000.0);
        ship_t.default_attributes.insert(48, 50_000.0);
    }
    sde.groups.insert(
        990_674,
        eve_sde::GroupInfo {
            group_id: 990_674,
            category_id: 11,
            name: "Deadspace Caldari State Battleship".into(),
            anchorable: false,
            anchored: false,
            fittable_non_singleton: false,
            use_base_price: false,
            published: true,
        },
    );
    sde.groups.insert(
        990_675,
        eve_sde::GroupInfo {
            group_id: 990_675,
            category_id: 11,
            name: "Synthetic Runtime Faction NPC".into(),
            anchorable: false,
            anchored: false,
            fittable_non_singleton: false,
            use_base_price: false,
            published: true,
        },
    );
    let mut wep_t = make_fighter_type(
        2_905,
        55,
        7,
        5.0,
        vec![
            (51, 1_000.0),
            (54, 15_000.0),
            (64, 2.0),
            (128, 1.0),
            (158, 10_000.0),
            (160, 100.0),
            (604, 83.0),
        ],
    );
    wep_t.effect_ids = vec![12];
    sde.types.insert(2_905, wep_t);
    sde.types.insert(
        192,
        make_fighter_type(
            192,
            83,
            8,
            0.01,
            vec![
                (128, 1.0),
                (114, 500.0),
                (116, 500.0),
                (117, 500.0),
                (118, 500.0),
            ],
        ),
    );
}

fn prepare_standings_test_pilot(ctx: &ServerContext, char_id: i64, ship_id: i64) {
    let now = ctx.now_unix();
    ctx.db
        .acquire_online_lock(1, 701, Some(char_id), true, now)
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.clear();
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let _ = ctx.drain_notifications_for_char(char_id);
}

fn make_standings_test_ctx_and_session(
    customize_sde: impl FnOnce(&mut eve_sde::SdeRegistry),
) -> (ServerContext, SessionState, i64, i64, u32) {
    let mut sde = make_fighter_sde_registry();
    customize_sde(&mut sde);
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    // Construct ServerContext after seeding custom SDE types so `npc_death_hook`'s `Weak<SdeRegistry>` stays valid.
    let ctx = ServerContext::with_sde_and_repository(std::sync::Arc::new(sde), db);
    let char_id = 90_000_001i64;
    let ship_id = 200_000_001i64;
    let sys_id = 30_000_142u32;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_fighter_item(ship_id, 23_915, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(1_000_009);
    session.ship_id = Some(ship_id);
    session.solar_system_id = Some(sys_id);
    (ctx, session, char_id, ship_id, sys_id)
}

#[tokio::test]
async fn legacy_activate_primary_npc_and_runtime_hook_apply_faction_standing_loss_without_double_apply() {
    use crate::services::combat_crimewatch::combat_npc_ai::combat_npc_ai_tests::make_test_npc_entity;
    use crate::services::combat_crimewatch::with_mut_combat_runtime;

    let (ctx, mut session, char_id, ship_id, sys_id) = make_standings_test_ctx_and_session(|sde| {
        seed_standings_test_turret_sde(sde);
        let mut npc_sde_faction = make_fighter_type(
            90_674,
            990_674,
            11,
            1_000.0,
            vec![(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 10_000.0), (562, 0.03)],
        );
        npc_sde_faction.faction_id = Some(500_001);
        npc_sde_faction.race_id = None;
        sde.types.insert(90_674, npc_sde_faction);

        let mut npc_runtime_faction = make_fighter_type(
            90_675,
            990_675,
            11,
            1_000.0,
            vec![(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 5_000.0), (562, 0.02)],
        );
        npc_runtime_faction.faction_id = None;
        npc_runtime_faction.race_id = None;
        sde.types.insert(90_675, npc_runtime_faction);
    });
    let kwargs = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            801_001,
            make_fighter_item(801_001, 2_905, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            801_002,
            make_fighter_item(801_002, 192, char_id, ship_id, InventoryFlag::HiSlot0, 10),
        );
    });
    prepare_standings_test_pilot(&ctx, char_id, ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ballpark_npc_id = 970_674_i64;
    let runtime_npc_id = 970_675_i64;
    for (id, tid, pos, hp) in [
        (ship_id, 23_915_u32, DVec3::ZERO, 1_000.0),
        (ballpark_npc_id, 90_674_u32, DVec3::new(1_000.0, 0.0, 0.0), 10.0),
        (runtime_npc_id, 90_675_u32, DVec3::new(1_500.0, 0.0, 0.0), 10.0),
    ] {
        sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id,
                    type_id: tid,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 200.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: 0.0,
                    max_shield_hp: hp,
                    armor_hp: 0.0,
                    max_armor_hp: hp,
                    structure_hp: hp,
                    max_structure_hp: hp,
                    signature_radius: 50.0,
                    ..CombatState::default()
                },
            )
            .await
            .unwrap();
    }

    // 1. Ballpark-only NPC (not in rt.npc_entities) killed via 1-arg dogmaIM.Activate
    let kill_ballpark = handle_legacy_and_ess(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(ballpark_npc_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(kill_ballpark.dict_get("destroyed"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|snap| {
        let st = &snap.characters[&char_id].standings;
        assert!(
            (st[&500_001] - (-0.03)).abs() < 1e-9,
            "1-arg Activate primary NPC kill must lower Caldari standing to -0.03, got {:?}",
            st.get(&500_001)
        );
        assert!(
            (st[&500_003] - (-0.015)).abs() < 1e-9,
            "derived ally Amarr standing must drop to -0.015, got {:?}",
            st.get(&500_003)
        );
        assert_eq!(st.get(&500_004).copied().unwrap_or(0.0), 0.0);
    });
    let notes_1 = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notes_1.iter().filter(|n| n.event == "OnStandingSet").count(), 1);
    assert_eq!(notes_1.iter().filter(|n| n.event == "OnStandingsModified").count(), 1);

    // 2. Runtime rt.npc_entities NPC with explicit faction_id (500_002) killed via 1-arg dogmaIM.Activate:
    //    apply_damage_with_attacker passes Some(char_id) to npc_death_hook AND inline path must NOT double-apply.
    ctx.weapon_cycle_state.write().unwrap().clear();
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.clear();
    });
    with_mut_combat_runtime(&ctx, |rt| {
        let mut ent = make_test_npc_entity(runtime_npc_id, 90_675, sys_id, "Minmatar");
        ent.faction_id = 500_002;
        rt.npc_entities.insert(runtime_npc_id, ent);
    });
    let kill_runtime = handle_legacy_and_ess(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(runtime_npc_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(kill_runtime.dict_get("destroyed"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|snap| {
        let st = &snap.characters[&char_id].standings;
        let got = st
            .get(&500_002)
            .copied()
            .unwrap_or_else(|| panic!("missing 500_002 in standings={st:?}"));
        assert!(
            (got - (-0.02)).abs() < 1e-9,
            "runtime npc_death_hook + legacy Activate must apply -0.02 once (not double-applied), got {got:?} (standings={st:?})"
        );
    });
    let notes_2 = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notes_2.iter().filter(|n| n.event == "OnStandingSet").count(), 1);
    assert_eq!(notes_2.iter().filter(|n| n.event == "OnStandingsModified").count(), 1);
}

#[tokio::test]
async fn legacy_activate_vorton_secondary_arc_kill_applies_standing_loss_when_primary_survives() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_standings_test_ctx_and_session(|sde| {
        if let Some(ship_t) = sde.types.get_mut(&23_915) {
            ship_t.default_attributes.insert(14, 8.0);
            ship_t.default_attributes.insert(102, 8.0);
            ship_t.default_attributes.insert(11, 50_000.0);
            ship_t.default_attributes.insert(48, 50_000.0);
        }
        sde.groups.insert(
            990_674,
            eve_sde::GroupInfo {
                group_id: 990_674,
                category_id: 11,
                name: "Deadspace Triglavian NPC".into(),
                anchorable: false,
                anchored: false,
                fittable_non_singleton: false,
                use_base_price: false,
                published: true,
            },
        );
        let mut vorton_t = make_fighter_type(
            54_801,
            4060,
            7,
            10.0,
            vec![
                (51, 1_000.0),
                (54, 25_000.0),
                (128, 1.0),
                (604, 4061.0),
                (2104, 5.0),
                (1353, 0.40),
            ],
        );
        vorton_t.effect_ids = vec![12];
        sde.types.insert(54_801, vorton_t);
        sde.types.insert(
            54_802,
            make_fighter_type(
                54_802,
                4061,
                8,
                0.1,
                vec![(128, 1.0), (114, 1_000.0), (653, 200.0), (654, 40.0)],
            ),
        );
        let mut primary_t = make_fighter_type(90_674, 990_674, 11, 50.0, vec![(562, 0.03)]);
        primary_t.faction_id = Some(500_001);
        primary_t.race_id = None;
        sde.types.insert(90_674, primary_t);

        let mut secondary_t = make_fighter_type(90_676, 990_674, 11, 50.0, vec![(562, 0.05)]);
        secondary_t.faction_id = Some(500_026);
        secondary_t.race_id = None;
        sde.types.insert(90_676, secondary_t);
    });
    let kwargs = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            882_001,
            make_fighter_item(882_001, 54_801, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            882_002,
            make_fighter_item(882_002, 54_802, char_id, ship_id, InventoryFlag::HiSlot0, 10),
        );
    });
    prepare_standings_test_pilot(&ctx, char_id, ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let primary_bid = 971_001_i64;
    let secondary_bid = 971_002_i64;
    for (bid, tid, x, hp) in [
        (ship_id, 23_915_u32, 0.0, 5_000.0),
        (primary_bid, 90_674_u32, 1_000.0, 5_000.0),
        (secondary_bid, 90_676_u32, 2_500.0, 10.0),
    ] {
        sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: bid,
                    type_id: tid,
                    position: DVec3::new(x, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 100.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: hp,
                    max_shield_hp: hp,
                    armor_hp: hp,
                    max_armor_hp: hp,
                    structure_hp: hp,
                    max_structure_hp: hp,
                    signature_radius: 50.0,
                    ..CombatState::default()
                },
            )
            .await
            .unwrap();
    }

    let res = handle_legacy_and_ess(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(primary_bid)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(res.dict_get("weaponKind"), Some(&PyRep::String("VortonProjector".into())));
    assert_eq!(res.dict_get("destroyed"), Some(&PyRep::Bool(false)));
    assert_eq!(res.dict_get("secondaryTargetsHit"), Some(&PyRep::Integer(1)));
    assert!(sys_handle.get_ball_snapshot(secondary_bid).await.unwrap().is_none());

    ctx.db.with_snapshot(|snap| {
        let st = &snap.characters[&char_id].standings;
        assert!(
            (st[&500_026] - (-0.05)).abs() < 1e-9,
            "destroyed Vorton secondary arc NPC must apply -0.05 standing loss even when primary survives, got {:?}",
            st.get(&500_026)
        );
        assert_eq!(
            st.get(&500_001).copied().unwrap_or(0.0),
            0.0,
            "surviving primary target must not incur standing loss"
        );
    });
    let notes = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notes.iter().filter(|n| n.event == "OnStandingSet").count(), 1);
    assert_eq!(notes.iter().filter(|n| n.event == "OnStandingsModified").count(), 1);
}

#[tokio::test]
async fn legacy_activate_convoy_kill_applies_faction_standing_loss() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_standings_test_ctx_and_session(|sde| {
        seed_standings_test_turret_sde(sde);
        let hauler_tid = crate::living_world::hauler_type_for_system(sde, 30_000_142);
        let convoy_t = sde.types.entry(hauler_tid).or_insert_with(|| {
            make_fighter_type(hauler_tid, 990_674, 11, 1_000.0, vec![])
        });
        convoy_t.category_id = 11;
        convoy_t.group_id = 990_674;
        convoy_t.faction_id = Some(500_026);
        convoy_t.race_id = None;
        convoy_t.default_attributes.insert(562, 0.01);
    });
    let kwargs = HashMap::new();

    let convoy_id = ctx.living_world.ensure_system_hauler_convoy(sys_id, &ctx.sde);
    assert_eq!(
        ctx.living_world
            .materialize_system_lod2(sys_id, &ctx.systems, &ctx.sde)
            .await,
        1
    );
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, convoy_ball) = sys_handle.get_ball_snapshot(convoy_id).await.unwrap().unwrap();

    assert!(sys_handle.try_update_combat_state(convoy_id, |cs| {
        cs.shield_hp = 0.0;
        cs.armor_hp = 0.0;
        cs.structure_hp = 5.0;
    }));
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 23_915,
                position: convoy_ball.position + DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 2_000.0,
                max_shield_hp: 2_000.0,
                armor_hp: 2_000.0,
                max_armor_hp: 2_000.0,
                structure_hp: 2_000.0,
                max_structure_hp: 2_000.0,
                signature_radius: 50.0,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            802_001,
            make_fighter_item(802_001, 2_905, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            802_002,
            make_fighter_item(802_002, 192, char_id, ship_id, InventoryFlag::HiSlot0, 5),
        );
    });
    prepare_standings_test_pilot(&ctx, char_id, ship_id);

    let convoy_kill = handle_legacy_and_ess(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(convoy_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(convoy_kill.dict_get("convoyID"), Some(&PyRep::Integer(convoy_id)));
    assert_eq!(convoy_kill.dict_get("destroyed"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|snap| {
        let st = &snap.characters[&char_id].standings;
        assert!(
            (st[&500_026] - (-0.01)).abs() < 1e-9,
            "convoy kill via 1-arg Activate must apply -0.01 standing loss, got {:?}",
            st.get(&500_026)
        );
    });
    let notes = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notes.iter().filter(|n| n.event == "OnStandingSet").count(), 1);
    assert_eq!(notes.iter().filter(|n| n.event == "OnStandingsModified").count(), 1);
}
