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
        if !snap.characters.contains_key(&char_id) {
            if let Some(mut tmpl) = snap.characters.values().next().cloned() {
                tmpl.character_id = char_id;
                tmpl.corporation_id = 1_000_002;
                snap.characters.insert(char_id, tmpl);
            }
        }
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
        let _ = sys_handle.add_target_lock(ship_id, eve_destiny::TargetLock {
            target_id: tid,
            lock_time_ms: 0.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(100_000.0),
        }).await;
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
    let wt_of = || ctx.crimewatch.read().unwrap().states.get(&char_id).map(|s| s.weapon_timer_until_unix).unwrap_or(0);
    let wt0 = wt_of();
    ctx.advance_time_seconds(3);
    step_combat_system(&ctx, sys_id, 3.5).await;
    assert_eq!(ammo(ammo_id), a1 - 1, "Spool cycle consumes one round from inventory");
    let m1 = eve_server::services::combat_weapons::get_entropic_spool_multiplier(&ctx, ship_id, ent_id);
    assert!((m1 - 1.07).abs() < 1e-6, "Second cycle spools to 1.07x (got {m1})");
    assert!(wt_of() > wt0, "Spool cycle itself must renew (extend) the CrimeWatch weapon timer");

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
    // Full capacitor (capacitor == max) so passive regen cannot mask a drain: the value must stay exact.
    (cs.capacitor, cs.max_capacitor) = (50.0, 50.0);
    let _ = sys_handle.set_combat_state(ship_id, cs).await;
    step_combat_system(&ctx, sys_id, 3.5).await;
    let m_dep = eve_server::services::combat_weapons::get_entropic_spool_multiplier(&ctx, ship_id, ent_id);
    assert_eq!(m_dep, 1.0, "Spool stops when no real ammo remains");
    assert_eq!(cap(&sys_handle).await, 50.0, "Ammo-less cycle must not drain capacitor");
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
    let _ = arena.add_target_lock(ship1, eve_destiny::TargetLock { target_id: ship2, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
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

#[tokio::test]
async fn test_proving_grounds_result_not_recorded_for_kills_outside_arena() {
    // Regression for P1-PG-RESULT-OUTSIDE-ARENA (no parity tag: guards the kill-pipeline hook only)
    let ctx = ServerContext::new();
    let (p1, p2, sys) = (90_000_001, 90_000_002, 30_000_142);
    let (ship1, ship2) = (100_000_201, 100_000_202);
    let mut s1 = setup_pilot(&ctx, 7321, p1, ship1, 587, sys, DVec3::ZERO).await;
    let _s2 = setup_pilot(&ctx, 7322, p2, ship2, 587, sys, DVec3::new(1_500.0, 0.0, 0.0)).await;
    let local = ctx.systems.get_or_spawn(sys);
    fit(&ctx, p1, ship1, (200_301, 10678), (200_302, 222, 10));
    let _ = local.add_target_lock(ship1, eve_destiny::TargetLock { target_id: ship2, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
    ctx.set_invention_roll_override(Some(0.50));
    let entry = |cid: i64| eve_server::services::pvp_filament_mgr::PvpQueueCharacterEntry {
        item_id: cid + 1, type_id: 53965, location_id: cid, flag: 4, solar_system_id: sys, joined_unix: ctx.now_unix(),
    };

    // 1. Both pilots queued but unmatched (still in normal space); P1 kills P2 with a real turret
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        store.queue.insert(p1, entry(p1));
        store.queue.insert(p2, entry(p2));
        save_pvp_store(snap, &store);
    });
    let mut cs = local.get_combat_state(ship2).await.unwrap().unwrap();
    (cs.shield_hp, cs.armor_hp, cs.structure_hp) = (0.0, 0.0, 1.0);
    let _ = local.set_combat_state(ship2, cs).await;
    let res = dispatch_combat_rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(200_301), PyRep::Integer(ship2)], &HashMap::new()).await.expect("Activate");
    assert_eq!(res.dict_get("destroyed").and_then(PyRep::as_bool), Some(true), "P2 ship must be destroyed");
    let (q1, q2, st1, st2) = ctx.db.with_snapshot(|s| {
        let store = load_pvp_store(s);
        (store.queue.contains_key(&p1), store.queue.contains_key(&p2), store.stats.get(&p1).cloned(), store.stats.get(&p2).cloned())
    });
    assert!(q1 && q2, "Normal-space kill must not dequeue queued pilots");
    assert_eq!((st1, st2), (None, None), "Normal-space kill must not record a Proving Grounds win/loss");

    // 2. Pilots matched into an arena match but the kill happens outside the arena system: no result
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        store.active_matches.insert(4343, PvpArenaMatchInstance {
            match_id: 4343, event_id: 2, arena_solar_system_id: ARENA, team_a: vec![p1], team_b: vec![p2],
            origin_systems: HashMap::new(), started_unix: ctx.now_unix(), active: true, winner_char_id: None,
            exited_pilots: HashSet::new(), end_gate_id: 70_001,
        });
        save_pvp_store(snap, &store);
    });
    eve_server::services::pvp_filament_mgr::record_proving_grounds_combat_result_if_queued(&ctx, p1, p2, sys, 1500.0);
    let (active, winner) = ctx.db.with_snapshot(|s| {
        let m = load_pvp_store(s).active_matches.get(&4343).cloned().expect("match");
        (m.active, m.winner_char_id)
    });
    assert_eq!((active, winner), (true, None), "Kill outside arena_solar_system_id must not conclude the match");
}

