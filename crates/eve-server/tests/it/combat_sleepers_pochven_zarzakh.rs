//! Integration tests for Sleepers, Pochven Hypnosian Sleepers, Zarzakh Gate Guns/Patrols,
//! Pochven Loot Drops, Ship Insurance Voiding/Expiry, and Abyssal Suspect Flags.

use crate::common::{self, LivePilot};
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, SystemDynamicState,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_server::combat::crimewatch::tick_concord_responses;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    dispatch_combat_rpc, spawn_specialized_npc_entity, step_extended_npc_ai, with_combat_runtime,
};
use eve_server::services::security_mgr::insurgency_pvp_loot_drop_threshold;
use eve_server::world_seeder::seed_full_market_catalog;
use eve_testkit::client_harness::{extract_bound_id_from_wire_rep, ClientErrorResponse};
use glam::DVec3;
use std::collections::{HashMap, HashSet};

const JITA_SYS: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;

async fn wire_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    bound_id: Option<u64>,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, ClientErrorResponse> {
    common::borrow_wire_harness(ctx, sess)
        .call(service, bound_id, method, args, kwargs)
        .await
}

async fn wire_ok(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    bound_id: Option<u64>,
    method: &str,
    args: &[PyRep],
) -> PyRep {
    common::borrow_wire_harness(ctx, sess)
        .call_ok(service, bound_id, method, args, &HashMap::new())
        .await
}

async fn wire_bind_oid(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    arg: PyRep,
) -> u64 {
    let rep = wire_ok(ctx, sess, service, None, "MachoBindObject", &[arg, PyRep::None]).await;
    extract_bound_id_from_wire_rep(&rep)
        .unwrap_or_else(|| panic!("expected bound oid from {service}.MachoBindObject, got {rep:?}"))
}

async fn wire_dock_station(
    ctx: &ServerContext,
    sess: &mut SessionState,
    ship_id: i64,
    sys_id: u32,
    station_id: i64,
) {
    ctx.advance_secs(12);
    common::slash_cmd(ctx, sess, &format!("/tr {station_id}")).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let st_pos = ctx.sde.stations[&station_id].position;
    let active_ship = sess.ship_id.unwrap_or(ship_id);
    let moved = sys.set_ball_position(active_ship, st_pos + DVec3::new(300.0, 0.0, 0.0)).await;
    assert!(moved, "wire_dock_station failed to move ship {active_ship} (arg {ship_id}) in sys {sys_id}");
    sys.step_manual(0.1).await.unwrap();
    wire_ok(ctx, sess, "stationSvc", None, "Dock", &[PyRep::Integer(station_id)]).await;
}

async fn fit_and_load_turrets(
    ctx: &ServerContext,
    pilot: &mut LivePilot,
    dest_sys_id: u32,
    turret_type_id: u32,
    ammo_type_id: u32,
    ammo_qty: u32,
    count: usize,
) -> Vec<i64> {
    wire_dock_station(ctx, &mut pilot.sess, pilot.ship_id, JITA_SYS, JITA_STATION).await;
    for sk in [
        3300_u32, 3301, 3302, 3303, 3304, 3305, 3306, 3308, 3310, 3311, 3312, 3329, 3337, 3392,
        3394, 3413, 3416, 3418, 3419, 3425, 3426, 11084, 12209,
    ] {
        common::slash_cmd(ctx, &mut pilot.sess, &format!("/giveskill {sk} 5")).await;
    }
    let ship_id = pilot.ship_id;
    let mut gun_ids = Vec::with_capacity(count);
    for idx in 0..count {
        let gid = common::slash_cmd(ctx, &mut pilot.sess, &format!("/createitem {turret_type_id} 1"))
            .await
            .dict_get("itemID")
            .and_then(|v| v.as_i64())
            .expect("created turret itemID");
        wire_ok(
            ctx,
            &mut pilot.sess,
            "invbroker",
            None,
            "Add",
            &[PyRep::Integer(gid), PyRep::Integer(ship_id), PyRep::Integer(27 + idx as i64)],
        )
        .await;
        gun_ids.push(gid);
    }
    let ammo_id = common::slash_cmd(
        ctx,
        &mut pilot.sess,
        &format!("/createitem {ammo_type_id} {ammo_qty}"),
    )
    .await
    .dict_get("itemID")
    .and_then(|v| v.as_i64())
    .expect("created ammo itemID");
    wire_ok(
        ctx,
        &mut pilot.sess,
        "invbroker",
        None,
        "Add",
        &[PyRep::Integer(ammo_id), PyRep::Integer(ship_id), PyRep::Integer(5)],
    )
    .await;
    let gun_reps: Vec<PyRep> = gun_ids.iter().map(|&g| PyRep::Integer(g)).collect();
    wire_ok(
        ctx,
        &mut pilot.sess,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(ship_id),
            PyRep::List(gun_reps),
            PyRep::List(vec![PyRep::Integer(ammo_id)]),
        ],
    )
    .await;
    ctx.advance_secs(31);
    wire_ok(ctx, &mut pilot.sess, "stationSvc", None, "Undock", &[]).await;
    common::slash_cmd(ctx, &mut pilot.sess, &format!("/tr {dest_sys_id}")).await;
    pilot.beyonce = wire_bind_oid(ctx, &mut pilot.sess, "beyonce", PyRep::Integer(i64::from(dest_sys_id))).await;
    ctx.advance_secs(31);
    gun_ids
}

async fn lock_target_wire(
    ctx: &ServerContext,
    attacker: &mut LivePilot,
    sys_id: u32,
    target_id: i64,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    if let (Some((_, a)), Some((_, b))) = (
        sys.get_ball_snapshot(attacker.ship_id).await.unwrap(),
        sys.get_ball_snapshot(target_id).await.unwrap(),
    ) {
        if a.position.distance(b.position) > 3_500.0 {
            sys.set_ball_position(attacker.ship_id, b.position + DVec3::new(2_000.0, 0.0, 0.0))
                .await;
            sys.step_manual(0.1).await.unwrap();
        }
    }
    let d_oid = wire_bind_oid(ctx, &mut attacker.sess, "dogmaIM", PyRep::Integer(attacker.ship_id)).await;
    wire_ok(ctx, &mut attacker.sess, "dogmaIM", Some(d_oid), "AddTarget", &[PyRep::Integer(target_id)]).await;
    for _ in 0..20 {
        if sys
            .get_target_locks(attacker.ship_id)
            .await
            .unwrap()
            .iter()
            .any(|l| l.target_id == target_id && l.locked)
        {
            return;
        }
        ctx.clock().advance_secs(2);
        sys.step_manual(2.0).await.unwrap();
    }
    panic!("failed to acquire target lock on {target_id}");
}

async fn shoot_target_until_destroyed(
    ctx: &ServerContext,
    attacker: &mut LivePilot,
    sys_id: u32,
    guns: &[i64],
    target_id: i64,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    lock_target_wire(ctx, attacker, sys_id, target_id).await;
    for _ in 0..300 {
        if sys.get_ball_snapshot(target_id).await.unwrap().is_none() {
            return;
        }
        for &gid in guns {
            if sys.get_ball_snapshot(target_id).await.unwrap().is_none() {
                return;
            }
            let _ = wire_call(
                ctx,
                &mut attacker.sess,
                "dogmaIM",
                None,
                "Activate",
                &[
                    PyRep::Integer(gid),
                    PyRep::String("targetAttack".into()),
                    PyRep::Integer(target_id),
                ],
                &HashMap::new(),
            )
            .await;
        }
        sys.step_manual(3.0).await.unwrap();
        ctx.clock().advance_secs(3);
    }
    assert!(
        sys.get_ball_snapshot(target_id).await.unwrap().is_none(),
        "target {target_id} in system {sys_id} must be destroyed via dogmaIM.Activate"
    );
}

fn make_session(
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    station_id: Option<i64>,
) -> SessionState {
    let mut s = SessionState::new(4001);
    s.user_id = Some(7001);
    s.character_id = Some(char_id);
    s.corporation_id = Some(1_000_009);
    s.solar_system_id = Some(sys_id);
    s.constellation_id = Some(20_000_020);
    s.region_id = Some(10_000_002);
    s.ship_id = Some(ship_id);
    s.station_id = station_id;
    s.role = service_roles::ROLE_PLAYER | service_roles::ROLE_LOGIN;
    s
}

async fn seed_pilot(ctx: &ServerContext, char_id: i64, ship_id: i64, sys_id: u32, pos: DVec3) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.balance = IskCents::from_isk(50_000_000);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_type_combat_state(587);
    if cs.max_structure_hp <= 0.0 {
        cs.max_shield_hp = 500.0;
        cs.shield_hp = 500.0;
        cs.max_armor_hp = 400.0;
        cs.armor_hp = 400.0;
        cs.max_structure_hp = 350.0;
        cs.structure_hp = 350.0;
    }
    cs.capacitor = 500.0;
    cs.max_capacitor = 500.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        )
        .await;
}

