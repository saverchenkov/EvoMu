//! Navigation parity tests: Upwell tethering, SDE-driven jump drives / fatigue, and the Zarzakh gate lock.

use eve_db::types::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord, UpwellStructureRecord};
use eve_destiny::{DamageVector, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const PILOT: i64 = 90_000_001;
const JITA: u32 = 30_000_142;
const OHKUNEN: u32 = 30_000_204;

async fn login_and_undock(ctx: &ServerContext, sess_id: u64) -> (SessionState, u64, i64) {
    let kw = HashMap::new();
    let mut sess = SessionState::new(sess_id);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(PILOT)], &kw, None)
        .await
        .expect("SelectCharacterID");
    let undock = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(kv) = &undock else { panic!("Undock must return KeyVal") };
    let beyonce = kv["beyonceBoundID"].as_i64().unwrap() as u64;
    let ship_id = sess.ship_id.expect("active ship");
    (sess, beyonce, ship_id)
}

fn item(item_id: i64, type_id: u32, location_id: i64, flag: InventoryFlag, quantity: i32) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id: PILOT,
        location_id,
        flag,
        quantity,
        singleton: quantity == 1,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

fn structure(structure_id: i64, type_id: u32, sys: u32, owner_corp_id: i64, pos: DVec3) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id,
        type_id,
        solar_system_id: sys,
        owner_corp_id,
        name: format!("Structure {structure_id}"),
        position: pos.to_array(),
        state: "ShieldVulnerable".into(),
        quantum_core_installed: true,
        fuel_blocks: 50,
        shield_hp: 50_000.0,
        armor_hp: 50_000.0,
        hull_hp: 50_000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
    }
}

fn set_skill(ctx: &ServerContext, skill_id: u32, level: u8) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT).or_default().insert(
            skill_id,
            TrainedSkillRecord {
                type_id: skill_id,
                level,
                skill_points: 256_000.0,
            },
        );
    });
}

fn attr(ctx: &ServerContext, type_id: u32, name: &str) -> f64 {
    let id = ctx
        .sde
        .attributes
        .iter()
        .find(|(_, a)| a.name == name)
        .map(|(id, _)| *id)
        .unwrap_or_else(|| panic!("SDE attribute {name} missing"));
    ctx.sde.types[&type_id].default_attributes.get(&id).copied().unwrap_or(0.0)
}

fn dist_ly(ctx: &ServerContext, a: u32, b: u32) -> f64 {
    let p = ctx.sde.solar_systems[&a].galactic_pos_ly;
    let q = ctx.sde.solar_systems[&b].galactic_pos_ly;
    ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt()
}

async fn update_state(ctx: &ServerContext, sess: &mut SessionState, beyonce: u64) {
    ctx.dispatch_rpc(sess, "beyonce", Some(beyonce), "UpdateStateRequest", &[], &HashMap::new(), None)
        .await
        .expect("UpdateStateRequest");
}

const HIT: DamageVector = DamageVector {
    em: 50.0,
    thermal: 50.0,
    kinetic: 50.0,
    explosive: 50.0,
};

