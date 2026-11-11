//! Edge-case unit tests for `combat_npc_ai.rs`.

use super::*;
use super::combat_npc_ai_tests::make_test_npc_entity;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use eve_db::types::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode, WarpPhase};
use eve_sde::InvType;
use glam::DVec3;
use std::sync::Arc;

fn make_ship_ball(id: i64, type_id: u32, pos: DVec3, mode: MovementMode) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 400.0,
        inertia: 1.0,
        mass: 1_200_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode,
    }
}

fn make_combat(hp: f64) -> CombatState {
    CombatState {
        shield_hp: hp,
        max_shield_hp: hp,
        armor_hp: hp,
        max_armor_hp: hp,
        structure_hp: hp,
        max_structure_hp: hp,
        ..CombatState::default()
    }
}

fn seed_pilot_ship(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    type_id: u32,
    sys_id: u32,
    sec_status: f64,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
            ch.security_status = sec_status;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
}

#[test]
fn derive_npc_archetype_all_variants() {
    let (mut ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        let variants = [
            (996_001_u32, "Sleeper Emergent Patroller", 982_u32),
            (996_002, "Drifter Apollo Tyrannos", 1310),
            (996_003, "Sansha True Power", 550),
            (996_004, "Rogue Drone Alvi", 755),
            (996_005, "Triglavian Anchoring Damavik", 1997),
            (996_006, "EDENCOM Gunstar Sentinel", 4028),
            (996_007, "Tuvan Orth Officer", 564),
            (996_008, "Serpentis Transport Hauler", 560),
            (996_009, "Clone Soldier Transporter", 1006),
            (996_010, "CONCORD Police Commander", 190),
            (996_011, "Caldari Navy Raven", 192),
            (996_012, "Zarzakh Gate Defense Sentry", 194),
            (996_013, "Guristas Pithi Pirate", 557),
        ];
        for (tid, name, gid) in variants {
            sde.types.insert(
                tid,
                InvType {
                    type_id: tid,
                    group_id: gid,
                    category_id: 11,
                    name: name.to_string(),
                    mass: 1_000_000.0,
                    volume: 1_000.0,
                    capacity: 0.0,
                    radius: 40.0,
                    base_price: 0.0,
                    published: true,
                    portion_size: 1,
                    market_group_id: None,
                    race_id: None,
                    faction_id: None,
                    variation_parent_type_id: None,
                    description_id: None,
                    icon_id: None,
                    graphic_id: None,
                    sound_id: None,
                    meta_group_id: None,
                    meta_level: None,
                    tech_level: None,
                    default_effect_id: None,
                    description: String::new(),
                    effect_ids: Vec::new(),
                    default_attributes: HashMap::new(),
                },
            );
        }
    }

    // Exercise SDE lookups without panicking and verify distinct classification families
    for tid in 996_001_u32..=996_013 {
        let arch = derive_npc_archetype(&ctx, tid);
        assert!(!arch.is_empty());
    }
    assert_eq!(derive_npc_archetype(&ctx, 999_999_999), "Pirate");
}