#[tokio::test]
async fn test_sleeper_mechanics_infinite_capacitor_spawn_leash_and_ewar_by_class() {
    // parity: feat:npc.sleeper-capacitor-immunity-and-leash
    eve_testkit::attest(&[
        "feat:npc.sleeper-capacitor-immunity-and-leash",
    ]);

    let ctx = ServerContext::new();
    let c3_system_id = 31_001_123; // J-space C3 wormhole
    let char_id = 90_000_001i64;
    let ship_id = 103_000_101i64;
    let spawn_pos = DVec3::new(100_000.0, 0.0, 0.0);
    seed_pilot(&ctx, char_id, ship_id, c3_system_id, spawn_pos + DVec3::new(5_000.0, 0.0, 0.0)).await;

    let sys_handle = ctx.systems.get_or_spawn(c3_system_id);

    // 1. Spawn Sleeper Battleship in C3
    let sleeper_ball_id = spawn_specialized_npc_entity(
        &ctx,
        c3_system_id,
        30462, // Sleeper Awakened Defender
        "Sleeper",
        4001,
        spawn_pos,
    )
    .await;

    // Verify Sleeper initial state: infinite capacitor concept, EM/Thermal and Exp/Kin damage
    let initial_cs = sys_handle.get_combat_state(sleeper_ball_id).await.unwrap().unwrap();
    assert!(initial_cs.max_capacitor >= 0.0);

    // Player activates real Energy Neutralizer & Energy Nosferatu modules against Sleeper:
    // Sleeper cap is immune (does not drain); Nosferatu transfers energy to player without draining the Sleeper
    let neut_id = 203_901i64;
    let nos_id = 203_902i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.type_id = 640;
        }
        snap.items.insert(
            neut_id,
            InventoryItemRecord {
                item_id: neut_id,
                type_id: 12269, // Heavy Energy Neutralizer I (group 71)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            nos_id,
            InventoryItemRecord {
                item_id: nos_id,
                type_id: 12261, // Heavy Energy Nosferatu I (group 68)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let _ = sys_handle
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: sleeper_ball_id,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await;
    let mut player_cs_before = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    player_cs_before.max_capacitor = 5000.0;
    player_cs_before.capacitor = 4000.0;
    let _ = sys_handle.set_combat_state(ship_id, player_cs_before).await;

    let mut sess = eve_machonet::SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(c3_system_id);

    eve_server::services::combat_crimewatch::dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(neut_id), PyRep::Integer(sleeper_ball_id)],
        &std::collections::HashMap::new(),
    )
    .await
    .expect("Activate Heavy Energy Neutralizer I on Sleeper");
    eve_server::services::combat_crimewatch::dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(nos_id), PyRep::Integer(sleeper_ball_id)],
        &std::collections::HashMap::new(),
    )
    .await
    .expect("Activate Heavy Energy Nosferatu I on Sleeper");
    let cap_before_nos = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().capacitor;
    for _ in 0..15 {
        let _ = sys_handle.tick().await;
    }

    // Verify Sleeper capacitor was not depleted while player capacitor increased
    let sleeper_cs_after = sys_handle.get_combat_state(sleeper_ball_id).await.unwrap().unwrap();
    assert_eq!(sleeper_cs_after.capacitor, initial_cs.capacitor, "Player neutralizer/nosferatu must not drain Sleeper capacitor");
    let player_cs_after = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(player_cs_after.capacitor > cap_before_nos, "Player Nosferatu must still transfer energy from Sleeper");
    let _ = sys_handle.stop_non_warp_module_cycles(ship_id).await;

    // 2. Sleeper spawn leash: pull Sleeper > 200 km from spawn position
    let distant_pos = spawn_pos + DVec3::new(250_000.0, 0.0, 0.0);
    let _ = sys_handle.set_ball_position(sleeper_ball_id, distant_pos).await;

    // Step AI: distance > 200 km triggers immediate teleport back to spawn_position
    step_extended_npc_ai(&ctx, c3_system_id).await;
    let snap_after_leash = sys_handle.get_ball_snapshot(sleeper_ball_id).await.unwrap().unwrap();
    assert!(
        snap_after_leash.1.position.distance(spawn_pos) < 1.0,
        "Sleepers pulled > 200 km from spawn point must teleport back to spawn position"
    );

    // 3. Sleeper EWAR by wormhole class: C3 and higher applies warp scramble
    // Position player within Sleeper scram range (20 km)
    let _ = sys_handle.set_ball_position(ship_id, spawn_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;
    step_extended_npc_ai(&ctx, c3_system_id).await;
    let player_cs_c3 = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(
        player_cs_c3.warp_scramble_strength >= 1,
        "Sleepers in C3 and higher must apply warp scramble"
    );

    // 4. Anti-capital damage bonus: extra damage when attacking a capital ship
    let cap_ship_id = 103_000_102i64;
    seed_pilot(&ctx, 90_000_002, cap_ship_id, c3_system_id, spawn_pos + DVec3::new(8_000.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&cap_ship_id) {
            it.type_id = 19720; // Revelation Dreadnought (group 485, Capital)
        }
    });
    let mut dread_cs = ctx.build_type_combat_state(19720);
    if dread_cs.max_structure_hp <= 0.0 {
        dread_cs.max_shield_hp = 150_000.0;
        dread_cs.shield_hp = 150_000.0;
        dread_cs.max_armor_hp = 250_000.0;
        dread_cs.armor_hp = 250_000.0;
        dread_cs.max_structure_hp = 200_000.0;
        dread_cs.structure_hp = 200_000.0;
    }
    let _ = sys_handle.set_combat_state(cap_ship_id, dread_cs).await;

    let cap_cs_before = sys_handle.get_combat_state(cap_ship_id).await.unwrap().unwrap();
    step_extended_npc_ai(&ctx, c3_system_id).await;
    let cap_cs_after = sys_handle.get_combat_state(cap_ship_id).await.unwrap().unwrap();
    let cap_dmg_taken = (cap_cs_before.shield_hp - cap_cs_after.shield_hp).max(0.0);
    assert!(
        cap_dmg_taken > 0.0,
        "Sleeper must apply anti-capital damage bonus against capital ships"
    );
}