#[tokio::test]
async fn test_upwell_tether_invulnerability_repair_and_breaks() {
    // parity: feat:navigation.tether, feat:navigation.tether-break
    eve_testkit::attest(&["feat:navigation.tether", "feat:navigation.tether-break"]);
    let ctx = ServerContext::new();
    let (mut sess, beyonce, ship_id) = login_and_undock(&ctx, 7101).await;
    let handle = ctx.systems.get_or_spawn(JITA);
    let astrahus = 35_832u32;
    let tether_range = attr(&ctx, astrahus, "tetheringRange");
    assert!(tether_range > 0.0, "Astrahus must carry SDE tetheringRange");
    let radius = ctx.sde.types[&astrahus].radius;
    let st_pos = DVec3::new(9_000_000.0, 0.0, 0.0);
    let inside = st_pos + DVec3::new(radius + tether_range * 0.5, 0.0, 0.0);
    let outside = st_pos + DVec3::new(radius + tether_range + 5_000.0, 0.0, 0.0);
    let (npc_st, foreign_st) = (1_040_000_101i64, 1_040_000_102i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(npc_st, structure(npc_st, astrahus, JITA, 1_000_125, st_pos));
    });

    let reset = |handle: eve_server::system_actor::SystemHandle| async move {
        let mut cs = handle.get_combat_state(ship_id).await.unwrap().unwrap();
        cs.shield_hp = cs.max_shield_hp * 0.2;
        cs.armor_hp = cs.max_armor_hp * 0.3;
        cs.structure_hp = cs.max_structure_hp * 0.4;
        cs.gate_cloak_remaining_ticks = 0;
        cs.undock_invulnerable_remaining_ticks = 0;
        cs.warp_exit_invulnerable_remaining_ticks = 0;
        cs.sync_invuln_with_undock_timer = false;
        cs.is_cloaked = false;
        handle.set_combat_state(ship_id, cs).await.unwrap();
    };
    reset(handle.clone()).await;
    assert!(handle.set_ball_position(ship_id, inside).await);
    update_state(&ctx, &mut sess, beyonce).await;
    let cs = handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs.is_tethered, "ship inside tetheringRange of an accessible cored structure must tether");
    assert!(cs.shield_hp > cs.max_shield_hp * 0.2, "tether must repair shield");
    assert!(cs.armor_hp > cs.max_armor_hp * 0.3, "tether must repair armor");
    assert!(cs.structure_hp > cs.max_structure_hp * 0.4, "tether must repair hull");
    let before = (cs.shield_hp, cs.armor_hp, cs.structure_hp);
    let (applied, destroyed, _) = handle.apply_damage(ship_id, HIT).await.unwrap().unwrap();
    assert_eq!((applied, destroyed), (0.0, false), "tethered ship must be invulnerable");
    let cs = handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!((cs.shield_hp, cs.armor_hp, cs.structure_hp), before);

    // Locking a target breaks tether immediately and the ship takes damage again.
    handle
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id: 60_003_760,
                lock_time_ms: 2_000.0,
                remaining_lock_ms: 2_000.0,
                locked: false,
                max_range_m: None,
            },
        )
        .await
        .unwrap();
    update_state(&ctx, &mut sess, beyonce).await;
    assert!(!handle.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered, "lock must break tether");
    let (applied, _, _) = handle.apply_damage(ship_id, HIT).await.unwrap().unwrap();
    assert!(applied > 0.0, "untethered ship must take damage");
    handle.clear_target_locks(ship_id).await.unwrap();
    update_state(&ctx, &mut sess, beyonce).await;
    assert!(handle.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered, "tether returns after unlock");

    // Aggression (weapons timer) prevents tether.
    ctx.crimewatch.write().unwrap().states.entry(PILOT).or_default().weapon_timer_until_unix = ctx.now_unix() + 60;
    update_state(&ctx, &mut sess, beyonce).await;
    assert!(!handle.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered, "weapons timer must break tether");
    ctx.advance_virtual_clock(61);
    update_state(&ctx, &mut sess, beyonce).await;
    assert!(handle.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered, "tether returns after weapons timer");

    // Activating a cyno (SDE disallowTethering) breaks tether; a non-offensive module does not.
    let cyno_type = 21_096u32;
    assert_eq!(attr(&ctx, cyno_type, "disallowTethering"), 1.0);
    let ab_type = 438u32; // 1MN Afterburner II: disallowTethering absent
    assert_eq!(attr(&ctx, ab_type, "disallowTethering"), 0.0);
    let (cyno_id, ab_id) = (77_000_001i64, 77_000_002i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ab_id, item(ab_id, ab_type, ship_id, InventoryFlag::MedSlot0, 1));
        snap.items.insert(cyno_id, item(cyno_id, cyno_type, ship_id, InventoryFlag::HiSlot0, 1));
        snap.module_active_states.insert(ab_id);
    });
    update_state(&ctx, &mut sess, beyonce).await;
    assert!(handle.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered, "propulsion module keeps tether");
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(cyno_id);
    });
    update_state(&ctx, &mut sess, beyonce).await;
    assert!(!handle.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered, "active cyno must break tether");
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cyno_id);
        snap.module_active_states.remove(&ab_id);
    });
    update_state(&ctx, &mut sess, beyonce).await;
    assert!(handle.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);

    // Entering warp (jump state) removes tether.
    handle
        .set_movement_mode(ship_id, MovementMode::new_warp(inside, inside + DVec3::new(1.0e9, 0.0, 0.0), 3.0))
        .await
        .unwrap();
    update_state(&ctx, &mut sess, beyonce).await;
    assert!(!handle.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered, "warp must break tether");
    handle.set_movement_mode(ship_id, MovementMode::Stop).await.unwrap();

    // Out of range: no tether.
    assert!(handle.set_ball_position(ship_id, outside).await);
    update_state(&ctx, &mut sess, beyonce).await;
    assert!(!handle.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered, "outside range must not tether");

    // A structure owned by another player corporation (no access) does not tether.
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.remove(&npc_st);
        snap.structures.insert(foreign_st, structure(foreign_st, astrahus, JITA, 98_000_777, st_pos));
    });
    assert!(handle.set_ball_position(ship_id, inside).await);
    update_state(&ctx, &mut sess, beyonce).await;
    assert!(!handle.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered, "no access => no tether");
}

