//! Additional edge-case and branch coverage tests for `combat_npc_ai`.

#![forbid(unsafe_code)]

use super::super::*;
use super::make_test_npc_entity;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_type,
};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode, SlimItemMeta};
use glam::DVec3;

#[tokio::test]
async fn sleeper_wh_class_scram_capital_damage_ecm_jammed_and_scram_cleanup() {
    let (mut ctx, _sess, char_id, ship_id, _sys) = make_fighter_ctx_and_session();
    let wh_sys = 31_000_005_u32;

    // Register capital dreadnought type (group 485) for 2.5x Sleeper capital multiplier
    let cap_tid = 940_001_u32;
    let mut cap_t = make_fighter_type(cap_tid, 485, 6, 2_000.0, vec![]);
    cap_t.name = "TestDread".into();
    std::sync::Arc::make_mut(&mut ctx.sde).types.insert(cap_tid, cap_t);

    let sys_h = ctx.systems.get_or_spawn(wh_sys);
    let subcap_id = 240_001_i64;
    let cap_id = ship_id;
    let subcap_cid = char_id + 1;

    for (bid, cid, tid, gid, pos) in [
        (subcap_id, subcap_cid, 587_u32, 25_u32, DVec3::new(1_000.0, 0.0, 0.0)),
        (cap_id, char_id, cap_tid, 485_u32, DVec3::new(1_500.0, 0.0, 0.0)),
    ] {
        sys_h
            .spawn_ball_with_meta(
                Ball {
                    id: bid,
                    type_id: tid,
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
                    shield_hp: 20_000.0,
                    max_shield_hp: 20_000.0,
                    armor_hp: 20_000.0,
                    max_armor_hp: 20_000.0,
                    structure_hp: 20_000.0,
                    max_structure_hp: 20_000.0,
                    capacitor: 500.0,
                    max_capacitor: 500.0,
                    signature_radius: 500.0,
                    ..CombatState::default()
                },
                SlimItemMeta {
                    char_id: Some(cid),
                    owner_id: cid,
                    group_id: gid,
                    category_id: 6,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    // Spawn:
    // 1. C2 Sleeper (sleeper_wh_class = 2 < 3) targeting subcap -> must NOT scram even with scram_strength > 0
    // 2. C5 Sleeper (sleeper_wh_class = 5 >= 3) targeting capital -> deals 2.5x damage + applies scram
    // 3. Jammed NPC (ecm_jammed = true) -> skips attacking
    let c2_sl = 540_001_i64;
    let c5_sl = 540_002_i64;
    let jammed_npc = 540_003_i64;
    let relay_npc = 540_004_i64;
    for (nid, pos) in [
        (c2_sl, DVec3::new(1_100.0, 0.0, 0.0)),
        (c5_sl, DVec3::new(1_600.0, 0.0, 0.0)),
        (jammed_npc, DVec3::new(1_200.0, 0.0, 0.0)),
        (relay_npc, DVec3::new(1_300.0, 0.0, 0.0)),
    ] {
        sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: nid,
                    type_id: 587,
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
                    shield_hp: 1_000.0,
                    max_shield_hp: 1_000.0,
                    ..CombatState::default()
                },
            )
            .await
            .unwrap();
    }

    with_mut_combat_ephemeral(&ctx, |rt| {
        let mut s2 = make_test_npc_entity(c2_sl, 30_195, wh_sys, "Sleeper");
        s2.sleeper_wh_class = 2;
        s2.scram_strength = 3.0;
        s2.scram_range = 20_000.0;
        s2.damage_vector = DamageVector::new(100.0, 0.0, 0.0, 0.0);
        s2.target_id = Some(subcap_id);
        rt.npc_entities.insert(c2_sl, s2);

        let mut s5 = make_test_npc_entity(c5_sl, 30_195, wh_sys, "Sleeper");
        s5.sleeper_wh_class = 5;
        s5.scram_strength = 3.0;
        s5.scram_range = 20_000.0;
        s5.damage_vector = DamageVector::new(100.0, 0.0, 0.0, 0.0);
        s5.target_id = Some(cap_id);
        rt.npc_entities.insert(c5_sl, s5);

        let mut j = make_test_npc_entity(jammed_npc, 30_195, wh_sys, "Pirate");
        j.ecm_jammed = true;
        j.damage_vector = DamageVector::new(5_000.0, 0.0, 0.0, 0.0);
        j.target_id = Some(subcap_id);
        rt.npc_entities.insert(jammed_npc, j);

        // Passive TransmissionRelay never picks a target or deals damage
        let rel = make_test_npc_entity(relay_npc, 41_589, wh_sys, "TransmissionRelay");
        rt.npc_entities.insert(relay_npc, rel);
    });

    step_extended_npc_ai(&ctx, wh_sys).await;

    let subcap_cs = sys_h.get_combat_state(subcap_id).await.unwrap().unwrap();
    let cap_cs = sys_h.get_combat_state(cap_id).await.unwrap().unwrap();
    // Subcap took only C2 sleeper damage (100), not jammed_npc (5_000), and is NOT warp scrambled because sleeper_wh_class < 3
    let subcap_dmg = 20_000.0 - subcap_cs.shield_hp;
    let cap_dmg = 20_000.0 - cap_cs.shield_hp;
    assert!(subcap_dmg < 1_000.0);
    assert!(!subcap_cs.is_warp_scrambled());
    // Capital took 2.5x damage from C5 sleeper AND was warp scrambled!
    assert!(cap_dmg > subcap_dmg * 1.5);
    assert!(cap_cs.is_warp_scrambled());

    // Step again after removing C5 sleeper entity -> scram cleanup clears is_warp_scrambled on cap_id
    with_mut_combat_ephemeral(&ctx, |rt| {
        rt.npc_entities.remove(&c5_sl);
    });
    step_extended_npc_ai(&ctx, wh_sys).await;
    let cap_cs_after = sys_h.get_combat_state(cap_id).await.unwrap().unwrap();
    assert!(!cap_cs_after.is_warp_scrambled());
}

#[tokio::test]
async fn destroy_npc_cache_fallback_custom_loot_tag_bounty_dedup_and_threat_noop() {
    let (mut ctx, _sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();

    let custom_npc_tid = 941_001_u32;
    let mut t = make_fighter_type(
        custom_npc_tid,
        550,
        11,
        1_000.0,
        vec![(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 400_000.0)],
    );
    t.name = "Mordu Chief".into();
    std::sync::Arc::make_mut(&mut ctx.sde).types.insert(custom_npc_tid, t);

    // 1. record_npc_threat_event on unknown NPC id is a clean no-op
    record_npc_threat_event(&ctx, 999_666_555, ship_id, 500.0, true, false);

    // 2. Cached destroyed NPC fallback (`destroyed_npc_cache`) + custom_cargo_loot + tag_drop_type_id
    let cached_ball = 542_001_i64;
    let mut cached = make_test_npc_entity(cached_ball, custom_npc_tid, sys_id, "MorduCommander");
    cached.bounty_isk = 400_000.0;
    cached.tag_drop_type_id = Some(35); // Pyerite tag stand-in
    cached.custom_cargo_loot = Some((36, 77)); // Mexallon
    with_mut_combat_ephemeral(&ctx, |rt| {
        rt.destroyed_npc_cache.insert(cached_ball, cached);
    });

    let (wreck_id, bounty1, drops) =
        destroy_specialized_npc_and_queue_bounty(&ctx, sys_id, cached_ball, char_id).await;
    assert!(wreck_id > 0);
    assert!((bounty1 - 400_000.0).abs() < 1e-2);
    assert!(drops.contains(&35));
    assert!(drops.contains(&36));
    ctx.db.with_snapshot(|snap| {
        let mex_qty: i32 = snap
            .items
            .values()
            .filter(|i| i.location_id == wreck_id && i.type_id == 36)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(mex_qty, 77);
    });

    // 3. Second kill call on same ball_id is blocked from double bounty credit (`bounty_credited_balls`)
    let (_wreck2, bounty2, _) =
        destroy_specialized_npc_and_queue_bounty(&ctx, sys_id, cached_ball, char_id).await;
    assert_eq!(bounty2, 0.0);

    // 4. Spawn NPC with missing SDE type ID -> safely inserts default fallback without panicking
    let missing_sde_npc =
        spawn_npc_entity_from_sde(&ctx, sys_id, 999_444_333, 2, DVec3::new(500.0, 0.0, 0.0)).await;
    assert!(missing_sde_npc > 0);
    with_combat_ephemeral(&ctx, |rt| {
        assert!(rt.npc_entities.contains_key(&missing_sde_npc));
    });

    // 5. Missing solar system step_extended_npc_ai returns cleanly
    step_extended_npc_ai(&ctx, 30_999_888).await;
}

#[tokio::test]
async fn insurgency_navy_police_aggression_and_edencom_unknown_candidate_edges() {
    let (mut ctx, _sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let sys_h = ctx.systems.get_or_spawn(sys_id);

    // Ensure non-capital frigate type exists
    std::sync::Arc::make_mut(&mut ctx.sde)
        .types
        .entry(587)
        .or_insert_with(|| make_fighter_type(587, 25, 6, 40.0, vec![]));

    sys_h
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1.0e6,
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
                ..CombatState::default()
            },
            SlimItemMeta {
                char_id: Some(char_id),
                owner_id: char_id,
                group_id: 25,
                category_id: 6,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // EDENCOM hostility on unknown candidate ball ID -> false
    let eden = make_test_npc_entity(550_001, 54_800, sys_id, "EDENCOM");
    assert!(!is_candidate_hostile_to_edencom(&ctx, &eden, 999_111_222, ship_id));

    // Spawn InsurgencyEnforcer (4_638), Navy, and Pirate NPCs near player ship and step AI
    for (nid, tid, arch, dmg) in [
        (551_001_i64, 910_007_u32, "InsurgencyEnforcer", 50.0),
        (551_002, 587, "Pirate", 75.0),
        (551_003, 587, "Navy", 40.0),
    ] {
        sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: nid,
                    type_id: tid,
                    position: DVec3::new(2_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 250.0,
                    inertia: 1.0,
                    mass: 1.0e6,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: 1_000.0,
                    max_shield_hp: 1_000.0,
                    ..CombatState::default()
                },
            )
            .await
            .unwrap();
        with_mut_combat_ephemeral(&ctx, |rt| {
            let mut ent = make_test_npc_entity(nid, tid, sys_id, arch);
            ent.faction_id = if arch == "Navy" { 500_001 } else { 500_010 };
            ent.damage_vector = DamageVector::new(dmg, 0.0, 0.0, 0.0);
            ent.optimal_range = 25_000.0;
            if arch == "Navy" {
                ent.threat_table.insert(ship_id, 50.0);
            }
            rt.npc_entities.insert(nid, ent);
        });
    }

    // Negative Navy faction standing makes Navy aggro player, and Pirate auto-aggros nearby ship
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .get_mut(&char_id)
            .unwrap()
            .standings
            .insert(500_001, -6.0);
    });

    step_extended_npc_ai(&ctx, sys_id).await;
    let post_cs = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(post_cs.shield_hp < 10_000.0);
    with_combat_ephemeral(&ctx, |rt| {
        assert_eq!(rt.npc_entities[&551_002].target_id, Some(ship_id));
        assert_eq!(rt.npc_entities[&551_003].target_id, Some(ship_id));
    });
}