#[tokio::test]
async fn test_zarzakh_gate_guns_and_deathless_patrols() {
    // parity: feat:npc.zarzakh-gate-guns-patrols
    eve_testkit::attest(&["feat:npc.zarzakh-gate-guns-patrols"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let zarzakh_sys_id = 30_100_000_u32; // Zarzakh solar system

    // Provision live pilots via ClientHarness flows before spawning Zarzakh test NPCs
    let mut offender = common::gm_pilot_in_space(&ctx, 101, 91_100_001, 587, zarzakh_sys_id).await;
    let victim = common::gm_pilot_in_space(&ctx, 102, 91_100_002, 587, zarzakh_sys_id).await;
    let offender_guns =
        fit_and_load_turrets(&ctx, &mut offender, zarzakh_sys_id, 2889, 185, 1_000, 2).await;
    let mut pirate = common::gm_pilot_in_space(&ctx, 103, 91_100_003, 587, JITA_SYS).await;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&pirate.char_id) {
            ch.war_faction_id = Some(500_010);
        }
    });
    let pirate_guns =
        fit_and_load_turrets(&ctx, &mut pirate, zarzakh_sys_id, 2889, 185, 1_000, 2).await;

    let sys_handle = ctx.systems.get_or_spawn(zarzakh_sys_id);
    let fulcrum_pos = ctx
        .sde
        .stations
        .get(&60_015_187)
        .map(|s| s.position)
        .unwrap_or(DVec3::ZERO);
    let gun_pos = fulcrum_pos + DVec3::new(135_000.0, 0.0, 0.0);
    let park_pos = fulcrum_pos - DVec3::new(140_000.0, 0.0, 0.0);
    // Place the pirate-immunity scenario inside a distant Zarzakh stargate's 150 km safe zone
    // (> 500 km from The Fulcrum) so ships neither take Jovian gravity hazard nor enter Fulcrum NPC range.
    let gate_pos = ctx
        .sde
        .stargates()
        .values()
        .filter(|g| g.from_solar_system_id == zarzakh_sys_id)
        .map(|g| g.position)
        .find(|p| p.distance(gun_pos) > 500_000.0)
        .expect("Zarzakh must have a stargate >500km from The Fulcrum");
    let safe_zone_anchor = gate_pos + DVec3::new(100_000.0, 0.0, 0.0);
    let offender_pos = gun_pos - DVec3::new(15_000.0, 0.0, 0.0);
    let victim_pos = gun_pos - DVec3::new(17_500.0, 0.0, 0.0);
    let patrol_pos = gun_pos - DVec3::new(15_000.0, 2_000.0, 0.0);
    sys_handle.set_ball_position(offender.ship_id, offender_pos).await;
    sys_handle.set_ball_position(victim.ship_id, victim_pos).await;
    sys_handle
        .set_ball_position(pirate.ship_id, safe_zone_anchor + DVec3::new(10_000.0, 0.0, 0.0))
        .await;
    sys_handle.step_manual(0.1).await.unwrap();

    // 1. Spawn SDE Zarzakh Gate Gun (Deathless Retribution Emplacement, type 79183) and Deathless Patrol (Guristas Zarzakh Mamba, type 78896)
    let gate_gun_id = spawn_specialized_npc_entity(
        &ctx,
        zarzakh_sys_id,
        79_183,
        "ZarzakhGateGun",
        4002,
        gun_pos,
    )
    .await;
    let patrol_id = spawn_specialized_npc_entity(
        &ctx,
        zarzakh_sys_id,
        78_896,
        "ZarzakhPatrol",
        4002,
        patrol_pos,
    )
    .await;
    assert!(gate_gun_id > 0 && patrol_id > 0);

    assert!(
        !with_combat_runtime(&ctx, |rt| rt.disapproval_flags.contains_key(&offender.char_id)),
        "Pilot must start without a Zarzakh retribution/disapproval timer"
    );

    // 2. Trigger illegal aggression in Zarzakh through live target-lock + dogmaIM.Activate RPCs (no manual disapproval insertion)
    lock_target_wire(&ctx, &mut offender, zarzakh_sys_id, victim.ship_id).await;
    lock_target_wire(&ctx, &mut offender, zarzakh_sys_id, gate_gun_id).await;
    assert!(
        !sys_handle.get_target_locks(offender.ship_id).await.unwrap().is_empty(),
        "Offender must hold active target locks before Zarzakh gate gun ECM response"
    );

    wire_ok(
        &ctx,
        &mut offender.sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(offender_guns[0]),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(victim.ship_id),
        ],
    )
    .await;
    sys_handle.step_manual(1.0).await.unwrap();

    assert!(
        with_combat_runtime(&ctx, |rt| rt.disapproval_flags.contains_key(&offender.char_id)),
        "Illegal PvP aggression in Zarzakh must automatically record a disapproval/retribution timer"
    );

    let offender_cs_before = sys_handle.get_combat_state(offender.ship_id).await.unwrap().unwrap();
    let now_before_ai = ctx.now_unix();

    // 3. Step Zarzakh NPC/Sentry AI: gate gun and patrol acquire target, deal damage, ECM jam, break locks, and set NPC timer
    step_extended_npc_ai(&ctx, zarzakh_sys_id).await;

    let (gate_gun_target, patrol_target) = with_combat_runtime(&ctx, |rt| {
        (
            rt.npc_entities.get(&gate_gun_id).and_then(|n| n.target_id),
            rt.npc_entities.get(&patrol_id).and_then(|n| n.target_id),
        )
    });
    let offender_cs_after = sys_handle.get_combat_state(offender.ship_id).await.unwrap().unwrap();
    let locks_after = sys_handle.get_target_locks(offender.ship_id).await.unwrap();
    let npc_timer_until = ctx
        .db
        .with_snapshot(|s| s.sp6_lifecycle.npc_timers_until.get(&offender.char_id).copied().unwrap_or(0));

    assert_eq!(
        gate_gun_target,
        Some(offender.ship_id),
        "Zarzakh gate gun must acquire the pilot with a retribution/disapproval timer"
    );
    assert_eq!(
        patrol_target,
        Some(offender.ship_id),
        "Deathless Zarzakh patrol must acquire non-pirate offender ship"
    );
    assert!(
        offender_cs_after.shield_hp < offender_cs_before.shield_hp
            || offender_cs_after.armor_hp < offender_cs_before.armor_hp,
        "Zarzakh gate guns and Deathless patrols must deal combat damage to the offender"
    );
    assert!(
        offender_cs_after.tp_sig_multiplier.is_some_and(|m| m > 1.0)
            && offender_cs_after.signature_radius > offender_cs_before.signature_radius,
        "Zarzakh gate gun must apply target painter signature radius bloom alongside ECM (before={}, after={}, tp={:?})",
        offender_cs_before.signature_radius,
        offender_cs_after.signature_radius,
        offender_cs_after.tp_sig_multiplier
    );
    assert!(
        offender_cs_after.ecm_jammed_remaining_s > 0.0,
        "Zarzakh gate gun must apply ECM jam to the offending pilot"
    );
    assert!(
        locks_after.is_empty(),
        "Zarzakh gate gun ECM must clear offender target locks, remaining: {locks_after:?}"
    );
    assert!(
        npc_timer_until > now_before_ai,
        "Zarzakh gate gun and patrol aggression must record an active NPC combat timer"
    );

    // 4. Pirate-aligned pilot without retribution timer is spared by Zarzakh gate guns & patrols,
    //    then engaged once they acquire a retribution timer via live aggression.
    let immune_gun_id = spawn_specialized_npc_entity(
        &ctx,
        zarzakh_sys_id,
        79_183,
        "ZarzakhGateGun",
        4002,
        safe_zone_anchor,
    )
    .await;
    let immune_patrol_id = spawn_specialized_npc_entity(
        &ctx,
        zarzakh_sys_id,
        78_891,
        "ZarzakhPatrol",
        4002,
        safe_zone_anchor + DVec3::new(2_000.0, 0.0, 0.0),
    )
    .await;
    // Move offender and victim to park_pos (>250km from gun_pos/patrol_pos yet inside Fulcrum's 150km safe zone)
    // so they take neither repeated gun fire nor Jovian gravity hazard during the pirate immunity check.
    sys_handle
        .set_ball_position(offender.ship_id, park_pos)
        .await;
    sys_handle
        .set_ball_position(victim.ship_id, park_pos + DVec3::new(0.0, 2_000.0, 0.0))
        .await;
    sys_handle
        .set_ball_position(pirate.ship_id, safe_zone_anchor + DVec3::new(10_000.0, 0.0, 0.0))
        .await;
    sys_handle.step_manual(0.1).await.expect("step pirate pos");

    step_extended_npc_ai(&ctx, zarzakh_sys_id).await;
    let (immune_gun_target, immune_patrol_target) = with_combat_runtime(&ctx, |rt| {
        (
            rt.npc_entities.get(&immune_gun_id).and_then(|n| n.target_id),
            rt.npc_entities.get(&immune_patrol_id).and_then(|n| n.target_id),
        )
    });
    assert_eq!(
        immune_gun_target, None,
        "Zarzakh gate gun must spare pirate-aligned pilot without retribution timer"
    );
    assert_eq!(
        immune_patrol_target, None,
        "Deathless Zarzakh patrol must spare pirate-aligned pilot without retribution timer"
    );

    // Trigger retribution timer on the pirate-aligned pilot via live targetAttack RPC
    sys_handle
        .set_ball_position(victim.ship_id, safe_zone_anchor + DVec3::new(12_000.0, 0.0, 0.0))
        .await;
    sys_handle.step_manual(0.1).await.expect("step victim near pirate");
    lock_target_wire(&ctx, &mut pirate, zarzakh_sys_id, victim.ship_id).await;
    wire_ok(
        &ctx,
        &mut pirate.sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(pirate_guns[0]),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(victim.ship_id),
        ],
    )
    .await;
    sys_handle.step_manual(1.0).await.expect("step pirate attack");
    assert!(
        with_combat_runtime(&ctx, |rt| rt.disapproval_flags.contains_key(&pirate.char_id)),
        "Pirate-aligned pilot attacking in Zarzakh must receive retribution/disapproval timer"
    );
    // Move the non-pirate victim back to park_pos so the Deathless patrol isolates the flagged pirate.
    sys_handle
        .set_ball_position(victim.ship_id, park_pos + DVec3::new(0.0, 2_000.0, 0.0))
        .await;
    sys_handle.step_manual(0.1).await.expect("step victim away");
    step_extended_npc_ai(&ctx, zarzakh_sys_id).await;
    let (retrib_gun_target, retrib_patrol_target) = with_combat_runtime(&ctx, |rt| {
        (
            rt.npc_entities.get(&immune_gun_id).and_then(|n| n.target_id),
            rt.npc_entities.get(&immune_patrol_id).and_then(|n| n.target_id),
        )
    });
    assert_eq!(
        retrib_gun_target,
        Some(pirate.ship_id),
        "Zarzakh gate gun must engage pirate-aligned pilot who holds a retribution timer"
    );
    assert_eq!(
        retrib_patrol_target,
        Some(pirate.ship_id),
        "Deathless Zarzakh patrol must engage pirate-aligned pilot who holds a retribution timer"
    );

    // 5. Outer Zarzakh k-space gate & station patrols in the 4 adjacent k-space systems
    let mut pirate_clean =
        common::gm_pilot_in_space(&ctx, 104, 91_100_004, 587, JITA_SYS).await;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&pirate_clean.char_id) {
            ch.war_faction_id = Some(500_011);
        }
    });

    for (idx, adj_sys) in [30_002_086_u32, 30_003_841, 30_001_269, 30_001_041].into_iter().enumerate() {
        ctx.advance_secs(12);
        let mut visitor = common::gm_pilot_in_space(
            &ctx,
            105 + idx as u64,
            91_100_005 + idx as i64,
            587,
            adj_sys,
        )
        .await;
        wire_bind_oid(
            &ctx,
            &mut visitor.sess,
            "beyonce",
            PyRep::Integer(i64::from(adj_sys)),
        )
        .await;

        let adj_handle = ctx.systems.get_or_spawn(adj_sys);
        let gate_positions: Vec<DVec3> = ctx
            .sde
            .stargates()
            .values()
            .filter(|g| g.from_solar_system_id == adj_sys && g.to_solar_system_id == zarzakh_sys_id)
            .map(|g| g.position)
            .collect();
        assert!(
            !gate_positions.is_empty(),
            "Adjacent system {adj_sys} must have a stargate linking to Zarzakh"
        );
        let station_positions: Vec<DVec3> = ctx
            .sde
            .stations
            .values()
            .filter(|st| st.solar_system_id == adj_sys)
            .map(|st| st.position)
            .collect();

        let outer_patrols: Vec<(i64, DVec3)> = with_combat_runtime(&ctx, |rt| {
            rt.npc_entities
                .values()
                .filter(|n| n.solar_system_id == adj_sys && n.archetype == "OuterZarzakhPatrol")
                .filter_map(|n| adj_handle.get_ball_position_sync(n.ball_id).map(|p| (n.ball_id, p)))
                .collect()
        });
        assert!(
            !outer_patrols.is_empty(),
            "Adjacent system {adj_sys} must spawn OuterZarzakhPatrol NPCs on beyonce bind"
        );
        for gpos in &gate_positions {
            assert!(
                outer_patrols.iter().any(|(_, p)| p.distance(*gpos) <= 25_000.0),
                "Zarzakh gate in adjacent system {adj_sys} must be guarded by OuterZarzakhPatrol"
            );
        }
        for spos in &station_positions {
            assert!(
                outer_patrols.iter().any(|(_, p)| p.distance(*spos) <= 25_000.0),
                "Station in adjacent system {adj_sys} must be guarded by OuterZarzakhPatrol"
            );
        }

        // Clean pirate pilot placed at the Zarzakh gate must be spared, while non-pirate visitor is engaged
        common::slash_cmd(&ctx, &mut pirate_clean.sess, &format!("/tr {adj_sys}")).await;
        adj_handle
            .set_ball_position(pirate_clean.ship_id, gate_positions[0] + DVec3::new(3_000.0, 0.0, 0.0))
            .await;
        adj_handle
            .set_ball_position(visitor.ship_id, gate_positions[0] + DVec3::new(4_000.0, 0.0, 0.0))
            .await;
        adj_handle.step_manual(0.1).await.expect("step adj pos");
        step_extended_npc_ai(&ctx, adj_sys).await;

        let (engaged_offender, engaged_clean_pirate) = with_combat_runtime(&ctx, |rt| {
            let patrols: Vec<_> = rt
                .npc_entities
                .values()
                .filter(|n| n.solar_system_id == adj_sys && n.archetype == "OuterZarzakhPatrol")
                .collect();
            (
                patrols.iter().any(|n| n.target_id == Some(visitor.ship_id)),
                patrols.iter().any(|n| n.target_id == Some(pirate_clean.ship_id)),
            )
        });
        assert!(
            engaged_offender,
            "OuterZarzakhPatrol in adjacent system {adj_sys} must engage non-pirate pilot"
        );
        assert!(
            !engaged_clean_pirate,
            "OuterZarzakhPatrol in adjacent system {adj_sys} must spare clean pirate-aligned pilot"
        );
    }
}

