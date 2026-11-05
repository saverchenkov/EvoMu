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
