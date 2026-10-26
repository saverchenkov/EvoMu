//! Regression tests for `P1-ENTROPIC-PARTIAL-MUTATION-AND-AMMO-DEPLETION` and
//! `P1-PVP-FILAMENT-GATE-AND-KILL-PIPELINE`:
//! - Entropic Disintegrator activation / spool cycles must not partially mutate capacitor or
//!   ammo on failure, and must deplete real inventory charges per cycle.
//! - A weapon kill inside the Proving Grounds arena must conclude the match through the real
//!   `dogmaIM.Activate` -> `handle_ship_or_npc_destruction` pipeline, after which the
//!   `AbyssalPVPEndGateActivation` gate checks (arena, concluded match, gate distance) apply.

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{dispatch_combat_rpc, step_combat_system};
use eve_server::services::pvp_filament_mgr::{load_pvp_store, save_pvp_store, PvpArenaMatchInstance};
use glam::DVec3;
use std::collections::{HashMap, HashSet};

const ARENA: u32 = 32_000_001;

fn item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

fn ball(id: i64, type_id: u32, pos: DVec3, radius: f64) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 100.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

async fn setup_pilot(ctx: &ServerContext, client_id: u64, char_id: i64, ship_id: i64, ship_type_id: u32, sys_id: u32, pos: DVec3) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(500_000_000);
        }
        snap.items.insert(ship_id, item(ship_id, ship_type_id, char_id, sys_id as i64, InventoryFlag::None, 1, true));
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball_with_combat(ball(ship_id, ship_type_id, pos, 40.0), ctx.build_type_combat_state(ship_type_id)).await;
    let mut sess = SessionState::new(client_id);
    let (user_id, corp_id) = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).map(|c| (c.user_id, c.corporation_id)).unwrap_or((1, 1_000_002)));
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    let _ = ctx.db.acquire_online_lock(user_id, client_id, Some(char_id), true, ctx.now_unix());
    sess
}

fn fit(ctx: &ServerContext, char_id: i64, ship_id: i64, module: (i64, u32), charge: (i64, u32, i32)) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(module.0, item(module.0, module.1, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(charge.0, item(charge.0, charge.1, char_id, ship_id, InventoryFlag::HiSlot0, charge.2, false));
    });
}

#[tokio::test]
async fn test_entropic_activation_and_cycle_do_not_partially_mutate_and_deplete_real_ammo() {
    // parity: feat:modules.turret-precursor
    eve_testkit::attest(&["feat:modules.turret-precursor"]);

    let ctx = ServerContext::new();
    let (char_id, ship_id, sys_id) = (90_000_001, 100_000_001, 30_000_142);
    let mut sess = setup_pilot(&ctx, 7301, char_id, ship_id, 47269, sys_id, DVec3::ZERO).await; // Damavik
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (near_id, far_id) = (511_001, 511_999);
    for (tid, x) in [(near_id, 2_000.0), (far_id, 80_000.0)] {
        let mut cs = ctx.build_type_combat_state(24690);
        cs.max_shield_hp = 80_000.0;
        cs.shield_hp = 80_000.0;
        let _ = sys_handle.spawn_ball_with_combat(ball(tid, 24690, DVec3::new(x, 0.0, 0.0), 400.0), cs).await;
    }
    // Light Entropic Disintegrator I (47272) + Meson Exotic Plasma S (47925)
    let (ent_id, ammo_id) = (201_101, 201_102);
    fit(&ctx, char_id, ship_id, (ent_id, 47272), (ammo_id, 47925, 50));
    ctx.set_invention_roll_override(Some(0.30));
    let kw = HashMap::new();
    let cap = |h: &eve_server::system_actor::SystemHandle| {
        let h = h.clone();
        async move { h.get_combat_state(ship_id).await.unwrap().unwrap().capacitor }
    };
    let ammo = |cid: i64| ctx.db.with_snapshot(|s| s.items.get(&cid).map(|i| i.quantity).unwrap_or(0));

    // 1. Out-of-range activation fails with no capacitor or ammo drain
    let (c0, a0) = (cap(&sys_handle).await, ammo(ammo_id));
    let err = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(ent_id), PyRep::Integer(far_id)], &kw).await;
    assert_eq!(err.unwrap_err().code, "TargetOutOfRange");
    assert_eq!((cap(&sys_handle).await, ammo(ammo_id)), (c0, a0), "TargetOutOfRange must not drain cap or ammo");

    // 2. Valid activation consumes exactly one real round
    dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(ent_id), PyRep::Integer(near_id)], &kw).await.expect("Activate");
    assert_eq!(ammo(ammo_id), a0 - 1, "Activation consumes one round from inventory");

    // 3. AlreadyActive on the same target fails with no capacitor or ammo drain
    let (c1, a1) = (cap(&sys_handle).await, ammo(ammo_id));
    let err = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(ent_id), PyRep::Integer(near_id)], &kw).await;
    assert_eq!(err.unwrap_err().code, "AlreadyActive");
    assert_eq!((cap(&sys_handle).await, ammo(ammo_id)), (c1, a1), "AlreadyActive must not drain cap or ammo");

    // 4. Each spool cycle depletes one real round and renews the weapon timer
    step_combat_system(&ctx, sys_id, 3.5).await;
    assert_eq!(ammo(ammo_id), a1 - 1, "Spool cycle consumes one round from inventory");
    let m1 = eve_server::services::combat_weapons::get_entropic_spool_multiplier(&ctx, ship_id, ent_id);
    assert!((m1 - 1.07).abs() < 1e-6, "Second cycle spools to 1.07x (got {m1})");
    let wt = ctx.crimewatch.read().unwrap().states.get(&char_id).map(|s| s.weapon_timer_until_unix).unwrap_or(0);
    assert!(wt > ctx.now_unix(), "Spool cycle renews CrimeWatch weapon timer");

    // 5. Single loaded round: activation consumes it; next cycle stops spool without draining capacitor
    let _ = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Deactivate", &[PyRep::Integer(ent_id)], &HashMap::new()).await;
    let single_id = 201_103;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&ammo_id);
    });
    fit(&ctx, char_id, ship_id, (ent_id, 47272), (single_id, 47925, 1));
    dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(ent_id), PyRep::Integer(near_id)], &kw).await.expect("Activate single");
    assert_eq!(ammo(single_id), 0, "Activation consumes the single round");
    let mut cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    cs.capacitor = 50.0;
    let _ = sys_handle.set_combat_state(ship_id, cs).await;
    step_combat_system(&ctx, sys_id, 3.5).await;
    let m_dep = eve_server::services::combat_weapons::get_entropic_spool_multiplier(&ctx, ship_id, ent_id);
    assert_eq!(m_dep, 1.0, "Spool stops when no real ammo remains");
    assert!(cap(&sys_handle).await >= 50.0 - 1e-9, "Ammo-less cycle must not drain capacitor");
}

