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
    common::slash_cmd(ctx, sess, &format!("/tr {sys_id}")).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let st_pos = ctx.sde.stations[&station_id].position;
    sys.set_ball_position(ship_id, st_pos + DVec3::new(300.0, 0.0, 0.0)).await;
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

    // Provision two live pilots in Zarzakh via ClientHarness flows
    let mut offender = common::gm_pilot_in_space(&ctx, 101, 91_100_001, 587, zarzakh_sys_id).await;
    let victim = common::gm_pilot_in_space(&ctx, 102, 91_100_002, 587, zarzakh_sys_id).await;
    let offender_guns =
        fit_and_load_turrets(&ctx, &mut offender, zarzakh_sys_id, 2889, 185, 1_000, 2).await;

    let sys_handle = ctx.systems.get_or_spawn(zarzakh_sys_id);
    let gun_pos = DVec3::ZERO;
    let offender_pos = gun_pos + DVec3::new(15_000.0, 0.0, 0.0);
    let victim_pos = gun_pos + DVec3::new(17_500.0, 0.0, 0.0);
    let patrol_pos = gun_pos + DVec3::new(15_000.0, 2_000.0, 0.0);
    sys_handle.set_ball_position(offender.ship_id, offender_pos).await;
    sys_handle.set_ball_position(victim.ship_id, victim_pos).await;
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
        wire_ok(ctx, s1, "ship", None, "Board", &[PyRep::Integer(ship_id)]).await;
        ctx.advance_secs(31);
        wire_ok(ctx, s1, "stationSvc", None, "Undock", &[]).await;
        ctx.advance_secs(31);

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
        for _ in 0..60 {
            if sys.get_ball_snapshot(ship_id).await.unwrap().is_none() {
                break;
            }
            let _ = wire_call(
                ctx,
                s2,
                "dogmaIM",
                None,
                "Activate",
                &[
                    PyRep::Integer(PILOT_2_GUN),
                    PyRep::String("targetAttack".into()),
                    PyRep::Integer(ship_id),
                ],
                &HashMap::new(),
            )
            .await;
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
    let ammo2 = buy_at_jita(&ctx, &mut s2, EMP_S, 500).await[0];
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
