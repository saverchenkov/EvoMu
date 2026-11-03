//! E2E parity integration tests for NPC & Mining features (Part 1).

#![forbid(unsafe_code)]

use crate::open_feats_and_rpcs_b22::{rpc, select_pilot, PILOT_1, PILOT_2};
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::living_world::{sde_ball_and_combat, LivingWorldConvoy, NpcArchetype};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    is_outlaw_ship_docking_refused, step_empire_navy_patrols, step_extended_npc_ai_sync,
    with_combat_runtime,
};
use eve_server::sp6_lifecycle::tick_background_cluster;
use eve_server::system_actor::AsteroidState;
use glam::DVec3;
use std::collections::HashSet;

const JITA_SYS: u32 = 30_000_142;
const PERIMETER_SYS: u32 = 30_000_144;

async fn spawn_pilot_ship_in_space(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    solar_system_id: u32,
    pos: DVec3,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = None;
            c.structure_id = None;
            c.solar_system_id = solar_system_id;
            c.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: solar_system_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    sess.solar_system_id = Some(solar_system_id);
    sess.station_id = None;
    sess.ship_id = Some(ship_id);

    let handle = ctx.systems.get_or_spawn(solar_system_id);
    let (ball, combat) = sde_ball_and_combat(&ctx.sde, ship_id, ship_type_id, pos);
    handle
        .spawn_ball_with_combat(ball, combat)
        .await
        .expect("spawn pilot ship ball");
}

#[tokio::test]
async fn test_parity_navy_npc_and_faction_police() {
    // parity: feat:npc.navy-npc
    eve_testkit::attest(&["feat:npc.navy-npc"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let outlaw_ship = 850_001i64;
    let mut s_outlaw = select_pilot(&ctx, 50_001, "demo", "demo-secret", PILOT_1).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_1) {
            c.security_status = -6.0;
        }
    });
    spawn_pilot_ship_in_space(
        &ctx,
        &mut s_outlaw,
        PILOT_1,
        outlaw_ship,
        624,
        JITA_SYS,
        DVec3::new(10_000.0, 0.0, 0.0),
    )
    .await;

    let pod_ship = 850_002i64;
    let mut s_pod = select_pilot(&ctx, 50_002, "other", "other-secret", PILOT_2).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_2) {
            c.security_status = -6.0;
        }
    });
    spawn_pilot_ship_in_space(
        &ctx,
        &mut s_pod,
        PILOT_2,
        pod_ship,
        670,
        JITA_SYS,
        DVec3::new(20_000.0, 0.0, 0.0),
    )
    .await;

    assert!(
        is_outlaw_ship_docking_refused(&ctx, JITA_SYS, PILOT_1, outlaw_ship),
        "Outlaw ship in highsec must be refused docking"
    );
    assert!(
        !is_outlaw_ship_docking_refused(&ctx, JITA_SYS, PILOT_2, pod_ship),
        "Outlaw capsule in highsec must be allowed to dock"
    );

    ctx.advance_secs(5);
    tick_background_cluster(&ctx).await;
    let _ = step_empire_navy_patrols(&ctx, JITA_SYS).await;
    step_extended_npc_ai_sync(&ctx, JITA_SYS);

    let (police_targets, navy_targets) = with_combat_runtime(&ctx, |rt| {
        let pol: Vec<Option<i64>> = rt
            .npc_entities
            .values()
            .filter(|n| n.solar_system_id == JITA_SYS && n.archetype == "FactionPolice")
            .map(|n| n.target_id)
            .collect();
        let nav: Vec<Option<i64>> = rt
            .npc_entities
            .values()
            .filter(|n| n.solar_system_id == JITA_SYS && n.archetype == "Navy")
            .map(|n| n.target_id)
            .collect();
        (pol, nav)
    });

    assert!(
        police_targets.contains(&Some(outlaw_ship)),
        "Faction Police must spawn and target outlaw ship"
    );
    assert!(
        !police_targets.contains(&Some(pod_ship)),
        "Faction Police must never target capsules"
    );
    assert!(
        !navy_targets.is_empty(),
        "Empire Navy patrol fleet must spawn in empire-owned system"
    );
    assert!(
        navy_targets.contains(&Some(outlaw_ship)),
        "Empire Navy patrol must engage outlaw ship"
    );
    assert!(
        !navy_targets.contains(&Some(pod_ship)),
        "Empire Navy patrol must never engage capsules"
    );
}