#[tokio::test]
async fn test_proving_grounds_weapon_kill_concludes_match_and_gates_exit() {
    // parity: rpc:pvpFilamentMgr.AbyssalPVPEndGateActivation
    eve_testkit::attest(&["rpc:pvpFilamentMgr.AbyssalPVPEndGateActivation"]);

    let ctx = ServerContext::new();
    let (p1, p2) = (90_000_001, 90_000_002);
    let (ship1, ship2) = (100_000_101, 100_000_102);
    let mut s1 = setup_pilot(&ctx, 7311, p1, ship1, 587, ARENA, DVec3::ZERO).await;
    let mut s2 = setup_pilot(&ctx, 7312, p2, ship2, 587, ARENA, DVec3::new(1_500.0, 0.0, 0.0)).await;
    let arena = ctx.systems.get_or_spawn(ARENA);
    let _ = arena.spawn_ball(ball(70_001, 47459, DVec3::new(500.0, 0.0, 0.0), 500.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        store.active_matches.insert(4242, PvpArenaMatchInstance {
            match_id: 4242,
            event_id: 2,
            arena_solar_system_id: ARENA,
            team_a: vec![p1],
            team_b: vec![p2],
            origin_systems: HashMap::from([(p1, 30_000_142), (p2, 30_000_144)]),
            started_unix: ctx.now_unix(),
            active: true,
            winner_char_id: None,
            exited_pilots: HashSet::new(),
            end_gate_id: 70_001,
        });
        save_pvp_store(snap, &store);
    });

    // Gate exit is refused while the match has no winner
    let err = ctx.dispatch_rpc(&mut s1, "pvpFilamentMgr", None, "AbyssalPVPEndGateActivation", &[PyRep::Integer(70_001)], &HashMap::new(), None).await;
    assert_eq!(err.unwrap_err().code, "MatchNotConcluded");

    // P1 destroys P2's ship with a real turret activation (125mm Railgun I + Antimatter Charge S)
    fit(&ctx, p1, ship1, (200_201, 10678), (200_202, 222, 10));
    let mut cs = arena.get_combat_state(ship2).await.unwrap().unwrap();
    (cs.shield_hp, cs.armor_hp, cs.structure_hp) = (0.0, 0.0, 1.0);
    let _ = arena.set_combat_state(ship2, cs).await;
    ctx.set_invention_roll_override(Some(0.50));
    let res = dispatch_combat_rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(200_201), PyRep::Integer(ship2)], &HashMap::new()).await.expect("Activate");
    assert_eq!(res.dict_get("destroyed").and_then(PyRep::as_bool), Some(true), "P2 ship must be destroyed");

    let (active, winner) = ctx.db.with_snapshot(|s| {
        let m = load_pvp_store(s).active_matches.get(&4242).cloned().expect("match");
        (m.active, m.winner_char_id)
    });
    assert_eq!((active, winner), (false, Some(p1)), "Weapon kill must conclude the match with P1 as winner");

    // Gate checks: wrong gate id is rejected; correct end gate within 50km extracts to origin
    let err = ctx.dispatch_rpc(&mut s1, "pvpFilamentMgr", None, "AbyssalPVPEndGateActivation", &[PyRep::Integer(99_999)], &HashMap::new(), None).await;
    assert_eq!(err.unwrap_err().code, "TooFarFromGate");
    let ok = ctx.dispatch_rpc(&mut s1, "pvpFilamentMgr", None, "AbyssalPVPEndGateActivation", &[PyRep::Integer(70_001)], &HashMap::new(), None).await.expect("gate exit");
    assert_eq!(ok.dict_get("originSystemID").and_then(PyRep::as_i64), Some(30_000_142));
    assert_eq!(s1.solar_system_id, Some(30_000_142));

    // Outside the arena, gate activation is refused
    let err = ctx.dispatch_rpc(&mut s1, "pvpFilamentMgr", None, "AbyssalPVPEndGateActivation", &[PyRep::Integer(70_001)], &HashMap::new(), None).await;
    assert_eq!(err.unwrap_err().code, "NotInArena");
    let _ = &mut s2;
}