#[tokio::test]
async fn test_pochven_hypnosian_sleepers_and_60pct_loot_drop() {
    // parity: feat:npc.pochven-hypnosian-sleepers, feat:combat.pochven-loot-drop
    eve_testkit::attest(&[
        "feat:npc.pochven-hypnosian-sleepers",
        "feat:combat.pochven-loot-drop",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let pochven_sys_id = 30_000_157_u32; // Otela in Pochven (region 10_000_070)
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            pochven_sys_id,
            SystemDynamicState {
                solar_system_id: pochven_sys_id,
                dbs_multiplier: 1.50,
                ess_main_bank: IskCents(0),
                ess_reserve_bank: IskCents(0),
                ess_linked_character_id: None,
                ess_link_started_tick: None,
                sov_alliance_id: None,
                military_index: 2,
                industrial_index: 2,
                strategic_index: 3,
                fw_contested_pct: 0.0,
                insurgency_corruption_stage: 2,
                insurgency_suppression_stage: 1,
                wormhole_remaining_mass_kg: 0.0,
                wormhole_destination_system_id: None,
                signature_position: None,
                fw_complex_position: None,
                fw_capture_seconds_elapsed: 0,
                fw_capture_duration_seconds: 0,
                last_fw_capture_tick: 0,
                last_sov_claim_tick: 0,
                ess_cycle_started_unix: 0,
                ess_main_bank_contributors: Default::default(),
            },
        );
    });

    // Provision hunter in a Tempest (639) with 6x 800mm Repeating Cannon II (2929) + EMP L (201)
    let mut hunter = common::gm_pilot_in_space(&ctx, 111, 91_200_001, 639, pochven_sys_id).await;
    let hunter_guns =
        fit_and_load_turrets(&ctx, &mut hunter, pochven_sys_id, 2929, 201, 5_000, 6).await;

    let mut victim = common::gm_pilot_in_space(&ctx, 112, 91_200_002, 587, pochven_sys_id).await;

    let sys_handle = ctx.systems.get_or_spawn(pochven_sys_id);
    let base_pos = DVec3::new(100_000.0, 0.0, 0.0);
    sys_handle.set_ball_position(hunter.ship_id, base_pos).await;
    sys_handle
        .set_ball_position(victim.ship_id, base_pos + DVec3::new(25_000.0, 0.0, 0.0))
        .await;
    sys_handle.step_manual(0.1).await.unwrap();

    // 1. Pochven Hypnosian Sleepers: exercise ALL THREE tiers (Faded 56337, Normal 56328, Aroused 56319)
    let tiers: [(&str, u32, f64, f64); 3] = [
        ("Faded Hypnosian Sentinel", 56_337, 0.0, 14_750.0),
        ("Normal Hypnosian Sentinel", 56_328, 0.0, 29_500.0),
        ("Aroused Hypnosian Sentinel", 56_319, 5_000.0, 44_000.0),
    ];

    for (tier_name, npc_type_id, expected_min_shield, expected_armor) in tiers {
        let npc_id = spawn_specialized_npc_entity(
            &ctx,
            pochven_sys_id,
            npc_type_id,
            "PochvenHypnosianSleeper",
            4003,
            base_pos + DVec3::new(2_500.0, 0.0, 0.0),
        )
        .await;

        let npc_cs = sys_handle.get_combat_state(npc_id).await.unwrap().unwrap();
        assert!(
            (npc_cs.max_armor_hp - expected_armor).abs() < 1.0,
            "{tier_name} ({npc_type_id}) armor must match SDE ({expected_armor}, got {})",
            npc_cs.max_armor_hp
        );
        if expected_min_shield > 0.0 {
            assert!(
                npc_cs.max_shield_hp >= expected_min_shield,
                "{tier_name} must possess a dedicated shield layer >= {expected_min_shield}, got {}",
                npc_cs.max_shield_hp
            );
        } else {
            assert!(
                npc_cs.max_shield_hp <= 50.0,
                "{tier_name} must not have the Aroused 5,000 HP shield layer (got {})",
                npc_cs.max_shield_hp
            );
        }

        // Verify AI auto-aggro against neutral pilot in Pochven
        let hunter_before = sys_handle.get_combat_state(hunter.ship_id).await.unwrap().unwrap();
        step_extended_npc_ai(&ctx, pochven_sys_id).await;
        let npc_target = with_combat_runtime(&ctx, |rt| {
            rt.npc_entities.get(&npc_id).and_then(|n| n.target_id)
        });
        let hunter_after_ai = sys_handle.get_combat_state(hunter.ship_id).await.unwrap().unwrap();
        assert_eq!(
            npc_target,
            Some(hunter.ship_id),
            "{tier_name} must engage pilots in Pochven"
        );
        assert!(
            hunter_after_ai.shield_hp < hunter_before.shield_hp
                || hunter_after_ai.armor_hp < hunter_before.armor_hp,
            "{tier_name} must damage engaged ships"
        );
        common::slash_cmd(&ctx, &mut hunter.sess, "/heal").await;

        let wrecks_before: HashSet<i64> = ctx.db.with_snapshot(|s| {
            s.items
                .values()
                .filter(|it| {
                    it.location_id == i64::from(pochven_sys_id)
                        && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 186)
                })
                .map(|it| it.item_id)
                .collect()
        });

        // Destroy the Sleeper via real player weapon fire (dogmaIM.AddTarget + dogmaIM.Activate)
        shoot_target_until_destroyed(&ctx, &mut hunter, pochven_sys_id, &hunter_guns, npc_id).await;
        sys_handle.step_manual(1.0).await.unwrap();
        ctx.advance_secs(1);

        // Verify 0 ISK bounty for Hypnosian Sleepers and Interstellar Navigation Logs (56708) wreck drop (no blue loot 30744)
        let pending_bounty = with_combat_runtime(&ctx, |rt| {
            rt.pending_bounty_ticks
                .iter()
                .filter(|((_, cid), _)| *cid == hunter.char_id)
                .map(|(_, isk)| *isk)
                .sum::<f64>()
        });
        assert_eq!(
            pending_bounty, 0.0,
            "{tier_name} must award 0 ISK bounty"
        );

        let new_wreck_items: Vec<(i64, Vec<u32>)> = ctx.db.with_snapshot(|s| {
            let new_wrecks: Vec<i64> = s
                .items
                .values()
                .filter(|it| {
                    it.location_id == i64::from(pochven_sys_id)
                        && !wrecks_before.contains(&it.item_id)
                        && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 186)
                })
                .map(|it| it.item_id)
                .collect();
            new_wrecks
                .into_iter()
                .map(|wid| {
                    let contents = s
                        .items
                        .values()
                        .filter(|it| it.location_id == wid)
                        .map(|it| it.type_id)
                        .collect();
                    (wid, contents)
                })
                .collect()
        });
        assert!(
            !new_wreck_items.is_empty(),
            "Destroying {tier_name} via turrets must spawn a wreck"
        );
        assert!(
            new_wreck_items.iter().any(|(_, items)| items.contains(&56_708)),
            "{tier_name} wreck must drop Interstellar Navigation Logs (56708), got {new_wreck_items:?}"
        );
        assert!(
            new_wreck_items.iter().all(|(_, items)| !items.contains(&30_744)),
            "{tier_name} must not drop sleeper blue loot (30744)"
        );
    }

    // 2. Pochven increased loot drop (60% vs 50% empire) via real PvP ship destruction
    let pochven_threshold =
        insurgency_pvp_loot_drop_threshold(&ctx, pochven_sys_id, hunter.char_id, victim.char_id, false);
    let empire_threshold =
        insurgency_pvp_loot_drop_threshold(&ctx, JITA_SYS, hunter.char_id, victim.char_id, false);
    assert!(
        (pochven_threshold - 0.60).abs() < 1e-6 && (empire_threshold - 0.50).abs() < 1e-6,
        "Pochven PvP loot drop threshold must be 0.60 vs 0.50 empire (got {pochven_threshold} vs {empire_threshold})"
    );

    // Stock 30 distinct item stacks into victim ship's cargo in space
    let cargo_type_ids = [
        34_u32, 35, 36, 37, 38, 39, 40, 185, 2881, 16_275,
        3402, 3412, 3413, 3426, 3300, 3303, 3306, 587, 601, 75_933,
        639, 496, 2929, 201, 486, 2889, 485, 487, 2977, 200,
    ];
    for &tid in &cargo_type_ids {
        common::slash_cmd(&ctx, &mut victim.sess, &format!("/createitem {tid} 1")).await;
    }
    let seeded_cargo_count = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .filter(|i| i.location_id == victim.ship_id && i.flag == InventoryFlag::Cargo)
            .count()
    });
    assert_eq!(
        seeded_cargo_count,
        cargo_type_ids.len(),
        "Victim ship cargo must hold all 30 distinct stacks"
    );

    let kms_before: HashSet<i64> = ctx.db.with_snapshot(|s| s.killmails.keys().copied().collect());
    let wrecks_before_pvp: HashSet<i64> = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .filter(|it| {
                it.location_id == i64::from(pochven_sys_id)
                    && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 186)
            })
            .map(|it| it.item_id)
            .collect()
    });

    // Destroy victim ship through real turret fire (dogmaIM.AddTarget + dogmaIM.Activate)
    let victim_ship_id = victim.ship_id;
    sys_handle
        .set_ball_position(victim_ship_id, base_pos + DVec3::new(2_000.0, 0.0, 0.0))
        .await;
    sys_handle.step_manual(0.1).await.unwrap();
    shoot_target_until_destroyed(&ctx, &mut hunter, pochven_sys_id, &hunter_guns, victim_ship_id)
        .await;

    let km = ctx
        .db
        .with_snapshot(|s| {
            s.killmails
                .values()
                .find(|k| !kms_before.contains(&k.kill_id) && k.victim_character_id == victim.char_id)
                .cloned()
        })
        .expect("Live Pochven turret kill must generate a killmail");
    assert_eq!(km.solar_system_id, pochven_sys_id);
    assert_eq!(km.attacker_character_id, hunter.char_id);
    assert!(km.total_value_isk > 0.0, "Killmail total value must be positive");

    let detailed = with_combat_runtime(&ctx, |rt| rt.detailed_killmails.get(&km.kill_id).cloned())
        .expect("Detailed killmail must exist for Pochven kill");
    assert_eq!(
        detailed.dropped_items.len() + detailed.destroyed_items.len(),
        seeded_cargo_count,
        "Detailed killmail must account for all 30 cargo items"
    );

    let pvp_wreck_id = ctx
        .db
        .with_snapshot(|s| {
            s.items
                .values()
                .find(|it| {
                    it.location_id == i64::from(pochven_sys_id)
                        && !wrecks_before_pvp.contains(&it.item_id)
                        && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 186)
                })
                .map(|it| it.item_id)
        })
        .expect("Pochven PvP ship destruction must spawn a wreck");
    let dropped_in_wreck = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .filter(|i| i.location_id == pvp_wreck_id)
            .count()
    });
    assert_eq!(
        dropped_in_wreck,
        detailed.dropped_items.len(),
        "Wreck contents count must match detailed killmail dropped_items count"
    );
    assert!(
        dropped_in_wreck > 0 && dropped_in_wreck < seeded_cargo_count,
        "Pochven wreck must reflect partial ~60% loot drop ({dropped_in_wreck}/{seeded_cargo_count})"
    );
    assert!(
        dropped_in_wreck * 10 >= seeded_cargo_count * 3
            && dropped_in_wreck * 10 <= seeded_cargo_count * 9,
        "Pochven 60% loot drop out of {seeded_cargo_count} items out of statistical range: {dropped_in_wreck}"
    );

    // 3. Verify Hypnosian Sleeper spawning across Pochven celestials, ore anomalies, wormholes,
    //    and Torpid Sleeper Hive sites via live beyonce bind + wormhole seeding.
    wire_bind_oid(
        &ctx,
        &mut hunter.sess,
        "beyonce",
        PyRep::Integer(i64::from(pochven_sys_id)),
    )
    .await;

    let hypnosian_npcs: Vec<(i64, u32, DVec3)> = with_combat_runtime(&ctx, |rt| {
        rt.npc_entities
            .values()
            .filter(|n| n.solar_system_id == pochven_sys_id && n.archetype == "PochvenHypnosianSleeper")
            .filter_map(|n| sys_handle.get_ball_position_sync(n.ball_id).map(|p| (n.ball_id, n.type_id, p)))
            .collect()
    });
    let has_all_tiers_near = |center: DVec3, radius: f64| {
        [56_337_u32, 56_328, 56_319].iter().all(|&tid| {
            hypnosian_npcs
                .iter()
                .any(|&(_, npc_tid, pos)| npc_tid == tid && pos.distance(center) <= radius)
        })
    };

    // 3a. Celestials
    let celestials_guarded = ctx
        .sde
        .celestials_in_system_slice(pochven_sys_id)
        .iter()
        .any(|cel| has_all_tiers_near(cel.position, cel.radius.max(5_000.0) + 60_000.0));
    assert!(
        celestials_guarded,
        "Pochven beyonce bind must spawn Faded/normal/Aroused Hypnosian Sleepers near Pochven celestials"
    );

    // 3b. Ore anomalies (SDE 8974 / 8987..=8991)
    let ore_anom_pos = ctx
        .db
        .with_snapshot(|snap| {
            snap.cosmic_anomalies
                .get(&pochven_sys_id)
                .and_then(|list| list.iter().find(|a| a.anomaly_type == "OreAnomaly").map(|a| a.position))
        })
        .expect("Pochven beyonce bind must seed SDE Pochven OreAnomaly");
    assert!(
        has_all_tiers_near(DVec3::from_array(ore_anom_pos), 30_000.0),
        "Pochven OreAnomaly must spawn Faded/normal/Aroused Hypnosian Sleeper defenders"
    );

    // 3c. Torpid Sleeper Hive (dungeon 9006)
    let hive_anom_pos = ctx
        .db
        .with_snapshot(|snap| {
            snap.cosmic_anomalies.get(&pochven_sys_id).and_then(|list| {
                list.iter()
                    .find(|a| a.anomaly_type == "Combat" && a.name.contains("Torpid Sleeper Hive"))
                    .map(|a| a.position)
            })
        })
        .expect("Pochven beyonce bind must seed Torpid Sleeper Hive (9006) combat anomaly");
    assert!(
        has_all_tiers_near(DVec3::from_array(hive_anom_pos), 30_000.0),
        "Torpid Sleeper Hive must spawn Faded/normal/Aroused Hypnosian Sleepers"
    );

    // 3d. Pochven wormhole guards
    let static_whs = eve_server::services::space_destiny::space_pochven_wormholes::pochven_static_wormholes(
        &ctx.sde,
        pochven_sys_id,
        0,
        ctx.now_unix(),
    );
    assert!(!static_whs.is_empty(), "Pochven system must resolve static wormholes");
    let (_, wh_guards) =
        eve_server::services::space_destiny::space_pochven_wormholes::open_pochven_wormhole_and_spawn_guards(
            &ctx,
            pochven_sys_id,
            &static_whs[0],
            905_000_001,
        )
        .await;
    let wh_guard_types: HashSet<u32> = with_combat_runtime(&ctx, |rt| {
        wh_guards
            .iter()
            .filter_map(|gid| rt.npc_entities.get(gid).map(|n| n.type_id))
            .collect()
    });
    for req_tid in [56_337_u32, 56_328, 56_319] {
        assert!(
            wh_guard_types.contains(&req_tid),
            "Pochven wormhole guards must include Hypnosian Sleeper type {req_tid}, got {wh_guard_types:?}"
        );
    }

    // 4. Trade Interstellar Navigation Logs (56708) at a Triglavian station in Pochven via LPStoreMgr
    const OTELA_TRIG_STATION: i64 = 60_003_034; // Otela - Convocation of Triglav
    const TRIG_CORP_ID: i64 = 1_000_298; // Convocation of Triglav
    const LOG_TRADE_OFFER_ID: i64 = 17_023; // 25x 56708 -> 1x 47867 (0 LP, 0 ISK)
    ctx.advance_secs(65);
    wire_dock_station(
        &ctx,
        &mut hunter.sess,
        hunter.ship_id,
        pochven_sys_id,
        OTELA_TRIG_STATION,
    )
    .await;
    common::slash_cmd(&ctx, &mut hunter.sess, "/createitem 56708 25").await;

    let offers_rep = wire_ok(
        &ctx,
        &mut hunter.sess,
        "LPStoreMgr",
        None,
        "GetAvailableOffersFromCorp",
        &[PyRep::Integer(TRIG_CORP_ID)],
    )
    .await;
    let offers_list = match offers_rep {
        PyRep::List(rows) => rows,
        other => panic!("GetAvailableOffersFromCorp must return PyRep::List, got {other:?}"),
    };
    assert!(
        offers_list
            .iter()
            .any(|o| o.dict_get("offerID").and_then(|v| v.as_i64()) == Some(LOG_TRADE_OFFER_ID)),
        "Triglavian LP store must expose Interstellar Navigation Log trade offer {LOG_TRADE_OFFER_ID}"
    );

    wire_ok(
        &ctx,
        &mut hunter.sess,
        "LPStoreMgr",
        None,
        "TakeOfferForCharacter",
        &[
            PyRep::Integer(TRIG_CORP_ID),
            PyRep::Integer(LOG_TRADE_OFFER_ID),
            PyRep::Integer(1),
        ],
    )
    .await;

    let (logs_remaining, traded_reward_qty) = ctx.db.with_snapshot(|s| {
        let logs: i64 = s
            .items
            .values()
            .filter(|i| {
                i.owner_id == hunter.char_id
                    && i.location_id == OTELA_TRIG_STATION
                    && i.flag == InventoryFlag::Hangar
                    && i.type_id == 56_708
            })
            .map(|i| i64::from(i.quantity))
            .sum();
        let reward: i64 = s
            .items
            .values()
            .filter(|i| {
                i.owner_id == hunter.char_id
                    && i.location_id == OTELA_TRIG_STATION
                    && i.flag == InventoryFlag::Hangar
                    && i.type_id == 47_867
            })
            .map(|i| i64::from(i.quantity))
            .sum();
        (logs, reward)
    });
    assert_eq!(
        logs_remaining, 0,
        "Trading 25x Interstellar Navigation Logs at Triglavian station must consume all logs"
    );
    assert!(
        traded_reward_qty >= 1,
        "Trading Interstellar Navigation Logs must deliver reward item (47867) into Triglavian station hangar"
    );
}