#[tokio::test]
async fn test_entropic_spool_cycle_kill_executes_canonical_death_pipeline() {
    // parity: feat:modules.turret-precursor
    eve_testkit::attest(&["feat:modules.turret-precursor"]);

    let ctx = ServerContext::new();
    let (p1, p2) = (90_000_101, 90_000_102);
    let (ship1, ship2) = (100_000_301, 100_000_302);
    let sys = 30_000_142;
    let mut s1 = setup_pilot(&ctx, 7331, p1, ship1, 47269, sys, DVec3::ZERO).await; // Damavik
    let _s2 = setup_pilot(&ctx, 7332, p2, ship2, 587, sys, DVec3::new(2_000.0, 0.0, 0.0)).await; // Rifter
    let sys_h = ctx.systems.get_or_spawn(sys);

    let (ent_id, ammo_id) = (201_301, 201_302);
    fit(&ctx, p1, ship1, (ent_id, 47272), (ammo_id, 47925, 50));
    let cargo_item_id = 201_303;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cargo_item_id,
            item(cargo_item_id, 34, p2, ship2, InventoryFlag::Cargo, 100, false),
        );
    });

    let _ = sys_h.add_target_lock(ship1, eve_destiny::TargetLock {
        target_id: ship2,
        lock_time_ms: 0.0,
        remaining_lock_ms: 0.0,
        locked: true,
        max_range_m: Some(50_000.0),
    }).await;

    let mut cs2 = sys_h.get_combat_state(ship2).await.unwrap().unwrap();
    (cs2.shield_hp, cs2.armor_hp, cs2.structure_hp) = (0.0, 0.0, 500.0);
    let _ = sys_h.set_combat_state(ship2, cs2).await;
    ctx.set_invention_roll_override(Some(0.50));

    let act_res = dispatch_combat_rpc(
        &ctx,
        &mut s1,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(ent_id), PyRep::Integer(ship2)],
        &HashMap::new(),
    ).await.expect("Initial activation");
    assert_eq!(act_res.dict_get("destroyed").and_then(PyRep::as_bool), Some(false));
    assert!(sys_h.get_combat_state(ship2).await.unwrap().is_some());

    let mut cs2_low = sys_h.get_combat_state(ship2).await.unwrap().unwrap();
    cs2_low.structure_hp = 1.0;
    let _ = sys_h.set_combat_state(ship2, cs2_low).await;

    ctx.advance_time_seconds(4);
    sys_h.step_manual(4.0).await.unwrap();

    // 1. Victim ship ball destroyed
    assert!(sys_h.get_combat_state(ship2).await.unwrap().is_none(), "Victim ship must be destroyed on spool cycle");

    // 2. Capsule ejected and ship item removed
    let active_ship = ctx.db.with_snapshot(|s| s.characters.get(&p2).unwrap().active_ship_id);
    assert_ne!(active_ship, ship2, "Victim active ship must be swapped to capsule");
    let ship_exists = ctx.db.with_snapshot(|s| s.items.contains_key(&ship2));
    assert!(!ship_exists, "Destroyed ship must be removed from DB items");

    // 3. Killmail recorded
    let km = ctx.db.with_snapshot(|s| {
        s.killmails.values().find(|k| k.victim_character_id == p2).cloned()
    }).expect("Killmail must be recorded");
    assert_eq!(km.ship_type_id, 587);
    assert_eq!(km.attacker_character_id, p1);

    // 4. Wreck container exists in solar system
    let wreck = ctx.db.with_snapshot(|s| {
        s.items.values().find(|i| i.location_id == sys as i64 && i.owner_id == p2).cloned()
    }).expect("Wreck must be spawned");
    assert!(wreck.singleton);

    // 5. Entropic spool cleared and module deactivated
    let mult = eve_server::services::combat_weapons::get_entropic_spool_multiplier(&ctx, ship1, ent_id);
    assert_eq!(mult, 1.0, "Spool must reset to 1.0 upon target destruction");
    let is_active = ctx.db.with_snapshot(|s| s.module_active_states.contains(&ent_id));
    assert!(!is_active, "Module must be deactivated upon target destruction");

    // 6. CrimeWatch PvP timer renewed
    let (cw_p1, cw_p2) = ctx.crimewatch.read().unwrap().states.iter().fold((0, 0), |(t1, t2), (&cid, st)| {
        if cid == p1 { (st.pvp_timer_until_unix, t2) }
        else if cid == p2 { (t1, st.pvp_timer_until_unix) }
        else { (t1, t2) }
    });
    assert!(cw_p1 > 0, "Attacker must have PvP timer");
    assert!(cw_p2 > 0, "Victim must have PvP timer");
}