#[tokio::test]
async fn test_parity_mining_op_npc_and_hauler_convoy() {
    // parity: feat:npc.mining-op-npc, feat:npc.npc-hauler
    eve_testkit::attest(&["feat:npc.mining-op-npc", "feat:npc.npc-hauler"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let ship_id = 850_010i64;
    let mut sess = select_pilot(&ctx, 50_010, "demo", "demo-secret", PILOT_1).await;
    spawn_pilot_ship_in_space(
        &ctx,
        &mut sess,
        PILOT_1,
        ship_id,
        624,
        PERIMETER_SYS,
        DVec3::ZERO,
    )
    .await;

    let perim_handle = ctx.systems.get_or_spawn(PERIMETER_SYS);
    let initial_asts = perim_handle
        .populate_asteroid_belt_entities()
        .await
        .expect("populate Perimeter belts");
    let initial_total_units: u64 = initial_asts.iter().map(|a| a.remaining_units).sum();

    let mining_convoy_id = ctx
        .living_world
        .ensure_system_mining_op(PERIMETER_SYS, &ctx.sde);
    for _ in 0..8 {
        ctx.advance_secs(20);
        let _ = ctx
            .living_world
            .step_all_mining_ops(&ctx.systems, &ctx.db, &ctx.sde)
            .await;
    }

    let post_asts = perim_handle
        .populate_asteroid_belt_entities()
        .await
        .expect("post-mining belts");
    let post_total_units: u64 = post_asts.iter().map(|a| a.remaining_units).sum();
    assert!(
        post_total_units < initial_total_units,
        "NPC mining op must extract ore from belt asteroids"
    );
    assert!(
        perim_handle
            .get_ball_snapshot(mining_convoy_id)
            .await
            .ok()
            .flatten()
            .is_some(),
        "NPC miner ball must be materialized in Perimeter ballpark"
    );

    let orders_rep = rpc(
        &ctx,
        &mut sess,
        "marketProxy",
        "GetOrders",
        &[PyRep::Integer(1230)],
    )
    .await
    .expect("marketProxy.GetOrders");
    assert!(!matches!(orders_rep, PyRep::None));

    // Multi-system HaulerConvoy stargate traversal (Jita -> Perimeter) & cargo drop
    let hauler_cid = 880_001i64;
    ctx.living_world.register_convoy(LivingWorldConvoy {
        convoy_id: hauler_cid,
        archetype: NpcArchetype::HaulerConvoy,
        ship_type_id: 648,
        origin_system_id: JITA_SYS,
        current_system_id: JITA_SYS,
        destination_system_id: PERIMETER_SYS,
        destination_station_id: 60_003_760,
        cargo_type_id: 34,
        cargo_quantity: 50_000,
        lod_level: 2,
        destroyed: false,
    });
    ctx.living_world
        .set_convoy_route(hauler_cid, vec![JITA_SYS, PERIMETER_SYS]);

    let jita_handle = ctx.systems.get_or_spawn(JITA_SYS);
    let (h_ball, h_combat) = sde_ball_and_combat(&ctx.sde, hauler_cid, 648, DVec3::ZERO);
    jita_handle
        .spawn_ball_with_combat(h_ball, h_combat)
        .await
        .expect("spawn hauler in Jita");

    let _ = ctx
        .living_world
        .step_hauler_convoy(hauler_cid, &ctx.systems, &ctx.db, &ctx.sde)
        .await;
    let transit = eve_server::living_world::LivingWorldDirector::compute_hauler_transit_duration_secs(
        &ctx.sde,
        648,
        JITA_SYS,
        None,
        Some(PERIMETER_SYS),
        60_003_760,
    );
    ctx.advance_secs(transit);
    let _ = ctx
        .living_world
        .step_hauler_convoy(hauler_cid, &ctx.systems, &ctx.db, &ctx.sde)
        .await;
    assert!(
        jita_handle
            .get_ball_snapshot(hauler_cid)
            .await
            .ok()
            .flatten()
            .is_none(),
        "Hauler ball must leave Jita ballpark after stargate jump"
    );
    assert!(
        perim_handle
            .get_ball_snapshot(hauler_cid)
            .await
            .ok()
            .flatten()
            .is_some(),
        "Hauler ball must materialize in Perimeter ballpark after stargate jump"
    );

    let (wreck_id, _bounty, dropped_types) =
        eve_server::services::combat_crimewatch::destroy_specialized_npc_and_queue_bounty(
            &ctx,
            PERIMETER_SYS,
            hauler_cid,
            PILOT_1,
        )
        .await;
    assert!(
        wreck_id > 0 && dropped_types.contains(&34),
        "Destroying hauler convoy must spawn wreck and drop Tritanium cargo"
    );
}

#[tokio::test]
async fn test_parity_mining_belts_as_balls_and_ore_distribution() {
    // parity: feat:mining.belts-as-balls, feat:mining.belt-ore-distribution
    eve_testkit::attest(&[
        "feat:mining.belts-as-balls",
        "feat:mining.belt-ore-distribution",
    ]);

    let ctx = ServerContext::new();

    let perim_handle = ctx.systems.get_or_spawn(PERIMETER_SYS);
    let perim_asts = perim_handle
        .populate_asteroid_belt_entities()
        .await
        .expect("Perimeter belts");
    assert!(!perim_asts.is_empty(), "Perimeter must spawn belt asteroids");
    let perim_types: HashSet<u32> = perim_asts.iter().map(|a| a.type_id).collect();
    assert!(perim_types.contains(&1230), "Highsec 0.9 must contain Veldspar");
    assert!(perim_types.contains(&1228), "Highsec 0.9 must contain Scordite");

    let first_ast = &perim_asts[0];
    let (_, ast_ball) = perim_handle
        .get_ball_snapshot(first_ast.asteroid_id)
        .await
        .expect("get_ball_snapshot")
        .expect("Asteroid must exist as a Ball in ballpark");
    assert_eq!(ast_ball.type_id, first_ast.type_id);
    assert!(ast_ball.radius >= 60.0 && ast_ball.mass > 0.0);

    let tama_handle = ctx.systems.get_or_spawn(30_002_813);
    let tama_asts = tama_handle
        .populate_asteroid_belt_entities()
        .await
        .expect("Tama belts");
    let tama_types: HashSet<u32> = tama_asts.iter().map(|a| a.type_id).collect();
    assert!(!tama_types.contains(&1230), "Lowsec must not spawn Veldspar");
    assert!(!tama_types.contains(&1228), "Lowsec must not spawn Scordite");
    assert!(
        tama_types.contains(&1226) || tama_types.contains(&1231) || tama_types.contains(&21),
        "Lowsec must spawn lowsec ores"
    );

    let null_handle = ctx.systems.get_or_spawn(30_004_759);
    let null_asts = null_handle
        .populate_asteroid_belt_entities()
        .await
        .expect("Nullsec belts");
    let null_types: HashSet<u32> = null_asts.iter().map(|a| a.type_id).collect();
    assert!(
        null_types.contains(&1229) && null_types.contains(&1225) && null_types.contains(&11396),
        "Nullsec must spawn Gneiss, Crokite, and Mercoxit"
    );
    let deep_null_ores: HashSet<u32> = ctx
        .sde
        .belt_ores_for_security_and_region(-0.8, 10_000_060)
        .into_iter()
        .map(|t| t.type_id)
        .collect();
    assert!(
        deep_null_ores.contains(&1223) && deep_null_ores.contains(&22) && deep_null_ores.contains(&11396),
        "Deep nullsec (-0.8) must distribute Bistot, Arkonor, and Mercoxit"
    );

    let pochven_ores: HashSet<u32> = ctx
        .sde
        .belt_ores_for_system(30_000_021)
        .into_iter()
        .map(|t| t.type_id)
        .collect();
    assert!(
        pochven_ores.contains(&52306)
            && pochven_ores.contains(&52315)
            && pochven_ores.contains(&52316),
        "Pochven must distribute Talassonite, Rakovene, and Bezdnacine"
    );
}

#[tokio::test]
async fn test_parity_excavator_drones_ore_and_ice() {
    // parity: feat:mining.excavator-drones
    eve_testkit::attest(&["feat:mining.excavator-drones"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let rorq_id = 850_020i64;
    let core_id = 850_021i64;
    let fuel_id = 850_022i64;
    let exc_ore_id = 850_023i64;
    let exc_ice_id = 850_024i64;
    let ore_ast_id = 850_025i64;
    let ice_ast_id = 850_026i64;

    let mut sess = select_pilot(&ctx, 50_020, "demo", "demo-secret", PILOT_1).await;
    spawn_pilot_ship_in_space(
        &ctx,
        &mut sess,
        PILOT_1,
        rorq_id,
        28352,
        PERIMETER_SYS,
        DVec3::ZERO,
    )
    .await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            core_id,
            InventoryItemRecord {
                item_id: core_id,
                type_id: 28583,
                owner_id: PILOT_1,
                location_id: rorq_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(core_id, true);
        snap.items.insert(
            fuel_id,
            InventoryItemRecord {
                item_id: fuel_id,
                type_id: 16273,
                owner_id: PILOT_1,
                location_id: rorq_id,
                flag: InventoryFlag::Cargo,
                quantity: 5_000,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            exc_ore_id,
            InventoryItemRecord {
                item_id: exc_ore_id,
                type_id: 41030,
                owner_id: PILOT_1,
                location_id: rorq_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            exc_ice_id,
            InventoryItemRecord {
                item_id: exc_ice_id,
                type_id: 43681,
                owner_id: PILOT_1,
                location_id: rorq_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        for (sk_id, lvl) in [(3438, 5), (3436, 5), (22541, 5), (43702, 5), (43703, 5), (28374, 5)] {
            snap.skills.entry(PILOT_1).or_default().insert(
                sk_id,
                TrainedSkillRecord {
                    type_id: sk_id,
                    level: lvl,
                    skill_points: 256_000.0,
                },
            );
        }
    });
    ctx.invalidate_ship_fitting(rorq_id);

    let handle = ctx.systems.get_or_spawn(PERIMETER_SYS);
    let (ore_ball, _) = sde_ball_and_combat(&ctx.sde, ore_ast_id, 1230, DVec3::new(1_500.0, 0.0, 0.0));
    handle
        .spawn_asteroid(
            AsteroidState {
                asteroid_id: ore_ast_id,
                belt_id: 0,
                type_id: 1230,
                initial_units: 100_000,
                remaining_units: 100_000,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            ore_ball,
        )
        .await
        .expect("spawn Veldspar");

    let (ice_ball, _) = sde_ball_and_combat(&ctx.sde, ice_ast_id, 16264, DVec3::new(1_800.0, 0.0, 0.0));
    handle
        .spawn_asteroid(
            AsteroidState {
                asteroid_id: ice_ast_id,
                belt_id: 0,
                type_id: 16264,
                initial_units: 1_000,
                remaining_units: 1_000,
                unit_volume: 1_000.0,
                depleted: false,
                respawn_at_tick: None,
            },
            ice_ball,
        )
        .await
        .expect("spawn Blue Ice");

    let _ = rpc(
        &ctx,
        &mut sess,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(exc_ore_id), PyRep::Integer(exc_ice_id)])],
    )
    .await
    .expect("LaunchDrones");

    let no_core_err = rpc(
        &ctx,
        &mut sess,
        "entity",
        "CmdMineRepeatedly",
        &[
            PyRep::List(vec![PyRep::Integer(exc_ore_id)]),
            PyRep::Integer(ore_ast_id),
        ],
    )
    .await
    .expect_err("Excavator without Industrial Core must fail");
    assert_eq!(no_core_err.code, "IndustrialCoreRequired");

    let _ = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(core_id), PyRep::String("industrialCore".into())],
    )
    .await
    .expect("Activate Capital Industrial Core");

    let _ = rpc(
        &ctx,
        &mut sess,
        "entity",
        "CmdMineRepeatedly",
        &[
            PyRep::List(vec![PyRep::Integer(exc_ore_id)]),
            PyRep::Integer(ore_ast_id),
        ],
    )
    .await
    .expect("Excavator ore mining with Industrial Core");

    let _ = rpc(
        &ctx,
        &mut sess,
        "entity",
        "CmdMineRepeatedly",
        &[
            PyRep::List(vec![PyRep::Integer(exc_ice_id)]),
            PyRep::Integer(ice_ast_id),
        ],
    )
    .await
    .expect("Excavator ice harvesting with Industrial Core");

    let next_tick = with_combat_runtime(&ctx, |rt| {
        rt.active_drone_miners
            .values()
            .map(|m| m.next_cycle_unix)
            .max()
            .unwrap()
    });
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&ctx, next_tick).await;

    let (ore_mined, ice_mined) = ctx.db.with_snapshot(|snap| {
        let ore: i32 = snap
            .items
            .values()
            .filter(|i| i.location_id == rorq_id && i.type_id == 1230)
            .map(|i| i.quantity)
            .sum();
        let ice: i32 = snap
            .items
            .values()
            .filter(|i| i.location_id == rorq_id && i.type_id == 16264)
            .map(|i| i.quantity)
            .sum();
        (ore, ice)
    });
    assert!(ore_mined > 0, "Excavator mining drone must yield ore into Rorqual");
    assert!(ice_mined > 0, "Excavator ice drone must yield ice into Rorqual");
}