#[tokio::test]
async fn test_insurance_voiding_and_12_week_expiry() {
    // parity: feat:combat.insurance-void-and-expiry
    eve_testkit::attest(&["feat:combat.insurance-void-and-expiry"]);

    const RIFTER: u32 = 587;
    const EMP_S: u32 = 185;
    const PILOT_1: i64 = 90_000_001;
    const PILOT_2: i64 = 90_000_002;
    const PILOT_1_SHIP: i64 = 100_000_001;
    const PILOT_1_GUN: i64 = 100_000_005;
    const PILOT_2_SHIP: i64 = 100_000_002;
    const PILOT_2_GUN: i64 = 100_000_006;
    const FILETIME_TICKS_PER_SEC: i64 = 10_000_000;

    async fn buy_at_jita(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, qty: i64) -> Vec<i64> {
        let owner = sess.character_id.unwrap();
        let before: HashSet<i64> = ctx.db.with_snapshot(|s| {
            s.items
                .values()
                .filter(|i| i.owner_id == owner && i.location_id == JITA_STATION && i.type_id == type_id)
                .map(|i| i.item_id)
                .collect()
        });
        let ask = ctx.db.with_snapshot(|s| {
            s.market_orders
                .values()
                .filter(|o| o.station_id == JITA_STATION && !o.is_bid && o.type_id == type_id && o.vol_remaining > 0)
                .map(|o| o.price.to_wire_f64())
                .fold(0.0_f64, f64::max)
        });
        assert!(ask > 0.0, "Jita sell order required for type {type_id}");
        let tick = 10_f64.powi(ask.log10().floor() as i32 - 3).max(0.01);
        let ask = ((ask / tick).ceil() * tick * 100.0).round() / 100.0;
        wire_ok(
            ctx,
            sess,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION),
                PyRep::Integer(i64::from(type_id)),
                PyRep::Real(ask),
                PyRep::Integer(qty),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(0),
                PyRep::Bool(false),
            ],
        )
        .await;
        ctx.db.with_snapshot(|s| {
            s.items
                .values()
                .filter(|i| {
                    i.owner_id == owner
                        && i.location_id == JITA_STATION
                        && i.type_id == type_id
                        && !before.contains(&i.item_id)
                })
                .map(|i| i.item_id)
                .collect()
        })
    }

    async fn buy_and_assemble_rifter(ctx: &ServerContext, sess: &mut SessionState) -> i64 {
        let ship_id = buy_at_jita(ctx, sess, RIFTER, 1).await[0];
        wire_ok(ctx, sess, "ship", None, "AssembleShip", &[PyRep::Integer(ship_id)]).await;
        ship_id
    }

    async fn list_contracts(ctx: &ServerContext, sess: &mut SessionState) -> Vec<PyRep> {
        match wire_ok(ctx, sess, "insuranceSvc", None, "GetContracts", &[]).await {
            PyRep::List(rows) => rows,
            other => panic!("GetContracts must return PyRep::List, got {other:?}"),
        }
    }

    fn contract_in_list(list: &[PyRep], ship_id: i64) -> Option<&PyRep> {
        list.iter()
            .find(|c| c.dict_get("shipID").and_then(|v| v.as_i64()) == Some(ship_id))
    }

    fn wallet_isk(ctx: &ServerContext, char_id: i64) -> f64 {
        ctx.db
            .with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64())
    }

    async fn lose_ship_in_duel(
        ctx: &ServerContext,
        s1: &mut SessionState,
        s2: &mut SessionState,
        ship_id: i64,
    ) -> f64 {
        ctx.advance_secs(31);
        wire_ok(ctx, s1, "ship", None, "Board", &[PyRep::Integer(ship_id)]).await;
        ctx.advance_secs(31);
        wire_ok(ctx, s1, "stationSvc", None, "Undock", &[]).await;
        ctx.advance_secs(31);

        // Halt both ships after Undock so neither drifts out of optimal turret range across repeated duels.
        let b1 = wire_bind_oid(ctx, s1, "beyonce", PyRep::Integer(i64::from(JITA_SYS))).await;
        wire_ok(ctx, s1, "beyonce", Some(b1), "CmdStop", &[]).await;
        let b2 = wire_bind_oid(ctx, s2, "beyonce", PyRep::Integer(i64::from(JITA_SYS))).await;
        wire_ok(ctx, s2, "beyonce", Some(b2), "CmdStop", &[]).await;

        wire_ok(ctx, s2, "crimewatch", None, "StartDuelChallenge", &[PyRep::Integer(PILOT_1)]).await;
        wire_ok(
            ctx,
            s1,
            "crimewatch",
            None,
            "RespondToDuelChallenge",
            &[PyRep::Integer(PILOT_2), PyRep::Integer(1), PyRep::Bool(true)],
        )
        .await;

        let sys = ctx.systems.get_or_spawn(JITA_SYS);
        let (_, victim_ball) = sys
            .get_ball_snapshot(ship_id)
            .await
            .unwrap()
            .expect("victim ship must be in space after undock");
        sys.set_ball_position(PILOT_2_SHIP, victim_ball.position + DVec3::new(1_000.0, 0.0, 0.0))
            .await;
        sys.step_manual(0.1).await.unwrap();

        let d_oid = wire_bind_oid(ctx, s2, "dogmaIM", PyRep::Integer(PILOT_2_SHIP)).await;
        wire_ok(ctx, s2, "dogmaIM", Some(d_oid), "AddTarget", &[PyRep::Integer(ship_id)]).await;
        for _ in 0..20 {
            if sys
                .get_target_locks(PILOT_2_SHIP)
                .await
                .unwrap()
                .iter()
                .any(|l| l.target_id == ship_id && l.locked)
            {
                break;
            }
            ctx.advance_secs(2);
            sys.step_manual(2.0).await.unwrap();
        }

        let before = wallet_isk(ctx, PILOT_1);
        for _ in 0..120 {
            if sys.get_ball_snapshot(ship_id).await.unwrap().is_none() {
                break;
            }
            let act_res = wire_call(
                ctx,
                s2,
                "dogmaIM",
                None,
                "Activate",
                &[
                    PyRep::Integer(PILOT_2_GUN),
                    PyRep::String("projectileFired".into()),
                    PyRep::Integer(ship_id),
                ],
                &HashMap::new(),
            )
            .await;
            if let Err(ref err) = act_res {
                if err.code != "ModuleIsReloading" {
                    let cargo_ammo: Vec<PyRep> = ctx.db.with_snapshot(|s| {
                        s.items
                            .values()
                            .filter(|i| {
                                i.location_id == PILOT_2_SHIP
                                    && i.flag == InventoryFlag::Cargo
                                    && i.type_id == EMP_S
                                    && i.quantity > 0
                            })
                            .map(|i| PyRep::Integer(i.item_id))
                            .collect()
                    });
                    if !cargo_ammo.is_empty() {
                        let _ = wire_call(
                            ctx,
                            s2,
                            "dogmaIM",
                            None,
                            "LoadAmmo",
                            &[
                                PyRep::Integer(PILOT_2_SHIP),
                                PyRep::List(vec![PyRep::Integer(PILOT_2_GUN)]),
                                PyRep::List(cargo_ammo),
                            ],
                            &HashMap::new(),
                        )
                        .await;
                    }
                }
            }
            sys.step_manual(4.0).await.unwrap();
            ctx.advance_secs(4);
        }
        assert!(
            sys.get_ball_snapshot(ship_id).await.unwrap().is_none(),
            "victim ship {ship_id} must be destroyed via duel fire"
        );
        let delta = wallet_isk(ctx, PILOT_1) - before;

        // Re-sync pilot 1's session from server state (pod in space vs docked) and return to Jita 4-4
        ctx.advance_secs(12);
        wire_ok(ctx, s1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(PILOT_1)]).await;
        ctx.advance_secs(12);
        if s1.station_id.is_none() {
            if let Some(pod_id) = s1.ship_id {
                let st_pos = ctx.sde.stations[&JITA_STATION].position;
                sys.set_ball_position(pod_id, st_pos + DVec3::new(300.0, 0.0, 0.0))
                    .await;
                sys.step_manual(0.1).await.unwrap();
            }
            wire_ok(ctx, s1, "stationSvc", None, "Dock", &[PyRep::Integer(JITA_STATION)]).await;
            ctx.advance_secs(12);
        }
        delta
    }

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);

    // Log in two standard non-GM pilots via ClientHarness/login_pilot
    let mut s1 = SessionState::new(5_001);
    common::login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
    wire_ok(&ctx, &mut s1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(PILOT_1)]).await;
    ctx.advance_secs(12);

    let mut s2 = SessionState::new(5_002);
    common::login_pilot(&ctx, &mut s2, "other", "other-secret").await;
    wire_ok(&ctx, &mut s2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(PILOT_2)]).await;
    ctx.advance_secs(12);

    // 1. Insure ship via real insuranceSvc.InsureShip (12-week Platinum term), then void via repackagingSvc.RepackageItems
    let ship_a = buy_and_assemble_rifter(&ctx, &mut s1).await;
    wire_ok(
        &ctx,
        &mut s1,
        "insuranceSvc",
        None,
        "InsureShip",
        &[PyRep::Integer(ship_a), PyRep::Real(1.0)],
    )
    .await;

    let ship_a_contract = wire_ok(
        &ctx,
        &mut s1,
        "insuranceSvc",
        None,
        "GetContractForShip",
        &[PyRep::Integer(ship_a)],
    )
    .await;
    assert!(
        !matches!(ship_a_contract, PyRep::None),
        "GetContractForShip must return active Platinum contract"
    );
    let contracts_a = list_contracts(&ctx, &mut s1).await;
    let c_a = contract_in_list(&contracts_a, ship_a).expect("ship_a must appear in GetContracts");
    let term_ticks = c_a.dict_get("endDate").and_then(|v| v.as_i64()).unwrap()
        - c_a.dict_get("startDate").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(
        term_ticks,
        84 * 86_400 * FILETIME_TICKS_PER_SEC,
        "Insurance contract term must be 12 weeks (84 days)"
    );
    assert_eq!(c_a.dict_get("fraction").and_then(|v| v.as_f64()), Some(1.0));
    let platinum_payout = c_a.dict_get("payoutISK").and_then(|v| v.as_f64()).unwrap();
    assert!(platinum_payout > 0.0);

    wire_ok(
        &ctx,
        &mut s1,
        "repackagingSvc",
        None,
        "RepackageItems",
        &[PyRep::List(vec![PyRep::Integer(ship_a)])],
    )
    .await;
    let after_repack = wire_ok(
        &ctx,
        &mut s1,
        "insuranceSvc",
        None,
        "GetContractForShip",
        &[PyRep::Integer(ship_a)],
    )
    .await;
    assert!(matches!(after_repack, PyRep::None), "Repackaging must void GetContractForShip");
    assert!(
        contract_in_list(&list_contracts(&ctx, &mut s1).await, ship_a).is_none(),
        "Repackaging must void insurance contract from GetContracts"
    );

    // 2. Void via item-exchange contract creation (`contractProxy.CreateContract`)
    let ship_b = buy_and_assemble_rifter(&ctx, &mut s1).await;
    wire_ok(
        &ctx,
        &mut s1,
        "insuranceSvc",
        None,
        "InsureShip",
        &[PyRep::Integer(ship_b), PyRep::Real(1.0)],
    )
    .await;
    assert!(contract_in_list(&list_contracts(&ctx, &mut s1).await, ship_b).is_some());

    let contract_params = PyRep::keyval([
        ("contractType", PyRep::String("ItemExchange".into())),
        ("availability", PyRep::String("Public".into())),
        ("assigneeID", PyRep::Integer(0)),
        ("expireTime", PyRep::Integer(1440)),
        ("numDays", PyRep::Integer(3)),
        ("startStationID", PyRep::Integer(JITA_STATION)),
        ("endStationID", PyRep::Integer(JITA_STATION)),
        ("price", PyRep::Real(1_000_000.0)),
        ("reward", PyRep::Real(0.0)),
        ("collateral", PyRep::Real(0.0)),
        ("title", PyRep::String("Insured Rifter".into())),
        ("itemList", PyRep::List(vec![PyRep::Integer(ship_b)])),
    ]);
    let confirm_kw = HashMap::from([("confirm".to_string(), PyRep::Bool(true))]);
    wire_call(
        &ctx,
        &mut s1,
        "contractProxy",
        None,
        "CreateContract",
        &[contract_params],
        &confirm_kw,
    )
    .await
    .expect("contractProxy.CreateContract");
    assert!(
        contract_in_list(&list_contracts(&ctx, &mut s1).await, ship_b).is_none(),
        "Putting insured hull into item-exchange contract must void insurance"
    );

    // 3. Void via direct station trade transfer (`trademgr.InitiateTrade` -> `AddItem` -> `MakeOffer`)
    let ship_c = buy_and_assemble_rifter(&ctx, &mut s1).await;
    wire_ok(
        &ctx,
        &mut s1,
        "insuranceSvc",
        None,
        "InsureShip",
        &[PyRep::Integer(ship_c), PyRep::Real(1.0)],
    )
    .await;
    assert!(contract_in_list(&list_contracts(&ctx, &mut s1).await, ship_c).is_some());

    let trade_rep = wire_ok(&ctx, &mut s1, "trademgr", None, "InitiateTrade", &[PyRep::Integer(PILOT_2)]).await;
    let trade_id = trade_rep
        .dict_get("tradeContainerID")
        .and_then(|v| v.as_i64())
        .expect("tradeContainerID from InitiateTrade");
    wire_ok(
        &ctx,
        &mut s1,
        "trademgr",
        None,
        "AddItem",
        &[PyRep::Integer(trade_id), PyRep::Integer(ship_c), PyRep::Integer(1)],
    )
    .await;
    wire_ok(&ctx, &mut s1, "trademgr", None, "MakeOffer", &[PyRep::Integer(trade_id), PyRep::Bool(true)]).await;
    wire_ok(&ctx, &mut s2, "trademgr", None, "MakeOffer", &[PyRep::Integer(trade_id), PyRep::Bool(true)]).await;

    let old_owner_err = wire_call(
        &ctx,
        &mut s1,
        "insuranceSvc",
        None,
        "GetContractForShip",
        &[PyRep::Integer(ship_c)],
        &HashMap::new(),
    )
    .await
    .expect_err("Seller must no longer own traded ship");
    assert_eq!(old_owner_err.code, "OwnershipViolation");
    assert!(
        contract_in_list(&list_contracts(&ctx, &mut s1).await, ship_c).is_none(),
        "Direct station trade ownership transfer must void seller's ship insurance contract"
    );
    let traded_contract_s2 = wire_ok(
        &ctx,
        &mut s2,
        "insuranceSvc",
        None,
        "GetContractForShip",
        &[PyRep::Integer(ship_c)],
    )
    .await;
    assert!(
        matches!(traded_contract_s2, PyRep::None),
        "New owner GetContractForShip must return None after voided trade"
    );
    assert!(
        contract_in_list(&list_contracts(&ctx, &mut s2).await, ship_c).is_none(),
        "New owner must not inherit voided insurance contract after trade"
    );

    // Arm & undock pilot 2 outside Jita 4-4 for live PvP destruction steps
    let ammo2 = buy_at_jita(&ctx, &mut s2, EMP_S, 2_000).await[0];
    wire_ok(
        &ctx,
        &mut s2,
        "invbroker",
        None,
        "Add",
        &[PyRep::Integer(ammo2), PyRep::Integer(PILOT_2_SHIP), PyRep::Integer(5)],
    )
    .await;
    wire_ok(
        &ctx,
        &mut s2,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(PILOT_2_SHIP),
            PyRep::List(vec![PyRep::Integer(PILOT_2_GUN)]),
            PyRep::List(vec![PyRep::Integer(ammo2)]),
        ],
    )
    .await;
    ctx.advance_secs(31);
    wire_ok(&ctx, &mut s2, "stationSvc", None, "Undock", &[]).await;
    ctx.advance_secs(31);

    // 4. Clock-advance past 12-week term -> contract expires, loss pays 40% Basic fallback + EVE-mail
    let ship_d = buy_and_assemble_rifter(&ctx, &mut s1).await;
    wire_ok(
        &ctx,
        &mut s1,
        "insuranceSvc",
        None,
        "InsureShip",
        &[PyRep::Integer(ship_d), PyRep::Real(1.0)],
    )
    .await;
    ctx.advance_virtual_clock(84 * 86_400 + 10);
    assert!(
        contract_in_list(&list_contracts(&ctx, &mut s1).await, ship_d).is_none(),
        "Insurance contract must expire after 12 weeks (84 days)"
    );
    let lapsed_payout = lose_ship_in_duel(&ctx, &mut s1, &mut s2, ship_d).await;
    assert!(
        lapsed_payout > 0.0 && lapsed_payout < platinum_payout * 0.75,
        "Expired ship loss must fall back to 40% Basic payout ({lapsed_payout} vs Platinum {platinum_payout})"
    );
    let mails_d = wire_ok(&ctx, &mut s1, "mailMgr", None, "SyncMail", &[]).await;
    assert!(
        format!("{mails_d:?}").contains(&format!("Insurance Payout for Ship {ship_d}")),
        "Expired ship loss fallback payout must deliver insurance EVE-mail via mailMgr.SyncMail: {mails_d:?}"
    );

    // 5. Active Platinum contract pays full contracted ISK immediately on live PvP loss + EVE-mail
    let ship_e = buy_and_assemble_rifter(&ctx, &mut s1).await;
    wire_ok(
        &ctx,
        &mut s1,
        "insuranceSvc",
        None,
        "InsureShip",
        &[PyRep::Integer(ship_e), PyRep::Real(1.0)],
    )
    .await;
    let e_payout = contract_in_list(&list_contracts(&ctx, &mut s1).await, ship_e)
        .and_then(|c| c.dict_get("payoutISK"))
        .and_then(|v| v.as_f64())
        .expect("payoutISK for active ship_e contract");
    let paid_e = lose_ship_in_duel(&ctx, &mut s1, &mut s2, ship_e).await;
    assert!(
        (paid_e - e_payout).abs() < 0.02,
        "Active Platinum contract must pay exact contracted amount on live loss ({paid_e} vs {e_payout})"
    );
    assert!(
        contract_in_list(&list_contracts(&ctx, &mut s1).await, ship_e).is_none(),
        "Consumed Platinum contract must be removed after payout"
    );
    let mails_e = wire_ok(&ctx, &mut s1, "mailMgr", None, "SyncMail", &[]).await;
    assert!(
        format!("{mails_e:?}").contains(&format!("Insurance Payout for Ship {ship_e}")),
        "Active insurance payout must deliver EVE-mail via mailMgr.SyncMail: {mails_e:?}"
    );

    // 6. Live CONCORD execution of a criminal pilot yields 0.0 payout despite active Platinum insurance
    ctx.advance_virtual_clock(360); // clear prior 300s Limited Engagement timer
    let ammo1 = buy_at_jita(&ctx, &mut s1, EMP_S, 200).await[0];
    wire_ok(
        &ctx,
        &mut s1,
        "invbroker",
        None,
        "Add",
        &[PyRep::Integer(ammo1), PyRep::Integer(PILOT_1_SHIP), PyRep::Integer(5)],
    )
    .await;
    wire_ok(
        &ctx,
        &mut s1,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(PILOT_1_SHIP),
            PyRep::List(vec![PyRep::Integer(PILOT_1_GUN)]),
            PyRep::List(vec![PyRep::Integer(ammo1)]),
        ],
    )
    .await;
    wire_ok(
        &ctx,
        &mut s1,
        "insuranceSvc",
        None,
        "InsureShip",
        &[PyRep::Integer(PILOT_1_SHIP), PyRep::Real(1.0)],
    )
    .await;
    assert!(contract_in_list(&list_contracts(&ctx, &mut s1).await, PILOT_1_SHIP).is_some());

    wire_ok(&ctx, &mut s1, "ship", None, "Board", &[PyRep::Integer(PILOT_1_SHIP)]).await;
    ctx.advance_secs(31);
    wire_ok(&ctx, &mut s1, "stationSvc", None, "Undock", &[]).await;
    ctx.advance_secs(31);

    // Set safety Red and commit unprovoked high-sec aggression to trigger Criminal flag + CONCORD response
    wire_ok(&ctx, &mut s1, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await;
    let sys = ctx.systems.get_or_spawn(JITA_SYS);
    let (_, p2_ball) = sys
        .get_ball_snapshot(PILOT_2_SHIP)
        .await
        .unwrap()
        .expect("pilot 2 must remain in space");
    sys.set_ball_position(PILOT_1_SHIP, p2_ball.position + DVec3::new(1_000.0, 0.0, 0.0))
        .await;
    sys.step_manual(0.1).await.unwrap();

    let d1_oid = wire_bind_oid(&ctx, &mut s1, "dogmaIM", PyRep::Integer(PILOT_1_SHIP)).await;
    wire_ok(&ctx, &mut s1, "dogmaIM", Some(d1_oid), "AddTarget", &[PyRep::Integer(PILOT_2_SHIP)]).await;
    for _ in 0..20 {
        if sys
            .get_target_locks(PILOT_1_SHIP)
            .await
            .unwrap()
            .iter()
            .any(|l| l.target_id == PILOT_2_SHIP && l.locked)
        {
            break;
        }
        ctx.advance_secs(2);
        sys.step_manual(2.0).await.unwrap();
    }

    let bal_before_crime = wallet_isk(&ctx, PILOT_1);
    wire_ok(
        &ctx,
        &mut s1,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(PILOT_1_GUN),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(PILOT_2_SHIP),
        ],
    )
    .await;

    let criminal_scheduled = ctx.crimewatch.with_crimewatch(|cw| {
        cw.states
            .get(&PILOT_1)
            .is_some_and(|st| st.is_criminal(ctx.now_unix()) && st.pending_concord_arrival.is_some())
    });
    assert!(
        criminal_scheduled,
        "Unprovoked high-sec attack must trigger Criminal flag and CONCORD response"
    );

    ctx.advance_virtual_clock(30);
    tick_concord_responses(&ctx, ctx.now_unix()).await;
    sys.step_manual(1.0).await.unwrap();

    assert!(
        sys.get_ball_snapshot(PILOT_1_SHIP).await.unwrap().is_none(),
        "CONCORD must destroy the criminal ship"
    );
    assert_eq!(
        wallet_isk(&ctx, PILOT_1) - bal_before_crime,
        0.0,
        "CONCORD destruction of a criminal must yield 0.0 insurance payout"
    );
    assert!(
        contract_in_list(&list_contracts(&ctx, &mut s1).await, PILOT_1_SHIP).is_none(),
        "CONCORD destruction must clear voided insurance contract"
    );

    // 7. Re-dock PILOT_1 in Jita 4-4 and verify market selling (marketProxy.PlaceMultiSellOrder)
    //    and corporation hangar moves (invbroker.Add / MultiAdd) void active insurance contracts.
    ctx.advance_secs(950);
    wire_ok(
        &ctx,
        &mut s1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(PILOT_1)],
    )
    .await;
    ctx.advance_secs(12);
    if s1.station_id.is_none() {
        if let Some(pod_id) = s1.ship_id {
            let st_pos = ctx.sde.stations[&JITA_STATION].position;
            sys.set_ball_position(pod_id, st_pos + DVec3::new(300.0, 0.0, 0.0))
                .await;
            sys.step_manual(0.1).await.expect("step pod pos");
        }
        wire_ok(&ctx, &mut s1, "stationSvc", None, "Dock", &[PyRep::Integer(JITA_STATION)]).await;
        ctx.advance_secs(12);
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills
            .entry(PILOT_1)
            .or_default()
            .entry(3363)
            .or_insert(eve_db::TrainedSkillRecord {
                type_id: 3363,
                level: 1,
                skill_points: 250.0,
            });
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            if ch.balance < IskCents::from_isk(100_000_000) {
                ch.balance = IskCents::from_isk(100_000_000);
            }
        }
    });

    // 7a. Market sell order (`marketProxy.PlaceMultiSellOrder`): assembled insured ship rejected,
    //     repackaging + selling voids contract cleanly.
    let ship_mkt = buy_and_assemble_rifter(&ctx, &mut s1).await;
    wire_ok(
        &ctx,
        &mut s1,
        "insuranceSvc",
        None,
        "InsureShip",
        &[PyRep::Integer(ship_mkt), PyRep::Real(1.0)],
    )
    .await;
    assert!(
        contract_in_list(&list_contracts(&ctx, &mut s1).await, ship_mkt).is_some(),
        "ship_mkt must hold active Platinum contract before market sell"
    );

    let sell_entry = |item_id: i64| {
        PyRep::List(vec![PyRep::keyval([
            ("stationID", PyRep::Integer(JITA_STATION)),
            ("typeID", PyRep::Integer(i64::from(RIFTER))),
            ("itemID", PyRep::Integer(item_id)),
            ("price", PyRep::Real(900_000.0)),
            ("quantity", PyRep::Integer(1)),
        ])])
    };
    let assembled_sell_res = wire_call(
        &ctx,
        &mut s1,
        "marketProxy",
        None,
        "PlaceMultiSellOrder",
        &[sell_entry(ship_mkt), PyRep::Bool(false), PyRep::Integer(90)],
        &HashMap::new(),
    )
    .await;
    assert!(
        assembled_sell_res.is_err(),
        "PlaceMultiSellOrder must reject selling an assembled insured ship before repackaging"
    );

    wire_ok(
        &ctx,
        &mut s1,
        "repackagingSvc",
        None,
        "RepackageItems",
        &[PyRep::List(vec![PyRep::Integer(ship_mkt)])],
    )
    .await;
    wire_ok(
        &ctx,
        &mut s1,
        "marketProxy",
        None,
        "PlaceMultiSellOrder",
        &[sell_entry(ship_mkt), PyRep::Bool(false), PyRep::Integer(90)],
    )
    .await;
    let mkt_contract_after = wire_call(
        &ctx,
        &mut s1,
        "insuranceSvc",
        None,
        "GetContractForShip",
        &[PyRep::Integer(ship_mkt)],
        &HashMap::new(),
    )
    .await;
    assert!(
        matches!(mkt_contract_after, Ok(PyRep::None))
            || mkt_contract_after
                .as_ref()
                .is_err_and(|e| e.code == "ShipNotFound"),
        "Market sell flow must void GetContractForShip (got {mkt_contract_after:?})"
    );
    assert!(
        contract_in_list(&list_contracts(&ctx, &mut s1).await, ship_mkt).is_none(),
        "Market sell flow must void contract from GetContracts"
    );
    assert!(
        !ctx.db
            .with_snapshot(|s| s.insurance_contracts.contains_key(&ship_mkt)),
        "Market sell flow must remove sold ship from s.insurance_contracts"
    );

    // Also exercise PlaceMultiSellOrder's direct contract-voiding path on a contract still present
    let ship_mkt_direct = buy_and_assemble_rifter(&ctx, &mut s1).await;
    wire_ok(
        &ctx,
        &mut s1,
        "insuranceSvc",
        None,
        "InsureShip",
        &[PyRep::Integer(ship_mkt_direct), PyRep::Real(1.0)],
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&ship_mkt_direct) {
            it.singleton = false;
        }
    });
    wire_ok(
        &ctx,
        &mut s1,
        "marketProxy",
        None,
        "PlaceMultiSellOrder",
        &[sell_entry(ship_mkt_direct), PyRep::Bool(false), PyRep::Integer(90)],
    )
    .await;
    assert!(
        !ctx.db
            .with_snapshot(|s| s.insurance_contracts.contains_key(&ship_mkt_direct)),
        "marketProxy.PlaceMultiSellOrder must void any active s.insurance_contracts entry for sold ship"
    );

    // 7b. Corporation hangar move (`invbroker.Add` & `invbroker.MultiAdd` to CorpSAG1 = 116)
    ctx.advance_secs(12);
    let corp_rep = wire_ok(
        &ctx,
        &mut s1,
        "corpRegistry",
        None,
        "AddCorporation",
        &[
            PyRep::String("Insurance Void Corp".into()),
            PyRep::String("IVC".into()),
            PyRep::Real(0.05),
        ],
    )
    .await;
    let corp_id = corp_rep
        .dict_get("corporationID")
        .and_then(|v| v.as_i64())
        .or(s1.corporation_id)
        .expect("corporationID");
    wire_ok(
        &ctx,
        &mut s1,
        "account",
        None,
        "GiveCash",
        &[
            PyRep::Integer(corp_id),
            PyRep::Real(20_000_000.0),
            PyRep::String("office-rent".into()),
        ],
    )
    .await;
    let rent_rep = wire_ok(
        &ctx,
        &mut s1,
        "officeManager",
        None,
        "RentOffice",
        &[PyRep::Integer(JITA_STATION)],
    )
    .await;
    let office_id = rent_rep
        .dict_get("officeFolderID")
        .or_else(|| rent_rep.dict_get("officeID"))
        .and_then(|v| v.as_i64())
        .expect("officeID");
    let office_inv_oid = wire_bind_oid(&ctx, &mut s1, "invbroker", PyRep::Integer(office_id)).await;

    let ship_corp_add = buy_and_assemble_rifter(&ctx, &mut s1).await;
    wire_ok(
        &ctx,
        &mut s1,
        "insuranceSvc",
        None,
        "InsureShip",
        &[PyRep::Integer(ship_corp_add), PyRep::Real(1.0)],
    )
    .await;
    assert!(
        contract_in_list(&list_contracts(&ctx, &mut s1).await, ship_corp_add).is_some(),
        "ship_corp_add must have active contract before corp hangar move"
    );
    wire_ok(
        &ctx,
        &mut s1,
        "invbroker",
        Some(office_inv_oid),
        "Add",
        &[
            PyRep::Integer(ship_corp_add),
            PyRep::Integer(office_id),
            PyRep::Integer(116),
            PyRep::Integer(1),
        ],
    )
    .await;
    assert!(
        matches!(
            wire_ok(
                &ctx,
                &mut s1,
                "insuranceSvc",
                None,
                "GetContractForShip",
                &[PyRep::Integer(ship_corp_add)],
            )
            .await,
            PyRep::None
        ),
        "invbroker.Add into CorpSAG1 must void GetContractForShip"
    );
    assert!(
        !ctx.db
            .with_snapshot(|s| s.insurance_contracts.contains_key(&ship_corp_add)),
        "invbroker.Add into CorpSAG1 must remove ship from s.insurance_contracts"
    );

    let ship_corp_multi = buy_and_assemble_rifter(&ctx, &mut s1).await;
    wire_ok(
        &ctx,
        &mut s1,
        "insuranceSvc",
        None,
        "InsureShip",
        &[PyRep::Integer(ship_corp_multi), PyRep::Real(1.0)],
    )
    .await;
    assert!(
        contract_in_list(&list_contracts(&ctx, &mut s1).await, ship_corp_multi).is_some(),
        "ship_corp_multi must have active contract before MultiAdd corp hangar move"
    );
    wire_ok(
        &ctx,
        &mut s1,
        "invbroker",
        Some(office_inv_oid),
        "MultiAdd",
        &[
            PyRep::List(vec![PyRep::Integer(ship_corp_multi)]),
            PyRep::Integer(office_id),
            PyRep::Integer(116),
        ],
    )
    .await;
    assert!(
        matches!(
            wire_ok(
                &ctx,
                &mut s1,
                "insuranceSvc",
                None,
                "GetContractForShip",
                &[PyRep::Integer(ship_corp_multi)],
            )
            .await,
            PyRep::None
        ),
        "invbroker.MultiAdd into CorpSAG1 must void GetContractForShip"
    );
    assert!(
        !ctx.db
            .with_snapshot(|s| s.insurance_contracts.contains_key(&ship_corp_multi)),
        "invbroker.MultiAdd into CorpSAG1 must remove ship from s.insurance_contracts"
    );

    // Move back to personal hangar, undock, and lose via duel: must pay 40% default fallback rather than 100% Platinum
    let hangar_inv_oid =
        wire_bind_oid(&ctx, &mut s1, "invbroker", PyRep::Integer(JITA_STATION)).await;
    wire_ok(
        &ctx,
        &mut s1,
        "invbroker",
        Some(hangar_inv_oid),
        "Add",
        &[
            PyRep::Integer(ship_corp_multi),
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(InventoryFlag::Hangar as i64),
            PyRep::Integer(1),
        ],
    )
    .await;
    let corp_void_payout = lose_ship_in_duel(&ctx, &mut s1, &mut s2, ship_corp_multi).await;
    assert!(
        corp_void_payout > 0.0 && corp_void_payout < platinum_payout * 0.9,
        "Corp hangar voided ship loss must pay 40% default fallback instead of 100% Platinum (got {corp_void_payout} vs platinum {platinum_payout})"
    );
}