/// Converts the pilot's active ship into an Archon carrier with Helium Isotopes in its fuel bay.
async fn make_archon(ctx: &ServerContext, ship_id: i64, fuel_item: i64, fuel: i32) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 23_757;
        snap.items.insert(fuel_item, item(fuel_item, 16_274, ship_id, InventoryFlag::SpecializedFuelBay, fuel));
    });
    ctx.invalidate_ship_fitting(ship_id);
    let handle = ctx.systems.get_or_spawn(JITA);
    let (_, mut ball) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    ball.type_id = 23_757;
    ball.mode = MovementMode::Stop;
    handle.spawn_ball(ball).await.unwrap();
}

async fn beacon_jump(ctx: &ServerContext, sess: &mut SessionState, sys: u32, beacon: i64) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(
        sess,
        "structureCynoBeaconMgr",
        None,
        "CmdJumpToStructureBeacon",
        &[PyRep::Integer(sys as i64), PyRep::Integer(beacon)],
        &HashMap::new(),
        None,
    )
    .await
}

fn fuel_left(ctx: &ServerContext, id: i64) -> i32 {
    ctx.db.with_snapshot(|snap| snap.items.get(&id).map(|i| i.quantity).unwrap_or(0))
}

#[tokio::test]
async fn test_jump_drive_range_fuel_fatigue_and_highsec_restriction() {
    // parity: feat:navigation.jump-drive, feat:navigation.jump-fatigue, feat:navigation.jump-range-restrictions
    // parity: rpc:jumpTimers.GetTimers, notify:OnJumpTimersUpdated
    eve_testkit::attest(&[
        "feat:navigation.jump-drive",
        "feat:navigation.jump-fatigue",
        "feat:navigation.jump-range-restrictions",
        "rpc:jumpTimers.GetTimers",
        "notify:OnJumpTimersUpdated",
    ]);
    let ctx = ServerContext::new();
    let (mut sess, _beyonce, ship_id) = login_and_undock(&ctx, 7104).await;
    let corp = sess.corporation_id.unwrap();
    let kw = HashMap::new();
    let beacon_type = 35_840u32;
    let d1 = dist_ly(&ctx, JITA, OHKUNEN);
    // Second hop: a low-sec, non-Pochven system near Ohkunen (DERIVED from SDE coordinates).
    let hop2 = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status > 0.0 && s.security_status < 0.45 && s.region_id < 11_000_000)
        .filter(|s| s.region_id != 10_000_070 && s.solar_system_id != OHKUNEN)
        .map(|s| (s.solar_system_id, dist_ly(&ctx, OHKUNEN, s.solar_system_id)))
        .filter(|(_, d)| *d > 1.0 && *d < 3.0)
        .min_by(|a, b| a.0.cmp(&b.0))
        .expect("low-sec system 1-3 LY from Ohkunen");
    let highsec = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status >= 0.5 && s.region_id < 11_000_000)
        .map(|s| (s.solar_system_id, dist_ly(&ctx, JITA, s.solar_system_id)))
        .filter(|(_, d)| *d > 0.5 && *d < 3.0)
        .min_by(|a, b| a.0.cmp(&b.0))
        .expect("high-sec system near Jita");
    let (b_ohk, b_hop2, b_high) = (1_046_000_001i64, 1_046_000_002i64, 1_046_000_003i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(b_ohk, structure(b_ohk, beacon_type, OHKUNEN, corp, DVec3::new(1.0e6, 0.0, 0.0)));
        snap.structures.insert(b_hop2, structure(b_hop2, beacon_type, hop2.0, corp, DVec3::new(2.0e6, 0.0, 0.0)));
        snap.structures.insert(b_high, structure(b_high, beacon_type, highsec.0, corp, DVec3::new(3.0e6, 0.0, 0.0)));
    });

    // A subcapital (no SDE canJump) has no jump drive.
    let err = beacon_jump(&ctx, &mut sess, OHKUNEN, b_ohk).await.expect_err("Rifter cannot jump");
    assert_eq!(err.code, "ShipHasNoJumpDrive");

    let fuel_item = 88_100_001i64;
    make_archon(&ctx, ship_id, fuel_item, 100_000).await;
    let base_range = attr(&ctx, 23_757, "jumpDriveRange");
    let per_ly = attr(&ctx, 23_757, "jumpDriveConsumptionAmount");
    assert_eq!(attr(&ctx, 23_757, "jumpDriveConsumptionType"), 16_274.0);
    assert_eq!(attr(&ctx, 23_757, "disallowInHighSec"), 1.0);
    assert!(d1 > base_range, "Ohkunen ({d1:.2} LY) must exceed untrained range {base_range}");

    // Range: untrained Jump Drive Calibration cannot reach; level 5 doubles the range.
    let err = beacon_jump(&ctx, &mut sess, OHKUNEN, b_ohk).await.expect_err("out of range");
    assert_eq!(err.code, "JumpDistanceOutOfRange");
    set_skill(&ctx, 21_611, 5);
    set_skill(&ctx, 21_610, 4);

    // Capitals may not jump into high-sec.
    let err = beacon_jump(&ctx, &mut sess, highsec.0, b_high).await.expect_err("high-sec forbidden");
    assert_eq!(err.code, "JumpDriveHighSecForbidden");
    assert_eq!(fuel_left(&ctx, fuel_item), 100_000, "refused jumps consume no fuel");

    // Initial GetTimers returns (0, 0, 0) and rejects mismatched or invalid charID arguments.
    let init_timers = ctx
        .dispatch_rpc(&mut sess, "jumpTimers", None, "GetTimers", &[PyRep::Integer(PILOT)], &kw, None)
        .await
        .expect("GetTimers initial");
    assert_eq!(init_timers, PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0)]));
    let bad_arg = ctx
        .dispatch_rpc(&mut sess, "jumpTimers", None, "GetTimers", &[PyRep::String("bad".into())], &kw, None)
        .await
        .expect_err("non-integer charID");
    assert_eq!(bad_arg.code, "InvalidArguments");
    let wrong_char = ctx
        .dispatch_rpc(&mut sess, "jumpTimers", None, "GetTimers", &[PyRep::Integer(99_999_999)], &kw, None)
        .await
        .expect_err("other charID");
    assert_eq!(wrong_char.code, "CharacterNotInSession");
    let _ = ctx.drain_notifications_for_char(PILOT);

    // Jump 1: isotopes consumed per LY with Jump Fuel Conservation IV, fatigue applied.
    let res = beacon_jump(&ctx, &mut sess, OHKUNEN, b_ohk).await.expect("jump to Ohkunen");
    let PyRep::KeyVal(kv) = &res else { panic!() };
    assert_eq!(kv["solarSystemID"].as_i64(), Some(OHKUNEN as i64));
    let expected_fuel = (per_ly * d1 * 0.6).ceil() as i32;
    assert_eq!(fuel_left(&ctx, fuel_item), 100_000 - expected_fuel);
    let (cd1, fat1) = (kv["jumpCooldownMinutes"].as_f64().unwrap(), kv["jumpFatigueMinutes"].as_f64().unwrap());
    assert!((cd1 - (1.0 + d1)).abs() < 1e-6, "activation = 1 + LY, got {cd1}");
    assert!((fat1 - 10.0 * (1.0 + d1)).abs() < 1e-6, "fatigue = 10 x (1 + LY), got {fat1}");
    assert!(ctx.systems.get_or_spawn(OHKUNEN).get_ball_snapshot(ship_id).await.unwrap().is_some());

    let timers = ctx
        .dispatch_rpc(&mut sess, "jumpTimers", None, "GetTimers", &[PyRep::Integer(PILOT)], &kw, None)
        .await
        .unwrap();
    let PyRep::Tuple(t) = &timers else { panic!("GetTimers must return 3-tuple") };
    assert_eq!(t.len(), 3, "(jumpActivation, jumpFatigue, lastUpdated)");
    let (act_ft, fat_ft, upd_ft) = (t[0].as_i64().unwrap(), t[1].as_i64().unwrap(), t[2].as_i64().unwrap());
    assert!(upd_ft > 116_444_736_000_000_000, "lastUpdated must be a Windows FILETIME");
    assert_eq!((act_ft - upd_ft) / 10_000_000, (cd1 * 60.0).round() as i64);
    assert_eq!((fat_ft - upd_ft) / 10_000_000, (fat1 * 60.0).round() as i64);
    let notifs = ctx.drain_notifications_for_char(PILOT);
    let jt_notif = notifs
        .iter()
        .find(|n| n.event == "OnJumpTimersUpdated")
        .expect("jump must emit OnJumpTimersUpdated");
    assert_eq!(jt_notif.id_type, "charid");
    assert_eq!(jt_notif.args, vec![PyRep::Integer(act_ft), PyRep::Integer(fat_ft), PyRep::Integer(upd_ft)]);

    // Activation cooldown blocks an immediate second jump.
    let err = beacon_jump(&ctx, &mut sess, hop2.0, b_hop2).await.expect_err("cooldown");
    assert_eq!(err.code, "JumpCooldownActive");

    // Capitals cannot enter high-sec through a stargate either (Ohkunen -> Mastakomon).
    let ohk_beyonce = kv["beyonceBoundID"].as_i64().unwrap() as u64;
    let to_highsec_gate = 50_012_926i64;
    let gate = &ctx.sde.jumps[&to_highsec_gate];
    assert!(ctx.sde.solar_systems[&gate.to_solar_system_id].security_status >= 0.45);
    ctx.advance_virtual_clock(11);
    assert!(ctx.systems.get_or_spawn(OHKUNEN).set_ball_position(ship_id, gate.position).await);
    let err = ctx
        .dispatch_rpc(&mut sess, "beyonce", Some(ohk_beyonce), "CmdStargateJump", &[PyRep::Integer(to_highsec_gate)], &kw, None)
        .await
        .expect_err("capital must not gate into high-sec");
    assert_eq!(err.code, "CapitalShipCannotJumpStargate");
    assert_eq!(sess.solar_system_id, Some(OHKUNEN));

    // Jump 2 after the cooldown: fatigue multiplies by (1 + LY) and grows.
    let jump1_unix = (upd_ft / 10_000_000 - 11_644_473_600) as u64;
    let waited = (cd1 * 60.0).ceil() as u64 + 1;
    ctx.advance_virtual_clock(waited - 11);
    let actual_waited = ctx.now_unix().saturating_sub(jump1_unix);
    let fat_before = fat1 - actual_waited as f64 / 60.0;
    let res = beacon_jump(&ctx, &mut sess, hop2.0, b_hop2).await.expect("second jump");
    let PyRep::KeyVal(kv) = &res else { panic!() };
    let (cd2, fat2) = (kv["jumpCooldownMinutes"].as_f64().unwrap(), kv["jumpFatigueMinutes"].as_f64().unwrap());
    let d2 = hop2.1;
    assert!((fat2 - (fat_before * (1.0 + d2)).min(300.0)).abs() < 0.2, "fatigue2 {fat2}");
    assert!(fat2 > fat1, "fatigue must grow across jumps");
    assert!((cd2 - (1.0 + d2).max(fat_before / 10.0).min(30.0)).abs() < 0.05, "activation2 {cd2}");
}

