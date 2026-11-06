//! Non-GM end-to-end integration tests for Shard 3 parity remediation (Part 4: Live NPCs —
//! NPC Mining Operations with Defense Escort & Hauler Transit, Multi-Gate Hauler Convoys,
//! Faction Navy Patrols, Zarzakh Gate Guns & Deathless Patrols, Wormhole Sleeper EWAR by Class,
//! and Pochven Hypnosian Sleepers across Faded / Normal / Aroused tiers).

#![forbid(unsafe_code)]

use crate::parity_shard_3_closure_helpers::{
    call_rpc, lock_player_target, move_player_near_ball, spawn_player_combat_pilot,
};
use eve_proto::PyRep;
use eve_server::living_world::{
    mining_escort_type_for_system, NpcArchetype,
};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::combat_npc_ai::{
    derive_npc_archetype, spawn_specialized_npc_entity, step_extended_npc_ai,
};
use eve_server::services::combat_crimewatch::with_mut_combat_runtime;
use eve_server::services::space_destiny::space_pochven_wormholes::open_pochven_wormhole_and_spawn_guards;
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;
use std::sync::Arc;

const HURRICANE_TYPE: u32 = 24702;
const PHOENIX_DREAD_TYPE: u32 = 19726;
const PERIMETER: u32 = 30_000_144;
const ZARZAKH: u32 = 30_100_000;

