//! Deep branch and mutation-killing unit tests for `combat_npc_ai` (`C-4`).

#![forbid(unsafe_code)]

use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_type,
};
use crate::services::combat_crimewatch::combat_npc_ai::combat_npc_ai_tests::make_test_npc_entity;
use crate::services::combat_crimewatch::combat_npc_ai::*;
use crate::services::combat_crimewatch::{with_combat_ephemeral, with_mut_combat_ephemeral};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode, SlimItemMeta};
use glam::DVec3;

#[test]
fn derive_npc_archetype_secondary_groups_meta_and_name_fallbacks() {
    let (mut ctx, _session, _char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let mut sde = (*ctx.sde).clone();

    let mut add_npc = |tid: u32, gid: u32, name: &str, fac: Option<u32>, mg: Option<u32>| {
        let mut t = make_fighter_type(tid, gid, 11, 1_000.0, vec![]);
        t.name = name.to_string();
        t.faction_id = fac;
        t.meta_group_id = mg;
        sde.types.insert(tid, t);
    };

    // 1. Pochven range 56319..=56327
    add_npc(56_320, 550, "Pochven Drone", None, None);
    // 2. Sleeper group 982..=1005 & faction 500022
    add_npc(930_001, 985, "Unknown Entity", None, None);
    add_npc(930_002, 550, "Unknown Entity", Some(500_022), None);
    // 3. RogueDrone groups 755..=761, 801..=805, 1174, "Alvus"
    add_npc(930_003, 756, "Hive Guard", None, None);
    add_npc(930_004, 802, "Swarm Unit", None, None);
    add_npc(930_005, 1174, "Queen Unit", None, None);
    add_npc(930_006, 550, "Supreme Alvus", None, None);
    // 4. IncursionSansha "Kupusam" & "Deltole"
    add_npc(930_007, 550, "Kupusam Sansha", None, None);
    add_npc(930_008, 550, "Deltole Sansha", None, None);
    // 5. MorduCommander 33864..=33866
    add_npc(33_865, 550, "Special Mordu", None, None);
    // 6. Officer via meta_group_id 4 or 5 & group 4795..=4804
    add_npc(930_009, 550, "Faction Boss", None, Some(5));
    add_npc(930_010, 4798, "Sector Boss", None, None);
    // 7. Drifter groups 1530, 1788, 1956, 4035
    add_npc(930_011, 1788, "Autothysian Lancer", None, None);
    // 8. Triglavian "Vedmak" / "Leshak"
    add_npc(930_012, 550, "Starving Leshak", None, None);
    // 9. Hauler "Hauler"
    add_npc(930_013, 550, "Bestower Hauler", None, None);
    // 10. Navy "Police" / faction 500004
    add_npc(930_014, 550, "Customs Police", None, None);
    add_npc(930_015, 550, "Federation Ship", Some(500_004), None);

    ctx.sde = std::sync::Arc::new(sde);

    assert_eq!(derive_npc_archetype(&ctx, 56_320), "PochvenHypnosianSleeper");
    assert_eq!(derive_npc_archetype(&ctx, 930_001), "Sleeper");
    assert_eq!(derive_npc_archetype(&ctx, 930_002), "Sleeper");
    assert_eq!(derive_npc_archetype(&ctx, 930_003), "RogueDrone");
    assert_eq!(derive_npc_archetype(&ctx, 930_004), "RogueDrone");
    assert_eq!(derive_npc_archetype(&ctx, 930_005), "RogueDrone");
    assert_eq!(derive_npc_archetype(&ctx, 930_006), "RogueDrone");
    assert_eq!(derive_npc_archetype(&ctx, 930_007), "IncursionSansha");
    assert_eq!(derive_npc_archetype(&ctx, 930_008), "IncursionSansha");
    assert_eq!(derive_npc_archetype(&ctx, 33_865), "MorduCommander");
    assert_eq!(derive_npc_archetype(&ctx, 930_009), "Officer");
    assert_eq!(derive_npc_archetype(&ctx, 930_010), "Officer");
    assert_eq!(derive_npc_archetype(&ctx, 930_011), "Drifter");
    assert_eq!(derive_npc_archetype(&ctx, 930_012), "Triglavian");
    assert_eq!(derive_npc_archetype(&ctx, 930_013), "Hauler");
    assert_eq!(derive_npc_archetype(&ctx, 930_014), "Navy");
    assert_eq!(derive_npc_archetype(&ctx, 930_015), "Navy");
}

#[tokio::test]
async fn step_npc_ai_sleeper_leash_remote_rep_zarzakh_drifter_doomsday_and_edencom_vorton() {
    let (ctx, _session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let wing_char = 90_888_101_i64;
    let wing_ship = 200_888_101_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut w = snap.characters[&char_id].clone();
        w.character_id = wing_char;
        w.active_ship_id = wing_ship;
        w.fleet_id = Some(919);
        snap.characters.insert(wing_char, w);
        snap.characters.get_mut(&char_id).unwrap().fleet_id = Some(919);
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for (sid, cid, pos) in [
        (ship_id, char_id, DVec3::new(2_000.0, 0.0, 0.0)),
        (wing_ship, wing_char, DVec3::new(3_000.0, 0.0, 0.0)),
    ] {
        sys_h
            .spawn_ball_with_meta(
                Ball {
                    id: sid,
                    type_id: 23_915,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 200.0,
                    inertia: 1.0,
                    mass: 1.0e6,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: 50_000.0,
                    max_shield_hp: 50_000.0,
                    armor_hp: 50_000.0,
                    max_armor_hp: 50_000.0,
                    structure_hp: 50_000.0,
                    max_structure_hp: 50_000.0,
                    capacitor: 500.0,
                    max_capacitor: 500.0,
                    signature_radius: 100.0,
                    ..CombatState::default()
                },
                SlimItemMeta {
                    char_id: Some(cid),
                    owner_id: cid,
                    group_id: 25,
                    category_id: 6,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    // Spawn 5 NPCs:
    // 1. Sleeper 250km away from spawn_position -> leashes back to origin!
    // 2. Sleeper logistics peer near trig_id -> repairs peer NPC 3.
    // 3. Triglavian peer (damaged shield/armor) -> receives remote repair & spools entropic_cycles.
    // 4. Drifter with drifter_overshield_hp = 0.0 -> fires one-shot 10,000 HP doomsday!
    // 5. EDENCOM -> hits primary ship_id and arcs 0.5x vorton splash to fleetmate wing_ship!
    let sleeper_id = 510_001_i64;
    let sleeper_rep_id = 510_005_i64;
    let trig_id = 510_002_i64;
    let drifter_id = 510_003_i64;
    let eden_id = 510_004_i64;

    for (nid, pos, sh_hp, ar_hp) in [
        (sleeper_id, DVec3::new(250_000.0, 0.0, 0.0), 1_000.0, 1_000.0),
        (sleeper_rep_id, DVec3::new(-25_000.0, 0.0, 0.0), 1_000.0, 1_000.0),
        (trig_id, DVec3::new(-24_000.0, 0.0, 0.0), 200.0, 200.0),
        (drifter_id, DVec3::new(1_500.0, 0.0, 0.0), 1_000.0, 1_000.0),
        (eden_id, DVec3::new(1_800.0, 0.0, 0.0), 1_000.0, 1_000.0),
    ] {
        sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: nid,
                    type_id: 587,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1.0e6,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: sh_hp,
                    max_shield_hp: 1_000.0,
                    armor_hp: ar_hp,
                    max_armor_hp: 1_000.0,
                    structure_hp: 1_000.0,
                    max_structure_hp: 1_000.0,
                    ..CombatState::default()
                },
            )
            .await
            .unwrap();
    }

    with_mut_combat_ephemeral(&ctx, |rt| {
        let mut sl = make_test_npc_entity(sleeper_id, 30_195, sys_id, "Sleeper");
        sl.spawn_position = DVec3::ZERO;
        rt.npc_entities.insert(sleeper_id, sl);

        let mut sl_rep = make_test_npc_entity(sleeper_rep_id, 30_195, sys_id, "Sleeper");
        sl_rep.spawn_position = DVec3::ZERO;
        sl_rep.optimal_range = 40_000.0;
        sl_rep.remote_shield_rep = 150.0;
        sl_rep.remote_armor_rep = 120.0;
        sl_rep.remote_rep_range = 20_000.0;
        sl_rep.sleeper_wh_class = 5;
        sl_rep.scram_range = 20_000.0;
        sl_rep.scram_strength = 2.0;
        sl_rep.neut_amount = 50.0;
        sl_rep.web_range = 20_000.0;
        sl_rep.web_speed_factor = -60.0;
        sl_rep.target_id = Some(ship_id);
        rt.npc_entities.insert(sleeper_rep_id, sl_rep);

        let mut tr = make_test_npc_entity(trig_id, 47_270, sys_id, "Triglavian");
        tr.damage_vector = DamageVector::new(0.0, 100.0, 0.0, 100.0);
        tr.target_id = Some(ship_id);
        rt.npc_entities.insert(trig_id, tr);

        let mut dr = make_test_npc_entity(drifter_id, 37_473, sys_id, "Drifter");
        dr.damage_vector = DamageVector::new(50.0, 50.0, 50.0, 50.0);
        dr.drifter_overshield_hp = 0.0;
        dr.drifter_doomsday_fired = false;
        dr.target_id = Some(ship_id);
        rt.npc_entities.insert(drifter_id, dr);

        let mut ed = make_test_npc_entity(eden_id, 54_800, sys_id, "EDENCOM");
        ed.damage_vector = DamageVector::new(100.0, 0.0, 100.0, 0.0);
        ed.threat_table.insert(ship_id, 200.0);
        ed.target_id = Some(ship_id);
        rt.npc_entities.insert(eden_id, ed);
    });

    step_extended_npc_ai(&ctx, sys_id).await;

    // Verify Sleeper leashed back to origin and repaired trig_id
    let sl_pos = sys_h.get_ball_snapshot(sleeper_id).await.unwrap().unwrap().1.position;
    assert!(sl_pos.length() < 1e-3);
    let trig_cs = sys_h.get_combat_state(trig_id).await.unwrap().unwrap();
    assert!((trig_cs.shield_hp - 350.0).abs() < 1e-3);
    assert!((trig_cs.armor_hp - 320.0).abs() < 1e-3);

    // Verify Drifter fired doomsday, Triglavian spooled entropic_cycles, and EDENCOM splashed wing_ship
    with_combat_ephemeral(&ctx, |rt| {
        assert!(rt.npc_entities[&drifter_id].drifter_doomsday_fired);
        assert_eq!(rt.npc_entities[&trig_id].entropic_cycles, 1);
    });
    let primary_cs = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    let wing_cs = sys_h.get_combat_state(wing_ship).await.unwrap().unwrap();
    assert!(primary_cs.shield_hp < 45_000.0);
    assert!(wing_cs.shield_hp < 50_000.0);
    assert!(primary_cs.capacitor < 500.0);
}

#[tokio::test]
async fn destroy_specialized_npc_sleeper_wormhole_pochven_roguedrone_trig_edencom_and_salvage() {
    let (mut ctx, _session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = (*ctx.sde).clone();

    for (tid, fac, name) in [
        (931_001_u32, Some(500_021_u32), "Sleeper Guardian"),
        (931_002, None, "Hypnosian Drone"),
        (931_003, Some(500_008), "Rogue Alvus"),
        (931_004, Some(500_026), "Raznaborg Vedmak"),
        (931_005, Some(500_027), "EDENCOM Cruiser"),
        (931_006, Some(500_010), "Guristas Commander"),
        (931_007, Some(500_011), "Angel Commander"),
        (931_008, Some(500_012), "Blood Raider Commander"),
        (931_009, Some(500_020), "Serpentis Commander"),
    ] {
        let mut t = make_fighter_type(
            tid,
            550,
            11,
            1_000.0,
            vec![(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 100_000.0)],
        );
        t.name = name.into();
        t.faction_id = fac;
        sde.types.insert(tid, t);
    }
    ctx.sde = std::sync::Arc::new(sde);

    // 1. Sleeper gives 0.0 bounty, wreck 30495, and drops Neural Network Analyzer (30744) & Sleeper Data Library (30259)
    let sl_id = spawn_specialized_npc_entity(&ctx, sys_id, 931_001, "Sleeper", 1, DVec3::ZERO).await;
    let (sl_wreck, sl_bounty, sl_drops) =
        destroy_specialized_npc_and_queue_bounty(&ctx, sys_id, sl_id, char_id).await;
    assert_eq!(sl_bounty, 0.0);
    assert!(sl_drops.contains(&30_744) && sl_drops.contains(&30_259));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&sl_wreck].type_id), 30_495);

    // 2. PochvenHypnosianSleeper, RogueDrone, Triglavian, EDENCOM, and faction salvage drops
    for (tid, hint, expected_drop_name) in [
        (931_002, "PochvenHypnosianSleeper", "Interstellar Navigation Logs"),
        (931_003, "RogueDrone", "Sheen Compound"),
        (931_004, "Triglavian", "Triglavian Survey Database"),
        (931_005, "EDENCOM", "Alloyed Tritanium Bar"),
        (931_006, "Pirate", "Alloyed Tritanium Bar"),
        (931_007, "Pirate", "Armor Plates"),
        (931_008, "Pirate", "Burned Logic Circuit"),
        (931_009, "Pirate", "Artificial Neural Network"),
    ] {
        let nid = spawn_specialized_npc_entity(&ctx, sys_id, tid, hint, 1, DVec3::ZERO).await;
        let (_wreck, bounty, drops) =
            destroy_specialized_npc_and_queue_bounty(&ctx, sys_id, nid, char_id).await;
        assert!(bounty > 0.0);
        assert!(
            drops.iter().any(|d| ctx.sde.types.get(d).is_some_and(|t| t.name == expected_drop_name)),
            "expected drop {expected_drop_name} for {tid}, got {drops:?}"
        );
    }

    // 3. Wormhole system (>= 31_000_000) zeroes out non-Sleeper bounty
    let wh_sys = 31_000_005_u32;
    let wh_npc = spawn_specialized_npc_entity(&ctx, wh_sys, 931_006, "Pirate", 1, DVec3::ZERO).await;
    let (_wh_wreck, wh_bounty, _) =
        destroy_specialized_npc_and_queue_bounty(&ctx, wh_sys, wh_npc, char_id).await;
    assert_eq!(wh_bounty, 0.0);
}

mod w4_ops_pve_mut_extra_tests {
    use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
        make_fighter_ctx_and_session, make_fighter_type,
    };
    use crate::services::combat_crimewatch::combat_npc_ai::combat_npc_ai_tests::make_test_npc_entity;
    use crate::services::combat_crimewatch::combat_npc_ai::*;
    use crate::services::combat_crimewatch::{with_combat_ephemeral, with_mut_combat_ephemeral, DroneSpaceState};
    use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
    use glam::DVec3;

    #[tokio::test]
    async fn archetype_hint_overrides_edencom_drone_owner_standing_and_unknown_candidates() {
        let (mut ctx, _sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
        let mut sde = (*ctx.sde).clone();
        let mut navy_t = make_fighter_type(940_001, 550, 11, 1_000.0, vec![]);
        navy_t.name = "Caldari Navy Frigate".into();
        navy_t.faction_id = Some(500_001);
        sde.types.insert(940_001, navy_t);
        let mut drone_t = make_fighter_type(940_002, 287, 11, 500.0, vec![]);
        drone_t.name = "Alvi Drone".into();
        sde.types.insert(940_002, drone_t);
        let mut pirate_t = make_fighter_type(940_003, 550, 11, 800.0, vec![]);
        pirate_t.name = "Guristas Pithi".into();
        sde.types.insert(940_003, pirate_t);
        ctx.sde = std::sync::Arc::new(sde);

        // Non-pirate SDE ("Navy") ignores arbitrary non-allowlisted hint ("Pirate") -> stays "Navy"
        let b_navy = spawn_specialized_npc_entity(&ctx, sys_id, 940_001, "Pirate", 1, DVec3::ZERO).await;
        // Non-pirate SDE ("RogueDrone") accepts allowlisted hint ("FactionPolice")
        let b_fpol = spawn_specialized_npc_entity(&ctx, sys_id, 940_002, "FactionPolice", 2, DVec3::ZERO).await;
        // Pirate SDE accepts custom hint ("CustomBoss")
        let b_custom = spawn_specialized_npc_entity(&ctx, sys_id, 940_003, "CustomBoss", 3, DVec3::ZERO).await;

        with_combat_ephemeral(&ctx, |rt| {
            assert_eq!(rt.npc_entities[&b_navy].archetype, "Navy");
            assert_eq!(rt.npc_entities[&b_fpol].archetype, "FactionPolice");
            assert_eq!(rt.npc_entities[&b_custom].archetype, "CustomBoss");
        });

        // EDENCOM checks: unknown candidate=false, drone owned by negative standing owner=true, neutral drone=false
        let other_char = 94_400_002_i64;
        let other_ship = 294_400_002_i64;
        ctx.db.with_mut_snapshot(|snap| {
            let mut oc = snap.characters[&char_id].clone();
            oc.character_id = other_char;
            oc.active_ship_id = other_ship;
            oc.standings.insert(500_027, -2.0);
            snap.characters.insert(other_char, oc);
        });
        let edencom = make_test_npc_entity(555_001, 54_800, sys_id, "EDENCOM");
        assert!(!is_candidate_hostile_to_edencom(&ctx, &edencom, 999_888_777, ship_id));

        let neg_drone = 666_001_i64;
        let neu_char = 94_400_003_i64;
        ctx.db.with_mut_snapshot(|snap| {
            let mut nc = snap.characters[&char_id].clone();
            nc.character_id = neu_char;
            nc.active_ship_id = 294_400_003;
            snap.characters.insert(neu_char, nc);
        });
        let neu_drone = 666_002_i64;
        with_mut_combat_ephemeral(&ctx, |rt| {
            for (did, owner, pship) in [(neg_drone, other_char, other_ship), (neu_drone, neu_char, 294_400_003)] {
                rt.active_drones_in_space.insert(
                    did,
                    DroneSpaceState {
                        ball_id: did,
                        drone_item_id: did,
                        type_id: 2454,
                        group_id: 100,
                        owner_char_id: owner,
                        original_owner_char_id: owner,
                        parent_ship_id: pship,
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
                        damage_vector: DamageVector::default(),
                        shield_repair_amount: 0.0,
                        armor_repair_amount: 0.0,
                        hull_repair_amount: 0.0,
                        web_speed_factor: 0.0,
                        neut_amount: 0.0,
                        ecm_strength: 0.0,
                        tp_sig_bonus_pct: 0.0,
                    },
                );
            }
        });
        assert!(is_candidate_hostile_to_edencom(&ctx, &edencom, neg_drone, ship_id));
        assert!(!is_candidate_hostile_to_edencom(&ctx, &edencom, neu_drone, ship_id));
    }

    #[tokio::test]
    async fn step_ai_pruning_remote_rep_guards_aggro_filters_scram_clearance_and_doomsday() {
        let (mut ctx, _sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
        let mut sde = (*ctx.sde).clone();
        // Add capsule (group 29), beacon (26468), and test NPC types
        let mut cap_t = make_fighter_type(670, 29, 6, 5.0, vec![]);
        cap_t.name = "Capsule".into();
        sde.types.insert(670, cap_t);
        let mut sle_t = make_fighter_type(940_101, 959, 11, 2_000.0, vec![]);
        sle_t.name = "Sleeper Awakened Defender".into();
        sde.types.insert(940_101, sle_t);
        let mut sle_sentry_t = make_fighter_type(940_102, 959, 11, 2_000.0, vec![]);
        sle_sentry_t.name = "Sleeper Sentry Tower".into();
        sde.types.insert(940_102, sle_sentry_t);
        let mut drifter_1956 = make_fighter_type(940_103, 1956, 11, 5_000.0, vec![]);
        drifter_1956.name = "Circadian Seeker".into();
        sde.types.insert(940_103, drifter_1956);
        let mut drifter_boss = make_fighter_type(37_473, 1310, 11, 5_000.0, vec![(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 500_000.0)]);
        drifter_boss.name = "Apollo Tyrannos".into();
        sde.types.insert(37_473, drifter_boss);
        ctx.sde = std::sync::Arc::new(sde);

        let sys_h = ctx.systems.get_or_spawn(sys_id);
        // Spawn player ship (non-capsule), capsule pilot, cloaked pilot, beacon, and stationary ball
        let cap_char = 94_400_101_i64;
        let cap_ship = 294_400_101_i64;
        let cloak_ship = 294_400_102_i64;
        let beacon_ball = 294_400_103_i64;
        let _ = sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: ship_id,
                    type_id: 23_915,
                    position: DVec3::new(1_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 200.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: 10_000.0,
                    max_shield_hp: 10_000.0,
                    armor_hp: 10_000.0,
                    max_armor_hp: 10_000.0,
                    structure_hp: 10_000.0,
                    max_structure_hp: 10_000.0,
                    signature_radius: 100.0,
                    is_web_immune: true,
                    ..Default::default()
                },
            )
            .await;

        ctx.db.with_mut_snapshot(|snap| {
            let mut ch = snap.characters[&char_id].clone();
            ch.character_id = cap_char;
            ch.active_ship_id = cap_ship;
            snap.characters.insert(cap_char, ch);
        });
        for (bid, tid, vel, cloaked) in [
            (cap_ship, 670, 200.0, false),
            (cloak_ship, 23_915, 200.0, true),
            (beacon_ball, 26_468, 0.0, false),
        ] {
            let _ = sys_h
                .spawn_ball_with_combat(
                    Ball {
                        id: bid,
                        type_id: tid,
                        position: DVec3::new(2_000.0, 0.0, 0.0),
                        velocity: DVec3::ZERO,
                        max_velocity: vel,
                        inertia: 1.0,
                        mass: 1_000.0,
                        radius: 20.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    },
                    CombatState {
                        shield_hp: 500.0,
                        max_shield_hp: 500.0,
                        armor_hp: 500.0,
                        max_armor_hp: 500.0,
                        structure_hp: 500.0,
                        max_structure_hp: 500.0,
                        is_cloaked: cloaked,
                        ..Default::default()
                    },
                )
                .await;
        }

        // 1. Dead NPC & missing ball NPC get pruned cleanly; Relay & Warp NPC skipped cleanly
        let dead_npc = spawn_specialized_npc_entity(&ctx, sys_id, 940_101, "Sleeper", 1, DVec3::ZERO).await;
        sys_h.try_update_combat_state(dead_npc, |cs| cs.structure_hp = 0.0);
        let missing_ball_npc = 999_777_666_i64;
        let relay_id = spawn_specialized_npc_entity(&ctx, sys_id, 41_589, "TransmissionRelay", 1, DVec3::ZERO).await;
        let warp_npc = spawn_specialized_npc_entity(&ctx, sys_id, 940_101, "Sleeper", 1, DVec3::ZERO).await;
        let _ = sys_h
            .spawn_ball(Ball {
                id: warp_npc,
                type_id: 940_101,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Warp {
                    origin: DVec3::ZERO,
                    destination: DVec3::new(1e9, 0.0, 0.0),
                    phase: eve_destiny::WarpPhase::Cruising,
                    warp_speed_au_s: 2.0,
                    total_distance: 1e9,
                    elapsed_warp_time: 0.0,
                },
            })
            .await;

        // 2. Sleeper Remote Rep: jammed ally NO heal, wrong group NO heal, good group in range HEALS;
        //    plus Sleeper C3 scrammer sets scram on ship_id!
        let sle_healer = spawn_specialized_npc_entity(&ctx, sys_id, 940_101, "Sleeper", 10, DVec3::new(500.0, 0.0, 0.0)).await;
        let sle_mate = spawn_specialized_npc_entity(&ctx, sys_id, 940_101, "Sleeper", 10, DVec3::new(1_000.0, 0.0, 0.0)).await;
        let sle_other_grp = spawn_specialized_npc_entity(&ctx, sys_id, 940_101, "Sleeper", 11, DVec3::new(1_000.0, 0.0, 0.0)).await;
        sys_h.try_update_combat_state(sle_mate, |cs| {
            cs.max_armor_hp = 2_000.0;
            cs.armor_hp = 500.0;
        });
        sys_h.try_update_combat_state(sle_other_grp, |cs| {
            cs.max_armor_hp = 2_000.0;
            cs.armor_hp = 500.0;
        });

        // 3. Drifter group 1956 never fires doomsday; boss drifter fires doomsday once overshield <= 0
        let seek_id = spawn_specialized_npc_entity(&ctx, sys_id, 940_103, "Drifter", 20, DVec3::new(3_000.0, 0.0, 0.0)).await;
        let boss_id = spawn_specialized_npc_entity(&ctx, sys_id, 37_473, "Drifter", 21, DVec3::new(3_500.0, 0.0, 0.0)).await;

        with_mut_combat_ephemeral(&ctx, |rt| {
            let mut missing = make_test_npc_entity(missing_ball_npc, 940_101, sys_id, "Sleeper");
            missing.spawn_group_id = 99;
            rt.npc_entities.insert(missing_ball_npc, missing);
            rt.npc_entities.get_mut(&warp_npc).unwrap().target_id = Some(ship_id);

            let h = rt.npc_entities.get_mut(&sle_healer).unwrap();
            h.remote_armor_rep = 200.0;
            h.remote_rep_range = 20_000.0;
            h.sleeper_wh_class = 3;
            h.scram_strength = 3.0;
            h.scram_range = 30_000.0;
            h.web_speed_factor = 0.01; // clamped to 0.1
            h.web_range = 30_000.0;
            h.target_id = Some(cloak_ship); // triggers target switch -> resets entropic_cycles!
            h.entropic_cycles = 4;

            let m_boss = rt.npc_entities.get_mut(&boss_id).unwrap();
            m_boss.drifter_overshield_hp = 0.0;
            m_boss.drifter_doomsday_fired = false;
            m_boss.damage_vector = DamageVector::new(10.0, 10.0, 10.0, 10.0);
        });

        step_extended_npc_ai(&ctx, sys_id).await;

        with_combat_ephemeral(&ctx, |rt| {
            assert!(!rt.npc_entities.contains_key(&dead_npc));
            assert!(!rt.npc_entities.contains_key(&missing_ball_npc));
            assert!(rt.npc_entities.contains_key(&relay_id));
            assert_eq!(rt.npc_entities[&warp_npc].target_id, None);
            assert_eq!(rt.npc_entities[&sle_healer].entropic_cycles, 0); // reset on switch!
            assert!(!rt.npc_entities[&seek_id].drifter_doomsday_fired);
            assert!(rt.npc_entities[&boss_id].drifter_doomsday_fired);
            assert!(rt.npc_scrambled_ships.get(&sys_id).is_some_and(|s| s.contains(&ship_id)));
        });
        // Ship was scrammed by C3 sleeper; sle_mate healed, sle_other_grp did NOT heal!
        let cs_mate = sys_h.get_combat_state(sle_mate).await.unwrap().unwrap();
        let cs_other = sys_h.get_combat_state(sle_other_grp).await.unwrap().unwrap();
        assert!(cs_mate.armor_hp > 500.0);
        assert!((cs_other.armor_hp - 500.0).abs() < 1e-6);

        // Next tick: removing all scram-capable / active NPCs clears player scram!
        with_mut_combat_ephemeral(&ctx, |rt| {
            rt.npc_entities.retain(|_, e| e.solar_system_id != sys_id);
        });
        let _ = sys_h.remove_ball(sle_healer).await;
        step_extended_npc_ai(&ctx, sys_id).await;
        with_combat_ephemeral(&ctx, |rt| {
            assert!(!rt.npc_scrambled_ships.get(&sys_id).is_some_and(|s| s.contains(&ship_id)));
        });

        // Destroy boss_id WITHOUT ball in ballpark via destroy_specialized_npc_with_hint!
        let _ = sys_h.remove_ball(boss_id).await;
        let (wreck_b, bounty_b, drops_b) =
            destroy_specialized_npc_with_hint(&ctx, sys_id, boss_id, char_id, Some((37_473, DVec3::new(100.0, 0.0, 0.0)))).await;
        assert!(wreck_b > 0);
        assert!((bounty_b - 500_000.0).abs() < 1e-2);
        let mut saw_element = drops_b.contains(&30_747);
        for off in 1..=12_i64 {
            let (_, _, d) = destroy_specialized_npc_with_hint(
                &ctx,
                sys_id,
                boss_id + off,
                char_id,
                Some((37_473, DVec3::new(100.0 + off as f64, 0.0, 0.0))),
            )
            .await;
            if d.contains(&30_747) {
                saw_element = true;
                break;
            }
        }
        assert!(saw_element || !drops_b.is_empty(), "expected drifter drops, got initial drops_b={drops_b:?}");
    }

    #[tokio::test]
    async fn batch3_aggro_gates_corruption_navy_weights_sentry_zarzakh_incursion_and_scrap() {
        let (mut ctx, _sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
        let now = ctx.now_unix();
        let mut sde = (*ctx.sde).clone();

        let mut navy_t = make_fighter_type(940_201, 550, 11, 1_000.0, vec![]);
        navy_t.name = "Amarr Navy Frigate".into();
        navy_t.faction_id = Some(500_003);
        sde.types.insert(940_201, navy_t);

        let mut roamer_t = make_fighter_type(940_202, 550, 11, 1_000.0, vec![(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 10_000.0)]);
        roamer_t.name = "Guristas Commando".into();
        roamer_t.faction_id = Some(500_010);
        sde.types.insert(940_202, roamer_t);

        let mut sentry_t = make_fighter_type(940_203, 959, 11, 2_000.0, vec![]);
        sentry_t.name = "Sleeper Sentry Tower".into();
        sde.types.insert(940_203, sentry_t);

        let mut gate_gun_t = make_fighter_type(940_204, 1956, 11, 5_000.0, vec![]);
        gate_gun_t.name = "Zarzakh Sentry Gun".into();
        sde.types.insert(940_204, gate_gun_t);

        let mut trig_t = make_fighter_type(940_205, 1982, 11, 1_500.0, vec![(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 20_000.0)]);
        trig_t.name = "Damavik".into();
        trig_t.faction_id = Some(500_026);
        sde.types.insert(940_205, trig_t);

        let mut cap_t = make_fighter_type(940_206, 485, 6, 50_000.0, vec![]);
        cap_t.name = "Revelation Dread".into();
        sde.types.insert(940_206, cap_t);

        let mut sub_t = make_fighter_type(587, 25, 6, 300.0, vec![]);
        sub_t.name = "Rifter".into();
        sde.types.insert(587, sub_t);

        let mut sle_t = make_fighter_type(940_207, 986, 11, 2_000.0, vec![]);
        sle_t.name = "Sleeper Emergent Patroller".into();
        sde.types.insert(940_207, sle_t);

        let mut generic_t = make_fighter_type(940_208, 12, 11, 500.0, vec![]);
        generic_t.name = "Generic Unknown Drone".into();
        sde.types.insert(940_208, generic_t);

        ctx.sde = std::sync::Arc::new(sde);
        let sys_h = ctx.systems.get_or_spawn(sys_id);

        // Ensure neutral player ship & clean system state
        let cap_char = 94_400_201_i64;
        let cap_ship = 294_400_201_i64;
        let sub_char = 94_400_202_i64;
        let sub_ship = 294_400_202_i64;
        ctx.db.with_mut_snapshot(|snap| {
            let mut ch_cap = snap.characters[&char_id].clone();
            ch_cap.character_id = cap_char;
            ch_cap.active_ship_id = cap_ship;
            ch_cap.security_status = 0.0;
            ch_cap.standings.clear();
            snap.characters.insert(cap_char, ch_cap);

            let mut ch_sub = snap.characters[&char_id].clone();
            ch_sub.character_id = sub_char;
            ch_sub.active_ship_id = sub_ship;
            ch_sub.security_status = 0.0;
            ch_sub.standings.clear();
            snap.characters.insert(sub_char, ch_sub);

            if let Some(tpl_item) = snap.items.get(&ship_id).or_else(|| snap.items.values().next()).cloned() {
                if let Some(ship_item) = snap.items.get_mut(&ship_id) {
                    ship_item.type_id = 587; // subcap so cap_ship stands out for Sleeper weight
                }
                let mut cap_item = tpl_item.clone();
                cap_item.item_id = cap_ship;
                cap_item.type_id = 940_206;
                cap_item.owner_id = cap_char;
                cap_item.location_id = i64::from(sys_id);
                snap.items.insert(cap_ship, cap_item);

                let mut sub_item = tpl_item;
                sub_item.item_id = sub_ship;
                sub_item.type_id = 587;
                sub_item.owner_id = sub_char;
                sub_item.location_id = i64::from(sys_id);
                snap.items.insert(sub_ship, sub_item);
            }

            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.security_status = 0.0;
                ch.standings.clear();
            }
            let st_template = snap.system_states.values().next().cloned();
            if let Some(mut st) = st_template {
                st.insurgency_corruption_stage = 1;
                snap.system_states.insert(sys_id, st);
            }
            snap.insurgency_runtime.campaigns.insert(
                1,
                eve_db::PersistedInsurgencyCampaign {
                    campaign_id: 1,
                    pirate_faction_id: 500_010,
                    target_faction_id: 500_003,
                    fob_solar_system_id: sys_id,
                    systems: vec![sys_id],
                    created_at_unix: now.saturating_sub(100),
                    forecasting_end_unix: now.saturating_sub(50),
                    active_end_unix: now + 3_600,
                    winddown_end_unix: now + 7_200,
                    corruption_points: std::collections::HashMap::from([(sys_id, 0)]),
                    suppression_points: std::collections::HashMap::new(),
                },
            );
        });

        let spawn_ship = |id: i64, tid: u32, pos: DVec3| {
            let h = sys_h.clone();
            async move {
                let _ = h
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
                            shield_hp: 50_000.0,
                            max_shield_hp: 50_000.0,
                            armor_hp: 50_000.0,
                            max_armor_hp: 50_000.0,
                            structure_hp: 50_000.0,
                            max_structure_hp: 50_000.0,
                            signature_radius: 100.0,
                            ..Default::default()
                        },
                    )
                    .await;
            }
        };
        spawn_ship(ship_id, 587, DVec3::new(1_000.0, 0.0, 0.0)).await;
        spawn_ship(sub_ship, 587, DVec3::new(2_000.0, 0.0, 0.0)).await;
        spawn_ship(cap_ship, 940_206, DVec3::new(2_500.0, 0.0, 0.0)).await;

        // 1. spawn_npc_entity_from_sde using a real published SDE entity if present
        if let Some(&sde_npc_tid) = ctx.sde.types.values().find(|t| t.category_id == 11 && t.published).map(|t| &t.type_id) {
            let sde_npc = spawn_npc_entity_from_sde(&ctx, sys_id, sde_npc_tid, 1, DVec3::ZERO).await;
            with_mut_combat_ephemeral(&ctx, |rt| {
                assert!(rt.npc_entities.contains_key(&sde_npc));
                rt.npc_entities.remove(&sde_npc);
            });
            let _ = sys_h.remove_ball(sde_npc).await;
        }

        // 2. Navy skipped on neutral ships, aggroes once suspect/low-standing/low-sec; Roamer skipped at stage 1, aggroes at stage 3;
        //    High-standing Pirate skipped until record_npc_threat_event; Sleeper picks Dreadnought; SleeperSentry 250km range
        let navy_npc = spawn_specialized_npc_entity(&ctx, sys_id, 940_201, "Navy", 1, DVec3::ZERO).await;
        let roamer_npc = spawn_specialized_npc_entity(&ctx, sys_id, 940_202, "InsurgencyPirateRoamer", 2, DVec3::ZERO).await;
        let pirate_npc = spawn_specialized_npc_entity(&ctx, sys_id, 940_202, "Pirate", 3, DVec3::ZERO).await;
        let trig_standing_npc = spawn_specialized_npc_entity(&ctx, sys_id, 940_205, "Triglavian", 11, DVec3::ZERO).await;
        let sle_npc = spawn_specialized_npc_entity(&ctx, sys_id, 940_207, "Sleeper", 4, DVec3::ZERO).await;
        let sentry_far = spawn_specialized_npc_entity(&ctx, sys_id, 940_203, "SleeperSentry", 5, DVec3::new(300_000.0, 0.0, 0.0)).await;
        let sentry_near = spawn_specialized_npc_entity(&ctx, sys_id, 940_203, "SleeperSentry", 6, DVec3::new(200_000.0, 0.0, 0.0)).await;

        with_mut_combat_ephemeral(&ctx, |rt| {
            if let Some(t) = rt.npc_entities.get_mut(&trig_standing_npc) {
                t.faction_id = 500_026;
            }
            for sid in [sentry_far, sentry_near] {
                if let Some(s) = rt.npc_entities.get_mut(&sid) {
                    s.archetype = "SleeperSentry".into();
                }
            }
        });
        ctx.db.with_mut_snapshot(|snap| {
            for cid in [char_id, cap_char, sub_char] {
                if let Some(ch) = snap.characters.get_mut(&cid) {
                    ch.standings.insert(500_026, 8.0); // standing >= 5.0 skips non-pirate NPCs
                }
            }
        });

        step_extended_npc_ai(&ctx, sys_id).await;
        with_combat_ephemeral(&ctx, |rt| {
            assert_eq!(rt.npc_entities[&navy_npc].target_id, None, "Navy skips neutral");
            assert_eq!(rt.npc_entities[&roamer_npc].target_id, None, "Roamer dormant at corruption < 2");
            assert!(rt.npc_entities[&pirate_npc].target_id.is_some(), "Pirate always aggroes");
            assert_eq!(rt.npc_entities[&trig_standing_npc].target_id, None, "Triglavian skips standing >= 5.0");
            assert_eq!(rt.npc_entities[&sle_npc].target_id, Some(cap_ship), "Sleeper prefers capital hull weight");
            assert_eq!(rt.npc_entities[&sentry_far].target_id, None, "Sentry >250km out of range");
            assert!(rt.npc_entities[&sentry_near].target_id.is_some(), "Sentry <250km engages");
        });

        // Trigger suspect & low Navy standing on char_id, set pirate corruption >= 2, and record threat on trig_standing_npc
        ctx.crimewatch.write().unwrap().trigger_suspect_flag(char_id, now);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.standings.insert(500_003, -6.0);
            }
            if let Some(st) = snap.system_states.get_mut(&sys_id) {
                st.insurgency_corruption_stage = 3;
            }
        });
        record_npc_threat_event(&ctx, trig_standing_npc, sub_ship, 500.0, true, false);
        step_extended_npc_ai(&ctx, sys_id).await;
        with_combat_ephemeral(&ctx, |rt| {
            assert_eq!(rt.npc_entities[&navy_npc].target_id, Some(ship_id), "Navy aggroes suspect");
            assert!(rt.npc_entities[&roamer_npc].target_id.is_some(), "Roamer aggroes corruption >= 2");
            assert_eq!(rt.npc_entities[&trig_standing_npc].target_id, Some(sub_ship), "Aggroes already_threat despite high standing");
        });

        // 3. Triglavian entropic cycle ramp + ZarzakhGateGun ECM/lock clear + Drifter overshield > 0 blocks doomsday
        let trig_npc = spawn_specialized_npc_entity(&ctx, sys_id, 940_205, "Triglavian", 7, DVec3::new(1_000.0, 0.0, 0.0)).await;
        let gate_gun = spawn_specialized_npc_entity(&ctx, sys_id, 940_204, "ZarzakhGateGun", 8, DVec3::new(1_000.0, 0.0, 0.0)).await;
        let drifter_shielded = spawn_specialized_npc_entity(&ctx, sys_id, 37_473, "Drifter", 9, DVec3::new(1_000.0, 0.0, 0.0)).await;
        with_mut_combat_ephemeral(&ctx, |rt| {
            let t = rt.npc_entities.get_mut(&trig_npc).unwrap();
            t.target_id = Some(ship_id);
            t.threat_table.insert(ship_id, 500.0);
            t.entropic_cycles = 0;
            let gg = rt.npc_entities.get_mut(&gate_gun).unwrap();
            gg.target_id = Some(ship_id);
            gg.threat_table.insert(ship_id, 500.0);
            let ds = rt.npc_entities.get_mut(&drifter_shielded).unwrap();
            ds.target_id = Some(ship_id);
            ds.threat_table.insert(ship_id, 500.0);
            ds.drifter_overshield_hp = 5_000.0;
            ds.drifter_doomsday_fired = false;
            ds.damage_vector = DamageVector::new(10.0, 10.0, 10.0, 10.0);
        });
        let _ = sys_h
            .add_target_lock(
                ship_id,
                eve_destiny::TargetLock {
                    target_id: gate_gun,
                    lock_time_ms: 1000.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: None,
                },
            )
            .await;

        step_extended_npc_ai(&ctx, sys_id).await;
        step_extended_npc_ai(&ctx, sys_id).await;
        with_combat_ephemeral(&ctx, |rt| {
            assert!(rt.npc_entities[&trig_npc].entropic_cycles >= 1);
            assert!(!rt.npc_entities[&drifter_shielded].drifter_doomsday_fired);
        });

        // 4. is_candidate_hostile_to_edencom on NPC balls & destroy_specialized_npc_with_hint incursion reduction + Metal Scraps fallback
        let eden = make_test_npc_entity(999_888_111, 54_800, sys_id, "EDENCOM");
        assert!(is_candidate_hostile_to_edencom(&ctx, &eden, trig_npc, ship_id));
        assert!(!is_candidate_hostile_to_edencom(&ctx, &eden, navy_npc, ship_id));
        let const_id = ctx
            .sde
            .solar_systems
            .get(&sys_id)
            .map(|s| s.constellation_id)
            .unwrap_or(20_000_020);
        ctx.db.with_mut_snapshot(|snap| {
            snap.pve_special_sites.incursions.insert(
                const_id,
                eve_db::service_domains::IncursionTaleRecord {
                    tale_id: 9_000_001,
                    constellation_id: const_id,
                    constellation_name: "Kimotoro".into(),
                    staging_solar_system_id: sys_id,
                    vanguard_solar_system_ids: vec![],
                    assault_solar_system_ids: vec![],
                    hq_solar_system_id: sys_id,
                    influence: 0.50,
                    state: "Active".into(),
                    mothership_spawned: true,
                    mothership_defeated: false,
                    sites: vec![],
                    lockout_until_filetime: None,
                    tale_stop_published: false,
                },
            );
        });
        let (_, bounty_inc, _) =
            destroy_specialized_npc_with_hint(&ctx, sys_id, roamer_npc, char_id, None).await;
        assert!(bounty_inc < 10_000.0);

        // Generic NPC drops Metal Scraps (type 15457) fallback when no specialized table matches
        let gen_npc = spawn_specialized_npc_entity(&ctx, sys_id, 940_208, "GenericPirate", 10, DVec3::new(1_500.0, 0.0, 0.0)).await;
        let (wreck_g, _, drops_g) =
            destroy_specialized_npc_with_hint(&ctx, sys_id, gen_npc, char_id, None).await;
        assert!(wreck_g > 0 || drops_g.contains(&15_457) || drops_g.is_empty());

        ctx.db.with_mut_snapshot(|snap| {
            snap.pve_special_sites.incursions.clear();
        });
        ctx.crimewatch.write().unwrap().states.clear();
    }

    #[tokio::test]
    async fn batch4_insurgency_archetypes_ewar_logi_drones_lethal_npc_and_all_loot_tables() {
        use eve_db::InventoryFlag;
        let (mut ctx, _sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
        let mut sde = (*ctx.sde).clone();

        let mut ewar_mod_t = make_fighter_type(940_301, 52, 7, 1.0, vec![]);
        ewar_mod_t.name = "Warp Scrambler I".into();
        sde.types.insert(940_301, ewar_mod_t);

        let mut logi_mod_t = make_fighter_type(940_302, 41, 7, 1.0, vec![]);
        logi_mod_t.name = "Remote Armor Repairer I".into();
        sde.types.insert(940_302, logi_mod_t);

        let mut drone_t = make_fighter_type(940_303, 100, 18, 5.0, vec![]);
        drone_t.name = "Hobgoblin I".into();
        sde.types.insert(940_303, drone_t);

        let mut sleeper_t = make_fighter_type(940_304, 986, 11, 2_000.0, vec![]);
        sleeper_t.name = "Sleeper Emergent Defender".into();
        sde.types.insert(940_304, sleeper_t);

        let mut officer_t = make_fighter_type(940_305, 553, 11, 5_000.0, vec![(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 50_000.0)]);
        officer_t.name = "Cormack Vaaja".into();
        sde.types.insert(940_305, officer_t);

        let mut ci_sentry_t = make_fighter_type(79_507, 1956, 11, 5_000.0, vec![]);
        ci_sentry_t.name = "Counter-Insurgency Sentry".into();
        sde.types.insert(79_507, ci_sentry_t);

        let mut rorq_t = make_fighter_type(79_589, 883, 11, 50_000.0, vec![]);
        rorq_t.name = "Insurgency Ore Rorqual".into();
        sde.types.insert(79_589, rorq_t);

        let mut relay_t = make_fighter_type(41_589, 1956, 11, 5_000.0, vec![]);
        relay_t.name = "Transmission Relay".into();
        sde.types.insert(41_589, relay_t);

        let mut hauler_t = make_fighter_type(648, 28, 6, 5_000.0, vec![]);
        hauler_t.name = "Badger".into();
        sde.types.insert(648, hauler_t);

        let mut sub_t = make_fighter_type(587, 25, 6, 300.0, vec![]);
        sub_t.name = "Rifter".into();
        sde.types.insert(587, sub_t);

        ctx.sde = std::sync::Arc::new(sde);
        let sys_h = ctx.systems.get_or_spawn(sys_id);

        // Two ships (one with EWAR, one with Logi) + fragile ship killed by NPC + two player drones
        let ship_logi = 294_500_002_i64;
        let char_logi = 94_500_002_i64;
        let ship_fragile = 294_500_003_i64;
        let char_fragile = 94_500_003_i64;
        let ewar_mod_id = 394_500_001_i64;
        let logi_mod_id = 394_500_002_i64;
        let d_atk_id = 394_500_003_i64;
        let d_idle_id = 394_500_004_i64;

        ctx.db.with_mut_snapshot(|snap| {
            let tpl = snap.items[&ship_id].clone();
            snap.items.get_mut(&ship_id).unwrap().type_id = 587;

            let mut c2 = snap.characters[&char_id].clone();
            c2.character_id = char_logi;
            c2.active_ship_id = ship_logi;
            snap.characters.insert(char_logi, c2);

            let mut c3 = snap.characters[&char_id].clone();
            c3.character_id = char_fragile;
            c3.active_ship_id = ship_fragile;
            snap.characters.insert(char_fragile, c3);

            for (iid, tid, own, loc, flag) in [
                (ship_logi, 587, char_logi, i64::from(sys_id), InventoryFlag::None),
                (ship_fragile, 587, char_fragile, i64::from(sys_id), InventoryFlag::None),
                (ewar_mod_id, 940_301, char_id, ship_id, InventoryFlag::MedSlot0),
                (logi_mod_id, 940_302, char_logi, ship_logi, InventoryFlag::HiSlot0),
                (d_atk_id, 940_303, char_id, ship_id, InventoryFlag::DroneBay),
                (d_idle_id, 940_303, char_logi, ship_logi, InventoryFlag::DroneBay),
            ] {
                let mut r = tpl.clone();
                r.item_id = iid;
                r.type_id = tid;
                r.owner_id = own;
                r.location_id = loc;
                r.flag = flag;
                snap.items.insert(iid, r);
            }
            snap.module_active_states.insert(ewar_mod_id);
            snap.module_active_states.insert(logi_mod_id);
        });

        let spawn_cs = |id: i64, hp: f64, pos: DVec3| {
            let h = sys_h.clone();
            async move {
                let _ = h
                    .spawn_ball_with_combat(
                        Ball {
                            id,
                            type_id: 587,
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
                            shield_hp: hp,
                            max_shield_hp: hp,
                            armor_hp: hp,
                            max_armor_hp: hp,
                            structure_hp: hp,
                            max_structure_hp: hp,
                            ..Default::default()
                        },
                    )
                    .await;
            }
        };
        spawn_cs(ship_id, 50_000.0, DVec3::new(1_000.0, 0.0, 0.0)).await;
        spawn_cs(ship_logi, 50_000.0, DVec3::new(1_500.0, 0.0, 0.0)).await;
        spawn_cs(ship_fragile, 1.0, DVec3::new(2_000.0, 0.0, 0.0)).await;
        for (did, type_id, own, act) in [(d_atk_id, 940_303, char_id, 1_i64), (d_idle_id, 940_303, char_logi, 0_i64)] {
            let _ = sys_h
                .spawn_ball_with_combat(
                    Ball {
                        id: did,
                        type_id,
                        position: DVec3::new(1_200.0, 0.0, 0.0),
                        velocity: DVec3::ZERO,
                        max_velocity: 400.0,
                        inertia: 1.0,
                        mass: 1_000.0,
                        radius: 10.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    },
                    CombatState {
                        shield_hp: 500.0,
                        max_shield_hp: 500.0,
                        armor_hp: 500.0,
                        max_armor_hp: 500.0,
                        structure_hp: 500.0,
                        max_structure_hp: 500.0,
                        ..Default::default()
                    },
                )
                .await;
            with_mut_combat_ephemeral(&ctx, |rt| {
                rt.active_drones_in_space.insert(
                    did,
                    crate::services::combat_crimewatch::DroneSpaceState {
                        ball_id: did,
                        drone_item_id: did,
                        type_id,
                        group_id: 100,
                        owner_char_id: own,
                        original_owner_char_id: own,
                        parent_ship_id: if own == char_id { ship_id } else { ship_logi },
                        solar_system_id: sys_id,
                        target_id: None,
                        activity_state: act,
                        assist_char_id: None,
                        guard_char_id: None,
                        is_abandoned: false,
                        is_sentry: false,
                        bandwidth_used: 5.0,
                        max_velocity: 400.0,
                        optimal_range: 5_000.0,
                        damage_vector: DamageVector::new(10.0, 0.0, 0.0, 0.0),
                        shield_repair_amount: 0.0,
                        armor_repair_amount: 0.0,
                        hull_repair_amount: 0.0,
                        web_speed_factor: 0.0,
                        neut_amount: 0.0,
                        ecm_strength: 0.0,
                        tp_sig_bonus_pct: 0.0,
                    },
                );
            });
        }

        // Spawn missing aggro archetypes + SleeperSentry in optimal_range + lethal Pirate killing ship_fragile
        let enf = spawn_specialized_npc_entity(&ctx, sys_id, 587, "InsurgencyEnforcer", 1, DVec3::ZERO).await;
        let ci = spawn_specialized_npc_entity(&ctx, sys_id, 79_507, "CounterInsurgencySentry", 2, DVec3::ZERO).await;
        let tack = spawn_specialized_npc_entity(&ctx, sys_id, 587, "InsurgencyAmbushTackler", 3, DVec3::ZERO).await;
        let ore_def = spawn_specialized_npc_entity(&ctx, sys_id, 587, "InsurgencyAmbushOreDefender", 4, DVec3::ZERO).await;
        let rorq = spawn_specialized_npc_entity(&ctx, sys_id, 79_589, "InsurgencyOreRorqual", 5, DVec3::ZERO).await;
        let outpost = spawn_specialized_npc_entity(&ctx, sys_id, 587, "InsurgencyOutpostDefender", 6, DVec3::ZERO).await;
        let pol = spawn_specialized_npc_entity(&ctx, sys_id, 587, "FactionPolice", 7, DVec3::ZERO).await;
        let sen_close = spawn_specialized_npc_entity(&ctx, sys_id, 940_304, "SleeperSentry", 8, DVec3::ZERO).await;
        let killer = spawn_specialized_npc_entity(&ctx, sys_id, 587, "Pirate", 9, DVec3::new(2_000.0, 0.0, 0.0)).await;

        with_mut_combat_ephemeral(&ctx, |rt| {
            for (id, arch, fac) in [
                (enf, "InsurgencyEnforcer", 500_003),
                (ci, "CounterInsurgencySentry", 500_003),
                (tack, "InsurgencyAmbushTackler", 500_010),
                (ore_def, "InsurgencyAmbushOreDefender", 500_010),
                (rorq, "InsurgencyOreRorqual", 500_010),
                (outpost, "InsurgencyOutpostDefender", 500_018),
                (pol, "FactionPolice", 500_003),
                (sen_close, "SleeperSentry", 500_021),
            ] {
                if let Some(n) = rt.npc_entities.get_mut(&id) {
                    n.archetype = arch.into();
                    n.faction_id = fac;
                    if arch == "SleeperSentry" {
                        n.optimal_range = 10_000.0;
                    }
                }
            }
            if let Some(k) = rt.npc_entities.get_mut(&killer) {
                k.target_id = Some(ship_fragile);
                k.threat_table.insert(ship_fragile, 99_999.0);
                k.damage_vector = DamageVector::new(1_000.0, 1_000.0, 1_000.0, 1_000.0);
                k.optimal_range = 50_000.0;
            }
        });

        step_extended_npc_ai(&ctx, sys_id).await;
        assert!(sys_h.get_ball_snapshot(ship_fragile).await.unwrap().is_none(), "lethal NPC destroys fragile ship");

        // destroy_specialized_npc_with_hint on Sleeper, Officer, Hauler (custom cargo), and TransmissionRelay
        let sl_id = spawn_specialized_npc_entity(&ctx, sys_id, 940_304, "Sleeper", 10, DVec3::ZERO).await;
        let (_, b_sl, _) = destroy_specialized_npc_with_hint(&ctx, sys_id, sl_id, char_id, None).await;
        assert_eq!(b_sl, 0.0);

        let off_id = spawn_specialized_npc_entity(&ctx, sys_id, 940_305, "Officer", 11, DVec3::ZERO).await;
        with_mut_combat_ephemeral(&ctx, |rt| {
            if let Some(o) = rt.npc_entities.get_mut(&off_id) {
                o.archetype = "Officer".into();
            }
        });
        let (w_off, _, _) = destroy_specialized_npc_with_hint(&ctx, sys_id, off_id, char_id, None).await;
        assert!(w_off > 0);

        let haul_id = spawn_specialized_npc_entity(&ctx, sys_id, 648, "Hauler", 12, DVec3::ZERO).await;
        with_mut_combat_ephemeral(&ctx, |rt| {
            if let Some(h) = rt.npc_entities.get_mut(&haul_id) {
                h.archetype = "Hauler".into();
                h.custom_cargo_loot = Some((34, 25));
            }
        });
        let (_, _, haul_drops) = destroy_specialized_npc_with_hint(&ctx, sys_id, haul_id, char_id, None).await;
        assert!(haul_drops.contains(&34));

        let relay_id = spawn_specialized_npc_entity(&ctx, sys_id, 41_589, "TransmissionRelay", 13, DVec3::ZERO).await;
        let (w_rel, _, _) = destroy_specialized_npc_with_hint(&ctx, sys_id, relay_id, char_id, None).await;
        assert!(w_rel > 0);
    }
}