async fn place_at_gate(ctx: &ServerContext, sess: &mut SessionState, ship_id: i64, sys: u32, gate: i64) -> u64 {
    let from = sess.solar_system_id.unwrap();
    let old = ctx.systems.get_or_spawn(from);
    let mut ball = old.remove_ball(ship_id).await.expect("ship ball");
    ctx.db.set_character_solar_system(PILOT, sys, &ctx.sde).unwrap();
    ball.position = ctx.sde.jumps[&gate].position;
    ball.mode = MovementMode::Stop;
    ctx.systems.get_or_spawn(sys).spawn_ball(ball).await.unwrap();
    sess.solar_system_id = Some(sys);
    ctx.bind_object(sess, "beyonce", sys as i64).unwrap()
}

async fn gate_jump(ctx: &ServerContext, sess: &mut SessionState, beyonce: u64, gate: i64) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, "beyonce", Some(beyonce), "CmdStargateJump", &[PyRep::Integer(gate)], &HashMap::new(), None)
        .await
}

#[tokio::test]
async fn test_zarzakh_entry_gate_lock() {
    // Not tagged: the Emanation Lock notices (`OnEmanationGateLockRemoved`, gate-lock details) are not
    // implemented, so `feat:navigation.zarzakh-gate-lock` stays open.
    let ctx = ServerContext::new();
    let (mut sess, _beyonce, ship_id) = login_and_undock(&ctx, 7106).await;
    let turnur = 30_002_086u32;
    let zarzakh = 30_100_000u32;
    let (turnur_to_z, z_to_turnur, z_to_alsa) = (50_016_562i64, 50_016_566i64, 50_016_567i64);
    assert_eq!(ctx.sde.solar_systems[&zarzakh].name, "Zarzakh");
    let b = place_at_gate(&ctx, &mut sess, ship_id, turnur, turnur_to_z).await;
    let res = gate_jump(&ctx, &mut sess, b, turnur_to_z).await.expect("jump into Zarzakh");
    let PyRep::KeyVal(kv) = &res else { panic!() };
    assert_eq!(kv["solarSystemID"].as_i64(), Some(zarzakh as i64));
    ctx.advance_virtual_clock(15);
    let b = place_at_gate(&ctx, &mut sess, ship_id, zarzakh, z_to_alsa).await;
    let err = gate_jump(&ctx, &mut sess, b, z_to_alsa).await.expect_err("other gate locked");
    assert_eq!(err.code, "ZarzakhGateLocked");
    let b = place_at_gate(&ctx, &mut sess, ship_id, zarzakh, z_to_turnur).await;
    let res = gate_jump(&ctx, &mut sess, b, z_to_turnur).await.expect("entry gate allowed");
    let PyRep::KeyVal(kv) = &res else { panic!() };
    assert_eq!(kv["solarSystemID"].as_i64(), Some(turnur as i64));
}