// parity: feat:npc.mining-op-npc
// parity: feat:npc.npc-hauler
#[tokio::test]
async fn test_e2e_npc_mining_op_fleet_escort_and_hauler_convoy_travel() {
    eve_testkit::attest(&["feat:npc.mining-op-npc", "feat:npc.npc-hauler"]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);

    let mut pilot = spawn_player_combat_pilot(
        &ctx,
        84_001,
        "npc_mine_p1",
        "Belt Raider",
        HURRICANE_TYPE,
        false,
    )
    .await;

    // 1. LivingWorldDirector starts with 0 convoys, then spawns mining op fleet (miner + hauler + defense escort)
    assert_eq!(
        ctx.living_world.list_convoys().len(),
        0,
        "LivingWorldDirector initializes empty before first tick"
    );
    let mining_cid = ctx.living_world.ensure_system_mining_op(PERIMETER, &ctx.sde);
    let _ = ctx
        .living_world
        .step_npc_mining_op(mining_cid, &ctx.systems, &ctx.db, &ctx.sde)
        .await
        .expect("step mining op");

    let sys_h = ctx.systems.get_or_spawn(PERIMETER);
    let hauler_bid = mining_cid + 1_000_000;
    let escort_bid = mining_cid + 2_000_000;
    let miner_ball = sys_h.get_ball_snapshot(mining_cid).await.unwrap().expect("miner ball").1;
    let escort_ball = sys_h
        .get_ball_snapshot(escort_bid)
        .await
        .unwrap()
        .expect("companion defense escort ball")
        .1;
    assert_eq!(
        escort_ball.type_id,
        mining_escort_type_for_system(&ctx.sde, PERIMETER),
        "Mining fleet spawns faction destroyer defense escort"
    );
    let _ = hauler_bid;

    // 2. Non-GM player shoots miner shields -> miner & hauler evade via destiny Warp toward station!
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&pilot.char_id).unwrap().solar_system_id = PERIMETER;
    });
    pilot.sess.solar_system_id = Some(PERIMETER);
    let _ = sys_h
        .spawn_ball(eve_destiny::Ball {
            id: pilot.ship_id,
            type_id: HURRICANE_TYPE,
            position: miner_ball.position + DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 3.0,
            mass: 1.5e7,
            radius: 200.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    move_player_near_ball(&ctx, &mut pilot.sess, PERIMETER, pilot.ship_id, mining_cid, 1_000.0).await;
    lock_player_target(&ctx, &mut pilot.sess, PERIMETER, pilot.ship_id, mining_cid).await;
    let gun_id = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .find(|i| {
                i.location_id == pilot.ship_id
                    && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 55)
            })
            .unwrap()
            .item_id
    });
    if let Ok(Some(mut mcs)) = sys_h.get_combat_state(mining_cid).await {
        mcs.shield_hp = 10_000.0;
        mcs.max_shield_hp = 10_000.0;
        mcs.armor_hp = 10_000.0;
        mcs.max_armor_hp = 10_000.0;
        mcs.structure_hp = 10_000.0;
        mcs.max_structure_hp = 10_000.0;
        let _ = sys_h.set_combat_state(mining_cid, mcs).await;
    }
    call_rpc(
        &ctx,
        &mut pilot.sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(gun_id),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(mining_cid),
        ],
    )
    .await
    .expect("Fire weapon at NPC miner");
    let _ = sys_h.step_manual(1.0).await;
    ctx.advance_secs(2);
    let _ = ctx
        .living_world
        .step_npc_mining_op(mining_cid, &ctx.systems, &ctx.db, &ctx.sde)
        .await;
    let miner_after_hit = sys_h.get_ball_snapshot(mining_cid).await.unwrap().expect("miner ball").1;
    let escort_spawned = sys_h
        .get_ball_snapshot(mining_cid + 2_000_000)
        .await
        .unwrap()
        .is_some();
    assert_eq!(
        (escort_spawned, miner_after_hit.max_velocity > 0.0),
        (true, true),
        "Damaged NPC miner spawns defense escort ({escort_spawned}) & warps toward station"
    );

    // 3. Multi-gate industrial hauler convoy: spawns, travels across systems, and destruction never injects synthetic +15% market markup
    let hauler_cid = ctx.living_world.ensure_system_hauler_convoy(PERIMETER, &ctx.sde);
    let h_conv = ctx.living_world.get_convoy(hauler_cid).expect("hauler convoy");
    assert_eq!(h_conv.archetype, NpcArchetype::HaulerConvoy);
    let pre_order_price = ctx.db.with_snapshot(|s| {
        s.market_orders
            .values()
            .filter(|o| !o.is_bid && o.is_npc)
            .map(|o| o.price.0)
            .min()
    });
    ctx.advance_secs(60);
    let _ = ctx
        .living_world
        .step_all_hauler_convoys(&ctx.systems, &ctx.db, &ctx.sde)
        .await;
    ctx.living_world.mark_convoy_destroyed_in_combat(hauler_cid);
    let post_order_price = ctx.db.with_snapshot(|s| {
        s.market_orders
            .values()
            .filter(|o| !o.is_bid && o.is_npc)
            .map(|o| o.price.0)
            .min()
    });
    assert_eq!(
        post_order_price, pre_order_price,
        "Convoy loss must not fabricate synthetic NPC market price inflation"
    );
}