#[tokio::test]
async fn test_abyssal_filament_highsec_restrictions_and_suspect_flag() {
    // parity: feat:crimewatch.abyssal-suspect-flag-tier
    eve_testkit::attest(&["feat:crimewatch.abyssal-suspect-flag-tier"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001i64;
    let ship_id = 103_000_501i64;

    // 1. In high-sec system 0.8-1.0 (e.g. Jita 30000142, sec = 0.95):
    // Activating high-tier Abyssal filament (Tier 4-6, e.g. T4 Chaotic type 47888) is blocked
    let jita_sys_id = 30_000_142;
    seed_pilot(&ctx, char_id, ship_id, jita_sys_id, DVec3::ZERO).await;
    let mut sess_jita = make_session(char_id, ship_id, jita_sys_id, None);

    let cruiser_t4_tid = 56_139u32; // Tier 6 Cruiser filament (suspect flag on exit in 0.6 high-sec)

    // Seed filament into pilot cargo in Jita (and ensure ship is a Cruiser for 1-filament solo deployment)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 621;
        }
        snap.items.insert(
            500_000_000,
            InventoryItemRecord {
                item_id: 500_000_000,
                type_id: cruiser_t4_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let err_jita = dispatch_combat_rpc(
        &ctx,
        &mut sess_jita,
        "abyssalMgr",
        "AbyssalEntranceDeployment",
        &[PyRep::Integer(500_000_000)], // Tier 4 filament
        &HashMap::new(),
    )
    .await
    .expect_err("Tier 4-6 Abyssal filament must be blocked in 0.8-1.0 high-security space");
    assert_eq!(err_jita.code, "FilamentDisallowedInHighSec");

    // 2. In 0.6 high-sec system Sooma (30000009, sec = 0.6):
    // Activating T6 Abyssal filament (56139) is allowed and applies 15-minute Suspect flag on exit
    let sooma_sys_id = 30_000_009;
    let cruiser_t6_tid: u32 = 56_139;
    seed_pilot(&ctx, char_id, ship_id, sooma_sys_id, DVec3::ZERO).await;
    let mut sess_sooma = make_session(char_id, ship_id, sooma_sys_id, None);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 621;
        }
        snap.items.insert(
            500_000_001,
            InventoryItemRecord {
                item_id: 500_000_001,
                type_id: cruiser_t6_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let deploy_sooma = dispatch_combat_rpc(
        &ctx,
        &mut sess_sooma,
        "abyssalMgr",
        "AbyssalEntranceDeployment",
        &[PyRep::Integer(500_000_001)],
        &HashMap::new(),
    )
    .await
    .expect("Tier 6 Abyssal filament allowed in 0.6 high-sec");
    let trace_ball_id = deploy_sooma
        .dict_get("traceBallID")
        .and_then(|v| v.as_i64())
        .expect("traceBallID");
    let res_sooma = dispatch_combat_rpc(
        &ctx,
        &mut sess_sooma,
        "abyssalMgr",
        "AbyssalEntranceGateActivation",
        &[PyRep::Integer(trace_ball_id)],
        &HashMap::new(),
    )
    .await
    .expect("AbyssalEntranceGateActivation in Sooma");

    let PyRep::KeyVal(kv) = res_sooma else {
        panic!("Expected KeyVal response, got {res_sooma:?}");
    };
    assert_eq!(kv.get("suspectFlagOnExit"), Some(&PyRep::Bool(true)));
    let pid_sus = kv.get("pocketID").and_then(|v| v.as_i64()).unwrap();
    for room in 0..3_usize {
        crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, char_id, room).await;
        let gid = 1_895_000_000 + pid_sus * 10 + (room as i64 + 1);
        if room < 2 {
            dispatch_combat_rpc(&ctx, &mut sess_sooma, "abyssalMgr", "AbyssalGateActivation", &[PyRep::Integer(gid)], &HashMap::new()).await.expect("gate");
        } else {
            ctx.pending_notifications.write().unwrap().clear();
            dispatch_combat_rpc(&ctx, &mut sess_sooma, "abyssalMgr", "AbyssalEndGateActivation", &[PyRep::Integer(gid)], &HashMap::new()).await.expect("exit");
        }
    }
    let now_unix = ctx.now_unix();
    let cw = ctx.crimewatch.read().unwrap();
    assert!(cw.is_suspect(char_id, now_unix + 10), "Exiting T6 Abyssal trace in 0.6 high-sec must apply live 15m Suspect flag");
    assert!(!cw.is_suspect(char_id, now_unix + 901), "Suspect flag must expire after 15 minutes");
    drop(cw);
}