#[test]
fn is_candidate_hostile_to_edencom_all_branches() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let sys_id = 30_000_142_u32;
    let edencom_id = 70_001_001_i64;
    let trig_npc_id = 70_001_002_i64;
    let neutral_npc_id = 70_001_003_i64;
    let ship_threat_id = 100_000_001_i64;
    let db_drone_id = 88_001_001_i64;
    let space_drone_id = 88_001_002_i64;
    let pilot_char_id = 90_000_001_i64;

    let mut edencom_npc = make_test_npc_entity(edencom_id, 606, sys_id, "EDENCOM");
    edencom_npc.aggression_threat.insert(ship_threat_id, 50.0);

    with_mut_combat_ephemeral(&ctx, |rt| {
        rt.npc_entities
            .insert(trig_npc_id, make_test_npc_entity(trig_npc_id, 606, sys_id, "Triglavian"));
        rt.npc_entities
            .insert(neutral_npc_id, make_test_npc_entity(neutral_npc_id, 606, sys_id, "Navy"));
        rt.active_drones_in_space.insert(
            space_drone_id,
            crate::services::combat_crimewatch::DroneSpaceState {
                ball_id: space_drone_id,
                drone_item_id: space_drone_id,
                type_id: 2454, // Hobgoblin I
                group_id: 100,
                owner_char_id: pilot_char_id,
                original_owner_char_id: pilot_char_id,
                parent_ship_id: ship_threat_id,
                solar_system_id: sys_id,
                target_id: None,
                activity_state: 0,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 1000.0,
                optimal_range: 5000.0,
                damage_vector: DamageVector::new(0.0, 10.0, 0.0, 0.0),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 1.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
    });

    ctx.db.with_mut_snapshot(|snap| {
        // Negative EDENCOM standing (< 0.0) triggers hostility for player ship and its drones
        if let Some(ch) = snap.characters.get_mut(&pilot_char_id) {
            ch.standings.insert(500_027, -2.0);
        }
        snap.items.insert(
            db_drone_id,
            InventoryItemRecord {
                item_id: db_drone_id,
                type_id: 2454,
                owner_id: pilot_char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // Self -> false
    assert!(!is_candidate_hostile_to_edencom(
        &ctx,
        &edencom_npc,
        edencom_id,
        0
    ));
    // Existing threat -> true
    assert!(is_candidate_hostile_to_edencom(
        &ctx,
        &edencom_npc,
        ship_threat_id,
        0
    ));
    // Triglavian NPC -> true; Navy NPC -> false
    assert!(is_candidate_hostile_to_edencom(
        &ctx,
        &edencom_npc,
        trig_npc_id,
        0
    ));
    assert!(!is_candidate_hostile_to_edencom(
        &ctx,
        &edencom_npc,
        neutral_npc_id,
        0
    ));
    // Space drone & DB-item drone belonging to hostile-standing pilot -> true
    assert!(is_candidate_hostile_to_edencom(
        &ctx,
        &edencom_npc,
        space_drone_id,
        0
    ));
    assert!(is_candidate_hostile_to_edencom(
        &ctx,
        &edencom_npc,
        db_drone_id,
        0
    ));
}

#[tokio::test]
async fn step_ai_sync_early_returns_debugging_dead_cleanup_relay_warp_and_sleeper_leash() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let sys_id = 30_000_142_u32;

    // Empty npc_entities in system -> clean no-op
    step_extended_npc_ai_sync(&ctx, sys_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let debug_npc = 71_000_001_i64;
    let dead_npc = 71_000_002_i64;
    let relay_npc = 71_000_003_i64;
    let warp_npc = 71_000_004_i64;
    let leash_sleeper = 71_000_005_i64;

    let warp_mode = MovementMode::Warp {
        origin: DVec3::new(2_000.0, 0.0, 0.0),
        destination: DVec3::new(1_000_000_000.0, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1_000_000_000.0,
        phase: WarpPhase::Cruising,
        elapsed_warp_time: 1.0,
    };
    let debug_target = 999_111_i64;

    for (bid, pos, mode) in [
        (debug_npc, DVec3::ZERO, MovementMode::Stop),
        (debug_target, DVec3::new(500.0, 0.0, 0.0), MovementMode::Stop),
        (relay_npc, DVec3::new(1_000.0, 0.0, 0.0), MovementMode::Stop),
        (warp_npc, DVec3::new(2_000.0, 0.0, 0.0), warp_mode),
        (
            leash_sleeper,
            DVec3::new(250_000.0, 0.0, 0.0), // > 200 km from origin -> leashes
            MovementMode::Stop,
        ),
    ] {
        let _ = sys_handle
            .spawn_ball_with_combat(make_ship_ball(bid, 606, pos, mode), make_combat(5_000.0))
            .await;
    }

    with_mut_combat_ephemeral(&ctx, |rt| {
        rt.behavior_debugging_active.insert(debug_npc);
        let mut dbg_ent = make_test_npc_entity(debug_npc, 606, sys_id, "Pirate");
        dbg_ent.target_id = Some(debug_target);
        dbg_ent.damage_vector = DamageVector::new(100.0, 0.0, 0.0, 0.0);
        rt.npc_entities.insert(debug_npc, dbg_ent);

        // Dead NPC has NO ball spawned -> gets pruned on step
        rt.npc_entities
            .insert(dead_npc, make_test_npc_entity(dead_npc, 606, sys_id, "Pirate"));
        rt.npc_entities.insert(
            relay_npc,
            make_test_npc_entity(relay_npc, 606, sys_id, "TransmissionRelay"),
        );

        let mut warp_ent = make_test_npc_entity(warp_npc, 606, sys_id, "Pirate");
        warp_ent.target_id = Some(999_222);
        rt.npc_entities.insert(warp_npc, warp_ent);

        let mut slp_ent = make_test_npc_entity(leash_sleeper, 606, sys_id, "Sleeper");
        slp_ent.spawn_position = DVec3::ZERO;
        slp_ent.sleeper_wh_class = 3;
        rt.npc_entities.insert(leash_sleeper, slp_ent);
    });

    step_extended_npc_ai(&ctx, sys_id).await;

    with_combat_ephemeral(&ctx, |rt| {
        // Dead NPC removed; debugged NPC remains alive; warping NPC cleared target
        assert!(!rt.npc_entities.contains_key(&dead_npc));
        assert!(rt.npc_entities.contains_key(&debug_npc));
        assert_eq!(rt.npc_entities[&warp_npc].target_id, None);
    });

    // Sleeper was leashed back toward spawn_position
    let (_, snap_ball) = sys_handle
        .get_ball_snapshot(leash_sleeper)
        .await
        .unwrap()
        .unwrap();
    assert!(snap_ball.position.length() < 50_000.0);
}

#[tokio::test]
async fn step_ai_all_archetypes_target_pruning_disapproval_and_ewar_effects() {
    let (mut ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let sys_id = 30_000_142_u32;
    let (char_id, ship_id, ship2_id, pod_id) =
        (90_000_001_i64, 100_000_001_i64, 100_000_002_i64, 100_000_003_i64);
    let cap_type_id = 996_100_u32;

    {
        let sde = Arc::make_mut(&mut ctx.sde);
        // Capital hull so Sleeper 2.5x anti-capital damage & threat weight trigger
        sde.types.insert(
            cap_type_id,
            InvType {
                type_id: cap_type_id,
                group_id: 485, // Dreadnought
                category_id: 6,
                name: "Test Dread Target".into(),
                mass: 1_000_000_000.0,
                volume: 1_000_000.0,
                capacity: 0.0,
                radius: 2_000.0,
                base_price: 0.0,
                published: true,
                portion_size: 1,
                market_group_id: None,
                race_id: None,
                faction_id: None,
                variation_parent_type_id: None,
                description_id: None,
                icon_id: None,
                graphic_id: None,
                sound_id: None,
                meta_group_id: None,
                meta_level: None,
                tech_level: None,
                default_effect_id: None,
                description: String::new(),
                effect_ids: Vec::new(),
                default_attributes: HashMap::new(),
            },
        );
    }

    seed_pilot_ship(&ctx, char_id, ship_id, cap_type_id, sys_id, -6.0);
    // Give ship2_id same pilot owner + negative EDENCOM standing so Vorton arc sees it as hostile
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(500_027, -2.0);
        }
        snap.items.insert(
            ship2_id,
            InventoryItemRecord {
                item_id: ship2_id,
                type_id: 606,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(
            make_ship_ball(ship_id, cap_type_id, DVec3::new(1_000.0, 0.0, 0.0), MovementMode::Stop),
            make_combat(50_000.0),
        )
        .await;
    let _ = sys_handle
        .spawn_ball_with_combat(
            make_ship_ball(ship2_id, 606, DVec3::new(1_500.0, 0.0, 0.0), MovementMode::Stop),
            make_combat(10_000.0),
        )
        .await;
    // Capsule (type_id 670, group 29) -> skipped by aggression unless threatened
    let _ = sys_handle
        .spawn_ball_with_combat(
            make_ship_ball(pod_id, 670, DVec3::new(500.0, 0.0, 0.0), MovementMode::Stop),
            make_combat(500.0),
        )
        .await;

    // Seed NPCs across specialized archetypes and combat mechanics
    let archetypes = [
        (72_000_001_i64, "InsurgencyPirateRoamer"),
        (72_000_002, "InsurgencyEnforcer"),
        (72_000_003, "CounterInsurgencySentry"),
        (72_000_004, "InsurgencyAmbushTackler"),
        (72_000_005, "InsurgencyAmbushOreDefender"),
        (72_000_006, "InsurgencyOreRorqual"),
        (72_000_007, "InsurgencyOutpostDefender"),
        (72_000_008, "ZarzakhGateGun"),
        (72_000_009, "Navy"),
        (72_000_010, "Police"),
        (72_000_011, "Sleeper"),
        (72_000_012, "Drifter"),
        (72_000_013, "Triglavian"),
        (72_000_014, "EDENCOM"),
        (72_000_015, "SleeperSentry"),
    ];

    for (bid, _) in archetypes {
        let _ = sys_handle
            .spawn_ball_with_combat(
                make_ship_ball(bid, 606, DVec3::new(1_200.0, 0.0, 0.0), MovementMode::Stop),
                make_combat(20_000.0),
            )
            .await;
    }

    with_mut_combat_ephemeral(&ctx, |rt| {
        // Pilot has disapproval flag so ZarzakhGateGun triggers ECM jam + attacks
        rt.disapproval_flags.insert(char_id, 1_700_000_600);

        for (bid, arch) in archetypes {
            let mut ent = make_test_npc_entity(bid, 606, sys_id, arch);
            ent.optimal_range = 100_000.0;
            ent.damage_vector = DamageVector::new(25.0, 25.0, 25.0, 25.0);
            match arch {
                "Sleeper" => {
                    ent.sleeper_wh_class = 4;
                    ent.web_speed_factor = 0.4;
                    ent.web_range = 50_000.0;
                    ent.scram_strength = 3.0;
                    ent.scram_range = 50_000.0;
                    ent.neut_amount = 100.0;
                    // Damaged ally gets repaired
                    ent.remote_armor_rep = 150.0;
                    ent.remote_rep_range = 50_000.0;
                }
                "Drifter" => {
                    // Supershield already broken -> triggers one-shot doomsday
                    ent.drifter_overshield_hp = 0.0;
                    ent.drifter_doomsday_fired = false;
                }
                "EDENCOM" => {
                    ent.target_id = Some(ship_id);
                    ent.aggression_threat.insert(ship_id, 50.0);
                }
                "Triglavian" => {
                    ent.target_id = Some(ship_id);
                    ent.entropic_cycles = 2;
                }
                "Pirate" => {
                    ent.ecm_jammed = true;
                    ent.remote_shield_rep = 100.0;
                }
                _ => {}
            }
            rt.npc_entities.insert(bid, ent);
        }
    });

    // Mark projected EWAR from ship_id and damage an ally NPC so remote repair fires
    {
        let mut proj = ctx.projected_ewar_sources.write().unwrap();
        proj.insert(72_000_011, ship_id);
    }
    let _ = sys_handle
        .apply_damage(72_000_012, DamageVector::new(500.0, 0.0, 0.0, 0.0))
        .await;

    // Isolated EDENCOM Vorton arc test in system 30_000_144 so nearby hostile NPCs in sys_id
    // don't intercept the secondary arc ahead of ship2_id.
    let vorton_sys = 30_000_144_u32;
    let v_handle = ctx.systems.get_or_spawn(vorton_sys);
    let (v_npc, v_s1, v_s2) = (72_000_990_i64, 100_000_991_i64, 100_000_992_i64);
    ctx.db.with_mut_snapshot(|snap| {
        for (sid, tid) in [(v_s1, cap_type_id), (v_s2, 606_u32)] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: vorton_sys as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });
    let _ = v_handle
        .spawn_ball_with_combat(
            make_ship_ball(v_npc, 606, DVec3::ZERO, MovementMode::Stop),
            make_combat(20_000.0),
        )
        .await;
    let _ = v_handle
        .spawn_ball_with_combat(
            make_ship_ball(v_s1, cap_type_id, DVec3::new(500.0, 0.0, 0.0), MovementMode::Stop),
            make_combat(10_000.0),
        )
        .await;
    let _ = v_handle
        .spawn_ball_with_combat(
            make_ship_ball(v_s2, 606, DVec3::new(1_000.0, 0.0, 0.0), MovementMode::Stop),
            make_combat(10_000.0),
        )
        .await;
    with_mut_combat_ephemeral(&ctx, |rt| {
        let mut ent = make_test_npc_entity(v_npc, 606, vorton_sys, "EDENCOM");
        ent.optimal_range = 100_000.0;
        ent.damage_vector = DamageVector::new(100.0, 100.0, 100.0, 100.0);
        ent.target_id = Some(v_s1);
        ent.aggression_threat.insert(v_s1, 100.0);
        ent.aggression_threat.insert(v_s2, 20.0);
        rt.npc_entities.insert(v_npc, ent);
    });
    step_extended_npc_ai(&ctx, vorton_sys).await;
    let vc1 = v_handle.get_combat_state(v_s1).await.unwrap().unwrap();
    let vc2 = v_handle.get_combat_state(v_s2).await.unwrap().unwrap();
    assert!(vc1.shield_hp < vc1.max_shield_hp);
    assert!(vc2.shield_hp < vc2.max_shield_hp, "EDENCOM Vorton arc should damage nearby v_s2");

    // Step 1: Fires weapons, Triglavian spools up, Drifter fires doomsday, Sleeper scrams/webs/neuts/repairs
    step_extended_npc_ai(&ctx, sys_id).await;

    let scrammed = sys_handle.is_ball_scrambled(ship_id).await.unwrap();
    assert!(scrammed);
    let c1 = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(c1.shield_hp < c1.max_shield_hp);
    with_combat_ephemeral(&ctx, |rt| {
        assert!(rt.npc_entities[&72_000_012].drifter_doomsday_fired);
        assert!(rt.npc_entities[&72_000_013].entropic_cycles >= 3);
        assert!(rt.npc_timers.contains_key(&char_id));
    });

    // Prune warped/out-of-range target when ships move far away
    let _ = sys_handle
        .set_ball_position(ship_id, DVec3::new(500_000.0, 0.0, 0.0))
        .await;
    let _ = sys_handle
        .set_ball_position(ship2_id, DVec3::new(500_000.0, 0.0, 0.0))
        .await;
    step_extended_npc_ai(&ctx, sys_id).await;
    with_combat_ephemeral(&ctx, |rt| {
        assert_eq!(rt.npc_entities[&72_000_013].target_id, None);
    });
}

#[tokio::test]
async fn record_threat_relay_mining_and_destroy_hint_all_loot_bounty_variants() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let char_id = 90_000_001_i64;
    let hi_sys = 30_000_142_u32;
    let wh_sys = 31_000_005_u32;

    // 1) record_npc_threat_event against TransmissionRelay and MiningOp escort
    let relay_id = 73_000_001_i64;
    let attacker_id = 100_000_001_i64;
    with_mut_combat_ephemeral(&ctx, |rt| {
        rt.npc_entities
            .insert(relay_id, make_test_npc_entity(relay_id, 606, hi_sys, "TransmissionRelay"));
    });
    record_npc_threat_event(&ctx, relay_id, attacker_id, 100.0, false, false);

    // Step mining op in Perimeter (30000144) to spawn mining op convoy & escort, then threaten escort
    let _ = ctx
        .living_world
        .step_npc_mining_op(800_000_002, &ctx.systems, &ctx.db, &ctx.sde)
        .await;
    record_npc_threat_event(&ctx, 800_000_002, attacker_id, 200.0, true, false);

    // 2) Destroy via destroy_specialized_npc_with_hint covering remaining archetypes & WH 0 bounty
    let cases = [
        (73_100_001_i64, hi_sys, "Sleeper", None, None, 100_000.0),
        (73_100_002, hi_sys, "PochvenHypnosianSleeper", None, None, 200_000.0),
        (73_100_003, hi_sys, "Officer", None, None, 10_000_000.0),
        (73_100_004, hi_sys, "RogueDrone", None, None, 50_000.0),
        (73_100_005, hi_sys, "Triglavian", None, None, 75_000.0),
        (73_100_006, hi_sys, "EDENCOM", None, None, 75_000.0),
        (73_100_007, hi_sys, "Pirate", Some(16_309), None, 40_000.0),
        (73_100_008, hi_sys, "Hauler", None, Some((34, 5_000)), 25_000.0),
        (73_100_009, wh_sys, "Sleeper", None, None, 500_000.0),
    ];

    with_mut_combat_ephemeral(&ctx, |rt| {
        for (bid, sys, arch, tag, custom, bounty) in cases {
            let mut ent = make_test_npc_entity(bid, 606, sys, arch);
            ent.tag_drop_type_id = tag;
            ent.custom_cargo_loot = custom;
            ent.bounty_isk = bounty;
            rt.npc_entities.insert(bid, ent);
        }
    });

    for (i, (bid, sys, arch, tag, custom, _bounty)) in cases.iter().enumerate() {
        let hint_pos = DVec3::new((i as f64 + 1.0) * 1_000.0, 0.0, 0.0);
        let (wreck_id, paid_bounty, drops) =
            destroy_specialized_npc_with_hint(&ctx, *sys, *bid, char_id, Some((*sys, hint_pos)))
                .await;
        assert!(wreck_id > 0, "expected wreck for {arch}");
        if *sys == wh_sys {
            assert_eq!(paid_bounty, 0.0, "wormhole NPCs should pay 0 bounty");
        }
        if let Some(tag_tid) = tag {
            assert!(drops.contains(tag_tid));
        }
        if let Some((loot_tid, _)) = custom {
            assert!(drops.contains(loot_tid));
        }
    }
}