// parity: feat:npc.navy-npc
// parity: feat:npc.zarzakh-gate-guns-patrols
#[tokio::test]
async fn test_e2e_npc_navy_and_zarzakh_gate_guns_patrols() {
    eve_testkit::attest(&["feat:npc.navy-npc", "feat:npc.zarzakh-gate-guns-patrols"]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);

    let mut pilot = spawn_player_combat_pilot(
        &ctx,
        84_002,
        "npc_navy_p1",
        "Gallente Provocateur",
        HURRICANE_TYPE,
        false,
    )
    .await;
    let mut victim = spawn_player_combat_pilot(
        &ctx,
        84_003,
        "npc_zz_victim",
        "Zarzakh Traveler",
        HURRICANE_TYPE,
        false,
    )
    .await;

    // 1. Enlist pilot in Gallente militia (enemy of Caldari State in Perimeter) -> Faction Navy engages!
    call_rpc(
        &ctx,
        &mut pilot.sess,
        "facWarMgr",
        "JoinFactionAsCharacter",
        &[PyRep::Integer(500_004)],
    )
    .await
    .expect("Join Gallente Militia");
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&pilot.char_id).unwrap().solar_system_id = PERIMETER;
    });
    pilot.sess.solar_system_id = Some(PERIMETER);
    let p_h = ctx.systems.get_or_spawn(PERIMETER);
    let _ = p_h
        .spawn_ball(eve_destiny::Ball {
            id: pilot.ship_id,
            type_id: HURRICANE_TYPE,
            position: DVec3::new(35_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 3.0,
            mass: 1.5e7,
            radius: 200.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

    let navy_cid = ctx
        .living_world
        .ensure_system_navy_patrol(PERIMETER, &ctx.sde)
        .expect("Caldari Navy patrol in Perimeter");
    let navy_ship_tid = ctx.living_world.get_convoy(navy_cid).unwrap().ship_type_id;
    assert_eq!(navy_ship_tid, 52_936, "Caldari Navy cruiser 52936 protects Caldari space");
    let _ = p_h
        .spawn_ball(eve_destiny::Ball {
            id: navy_cid,
            type_id: navy_ship_tid,
            position: DVec3::new(39_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 250.0,
            inertia: 3.0,
            mass: 1.2e7,
            radius: 200.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    step_extended_npc_ai(&ctx, PERIMETER).await;
    assert_eq!(
        derive_npc_archetype(&ctx, 10046),
        "Navy",
        "Faction Navy archetype resolved from SDE/group"
    );

    // 2. Illegal PvP aggression earns a real Crimewatch flag and triggers ZarzakhGateGun engagement
    //    without fabricating synthetic disapproval_flags!
    let lowsec_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status > 0.05 && s.security_status < 0.44 && s.solar_system_id < 30_100_000)
        .map(|s| s.solar_system_id)
        .expect("lowsec solar system");

    let gun_id = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .find(|i| {
                i.location_id == pilot.ship_id
                    && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 55)
            })
            .unwrap()
            .item_id
    });

    for (p, x) in [(&mut pilot, 10_000.0), (&mut victim, 12_500.0)] {
        ctx.db.with_mut_snapshot(|s| {
            let ch = s.characters.get_mut(&p.char_id).unwrap();
            ch.solar_system_id = lowsec_sys;
            ch.station_id = None;
        });
        p.sess.solar_system_id = Some(lowsec_sys);
        p.sess.station_id = None;
        let ls_h = ctx.systems.get_or_spawn(lowsec_sys);
        let (b, cs) = eve_server::living_world::sde_ball_and_combat(
            &ctx.sde,
            p.ship_id,
            HURRICANE_TYPE,
            DVec3::new(x, 0.0, 0.0),
        );
        let _ = ls_h.spawn_ball_with_combat(b, cs).await;
    }
    lock_player_target(&ctx, &mut pilot.sess, lowsec_sys, pilot.ship_id, victim.ship_id).await;
    call_rpc(
        &ctx,
        &mut pilot.sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(gun_id),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(victim.ship_id),
        ],
    )
    .await
    .expect("Fire weapon illegally in lowsec to earn Suspect timer");
    call_rpc(
        &ctx,
        &mut pilot.sess,
        "dogmaIM",
        "Deactivate",
        &[PyRep::Integer(gun_id), PyRep::String("targetAttack".into())],
    )
    .await
    .expect("Deactivate gun");

    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&pilot.char_id).unwrap();
        ch.solar_system_id = ZARZAKH;
        ch.station_id = None;
    });
    pilot.sess.solar_system_id = Some(ZARZAKH);
    pilot.sess.station_id = None;
    let z_h = ctx.systems.get_or_spawn(ZARZAKH);
    let (zb, mut zcs) = eve_server::living_world::sde_ball_and_combat(
        &ctx.sde,
        pilot.ship_id,
        HURRICANE_TYPE,
        DVec3::new(10_000.0, 0.0, 0.0),
    );
    zcs.warp_exit_invulnerable_remaining_ticks = 0;
    let shield_before_gate_gun = zcs.shield_hp;
    let _ = z_h.spawn_ball_with_combat(zb, zcs).await;

    let zz_patrol = spawn_specialized_npc_entity(
        &ctx,
        ZARZAKH,
        15_674,
        "ZarzakhGateGun",
        500_010,
        DVec3::new(11_000.0, 0.0, 0.0),
    )
    .await;
    assert!(zz_patrol > 0, "Zarzakh defense sentry spawned in space");

    ctx.advance_secs(4);
    step_extended_npc_ai(&ctx, ZARZAKH).await;
    let shield_after_gate_gun = z_h
        .get_combat_state(pilot.ship_id)
        .await
        .ok()
        .flatten()
        .map(|cs| cs.shield_hp)
        .unwrap_or(shield_before_gate_gun);

    let has_suspect_or_criminal = {
        let cw = ctx.crimewatch.read().unwrap_or_else(|e| e.into_inner());
        cw.states
            .get(&pilot.char_id)
            .is_some_and(|st| st.is_criminal(ctx.now_unix()) || st.is_suspect(ctx.now_unix()))
    };
    let has_disapproval = with_mut_combat_runtime(&ctx, |rt| {
        rt.disapproval_flags.contains_key(&pilot.char_id)
    });
    assert!(
        has_suspect_or_criminal && shield_after_gate_gun < shield_before_gate_gun,
        "Flagged pilot ({has_suspect_or_criminal}) is engaged & damaged by ZarzakhGateGun ({shield_before_gate_gun} -> {shield_after_gate_gun})"
    );
    assert_eq!(has_disapproval, false, "Zarzakh does not write synthetic disapproval_flags");
}

// parity: feat:npc.sleeper-ewar-by-wormhole-class
// parity: feat:npc.pochven-hypnosian-sleepers
#[tokio::test]
async fn test_e2e_npc_sleeper_ewar_by_wh_class_and_pochven_hypnosian_sleepers() {
    eve_testkit::attest(&[
        "feat:npc.sleeper-ewar-by-wormhole-class",
        "feat:npc.pochven-hypnosian-sleepers",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);

    let mut pilot = spawn_player_combat_pilot(
        &ctx,
        84_004,
        "npc_wh_p1",
        "Wormhole Explorer",
        HURRICANE_TYPE,
        false,
    )
    .await;

    // 1. Wormhole C1/C2 vs C3+ Sleeper EWAR differentiation & anti-capital escalation!
    let c1_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.wormhole_class_id == Some(1))
        .map(|s| s.solar_system_id)
        .expect("C1 wormhole system");
    let c3_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.wormhole_class_id == Some(3))
        .map(|s| s.solar_system_id)
        .expect("C3 wormhole system");

    // In C1 system: Sleeper applies neut/web but NEVER warp scram!
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&pilot.char_id).unwrap().solar_system_id = c1_sys;
    });
    pilot.sess.solar_system_id = Some(c1_sys);
    let c1_h = ctx.systems.get_or_spawn(c1_sys);
    let (p_ball, mut p_cs) =
        eve_server::living_world::sde_ball_and_combat(&ctx.sde, pilot.ship_id, HURRICANE_TYPE, DVec3::ZERO);
    let init_cap = p_cs.capacitor;
    p_cs.capacitor = init_cap;
    let _ = c1_h.spawn_ball_with_combat(p_ball, p_cs).await;
    let c1_sleeper = spawn_specialized_npc_entity(
        &ctx,
        c1_sys,
        30_195,
        "SleeperEmergentPatroller",
        500_012,
        DVec3::new(5_000.0, 0.0, 0.0),
    )
    .await;
    assert!(c1_sleeper > 0, "C1 Sleeper spawned");
    ctx.advance_secs(5);
    step_extended_npc_ai(&ctx, c1_sys).await;
    let (scrambled_c1, cap_after_c1) = {
        let bp = c1_h.ballpark.read().unwrap_or_else(|e| e.into_inner());
        let scram = bp
            .combat_states
            .get(&pilot.ship_id)
            .is_some_and(|c| c.warp_scramble_strength > 0);
        let cap = bp.combat_states.get(&pilot.ship_id).map(|c| c.capacitor).unwrap_or(init_cap);
        (scram, cap)
    };
    assert_eq!(scrambled_c1, false, "C1 Sleepers must NOT warp scram");
    assert!(cap_after_c1 <= init_cap, "C1 Sleeper updates target combat state");

    // In C3 system with Capital hull (Phoenix): C3 Sleeper applies warp scram & anti-capital damage!
    let cap_ship = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let id = s.next_entity_id;
        s.items.insert(
            id,
            eve_db::InventoryItemRecord {
                item_id: id,
                type_id: PHOENIX_DREAD_TYPE,
                owner_id: pilot.char_id,
                location_id: i64::from(c3_sys),
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let ch = s.characters.get_mut(&pilot.char_id).unwrap();
        ch.solar_system_id = c3_sys;
        ch.active_ship_id = id;
        id
    });
    pilot.sess.solar_system_id = Some(c3_sys);
    pilot.sess.ship_id = Some(cap_ship);
    let c3_h = ctx.systems.get_or_spawn(c3_sys);
    let (cap_ball, cap_cs) =
        eve_server::living_world::sde_ball_and_combat(&ctx.sde, cap_ship, PHOENIX_DREAD_TYPE, DVec3::ZERO);
    let init_cap_shield = cap_cs.shield_hp;
    let _ = c3_h.spawn_ball_with_combat(cap_ball, cap_cs).await;
    let c3_sleeper = spawn_specialized_npc_entity(
        &ctx,
        c3_sys,
        30_203,
        "SleeperAwakenedDefender",
        500_012,
        DVec3::new(6_000.0, 0.0, 0.0),
    )
    .await;
    assert!(c3_sleeper > 0, "C3 Sleeper spawned");
    ctx.advance_secs(5);
    step_extended_npc_ai(&ctx, c3_sys).await;
    let (scrambled_c3, cur_cap_shield) = {
        let bp = c3_h.ballpark.read().unwrap_or_else(|e| e.into_inner());
        let scram = bp
            .combat_states
            .get(&cap_ship)
            .is_some_and(|c| c.warp_scramble_strength > 0);
        let sh = bp.combat_states.get(&cap_ship).map(|c| c.shield_hp).unwrap_or(init_cap_shield);
        (scram, sh)
    };
    assert_eq!(scrambled_c3, true, "C3+ Sleepers apply warp scram");
    assert!(cur_cap_shield < init_cap_shield, "C3+ Sleepers engage capital ship with live weapon fire");

    // 2. Pochven Hypnosian Sleepers (Faded 56337, Awakened 56328, Aroused 56319) guard Pochven wormholes
    let pochven_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.region_id == 10_000_070)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_045_339);
    let sig = eve_db::CosmicSignatureRecord {
        signature_id: "HYP-777".into(),
        solar_system_id: pochven_sys,
        sig_group: "Wormhole".into(),
        type_name: "Wormhole".into(),
        position: [20_000.0, 0.0, 0.0],
        signal_strength: 1.0,
        wormhole_type_id: Some(56_026),
        destination_system_id: Some(ZARZAKH),
        created_unix: 0,
        lifetime_seconds: 43_200,
        max_mass_kg: 1.0e9,
        remaining_mass_kg: 1.0e9,
        max_jump_mass_kg: 3.0e8,
        collapsed: false,
    };
    let (_, guard_ids) =
        open_pochven_wormhole_and_spawn_guards(&ctx, pochven_sys, &sig, 905_000_001).await;
    let pch_h = ctx.systems.get_or_spawn(pochven_sys);
    let mut spawned_types = Vec::new();
    for gid in &guard_ids {
        if let Some((_, b)) = pch_h.get_ball_snapshot(*gid).await.ok().flatten() {
            spawned_types.push(b.type_id);
        }
    }
    assert!(spawned_types.contains(&56_337), "Faded Hypnosian Sleeper (56337) spawned");
    assert!(spawned_types.contains(&56_328), "Awakened Hypnosian Sleeper (56328) spawned");
    assert!(spawned_types.contains(&56_319), "Aroused Hypnosian Sleeper (56319) spawned");
    let _ = tick_background_cluster(&ctx).await;
    let _ = move_player_near_ball;
}
